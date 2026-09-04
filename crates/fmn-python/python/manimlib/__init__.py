"""FrankenManim's supported ``manimlib`` package surface.

The implementation lives in the private, CPython-versioned ``manimlib``
extension.  This initializer re-exports that module's public schema and applies
small Python semantic continuations directly to its existing class objects;
geometry, rendering, and engine state remain native.
"""

import sys as _sys

from fmn_python import (
    _ensure_exclusive_manimlib_namespace as _ensure_exclusive_manimlib_namespace,
)
from fmn_python.library_constructor_authority import (
    REFERENCE_CLASS_BY_RUST_HELPER as _CONTRACT_REFERENCE_CLASS_BY_RUST_HELPER,
    REFERENCE_MODULE_BY_RUST_HELPER as _CONTRACT_REFERENCE_MODULE_BY_RUST_HELPER,
)
from fmn_python.schema_provenance import (
    SchemaProvenanceError as _SchemaProvenanceError,
    apply_schema_placeholder_provenance as _apply_schema_placeholder_provenance,
)

_ensure_exclusive_manimlib_namespace()

from . import manimlib as _native
from ._animation_semantics import install as _install_animation_semantics

_install_animation_semantics(_native)
del _install_animation_semantics

_SEMANTIC_METHODS = {
    _native.Mobject: ("__str__", "interpolate"),
    _native.Animation: (
        "__init__",
        "_validate_input_type",
        "_ensure_runtime_defaults",
        "__str__",
        "begin",
        "finish",
        "create_starting_mobject",
        "get_all_mobjects",
        "get_all_families_zipped",
        "get_all_mobjects_to_update",
        "update_mobjects",
        "copy",
        "update_rate_info",
        "interpolate",
        "update",
        "time_spanned_alpha",
        "interpolate_mobject",
        "interpolate_submobject",
        "get_sub_alpha",
        "set_run_time",
        "get_run_time",
        "set_rate_func",
        "get_rate_func",
        "set_name",
        "is_remover",
        "clean_up_from_scene",
    ),
    _native._NativeAnimation: ("__init__",),
    _native.Transform: (
        "init_path_func",
        "create_target",
        "check_target_mobject_validity",
        "_native_target",
        "begin",
        "finish",
        "clean_up_from_scene",
        "get_all_mobjects",
        "get_all_families_zipped",
        "interpolate_submobject",
    ),
    _native.TransformFromCopy: ("__init__",),
}
for _semantic_class, _semantic_names in _SEMANTIC_METHODS.items():
    for _semantic_name in _semantic_names:
        _semantic_function = vars(_semantic_class)[_semantic_name]
        _semantic_function.__name__ = _semantic_name
        _semantic_function.__qualname__ = (
            f"{_semantic_class.__qualname__}.{_semantic_name}"
        )
        _semantic_function.__module__ = _semantic_class.__module__
del _SEMANTIC_METHODS, _semantic_class, _semantic_names
del _semantic_name, _semantic_function

try:
    _apply_schema_placeholder_provenance(_native)
except _SchemaProvenanceError as error:
    raise ImportError(f"invalid manimlib schema provenance: {error}") from error
del _SchemaProvenanceError, _apply_schema_placeholder_provenance


# The Rust API deliberately exposes ergonomic snake_case constructors, while
# the pinned Reference exports the corresponding CamelCase classes.  Treat a
# leak, missing class, or qualified-module identity split as an invalid wheel
# rather than silently widening, shrinking, or forking the compatibility API.
_REFERENCE_CLASS_BY_RUST_HELPER = {
    "group": "Group",
    "v_group": "VGroup",
    "vectorized_point": "VectorizedPoint",
    "small_dot": "SmallDot",
    "sector": "Sector",
    "vector": "Vector",
    "polyline": "Polyline",
    "triangle": "Triangle",
    "rounded_rectangle": "RoundedRectangle",
    "svg_mobject": "SVGMobject",
    "tangent_line": "TangentLine",
    "curved_arrow": "CurvedArrow",
    "curved_double_arrow": "CurvedDoubleArrow",
    "curves_as_submobjects": "CurvesAsSubmobjects",
    "dashed_vmobject": "DashedVMobject",
    "v_highlight": "VHighlight",
}
if _REFERENCE_CLASS_BY_RUST_HELPER != _CONTRACT_REFERENCE_CLASS_BY_RUST_HELPER:
    raise ImportError(
        "manimlib constructor alias guard drifted from the authority manifest"
    )
if set(_REFERENCE_CLASS_BY_RUST_HELPER) != set(
    _CONTRACT_REFERENCE_MODULE_BY_RUST_HELPER
):
    raise ImportError(
        "manimlib constructor module guard drifted from the authority manifest"
    )
for _rust_helper, _reference_class in _REFERENCE_CLASS_BY_RUST_HELPER.items():
    if hasattr(_native, _rust_helper):
        raise ImportError(
            f"Rust-only constructor helper {_rust_helper!r} leaked into manimlib"
        )
    if not hasattr(_native, _reference_class):
        raise ImportError(
            f"Reference constructor class {_reference_class!r} is missing from manimlib"
        )
    _reference_module = _CONTRACT_REFERENCE_MODULE_BY_RUST_HELPER[_rust_helper]
    _module = _sys.modules.get(_reference_module)
    if _module is None:
        raise ImportError(
            f"Reference constructor module {_reference_module!r} is missing from manimlib"
        )
    _reference_value = getattr(_native, _reference_class)
    if vars(_module).get(_reference_class) is not _reference_value:
        raise ImportError(
            f"{_reference_module}.{_reference_class} is not the root "
            "constructor identity"
        )
del _REFERENCE_CLASS_BY_RUST_HELPER, _rust_helper, _reference_class, _reference_module
del _reference_value, _module
del _CONTRACT_REFERENCE_CLASS_BY_RUST_HELPER
del _CONTRACT_REFERENCE_MODULE_BY_RUST_HELPER, _sys


for _name in dir(_native):
    if not _name.startswith("_"):
        globals()[_name] = getattr(_native, _name)

__version__ = _native.__version__
__distribution__ = _native.__distribution__
__franken_manim__ = _native.__franken_manim__
__abi_policy__ = _native.__abi_policy__
__engine__ = _native.__engine__
__thread_policy__ = _native.__thread_policy__
__reference_commit__ = _native.__reference_commit__


def __getattr__(name):
    """Forward private diagnostic seams without widening wildcard imports."""

    return getattr(_native, name)


def __dir__():
    return sorted(set(globals()) | set(dir(_native)))


# Importing the native member assembles ``manimlib.*`` compatibility modules.
# CPython attaches each child module to this parent package as a public
# attribute (``manimlib.scene``, ``manimlib.mobject``, ...), but the pinned
# Reference root schema exports none of those package handles.  Keep the wheel
# package's wildcard surface exactly equal to the native schema rather than
# leaking import-system bookkeeping.
for _name in list(globals()):
    if not _name.startswith("_") and not hasattr(_native, _name):
        del globals()[_name]


# Deliberately no __all__: the pinned Reference exports every non-underscore
# root name under `from manimlib import *`.
