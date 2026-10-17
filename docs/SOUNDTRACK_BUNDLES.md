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

PNG, GIF and Y4M replay refuses sound-bearing input before publication. The
current standalone CLI, Studio and WASM adapters have not gained audio playback
through this change: picture-only consumers fail closed. WASM refuses at load.
An audio-aware host can explicitly `take_soundtrack()` from TimelineBundle and
then dispatch visual jobs, accepting responsibility for synchronized sound.
Simply loading the artifact never authorizes dropping its audio.

Validation: `fmn/tests/soundtrack_bundle.rs` compares generated PCM with an
independently authored native mix, preserves observed geometry/camera captures,
checks cumulative limits and no-clobber publication, and exercises real
MP4/MOV replay after deleting all original source assets. Retained mux-input
WAV samples are checked against that mix; published files must contain both
video and sound tracks. Worker counts 1/4/16 and bounded pipeline completion
are covered. Host ffmpeg absence is a named test-input skip unless
`FMN_REQUIRE_FULL_INPUTS=1`, in which case it fails.
