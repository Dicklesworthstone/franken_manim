"""Production callback dispatch with an explicit rational timing fixture.

These tests exercise Python lifecycle routing. The installed-portal suite
exact_frame_composition_positions.py independently exercises native timing.
"""
from __future__ import annotations

import ast
from fractions import Fraction
from pathlib import Path
from types import SimpleNamespace
import unittest


def linear(alpha):
    return alpha


class TimingFixture:
    """Exact interval oracle, deliberately independent of native arithmetic."""

    frame_intervals = True
    exact_positions = True

    def __init__(self, durations, *, run_frames=None, fps=24):
        self.fps = fps
        self.windows, start = [], Fraction(0)
        for duration in durations:
            end = start + duration
            self.windows.append((start, end))
            start = end
        self.total = start
        self.duration = start if run_frames is None else Fraction(run_frames)
        self.intervals = [(float(start), float(end)) for start, end in self.windows]
        self.max_end = float(start)

    def alpha_at_frame(self, frame, fps):
        assert fps == self.fps
        return float(Fraction(frame) / self.duration)

    def exact_position_at_frame(self, frame, fps):
        assert fps == self.fps
        return Fraction(frame) * self.total / self.duration

    def rate_alpha_at_frame(self, frame, fps):
        return self.alpha_at_frame(frame, fps)

    def position_from_rate_alpha(self, alpha):
        return Fraction.from_float(float(alpha)) * self.total

    def member_at_position(self, position):
        eligible = [index for index, (start, _) in enumerate(self.windows)
                    if position >= start]
        return max(eligible, default=0) if self.windows else None

    def child_sample_at_position(self, index, position):
        start, end = self.windows[index]
        alpha = 0. if end == start else float(min(max((position - start) / (end - start), 0), 1))
        return alpha, position - start if start < position < end else None


class OpaquePositionFixture:
    """The native token supports float projection but no Python arithmetic."""

    def __init__(self, value):
        self.value = Fraction(value)
        self.float_calls = 0

    def __float__(self):
        self.float_calls += 1
        return float(self.value)


class OpaqueChildTimingFixture(TimingFixture):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.positions = []

    def child_sample_at_position(self, index, position):
        alpha, local = super().child_sample_at_position(index, position)
        if local is not None:
            local = OpaquePositionFixture(local)
            self.positions.append(local)
        return alpha, local


class Leaf:
    def __init__(self, name, frames, events):
        self.name, self.frames, self.events = name, frames, events

    def get_run_time(self):
        return float(self.frames / 24)

    def begin(self):
        self.events.append((self.name, "begin"))

    def finish(self):
        self.events.append((self.name, "finish"))

    def interpolate(self, alpha):
        self.events.append((self.name, "alpha", alpha))

    def _fmn_interpolate_at_frame(self, frame, fps, alpha=None):
        self.events.append((self.name, "position", frame))


def driver_class():
    source = Path(__file__).resolve().parents[1] / "python" / "manimlib_bootstrap.py"
    names = {"_CompositionCallbackDriver", "_composition_timeline_position"}
    nodes = [node for node in ast.parse(source.read_text()).body
             if isinstance(node, (ast.ClassDef, ast.FunctionDef)) and node.name in names]
    assert {node.name for node in nodes} == names

    def intervals(group, children):
        rows, start = [], 0.
        for child in children:
            end = start + child.get_run_time()
            rows.append((child, start, end))
            start = end
        return rows, start

    namespace = {"_RATE_FUNC_NAMES": {linear: "linear"}, "_composition_timings": intervals}
    # ubs:ignore -- only fixed production definitions are compiled, never authored input.
    exec(compile(ast.Module(nodes, []), str(source), "exec"), namespace)
    return namespace["_CompositionCallbackDriver"]


