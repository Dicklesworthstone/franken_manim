"""Speed-profile and lifecycle tests; no extension needed for protocol tests.

Run: python crates/fmn-python/tests/test_speed.py
Also exercise the installed native portal: FMN_TEST_NATIVE=1 python <this file>
The protocol doubles test adapter ownership, not native geometry or rendering.
"""
from __future__ import annotations

import ast
import importlib.util
import math
import os
from pathlib import Path
import types
import unittest
from unittest.mock import patch

_SOURCE = Path(__file__).resolve().parents[1] / "python/fmn_python/speed.py"
_spec = importlib.util.spec_from_file_location("speed_under_test", _SOURCE)
speed = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(speed)


def protocol():
    """Only the shared group's public protocol; no alternative production loop."""
    def linear(alpha):
        return alpha

    class Animation:
        def __init__(self, duration=2.0, rate_func=linear):
            self.run_time, self.rate_func = duration, rate_func
            self.mobject = object()
            self.events = []
            self.failure = None

        def get_run_time(self):
            return self.run_time

        def begin(self):
            self.events.append("begin")

        def interpolate(self, alpha):
            self.events.append(("alpha", self.rate_func(alpha)))
            if self.failure is not None:
                raise self.failure

        def update_mobjects(self, dt):
            self.events.append(("dt", dt))

        def finish(self):
            self.events.append("finish")

        def abort(self):
            self.events.append("abort")

        def clean_up_from_scene(self, scene):
            self.events.append(("cleanup", scene))

    class NativeAnimation(Animation):
        pass

    class Builder:
        def __init__(self, child):
            self.child, self.calls = child, 0

        def build(self):
            self.calls += 1
            return self.child

    class AnimationGroup(NativeAnimation):
        def __init__(self, *animations, run_time=-1, rate_func=None, **kwargs):
            self.animations = [a.build() if isinstance(a, Builder) else a for a in animations]
            if not all(isinstance(a, Animation) for a in self.animations):
                raise TypeError("expected Animation")
            self.rate_func = rate_func or linear
            self.run_time_frames = kwargs.get("run_time_frames")
            self._fmn_derived_run_time = run_time == -1 and self.run_time_frames is None
            self.run_time = max(a.get_run_time() for a in self.animations) if run_time == -1 else run_time
            self.mobject = object()
            self._composition_driver = None

        def begin(self):
            self._composition_driver = tuple(self.animations)
            for child in self._composition_driver:
                child.begin()
            self.interpolate(0.0)

        def interpolate(self, alpha):
            mapped = self.rate_func(alpha)
            for child in self._composition_driver:
                child.interpolate(mapped)

        def update_mobjects(self, dt):
            for child in self._composition_driver:
                child.update_mobjects(dt)

        def finish(self):
            for child in self._composition_driver:
                child.finish()

        def abort(self):
            children, self._composition_driver = self._composition_driver, None
            if children:
                for child in children:
                    child.abort()

        def clean_up_from_scene(self, scene):
            for child in self._composition_driver:
                child.clean_up_from_scene(scene)
            self._composition_driver = None

    class ChangeSpeed(Animation):
        pass

    module = types.SimpleNamespace(
        ChangeSpeed=ChangeSpeed, AnimationGroup=AnimationGroup, Animation=Animation,
        Builder=Builder, _linear_rate=linear, _RATE_FUNC_NAMES={linear: "linear"},
        _composition_member_run_time=lambda child: child.get_run_time(),
    )
    speed.install_speed(module)
    return module


