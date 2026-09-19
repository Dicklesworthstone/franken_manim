"""Python Matrix dispatch and aliasing tests; storage/typesetting are doubles.

The production adapter is imported unchanged. These tests are not native
geometry, font, render, wheel, or cross-platform certification evidence.
"""
from __future__ import annotations

import copy
import importlib.util
import math
from pathlib import Path
from types import ModuleType
import unittest

import numpy as np

MODULE_PATH = Path(__file__).resolve().parents[1] / "python/fmn_python/matrix.py"
_spec = importlib.util.spec_from_file_location("matrix_adapter_under_test", MODULE_PATH)
adapter = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(adapter)


def fixture():
    """Fresh module/classes per test, never an installed manimlib substitute."""
    native = ModuleType("matrix_fixture")
    native._np = np
    native.RIGHT = np.array([1., 0., 0.])
    native.LEFT = -native.RIGHT
    native.DOWN = np.array([0., -1., 0.])
    native.ORIGIN = np.zeros(3)
    native.DEG = math.pi / 180
    native.calls = []
    native.fail_tex_at = None

    class VMobject:
        def __init__(self, width=1., height=1., **config):
            self.points = np.array([[-width / 2, -height / 2, 0.], [width / 2, height / 2, 0.]])
            self.submobjects = []
            self.parents = []
            self.config = dict(config)
            self.color = config.get("color", "white")
            self.operations = []
            self._scene = None

        def __iter__(self):
            return iter(self.submobjects)

        def __len__(self):
            return len(self.submobjects)

        def __getitem__(self, index):
            value = self.submobjects[index]
            return VGroup(*value) if isinstance(index, slice) else value

        def get_family(self):
            found, seen, stack = [], set(), [self]
            while stack:
                item = stack.pop()
                if id(item) in seen:
                    continue
                seen.add(id(item))
                found.append(item)
                stack.extend(reversed(item.submobjects))
            return found

        def get_bounding_box(self):
            arrays = [mob.points for mob in self.get_family() if len(mob.points)]
            points = np.concatenate(arrays) if arrays else np.zeros((1, 3))
            lo, hi = points.min(axis=0), points.max(axis=0)
            return np.array([lo, (lo + hi) / 2, hi])

        def get_center(self):
            return self.get_bounding_box()[1]

        def length_over_dim(self, dim):
            box = self.get_bounding_box()
            return box[2, dim] - box[0, dim]

        def get_width(self):
            return self.length_over_dim(0)

        def get_height(self):
            return self.length_over_dim(1)

        def get_bounding_box_point(self, direction):
            lo, center, hi = self.get_bounding_box()
            return np.where(np.array(direction) > 0, hi, np.where(np.array(direction) < 0, lo, center))

        def shift(self, vector):
            for mob in self.get_family():
                mob.points += np.asarray(vector)
            return self

        def move_to(self, point, aligned_edge=None):
            target = point.get_center() if isinstance(point, VMobject) else np.asarray(point)
            edge = np.zeros(3) if aligned_edge is None else aligned_edge
            return self.shift(target - self.get_bounding_box_point(edge))

        def scale(self, factor, about_point=None):
            center = self.get_center() if about_point is None else np.asarray(about_point)
            for mob in self.get_family():
                mob.points[:] = center + (mob.points - center) * factor
            return self

        def set_height(self, height):
            if self.get_height():
                self.scale(height / self.get_height())
            return self

        def set_width(self, width):
            if self.get_width():
                self.scale(width / self.get_width())
            return self

        def next_to(self, other, direction, buff):
            self.operations.append(("next_to", other, tuple(direction), buff))
            return self.shift(other.get_bounding_box_point(direction) + np.asarray(direction) * buff
                              - self.get_bounding_box_point(-np.asarray(direction)))

        def become(self, other):
            self.operations.append(("become", getattr(other, "source", None)))
            self.points = other.points.copy()
            self.source = getattr(other, "source", None)
            self.color = other.color
            self.submobjects = list(other.submobjects)
            return self

        def rotate(self, angle):
            self.operations.append(("rotate", angle))
            return self

        def set_color(self, color):
            for mob in self.get_family():
                mob.color = color
            return self

        def add_background_rectangle(self):
            self.operations.append(("background",))
            return self

        def set_submobjects(self, members):
            for previous in self.submobjects:
                previous.parents = [parent for parent in previous.parents if parent is not self]
            self.submobjects = list(members)
            for member in self.submobjects:
                if not any(parent is self for parent in member.parents):
                    member.parents.append(self)
            return self

        def add(self, *members):
            return self.set_submobjects([*self.submobjects, *members])

        def center(self):
            return self.shift(-self.get_center())

        def copy(self, deep=False):
            return copy.deepcopy(self)

    class VGroup(VMobject):
        def __init__(self, *members):
            super().__init__()
            self.points = np.empty((0, 3))
            self.set_submobjects(members)

    class Tex(VMobject):
        def __init__(self, source, **config):
            native.calls.append(("tex", source, dict(config)))
            if native.fail_tex_at == len(native.calls):
                raise RuntimeError("native typesetting refusal fixture")
            super().__init__(**config)
            self.source = source
            if source.startswith(r"\left["):
                self.points = np.empty((0, 3))
                self.set_submobjects([VMobject(.1, 1).shift([-1, 0, 0]), VMobject(.1, 1).shift([1, 0, 0])])

    class DecimalNumber(VMobject):
        def __init__(self, value, **config):
            native.calls.append(("number", value, dict(config)))
            super().__init__(**config)
            self.value = value

    class Matrix(VMobject):
        def __init__(self, grid):
            VMobject.__init__(self)
            self.points = np.empty((0, 3))
            self.mob_matrix = grid
            self.elements = [entry for row in grid for entry in row]
            self.ellipses, self.brackets = [], []
            self.rows = VGroup(*(VGroup(*row) for row in grid))
            self.columns = VGroup(*(VGroup(*column) for column in zip(*grid)))
            self._matrix_shape = len(grid), len(grid[0])
            self.add(*self.elements)

        def get_rows(self):
            return self.rows

        def get_columns(self):
            return self.columns

        def get_entries(self):
            return VGroup(*self.elements)

    class DecimalMatrix(Matrix):
        pass

    class IntegerMatrix(DecimalMatrix):
        pass

    class TexMatrix(Matrix):
        pass

    class MobjectMatrix(Matrix):
        pass

    for cls in (VMobject, VGroup, Tex, DecimalNumber, Matrix, DecimalMatrix, IntegerMatrix, TexMatrix, MobjectMatrix):
        setattr(native, cls.__name__, cls)
    return native


