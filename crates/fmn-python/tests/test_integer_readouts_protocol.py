"""Exact integer orchestration with explicit native glyph/storage doubles.

The installed production decimal owner runs unchanged. NumPy is real; these
VMobject/Text doubles record which values and glyph strings cross native
boundaries. They are not native typography, renderer or scene-clock evidence.
"""
from contextlib import contextmanager
import copy
import importlib.util
import inspect
import os
from pathlib import Path
import sys
from types import ModuleType, SimpleNamespace
import unittest
from unittest.mock import patch

import numpy as np

SOURCE = Path(os.environ.get("FMN_DECIMAL_SOURCE", str(
    Path(__file__).resolve().parents[1] / "python/fmn_python/decimal_authoring.py")))


@contextmanager
def host(*, animations=False):
    package = ModuleType("fmn_python")
    package.__path__ = [str(SOURCE.parent)]
    copying = ModuleType("fmn_python.copying")
    copying.FamilyRefs = tuple
    movement = ModuleType("fmn_python.movement")
    movement._protocols = lambda g, cls, names: {name: getattr(cls, name, None) for name in names}
    movement._changed = lambda obj, methods: any(
        name in vars(obj) or inspect.getattr_static(type(obj), name, None) is not expected
        for name, expected in methods.items())
    with patch.dict(sys.modules, {"fmn_python": package, copying.__name__: copying,
                                 movement.__name__: movement}):
        spec = importlib.util.spec_from_file_location("fmn_python.decimal_authoring", SOURCE)
        owner = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(owner)
        events = []

        class VMobject:
            def __init__(self, *children):
                self.submobjects = list(children)
                self.data = np.zeros(0, dtype=[("point", "f8", 3)])
                self.uniforms, self.style = {}, {"color": "white"}
                self.center_value = np.zeros(3)

            def _build_decimal_number(self, factory, value, *params):
                events.append(("native_number", value))
                value = float(value)
                precision, width, signed, grouped = params[:4]
                displayed = int(value) if precision == 0 else value
                fmt = ("+" if signed else "") + ("0" + str(width) if width else "")
                fmt += ("," if grouped else "") + ("d" if precision == 0 else f".{precision}f")
                text = format(displayed, fmt).replace("-", "–")
                return [Glyph(char) for char in text]

            def has_points(self): return bool(len(self.data))
            def get_points(self): return self.data["point"]
            def set_points(self, points):
                self.data = np.zeros(len(points), dtype=self.data.dtype)
                self.data["point"] = points
            def clear_points(self): self.set_points([])
            def set_data(self, data): self.data = data.copy()
            def set_uniforms(self, values): self.uniforms = dict(values)
            def get_style(self): return dict(self.style)
            def set_style(self, **values):
                self.style.update(values)
                for child in self.submobjects: child.set_style(**values)
                return self
            def set_submobjects(self, values): self.submobjects = list(values)
            def family_members_with_points(self):
                return ([self] if self.has_points() else []) + [m for c in self.submobjects
                                                               for m in c.family_members_with_points()]
            def copy(self): return copy.deepcopy(self)
            def _is_bound(self): return False
            def scale(self, value): self.data["point"] *= value; return self
            def shift(self, value): self.center_value += value; return self
            def align_to(self, other, direction): return self
            def get_height(self): return 1.0
            def get_bottom(self): return self.center_value.copy()
            def get_center(self): return self.center_value.copy()
            def get_edge_center(self, edge): return self.center_value.copy()
            def move_to(self, point, edge=None): self.center_value = point.copy(); return self
            def next_to(self, other, direction, buff=0): return self
            def center(self): return self

        class Glyph(VMobject):
            def __init__(self, text):
                super().__init__()
                self.text = text
                self.data = np.zeros(1, dtype=self.data.dtype)

        class Text(Glyph):
            def __init__(self, text, **options):
                events.append(("native_text", text))
                if text == "!": raise ValueError("fixture glyph refusal")
                super().__init__(text)
                self.font_size = 48.0

        class VGroup(VMobject):
            def arrange(self, direction, **kwargs): return self

        class Decimal(VMobject):
            def __init__(self, number=0, color=None, stroke_width=0, fill_opacity=1.0,
                         fill_border_width=0.5, num_decimal_places=2, min_total_width=0,
                         include_sign=False, group_with_commas=True, digit_buff_per_font_unit=.001,
                         show_ellipsis=False, unit=None, include_background_rectangle=False,
                         hide_zero_components_on_complex=True, edge_to_fix=(-1, 0, 0),
                         font_size=48, text_config=None, **kwargs):
                raise AssertionError("the production initializer must replace this signature fixture")

            def _formatter_config(self):
                p = self._decimal_params
                return dict(num_decimal_places=p[0], min_total_width=p[1], include_sign=p[2], group_with_commas=p[3])
            def get_formatter(self, **kwargs):
                c = self._formatter_config(); c.update(kwargs)
                return "{" + c.get("field_name", "") + ":" + ("+" if c["include_sign"] else "") + (
                    "0" + str(c["min_total_width"]) if c["min_total_width"] else "") + (
                    "," if c["group_with_commas"] else "") + (
                    f'.{c["num_decimal_places"]}f' if c["num_decimal_places"] else "d") + "}"
            def get_complex_formatter(self):
                return self.get_formatter(field_name="0.real") + self.get_formatter(field_name="0.imag", include_sign=True) + "i"
            def get_font_size(self): return self.font_size
            def get_value(self): return self.number
            def get_tex(self): return self.num_string
            def set_value(self, number):
                point = self.get_edge_center(self.edge_to_fix)
                self.set_submobjects_from_number(number)
                self.move_to(point, self.edge_to_fix)
                return self
            def increment_value(self, delta_t=1):
                self.set_value(self.get_value() + delta_t)
                return self

        class Integer(Decimal):
            def __init__(self, number=0, num_decimal_places=0, **kwargs):
                super().__init__(number, num_decimal_places=num_decimal_places, **kwargs)
            def get_value(self): return int(np.round(super().get_value()))

        g = SimpleNamespace(DecimalNumber=Decimal, Integer=Integer, VMobject=VMobject, VGroup=VGroup,
                            Text=Text, _np=np, _install_live_state=VMobject.__init__,
                            _native_shell_factory=None, _hang_native_children=lambda row, children: row.set_submobjects(children),
                            _vec3=lambda value: tuple(value), RIGHT=np.array([1., 0., 0.]),
                            UP=np.array([0., 1., 0.]), DOWN=np.array([0., -1., 0.]),
                            BLACK="black", WHITE="white",
                            _init_native_vmobject=lambda obj, options: VMobject.__init__(obj),
                            _apply_vmobject_style_kwargs=lambda obj, options: obj.set_style(**options))
        if animations:
            class ChangeDecimalToValue:
                def __init__(self, decimal_mob, target_number, **kwargs):
                    start = decimal_mob.number
                    self.mobject, self.options = decimal_mob, kwargs
                    self.number_update_func = lambda alpha: (1 - alpha) * start + alpha * target_number
            g.ChangeDecimalToValue = ChangeDecimalToValue
        owner.install_decimal_authoring(g)
        yield SimpleNamespace(native=g, owner=owner, events=events)


