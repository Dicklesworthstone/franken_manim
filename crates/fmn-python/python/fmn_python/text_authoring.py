"""Native rich typography without SVG round-trips or rewritten source spans."""
from __future__ import annotations

from collections.abc import Mapping
from functools import wraps
import inspect
import itertools
import math
import operator

_MAX_BYTES = 262_144
_MAX_ITEMS = 4096
_MAX_STYLE_WORK = 16_777_216


def _number(value, name, *, positive=False):
    result = float(value)
    if not math.isfinite(result) or (positive and result <= 0) or abs(result) > 3.4028234663852886e38:
        raise ValueError(name + " must be finite, f32-representable" + (" and positive" if positive else ""))
    return result


def _style_flag(value, name, positive):
    if not isinstance(value, str):
        raise TypeError(name + " must be a style name")
    normalized = value.upper()
    if normalized not in ("NORMAL", positive):
        raise ValueError(name + " supports NORMAL or " + positive)
    return normalized == positive


def _map(long, short, name):
    selected = long or short
    if not isinstance(selected, Mapping):
        raise TypeError(name + " must be a mapping")
    if len(selected) > _MAX_ITEMS:
        raise ValueError(name + " exceeds the 4096-entry budget")
    items = list(itertools.islice(selected.items(), _MAX_ITEMS + 1))
    if len(items) > _MAX_ITEMS:
        raise ValueError(name + " exceeds the 4096-entry budget")
    return dict(items)


def _stops(g, values, name):
    if isinstance(values, (str, bytes)):
        raise TypeError(name + " must be an iterable of colors, not one color string")
    stops = tuple(itertools.islice(iter(values), _MAX_ITEMS + 1))
    if len(stops) > _MAX_ITEMS:
        raise ValueError(name + " exceeds the 4096-stop budget")
    # Freeze colors too; caller-owned arrays cannot change during construction.
    result = tuple(tuple(float(v) for v in g["color_to_rgb"](stop)) for stop in stops)
    if any(len(row) != 3 or not all(math.isfinite(v) and 0 <= v <= 1 for v in row) for row in result):
        raise ValueError(name + " colors must have finite RGB components in [0, 1]")
    return result


def _resolved_selector(obj, selector, offsets, budget, multiplier):
    """Use the public selector protocol, then retain exact original bytes."""
    result = []
    for span in itertools.islice(iter(obj.find_spans_by_selector(selector)), budget[0] + 1):
        if budget[0] == 0:
            raise ValueError("Text selectors exceed the 262145-range budget")
        start, end = (operator.index(index) for index in span)
        if not 0 <= start <= end < len(offsets):
            raise ValueError("Text style selectors must identify valid source character ranges")
        start, end = offsets[start], offsets[end]
        budget[0] -= 1
        budget[1] -= max(end - start, 1) * multiplier
        if budget[1] < 0:
            raise ValueError("Text style matching exceeds the 16777216-byte-work budget")
        result.append((start, end))
    return result


