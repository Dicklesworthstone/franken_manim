"""Source/tail callback dispatch on native geometry and the shared Scene clock."""
import gc
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import numpy as np
import manimlib as m
from manimlib.mobject.changing import TracingTail as QualifiedTail


class DetachedTailTests(unittest.TestCase):
    def test_trace_and_tail_keep_definition_time_color_defaults(self):
        with patch.multiple(m._native, DEFAULT_MOBJECT_COLOR=m.RED, _WHITE=m.RED):
            for cls in (m.TracedPath, m.TracingTail):
                with self.subTest(cls=cls):
                    source = m.Dot()
                    trace = cls(source.get_center, time_per_anchor=.125)
                    source.shift(m.RIGHT)
                    trace.update(.125)
                    np.testing.assert_allclose(m.color_to_rgb(trace.get_stroke_color()),
                                               m.color_to_rgb(m.DEFAULT_MOBJECT_COLOR))
                    explicit = cls(source.get_center, stroke_color=m.YELLOW)
                    explicit.update(.125)
                    np.testing.assert_allclose(m.color_to_rgb(explicit.get_stroke_color()),
                                               m.color_to_rgb(m.YELLOW))

    def test_unadded_source_is_not_adopted_or_parented_at_construction(self):
        source = m.Dot(m.LEFT)
        tail = m.TracingTail(source, time_traced=.5, time_per_anchor=.125)
        self.assertFalse(source._is_bound())
        self.assertFalse(tail._is_bound())
        self.assertEqual(source.parents, [])
        self.assertNotIn(source, tail.get_family())
        self.assertIs(type(tail), QualifiedTail)
        self.assertIsInstance(tail, m.TracedPath)
        self.assertEqual(tail.get_num_points(), 0)
        self.assertEqual(len(tail.traced_points), 4)
        tail.update(0)
        self.assertEqual(tail.get_num_points(), 0)
        source.shift(2 * m.RIGHT)
        tail.update(.125)
        np.testing.assert_allclose(tail.get_start(), m.LEFT)
        np.testing.assert_allclose(tail.get_end(), m.RIGHT)

    def test_later_adoption_uses_the_same_live_source_and_scene_clock(self):
        scene, source = m.Scene(), m.Dot()
        tail = m.TracingTail(source, time_traced=.5, time_per_anchor=.125)
        scene.add(source, tail)
        before = scene.time
        scene.play(source.animate.shift(2 * m.RIGHT), run_time=.25, rate_func=m.linear)
        np.testing.assert_allclose(tail.get_end(), source.get_center(), atol=1e-6)
        self.assertAlmostEqual(tail.time, scene.time - before)
        self.assertEqual(scene.mobjects, [scene.frame, source, tail])
        self.assertLess(tail.get_stroke_widths()[0], tail.get_stroke_widths()[-1])
        self.assertLess(tail.get_stroke_opacities()[0], tail.get_stroke_opacities()[-1])

    def test_detached_object_and_equivalent_callback_have_identical_samples(self):
        source = m.Dot()
        direct = m.TracingTail(source, time_traced=.5, time_per_anchor=.125)
        callback = m.TracingTail(source.get_center, time_traced=.5, time_per_anchor=.125)
        for delta in (.125, .25, .125, .375):
            source.shift(delta * m.RIGHT + delta * .5 * m.UP)
            direct.update(delta)
            callback.update(delta)
            np.testing.assert_array_equal(direct.data, callback.data)
            self.assertEqual(direct._trace_anchors, callback._trace_anchors)
            self.assertEqual(direct.time, callback.time)

    def test_copied_tail_advances_its_own_history_not_the_original(self):
        source = m.Dot()
        tail = m.TracingTail(source, time_traced=.5, time_per_anchor=.125)
        source.shift(m.RIGHT)
        tail.update(.125)
        clone = tail.copy()
        before, history = tail.data.copy(), tail._trace_anchors
        source.shift(m.RIGHT)
        clone.update(.25)
        np.testing.assert_array_equal(tail.data, before)
        self.assertEqual(tail._trace_anchors, history)
        np.testing.assert_allclose(clone.get_end(), 2 * m.RIGHT)
        self.assertGreater(clone.time, tail.time)

    def test_existing_bound_source_keeps_the_native_tracer(self):
        scene, source = m.Scene(), m.Dot()
        scene.add(source)
        tail = m.TracingTail(source, time_traced=.5, time_per_anchor=.125)
        self.assertTrue(tail._is_bound())
        self.assertIs(tail._scene, scene)
        self.assertEqual(tail.updaters, [], "bound source lost its native updater")
        self.assertEqual(tail.get_num_points(), 0)
        scene.add(tail)
        scene.update_frame(0)
        self.assertEqual(tail.get_num_points(), 0)
        scene.play(source.animate.shift(m.RIGHT), run_time=.25, rate_func=m.linear)
        np.testing.assert_allclose(tail.get_end(), source.get_center(), atol=1e-6)

    def test_bound_tail_uses_elapsed_time_and_bounded_geometry(self):
        scene, source = m.Scene(), m.Dot()
        scene.add(source)
        tail = m.TracingTail(source, time_traced=.5, time_per_anchor=.125)
        scene.add(tail)
        elapsed = 0.
        for delta in (1e-9, .03125, .09375, .25, .375, 4.):
            elapsed += delta
            source.move_to(elapsed * m.RIGHT)
            scene.update_frame(delta)
            self.assertEqual(tail.updaters, [])
            self.assertLessEqual(tail.get_num_points(), 13)
            np.testing.assert_allclose(tail.get_start(),
                                       max(0., elapsed - .5) * m.RIGHT, atol=1e-6)
            np.testing.assert_allclose(tail.get_end(),
                                       elapsed * m.RIGHT, atol=1e-6)

    def test_authored_source_exception_preserves_history_and_allows_retry(self):
        failure = LookupError("authored center failed")
        class Source(m.Dot):
            fail = False
            def get_center(self):
                if self.fail:
                    raise failure
                return super().get_center()
        for bound in (False, True):
            with self.subTest(bound=bound):
                source = Source()
                if bound:
                    scene = m.Scene()
                    scene.add(source)
                tail = m.TracingTail(source, time_traced=.5, time_per_anchor=.125)
                before, history, clock = tail.data.copy(), tail._trace_anchors, tail.time
                source.fail = True
                with self.assertRaises(LookupError) as caught:
                    tail.update(.125)
                self.assertIs(caught.exception, failure)
                np.testing.assert_array_equal(tail.data, before)
                self.assertEqual(tail._trace_anchors, history)
                self.assertEqual(tail.time, clock)
                source.fail = False
                source.shift(m.UP)
                tail.update(.125)
                np.testing.assert_allclose(tail.get_end(), m.UP)

    def test_authored_center_is_independent_of_scene_add_order(self):
        class Source(m.Dot):
            def get_center(self):
                return super().get_center() + getattr(self, "trace_offset", 0.)
        expected_frames = None
        for bound in (False, True):
            for instance_override in (False, True):
                with self.subTest(bound=bound, instance_override=instance_override):
                    scene = m.Scene()
                    source = m.Dot(m.LEFT) if instance_override else Source(m.LEFT)
                    if instance_override:
                        center = source.get_center
                        source.get_center = lambda: center() + .5 * m.UP
                    else:
                        source.trace_offset = .5 * m.UP
                    if bound:
                        scene.add(source)
                    tail = m.TracingTail(source, time_traced=.5, time_per_anchor=.125)
                    callback = m.TracingTail(source.get_center, time_traced=.5, time_per_anchor=.125)
                    self.assertFalse(tail._is_bound(), "authored center was lowered to a Stage center")
                    scene.add(source, tail, callback)
                    frames = []
                    for delta in (.125, .25, .125, .375):
                        source.shift(delta * m.RIGHT)
                        scene.update_frame(delta)
                        np.testing.assert_allclose(tail.get_end(), source.get_center(), atol=1e-6)
                        np.testing.assert_array_equal(tail.data, callback.data)
                        self.assertEqual(tail._trace_anchors, callback._trace_anchors)
                        frames.append(tail.data.copy())
                    if expected_frames is None:
                        expected_frames = frames
                    else:
                        for actual, expected in zip(frames, expected_frames):
                            np.testing.assert_array_equal(actual, expected)

    def test_bound_source_preserves_tail_initialization_and_update_hooks(self):
        calls = []
        class Tail(m.TracingTail):
            def init_points(self):
                calls.append("init_points")
                return super().init_points()
            def update_path(self, dt):
                calls.append(("update", dt))
                return super().update_path(dt)
            def set_points_smoothly(self, points, *args, **kwargs):
                calls.append("geometry")
                return super().set_points_smoothly(points, *args, **kwargs)
            def set_stroke(self, *args, **kwargs):
                calls.append("style")
                return super().set_stroke(*args, **kwargs)
        scene, source = m.Scene(), m.Dot()
        scene.add(source)
        tail = Tail(source, time_traced=.5, time_per_anchor=.125)
        self.assertEqual(calls.count("init_points"), 1)
        self.assertEqual(tail.get_num_points(), 0)
        scene.add(tail)
        calls.clear()
        source.shift(m.RIGHT)
        scene.update_frame(.125)
        self.assertEqual(calls, [("update", .125), "geometry", "style"])
        np.testing.assert_allclose(tail.get_end(), source.get_center(), atol=1e-6)

    def test_bound_group_preserves_child_bounding_box_override(self):
        class Extent(m.Dot):
            def compute_bounding_box(self):
                return super().compute_bounding_box() + getattr(self, "trace_offset", 0.)
        source = m.Group(Extent())
        source[0].trace_offset = .5 * m.UP
        scene = m.Scene()
        scene.add(source)
        tail = m.TracingTail(source, time_traced=.5, time_per_anchor=.125)
        callback = m.TracingTail(source.get_center, time_traced=.5, time_per_anchor=.125)
        scene.add(tail, callback)
        source.shift(m.RIGHT)
        scene.update_frame(.125)
        np.testing.assert_allclose(tail.get_end(), m.RIGHT + .5 * m.UP, atol=1e-6)
        np.testing.assert_array_equal(tail.data, callback.data)

    def test_invalid_window_fails_without_binding_the_source(self):
        source = m.Dot()
        for controls in (dict(time_traced=-1), dict(time_traced=float("inf")),
                         dict(time_per_anchor=0), dict(time_traced=1e6, time_per_anchor=.001)):
            with self.subTest(controls=controls), self.assertRaises(ValueError):
                m.TracingTail(source, **controls)
            self.assertFalse(source._is_bound())
            self.assertEqual(source.parents, [])

    def test_rendered_detached_tail_matches_callback_at_every_frame_and_thread_count(self):
        def render(path, callback, threads):
            scene, source = m.Scene(), m.Dot(2 * m.LEFT)
            tail = m.TracingTail(source.get_center if callback else source,
                                  time_traced=.5, time_per_anchor=.125,
                                  stroke_color=m.WHITE, stroke_width=(0, 20))
            source.add_updater(lambda current, dt: current.shift(4 * dt * m.RIGHT), call=False)
            with scene.render_session(path, format="png_sequence", resolution=(96, 64), fps=8, threads=threads):
                scene.add(source, tail)
                scene.wait(.75)
            return [file.read_bytes() for file in sorted(Path(path).glob("*.png"))]
        with tempfile.TemporaryDirectory(prefix="fmn-detached-tail-") as directory:
            root = Path(directory)
            frames = render(root / "direct", False, 1)
            self.assertEqual(len(frames), 6)
            self.assertEqual(len(set(frames)), 6)
            self.assertEqual(frames, render(root / "callback", True, 1))
            self.assertEqual(frames, render(root / "four", False, 4))

    def test_rendered_bound_authored_center_matches_public_callback(self):
        class Source(m.Dot):
            def get_center(self):
                return super().get_center() + getattr(self, "trace_offset", 0.)
        def render(path, callback, threads):
            scene, source = m.Scene(), Source(2 * m.LEFT)
            source.trace_offset = .75 * m.UP
            scene.add(source)
            tail = m.TracingTail(source.get_center if callback else source,
                                 time_traced=.5, time_per_anchor=.125,
                                 stroke_color=m.WHITE, stroke_width=(0, 20))
            source.add_updater(lambda current, dt: current.shift(4 * dt * m.RIGHT), call=False)
            with scene.render_session(path, format="png_sequence", resolution=(96, 64), fps=8, threads=threads):
                scene.add(tail)
                scene.wait(.75)
            return [file.read_bytes() for file in sorted(Path(path).glob("*.png"))]
        with tempfile.TemporaryDirectory(prefix="fmn-bound-authored-tail-") as directory:
            root = Path(directory)
            expected = render(root / "callback", True, 1)
            self.assertEqual(len(expected), 6)
            self.assertEqual(len(set(expected)), 6)
            for threads in (1, 4, 16):
                self.assertEqual(expected, render(root / str(threads), False, threads))


suite = unittest.defaultTestLoader.loadTestsFromTestCase(DetachedTailTests)
assert suite.countTestCases() == 14, "detached-tail acceptance inventory changed"
result = unittest.TextTestRunner(verbosity=2).run(suite)
gc.collect()
if not result.wasSuccessful():
    raise AssertionError("detached-tail native acceptance failed")
