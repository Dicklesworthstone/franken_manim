"""InteractiveScene gestures over the existing native selection and SceneState.

No window, renderer, geometry store or history serializer lives here. The
worker's admitted events drive the Reference-named methods on the original
class. Transformations still use native Mobject/Group operations, and history
still uses the normal SceneState scope (not arbitrary Python-effect rollback).
"""
from __future__ import annotations

from types import SimpleNamespace
from typing import Any

from .interaction import _method, input_key_pressed, input_modifiers


def install_interactive_editing(native: Any) -> None:
    g = vars(native)
    if g.get("_FMN_INTERACTIVE_EDITING_INSTALLED", False):
        return
    Scene, Interactive, np = g["Scene"], g["InteractiveScene"], g["_np"]
    old_restore = Interactive.restore_state
    primary = g["_PYGLET_MOD_CTRL"] | g["_PYGLET_MOD_COMMAND"]
    shift, control = g["_PYGLET_MOD_SHIFT"], g["_PYGLET_MOD_CTRL"]

    def keys():
        return g["_pinned_manim_config"]().key_bindings

    def selected(scene):
        return tuple(id(mob) for mob in scene.selection)

    def remember(scene, gesture=None):
        if gesture is None or not gesture.saved:
            scene.save_state()
            # save_state deduplicates identical snapshots. A real new edit
            # still invalidates redo even when its pre-state was deduplicated.
            scene.redo_stack.clear()
            if gesture is not None:
                gesture.saved = True

    def cancel(scene):
        scene.__dict__.pop("_fmn_edit_gesture", None)
        scene.is_grabbing = False

    def current(scene, kind):
        gesture = scene.__dict__.get("_fmn_edit_gesture")
        if gesture is not None and (gesture.members != selected(scene) or not gesture.members):
            cancel(scene)
            return None
        return gesture if gesture is not None and gesture.kind == kind else None

    def prepare_grab(self):
        if not len(self.selection):
            cancel(self)
            return
        old = self.__dict__.get("_fmn_edit_gesture")
        self.mouse_to_selection = self.mouse_point.get_center() - self.selection.get_center()
        self._fmn_edit_gesture = SimpleNamespace(
            kind="grab", members=selected(self), saved=bool(old and old.saved),
            key=None, axis=None,
        )
        self.is_grabbing = True

    def handle_grabbing(self, point):
        gesture = current(self, "grab")
        if gesture is None:
            return
        desired = np.asarray(g["_vec3"](point)) - self.mouse_to_selection
        delta = desired - self.selection.get_center()
        k = keys()
        # The key that opened a gesture is its owner; unrelated held keys and
        # auto-repeat cannot redirect it. Direct calls may use host key state.
        axis = gesture.axis
        if gesture.key is None:
            for index, key in enumerate((k.x_grab, k.y_grab, k.z_grab)):
                if input_key_pressed(self, ord(key)):
                    axis = index
                    break
        if axis is not None:
            delta = np.array([value if index == axis else 0.0 for index, value in enumerate(delta)])
        if not np.isfinite(delta).all():
            raise ValueError("grab must produce finite coordinates")
        if np.any(delta != 0):
            remember(self, gesture)
            self.selection.shift(delta)

    def prepare_resizing(self, about_corner=False):
        if not len(self.selection):
            cancel(self)
            return
        old = self.__dict__.get("_fmn_edit_gesture")
        center, mouse = self.selection.get_center(), self.mouse_point.get_center()
        self.scale_about_point = (self.selection.get_corner(center - mouse)
                                  if about_corner else center.copy())
        self.scale_ref_vect = mouse - self.scale_about_point
        self.scale_ref_width, self.scale_ref_height = self.selection.get_width(), self.selection.get_height()
        self._fmn_edit_gesture = SimpleNamespace(
            kind="resize", members=selected(self), saved=bool(old and old.saved),
            key=keys().resize, scales=np.ones(3),
        )
        self.is_grabbing = False

    def handle_resizing(self, point):
        gesture = current(self, "resize")
        if gesture is None:
            return
        vector = np.asarray(g["_vec3"](point)) - self.scale_about_point
        reference = self.scale_ref_vect
        if input_modifiers(self) & control:
            target = np.ones(3)
            for dim in (0, 1):
                if abs(reference[dim]) > np.finfo(float).eps:
                    ratio = vector[dim] / reference[dim]
                    # Keep a reversible, noncollapsed geometry at the pivot;
                    # crossing it can reflect an axis, never divide by zero.
                    target[dim] = np.copysign(max(abs(ratio), 1e-6), ratio)
        else:
            length = float(np.linalg.norm(reference))
            if length <= np.finfo(float).eps:
                return
            target = np.full(3, max(float(np.linalg.norm(vector)) / length, 1e-6))
        if not np.isfinite(target).all() or np.any(np.abs(target) > 1e6):
            raise ValueError("resize scale must be finite and no larger than 1000000")
        ratios = target / gesture.scales
        if np.any(ratios != 1):
            remember(self, gesture)
            if np.all(ratios == ratios[0]) and ratios[0] > 0:
                self.selection.scale(float(ratios[0]), about_point=self.scale_about_point)
            else:
                for dim, ratio in enumerate(ratios):
                    if ratio != 1:
                        self.selection.stretch(float(ratio), dim, about_point=self.scale_about_point)
            gesture.scales = target

    def on_mouse_motion(self, point, d_point):
        Scene.on_mouse_motion(self, point, d_point)
        self.crosshair.move_to(self.frame.to_fixed_frame_point(point))
        gesture = self.__dict__.get("_fmn_edit_gesture")
        if gesture is not None:
            if gesture.kind == "grab":
                self.handle_grabbing(point)
            else:
                self.handle_resizing(point)
        elif self.is_selecting:
            if input_modifiers(self) & shift:
                self._fmn_selection_swept = True
                self.handle_sweeping_selection(point)
            else:
                self.update_selection_rectangle(self.selection_rectangle)

    def on_mouse_drag(self, point, d_point, buttons, modifiers):
        # Preserve ordinary camera drag when no editing gesture owns it.
        # Listeners already had their chance to consume the event in the
        # scene input gateway; editing never dispatches them a second time.
        if self.__dict__.get("_fmn_edit_gesture") is not None or self.is_selecting:
            on_mouse_motion(self, point, d_point)
        else:
            Scene.on_mouse_drag(self, point, d_point, buttons, modifiers)
            self.crosshair.move_to(self.frame.to_fixed_frame_point(point))

    def on_key_press(self, symbol, modifiers):
        k = keys()
        try:
            char = chr(int(symbol))
        except (OverflowError, TypeError, ValueError):
            return
        modifiers = int(modifiers)
        ctrl = bool(modifiers & primary)
        grabs = (k.grab, k.x_grab, k.y_grab, k.z_grab)
        # Scene owns undo/redo, camera reset and presenter keys. Return after
        # undo/redo: ctrl-z must not create a new grab snapshot of its result.
        Scene.on_key_press(self, symbol, modifiers)
        if ctrl and char == "z":
            return
        if char in grabs and modifiers == 0:
            gesture = self.__dict__.get("_fmn_edit_gesture")
            if gesture is None or gesture.kind != "grab" or gesture.key != char:
                self.prepare_grab()
                gesture = current(self, "grab")
                if gesture is not None:
                    gesture.key = char
                    gesture.axis = None if char == k.grab else grabs.index(char) - 1
        elif char == k.resize and not ctrl:
            gesture = current(self, "resize")
            if gesture is None:
                self.prepare_resizing(about_corner=bool(modifiers & shift))
        elif char == k.select and not ctrl:
            if not self.is_selecting:
                cancel(self)
                self._fmn_selection_swept = False
                self.enable_selection()
                self.update_selection_rectangle(self.selection_rectangle)
            self.add(self.crosshair)
        elif char == k.unselect:
            cancel(self)
            self.clear_selection()
        elif char == k.color and modifiers == 0:
            self.toggle_color_palette()
        elif char == k.information and modifiers == 0:
            self.display_information()
        elif ctrl:
            cancel(self)
            if char == "a":
                self.clear_selection()
                self.add_to_selection(*self.get_selection_search_set())
            elif char == "g" and len(self.selection):
                remember(self)
                self.ungroup_selection() if modifiers & shift else self.group_selection()
            elif char == "t":
                self.toggle_selection_mode()
            elif char == "c":
                self.copy_selection()
            elif char == "v":
                self.paste_selection()
            elif char == "x" and len(self.selection):
                self.copy_selection()
                remember(self)
                self.delete_selection()
        elif symbol == g["_PYGLET_BACKSPACE"] and len(self.selection):
            cancel(self)
            remember(self)
            self.delete_selection()
        elif symbol in g["_PYGLET_ARROW_SYMBOLS"] and len(self.selection):
            cancel(self)
            remember(self)
            vectors = (g["_LEFT"], g["_UP"], g["_RIGHT"], g["_DOWN"])
            self.nudge_selection(vectors[g["_PYGLET_ARROW_SYMBOLS"].index(symbol)], large=bool(modifiers & shift))
        elif char == "d" and modifiers & shift:
            self.copy_frame_positioning()
        elif char == "c" and modifiers & shift:
            self.copy_cursor_position()
        if char == k.cursor and not ctrl:
            if self.crosshair in self.mobjects:
                self.remove(self.crosshair)
            else:
                self.add(self.crosshair)

    def on_key_release(self, symbol, modifiers):
        Scene.on_key_release(self, symbol, modifiers)
        try:
            char = chr(int(symbol))
        except (OverflowError, TypeError, ValueError):
            return
        gesture = self.__dict__.get("_fmn_edit_gesture")
        if gesture is not None and gesture.key == char:
            cancel(self)
        k = keys()
        if char == k.select:
            # Sweeping is additive. Reapplying the marquee toggle would
            # unselect the very objects collected during this gesture.
            if self.__dict__.pop("_fmn_selection_swept", False):
                self.is_selecting = False
                self.remove(self.selection_rectangle)
            elif self.is_selecting:
                self.gather_new_selection()
        elif char == k.information:
            self.display_information(False)

    def restore_state(self, state):
        old_restore(self, state)
        cancel(self)
        self.is_selecting = False
        self.__dict__.pop("_fmn_selection_swept", None)
        self.clear_selection()
        self.regenerate_selection_search_set()

    for name, method in {
        "prepare_grab": prepare_grab, "handle_grabbing": handle_grabbing,
        "prepare_resizing": prepare_resizing, "handle_resizing": handle_resizing,
        "on_mouse_motion": on_mouse_motion, "on_mouse_drag": on_mouse_drag,
        "on_key_press": on_key_press, "on_key_release": on_key_release,
        "restore_state": restore_state,
    }.items():
        _method(Interactive, name, method)
    g["_FMN_INTERACTIVE_EDITING_INSTALLED"] = True
