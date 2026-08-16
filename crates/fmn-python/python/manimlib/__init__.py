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