class ProfileTests(unittest.TestCase):
    def test_identity_and_clamping(self):
        profile = speed._SpeedProfile()
        self.assertEqual(profile.total_time, 1.0)
        for alpha in (0.0, 0.1, 0.25, 0.8, 1.0):
            self.assertAlmostEqual(profile.map(alpha), alpha)
        self.assertEqual(profile.map(-3), 0)
        self.assertEqual(profile.map(3), 1)

    def test_constant_speed_changes_duration_not_normalized_progress(self):
        profile = speed._SpeedProfile({0: 2, 1: 2})
        self.assertEqual(profile.total_time, 0.5)
        self.assertAlmostEqual(profile.map(0.25), 0.25)

    def test_exact_dyadic_frame_scales(self):
        for speed_value, expected in ((1., 1.), (2., .5), (.5, 2.), (4., .25)):
            with self.subTest(speed=speed_value):
                profile = speed._SpeedProfile({0: speed_value, 1: speed_value})
                self.assertEqual(profile.frame_scale(), expected)
                self.assertEqual(profile.frame_scale(), expected)

    def test_frame_scale_sums_exact_piecewise_knots(self):
        # The two half-width intervals take 1/4 and 1/2 respectively.
        profile = speed._SpeedProfile({0: 2., .5: 2., 1: 0.})
        self.assertEqual(profile.frame_scale(), .75)

    def test_non_dyadic_scale_requires_explicit_duration(self):
        for value in (3., 5., 7.):
            with self.subTest(speed=value):
                profile = speed._SpeedProfile({0: value, 1: value})
                with self.assertRaisesRegex(ValueError, "specify run_time_frames or run_time"):
                    profile.frame_scale()
                # Asking for frame arithmetic must not change seconds behavior.
                self.assertEqual(profile.total_time, 1. / value)
                self.assertEqual(profile.map(.5), .5)

    def test_exact_factor_check_uses_sum_not_individual_intervals(self):
        # Each interval takes 1/3, but their exact total is representable.
        profile = speed._SpeedProfile({0: .5, .25: 1., .5: .5, 1: 2.5})
        self.assertEqual(profile.frame_scale(), 1.)

    def test_dyadic_scale_must_fit_binary64_without_rounding(self):
        # The exact factor is 3/8 + 2**-1075, despite total_time rounding to
        # 3/8. Being dyadic alone is insufficient for the transport value.
        profile = speed._SpeedProfile({0: 1., math.nextafter(0., 1.): 1., .5: 3., 1: 5.})
        self.assertEqual(profile.total_time, .375)
        with self.assertRaisesRegex(ValueError, "no exactly representable frame scale"):
            profile.frame_scale()

    def test_accelerating_from_rest(self):
        profile = speed._SpeedProfile({0: 0, 1: 2})
        self.assertEqual(profile.total_time, 1.0)
        self.assertEqual(profile.map(0.5), 0.25)

    def test_decelerating_to_rest(self):
        self.assertEqual(speed._SpeedProfile({0: 2, 1: 0}).map(0.5), 0.75)

    def test_defaults_and_input_not_mutated(self):
        original = {0.5: 2}
        profile = speed._SpeedProfile(original)
        self.assertEqual(original, {0.5: 2})
        self.assertEqual(profile.nodes, ((0.0, 1.0), (0.5, 2.0), (1.0, 2.0)))
        original[0.5] = 50
        self.assertEqual(profile.nodes[1][1], 2)

    def test_piecewise_duration_knots_and_monotonicity(self):
        profile = speed._SpeedProfile({0: 1, 0.25: 1, 0.75: 0.25, 1: 0.25})
        self.assertAlmostEqual(profile.total_time, 2.05)
        self.assertAlmostEqual(profile.map(0.25 / 2.05), 0.25)
        self.assertAlmostEqual(profile.map(1.05 / 2.05), 0.75)
        values = [profile.map(i / 10000) for i in range(10001)]
        self.assertEqual(values, sorted(values))
        self.assertEqual((values[0], values[-1]), (0, 1))

    def test_large_finite_endpoint_speeds_do_not_overflow(self):
        profile = speed._SpeedProfile({0: 1e308, 1: 1e308})
        self.assertTrue(0 < profile.total_time < 1e-307)
        self.assertAlmostEqual(profile.map(0.5), 0.5)

    def test_invalid_profiles_fail_before_execution(self):
        for value in ([], {True: 1}, {0: True}, {0: -1}, {-0.1: 1}, {1.1: 1},
                      {float("nan"): 1}, {0: float("inf")}, {0: 0, 1: 0},
                      {0: 1e-320, 1: 1e-320}):
            with self.subTest(value=value), self.assertRaises((ValueError, TypeError)):
                speed._SpeedProfile(value)

    def test_budget(self):
        with self.assertRaisesRegex(ValueError, "budget"):
            speed._SpeedProfile({i / 5000: 1 for i in range(5001)}
            )

    def test_nonfinite_alpha_refused(self):
        for value in (float("inf"), float("nan"), True):
            with self.subTest(value=value), self.assertRaises((ValueError, TypeError)):
                speed._SpeedProfile().map(value)

    def test_authored_outer_curve_evaluated_once_per_sample(self):
        calls = []
        curve = speed._ClockCurve(speed._SpeedProfile({0: 0, 1: 2}),
                                  lambda a: calls.append(a) or a)
        self.assertEqual(calls, [])
        self.assertEqual(curve(0.5), 0.25)
        self.assertEqual(calls, [0.5])
        self.assertEqual(curve.last_value, 0.25)


