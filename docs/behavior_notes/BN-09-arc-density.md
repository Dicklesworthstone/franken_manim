# BN-09 — One arc-density rule

**Status:** Draft (W2, fm-e3f). Consumed by the W7 geometry lineage (fm-oab)
when Arc/Circle land.

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

## Evidence

- `crates/fmn-geom/src/bezier.rs` (`arc_n_components`, unit tests).
- Reference sites: `manimlib/mobject/geometry.py` (`Arc.__init__`),
  `manimlib/mobject/types/vectorized_mobject.py` (`add_arc_to`),
  `manimlib/utils/bezier.py` (`quadratic_bezier_points_for_arc`), all at
  the pinned commit `6199a00d`.
