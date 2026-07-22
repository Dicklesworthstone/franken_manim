# BN-07 — `add_updater(call=True)` runs once (C-5), and group addition is a value operation (C-6)

**Status:** Draft (W3, fm-yra). Consumed by Choreo (§9.1's
`suspend_mobject_updating` interaction), fmn-python (whose `manimlib`
surface presents these semantics), and the Parity Ledger.

Two Appendix-C rulings owned by the dynamic-behavior surface (§8.6). Both
are deliberate, correct divergences from the pinned Reference (D-05); the
API names and everything around them carry over exactly.

## C-5 — `add_updater(call=True)` runs the update pass exactly once

The Reference (`mobject.py`):

```python
def add_updater(self, update_func: Updater, call: bool = True) -> Self:
    self.updaters.append(update_func)
    if call:
        self.update(dt=0)
    self.refresh_has_updater_status()
    self.update()          # <-- unconditional second update(dt=0) pass
    return self
```

With `call=True`, every updater in the mobject's family runs **twice** at
registration (and even with `call=False`, the trailing `self.update()`
still runs one uninvited pass). An updater with side effects — a counter,
an appender, anything non-idempotent at dt=0 — observes double execution
on every registration.

**FrankenManim:** [`Stage::add_updater`] /
[`Stage::add_dt_updater`](../../crates/fmn-mobject/src/stage.rs) with
`call = true` run exactly one `update(dt=0)` pass over the mobject's
family (matching the Reference's *intended* `self.update(dt=0)` semantics,
including running pre-existing updaters — that part is Reference behavior,
not a bug); with `call = false`, no pass runs at all.

**Migration:** scene code that (knowingly or not) depended on the double
call — e.g. an updater that must run twice before the first frame — should
call `stage.update(0.0)` explicitly for the second pass. Non-idempotent
updaters simply stop double-firing; no action needed.

Locked by `tests/dynamics.rs::c5_call_runs_the_update_pass_exactly_once`
and `tests/scenarios.rs` (s9).

## C-6 — group addition always builds a new group

The Reference has two different semantics under one operator:

```python
class Mobject:
    def __add__(self, other):            # value semantics
        return self.get_group_class()(self, other)

class Group(Mobject):
    def __add__(self, other):            # in-place mutation
        return self.add(other)           # returns self
```

`square + circle` builds a fresh group, but `group + circle` **mutates the
existing group** and returns it — whether `a + b` aliases `a` depends on
`a`'s runtime type, which is exactly the kind of surprise that corrupts a
scene when a "combined" group is positioned independently.

**FrankenManim:**
[`Stage::group_add`](../../crates/fmn-mobject/src/dynamics.rs) always
builds a new group containing both operands, including when the left
operand is itself a group. Consistent value semantics; operands are never
mutated. (fmn-python's `Group.__add__` presents this corrected behavior
under the original name.)

**Migration:** code that relied on `group + mob` mutating `group` should
call `group.add(mob)` — the explicit in-place API, which keeps its
Reference semantics unchanged.

Locked by `tests/dynamics.rs::c6_group_add_is_a_value_operation`.
