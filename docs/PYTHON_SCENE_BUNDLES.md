# Python-authored standalone scene bundles

`fmn_python.export_bundle`, `BundleExportSession`, and `fmn-python --format fmtl`
connect the optional Python authoring portal to the same FMTL/1 reader used by
standalone `fmn` and the WASM timeline player. The host interpreter runs the
scene once. Actual post-updater snapshots are recorded; playback does not need
Python, execute callbacks, approximate a rate function, or re-run the source.

## Export and replay

With a matching installed wheel and native binary:

```sh
fmn-python crates/fmn-python/examples/portable_scene.py PortableScene \
  --format fmtl --resolution 960x540 --fps 30 --output portable.fmtl

fmn portable.fmtl --format png_sequence --resolution 960x540 \
  --video_dir replayed
```

The portal's `--output` names the **exact file** for a single export.
`--video_dir` names an **output directory**, both for portal scene selection
and native replay. Without a portal destination, the export is
`media/videos/SOURCE_STEM/SCENE.fmtl`. The single-scene form can omit its scene
name when the source declares exactly one scene. Both storage modes support
multiple scene names or `--write_all`, with a separate artifact per scene.
`--robot` reserves stdout for one JSON
receipt; authored import/setup/construct/teardown chatter goes to stderr.
`--format=fmtl` is also accepted.

The bundle stores its rational FPS and exact frame schedule. Geometry-only
exports leave resolution and fixed framing to playback; camera-aware exports
also record the captured view and aspect. Replay with the same resolution for
matching pixel dimensions. A conflicting explicit FPS is rejected by the native player.
The native/WASM readers retain their own resource limits and capability checks.

### Compact storage for long scenes

Choose compact capture explicitly on the authoring command:

```sh
fmn-python crates/fmn-python/examples/portable_scene.py PortableScene \
  --format fmtl --bundle-storage compact --resolution 960x540 --fps 30 \
  --output portable-compact.fmtl
```

The library equivalent is `export_bundle(PortableScene,
"portable-compact.fmtl", storage="compact")`; `BundleExportSession` accepts the
same storage choice. Compact recordings use FMTL/1 minor 3 and replay through
the same native and WASM production reader. The default remains
`storage="recorded"`, preserving the original snapshot recording format.
Compact storage also supports named batches, `--write_all`, isolated workers,
parameter jobs, and checkpoint recovery through the same publication owner.

Compact mode still runs every authored updater at every ordinary capture
boundary. It stores consecutive groups of at most 64 captures as a complete
base snapshot plus exact byte patches, repeated-base references or full
literals. Every frame therefore retains its actual observed geometry and
state, including arbitrary supported Python callback output. Playback does
not need Python or preceding frames from the movie. Storage savings depend on
the captured changes; this mode makes no purity claim and does not change
sampling or supported scene capabilities.

Compact byte budgets have matching API and CLI controls:

| `compact_limits` key | CLI flag | Default / maximum |
|---|---|---:|
| `max_retained_bytes` | `--bundle-max-retained-bytes` | 268,435,456 bytes (256 MiB) |
| `max_snapshot_bytes` | `--bundle-max-snapshot-bytes` | 67,108,864 bytes (64 MiB) |
| `max_snapshot_work_bytes` | `--bundle-max-snapshot-work-bytes` | 17,179,869,184 bytes (16 GiB) |

Values must be positive integers within those limits; the CLI accepts unsigned
decimal bytes. These options require compact storage. Existing `max_frames`
and `max_output_bytes` remain top-level library options; `max_capture_bytes`
belongs to recorded storage. Compact mode refuses a nondefault
`max_capture_bytes` before constructing the scene; use `compact_limits` for
that mode's byte budgets. For example:

```python
receipt = export_bundle(
    PortableScene,
    "portable-compact.fmtl",
    storage="compact",
    max_frames=30_000,
    compact_limits={
        "max_retained_bytes": 64 * 1024 * 1024,
        "max_snapshot_bytes": 16 * 1024 * 1024,
        "max_snapshot_work_bytes": 1024 * 1024 * 1024,
    },
    max_output_bytes=64 * 1024 * 1024,
)
```

### Batches, isolated workers, and recovery

Export every local scene with the same per-scene compact budgets:

```sh
fmn-python lessons.py --write_all --format fmtl --bundle-storage compact \
  --bundle-max-retained-bytes 67108864 \
  --bundle-max-snapshot-bytes 16777216 \
  --bundle-max-snapshot-work-bytes 1073741824 \
  --video_dir bundles --workers 2 \
  --checkpoint bundle-progress.json --resume-key lessons-assets-v1
```

Repeat the command with `--resume` to hash-verify and reuse completed artifacts.
Incomplete scenes run in fresh workers. The same storage choice and budgets
also work with an explicit list of scene names or `--jobs-file variants.json`.
Without `--workers`, the existing serial batch path owns each scene in order.
Worker count and timeout may change when resuming; capture settings may not.

The matching APIs are `render_scenes` for classes or pristine instances and
`render_source_scenes` for source-file jobs. Both accept `bundle_storage` and
`bundle_compact_limits`:

```python
from fmn_python import render_scenes

report = render_scenes(
    {"Introduction": Introduction, "Proof": Proof},
    "bundles",
    format="fmtl",
    bundle_storage="compact",
    bundle_compact_limits={
        "max_retained_bytes": 64 * 1024 * 1024,
        "max_snapshot_bytes": 16 * 1024 * 1024,
        "max_snapshot_work_bytes": 1024 * 1024 * 1024,
    },
    bundle_limits={"max_frames": 30_000, "max_output_bytes": 64 * 1024 * 1024},
    checkpoint="bundle-progress.json",
    resume_key="lessons-assets-v1",
    # Set resume=True when continuing this same admitted scene plan.
)
```

