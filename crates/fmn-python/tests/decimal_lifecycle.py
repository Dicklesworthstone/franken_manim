"""Native numeric readout lifecycle, record ownership, regeneration and pixels."""
from pathlib import Path
import copy
import pickle
import tempfile
import unittest

import numpy as np
import manimlib as m


class DecoratedNumber(m.DecimalNumber):
    data_dtype = m.VMobject.data_dtype + [('mass', 1)]

    def init_data(self):
        super().init_data()
        self.decoration = m.Dot(2 * m.UP)
        self.add(self.decoration)

    def init_points(self):
        super().init_points()
        self.set_points_as_corners([[-1, -1, 0], [1, -1, 0]])
        self.data['mass'][:] = 7


class DecimalLifecycleTests(unittest.TestCase):
    def test_reference_hook_order_and_recipe_visibility(self):
        class Number(m.DecimalNumber):
            def init_data(self):
                self.events = ['data']
                self.recipe = (self.num_decimal_places, self.unit, self.font_size)
                super().init_data()
            def init_points(self):
                self.events.append('points')
                super().init_points()
            def init_uniforms(self):
                self.events.append('uniforms')
                super().init_uniforms()
            def init_colors(self):
                self.events.append(('colors', bool(self.submobjects)))
                return super().init_colors()
            def set_submobjects_from_number(self, value):
                self.events.append(('digits', value))
                return super().set_submobjects_from_number(value)
        number = Number(12.5, num_decimal_places=1, unit='V', font_size=30)
        self.assertEqual(number.recipe, (1, 'V', 30))
        self.assertEqual(number.events, ['data', 'points', 'uniforms', ('colors', False),
                                        ('digits', 12.5), ('colors', True)])

    def test_root_records_custom_lanes_and_decorations_survive_value_changes(self):
        number = DecoratedNumber(123, num_decimal_places=0)
        points = number.get_points().copy()
        for value in [1, 12345, -8, 2 + 3j]:
            number.set_submobjects_from_number(value)
            np.testing.assert_array_equal(number.get_points(), points)
            np.testing.assert_array_equal(number.data['mass'], 7)
            self.assertIn(number.decoration, number.submobjects)
            self.assertEqual(len(number), 1 + len(number.num_string))

    def test_color_hooks_reach_real_glyphs_and_preserve_update_style(self):
        class Green(m.DecimalNumber):
            def init_colors(self):
                super().init_colors()
                self.set_color(m.GREEN)
        number = Green(12)
        for value in (12, 1 + 2j, -3):
            number.set_value(value)
            self.assertTrue(all(leaf.get_fill_color() == m.GREEN
                                for leaf in number.family_members_with_points()))

    def test_background_keeps_root_schema_and_same_size_views_live(self):
        class Number(m.DecimalNumber):
            data_dtype = m.VMobject.data_dtype + [('mass', 1)]
        number = Number(1, include_background_rectangle=True)
        number.data['mass'][:] = 9
        view = number.data
        count = len(view)
        number.set_value(100)
        self.assertEqual(len(number.data), count)
        np.testing.assert_array_equal(number.data['mass'], 9)
        view['mass'][:] = 11
        np.testing.assert_array_equal(number.data['mass'], 11)
        np.testing.assert_array_equal(number.data['fill_rgba'][:, :3], 0)
        number.include_background_rectangle = False
        number.set_value(1)
        self.assertEqual(len(number.data), 0)
        self.assertIn('mass', number.data.dtype.names)
        np.testing.assert_array_equal(view['mass'], 11)

    def test_decorated_root_and_background_coexist_without_duplicate_children(self):
        number = DecoratedNumber(1, include_background_rectangle=True)
        points = number.get_points().copy()
        for value in (2, 20, 200, 3):
            number.set_submobjects_from_number(value)
            np.testing.assert_array_equal(number.get_points(), points)
            np.testing.assert_array_equal(number.data['mass'], 7)
            self.assertEqual(len(number), len(number.num_string) + 2)
            background = number._fmn_decimal_background_child
            self.assertIsNotNone(background)
            self.assertEqual(background.get_fill_color(), m.BLACK)
        number.include_background_rectangle = False
        number.set_submobjects_from_number(1)
        self.assertIsNone(number._fmn_decimal_background_child)
        self.assertEqual(len(number), len(number.num_string) + 1)
        np.testing.assert_array_equal(number.get_points(), points)

    def test_glyph_style_donor_is_not_a_decoration(self):
        number = DecoratedNumber(1, color=m.GREEN)
        number.decoration.set_color(m.RED)
        number.set_value(2)
        self.assertEqual(number.decoration.get_fill_color(), m.RED)
        self.assertTrue(all(leaf.get_fill_color() == m.GREEN
                            for child in number._fmn_decimal_children
                            for leaf in child.family_members_with_points()))

    def test_failures_keep_the_bound_generation_unchanged(self):
        number = DecoratedNumber(1, include_background_rectangle=True)
        scene = m.Scene()
        scene.add(number)
        error = RuntimeError('authored formatter')
        def fail(value):
            raise error
        number.get_num_string = fail
        before = (tuple(map(id, number.get_family())), number.data.copy(), number.number,
                  number.num_string, tuple(number._fmn_decimal_children))
        with self.assertRaises(RuntimeError) as caught:
            number.set_value(2)
        self.assertIs(caught.exception, error)
        self.assertEqual(tuple(map(id, number.get_family())), before[0])
        np.testing.assert_array_equal(number.data, before[1])
        self.assertEqual((number.number, number.num_string, tuple(number._fmn_decimal_children)), before[2:])

    def test_invalid_options_fail_before_hooks(self):
        calls = []
        class Number(m.DecimalNumber):
            def init_data(self):
                calls.append('data')
                return super().init_data()
        for options in ({'number': float('nan')}, {'num_decimal_places': 4097},
                        {'font_size': 0}, {'edge_to_fix': [float('inf'), 0, 0]},
                        {'stroke_width': float('nan')}):
            with self.subTest(options=options), self.assertRaises((ValueError, TypeError)):
                Number(**options)
        self.assertEqual(calls, [])

    def test_init_colors_exception_propagates_without_retry(self):
        calls, failure = [], RuntimeError('authored colors')
        class Number(m.DecimalNumber):
            def init_colors(self):
                calls.append('colors')
                raise failure
        with self.assertRaises(RuntimeError) as caught:
            Number(1)
        self.assertIs(caught.exception, failure)
        self.assertEqual(calls, ['colors'])

    def test_copy_deepcopy_and_pickle_remap_generated_children(self):
        number = DecoratedNumber(123, include_background_rectangle=True)
        for clone in (number.copy(), copy.deepcopy(number), pickle.loads(pickle.dumps(number))):
            self.assertIsNot(clone.decoration, number.decoration)
            self.assertIn(clone.decoration, clone.submobjects)
            self.assertTrue(all(child in clone.submobjects for child in clone._fmn_decimal_children))
            clone.set_value(9)
            self.assertEqual(len(clone), len(clone.num_string) + 2)
            self.assertEqual(number.get_value(), 123)
            np.testing.assert_array_equal(clone.data['mass'], 7)

    def test_integer_uses_same_schema_and_hook_protocol(self):
        class Integer(m.Integer):
            data_dtype = m.VMobject.data_dtype + [('mass', 1)]
            def init_points(self):
                self.called = True
                return super().init_points()
        integer = Integer(8)
        self.assertTrue(integer.called)
        integer.set_value(-15)
        self.assertEqual(integer.get_value(), -15)
        self.assertIn('mass', integer.data.dtype.names)

    def test_existing_matrix_cells_remain_updatable(self):
        matrix = m.DecimalMatrix([[1.5, 2.5]])
        cell = matrix.get_entries()[0]
        cell.set_value(13.5)
        cell.init_colors()
        self.assertEqual(cell.get_value(), 13.5)
        self.assertEqual(len(cell), len(cell.num_string))

    def test_animated_readout_preserves_native_root_and_hook_geometry(self):
        number = DecoratedNumber(1, num_decimal_places=0)
        scene = m.Scene()
        scene.add(number)
        points, decoration = number.get_points().copy(), number.decoration
        scene.play(m.ChangeDecimalToValue(number, 12), run_time=2 / 30, rate_func=m.linear)
        self.assertEqual(number.get_value(), 12)
        self.assertIn(decoration, number.submobjects)
        # set_value re-seats the full family's fixed edge. Compare shape, not its
        # translation, so the test does not assume a different anchoring rule.
        np.testing.assert_allclose(np.diff(number.get_points(), axis=0), np.diff(points, axis=0), atol=1e-6)
        np.testing.assert_array_equal(number.data['mass'], 7)

    def test_color_hook_pngs_match_independent_numbers_at_each_sample(self):
        root = Path(tempfile.mkdtemp(prefix='fmn-decimal-lifecycle-'))
        def render(path, threads, authored):
            scene = m.Scene()
            with scene.render_session(path, format='png_sequence', resolution=(128, 72), fps=4, threads=threads):
                if authored:
                    class Green(m.DecimalNumber):
                        def init_colors(self):
                            super().init_colors()
                            self.set_color(m.GREEN)
                    number = Green(1, num_decimal_places=0).scale(2)
                    scene.add(number)
                    scene.play(m.ChangeDecimalToValue(number, 9), run_time=1, rate_func=m.linear)
                else:
                    number = m.DecimalNumber(1, num_decimal_places=0, color=m.GREEN).scale(2)
                    edge = number.get_left().copy()
                    # Choreo emits the interval's right-end samples: .25,.5,.75,1.
                    for value in (3, 5, 7, 9):
                        literal = m.DecimalNumber(value, num_decimal_places=0, color=m.GREEN).scale(2)
                        literal.move_to(edge, m.LEFT)
                        scene.clear()
                        scene.add(literal)
                        scene.wait(.25)
            return [file.read_bytes() for file in sorted(path.glob('*.png'))]
        first = render(root / 'one', 1, True)
        self.assertEqual(first, render(root / 'four', 4, True))
        self.assertEqual(first, render(root / 'independent', 1, False))
        self.assertEqual(len(first), 4)
        self.assertGreater(len(set(first)), 1)


if __name__ in ('__main__', '<run_path>'):
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(DecimalLifecycleTests)
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    if not result.wasSuccessful():
        raise AssertionError('native decimal lifecycle regressions failed')
