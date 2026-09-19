"""Actual installed-portal graph sampling, updater, geometry and output acceptance.

No native substitute and no missing-extension skip. Run this against a wheel
built from the matching source, or through scripts/check_portal_runtime.sh.
"""
from __future__ import annotations

import math
from pathlib import Path
import tempfile
import unittest

import manimlib as m
import numpy as np

from fmn_python import render_scene


def y4m_frames(data):
    header, separator, tail = data.partition(b'\n')
    if not separator or not header.startswith(b'YUV4MPEG2 '):
        raise AssertionError('expected native Y4M output')
    fields = header.split()[1:]
    width = int(next(value[1:] for value in fields if value.startswith(b'W')))
    height = int(next(value[1:] for value in fields if value.startswith(b'H')))
    chroma = next(value for value in fields if value.startswith(b'C'))
    if chroma not in {b'C420', b'C420jpeg', b'C420mpeg2'} or width % 2 or height % 2:
        raise AssertionError('expected 8-bit 4:2:0 output with even dimensions')
    size, position, frames = width * height * 3 // 2, 0, []
    while position < len(tail):
        if tail[position:position + 6] != b'FRAME\n':
            raise AssertionError('bad native frame marker')
        start = position + 6
        frame = tail[start:start + size]
        if len(frame) != size:
            raise AssertionError('truncated native frame')
        frames.append(frame)
        position = start + size
    return width, height, frames


