"""Native typesetting inside the ordinary VMobject initialization lifecycle.

Scribe owns glyph outlines and byte spans. This adapter publishes its detached
candidate without replacing the subclass's record schema or hook-owned children.
No SVG parser, second typesetter, or substitute interpolation is introduced.
"""
from __future__ import annotations

from .coordinate_lifecycle import _bind


def initialize_string(g, obj, style):
    """Recipe fields must already exist before init_data/init_points execute."""
    g["_preflight_vmobject_style_kwargs"](style)
    obj._fmn_string_style = dict(style)
    obj._string_sub_spans, obj._string_sub_paths = [], []
    obj._fmn_string_children = g["_np"].empty(0, dtype=object)
    options = dict(style)
    options.setdefault("fill_opacity", 1.0)
    options.setdefault("stroke_width", 0.0)
    options.setdefault("fill_color", obj.base_color)
    # VMobject dispatches init_colors after native data/points/uniforms. The
    # string-specific default below preserves native markup/highlight paints;
    # authored overrides can call it and then change real glyph colors.
    g["_init_native_vmobject"](obj, options)


def _publish(g, obj, candidate, specs):
    g["_hang_native_children"](candidate, specs)
    spans = list(candidate._string_sub_spans)
    paths = [list(path) for path in candidate._string_sub_paths]
    previous = {id(child) for child in getattr(obj, "_fmn_string_children", ())}
    retained = [child for child in obj.submobjects if id(child) not in previous]
    children = tuple(candidate.submobjects)
    offset = len(retained)
    if any(not path or not 0 <= path[0] < len(children) for path in paths):
        raise ValueError("native string span paths do not identify glyph children")
    # Rebase only the first component: TeX parts retain their nested structure.
    paths = [[path[0] + offset, *path[1:]] for path in paths]
    # Plain tuples keep shallow references under the public copy contract.
    # Record generated members in the shared copier's object-array protocol
    # so a copied string claims its own glyphs, not the original's objects.
    owned = g["_np"].empty(len(children), dtype=object)
    for index, child in enumerate(children):
        owned[index] = child
    obj.set_points(candidate.get_points())
    obj.set_submobjects([*retained, *children])
    obj._string_sub_spans, obj._string_sub_paths = spans, paths
    obj._fmn_string_children = owned
    return obj


def install_string_lifecycle(native):
    g = vars(native)
    if g.get("_FMN_STRING_LIFECYCLE_INSTALLED", False):
        return
    Markup, Tex = g["MarkupText"], g["Tex"]

    def init_colors(self):
        # Only explicit caller styles override the native per-glyph paints.
        # Give the mutating style adapter a fresh dictionary on every call.
        g["_apply_vmobject_style_kwargs"](self, dict(self._fmn_string_style))
        return self

    def text_points(self):
        candidate = g["_native_shell_factory"]()
        options = dict(self._fmn_text_options, font_size=float(self.font_size), font=getattr(self, "native_font", self.font))
        specs = g["_build_styled_text"](
            candidate, g["_native_shell_factory"], self.text,
            type(self)._native_markup, type(self)._hoist_descendant_records, options,
        )
        _publish(g, self, candidate, specs)
        self.string = self.text
        return self

    def tex_points(self):
        separator = getattr(self, "_tex_arg_separator", " ")
        parts, separator = self._isolate_segments(separator)
        native_colors = {key: value for key, value in self.tex_to_color_map.items() if isinstance(key, str)}
        candidate = g["_native_shell_factory"]()
        specs = candidate._build_tex(
            g["_native_shell_factory"], parts, separator,
            bool(self._native_text_mode), self.font_size, native_colors or None,
            bool(self._native_group_single_part), self.template, self.additional_preamble,
            g["_tex_line_align"](self.alignment, bool(self._native_text_mode)),
        )
        return _publish(g, self, candidate, specs)

    def tex_init(self, *tex_strings, font_size=48, alignment="\\centering", template="",
                 additional_preamble="", tex_to_color_map={}, t2c={}, isolate=[],
                 use_labelled_svg=True, **kwargs):
        should_center = kwargs.pop("should_center", True)
        self.base_color = kwargs.pop("base_color", "#FFFFFF")
        self.protect = kwargs.pop("protect", ())
        g["_validate_tex_options"](template, additional_preamble, bool(self._native_text_mode))
        g["_tex_line_align"](alignment, bool(self._native_text_mode))
        colors = dict(t2c or {})
        colors.update(tex_to_color_map or {})
        isolate = [] if isolate is None else isolate
        if len(tex_strings) > 1:
            if isinstance(isolate, (str, g["_re"].Pattern, tuple)):
                isolate = [isolate]
            isolate = [*(isolate or []), *tex_strings]
        self.alignment, self.template, self.additional_preamble = alignment, template, additional_preamble
        self.tex_to_color_map = colors
        self.use_labelled_svg, self.isolate = bool(use_labelled_svg), isolate
        separator = getattr(self, "_tex_arg_separator", " ")
        self.tex_strings = [str(part) for part in tex_strings]
        if self.tex_strings:
            self.tex_strings[0] = self.tex_strings[0].lstrip()
            self.tex_strings[-1] = self.tex_strings[-1].rstrip()
        self.tex_string = separator.join(self.tex_strings).strip()
        if not self.tex_string:
            self.tex_strings, self.tex_string = [r"\\"], r"\\"
        self.string, self.font_size = self.tex_string, float(font_size)
        initialize_string(g, self, kwargs)
        self._validate_isolate_spans()
        self.set_color_by_tex_to_color_map(colors)
        if should_center:
            self.center()

    _bind(Markup, "init_points", text_points)
    _bind(Tex, "init_points", tex_points)
    tex_init.__annotations__ = dict(Tex.__init__.__annotations__)
    _bind(Tex, "__init__", tex_init)
    # Distinct functions keep correct defining-class introspection on aliases.
    def text_colors(self):
        return init_colors(self)
    def tex_colors(self):
        return init_colors(self)
    _bind(Markup, "init_colors", text_colors)
    _bind(Tex, "init_colors", tex_colors)
    g["_FMN_STRING_LIFECYCLE_INSTALLED"] = True
