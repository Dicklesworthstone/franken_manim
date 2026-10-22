"""Installed-extension easing acceptance over the real Scene clock and renderer."""
import math
from pathlib import Path
import tempfile
import unittest

import numpy as np
import manimlib as m
from fmn_python import render_session


def test_sixty_hz_curve_drives_actual_tracker_and_output():
    scene, tracker, dot = m.Scene(), m.ValueTracker(0), m.Dot()
    alphas, values = [], []
    def curve(t):
        alphas.append(float(t))
        return t + .02 * math.sin(30 * math.pi * t)
    def update(obj):
        value = float(tracker.get_value())
        values.append(value)
        obj.move_to(value * m.RIGHT)
    dot.add_updater(update)
    directory = Path(tempfile.mkdtemp(prefix="fmn-live-rates-"))
    with render_session(scene, directory / "motion.y4m", resolution=(32, 18), fps=60, threads=1) as output:
        scene.add(tracker, dot)
        scene.play(tracker.animate(run_time=1, rate_func=curve).set_value(1))
    assert any(np.isclose(t, 1/60, atol=1e-13) for t in alphas), alphas
    expected = 1/60 + .02
    assert any(np.isclose(value, expected, atol=1e-12) for value in values), values
    assert np.isclose(tracker.get_value(), 1)
    assert output.result.frame_count == 60
    assert (directory / "motion.y4m").read_bytes().startswith(b"YUV4MPEG2")
    print(f"retained live-rate output: {directory}")


def test_global_rate_is_not_probed_before_begin():
    began, phases = [False], []
    class Transform(m.Transform):
        def begin(self):
            began[0] = True
            super().begin()
    def curve(t):
        phases.append(began[0])
        return t*t
    square, scene = m.Square(), m.Scene()
    scene.add(square)
    scene.play(Transform(square, square.copy().shift(m.RIGHT)), run_time=.2, rate_func=curve)
    assert phases and all(phases), phases
    assert np.isclose(square.get_center()[0], 1)


def test_nested_custom_group_uses_existing_composition_driver():
    left, right, scene = m.Square(), m.Square().shift(3*m.RIGHT), m.Scene()
    seen = []
    def curve(t):
        seen.append(t)
        return t*t
    inner = m.AnimationGroup(m.Transform(left, left.copy().shift(m.UP)), rate_func=curve)
    outer = m.LaggedStart(inner, m.Transform(right, right.copy().shift(m.DOWN)))
    scene.add(left, right)
    scene.play(outer, run_time=.2)
    assert seen, "nested group curve was not executed"
    assert np.isclose(left.get_center()[1], 1)
    assert np.isclose(right.get_center()[1], -1)
    assert getattr(inner, "_composition_scene", None) is None


def test_catalog_spelling_on_authored_transform_path():
    square, scene = m.Square(), m.Scene()
    scene.add(square)
    scene.play(m.Transform(square, square.copy().shift(m.RIGHT), rate_func="linear",
                           path_func=lambda a,b,t:(1-t)*a+t*b), run_time=.1)
    assert np.isclose(square.get_center()[0], 1)


def test_unhashable_rate_object_uses_live_callback():
    class Curve:
        __hash__ = None
        def __eq__(self, other):
            raise AssertionError("rate equality must not be inspected")
        def __call__(self, t):
            return t*t
    square, scene = m.Square(), m.Scene()
    scene.add(square)
    scene.play(m.Transform(square, square.copy().shift(m.RIGHT), rate_func=Curve()), run_time=.1)
    assert np.isclose(square.get_center()[0], 1)


def test_authored_rate_failure_releases_native_animation_state():
    square, scene = m.Square(), m.Scene()
    scene.add(square)
    failure = ValueError("authored curve failed")
    def curve(t):
        if t > 0:
            raise failure
        return t
    try:
        scene.play(m.Transform(square, square.copy().shift(m.RIGHT), rate_func=curve,
                               suspend_mobject_updating=True), run_time=.1)
    except ValueError as caught:
        assert caught is failure
    else:
        raise AssertionError("the authored rate failure was lost")
    assert not square._is_updating_suspended()
    target = square.copy().shift(m.UP)
    scene.play(m.Transform(square, target), run_time=.1)
    assert np.allclose(square.get_center(), target.get_center())


for name, function in tuple(globals().items()):
    if name.startswith("test_") and callable(function):
        function()
print("live easing: 6 installed-extension cases passed")


