# Point-cloud authoring and live glow

`PMobject`, `PGroup`, `DotCloud`, `TrueDot`, `GlowDots`, and `GlowDot` use the
shared initialization hooks. A cloud subclass can declare extra float32 record
fields and author points, uniforms, colors, children, and saved state without
being replaced by a stock native object. The native initializer assigns the
dot-cloud renderer identity before hooks, including for copies made by a hook.

```python
class Samples(DotCloud):
    data_dtype = [*DotCloud.data_dtype, ("temperature", np.float32, (1,))]

    def init_points(self):
        self.set_points([[-1, 0, 0], [0, 1, 0], [1, 0, 0]])
        self.data["temperature"][:] = [[10], [20], [30]]

samples = Samples(points=None, radius=0.2, color=BLUE)
samples.uniforms["glow_factor"] = 2.0
```

`points=None` retains hook-authored points. Omitting `points` uses the Reference's
one-point origin default. Explicit points replace hook-authored points after
initialization and radius assignment; an empty sequence produces an empty cloud.
`PGroup` retains hook-added children before its supplied point-cloud members.

## Packed records and one native material

`DotCloud`, `TrueDot`, `GlowDots`, and `GlowDot` use the Reference's exact packed
32-byte record dtype. The exported NumPy array is a writable view of the actual
native records, with no extra material lane:

| Field | NumPy type and shape | Byte offset |
| --- | --- | --- |
| `point` | `float32`, `(3,)` | 0 |
| `radius` | `float32`, `(1,)` | 12 |
| `rgba` | `float32`, `(4,)` | 16 |

`set_data` accepts independently created arrays with this dtype. Subclasses keep
their declared extra float32 fields and resulting stride. Even an authored
field named `glow_factor` is ordinary per-point data: copying, `set_data`, and
record interpolation preserve it without giving it control of the renderer.

Glow is a native float64 uniform shared by all points in a cloud. Writes through
`set_glow_factor`, `set_uniform`, `set_uniforms`, or the live `uniforms` mapping
update that scalar and invalidate retained rendering without changing record
bytes. Zero gives a hard disc; `GlowDot` and `GlowDots` default to a radial
exponent of `2.0`. An empty cloud retains its material independently of its
records, so appends, copies, and deep copies keep the current glow.

Native and callback-driven `Transform` animations interpolate the same scalar
uniform. Callback data/uniform locks and authored paths remain on the shared
interpolation protocol. Copies, `become`, saved-state restoration, Python
pickles, and scene checkpoints carry the native material with the geometry.

Scalar glow must be finite and non-negative; it retains float64 precision and
does not require float32 representability. Invalid values refuse before changing
the material. Other mobject types and unrelated custom uniforms retain their
existing behavior.

## Durable material state

Dot-cloud snapshots use FMNA 1.9, which stores the scalar independently of the
record schema. A legacy snapshot with a constant per-point glow lane promotes
that value exactly to the native uniform while preserving its existing record
columns. An empty legacy cloud uses its retained default record when available.
A legacy cloud with heterogeneous glow values receives an explicit migration
error because one shared scalar cannot preserve those different materials.

## Validation boundary

`native_point_cloud_lifecycle.py` covers the new Rust initialization/validation
boundary and its public constructors. It requires a rebuilt native extension;
protocol probes against an older extension cannot prove primitive publication.
`native_point_cloud_materials.py` exercises actual records and Y4M output,
including retained-frame edits, independent constructor controls, and 1/4/16
worker comparisons. Both are registered in the embedded and installed gates.
The named point-cloud-uniform gauntlet also checks packed dtype/byte parity,
native material lifecycle, native/callback animation agreement, and rendered
output after live uniform edits.
