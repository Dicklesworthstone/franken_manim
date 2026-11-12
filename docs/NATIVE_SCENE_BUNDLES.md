# Native scenes to standalone FMTL artifacts

`fmn::export_bundle` runs an ordinary `SceneConstruct` once and publishes a
validated FMTL/1 file. The existing standalone CLI and bundle players can render
that file without linking the original program or executing its updaters.

```sh
cargo run --locked -p fmn --example export_scene -- scene.fmtl
fmn --format png_sequence scene.fmtl
```

A new destination is required. Exports never overwrite an existing file,
directory or symlink, including when another process creates the destination
between capture and publication.

## Rust API

```rust,no_run
use fmn::prelude::*;

struct Example;
impl SceneConstruct for Example {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        let circle = stage.add(Circle::new().color(BLUE))?;
        stage.play(circle.animate().shift(RIGHT)?)?;
        stage.wait(0.5)?;
        Ok(())
    }
}

let mut options = BundleExportOptions::new()?;
options.config.camera.fps = 30;
options.limits.max_frames = 30 * 60;
let receipt = export_bundle(&mut Example, "example.fmtl", options)?;
println!("{} frames, sha256 {}", receipt.frame_count, receipt.digest);
# Ok::<(), Box<dyn std::error::Error>>(())
```

`export_bundle_bytes` returns a `SceneBundleExport` without filesystem access.
`export_bundle_with_fs` accepts the existing `FileSystem` capability and uses
its atomic create-only publication contract. The native error, recording error
and filesystem error remain distinct typed variants of `BundleExportError`.
A caller-provided filesystem without atomic create-only support is refused.

### Compact recordings

Choose compact storage when a long imperative scene would otherwise retain a
complete snapshot for every frame:

```rust,no_run
use fmn::exporting::{BundleExportOptions, export_bundle_bytes};

let mut options = BundleExportOptions::compact()?;
options.config.camera.fps = 30;
let artifact = export_bundle_bytes(&mut Example, options)?;
println!("{} bytes for {} frames", artifact.bundle.bytes.len(), artifact.bundle.frame_count);
# Ok::<(), Box<dyn std::error::Error>>(())
```

`BundleExportOptions::compact()` selects
`BundleStorage::Compact(CompactRecordingLimits::default())`; the two public
types live in `fmn::exporting`. An existing options value can choose that same
mode through its `storage` field. `BundleExportOptions::new()` continues to
select `BundleStorage::Recorded`, preserving the original export format.

For custom limits, set the selected storage mode explicitly:

```rust
use fmn::exporting::{BundleStorage, CompactRecordingLimits};

options.storage = BundleStorage::Compact(CompactRecordingLimits {
    max_frames: 30_000,
    max_retained_bytes: 64 * 1024 * 1024,
    max_snapshot_bytes: 16 * 1024 * 1024,
    max_snapshot_work_bytes: 1024 * 1024 * 1024,
});
```

`options.limits` belongs to recorded storage; compact mode uses the limits
inside `BundleStorage::Compact`. `max_output_bytes` applies in both modes.
Compact mode refuses nondefault legacy `options.limits` before scene execution
so an intended capture limit cannot be silently ignored.

Compact mode writes FMTL/1 minor 3. Each group of at most 64 captured frames
stores one complete snapshot followed by exact base-relative byte patches,
identical-frame references or complete literals when a patch would cost more.
Random access needs only the selected block, and owned shared jobs keep this
representation compact across worker handoff. This saves storage for repeated
geometry and sparse changes while preserving every observed frame. It does
not skip source execution, infer a callback's purity, change sampling or
approximate a frame. The mode is accepted by ordinary, camera-bearing and
soundtrack export entry points.

At the lower-level sink boundary, `SceneBundleRecorder::new_compact` retains
the same full-state snapshots as `new`, including detached arena entries.
`new_compact_render_only` explicitly chooses the rooted render projection, and
`new_compact_with_camera` retains the same render projection and camera samples
as its original camera-aware counterpart. Storage selection does not silently
switch an ordinary native export to a render-only snapshot contract.

## Sampling and replay

