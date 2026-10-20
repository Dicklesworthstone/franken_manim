"""Native glyph, style, span and pixel witnesses for rich text construction."""
import gc
from pathlib import Path
import re
import tempfile
import unittest

import numpy as np
import manimlib as m
from manimlib.mobject.svg.text_mobject import Text as QualifiedText


def points(obj):
    return np.array(obj.get_all_points(), copy=True)


def records(obj):
    return [member.data.copy() for member in obj.family_members_with_points()]


class TextAuthoringTests(unittest.TestCase):
    def test_bundled_fonts_select_different_native_glyph_geometry(self):
        serif = m.Text("Typography", font="Computer Modern")
        sans = m.Text("Typography", font="IBM Plex Sans")
        mono = m.Text("Typography", font="CM Typewriter")
        self.assertIs(QualifiedText, m.Text)
        self.assertFalse(np.array_equal(points(serif), points(sans)))
        self.assertFalse(np.array_equal(points(sans), points(mono)))
        self.assertEqual(serif.get_string(), "Typography")

    def test_bold_and_italic_equal_native_markup_faces(self):
        for options, content in (({"weight": "BOLD"}, "<b>A</b>"),
                                 ({"slant": "ITALIC"}, "<i>A</i>"),
                                 ({"slant": "ITALIC", "weight": "BOLD"}, "<b><i>A</i></b>")):
            np.testing.assert_array_equal(points(m.Text("A", **options)), points(m.MarkupText(content)))

    def test_markup_overrides_inherited_font_and_face_without_span_rewriting(self):
        source = 'é<span weight="normal" font_family="Computer Modern">B</span>C'
        text = m.MarkupText(source, font="IBM Plex Sans", weight="BOLD")
        self.assertEqual(text.get_string(), source)
        self.assertEqual(text._string_sub_spans[0], (0, 2))
        part = text.get_part_by_text("B")
        expected = m.Text("B")
        np.testing.assert_allclose(points(part.copy().center()), points(expected), atol=1e-6)

    def test_local_face_maps_and_long_aliases_keep_unicode_selectors(self):
        text = m.Text("éABA", text2font={"B": "IBM Plex Sans"}, t2w={"A": "BOLD"}, t2s={"B": "ITALIC"})
        for substring, options in (("A", {"weight": "BOLD"}), ("B", {"font": "IBM Plex Sans", "slant": "ITALIC"})):
            for part in text.get_parts_by_text(substring):
                np.testing.assert_allclose(points(part.copy().center()), points(m.Text(substring, **options)), atol=1e-6)
        self.assertEqual(text._string_sub_spans[0], (0, 2))
        self.assertEqual(len(text.get_parts_by_text("A")), 2)

    def test_global_and_local_gradients_and_final_color_map(self):
        local = m.Text("ABC", t2g={"ABC": [m.RED, m.BLUE]})
        colors = [part.get_fill_color() for part in local]
        self.assertEqual(colors[0], m.RED)
        self.assertEqual(colors[-1], m.BLUE)
        self.assertNotEqual(colors[1], colors[0])
        text = m.Text("ABC", gradient=(c for c in [m.RED, m.BLUE]), t2c={"B": m.GREEN})
        self.assertEqual(text.get_part_by_text("B")[0].get_fill_color(), m.GREEN)

    def test_alignment_changes_lines_not_glyph_shapes(self):
        source = "AAAA\nA"
        left, right = m.Text(source, alignment="LEFT"), m.Text(source, alignment="RIGHT")
        self.assertLess(left[-1].get_x(), right[-1].get_x())
        center = m.Text(source, alignment="CENTER")
        self.assertLess(left[-1].get_x(), center[-1].get_x())
        self.assertLess(center[-1].get_x(), right[-1].get_x())
        self.assertAlmostEqual(left[-1].get_width(), right[-1].get_width(), places=6)

    def test_line_spacing_changes_baselines_without_scaling_ink(self):
        first, second = m.Text("A\nA", lsh=1), m.Text("A\nA", line_spacing_height=2)
        self.assertAlmostEqual(second[0].get_y() - second[1].get_y(),
                               2 * (first[0].get_y() - first[1].get_y()), places=5)
        self.assertAlmostEqual(first[0].get_height(), second[0].get_height(), places=6)

    def test_literal_text_is_not_interpreted_as_markup(self):
        text = m.Text("<b>A</b>", weight="BOLD")
        self.assertEqual(len(text._string_sub_spans), 8)
        self.assertEqual(text.get_string(), "<b>A</b>")

    def test_unknown_fonts_and_unimplemented_styles_refuse(self):
        for options in ({"font": "not-a-font"}, {"t2f": {"A": "not-a-font"}},
                        {"weight": "HEAVY"}, {"slant": "OBLIQUE"},
                        {"alignment": "diagonal"}, {"lsh": 0}, {"line_width": float("nan")},
                        {"t2w": {"A": "BOLDER"}}, {"t2f": {("A", 3): "Computer Modern"}}):
            with self.subTest(options=options), self.assertRaises((ValueError, TypeError)):
                m.Text("A", **options)
        with self.assertRaises(ValueError):
            m.Text("", font="not-a-font")

    def test_copy_animation_and_matching_preserve_native_spans(self):
        source = m.Text("Move", font="IBM Plex Sans", weight="BOLD", t2c={"M": m.RED})
        duplicate = source.copy()
        scene = m.Scene()
        scene.add(source)
        scene.play(source.animate.shift(m.RIGHT), run_time=.125)
        self.assertFalse(np.array_equal(points(source), points(duplicate)))
        self.assertEqual(source._string_sub_spans, duplicate._string_sub_spans)
        target = m.Text("Move", font="CM Typewriter").shift(m.UP)
        scene.play(m.TransformMatchingStrings(source, target, run_time=.125))
        self.assertIn(target, scene.mobjects)
        self.assertNotIn(source, scene.mobjects)

    def test_constructor_hooks_and_qualified_aliases_are_preserved(self):
        calls = []
        class Authored(m.Text):
            def set_color_by_text_to_color_map(self, mapping):
                calls.append(self)
                return super().set_color_by_text_to_color_map(mapping)
        text = Authored("A", font="IBM Plex Sans", t2w={"A": "BOLD"})
        self.assertEqual(calls, [text])
        self.assertIsInstance(text, QualifiedText)

    def test_inner_monospace_tag_overrides_inherited_family_without_rewriting_source(self):
        source = "<tt>A</tt>"
        actual = m.MarkupText(source, font="IBM Plex Sans")
        expected = m.Text("A", font="CM Typewriter")
        np.testing.assert_array_equal(points(actual), points(expected))
        self.assertEqual(actual._string_sub_spans, [(4, 5)])
        self.assertEqual(actual.get_string(), source)

    def test_resource_bounds_and_nonfinite_paint_are_refused(self):
        import itertools
        cases = (("A" * 262_145, {}),
                 ("A", {"t2w": {str(i): "BOLD" for i in range(4097)}}),
                 ("A", {"gradient": itertools.repeat(m.RED)}),
                 ("A", {"gradient": [[float("nan"), 0, 0]]}),
                 ("A", {"gradient": m.RED}),
                 ("A", {"font_size": float("inf")}),
                 ("A", {"height": 0}),
                 ("A" * 10_000, {"t2w": {tuple((0, 10_000) for _ in range(2000)): "BOLD"}}))
        for source, options in cases:
            with self.subTest(options=tuple(options)), self.assertRaises((ValueError, TypeError)):
                m.Text(source, **options)
        with self.assertRaisesRegex(NotImplementedError, "Pango attributes"):
            m.Text("A", global_config={"letter_spacing": 10})

    def test_regex_composite_and_negative_range_maps_select_exact_unicode_occurrences(self):
        source = "éABA éABA"
        text = m.Text(source, t2f={re.compile(r"B(?=A$)"): "IBM Plex Sans"},
                      t2w={("A", (None, 1)): "BOLD"}, t2s={(-2, -1): "ITALIC"})
        first, second = text.get_parts_by_text("B")
        np.testing.assert_allclose(points(first.copy().center()), points(m.Text("B")), atol=1e-6)
        np.testing.assert_allclose(points(second.copy().center()),
                                   points(m.Text("B", font="IBM Plex Sans", slant="ITALIC")), atol=1e-6)
        np.testing.assert_allclose(points(text[0].copy().center()),
                                   points(m.Text("é", weight="BOLD")), atol=1e-6)
        self.assertEqual(text._string_sub_spans[0], (0, 2))
        self.assertEqual(text.get_string(), source)
        for part in text.get_parts_by_text("A"):
            np.testing.assert_allclose(points(part.copy().center()),
                                       points(m.Text("A", weight="BOLD")), atol=1e-6)

    def test_overlapping_mixed_selectors_keep_authored_order_and_other_style_fields(self):
        text = m.Text("AAAA", t2f={("A", "B"): "IBM Plex Sans"},
                      t2w={"A": "BOLD", (1, None): "NORMAL", re.compile(r"A$"): "BOLD"},
                      t2s={(2, None): "ITALIC"})
        for index, (weight, slant) in enumerate((("BOLD", "NORMAL"), ("NORMAL", "NORMAL"),
                                               ("NORMAL", "ITALIC"), ("BOLD", "ITALIC"))):
            expected = m.Text("A", font="IBM Plex Sans", weight=weight, slant=slant)
            np.testing.assert_allclose(points(text[index].copy().center()), points(expected), atol=1e-6)

    def test_markup_source_ranges_select_whole_entities_without_rewriting_source(self):
        source = "é<b>A&amp;A</b>"
        start = source.index("&amp;")
        text = m.MarkupText(source, t2w={(start, start + 5): "NORMAL"},
                            t2f={(start + 1, start + 4): "IBM Plex Sans"})
        entity = text.get_part_by_text("&amp;")
        np.testing.assert_allclose(points(entity.copy().center()), points(m.Text("&")), atol=1e-6)
        byte_start = len(source[:start].encode("utf-8"))
        self.assertEqual(text._string_sub_spans[2], (byte_start, byte_start + 5))
        self.assertEqual(text.get_string(), source)
        for part in text.get_parts_by_text("A"):
            np.testing.assert_allclose(points(part.copy().center()),
                                       points(m.Text("A", weight="BOLD")), atol=1e-6)

    def test_regex_gradients_restart_per_match_and_final_color_selectors_win(self):
        text = m.Text("AB AB", t2g={re.compile("AB"): [m.RED, m.BLUE]},
                      t2c={(0, 1): m.GREEN})
        first, second = text.get_parts_by_text("AB")
        self.assertEqual([part.get_fill_color() for part in first], [m.GREEN, m.BLUE])
        self.assertEqual([part.get_fill_color() for part in second], [m.RED, m.BLUE])

    def test_regeneration_uses_current_layout_style_maps_and_whole_gradient(self):
        text = m.Text("seed", gradient=(color for color in (m.RED, m.BLUE)),
                      fill_opacity=.35, stroke_color=m.YELLOW, stroke_width=.8)
        source = "éAAA\nBC"
        options = dict(font="IBM Plex Sans", font_size=36, weight="BOLD", slant="ITALIC",
                       alignment="RIGHT", lsh=1.75, line_width=8., indent=.25,
                       justify=False, disable_ligatures=True)
        for name, value in options.items():
            setattr(text, name, value)
        text.text = source
        text.t2w = {(-2, -1): "NORMAL"}
        text.t2g = {re.compile("A+"): (m.GREEN, m.BLUE)}
        text.t2c = {"B": m.GREEN}
        text.gradient = (m.WHITE, m.RED)
        text.init_points()
        expected = m.Text(source, **options, t2w=text.t2w, t2g=text.t2g, t2c=text.t2c,
                          gradient=text.gradient, fill_opacity=.35,
                          stroke_color=m.YELLOW, stroke_width=.8)
        np.testing.assert_allclose(points(text.copy().center()), points(expected), atol=1e-6)
        self.assertEqual(text._string_sub_spans, expected._string_sub_spans)
        self.assertEqual(text.get_string(), source)
        for actual, wanted in zip(text, expected):
            self.assertEqual(actual.get_fill_color(), wanted.get_fill_color())
            self.assertEqual(actual.get_stroke_color(), wanted.get_stroke_color())
            self.assertAlmostEqual(actual.get_fill_opacity(), .35, places=6)
            self.assertAlmostEqual(actual.get_stroke_width(), wanted.get_stroke_width(), places=6)

    def test_regeneration_freezes_single_use_gradients_and_accepts_current_local_stops(self):
        text = m.Text("AB", gradient=(color for color in (m.WHITE, m.BLUE)),
                      t2g={"AB": (color for color in (m.RED, m.BLUE))})
        text.init_points()
        self.assertEqual(text[-1].get_fill_color(), m.BLUE)
        text.text = "AB AB"
        text.gradient = None
        text.t2g["AB"] = (color for color in (m.GREEN, m.YELLOW))
        text.init_points()
        text.init_points()
        for part in text.get_parts_by_text("AB"):
            self.assertEqual([glyph.get_fill_color() for glyph in part], [m.GREEN, m.YELLOW])

    def test_code_regeneration_rehighlights_current_source_language_theme_and_font(self):
        code = m.Code("value = 1")
        source = "fn main() { let value = 2; }"
        code.code, code.language, code.code_style, code.font = source, "rust", "default", "IBM Plex Sans"
        code.t2w = {re.compile(r"\bfn\b"): "BOLD"}
        code.t2c = {"value": m.GREEN}
        code.init_points()
        expected = m.Code(source, language="rust", code_style="default", font="IBM Plex Sans",
                          t2w=code.t2w, t2c=code.t2c)
        np.testing.assert_allclose(points(code.copy().center()), points(expected), atol=1e-6)
        self.assertEqual([glyph.get_fill_color() for glyph in code],
                         [glyph.get_fill_color() for glyph in expected])
        self.assertEqual((code.code, code.text, code.string), (source, source, source))
        self.assertEqual(code.native_font, "IBM Plex Sans")
        self.assertFalse(code.has_points())
        code.text, code.language, code.code_style, code.font = "print(3)", "python", "monokai", "Consolas"
        code.t2w, code.t2c = {}, {}
        code.init_points()
        self.assertEqual(code.code, "print(3)")
        self.assertEqual(code.native_font, "CM Typewriter")
        np.testing.assert_allclose(points(code.copy().center()), points(m.Code("print(3)")), atol=1e-6)

    def test_public_selector_and_color_hooks_run_once_per_build_with_the_new_source(self):
        events = []
        class Authored(m.Text):
            def find_spans_by_selector(self, selector):
                if selector == "last":
                    events.append(("selector", self.string))
                    return [(len(self.string) - 1, len(self.string))]
                return super().find_spans_by_selector(selector)
            def init_colors(self):
                events.append("colors")
                return super().init_colors()
            def set_color_by_text_to_color_map(self, mapping):
                events.append("paint")
                return super().set_color_by_text_to_color_map(mapping)
        text = Authored("AB", t2w={"last": "BOLD"})
        text.text = "éABC"
        text.init_points()
        self.assertEqual(events, [("selector", "AB"), "colors", "paint",
                                  ("selector", "éABC"), "colors", "paint"])
        np.testing.assert_allclose(points(text[-1].copy().center()),
                                   points(m.Text("C", weight="BOLD")), atol=1e-6)

    def test_failed_regeneration_restores_published_source_family_and_releases_guard(self):
        text = m.Text("AB")
        original_points, children, spans = points(text), tuple(text.submobjects), list(text._string_sub_spans)
        text.text, text.font = "éZ", "not-a-font"
        with self.assertRaises(ValueError):
            text.init_points()
        self.assertEqual(text.string, "AB")
        self.assertEqual(tuple(text.submobjects), children)
        self.assertEqual(text._string_sub_spans, spans)
        np.testing.assert_array_equal(points(text), original_points)
        text.font, text.t2w = "", {"Z": "BOLD"}
        selector = text.find_spans_by_selector
        failure = RuntimeError("authored selector failure")
        def failing_selector(value):
            raise failure
        text.find_spans_by_selector = failing_selector
        with self.assertRaises(RuntimeError) as caught:
            text.init_points()
        self.assertIs(caught.exception, failure)
        self.assertEqual(text.string, "AB")
        text.find_spans_by_selector = lambda value: text.init_points()
        with self.assertRaisesRegex(RuntimeError, "regeneration is already active"):
            text.init_points()
        self.assertEqual(tuple(text.submobjects), children)
        text.find_spans_by_selector = selector
        text.init_points()
        self.assertEqual(text.string, "éZ")
        self.assertEqual(len(text.get_parts_by_text("Z")), 1)

    def test_copied_text_regeneration_preserves_its_own_hook_children_and_native_selections(self):
        text = m.Text("AB", t2w={"A": "BOLD"})
        hook = m.Dot().shift(2 * m.RIGHT)
        text.add(hook)
        original_points = points(text)
        duplicate = text.copy()
        copied_hook = duplicate.submobjects[-1]
        duplicate.text, duplicate.t2w = "C", {"C": "BOLD"}
        duplicate.init_points()
        self.assertIs(duplicate.submobjects[0], copied_hook)
        self.assertIsNot(copied_hook, hook)
        self.assertEqual(len(duplicate._fmn_string_children), 1)
        selected = duplicate.get_part_by_text("C")
        np.testing.assert_allclose(points(selected.copy().center()),
                                   points(m.Text("C", weight="BOLD")), atol=1e-6)
        self.assertEqual(text.string, "AB")
        np.testing.assert_array_equal(points(text), original_points)

    def test_textbox_replacement_transfers_the_gradient_recipe_for_later_regeneration(self):
        widget = m.Textbox("AB", text_kwargs={"gradient": (m.RED, m.BLUE)})
        label = widget.text
        widget.text_kwargs["gradient"] = (m.GREEN, m.YELLOW)
        widget.update_text("ABCD")
        self.assertIs(widget.text, label)
        label.init_points()
        self.assertEqual(label.string, "ABCD")
        self.assertEqual(label[0].get_fill_color(), m.GREEN)
        self.assertEqual(label[-1].get_fill_color(), m.YELLOW)

    def test_native_frames_match_thread_counts_and_differ_from_plain_text(self):
        def render(output, threads, styled):
            scene = m.Scene()
            with scene.render_session(output, format="png_sequence", resolution=(160, 90), fps=4, threads=threads):
                text = m.Text("Native", font="IBM Plex Sans" if styled else "", weight="BOLD" if styled else "NORMAL")
                scene.add(text)
                scene.play(text.animate(rate_func=m.linear).shift(m.RIGHT), run_time=.5)
            return [path.read_bytes() for path in sorted(Path(output).glob("*.png"))]
        with tempfile.TemporaryDirectory() as root:
            path = Path(root)
            frames = render(path/"one", 1, True)
            self.assertEqual(len(frames), 2)
            self.assertEqual(frames, render(path/"four", 4, True))
            self.assertNotEqual(frames, render(path/"plain", 1, False))
            self.assertNotEqual(frames[0], frames[1])


suite = unittest.defaultTestLoader.loadTestsFromTestCase(TextAuthoringTests)
assert suite.countTestCases() == 25
result = unittest.TextTestRunner(verbosity=2).run(suite)
gc.collect()
if not result.wasSuccessful():
    raise AssertionError("native text authoring acceptance failed")
