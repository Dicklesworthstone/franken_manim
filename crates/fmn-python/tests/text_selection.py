"""Occurrence selection stays a live native glyph view across text front doors."""
import copy
import re
import unittest

import manimlib as m
import numpy as np


class TextSelectionTests(unittest.TestCase):
    def test_positional_and_keyword_occurrences_select_the_same_live_glyphs(self):
        for cls in (m.Text, m.MarkupText, m.Code):
            with self.subTest(cls=cls.__name__):
                text = cls('café café café')
                parts = text.get_parts_by_text('café')
                self.assertEqual(len(parts), 3)
                for index in (0, 1, 2, -1, np.int64(1)):
                    selected = text.get_part_by_text('café', index)
                    expected = parts[index]
                    self.assertEqual(tuple(map(id, selected)), tuple(map(id, expected)))
                    keyword = text.get_part_by_text('café', index=index)
                    self.assertEqual(tuple(map(id, selected)), tuple(map(id, keyword)))
                text.get_part_by_text('café', 1).set_color(m.RED)
                self.assertTrue(all(glyph.get_fill_color() == m.RED for glyph in parts[1]))
                self.assertTrue(all(glyph.get_fill_color() != m.RED for glyph in parts[0]))

    def test_regex_and_unicode_keep_native_source_mapping(self):
        text = m.MarkupText('<b>café</b> + <i>café</i>')
        literal = text.get_part_by_text('café', 1)
        pattern = text.get_part_by_text(re.compile(r'caf.'), 1)
        self.assertEqual(tuple(map(id, literal)), tuple(map(id, pattern)))
        self.assertGreater(len(literal), 0)
        self.assertEqual(tuple(map(id, literal)), tuple(map(id, text.select_part('café', 1))))

    def test_authored_selector_dispatch_preserves_argument_forms(self):
        calls = []
        class Authored(m.Text):
            def select_part(self, selector, *args, **kwargs):
                calls.append((self, selector, args, dict(kwargs)))
                kwargs.pop('custom', None)
                return super().select_part(selector, *args, **kwargs)
        text = Authored('A A')
        text.get_part_by_text('A')
        text.get_part_by_text('A', 1, custom=True)
        text.get_part_by_text('A', index=-1)
        self.assertEqual(calls, [(text, 'A', (), {}), (text, 'A', (1,), {'custom': True}),
                                 (text, 'A', (), {'index': -1})])

    def test_errors_match_the_underlying_selector_without_geometry_mutation(self):
        text = m.Text('A A')
        before = text.get_all_points().copy()
        for args, kwargs in (((3,), {}), ((1.2,), {}), ((1,), {'index': 0}),
                             ((0, 1), {}), ((), {'unknown': True})):
            with self.subTest(args=args, kwargs=kwargs):
                with self.assertRaises((TypeError, IndexError)) as expected:
                    text.select_part('A', *args, **kwargs)
                with self.assertRaises(type(expected.exception)):
                    text.get_part_by_text('A', *args, **kwargs)
                np.testing.assert_array_equal(text.get_all_points(), before)

    def test_copy_selection_changes_only_owned_glyphs_and_native_pixels(self):
        text = m.Code('let let', language='rust')
        duplicate = copy.deepcopy(text)
        text.get_part_by_text('let', -1).set_color(m.BLUE)
        camera = m.Camera()
        camera.reset_pixel_shape(160, 90)
        changed = camera.capture_snapshot(text).pixels()
        self.assertNotEqual(changed, camera.capture_snapshot(duplicate).pixels())
        duplicate.select_part('let', -1).set_color(m.BLUE)
        self.assertEqual(changed, camera.capture_snapshot(duplicate).pixels())


