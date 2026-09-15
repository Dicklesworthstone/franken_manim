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
    if "CyclicReplace" in g:
        _install_cyclic_replace(g)
    # The wheel exports the complete table; smaller embedding tables may
    # install only their available playback families. Missing exports remain
    # missing and are still the independent parity auditor's responsibility.
    if "Homotopy" in g:
        from fmn_python.movement import install_movement

        install_movement(native)
    if "Rotating" in g:
        from fmn_python.rotation import install_rotation

        install_rotation(native)
    if "MaintainPositionRelativeTo" in g:
        from fmn_python.update_animations import install_update_animations

        install_update_animations(native)
    if "ValueTracker" in g:
        from fmn_python.trackers import install_tracker_interpolation

        install_tracker_interpolation(native)
    if "_RATE_FUNC_NAMES" in g and "AnimationGroup" in g:
        from fmn_python.live_rates import install_live_rates

        install_live_rates(native)
    if "turn_animation_into_updater" in g and "cycle_animation" in g:
        from fmn_python.animation_updaters import install_animation_updaters

        install_animation_updaters(native)
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


def _install_cyclic_replace(g: dict[str, Any]) -> None:
    """Cycle a real group through Transform, not a targetless spec.

    The group retains the original operands. Its destination is built at
    begin, so edits between construction and playback remain observable.
    Copy, placement, alignment and interpolation use the existing Mobject
    and Transform implementations; there is no second animation kernel.
    """
    Cyclic = g["CyclicReplace"]
    Transform, Mobject, Group = g["Transform"], g["Mobject"], g["Group"]
    Swap = g.get("Swap")

    def cyclic_init(self, *mobjects, path_arc=1.5707963267948966, **kwargs):
        if len(mobjects) < 2:
            raise ValueError(
                type(self).__name__ + " needs at least two mobjects to cycle; got "
                + str(len(mobjects))
            )
        if not all(isinstance(mobject, Mobject) for mobject in mobjects):
            raise TypeError(type(self).__name__ + " cycles Mobjects only")
        super(Cyclic, self).__init__(Group(*mobjects), path_arc=path_arc, **kwargs)

    def create_target(self):
        source = self.mobject
        target = source.copy()
        # Reference direction: operand i moves to operand i+1; the last
        # returns to the first. Iterate the copied children in that order
        # without ever moving a source operand while planning destinations.
        if len(target):
            for destination, original in zip([target[-1], *target[:-1]], source):
                destination.move_to(original)
        return target

    Cyclic._native_kind = "transform"
    Cyclic._target_attr = "target_mobject"
    Cyclic.__init__ = cyclic_init
    Cyclic.create_target = create_target
    Cyclic._native_target = Transform._native_target
    Cyclic._native_params = Transform._native_params
    if Swap is not None:
        Swap._native_kind = "transform"
        Swap._target_attr = "target_mobject"
    for name in ("__init__", "create_target"):
        function = vars(Cyclic)[name]
        function.__name__ = name
        function.__qualname__ = Cyclic.__qualname__ + "." + name
        function.__module__ = Cyclic.__module__

    # The shared installer captured its Transform hook baselines before
    # this targetless class acquired a real create_target. Capture the new
    # shipped protocol here, rather than misclassifying that implementation
    # itself as an authored hook on every ordinary Swap.
    hooks = (
        "begin", "finish", "interpolate", "interpolate_mobject",
        "interpolate_submobject", "create_target", "create_starting_mobject",
        "init_path_func", "check_target_mobject_validity", "get_all_mobjects",
        "get_all_families_zipped", "get_all_mobjects_to_update", "update_mobjects",
        "get_sub_alpha", "time_spanned_alpha", "clean_up_from_scene",
        "_ensure_runtime_defaults", "_native_target", "_native_params",
    )
    protocols = {
        cls: {name: getattr(cls, name, None) for name in hooks}
        for cls in (Cyclic, Swap) if cls is not None
    }
    previous_requires = g["_requires_python_animation"]

    def requires(animation):
        if not isinstance(animation, Cyclic):
            return previous_requires(animation)
        # Authored subclasses take the shared callback route, conservatively.
        # It calls their live hooks and handles nesting/updater conversion.
        baseline = protocols.get(type(animation))
        if baseline is None:
            return True
        for name, expected in baseline.items():
            method = getattr(animation, name, None)
            if getattr(method, "__func__", method) is not expected:
                return True
        path = getattr(animation, "path_func", None)
        rate = getattr(animation, "rate_func", None)
        catalog = g.get("_RATE_FUNC_NAMES", {})
        known_rate = rate is None or isinstance(rate, str) or any(
            rate is function for function in catalog
        )
        return (
            getattr(animation, "_native_kind", None) != "transform"
            or getattr(animation, "final_alpha_value", 1.0) != 1.0
            or bool(getattr(animation, "remover", False))
            or bool(getattr(animation, "replace_mobject_with_target_in_scene", False))
            or (path is not None and getattr(path, "_fmn_path_arc", None) is None)
            or not known_rate
        )

    g["_requires_python_animation"] = requires
