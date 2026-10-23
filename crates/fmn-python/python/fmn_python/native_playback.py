"""Conservative camera admission for native, independently sampled plays.

The owner thread still runs every authored camera/accessor callback through the
ordinary stepped driver. This predicate only inspects identities and plain
instance state; asking whether a callback exists must never execute it.
"""
from __future__ import annotations

from inspect import getattr_static
from types import GetSetDescriptorType

from .updater_dispatch import _class_dict, _class_mro, _generic_getattr, _static_is

_MISSING = object()


def _plain_state(value):
    cls = type(value)
    if type(cls) is not type or not _generic_getattr(cls):
        return None
    # A shadowed __dict__ can itself run authored code even when the ordinary
    # __getattribute__ implementation is inherited unchanged.
    for base in _class_mro(cls):
        descriptor = _class_dict(base).get("__dict__", _MISSING)
        if descriptor is not _MISSING:
            if (type(descriptor) is not GetSetDescriptorType
                    or descriptor.__name__ != "__dict__"
                    or descriptor.__objclass__ is not base):
                return None
            break
    try:
        state = object.__getattribute__(value, "__dict__")
    except AttributeError:
        return None
    return state if type(state) is dict else None


def install_native_playback(native):
    g = vars(native)
    if g.get("_FMN_NATIVE_PLAYBACK_INSTALLED", False):
        return
    Scene, Camera, Frame, Point = (g[name] for name in
                                  ("Scene", "Camera", "CameraFrame", "Point"))
    light_hooks = ("get_center", "get_bounding_box", "compute_bounding_box",
                   "get_family", "_bbox_rows", "_is_bound", "_get_bbox",
                   "_get_installed_bbox", "_has_points")
    baselines, light_methods = {}, {}
    scene_camera = None
    numeric_types = (int, float, *(getattr(g["_np"], name) for name in
                      ("float16", "float32", "float64", "int8", "uint8", "int16",
                       "uint16", "int32", "uint32", "int64", "uint64")))

    def finalize():
        nonlocal scene_camera
        scene_camera = getattr_static(Scene, "camera")
        # Exact stock capture objects keep the proof small. Snapshot the
        # completed runtime, so later class monkey-patches also demote instead
        # of silently becoming a newly trusted baseline on the first play.
        baselines.clear()
        for cls in (Camera, Frame, Point, g["_LiveSubmobjects"]):
            for base in _class_mro(cls):
                baselines.setdefault(base, dict(_class_dict(base)))
        light_methods.update({name: getattr_static(Point, name) for name in light_hooks})

    def classes_unchanged():
        for cls, expected in baselines.items():
            actual = _class_dict(cls)
            if (len(actual) != len(expected)
                    or any(actual.get(name, _MISSING) is not value
                           for name, value in expected.items())):
                return False
        return True

    def frozen_light(scene, members=()):
        """Return a stock light proxy, or None to retain per-frame callbacks.

        RenderSession already initializes the capture Camera. A private caller
        with a still-lazy camera simply retains its existing stepped behavior.
        No authored descriptors, numeric conversions or family walkers run.
        Native admission separately checks this light's storage/view lifetime.
        """
        own = _plain_state(scene)
        if (own is None or not _static_is(scene, "camera", scene_camera)
                or not classes_unchanged()):
            return None
        for member in members:
            state = _plain_state(member)
            if state is None:
                return None
            updaters = state.get("updaters")
            # Empty custom containers and descriptors can still observe each
            # frame when the stepped updater pass reads/iterates them.
            if (type(updaters) is not list or len(updaters) != 0
                    or not _static_is(member, "updaters", updaters)):
                return None
        camera = own.get("_camera")
        if type(camera) is not Camera:
            return None
        camera_state = _plain_state(camera)
        if camera_state is None:
            return None
        frame = camera_state.get("frame")
        rgba = camera_state.get("background_rgba")
        light = camera_state.get("light_source")
        if (type(frame) is not Frame or type(light) is not Point
                or any(not _static_is(camera, name, value) for name, value in
                       (("frame", frame), ("background_rgba", rgba), ("light_source", light)))):
            return None
        frame_state, light_state = _plain_state(frame), _plain_state(light)
        if frame_state is None or light_state is None:
            return None
        core = frame_state.get("_core", _MISSING)
        if core is _MISSING or not _static_is(frame, "_core", core):
            return None
        # Custom __float__/iteration implementations remain frame observers.
        # Stock Camera owns a list of ordinary numeric values here.
        if (type(rgba) not in (list, tuple) or len(rgba) != 4
                or any(type(value) not in numeric_types for value in rgba)):
            return None
        if any(not _static_is(light, name, method)
               for name, method in light_methods.items()):
            return None
        children = light_state.get("submobjects")
        if (type(children) not in (list, g["_LiveSubmobjects"])
                or list.__len__(children) != 0
                or not _static_is(light, "submobjects", children)):
            return None
        # A custom iterator attached to the live list also changes get_family.
        if type(children) is not list:
            # This exact shipping list subclass inherits list's C attribute
            # lookup, not object's slot. Its class identity/map was checked
            # above; read its ordinary dictionary without virtual dispatch.
            state = object.__getattribute__(children, "__dict__")
            if "__iter__" in state or "__len__" in state:
                return None
        return light

    g["_fmn_native_playback_light"] = frozen_light
    g["_fmn_finalize_native_playback"] = finalize
    g["_FMN_NATIVE_PLAYBACK_INSTALLED"] = True
