"""Evaluate authored animation easing on the callback clock.

Native catalog curves stay native. Authored callables on supported protocols
must not be replaced by a fixed-30-Hz table, especially with lag/time windows
or a caller-selected frame rate. No sampling or frame clock is defined here.
"""
from __future__ import annotations

from functools import wraps
from typing import Any


def install_live_rates(native: Any) -> None:
    """Route supported authored rates through existing Python lifecycles."""
    g = vars(native)
    if g.get("_FMN_LIVE_RATES_INSTALLED", False):
        return
    Animation, Transform, Group = g["Animation"], g["Transform"], g["AnimationGroup"]
    Builder, Scene = g["_AnimationBuilder"], g["Scene"]
    previous_requires, previous_play = g["_requires_python_animation"], Scene.play
    previous_defaults = Transform._ensure_runtime_defaults

    def update_rate_info(self, run_time=None, rate_func=None, lag_ratio=None):
        # BN-12: None is the only absent option. In particular, zero duration
        # and zero family lag must mean the same thing on Choreo's native and
        # callback paths. Never ask an authored rate object for its truth value.
        if run_time is not None:
            self.run_time = run_time
        if rate_func is not None:
            self.rate_func = rate_func
        if lag_ratio is not None:
            self.lag_ratio = lag_ratio
        return self

    def get_run_time(self):
        # Native leaves retain None until lowering selects their constructor
        # defaults. Composition must nevertheless be able to query duration
        # before begin, especially when time_span widens that default. Do not
        # call _ensure_runtime_defaults here: an observation must not execute
        # authored setup, construct a target, or consume the native sentinel.
        duration = self.run_time
        if duration is None:
            duration = self.max_end_time if isinstance(self, Group) else 1.0
        if self.time_span is not None:
            duration = max(duration, float(self.time_span[1]))
        return duration

    def custom(rate):
        # Identity lookup also accepts callable objects with __hash__ = None;
        # checking a rate must never evaluate it or invoke its equality hook.
        return callable(rate) and all(rate is not known for known in g["_RATE_FUNC_NAMES"])

    def catalog(rate):
        if not isinstance(rate, str):
            return rate
        for function, name in g["_RATE_FUNC_NAMES"].items():
            if name == rate:
                return function
        raise ValueError("unknown rate function: " + rate)

    def supported(animation):
        if not isinstance(animation, Animation):
            return False
        if not getattr(animation, "_native_kind", None):
            return True
        if isinstance(animation, Transform) and animation._target_attr is not None:
            return True
        # Specialized group kinds (for example ShowCreationThenFadeOut)
        # already use the same complete composition driver. Their native
        # lowering tag is not a statement about callback capability.
        if isinstance(animation, Group):
            return True
        # These families have complete Python lifecycle implementations over
        # native geometry. Their stock catalog rates still take the native
        # path; only authored easing needs the callback clock. In particular,
        # ShowPartial includes creation, uncreation and passing flashes, and
        # DrawBorderThenFill includes Write. Treating these as unsupported
        # silently sampled even a mixed Transform/Write play at 30 Hz.
        for name in ("Rotating", "MoveAlongPath", "ShowPartial", "DrawBorderThenFill", "VFadeIn"):
            cls = g.get(name)
            if cls is not None and isinstance(animation, cls):
                return True
        # Adapters installed before this one already know whether an authored
        # or specialized animation has a callback lifecycle. Honor that
        # decision instead of reverting its play-level rate to a native table.
        return previous_requires(animation)

    def requires(animation):
        if supported(animation) and custom(getattr(animation, "rate_func", None)):
            return True
        return previous_requires(animation)

    @wraps(previous_defaults)
    def defaults(self):
        previous_defaults(self)
        # A catalog spelling can reach a callback because of an authored
        # path or lifecycle hook. The callback needs the catalog callable;
        # the native route still receives the original catalog payload.
        self.rate_func = catalog(self.rate_func)

    @wraps(previous_play)
    def play(self, *proto_animations, **kwargs):
        rate = kwargs.get("rate_func")
        if isinstance(rate, str):
            kwargs = dict(kwargs, rate_func=catalog(rate))
            rate = kwargs["rate_func"]
        animations = []
        for proto in proto_animations:
            animation = g["prepare_animation"](proto) if isinstance(proto, Builder) else proto
            if isinstance(proto, Builder) and not isinstance(animation, Animation):
                raise TypeError("AnimationBuilder.build must return an Animation")
            animations.append(animation)
        if custom(rate) and animations and all(supported(animation) for animation in animations):
            # The Reference applies play-level options to the top-level
            # animations. A group's curve acts on its timeline, not on each
            # child a second time. Once all top-level animations can execute
            # callbacks, no native global lookup table is needed at all.
            for animation in animations:
                animation.rate_func = rate
            kwargs = dict(kwargs, rate_func=None)
        # A mixed play with an unsupported native kind keeps its old global
        # lowering. Do not remove the native sibling's easing override or
        # reinterpret a specialized animation as a generic Transform.
        # Older composition wrappers close over their original classifier.
        # Prepare newly callback-selected groups here with the same root and
        # context helpers, including nested groups and builder-returned groups.
        groups, seen, visiting = [], set(), set()
        stack = [(animation, False) for animation in reversed(animations)]
        while stack:
            animation, leaving = stack.pop()
            if not isinstance(animation, Group):
                continue
            marker = id(animation)
            if leaving:
                visiting.remove(marker)
                seen.add(marker)
                if supported(animation) and custom(animation.rate_func):
                    groups.append(animation)
                continue
            if marker in visiting:
                raise ValueError("Animation composition contains a cycle")
            if marker in seen:
                continue
            visiting.add(marker)
            stack.append((animation, True))
            stack.extend((child, False) for child in reversed(animation.animations))
        absent, bound = object(), []
        try:
            for group in groups:
                g["_fmn_ensure_composition_root"](group)
                bound.append((group, group.__dict__.get("_composition_scene", absent)))
                group._composition_scene = self
            return previous_play(self, *animations, **kwargs)
        except BaseException:
            for group, _ in reversed(bound):
                try:
                    group.abort()
                except BaseException:
                    pass
            raise
        finally:
            for group, prior in reversed(bound):
                if prior is absent:
                    group.__dict__.pop("_composition_scene", None)
                else:
                    group._composition_scene = prior

    for name, function in (("update_rate_info", update_rate_info),
                           ("get_run_time", get_run_time)):
        function.__name__ = name
        function.__qualname__ = Animation.__qualname__ + "." + name
        function.__module__ = Animation.__module__
        setattr(Animation, name, function)
    Transform._ensure_runtime_defaults = defaults
    g["_requires_python_animation"] = requires
    Scene.play = play
    g["_FMN_LIVE_RATES_INSTALLED"] = True