class LifecycleTests(unittest.TestCase):
    def setUp(self):
        self.g = protocol()
        self.child = self.g.Animation()

    def test_published_identity_and_install_once(self):
        alias = self.g.ChangeSpeed
        method = alias.begin
        speed.install_speed(self.g)
        self.assertIs(alias, self.g.ChangeSpeed)
        self.assertIs(method, alias.begin)
        self.assertTrue(issubclass(alias, self.g.AnimationGroup))
        self.assertIsNone(alias._native_kind)

    def test_builder_prepared_once(self):
        builder = self.g.Builder(self.child)
        animation = self.g.ChangeSpeed(builder, {0: 2, 1: 2})
        self.assertIs(animation.anim, self.child)
        self.assertEqual(builder.calls, 1)
        self.assertEqual(animation.run_time, 1.0)
        self.assertEqual(self.child.run_time, 2.0)

    def test_shared_child_lifecycle_and_wall_clock_helpers(self):
        animation = self.g.ChangeSpeed(self.child, {0: 0, 1: 2})
        animation.begin()
        animation.update_mobjects(0.125)
        animation.interpolate(0.5)
        animation.finish()
        animation.finish()
        animation.clean_up_from_scene("scene")
        animation.clean_up_from_scene("scene")
        self.assertEqual(self.child.events,
                         ["begin", ("alpha", 0), ("dt", 0.125), ("alpha", 0.25),
                          "finish", ("cleanup", "scene")])

    def test_child_easing_applies_once_after_timeline_map(self):
        calls = []
        self.child.rate_func = lambda a: calls.append(a) or a * a
        animation = self.g.ChangeSpeed(self.child, {0: 0, 1: 2})
        self.assertEqual(calls, [])
        animation.begin()
        animation.interpolate(0.5)
        self.assertEqual(calls, [0, 0.25])
        self.assertEqual(self.child.events[-1], ("alpha", 0.0625))

    def test_child_rate_override_is_execution_scoped(self):
        old = self.child.rate_func
        replacement = lambda a: a * a
        animation = self.g.ChangeSpeed(self.child, {0: 1}, rate_func=replacement)
        self.assertIs(self.child.rate_func, old)
        animation.begin()
        self.assertIs(self.child.rate_func, replacement)
        animation.finish()
        self.assertIs(self.child.rate_func, old)
        animation.clean_up_from_scene(None)
        animation.begin()
        animation.abort()
        self.assertIs(self.child.rate_func, old)

    def test_nested_wrapper_restores_composite_rate_without_cycle(self):
        inner = self.g.ChangeSpeed(self.child, {0: 0, 1: 2})
        old = inner._speed_curve.easing
        outer = self.g.ChangeSpeed(inner, {0: 1}, rate_func=lambda a: a * a)
        outer.begin()
        outer.interpolate(0.5)
        self.assertEqual(self.child.events[-1], ("alpha", 0.0625))
        outer.finish()
        self.assertIs(inner._speed_curve.easing, old)
        self.assertEqual(inner.rate_func(0.5), 0.25)

    def test_scene_rate_assignment_preserves_profile(self):
        animation = self.g.ChangeSpeed(self.child, {0: 0, 1: 2})
        curve = animation.rate_func
        animation.rate_func = lambda a: a * a
        self.assertIs(animation.rate_func, curve)
        self.assertEqual(animation.rate_func(0.5), 0.0625)

    def test_failure_aborts_and_restores_child_without_finishing(self):
        old = self.child.rate_func
        animation = self.g.ChangeSpeed(self.child, {0: 1}, rate_func=lambda a: a)
        animation.begin()
        failure = RuntimeError("authored callback")
        self.child.failure = failure
        with self.assertRaises(RuntimeError) as caught:
            animation.interpolate(0.5)
        self.assertIs(caught.exception, failure)
        self.assertIs(self.child.rate_func, old)
        self.assertEqual(self.child.events[-1], "abort")
        self.assertNotIn("finish", self.child.events)
        self.assertIsNone(animation._composition_driver)

    def test_secondary_abort_error_does_not_replace_primary(self):
        animation = self.g.ChangeSpeed(self.child, {})
        animation.begin()
        failure = RuntimeError("primary")
        self.child.failure = failure
        def abort():
            raise ValueError("secondary")
        self.child.abort = abort
        with self.assertRaises(RuntimeError) as caught:
            animation.interpolate(0.5)
        self.assertIs(caught.exception, failure)
        self.assertTrue(any("ValueError" in note for note in failure.__notes__))

    def test_failed_cleanup_is_not_retried(self):
        calls = []
        def cleanup(scene):
            calls.append(scene)
            raise RuntimeError("publication failed")
        self.child.clean_up_from_scene = cleanup
        animation = self.g.ChangeSpeed(self.child, {})
        animation.begin()
        animation.finish()
        with self.assertRaises(RuntimeError):
            animation.clean_up_from_scene("scene")
        animation.clean_up_from_scene("scene")
        self.assertEqual(calls, ["scene"])
        self.assertIsNone(animation._composition_driver)

    def test_invalid_child_duration_and_explicit_duration(self):
        for duration in (-1, float("nan"), float("inf")):
            self.child.run_time = duration
            with self.assertRaises(ValueError):
                self.g.ChangeSpeed(self.child, {})
        self.child.run_time = 2
        self.assertEqual(self.g.ChangeSpeed(self.child, {0: 2}, run_time=3).run_time, 3)
        self.assertEqual(self.g.ChangeSpeed(self.child, {}, run_time=0).run_time, 0)
        for duration in (-1, True, float("inf")):
            with self.assertRaises((ValueError, TypeError)):
                self.g.ChangeSpeed(self.child, {}, run_time=duration)

    def test_explicit_seconds_remain_an_override_during_frame_binding(self):
        for duration in (0., 3.):
            with self.subTest(duration=duration):
                animation = self.g.ChangeSpeed(self.child, {0: 2.}, run_time=duration)
                self.assertEqual(animation.run_time, duration)
                self.assertFalse(animation._fmn_derived_run_time)

    def test_explicit_frames_remain_authoritative(self):
        for frames in (0, 5, 29):
            with self.subTest(frames=frames):
                animation = self.g.ChangeSpeed(self.child, {0: 5.}, run_time_frames=frames)
                self.assertEqual(animation.run_time_frames, frames)
                self.assertFalse(animation._fmn_derived_run_time)

    def test_conflicting_duration_units_fail_before_builder_preparation(self):
        builder = self.g.Builder(self.child)
        for seconds, frames in ((0., 0), (1., 5)):
            with self.subTest(seconds=seconds, frames=frames):
                with self.assertRaisesRegex(ValueError, "mutually exclusive"):
                    self.g.ChangeSpeed(builder, {}, run_time=seconds, run_time_frames=frames)
        self.assertEqual(builder.calls, 0)

    def test_frame_scale_preserves_seconds_arithmetic_until_requested(self):
        animation = self.g.ChangeSpeed(self.child, {0: 5.})
        self.assertEqual(animation._fmn_duration_scale(False), .2)
        with self.assertRaisesRegex(ValueError, "specify run_time_frames or run_time"):
            animation._fmn_duration_scale(True)

    def test_begin_refreshes_bound_child_duration_for_speed_updaters(self):
        animation = self.g.ChangeSpeed(self.child, {0: 2.}, run_time_frames=7)
        self.assertEqual(animation._speed_child_duration, 2.)
        for fps in (24, 25, 49, 60):
            with self.subTest(fps=fps):
                self.child._fmn_frame_timing = types.SimpleNamespace(seconds=12 / fps)
                animation.begin()
                self.assertEqual(animation._speed_child_duration, 12 / fps)
                animation.abort()

    def test_reuse_after_frame_binding_clears_refreshes_seconds_duration(self):
        animation = self.g.ChangeSpeed(self.child, {0: 2.}, run_time=3.)
        self.child._fmn_frame_timing = types.SimpleNamespace(seconds=.5)
        animation.begin()
        animation.abort()
        del self.child._fmn_frame_timing
        self.child.run_time = .25
        animation.begin()
        self.assertEqual(animation.run_time, 3.)
        self.assertEqual(animation._speed_child_duration, .25)
        animation.abort()

    def test_invalid_child_and_rate(self):
        with self.assertRaisesRegex(NotImplementedError, "clock-remap seam"):
            self.g.ChangeSpeed(None, {})
        with self.assertRaises(TypeError):
            self.g.ChangeSpeed(object(), {})
        with self.assertRaises(ValueError):
            self.g.ChangeSpeed(self.child, {}, rate_func="missing")
        with self.assertRaises(TypeError):
            self.g.ChangeSpeed(self.child, {}, rate_func=2)


