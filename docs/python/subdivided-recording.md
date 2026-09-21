# Per-play and per-wait native clips

`fmn_python.record_subdivided_scene` records each subsequent selected top-level
`Scene.play` or `Scene.wait` into its own native Reel generation. It is usable
with an already populated Scene and with ordinary `scene.run()` authoring.
It does not replay `construct`, replace the Stage, reseed randomness, or create
another animation clock, renderer, or encoder.

```python
from manimlib import Scene, Square, RIGHT, linear
from fmn_python import record_subdivided_scene

scene = Scene()
square = Square(fill_opacity=1)
scene.add(square)
with record_subdivided_scene(
    scene, "clips", format="gif", resolution=(640, 360), threads=4,
) as recording:
    scene.play(square.animate.shift(RIGHT), run_time=1, rate_func=linear)
    scene.wait(.5)

assert recording.result.completed
for segment in recording.result.segments:
    print(segment.play_index, segment.render.destination, segment.render.digest)
```

The destination directory must be new. Children retain original zero-based
`Scene.num_plays` indices, for example `segment-000003.gif`; PNG sequences use
`segment-000003/`. Source range selection and temporary skipping still execute
the native scene semantics, but omitted segments do not publish clips. An empty
`play()` produces no clip. Nested animation groups remain a single top-level
clip. A selected zero-duration call uses the existing recording contract of
capturing one final frame without advancing time.

Supported formats are **GIF, y4m, and PNG sequences**. These are silent outputs.
WAV and MP4/MOV soundtrack subdivision, CLI `--subdivide` routing, and automatic
prerun are not provided by this live-recording API. Ordinary whole-scene and
live single-clip recording remain available through the existing APIs. FPS
must match the live rational clock; changing `camera.fps` is not resampling.

## Publication and failure

Each clip is an independent atomic, no-clobber publication. A failed or
cancelled segment never rolls back scene effects or deletes earlier completed
clips. `partial_result` identifies those completed clips; it does not claim a
successful collection. Explicit cancellation leaves `result` unset. A rare
receipt failure after native publication is reported through
`partial_result.unreceipted_artifacts`, rather than losing the artifact's path
or counting it as a verified receipt. Such a collection cannot finish as a
success. The default budget is 10,000 completed segments and can be lowered via
`max_segments` (maximum 100,000).

The collection must begin and finish between play/wait calls. It cannot nest
another output owner or steal a recording. Existing mobject identities, live
views, updater registration and native time remain intact. Finish before
restoring checkpoints or resetting the clock. The returned receipts describe
standard-mode execution; recording arbitrary host history is not a certified
input closure.

## Native acceptance

`crates/fmn-python/tests/subdivided_recording.py` exercises the actual installed
extension, including byte equality between concatenated split frames and an
uninterrupted recording at one and four render threads, moving-object pixels,
source ranges, nested groups, all three formats, cancellation, primary-error
preservation, ownership refusals, publication races and partial receipts.
`.github/workflows/subdivided-recording.yml` builds the exact committed source
and runs the installed-wheel tests without source activation patches.
