# BN-04 — Callable color fields on native record schemas

**Status:** Draft. Implemented with native runtime witnesses; not a full certification gate.
**Plan:** §8.2, §10.2, §15 · **Related beads:** fm-sq8, fm-5wq.4.143


`set_color_by_rgb_func` and `set_color_by_rgba_func` now operate on the native
record schema: point and surface records keep their `rgba` column, while
VMobjects receive the same callable colors in `fill_rgba` and `stroke_rgba`.
Previously, these inherited methods raised a missing-`rgba` exception on common
vector shapes such as `Square`, `Circle`, and `Line`. This is a deliberate
correction, not reproduction of the Reference's fixed-column limitation.

The callback still receives each point-bearing family member's live point
array in deterministic family order. Empty members are skipped, shared members
are evaluated once, and `recurse=False` stays local to the receiver. Both a
constant color vector and one row per point are admitted. Values must be real,
finite and f32-representable; values outside the usual zero-to-one color range
are not silently clipped. The RGB method applies its explicit opacity, whose
default remains one.

All callback outputs are frozen and validated before publishing any paint. A
late callback exception or invalid result therefore leaves earlier members'
paint untouched. Reentrant coloring and callback-induced family/geometry edits
refuse the obsolete paint plan. Authored side effects are not rolled back.
Public `set_rgba_array` overrides still receive the final writes and retain
responsibility for their own effects; failures in arbitrary authored setters
are not advertised as rollback-safe.

No shader language is admitted by this correction: `set_color_by_code` and
the GLSL-snippet form of `set_color_by_xyz_func` still give the existing named
capability refusal. Callable fields evaluate on the host, write existing live
record columns, and use the same native fill/stroke renderer and cache.

Evidence: `crates/fmn-python/tests/functional_color.py` exercises real native
record layouts, bound views, mixed families, shared children, override dispatch,
invalid-input recovery and actual rendered stroke pixels. Full fill pixel and
animation witnesses are described in `BN-06-analytic-fill.md`.

## StreamLines magnitude color maps

`StreamLines(..., color_map=callable)` supports authored magnitude coloring
during construction, `init_style()`, and `draw_lines()`. The same callable
protocol is available through `get_rgb_gradient_function` and
`get_vectorized_rgb_gradient_function`. The bundled
`"3b1b_colormap"` continues to use Atlas's native interpolation.

The callback receives a one-dimensional NumPy array of magnitudes normalized
to `magnitude_range` and clipped to `[0, 1]`. It returns one RGB or RGBA row
per value. RGB controls color; `stroke_opacity` controls opacity even when
the callback returns four columns. A constant magnitude range maps to zero,
and an empty input returns an empty RGB table without calling user code.

```python
def speed_colors(alpha):
    return np.column_stack((alpha, 1 - alpha, np.zeros_like(alpha)))

lines = StreamLines(
    field, axes,
    magnitude_range=(0, 2),
    color_map=speed_colors,
    stroke_opacity=0.8,
)
```

Colors must be real, finite and representable in the native f32 record.
Returned RGB arrays are copied before evaluating the next line, so a callback
can safely reuse scratch storage. All lines' colors are validated before
publication: a late callback failure preserves the displayed family and its
paint. `color_by_magnitude=False` uses the uniform stroke without invoking
the color callback. Unknown named palettes still produce a capability refusal.

Coverage is in `test_streamline_animation_protocol.py` and
`streamline_authoring.py` under `crates/fmn-python/tests`. The native
acceptance includes live restyling, redraw, animated-flow cleanup, failure
recovery, and rendered comparisons with independently colored geometry.