class FrameBindingTests(unittest.TestCase):
    """Exercise production descriptor construction; no native clock is faked."""

    def setUp(self):
        from test_composition_lifecycle_protocol import environment, semantics, SOURCE
        self.g = environment()
        semantics._install_composition_lifecycle(self.g)
        self.g["ChangeSpeed"] = type("ChangeSpeed", (self.g["Animation"],), {})
        speed.install_speed(types.SimpleNamespace(**self.g))
        self.g["g"] = self.g
        self.g["_validate_frame_count"] = semantics._validate_frame_count
        self.g["_fmn_validate_frame_count"] = semantics._validate_frame_count
        install = next(node for node in ast.parse(SOURCE.read_text()).body
                       if isinstance(node, ast.FunctionDef) and node.name == "install")
        names = {"bind_frame_timing", "set_run_time", "set_run_time_frames"}
        nodes = [node for node in install.body if isinstance(node, ast.FunctionDef) and node.name in names]
        # ubs:ignore -- only fixed repository protocol definitions are executed.
        exec(compile(ast.Module(nodes, []), str(SOURCE), "exec"), self.g)
        for name in ("set_run_time", "set_run_time_frames"):
            setattr(self.g["Animation"], name, self.g[name])
        path = _SOURCE.with_name("live_rates.py")
        install = next(node for node in ast.parse(path.read_text()).body
                       if isinstance(node, ast.FunctionDef) and node.name == "install_live_rates")
        node = next(node for node in install.body
                    if isinstance(node, ast.FunctionDef) and node.name == "update_rate_info")
        # ubs:ignore -- only the fixed repository duration setter is executed.
        exec(compile(ast.Module([node], []), str(path), "exec"), self.g)
        self.g["Animation"].update_rate_info = self.g["update_rate_info"]
        self.planned = []

        def inspect_descriptor(descriptor, fps):
            self.planned.append((descriptor, fps))
            # This suite inspects argument transport only. Native extent
            # arithmetic and rendered counts belong to exact_frame_durations.py.
            return types.SimpleNamespace(seconds=0., frame_count=0)

        self.g["_frame_timing"] = inspect_descriptor
        self.g["_SceneCore"].get_frame_count = lambda scene: 0
        self.g["Scene"]._frame_rate = lambda scene: 24
        self.scene = self.g["Scene"]()

    def child(self, frames=12):
        animation = self.g["Animation"](self.g["Mobject"]())
        if frames is not None:
            animation.set_run_time_frames(frames)
        return animation

    def bind(self, animation):
        self.g["bind_frame_timing"](self.scene, (animation,))
        return animation._fmn_frame_descriptor

    def test_derived_wrapper_forwards_exact_scale(self):
        animation = self.g["ChangeSpeed"](self.child(), {0: 2.})
        descriptor = self.bind(animation)
        self.assertEqual(descriptor[:2], (None, None))
        self.assertEqual(descriptor[5], .5)
        self.assertEqual(descriptor[4][0][1], 12)

    def test_explicit_seconds_bypass_non_dyadic_scale(self):
        animation = self.g["ChangeSpeed"](self.child(), {0: 5.}, run_time=3.)
        descriptor = self.bind(animation)
        self.assertEqual(descriptor[:2], (3., None))
        self.assertEqual(len(descriptor), 5)

    def test_setters_and_rate_updates_replace_derived_duration(self):
        for method, options, expected in (
            ("set_run_time", (0.,), (0., None)),
            ("set_run_time_frames", (5,), (None, 5)),
            ("update_rate_info", {"run_time": 3.}, (3., None)),
            ("update_rate_info", {"run_time_frames": 7}, (None, 7)),
        ):
            with self.subTest(method=method, options=options):
                animation = self.g["ChangeSpeed"](self.child(), {0: 5.})
                if isinstance(options, dict):
                    getattr(animation, method)(**options)
                else:
                    getattr(animation, method)(*options)
                descriptor = self.bind(animation)
                self.assertEqual(descriptor[:2], expected)
                self.assertEqual(len(descriptor), 5)

    def test_non_dyadic_frame_profile_refuses_before_native_preparation(self):
        animation = self.g["ChangeSpeed"](self.child(5), {0: 5.})
        with self.assertRaisesRegex(ValueError, "specify run_time_frames or run_time"):
            self.bind(animation)
        self.assertEqual(self.planned, [])

    def test_seconds_child_keeps_float_scale_inside_frame_parent(self):
        wrapper = self.g["ChangeSpeed"](self.child(None), {0: 5.})
        outer = self.g["AnimationGroup"](wrapper).set_run_time_frames(7)
        descriptor = self.bind(outer)
        self.assertEqual(descriptor[4][0][5], .2)

    def test_unrelated_seconds_play_does_not_resolve_frame_scale(self):
        wrapper = self.g["ChangeSpeed"](self.child(None), {0: 5.})
        self.g["bind_frame_timing"](self.scene, (wrapper,))
        self.assertEqual(self.planned, [])
        self.assertEqual(wrapper.run_time, .2)


