# Native in-memory images and live textures

`ImageMobject`, `TexturedSurface` and `TexturedGeometry` accept in-memory raster
inputs. They use the same native image resources, sampling, lighting, snapshots
and output pipeline as file-backed images; no temporary input files are needed.

## Construct and update an image

```python
import numpy as np
from manimlib import ImageMobject

rgba = np.zeros((64, 128, 4), dtype=np.uint8)
rgba[..., 0] = 255
rgba[..., 3] = 255
image = ImageMobject.from_pixel_array(rgba, height=4)
# ImageMobject(rgba, height=4) is equivalent.

image.rotate(.2).shift((1, 0, 0))
rgba[..., 0] = 0
rgba[..., 2] = 255
image.set_pixel_array(rgba)  # Preserve the transformed quad and its identity.

owned_rgba = image.get_pixel_array()
```

Supported array layouts are `(H, W)` and `(H, W, C)` with one, two, three or four
channels: grayscale, grayscale plus alpha, RGB or RGBA. Samples must be `uint8`.
Normalize floating-point or wider integer data explicitly; the API never guesses
a scaling law or silently wraps integers. Row zero is the top row. Reversed,
strided and transposed arrays are copied in that logical order.

Input arrays are not retained. `get_pixel_array()` returns a writable, detached
RGBA8 copy; modifying it does not alter the object. Call `set_pixel_array()` to
publish another immutable resource. Pixel dimensions may change, but replacing
the raster does not resize the existing scene-space quad or reset its UVs,
per-vertex opacity, lighting, placement, children or updaters. New constructors
use the raster's aspect ratio and the requested height. `pixel_width` and
`pixel_height` read the native resource and are read-only, including after
`become`, `restore` and pickle restoration; assigning them does not resize pixels.

`ImageMobject.from_bytes(encoded)` accepts PNG/JPEG bytes without filesystem
access. The ordinary constructor and `set_image(source)` accept those bytes,
arrays or existing local image paths. Decode and array validation finish before
pixel publication. The input boundary rejects empty or over-16,777,216-pixel
rasters; encoded data is bounded to 64 MiB, and decode dimensions are checked
before decompression/allocation. Bytes-like values are encoded images, not raw
unshaped pixel arrays.

## Light/dark materials and indexed meshes

```python
from manimlib import Surface, TexturedSurface

surface = Surface(resolution=(24, 32))
material = TexturedSurface(surface, rgba, dark_rgba)
material.set_pixel_array(new_light_rgba, dark_pixels=new_dark_rgba)
material.set_textures("light.png", "dark.png")
owned_dark_rgba = material.get_pixel_array(dark=True)
```

Both sides are prepared before a single native resource replacement; an invalid
dark input cannot publish a new light side. Omitting the dark side removes the
pair. Light/dark raster dimensions need not match because each side samples its
own normalized UV domain. An absent dark side raises on `get_pixel_array(dark=True)`.
`TexturedGeometry(geometry, rgba)` supports the same updates over its existing
vertices, triangle faces and `geometry.visual.uv` protocol. No mesh is regenerated
just to replace its material. Same-file light/dark paths retain the existing
single-texture behavior.

## Animate pixels with the ordinary scene clock

```python
image.add_updater(lambda mob: mob.set_pixel_array(make_current_pixels()))
scene.add(image)
scene.wait(1)
```

Raster-to-raster morphing is not implemented by the generic record-field
animation. `.animate.set_image(...)`, `.animate.set_pixel_array(...)` and
`.animate.set_textures(...)` explicitly refuse instead of silently losing the
new pixels. Ordinary image placement/opacity animations remain available.
Use the normal updater path above to publish a sequence of generated rasters.

The callback is ordinary authored Python. There is no image worker thread,
independent clock or hidden video player. The immutable resource is frozen into
each native frame job, so later pixel updates cannot rewrite queued frames.
`demo/python/live_raster.py` includes a complete tracker-driven example.

Copies, pickles, camera snapshots and scene checkpoints retain the old resource.
`looks_identical`, checkpoint equality, mirror reuse and undo/redo compare native
materials as well as geometry, including dark-only edits and dimensions. An
unchanged pixel payload can reuse a checkpoint mirror; a changed material cannot
be mistaken for unchanged records. No caller's array becomes a writable alias
of a saved image.

This is standard raster authoring, not an assertion of complete certified input
closure for arbitrary Python computations or external arrays. Conversion may run
an authored `__array__`/path hook with normal host authority. Same-object reentry
through these setters is rejected; unrelated authored side effects are not
rolled back. Pixels copied from concurrently modified caller storage are the
caller's synchronization responsibility. Retained frames/checkpoints consume
memory for distinct raster generations; the per-raster admission limit is not a
global scene-history memory limit.

Acceptance suites are `raster_authoring.py`, `raster_textures.py` and
`raster_frames.py` under `crates/fmn-python/tests`. They exercise actual native
resources, geometry, checkpoints, captures and ordered PNG frame publication.
