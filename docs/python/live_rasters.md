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

### Other encoded image formats

GIF, BMP, WebP and TIFF inputs use the optional, governed ffmpeg decoder when
you supply its executable explicitly. The bytes select a recognized image
container; a filename extension never selects a decoder. An animated image
contributes its first frame.

```python
image = ImageMobject("diagram.webp", ffmpeg_bin="/usr/bin/ffmpeg")
image.set_image("replacement.tiff", ffmpeg_bin="/usr/bin/ffmpeg")
from_memory = ImageMobject.from_bytes(gif_bytes, ffmpeg_bin="/usr/bin/ffmpeg")

scene.play(image.animate.set_image("next.bmp", ffmpeg_bin="/usr/bin/ffmpeg"))
```

The same keyword is accepted by `TexturedSurface`, `TexturedGeometry`,
`set_textures` and `RasterTransition`. PNG/JPEG always use native decoding,
including when the supplied executable is missing. A corrupt native image
does not fall through to ffmpeg. Unsupported byte signatures and missing
capabilities raise before the current image material is replaced.

External decoding uses the native argv-only media boundary, with bounded input,
output, pixels, time and process resources. No URL, playlist or implicit network
lookup is accepted. The decoded output retains its dimensions and top-row-first
RGBA orientation; it is not resized to fit an output budget.

### Input receipts and reproducible rendering

`RenderResult.image_inputs` records each consumed image input's container,
dimensions, source SHA-256, normalized pixel SHA-256, raw RGBA SHA-256, source
path when applicable, decoder identity and optional executable SHA-256. An
image's ffmpeg invocation is also included in `RenderResult.ffmpeg_invocations`,
including for PNG and PNG-sequence outputs. Copies share one decode receipt;
separately executed decodes retain their separate invocation receipts.

Receipts follow native immutable pixel storage through copies, deep copies,
frame snapshots and intermediate raster transitions. Scene checkpoints also
retain their receipts when their canonical bytes reconstruct pixel resources
on undo or restore. Checkpoint receipt metadata is bounded to 4096 checkpoint
digests and 65,536 source associations per live scene; it retains no pixel
buffers. Byte-identical checkpoints retain the union of their observed input
origins. Receipt history uses metadata-only job identities, so replacing live
pixels does not keep the retired pixel allocations alive. Explicit RGBA inputs
with equal dimensions, transfer encoding and pixels share one content receipt.

Certified rendering accepts native PNG/JPEG and explicit RGBA inputs. It
refuses an externally decoded live image before frame publication, even when
the image was constructed before the render or restored from a checkpoint.
During a certified generation, external image decoding refuses before looking
up the executable. Source and normalized image hashes enter the certified
input closure, including for bytes prepared before the generation.

Standalone durable serialization, including a mobject pickle, stores baked
pixels. Those reconstructed resources are reported as `embedded-rgba8`, with
their actual pixel hashes and no original codec or executable claim. Supplying
`get_pixel_array()` as a new input is the same explicit baking boundary.

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

Raster transitions now use the shared Animation lifecycle and a native texture
interpolation plan. The image/material setters have dedicated `.animate`
overrides rather than silently leaving pixels out of a record-field transform:

```python
scene.play(image.animate(run_time=1).set_pixel_array(next_rgba))
scene.play(material.animate(run_time=1).set_textures(next_light, next_dark))
```

For explicit compositions or independent animation settings:

```python
from fmn_python.raster_animation import RasterTransition

scene.play(RasterTransition(image, next_rgba, run_time=1, rate_func=linear))
scene.play(Succession(
    RasterTransition(image, blue_rgba, run_time=1),
    RasterTransition(image, red_rgba, run_time=1),
))
```

PNG `gAMA` metadata is carried into native image resources; an `sRGB` chunk
takes precedence and untagged images remain sRGB. Raw decoded bytes are not
changed or pre-corrected by this metadata handoff.

Target inputs are copied/decoded once at animation construction. Each starting
material is captured at `begin()`, so a later Succession member begins from the
preceding member's actual result, not stale construction-time pixels. Input
paths are informational after decoding and are not reopened during playback.

Lumen's existing native sampler decodes each endpoint's transfer function and
samples both on a common UV lattice. The lattice uses the larger width and
height of each corresponding side; the output product and total decoded
endpoint storage are checked before allocating. Interpolation is in
**premultiplied linear light**, then encoded as an immutable sRGB RGBA8 image.
Opaque black/white halfway is 188, not 128. Transparent hidden RGB does not leak
into visible edges. The exact endpoints restore their original resources,
including transfer metadata, dimensions, alpha and hidden RGB.

Light and dark sides are sampled and published as one material revision. A
missing dark side uses that endpoint's light image during the transition, then
is truly absent again at its endpoint. Geometry, UV coordinates, placement and
per-vertex opacity do not interpolate as a side effect of changing pixels.
Native placement and a `RasterTransition` may share a `Scene.play` invocation.
Method chaining on a raster builder still follows the existing override rule
and refuses; this feature does not change other custom animation overrides.

`rate_func`, `time_span`, `final_alpha_value`, `remover`, suspension and composition
use the existing Animation protocol. A there-and-back rate ends on the starting
material. Overshooting eased values clamp to endpoint coverage; nonfinite values
raise before publishing an image. Failed/cancelled execution releases acquired
animation flags and decoded plans, retaining the last published scene state;
it does not rewind authored side effects or scene time. Owned output sessions
retain their existing cancellation/no-partial-publication rules.

The per-transition decoded endpoint ceiling is 256 MiB (counting explicit
light/dark endpoints conservatively), and each intermediate side has the
existing 16,777,216-pixel limit. Endpoint samplers must agree. These are per-plan
limits, not a global history or process memory budget. Raster interpolation is
standard image animation, not certification of arbitrary Python inputs. Generic
`Transform` also interpolates compatible native image materials alongside its
record/path fields; see `raster_transforms.md`. Dedicated setter overrides or
`RasterTransition` change only the image axis without moving geometry.

The updater path above remains appropriate for externally generated sequences;
`demo/python/raster_transition.py` demonstrates native interpolation instead.

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

Acceptance suites are `raster_authoring.py`, `raster_textures.py`,
`raster_frames.py`, `raster_transition_kernel.py` and `raster_animation.py` under `crates/fmn-python/tests`. They exercise actual native
resources, geometry, checkpoints, captures and ordered PNG frame publication.
