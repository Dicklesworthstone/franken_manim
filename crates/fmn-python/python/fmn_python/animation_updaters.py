"""Persistent animation lifecycles driven by the Scene's existing updater dt.

This adapter does not play a segment, sample frames, or own a second clock.
It implements the public animation-to-updater helpers using the same Animation
protocol used by ordinary playback.
"""
from __future__ import annotations

import math
import sys
from typing import Any


def _note(error, message):
    try:
        error.add_note(message)
    except BaseException:
        pass


def _duration(driver):
    value = float(driver.get_run_time())
    if not math.isfinite(value) or value < 0:
        raise ValueError("animation updater run_time must be finite and nonnegative")
    return value


def _has_python_interpolation(g, animation):
    if getattr(animation, "_native_kind", None) in {"cyclic_replace", "swap"}:
        return False
    base = g["Animation"]
    for name in ("interpolate", "interpolate_mobject", "interpolate_submobject"):
        method = getattr(animation, name)
        implementation = getattr(method, "__func__", method)
        if implementation is not getattr(base, name):
            if not getattr(implementation, "_fmn_schema_placeholder", False):
                return True
    return not getattr(animation, "_native_kind", None)


class _PersistentAnimation:
    def __init__(self, g, animation, anchor, cycle):
        self.g, self.animation, self.anchor = g, animation, anchor
        self.cycle = bool(cycle)
        self.driver = animation
        self.closed = self.busy = self.begun = False
        self.prior_suspension = []

        def update(current, dt):
            return self.step(current, dt)

        self.update = update

    def detach(self):
        self.anchor.remove_updater(self.update)

    def cancel(self, original):
        if self.closed:
            return
        self.closed = True
        actions = []
        if self.begun:
            abort = getattr(self.driver, "abort", None)
            if callable(abort):
                actions.append(abort)
            actions.append(lambda: self.anchor.set_animating_status(False))
            if isinstance(self.animation, self.g["Transform"]):
                actions.append(self.anchor.unlock_data)
            for member, suspended in self.prior_suspension:
                if not suspended:
                    actions.append(lambda member=member: member.resume_updating(
                        recurse=False, call_updater=False,
                    ) if member._is_updating_suspended() else None)
        actions.append(self.detach)
        for action in actions:
            try:
                action()
            except BaseException as error:
                _note(original, "animation updater cleanup failed: " + type(error).__name__)

    def start(self):
        duration = _duration(self.driver)
        if self.cycle and duration == 0:
            raise ValueError("a cycling animation updater requires positive run_time")
        self.animation.suspend_mobject_updating = False
        self.animation.total_time = 0.0
        self.prior_suspension = [
            (member, member._is_updating_suspended())
            for member in self.anchor.get_family()
        ]
        self.begun = True
        try:
            self.driver.begin()
            # Retain Reference's immediate zero-dt registration update.
            self.anchor.add_updater(self.update)
        except BaseException as error:
            self.cancel(error)
            raise
        return self.anchor

    def step(self, current, dt):
        # Animation starting/target copies share updater callables by design.
        # They must not advance this controller on behalf of its original mob.
        if self.closed or self.busy or current is not self.anchor:
            return
        self.busy = True
        try:
            delta = float(dt)
            elapsed = float(self.animation.total_time)
            if not math.isfinite(delta) or not math.isfinite(elapsed):
                raise ValueError("animation updater dt and total_time must be finite")
            duration = _duration(self.driver)
            if self.cycle and duration == 0:
                raise ValueError("a cycling animation updater requires positive run_time")
            if not self.cycle and (duration == 0 or elapsed >= duration):
                self.driver.finish()
                self.closed = True
                self.detach()
                return
            following = elapsed + delta
            if not math.isfinite(following):
                raise ValueError("animation updater accumulated time is not finite")
            alpha = (elapsed / duration) % 1.0 if self.cycle else max(0.0, elapsed / duration)
            # This helper's pinned contract is interpolate THEN helper update.
            # It is itself executed inside the Scene's ordinary updater phase.
            self.driver.interpolate(alpha)
            self.driver.update_mobjects(delta)
            self.animation.total_time = following
        except BaseException as error:
            self.cancel(error)
            raise
        finally:
            self.busy = False


def install_animation_updaters(native: Any) -> None:
    """Install the two helpers without replacing any public animation class."""
    g = vars(native)
    if g.get("_FMN_ANIMATION_UPDATERS_INSTALLED", False):
        return
    Animation, Mobject = g["Animation"], g["Mobject"]
    previous = {name: g[name] for name in ("turn_animation_into_updater", "cycle_animation")}

    def turn_animation_into_updater(animation, cycle=False, **kwargs):
        if not isinstance(animation, Animation):
            raise TypeError("turn_animation_into_updater requires an Animation")
        if not _has_python_interpolation(g, animation):
            raise NotImplementedError(
                type(animation).__name__ + " requires a scene-bound native animation driver"
            )
        animation.update_rate_info(**kwargs)
        animation._ensure_runtime_defaults()
        if isinstance(animation, g["AnimationGroup"]):
            g["_fmn_ensure_composition_root"](animation)
        anchor = animation.mobject
        if not isinstance(anchor, Mobject):
            raise TypeError("animation updater must animate a Mobject")
        return _PersistentAnimation(g, animation, anchor, cycle).start()

    def cycle_animation(animation, **kwargs):
        return turn_animation_into_updater(animation, cycle=True, **kwargs)

    replacements = {
        "turn_animation_into_updater": turn_animation_into_updater,
        "cycle_animation": cycle_animation,
    }
    # The schema may re-export a helper into multiple compatibility modules.
    # Replace only aliases to our old function, not later authored replacements.
    for name, function in replacements.items():
        old = previous[name]
        function.__name__ = function.__qualname__ = name
        function.__module__ = old.__module__
        for module in tuple(sys.modules.values()):
            module_name = getattr(module, "__name__", "")
            if module_name == "manimlib" or module_name.startswith("manimlib."):
                namespace = vars(module)
                for key, value in tuple(namespace.items()):
                    if value is old:
                        namespace[key] = function
        for key, value in tuple(g.items()):
            if value is old:
                g[key] = function
        g[name] = function
    g["_FMN_ANIMATION_UPDATERS_INSTALLED"] = True
