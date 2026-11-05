"""Public Scene hooks over the production adapter and explicit native-loop fixture.

These tests verify orchestration, not native clock/rendering. Installed-wheel
acceptance separately exercises the bootstrap's real hooks and native state.
"""
from __future__ import annotations

import ast
from fractions import Fraction
from pathlib import Path
from types import SimpleNamespace
import unittest

import numpy as np

from test_animation_updater_protocol import environment as animation_environment
from test_scene_execution_protocol import environment, execution


def progression_environment(fps=24):
    """Lift the public helpers and frame binder; native timing is an explicit fixture."""
    native = animation_environment()
    g = vars(native)
    root = Path(__file__).resolve().parents[1] / "python"
    trees = {}

    def install_function(path, parent_name, name):
        if path not in trees:
            trees[path] = ast.parse(path.read_text())
        tree = trees[path]
        parent = next(node for node in tree.body
                      if isinstance(node, (ast.ClassDef, ast.FunctionDef))
                      and node.name == parent_name)
        node = next(node for node in parent.body
                    if isinstance(node, ast.FunctionDef) and node.name == name)
        exec(compile(ast.Module([node], []), str(path), "exec"), g)
        return g[name]

    class Scene:
        def __init__(self, frame_rate=fps):
            self._fps = frame_rate
            self.camera = SimpleNamespace(fps=frame_rate)
            self.skip_animations = False
            self.num_plays = 0
            self._step_context = None

        def _frame_rate(self):
            return self._fps

        def _adopt(self, mobject):
            for member in mobject.get_family():
                member._scene = self

    def frame_timing(descriptor, rate):
        seconds, frames, span, lag, children = descriptor
        del lag
        if children is not None:
            raise AssertionError("this timing fixture only covers leaf progressions")
        count = (Fraction(frames) if frames is not None
                 else Fraction.from_float(seconds) * rate)
        if span is not None:
            count = max(count, Fraction.from_float(float(span[1])) * rate)
        return SimpleNamespace(seconds=float(count / rate),
                               frame_count=-(-count.numerator // count.denominator),
                               duration_ratio=(count.numerator, count.denominator),
                               position_from_ratio=lambda numerator, denominator, fps: Fraction(numerator, denominator),
                               alpha_at_frame=lambda frame, fps: float(min(Fraction(1), max(Fraction(0), frame / count)))
                               if count else 1.)

    g.update(_np=np, Group=native.AnimationGroup, _frame_timing=frame_timing,
             _fmn_validate_frame_count=g["_validate_frame_count"],
             _SceneCore=SimpleNamespace(get_frame_count=lambda scene: 0,
                                       _frame_step_context=lambda scene: scene._step_context))
    native.Animation.get_run_time = install_function(
        root / "fmn_python/live_rates.py", "install_live_rates", "get_run_time")
    g["_fmn_bind_frame_timing"] = install_function(
        root / "manimlib/_animation_semantics.py", "install", "bind_frame_timing")
    for name in ("get_time_progression", "get_run_time", "get_animation_time_progression",
                 "get_wait_time_progression"):
        setattr(Scene, name, install_function(root / "manimlib_bootstrap.py", "Scene", name))
    native.Scene = Scene
    return native


class FrameProgressionTests(unittest.TestCase):
    """Production Python routing only; exact_frame_durations.py covers native timing."""

    def test_unbound_and_bound_frames_do_not_round_trip_seconds(self):
        native = progression_environment()
        scene = native.Scene()
        animation = native.Animation(native.Mobject(), run_time_frames=43)
        self.assertEqual(animation.run_time, 1.)
        animation.begin = lambda: self.fail("progression lookup must not begin animation")
        for _ in range(2):
            samples = scene.get_animation_time_progression([animation])
            self.assertEqual(len(samples), 43)
            np.testing.assert_array_equal(samples, [frame / 24 for frame in range(1, 44)])
            self.assertEqual(animation._fmn_frame_timing.frame_count, 43)

    def test_seconds_compatibility_keeps_existing_arange_samples(self):
        native = progression_environment()
        scene = native.Scene()
        for seconds, count in ((1., 24), (43 / 24, 44), (0., 0)):
            animation = native.Animation(native.Mobject(), run_time=seconds)
            expected = np.arange(0., seconds, 1 / 24) + 1 / 24
            for samples in (scene.get_time_progression(seconds),
                            scene.get_wait_time_progression(seconds),
                            scene.get_animation_time_progression([animation])):
                self.assertEqual(len(samples), count)
                np.testing.assert_array_equal(samples, expected)

    def test_frame_rates_zero_and_active_clock_projection(self):
        native = progression_environment()
        for fps in (1, 7, 24, 25, 30, 60, 120):
            scene = native.Scene(fps)
            scene.camera.fps = fps + 1  # The native clock owns frame units.
            for frames in (0, 1, 29, 43):
                animation = native.Animation(native.Mobject(), run_time_frames=frames)
                samples = scene.get_animation_time_progression([animation])
                self.assertEqual(len(samples), frames)
                np.testing.assert_array_equal(samples, [index / fps for index in range(1, frames + 1)])

    def test_mixed_root_animations_cover_literal_seconds_exactly(self):
        native = progression_environment(30)
        scene = native.Scene()
        exact = native.Animation(native.Mobject(), run_time_frames=3)
        seconds = native.Animation(native.Mobject(), run_time=.1)
        self.assertEqual(len(scene.get_animation_time_progression([exact])), 3)
        self.assertEqual(len(scene.get_animation_time_progression([seconds])), 3)
        # .1 as binary64 is greater than three exact frames at 30 fps.
        self.assertEqual(len(scene.get_animation_time_progression([exact, seconds])), 4)
        exact.run_time_frames = 7
        self.assertEqual(len(scene.get_animation_time_progression([seconds, exact])), 7)

    def test_rebinding_frame_duration_recomputes_time_span_on_each_grid(self):
        native = progression_environment()
        animation = native.Animation(native.Mobject(), run_time_frames=12, time_span=(0., .75))
        for fps, expected in ((24, 18), (12, 12), (60, 45)):
            self.assertEqual(len(native.Scene(fps).get_animation_time_progression([animation])), expected)
            self.assertEqual(animation.run_time_frames, 12)

    def test_frame_wait_skip_condition_and_zero(self):
        native = progression_environment()
        scene = native.Scene()
        scene.skip_animations = True
        self.assertEqual(scene.get_wait_time_progression(run_time_frames=43), [43 / 24])
        self.assertEqual(scene.get_wait_time_progression(run_time_frames=0), [])
        self.assertEqual(scene.get_time_progression(run_time_frames=0), [])
        zero = native.Animation(native.Mobject(), run_time_frames=0)
        self.assertEqual(scene.get_animation_time_progression([zero]), [])
        self.assertEqual(scene.get_time_progression(0.), [0.])
        samples = scene.get_wait_time_progression(run_time_frames=43, stop_condition=lambda: True)
        self.assertEqual(len(samples), 43)
        self.assertEqual(len(scene.get_wait_time_progression(
            run_time_frames=0, stop_condition=lambda: True)), 0)
        self.assertEqual(len(scene.get_time_progression(run_time_frames=(1 << 53) + 1)), 1)

    def test_frame_options_share_validation_and_reject_seconds_ambiguity(self):
        native = progression_environment()
        scene = native.Scene()
        for method in (scene.get_time_progression, scene.get_wait_time_progression):
            for frames, error in ((True, TypeError), (3., TypeError), (-1, ValueError),
                                  (1 << 63, OverflowError)):
                with self.subTest(method=method.__name__, frames=frames), self.assertRaises(error):
                    method(run_time_frames=frames)
            with self.assertRaises(ValueError):
                method(.1, run_time_frames=3)
            self.assertEqual(len(method(run_time_frames=np.int64(3))), 3)

    def test_seconds_override_drops_prior_frame_binding(self):
        native = progression_environment(30)
        scene = native.Scene()
        animation = native.Animation(native.Mobject(), run_time_frames=3)
        scene.get_animation_time_progression([animation])
        animation.run_time, animation.run_time_frames = .1, None
        self.assertEqual(len(scene.get_animation_time_progression([animation])), 3)
        self.assertFalse(hasattr(animation, "_fmn_frame_timing"))

    def test_direct_seconds_assignment_clears_even_an_equal_frame_projection(self):
        native = progression_environment(30)
        scene = native.Scene()
        animation = native.Animation(native.Mobject(), run_time_frames=3)
        scene.get_animation_time_progression([animation])
        self.assertEqual(animation.run_time, .1)
        animation.run_time = .1
        self.assertIsNone(animation.run_time_frames)
        self.assertFalse(hasattr(animation, "_fmn_frame_timing"))
        self.assertFalse(animation._fmn_derived_run_time)


class HookTests(unittest.TestCase):
    def setUp(self):
        self.g = environment()
        self.scene = self.g.Scene()
        self.mob = self.g.Mobject()
        self.events = []
        self.scene.pre_play = lambda: self.events.append("pre")
        def post():
            self.events.append("post")
            self.scene.num_plays += 1
        self.scene.post_play = post

    def anim(self, **kwargs):
        return self.g.Animation(self.mob, **kwargs)

    def test_prepare_rate_info_and_hooks_surround_actual_animation_lifecycle(self):
        anim = self.anim()
        prepare = self.g.prepare_animation
        def prepare_logged(item):
            self.events.append("prepare")
            return prepare(item)
        self.g.prepare_animation = prepare_logged
        anim.update_rate_info = lambda **kw: self.events.append(("rate-info", kw))
        anim.event = self.events.append
        self.assertEqual(self.scene.play(anim, run_time=.25, lag_ratio=0), [.25])
        self.assertEqual(self.events, [
            "prepare", ("rate-info", {"run_time": .25, "lag_ratio": 0.0}), "pre",
            "begin", ("helpers", .25), "interpolate", "finish", "cleanup", "post",
        ])
        self.assertEqual(self.scene.num_plays, 1)

    def test_builder_is_resolved_once_before_pre_hook_edits_product(self):
        builder, anim = self.g._AnimationBuilder(self.mob), self.anim()
        calls = []
        def prepare(item):
            calls.append(item)
            if item is not builder:
                self.fail("builder product reconstructed")
            return anim
        self.g.prepare_animation = prepare
        self.scene.pre_play = lambda: setattr(anim, "on_interpolate", lambda: self.events.append("edited"))
        self.scene.play(builder)
        self.assertEqual(calls, [builder])
        self.assertIs(self.scene._last_specs[0], anim)
        self.assertEqual(self.events, ["edited", "post"])

    def test_live_base_hook_changes_and_cooperative_subclasses_are_observed(self):
        events, Base = self.events, self.g.Scene
        class Authored(Base):
            def pre_play(self):
                events.append("authored-pre")
                super().pre_play()
            def post_play(self):
                events.append("authored-post")
                super().post_play()
        scene = Authored()
        Base.pre_play = lambda self: events.append("patched-base")
        scene.play(self.anim())
        self.assertEqual(events, ["authored-pre", "patched-base", "authored-post"])
        self.assertEqual(scene.num_plays, 1)

    def test_failed_execution_does_not_post_or_increment_count(self):
        with self.assertRaises(LookupError):
            self.scene.play(self.anim(fail="interpolate"))
        self.assertEqual(self.events, ["pre"])
        self.assertEqual(self.scene.num_plays, 0)

    def test_failed_pre_hook_never_begins_animation_or_runs_post(self):
        error, anim = LookupError("pre refused"), self.anim()
        def pre():
            raise error
        self.scene.pre_play = pre
        with self.assertRaises(LookupError) as result:
            self.scene.play(anim)
        self.assertIs(result.exception, error)
        self.assertEqual(self.scene.events, [])
        self.assertEqual(anim.events, [])
        self.assertEqual(self.events, [])
        self.assertNotIn(execution._OWNER_KEY, vars(self.scene))

    def test_post_hook_failure_is_not_retried_or_turned_into_animation_abort(self):
        anim, error = self.anim(), LookupError("post refused")
        anim.abort = lambda: self.fail("completed animation aborted")
        def post():
            self.events.append("failed-post")
            raise error
        self.scene.post_play = post
        with self.assertRaises(LookupError) as result:
            self.scene.play(anim)
        self.assertIs(result.exception, error)
        self.assertEqual(self.events, ["pre", "failed-post"])
        self.assertEqual(anim.events.count("finish"), 1)
        self.assertEqual(anim.events.count("cleanup"), 1)
        self.assertFalse(self.mob.suspended)

    def test_empty_play_is_not_a_segment(self):
        self.assertIsNone(self.scene.play())
        self.assertEqual(self.events, [])
        self.assertEqual(self.scene.events, [])
        self.assertEqual(self.scene.num_plays, 0)

    def test_wait_and_early_condition_each_count_one_completed_segment(self):
        predicate_calls = []
        self.scene.play(self.anim())
        self.scene.wait(.1, stop_condition=lambda: predicate_calls.append(1) or True)
        self.assertEqual(predicate_calls, [1])
        self.assertEqual(self.scene.num_plays, 2)
        self.assertEqual(self.events, ["pre", "post", "pre", "post"])

    def test_exclusive_end_gate_can_stop_play_and_wait_before_backend_work(self):
        class EndScene(Exception):
            pass
        def pre():
            if self.scene.num_plays >= 2:
                raise EndScene("end reached")
        self.scene.pre_play = pre
        self.scene.play(self.anim())
        self.scene.wait(.1)
        before = list(self.scene.events)
        for operation in (lambda: self.scene.play(self.anim()), lambda: self.scene.wait(.1)):
            with self.assertRaises(EndScene):
                operation()
        self.assertEqual(self.scene.events, before)
        self.assertEqual(self.scene.num_plays, 2)

    def test_invalid_play_options_and_animation_types_are_rejected_before_hooks(self):
        cases = [((), {"unknown": 1}), ((object(),), {}),
                 ((self.anim(),), {"run_time": -1}), ((self.anim(),), {"run_time": float("nan")}),
                 ((self.anim(),), {"lag_ratio": float("inf")}), ((self.anim(),), {"rate_func": 7})]
        for args, kwargs in cases:
            with self.subTest(kwargs=kwargs), self.assertRaises((TypeError, ValueError, NotImplementedError)):
                self.scene.play(*args, **kwargs)
        self.assertEqual(self.events, [])
        self.assertEqual(self.scene.events, [])

    def test_invalid_wait_arguments_are_rejected_before_hooks(self):
        for kwargs in ({"duration": -1}, {"duration": float("inf")}, {"stop_condition": 1}, {"unknown": 1}):
            with self.subTest(kwargs=kwargs), self.assertRaises((TypeError, ValueError, NotImplementedError)):
                self.scene.wait(**kwargs)
        self.assertEqual(self.events, [])
        self.assertEqual(self.scene.events, [])

    def test_internal_controller_keyword_names_are_not_public_options(self):
        for key in ("hooks", "prepare", "operation", "args", "kwargs"):
            with self.subTest(key=key), self.assertRaises(TypeError):
                self.scene.play(self.anim(), **{key: False})
            with self.subTest(key=key), self.assertRaises(NotImplementedError):
                self.scene.wait(**{key: False})
        self.assertEqual(self.events, [])

    def test_wait_default_and_note_options_reach_the_original_operation(self):
        g = environment(False)
        calls = []
        def original(self, *args, **kwargs):
            calls.append((args, kwargs))
            return "wait result"
        g.Scene.wait = original
        execution.install_scene_execution(g)
        scene = g.Scene()
        scene.default_wait_time = .75
        marker, condition = object(), lambda: True
        self.assertEqual(scene.wait(stop_condition=condition, note=marker, ignore_presenter_mode=True), "wait result")
        self.assertEqual(calls, [((.75, condition), {"note": marker, "ignore_presenter_mode": True})])
        self.assertEqual(scene.num_plays, 1)

    def test_private_native_entry_remains_hook_free(self):
        anim = self.anim()
        self.scene._play_animations([object()], [anim], None, None, None, None)
        self.assertEqual(self.events, [])
        self.assertEqual(self.scene.num_plays, 0)

    def test_invalid_builder_product_never_enters_pre_hook(self):
        self.g.prepare_animation = lambda item: None
        with self.assertRaisesRegex(TypeError, "must return an Animation"):
            self.scene.play(self.g._AnimationBuilder(self.mob))
        self.assertEqual(self.events, [])
        self.assertNotIn(execution._OWNER_KEY, vars(self.scene))

    def test_same_scene_hook_reentry_is_rejected_but_other_scene_is_allowed(self):
        self.scene.pre_play = lambda: self.scene.wait(.1)
        with self.assertRaisesRegex(RuntimeError, "another play/wait"):
            self.scene.play(self.anim())
        self.assertEqual(self.scene.events, [])
        other = self.g.Scene()
        self.scene.pre_play = lambda: other.wait(.1)
        self.scene.play(self.anim())
        self.assertEqual(other.num_plays, 1)
        self.assertEqual(self.scene.num_plays, 1)

    def test_authored_rate_info_failure_propagates_without_running_pre_hook(self):
        anim, error = self.anim(), ValueError("rate-info refused")
        def update(**kwargs):
            raise error
        anim.update_rate_info = update
        with self.assertRaises(ValueError) as result:
            self.scene.play(anim)
        self.assertIs(result.exception, error)
        self.assertEqual(self.events, [])
        self.assertEqual(self.scene.events, [])

    def test_wait_failure_preserves_exception_without_post_hook(self):
        error = KeyboardInterrupt()
        def predicate():
            raise error
        with self.assertRaises(KeyboardInterrupt) as result:
            self.scene.wait(.1, stop_condition=predicate)
        self.assertIs(result.exception, error)
        self.assertEqual(self.events, ["pre"])
        self.assertEqual(self.scene.num_plays, 0)
        self.assertNotIn(execution._OWNER_KEY, vars(self.scene))


if __name__ == "__main__":
    unittest.main()
