# Portable native scene recordings with sound

FMTL/1 minor 2 carries already mixed mono/stereo Float32 PCM alongside the
original visual recording. Rendering the bundle no longer needs the source
audio files, scene code, callbacks or a media decoder. Camera-rig recordings can
carry sound using the same API family. Legacy picture-only exports keep their
original bytes and continue refusing authored sound cues.

## Export a native scene once

Use the explicit APIs in `fmn::exporting::soundtrack`. Source access goes through
the supplied `FileSystem`, including VirtualFs. The default decodes supported
WAV audio natively. Compressed sources require an explicit ffmpeg capability
and standard-mode preparation; there is no ambient decoder discovery or silent
cue omission.

```rust
use std::{path::PathBuf, sync::Arc};
use fmn::prelude::*;
use fmn::exporting::BundleExportOptions;
use fmn::exporting::soundtrack::{BundleSoundtrackOptions, export_bundle_with_soundtrack_with_fs};
use fmn_platform::fs::StdFs;

struct NarratedScene { narration: PathBuf }
impl SceneConstruct for NarratedScene {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        let circle = stage.add(Circle::new().radius(0.8).color(BLUE))?;
        stage.scene_mut().add_sound(&self.narration, 0.0, None, None)?;
        stage.play(circle.animate().shift(RIGHT)?)?;
        stage.wait(0.5)?;
        Ok(())
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut scene = NarratedScene { narration: "narration.wav".into() };
    export_bundle_with_soundtrack_with_fs(
        &mut scene, "narrated.fmtl", BundleExportOptions::new()?,
        &BundleSoundtrackOptions::default(), Arc::new(StdFs),
    )?;
    Ok(())
}
```

The supplied WAV must exist in that filesystem. Publication is create-only;
an existing `narrated.fmtl` is never replaced. Recording, native mixing and final
format validation all complete before private output staging starts.

`export_bundle_with_soundtrack_bytes` returns the in-memory SceneBundleExport.
The camera equivalents are `export_camera_bundle_with_soundtrack_bytes` and
`export_camera_bundle_with_soundtrack_with_fs`: they accept the same camera
factory and CameraConfig as the established camera export API. The factory
must not run playback before returning its program and rig.

## Opt in to compressed narration in the native API

With the `fmn` crate's `ffmpeg` feature enabled, the host can explicitly supply
the existing governed process capability. Using the scene from the example
above, an MP3 export becomes:

```rust
let mut bundle = BundleExportOptions::new()?;
bundle.config.determinism.mode = DeterminismMode::Standard;
bundle.config.file_writer.ffmpeg_bin = "ffmpeg".to_owned();
let audio = BundleSoundtrackOptions {
    ffmpeg: Some(FfmpegCapability::host()),
    ..BundleSoundtrackOptions::default()
};
let receipt = export_bundle_with_soundtrack_with_fs(
    &mut NarratedScene { narration: "narration.mp3".into() },
    "narrated.fmtl", bundle, &audio, Arc::new(StdFs),
)?;
let sound = receipt.soundtrack.as_ref().expect("the scene authored a cue");
println!("{} cues, {} external decodes",
         sound.mix.cues_mixed, sound.audio_decodes.len());
```

Hosts may instead inject their own `FfmpegCapability` runner, locator and private
work-directory parent; enabling a host runner is not necessary for an injected
capability. The executable request is the existing
`BundleExportOptions.config.file_writer.ffmpeg_bin`. Empty or NUL-containing
requests are rejected when a capability is supplied. `ffmpeg_limits` on the
audio options bounds each external decode's duration, logs and artifact bytes.
These limits and the ordinary recording/mixing settings are validated before
scene execution or a camera factory. Sample-derived decode limits remain an
independent, possibly tighter expansion bound.

Container detection follows the actual source bytes. FLAC, MP3, ADTS AAC, Ogg,
M4A/MP4, AIFF and supported non-PCM WAV can use the existing Reel decoder.
Playlists, URLs and unknown bytes are refused. Native PCM WAV, malformed native
PCM, exhausted native limits and scenes without cues never resolve or probe the
external capability. The first compressed source that needs decoding resolves
one bounded, fingerprinted ffmpeg boundary; subsequent sources in that export
reuse its identity. Repeated cue paths still read and decode once.

