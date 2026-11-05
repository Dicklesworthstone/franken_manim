# BN-02 — The rational clock: drift-free time, exact frame counts

**Status:** Final (W4, fm-wuq). The six-step frame order landed (fm-x79),
and G1 (fm-o3j) passed on 2026-08-20. The migration table was checked
against real behaviour on 2026-10-04:
- the Reference's `np.arange(0, d, 1/fps)` gives 3, 6, 30, 10, 90 and 15
  frames for the rows below;
- a portal `wait(0.1)` publishes 4 frames at 30 fps and 7 at 60;
- `wait(2.9999)` publishes 90 and 180, equal to the Reference.

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

For a duration supplied in **seconds**, the segment frame count is
`n = ceil(run_time · fps)`, computed on the
**exact rational value of the received f64 duration**:
`(n−1)/fps < run_time ≤ n/fps`, exactly (locked by a 20 000-duration
property test).

An explicit **frame duration** instead supplies `n` directly. It never passes
through `float(n / fps)` to decide the number of frames. The distinction is
preserved through animation composition, scheduling, pure and serial frame
capture, and compiled timeline serialization and replay.

## What deliberately did NOT change

The Reference's emission semantics are kept verbatim as API behavior:

- samples are `t_k = k/fps`, `k = 1..=n` — no alpha-zero frame in the
  emission sequence (`begin()` interpolates at zero separately);
- upward duration rounding: the final sample may exceed `run_time`;
- alpha = `t/run_time`, clamped to `[0, 1]` (the Reference's
  `interpolate` clip);
- skipped playback visits the same clock samples and updater boundaries,
  while suppressing capture (BN-10).

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

Dividing a frame count by `fps` in Python or Rust does **not** reliably
request that count: the quotient is a rounded binary float, and some
quotients lie just above their intended frame boundary. Issue #10's twenty
segments total 864 frames, but their `n / 24` second durations cover 872
frames under the deliberate ceiling rule. There is no epsilon or
near-integer exception to that rule.

Use the frame-native forms when segment lengths come from an audio edit,
storyboard, or other integer frame schedule:

```python
self.wait_frames(17)
self.play(square.animate.shift(RIGHT), run_time_frames=43)
self.play(square.animate(run_time_frames=43).shift(RIGHT))
self.play(FadeIn(square, run_time_frames=43))
```

The native Rust equivalents are `Stage::wait_frames(u64)`,
`AnimateArgs { run_time_frames: Some(n), ..Default::default() }`,
`AnimConfig { run_time_frames: Some(n), ..Default::default() }`, and
`PlayOverrides { run_time_frames: Some(n), ..Default::default() }`.
`AnimationGroup` and `Succession` offer `with_run_time_frames(n)`;
`Timeline::wait_frames(n)` retains the exact count in its compiled schedule.
At the clock layer, use `segment_frames(n)` or `segment(Duration::Frames(n))`.

Frames refer to the scene's **active** frame rate, including its windowed
frame-rate setting. An exact finite wait bypasses presenter-controlled holds.
Zero frames perform the normal segment lifecycle without a progression frame
or elapsed time. A movie containing only zero-frame requests is refused as an
empty stream instead of gaining an implicit static frame; explicitly exporting
a final still remains a one-image operation. Counts and cumulative positions must fit the clock's
nonnegative `i64` range; an oversized request fails before playback starts.
Python frame values must be nonnegative integers (including integer objects
supporting `__index__`); booleans and floating-point values are refused.
Supplying both `run_time` and `run_time_frames` to one Python operation,
animation builder, or play override is an error. A later seconds override
clears an earlier frame override.

Groups containing frame-native children derive their extent in exact frame
units, including mixed seconds/frame children and nested groups. Members are
not rounded separately before composition. Fractional lag ratios can create
fractional-frame endpoints; those are still covered by an upward rounding at
the final segment boundary. Groups containing only second-based durations
retain their existing floating-point interval arithmetic and operation order.
An explicit group duration scales its internal timeline as before, and
`time_span` keeps its existing duration-widening semantics.

`ChangeSpeed` accepts an explicit `run_time_frames` or `run_time` on the
wrapper. These overrides set its duration while the speed profile still maps
progress through its child. With a frame-timed child and no override, the
profile's duration scale must be exactly representable as a finite binary64
value. The native planner keeps the resulting frame extent exact. For example,
twelve frames at constant speed two take six frames; fractional extents remain
unrounded inside nested groups.