class FrameClockSamplingTests(unittest.TestCase):
    """Production driver sampling over the native planner's known 67-frame grid."""

    def setUp(self):
        self.g = protocol()
        path = _SOURCE.parents[1] / "manimlib_bootstrap.py"
        node = next(node for node in ast.parse(path.read_text()).body
                    if isinstance(node, ast.ClassDef) and node.name == "_CompositionCallbackDriver")
        namespace = {
            "_RATE_FUNC_NAMES": self.g._RATE_FUNC_NAMES,
            "_fmn_frame_identity_rate": self.g._fmn_frame_identity_rate,
            "_composition_timings": lambda group, children: ([(children[0], 0., 67.)], 67.),
        }
        # ubs:ignore -- only the fixed repository callback driver is executed.
        exec(compile(ast.Module([node], []), str(path), "exec"), namespace)
        self.Driver = namespace["_CompositionCallbackDriver"]
        self.samples = []
        self.timing = types.SimpleNamespace(
            intervals=[(0., 67.)], max_end=67., frame_intervals=True,
            alpha_at_frame=lambda frame, fps: frame / 67,
            rate_alpha_at_frame=lambda frame, fps: frame / 67,
            position_at_frame=lambda frame, fps: float(frame),
        )

    def driver(self, profile):
        animation = self.g.ChangeSpeed(self.g.Animation(), profile)
        animation._fmn_frame_timing = self.timing
        animation.time_span = None
        leaf = types.SimpleNamespace(_fmn_interpolate_at_frame=lambda frame, fps: self.samples.append(frame))
        return self.Driver(animation, [leaf]), animation.rate_func

    def test_constant_profile_retains_frame_boundary_and_observes_curve(self):
        for speed_value in (1., 2., .5):
            with self.subTest(speed=speed_value):
                driver, curve = self.driver({0: speed_value})
                driver._fmn_interpolate_at_frame(29, 24)
                self.assertEqual(self.samples[-1], 29.)
                self.assertEqual(curve.last_value, 29 / 67)

    def test_live_easing_changes_keep_the_authored_sample(self):
        driver, curve = self.driver({0: 2.})
        seen = []
        curve.easing = lambda alpha: seen.append(alpha) or alpha * alpha
        driver._fmn_interpolate_at_frame(29, 24)
        self.assertEqual(seen, [29 / 67])
        self.assertEqual(self.samples[-1], (29 / 67) ** 2 * 67)
        curve.easing = self.g._linear_rate
        driver._fmn_interpolate_at_frame(29, 24)
        self.assertEqual(self.samples[-1], 29.)

    def test_authored_curve_call_is_not_treated_as_catalog_identity(self):
        driver, curve = self.driver({0: 2.})
        seen = []
        original = speed._ClockCurve.__call__

        def authored(current, alpha):
            seen.append(alpha)
            return original(current, alpha) / 2

        with patch.object(speed._ClockCurve, "__call__", authored):
            driver._fmn_interpolate_at_frame(29, 24)
        self.assertEqual(seen, [29 / 67])
        self.assertEqual(self.samples[-1], (29 / 67) / 2 * 67)

    def test_authored_profile_map_is_not_treated_as_catalog_identity(self):
        driver, curve = self.driver({0: 2.})
        seen = []
        curve.profile.map = lambda alpha: seen.append(alpha) or alpha * alpha
        driver._fmn_interpolate_at_frame(29, 24)
        self.assertEqual(seen, [29 / 67])
        self.assertEqual(self.samples[-1], (29 / 67) ** 2 * 67)
        # Borrowing another profile's bound method has the shipped __func__
        # identity, but it is still an authored replacement of this map.
        curve.profile.map = speed._SpeedProfile({0: 0., 1: 2.}).map
        driver._fmn_interpolate_at_frame(29, 24)
        self.assertEqual(self.samples[-1], (29 / 67) ** 2 * 67)

    def test_nonconstant_profile_keeps_its_normalized_map(self):
        driver, curve = self.driver({0: 0., 1: 2.})
        driver._fmn_interpolate_at_frame(29, 24)
        self.assertEqual(self.samples[-1], (29 / 67) ** 2 * 67)