def text_recipe(g, obj, source):
    """Compile the current public fields before publishing a new glyph family."""
    size = len(source.encode("utf-8"))
    if size > _MAX_BYTES:
        raise ValueError("Text source exceeds 262144 bytes")
    if obj.global_config or obj.local_configs:
        raise NotImplementedError("Text global_config/local_configs require unsupported Pango attributes; use native font/style maps")
    font = obj.font
    if not isinstance(font, str):
        raise TypeError("Text font must be a family name")
    alignment = obj.alignment.upper() if isinstance(obj.alignment, str) else None
    if alignment not in ("", "LEFT", "CENTER", "RIGHT"):
        raise ValueError("Text alignment must be LEFT, CENTER or RIGHT")
    maps = {name: _map({}, getattr(obj, name), name)
            for name in ("t2c", "t2f", "t2s", "t2w", "t2g")}
    if any(not isinstance(value, str) for value in maps["t2f"].values()):
        raise TypeError("t2f values must be font family names")
    maps["t2g"] = {key: _stops(g, value, "t2g") for key, value in maps["t2g"].items()}
    stop_count = sum(len(stops) for stops in maps["t2g"].values())
    if stop_count > _MAX_ITEMS:
        raise ValueError("Text gradients exceed 4096 total stops")
    gradient = None if obj.gradient is None else _stops(g, obj.gradient, "gradient")
    count = sum(len(maps[name]) for name in ("t2f", "t2s", "t2w", "t2g"))
    if size * max(count + stop_count, 1) > _MAX_STYLE_WORK:
        raise ValueError("Text style matching exceeds the 16777216-byte-work budget")
    options = dict(font=font,
                   font_size=_number(obj.font_size, "Text font_size", positive=True),
                   bold=_style_flag(obj.weight, "Text weight", "BOLD"),
                   italic=_style_flag(obj.slant, "Text slant", "ITALIC"),
                   alignment=alignment or "LEFT",
                   line_spacing=1. if obj.lsh is None else _number(obj.lsh, "Text line spacing", positive=True),
                   justify=bool(obj.justify), indent=_number(obj.indent, "Text indent"),
                   disable_ligatures=bool(obj.disable_ligatures))
    if obj.line_width is not None:
        options["line_width"] = _number(obj.line_width, "Text line_width", positive=True)
    if isinstance(obj, g["Code"]):
        code_options = vars(obj).get("_fmn_code_layout")
        language, theme = code_options if code_options is not None else (obj.language, obj.code_style)
        _validate_code_names(language, theme)
        options["code_language"], options["code_style"] = language, theme
        options["font"] = "CM Typewriter" if font in ("", "Consolas") else font
    offsets = [0]
    for character in source:
        offsets.append(offsets[-1] + len(character.encode("utf-8")))
    budget = [_MAX_BYTES + 1, _MAX_STYLE_WORK]
    previous_source = obj.string
    # Authored selector overrides must see the new source even during a live
    # rebuild. Restore the old published source if validation/layout fails.
    obj.string = source
    try:
        for name in ("t2f", "t2s", "t2w", "t2g"):
            entries = []
            for selector, value in maps[name].items():
                if name == "t2s":
                    value = _style_flag(value, name, "ITALIC")
                elif name == "t2w":
                    value = _style_flag(value, name, "BOLD")
                multiplier = 1 + len(value) if name == "t2g" else 1
                entries.append((_resolved_selector(obj, selector, offsets, budget, multiplier), value))
            options[name] = entries
    finally:
        obj.string = previous_source
    return options, maps, gradient


def apply_text_colors(obj):
    """Run paint hooks after constructor or regeneration style initialization."""
    if obj.gradient:
        obj.set_color_by_gradient(*obj.gradient)
    obj.set_color_by_text_to_color_map(obj.t2c)


def _validate_code_names(language, theme):
    if not isinstance(language, str) or not isinstance(theme, str):
        raise TypeError("Code language and code_style must be strings")
    if len(language.encode("utf-8")) > 1024 or len(theme.encode("utf-8")) > 1024:
        raise ValueError("Code language/style names exceed 1024 bytes")