class LiveGraphingAcceptance(unittest.TestCase):
    def axes(self):
        return m.Axes(x_range=(-2, 2, 1), y_range=(-2, 2, 1), width=8, height=4)

    def test_scalar_math_function_updates_on_the_native_scene_clock(self):
        axes = self.axes()
        amplitude = m.ValueTracker(.5)
        calls = []
        def function(x):
            self.assertIs(type(x), float)
            calls.append(x)
            return amplitude.get_value() * math.sin(x)
        graph = axes.get_graph(function, bind=True, color=m.BLUE)
        scene = m.Scene()
        scene.add(graph)
        calls.clear()
        before = graph.get_points().copy()
        scene.play(amplitude.animate.set_value(1.5), run_time=.25)
        self.assertTrue(calls)
        self.assertFalse(np.array_equal(graph.get_points(), before))
        np.testing.assert_allclose(axes.i2gp(.5, graph), axes.c2p(.5, 1.5 * math.sin(.5)), atol=1e-5)
        self.assertEqual(len(graph.updaters), 1)

    def test_moving_discontinuity_keeps_full_domain_and_disconnected_native_paths(self):
        axes = self.axes()
        cut = m.ValueTracker(0)
        graph = axes.get_graph(lambda x: x, color=m.YELLOW, stroke_width=6)
        graph.epsilon = .01
        axes.bind_graph_to_func(graph, lambda xs: np.where(xs < cut.get_value(), -.75, .75),
                                jagged=False, get_discontinuities=lambda: [cut.get_value()])
        scene = m.Scene()
        scene.add(graph)
        for value in (0., .5, -.75, 1., 0., 1.25):
            cut.set_value(value)
            scene.wait(.125)
            paths = graph.get_subpaths()
            self.assertEqual(len(paths), 2)
            self.assertAlmostEqual(axes.p2c(paths[0][0])[0], -2., places=4)
            self.assertAlmostEqual(axes.p2c(paths[-1][-1])[0], 2., places=4)
            self.assertLess(axes.p2c(paths[0][-1])[0], value)
            self.assertGreater(axes.p2c(paths[-1][0])[0], value)
            self.assertTrue(np.isfinite(graph.get_points()).all())
            np.testing.assert_allclose(graph.get_stroke_widths(), 6)

    def test_disappearing_domain_preserves_empty_style_edits_on_recovery(self):
        axes = self.axes()
        hidden = [True]
        graph = axes.get_graph(lambda x: x, color=m.BLUE, stroke_width=5)
        graph.epsilon = 10
        axes.bind_graph_to_func(graph, lambda xs: xs,
                                get_discontinuities=lambda: [0.] if hidden[0] else [])
        graph.update(0)
        self.assertEqual(graph.get_num_points(), 0)
        graph.set_stroke(color=m.YELLOW, width=7, opacity=.6)
        hidden[0] = False
        graph.update(0)
        self.assertGreater(graph.get_num_points(), 2)
        np.testing.assert_allclose(graph.get_stroke_widths(), 7)
        np.testing.assert_allclose(graph.get_stroke_opacities(), .6)

    def test_geometric_queries_read_native_curves_and_skip_gaps(self):
        axes = self.axes()
        left = m.VMobject().set_points_as_corners([axes.c2p(-2, -1), axes.c2p(-.5, -1)])
        right = m.VMobject().set_points_as_corners([axes.c2p(.5, 1), axes.c2p(2, 1)])
        left.add_subpath(right.get_points())
        np.testing.assert_allclose(axes.i2gp(-1, left), axes.c2p(-1, -1), atol=1e-5)
        np.testing.assert_allclose(axes.i2gp(1, left), axes.c2p(1, 1), atol=1e-5)
        self.assertIsNone(axes.i2gp(0, left))
        self.assertIsNone(axes.i2gp(3, left))
        # The x data range need not contain any [0,1] curve parameter at all.
        positive = m.Axes(x_range=(10, 20, 1), y_range=(0, 10, 1))
        line = m.VMobject().set_points_as_corners([positive.c2p(10, 0), positive.c2p(20, 10)])
        np.testing.assert_allclose(positive.i2gp(12, line), positive.c2p(12, 2), atol=1e-5)

    def test_native_quadratic_and_reversed_geometric_lookup(self):
        axes = self.axes()
        graph = m.VMobject()
        graph.set_points([axes.c2p(0, 0), axes.c2p(.5, 0), axes.c2p(2, 2)])
        for x in (.1, .5, 1., 1.8):
            t = (-1 + math.sqrt(1 + 4 * x)) / 2
            np.testing.assert_allclose(axes.i2gp(x, graph), axes.c2p(x, 2 * t * t), atol=2e-5)
        graph.reverse_points()
        np.testing.assert_allclose(axes.i2gp(1, graph), axes.c2p(1, 2 * ((math.sqrt(5) - 1) / 2) ** 2), atol=2e-5)

    def test_native_callback_failure_preserves_last_graph_and_next_wait_recovers(self):
        axes = self.axes()
        should_fail = [False]
        failure = LookupError('live graph callback failed')
        def function(xs):
            if should_fail[0]:
                raise failure
            return np.sin(xs)
        graph = axes.get_graph(math.sin)
        axes.bind_graph_to_func(graph, function)
        scene = m.Scene()
        scene.add(graph)
        scene.wait(.125)
        before = graph.get_points().copy()
        should_fail[0] = True
        with self.assertRaises(LookupError) as raised:
            scene.wait(.125)
        self.assertIs(raised.exception, failure)
        np.testing.assert_array_equal(graph.get_points(), before)
        should_fail[0] = False
        scene.wait(.125)
        self.assertTrue(np.isfinite(graph.get_points()).all())

    def test_copy_and_unbind_keep_native_object_ownership(self):
        axes = self.axes()
        factor = [1.]
        graph = axes.get_graph(lambda x: factor[0] * x, bind=True)
        copied = graph.copy(deep=True)
        before = graph.get_points().copy()
        factor[0] = .5
        copied.update(0)
        np.testing.assert_array_equal(graph.get_points(), before)
        self.assertFalse(np.array_equal(copied.get_points(), before))
        axes.unbind_graph_from_func(copied)
        factor[0] = 2.
        copied.update(0)
        np.testing.assert_allclose(axes.i2gp(1, copied), axes.c2p(1, .5), atol=1e-5)
        self.assertEqual(len(graph.updaters), 1)
        self.assertEqual(len(copied.updaters), 0)

    def test_real_native_frames_change_and_match_across_thread_counts(self):
        class WaveScene(m.Scene):
            def construct(self):
                axes = m.Axes(x_range=(-2, 2, 1), y_range=(-2, 2, 1), width=8, height=4)
                amplitude = m.ValueTracker(.25)
                graph = axes.get_graph(lambda x: amplitude.get_value() * math.sin(2 * x),
                                       bind=True, color=m.WHITE, stroke_width=8)
                self.add(graph)
                self.wait(.125)
                self.play(amplitude.animate.set_value(1.25), run_time=1, rate_func=m.linear)
        with tempfile.TemporaryDirectory(prefix='fmn-live-graphing-') as directory:
            artifacts = []
            for threads in (1, 4):
                path = Path(directory) / f'wave-{threads}.y4m'
                receipt = render_scene(WaveScene, path, format='y4m', resolution=(160, 90),
                                       fps=8, threads=threads)
                data = path.read_bytes()
                width, height, frames = y4m_frames(data)
                self.assertEqual((width, height), (160, 90))
                self.assertEqual(len(frames), receipt.frame_count)
                self.assertGreaterEqual(len(frames), 8)
                self.assertGreater(len(set(frames)), 4)
                for frame in frames:
                    luma = np.frombuffer(frame[:width * height], dtype=np.uint8)
                    self.assertGreater(int(luma.max()) - int(luma.min()), 20)
                artifacts.append(data)
            self.assertEqual(artifacts[0], artifacts[1])


suite = unittest.defaultTestLoader.loadTestsFromTestCase(LiveGraphingAcceptance)
if suite.countTestCases() != 8:
    raise AssertionError('live graph native acceptance inventory drift')
result = unittest.TextTestRunner(verbosity=2).run(suite)
if not result.wasSuccessful():
    raise AssertionError('live graph native acceptance failed')
