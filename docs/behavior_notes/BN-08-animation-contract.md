# BN-08 — The Animation contract's typed edges (§9.1)

**Status:** Draft (W4, fm-67a). Consumed by Choreo's mechanism families
(fm-cye), composition operators (fm-hfe), fmn-python (whose `manimlib`
surface presents these semantics), and the Parity Ledger.

The Animation lifecycle, the constructor surface, `get_sub_alpha`'s lag
formula, and `time_spanned_alpha`'s re-window are kept **exactly** (fixture
corpus: `crates/fmn-anim/fixtures/`, generated from the pinned formulas by
`scripts/gen_anim_fixtures.py`). Three edges of the surface diverge
deliberately (D-05) — each replaces an accident of Python with the correct
behavior under the same name.

## 1. `update_rate_info` honors explicit zeros

The Reference (`animation.py`):

```python
def update_rate_info(self, run_time=None, rate_func=None, lag_ratio=None):
    self.run_time = run_time or self.run_time
    self.rate_func = rate_func or self.rate_func
    self.lag_ratio = lag_ratio or self.lag_ratio
```

Python's `or` treats `0`/`0.0` as absent: an explicit `run_time=0` or
`lag_ratio=0` is **silently ignored** (`AnimationGroup` passing
`lag_ratio=0` down cannot actually reset a child's lag). FrankenManim's
`Animation::update_rate_info` takes `Option<f64>`: `None` keeps the current
value, `Some(0.0)` **sets zero**. The truthiness trap is unrepresentable.

**Migration:** code relying on `update_rate_info(run_time=0)` being a no-op
must pass `None` instead; passing zero now means zero.

## 2. A hollow `time_span` is a named error

`time_span=(t, t)` (or `end < start`) reaches
`clip(...) / (end - start)` in the Reference and raises
`ZeroDivisionError` at the first interpolation. Here `begin` refuses it
up front as `AnimError::InvalidTimeSpan { start, end }` — the error names
the values and arrives before any state is touched (no animating-status
mark, no starting copy).

## 3. `prepare_animation`'s rejection is a compile error

The Reference rejects bare bound methods at runtime
(`TypeError: Object ... cannot be converted to an animation`). The typed
form of the same contract: `IntoAnimation` is implemented for `Animation`
types, `AnimBuilder`, and `BuiltAnimate` — nothing else — so the invalid
call does not compile. fmn-python restores the Reference's runtime
`TypeError` at the bridge, where arbitrary Python objects can still arrive.

## Staging boundaries (not divergences)

Two precise, named errors mark where the Transform family (fm-cye) takes
over from the fm-67a carrier; both disappear as capabilities when it lands:

- `AnimError::PathArcUnsupported` — a recorded `path_arc` on a built
  `.animate` chain (arcs are Transform's `path_func` mechanism). Never a
  silent straight line.
- `AnimError::UnalignedFamilies` — a source/target pair that structurally
  diverged between build and play (alignment of heterogeneous pairs is
  `align_data`). Never a partial lerp.
