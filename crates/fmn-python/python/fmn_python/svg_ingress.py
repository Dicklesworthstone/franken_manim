"""Native SVG paint preparation under the existing SVGMobject front door.

The host only installs prepared families and preserves their paint roles during
ordinary styling. XML, inherited styles, filled-set normalization, subdivision,
and arclength all remain native. No source rewriting or second path model.
"""
from __future__ import annotations

from functools import wraps
import inspect

from .invocation import InvocationGuard

_ROLE = "_fmn_svg_paint_role"


def install_svg_ingress(native):
    g = vars(native)
    if g.get("_FMN_SVG_INGRESS_INSTALLED", False):
        return
    Svg, VM, Mob, np = g["SVGMobject"], g["VMobject"], g["Mobject"], g["_np"]
    builder = g.get("_build_svg_paints")
    if not callable(builder):
        raise ImportError("native SVG paint preparation is missing from this wheel")

    def members(obj, recurse):
        return tuple(g["_family_preorder"](obj)) if recurse else (obj,)

    def disallowed(obj, field):
        role = vars(obj).get(_ROLE)
        return ((role == "fill" and field == "stroke_rgba")
                or (role == "stroke" and field == "fill_rgba"))

    old_color = Mob.set_rgba_array_by_color
    @wraps(old_color)
    def by_color(self, color=None, opacity=None, name="rgba", recurse=True):
        family = members(self, recurse)
        if not any(_ROLE in vars(obj) for obj in family):
            return old_color(self, color, opacity, name, recurse)
        for obj in family:
            old_color(obj, color, 0.0 if disallowed(obj, name) else opacity, name, False)
        return self

    def masked(value):
        rgba = np.array(value, dtype=float, copy=True)
        if rgba.ndim == 0 or rgba.shape[-1] != 4:
            raise ValueError("SVG paint RGBA arrays must have four channels")
        rgba[..., 3] = 0
        return rgba

    old_array = Mob.set_rgba_array
    @wraps(old_array)
    def array(self, rgba_array, name="rgba", recurse=False):
        family = members(self, recurse)
        if not any(disallowed(obj, name) for obj in family):
            return old_array(self, rgba_array, name, recurse)
        inactive = masked(rgba_array)
        for obj in family:
            old_array(obj, inactive if disallowed(obj, name) else rgba_array, name, False)
        return self

    old_style = VM.set_style
    style_signature = inspect.signature(old_style)
    @wraps(old_style)
    def style(self, *args, **kwargs):
        bound = style_signature.bind(self, *args, **kwargs)
        bound.apply_defaults()
        family = members(self, bound.arguments["recurse"])
        if not any(_ROLE in vars(obj) for obj in family):
            return old_style(self, *args, **kwargs)
        values = dict(bound.arguments)
        values.pop("self")
        values["recurse"] = False
        # Explicit RGBA style arrays bypass the ordinary color setter in the
        # base class. Keep those aliases, and match_style, paint-role aware too.
        for obj in family:
            current = dict(values)
            for field in ("fill_rgba", "stroke_rgba"):
                if disallowed(obj, field):
                    current[field.removesuffix("rgba") + "opacity"] = 0.0
                    if current[field] is not None:
                        current[field] = masked(current[field])
            old_style(obj, **current)
        return self

    def tag(specs):
        # The native checked builder emits either an unchanged flat shape or
        # a recordless shape containing one fill layer followed by strokes.
        # Even empty/hidden roles are retained, so future styling stays correct.
        for _, children in specs:
            for index, (layer, _) in enumerate(children):
                vars(layer)[_ROLE] = "fill" if index == 0 else "stroke"
        return specs

    constructors = InvocationGuard()

    def build(self, factory, file_name, svg_string):
        if svg_string is not None:
            source = svg_string
        elif constructors.busy(self):
            # The unchanged constructor has already called the authored
            # resolver after installing live state and saved these exact bytes.
            if "svg_string" not in vars(self):
                raise RuntimeError("SVG source requested before its resolver completed")
            source = vars(self)["svg_string"]
        else:
            source = self.file_name_to_svg_string(file_name)
        return tag(builder(self, factory, source, {}))

    old_init = Svg.__init__
    @wraps(old_init)
    def initialize(self, *args, **kwargs):
        # Keep native-class identity, original signature and all authored
        # constructor/resolver dispatch. Only the redundant second read changes.
        with constructors.hold(self, message="SVG construction cannot reenter the same object"):
            return old_init(self, *args, **kwargs)

    def parts(self, source):
        # This API returns detached shapes; it must not replace the receiver's
        # nursery as the former native-builder call on self did.
        root = VM()
        specs = tag(builder(root, g["_native_shell_factory"], source, {}))
        g["_hang_native_children"](root, specs)
        result = list(root.submobjects)
        root.remove(*result)
        return result

    def rebuild(self):
        # Reference svg_mobject.py:123 (Ledger row `same`): add a freshly
        # built family, so a second call appends a second one, and return
        # None. Preparing first keeps a refused source from publishing.
        self.add(*parts(self, self.svg_string))

    Mob.set_rgba_array_by_color = by_color
    Mob.set_rgba_array = array
    VM.set_style = style
    Svg.__init__ = initialize
    for name, function in (("_build_svg_mobject", build),
                           ("mobjects_from_svg_string", parts),
                           ("init_svg_mobject", rebuild)):
        function.__name__ = name
        function.__qualname__ = Svg.__qualname__ + "." + name
        function.__module__ = Svg.__module__
        setattr(Svg, name, function)
    g["_FMN_SVG_INGRESS_INSTALLED"] = True
