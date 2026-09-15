"""Persistent animations on the Scene's existing updater boundary.

Callback animations retain their authored lifecycle. Native-only animations
borrow Choreo's existing scene-bound driver, not another interpolation engine
or clock. Persistent effects never perform scene-removal/publication cleanup.
"""
from __future__ import annotations

from functools import wraps
import inspect
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


def _scene_for(g, animation, anchor):
    """Resolve ownership without invoking create_target or changing draw roots."""
    objects = [anchor, *getattr(animation, "_native_extra_mobjects", ())]
    target_name = getattr(animation, "_target_attr", None)
    target = getattr(animation, target_name, None) if target_name else None
    if isinstance(target, g["Mobject"]):
        objects.append(target)
    scene = None
    for obj in objects:
        for member in obj.get_family():
            owner = getattr(member, "_scene", None)
            if owner is not None:
                if scene is not None and owner is not scene:
                    error = g.get("_ForeignStageError", ValueError)
                    raise error("persistent animation cannot reference multiple Scenes; copy its mobjects")
                scene = owner
    return scene


def _implementation(obj, name):
    value = inspect.getattr_static(obj, name, None)
    return value.__func__ if isinstance(value, (staticmethod, classmethod)) else getattr(value, "__func__", value)


_NATIVE_HOOKS = (
    "begin", "finish", "interpolate", "interpolate_mobject", "interpolate_submobject",
    "update_mobjects", "create_starting_mobject", "get_sub_alpha", "time_spanned_alpha",
    "get_all_mobjects", "get_all_families_zipped", "get_all_mobjects_to_update",
)


def _native_hooks_unchanged(animation, protocols):
    found = False
    for cls in type(animation).__mro__:
        baseline = protocols.get(cls)
        if baseline is None:
            continue
        if not found:
            found = True
            if any(_implementation(animation, name) is not value for name, value in baseline.items()):
                return False
        if any(_implementation(cls, name) is not value for name, value in baseline.items()):
            return False
    return found


class _PersistentAnimation:
    def __init__(self, g, animation, anchor, cycle, driver_factory=None):
        self.g, self.animation, self.anchor = g, animation, anchor
        self.cycle = bool(cycle)
        self.driver = animation
        self.driver_factory = driver_factory
        self.closed = self.busy = self.begun = False
        self.prior_suspension = []

        def update(current, dt):
            return self.step(current, dt)

        update._fmn_persistent_controller = self
        self.update = update

    def detach(self):
        self.anchor.remove_updater(self.update)

    def cancel(self, original=None):
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
        first = None
        for action in actions:
            try:
                action()
            except BaseException as error:
                if first is None:
                    first = error
                if original is not None:
                    _note(original, "animation updater cleanup failed: " + type(error).__name__)
        self.driver = None
        if first is not None and original is None:
            raise first

    def start(self):
        duration = _duration(self.animation)
        if self.cycle and duration == 0:
            raise ValueError("a cycling animation updater requires positive run_time")
        self.animation.suspend_mobject_updating = False
        self.animation.total_time = 0.0
        self.prior_suspension = [
            (member, member._is_updating_suspended())
            for member in self.anchor.get_family()
        ]
        try:
            if self.driver_factory is not None:
                self.driver = self.driver_factory()
            duration = _duration(self.driver)
            if self.cycle and duration == 0:
                raise ValueError("a cycling animation updater requires positive run_time")
            self.begun = True
            self.driver.begin()
            self.anchor.add_updater(self.update)
        except BaseException as error:
            self.cancel(error)
            raise
        return self.anchor

    def step(self, current, dt):
        # Starting/target copies share callables, but cannot drive the original.
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
                self.driver = None
                return
            following = elapsed + delta
            if not math.isfinite(following):
                raise ValueError("animation updater accumulated time is not finite")
            alpha = (elapsed / duration) % 1.0 if self.cycle else max(0.0, elapsed / duration)
            # The helper's pinned contract is interpolate THEN helper update.
            self.driver.interpolate(alpha)
            if self.closed:
                return
            self.driver.update_mobjects(delta)
            if not self.closed:
                self.animation.total_time = following
        except BaseException as error:
            self.cancel(error)
            raise
        finally:
            self.busy = False


