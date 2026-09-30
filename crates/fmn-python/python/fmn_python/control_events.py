"""Compose native-backed controls and connect their public event listeners.

Public primitive constructors own the geometry; the tracker lifecycle owns
scalar state and record schemas. This adapter assembles those objects and
input transitions without introducing a renderer or a second frame loop.
"""
from __future__ import annotations

from collections.abc import Mapping
from functools import wraps
import inspect
import math
from typing import Any

from .interaction import _method


def _same_callback(left, right):
    if left is right:
        return True
    function = getattr(left, "__func__", None)
    return (function is not None and function is getattr(right, "__func__", None)
            and getattr(left, "__self__", None) is getattr(right, "__self__", None))


def _connect(g, owner, specifications):
    planned = []
    for attribute, kind, registrar, handler in specifications:
        target = owner if not attribute else getattr(owner, attribute)
        if not isinstance(target, g["Mobject"]):
            raise TypeError(type(owner).__name__ + " event target must be a Mobject")
        callback = getattr(owner, handler)
        if not callable(callback):
            raise TypeError(type(owner).__name__ + "." + handler + " must be callable")
        planned.append((target, getattr(g["EventType"], kind), registrar, callback))
    touched = []
    try:
        for target, kind, registrar, callback in planned:
            before = tuple(target.get_event_listners())
            if any(listener.event_type == kind and _same_callback(listener.callback, callback)
                   for listener in before):
                continue
            touched.append((target, before))
            getattr(target, registrar)(callback)
    except BaseException as error:
        # A later registration can fail in an authored registrar. Remove only
        # new registrations and preserve the primary construction exception.
        for target, before in reversed(touched):
            for listener in tuple(target.get_event_listners()):
                if any(listener is prior for prior in before):
                    continue
                try:
                    target.remove_event_listner(listener.event_type, listener.callback)
                except BaseException as cleanup_error:
                    error.add_note("control listener cleanup also failed: " + type(cleanup_error).__name__)
        raise


def _bind_constructor(g, name, specifications, constructor=None):
    cls = g.get(name)
    if cls is None:
        return
    original = cls.__init__

    @wraps(original)
    def initialize(self, *args, **kwargs):
        (original if constructor is None else constructor)(self, *args, **kwargs)
        if name == "Button" and not callable(self.on_click):
            raise TypeError("Button.on_click must be callable")
        if name == "ControlPanel":
            self.panel_opener_rect, self.panel_info_text = self.panel_opener.submobjects
            self.fix_in_frame()
        _connect(g, self, specifications)

    _method(cls, "__init__", initialize)


def _shape(g, name, config, option_name, *args):
    """Keep nested option diagnostics while admitting the public shape API.

    Recognized constructor keys come from the current class signatures; style
    keys come from the existing native adapter. Do not maintain another subset
    of Rectangle/Line/Circle options or rewrite exceptions from authored hooks.
    """
    cls = g[name]
    accepted = set(g["_NATIVE_VMOBJECT_STYLE_KEYS"]) | {"shading"}
    for base in cls.__mro__:
        constructor = vars(base).get("__init__")
        if constructor is None:
            continue
        try:
            parameters = inspect.signature(constructor).parameters.values()
        except (TypeError, ValueError):
            continue  # Builtin slot wrappers need no extra constructor keys.
        accepted.update(p.name for p in parameters if p.kind in (
            inspect.Parameter.POSITIONAL_OR_KEYWORD, inspect.Parameter.KEYWORD_ONLY,
        ) and p.name != "self")
    unknown = set(config) - accepted
    if unknown:
        raise TypeError("unexpected keyword arguments: " + ", ".join(
            option_name + "." + key for key in sorted(unknown)))
    return cls(*args, **config)


