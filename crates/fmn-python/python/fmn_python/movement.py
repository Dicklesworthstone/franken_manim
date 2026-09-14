"""Functional movement on the shared Animation lifecycle and native geometry.

The host executes authored maps; Mobject owns geometry and Choreo owns time.
Installation preserves public classes and qualified aliases. No point sampler,
integrator, frame loop, renderer, or dependency is introduced here.
"""
from __future__ import annotations

from functools import wraps
from typing import Any


def _method(cls, name, function):
    function.__name__ = name
    function.__qualname__ = cls.__qualname__ + "." + name
    function.__module__ = cls.__module__
    setattr(cls, name, function)


def _callback_rate(g, rate):
    if isinstance(rate, str):
        name = rate
        rate = next((function for function, label in g["_RATE_FUNC_NAMES"].items()
                     if label == name), None)
        if rate is None:
            raise ValueError("unknown rate function: " + name)
    if not callable(rate):
        raise TypeError("rate_func must be a callable or a catalog name")
    return rate


def _cancel_preserving(animation, error):
    try:
        animation.abort()
    except BaseException as cleanup_error:
        error.add_note(f"movement cleanup also failed: {cleanup_error}")


def _install_lifecycle(g, cls, prepare):
    """Add failure ownership around the existing cooperative base protocol."""
    def release(self, *, update=False):
        prior = getattr(self, "_movement_prior", ())
        self._movement_prior = ()
        acquired = getattr(self, "_movement_suspends", False)
        self.mobject_was_updating = False
        first = None
        try:
            self.mobject.set_animating_status(False)
        except BaseException as error:
            first = error
        if acquired:
            for member, was_suspended in prior:
                if was_suspended:
                    continue
                try:
                    if member._is_updating_suspended():
                        member.resume_updating(recurse=False, call_updater=False)
                except BaseException as error:
                    if first is None:
                        first = error
        if first is not None:
            raise first
        # Successful Animation.finish historically resumes with update(0).
        # Restore flags first so children suspended before this animation stay
        # pruned. Error unwinding deliberately never invokes another updater.
        if update and acquired and prior and not prior[0][1]:
            self.mobject.update(0.0)

    def abort(self):
        if not getattr(self, "_movement_active", False):
            return
        self._movement_active = False
        release(self)

    def defaults(self):
        super(cls, self)._ensure_runtime_defaults()
        self.rate_func = _callback_rate(g, self.rate_func)

    def begin(self):
        if getattr(self, "_movement_active", False):
            self.abort()
        self._ensure_runtime_defaults()
        prepare(self)
        prior, seen = [], set()
        for member in self.mobject.get_family():
            if id(member) not in seen:
                seen.add(id(member))
                prior.append((member, member._is_updating_suspended()))
        self._movement_prior = prior
        self._movement_suspends = bool(self.suspend_mobject_updating)
        self._movement_active, self._movement_finished = True, False
        self._movement_cleaned = False
        try:
            super(cls, self).begin()
        except BaseException as error:
            _cancel_preserving(self, error)
            raise

    def finish(self):
        if not getattr(self, "_movement_active", False):
            if getattr(self, "_movement_finished", False):
                return
            raise RuntimeError(type(self).__name__ + " must begin before finish")
        # Shared finish performs final-alpha interpolation and status updates;
        # member-wise resumption below replaces only its recursive resume.
        self.mobject_was_updating = False
        try:
            super(cls, self).finish()
            release(self, update=True)
        except BaseException as error:
            _cancel_preserving(self, error)
            raise
        self._movement_active, self._movement_finished = False, True

    def interpolate(self, alpha):
        try:
            return super(cls, self).interpolate(alpha)
        except BaseException as error:
            _cancel_preserving(self, error)
            raise

    def update_mobjects(self, dt):
        try:
            return super(cls, self).update_mobjects(dt)
        except BaseException as error:
            _cancel_preserving(self, error)
            raise

    def cleanup(self, scene):
        if not getattr(self, "_movement_finished", False) or self._movement_cleaned:
            return
        super(cls, self).clean_up_from_scene(scene)
        self._movement_cleaned = True

    for name, function in {
        "begin": begin, "finish": finish, "abort": abort,
        "interpolate": interpolate, "update_mobjects": update_mobjects,
        "clean_up_from_scene": cleanup, "_ensure_runtime_defaults": defaults,
    }.items():
        _method(cls, name, function)


def install_movement(native: Any) -> None:
    """Install available movement classes without replacing their identities.

    A reduced embedding namespace with no movement classes is unchanged; this
    does not manufacture missing exports or alter the independent parity audit.
    """
    g = vars(native)
    if g.get("_FMN_MOVEMENT_INSTALLED", False):
        return
    Homotopy = g.get("Homotopy")
    if Homotopy is None:
        return
    Animation, Scene = g["Animation"], g["Scene"]
    if not isinstance(Homotopy, type) or not issubclass(Homotopy, Animation):
        raise TypeError("Homotopy must be an Animation class")

    def prepare_homotopy(self):
        if not callable(self.homotopy):
            raise TypeError("Homotopy requires a callable (x, y, z, t) map")

    def interpolate_family(self, alpha):
        # Restore the shared get_all_families_zipped/get_sub_alpha pipeline.
        # The previous bootstrap skipped point-free roots, starting-copy hooks
        # and family lag, and never acquired updater suspension in begin.
        return super(Homotopy, self).interpolate_mobject(alpha)

    _install_lifecycle(g, Homotopy, prepare_homotopy)
    _method(Homotopy, "interpolate_mobject", interpolate_family)
    previous_play = Scene.play

    @wraps(previous_play)
    def play(self, *proto_animations, **kwargs):
        Builder = g.get("_AnimationBuilder")
        animations = []
        for proto in proto_animations:
            if Builder is not None and isinstance(proto, Builder):
                proto = g["prepare_animation"](proto)
                if not isinstance(proto, Animation):
                    raise TypeError("AnimationBuilder.build must return an Animation")
            animations.append(proto)
        movements, seen, visiting = [], set(), set()
        stack = [(animation, False) for animation in reversed(animations)]
        while stack:
            animation, exiting = stack.pop()
            marker = id(animation)
            if exiting:
                visiting.remove(marker)
                continue
            if marker in visiting:
                raise ValueError("Animation composition contains a cycle")
            if marker in seen:
                continue
            seen.add(marker)
            visiting.add(marker)
            stack.append((animation, True))
            if isinstance(animation, Homotopy):
                movements.append(animation)
            if isinstance(animation, g["AnimationGroup"]):
                stack.extend((child, False) for child in reversed(animation.animations))
        try:
            return previous_play(self, *animations, **kwargs)
        except BaseException as error:
            # Authored overrides can fail outside super(), or a sibling/scene
            # updater can fail after this animation's callbacks have returned.
            for animation in reversed(movements):
                _cancel_preserving(animation, error)
            raise

    Scene.play = play
    g["_FMN_MOVEMENT_INSTALLED"] = True