Certified configuration continues to require native PCM WAV, even when a
capability is supplied. A recognized external-decode request returns a named
certified-preparation error before any locator or process call. Standard-mode
decoded samples enter the same native placement, gain, ducking and resampling
path as WAV. They are embedded as PCM, so the resulting bundle adds no decoder
requirement to Studio, the CLI or the browser player.

Both `SceneBundleExport` and the published `BundleExportReport` now expose
`soundtrack: Option<BundleSoundtrackReport>`. It is `None` when no cue was
authored. A sound-bearing receipt contains the existing native `SoundtrackReport`
as `mix`, the actual `workers_used`, cue-ordered `SoundtrackInput` records in
`inputs`, and distinct external `audio_decodes` in first-source-use order.
Each input retains the original source bytes plus its source/normalized-PCM
digests and decoder receipt. Repeated cues share their byte allocation. Receipt
construction never reopens a source path, including when a source changed during
decoding. Native inputs have no external invocation. These reports accompany
the result; original files, paths and decoder instructions are not embedded as
playback dependencies or presented as a complete certified input closure.

## Python-authored scenes

The Python API can export the same portable sound-bearing format with explicit
`audio=True`. A matching native wheel must expose both soundtrack acquisition
and finalization before the API constructs a Scene class. Planar and camera
recordings use the same native recorder and mixer as their respective paths.

```python
from manimlib import Scene, Square, RIGHT
from fmn_python import export_bundle

class NarratedScene(Scene):
    def construct(self):
        square = Square(fill_opacity=1)
        self.add(square)
        self.add_sound("narration.wav", gain=-3.0)
        self.play(square.animate.shift(RIGHT), run_time=2)

receipt = export_bundle(NarratedScene, "narrated.fmtl", audio=True,
                        camera=True, resolution=(1280, 720), fps=30)
print(receipt.as_dict())
```

`narration.wav` must exist, and the destination must not. Relative cue paths
are anchored at the native `add_sound` call. Playback needs neither Python nor
these original audio assets. Open the artifact in the local browser player or
use the native `render_bundle` video API described below.

Defaults are `audio_sample_rate=48000`, `audio_channels=2`, `audio_threads=1`,
`max_audio_bytes=64*1024*1024`, and `audio_ffmpeg=False`. Rates 1..384000, mono/stereo, 1..64 workers,
and 8 bytes..256 MiB budgets are accepted; incompatible options are rejected
before Scene class construction. Nondefault audio settings require `audio=True`.
Browser audio devices may support fewer sample rates than the native export.

The byte setting independently caps cumulative unique source bytes, cached
Float32 PCM, prepared Float64 cue samples, and final Float32 PCM. One transient
channel-mapping buffer has the same cap; these are separate bounded pools, not
a total process-RSS ceiling. Each new source's decode receives the *remaining*
decoded-sample allowance before allocating. Repeated paths are read/decoded
once, but each cue's prepared buffer is charged separately. Exports admit at
most 4096 cues. The complete pictures-plus-sound file still obeys
`max_output_bytes`, and nothing is published until capture, mix and validation
succeed. A competing destination writer is never overwritten.

Compressed source cues require the explicit `audio_ffmpeg=True` option in
addition to `audio=True`. The byte-recognized formats are the same as ordinary
Python audio rendering: FLAC, MP3, ADTS AAC, M4A/MP4, Ogg Vorbis/Opus, AIFF and
supported non-PCM WAV. The source can have any filename extension. The default
remains native WAV only, even if ffmpeg is installed.

```python
class CompressedNarration(Scene):
    def construct(self):
        self.add(Square())
        self.add_sound("narration.mp3", gain=-3)
        self.wait(5)

receipt = export_bundle(
    CompressedNarration, "compressed.fmtl",
    audio=True, audio_ffmpeg=True, camera=True, storage="compact",
    scene_kwargs={"file_writer_config": {
        "ffmpeg_bin": "/opt/media tools/ffmpeg",
        "ffmpeg_timeout": 900,
    }},
)
print(receipt.audio_inputs)
print(receipt.ffmpeg_invocations)
```

