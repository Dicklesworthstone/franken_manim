# BN-09 — One arc-density rule

**Status:** Final. Drafted in W2 (fm-e3f) with the path model; applied
across the geometry lineage in W7 (fm-oab), where `Arc`, `Circle`, `Dot`,
`Ellipse`, the sectors, `ArcBetweenPoints`, and every `path_arc` line are
built through the one rule.

## What changed

Classic manim has **three mutually inconsistent conventions** for how many
quadratic components trace an arc of subtended angle θ:

| Site | Rule | Full circle | Quarter circle |
|---|---|---|---|
| `Arc.__init__` (and everything built on it) | `int(15·|θ|/TAU) + 1` | 16 | 4 |
| `VMobject.add_arc_to` | `ceil(8·|θ|/TAU)` | 8 | 2 |
| `quadratic_bezier_points_for_arc` default | fixed `8` | 8 | 8 |

The same quarter-arc is 4 components when drawn as an `Arc`, 2 when appended
with `add_arc_to`, and 8 when produced by the bare helper. Curve quality —
and point counts, which user code indexes — depend on which code path
happened to build the arc.

FrankenManim uses **one rule everywhere** (`fmn_geom::bezier::arc_n_components`):

```text
n(θ) = max(1, ceil(16·|θ|/TAU))
```

16 components for a full circle — the Reference's `Arc`/`Circle` convention,
its finest — and agreement with that convention at every common angle
(quarter → 4, half → 8, three-quarter → 12). The rule is monotone in |θ|,
never coarser than any of the three Reference conventions, and gives at
least one component to any arc, however small.

An explicit `n_components` argument is honored verbatim at every call site,
exactly as in the Reference.

## Migration guidance

- Arcs built through `Arc`-family constructors keep their Reference point
  counts at the standard angles; code indexing those points is unaffected.
- Paths built with `add_arc_to(..., n_components=None)` gain resolution
  (16/TAU density instead of 8/TAU): point counts along such paths grow.
  Pass an explicit `n_components` to reproduce old counts.
- Anything that consumed `quadratic_bezier_points_for_arc`'s fixed default
  of 8 regardless of angle now scales with the angle instead.
- An arced `Arrow` scales its point count with its stem angle, where the
  Reference's arrow always carries 43 points. A rebuild at a new length
  therefore changes the count too: `GrowArrow`'s zero-scale start copy of a
  `path_arc=PI` arrow has fewer points than the arrow. A `Transform` whose
  starting mobject no longer aligns with its mobject re-aligns the start, the
  mobject and its target copy before the first interpolation, so these
  animations run as in the Reference. Pointwise results are unchanged
  whenever the start already aligned.
- A non-finite implicit arc angle (`path_arc=NaN` or infinity) is rejected
  before geometry or the stored angle changes. The Reference can publish the
  invalid angle and clear the existing points before failing during component
  selection. Validate computed angles or pass a finite value; FrankenManim
  returns a typed error and leaves the prior line intact.

## Worked examples

| Construction | Reference | FrankenManim |
|---|---|---|
| `Circle()` | 16 components, 33 points | **same** |
| `Arc(angle=TAU/4)` | 4 components, 9 points | **same** |
| `Arc(angle=PI)` | 8, 17 | **same** |
| `Arc(angle=1.7)` | 5, 11 | **same** |
| `Arc(angle=0.13·TAU)` | `int(1.95)+1 = 2` | `ceil(2.08) = 3` — finer |
| `path.add_arc_to(p, TAU/4)` | `ceil(2) = 2` | `4` — matches `Arc` |
| `quadratic_bezier_points_for_arc(TAU/4)` | 8, regardless of angle | `4` — scales with angle |
| `Arc(angle=TAU, n_components=3)` | 3 | **3** — explicit wins |

The first four rows are the common cases, and they are unchanged: a
`Circle`'s 33 points are still 33 points, so code that indexes them is
unaffected. The rows that change are the ones where the Reference was
*inconsistent with itself*.

```python
# Before and after, in FrankenManim:
Circle().get_points().shape          # (33, 3) — as in the Reference
Arc(angle=TAU / 4).get_points()      # 9 points — as in the Reference

path = VMobject()
path.start_new_path(ORIGIN)
path.add_arc_to(RIGHT, TAU / 4)      # 9 points now, 5 in the Reference
path.add_arc_to(UP, TAU / 4, n_components=2)   # 5 points, either engine
```

## Curve counts after Transform alignment

Constructor point counts do not promise identical point indices after a
`Transform` aligns shapes with different curve counts. Alignment subdivides
the shorter path, greedily choosing its longest anchor chord and keeping the
first index when computed priorities are exactly equal. Subdivision preserves
the traced quadratic path, but changes where its anchors and handles occur.

The plan's §6.1 requires semantic geometry in f64. This includes chord
differences, norms and subdivision priorities, even when the input came from
f32 records. The pinned Reference's `get_norm` instead returns a float32 for
float32 inputs under the measured NumPy 2.5.2 environment. Nearly equal polygon
edges can therefore receive different priorities. The first-index tie rule
agrees; the numerical values being compared do not.

For example, aligning the Reference's nine-gon points to sixteen curves adds
seven curves. Supplying those exact same f32 points to both public insertion
methods gives these insertion counts by original edge:

```text
Reference:    1 1 1 1 1 0 1 0 1
FrankenManim: 1 0 1 1 1 1 1 0 1
```

The native choices match the independently computed f64 chord priorities;
the Reference choices match its float32 priorities. Checking every output
control against its original straight edge bounds both complete quadratic
paths within 2.6e-8 scene units; all projections remain inside their edges.
This explains the point-distribution difference reached by
`Transform(Circle(radius=1.4), RegularPolygon(n=9))`. The minimized
Ellipse-to-Arrow transform also retains a point-array difference; the
two-program reproduction is not a full seed-34 campaign qualification.

**Migration:** read current aligned points rather than assuming the
Reference's post-Transform point indices. Use explicit authored subdivision
when particular correspondence is part of the scene's intent. Do not round
native geometry down to float32 to reproduce the Reference's priority noise.
Arc-specific getters on an object that has become a polygon require their own
analysis; this point-distribution explanation does not waive their observed
differences.

## Evidence

- `crates/fmn-geom/src/bezier.rs` (`arc_n_components`, unit tests).
- `crates/fmn-library/src/arc.rs` (the lineage; `Arc::component_count`).
- `crates/fmn-library/tests/geometry_parity.rs`
  (`the_arc_density_rule_is_ours_and_never_coarser`, which asserts the
  rule's value *and* that it is never coarser than the Reference's, over
  the fixture corpus).
- `crates/fmn-python/tests/bridge.py` (portal `Line.set_path_arc` density,
  identity preservation, and failure atomicity).
- Reference sites: `manimlib/mobject/geometry.py` (`Arc.__init__`),
  `manimlib/mobject/types/vectorized_mobject.py` (`add_arc_to`),
  `manimlib/utils/bezier.py` (`quadratic_bezier_points_for_arc`), all at
  the pinned commit `6199a00d`.
- `fmn-geom/src/quadpath.rs` (`insert_n_curves_to_point_list`, including
  the first-maximum tournament fixture) and `fmn-mobject/src/align.rs`:
  curve-count alignment and shape-preserving subdivision.
- `fm-align-chord-tie-norms-uw95o`: actual fresh `face3be3` wheel versus
  pinned Reference, two minimized programs and common-input insertion probe
  under `/data/tmp/fmn-execute-20261101-align-chord-face`. The raw point
  arrays, norm types, priorities and geometry distances are retained; no
  comparator exclusion or tolerance is changed.
