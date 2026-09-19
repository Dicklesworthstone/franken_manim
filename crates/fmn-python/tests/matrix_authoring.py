"""Real installed-portal Matrix acceptance. No storage or renderer doubles.

Registered by scripts/check_portal_runtime.sh. Missing native installation is
an error, not a skip or an invitation to substitute fixture geometry.
"""
from __future__ import annotations

import pathlib
import tempfile
import unittest
import zlib

import manimlib as m
import numpy as np

from fmn_python import render_scene


def rgba_png(path):
    """Small independent RGBA8 PNG reader for these native render witnesses."""
    data = path.read_bytes()
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise AssertionError("expected a real PNG")
    payload, offset, shape = bytearray(), 8, None
    while offset < len(data):
        size = int.from_bytes(data[offset:offset + 4], "big")
        kind = data[offset + 4:offset + 8]
        body = data[offset + 8:offset + 8 + size]
        if len(body) != size:
            raise AssertionError("truncated native PNG")
        if kind == b"IHDR":
            width, height = int.from_bytes(body[:4], "big"), int.from_bytes(body[4:8], "big")
            if body[8:] != bytes((8, 6, 0, 0, 0)):
                raise AssertionError("expected noninterlaced RGBA8 PNG")
            shape = height, width
        elif kind == b"IDAT":
            payload.extend(body)
        offset += size + 12
        if kind == b"IEND":
            break
    if shape is None:
        raise AssertionError("missing PNG dimensions")
    height, width = shape
    raw = zlib.decompress(payload)
    stride = width * 4
    if len(raw) != height * (stride + 1):
        raise AssertionError("bad PNG row size")
    result = np.empty((height, stride), dtype=np.uint8)
    previous = bytearray(stride)
    for y in range(height):
        start = y * (stride + 1)
        filter_type = raw[start]
        row = bytearray(raw[start + 1:start + 1 + stride])
        for x in range(stride):
            left = row[x - 4] if x >= 4 else 0
            up = previous[x]
            upper_left = previous[x - 4] if x >= 4 else 0
            if filter_type == 1:
                predictor = left
            elif filter_type == 2:
                predictor = up
            elif filter_type == 3:
                predictor = (left + up) // 2
            elif filter_type == 4:
                estimate = left + up - upper_left
                distances = abs(estimate - left), abs(estimate - up), abs(estimate - upper_left)
                predictor = (left, up, upper_left)[distances.index(min(distances))]
            elif filter_type == 0:
                predictor = 0
            else:
                raise AssertionError("invalid PNG filter")
            row[x] = (row[x] + predictor) & 255
        result[y] = np.frombuffer(row, dtype=np.uint8)
        previous = row
    return result.reshape(height, width, 4)


class MatrixAuthoringAcceptance(unittest.TestCase):
    def test_live_ellipses_preserve_native_cells_and_aliases(self):
        matrix = m.Matrix([[1, 2, 3], [4, 5, 6]])
        scene = m.Scene()
        scene.add(matrix)
        cells = [cell for row in matrix.mob_matrix for cell in row]
        centers = [cell.get_center().copy() for cell in cells]
        roots = list(matrix.submobjects)
        self.assertIs(matrix.swap_entries_for_ellipses(-1, -1), matrix)
        self.assertEqual(list(matrix.submobjects), roots)
        self.assertEqual(len(matrix.get_ellipses()), 4)
        self.assertEqual(len(matrix.get_entries()), 2)
        for index, cell in enumerate(cells):
            self.assertIs(matrix.get_row(index // 3)[index % 3], cell)
            self.assertIs(matrix.get_column(index % 3)[index // 3], cell)
            np.testing.assert_allclose(cell.get_center(), centers[index], atol=2e-5)
        copied = matrix.copy()
        self.assertEqual(len(copied.ellipses), 4)
        original_family = set(map(id, matrix.get_family()))
        self.assertFalse(original_family.intersection(map(id, copied.get_family())))
        scene.play(cells[0].animate.shift(.25 * m.RIGHT), run_time=.125)
        self.assertGreater(cells[0].get_center()[0], centers[0][0])

    def test_decimal_helper_uses_real_native_formatter(self):
        matrix = m.DecimalMatrix([[1.5]], num_decimal_places=3)
        result = matrix.element_to_mobject(2.5, num_decimal_places=1)
        self.assertIsInstance(result, m.DecimalNumber)
        self.assertEqual(result.get_value(), 2.5)
        self.assertEqual(result.get_num_string(2.5), "2.5")
        self.assertGreater(len(result.get_family()), 1)

    def test_default_brackets_are_native_glyph_geometry(self):
        matrix = m.Matrix([[1, 2], [3, 4]])
        brackets = matrix.create_brackets(matrix.get_rows(), .25, .2)
        self.assertEqual(len(brackets), 2)
        self.assertGreater(brackets.get_height(), matrix.get_rows().get_height())
        self.assertLess(brackets[0].get_center()[0], matrix.get_center()[0])
        self.assertGreater(brackets[1].get_center()[0], matrix.get_center()[0])
        self.assertTrue(any(member.has_points() for member in brackets.get_family()))

    def test_replacement_callback_runs_for_every_native_cell(self):
        seen = []
        class Custom(m.Matrix):
            def swap_entry_for_dots(self, entry, dots):
                seen.append(entry)
                return super().swap_entry_for_dots(entry, dots)
        matrix = Custom([[1, 2], [3, 4]])
        matrix.swap_entries_for_ellipses(row_index=0)
        self.assertEqual(seen, list(matrix.get_row(0)))

    def test_matrix_edits_and_motion_are_visible_in_real_frames(self):
        class MatrixScene(m.Scene):
            def construct(self):
                matrix = m.Matrix([[1, 2, 3], [4, 5, 6]])
                matrix.scale(1.5)
                self.add(matrix)
                self.wait(.25)
                matrix.swap_entries_for_ellipses(row_index=1, col_index=2)
                self.wait(.25)
                self.play(matrix.get_row(0)[0].animate.shift(.5 * m.UP), run_time=.25)
        with tempfile.TemporaryDirectory(prefix="fmn-matrix-authoring-") as directory:
            root = pathlib.Path(directory)
            receipt = render_scene(MatrixScene, root / "frames", format="png_sequence",
                                   resolution=(160, 90), fps=8, threads=1)
            files = sorted((root / "frames").glob("*.png"))
            self.assertGreaterEqual(receipt.frame_count, 6)
            self.assertEqual(len(files), receipt.frame_count)
            pixels = [rgba_png(path) for path in files]
            self.assertTrue(all(np.any(frame[:, :, :3]) for frame in pixels))
            self.assertFalse(np.array_equal(pixels[0], pixels[2]))
            self.assertFalse(np.array_equal(pixels[2], pixels[-1]))


suite = unittest.defaultTestLoader.loadTestsFromTestCase(MatrixAuthoringAcceptance)
if not suite.countTestCases():
    raise AssertionError("matrix authoring acceptance selected no tests")
result = unittest.TextTestRunner(verbosity=2).run(suite)
if not result.wasSuccessful():
    raise AssertionError("native matrix authoring acceptance failed")
