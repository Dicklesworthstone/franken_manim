# BN-11 — Composition honors what it was told (§9.4)

**Status:** Draft (W4, fm-hfe). Consumed by the scene runtime (fm-5xm),
the Studio's scrubbing (fm-yh0), the WASM timeline player (fm-oee), and the
Parity Ledger.

Two Reference defects in `manimlib/animation/composition.py`, both of the
same shape: a composition derives a number from its inputs and then ignores
it. Both are fixed; nothing else about the operators changes.

## C-10 — a group's `rate_func` and `time_span` are inert

`AnimationGroup.interpolate` overrides `Animation.interpolate` and consumes
raw alpha:

```python
def interpolate(self, alpha: float) -> None:
    time = alpha * self.max_end_time
    for anim, start_time, end_time in self.anims_with_timings:
        ...
        anim.interpolate(sub_alpha)
```

The constructor still *accepts* `rate_func=` and `time_span=` (they are
`Animation.__init__`'s parameters and `AnimationGroup` forwards `**kwargs`
to it), and `self.rate_func` is duly set — it is simply never read. So
`AnimationGroup(..., rate_func=there_and_back)` is a silent no-op, and so is
a group's `time_span`. Nesting compounds it: an outer group's curve cannot
reach an inner one, which is exactly what nesting is *for*.

**The ruling.** A composition's alpha runs the same normalized-alpha
pipeline every leaf animation runs — `time_spanned_alpha` then the rate
curve — before it becomes a position on the internal timeline. The group's
`lag_ratio` is **not** re-applied as a per-submobject lag: it is already
spent in the interval table (`build_animations_with_timings`), and applying
it twice would double-count.

The corollary that keeps the common case unchanged: **a composition's
`rate_func` defaults to `linear`, not `smooth`.** Members own their easing;
the group's curve shapes the composition's timeline. With the identity
default, a group built the ordinary way produces exactly the Reference's
member alphas — which the fixture corpus asserts, case for case, against the
Reference's own arithmetic.

Locked by
`crates/fmn-anim/tests/composition.rs::group_member_alphas_match_the_reference_corpus`
(the Reference's mapping, 1008 cases),
`::the_default_group_rate_func_is_the_identity`,
`::a_group_rate_func_shapes_the_composition`,
`::a_group_time_span_re_windows_the_composition`, and
`::nested_groups_compose_with_independent_rate_funcs`.

## C-11 — `Succession` ignores its members' run times, and drops the ones a coarse step passes

`Succession` derives its own `run_time` from its members' run times (it is
an `AnimationGroup` at `lag_ratio = 1`, so `max_end_time` is their sum), and
then picks the active member by **equal shares**:

```python
def interpolate(self, alpha: float) -> None:
    index, subalpha = integer_interpolate(0, len(self.animations), alpha)
```

`Succession(a(run_time=3), b(run_time=1))` therefore runs for 4 seconds and
gives each member 2 of them: `a` is rushed through at 1.5× and `b` crawls at
0.5×. The longer the spread, the worse it reads.

The same line drops members. When one frame's alpha step crosses more than
one member — a low fps, a short `run_time`, a scrub — the Reference jumps
straight to the target index, so every member in between is never begun and
never finished. Their effects (a remover's removal, a transform's end state)
simply do not happen.

**The ruling.** `Succession` maps alpha through the *same interval table*
every other operator uses, so a member's share of the composition is its own
run time; and it **walks** the active member forward one step at a time,
finishing each member it passes and beginning the next, so no member is ever
skipped. Just-in-time `begin` is kept exactly as the Reference has it — it
is the whole point of the operator, since member *k*'s starting copy must
freeze what member *k-1* left behind.

Because `interpolate` has no error channel, a member whose just-in-time
`begin` fails records the failure (`Animation::deferred_error`) and the
segment driver surfaces it by name at the end of the segment, rather than
continuing on stale state.

Locked by
`crates/fmn-anim/tests/composition.rs::succession_honours_member_run_times_where_the_reference_does_not`
(both answers per case, 168 cases — 31 of them divergent),
`::succession_walks_the_members_a_coarse_step_would_skip`, and
`::a_just_in_time_begin_failure_surfaces_from_the_segment`.

## What is *not* changed

A member still lands on **its own** `final_alpha_value` at `finish`; a
composition's `final_alpha_value` is not a second landing point. That is
load-bearing rather than incidental — `FadeOut` finishes at alpha 0 exactly
so a removed mobject is left in its original state — and a container that
overrode it would break every remover it contains. Removal likewise stays
each member's decision: `clean_up_from_scene` delegates, so a `FadeOut`
inside a group still leaves the scene and the container never does.

**Migration:** scenes whose look depended on `Succession`'s equal-share
timing will see members run at their declared speeds instead. A scene that
wants equal shares asks for it directly — give the members equal run times.
Group `rate_func`/`time_span` arguments that were previously ignored now
take effect; a group that should progress linearly needs no argument at all,
because linear is the default.
