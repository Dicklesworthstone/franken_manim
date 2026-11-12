"""Author text configs through production code without loading a native wheel.

Only color conversion and literal native span lookup are boundary doubles. The
real selector method, constructor installer and recipe compiler execute here;
native text_authoring.py separately checks glyphs, scopes, decorations and pixels.
"""
from __future__ import annotations

import ast
from pathlib import Path
import re
import sys
from types import ModuleType
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "python"))
from fmn_python import text_authoring as authoring


def color(value):
    if isinstance(value, str):
        known = {"red": (1., 0., 0.), "blue": (0., 0., 1.)}
        if value in known:
            return known[value]
        if re.fullmatch(r"#[0-9a-fA-F]{6}", value):
            return tuple(int(value[i:i + 2], 16) / 255 for i in (1, 3, 5))
        raise ValueError("unknown test color")
    return value


def production_methods():
    path = ROOT / "python/manimlib_bootstrap.py"
    tree = ast.parse(path.read_text())
    definitions = []
    for owner, name in (("StringMobject", "find_spans_by_selector"), ("MarkupText", "__init__")):
        cls = next(node for node in tree.body if isinstance(node, ast.ClassDef) and node.name == owner)
        definitions.append(next(node for node in cls.body if isinstance(node, ast.FunctionDef) and node.name == name))
    module = ast.Module(body=[ast.ImportFrom(module="__future__", names=[ast.alias(name="annotations")], level=0),
                             *definitions], type_ignores=[])
    namespace = {"_re": re,
                 "_string_find_spans": lambda source, needle: [m.span() for m in re.finditer(re.escape(needle), source)]}
    # ubs:ignore -- executes selected definitions from the repository's fixed bootstrap.
    exec(compile(ast.fix_missing_locations(module), str(path), "exec"), namespace)
    return namespace


METHODS = production_methods()


class AuthoredText:
    find_spans_by_selector = METHODS["find_spans_by_selector"]

    def __init__(self, source="éABC éABC", **changes):
        self.text = self.string = source
        self.font, self.font_size = "Computer Modern", 48
        self.weight, self.slant, self.alignment = "NORMAL", "NORMAL", "LEFT"
        self.lsh, self.line_width, self.gradient = None, None, None
        self.indent, self.justify, self.disable_ligatures = 0, False, True
        self.t2c, self.t2f, self.t2s, self.t2w, self.t2g = {}, {}, {}, {}, {}
        self.global_config, self.local_configs = {}, {}
        vars(self).update(changes)