def _typed_scalar_constructors(g):
    """Compose actual public primitive classes before the tracker hook pass.

    Atlas still owns every shape. Anonymous widget-builder shells cannot
    substitute for Rectangle/Circle/Line: those classes have public methods
    and authorable constructors of their own. Reduced event-only embedding
    tables supply their own constructors and do not include this geometry.
    """
    names = ("ControlMobject", "Rectangle", "RoundedRectangle", "Circle", "Line",
             "VGroup", "VMobject", "_NATIVE_VMOBJECT_STYLE_KEYS")
    if not all(name in g for name in names):
        return {}
    root_keys = {"color", "opacity", "shading", "texture_paths",
                 "is_fixed_in_frame", "depth_test", "z_index"}

    def options(value, name):
        if not isinstance(value, Mapping):
            raise TypeError(name + " must be a mapping")
        if len(value) > 4096:
            raise ValueError(name + " exceeds 4096 entries")
        return dict(value)

    def finite(value, name):
        value = float(value)
        if not math.isfinite(value):
            raise ValueError(name + " must be finite")
        return value

    def make_constructor(name, cls):
        signature = inspect.signature(cls.__init__)

        def initialize(self, *args, **kwargs):
            bound = signature.bind(self, *args, **kwargs)
            bound.apply_defaults()
            p = bound.arguments
            root = dict(p["kwargs"])
            # Retain the existing slider's user attributes (e.g. name=), while
            # passing actual base options into its one cooperative lifecycle.
            extra = {key: value for key, value in root.items() if key not in root_keys}
            if name != "LinearNumberSlider" and extra:
                raise TypeError("unexpected keyword arguments: " + ", ".join(sorted(extra)))
            root = {key: value for key, value in root.items() if key in root_keys}
            value_type = g["_np"].dtype(p["value_type"]).type
            if name == "Checkbox":
                if not isinstance(p["value"], bool):
                    raise AssertionError("Checkbox value must be bool")
                rect = options(p["rect_kwargs"], "rect_kwargs")
                check = options(p["checkmark_kwargs"], "checkmark_kwargs")
                cross = options(p["cross_kwargs"], "cross_kwargs")
                buff = finite(p["box_content_buff"], "box_content_buff")
                box = _shape(g, "Rectangle", rect, "rect_kwargs")
                self.value_type, self.rect_kwargs = value_type, rect
                self.checkmark_kwargs, self.cross_kwargs = check, cross
                self.box_content_buff, self.box = buff, box
                # These are public factories: subclasses may return their own
                # native-backed mark, which is attached without recasting it.
                mark = self.get_checkmark() if p["value"] else self.get_cross()
                if not isinstance(mark, g["VMobject"]):
                    raise TypeError("Checkbox mark factories must return VMobjects")
                self.box_content = mark
                super(cls, self).__init__(p["value"], box, mark, **root)
            elif name == "EnableDisableButton":
                if not isinstance(p["value"], bool):
                    raise AssertionError("EnableDisableButton value must be bool")
                rect = options(p["rect_kwargs"], "rect_kwargs")
                enable, disable = p["enable_color"], p["disable_color"]
                rgb = [g["_color_to_rgb"](color) for color in (enable, disable)]
                if not all(g["_np"].isfinite(color).all() for color in rgb):
                    raise ValueError("control colors must be finite")
                style = dict(rect)
                # Preserve the existing native white-at-construction default;
                # genuine caller paints and custom state colors still win.
                if style.get("fill_color") is None and style.get("color") is None:
                    style["fill_color"] = g["WHITE"]
                box = _shape(g, "Rectangle", style, "rect_kwargs")
                if any(not g["_np"].array_equal(actual, g["_color_to_rgb"](default))
                       for actual, default in zip(rgb, (g["GREEN"], g["RED"]))):
                    box.set_fill(enable if p["value"] else disable)
                self.rect_kwargs = rect
                self.enable_color, self.disable_color = enable, disable
                self.value_type, self.box = value_type, box
                super(cls, self).__init__(p["value"], box, **root)
            else:
                low, high = (finite(p[key], "slider bound")
                             for key in ("min_value", "max_value"))
                step, value = finite(p["step"], "slider step"), finite(p["value"], "slider value")
                if (low >= high or not math.isfinite(high - low) or step <= 0
                        or not math.isfinite((high - low) / step)):
                    raise ValueError("slider bounds must be finite, ordered and step positive")
                if not low <= value <= high:
                    raise ValueError("slider value must lie within its bounds")
                rect = options(p["rounded_rect_kwargs"], "rounded_rect_kwargs")
                circle = options(p["circle_kwargs"], "circle_kwargs")
                bar = _shape(g, "RoundedRectangle", rect, "rounded_rect_kwargs")
                handle = _shape(g, "Circle", circle, "circle_kwargs")
                axis = g["Line"](bar.get_bounding_box_point(g["LEFT"]),
                                 bar.get_bounding_box_point(g["RIGHT"]))
                if not math.isfinite(axis.get_length()) or axis.get_length() == 0:
                    raise ValueError("slider axis must have distinct finite endpoints")
                axis.set_opacity(0.)
                handle.move_to(axis)
                self.value_type = value_type
                self.min_value, self.max_value, self.step = low, high, step
                self.rounded_rect_kwargs, self.circle_kwargs = rect, circle
                self.bar, self.slider, self.slider_axis = bar, handle, axis
                super(cls, self).__init__(value, bar, handle, axis, **root)
                for key, value in extra.items():
                    setattr(self, key, value)
        return initialize

    return {name: make_constructor(name, g[name])
            for name in ("EnableDisableButton", "LinearNumberSlider", "Checkbox") if name in g}


