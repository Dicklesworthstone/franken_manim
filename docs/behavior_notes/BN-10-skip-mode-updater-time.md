# BN-10 — Skip mode delivers the same updater time as playback (§9.3)

**Status:** Draft (W4, fm-x79). Consumed by the scene runtime (fm-5xm),
the segment-purity classifier (fm-3xk), and the Parity Ledger.

## The Reference's defect

Skipped playback in the Reference advances the segment in one step, then
double-applies its duration to dt-updaters. `progress_through_animations`
over the skip progression `[run_time]` calls `update_frame(run_time)` —
which increments time and runs scene updaters with `dt = run_time` — and
then `finish_animations` runs a **second** full-duration pass:

```python
def finish_animations(self, animations):
    for animation in animations:
        animation.finish()
        animation.clean_up_from_scene(self)
    if self.skip_animations:
        self.update_mobjects(self.get_run_time(animations))   # dt = run_time, again
    else:
        self.update_mobjects(0)
```

A played segment delivers `run_time` of total dt to scene updaters (one
frame at a time) plus a final `dt = 0` pass; a skipped segment delivers
`2 × run_time`. Any scene with a dt-updater (a clock readout, a physics
integration, a `TracedPath`) ends a skipped segment in a **different state**
than a played one — skipping (`-s`, `skip_animations`) is supposed to be a
preview accelerator, not a state change.

## The ruling

FrankenManim's `finish_animations` pass runs at `dt = 0` in **both** modes.
Under skip, the segment's whole duration reaches updaters exactly once
(the single big step); under playback, frame by frame. Total updater time
is identical either way, and `sum(dt) = run_time` holds for every segment
regardless of skip status. The frame order itself (steps 1–6), the
no-capture/no-emit skip behavior, and the `dt = 0` finish pass are all
kept exactly.

Locked by the update-order corpus:
`crates/fmn-anim/tests/frame_order.rs::skip_mode_matches_played_final_state_and_emits_nothing`.

**Migration:** scenes that (knowingly or not) relied on skipped segments
running their dt-updaters at double speed will now see identical state on
both paths. There is no way to ask for the doubled behavior.
