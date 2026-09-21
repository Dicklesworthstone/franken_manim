"""Source-compatible complex matrix construction and native live-cell updates."""
import copy
import unittest

import manimlib as m
import numpy as np


class ComplexMatrixTests(unittest.TestCase):
    def test_builtin_matrix_types_route_complex_values_to_native_readouts(self):
        values = [[1 + 2j, -3j], [complex(4, 0), 5.25 - 6.75j]]
        for constructor in (m.Matrix, m.DecimalMatrix, m.IntegerMatrix, m.TexMatrix):
            for data in (values, np.asarray(values), (iter(row) for row in values)):
                with self.subTest(constructor=constructor.__name__, container=type(data).__name__):
                    matrix = constructor(data)
                    for index, entry in enumerate(matrix.elements):
                        self.assertIsInstance(entry, m.DecimalNumber)
                        self.assertEqual(entry.get_value(), values[index // 2][index % 2])
                        self.assertEqual(len(entry), len(entry.num_string))
                        self.assertTrue(entry.family_members_with_points())
                    self.assertEqual(matrix.elements[0].get_tex(),
                                     "1+2i" if constructor is m.IntegerMatrix else "1.00+2.00i")
                    self.assertEqual(len(matrix.brackets), 2)
                    self.assertLess(matrix.brackets[0].get_right()[0], matrix.get_entries().get_left()[0])
                    self.assertGreater(matrix.brackets[1].get_left()[0], matrix.get_entries().get_right()[0])

    def test_mixed_native_entries_are_not_replaced_or_stringified(self):
        existing = m.Circle(color=m.GREEN)
        matrix = m.Matrix([[existing, 1 + 2j], ["x", 3.5]])
        self.assertIs(matrix.mob_matrix[0][0], existing)
        self.assertIsInstance(matrix.mob_matrix[0][1], m.DecimalNumber)
        self.assertIsInstance(matrix.mob_matrix[1][0], m.Tex)
        self.assertIsInstance(matrix.mob_matrix[1][1], m.DecimalNumber)
        self.assertEqual(matrix.mob_matrix[0][1].get_value(), 1 + 2j)

    def test_precision_style_and_numpy_scalar_types_survive(self):
        values = np.array([[1 + 2j, 3 - 4j]], dtype=np.complex64)
        matrix = m.DecimalMatrix(values, num_decimal_places=3,
                                 decimal_config=dict(color=m.BLUE, include_sign=True, font_size=32))
        self.assertIs(matrix.float_matrix, values)
        for entry in matrix.elements:
            self.assertEqual(entry.get_font_size(), 32)
            self.assertTrue(all(member.get_fill_color() == m.BLUE
                                for member in entry.family_members_with_points()))
        self.assertEqual(matrix.elements[0].get_tex(), "+1.000+2.000i")
        self.assertEqual(matrix.elements[1].get_tex(), "+3.000–4.000i")

    def test_invalid_complex_cell_does_not_move_existing_native_input(self):
        for bad in (complex(1, float("inf")), complex(float("nan"), 2)):
            existing = m.Circle().shift(2 * m.RIGHT)
            before = existing.get_points().copy()
            with self.assertRaises(ValueError):
                m.Matrix([[existing, bad]])
            np.testing.assert_array_equal(existing.get_points(), before)

    def test_nested_live_cells_keep_matrix_aliases_and_scene_roots(self):
        matrix = m.DecimalMatrix([[1 + 2j, 3j]])
        entries, brackets = list(matrix.elements), list(matrix.brackets)
        scene = m.Scene(camera_config=dict(resolution=(192, 108), fps=8))
        parent = m.VGroup(matrix)
        scene.add(parent)
        scene.play(m.ChangeDecimalToValue(entries[0], -2j),
                   m.ChangeDecimalToValue(entries[1], 7), run_time=0.5)
        self.assertEqual(list(scene.mobjects), [parent])
        self.assertEqual(list(matrix.elements), entries)
        self.assertEqual(list(matrix.brackets), brackets)
        for index, expected in enumerate((-2j, 7)):
            entry = entries[index]
            self.assertIs(matrix.get_row(0)[index], entry)
            self.assertIs(matrix.get_column(index)[0], entry)
            self.assertEqual(entry.get_value(), expected)
            self.assertEqual(len(entry), len(entry.num_string))

    def test_complex_matrix_copy_owns_independent_numeric_cells(self):
        matrix = m.DecimalMatrix([[1 + 2j, 3j]])
        for clone in (matrix.copy(), copy.deepcopy(matrix)):
            clone.elements[0].set_value(8 - 9j)
            self.assertEqual(clone.elements[0].get_value(), 8 - 9j)
            self.assertEqual(matrix.elements[0].get_value(), 1 + 2j)
            self.assertIs(clone.mob_matrix[0][0], clone.elements[0])
            self.assertIsNot(clone.elements[0], matrix.elements[0])


if __name__ in ("__main__", "<run_path>"):
    result = unittest.TextTestRunner(verbosity=2).run(
        unittest.defaultTestLoader.loadTestsFromTestCase(ComplexMatrixTests))
    if not result.wasSuccessful():
        raise AssertionError("complex matrix acceptance failed")