@unittest.skipUnless(os.environ.get("FMN_TEST_NATIVE") == "1", "requires installed native portal; set FMN_TEST_NATIVE=1")
class NativeSpeedTests(unittest.TestCase):
    def test_native_transform_and_clock(self):
        import manimlib as m
        from manimlib.animation.speed import ChangeSpeed
        scene, dot = m.Scene(), m.Dot()
        scene.add(dot)
        start = scene.time
        scene.play(ChangeSpeed(dot.animate(run_time=2, rate_func=m.linear).shift(m.RIGHT * 2),
                               {0: 2, 1: 2}))
        self.assertAlmostEqual(scene.time - start, 1.0, places=6)
        self.assertAlmostEqual(float(dot.get_center()[0]), 2.0, places=5)

    def test_native_nested_group_and_remover(self):
        import manimlib as m
        from manimlib.animation.speed import ChangeSpeed
        scene, left, right = m.Scene(), m.Dot(), m.Dot()
        scene.add(left, right)
        group = m.AnimationGroup(left.animate.shift(m.RIGHT), m.FadeOut(right), run_time=1)
        scene.play(ChangeSpeed(group, {0: 1, 1: 2}))
        self.assertAlmostEqual(float(left.get_center()[0]), 1.0, places=5)
        self.assertNotIn(right, scene.mobjects)


if __name__ == "__main__":
    unittest.main()