Limits are normalized and copied before iterating caller-supplied scene jobs,
constructing scenes, or invoking result callbacks. Each scene receives its own
full budget. `bundle_limits.max_capture_bytes` belongs to recorded storage;
compact batches refuse a nondefault value and use `bundle_compact_limits`
instead. Resolution, FPS, camera/audio choices, constructor inputs, storage,
frame/output caps, and all three compact caps form part of checkpoint identity.
Changing any of them rejects reuse before scene constructors or workers run.
Recorded defaults retain their existing journal and receipt representation.

Compact receipts declare `storage: "compact"` and `fmtl_minor: 3`. When a scene
has a soundtrack, they declare outer `fmtl_minor: 2` and
`picture_fmtl_minor: 3`; a scene without cues keeps its minor-3 picture receipt.
The coordinator validates these fields against its admitted plan before
accepting a worker success or reusing a checkpoint. Missing, contradictory, or
downgraded storage metadata is rejected even when the file digest still matches.

The journal independently verifies artifact sizes and hashes. It does not
restore Python objects or certify imports and external effects. Keep the
caller-owned `resume_key` current when scene or asset inputs change. Publication
remains create-only: an unrecorded file left by a crash is a collision requiring
the caller's attention, not an artifact the exporter silently adopts.

## Library API

```python
from fmn_python import export_bundle
from portable_scene import PortableScene

receipt = export_bundle(
    PortableScene,
    "portable.fmtl",
    resolution=(960, 540),
    fps=30,
    max_frames=30_000,
    max_capture_bytes=64 * 1024 * 1024,
    max_output_bytes=64 * 1024 * 1024,
)
print(receipt.frame_count, receipt.digest)
```

A Scene class is constructed once. A supplied Scene instance must still be
pristine. `scene_kwargs` applies only to Scene classes. The ordinary `run()`
lifecycle owns setup, construct, and teardown; successful `EndScene` termination
is retained. Alternatively, use `BundleExportSession(scene, path, ...)` as a
context manager and author operations inside it. That context does not call
`run()` implicitly.

The recorded artifact preserves stateful Python updater output, arbitrary
supported easing callbacks, play/wait boundaries, explicit captures, and the
actual sampled frame count. Empty/static scenes get one terminal still; an
empty `play()` creates no phantom segment. Recorded segments are snapshots,
not assertions that a callback is pure or serializable.

## Explicit boundaries

The default geometry-only export admits **planar vector content**, including
native text and math outlines, at the default camera, light and background.
It refuses camera motion, non-default camera/background/light state and content
requiring camera-aware replay. Select `camera=True` or `--bundle-camera` to
record the captured view, lighting and background together with supported
3D, raster and vector geometry. That explicit route uses minor 1 for recorded
storage or the camera-bearing minor-3 form for compact storage. Camera
validation remains authoritative, including aspect and frame mapping; the
examples use the default 16:9 frame.

Select `audio=True` or `--bundle-audio` to mix native WAV cues into the minor-2
PCM envelope. This combines with either camera choice and either storage mode.
The default mixer is 48 kHz stereo with one worker and a 64 MiB audio budget;
its explicit audio options remain separate from capture/output budgets.
No transcoder runs during export. Compressed cues are refused, and picture-only
export refuses authored sound instead of silently dropping it. A scene without
sound cues retains its visual document even when audio export was selected.

Skipping, animation ranges, presenter mode, subdivided output, and source
certification are not supported by this full-scene export command. Each scene's
capture is serial authoring; batch `--workers` isolates independent scenes.
Set playback thread counts on `fmn`, not on the exporter.
The receipt explicitly says `certified_source: false`: storing snapshots does
not attest arbitrary Python imports, asset reads, or side effects.

Capture defaults are one million frames and 256 MiB of charged snapshot/table
storage in recorded mode. Compact mode instead has separate limits for one
million captured frames, 256 MiB of retained compact data, 64 MiB per complete
canonical snapshot and 16 GiB of cumulative canonical snapshot work. Identical
frames still consume the full snapshot-work budget; compression does not allow
unbounded capture or validation. `compact_limits` supplies explicit compact
budget overrides on `export_bundle` and `BundleExportSession`.

Output defaults to the production reader's 256 MiB cap in either mode. The
capture and output budgets are independently adjustable downward in the API.
They are not a bound on arbitrary authored Python allocations or execution
time. In-flight native animation cleanup retains its existing semantics after
a capture failure. Compact storage retains the explicit camera/audio selections
and their validation; it does not silently widen the authoring contract.

No destination is published until execution, bounded encoding, and production
reader validation succeed. Publication is atomic and create-only, including a
race with another writer after initial checks. An existing file, directory, or
symlink is never replaced. An updater/capture/segment failure stays fatal even
when authored code catches it: truncated animation is not a successful export.

## Validation entry points

The host-only tests explicitly spy on native output boundaries:

```sh
python -m unittest discover -s crates/fmn-python/tests -p 'test_bundle*protocol.py' -v
python -m unittest discover -s crates/fmn-python/tests -p 'test_parallel_bundle*protocol.py' -v
```

The native integration test exercises the actual shared recorder/reader and
compares every decoded RGBA frame with independent direct rendering:

```sh
cargo test --locked -p fmn-python portal_studio::bundle -- --nocapture
```

The installed-wheel/standalone test exercises both public entry points and
compares complete Y4M output at 24/30/60 FPS and one/four replay workers:

```sh
FMN_TEST_BIN="$PWD/target/portable/debug/fmn" \
  python crates/fmn-python/tests/bundle_cli_native.py
```

These are validation commands, not a claim that the native build has passed.
The implementation session ran the Python protocol suites; Rust and fresh-wheel
acceptance require the matching pinned-toolchain build.
