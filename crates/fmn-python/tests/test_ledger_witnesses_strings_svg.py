"""Ledger witnesses for strings_svg (fm-6u1g): each test calls a manimlib symbol the Parity Ledger
marks same/improved and checks the Reference's behaviour (manimlib @ 6199a00d)."""
import os
import re
import tempfile
import types
import unittest
import xml.etree.ElementTree as ET

import numpy as np

from test_parallel_batch_native import native_wheel_available

SVG_NS = "{http://www.w3.org/2000/svg}"


def _floats(path_data):
    return [float(token) for token in re.findall(r"-?\d+(?:\.\d+)?(?:e[-+]?\d+)?", path_data)]


@unittest.skipUnless(native_wheel_available(), "requires the built FrankenManim native wheel")
class StringsSvgWitnesses(unittest.TestCase):
    # manimlib.mobject.svg.old_tex_mobject (Appendix C-18: the pinned Reference cannot construct
    # these classes; the portal constructs them, so the checks follow the Reference's code intent).

    def test_old_tex_set_bstroke(self):
        from manimlib import BLACK, BLUE
        from manimlib.mobject.svg.old_tex_mobject import OldTex

        # Reference old_tex_mobject.py:312: set_stroke(color, width, background=True); return self.
        tex = OldTex("a", "+", "b")
        glyphs = tex.family_members_with_points()
        self.assertEqual(len(glyphs), 3)
        for glyph in glyphs:
            self.assertFalse(glyph.get_style()["stroke_behind"])
        fills = [glyph.get_fill_color() for glyph in glyphs]
        self.assertIs(tex.set_bstroke(BLUE, 3), tex)
        for glyph, fill in zip(glyphs, fills):
            self.assertEqual(glyph.get_stroke_color(), BLUE.upper())
            self.assertEqual(glyph.get_stroke_width(), 3.0)
            self.assertTrue(glyph.get_style()["stroke_behind"])
            self.assertEqual(glyph.get_fill_color(), fill)

        # Defaults: color=BLACK, width=4.
        plain = OldTex("x")
        self.assertIs(plain.set_bstroke(), plain)
        for glyph in plain.family_members_with_points():
            self.assertEqual(glyph.get_stroke_color(), BLACK.upper())
            self.assertEqual(glyph.get_stroke_width(), 4.0)
            self.assertTrue(glyph.get_style()["stroke_behind"])

    def test_old_tex_sort_alphabetically(self):
        from manimlib.mobject.svg.old_tex_mobject import OldTex

        # Reference old_tex_mobject.py:309: submobjects.sort(key=lambda m: m.get_tex()).
        tex = OldTex("c", "a", "b")
        parts = list(tex.submobjects)
        self.assertEqual([part.get_tex() for part in parts], ["c", "a", "b"])
        centers = [part.get_center().copy() for part in parts]
        self.assertTrue(centers[0][0] < centers[1][0] < centers[2][0])
        tex.sort_alphabetically()
        self.assertEqual([part.get_tex() for part in tex.submobjects], ["a", "b", "c"])
        # Same part objects, reordered, and nothing moved.
        for got, expected in zip(tex.submobjects, (parts[1], parts[2], parts[0])):
            self.assertIs(got, expected)
        np.testing.assert_allclose(tex.submobjects[0].get_center(), centers[1], atol=1e-12)
        np.testing.assert_allclose(tex.submobjects[2].get_center(), centers[0], atol=1e-12)

    def test_single_string_tex_hash_seed(self):
        from manimlib.mobject.svg.old_tex_mobject import SingleStringTex

        # Reference old_tex_mobject.py:65: (class name, svg_default, path_string_config,
        # tex_string, alignment, math_mode, template, additional_preamble).
        tex = SingleStringTex("abc")
        seed = tex.hash_seed
        self.assertIsInstance(seed, tuple)
        self.assertEqual(len(seed), 8)
        self.assertEqual(seed[0], "SingleStringTex")
        self.assertEqual(seed[1], tex.svg_default)
        self.assertEqual(seed[2], {})
        self.assertEqual(seed[3:], ("abc", r"\centering", True, "", ""))

        text_mode = SingleStringTex("abc", math_mode=False, alignment="")
        self.assertEqual(text_mode.hash_seed[3:], ("abc", "", False, "", ""))

        # A subclass reports its own name; equal inputs give equal seeds, other strings differ.
        class Shaded(SingleStringTex):
            pass

        self.assertEqual(Shaded("y").hash_seed[0], "Shaded")
        self.assertEqual(Shaded("y").hash_seed[3], "y")
        self.assertEqual(SingleStringTex("abc").hash_seed, seed)
        self.assertNotEqual(SingleStringTex("abd").hash_seed, seed)

    # manimlib.mobject.svg.string_mobject

    def test_string_mobject_select_unisolated_substring(self):
        from manimlib import Tex, Text, VGroup

        # Reference string_mobject.py:526: per match, self[start:end] with start/end counted
        # by substr_to_path_count over the preceding text (num_tex_symbols for Tex,
        # non-whitespace characters for text).
        tex = Tex(r"a + b = c + a")
        self.assertEqual(len(tex), 7)
        result = tex.select_unisolated_substring("a")
        self.assertIsInstance(result, VGroup)
        self.assertEqual(
            [[tex.submobjects.index(glyph) for glyph in part] for part in result], [[0], [6]]
        )
        self.assertIs(result[1][0], tex.submobjects[6])
        regex = tex.select_unisolated_substring(re.compile(r"[a-c]"))
        self.assertEqual(
            [[tex.submobjects.index(glyph) for glyph in part] for part in regex],
            [[0], [2], [4], [6]],
        )

        # "\sin" is three glyphs: matches at 0 and after "\sin x + " (5 symbols).
        trig = Tex(r"\sin x + \sin y")
        self.assertEqual(len(trig), 9)
        sines = trig.select_unisolated_substring(r"\sin")
        self.assertEqual(
            [[trig.submobjects.index(glyph) for glyph in part] for part in sines],
            [[0, 1, 2], [5, 6, 7]],
        )

        text = Text("hello world")
        self.assertEqual(len(text), 10)
        o_parts = text.select_unisolated_substring("o")
        self.assertEqual(
            [[text.submobjects.index(glyph) for glyph in part] for part in o_parts], [[4], [6]]
        )
        np.testing.assert_allclose(o_parts[1].get_center(), text[6].get_center(), atol=1e-12)
        across = text.select_unisolated_substring("lo w")
        self.assertEqual(
            [[text.submobjects.index(glyph) for glyph in part] for part in across], [[3, 4, 5]]
        )
        self.assertEqual(len(text.select_unisolated_substring("zz")), 0)

    def test_string_mobject_span_contains(self):
        from manimlib.mobject.svg.string_mobject import StringMobject

        # Reference string_mobject.py:229: span_0[0] <= span_1[0] and span_0[1] >= span_1[1].
        self.assertIs(StringMobject.span_contains((0, 5), (1, 3)), True)
        self.assertIs(StringMobject.span_contains((0, 5), (0, 5)), True)
        self.assertIs(StringMobject.span_contains((0, 3), (3, 3)), True)
        self.assertIs(StringMobject.span_contains((1, 3), (0, 5)), False)
        self.assertIs(StringMobject.span_contains((2, 5), (1, 4)), False)
        self.assertIs(StringMobject.span_contains((0, 4), (1, 5)), False)

    # manimlib.mobject.svg.svg_mobject (no svgelements in the portal: the static helpers read
    # the svgelements attribute layout off any object, as the Reference's code does).

    def test_svg_mobject_apply_style_to_mobject(self):
        from manimlib import Square
        from manimlib.mobject.svg.svg_mobject import SVGMobject

        # Reference svg_mobject.py:279: set_style from shape.stroke_width, shape.stroke.hexrgb,
        # shape.stroke.opacity, shape.fill.hexrgb, shape.fill.opacity; returns mob.
        mob = Square()
        shape = types.SimpleNamespace(
            stroke_width=3.0,
            stroke=types.SimpleNamespace(hexrgb="#ff0000", opacity=0.5),
            fill=types.SimpleNamespace(hexrgb="#00ff00", opacity=0.25),
        )
        self.assertNotEqual(mob.get_stroke_width(), 3.0)
        self.assertIs(SVGMobject.apply_style_to_mobject(mob, shape), mob)
        self.assertEqual(mob.get_stroke_width(), 3.0)
        self.assertEqual(mob.get_stroke_color(), "#FF0000")
        self.assertAlmostEqual(mob.get_stroke_opacity(), 0.5, places=6)
        self.assertEqual(mob.get_fill_color(), "#00FF00")
        self.assertAlmostEqual(mob.get_fill_opacity(), 0.25, places=6)

    def test_svg_paths_and_glyphs_carry_vmobject_construction_fields(self):
        from manimlib import RED, Tex, Text
        from manimlib.mobject.svg.svg_mobject import SVGMobject

        # Reference svg_mobject.py:268/283 builds each path as VMobjectFromSVGPath(path,
        # **path_string_config), whose VMobject.__init__ (vectorized_mobject.py:99-118) stores
        # the constructor defaults; the SVG's style then arrives through set_style and leaves
        # these fields alone, even with fill_color= on the SVGMobject (fm-fxov). Values read
        # from the pinned Reference.
        expected = dict(
            fill_color="#888888", fill_opacity=0.0, stroke_color="#DDDDDD", stroke_opacity=1.0,
            stroke_width=4.0, stroke_behind=False, background_image_file=None, long_lines=False,
            joint_type="auto", flat_stroke=False, scale_stroke_with_zoom=False,
            use_simple_quadratic_approx=False, anti_alias_width=1.5, fill_border_width=0.0,
            needs_new_joint_angles=True, subpath_end_indices=None, shader_program_type=None,
        )
        svg = ('<svg xmlns="http://www.w3.org/2000/svg"><path d="M0 0 L4 0 L4 2 Z" '
               'fill="#ff0000"/></svg>')
        # Text's path_string_config is use_simple_quadratic_approx=True (text_mobject.py:407).
        text_glyph = dict(expected, use_simple_quadratic_approx=True)
        for mob, fields in ((SVGMobject(svg_string=svg)[0], expected),
                            (SVGMobject(svg_string=svg, fill_color=RED, stroke_width=2)[0], expected),
                            (Tex("x")[0], expected),
                            (Text("a")[0], text_glyph),
                            (Text("a", color=RED)[0], text_glyph)):
            for name, value in fields.items():
                self.assertEqual(getattr(mob, name), value, name)
            self.assertEqual(mob.outer_vert_indices.shape, (0,))
        # The style itself still comes from the source.
        self.assertEqual(SVGMobject(svg_string=svg)[0].get_fill_color(), "#FF0000")

    def test_ensure_positive_orientation(self):
        from manimlib import Polygon, VGroup

        # vectorized_mobject.py:957: every family member whose unit normal points into the screen
        # is reversed (reverse_points), others untouched; returns self (fm-fxov).
        clockwise = Polygon([0, 0, 0], [0, 1, 0], [1, 1, 0], [1, 0, 0])
        counter = Polygon([0, 0, 0], [1, 0, 0], [1, 1, 0], [0, 1, 0])
        before = clockwise.get_points().copy()
        self.assertLess(clockwise.get_unit_normal()[2], 0)
        group = VGroup(clockwise, counter)
        kept = counter.get_points().copy()
        self.assertIs(group.ensure_positive_orientation(), group)
        np.testing.assert_allclose(clockwise.get_points(), before[::-1])
        np.testing.assert_allclose(counter.get_points(), kept)
        self.assertGreater(clockwise.get_unit_normal()[2], 0)
        lone = Polygon([0, 0, 0], [0, 1, 0], [1, 1, 0], [1, 0, 0])
        VGroup(lone).ensure_positive_orientation(recurse=False)
        self.assertLess(lone.get_unit_normal()[2], 0)

    def test_svg_paths_and_glyphs_run_counter_clockwise(self):
        from manimlib import Tex, Text
        from manimlib.mobject.svg.svg_mobject import SVGMobject

        # svg_mobject.py:100: SVGMobject applies ensure_positive_orientation after its flip,
        # and Tex and Text are built through it, so every path and glyph normal is +z on the
        # pinned Reference (Text("ox"): [+1, +1]; the two paths below: [+1, +1]) (fm-hc78).
        svg = ('<svg xmlns="http://www.w3.org/2000/svg"><path d="M0 0 L4 0 L4 2 Z"/>'
               '<path d="M0 0 L0 2 L4 2 Z"/></svg>')
        for mob in [*SVGMobject(svg_string=svg), *Text("ox"), *Tex(r"\int x\,dx = \frac{x^2}{2}")]:
            if mob.get_num_points() >= 3:
                self.assertGreater(mob.get_unit_normal()[2], 0, type(mob).__name__)

    # Measured on the pinned Reference (6199a00d): SVGMobject(svg_string=...) of one element in
    # a viewBox="-1 -1 8 6" document; each child's class, point count, and its first three points
    # as fractions of its own box (-1 left/bottom .. +1 right/top). Produced by
    # `PYTHONPATH=scripts/manim_ref xvfb-run -a python svg_shapes_probe.py` (fm-5rlu).
    _SVG_SHAPES = {
        '<path d="M0 0 L4 0 L4 2 Z"/>':
            ("VMobjectFromSVGPath", 7, [(-1.0, 1.0), (0.0, 0.0), (1.0, -1.0)]),
        '<rect x="1" y="1" width="4" height="2"/>':
            ("Rectangle", 9, [(1.0, -1.0), (1.0, 0.0), (1.0, 1.0)]),
        '<rect x="1" y="1" width="4" height="2" rx="0.5" ry="0.5"/>':
            ("RoundedRectangle", 25, [(1.0, -0.5), (1.0, 0.0), (1.0, 0.5)]),
        '<circle cx="2" cy="2" r="1"/>':
            ("Circle", 33, [(1.0, 0.0), (1.0, 0.2), (0.92, 0.38)]),
        '<ellipse cx="2" cy="2" rx="2" ry="1"/>':
            ("Circle", 33, [(1.0, 0.0), (1.0, 0.2), (0.92, 0.38)]),
        '<polygon points="0,0 4,0 2,3"/>':
            ("Polygon", 7, [(-1.0, 1.0), (-0.5, 0.0), (0.0, -1.0)]),
        '<polyline points="0,0 4,0 2,3"/>':
            ("Polyline", 5, [(0.0, -1.0), (0.5, 0.0), (1.0, 1.0)]),
        '<line x1="0" y1="0" x2="4" y2="2" stroke="#ffffff"/>':
            ("Line", 3, [(-1.0, 1.0), (0.0, 0.0), (1.0, -1.0)]),
        '<rect x="1" y="1" width="4" height="2" transform="rotate(90)"/>':
            ("Rectangle", 9, [(-1.0, -1.0), (0.0, -1.0), (1.0, -1.0)]),
    }

    def test_svg_shapes_keep_the_references_classes_and_start_vertices(self):
        import numpy as np
        from manimlib.mobject.svg.svg_mobject import SVGMobject

        # svg_mobject.py mobjects_from_svg: each element is built as its Reference class, so
        # Write and Transform start each shape where the Reference does (fm-5rlu).
        for element, (cls, count, first) in self._SVG_SHAPES.items():
            with self.subTest(element=element):
                mob = SVGMobject(svg_string='<svg xmlns="http://www.w3.org/2000/svg" viewBox="-1 -1 8 6">'
                                 + element + "</svg>")
                child, = mob.family_members_with_points()
                points = child.get_points()
                lo, hi = points.min(0), points.max(0)
                span = np.where(hi - lo > 1e-9, hi - lo, 1.0)
                where = [tuple(round(float(v), 2) for v in ((p - lo) / span * 2 - 1)[:2])
                         for p in points[:3]]
                self.assertEqual((type(child).__name__, len(points)), (cls, count))
                self.assertEqual(where, first)
                self.assertGreater(child.get_unit_normal()[2], 0)

    def test_glyph_contours_trace_like_the_reference(self):
        import numpy as np
        from manimlib import Tex

        # The pinned Reference's Tex("0") (cmr10 through dvisvgm): the outer contour first,
        # counter-clockwise from the east point of its box, then the counter clockwise from
        # the south point. So Write draws the outline first, the hole after (fm-hc78).
        def trace(sub):
            anchors = sub[0::2]
            area = 0.5 * float(np.sum(anchors[:-1, 0] * anchors[1:, 1]
                                      - anchors[1:, 0] * anchors[:-1, 1]))
            lo, hi = sub.min(0), sub.max(0)
            start = (sub[0] - (lo + hi) / 2)[:2] / ((hi - lo)[:2] / 2)
            return area, np.round(start).tolist()

        outer, counter = (trace(sub) for sub in Tex("0")[0].get_subpaths())
        self.assertGreater(outer[0], -counter[0])
        self.assertEqual((outer[0] > 0, outer[1]), (True, [1.0, 0.0]))
        self.assertEqual((counter[0] < 0, counter[1]), (True, [0.0, -1.0]))

    def test_svg_mobject_handle_transform(self):
        from manimlib import VMobject
        from manimlib.mobject.svg.svg_mobject import SVGMobject

        # Reference svg_mobject.py:268: apply [[a, c], [b, d]] about the origin, then shift (e, f).
        mob = VMobject().set_points_as_corners([[1, 0, 0], [2, 0, 0], [2, 3, 0]])
        before = mob.get_points().astype(float).copy()
        matrix = types.SimpleNamespace(a=2.0, b=1.0, c=0.5, d=3.0, e=-1.0, f=4.0)
        self.assertIs(SVGMobject.handle_transform(mob, matrix), mob)
        x, y = before[:, 0], before[:, 1]
        expected = np.stack([2 * x + 0.5 * y - 1, x + 3 * y + 4, before[:, 2]], axis=1)
        np.testing.assert_allclose(mob.get_points(), expected, atol=1e-6)
        # First anchor (1, 0) -> (1, 5); last anchor (2, 3) -> (4.5, 15).
        np.testing.assert_allclose(mob.get_points()[0], [1.0, 5.0, 0.0], atol=1e-6)
        np.testing.assert_allclose(mob.get_points()[-1], [4.5, 15.0, 0.0], atol=1e-6)

    # manimlib.mobject.svg.tex_mobject

    def test_tex_substr_to_path_count(self):
        from manimlib import Tex

        # Reference tex_mobject.py:220: num_tex_symbols(substr). Expected counts are the
        # Reference's num_tex_symbols over its TEX_TO_SYMBOL_COUNT table.
        tex = Tex(r"\sin x + y^2")
        self.assertEqual(len(tex), 7)
        self.assertEqual(tex.substr_to_path_count(r"\sin x + y^2"), 7)
        self.assertEqual(tex.substr_to_path_count(r"\sin x"), 4)
        self.assertEqual(tex.substr_to_path_count("y^2"), 2)
        self.assertEqual(tex.substr_to_path_count("+"), 1)
        # Substrings absent from the formula still count their symbols.
        cases = {
            r"\alpha\beta": 2,
            r"\frac{1}{2}": 3,
            r"\sqrt{x}": 3,
            r"\sum_{i=1}^n i": 6,
            r"\cdots": 3,
            r"e^{i\pi}": 3,
            r"\left( x \right)": 3,
        }
        for substr, count in cases.items():
            self.assertEqual(tex.substr_to_path_count(substr), count, substr)

    def test_tex_get_color_command(self):
        # Reference tex_mobject.py:163, a staticmethod: the hex string's RGB bytes as an
        # xcolor RGB-model command.
        from manimlib import Tex

        self.assertEqual(Tex.get_color_command("#FF8000"), "\\color[RGB]{255, 128, 0}")
        self.assertEqual(Tex.get_color_command("#58C4DD"), "\\color[RGB]{88, 196, 221}")
        self.assertEqual(Tex("x").get_color_command("#000000"), "\\color[RGB]{0, 0, 0}")

    # manimlib.mobject.svg.text_mobject

    def test_markup_text_escape_and_unescape_markup_char(self):
        from manimlib.mobject.svg.text_mobject import MarkupText

        # Reference text_mobject.py:218 and :222 over MARKUP_ENTITY_DICT.
        entities = {"<": "&lt;", ">": "&gt;", "&": "&amp;", '"': "&quot;", "'": "&apos;"}
        for char, entity in entities.items():
            self.assertEqual(MarkupText.escape_markup_char(char), entity)
            self.assertEqual(MarkupText.unescape_markup_char(entity), char)
        # Anything else passes through unchanged.
        self.assertEqual(MarkupText.escape_markup_char("a"), "a")
        self.assertEqual(MarkupText.escape_markup_char("&lt;"), "&lt;")
        self.assertEqual(MarkupText.unescape_markup_char("x"), "x")
        self.assertEqual(MarkupText.unescape_markup_char("&#60;"), "&#60;")

    # manimlib.utils.svg_export

    def test_svg_export_get_path_data(self):
        from manimlib import Circle, RIGHT, UP, Square, VMobject
        from manimlib.utils.svg_export import _get_path_data

        # Reference svg_export.py:73: per subpath "M x,-y", then "L" when the handle sits on the
        # start anchor or the midpoint, else "Q", and "Z" when the subpath closes.
        self.assertEqual(
            _get_path_data(Square(side_length=2)), "M 1,-1 L -1,-1 L -1,1 L 1,1 L 1,-1 Z"
        )
        curve = VMobject()
        curve.set_points(np.array([[0, 0, 0], [1, 2, 0], [2, 0, 0]], dtype=float))
        self.assertEqual(_get_path_data(curve), "M 0,-0 Q 1,-2 2,-0")
        near_start = VMobject()
        near_start.set_points(np.array([[0, 0, 0], [5e-5, 0, 0], [2, 0, 0]], dtype=float))
        self.assertEqual(_get_path_data(near_start), "M 0,-0 L 2,-0")
        two = VMobject()
        two.start_new_path(np.array([0.0, 0.0, 0.0]))
        two.add_line_to(np.array([1.0, 0.0, 0.0]))
        two.start_new_path(np.array([3.0, 3.0, 0.0]))
        two.add_line_to(np.array([4.0, 3.0, 0.0]))
        self.assertEqual(_get_path_data(two), "M 0,-0 L 1,-0 M 3,-3 L 4,-3")
        self.assertEqual(_get_path_data(VMobject()), "")

        # A circle: every quadratic segment, against the mobject's own points with y negated.
        circle = Circle(radius=1.5).shift(2 * RIGHT + UP)
        data = _get_path_data(circle)
        self.assertTrue(data.startswith("M "))
        self.assertTrue(data.endswith(" Z"))
        points = circle.get_points().astype(float)
        self.assertEqual(data.count("Q"), (len(points) - 1) // 2)
        flipped = (points[:, :2] * np.array([1.0, -1.0])).flatten()
        np.testing.assert_allclose(_floats(data), flipped, rtol=1e-5, atol=1e-5)

    def test_svg_export_vmobject_to_svg(self):
        from manimlib import BLUE, Circle, GREEN, LEFT, Line, RED, RIGHT, Square, VGroup, YELLOW
        from manimlib.constants import FRAME_HEIGHT, FRAME_WIDTH
        from manimlib.utils.svg_export import vmobject_to_svg

        # Reference svg_export.py:8: a frame-unit viewBox, one <path> per point-carrying family
        # member, fill/stroke attributes from its style, stroke width over pixel_width/FRAME_WIDTH.
        square = Square(side_length=2).set_fill(RED, 0.5).set_stroke(BLUE, 4, 0.5)
        circle = Circle().shift(3 * RIGHT).set_fill(GREEN, 1).set_stroke(width=0)
        line = Line(LEFT, RIGHT).shift(2 * LEFT).set_stroke(YELLOW, 2).set_fill(opacity=0)
        group = VGroup(square, VGroup(circle, line))
        temporary = tempfile.TemporaryDirectory(prefix="fm6u1g_svg_export_")
        self.addCleanup(temporary.cleanup)
        path = os.path.join(temporary.name, "group.svg")
        svg = vmobject_to_svg(group, filename=path)
        self.assertIsInstance(svg, str)
        with open(path) as handle:
            self.assertEqual(handle.read(), svg)

        root = ET.fromstring(svg)
        self.assertEqual(root.tag, SVG_NS + "svg")
        self.assertEqual(
            root.get("viewBox"), f"{-FRAME_WIDTH / 2} {-FRAME_HEIGHT / 2} {FRAME_WIDTH} {FRAME_HEIGHT}"
        )
        self.assertEqual((root.get("width"), root.get("height")), ("1920", "1080"))
        paths = root.findall(SVG_NS + "path")
        self.assertEqual(len(paths), 3)
        sq, ci, li = (p.attrib for p in paths)

        self.assertEqual(sq["d"], "M 1,-1 L -1,-1 L -1,1 L 1,1 L 1,-1 Z")
        self.assertEqual(sq["fill"], RED.upper())
        self.assertEqual(sq["fill-opacity"], "0.5")
        self.assertEqual(sq["stroke"], BLUE.upper())
        self.assertEqual(sq["stroke-width"], f"{4 / (1920 / FRAME_WIDTH):.6g}")
        self.assertEqual(sq["stroke-opacity"], "0.5")

        self.assertEqual(ci["fill"], GREEN.upper())
        self.assertNotIn("fill-opacity", ci)
        self.assertEqual(ci["stroke"], "none")
        self.assertNotIn("stroke-width", ci)
        np.testing.assert_allclose(
            _floats(ci["d"]),
            (circle.get_points()[:, :2].astype(float) * [1.0, -1.0]).flatten(),
            rtol=1e-5,
            atol=1e-5,
        )

        self.assertEqual(li["d"], "M -3,-0 L -1,-0")
        self.assertEqual(li["fill"], "none")
        self.assertEqual(li["stroke"], YELLOW.upper())
        self.assertEqual(li["stroke-width"], f"{2 / (1920 / FRAME_WIDTH):.6g}")
        self.assertNotIn("stroke-opacity", li)

        # The pixel size sets the root size and rescales stroke widths.
        half = ET.fromstring(vmobject_to_svg(square, pixel_width=960, pixel_height=540))
        self.assertEqual((half.get("width"), half.get("height")), ("960", "540"))
        (only,) = half.findall(SVG_NS + "path")
        self.assertEqual(only.get("stroke-width"), f"{4 / (960 / FRAME_WIDTH):.6g}")


if __name__ == "__main__":
    unittest.main()
