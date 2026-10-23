# BN-25 — A camera at any resolution keeps square pixels

**Status:** Draft (W5/W10, fm-fxov). Consumed by Lumen's camera
(`crates/fmn-render/src/camera.rs`), the fmn-python `Camera`, and the Parity
Ledger rows for `Camera.__init__`, `Camera.refresh_uniforms` and
`CameraFrame`.

## What the Reference does

The pinned Reference's frame shape follows the *configured* resolution, not
the camera's. `constants.py:13-21` reads `manim_config.camera.resolution`
(the `-r`/`-l`/`--hd` flags or `custom_config.yml`) and derives
`ASPECT_RATIO`, `FRAME_WIDTH = FRAME_HEIGHT * ASPECT_RATIO` and the default
`FRAME_SHAPE` from it. `Camera.refresh_uniforms` (`camera.py:243-256`) then
projects with the fixed factors `2 / FRAME_WIDTH` and `2 / FRAME_HEIGHT`,
after dividing by `frame.get_scale() = frame height / FRAME_HEIGHT`.
Consequences:

- With the configured resolution, the frame keeps height 8 at any aspect and
  pixels are square: `-r 1080x1920` shows a 4.5 × 8 frame.
- A `Camera(resolution=...)` whose aspect differs from the configured one
  keeps the configured factors, so its image is stretched: a square becomes
  a rectangle. Its frame stays the configured shape.
- Only the frame's height changes what is displayed. A frame stretched to
  another width leaves the image unchanged.

## FrankenManim

Every camera behaves as the Reference does at its configured resolution,
using the camera's own resolution:

- At construction the frame keeps its height and takes the camera's pixel
  aspect. A camera at 96 × 64 has a 12 × 8 frame; at 64 × 64, an 8 × 8 one.
- The projection's x factor is `2 / (FRAME_HEIGHT * width / height)` of the
  camera's pixel shape. Pixels are therefore square at every resolution, and
  the frame shown is exactly `camera.frame`.
- At any 16:9 resolution (1920 × 1080, 1280 × 720, 3840 × 2160 and the
  320 × 180 golden rig) the frame and both factors are bit-identical to the
  Reference's.
- The frame's own width still never enters the projection, as in the
  Reference.
- `Camera.reset_pixel_shape` / Lumen's `set_pixel_shape` still keep the
  frame *width*, as the Reference's `resize_frame_shape()` default does.

Standalone `fmn` already rendered this way on its retained 2D route, which
maps output height to `sizes.frame_height`. Its camera route now agrees.

**What changed, and why it was a defect.** Before fm-fxov, Lumen resized the
frame keeping its *width* and kept the 16:9 x factor at every aspect. The
image matched neither the frame nor the Reference:

- At 96 × 64 a side-2 square at (3, 2) covered columns 59-70, where the
  frame implied 61.5-75.
- At 64 × 64 it was squeezed to 4-5 px wide.
- At 854 × 480 (`-l`) the camera route was 0.08% narrower than both the
  Reference and the retained route, so 2D content jumped by up to a third of
  a pixel when a 3D object switched the route.

## Migration

- Scenes rendered at their configured resolution need no change. They show
  the frame the Reference shows.
- A scene that builds a `Camera` with a resolution of another aspect,
  expecting the stretched image, now gets square pixels and a frame of that
  aspect. The frame height sets the region shown. To keep the full
  `FRAME_WIDTH` across a narrower output, scale the frame:
  `camera.frame.set_height(FRAME_WIDTH * pixel_height / pixel_width)`.
- Not covered here: the module constants `FRAME_WIDTH`, `FRAME_X_RADIUS`
  and `ASPECT_RATIO` stay at their 16:9 values whatever the configured
  resolution, so `to_edge(LEFT)` on a portrait render still uses the 16:9
  frame edge (fm-8azr).

Witnesses:
- `crates/fmn-render/src/camera.rs`:
  `every_output_aspect_projects_its_own_frame_with_square_pixels` and
  `sixteen_by_nine_outputs_keep_the_reference_frame_and_factors_bit_for_bit`.
- `crates/fmn-python/tests/test_ledger_witnesses_geometry_camera.py`:
  `test_camera_frame_and_pixels_at_other_aspects`.