def _install_transitions(g, *, typed_checkbox=False):
    Checkbox = g.get("Checkbox")
    if Checkbox is not None:
        def place_factory(original):
            @wraps(original)
            def mark(self):
                result = original(self)
                result.stretch_to_fit_width(self.box.get_width())
                result.stretch_to_fit_height(self.box.get_height())
                result.scale(0.5)
                result.move_to(self.box)
                if typed_checkbox:
                    # A fresh mark otherwise resets the fixed-frame uniform
                    # when become replaces it, letting it drift off its box.
                    if self.box.is_fixed_in_frame():
                        result.fix_in_frame()
                    else:
                        result.unfix_from_frame()
                return result
            return mark
        if typed_checkbox:
            # Same two-Line compositions as the Reference interactive.py,
            # using public native-backed constructors, not traced glyph paths.
            def checkmark(self):
                return g["VGroup"](
                    _shape(g, "Line", self.checkmark_kwargs, "checkmark_kwargs",
                           g["UP"] / 2 + 2 * g["LEFT"], g["DOWN"] + g["LEFT"]),
                    _shape(g, "Line", self.checkmark_kwargs, "checkmark_kwargs",
                           g["DOWN"] + g["LEFT"], g["UP"] + g["RIGHT"]),
                )

            def cross(self):
                return g["VGroup"](
                    _shape(g, "Line", self.cross_kwargs, "cross_kwargs",
                           g["UP"] + g["LEFT"], g["DOWN"] + g["RIGHT"]),
                    _shape(g, "Line", self.cross_kwargs, "cross_kwargs",
                           g["UP"] + g["RIGHT"], g["DOWN"] + g["LEFT"]),
                )
            factories = (("get_checkmark", checkmark), ("get_cross", cross))
        else:
            factories = tuple((name, getattr(Checkbox, name))
                              for name in ("get_checkmark", "get_cross"))
        for name, factory in factories:
            _method(Checkbox, name, place_factory(factory))

    Toggle = g.get("EnableDisableButton")
    if Toggle is not None:
        def color(self, value):
            # Rebuilding a native rectangle would reset a moved/resized box.
            self.box.set_fill(self.enable_color if value else self.disable_color)
        _method(Toggle, "set_value_anim", color)

    Textbox = g.get("Textbox")
    if Textbox is not None:
        def active(self, isActive):
            self.box.set_stroke(self.active_color if isActive else self.deactive_color)

        def key(self, mob, event_data):
            if not mob.isActive:
                return None
            symbol, modifiers = int(event_data["symbol"]), int(event_data["modifiers"])
            # Key symbols are not text events. In particular X11 navigation
            # keysyms such as 0xff51 must not become full-width Unicode letters.
            # An active editor consumes unsupported shortcuts without editing.
            if modifiers & (2 | 64):
                return False
            old = mob.get_value()
            if symbol == 0xFF08:
                new = old[:-1]
            elif symbol == 0xFF09:
                new = old + "\t"
            elif symbol == 0x20:
                new = old + " "
            elif 0 <= symbol < 0xFF00 and chr(symbol).isalnum():
                character = chr(symbol)
                new = old + (character.upper() if modifiers & (1 | 8) else character.lower())
            else:
                return False
            if new != old:
                mob.set_value(new)
            return False

        original_parts = Textbox._native_textbox_parts

        @wraps(original_parts)
        def text_parts(self, *args, **kwargs):
            parts = original_parts(self, *args, **kwargs)
            box = getattr(self, "box", None)
            if box is not None:
                # The constructor's native candidate is unpositioned. Re-seat
                # later text candidates against the live box, not the origin.
                parts[1].move_to(box)
            return parts

        _method(Textbox, "active_anim", active)
        _method(Textbox, "on_key_press", key)
        _method(Textbox, "_native_textbox_parts", text_parts)


def install_control_events(native: Any) -> None:
    """Activate public control handlers without replacing any class identity."""
    g = vars(native)
    if g.get("_FMN_CONTROL_EVENTS_INSTALLED", False):
        return
    constructors = _typed_scalar_constructors(g)
    _install_transitions(g, typed_checkbox="Checkbox" in constructors)
    bindings = {
        "MotionMobject": (("mobject", "MouseDragEvent", "add_mouse_drag_listner", "mob_on_mouse_drag"),),
        "Button": (("mobject", "MousePressEvent", "add_mouse_press_listner", "mob_on_mouse_press"),),
        "Checkbox": (("", "MousePressEvent", "add_mouse_press_listner", "on_mouse_press"),),
        "EnableDisableButton": (("", "MousePressEvent", "add_mouse_press_listner", "on_mouse_press"),),
        "LinearNumberSlider": (("slider", "MouseDragEvent", "add_mouse_drag_listner", "slider_on_mouse_drag"),),
        "Textbox": (
            ("box", "MousePressEvent", "add_mouse_press_listner", "box_on_mouse_press"),
            ("", "KeyPressEvent", "add_key_press_listner", "on_key_press"),
        ),
        "ControlPanel": (
            ("panel", "MouseScrollEvent", "add_mouse_scroll_listner", "panel_on_mouse_scroll"),
            ("panel_opener", "MouseDragEvent", "add_mouse_drag_listner", "panel_opener_on_mouse_drag"),
        ),
    }
    for name, specifications in bindings.items():
        _bind_constructor(g, name, specifications, constructors.get(name))
    g["_FMN_CONTROL_EVENTS_INSTALLED"] = True
