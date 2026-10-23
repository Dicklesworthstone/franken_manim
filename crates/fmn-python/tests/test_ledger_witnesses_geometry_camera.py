"""Ledger witnesses for geometry_camera (fm-6u1g): each test calls a manimlib symbol the Parity Ledger
marks same/improved and checks the Reference's behaviour (manimlib @ 6199a00d)."""
import itertools
import math
import pathlib
import struct
import tempfile
import unittest
import zlib

import numpy as np

from test_parallel_batch_native import native_wheel_available


def _png(rgba):
    """A tiny stdlib-encoded RGBA PNG fixture (no production codec stands in for the portal)."""
    height, width, _ = rgba.shape

    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))

    rows = b"".join(b"\0" + row.tobytes() for row in rgba)
    return (b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">2I5B", width, height, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b""))


_TILES = np.array([[[255, 0, 0, 255], [0, 255, 0, 255]],
                   [[0, 0, 255, 255], [255, 255, 0, 255]]], dtype=np.uint8)


@unittest.skipUnless(native_wheel_available(), "requires the built FrankenManim native wheel")
class GeometryCameraWitnesses(unittest.TestCase):
    def setUp(self):
        self._temp = tempfile.TemporaryDirectory(prefix="fmn-6u1g-geometry-camera-")
        self.addCleanup(self._temp.cleanup)
        self.root = pathlib.Path(self._temp.name)

    def _write(self, name, data):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data.encode() if isinstance(data, str) else data)
        return path

    # manimlib.camera.camera ------------------------------------------------------------------

    def test_camera_get_pixel_array(self):
        from manimlib import RED, DOWN, LEFT, RIGHT, UP, Scene, Square
        from manimlib.utils.color import color_to_rgba

        # camera.py:157: (rgb_max_val * float readback).astype(uint8), rows flipped so row 0 is
        # the top of the frame, shape (pixel height, pixel width, n_channels).
        scene = Scene(camera_config=dict(resolution=(128, 72), background_color="#123456"))
        camera = scene.camera
        square = Square(side_length=2, fill_color=RED, fill_opacity=1, stroke_width=0)
        square.shift(2 * UP + 3 * RIGHT)
        scene.add(square)
        camera.capture(*scene.mobjects)
        pixels = camera.get_pixel_array()
        self.assertIsInstance(pixels, np.ndarray)
        self.assertEqual(pixels.shape, (72, 128, 4))
        self.assertEqual(pixels.dtype, np.uint8)

        red = np.round(255 * color_to_rgba(RED, 1.0)).astype(int)
        background = np.array([0x12, 0x34, 0x56, 255])
        # The default 14.22 x 8 frame over 128 x 72 px is 9 px per unit, origin at (col 64, row 36).
        # The square spans x in [2, 4], y in [1, 3]: cols [82, 100), rows [9, 27). Its interior
        # (one pixel inset, so edge antialiasing cannot matter) is the fill colour; pixels two or
        # more px away are background. The 8-bit float readback may truncate by one level.
        def check(top, left):
            frame = pixels.astype(int)
            inside = frame[top + 1:top + 17, left + 1:left + 17]
            self.assertLessEqual(np.abs(inside - red).max(), 1)
            mask = np.ones((72, 128), dtype=bool)
            mask[max(top - 2, 0):top + 20, max(left - 2, 0):left + 20] = False
            self.assertLessEqual(np.abs(frame[mask] - background).max(), 1)

        check(9, 82)
        # The array is a fresh copy, not a view of the frame buffer.
        pixels[:] = 0
        pixels = camera.get_pixel_array()
        check(9, 82)
        # It reflects the latest capture: centre (-2, -2) is cols [37, 55), rows [45, 63).
        square.shift(5 * LEFT + 4 * DOWN)
        camera.capture(*scene.mobjects)
        pixels = camera.get_pixel_array()
        check(45, 37)

    # manimlib.mobject.types.surface ----------------------------------------------------------

    def test_textured_surface_set_image_coords_by_uv_func(self):
        from manimlib import ParametricSurface, TexturedSurface

        image = self._write("tiles.png", _png(_TILES))
        surface = ParametricSurface(lambda u, v: (u, v, 0.0), u_range=(0, 1), v_range=(0, 1),
                                    resolution=(4, 3))
        textured = TexturedSurface(surface, str(image))
        nu, nv = surface.resolution
        grid = [(u, v) for u in np.linspace(0, 1, nu) for v in np.linspace(1, 0, nv)]
        # surface.py:346: the initial image coordinates are the (u, reversed v) grid itself.
        np.testing.assert_allclose(textured.data["im_coords"], grid, atol=1e-7)
        points = textured.get_points().copy()

        calls = []

        def uv_func(u, v):
            calls.append((float(u), float(v)))
            return (0.25 + 0.5 * u, v * v)

        result = textured.set_image_coords_by_uv_func(uv_func)
        # surface.py:353: uv_func is evaluated over the uv surface's grid, u-major with v reversed,
        # and its pairs become im_coords; the method returns self and leaves the geometry alone.
        self.assertIs(result, textured)
        self.assertEqual(calls, [(float(u), float(v)) for u, v in grid])
        np.testing.assert_allclose(textured.data["im_coords"],
                                   [(0.25 + 0.5 * u, v * v) for u, v in grid], atol=1e-7)
        np.testing.assert_array_equal(textured.get_points(), points)

    def test_three_d_model_get_textures_from_mtl(self):
        from manimlib import ThreeDModel

        self._write("materials/maps/tiles.png", _png(_TILES))
        self._write("materials/scene.mtl", "newmtl painted\nKd 1 1 1\nmap_Kd maps/tiles.png\n"
                                           "newmtl plain\nKd 0.1 0.7 0.2\n")
        obj = self._write("model.obj", "mtllib materials/scene.mtl\n"
                                       "v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\n"
                                       "vt 0 0\nvt 1 0\nvt 1 1\nvt 0 1\nvn 0 0 1\n"
                                       "usemtl painted\nf 1/1/1 2/2/1 3/3/1\n"
                                       "usemtl plain\nf 1/1/1 3/3/1 4/4/1\n")
        model = ThreeDModel(str(obj))
        # surface.py:460 over pywavefront: {material name: map_Kd joined to the .mtl's directory,
        # or None}, in .mtl order. (Checked against pywavefront 1.3 on these exact files.)
        expected = {"painted": str(self.root / "materials" / "maps" / "tiles.png"), "plain": None}
        textures = model.get_textures_from_mtl(str(obj))
        self.assertEqual(textures, expected)
        self.assertEqual(list(textures), ["painted", "plain"])
        self.assertEqual(model.get_textures_from_mtl(str(obj), suppress_warnings=False), expected)

    # manimlib.mobject.coordinate_systems -----------------------------------------------------

    def test_complex_plane_add_coordinate_labels(self):
        from manimlib import SMALL_BUFF, ComplexPlane, DecimalNumber, VGroup
        from manimlib.constants import DL

        plane = ComplexPlane(x_range=(-3, 3, 1), y_range=(-2, 2, 1))
        result = plane.add_coordinate_labels()
        self.assertIs(result, plane)
        labels = plane.coordinate_labels
        self.assertIsInstance(labels, VGroup)
        self.assertIn(labels, plane.submobjects)
        # coordinate_systems.py:750: x ticks [-3..3][1:], then nonzero y ticks [-2..2][1:] as
        # imaginary numbers; each labelled by its axis's get_number_mobject at font_size 36, the
        # imaginary ones with unit "i". NumberPlane axes place numbers DL with SMALL_BUFF.
        values = [-2, -1, 0, 1, 2, 3, -1, 1, 2]
        self.assertEqual(len(labels), len(values))
        for index, (label, value) in enumerate(zip(labels, values)):
            imaginary = index >= 6
            self.assertIsInstance(label, DecimalNumber)
            self.assertEqual(label.get_value(), value)
            self.assertEqual(label.font_size, 36)
            self.assertEqual(label.unit, "i" if imaginary else None)
            anchor = plane.c2p(0, value) if imaginary else plane.c2p(value, 0)
            if abs(value) != 1 or not imaginary:
                # next_to(anchor, DL, SMALL_BUFF): the label's upper-right corner sits at
                # anchor + SMALL_BUFF * DL.
                np.testing.assert_allclose(label.get_corner(-DL)[:2], (anchor + SMALL_BUFF * DL)[:2],
                                           atol=1e-6)
        # number_line.py:189: for +-1 with a unit, the "1" is removed and the label re-centred where
        # the full "+-1i" stood; the glyph left over is the same "i" as in "2i".
        one_i, minus_i, two_i, minus_two = labels[7], labels[6], labels[8], labels[0]
        self.assertEqual(len(two_i.submobjects), 2)
        self.assertEqual(len(one_i.submobjects), 1)
        self.assertEqual(len(minus_i.submobjects), 2)
        for label, value, glyphs in ((one_i, 1, [(0, two_i[1])]),
                                     (minus_i, -1, [(0, minus_two[0]), (1, two_i[1])])):
            full = DecimalNumber(float(value), num_decimal_places=0, font_size=36, unit="i")
            full.next_to(plane.c2p(0, value), DL, buff=SMALL_BUFF)
            np.testing.assert_allclose(label.get_center(), full.get_center(), atol=1e-6)
            for slot, model in glyphs:
                np.testing.assert_allclose(label[slot].get_points() - label[slot].get_center(),
                                           model.get_points() - model.get_center(), atol=1e-6)

    def test_coordinate_system_bind_graph_to_func(self):
        from manimlib import Axes

        axes = Axes(x_range=(-1, 5, 1), y_range=(-2, 10, 2), width=8, height=5)
        graph = axes.get_graph(lambda x: x)
        # coordinate_systems.py:264: the x samples are the x-coordinates of the graph's points.
        xs = np.array([axes.p2c(point)[0] for point in graph.get_points()])
        scale = [1.0]

        def func(x):
            return scale[0] * x ** 2 - 1

        result = axes.bind_graph_to_func(graph, func, jagged=True)
        self.assertIs(result, graph)

        def check(k):
            # The updater (run at once by add_updater) sets the points as corners through
            # c2p(xs, func(xs)): one anchor per sample, straight handles at the midpoints.
            anchors = np.array([axes.c2p(x, k * x ** 2 - 1) for x in xs])
            points = graph.get_points()
            self.assertEqual(len(points), 2 * len(xs) - 1)
            np.testing.assert_allclose(points[0::2], anchors, atol=1e-6)
            np.testing.assert_allclose(points[1::2], (anchors[:-1] + anchors[1:]) / 2, atol=1e-6)

        check(1.0)
        # Bound means bound: the graph follows the function when it updates.
        scale[0] = 0.5
        graph.update()
        check(0.5)

        # Without jagged, make_smooth(approx=True) keeps the anchors and bends the handles.
        smooth = axes.get_graph(lambda x: x)
        axes.bind_graph_to_func(smooth, np.sin)
        points = smooth.get_points()
        anchors = np.array([axes.c2p(x, math.sin(x)) for x in xs])
        np.testing.assert_allclose(points[0::2], anchors, atol=1e-6)
        self.assertGreater(np.abs(points[1::2] - (anchors[:-1] + anchors[1:]) / 2).max(), 1e-4)

    def _graph_lines(self):
        from manimlib import Axes, LEFT, DOWN

        axes = Axes(x_range=(-1, 5, 1), y_range=(-2, 10, 2), width=8, height=5)
        axes.shift(0.7 * LEFT + 0.3 * DOWN)

        def f(x):
            return 0.5 * x ** 2 - 1

        graph = axes.get_graph(f)
        x = 2.3
        return axes, graph, x, axes.c2p(x, f(x))

    def _check_dashes(self, line, start, end):
        from manimlib import DashedLine, GREY_A

        # get_line_from_axis_to_point: DashedLine(axis.get_projection(point), point), GREY_A, 2.
        self.assertIsInstance(line, DashedLine)
        np.testing.assert_allclose(line.start, start, atol=1e-9)
        np.testing.assert_allclose(line.end, end, atol=1e-9)
        self.assertEqual(line.get_stroke_color().upper(), GREY_A.upper())
        self.assertEqual(line.get_stroke_width(), 2)
        # The dashes run along the segment from the axis to the graph point.
        direction = (end - start) / np.linalg.norm(end - start)
        np.testing.assert_allclose(line.get_start(), start, atol=1e-6)
        self.assertLess(np.linalg.norm(line.get_end() - end), 0.05)
        for dash in line.submobjects:
            offsets = dash.get_points() - start
            along = offsets @ direction
            self.assertLess(np.abs(offsets - np.outer(along, direction)).max(), 1e-6)
            self.assertGreaterEqual(along.min(), -1e-6)
            self.assertLessEqual(along.max(), np.linalg.norm(end - start) + 1e-6)

    def test_coordinate_system_get_v_line_to_graph(self):
        axes, graph, x, point = self._graph_lines()
        line = axes.get_v_line_to_graph(x, graph)
        # The foot is the x-axis projection: same x as the graph point, on the y = 0 line.
        foot = axes.c2p(x, 0)
        self.assertAlmostEqual(foot[0], point[0], places=9)
        self.assertAlmostEqual(foot[1], axes.x_axis.get_start()[1], places=6)
        self._check_dashes(line, foot, point)

    def test_coordinate_system_get_h_line_to_graph(self):
        axes, graph, x, point = self._graph_lines()
        line = axes.get_h_line_to_graph(x, graph)
        # The foot is the y-axis projection: same y as the graph point, on the x = 0 line.
        foot = axes.c2p(0, 0.5 * x ** 2 - 1)
        self.assertAlmostEqual(foot[1], point[1], places=9)
        self.assertAlmostEqual(foot[0], axes.y_axis.get_start()[0], places=6)
        self._check_dashes(line, foot, point)

    # manimlib.mobject.vector_field -----------------------------------------------------------

    def test_stream_lines_init(self):
        from manimlib import Axes, BLUE_E, GREEN, RED, StreamLines, YELLOW
        from manimlib.utils.color import color_to_rgb

        axes = Axes(x_range=(-2, 2, 1), y_range=(-2, 2, 1), width=4, height=4)

        def sink(coords):  # dx/dt = -x: the solution from x0 is x0 * exp(-t)
            return -np.asarray(coords)

        # noise_factor=0 removes the jitter; arc_len is unused by the pinned draw_lines, and
        # infinity is the portal's way of not capping (see the report on the default arc_len).
        lines = StreamLines(sink, axes, noise_factor=0, arc_len=np.inf)
        self.assertIs(lines.func, sink)
        self.assertIs(lines.coordinate_system, axes)
        self.assertEqual((lines.density, lines.n_repeats, lines.solution_time, lines.dt,
                          lines.stroke_width, lines.magnitude_range), (1.0, 1, 3, 0.05, 1.0, (0, 2.0)))
        # vector_field.py:119: seeds are the product of arange(min, max + step, step) per range.
        grid = np.arange(-2, 3, 1.0)
        seeds = np.array(list(itertools.product(grid, grid)))
        self.assertEqual(len(lines), len(seeds))
        # ode_solution_points samples t_eval = arange(0, solution_time, dt): the last is 2.95.
        last = np.arange(0, 3, 0.05)[-1]
        for line, seed in zip(lines, seeds):
            coords = np.array([axes.p2c(point) for point in line.get_points()])
            np.testing.assert_allclose(coords[0], seed, atol=1e-6)
            # solve_ivp's RK45 defaults (rtol 1e-3, atol 1e-6) bound the integration error.
            np.testing.assert_allclose(coords[-1], seed * math.exp(-last), rtol=1e-3, atol=1e-6)
            radius = np.linalg.norm(seed)
            if radius:
                unit = seed / radius
                self.assertLess(np.abs(coords @ np.array([-unit[1], unit[0]])).max(), 1e-6)
                along = coords @ unit
                self.assertTrue(np.all(np.diff(along) <= 1e-9))
            self.assertEqual(line.virtual_time, 3)
            np.testing.assert_allclose(line.get_stroke_widths(), 1.0)
        # init_style colours each point by |func| over (0, 2) on the 3b1b colormap.
        first = {tuple(seed): line.data["stroke_rgba"][0] for line, seed in zip(lines, seeds)}
        np.testing.assert_allclose(first[(0.0, 0.0)][:3], color_to_rgb(BLUE_E), atol=1e-6)
        np.testing.assert_allclose(first[(2.0, 2.0)][:3], color_to_rgb(RED), atol=1e-6)
        np.testing.assert_allclose(first[(-1.0, 0.0)][:3],
                                   (color_to_rgb(GREEN) + color_to_rgb(YELLOW)) / 2, atol=1e-6)

    # manimlib.utils.simple_functions ---------------------------------------------------------

    def test_gen_choose(self):
        from manimlib.utils.simple_functions import gen_choose

        # simple_functions.py: int(np.prod(range(n, n - r, -1)) / math.factorial(r)).
        for n in range(16):
            for r in range(n + 1):
                value = gen_choose(n, r)
                self.assertIs(type(value), int)
                self.assertEqual(value, math.comb(n, r))
        self.assertEqual(gen_choose(20, 10), 184756)
        # r > n: the falling product passes through zero.
        self.assertEqual(gen_choose(3, 5), 0)


if __name__ == "__main__":
    unittest.main()
