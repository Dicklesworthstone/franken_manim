"""Installed-wheel builder playback over real native geometry and clocks."""
import numpy as np
import manimlib as m


def close(actual, expected):
    assert np.allclose(actual, expected, atol=2e-5), (actual, expected)


def authored_path():
    scene, square, samples, calls = m.Scene(), m.Square(), [], []
    scene.add(square)
    square.add_updater(lambda obj: samples.append(tuple(obj.get_center())), call=False)
    def path(start, end, alpha):
        calls.append(alpha)
        return (1 - alpha) * start + alpha * end + 4 * alpha * (1 - alpha) * m.UP
    scene.play(square.animate(run_time=.25, rate_func=m.linear, path_func=path).shift(2 * m.RIGHT))
    close(square.get_center(), 2 * m.RIGHT)
    assert calls and max(point[1] for point in samples) > .5


def final_alpha():
    scene, square = m.Scene(), m.Square()
    scene.add(square)
    scene.play(square.animate(run_time=.125, rate_func=m.linear, final_alpha_value=.25).shift(4 * m.RIGHT))
    close(square.get_center(), m.RIGHT)
    assert square in scene.mobjects


def removal():
    scene, square = m.Scene(), m.Square()
    scene.add(square)
    scene.play(square.animate(run_time=.125, rate_func=m.linear, remover=True).shift(m.RIGHT))
    assert square not in scene.mobjects
    close(square.get_center(), m.RIGHT)


def timed_window_and_suspension():
    scene, square, samples = m.Scene(), m.Square(), []
    scene.add(square)
    # fm-k00v: the animation's starting copy and target carry this updater as well, as in the
    # Reference; the window is about the square itself.
    square.add_updater(lambda obj: samples.append((scene.get_time(), obj.get_x())) if obj is square else None,
                       call=False)
    scene.play(square.animate(run_time=.25, rate_func=m.linear, time_span=(.125, .25)).shift(4 * m.RIGHT))
    assert len(samples) >= 4
    assert all(abs(x) < 2e-5 for time, x in samples if 0 < time < .125), samples
    close(square.get_x(), 4)
    calls = []
    square.clear_updaters()
    square.add_updater(lambda obj, dt: calls.append(dt) if obj is square else None, call=False)
    scene.play(square.animate(run_time=.125, rate_func=m.linear, suspend_mobject_updating=True).shift(m.RIGHT))
    assert calls and all(dt == 0 for dt in calls), calls
    assert not square._is_updating_suspended()


def nested_builder():
    scene, left, right = m.Scene(), m.Square(), m.Square().shift(3 * m.RIGHT)
    scene.add(left, right)
    scene.play(m.AnimationGroup(
        left.animate(run_time=.125, rate_func=m.linear, final_alpha_value=.5).shift(2 * m.UP),
        right.animate(run_time=.125, rate_func=m.linear).shift(m.RIGHT),
    ))
    close(left.get_center(), m.UP)
    close(right.get_center(), 4 * m.RIGHT)


def build_override():
    scene, square, seen = m.Scene(), m.Square(), []
    scene.add(square)
    target = square.copy().shift(m.RIGHT)
    product = m.Transform(square, target, run_time=.125, rate_func=m.linear)
    class Builder(m._AnimationBuilder):
        def build(self):
            seen.append(self)
            return product
    builder = Builder(square)
    scene.play(builder)
    assert seen == [builder]
    close(square.get_center(), m.RIGHT)


def explicit_transform_endpoint():
    scene, square = m.Scene(), m.Square()
    scene.add(square)
    scene.play(m.Transform(square, square.copy().shift(2 * m.RIGHT),
                           run_time=.125, rate_func=m.linear, final_alpha_value=.5))
    close(square.get_center(), m.RIGHT)


def mixed_camera_builder():
    scene, square = m.Scene(), m.Square()
    scene.add(square)
    frame = scene.frame
    core = frame._core
    scene.play(frame.animate(run_time=.125, rate_func=m.linear).shift(m.RIGHT),
               square.animate(run_time=.125, rate_func=m.linear, final_alpha_value=.5).shift(2 * m.UP))
    close(frame.get_center(), m.RIGHT)
    close(square.get_center(), m.UP)
    assert scene.frame is frame and frame._core is core