class NativeTexSelectionTests(unittest.TestCase):
    def test_literal_regex_and_ranges_use_the_native_selector(self):
        formula = m.Tex(r'e^{i\pi} + 1 = 0')
        calls = []
        original = m._native._string_span_selections

        def observe(source, spans, selections):
            calls.append((source, tuple(spans), tuple(selections)))
            return original(source, spans, selections)

        m._native._string_span_selections = observe
        try:
            literal = formula.select_parts('i')
            pattern = formula.select_parts(re.compile('i'))
            indices = formula.get_submob_indices_list_by_span((3, 4))
        finally:
            m._native._string_span_selections = original
        self.assertEqual(len(calls), 3)
        self.assertEqual(len(literal), 1)
        self.assertEqual(len(literal[0]), 1)
        self.assertEqual(tuple(map(id, literal[0])), tuple(map(id, pattern[0])))
        self.assertIs(literal[0][0], formula._string_submobject(indices[0]))
        self.assertEqual(formula.find_spans_by_selector('i'), [(3, 4), (6, 7)])

    def test_coloring_literal_i_does_not_color_pi_or_other_glyphs(self):
        formula = m.Tex(r'e^{i\pi} + 1 = 0')
        selected = formula.select_part('i')[0]
        before = [part.get_fill_color() for part in formula]
        formula.set_color_by_tex('i', m.RED)
        for part, color in zip(formula, before):
            self.assertEqual(part.get_fill_color(), m.RED if part is selected else color)
        self.assertNotEqual(formula.select_part(r'\pi')[0].get_fill_color(), m.RED)
        self.assertNotEqual(formula.select_part('+')[0].get_fill_color(), m.RED)

    def test_tex_color_map_runs_once_after_styles_and_once_on_regeneration(self):
        events = []

        class Observed(m.Tex):
            def init_colors(self):
                events.append(('before_styles', self.select_part('x')[0].get_fill_color()))
                return super().init_colors()

            def set_color_by_tex_to_color_map(self, colors):
                events.append(('color_map', dict(colors)))
                return super().set_color_by_tex_to_color_map(colors)

        colors = {'x': m.RED, re.compile('y'): m.BLUE}
        formula = Observed('x+y', t2c=colors, fill_color=m.GREEN)
        self.assertEqual(events[0], ('before_styles', m.WHITE))
        self.assertEqual(sum(event[0] == 'color_map' for event in events), 1)
        self.assertEqual(formula.select_part('x')[0].get_fill_color(), m.RED)
        self.assertEqual(formula.select_part('y')[0].get_fill_color(), m.BLUE)
        self.assertEqual(formula.select_part('+')[0].get_fill_color(), m.GREEN)
        formula.init_points()
        self.assertEqual(sum(event[0] == 'color_map' for event in events), 2)
        self.assertEqual(formula.select_part('x')[0].get_fill_color(), m.RED)
        self.assertEqual(formula.select_part('y')[0].get_fill_color(), m.BLUE)

    def test_substring_path_counts_follow_native_primitives(self):
        formula = m.Tex(r'\sin x + \frac{1}{2} + \sqrt{y}')
        for substring, count in [
            (r'\sin', 3), (r'\frac', 1), (r'\sqrt', 2),
            (r'\frac{1}{2}', 3), ('x', 1), (' ', 0), ('', 0),
            (r'\alpha+\beta', 3),
        ]:
            with self.subTest(substring=substring):
                self.assertEqual(formula.substr_to_path_count(substring), count)
        for text in (m.Text('a b'), m.MarkupText('<b>a b</b>')):
            self.assertEqual(text.substr_to_path_count('a b'), 2)
            self.assertEqual(text.substr_to_path_count(' '), 0)
            self.assertEqual(text.substr_to_path_count('c d'), 2)

    def test_copy_and_live_numeric_replacement_keep_current_native_selection(self):
        formula = m.Tex('x=12')
        duplicate = copy.deepcopy(formula)
        number = duplicate.make_number_changeable(12)
        self.assertEqual(duplicate.substr_to_path_count(r'\decimalmob'), 1)
        self.assertIs(duplicate.select_part(r'\decimalmob')[0], number)
        self.assertEqual(formula.substr_to_path_count('12'), 2)
        duplicate.set_color_by_tex('x', m.RED)
        self.assertNotEqual(formula.select_part('x')[0].get_fill_color(), m.RED)

    def test_tex_reordered_blocks_and_pinned_keys_keep_every_native_part(self):
        source, target = m.Tex('ab+cd'), m.Tex('cd+ab')
        animation = m.TransformMatchingTex(source, target)
        blocks = [child for child in animation.animations if isinstance(child, m.Transform)]
        self.assertEqual([len(child.mobject.family_members_with_points()) for child in blocks],
                         [2, 2, 1])
        pinned = m.TransformMatchingTex(source, target, matched_keys=['a'])
        matched = [part for child in pinned.animations if isinstance(child, m.Transform)
                   for part in child.mobject.family_members_with_points()]
        expected = source.family_members_with_points()
        self.assertEqual(set(map(id, matched)), set(map(id, expected)))
        self.assertEqual(len(matched), len(expected))

    def test_invalid_utf8_native_metadata_is_refused(self):
        text = m.Text('é')
        text._string_sub_spans = [(0, 1)]
        with self.assertRaisesRegex(Exception, 'UTF-8'):
            text.select_parts('é')


if __name__ == '__main__':
    unittest.main(verbosity=2)
