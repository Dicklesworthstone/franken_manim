"""Live numeric TeX acceptance through real native mobjects and rendering."""
from __future__ import annotations

import unittest

import manimlib as m
import numpy as np


class JoinedTex(m.Tex):
    _tex_arg_separator = ""


def identities(tex):
    return [tex._string_submobject(i) for i in range(len(tex._string_sub_paths))]


def metadata(tex):
    return (tex.string, tex.tex_string, list(tex.tex_strings),
            list(tex._string_sub_spans), [list(p) for p in tex._string_sub_paths])


def nest(tex):
    """An authored wrapper around existing native glyphs, not fake glyph data."""
    old = list(tex.submobjects)
    wrapper = m.VGroup(m.VGroup(*old))
    tex.set_submobjects([wrapper])
    tex._string_sub_paths = [[0, 0, *p] for p in tex._string_sub_paths]
    return wrapper


class LiveTexAcceptance(unittest.TestCase):
    def assert_map(self, tex):
        payload = tex.string.encode("utf-8")
        self.assertEqual(len(tex._string_sub_spans), len(tex._string_sub_paths))
        self.assertEqual(tex.get_tex(), tex.string)
        for i, (start, end) in enumerate(tex._string_sub_spans):
            self.assertTrue(0 <= start <= end <= len(payload), (start, end, payload))
            payload[:start].decode("utf-8")
            payload[:end].decode("utf-8")
            self.assertIsInstance(tex._string_submobject(i), m.VMobject)

    def test_multiple_independent_numbers_can_be_made_live_sequentially(self):
        tex = m.Tex("a=1.00+b=2.00+c=3.00")
        values = [tex.make_number_changeable(v) for v in ("1.00", "2.00", "3.00")]
        self.assertTrue(all(isinstance(v, m.DecimalNumber) for v in values))
        self.assertEqual(tex.string, r"a=\decimalmob+b=\decimalmob+c=\decimalmob")
        self.assertEqual(len(tex.get_parts_by_tex(r"\decimalmob")), 3)
        self.assert_map(tex)

    def test_index_selects_the_matching_source_occurrence(self):
        tex = m.Tex("1.00+1.00+1.00")
        first = list(tex.get_part_by_tex("1.00", 0))
        decimal = tex.make_number_changeable("1.00", index=1)
        self.assertIsInstance(decimal, m.DecimalNumber)
        self.assertEqual(tex.string, r"1.00+\decimalmob+1.00")
        self.assertEqual(list(tex.get_part_by_tex("1.00", 0)), first)
        self.assert_map(tex)

    def test_negative_index_and_later_conversion(self):
        tex = m.Tex("1.00+1.00")
        last = tex.make_number_changeable("1.00", index=-1)
        first = tex.make_number_changeable("1.00")
        self.assertIsInstance(first, m.DecimalNumber)
        self.assertIsNot(first, last)
        self.assertEqual(tex.string, r"\decimalmob+\decimalmob")
        self.assert_map(tex)

    def test_replace_all_retains_independent_live_values(self):
        tex = m.Tex("1.00+1.00=2.00")
        decimals = tex.make_number_changeable("1.00", replace_all=True)
        self.assertEqual(len(decimals), 2)
        decimals[0].set_value(8.25)
        self.assertEqual(decimals[1].get_value(), 1)
        self.assertIsInstance(tex.make_number_changeable("2.00"), m.DecimalNumber)
        self.assert_map(tex)

    def test_native_utf8_spans_remain_selectable_after_replacement(self):
        tex = m.TexText("é $1.00$ then $2.00$ τέλος")
        tex.make_number_changeable("1.00")
        self.assertIsInstance(tex.make_number_changeable("2.00"), m.DecimalNumber)
        self.assertTrue(len(tex.get_part_by_tex("é")))
        self.assertTrue(len(tex.get_part_by_tex("τέλος")))
        self.assert_map(tex)

    def test_nested_part_identity_and_unmapped_decorations_survive(self):
        tex = m.Tex("y =", "1.00 x + 2.00")
        roots = list(tex.submobjects)
        dot = m.Dot().shift(3 * m.UP)
        roots[-1].add(dot)
        wrapper = nest(tex)
        first = tex.make_number_changeable("1.00")
        second = tex.make_number_changeable("2.00")
        self.assertIsInstance(first, m.DecimalNumber)
        self.assertIsInstance(second, m.DecimalNumber)
        self.assertIs(tex.submobjects[0], wrapper)
        self.assertEqual(list(wrapper[0]), roots)
        self.assertIn(dot, roots[-1].submobjects)
        self.assert_map(tex)

    def test_number_crossing_argument_groups_is_replaced_once(self):
        tex = JoinedTex("a=1", "2.50", "+3.00")
        roots = list(tex.submobjects)
        number = tex.make_number_changeable("12.50")
        self.assertIsInstance(number, m.DecimalNumber)
        self.assertEqual(number.get_value(), 12.5)
        self.assertEqual(list(tex.submobjects), roots)
        self.assertEqual(tex.tex_strings, [r"a=\decimalmob", "", "+3.00"])
        self.assertIsInstance(tex.make_number_changeable("3.00"), m.DecimalNumber)
        self.assertEqual(identities(tex).count(number), 1)
        self.assert_map(tex)

    def test_native_matrix_tex_entry_can_become_a_live_number(self):
        matrix = m.TexMatrix([["12.50", "x"]])
        entry = matrix.get_mob_matrix()[0][0]
        self.assertEqual(entry._string_sub_paths, [[]])
        before = entry.get_bounding_box().copy()
        number = entry.make_number_changeable("12.50")
        self.assertIs(matrix.get_mob_matrix()[0][0], entry)
        self.assertIsInstance(number, m.DecimalNumber)
        self.assertEqual(entry._string_sub_paths, [[0]])
        np.testing.assert_allclose(number.get_center(), (before[0] + before[2]) / 2, atol=2e-5)
        self.assert_map(entry)

    def test_preserves_live_scene_identity_and_style(self):
        scene = m.Scene()
        tex = m.Tex("x=1.00+2.00", color=m.RED)
        scene.add(tex)
        before = list(scene.mobjects)
        first = tex.make_number_changeable("1.00")
        second = tex.make_number_changeable("2.00")
        self.assertEqual(list(scene.mobjects), before)
        self.assertEqual(first.get_color(), m.RED)
        scene.play(m.ChangeDecimalToValue(first, 4.25),
                   m.ChangeDecimalToValue(second, 8.5), run_time=2 / 30)
        self.assertAlmostEqual(first.get_value(), 4.25)
        self.assertAlmostEqual(second.get_value(), 8.5)
        self.assert_map(tex)

    def test_copy_remaps_live_number_paths_without_mutating_original(self):
        tex = m.Tex("1.00+2.00")
        original = tex.make_number_changeable("1.00")
        duplicate = tex.copy()
        copied = duplicate.get_part_by_tex(r"\decimalmob")[0]
        self.assertIsNot(copied, original)
        copied.set_value(9)
        self.assertEqual(original.get_value(), 1)
        self.assertIsInstance(duplicate.make_number_changeable("2.00"), m.DecimalNumber)
        self.assertIn("2.00", tex.string)
        self.assert_map(duplicate)

    def test_invalid_configuration_leaves_source_and_topology_unchanged(self):
        tex = m.Tex("1.00+1.00")
        before = metadata(tex), identities(tex)
        with self.assertRaises((TypeError, ValueError, RuntimeError)):
            tex.make_number_changeable("1.00", replace_all=True, definitely_unknown=True)
        self.assertEqual((metadata(tex), identities(tex)), before)

    def test_cross_part_splice_failure_rolls_back_both_native_parents(self):
        tex = JoinedTex("1", "2.50+3.00")
        scene = m.Scene()
        scene.add(tex)
        before = metadata(tex), identities(tex), list(scene.mobjects)
        parents = list(tex.submobjects)
        old_children = [list(parent.submobjects) for parent in parents]
        native_set = parents[1].set_submobjects
        sentinel = RuntimeError("authored splice failed after native mutation")
        calls = []

        def failing_once(children):
            result = native_set(children)
            calls.append(tuple(children))
            if len(calls) == 1:
                raise sentinel
            return result

        parents[1].set_submobjects = failing_once
        self.addCleanup(parents[1].__dict__.pop, "set_submobjects", None)
        with self.assertRaises(RuntimeError) as caught:
            tex.make_number_changeable("12.50")
        self.assertIs(caught.exception, sentinel)
        self.assertEqual((metadata(tex), identities(tex), list(scene.mobjects)), before)
        self.assertEqual([list(parent.submobjects) for parent in parents], old_children)
        self.assertEqual(len(calls), 2)
        self.assertIsInstance(tex.make_number_changeable("12.50"), m.DecimalNumber)
        self.assert_map(tex)

    def test_whole_matrix_entry_failure_restores_native_points(self):
        entry = m.TexMatrix([["12.50"]]).get_mob_matrix()[0][0]
        before, points, children = metadata(entry), entry.get_points().copy(), list(entry.submobjects)
        native_set = entry.set_submobjects
        sentinel = RuntimeError("matrix splice failed")
        failed = False

        def failing_once(values):
            nonlocal failed
            result = native_set(values)
            if not failed:
                failed = True
                raise sentinel
            return result

        entry.set_submobjects = failing_once
        self.addCleanup(entry.__dict__.pop, "set_submobjects", None)
        with self.assertRaises(RuntimeError) as caught:
            entry.make_number_changeable("12.50")
        self.assertIs(caught.exception, sentinel)
        self.assertEqual(metadata(entry), before)
        self.assertEqual(list(entry.submobjects), children)
        np.testing.assert_array_equal(entry.get_points(), points)
        self.assertIsInstance(entry.make_number_changeable("12.50"), m.DecimalNumber)

    def test_same_number_replaced_in_reverse_order_preserves_every_span(self):
        tex = m.Tex("x=1.00+1.00+1.00+1.00")
        decimals = [tex.make_number_changeable("1.00", index=i) for i in (3, 1, 1, 0)]
        self.assertTrue(all(isinstance(decimal, m.DecimalNumber) for decimal in decimals))
        self.assertEqual(len(set(map(id, decimals))), 4)
        self.assertEqual(tex.string, r"x=\decimalmob+\decimalmob+\decimalmob+\decimalmob")
        self.assertEqual(len(tex.get_parts_by_tex(r"\decimalmob")), 4)
        self.assert_map(tex)

    def test_missing_and_past_end_are_empty_without_mutation(self):
        tex = m.Tex("1.00")
        before = metadata(tex)
        for value, index in (("9.99", 0), ("1.00", 4)):
            result = tex.make_number_changeable(value, index=index)
            self.assertIsInstance(result, m.VMobject)
            self.assertEqual(len(result), 0)
        self.assertEqual(metadata(tex), before)


suite = unittest.defaultTestLoader.loadTestsFromTestCase(LiveTexAcceptance)
result = unittest.TextTestRunner(verbosity=2).run(suite)
assert result.wasSuccessful(), "native live-TeX acceptance failed"