Both recorded and compact storage support this opt-in, with planar or camera
capture. The decoder freezes `SceneFileWriter.ffmpeg_bin`,
`ffmpeg_timeout`, `ffmpeg_max_log_bytes`, `ffmpeg_max_artifact_bytes`,
and `ffmpeg_max_input_bytes` when the export generation opens. Invalid policy
is refused before setup/construct/tear-down. The existing decoder snapshots its
host search path, working directory and temporary-directory root at that point;
later callbacks cannot redirect the executable request or replace its policy.
No-cue exports and native PCM WAV never locate, probe or launch ffmpeg.
Malformed PCM and unknown containers also fail before external discovery.

The first recognized source requiring external decode resolves one bounded,
fingerprinted tool. Later distinct sources retain that tool identity; repeated
source paths reuse their decoded PCM and immutable report. Each cue still has
its own timing, gain and ducking request. Source reads and decompression obey the
remaining cumulative budgets before allocation. The independent process artifact
limit remains in force even when the native sample allowance is larger. Missing
capabilities, decode failures and exhausted limits leave no published bundle.

Scene setup/construct/tear-down run once; audio capture does not rerun Python
callbacks. Offsets, gains, ducking and resampling are performed by the existing
native mixer. Short cues pad through the picture grid; long tails are retained.
Static scenes get one terminal still without advancing authored time.
`show()` captures do not advance Scene time and are refused for audible exports
rather than silently moving later cues. Skip/range/presenter modes remain
unsupported. Use ordinary play/wait playback for audible bundles.

No-cue exports retain the exact picture-only bytes and existing receipt keys,
including compact storage and `audio_ffmpeg=True`. A sound-bearing receipt adds
`fmtl_minor=2` and a `soundtrack` object containing sample rate, channels and
sample-frame count; `receipt.soundtrack` exposes the corresponding immutable
attributes. Default native-only exports retain their existing receipt keys.

Opted-in, sound-bearing exports additionally expose `audio_inputs`,
`ffmpeg_invocations` and `ffmpeg_limits`, using the ordinary render receipt
schema. Source facts are in cue order and contain path, source SHA-256,
normalized PCM SHA-256, container, source rate/channels/frame count and decoder
identity. Invocation facts appear once per decoded source in first-use order;
a repeated compressed cue does not produce a duplicate invocation. Native WAV
with an offered capability records its source facts and admitted policy with an
empty invocation list. `as_dict()` returns independent nested copies.
These facts do not certify arbitrary Python effects or external decoding;
`certified_source` remains false.

The console exposes the same opt-in for single scenes, sequential batches and
spawned-worker batches:

```bash
fmn-python lesson.py NarratedScene --format fmtl --bundle-audio --bundle-audio-ffmpeg \
  --ffmpeg_bin '/opt/media tools/ffmpeg' --output narrated.fmtl
fmn-python lesson.py First Second --format fmtl --bundle-audio --bundle-audio-ffmpeg \
  --bundle-camera --bundle-storage compact --workers 2 --video_dir bundles
```

The explicit console executable override is frozen before authored source or
constructors can change its relative path or value, then applied before native
decoder capture. Other writer, camera and text settings retain their existing
precedence. Batch checkpoint identity includes the compressed-audio opt-in and
explicit decoder override; resumed completed artifacts retain validated source,
decoder and policy receipts without decoding their assets again.

The native acceptance fixture `fmn-python/tests/soundtrack_bundle_export.py`
compares analytic PCM, direct WAV output, planar/camera replay at 1/4/16 workers,
source-independent replay, exact budgets and failure/publication ownership.
It also compares eight real encoded fixture formats with independently decoded
native WAV through every recorded/compact and planar/camera combination,
checks one-decode repeated paths, retained policy, caller mutations, private
decoder overrides and failed publication. The acceptance helper reports actual
completed artifact comparisons separately from compressed-source comparisons.
Without an installed optional decoder the external count is zero;
`FMN_REQUIRE_FFMPEG=1` requires that coverage.

