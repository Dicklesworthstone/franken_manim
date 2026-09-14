"""Wheel playback normalization over the existing Choreo execution boundary.

Builders produce real Animation objects before any native specification is
made. This module owns no interpolation, geometry, renderer, or frame clock.
"""

from __future__ import annotations

from functools import wraps
from typing import Any


def install_scene_playback(native: Any) -> None:
    """Keep authored builder products and Transform endpoint options live.

    The wheel calls this after the shared native animation initialization.
    Direct ExtensionFileLoader consumers may install it on their native module
    explicitly. Published class objects and qualified aliases stay identical.
    """
    g = vars(native)
    if g.get("_FMN_SCENE_PLAYBACK_INSTALLED", False):
        return
    _install_restore_playback(g)
    Scene, Animation = g["Scene"], g["Animation"]
    Builder, Transform = g["_AnimationBuilder"], g["Transform"]
    original_play = Scene.play
    original_requires = g["_requires_python_animation"]

    def requires_python_animation(animation):
        # Choreo's ordinary Transform lowering currently has no general
        # final-alpha/remover parameter application. Its shared Python
        # Transform protocol implements both, including replacement cleanup.
        # Specialized native kinds (notably FadeOut's final_alpha=0) have
        # their own defaults and must not be reclassified by this rule.
        if (isinstance(animation, Transform)
                and getattr(animation, "_native_kind", None)
                in {"transform", "replacement_transform", "transform_from_copy"}
                and (getattr(animation, "final_alpha_value", 1.0) != 1.0
                     or bool(getattr(animation, "remover", False)))):
            return True
        return original_requires(animation)

    @wraps(original_play)
    def play(self, *proto_animations, run_time=None, rate_func=None, lag_ratio=None):
        # CPython evaluates all .animate chains before this call. Build in
        # argument order, at the same dynamic-target lookup point as
        # prepare_animation, and never reconstruct an authored product.
        animations = []
        for proto in proto_animations:
            if isinstance(proto, Builder):
                animation = g["prepare_animation"](proto)
                if not isinstance(animation, Animation):
                    raise TypeError("AnimationBuilder.build must return an Animation")
                animations.append(animation)
            else:
                animations.append(proto)
        return original_play(self, *animations, run_time=run_time,
                             rate_func=rate_func, lag_ratio=lag_ratio)

    g["_requires_python_animation"] = requires_python_animation
    Scene.play = play
    # The wheel exports the complete table; smaller embedding tables may
    # install only their available playback families. Missing exports remain
    # missing and are still the independent parity auditor's responsibility.
    if "Homotopy" in g:
        from fmn_python.movement import install_movement

        install_movement(native)
    g["_FMN_SCENE_PLAYBACK_INSTALLED"] = True


def _install_restore_playback(g: dict[str, Any]) -> None:
    """Restore the public Transform lineage and constructor-selected target."""
    Restore, Transform, Mobject = g["Restore"], g["Transform"], g["Mobject"]

    def restore_init(self, mobject, path_arc=0.0, path_arc_axis=g["_OUT"],
                     path_func=None, **kwargs):
        saved = getattr(mobject, "saved_state", None)
        if saved is None:
            raise Exception("Trying to restore without having saved")
        if not isinstance(saved, Mobject):
            raise TypeError("Restore saved_state must be a Mobject")
        # Capture the saved object by reference at construction, as the
        # Reference does. Rebinding mobject.saved_state later must not change
        # this animation's destination; edits to the saved object remain live.
        super(Restore, self).__init__(
            mobject, saved, path_arc=path_arc, path_arc_axis=path_arc_axis,
            path_func=path_func, **kwargs,
        )

    # Keep every qualified import and existing subclass attached to the same
    # public class object. Native Transform owns ordinary restoration; the
    # shared Transform protocol owns authored paths, hooks and camera poses.
    # Neither route performs the legacy play-time saved_state relinking.
    Restore.__bases__ = (Transform,)
    Restore._native_kind = "transform"
    restore_init.__name__ = "__init__"
    restore_init.__qualname__ = Restore.__qualname__ + ".__init__"
    restore_init.__module__ = Restore.__module__
    Restore.__init__ = restore_init
