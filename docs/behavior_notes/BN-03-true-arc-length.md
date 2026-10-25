# BN-03 — True arc length under the original names

**Status:** Draft (W2, fm-xci). Consumed by W7 (dashes, tips, tracers)
and Choreo's `MoveAlongPath`.

## What changed

Classic manim's proportion/length layer is chord-distance heuristics —
three mutually inconsistent approximations:

- `get_arc_length` returns a blend of anchor-polyline and full-polygon
  length (`interpolate(inner, outer, 1/3)`), not arc length;
- `point_from_proportion` weights curves by *chord* length and walks the
  curve's parameter linearly within each — parameter speed, not arc
  speed;
- `quick_point_from_proportion` assumes all curves have equal length.

FrankenManim keeps the names and fixes the math (D-05):

- `get_arc_length` returns the actual arc length. A quadratic Bézier's
  speed is the norm of a linear function, so each curve's length is a
  **closed form** (exact to rounding; logarithm via fmn-dmath — no
  quadrature, no tolerance knob). Degenerate cases (lines, interior
  cusps) have exact branches.
- `point_from_proportion` — and therefore `MoveAlongPath`, dashes, tips,
  and tracers — places by **true arc length**: constant-speed motion, as
  every user always assumed it worked. Inversion is Newton on the exact
  partial-length form with a bisection safeguard, fixed iteration count.
- `quick_point_from_proportion` remains, verbatim, as the documented
  fast approximation — a labeled option, never a silent substitute.
- The per-path `ArcLengthTable` is a §10.8 retained artifact keyed by
  the geometry revision only: transforms and style changes never rebuild
  it (`CachedArcLength`, locked by test).

## Migration guidance

Paths animate at slightly different (correct) pacing than the Python
engine. Example: a `MoveAlongPath` over a path made of one short curve
followed by one long curve. In classic manim the mover crosses the short
curve in (roughly) half the run time — chord weighting per curve, then
parameter speed inside it — so its apparent speed visibly jumps at the
seam. Here it moves at constant speed for the whole run: the short curve
takes its fair share of time and no more. Scenes that (usually
inadvertently) relied on the speed jump will look smoother; keyframe
timings tied to "the mover reaches the seam at t/2" shift to the true
arc-length fraction.

`get_arc_length` values change too: the Reference's blend systematically
overestimates curved paths (its outer polygon dominates). Dash counts and
tip placements derived from it move accordingly — by design.

Points sampled with `point_from_proportion` move as well, and a decision
taken on them can flip near its threshold. `_2021/bertrands_paradox.py`
`CompareFirstTwoMethods` builds random chords from `circle.pfp(random())` and
colors those longer than the inscribed triangle's side. One chord in a
thousand changes color (corpus differential row
`bn03-chord-length-threshold`).

Tip attachment also follows the actual curve. Classic manim aims a curved
tip from the final control handle and then shortens the shaft by applying
`put_start_and_end_on` to the whole path. The head can therefore follow a
component-local approximation while the shaft is chord-rescaled away from
its original curve. FrankenManim aims the head along the true terminal
quadratic derivative and cuts the shaft back by true arc length, preserving
the remaining curve. A cut through a circular-arc component is rebuilt on
that circle, and the components the cut removes are dropped rather than
kept as zero-length points, so a tipped arc keeps its own centre:
`Arc(angle=2).add_tip(at_start=True).get_arc_center()` is the origin, where
the Reference's rescaled shaft reports (0.112, 0.175)
(fm-arc-start-tip-degenerate-component-izct). Straight shafts are unchanged. Scenes that deliberately
compensated for the classic curved-tip tilt or rescaling should remove that
compensation; custom `ArrowTip` subclasses keep their Python identity and
style while the native placement algebra transforms their point run.

## StreamLines honour `arc_len`, `n_samples_per_line` and `cutoff_norm`

The Reference's `StreamLines.draw_lines` (`vector_field.py`) integrates each
seed for `solution_time` (3) with `ode_solution_points` at `dt` (0.05) and
smooths through all 60 solver samples (`t_eval = np.arange(0, 3, 0.05)`). It stores `arc_len`, `max_time_steps`,
`n_samples_per_line` and `cutoff_norm` but never reads them (its comment:
`TODO, account for arc length somehow?`). A line's drawn length is therefore
however far the field carries it in 3 time units: long in a fast field,
short in a slow one.

**FrankenManim:** each line still integrates for `solution_time`, through
fsci's adaptive RK45 with dense output. Then the four declared parameters do
what their names say:
- the line is cut at true arc length `arc_len` (default 3);
- it is resampled to `n_samples_per_line` (default 10) evenly spaced samples
  and smoothed through those;
- it stops at the first sample whose field norm exceeds `cutoff_norm`;
- at most `max_time_steps` dense samples are taken.

In a field whose speed stays near 1 the lines look as they do in the
Reference. In a fast field they are capped at 3 units rather than running
off the frame. Each line has fewer, evenly spaced points (19 per line against
the Reference's 119), which `AnimatedStreamLines` and `VShowPassingFlash`
then sweep at constant speed.

**Migration:** to draw longer lines in a fast field, raise `arc_len`. For
more points per line, raise `n_samples_per_line`.

## Straight segments remain whole under point reversal

A quadratic whose control point equals its end anchor is a valid straight
segment. Reversing its rows naively puts that handle on its start anchor,
which the shared-anchor format reserves for a subpath break. The resulting
open runs can acquire extra closing chords when filled.

FrankenManim normalizes that straight handle before reversal. Stage family
reversal uses the same geometry primitive, then preserves the complete
record-row reversal for widths, colours and other fields. When float32
record storage cannot represent a midpoint without turning it into a break,
the handle uses the representable end anchor of the reversed straight
segment. Intentional subpath breaks remain intact. The existing
`recurse=false` pre-repair scope is unchanged.

The segment's geometry and true arc length are unchanged; its raw control
point may differ. Code inspecting control arrays should accept the
equivalent straight representation rather than require a split contour.
The Stage contour/record tests and
`lifecycle.family_reversal_end_handle.v1` cover this case, including a real
rendered raw-reversal negative.

## Evidence

- `crates/fmn-geom/src/arclength.rs`; `crates/fmn-geom/tests/arclength.rs`
  (closed form vs a 20k-interval Simpson oracle at 1e-9; the analytic
  parabola √2 + asinh 1; exact line/cusp/point branches; inverse
  round-trip; constant-speed metamorphic test on an uneven path — with a
  demonstration that the quick heuristic diverges there; revision-keyed
  cache tests).
- Reference: `VMobject.get_arc_length` / `point_from_proportion` /
  `quick_point_from_proportion` at the pinned commit.
- `crates/fmn-library/src/fields.rs` (`resample_even_arc`, the StreamLines
  builder) and `crates/fmn-python/tests/live_streamlines.py`.
- `crates/fmn-library/src/tip.rs` (true terminal tangents, reusable built-tip
  placement, true-length trimming, straight/curved/degenerate tests).
- `crates/fmn-python/tests/bridge.py` (authored `TipableVMobject` MRO and full
  detached/scene-bound lifecycle with curved placement and family identity).
