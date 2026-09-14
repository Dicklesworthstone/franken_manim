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
    g["_FMN_SCENE_PLAYBACK_INSTALLED"] = True
