# Animation output and a final PNG in one execution

The Python portal can publish an animation or soundtrack and an independent,
full-resolution RGBA final image without reconstructing the scene. Geometry,
clocks, updaters, RNG, output encoding and publication use the existing native
owners. The PNG is a snapshot of the scene after its lifecycle completes.

## Explicit output

```python
from manimlib import *
from fmn_python import render_scene_with_still

class Example(Scene):
    def construct(self):
        box = Square(fill_opacity=1)
        self.add(box)
        self.play(box.animate.shift(RIGHT), run_time=1)

result = render_scene_with_still(
    Example, "outputs/example.y4m", "outputs/example.png",
    resolution=(960, 540), fps=30, threads=4,
)
assert result.completed
print(result.primary.digest, result.still.digest)
```

The primary supports Y4M, GIF, PNG sequences, WAV, and the existing optional
ffmpeg-backed MP4/MOV modes. It retains its ordinary native limits, audio mix,
codec options and receipts. The final image is always a separate PNG, including
for transparent animations. `result.as_dict()` includes both receipts and a
completion flag; it is JSON-serializable. A scene instance must still be pristine
when its render generation starts. Constructor kwargs are accepted when the
first argument is a Scene class.

For imperative construction, use `paired_render_session(scene, movie_path,
image_path, ...)` as a context manager around `add`, `play` and `wait`. The context
does not invoke `construct`; `session.result` becomes available on successful
exit. Successful `finish()` is idempotent; `abort()` cancels an active session.
The session is confined to its creating thread, like its native primary owner.

## Existing SceneFileWriter preferences

`scene.render()` and `scene.render_session()` now honor the combination
`write_to_movie=True, save_last_frame=True` when neither destination nor format
is explicitly supplied. The normal writer paths are used for both outputs.
With `subdivide_output=True`, the primary is the existing per-call clip collection
and one final PNG accompanies it. An explicit ordinary destination or format
continues to select a single output; use the paired functions to select two
explicitly. No console option is changed by this API.

## Failure and publication semantics

The PNG is captured, encoded and prepared privately **before** primary
finalization, and published **after** the primary finishes successfully. Failed
construction, failed capture, an existing still, or failed primary finalization
cancels the unpublished PNG. Existing files and dangling symlink destinations
are refused; movie and still destinations cannot overlap. Caller paths are
made absolute before entering the generation so later `chdir` does not redirect
publication.

Each artifact is atomic and create-only, but two arbitrary filesystem paths are
**not one transaction**. A concurrent writer can claim the PNG path after
preparation. In that case the movie remains published and the original exception
has `render_pair_result`: `completed=False`, the completed primary receipt, and
no still receipt. A failed subdivided render similarly retains earlier completed
clips. Artifacts are never destructively rolled back or silently overwritten.
Missing receipts after reported publication are listed separately rather than
represented as successful outputs.

The observational final PNG uses the standard fast-CPU camera path. The paired
workflow refuses reproducible/certified implicit rendering; it never inherits a
certification claim from a different artifact. A thread-invariance test is not
cross-platform certification. `animation_range` applies to the primary's existing
scene execution semantics; the still observes the state that execution reaches.

## Publish an existing immutable capture

```python
snapshot = scene.camera.capture_snapshot(*scene.mobjects)
path, size, digest = snapshot.save_png("outputs/still.png", threads=4)
```

`scene.camera.save_png(...)` publishes its last captured frame without recapture.
`scene.file_writer.save_final_image(snapshot)` uses its configured image path and
accepts a native capture, not an arbitrary array or Pillow image. For coordinated
publication, `snapshot.prepare_png(...)` returns an owner with `commit()` and
`abort()`; aborting or dropping an unpublished preparation removes only its
private native file. A committed preparation returns the same receipt on repeated
commit and never deletes the published image on abort.