Profiles whose exact scale cannot be represented are refused before playback
and request an explicit duration. A rounded reciprocal must not silently add
a frame: five frames at constant speed five can be requested as
`ChangeSpeed(child, {0: 5}, run_time_frames=1)`. Seconds-only `ChangeSpeed`
retains its existing floating-point profile calculation. In Rust,
`AnimationGroup::with_duration_scale(factor)` scales a derived extent by the
exact supplied binary64 factor; explicit seconds or frame overrides take
precedence over that scale.

Frame counts in segment and render reports remain authoritative integers.
Their seconds fields are presentation values; passing such a value back to
`wait(seconds)` requests a new second-based duration. Audio cue positions
continue to use the exact scene frame and rate under BN-14. Frame-native
segment chains therefore keep their authored frame total and audio positions
without a per-operation `math.nextafter` workaround.

## Evidence

- `crates/fmn-anim/src/clock.rs`; `crates/fmn-anim/tests/clock.rs`
  (count fixtures cross-checked against Python `fractions.Fraction` and
  the Reference's `arange` output, drift test, coverage property).
- Reference: `manimlib/scene/scene.py::get_time_progression` at the
  pinned commit.
- The corpus differential counts each Reference segment by this rule
  (`time_bn02` in `structural_facts.py`); a scene whose portal clock equals
  that count differs only under this note (exclusion row
  `bn02-frame-count`). `UniformSamples` (`_2023/convolutions2/continuous.py`)
  waits 0.1 s a hundred times: 35.000 s in the Reference, 38.333 s here.

## Native tracing cadence and time windows

Native `TracedPath` and `TracingTail`, including the Python portal's
already-bound tail source, measure history in elapsed seconds.
`time_per_anchor` spaces observations on a regular temporal grid; the
moving `time_traced` boundary and the current endpoint interpolate between
those observations. Smoothing and the true-arc-length stroke and opacity
tapers still operate on that path. Scene frame sampling and the rational
clock are unchanged.

The Reference chooses a recent-point count from the latest `dt`, so a
change of update size changes the duration represented by older samples.
The native tracers retain the configured time window under tiny,
irregular, and large positive updates. Their storage and sampling work are
bounded by the window/cadence ratio, including the predecessor needed to
clip the boundary. Expired samples within a large update are skipped;
they are never allocated. Configurations exceeding the shared 100,000
anchor budget fail before stage adoption.

`TracedPath` begins at its first positive-dt observation. Construction and
zero-dt updates neither call its source function nor invent earlier
history. Its default infinite duration retains all regular grid samples
up to the same anchor budget. A step that would exceed that budget fails
before changing the stored history or geometry; the caller can choose a
finite window or a larger `time_per_anchor` for a longer recording.

`TracingTail` instead seeds stationary history at the source's current
center while keeping public geometry empty. Zero-dt updates leave it
empty; the first positive update publishes the seeded path. This preserves
the Reference's construction lifecycle while correcting the previous
native eager path.

**Migration:** use `time_per_anchor` to choose temporal detail and
`time_traced` for duration. Native `TracedPath` previously stored
`time_per_anchor` without applying it; direct point inspection now reflects
that cadence instead of one anchor per update. Code that inspects native
trace points directly after construction should advance by the intended
positive scene step first. A caller supplying negative or non-finite
native updater deltas, or non-finite source coordinates, gets an explicit
panic before trace history or geometry changes. The infallible
`Stage::update` API does not roll back its own clock or arbitrary source
callback effects.

The native regressions are in `crates/fmn-library/src/fields.rs`; the
bound-source portal cases are in `crates/fmn-python/tests/bridge.py` and
`crates/fmn-python/tests/detached_tracing_tail.py`. The registered
`lifecycle.tracing_tail_empty_until_update.v1` scenario covers stage
adoption, zero-dt behavior, seed/endpoints, tapers, and stateful scheduling.
`lifecycle.traced_path_cadence.v1` drives the native timeline and renderer,
checking first observation, regular anchors, the exact retained window,
tapers, stateful scheduling, and visible output.
