"""Native rich typography without SVG round-trips or rewritten source spans."""
from __future__ import annotations

from collections.abc import Mapping
from functools import wraps
import inspect
import itertools
import math

_MAX_BYTES = 262_144
_MAX_ITEMS = 4096


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


def install_text_authoring(native):
    g = vars(native)
    if g.get("_FMN_TEXT_AUTHORING_INSTALLED", False):
        return
    Markup = g["MarkupText"]
    build = g["_build_styled_text"]
    previous = Markup.__init__
    signature = inspect.signature(previous)

    @wraps(previous)
    def initialize(self, *args, **kwargs):
        bound = signature.bind(self, *args, **kwargs)
        bound.apply_defaults()
        p = dict(bound.arguments)
        p.pop("self")
        style = dict(p.pop("kwargs"))
        source = str(p["text"])
        if len(source.encode("utf-8")) > _MAX_BYTES:
            raise ValueError("Text source exceeds 262144 bytes")
        if p["global_config"] or p["local_configs"]:
            raise NotImplementedError("Text global_config/local_configs require unsupported Pango attributes; use native font/style maps")
        font = p["font"]
        if not isinstance(font, str):
            raise TypeError("Text font must be a family name")
        alignment = p["alignment"].upper() if isinstance(p["alignment"], str) else None
        if alignment not in ("", "LEFT", "CENTER", "RIGHT"):
            raise ValueError("Text alignment must be LEFT, CENTER or RIGHT")
        spacing = p["line_spacing_height"] if p["line_spacing_height"] is not None else p["lsh"]
        spacing = 1. if spacing is None else _number(spacing, "Text line spacing", positive=True)
        maps = {name: _map(p[long], p[name], name) for name, long in (
            ("t2c", "text2color"), ("t2f", "text2font"), ("t2s", "text2slant"),
            ("t2w", "text2weight"), ("t2g", "text2gradient"),
        )}
        for name in ("t2f", "t2s", "t2w", "t2g"):
            if any(not isinstance(key, str) for key in maps[name]):
                raise TypeError(name + " currently requires literal source-string keys")
        if any(not isinstance(value, str) for value in maps["t2f"].values()):
            raise TypeError("t2f values must be font family names")
        gradients = [(key, _stops(g, value, "t2g")) for key, value in maps["t2g"].items()]
        if sum(len(stops) for _, stops in gradients) > _MAX_ITEMS:
            raise ValueError("Text gradients exceed 4096 total stops")
        gradient = None if p["gradient"] is None else _stops(g, p["gradient"], "gradient")
        options = dict(font=font, font_size=_number(p["font_size"], "Text font_size", positive=True),
                       bold=_style_flag(p["weight"], "Text weight", "BOLD"),
                       italic=_style_flag(p["slant"], "Text slant", "ITALIC"),
                       alignment=alignment or "LEFT", line_spacing=spacing,
                       justify=bool(p["justify"]), indent=_number(p["indent"], "Text indent"),
                       disable_ligatures=bool(p["disable_ligatures"]),
                       t2f=list(maps["t2f"].items()),
                       t2s=[(key, _style_flag(value, "t2s", "ITALIC")) for key, value in maps["t2s"].items()],
                       t2w=[(key, _style_flag(value, "t2w", "BOLD")) for key, value in maps["t2w"].items()],
                       t2g=gradients)
        if p["line_width"] is not None:
            options["line_width"] = _number(p["line_width"], "Text line_width", positive=True)
        height = None if p["height"] is None else _number(p["height"], "Text height", positive=True)
        labelled = bool(style.pop("use_labelled_svg", False))
        centered = style.pop("should_center", True)
        style.pop("path_string_config", None)
        base_color = style.pop("base_color", "#FFFFFF")
        protect = style.pop("protect", ())
        g["_preflight_vmobject_style_kwargs"](style)
        g["_install_live_state"](self)
        self.text = self.string = source
        self.font_size, self.font = options["font_size"], font
        self.weight, self.slant = p["weight"], p["slant"]
        self.alignment, self.line_width = p["alignment"], p["line_width"]
        self.justify, self.indent, self.lsh = options["justify"], options["indent"], spacing
        self.global_config, self.local_configs = {}, {}
        self.disable_ligatures, self.isolate = options["disable_ligatures"], p["isolate"]
        self.use_labelled_svg, self.base_color, self.protect = labelled, base_color, protect
        for name, value in maps.items():
            setattr(self, name, value)
        specs = build(self, g["_native_shell_factory"], source, type(self)._native_markup,
                      type(self)._hoist_descendant_records, options)
        g["_hang_native_children"](self, specs)
        g["_apply_vmobject_style_kwargs"](self, style)
        if gradient:
            self.set_color_by_gradient(*gradient)
        # Existing selectors retain regex/Unicode/byte-span behavior and win
        # over a whole-object gradient, as on the public authoring surface.
        self.set_color_by_text_to_color_map(self.t2c)
        if centered:
            self.center()
        if height is not None:
            self.set_height(height)

    Markup.__init__ = initialize
    g["_FMN_TEXT_AUTHORING_INSTALLED"] = True