class MatrixEditingTests(unittest.TestCase):
    def setUp(self):
        self.native = fixture()
        adapter.install_matrix(self.native)
        self.grid = [[self.native.VMobject().shift([3 * column, -2 * row, 0])
                      for column in range(3)] for row in range(2)]
        self.matrix = self.native.Matrix(self.grid)

    def test_installer_is_idempotent_and_keeps_class_identity(self):
        cls = self.native.Matrix
        method = cls.swap_entries_for_ellipses
        adapter.install_matrix(self.native)
        self.assertIs(self.native.Matrix, cls)
        self.assertIs(cls.swap_entries_for_ellipses, method)
        self.assertFalse(getattr(method, "_fmn_schema_placeholder", False))

    def test_conversion_uses_native_types_and_explicit_config(self):
        for value in (1.25, complex(0, 2), np.float32(3), np.complex128(2j)):
            result = self.matrix.element_to_mobject(value, font_size=72, color="blue")
            self.assertIsInstance(result, self.native.DecimalNumber)
            self.assertEqual(result.config["font_size"], 72)
        for value in (4, "x", np.int64(5)):
            result = self.matrix.element_to_mobject(value)
            self.assertIsInstance(result, self.native.Tex)
            self.assertEqual(result.source, str(value))

    def test_caller_owned_mobject_is_not_restyled_or_copied(self):
        entry = self.grid[0][0]
        self.assertIs(self.matrix.element_to_mobject(entry, color="blue"), entry)
        self.assertEqual(entry.color, "white")

    def test_decimal_helper_preserves_configured_defaults(self):
        matrix = self.native.DecimalMatrix(self.grid)
        matrix._matrix_entry_font_size = 64
        matrix._matrix_decimal_places = 3
        matrix._matrix_entry_style = {"color": "red"}
        number = matrix.element_to_mobject(1, num_decimal_places=5)
        self.assertIsInstance(number, self.native.DecimalNumber)
        self.assertEqual(number.config, {"font_size": 64, "num_decimal_places": 5, "color": "red"})

    def test_single_replacement_keeps_live_aliases_and_returns_none(self):
        entry = self.grid[0][0]
        center = entry.get_center().copy()
        result = self.matrix.swap_entry_for_dots(entry, self.native.Tex(r"\vdots"))
        self.assertIsNone(result)
        self.assertIs(self.matrix.rows[0][0], entry)
        self.assertIs(self.matrix.columns[0][0], entry)
        self.assertIs(self.matrix.submobjects[0], entry)
        np.testing.assert_array_equal(entry.get_center(), center)
        self.assertNotIn(entry, self.matrix.elements)
        self.assertEqual(self.matrix.ellipses, [entry])

    def test_row_and_column_keep_order_identity_and_intersection_angle(self):
        before = [entry.get_center().copy() for row in self.grid for entry in row]
        result = self.matrix.swap_entries_for_ellipses(1, 2)
        self.assertIs(result, self.matrix)
        self.assertEqual(len(self.matrix.elements), 2)
        self.assertEqual(len(self.matrix.ellipses), 4)
        self.assertEqual([call[1] for call in self.native.calls], [r"\vdots"] * 3 + [r"\hdots"] * 2)
        for entry, center in zip(self.matrix.submobjects, before):
            np.testing.assert_allclose(entry.get_center(), center)
        self.assertEqual(self.grid[1][2].operations[-1], ("rotate", -math.pi / 4))
        self.assertEqual(self.grid[1][2].source, r"\hdots")
        self.assertAlmostEqual(self.grid[1][0].get_height(), .65 * 3 / 2)
        self.assertAlmostEqual(self.grid[0][2].get_width(), .4 * 7 / 3)

    def test_negative_and_numpy_indices(self):
        self.matrix.swap_entries_for_ellipses(np.int64(-1), -1)
        self.assertEqual(self.grid[1][2].source, r"\hdots")
        self.assertFalse(hasattr(self.grid[0][0], "source"))

    def test_out_of_range_is_noop(self):
        self.assertIs(self.matrix.swap_entries_for_ellipses(200, -100), self.matrix)
        self.assertEqual(self.native.calls, [])
        self.assertEqual(len(self.matrix.elements), 6)

    def test_bad_indices_and_ratios_refuse_before_mutation(self):
        for kwargs in ({"row_index": 1.5}, {"col_index": "1"},
                       {"row_index": 0, "height_ratio": float("nan")},
                       {"col_index": 0, "width_ratio": float("inf")},
                       {"row_index": 0, "height_ratio": -1}):
            with self.subTest(kwargs=kwargs), self.assertRaises((TypeError, ValueError)):
                self.matrix.swap_entries_for_ellipses(**kwargs)
        self.assertEqual(self.native.calls, [])
        self.assertEqual(len(self.matrix.elements), 6)

    def test_typesetting_failure_does_not_partly_replace_grid(self):
        self.native.fail_tex_at = 4
        with self.assertRaisesRegex(RuntimeError, "typesetting refusal"):
            self.matrix.swap_entries_for_ellipses(0, 1)
        self.assertEqual(self.matrix.ellipses, [])
        self.assertEqual(len(self.matrix.elements), 6)
        self.assertTrue(all(not entry.operations for row in self.grid for entry in row))

    def test_replacement_override_is_actually_called(self):
        calls = []
        class Custom(self.native.Matrix):
            def swap_entry_for_dots(self, entry, dots):
                calls.append(entry)
                return super().swap_entry_for_dots(entry, dots)
        matrix = Custom(self.grid)
        matrix.swap_entries_for_ellipses(row_index=0)
        self.assertEqual(calls, self.grid[0])

    def test_callback_failure_retains_original_exception_and_applied_prefix(self):
        failure = LookupError("authored hook")
        calls = []
        class Custom(self.native.Matrix):
            def swap_entry_for_dots(self, entry, dots):
                calls.append(entry)
                if len(calls) == 2:
                    raise failure
                return super().swap_entry_for_dots(entry, dots)
        matrix = Custom(self.grid)
        with self.assertRaises(LookupError) as raised:
            matrix.swap_entries_for_ellipses(row_index=0)
        self.assertIs(raised.exception, failure)
        self.assertEqual(matrix.ellipses, [self.grid[0][0]])

    def test_repeated_replacement_does_not_duplicate_ellipsis_alias(self):
        self.matrix.swap_entries_for_ellipses(row_index=0)
        self.matrix.swap_entries_for_ellipses(row_index=0)
        self.assertEqual(len(self.matrix.ellipses), 3)
        self.assertEqual(len(self.matrix.elements), 3)

    def test_native_bracket_request_and_placement(self):
        result = self.matrix.create_brackets(self.matrix.rows, .25, .2)
        self.assertEqual(len(result), 2)
        self.assertEqual(self.native.calls[0][1],
                         r"\left[\begin{array}{c}\quad \\\quad \\\end{array}\right]")
        self.assertAlmostEqual(result.get_height(), self.matrix.rows.get_height() + .25)
        self.assertEqual(result[0].operations[-1][-1], .2)

    def test_color_and_background_helpers_use_public_getters(self):
        entry = self.grid[0][0]
        native = self.native
        class Custom(native.Matrix):
            def get_columns(self):
                return native.VGroup(native.VGroup(entry))
            def get_entries(self):
                return native.VGroup(entry)
        matrix = Custom(self.grid)
        self.assertIs(matrix.set_column_colors("blue", "green"), matrix)
        self.assertEqual(entry.color, "blue")
        self.assertEqual(self.grid[0][1].color, "white")
        self.assertIs(matrix.add_background_to_entries(), matrix)
        self.assertEqual(entry.operations, [("background",)])

    def test_invalid_entry_type_never_mutates_lists(self):
        with self.assertRaises(TypeError):
            self.matrix.swap_entry_for_dots(self.grid[0][0], object())
        self.assertEqual(len(self.matrix.elements), 6)


if __name__ == "__main__":
    unittest.main()