def authored_transform_lifecycle():
    # The endpoints deliberately differ, but authored interpolate is a no-op.
    # A native endpoint lerp would visibly move the square and skip the hooks.
    scene, square, events = m.Scene(), m.Square(), []
    scene.add(square)
    before = square.get_points().copy()

    class StationaryTransform(m.Transform):
        def begin(self):
            events.append("begin")
            super().begin()

        def interpolate(self, alpha):
            events.append(("interpolate", float(alpha)))

        def update_mobjects(self, dt):
            events.append(("update", float(dt)))
            super().update_mobjects(dt)

        def finish(self):
            events.append("finish")
            super().finish()

        def clean_up_from_scene(self, owner):
            events.append("cleanup")
            super().clean_up_from_scene(owner)

    animation = StationaryTransform(square, square.copy().shift(2 * m.RIGHT),
                                    run_time=.25, rate_func=m.linear)
    scene.play(animation)
    assert np.array_equal(square.get_points(), before)
    assert events[0] == "begin" and events[-1] == "cleanup", events
    assert "finish" in events
    assert any(isinstance(event, tuple) and event[0] == "update" and event[1] > 0
               for event in events), events
    assert any(isinstance(event, tuple) and event[0] == "interpolate" and 0 < event[1] < 1
               for event in events), events
    assert not square._is_updating_suspended()


def authored_mobject_inside_stock_transform():
    for grouped in (False, True):
        samples = []

        class ArchedSquare(m.Square):
            def interpolate(self, start, end, alpha, path_func=None):
                result = super().interpolate(start, end, alpha, path_func)
                self.shift(4 * alpha * (1 - alpha) * m.UP)
                samples.append((float(alpha), tuple(self.get_center())))
                return result

        scene, square, circle = m.Scene(), ArchedSquare(), m.Circle().shift(3 * m.RIGHT)
        root = m.VGroup(square, circle)
        scene.add(root)
        animation = m.Transform(root, root.copy().shift(2 * m.RIGHT),
                                run_time=.25, rate_func=m.linear)
        entry = m.AnimationGroup(animation) if grouped else animation
        scene.play(entry)
        close(square.get_center(), 2 * m.RIGHT)
        close(circle.get_center(), 5 * m.RIGHT)
        assert samples and max(center[1] for _, center in samples) > .5, samples
        assert any(0 < alpha < 1 for alpha, _ in samples), samples
        assert not square._is_updating_suspended()


def stock_transform_keeps_native_admission():
    native = getattr(m, "_native", m)
    root = m.VGroup(m.Square(), m.Circle().shift(3 * m.RIGHT))
    animation = m.Transform(root, root.copy().shift(m.RIGHT))
    assert not native._requires_python_animation(animation)


for case in (authored_path, final_alpha, removal, timed_window_and_suspension,
             nested_builder, build_override, explicit_transform_endpoint, mixed_camera_builder,
             authored_transform_lifecycle, authored_mobject_inside_stock_transform,
             stock_transform_keeps_native_admission):
    case()
    print("builder playback acceptance:", case.__name__)

# A native Transform is legal only when its objects' lifecycle is native too.
# These are actual-extension tests; the control shapes are built independently.
import tempfile as _hooks_tempfile
import unittest as _hooks_unittest
from pathlib import Path as _HooksPath
from types import MethodType as _HookMethod


