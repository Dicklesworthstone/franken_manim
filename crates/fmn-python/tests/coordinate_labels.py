"""Native coordinate annotations: actual glyphs, ownership and rendered placement."""
from __future__ import annotations

import copy
import itertools
from pathlib import Path
import tempfile
import unittest

import numpy as np
import manimlib as m


class NumberLineLabelsTests(unittest.TestCase):
    def assert_points(self, left, right):
        a, b = left.family_members_with_points(), right.family_members_with_points()
        self.assertEqual(len(a), len(b))
        for actual, expected in zip(a, b):
            np.testing.assert_allclose(actual.get_points(), expected.get_points(), atol=2e-6)

    def test_vertical_line_labels_have_actual_numeric_spacing(self):
        line = m.NumberLine((-2, 2, 1)).rotate(m.PI / 2)
        labels = line.add_numbers([-1, 0, 1], direction=m.RIGHT, buff=.2)
        self.assertIs(line.numbers, labels)
        self.assertIsInstance(labels, m.VGroup)
        np.testing.assert_allclose([x.get_center()[1] for x in labels], [-1, 0, 1], atol=1e-6)
        for label in labels:
            self.assertIsInstance(label, m.DecimalNumber)
            self.assertAlmostEqual(label.get_left()[0], .2, places=6)

    def test_reflected_sheared_tilted_line_matches_independent_coordinates(self):
        matrix = np.array([[-1.5, .5, 0], [.75, 2, 0], [.5, -.25, 1.]])
        offset = np.array([.2, -.4, 1.])
        line = m.NumberLine((-2, 3, 1))
        old_origin = line.n2p(0).copy()
        line.apply_matrix(matrix).shift(offset)
        labels = line.add_numbers([-1, 0, 2], direction=m.RIGHT, buff=.3)
        for value, label in zip((-1, 0, 2), labels):
            anchor = matrix @ (old_origin + [value, 0, 0]) + offset
            expected = m.DecimalNumber(value, num_decimal_places=0, font_size=24)
            expected.next_to(anchor, m.RIGHT, buff=.3)
            self.assert_points(label, expected)

    def test_existing_children_views_and_scene_are_not_rebuilt(self):
        line = m.NumberLine((-2, 2, 1)).rotate(.4)
        marker = m.Dot((3, 1, 0)); line.add(marker)
        ticks = line.ticks
        calls = []; updater = lambda mob, dt: calls.append(dt)
        line.add_updater(updater, call=False)
        scene = m.Scene(); scene.add(line)
        line.save_state(); saved = line.saved_state
        # Nursery-to-scene binding is allowed to change record ownership.
        # The operation under test starts after that transfer, not before it.
        records = line.data.copy(); live = line.get_points()
        children = tuple(line.submobjects)
        labels = line.add_numbers([1, 2])
        self.assertEqual(tuple(line.submobjects[:-1]), children)
        self.assertIs(line.ticks, ticks)
        self.assertIs(line.saved_state, saved)
        self.assertEqual(tuple(scene.mobjects), (line,))
        self.assertEqual(calls, [])
        self.assertEqual(scene.get_time(), 0)
        np.testing.assert_array_equal(line.data, records)
        line.shift(m.UP)
        np.testing.assert_allclose(live, records['point'] + m.UP, atol=1e-6)
        self.assertIs(labels._scene, scene)
        self.assertNotIn(labels, saved.submobjects)

    def test_repeated_calls_keep_old_groups_and_append_one_new_group(self):
        line = m.NumberLine((0, 2, 1))
        first = line.add_numbers([0]); before = first.get_center().copy()
        second = line.add_numbers([1, 2])
        self.assertIn(first, line.submobjects)
        self.assertIs(line.numbers, second)
        np.testing.assert_array_equal(first.get_center(), before)
        self.assertEqual([n.get_value() for n in second], [1., 2.])

    def test_default_ticks_exclusions_and_duplicate_values(self):
        line = m.NumberLine((-1, 2, 1), numbers_to_exclude=[0])
        self.assertEqual([x.get_value() for x in line.add_numbers()], [-1, 1, 2])
        self.assertEqual([x.get_value() for x in line.add_numbers([1, 0, 1], excluding=[])], [1, 0, 1])
        tipped = m.NumberLine((0, 2, 1), include_tip=True)
        self.assertEqual([x.get_value() for x in tipped.add_numbers()], [0, 1])

    def test_units_precision_and_signs_use_public_number_builder(self):
        line = m.NumberLine((-4, 4, 1)).rotate(.7)
        kwargs = dict(unit=2, unit_tex='m', num_decimal_places=0, include_sign=True, color=m.RED)
        labels = line.add_numbers([-2, 2, 4], **kwargs)
        for value, label in zip((-2, 2, 4), labels):
            self.assert_points(label, line.get_number_mobject(value, font_size=24, **kwargs))
            self.assertEqual(label.get_value(), value / 2)

    def test_authored_formatter_receives_keywords_and_returns_actual_objects(self):
        calls, returned = [], []
        class Custom(m.NumberLine):
            def get_number_mobject(self, value, *, word, **kwargs):
                calls.append((value, word, kwargs['font_size']))
                label = m.Text(word).move_to(self.n2p(value))
                returned.append(label)
                return label
        line = Custom((0, 1, 1))
        labels = line.add_numbers([0, 1], word='x', font_size=18)
        self.assertEqual(calls, [(0., 'x', 18.), (1., 'x', 18.)])
        self.assertEqual(tuple(labels), tuple(returned))

    def test_authored_tick_range_and_number_mapping_dispatch(self):
        class Custom(m.NumberLine):
            def get_tick_range(self): return iter((.25, .75))
            def number_to_point(self, value): return np.array([value, value * value, 1.])
        line = Custom((0, 1, 1))
        labels = line.add_numbers(direction=m.RIGHT)
        np.testing.assert_allclose([x.get_center()[1] for x in labels], [.0625, .5625], atol=1e-6)
        np.testing.assert_allclose([x.get_center()[2] for x in labels], [1, 1], atol=1e-6)

    def test_later_formatter_failure_never_publishes_partial_labels(self):
        for error in (ValueError('label'), KeyboardInterrupt('cancel')):
            class Custom(m.NumberLine):
                def get_number_mobject(self, value, **kwargs):
                    if value == 1: raise error
                    return super().get_number_mobject(value, **kwargs)
            line = Custom((0, 2, 1)); before = tuple(line.submobjects)
            with self.assertRaises(type(error)) as raised: line.add_numbers([0, 1, 2])
            self.assertIs(raised.exception, error)
            self.assertEqual(tuple(line.submobjects), before)
            self.assertNotIn('numbers', vars(line))
            self.assertEqual(len(line.add_numbers([0])), 1)

    def test_recursive_labeling_refuses_and_next_call_recovers(self):
        class Custom(m.NumberLine):
            recursive = True
            def get_number_mobject(self, value, **kwargs):
                if self.recursive: self.add_numbers([0])
                return super().get_number_mobject(value, **kwargs)
        line = Custom((0, 1, 1))
        with self.assertRaisesRegex(RuntimeError, 'reenter'): line.add_numbers([0])
        line.recursive = False
        self.assertEqual(len(line.add_numbers([0])), 1)

    def test_copy_during_formatter_does_not_inherit_permanent_busy_state(self):
        copies = []
        class Custom(m.NumberLine):
            def get_number_mobject(self, value, **kwargs):
                if not copies:
                    copies.extend((self.copy(), copy.deepcopy(self)))
                    self.save_state(); copies.append(self.saved_state)
                return super().get_number_mobject(value, **kwargs)
        line = Custom((0, 1, 1)); line.add_numbers([0])
        for item in copies: self.assertEqual(len(item.add_numbers([1])), 1)

    def test_generator_or_formatter_mutations_are_not_overwritten(self):
        line = m.NumberLine((0, 1, 1)); before = tuple(line.submobjects)
        def values():
            yield 0
            line.shift(m.UP)
            yield 1
        with self.assertRaisesRegex(RuntimeError, 'changed'): line.add_numbers(values())
        self.assertEqual(tuple(line.submobjects), before)
        self.assertAlmostEqual(line.n2p(0)[1], 1.)
        self.assertEqual(len(line.add_numbers([0])), 1)

    def test_bounded_iterables_and_nonfinite_values_fail_before_formatter(self):
        calls = []
        class Custom(m.NumberLine):
            def get_number_mobject(self, value, **kwargs):
                calls.append(value)
                return super().get_number_mobject(value, **kwargs)
        line = Custom((0, 1, 1))
        for values in (itertools.repeat(1), [0, float('nan')], [float('inf')]):
            with self.assertRaises(ValueError): line.add_numbers(values)
        line.x_step = 1e-50
        with self.assertRaisesRegex(ValueError, 'budget'): line.add_numbers()
        self.assertEqual(calls, [])

    def test_unknown_keyword_keeps_existing_refusal_contract(self):
        line = m.NumberLine((0, 1, 1)); before = tuple(line.submobjects)
        with self.assertRaises(NotImplementedError) as raised: line.add_numbers([0], bogus=True)
        self.assertEqual(str(raised.exception), 'NumberLine.add_numbers() keyword(s) not yet routed to the native builder: bogus')
        self.assertEqual(tuple(line.submobjects), before)

    def test_render_matches_independent_rotated_number_line_at_one_and_four_threads(self):
        angle = .8
        class Labeled(m.Scene):
            manual = False
            def construct(self):
                line = m.NumberLine((-2, 2, 1)).rotate(angle).shift((.5, .3, 0))
                if self.manual:
                    labels = m.VGroup()
                    for value in (-1, 1, 2):
                        anchor = np.array([value*np.cos(angle)+.5, value*np.sin(angle)+.3, 0])
                        label = m.DecimalNumber(value, num_decimal_places=0, font_size=24)
                        label.next_to(anchor, m.RIGHT, buff=.2)
                        labels.add(label)
                    line.add(labels)
                else: line.add_numbers([-1, 1, 2], direction=m.RIGHT, buff=.2)
                self.add(line)
                self.play(line.animate.shift((.4, .2, 0)), run_time=.5, rate_func=m.linear)
        class Reference(Labeled): manual = True
        with tempfile.TemporaryDirectory() as d:
            outputs = []
            for cls, threads in ((Labeled, 1), (Labeled, 4), (Reference, 1), (Reference, 4)):
                path = Path(d) / f'{cls.__name__}-{threads}.y4m'
                result = cls().render(path, format='y4m', resolution=(96, 64), fps=8, threads=threads)
                self.assertEqual(result.frame_count, 4)
                outputs.append(path.read_bytes())
            self.assertTrue(all(data == outputs[0] for data in outputs))


if __name__ == '__main__':
    unittest.main()