class ExactCompositionProtocolTests(unittest.TestCase):
    def setUp(self):
        self.Driver = driver_class()
        self.events = []

    def driver(self, durations, *, run_frames=None, children=None, rate=linear):
        timing = TimingFixture(durations, run_frames=run_frames)
        group = SimpleNamespace(
            rate_func=rate, _native_kind="succession", time_span=None,
            run_time=float(timing.duration / timing.fps), final_alpha_value=1.,
            _fmn_frame_timing=timing,
        )
        if children is None:
            children = [Leaf(str(index), duration, self.events)
                        for index, duration in enumerate(durations)]
        return self.Driver(group, children)

    def test_mixed_seconds_boundary_does_not_start_child_one_sample_early(self):
        boundary = Fraction.from_float(43 / 24) * 24
        self.assertEqual(boundary - 43, Fraction(1, 2**49))
        driver = self.driver([boundary, Fraction(1)])
        driver.begin()
        driver._fmn_interpolate_at_frame(43, 24)
        self.assertNotIn(("1", "begin"), self.events)
        driver._fmn_interpolate_at_frame(44, 24)
        self.assertLess(self.events.index(("0", "finish")), self.events.index(("1", "begin")))
        self.assertIn(("1", "position", 44 - boundary), self.events)

    def test_large_frame_indices_remain_distinct_without_iterating_them(self):
        boundary = 2**53 + 1
        driver = self.driver([boundary, 1])
        driver.begin()
        driver._fmn_interpolate_at_frame(boundary - 1, 24)
        self.assertNotIn(("1", "begin"), self.events)
        driver._fmn_interpolate_at_frame(boundary, 24)
        self.assertEqual(self.events[-2:], [("1", "begin"), ("1", "alpha", 0.)])

    def test_nested_retiming_retains_rational_child_positions(self):
        inner = self.driver([3, 5])
        outer = self.driver([8], run_frames=7, children=[inner])
        outer.begin()
        outer._fmn_interpolate_at_frame(2, 24)
        self.assertIn(("0", "position", Fraction(16, 7)), self.events)
        self.assertNotIn(("1", "begin"), self.events)
        outer._fmn_interpolate_at_frame(3, 24)
        self.assertIn(("1", "position", Fraction(3, 7)), self.events)

    def test_rate_callback_result_maps_to_exact_large_interval(self):
        driver = self.driver([2**53 + 1, 1], rate=lambda alpha: .5)
        driver.begin()
        driver._fmn_interpolate_at_frame(1, 24)
        self.assertIn(("0", "position", Fraction(2**53 + 2, 2)), self.events)

    def test_live_root_rebinding_uses_new_timing_without_restarting_children(self):
        driver = self.driver([3, 5])
        driver.begin()
        driver.group._fmn_frame_timing = TimingFixture([3, 5], run_frames=16)
        driver.group.run_time = 16 / 24
        driver._fmn_interpolate_at_frame(5, 24)
        self.assertNotIn(("1", "begin"), self.events)
        driver._fmn_interpolate_at_frame(6, 24)
        self.assertIn(("1", "begin"), self.events)
        self.assertEqual(self.events.count(("0", "begin")), 1)

    def python_child(self, *, seconds_at_begin=None):
        from test_animation_updater_protocol import environment
        native = environment()
        events = self.events

        class Callback(native.Animation):
            def begin(self):
                events.append(("python", "begin"))
                if seconds_at_begin is not None:
                    self.run_time = seconds_at_begin

            def interpolate(self, alpha):
                events.append(("python", "alpha", alpha))

            def finish(self):
                events.append(("python", "finish"))

        child = Callback(native.Mobject(), run_time_frames=48)
        object.__setattr__(child, "run_time", 2.)
        child._fmn_frame_timing = SimpleNamespace(
            alpha_at_frame=lambda frame, fps: float(frame.value / 48),
        )
        return child

    def test_child_seconds_assignment_in_begin_accepts_opaque_parent_position(self):
        child = self.python_child(seconds_at_begin=4.)
        timing = OpaqueChildTimingFixture([48, 1])
        driver = self.driver([48, 1], children=[child, Leaf("next", 1, self.events)])
        driver.group._fmn_frame_timing = timing
        driver.begin()
        self.assertIsNone(child.run_time_frames)
        self.assertFalse(hasattr(child, "_fmn_frame_timing"))
        driver._fmn_interpolate_at_frame(24, 24)
        self.assertIn(("python", "alpha", .25), self.events)
        self.assertEqual(timing.positions[-1].float_calls, 1)
        # A child's authored edit does not rebuild the parent's prepared
        # windows or restart its lifecycle while playback is active.
        self.assertEqual(timing.duration, 49)
        driver._fmn_interpolate_at_frame(48, 24)
        self.assertLess(self.events.index(("python", "finish")),
                        self.events.index(("next", "begin")))
        self.assertEqual(self.events.count(("python", "begin")), 1)

    def test_bound_child_keeps_opaque_parent_position_without_projection(self):
        child = self.python_child()
        timing = OpaqueChildTimingFixture([48])
        driver = self.driver([48], children=[child])
        driver.group._fmn_frame_timing = timing
        driver.begin()
        driver._fmn_interpolate_at_frame(24, 24)
        self.assertIn(("python", "alpha", .5), self.events)
        self.assertEqual(timing.positions[-1].float_calls, 0)

    def test_child_zero_seconds_assignment_keeps_finite_samples_and_parent_lifecycle(self):
        child = self.python_child(seconds_at_begin=0.)
        timing = OpaqueChildTimingFixture([48, 1])
        driver = self.driver([48, 1], children=[child, Leaf("next", 1, self.events)])
        driver.group._fmn_frame_timing = timing
        driver.begin()
        self.assertIsNone(child.run_time_frames)
        self.assertFalse(hasattr(child, "_fmn_frame_timing"))
        for frame, expected in ((-1, 0.), (0, 0.), (24, 1.)):
            with self.subTest(frame=frame):
                child._fmn_interpolate_at_frame(OpaquePositionFixture(frame), 24)
                self.assertEqual(self.events[-1], ("python", "alpha", expected))
        driver._fmn_interpolate_at_frame(24, 24)
        self.assertEqual(self.events[-1], ("python", "alpha", 1.))
        self.assertEqual(timing.duration, 49)
        self.assertNotIn(("python", "finish"), self.events)
        driver._fmn_interpolate_at_frame(48, 24)
        self.assertLess(self.events.index(("python", "finish")),
                        self.events.index(("next", "begin")))
        self.assertEqual(self.events.count(("python", "begin")), 1)

    def test_unbound_composition_accepts_float_only_position_in_seconds(self):
        driver = self.driver([24], run_frames=48)
        driver.begin()
        driver.group.run_time = 2.
        del driver.group._fmn_frame_timing
        position = OpaquePositionFixture(12)
        driver._fmn_interpolate_at_frame(position, 24)
        self.assertIn(("0", "alpha", .25), self.events)
        self.assertEqual(position.float_calls, 1)

    def test_unbound_zero_seconds_composition_has_finite_begin_and_endpoint(self):
        driver = self.driver([24], run_frames=48)
        driver.begin()
        driver.run_time = driver.group.run_time = 0.
        del driver.group._fmn_frame_timing
        for frame, expected in ((-1, 0.), (0, 0.), (12, 1.)):
            with self.subTest(frame=frame):
                position = OpaquePositionFixture(frame)
                driver._fmn_interpolate_at_frame(position, 24)
                self.assertEqual(self.events[-1], ("0", "alpha", expected))
                self.assertEqual(position.float_calls, 1)

    def test_empty_and_zero_duration_members_finish_in_order(self):
        empty = self.driver([], run_frames=1)
        empty.begin()
        empty._fmn_interpolate_at_frame(1, 24)
        driver = self.driver([0, 0, 1])
        driver.begin()
        driver._fmn_interpolate_at_frame(1, 24)
        self.assertEqual([event for event in self.events if event[1] in {"begin", "finish"}],
                         [("0", "begin"), ("0", "finish"), ("1", "begin"),
                          ("1", "finish"), ("2", "begin")])


if __name__ == "__main__":
    unittest.main()
