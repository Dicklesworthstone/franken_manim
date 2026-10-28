"""Isolated source-level regressions; these do not load the native extension.

They exercise the uniform interpolator the portal ships: install()'s nested
`interpolate_uniform` in the embedded manimlib/_animation_semantics.py, compiled on its own.
The live Mobject.interpolate path, uniform locks included, is covered by
tests/animation_semantics.py against the wheel.
"""
import ast
from pathlib import Path
import unittest
import numpy as np

SOURCE = Path(__file__).resolve().parents[1] / 'python' / 'manimlib' / '_animation_semantics.py'
installer = next(node for node in ast.parse(SOURCE.read_text(encoding='utf-8')).body
                 if isinstance(node, ast.FunctionDef) and node.name == 'install')
functions = [node for node in installer.body
             if isinstance(node, ast.FunctionDef) and node.name == 'interpolate_uniform']
if len(functions) != 1:
    raise RuntimeError('install() does not define exactly one interpolate_uniform')
namespace = {'np': np, 'interpolate_value': lambda a, b, t: (1-t)*a+t*b}
exec(compile(ast.Module(body=functions, type_ignores=[]), str(SOURCE), 'exec'), namespace)  # ubs:ignore -- test-only named function parsed from the fixed repository semantics path; no external input.
lerp = namespace['interpolate_uniform']


def tuple_tree(value):
    return tuple(tuple_tree(item) if isinstance(item, list) else item for item in value)


class UniformInterpolationTests(unittest.TestCase):
    def test_scalar(self):
        self.assertEqual(lerp(2., 10., .25), 4.)

    def test_flat_list(self):
        result = lerp([0, 2, 4], [4, 6, 8], .25)
        self.assertEqual(result, [1., 3., 5.])
        self.assertIs(type(result), list)

    def test_flat_tuple(self):
        result = lerp((0, 2, 4), (4, 6, 8), .25)
        self.assertEqual(result, (1., 3., 5.))
        self.assertIs(type(result), tuple)

    def test_list_clip_plane_matrix(self):
        result = lerp([[1, 0, 0, 0], [0, 1, 0, 0]], [[1, 0, 0, 2], [0, 1, 0, 4]], .5)
        self.assertEqual(result, [[1., 0., 0., 1.], [0., 1., 0., 2.]])

    def test_tuple_clip_plane_matrix(self):
        result = lerp(((1, 0, 0, 0), (0, 1, 0, 0)), ((1, 0, 0, 2), (0, 1, 0, 4)), .5)
        self.assertEqual(result, ((1., 0., 0., 1.), (0., 1., 0., 2.)))

    def test_rank_three_list(self):
        start = np.arange(12).reshape(2, 2, 3).tolist()
        end = (np.arange(12).reshape(2, 2, 3) + 4).tolist()
        self.assertEqual(lerp(start, end, .25), (np.arange(12).reshape(2, 2, 3) + 1).tolist())

    def test_rank_three_tuple(self):
        start = tuple_tree(np.arange(12).reshape(2, 2, 3).tolist())
        end = tuple_tree((np.arange(12).reshape(2, 2, 3) + 4).tolist())
        self.assertEqual(lerp(start, end, .25), tuple_tree((np.arange(12).reshape(2, 2, 3) + 1).tolist()))

    def test_array_behavior_and_precedence_unchanged(self):
        for other in ([[4, 6], [8, 10]], ((4, 6), (8, 10))):
            result = lerp(np.array([[0., 2.], [4., 6.]]), other, .25)
            self.assertIsInstance(result, np.ndarray)
            np.testing.assert_array_equal(result, [[1., 3.], [5., 7.]])

    def test_tuple_precedence_unchanged(self):
        self.assertEqual(lerp([0, 2], (4, 6), .25), (1., 3.))
        self.assertEqual(lerp((0, 2), [4, 6], .25), (1., 3.))

    def test_empty_containers(self):
        self.assertEqual(lerp([], [], .5), [])
        self.assertEqual(lerp((), (), .5), ())
        self.assertEqual(lerp([[], []], [[], []], .5), [[], []])
        self.assertEqual(lerp(((), ()), ((), ()), .5), ((), ()))

    def test_scalar_broadcast(self):
        self.assertEqual(lerp(0, [[2, 4], [6, 8]], .5), [[1., 2.], [3., 4.]])
        self.assertEqual(lerp(((2, 4), (6, 8)), 0, .5), ((1., 2.), (3., 4.)))

    def test_row_broadcast(self):
        self.assertEqual(lerp([[0, 2], [4, 6]], [4, 6], .5), [[2., 4.], [4., 6.]])

    def test_inputs_not_mutated_or_aliased(self):
        start, end = [[0., 2.], [4., 6.]], [[4., 6.], [8., 10.]]
        result = lerp(start, end, .25)
        result[0][0] = 999
        self.assertEqual(start, [[0., 2.], [4., 6.]])
        self.assertEqual(end, [[4., 6.], [8., 10.]])

    def test_shape_mismatch_rejected(self):
        with self.assertRaises(ValueError):
            lerp([[1, 2], [3, 4]], [[1, 2, 3], [4, 5, 6]], .5)

    def test_endpoints_and_extrapolation(self):
        for alpha, expected in [(0., [[0., 2.]]), (1., [[4., 6.]]), (1.5, [[6., 8.]])]:
            self.assertEqual(lerp([[0, 2]], [[4, 6]], alpha), expected)


if __name__ == '__main__':
    unittest.main(verbosity=2)