def install_text_authoring(native):
    g = vars(native)
    if g.get("_FMN_TEXT_AUTHORING_INSTALLED", False):
        return
    Markup = g["MarkupText"]
    from .string_lifecycle import initialize_string, explicit_channel_colors
    previous = Markup.__init__
    signature = inspect.signature(previous)

    @wraps(previous)
    def initialize(self, *args, **kwargs):
        bound = signature.bind(self, *args, **kwargs)
        bound.apply_defaults()
        p = dict(bound.arguments)
        p.pop("self")
        style = explicit_channel_colors(p.pop("kwargs"))
        source = str(p["text"])
        if len(source.encode("utf-8")) > _MAX_BYTES:
            raise ValueError("Text source exceeds 262144 bytes")
        if p["global_config"] or p["local_configs"]:
            raise NotImplementedError("Text global_config/local_configs require unsupported Pango attributes; use native font/style maps")
        maps = {name: _map(p[long], p[name], name) for name, long in (
            ("t2c", "text2color"), ("t2f", "text2font"), ("t2s", "text2slant"),
            ("t2w", "text2weight"), ("t2g", "text2gradient"),
        )}
        spacing = p["line_spacing_height"] if p["line_spacing_height"] is not None else p["lsh"]
        height = None if p["height"] is None else _number(p["height"], "Text height", positive=True)
        labelled = bool(style.pop("use_labelled_svg", False))
        centered = style.pop("should_center", True)
        path_string_config = dict(style.pop("path_string_config", None) or {})
        base_color = style.pop("base_color", "#FFFFFF")
        protect = style.pop("protect", ())
        g["_preflight_vmobject_style_kwargs"](style)
        self.text = self.string = source
        self.font_size, self.font = p["font_size"], p["font"]
        self.weight, self.slant = p["weight"], p["slant"]
        self.alignment, self.line_width = p["alignment"], p["line_width"]
        self.justify, self.indent, self.lsh = p["justify"], p["indent"], spacing
        self.global_config, self.local_configs = {}, {}
        self.disable_ligatures, self.isolate = p["disable_ligatures"], p["isolate"]
        self.gradient = None if p["gradient"] is None else _stops(g, p["gradient"], "gradient")
        self.use_labelled_svg, self.base_color, self.protect = labelled, base_color, protect
        self.path_string_config = path_string_config
        for name, value in maps.items():
            setattr(self, name, value)
        self._fmn_text_options = {}
        initialize_string(g, self, style)
        apply_text_colors(self)
        if centered:
            self.center()
        if height is not None:
            self.set_height(height)

    Markup.__init__ = initialize

    def get_part_by_text(self, selector, *args, **kwargs):
        # This is an alias of select_part, not a second selector. Forward the
        # occurrence index positionally or by keyword and retain authored
        # select_part overrides, including their optional extension keywords.
        return self.select_part(selector, *args, **kwargs)

    get_part_by_text.__qualname__ = Markup.__qualname__ + ".get_part_by_text"
    get_part_by_text.__module__ = Markup.__module__
    Markup.get_part_by_text = get_part_by_text
    _install_code(g)
    g["_FMN_TEXT_AUTHORING_INSTALLED"] = True


def _install_code(g):
    Code = g["Code"]
    previous = Code.__init__
    missing = object()

    @wraps(previous)
    def initialize(self, code, font="Consolas", font_size=24, lsh=1.0,
                   fill_color=None, stroke_color=None, language="python",
                   code_style="monokai", **kwargs):
        if not isinstance(font, str):
            raise TypeError("Code font must be a family name")
        _validate_code_names(language, code_style)
        # The legacy default name is an explicit Code-only compatibility
        # policy, not OS font discovery. Record the real bundled family.
        native_font = "CM Typewriter" if font in ("", "Consolas") else font
        source = str(code)
        if fill_color is not None:
            kwargs["fill_color"] = fill_color
        if stroke_color is not None:
            kwargs["stroke_color"] = stroke_color
        attrs = vars(self)
        old = attrs.get("_fmn_code_layout", missing)
        attrs["_fmn_code_layout"] = (language, code_style)
        try:
            super(Code, self).__init__(source, font=native_font, font_size=font_size,
                                      lsh=lsh, **kwargs)
        finally:
            if old is missing:
                attrs.pop("_fmn_code_layout", None)
            else:
                attrs["_fmn_code_layout"] = old
        self.code, self.font, self.native_font = source, font, native_font
        self.language, self.code_style = language, code_style

    # Code is a StringMobject with one live family of glyphs. The earlier
    # scaffold duplicated all child records into the parent, double-rendering
    # translucent ink and leaving stale copies after selector-based edits.
    Code._hoist_descendant_records = False
    Code.__init__ = initialize
    Code.__doc__ = (
        "Syntax-highlighted literal source over native Scribe glyphs. "
        "The existing native highlighter supplies positional token colors; "
        "Code's legacy Consolas default selects the bundled CM Typewriter. "
        "native_font records that actual choice. Unknown languages use plain "
        "native text, and unsupported theme/font names refuse."
    )
