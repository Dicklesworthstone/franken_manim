"""FrankenManim's supported ``manimlib`` package surface.

The implementation lives in the private, CPython-versioned ``manimlib``
extension.  This initializer only re-exports that module's public schema;
geometry, animation, rendering, and compatibility semantics remain native or
in the bootstrap embedded in the extension.
"""

from fmn_python import (
    _ensure_exclusive_manimlib_namespace as _ensure_exclusive_manimlib_namespace,
)

_ensure_exclusive_manimlib_namespace()

from . import manimlib as _native


# The Rust API deliberately exposes ergonomic snake_case constructors, while
# the pinned Reference exports the corresponding CamelCase classes.  Treat a
# leak in either direction as an invalid wheel rather than silently widening
# or shrinking ``from manimlib import *``.
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
for _rust_helper, _reference_class in _REFERENCE_CLASS_BY_RUST_HELPER.items():
    if hasattr(_native, _rust_helper):
        raise ImportError(
            f"Rust-only constructor helper {_rust_helper!r} leaked into manimlib"
        )
    if not hasattr(_native, _reference_class):
        raise ImportError(
            f"Reference constructor class {_reference_class!r} is missing from manimlib"
        )
del _REFERENCE_CLASS_BY_RUST_HELPER, _rust_helper, _reference_class


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
