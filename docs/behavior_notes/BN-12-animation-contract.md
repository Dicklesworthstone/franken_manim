# BN-12 — The Animation contract's typed edges (§9.1)

> **Renumbered from BN-08 on 2026-07-25 (fm-nn8, ADR-0009).** §16.8 assigns
> BN-08 to the de-TeX'd classes (§11.6); this note took that number before the
> de-TeX'd note existed. Nothing outside this file referenced it — the Parity
> Ledger's `BN-08` evidence cells all point at the de-TeX'd natives — so the
> renumbering changed one heading and one filename. Numbers are never reused.

**Status:** Draft (W4, fm-67a; W10, fm-k8e3). Consumed by Choreo's mechanism families
(fm-cye), composition operators (fm-hfe), fmn-python (whose `manimlib`
surface presents these semantics), and the Parity Ledger.

The Animation lifecycle, the constructor surface, `get_sub_alpha`'s lag
formula, and `time_spanned_alpha`'s re-window are kept **exactly** (fixture
corpus: `crates/fmn-anim/fixtures/`, generated from the pinned formulas by
`scripts/gen_anim_fixtures.py`). The edges below diverge
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

## 4. Three-dimensional arc paths preserve axial motion

The native `PathFunc::Arc` uses full Rodrigues rotation perpendicular to its
axis and linearly interpolates displacement along that axis. Different axial
coordinates therefore follow a helix with exact start/end values rather than
the old two-term rotation, which could miss its requested endpoint. Finite
nonzero axes are normalized without overflow or underflow; a zero axis still
means OUT. The existing small-angle straight-path threshold is unchanged.

Non-finite arc angles and axes are refused as `AnimError::InvalidPath` before
Transform alignment or snapshot allocation. The affine lift interpolates its
linear columns separately from translation, so a large world origin cannot
round away local shape through translated-basis subtraction.

**Migration:** spatial arc motion with displacement parallel to the axis now
has a linear axial component. Planar arcs keep their circular geometry. Code
must not rely on malformed non-finite arc parameters reaching interpolation.

## 5. A growing animation's anchor is the point it was constructed with

`GrowFromCenter(mob)` stores `point = mob.get_center()`. In the Reference,
`get_center()` returns `self.get_bounding_box()[1]`, a row *view* into the
cached bounding box. `get_bounding_box` refreshes that array in place, so
moving `mob` after constructing the animation also moves its "fixed" anchor,
and the growth starts from wherever the mobject is at play time.
`GrowFromEdge` and `GrowArrow` compute a fresh array and do not alias.
FrankenManim's anchor is a value for all three: the point where the mobject
was when the animation was constructed.

**Migration:** to grow from the play-time center, construct the animation
after moving the mobject, or pass `GrowFromPoint(mob, mob.get_center())`
at that time.

## 6. `Transform` resolves its arc path when it begins

The Reference's `Transform.__init__` calls `init_path_func()`, so
`path_func` is built from the constructor's `path_arc`/`path_arc_axis`, and
assigning `anim.path_arc` later has no effect. FrankenManim leaves
`path_func` as `None` unless one is authored, and resolves `path_arc` and
`path_arc_axis` when the animation begins. A later assignment takes effect.
An authored `path_func`, or an explicit `init_path_func()` call, fixes the
path as before.

**Migration:** code that reads `anim.path_func` right after construction gets
`None` for an arc given only by `path_arc`. Call `anim.init_path_func()`
first if the callable itself is needed.

## 7. Rotating the camera frame turns it by the animation's angle (C-34)

The Reference's `Rotate.interpolate_mobject` resets only the pointlike data
of `family_members_with_points` from the starting copy, then rotates the
mobject by `rate(alpha) * angle` (`rotation.py:42-55`). A `CameraFrame`
keeps its orientation in uniforms, which are never reset, so each sample's
rotation adds to the last, and the result depends on the frame count:
`Rotate(frame, PI/2)` ends at Euler angle π, −3π/4, π/2 or π/4 for run times
of 1/30, 1/15, 0.5 and 1 s. Mobjects with points rotate correctly in the
Reference.

