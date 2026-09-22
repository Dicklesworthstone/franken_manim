# BN-20 — Calculus helpers operate in the axes' coordinate system

**Status:** Draft. **Owner:** W7/W10. **Related work:** fm-5wq.4.143.

## Riemann rectangles

`CoordinateSystem.get_riemann_rectangles` now constructs native Rectangle paths
from data-coordinate corners. Rotating, stretching, translating, or tilting an
Axes no longer collapses the rectangles or changes which samples are negative.
Rectangle identity, native records, styles, family ownership and rendering are
preserved; there is no alternative path or rasterization engine.

The integration interval is authoritative. A final partial bin ends at the
requested upper bound; right and center sampling use that bin's actual bounds.
A three-value `x_range` supplies its own step; otherwise `dx` (or the axes'
default step) supplies it. Caller-owned ranges, including NumPy arrays, are not
modified. A zero-length interval yields an empty VGroup.

`show_signed_area=False` leaves the ordinary gradient on negative rectangles;
it does not reflect them above the baseline. With the default `True`, negative
samples receive `negative_color`. Zero-height rectangles are nonnegative.

The Reference at `6199a00d4c1b1127ebe45cb629c3f22538b10e13` instead measures
width and sign using world-space components, extends the sampling range, and
does not act on `show_signed_area`. These are intentional corrections under
the plan's correctness-first contract, not pixel-compatibility promises.

## Migration and bounds

Remove workarounds that unrotate axes, extend the upper bound manually, or
recolor negative bins when signed coloring is disabled. Code that intentionally
needs an overshooting last bin should request that larger interval explicitly.

Ranges must be finite and nondecreasing, with a positive finite step. Requests
are limited to 65,536 rectangles and reject steps that cannot advance the
floating-point coordinate. Sampling mode and style inputs are validated before
evaluating authored graph callbacks. A sample in a geometric graph's missing
interval raises a ValueError instead of inventing a bridge across the gap.
No rollback of effects performed inside authored callbacks is promised.

## Evidence

`crates/fmn-python/tests/live_graphing.py` exercises native transformed geometry,
negative and zero samples, clipped bins, all sampling modes, styles, invalid
requests, disconnected paths and a rendered reference-polygon comparison.
The same Y4M output is checked at one and four threads. These witnesses do not
close the wider compatibility or certified-rendering gates.