def glyphs(mob):
    return "".join(child.text for child in mob.submobjects)


class IntegerReadoutTests(unittest.TestCase):
    def test_constructor_preserves_exact_wide_python_integers(self):
        with host() as h:
            for value in (2**53 + 1, -(2**53 + 1), 10**30 + 123456789, -(10**30 + 123456789), 10**400):
                with self.subTest(value_bits=value.bit_length()):
                    m = h.native.Integer(value)
                    expected = format(value, ",d").replace("-", "–")
                    self.assertIs(type(m.number), int)
                    self.assertEqual(m.get_value(), value)
                    self.assertEqual(m.get_tex(), expected)
                    self.assertEqual(glyphs(m), expected)
            self.assertFalse(any(kind == "native_number" for kind, _ in h.events))

    def test_numpy_integers_are_widened_before_arithmetic(self):
        with host() as h:
            for value in (np.int64(2**63 - 1), np.uint64(2**64 - 1), np.int64(-(2**63))):
                m = h.native.Integer(value)
                self.assertIs(type(m.number), int)
                self.assertEqual(m.get_value(), int(value))
                m.increment_value()
                self.assertEqual(m.get_value(), int(value) + 1)
                self.assertEqual(glyphs(m), format(int(value) + 1, ",d").replace("-", "–"))

    def test_updates_increment_and_decrement_do_not_round(self):
        with host() as h:
            m = h.native.Integer(0)
            for value in (10**30, 2**53 + 1, -10**400):
                self.assertIs(m.set_value(value), m)
                self.assertIs(m.increment_value(), m)
                self.assertEqual(m.get_value(), value + 1)
                self.assertEqual(m.get_tex(), format(value + 1, ",d").replace("-", "–"))
                m.increment_value(-2)
                self.assertEqual(m.get_value(), value - 1)
                self.assertEqual(glyphs(m), format(value - 1, ",d").replace("-", "–"))

    def test_small_integer_native_path_and_fractional_getter_are_unchanged(self):
        with host() as h:
            for value in (0, 1, -3, True, 2**53, -(2**53)):
                m = h.native.Integer(value)
                self.assertEqual(m.get_value(), int(value))
            self.assertEqual(sum(kind == "native_number" for kind, _ in h.events), 6)
            for value in (1.25, 1.75, -1.75, 2.5):
                m = h.native.Integer(value)
                self.assertIs(type(m.number), float)
                self.assertEqual(m.get_value(), int(np.round(value)))
                self.assertEqual(m.get_tex(), str(int(value)).replace("-", "–"))

    def test_decimal_number_keeps_an_integral_value_like_the_reference(self):
        # Measured on the pinned Reference: DecimalNumber(10**18 + 99) keeps the int, spells
        # it exactly with zero decimals and through float with decimals (fm-yuin).
        with host() as h:
            value = 10**16 + 1
            for cls in (h.native.DecimalNumber, type("Child", (h.native.DecimalNumber,), {})):
                m = cls(value, num_decimal_places=0)
                self.assertIs(type(m.number), int)
                self.assertEqual(m.get_value(), value)
                self.assertEqual(m.get_tex(), "10,000,000,000,000,001")
                self.assertEqual(cls(value).get_tex(), "10,000,000,000,000,000.00")
                self.assertIs(type(cls(1.5).number), float)

    def test_format_width_grouping_sign_and_negative_values(self):
        with host() as h:
            for value in (10**30 + 1, -10**30 - 1):
                for grouped in (False, True):
                    m = h.native.Integer(value, group_with_commas=grouped, include_sign=True, min_total_width=45)
                    expected = format(value, "+045" + ("," if grouped else "") + "d").replace("-", "–")
                    self.assertEqual(m.get_tex(), expected)
                    self.assertEqual(glyphs(m), expected)

    def test_authored_formatter_receives_exact_value(self):
        with host() as h:
            calls = []
            class Counter(h.native.Integer):
                def get_num_string(self, value):
                    calls.append(value)
                    return "v" + str(value)
            value = 10**100 + 13
            m = Counter(value)
            m.set_value(value + 1)
            self.assertEqual(calls, [value, value + 1])
            self.assertTrue(all(type(v) is int for v in calls))
            self.assertEqual(glyphs(m), "v" + str(value + 1))

    def test_subclass_value_override_remains_authoritative(self):
        with host() as h:
            class Counter(h.native.Integer):
                def get_value(self): return super().get_value() + 17
            self.assertEqual(Counter(10**100 + 1).get_value(), 10**100 + 18)

    def test_character_budget_rejects_before_native_glyph_allocation(self):
        with host() as h:
            m = h.native.Integer(7)
            before = (m.number, m.num_string, tuple(map(id, m.submobjects)))
            h.events.clear()
            for value in (10**4096, -(10**4095), 1 << 1_000_000):
                with self.assertRaisesRegex(ValueError, "characters"):
                    m.set_value(value)
                self.assertEqual((m.number, m.num_string, tuple(map(id, m.submobjects))), before)
                self.assertEqual(h.events, [])

    def test_exact_character_boundary_without_grouping(self):
        with host() as h:
            m = h.native.Integer(10**4095, group_with_commas=False)
            self.assertEqual(len(m.num_string), 4096)
            self.assertEqual(m.get_value(), 10**4095)
            self.assertEqual(glyphs(m), m.num_string)

    def test_glyph_failure_keeps_prior_exact_value_and_family(self):
        with host() as h:
            class Counter(h.native.Integer):
                def get_num_string(self, value):
                    return "!" if value == 10**100 else super().get_num_string(value)
            m = Counter(10**30 + 1)
            before = (m.number, m.num_string, tuple(map(id, m.submobjects)))
            with self.assertRaisesRegex(ValueError, "glyph refusal"): m.set_value(10**100)
            self.assertEqual((m.number, m.num_string, tuple(map(id, m.submobjects))), before)

    def test_rebuilding_between_native_and_large_text_paths_preserves_style_and_anchor(self):
        with host() as h:
            m = h.native.Integer(7)
            m.center_value[:] = [3, 2, 0]
            m.submobjects[0].set_style(color="blue")
            for value in (10**30 + 1, 5, -10**400, 8):
                m.set_value(value)
                self.assertEqual(m.get_value(), value)
                np.testing.assert_array_equal(m.center_value, [3, 2, 0])
                self.assertTrue(all(c.style["color"] == "blue" for c in m.submobjects))

    def test_installer_is_idempotent(self):
        with host() as h:
            original = h.native.Integer.get_value
            h.owner.install_decimal_authoring(h.native)
            self.assertIs(h.native.Integer.get_value, original)
            self.assertEqual(h.native.Integer(10**30 + 1).get_value(), 10**30 + 1)


if __name__ == "__main__":
    unittest.main()