Run it through `cargo test -p fmn-python --lib portal_studio::bundle::tests`.
Host-protocol tests use explicit native-boundary spies and are separate from
that real-engine evidence.

## Timing, mixing and bounds

The original Scene sound requests are fed to PreparedSceneSoundtrack and
SoundMixer, not a new audio loop. Rational cue call sites, relative offsets,
gains, insertion-ordered background ducking, channel mapping and native
resampling retain their established semantics. Negative offsets preserve
source phase. Identical paths are read/decoded once; each cue retains its own
placement and mix parameters. Changing the worker count does not change PCM.

The track is padded to the observed picture duration, including an explicit
one-frame terminal still, without advancing the authored scene clock. Cue
tails are retained. The sample clock is independent of the picture FPS.
Only the final normalized Float32 samples enter the portable artifact; no
source path or decoder instruction is serialized as an audio dependency.

BundleSoundtrackOptions sets the optional decoder capability and its process
limits alongside the mixer/decoder limits, worker count and
SoundtrackPreparationLimits. The latter separately bounds cue count, unique
source bytes, retained decoded samples and per-cue mapped/resampled samples.
Decoded samples are f32; mapped/resampled samples are f64. Each decode also
has its own AudioDecodeLimits bound. A rejection after a decode can temporarily
hold that one bounded decode in addition to the previous retained cache.
Transient channel mapping, output accumulation and the final PCM are additional
bounded buffers, so these are not a claimed process RSS ceiling. Arbitrary
allocations made by authored scene code are not controlled by these limits.

The output mixer duration is additionally bounded by the remaining canonical
bundle byte allowance before preparation. The complete pictures-plus-sound
envelope, including its framing, is checked against the exact final byte cap.
A missing source, malformed WAV, exhausted budget or failed publication returns
an error and never publishes a partial sound-bearing artifact.

## Replay to MP4/MOV

`fmn::rendering::render_bundle` and `render_bundle_with_fs` now route stored PCM
to the existing governed MP4/MOV output boundary. The caller still supplies
standard-mode video configuration and an explicit FfmpegCapability for the
capability-based API. Picture workers reconstruct the same stored frames and
cameras; no authored callback, source-file read or audio decoder runs on replay.
The stored PCM is one already-mixed native cue, with no second gain or
resampling. Accordingly replay's SoundtrackReport counts one cue, not the
number of original authored sound requests. Final audio encoding may be lossy;
the portable PCM itself is not.

PNG, GIF and Y4M replay refuses sound-bearing input before publication.
The standalone CLI and recorded Studio audio adapters consume the embedded
track without decoding its original sources. The WASM
picture-only constructor still refuses sound at load, while the explicit
`FmnPlayer.from_bundle_with_audio` route and local browser player now support
synchronized audio; see [Browser soundtracks](BROWSER_SOUNDTRACKS.md).
An audio-aware host explicitly takes the soundtrack before dispatching visual
jobs. Simply loading an artifact never authorizes dropping its audio.

Validation: `fmn/tests/soundtrack_bundle.rs` compares generated PCM with an
independently authored native mix, preserves observed geometry/camera captures,
checks cumulative limits and no-clobber publication, and exercises real
MP4/MOV replay after deleting all original source assets. Retained mux-input
WAV samples are checked against that mix; published files must contain both
video and sound tracks. Worker counts 1/4/16 and bounded pipeline completion
are covered. Host ffmpeg absence is a named test-input skip unless
`FMN_REQUIRE_FULL_INPUTS=1`, in which case it fails.

The compressed native tests use a scripted process capability with tiny native
image fixtures: no installed ffmpeg, media fixture download or external process
is needed. They compare recorded and compact FMTL bytes with equivalent native
WAV preparation, exercise camera publication, retain exact source/decode receipts
after source mutation, and check typed missing/certified capability errors,
decode/expansion limits and failed publication. The decoder's focused admission
tests run in `fmn-output` without its default process feature:

```bash
cargo test -p fmn-output --no-default-features --test audio audio_deferred_boundary
cargo test -p fmn --test soundtrack_bundle
```