FrankenManim rotates the frame on the camera track from its starting pose,
so it ends at the animation's angle at any frame rate.

Locked by `frame_rotation_ends_at_its_angle_whatever_the_frame_count` in
`crates/fmn-python/tests/camera_motion.py` (fm-k8e3).

**Migration:** none needed for scenes that meant the angle they wrote. A scene
tuned to the Reference's accumulated result rotates by its written angle.

## 8. `Transform.update_config` applies its arguments

The Reference (`transform.py:95`):

```python
def update_config(self, **kwargs) -> None:
    Animation.update_config(self, **kwargs)
    if "path_arc" in kwargs:
        self.path_func = path_along_arc(
            kwargs["path_arc"],
            kwargs.get("path_arc_axis", OUT)
        )
```

At the pin `Animation` has no `update_config`, so every call raises `AttributeError`.

FrankenManim does what the method body is written to do:
- each keyword becomes an attribute;
- a `path_arc` rebuilds `path_func` as `path_along_arc(path_arc, path_arc_axis or OUT)`, so `path_func` and the arc that native playback follows agree;
- a `path_arc_axis` on its own changes only the axis.

Evidence: `crates/fmn-python/tests/test_ledger_witnesses_animation.py` (`test_transform_update_config`), fm-6u1g.

## Complete native method transforms

Native `.animate` recordings use the same Transform implementation as explicit
transforms: family/record alignment, `path_arc` and `path_arc_axis`, matching
field locks, tracker/uniform interpolation, and host-view materialization.
The former staging refusals `PathArcUnsupported` and `UnalignedFamilies` remain
as enum variants for source compatibility, but the method carrier no longer
emits them. A diverged target is aligned privately rather than mutated.

## Complete one-by-one reveals and balanced hidden lifetimes

`ShowSubmobjectsOneByOne` now selects child `ceil(alpha * n) - 1` for
positive indices clamped to `1..=n`; index zero is the empty initial frame.
The pinned Reference clips the index to `n - 1` before subtracting one,
which makes the last child unreachable and every one-child group invisible.
This off-by-one is corrected rather than reproduced. Migration: a scene
that intentionally stopped on the penultimate child should use an explicit
final alpha such as `(n - 1) / n`, or omit that last child from its input.

Subset, word, letter, and one-by-one reveals release animation flags and any
animation-owned updater suspension over the family captured at begin,
even when the final alpha hides some descendants. This repairs permanently
paused objects when authors re-add hidden members later. The final visible
family receives its usual single resume-updater pass. Hidden members receive
no synthetic updater call; abort neither advances to the final alpha nor
executes updaters. A family already suspended before begin remains suspended.

Evidence: `fmn-anim/tests/reveal_lifecycle.rs` and the real public scene path
in `fmn/tests/reveal_lifecycle.rs`, plus the existing native/serial PNG
comparisons for all subset reveal parameterizations.

## Collapsed wave domains

`ApplyWave` assigns the midpoint phase to a family with zero horizontal
width. A vertical curve or point-sized family receives one simultaneous
finite pulse instead of NaN coordinates from division by zero. Nonzero-width
wave formulas and construction-time bounds are unchanged. This does not
change the animation's native-dispatch eligibility or callback contract.
Evidence: `fmn-anim/tests/wave_domains.rs`.

## Broadcast options control playback

`broadcast` now constructs its interval table with the supplied lag ratio.
Previously it built a 5-percent `LaggedStart` table and changed only the
reported config afterward, so the requested delays never reached playback.
A true `remover` option is applied to the supplied animation members, because
AnimationGroup cleanup is member-owned. False preserves each member's own
removal policy. Group cleanup semantics themselves are unchanged.
Evidence: `fmn-anim/tests/broadcast_timing.rs` checks both the analytical
intervals and actual intermediate positions for unequal member durations,
plus scene membership after completion.
