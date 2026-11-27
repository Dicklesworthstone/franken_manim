# Authored vector fields and native taper profiles

`VectorField` and `TimeVaryingVectorField` initialize authored subclasses through
`VMobject`: data, point, uniform and color hooks run on the actual receiver.
The constructor prepares sample coordinates and width metadata first, then
applies the constructor stroke and calls the public `update_vectors` operation.
Custom record fields, children and subclass identities survive construction,
copying and live resampling. The exported classes and qualified aliases stay
unchanged.

### Fields authored in scene coordinates

Omit `coordinate_system` to use the world-space callback used by older
ManimGL optics scenes. The callback takes an array of XYZ sample positions
and returns one XYZ displacement per row. For example:

```python
import numpy as np
from manimlib import TimeVaryingVectorField

wave = TimeVaryingVectorField(
    lambda points, t: np.column_stack((
        np.zeros(len(points)), np.sin(points[:, 0] - t), np.zeros(len(points)),
    )),
    width=8, height=0, x_density=4,
)
```

The default grid spans 14 by 8 scene units, centered at the origin, with
two samples per unit along each active axis. Set `depth` for a 3D grid.
`sample_points` or an overridden `get_sample_points` supplies custom bases.
`set_sample_points(points).update_vectors()` resamples a live field. The
same update also picks up a newly assigned or edited `sample_points` array.
Supply at least two positions within the shared 65,536-sample limit.

`max_vect_len` is measured in scene units and defaults to the reciprocal
of the mean density across active axes. Positive infinity draws full-length
vectors. `min_drawn_norm` defaults to 0.01; smaller or equal magnitudes have
no visible arrow. Both `norm_to_rgb_func` and `norm_to_opacity_func` receive
the original magnitudes, repeated for the arrow's stroke records. Passing
a coordinate system selects the chart-based API described below.

```python
from manimlib import VectorField, UP

class LiftedField(VectorField):
    def update_vectors(self):
        super().update_vectors()
        self.shift(UP)
        return self

class WiderHeads(VectorField):
    def init_base_stroke_width_array(self, count):
        super().init_base_stroke_width_array(count)
        self.base_stroke_width_array[4::8] *= 2
        self.base_stroke_width_array[5::8] *= 2
```

An `init_points` override participates in initialization, but the final vector
update normally replaces the root's preliminary points, as in the Reference.
Override `update_vectors` to change final displayed geometry, or
`update_sample_points` to customize where native arrows originate. Decorations
added as children and additional record columns are not replaced by that update.

When a subclass needs an inferred magnitude range, the constructor samples the
field before base hooks, then samples again in its final public update. With an
explicit range only the latter evaluation is needed. The entirely unchanged
concrete class keeps its established single callback evaluation: its prepared
outputs enter the same update/publication implementation without another call.
A later monkey patch to a relevant hook disables that shortcut. Classification
does not probe authored callbacks or descriptors.

## Tapering and short vectors

Atlas still computes scene-space vectors, bounded lengths, arrow tips and
short-vector width attenuation. Construction now retains that attenuation:
zero vectors have zero stroke width, and short arrows are not overwritten by
an unattenuated constructor style pass.

`init_base_stroke_width_array` supplies one real nonnegative scalar for each
of the native `8 * sample_count - 1` points. Every update freezes and validates
that profile before constructing or publishing replacement geometry. An authored
profile multiplies the native per-arrow shaft width, which already includes
short-vector attenuation. This uses the native result rather than recomputing
tanh, arclength or tip geometry in Python. An unchanged profile preserves the
exact native width bytes; custom products use the published native shaft value
and are checked before narrowing into the native records.

Changing a width profile changes paint, not arrow endpoint or head-base geometry.
Use the existing tip geometry parameters when the head's length must also change.
Zero-tip settings and zero-length vectors follow the existing native rules.

Malformed, nonfinite, negative, wrong-sized or overflowing profiles fail without
publishing a partial geometry/paint update. A paint callback that changes the
already prepared profile or tip ratio is also refused. The existing scene,
family, coordinate and record checks remain in force. These guards are not a
rollback of arbitrary authored side effects; a custom publication method such
as `set_points` still owns its own effects and exceptions.

Same-size native record views stay live. Resampling retains the existing
copy-on-resize rules and preserves unrelated custom columns and live paint.
Construction is not reentrant, and scene-bound objects must be updated rather
than reconstructed. Native object initialization remains one-shot after a
partially completed base initialization.

## Acceptance

`vector_field_lifecycle.py` and `vector_field_profiles.py` are registered in the
installed-portal gate. They test real native records, callback dispatch, copied
fields, time-varying updates, short/zero vectors, failed updates, and changing
PNG frames against independent fields/native geometry and literal record edits
at one and four workers. Retained-wheel development runs are not fresh-tree
wheel builds, full-workspace passes or cross-platform certification.