class TransformObjectHooks(_hooks_unittest.TestCase):
    def test_object_lifecycle_hooks_select_callbacks_without_admission_calls(self):
        for name in ("copy", "is_aligned_with", "align_data_and_family", "align_family",
                     "align_data", "align_points", "has_updaters", "lock_matching_data",
                     "lock_data", "lock_uniforms", "unlock_data", "set_animating_status",
                     "suspend_updating", "resume_updating", "update"):
            with self.subTest(hook=name):
                calls = []
                def hook(self, *args, _name=name, **kwargs):
                    calls.append(_name)
                    return getattr(super(Authored, self), _name)(*args, **kwargs)
                Authored = type("Authored", (m.Square,), {name: hook})
                square = Authored()
                animation = m.Transform(square, m.Square().shift(m.RIGHT),
                                        suspend_mobject_updating=True)
                calls.clear()
                self.assertTrue(m._requires_python_animation(animation))
                self.assertEqual(calls, [], "route selection must not execute object hooks")
                scene = m.Scene().add(square)
                scene.play(animation, run_time=.1, rate_func=m.linear)
                self.assertIn(name, calls)
                self.assertFalse(square._is_updating_suspended())

    def test_authored_starting_copy_changes_every_sample(self):
        copies, samples = [], []
        class Authored(m.Square):
            def copy(self, *args, **kwargs):
                copies.append(self)
                return super().copy(*args, **kwargs).shift(m.UP)
        square, target, scene = Authored(), m.Square().shift(m.RIGHT), m.Scene()
        probe = m.Mobject()
        probe.add_updater(lambda obj, dt: samples.append((scene.time, square.get_center().copy()))
                          if dt > 0 else None, call=False)
        scene.add(square, probe)
        scene.play(m.Transform(square, target), run_time=.125, rate_func=m.linear)
        self.assertEqual(copies, [square])
        self.assertEqual(len(samples), 4)
        for time, center in samples:
            alpha = min(time/.125, 1.)
            np.testing.assert_allclose(center, [alpha, 1-alpha, 0], atol=2e-6)
        np.testing.assert_allclose(square.get_center(), m.RIGHT, atol=1e-6)

    def test_unaligned_target_copy_is_authored_without_mutating_target(self):
        copies = []
        class Target(m.Triangle):
            def copy(self, *args, **kwargs):
                copies.append(self)
                return super().copy(*args, **kwargs).shift(m.UP)
        square, target, scene = m.Square(), Target().shift(m.RIGHT), m.Scene()
        before = target.get_points().copy()
        expected = m.Triangle().shift(m.RIGHT + m.UP)
        self.assertFalse(square.is_aligned_with(target))
        scene.play(m.Transform(square, target), run_time=.1, rate_func=m.linear)
        self.assertTrue(copies and copies[0] is target)
        np.testing.assert_array_equal(target.get_points(), before)
        np.testing.assert_allclose(square.get_center(), expected.get_center(), atol=1e-6)

    def test_instance_copy_patch_is_seen_between_plays(self):
        square, scene = m.Square(), m.Scene()
        scene.add(square)
        first = m.Transform(square, m.Square().shift(m.RIGHT))
        self.assertFalse(m._requires_python_animation(first))
        scene.play(first, run_time=.1)
        calls = []
        original = square.copy
        def copy(self, *args, **kwargs):
            calls.append(self)
            return original(*args, **kwargs)
        square.copy = _HookMethod(copy, square)
        second = m.Transform(square, m.Square())
        self.assertTrue(m._requires_python_animation(second))
        scene.play(second, run_time=.1)
        self.assertEqual(calls, [square])

    def test_deep_shared_families_detect_object_hooks(self):
        class Authored(m.Square):
            def update(self, *args, **kwargs):
                return super().update(*args, **kwargs)
        child = Authored()
        root = m.Group(m.Group(child), m.Group(child))
        animation = m.Transform(root, root.copy())
        self.assertTrue(m._requires_python_animation(animation))
        m.Scene().play(animation, run_time=.1)
        self.assertIs(root[0][0], root[1][0])

    def test_target_family_descriptor_is_not_evaluated_for_admission(self):
        class Authored(m.Square):
            @property
            def copy(self):
                raise AssertionError("copy descriptor ran while choosing a route")
        target = Authored()
        animation = m.Transform(m.Square(), m.Group(target))
        self.assertTrue(m._requires_python_animation(animation))

    def test_late_base_copy_patch_is_seen_through_shipped_override(self):
        original = m.Mobject.copy
        calls = []
        def copy(self, *args, **kwargs):
            calls.append(self)
            return original(self, *args, **kwargs)
        source = m.Square()
        animation = m.Transform(source, m.Square().shift(m.UP))
        try:
            m.Mobject.copy = copy
            self.assertTrue(m._requires_python_animation(animation))
            m.Scene().play(animation, run_time=.1)
            self.assertIn(source, calls)
        finally:
            m.Mobject.copy = original

    def test_stock_objects_and_unmodified_subclasses_stay_native(self):
        class Plain(m.Square):
            pass
        for source in (m.Square(), Plain(), m.Circle(), m.Group(m.Square(), m.Circle()),
                       m.ValueTracker(0)):
            with self.subTest(kind=type(source).__name__):
                animation = m.Transform(source, source.copy())
                self.assertFalse(m._requires_python_animation(animation))

    def test_reinitialization_does_not_recapture_authored_methods(self):
        from fmn_python.initialization import initialize
        native = getattr(m, "_native", m)
        self.assertNotIn("_fmn_finalize_transform_dispatch", vars(native))
        square = m.Square()
        original = square.copy
        square.copy = lambda *args, **kwargs: original(*args, **kwargs)
        animation = m.Transform(square, m.Square())
        self.assertTrue(m._requires_python_animation(animation))
        self.assertIs(initialize(native), native)
        self.assertTrue(m._requires_python_animation(animation))
        self.assertNotIn("_fmn_finalize_transform_dispatch", vars(native))

    def test_copy_failure_keeps_original_error_and_releases_transients(self):
        error = RuntimeError("authored snapshot failure")
        class Authored(m.Square):
            fail = True
            def copy(self, *args, **kwargs):
                if self.fail:
                    raise error
                return super().copy(*args, **kwargs)
        square, scene = Authored(), m.Scene()
        scene.add(square)
        before = square.get_points().copy()
        with self.assertRaises(RuntimeError) as caught:
            scene.play(m.Transform(square, m.Square(), suspend_mobject_updating=True), run_time=.1)
        self.assertIs(caught.exception, error)
        self.assertFalse(square._is_updating_suspended())
        self.assertFalse(square.is_changing())
        np.testing.assert_array_equal(square.get_points(), before)
        square.fail = False
        scene.play(m.Transform(square, m.Square().shift(m.UP)), run_time=.1)
        np.testing.assert_allclose(square.get_center(), m.UP, atol=1e-6)

    def test_cleanup_failure_keeps_original_error_and_recovers(self):
        error = RuntimeError("authored unlock failure")
        class Authored(m.Square):
            fail = True
            def unlock_data(self):
                super().unlock_data()
                if self.fail:
                    raise error
                return self
        square, scene = Authored(), m.Scene()
        scene.add(square)
        with self.assertRaises(RuntimeError) as caught:
            scene.play(m.Transform(square, m.Square().shift(m.RIGHT)), run_time=.1)
        self.assertIs(caught.exception, error)
        self.assertFalse(square._is_updating_suspended())
        self.assertFalse(square.is_changing())
        square.fail = False
        scene.play(m.Transform(square, m.Square()), run_time=.1)
        np.testing.assert_allclose(square.get_center(), m.ORIGIN, atol=1e-6)

    def test_nested_succession_uses_live_object_snapshots(self):
        calls = []
        class Authored(m.Square):
            def copy(self, *args, **kwargs):
                calls.append(self.get_center().copy())
                return super().copy(*args, **kwargs)
        square, scene = Authored(), m.Scene()
        scene.add(square)
        animation = m.Succession(m.Transform(square, m.Square().shift(m.RIGHT), run_time=.1),
                                 m.Transform(square, m.Square().shift(2*m.RIGHT), run_time=.1))
        scene.play(m.AnimationGroup(animation), rate_func=m.linear)
        self.assertTrue(any(np.allclose(value, m.ORIGIN) for value in calls))
        self.assertTrue(any(np.allclose(value, m.RIGHT) for value in calls))
        np.testing.assert_allclose(square.get_center(), 2*m.RIGHT, atol=1e-6)

    def test_rendered_snapshots_match_independent_native_control(self):
        class Authored(m.Square):
            def copy(self, *args, **kwargs):
                return super().copy(*args, **kwargs).shift(m.UP)
        def render(path, kind, threads):
            scene = m.Scene()
            with scene.render_session(path, format="y4m", resolution=(64, 40), fps=24,
                                      threads=threads) as output:
                source = Authored() if kind == "authored" else m.Square()
                if kind == "control":
                    source.shift(m.UP)
                scene.add(source)
                scene.play(m.Transform(source, m.Square().shift(m.RIGHT)),
                           run_time=.125, rate_func=m.linear)
            self.assertEqual(output.result.frame_count, 3)
            return path.read_bytes()
        with _hooks_tempfile.TemporaryDirectory() as tmp:
            root = _HooksPath(tmp)
            expected = render(root/"control.y4m", "control", 1)
            for threads in (1, 4, 16):
                self.assertEqual(render(root/f"authored-{threads}.y4m", "authored", threads), expected)
            self.assertNotEqual(render(root/"unchanged.y4m", "unchanged", 1), expected)
            payloads = expected.split(b"FRAME\n")[1:]
            self.assertEqual(len(payloads), 3)
            self.assertNotEqual(payloads[0], payloads[1])


