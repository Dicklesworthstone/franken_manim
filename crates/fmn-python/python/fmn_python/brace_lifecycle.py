"""Authored braces over Atlas's parametric outline and Marionette records.

A brace is not a TeX glyph in this engine (BN-08). Its inherited Tex identity
is retained, but ordinary VMobject hooks operate on the actual analytic path.
The native builder only prepares a detached candidate; it never replaces an
initialized receiver's schema, children, saved state or view generation.
"""
from __future__ import annotations

import math

from .invocation import InvocationGuard


def install_brace_lifecycle(native):
    g = vars(native)
    if g.get("_FMN_BRACE_LIFECYCLE_INSTALLED", False):
        return
    Brace, LineBrace, VMobject = g["Brace"], g["LineBrace"], g["VMobject"]
    np = g["_np"]
    constructing, sampling = InvocationGuard(), InvocationGuard()

    def number(value, name, *, positive=False):
        value = float(value)
        if (not math.isfinite(value) or abs(value) > np.finfo(np.float32).max
                or (positive and value <= 0)):
            raise ValueError(name + " must be finite and f32-representable" +
                             (" and positive" if positive else ""))
        return value

    def vector(value):
        result = np.array(g["_vec3"](value), dtype=float)
        if not np.isfinite(result).all():
            raise ValueError("brace direction must be finite")
        return result

    def idle(self):
        if vars(self).get("_is_animating", False) or getattr(self, "locked_data_keys", ()):
            raise RuntimeError("release the brace's active animation before regenerating its points")

    def init(self, mobject, direction=g["_DOWN"], buff=0.2,
             tex_string=r"\underbrace{\qquad}", **kwargs):
        if not isinstance(mobject, g["Mobject"]):
            raise TypeError("Brace expects a Mobject")
        if mobject is self:
            raise ValueError("a brace cannot be its own source")
        if self._is_bound():
            raise RuntimeError("a brace constructor requires a detached target; use init_points")
        idle(self)
        selected = vector(direction)
        buff = number(buff, "brace buff")
        size = number(kwargs.pop("font_size", 48), "brace font_size", positive=True)
        text = str(tex_string)
        g["_preflight_vmobject_style_kwargs"](kwargs)
        options = dict(kwargs)
        options.setdefault("fill_color", g["_WHITE"])
        options.setdefault("fill_opacity", 1.0)
        options.setdefault("stroke_width", 0.0)
        with constructing.hold(self, message="brace initialization is already in progress"):
            self._brace_source = mobject
            self.direction, self.buff = selected, buff
            self.font_size = size
            self.tex_string, self.tex_strings = text, [text]
            self.string = text
            self.tip_point_index = None
            # Do not invoke Tex.__init__: that would typeset an unrelated glyph.
            # The shared initializer dispatches the complete real VMobject MRO.
            g["_init_native_vmobject"](self, options)
            # A replacement point hook can supply a completely different path.
            # Honor an explicit tip index; otherwise locate its outward extreme.
            if self.tip_point_index is None and self.get_num_points():
                self.tip_point_index = int(np.argmax(self.get_points() @ selected))

    def line_init(self, line, direction=g["_UP"], **kwargs):
        super(LineBrace, self).__init__(line, direction, **kwargs)

    def init_points(self):
        with sampling.hold(self, message="brace point regeneration is already in progress"):
            idle(self)
            source = self._brace_source
            if not isinstance(source, g["Mobject"]):
                raise TypeError("brace source must be a Mobject")
            selected = vector(self.direction)
            buff = number(self.buff, "brace buff")
            owner, bound = vars(self).get("_scene"), self._is_bound()
            before = self.data.copy()
            children = tuple(self.submobjects)
            candidate = g["_native_shell_factory"]()
            if isinstance(self, LineBrace):
                # Read each public endpoint once. The native builder owns the
                # line-local frame, including arbitrary angle and translation.
                start, end = vector(source.get_start()), vector(source.get_end())
                specs, tip = candidate._build_line_brace(
                    g["_native_shell_factory"], start, end, selected, buff)
            else:
                specs, tip = candidate._build_brace(
                    g["_native_shell_factory"], source, selected, buff)
            if specs:
                raise RuntimeError("a native brace outline unexpectedly returned children")
            points = candidate.get_points()
            if (not np.isfinite(points).all()
                    or (len(points) and not 0 <= tip < len(points))):
                raise ValueError("native brace returned invalid points or tip index")
            # Endpoint getters can run arbitrary authored code. Do not consume
            # a changed recipe or enter an animation lock after they return.
            idle(self)
            if (vars(self).get("_scene") is not owner or self._is_bound() != bound
                    or tuple(self.submobjects) != children
                    or self.data.dtype != before.dtype or self.data.tobytes() != before.tobytes()
                    or self._brace_source is not source or self.buff != buff
                    or not np.array_equal(vector(self.direction), selected)):
                raise RuntimeError("brace recipe changed during sampling; points were not published")
            self.set_points(points)
            self.tip_point_index = int(tip)
        return self

    def init_colors(self):
        # Tex's default hook expects typesetting metadata and preserves glyph
        # paints. Braces are an ordinary filled native VMobject instead.
        return VMobject.init_colors(self)

    for cls, name, function in (
        (Brace, "__init__", init), (Brace, "init_points", init_points),
        (Brace, "init_colors", init_colors), (LineBrace, "__init__", line_init),
    ):
        function.__name__ = name
        function.__qualname__ = cls.__qualname__ + "." + name
        function.__module__ = cls.__module__
        setattr(cls, name, function)
    g["_FMN_BRACE_LIFECYCLE_INSTALLED"] = True
