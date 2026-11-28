# Python vector output

`Scene.render("diagram.svg")`, `Scene.render_session("diagram.svg")`, and
`fmn-python --format svg` export the final native scene as an SVG document.
Text and mathematics are native glyph outlines: the SVG does not need fonts
installed in its viewer, invoke a typesetter, or contain a raster screenshot.

```python
from manimlib import Scene, Square, Tex, RIGHT, UP

class Diagram(Scene):
    def construct(self):
        box = Square(fill_opacity=1)
        self.add(box, Tex(r"x^2 + y^2").shift(UP))
        self.play(box.animate.shift(RIGHT), run_time=1)

result = Diagram().render("diagram.svg", resolution=(1280, 720))
```

The console uses the same native generation owner and supports explicit scene
names and `--write_all`. With multiple names, the output directory contains one
`NAME.svg` per selected scene; selectors and destinations are checked before
rendering. `--transparent` omits the background rather than rasterizing alpha.

```sh
fmn-python lesson.py Diagram --format svg --output diagram.svg
fmn-python --robot lesson.py First Second --format svg --video_dir diagrams
fmn-python lesson.py --write_all --format svg --transparent --video_dir diagrams
```

## What the document represents

SVG is **final-state vector output**, not an animated SVG or a frame sequence.
The native skip/final-state timeline runs the scene to its semantic endpoint,
then serializes its final quadratic paths. Camera pan, zoom, rotation and
fixed-in-frame attachment use the actual native camera. Families retain
native painter order and shared-object placements. `stroke_behind` emits two
adjacent vector paint passes in the appropriate order. Camera backgrounds are
ordinary vector rectangles; fully transparent backgrounds add no geometry.

Flat fill/stroke colors, native text/TeX outlines, and hard `DotCloud`/`TrueDot`
discs are supported. Each visible dot becomes its own filled quadratic path,
preserving its individual radius, color, opacity and place in painter order.
Zero-radius and fully transparent dots add no geometry. Point transforms move
dot centers; radius follows the native radius data, so a nonuniform stretch does
not invent a wider disc. The same support is available through the native Rust
`render_svg_with_fs` and `render_svg_camera_with_fs` entry points.

The dot silhouette uses the native camera-facing billboard and its full
projective ellipse, including ordinary off-center perspective, camera pan/zoom,
and fractional fixed-frame attachment. It starts with BN-09's 16 quadratic
components per full ellipse, doubling when necessary to keep curve error within
0.05 output pixel. Rounding into native point records is checked against another
0.05 pixel bound. This conversion preserves the silhouette within 0.1 pixel; the
SVG viewer supplies edge antialiasing. It does not embed raster pixels.

Tilted cameras and paths with points at different depths can produce rational
quadratic curves in screen space. The exporter subdivides these into SVG
quadratics until the projected path fits within 0.1 output pixel, including
point-record rounding. Straight polygon and plot edges retain a single
quadratic when their projected coordinates fit the same tolerance. Authored
contour breaks stay separate, including the
holes in glyph outlines. Subdivision consumes the expanded point budget.

Vector paths crossing the camera's depth range use native clipping before
projection. Clipped fills close along the cut. Strokes follow the surviving
parts of the original curves, so the SVG has no artificial stroke across a
cut. World-space user clip planes also work on fill-only paths.

The route refuses image/texture/surface primitives, radial dot glow (including
`GlowDot`/`GlowDots`), lighting, depth tests, per-vertex vector paint and
variable-width strokes. User clip planes on strokes or dots and depth-clipped
dot discs require the PNG renderer. Curves exceeding
the subdivision, point-record precision or geometry limits refuse publication.

Stroke widths follow the existing native SVG export convention (SVG pixel
widths). The SVG viewer owns its rasterization and compositing. This is not a
promise to match Lumen antialiasing, curve-distance strokes, or linear-light
blending pixel for pixel. Python SVG output is standard-mode, not a certified
C1-C10 render; `--reproducible` remains an explicit refusal.

## Publication and limits

One completed generation publishes one document and a `RenderResult` with
`format="svg"`, `engine="native-svg"`, `frame_count=1`, byte length and SHA-256.
The destination must not exist. Reel's atomic create-new capability arbitrates
concurrent writers at publication, so an earlier artifact is never overwritten.
An exception, unsupported scene, invalid geometry or exceeded budget publishes
nothing. The source scene's geometry is never replaced by projected coordinates:
projection is applied to a private native copy-on-write snapshot.

The vector route admits at most 32,768 projected shapes, 262,144 source records
and 262,144 expanded vector point records, and 64 MiB of encoded SVG, with
dimensions 1–16,384 and thread budgets 1–96. Dot expansion and repeated shared
placements count toward those limits: an ordinary visible dot uses one shape
and 33 point records before any screen-error refinement. Thread count does not
select an alternative SVG serializer.
Video encoder, pixel-format and ffmpeg options do not apply to SVG. The existing
`-s` switch specifically requests a final PNG; use `--format svg` on its own for
a vector still.
