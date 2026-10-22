"""Installed-extension authored easing over the production renderer, not a 30-Hz table.

Fades, a `.animate` builder Transform and a lagged group's members each ease with the authored
curve at every frame the clock samples, at several frame rates (fm-5wq.13).

Run against the freshly built wheel: python crates/fmn-python/tests/live_fade_rates.py
Missing native dependencies are errors, not skipped acceptance cases.
"""
from pathlib import Path
import tempfile
import unittest

import numpy as np
import manimlib as m
from fmn_python import render_session


class LiveFadeRates(unittest.TestCase):
    def test_stock_fades_sample_the_actual_frame_clock(self):
        for fps in (24, 60, 120):
            # FadeInFromPoint is the Reference's FadeIn with a scale/shift
            # start (fading.py:71); the portal-only FadeInFromLarge is gone.
            for kind, extra in ((m.FadeIn, ()), (m.FadeOut, ()), (m.FadeInFromPoint, (2 * m.UP,))):
                with self.subTest(fps=fps, kind=kind.__name__), tempfile.TemporaryDirectory() as tmp:
                    scene = m.Scene()
                    square = m.Square(fill_opacity=.8, stroke_opacity=.6)
                    animation = kind(square, *extra, run_time=.5)
                    sampled, observed = [], []

                    def rate(t):
                        sampled.append(float(t))
                        return t ** 3

                    animation.rate_func = rate
                    probe = m.Mobject()
                    probe.add_updater(lambda obj, dt: observed.append(
                        (square.get_fill_opacity(), square.get_stroke_opacity())) if dt > 0 else None)
                    with render_session(scene, Path(tmp) / "fade.y4m", fps=fps,
                                        resolution=(32, 18), threads=1) as output:
                        scene.add(square, probe)
                        scene.play(animation)
                    self.assertEqual(output.result.frame_count, fps // 2)
                    self.assertEqual(len(observed), fps // 2)
                    expected = (np.arange(1, fps // 2 + 1) / (fps / 2)) ** 3
                    if kind is m.FadeOut:
                        expected = 1 - expected
                    np.testing.assert_allclose(observed, expected[:, None] * [.8, .6], atol=2e-6)
                    self.assertTrue(any(abs(t - 2 / fps) < 1e-13 for t in sampled), sampled)
                    self.assertFalse(square._is_updating_suspended())
                    self.assertEqual(square in scene.mobjects, kind is not m.FadeOut)

    def test_global_curve_is_live_for_a_mixed_fade_and_transform(self):
        for fps in (24, 60, 120):
            with self.subTest(fps=fps), tempfile.TemporaryDirectory() as tmp:
                scene, fading, moving = m.Scene(), m.Square(fill_opacity=1), m.Square()
                moving.shift(3 * m.LEFT)
                observed = []
                probe = m.Mobject()
                probe.add_updater(lambda obj, dt: observed.append(
                    (fading.get_fill_opacity(), moving.get_center()[0])) if dt > 0 else None)
                with render_session(scene, Path(tmp) / "mixed.y4m", fps=fps,
                                    resolution=(32, 18), threads=1):
                    scene.add(fading, moving, probe)
                    scene.play(m.FadeIn(fading), m.Transform(moving, moving.copy().shift(m.RIGHT)),
                               run_time=.5, rate_func=lambda t: t ** 3)
                expected = (np.arange(1, fps // 2 + 1) / (fps / 2)) ** 3
                np.testing.assert_allclose(observed, np.column_stack((expected, -3 + expected)), atol=2e-6)

    def test_builder_transform_eases_with_the_authored_curve_at_each_frame(self):
        for fps in (24, 30, 60):
            with self.subTest(fps=fps), tempfile.TemporaryDirectory() as tmp:
                scene, square = m.Scene(), m.Square()
                observed = []
                probe = m.Mobject()
                probe.add_updater(lambda obj, dt: observed.append(square.get_center()[0]) if dt > 0 else None)
                with render_session(scene, Path(tmp) / "builder.y4m", fps=fps,
                                    resolution=(32, 18), threads=1):
                    scene.add(square, probe)
                    scene.play(square.animate.shift(2 * m.RIGHT), rate_func=lambda t: t ** 3, run_time=.5)
                frames = np.arange(1, fps // 2 + 1) / (fps / 2)
                # float32 point storage. A 1/30-grid table interpolated between samples would
                # miss t**3 by ~1e-3 per unit of travel at 24 and 60 fps.
                np.testing.assert_allclose(observed, 2 * frames ** 3, atol=1e-6)

    def test_lagged_members_ease_on_their_own_windows_at_each_frame(self):
        for fps in (24, 30, 60):
            with self.subTest(fps=fps), tempfile.TemporaryDirectory() as tmp:
                scene = m.Scene()
                first, second = m.Square(fill_opacity=1), m.Circle(fill_opacity=1)
                observed = []
                probe = m.Mobject()
                probe.add_updater(lambda obj, dt: observed.append(
                    (first.get_fill_opacity(), second.get_fill_opacity())) if dt > 0 else None)
                with render_session(scene, Path(tmp) / "lagged.y4m", fps=fps,
                                    resolution=(32, 18), threads=1):
                    scene.add(first, second, probe)
                    scene.play(m.LaggedStart(m.FadeIn(first, rate_func=lambda t: t ** 3),
                                             m.FadeIn(second, rate_func=lambda t: t ** 3),
                                             lag_ratio=0.5), run_time=.75)
                # Reference AnimationGroup timing: run_time-1 members start at 0 and 0.5, so
                # max_end_time is 1.5. The group's alpha maps to time = 1.5 * alpha, and member
                # i eases clip(time - 0.5 * i, 0, 1). The clock emits ceil(fps * run_time) frames
                # and clamps the last alpha to 1.
                alpha = np.minimum(np.arange(1, int(np.ceil(fps * .75)) + 1) / (fps * .75), 1)
                expected = np.column_stack([np.clip(1.5 * alpha - 0.5 * i, 0, 1) ** 3 for i in (0, 1)])
                np.testing.assert_allclose(observed, expected, atol=2e-6)


if __name__ == "__main__":
    unittest.main()
