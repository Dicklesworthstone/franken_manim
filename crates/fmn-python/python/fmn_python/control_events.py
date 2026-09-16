"""Connect Atlas-built controls to the existing public event-listener API.

Constructors still delegate geometry and scalar state to their native owners.
Only input registration and live-control transitions are assembled here.
"""
from __future__ import annotations

from functools import wraps
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


def _bind_constructor(g, name, specifications):
    cls = g.get(name)
    if cls is None:
        return
    original = cls.__init__

    @wraps(original)
    def initialize(self, *args, **kwargs):
        original(self, *args, **kwargs)
        if name == "Button" and not callable(self.on_click):
            raise TypeError("Button.on_click must be callable")
        if name == "ControlPanel":
            self.panel_opener_rect, self.panel_info_text = self.panel_opener.submobjects
            self.fix_in_frame()
        _connect(g, self, specifications)

    _method(cls, "__init__", initialize)


def _install_transitions(g):
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
                return result
            return mark
        for name in ("get_checkmark", "get_cross"):
            _method(Checkbox, name, place_factory(getattr(Checkbox, name)))

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
    _install_transitions(g)
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
        _bind_constructor(g, name, specifications)
    g["_FMN_CONTROL_EVENTS_INSTALLED"] = True
