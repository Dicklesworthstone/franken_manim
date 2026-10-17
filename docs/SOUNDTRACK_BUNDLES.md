# Portable native scene recordings with sound

FMTL/1 minor 2 carries already mixed mono/stereo Float32 PCM alongside the
original visual recording. Rendering the bundle no longer needs the source
WAV files, scene code, callbacks or a media decoder. Camera-rig recordings can
carry sound using the same API family. Legacy picture-only exports keep their
original bytes and continue refusing authored sound cues.

## Export a native scene once

Use the explicit APIs in `fmn::exporting::soundtrack`. Source access goes through
the supplied `FileSystem`, including VirtualFs; export does not launch ffmpeg.
These entry points decode native supported WAV audio. Compressed media is a
named capability refusal, not an implicit transcode or silent cue omission.

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
and `max_audio_bytes=64*1024*1024`. Rates 1..384000, mono/stereo, 1..64 workers,
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

Native supported WAV inputs require no ffmpeg process. Compressed audio is
an explicit capability refusal in this export API, even when ffmpeg is installed.
Scene setup/construct/tear-down run once; audio capture does not rerun Python
callbacks. Offsets, gains, ducking and resampling are performed by the existing
native mixer. Short cues pad through the picture grid; long tails are retained.
Static scenes get one terminal still without advancing authored time.
`show()` captures do not advance Scene time and are refused for audible exports
rather than silently moving later cues. Skip/range/presenter modes remain
unsupported. Use ordinary play/wait playback for audible bundles.

No-cue exports retain the exact minor-0/minor-1 bytes and existing receipt keys,
even with `audio=True`. A sound-bearing receipt adds `fmtl_minor=2` and a
`soundtrack` object containing sample rate, channels and sample-frame count;
`receipt.soundtrack` exposes the corresponding immutable attributes. The
snapshot is not certification of arbitrary Python source effects. CLI and batch
soundtrack-option wiring remain separate from this Python single-scene API.

The native acceptance fixture `fmn-python/tests/soundtrack_bundle_export.py`
compares analytic PCM, direct WAV output, planar/camera replay at 1/4/16 workers,
source-independent replay, exact budgets and failure/publication ownership.
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

BundleSoundtrackOptions sets the mixer/decoder limits, worker count and
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
Standalone CLI and Studio audio adapters remain separate work. The WASM
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