def _install_removal(g):
    Mobject = g["Mobject"]
    previous_remove = Mobject.remove_updater

    @wraps(previous_remove)
    def remove(self, updater):
        owner = getattr(updater, "_fmn_persistent_controller", None)
        if isinstance(owner, _PersistentAnimation) and owner.anchor is self and not owner.closed:
            owner.cancel()
        return previous_remove(self, updater)

    Mobject.remove_updater = remove
    previous_clear = getattr(Mobject, "clear_updaters", None)
    if previous_clear is None:
        return

    @wraps(previous_clear)
    def clear(self, recurse=True):
        first = None
        for member in self.get_family() if recurse else [self]:
            for updater in tuple(member.updaters):
                owner = getattr(updater, "_fmn_persistent_controller", None)
                if isinstance(owner, _PersistentAnimation) and owner.anchor is member:
                    try:
                        owner.cancel()
                    except BaseException as error:
                        if first is None:
                            first = error
        result = previous_clear(self, recurse=recurse)
        if first is not None:
            raise first
        return result

    Mobject.clear_updaters = clear


def install_animation_updaters(native: Any) -> None:
    """Install helpers without replacing any public animation class."""
    g = vars(native)
    if g.get("_FMN_ANIMATION_UPDATERS_INSTALLED", False):
        return
    Animation, Mobject = g["Animation"], g["Mobject"]
    previous = {name: g[name] for name in ("turn_animation_into_updater", "cycle_animation")}
    protocols = {
        cls: {name: _implementation(cls, name) for name in _NATIVE_HOOKS}
        for cls in tuple(g.values()) if isinstance(cls, type) and issubclass(cls, Animation)
    }
    _install_removal(g)

    def turn_animation_into_updater(animation, cycle=False, **kwargs):
        if not isinstance(animation, Animation):
            raise TypeError("turn_animation_into_updater requires an Animation")
        current_anchor = getattr(animation, "mobject", None)
        if isinstance(current_anchor, Mobject):
            for updater in tuple(current_anchor.updaters):
                owner = getattr(updater, "_fmn_persistent_controller", None)
                if isinstance(owner, _PersistentAnimation) and not owner.closed and owner.animation is animation:
                    raise RuntimeError("animation already has a persistent updater")
        animation.update_rate_info(**kwargs)
        animation._ensure_runtime_defaults()
        if isinstance(animation, g["AnimationGroup"]):
            g["_fmn_ensure_composition_root"](animation)
        anchor = animation.mobject
        if not isinstance(anchor, Mobject):
            raise TypeError("animation updater must animate a Mobject")
        for updater in tuple(anchor.updaters):
            owner = getattr(updater, "_fmn_persistent_controller", None)
            if isinstance(owner, _PersistentAnimation) and not owner.closed and owner.animation is animation:
                raise RuntimeError("animation already has a persistent updater")
        factory = None
        if not _has_python_interpolation(g, animation):
            make_driver = g.get("_fmn_make_animation_driver")
            scene = _scene_for(g, animation, anchor)
            if make_driver is None or scene is None:
                raise NotImplementedError(type(animation).__name__ + " requires a scene-bound native animation driver")
            if not _native_hooks_unchanged(animation, protocols):
                raise NotImplementedError("native-only persistent animation has authored lifecycle hooks without a Python interpolator")
            if float(animation.final_alpha_value) != 1.0:
                raise NotImplementedError("native-only persistent animation requires its default final_alpha_value=1")
            factory = lambda: make_driver(scene, animation)
        return _PersistentAnimation(g, animation, anchor, cycle, factory).start()

    def cycle_animation(animation, **kwargs):
        return turn_animation_into_updater(animation, cycle=True, **kwargs)

    replacements = {"turn_animation_into_updater": turn_animation_into_updater, "cycle_animation": cycle_animation}
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