This imperative path uses `SceneBundleRecorder`, a real `SceneSink`. It records
actual immutable captures in their emitted order, preserving play/wait
boundaries, zero-frame segments, early-stop waits and explicit `show()` captures
from the lower-level Scene API. All segments retain recorded snapshot
semantics, using kind 1 for the original representation or kind 2 for compact
storage. There is no endpoint-only purity guess, callback serialization or
second scene execution. Arbitrary native updaters and noncatalog rate functions
therefore retain their observed output on random-access replay.

The exported schedule describes the captured frame grid, not the original
floating-point duration argument. It is validated by the existing exact rational
clock and FMNA/5 reader. Explicit shows are one-frame holds; a native program
that emits no captures produces one terminal still. Stills do not advance the
source scene's clock, so the receipt's scene time and output frame count can
differ. Ordinary native `EndScene` termination remains successful.

The geometry-only entry point leaves renderer configuration to playback: use
matching resolution, background, frame height, antialiasing and fixed camera.
The camera-aware entry points below record those camera samples explicitly.
The existing `fmn::rendering::render_bundle` and `render_bundle_with_fs`
functions consume either storage mode through the shared production reader.

## Recorded camera and sound

`fmn::exporting::export_camera_bundle_bytes`, `export_camera_bundle` and
`export_camera_bundle_with_fs` accept a factory that binds a `CameraRig` to the
new scene and returns the authored program with that rig. They sample the
actual post-updater camera at each capture boundary. Position, orientation,
background, light, frame shape and sample policy travel with the picture;
playback does not need the original rig or its callbacks. Recorded mode uses
minor 1 and compact mode uses minor 3 with its camera-present flag. Paired
`stage_at_with_camera` and shared `materialize_with_camera` access preserve the
camera contract; geometry-only access refuses to drop it.

The explicit `fmn::exporting::soundtrack` entry points mix native WAV assets
through a caller-provided `FileSystem`, then wrap the validated visual recording
and final PCM in minor 2. Both ordinary and camera-aware soundtrack exports
accept the storage option. No sound requests means no extra sound envelope.
Picture-only export entry points continue to refuse sound requests. A reader
must explicitly take responsibility for an attached soundtrack before
dispatching pictures; loading an artifact does not authorize discarding audio.

## Limits and refusals

Recorded capture limits bound admitted frame count and cumulative canonical
snapshot and destination-table storage. Compact limits independently bound
frame count, retained compact storage, the size of one complete canonical
snapshot and cumulative canonical snapshot work. The compact defaults are
1,000,000 frames, 256 MiB retained storage, 64 MiB per snapshot and 16 GiB of
snapshot work. Every capture consumes work even if its compact entry is Same;
a long static scene cannot bypass admission by compressing well. Camera storage
is also charged. These limits do not interrupt arbitrary Rust code or the
runtime's deterministic completion of an already-started animation segment.
The complete canonical artifact also has the production format limit and the
caller's `max_output_bytes` limit, enforced by bounded writers before growth.
Recorded snapshots are released as they are encoded; compact capture retains
its bounded block representation instead of a complete decoded movie.
The recorder rejects missing, duplicated, reordered and mistagged segment
captures, including a missing final capture detected at the finish boundary.
Any capture failure permanently poisons the recording, even when scene code catches its play/wait error. Scene execution,
serialization, size checks and production-reader validation all finish before
file staging starts; a failed or panicking scene cannot publish partial output.

Renderer policy beyond recorded camera samples and source/asset closure
attestation remain separate contracts. Compact storage does not promote an
imperative segment to a proven pure segment. Existing declarative
`export_timeline_bundle` retains its separate proven-purity path. See
[`FMNT1_TIMELINE_BUNDLE.md`](FMNT1_TIMELINE_BUNDLE.md) for the wire layout,
decoder budgets and camera/sound consumer requirements.

The regression tests exercise real stateful scene capture, canonical
snapshot equality, direct-versus-replayed PNG bytes at different worker counts,
clock rounding, static scenes, deterministic re-export, errors, budgets,
no-clobber publication and unwinding. Compact regressions additionally compare
independent serial captures across block boundaries, reverse/random access,
shared worker ownership and the explicit camera/sound routes.