class ExactRateDispatch(unittest.TestCase):
    def test_vector_fade_rates_follow_each_actual_frame(self):
        for fps in (24, 30, 60):
            for kind in (m.VFadeIn, m.VFadeOut, m.VFadeInThenOut):
                with self.subTest(fps=fps, kind=kind.__name__), tempfile.TemporaryDirectory() as tmp:
                    scene = m.Scene()
                    square = m.Square(fill_opacity=.8, stroke_opacity=.6)
                    samples, observed = [], []
                    animation = kind(square, run_time=.5)
                    def rate(t):
                        samples.append((float(t), getattr(animation, '_vector_fade_active', False)))
                        return t*t*t
                    animation.rate_func = rate
                    probe = m.Mobject()
                    probe.add_updater(lambda obj, dt: observed.append(
                        (square.get_fill_opacity(), square.get_stroke_opacity())) if dt > 0 else None)
                    with render_session(scene, Path(tmp) / 'fade.y4m', fps=fps,
                                        resolution=(32, 18), threads=1) as output:
                        scene.add(square, probe)
                        scene.play(animation)
                    self.assertEqual(output.result.frame_count, fps // 2)
                    self.assertEqual(len(observed), fps // 2)
                    self.assertTrue(samples and all(active for _, active in samples), samples)
                    expected = (np.arange(1, fps // 2 + 1) / (fps / 2))**3
                    if kind is m.VFadeOut:
                        expected = 1 - expected
                    np.testing.assert_allclose(observed, expected[:, None] * [.8, .6], atol=2e-6)
                    self.assertTrue(any(abs(t - 2 / fps) < 1e-13 for t, _ in samples))

    def test_mixed_play_global_rate_is_not_a_lookup_table(self):
        for fps in (24, 30, 60):
            with self.subTest(fps=fps), tempfile.TemporaryDirectory() as tmp:
                scene, fading, moving = m.Scene(), m.Square(fill_opacity=1), m.Square()
                moving.shift(3*m.LEFT)
                fade = m.VFadeIn(fading)
                samples, observed = [], []
                def rate(t):
                    samples.append((float(t), getattr(fade, '_vector_fade_active', False)))
                    return t*t
                probe = m.Mobject()
                probe.add_updater(lambda obj, dt: observed.append(
                    (fading.get_fill_opacity(), moving.get_center()[0])) if dt > 0 else None)
                with render_session(scene, Path(tmp) / 'mixed.y4m', fps=fps,
                                    resolution=(32, 18), threads=1):
                    scene.add(fading, moving, probe)
                    scene.play(fade, m.Transform(moving, moving.copy().shift(m.RIGHT)),
                               run_time=.5, rate_func=rate)
                self.assertTrue(samples and samples[0][1], samples)
                self.assertTrue(all(active or t == 1.0 for t, active in samples), samples)
                expected = (np.arange(1, fps // 2 + 1) / (fps / 2))**2
                np.testing.assert_allclose(observed, np.column_stack((expected, -3+expected)), atol=2e-6)

    def test_specialized_compositions_evaluate_custom_rate_during_begin(self):
        factories = (m.ShowCreationThenFadeOut, m.ShowCreationThenFadeAround,
                     m.ShowCreationThenDestructionAround)
        for factory in factories:
            for global_rate in (False, True):
                with self.subTest(factory=factory.__name__, global_rate=global_rate):
                    scene, square = m.Scene(), m.Square()
                    group = factory(square)
                    seen = []
                    def rate(t):
                        # Admission must not speculatively evaluate this closure.
                        seen.append((float(t), getattr(group, '_composition_scene', None)))
                        return t*t
                    options = {'run_time': .2}
                    if global_rate:
                        options['rate_func'] = rate
                    else:
                        group.rate_func = rate
                    scene.add(square)
                    scene.play(group, **options)
                    self.assertTrue(seen and all(owner is scene for _, owner in seen))
                    self.assertIsNone(getattr(group, '_composition_scene', None))

    def test_nested_specialized_group_keeps_children_native(self):
        scene, square = m.Scene(), m.Square()
        seen = []
        effect = m.ShowCreationThenFadeOut(square)
        def rate(t):
            seen.append((t, getattr(effect, '_composition_scene', None)))
            return t*t
        effect.rate_func = rate
        scene.add(square)
        scene.play(m.AnimationGroup(effect), run_time=.2)
        self.assertTrue(seen and all(owner is scene for _, owner in seen))
        self.assertNotIn(square, scene.mobjects)
        self.assertIsNone(getattr(effect, '_composition_scene', None))

    def test_catalog_rates_keep_the_native_route(self):
        # Negative control: adding live easing must not turn stock animations
        # into Python work merely because a callback implementation exists.
        for factory in (m.VFadeIn, m.VFadeOut, m.VFadeInThenOut,
                        m.ShowCreationThenFadeOut, m.ShowCreationThenFadeAround,
                        m.ShowCreationThenDestructionAround):
            with self.subTest(factory=factory.__name__):
                animation = factory(m.Square(), rate_func=m.linear)
                self.assertFalse(m._native._requires_python_animation(animation))

    def test_authored_rate_failure_preserves_exception_and_releases_state(self):
        for factory in (m.VFadeIn, m.ShowCreationThenFadeOut):
            with self.subTest(factory=factory.__name__), tempfile.TemporaryDirectory() as tmp:
                scene, square = m.Scene(), m.Square(fill_opacity=1)
                error = RuntimeError('rate failed during playback')
                def rate(t):
                    if t > 0:
                        raise error
                    return t
                animation = factory(square, rate_func=rate, suspend_mobject_updating=True)
                destination = Path(tmp) / 'failed.y4m'
                with self.assertRaises(RuntimeError) as caught:
                    with render_session(scene, destination, resolution=(32, 18), fps=24, threads=1):
                        scene.add(square)
                        scene.play(animation, run_time=.25)
                self.assertIs(caught.exception, error)
                self.assertFalse(destination.exists())
                self.assertFalse(square._is_updating_suspended())
                self.assertIsNone(getattr(animation, '_composition_scene', None))
                scene.play(m.VFadeIn(square), run_time=.25)
                retry = m.Scene()
                with render_session(retry, destination, resolution=(32, 18), fps=24, threads=1):
                    retry.add(m.Square(fill_opacity=1))
                    retry.wait(.25)
                self.assertTrue(destination.exists())



class UpdatersThroughTransformCopies(unittest.TestCase):
    """fm-k00v: values measured on the pinned Reference (6199a00d) in playback.

    Animation.update_mobjects updates the starting copy and the target, which carry the
    mobject's updaters, so an updater keeps acting through a Transform-family animation.
    """

    def play(self, construct, skip=False):
        with tempfile.TemporaryDirectory() as tmp:
            scene = m.Scene(skip_animations=skip)
            with render_session(scene, Path(tmp) / "updaters.y4m", resolution=(32, 18), fps=24, threads=1):
                return construct(scene)

    @staticmethod
    def fade_in(scene, **options):
        square = m.Square()
        square.add_updater(lambda mob, dt: mob.shift(dt * m.RIGHT))
        scene.play(m.FadeIn(square, **options))
        after = float(square.get_center()[0])
        scene.wait(1)
        return after, float(square.get_center()[0])

    @staticmethod
    def animate(scene):
        square = m.Square()
        scene.add(square)
        square.add_updater(lambda mob, dt: mob.shift(dt * m.UP))
        scene.play(square.animate.shift(m.RIGHT))
        return tuple(float(value) for value in square.get_center()[:2])

    @staticmethod
    def alpha_func(scene, suspend):
        square = m.Square()
        scene.add(square)
        square.add_updater(lambda mob, dt: mob.shift(dt * m.RIGHT))
        scene.play(m.UpdateFromAlphaFunc(square, lambda mob, alpha: None, suspend_mobject_updating=suspend))
        return float(square.get_center()[0])

    def test_updater_motion_continues_through_fade_in_and_animate(self):
        for skip in (False, True):  # BN-10: skip mode equals playback
            with self.subTest(skip=skip):
                np.testing.assert_allclose(self.play(self.fade_in, skip), (1.0, 2.0), atol=1e-6)
                np.testing.assert_allclose(self.play(self.animate, skip), (1.0, 1.0), atol=1e-6)

    def test_the_suspend_flag_freezes_where_the_reference_freezes(self):
        # The Reference copies before it suspends, so Transform-family motion still arrives
        # through the copies; an animation that interpolates no copy stays frozen.
        np.testing.assert_allclose(self.play(lambda scene: self.fade_in(
            scene, suspend_mobject_updating=True)), (1.0, 2.0), atol=1e-6)
        self.assertAlmostEqual(self.play(lambda scene: self.alpha_func(scene, True)), 0.0, places=6)
        self.assertAlmostEqual(self.play(lambda scene: self.alpha_func(scene, False)), 1.0, places=6)

    def test_only_an_updater_carrying_family_takes_the_callback_route(self):
        requires = m._native._requires_python_animation
        plain, carried = m.Square(), m.Square()
        carried.add_updater(lambda mob, dt: None)
        self.assertFalse(requires(m.FadeIn(plain)))
        self.assertTrue(requires(m.FadeIn(carried)))
        group = m.VGroup(m.Square(), carried.copy())
        self.assertTrue(requires(m.FadeIn(group)))


_result = unittest.TextTestRunner(verbosity=2).run(unittest.TestSuite(
    unittest.defaultTestLoader.loadTestsFromTestCase(case)
    for case in (ExactRateDispatch, UpdatersThroughTransformCopies)
))
if not _result.wasSuccessful():
    raise AssertionError("exact native-family easing acceptance failed")
