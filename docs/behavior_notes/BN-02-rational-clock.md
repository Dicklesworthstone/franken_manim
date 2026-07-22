# BN-02 — The rational clock: drift-free time, exact frame counts

**Status:** Draft (W4, fm-wuq). Finalized when the six-step frame order
lands (fm-x79).

## What changed

Classic manim advances a float accumulator over
`np.arange(0, run_time, 1/fps) + 1/fps`. Two consequences:

1. **Drift.** `1/30` is not representable; an hour at 30 fps accumulates
   error visibly, and the same scene disagrees with itself under different
   play/wait chunkings.
2. **Float-boundary frame counts.** `arange`'s length depends on float
   division rounding, so the number of emitted frames for a duration is an
   artifact of the float pipeline, not of the duration.

FrankenManim's clock is `(frame_index, fps)` — a `RationalTime` whose
value is exactly `frames / fps`. Time *derives*; it never accumulates.
A million frames at 30 fps is exactly `1_000_000/30` seconds, bit-equal
to the closed form (locked by test).

The segment frame count is `n = ceil(run_time · fps)`, computed on the
**exact rational value of the received f64 duration**:
`(n−1)/fps < run_time ≤ n/fps`, exactly (locked by a 20 000-duration
property test).

## What deliberately did NOT change

The Reference's emission semantics are kept verbatim as API behavior:

- samples are `t_k = k/fps`, `k = 1..=n` — no alpha-zero frame in the
  emission sequence (`begin()` interpolates at zero separately);
- upward duration rounding: the final sample may exceed `run_time`;
- alpha = `t/run_time`, clamped to `[0, 1]` (the Reference's
  `interpolate` clip);
- skipped playback advances the whole segment in one step.

Adaptive or variable frame sampling remains **permanently refused**
(D-18): `RationalTime` is only constructible as whole frames over fps —
there is no API for an off-grid sample.

## Migration guidance

Frame counts can differ **by one** from the Python engine exactly where
binary f64 representation puts the requested duration off its decimal
intent:

| duration | fps | Python frames | FrankenManim | why |
|---|---|---|---|---|
| `0.1` | 30 | 3 | 4 | f64(0.1) > 1/10; three frames end at 0.1 exactly, which is *less* than the requested duration |
| `0.1` | 60 | 6 | 7 | same excess |
| `1.0`, `1/3`, `2.9999`, `0.5`, whole numbers… | any | equal | equal | representable-boundary cases agree |

If a scene needs the old count, request a grid-exact duration
(`n/fps`). Everything downstream gains: `wait()` chains of any length
hold A/V sync exactly, and a scene's timing is identical however its
plays are chunked.

## Evidence

- `crates/fmn-anim/src/clock.rs`; `crates/fmn-anim/tests/clock.rs`
  (count fixtures cross-checked against Python `fractions.Fraction` and
  the Reference's `arange` output, drift test, coverage property).
- Reference: `manimlib/scene/scene.py::get_time_progression` at the
  pinned commit.