class TextConfigurationProtocolTests(unittest.TestCase):
    def setUp(self):
        self.g = {"Code": type("Code", (), {}), "color_to_rgb": color}

    def recipe(self, obj=None, source=None, **changes):
        obj = obj or AuthoredText(**changes)
        return authoring.text_recipe(self.g, obj, obj.text if source is None else source)[0]

    def test_global_attributes_use_typed_native_fields_and_preserve_public_defaults(self):
        obj = AuthoredText(global_config={"foreground": "red", "font_family": "IBM Plex Sans",
                                         "font_style": "oblique", "font_weight": "700",
                                         "underline": "single", "strikethrough": "true",
                                         "font_size": "72pt"})
        options = self.recipe(obj)
        self.assertEqual(options["global_style"], {"color": (1., 0., 0.), "family": "IBM Plex Sans",
                         "italic": True, "bold": True, "underline": True, "strike": True,
                         "size": ("absolute", 1.5)})
        self.assertEqual((obj.font, obj.font_size, obj.weight, obj.lsh), ("Computer Modern", 48, "NORMAL", None))
        self.assertEqual(options["font_size"], 48)
        self.assertEqual(options["line_spacing"], 1)
        self.assertEqual(options, self.recipe(obj))

    def test_attribute_aliases_override_only_their_own_channel(self):
        config = {"foreground": "red", "fgcolor": "blue", "color": "#00ff00",
                  "font_family": "Computer Modern", "face": "IBM Plex Sans",
                  "font_weight": "700", "weight": "normal", "font_style": "italic", "style": "normal"}
        fields = self.recipe(global_config=config)["global_style"]
        self.assertEqual(fields, {"color": (0., 1., 0.), "family": "IBM Plex Sans", "bold": False, "italic": False})

    def test_pango_units_points_and_percentages_use_the_original_font_size(self):
        for value, expected in ((49152, ("absolute", 1.)), ("24576", ("absolute", .5)),
                                ("12.5pt", ("absolute", 12.5 / 48)), ("150%", ("relative", 1.5)),
                                ("larger", ("relative", 1.2)), ("smaller", ("relative", 1 / 1.2))):
            with self.subTest(value=value):
                self.assertEqual(self.recipe(global_config={"size": value})["global_style"]["size"], expected)

    def test_global_line_height_keeps_relative_and_absolute_native_baselines_distinct(self):
        self.assertEqual(self.recipe(global_config={"line_height": "1.5"})["line_height"], ("relative", 1.5))
        self.assertEqual(self.recipe(global_config={"line_height": 98304})["line_height"], ("absolute", 2.))
        with self.assertRaisesRegex(NotImplementedError, "local_configs.*line_height"):
            self.recipe(local_configs={"A": {"line_height": 1.5}})

    def test_only_uniform_complete_ligature_features_map_to_the_native_switch(self):
        for bit in (0, 1):
            config = {"font_features": ",".join(f"{key}={bit}" for key in ("liga", "hlig", "clig", "dlig"))}
            self.assertEqual(self.recipe(global_config=config)["disable_ligatures"], not bit)
        for value in ("kern=0", "liga=0", "liga=0,dlig=1,clig=0,hlig=0", "", "liga=0,liga=0,clig=0,hlig=0"):
            with self.subTest(value=value), self.assertRaisesRegex(NotImplementedError, "font_features"):
                self.recipe(global_config={"font_features": value})

    def test_literal_regex_and_negative_range_selectors_retain_utf8_source_bytes(self):
        obj = AuthoredText(local_configs={"é": {"weight": "bold"}, re.compile("B(?=C$)"): {"style": "italic"},
                                         (-1, None): {"color": "blue"}})
        self.assertEqual(self.recipe(obj)["scoped_styles"], [
            (0, 2, {"bold": True}), (6, 8, {"bold": True}),
            (9, 10, {"italic": True}), (10, 11, {"color": (0., 0., 1.)})])
        self.assertEqual(obj.string, "éABC éABC")

    def test_nested_and_equal_ranges_are_sent_intact_to_the_native_scope_owner(self):
        obj = AuthoredText("ABCDE", local_configs={"BCD": {"weight": "bold"},
                           "ABCDE": {"weight": "normal"}, (1, 4): {"style": "italic"}})
        self.assertEqual(self.recipe(obj)["scoped_styles"], [
            (1, 4, {"bold": True}), (0, 5, {"bold": False}), (1, 4, {"italic": True})])

    def test_markup_containment_is_not_erased_by_rewriting_or_clipping_ranges(self):
        source = "é<b>A&amp;B</b>"
        obj = AuthoredText(source, local_configs={source[1:]: {"weight": "normal"}, "&amp;": {"size": "200%"}})
        scopes = self.recipe(obj)["scoped_styles"]
        self.assertEqual(scopes, [(2, 16, {"bold": False}), (6, 11, {"size": ("relative", 2.)})])
        self.assertEqual(obj.string, source)

    def test_existing_t2_maps_precede_equal_range_local_configs(self):
        obj = AuthoredText("ABC", t2w={"B": "BOLD"}, t2f={"B": "Computer Modern"},
                           local_configs={"B": {"weight": "normal", "face": "IBM Plex Sans"}})
        options = self.recipe(obj)
        self.assertEqual(options["t2f"], [])
        self.assertEqual(options["t2w"], [])
        self.assertEqual(options["scoped_styles"], [(1, 2, {"family": "Computer Modern"}),
                         (1, 2, {"bold": True}), (1, 2, {"bold": False, "family": "IBM Plex Sans"})])

    def test_no_local_configs_preserve_the_existing_ordered_map_route(self):
        options = self.recipe(t2w={"A": "BOLD"})
        self.assertEqual(options["scoped_styles"], [])
        self.assertEqual(options["t2w"], [([(2, 3), (8, 9)], True)])

    def test_live_regeneration_reads_new_configs_and_restores_published_source_during_recipe(self):
        obj = AuthoredText("AB", local_configs={"B": {"weight": "bold"}})
        self.recipe(obj)
        obj.text, obj.local_configs = "éZ", {"Z": {"font_size": "24pt"}}
        result = self.recipe(obj)
        self.assertEqual(result["scoped_styles"], [(2, 3, {"size": ("absolute", .5)})])
        self.assertEqual(obj.string, "AB")

    def test_authored_selector_runs_once_against_new_source(self):
        calls = []
        obj = AuthoredText("AB", local_configs={"last": {"weight": "bold"}})
        def selector(value):
            calls.append((value, obj.string))
            return [(len(obj.string) - 1, len(obj.string))]
        obj.find_spans_by_selector = selector
        options = self.recipe(obj, "éABC")
        self.assertEqual(calls, [("last", "éABC")])
        self.assertEqual(options["scoped_styles"], [(4, 5, {"bold": True})])
        self.assertEqual(obj.string, "AB")

    def test_selector_failure_restores_source_and_keeps_original_exception(self):
        obj = AuthoredText("AB", local_configs={"A": {"weight": "bold"}})
        failure = LookupError("authored selector")
        def selector(value):
            raise failure
        obj.find_spans_by_selector = selector
        with self.assertRaises(LookupError) as caught:
            self.recipe(obj, "éZ")
        self.assertIs(caught.exception, failure)
        self.assertEqual(obj.string, "AB")

    def test_unknown_pango_attributes_fail_before_any_selector_runs(self):
        obj = AuthoredText(local_configs={"A": {"letter_spacing": 10}})
        obj.find_spans_by_selector = lambda _: self.fail("unsupported attributes must be rejected first")
        with self.assertRaisesRegex(NotImplementedError, "local_configs.*letter_spacing"):
            self.recipe(obj)
        with self.assertRaisesRegex(NotImplementedError, "global_config.*font"):
            self.recipe(global_config={"font": "Sans Italic 12"})

    def test_unsupported_face_or_decoration_is_not_silently_substituted(self):
        for key, value in (("weight", "heavy"), ("underline", "double"), ("size", "medium"), ("color", "#ff000080")):
            with self.subTest(key=key), self.assertRaisesRegex(NotImplementedError, key):
                self.recipe(global_config={key: value})

    def test_nonfinite_and_invalid_config_values_fail(self):
        cases = ({"size": 0}, {"size": "-20%"}, {"size": "nanpt"}, {"size": 1234.5},
                 {"color": [float("nan"), 0, 0]}, {"color": [2, 0, 0]}, {"line_height": float("inf")},
                 {"strikethrough": "yes"}, {"face": 3}, {"style": "diagonal"})
        for config in cases:
            with self.subTest(config=config), self.assertRaises((ValueError, TypeError)):
                self.recipe(global_config=config)

    def test_mapping_range_and_work_budgets_are_shared_with_text_maps(self):
        with self.assertRaises(TypeError):
            self.recipe(global_config=[])
        with self.assertRaisesRegex(ValueError, "4096-entry"):
            self.recipe(local_configs={str(i): {} for i in range(4097)})
        repeated = tuple((0, 1) for _ in range(4097))
        with self.assertRaisesRegex(ValueError, "scoped styles.*4096-entry"):
            self.recipe(AuthoredText("A", local_configs={repeated: {"weight": "bold"}}))
        obj = AuthoredText("A" * 10000, local_configs={tuple((0, 10000) for _ in range(2000)): {"weight": "bold"}})
        with self.assertRaisesRegex(ValueError, "byte-work"):
            self.recipe(obj)
        self.assertEqual(obj.string, "A" * 10000)

    def test_constructor_preserves_configs_as_writable_independent_dictionaries(self):
        native = ModuleType("text_constructor_boundary")
        class Markup:
            __init__ = METHODS["__init__"]
            def center(self):
                return self
            def select_part(self, *args, **kwargs):
                return None
            def set_color_by_text_to_color_map(self, mapping):
                pass
        class Code(Markup):
            def __init__(self, code, **kwargs):
                pass
        native.MarkupText, native.Code = Markup, Code
        native._typesetting_default = lambda *args: ""
        native._preflight_vmobject_style_kwargs = lambda options: None
        source_configs = {"weight": "bold"}, {"B": {"style": "italic"}}
        with patch("fmn_python.string_lifecycle.initialize_string", lambda *args: None):
            authoring.install_text_authoring(native)
            obj = Markup("AB", global_config=source_configs[0], local_configs=source_configs[1])
        self.assertEqual((obj.global_config, obj.local_configs), source_configs)
        self.assertIsNot(obj.global_config, source_configs[0])
        self.assertIsNot(obj.local_configs["B"], source_configs[1]["B"])
        obj.global_config["weight"] = "normal"
        obj.local_configs["B"]["style"] = "normal"
        self.assertEqual(source_configs, ({"weight": "bold"}, {"B": {"style": "italic"}}))


if __name__ == "__main__":
    unittest.main()
