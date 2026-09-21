"""Real and complex readouts over the native number and text shelves.

Python selects components, dispatches authored formatters and composes native
mobjects. It does not typeset glyphs, generate outlines, or own an animation
clock. Replacements are built and styled before geometry and value publication.
"""
from __future__ import annotations

from collections.abc import Mapping
from functools import wraps
import inspect
import math

_MAX_CHARACTERS = 4096


def install_decimal_authoring(native):
    from .movement import _changed, _protocols

    g = vars(native)
    if g.get("_FMN_DECIMAL_AUTHORING_INSTALLED", False):
        return
    Decimal, VMobject = g["DecimalNumber"], g["VMobject"]
    np = g["_np"]
    previous_init = Decimal.__init__
    signature = inspect.signature(previous_init)
    stock_complex_formatter = Decimal.get_complex_formatter

    def text_options(self):
        options = getattr(self, "text_config", {})
        if not isinstance(options, Mapping):
            raise TypeError("DecimalNumber text_config must be a mapping")
        if len(options) > _MAX_CHARACTERS:
            raise ValueError("DecimalNumber text_config exceeds 4096 entries")
        if "font_size" in options:
            raise ValueError("pass font_size to DecimalNumber, not its text_config")
        return dict(options)

    def normalize(value):
        if isinstance(value, (complex, np.complexfloating)):
            result = complex(value)
            finite = math.isfinite(result.real) and math.isfinite(result.imag)
        else:
            try:
                result = float(value)
            except (TypeError, ValueError, OverflowError) as error:
                raise TypeError("DecimalNumber requires a real or complex number") from error
            finite = math.isfinite(result)
        if not finite:
            raise ValueError("DecimalNumber requires a finite value in both real and imaginary components")
        return result

    def validate_format(self):
        params = self._decimal_params
        for name, value in (("precision", params[0]), ("minimum width", params[1])):
            if not 0 <= value <= _MAX_CHARACTERS:
                raise ValueError(f"decimal-number {name} must be in 0..4096")
        size = float(self.font_size)
        spacing = params[4] * size
        if not math.isfinite(size) or not 0 < size <= 3.4028234663852886e38:
            raise ValueError("decimal-number font size must be finite, positive and f32-representable")
        if not math.isfinite(spacing) or abs(spacing) > 3.4028234663852886e38:
            raise ValueError("decimal-number spacing must be finite and f32-representable")
        if not np.isfinite(self.edge_to_fix).all():
            raise ValueError("decimal-number fixed edge must have finite components")
        unit = params[6]
        if unit is not None and len(unit.removeprefix("^")) > _MAX_CHARACTERS:
            raise ValueError("decimal-number unit exceeds the 4096-character limit")

    def scalar_string(self, value, sign):
        # The public formatter is CPython's format specification, as on the
        # existing real-only path. The native shelf independently produces the
        # same digits, including integer truncation and negative-zero handling.
        displayed = int(value) if self._decimal_params[0] == 0 else value
        text = self.get_formatter(include_sign=sign).format(displayed)
        if text.startswith("-") and np.round(value, self._decimal_params[0]) == 0:
            text = ("+" if sign else "") + text[1:]
        return text.replace("-", "–")

    def components(self, number):
        signed = self._decimal_params[2]
        if not isinstance(number, complex):
            return [(number, signed, False)]
        if self.hide_zero_components_on_complex:
            if number.imag == 0:
                return [(number.real, signed, False)]
            if number.real == 0:
                return [(number.imag, signed, True)]
        return [(number.real, signed, False), (number.imag, True, True)]

    def describe(self, number):
        validate_format(self)
        parts = components(self, number)
        texts = [scalar_string(self, value, sign) for value, sign, _ in parts]
        text = "".join(value + ("i" if part[2] else "") for value, part in zip(texts, parts))
        check_string(self, text)
        return parts, texts, text

    def check_string(self, text):
        if not isinstance(text, str):
            raise TypeError("DecimalNumber.get_num_string must return a string")
        params = self._decimal_params
        unit_length = len(params[6].removeprefix("^")) if params[6] is not None else 0
        if len(text) + int(params[5]) + unit_length > _MAX_CHARACTERS:
            raise ValueError("decimal-number characters exceed the declared limit of 4096")

    def build_row(value, params):
        row = VMobject.__new__(VMobject)
        g["_install_live_state"](row)
        specs = row._build_decimal_number(g["_native_shell_factory"], value, *params)
        g["_hang_native_children"](row, specs)
        return row

    def build(self, parts, texts):
        params = list(self._decimal_params)
        params[9] = float(self.font_size)
        # Keep the native real-number fast path unchanged, including its
        # placement and the portal's root-level background rectangle contract.
        if len(parts) == 1 and not parts[0][2]:
            return build_row(parts[0][0], params)

        rows = []
        baseline = None
        buff = params[4] * params[9]
        for (value, sign, imaginary), text in zip(parts, texts):
            selected = params.copy()
            selected[2], selected[5], selected[6], selected[7] = sign, False, "i" if imaginary else None, False
            row = build_row(value, selected)
            # Numeric digits, not comma descenders or a leading minus, define
            # the baseline between two independently native-built components.
            digit = row.submobjects[int(text.startswith(("+", "–")))]
            if rows:
                row.next_to(rows[-1], g["RIGHT"], buff=buff)
                row.shift((baseline - digit.get_bottom()[1]) * g["UP"])
            else:
                baseline = digit.get_bottom()[1]
            rows.append(row)

        # Ellipsis and user unit follow the complete number, never precede i.
        # Obtain those trailing children from the same native number builder;
        # using Text(unit) here would silently introduce different kerning.
        if params[5] or params[6] is not None:
            selected = params.copy()
            selected[0:4] = [0, 0, False, False]
            selected[7] = False
            tail = build_row(0.0, selected)
            digit = tail.submobjects[0]
            tail.shift((baseline - digit.get_bottom()[1]) * g["UP"])
            suffix = g["VGroup"](*list(tail.submobjects)[1:])
            old_y = suffix.get_center()[1]
            suffix.next_to(rows[-1], g["RIGHT"], buff=buff)
            suffix.shift((old_y - suffix.get_center()[1]) * g["UP"])
            rows.append(suffix)

        root = rows[0]
        children = [child for row in rows for child in list(row.submobjects)]
        root.set_submobjects(children)
        if params[6] is not None and params[6].startswith("^"):
            children[-1].align_to(root, g["UP"])
        root.center()
        if params[7]:
            rectangle = g["BackgroundRectangle"](root, color=g["BLACK"], buff=0, fill_opacity=1)
            root.set_data(rectangle.data.copy())
        return root

    def get_num_string(self, number):
        number = normalize(number)
        validate_format(self)
        if isinstance(number, complex) and len(components(self, number)) == 2:
            formatter = self.get_complex_formatter
            if getattr(formatter, "__func__", formatter) is not stock_complex_formatter:
                text = formatter().format(number).replace("-", "–")
                check_string(self, text)
                return text
        return describe(self, number)[2]

    def char_to_mob(self, char):
        # BN-08: units are literal native text, including i and a backslash.
        # The separately exported char_to_cahced_mob utility retains its own
        # Reference-compatible Tex dispatch; the readout needs no TeX detour.
        return g["Text"](char, **text_options(self))

    def build_authored(self, text):
        """Compose native glyph mobjects without lowering away Python hooks.

        Unlike the scalar native builder, this route can execute an authored
        formatter or glyph converter. Every transform targets a detached copy:
        a callback may return the same cached template for multiple characters,
        or a mobject that already belongs to another Scene.
        """
        params = self._decimal_params
        size = float(self.get_font_size())
        if not math.isfinite(size) or not 0 < size <= 3.4028234663852886e38:
            raise ValueError("decimal-number font size must be finite, positive and f32-representable")
        buff = params[4] * size
        if not math.isfinite(buff) or abs(buff) > 3.4028234663852886e38:
            raise ValueError("decimal-number spacing must be finite and f32-representable")

        def glyph(char):
            template = self.char_to_mob(char)
            if not isinstance(template, VMobject):
                raise TypeError("DecimalNumber.char_to_mob must return a VMobject")
            template_size = float(getattr(template, "font_size", 48.0))
            if not math.isfinite(template_size) or template_size <= 0:
                raise ValueError("decimal-number glyph template font size must be finite and positive")
            child = template.copy()
            if not isinstance(child, VMobject) or child is template:
                raise TypeError("decimal-number glyph copy must be an independent VMobject")
            if child._is_bound():
                # A native CoW copy can keep its source arena even though its
                # Python _scene link is gone. Use the existing graph snapshot
                # copier to obtain a nursery, not a forged ownership flag.
                child = g["_copy_mobject_graph"](child, False, {}, detach_bound=True)
            child.scale(size / template_size)
            return child

        children = [glyph(char) for char in text]
        if params[5]:
            children.append(glyph("\u2026"))
        if params[6] is not None:
            unit = g["VGroup"](*(glyph(char) for char in params[6].removeprefix("^")))
            unit.arrange(g["RIGHT"], buff=buff, aligned_edge=g["DOWN"])
            children.append(unit)
        root = g["VGroup"](*children)
        root.arrange(g["RIGHT"], buff=buff, aligned_edge=g["DOWN"])
        for index, char in enumerate(text):
            if char == "–" and index + 1 < len(text):
                children[index].align_to(children[index + 1], g["UP"])
                children[index].shift(children[index + 1].get_height() * g["DOWN"] / 2)
            elif char == ",":
                children[index].shift(children[index].get_height() * g["DOWN"] / 2)
        if params[6] is not None and params[6].startswith("^"):
            children[-1].align_to(root, g["UP"])
        style = dict(stroke_width=params[11], fill_opacity=params[12], fill_border_width=params[13])
        if params[10] is not None:
            style["color"] = params[10]
        g["_apply_vmobject_style_kwargs"](root, style)
        if params[7]:
            rectangle = g["BackgroundRectangle"](root, color=g["BLACK"], buff=0, fill_opacity=1)
            root.set_data(rectangle.data.copy())
        return root

    def set_submobjects_from_number(self, number):
        number = normalize(number)
        validate_format(self)
        if text_options(self) or _changed(self, protocols):
            text = self.get_num_string(number)
            check_string(self, text)
            scratch = build_authored(self, text)
            parts = components(self, number)
        else:
            parts, texts, text = describe(self, number)
            scratch = build(self, parts, texts)
        # Authored glyphs may be Text/VGroup roots with ink in descendants.
        # Never select this readout's own background rectangle as the donor.
        donor = next((member for child in self.submobjects
                      for member in child.family_members_with_points()), None)
        if donor is not None:
            style = donor.get_style()
            for child in scratch.submobjects:
                child.set_style(**style)
        # All value/format/resource/typography validation and glyph styling
        # finish before the first write to the live receiver. Replace the exact
        # child list: become's family padding leaves stale glyphs after shrink.
        self.set_data(scratch.data.copy())
        self.set_submobjects(list(scratch.submobjects))
        self.number, self.num_string = number, text
        self._complex_imag_mode = len(parts) == 1 and parts[0][2]

    @wraps(previous_init)
    def decimal_init(self, *args, **kwargs):
        bound = signature.bind(self, *args, **kwargs)
        bound.apply_defaults()
        config = bound.arguments
        self.text_config = {} if config["text_config"] is None else config["text_config"]
        self.text_config = text_options(self)
        number = normalize(config["number"])
        self.hide_zero_components_on_complex = bool(config["hide_zero_components_on_complex"])
        self.font_size = float(config["font_size"])
        self.edge_to_fix = np.array(g["_vec3"](config["edge_to_fix"]))
        self._decimal_params = (
            int(config["num_decimal_places"]),
            0 if config["min_total_width"] is None else int(config["min_total_width"]),
            bool(config["include_sign"]), bool(config["group_with_commas"]),
            float(config["digit_buff_per_font_unit"]), bool(config["show_ellipsis"]),
            None if config["unit"] is None else str(config["unit"]),
            bool(config["include_background_rectangle"]), g["_vec3"](config["edge_to_fix"]),
            self.font_size, config["color"], float(config["stroke_width"]),
            float(config["fill_opacity"]), float(config["fill_border_width"]),
        )
        validate_format(self)
        g["_install_live_state"](self)
        self._engine_init()
        self.set_submobjects_from_number(number)
        g["_apply_vmobject_style_kwargs"](self, config["kwargs"])

    for name, method in (("__init__", decimal_init), ("get_num_string", get_num_string),
                         ("char_to_mob", char_to_mob),
                         ("set_submobjects_from_number", set_submobjects_from_number)):
        method.__name__ = name
        method.__qualname__ = Decimal.__qualname__ + "." + name
        method.__module__ = Decimal.__module__
        setattr(Decimal, name, method)

    # Keep the tuple used by native constructors and decorated Matrix cells as
    # the single source of truth, while exposing the Reference's authoring
    # attributes. Changes take effect on the next rebuild, not mid-frame.
    def parameter(index, convert):
        def get(self):
            return self._decimal_params[index]

        def set(self, value):
            params = list(self._decimal_params)
            params[index] = convert(value)
            self._decimal_params = tuple(params)

        return property(get, set)

    for index, name, convert in (
        (0, "num_decimal_places", int),
        (1, "min_total_width", lambda value: 0 if value is None else int(value)),
        (2, "include_sign", bool), (3, "group_with_commas", bool),
        (4, "digit_buff_per_font_unit", float), (5, "show_ellipsis", bool),
        (6, "unit", lambda value: None if value is None else str(value)),
        (7, "include_background_rectangle", bool),
    ):
        # Authored subclasses can still override these descriptors normally.
        setattr(Decimal, name, parameter(index, convert))
    protocols = _protocols(g, Decimal, (
        "get_num_string", "get_formatter", "get_complex_formatter", "_formatter_config", "char_to_mob",
        "get_font_size", "__getattribute__", "__getattr__",
    ))
    g["_FMN_DECIMAL_AUTHORING_INSTALLED"] = True