class BuilderOverrideOptions(_hooks_unittest.TestCase):
    def test_custom_override_uses_builder_timing_easing_and_lifecycle(self):
        events, token = [], object()
        class Source(m.VectorizedPoint):
            def travel(self, distance):
                return self.shift(distance * m.RIGHT)
        class Owned(m.Transform):
            def __init__(self, source, distance):
                events.append("constructor")
                super().__init__(source, source.copy().shift(distance * m.RIGHT),
                                 run_time=7, rate_func=m.linear)
                self.token = token
            def finish(self):
                events.append("finish")
                return super().finish()
        @m.override_animate(Source.travel)
        def travel_animation(source, distance):
            events.append(("factory", distance))
            return Owned(source, distance)
        source, scene = Source(), m.Scene()
        ticks = []
        source.add_updater(lambda obj, dt: ticks.append(dt) if obj is source else None, call=False)
        rate = lambda alpha: alpha * alpha
        builder = source.animate(run_time_frames=4, rate_func=rate, final_alpha_value=.5,
                                 time_span=(0, .5), suspend_mobject_updating=True,
                                 remover=True, name="custom trip").travel(4)
        animation = builder.build()
        self.assertIs(animation, builder.overridden_animation)
        self.assertIs(animation.token, token)
        self.assertEqual(animation.name, "custom trip")
        with _hooks_tempfile.TemporaryDirectory() as directory:
            with scene.render_session(_HooksPath(directory) / "override.y4m",
                                      resolution=(16, 8), fps=8, threads=1) as output:
                scene.add(source)
                scene.play(builder)
            self.assertEqual(output.result.frame_count, 4)
        self.assertEqual(scene.time, .5)
        close(source.get_center(), m.RIGHT)
        self.assertNotIn(source, scene.mobjects)
        self.assertFalse(source._is_updating_suspended())
        self.assertTrue(ticks and all(dt == 0 for dt in ticks), ticks)
        self.assertEqual(events, [("factory", 4), "constructor", "finish"])
        self.assertIs(animation.token, token)

    def test_override_composition_retains_members_and_exact_frame_duration(self):
        class Source(m.VectorizedPoint):
            def perform(self):
                return self
        source, second, scene = Source(), m.VectorizedPoint(m.UP), m.Scene()
        children = (
            m.Transform(source, source.copy().shift(m.RIGHT), run_time_frames=2, rate_func=m.linear),
            m.Transform(second, second.copy().shift(2*m.RIGHT), run_time_frames=4, rate_func=m.linear),
        )
        product = m.AnimationGroup(*children)
        root = product.mobject
        Source.perform._override_animate = lambda obj: product
        builder = source.animate(run_time_frames=3, rate_func=m.linear, lag_ratio=1).perform()
        self.assertIs(builder.build(), product)
        self.assertEqual(tuple(product.animations), children)
        self.assertIs(product.mobject, root)
        self.assertFalse(product._fmn_derived_run_time)
        seen = []
        observer = m.Mobject()
        observer.add_updater(lambda obj, dt: seen.append((source.get_x(), second.get_x())) if dt > 0 else None,
                             call=False)
        with _hooks_tempfile.TemporaryDirectory() as directory:
            with scene.render_session(_HooksPath(directory) / "group.y4m",
                                      resolution=(16, 8), fps=12, threads=1) as output:
                scene.add(root, observer)
                scene.play(builder)
            self.assertEqual(output.result.frame_count, 3)
        self.assertEqual(scene.time, .25)
        np.testing.assert_allclose(seen, [(1, 0), (1, 1), (1, 2)], atol=2e-5)
        self.assertIs(product.mobject, root)

    def test_override_lag_recomputes_derived_duration_before_nesting(self):
        class Source(m.VectorizedPoint):
            def perform(self):
                return self
        source, other = Source(), m.VectorizedPoint(m.UP)
        product = m.AnimationGroup(
            m.Transform(source, source.copy().shift(m.RIGHT), run_time=.125, rate_func=m.linear),
            m.Transform(other, other.copy().shift(m.RIGHT), run_time=.25, rate_func=m.linear))
        Source.perform._override_animate = lambda obj: product
        self.assertEqual(product.get_run_time(), .25)
        builder = source.animate(lag_ratio=1).perform()
        self.assertIs(builder.build(), product)
        self.assertTrue(product._fmn_derived_run_time)
        self.assertEqual(product.get_run_time(), .375)
        self.assertEqual([row[1:] for row in product.anims_with_timings], [(0, .125), (.125, .375)])
        parent = m.AnimationGroup(builder)
        self.assertEqual(parent.get_run_time(), .375)
        scene = m.Scene()
        scene.play(parent)
        close(source.get_center(), m.RIGHT)
        close(other.get_center(), m.UP + m.RIGHT)

    def test_override_group_remover_uses_its_existing_cleanup(self):
        class Source(m.VectorizedPoint):
            def perform(self):
                return self
        source, scene = Source(), m.Scene()
        product = m.AnimationGroup(m.Transform(source, source.copy().shift(m.RIGHT), rate_func=m.linear))
        root = product.mobject
        Source.perform._override_animate = lambda obj: product
        builder = source.animate(run_time_frames=2, remover=True).perform()
        self.assertIs(builder.build(), product)
        self.assertTrue(m._requires_python_animation(product))
        scene.add(root)
        scene.play(builder)
        close(source.get_center(), m.RIGHT)
        self.assertNotIn(root, scene.mobjects)
        self.assertFalse(source._is_updating_suspended())

    def test_play_options_still_override_builder_and_factory_timing(self):
        class Source(m.VectorizedPoint):
            def perform(self):
                return self
        source, scene = Source(), m.Scene()
        product = m.Transform(source, source.copy().shift(m.RIGHT), run_time_frames=9)
        Source.perform._override_animate = lambda obj: product
        builder = source.animate(run_time=.5, rate_func=m.linear).perform()
        self.assertIs(builder.build(), product)
        self.assertIsNone(product.run_time_frames)
        scene.play(builder, run_time_frames=3)
        self.assertEqual(scene.time, .1)
        close(source.get_center(), m.RIGHT)

    def test_transform_override_paths_reach_native_and_callback_playback(self):
        class Source(m.VectorizedPoint):
            def perform(self):
                return self
        arc = m.path_along_arc(m.PI / 2)
        def custom(start, end, alpha):
            return (1-alpha)*start + alpha*end + 4*alpha*(1-alpha)*m.UP
        for options, path, callback in (
            ({"path_arc": m.PI / 2}, arc, False),
            ({"path_func": arc}, arc, False),
            ({"path_func": custom}, custom, True),
        ):
            with self.subTest(options=options):
                source, scene, seen = Source(), m.Scene(), []
                product = m.Transform(source, source.copy().shift(2*m.RIGHT))
                Source.perform._override_animate = lambda obj: product
                builder = source.animate(run_time_frames=4, rate_func=m.linear, **options).perform()
                self.assertIs(builder.build(), product)
                self.assertEqual(m._requires_python_animation(product), callback)
                observer = m.Mobject()
                observer.add_updater(lambda obj, dt: seen.append(source.get_center().copy()) if dt > 0 else None,
                                     call=False)
                scene.add(source, observer)
                scene.play(builder)
                expected = [path(np.zeros((1, 3)), np.array([2*m.RIGHT]), alpha)[0]
                            for alpha in (.25, .5, .75, 1)]
                np.testing.assert_allclose(seen, expected, atol=2e-5)


_hooks_result = _hooks_unittest.TextTestRunner(verbosity=2).run(_hooks_unittest.TestSuite(
    _hooks_unittest.defaultTestLoader.loadTestsFromTestCase(case)
    for case in (TransformObjectHooks, BuilderOverrideOptions)))
if not _hooks_result.wasSuccessful():
    raise AssertionError("native Transform object-hook acceptance failed")
