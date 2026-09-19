"""The one production initializer for embedded and installed portal modules.

The native extension supplies the object model and shared animation protocol.
All entry points install these same adapters, in the same order, before exposing
that module. The pure-Python package is part of the portal, not an optional set
of monkey patches whose omission silently changes a scene's meaning.
"""
from __future__ import annotations

from importlib import import_module
from types import ModuleType


# Resolve the entire installation plan before mutating any class. In particular,
# an incomplete wheel must fail closed, not expose a partly initialized engine.
_STEPS = (
    ("rendering", "install_scene_rendering"),
    ("camera_capture", "install_camera_capture"),
    ("surface_textures", "install_surface_textures"),
    ("live_tex", "install_live_tex"),
    ("matrix", "install_matrix"),
    ("text_reveal", "install_text_reveal"),
    ("subset_reveal", "install_subset_reveal"),
    ("fading", "install_fading"),
    ("playback", "install_scene_playback"),
    ("scene_state", "install_scene_state"),
    ("streamline_animation", "install_streamline_animation"),
    ("traced_path", "install_traced_path"),
    ("interactive_editing", "install_interactive_editing"),
    ("interaction", "install_interaction"),
    ("control_events", "install_control_events"),
    ("color_sliders", "install_color_sliders"),
    ("scene_execution", "install_scene_execution"),
    ("embedded_shell", "install_embedded_shell"),
)
_STATE = "_FMN_PORTAL_RUNTIME_STATE"


def initialize(native: ModuleType) -> ModuleType:
    """Initialize one actual native module exactly once, or refuse publication.

    State belongs to the native module, never to this reusable Python module.
    Independent embedded interpreters/workers must not share installed classes,
    and repeated package imports must not stack wrappers or replace identities.
    """
    namespace = vars(native)
    state = namespace.get(_STATE)
    if state == "ready":
        return native
    if state is not None:
        raise ImportError(f"FrankenManim portal runtime initialization is {state}")
    if not namespace.get("_FMN_ANIMATION_SEMANTICS_INSTALLED", False):
        raise ImportError("FrankenManim shared animation semantics are not initialized")
    namespace[_STATE] = "initializing"
    try:
        from fmn_python import _ensure_exclusive_manimlib_namespace
        from fmn_python.schema_provenance import (
            SchemaProvenanceError, apply_schema_placeholder_provenance,
        )

        _ensure_exclusive_manimlib_namespace()
        installers = []
        for module_name, attribute in _STEPS:
            module = import_module("fmn_python." + module_name)
            installer = getattr(module, attribute, None)
            if not callable(installer):
                raise ImportError(f"missing portal runtime installer: {module_name}.{attribute}")
            installers.append(installer)
        try:
            apply_schema_placeholder_provenance(native)
        except SchemaProvenanceError as error:
            raise ImportError(f"invalid manimlib schema provenance: {error}") from error
        for installer in installers:
            installer(native)
    except BaseException:
        # Do not retain an exception/traceback cycle owning native proxies. A
        # failed instance must be discarded, never retried as a success-shaped
        # mixture of two initialization attempts.
        namespace[_STATE] = "failed"
        raise
    namespace[_STATE] = "ready"
    return native
