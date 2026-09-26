"""Shape-matcher lifecycle over the existing native-backed Rectangle protocol.

A surrounding box is an authored Rectangle that is resized and positioned,
not a disposable native tree. Keep its record schema, children, styles and
public hooks when following a moving target or changing its padding.
"""
from __future__ import annotations

import math


def install_shape_matchers(native):
    g = vars(native)
    if g.get("_FMN_SHAPE_MATCHERS_INSTALLED", False):
        return
    Surrounding = g["SurroundingRectangle"]
    Mobject = g["_BridgeMobject"]

    def require_target(mobject, operation):
        if not isinstance(mobject, Mobject):
            raise TypeError(operation + " expects a Mobject")

    def padding(value):
        value = float(value)
        if not math.isfinite(value):
            raise ValueError("surrounding rectangle buff must be finite")
        return value

    def surrounding_init(self, mobject, buff=0.1, color=g["_YELLOW"], **kwargs):
        require_target(mobject, "SurroundingRectangle")
        buff = padding(buff)
        # Preserve the portal's established fallback policy: explicit channel
        # colors win over the surrounding box's constructor color.
        if kwargs.get("fill_color") is None:
            kwargs["fill_color"] = color
        if kwargs.get("stroke_color") is None:
            kwargs["stroke_color"] = color
        # Rectangle owns the native geometry recipe and the single ordinary
        # init_data -> init_points -> init_uniforms -> init_colors dispatch.
        super(Surrounding, self).__init__(**kwargs)
        self.buff = buff
        self.surround(mobject)
        if mobject.is_fixed_in_frame():
            self.fix_in_frame()

    def surround(self, mobject, buff=None):
        require_target(mobject, "SurroundingRectangle.surround")
        buff = padding(self.buff if buff is None else buff)
        self.mobject = mobject
        self.buff = buff
        # This is Rectangle.surround's native set_shape/move_to operation on
        # the existing family. Rebuilding a fresh rectangle here destroys a
        # hook-authored outline or gradient. Reapplying get_style() also calls
        # BackgroundRectangle.set_style, which deliberately forces black; a
        # size change must not accidentally become that explicit style edit.
        super(Surrounding, self).surround(mobject, self.buff)
        return self

    def set_buff(self, buff):
        self.buff = padding(buff)
        # Reference set_buff calls the public hook with just the mobject. A
        # subclass accepting only that positional argument remains legal.
        self.surround(self.mobject)
        return self

    for name, function in (("__init__", surrounding_init), ("surround", surround),
                           ("set_buff", set_buff)):
        function.__name__ = name
        function.__qualname__ = Surrounding.__qualname__ + "." + name
        function.__module__ = Surrounding.__module__
        setattr(Surrounding, name, function)
    g["_FMN_SHAPE_MATCHERS_INSTALLED"] = True
