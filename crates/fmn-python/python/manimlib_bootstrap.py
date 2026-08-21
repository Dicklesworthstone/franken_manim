"""The pure-Python skin over FrankenManim's narrow PyO3 engine seam.

This file is embedded in the extension module.  It deliberately contains the
ordinary Python object-model pieces which CPython implements better than an FFI
type can: cooperative ``__init__``, mutable live containers, copy/deepcopy and
pickle state, and schema-driven module/class construction.
"""

from __future__ import annotations

import abc as _abc
import ast as _ast
import collections.abc as _collections_abc
import copy as _copy
import difflib as _difflib
import enum as _enum
import importlib as _importlib
import inspect as _inspect
import itertools as _itertools
import math as _math
import operator as _operator
import pathlib as _pathlib
import re as _re
import sys as _sys
import textwrap as _textwrap
import types as _types
import weakref as _weakref

import numpy as _np

# The module seam handle `_FMN_MODULE` is removed from the module dict once
# the bootstrap finishes (execute_bootstrap's temporary self-reference), so
# anything that must resolve the root module at RUNTIME — lazy class lookups
# in methods — goes through this persistent alias instead.
_FMN_ROOT = _FMN_MODULE

# Reference default values used by the positional surface's signatures.
# These are the same objects the schema constant pass later publishes under
# their public names (RIGHT, ORIGIN, ...); they exist here privately because
# class-body default expressions evaluate before that pass runs.
_ORIGIN = _np.array([0.0, 0.0, 0.0])
_RIGHT = _np.array([1.0, 0.0, 0.0])
_LEFT = _np.array([-1.0, 0.0, 0.0])
_UP = _np.array([0.0, 1.0, 0.0])
_DOWN = _np.array([0.0, -1.0, 0.0])
_OUT = _np.array([0.0, 0.0, 1.0])
_IN = _np.array([0.0, 0.0, -1.0])
_DL = _LEFT + _DOWN
_ONES = _np.array([1.0, 1.0, 1.0])
# manimlib/default_config.yml `sizes`: the Reference's default buffers.
_DEFAULT_MOBJECT_TO_EDGE_BUFF = 0.5
_DEFAULT_MOBJECT_TO_MOBJECT_BUFF = 0.25
_SMALL_BUFF = 0.1
_MED_SMALL_BUFF = 0.25
_MED_LARGE_BUFF = 0.5
_BLACK = "#000000"
_WHITE = "#FFFFFF"
_GREY_A = "#DDDDDD"
_GREY_B = "#BBBBBB"
_GREY_C = "#888888"
_GREY_D = "#444444"
_GREY_E = "#222222"
_GREEN = "#83C167"
_RED = "#FC6255"
_YELLOW = "#FFFF00"
_BLUE = "#58C4DD"
_BLUE_D = "#29ABCA"
_BLUE_E = "#1C758A"
_DEFAULT_LIGHT_COLOR = "#BBBBBB"
_ASPECT_RATIO = 16.0 / 9.0
_FRAME_HEIGHT = 8.0

# Function object -> catalog name, filled by _install_rate_functions;
# Scene.play maps rate_func callables into the engine's named catalog.
_RATE_FUNC_NAMES = {}


def _linear_rate(t):
    return t


def _smooth_rate(t):
    s = 1 - t
    return (t**3) * (10 * s * s + 5 * s * t + t * t)


def _there_and_back_rate(t):
    new_t = 2 * t if t < 0.5 else 2 * (1 - t)
    return _smooth_rate(new_t)


_there_and_back_rate.__name__ = "there_and_back"
_there_and_back_rate.__qualname__ = "there_and_back"

# The Reference leaks its OpenGL/window implementation modules into ordinary
# Python namespaces.  Those names remain part of the compatibility surface,
# but importing their host packages would initialize a second renderer (and
# pyglet creates a shadow X11 window merely on import).  The sovereign portal
# therefore keeps these names as precise unavailable callables regardless of
# which incidental packages happen to be installed beside the wheel.
_REFUSED_REFERENCE_RENDER_IMPORT_ROOTS = frozenset(
    {"OpenGL", "moderngl", "moderngl_window", "pyglet", "screeninfo"}
)


def _vec3(value):
    """A sequence (list/tuple/numpy array) as the engine's (x, y, z) floats."""
    return (float(value[0]), float(value[1]), float(value[2]))


def _interpolate(start, end, alpha):
    # manimlib/utils/bezier.py interpolate, verbatim.
    return (1 - alpha) * start + alpha * end


def _outer_interpolate(start, end, alpha):
    # manimlib/utils/bezier.py outer_interpolate, verbatim.
    result = _np.outer(1 - alpha, start) + _np.outer(alpha, end)
    return result.reshape((*_np.shape(alpha), *_np.shape(start)))


def _binary_search(function, target, lower_bound, upper_bound, tolerance=1e-4):
    # manimlib/utils/simple_functions.py binary_search, verbatim. This is
    # intentionally the Reference's monotonic-coordinate search rather than
    # Chisel's true-arclength inverse: CoordinateSystem uses it only when a
    # graph does not retain its underlying function.
    left = lower_bound
    right = upper_bound
    middle = (left + right) / 2
    while abs(right - left) > tolerance:
        left_value, middle_value, right_value = [
            function(value) for value in (left, middle, right)
        ]
        if left_value == target:
            return left_value
        if right_value == target:
            return right_value
        if left_value <= target and right_value >= target:
            if middle_value > target:
                right = middle
            else:
                left = middle
        elif left_value > target and right_value < target:
            left, right = right, left
        else:
            return None
        middle = (left + right) / 2
    return middle


def _axis_number_to_point(axis, x_min, x_max, number):
    # NumberLine.number_to_point (number_line.py:134) over the axis
    # proxy's LIVE own points — exact after any rescale/move/stretch.
    start = _np.array(axis._get_start())
    end = _np.array(axis._get_end())
    alpha = (_np.asarray(number, dtype=float) - x_min) / (x_max - x_min)
    if _np.shape(alpha) == ():
        return _interpolate(start, end, float(alpha))
    return _outer_interpolate(start, end, alpha)


def _axis_point_to_number(axis, x_min, x_max, point):
    # NumberLine.point_to_number (number_line.py:140), verbatim.
    start = _np.array(axis._get_start())
    end = _np.array(axis._get_end())
    vect = end - start
    proportion = _np.dot(_np.asarray(point, dtype=float) - start, vect) / _np.dot(
        vect, vect
    )
    return _interpolate(x_min, x_max, proportion)


_FRAME_X_RADIUS, _FRAME_Y_RADIUS = _BridgeMobject._frame_radii()
_FRAME_SHAPE = (2.0 * _FRAME_X_RADIUS, 2.0 * _FRAME_Y_RADIUS)
_DEG = _math.tau / 360.0
_RADIANS = 1.0


# Reference iterable plumbing (manimlib/utils/iterables.py), verbatim: the
# style surface resizes color/width runs with these exact rules.
def _listify(obj):
    if isinstance(obj, str):
        return [obj]
    try:
        return list(obj)
    except TypeError:
        return [obj]


def _array_is_constant(arr):
    return len(arr) > 0 and (arr == arr[0]).all()


def resize_array(nparray, length):
    """Reference cyclic resize over an arbitrary NumPy record array."""

    if len(nparray) == length:
        return nparray
    return _np.resize(nparray, (length, *nparray.shape[1:]))


def resize_preserving_order(nparray, length):
    """Reference proportional-index resize without interpolation."""

    if len(nparray) == 0:
        return _np.resize(nparray, length)
    if len(nparray) == length:
        return nparray
    indices = _np.arange(length) * len(nparray) // length
    return nparray[indices]


def _resize_with_interpolation(nparray, length):
    if len(nparray) == length:
        return nparray
    if len(nparray) == 1 or _array_is_constant(nparray):
        return nparray[:1].repeat(length, axis=0)
    if length == 0:
        return _np.zeros((0, *nparray.shape[1:]))
    cont_indices = _np.linspace(0, len(nparray) - 1, length)
    return _np.array(
        [
            (1 - a) * nparray[lh] + a * nparray[rh]
            for ci in cont_indices
            for lh, rh, a in [(int(ci), int(_np.ceil(ci)), ci % 1)]
        ]
    )


def _make_even(iterable_1, iterable_2):
    len1 = len(iterable_1)
    len2 = len(iterable_2)
    if len1 == len2:
        return iterable_1, iterable_2
    new_len = max(len1, len2)
    return (
        [iterable_1[(n * len1) // new_len] for n in range(new_len)],
        [iterable_2[(n * len2) // new_len] for n in range(new_len)],
    )


class _ColorValue:
    """A scalar Manim color carrying unquantized native gradient output."""

    def __init__(self, rgb):
        self.rgb = tuple(float(component) for component in rgb)

    def get_rgb(self):
        return self.rgb

    def get_hex_l(self):
        return _rgb_to_hex(self.rgb)

    def __eq__(self, other):
        try:
            return self.rgb == tuple(float(component) for component in _color_to_rgb(other))
        except (TypeError, ValueError):
            return False

    def __repr__(self):
        return f"Color({self.get_hex_l()!r})"


def _color_to_rgb(color):
    # Hex spellings route through fmn-core's one color model (D4); RGB(A)
    # sequences pass through. Anything else refuses precisely.
    if isinstance(color, _ColorValue):
        return _np.array(color.rgb)
    if isinstance(color, str):
        return _np.array(_BridgeMobject._hex_to_rgb(color))
    return _np.array([float(component) for component in color][:3])


def _color_to_rgba(color, alpha=1.0):
    return _np.array([*_color_to_rgb(color), float(alpha)])


def _rgb_to_hex(rgb):
    return _BridgeMobject._rgb_to_hex(
        (float(rgb[0]), float(rgb[1]), float(rgb[2]))
    )


def _color_gradient(colors, length, interp_by_hsl=False):
    rgbs = [
        tuple(float(component) for component in _color_to_rgb(color))
        for color in colors
    ]
    return [
        _ColorValue(rgb)
        for rgb in _BridgeMobject._color_gradient(
            rgbs, int(length), bool(interp_by_hsl)
        )
    ]


class _LiveSubmobjects(list):
    """A list whose accepted mutations are mirrored into Marionette."""

    def __init__(self, owner):
        super().__init__()
        self._owner_ref = _weakref.ref(owner)

    def _commit(self, candidate):
        candidate = list(candidate)
        owner = self._owner_ref()
        if owner is None:
            raise ReferenceError("the owning Mobject has been collected")
        owner._replace_submobjects(candidate)
        list.clear(self)
        list.extend(self, candidate)

    def append(self, value):
        self._commit([*self, value])

    def extend(self, values):
        self._commit([*self, *list(values)])

    def insert(self, index, value):
        candidate = list(self)
        candidate.insert(index, value)
        self._commit(candidate)

    def clear(self):
        self._commit([])

    def pop(self, index=-1):
        candidate = list(self)
        value = candidate.pop(index)
        self._commit(candidate)
        return value

    def remove(self, value):
        candidate = list(self)
        candidate.remove(value)
        self._commit(candidate)

    def reverse(self):
        candidate = list(self)
        candidate.reverse()
        self._commit(candidate)

    def sort(self, *args, **kwargs):
        candidate = list(self)
        candidate.sort(*args, **kwargs)
        self._commit(candidate)

    def __setitem__(self, index, value):
        candidate = list(self)
        candidate[index] = value
        self._commit(candidate)

    def __delitem__(self, index):
        candidate = list(self)
        del candidate[index]
        self._commit(candidate)

    def __iadd__(self, values):
        self.extend(values)
        return self

    def __imul__(self, count):
        candidate = list(self)
        candidate *= count
        self._commit(candidate)
        return self


class _LiveUniforms(dict):
    """Typed engine uniforms plus an open-ended Python extension map."""

    def __init__(self, owner):
        super().__init__()
        self._owner_ref = _weakref.ref(owner)
        self._extras = {"opacity": 1.0}

    def _owner(self):
        owner = self._owner_ref()
        if owner is None:
            raise ReferenceError("the owning Mobject has been collected")
        return owner

    def __getitem__(self, key):
        owner = self._owner()
        if key in owner._uniform_names():
            return owner._get_uniform(key)
        return self._extras[key]

    def __setitem__(self, key, value):
        owner = self._owner()
        if key in owner._uniform_names():
            owner._set_uniform(key, value)
        else:
            self._extras[key] = value

    def __delitem__(self, key):
        if key in self._owner()._uniform_names():
            raise TypeError(f"engine uniform {key!r} cannot be deleted")
        del self._extras[key]

    def __iter__(self):
        yield from self._owner()._uniform_names()
        yield from self._extras

    def __len__(self):
        return len(self._owner()._uniform_names()) + len(self._extras)

    def __contains__(self, key):
        return key in self._owner()._uniform_names() or key in self._extras

    def get(self, key, default=None):
        try:
            return self[key]
        except KeyError:
            return default

    def keys(self):
        return dict.fromkeys(iter(self)).keys()

    def items(self):
        return [(key, self[key]) for key in self]

    def values(self):
        return [self[key] for key in self]

    def update(self, other=(), /, **kwargs):
        incoming = dict(other, **kwargs)
        for key, value in incoming.items():
            self[key] = value

    def copy(self):
        return dict(self.items())

    def __repr__(self):
        return repr(dict(self.items()))

    def __eq__(self, other):
        try:
            return dict(self.items()) == dict(other)
        except (TypeError, ValueError):
            return False


def _install_live_state(mobject):
    mobject.submobjects = _LiveSubmobjects(mobject)
    mobject.uniforms = _LiveUniforms(mobject)
    mobject.updaters = []
    mobject.saved_state = None
    mobject.target = None


class _AnimationBuilder:
    """Reference mobject.py:2250 verbatim: intercepted methods apply to
    the mobject's generated target through the ordinary bound surfaces;
    the product is a MoveToTarget-shaped (mobject, target) spec plus
    anim_args."""

    def __init__(self, mobject):
        self.mobject = mobject
        self.overridden_animation = None
        self.mobject.generate_target()
        self.is_chaining = False
        self.methods = []
        self.anim_args = {}
        self.can_pass_args = True

    def __getattr__(self, method_name):
        method = getattr(self.mobject.target, method_name)
        self.methods.append(method)
        has_overridden_animation = hasattr(method, "_override_animate")

        if (self.is_chaining and has_overridden_animation) or self.overridden_animation:
            raise NotImplementedError(
                "Method chaining is currently not supported for "
                "overridden animations"
            )

        def update_target(*method_args, **method_kwargs):
            if has_overridden_animation:
                self.overridden_animation = method._override_animate(
                    self.mobject, *method_args, **method_kwargs
                )
            else:
                method(*method_args, **method_kwargs)
            return self

        self.is_chaining = True
        return update_target

    def __call__(self, **kwargs):
        return self.set_anim_args(**kwargs)

    def set_anim_args(self, **kwargs):
        if not self.can_pass_args:
            raise ValueError(
                "Animation arguments can only be passed by calling ``animate`` "
                "or ``set_anim_args`` and can only be passed once",
            )
        self.anim_args = kwargs
        self.can_pass_args = False
        return self

    def build(self):
        if self.overridden_animation is not None:
            return self.overridden_animation
        return _MethodAnimation(self.mobject, self.methods, **self.anim_args)


def override_animate(method):
    def decorator(animation_method):
        method._override_animate = animation_method
        return animation_method

    return decorator


class _UpdaterBuilder:
    """Reference mobject.py:2339 verbatim: `mob.always.method(*args)`
    registers a per-frame `mob.method(*args)` updater."""

    def __init__(self, mobject):
        self.mobject = mobject

    def __getattr__(self, method_name):
        def add_updater(*method_args, **method_kwargs):
            self.mobject.add_updater(
                lambda m: getattr(m, method_name)(*method_args, **method_kwargs)
            )
            return self

        return add_updater


class _FunctionalUpdaterBuilder:
    """Reference mobject.py:2352 verbatim: arguments are thunks evaluated
    on every frame."""

    def __init__(self, mobject):
        self.mobject = mobject

    def __getattr__(self, method_name):
        def add_updater(*method_args, **method_kwargs):
            self.mobject.add_updater(
                lambda m: getattr(m, method_name)(
                    *(arg() for arg in method_args),
                    **{key: value() for key, value in method_kwargs.items()},
                )
            )
            return self

        return add_updater


def _family_preorder(root):
    result = []
    seen = set()
    visiting = set()

    def visit(mobject):
        marker = id(mobject)
        if marker in visiting:
            raise _FamilyCycleError("submobjects would create a family cycle")
        if marker in seen:
            return
        if not isinstance(mobject, _BridgeMobject):
            raise TypeError("submobjects must be Mobject instances")
        visiting.add(marker)
        seen.add(marker)
        result.append(mobject)
        for child in mobject.submobjects:
            visit(child)
        visiting.remove(marker)

    visit(root)
    return result


def _copy_mobject_graph(root, deep, memo=None, detach_bound=False):
    if memo is None:
        memo = {}
    existing = memo.get(id(root))
    if existing is not None:
        return existing

    detach_live = detach_bound and root._is_bound()
    if detach_live:
        pairs = []
        for old in _family_preorder(root):
            new = type(old).__new__(type(old))
            new._restore_engine_state(old._engine_state())
            pairs.append((old, new))
    else:
        bound_shells = root._copy_family_shells()
        if bound_shells is None:
            pairs = []
            for old in _family_preorder(root):
                new = type(old).__new__(type(old))
                old._copy_detached_state_to(new)
                pairs.append((old, new))
        else:
            pairs = list(bound_shells)

    mapping = {old: new for old, new in pairs}
    for old, new in pairs:
        memo[id(old)] = new
        _install_live_state(new)

    # Reference copy() temporarily stashes these relationship pointers and
    # installs None on the copy; generate_target/save_state reconnect only
    # their one prescribed cross-link afterward.
    internal = {
        "submobjects",
        "uniforms",
        "updaters",
        "_scene",
        "target",
        "saved_state",
    }
    for old, new in pairs:
        mapped_children = [mapping[child] for child in old.submobjects]
        new.submobjects.extend(mapped_children)

        attributes = {
            key: value
            for key, value in old.__dict__.items()
            if key not in internal
        }
        if deep:
            attributes = _copy.deepcopy(attributes, memo)
        else:
            attributes = {
                key: mapping.get(value, value)
                if isinstance(value, _BridgeMobject)
                else value
                for key, value in attributes.items()
            }
        new.__dict__.update(attributes)

        extras = old.uniforms._extras
        new.uniforms._extras = (
            _copy.deepcopy(extras, memo) if deep else dict(extras)
        )
        # Function objects remain shared even under deepcopy, while the list is
        # independent, matching manim's updater copy rule.
        new.updaters = list(old.updaters)

    return mapping[root]


def _restore_mobject(cls, engine_state, attributes, children, extras, updaters):
    result = cls.__new__(cls)
    result._restore_engine_state(engine_state)
    _install_live_state(result)
    result.__dict__.update(attributes)
    result.uniforms._extras = extras
    result.updaters = updaters
    result.submobjects.extend(children)
    return result


class Mobject(_BridgeMobject):
    dim = 3
    data_dtype = [("point", 3), ("rgba", 4)]
    aligned_data_keys = ["point"]
    pointlike_data_keys = ["point"]

    def __init__(self, *submobjects, **kwargs):
        _install_live_state(self)
        for key, value in kwargs.items():
            setattr(self, key, value)
        self._engine_init()
        if submobjects:
            self._ingest_args(*submobjects)

    def _ingest_args(self, *args):
        # Reference Group._ingest_args (mobject.py:2201) verbatim: a single
        # list/tuple/generator argument spreads into the member list.
        if len(args) == 0:
            return
        if all(isinstance(mob, _BridgeMobject) for mob in args):
            self.submobjects.extend(args)
        elif isinstance(args[0], _collections_abc.Iterable):
            self.submobjects.extend(args[0])
        else:
            raise Exception(f"Invalid argument to Group of type {type(args[0])}")

    def point_from_proportion(self, alpha):
        # Reference Mobject's raw-record interpolation. VMobject overrides
        # this with BN-03's true-arclength path implementation below.
        points = self.get_points()
        end = len(points) - 1
        alpha = float(alpha)
        if alpha >= 1.0:
            index, residue = end - 1, 1.0
        elif alpha <= 0.0:
            index, residue = 0, 0.0
        else:
            value = end * alpha
            index, residue = int(value), value % 1.0
        return _interpolate(points[index], points[index + 1], residue)

    def pfp(self, alpha):
        """Abbreviation for point_from_proportion."""
        return self.point_from_proportion(alpha)

    def init_data(self):
        pass

    def init_points(self):
        pass

    def init_uniforms(self):
        if not isinstance(getattr(self, "uniforms", None), _LiveUniforms):
            self.uniforms = _LiveUniforms(self)

    @property
    def data(self):
        return self._data_array()

    @property
    def points(self):
        return self.data["point"]

    def add(self, *mobjects):
        # Reference Mobject.add (mobject.py:457) is an ordered identity-set
        # insertion, not list.extend: repeated arguments and children already
        # present under this parent are ignored.  Commit one child at a time
        # through the live-list seam so an ordinary later Python failure keeps
        # the successfully attached prefix, while Marionette still refuses
        # foreign stages and cycles before corrupting its graph.
        if self in mobjects:
            raise Exception("Mobject cannot contain self")
        for mobject in mobjects:
            if mobject not in self.submobjects:
                self.submobjects.append(mobject)
        return self

    def add_to_back(self, *mobjects):
        # Reference list_update keeps the last occurrence of each identity:
        # an existing child therefore wins over the same child in mobjects.
        candidate = [*mobjects, *self.submobjects]
        candidate = list(reversed(dict.fromkeys(reversed(candidate))))
        self.set_submobjects(candidate)
        return self

    def get_family(self, recurse=True):
        if not recurse:
            return [self]

        # Preserve the Reference's path-wise preorder semantics: a shared
        # descendant appears once for every path that reaches it.  The
        # engine's internal family walker intentionally deduplicates shared
        # descendants, so this compatibility-facing traversal must remain
        # separate.  Enter/exit markers keep the implementation iterative
        # while still refusing genuine cycles.
        family = []
        visiting = set()
        stack = [(True, self)]
        while stack:
            entering, mobject = stack.pop()
            marker = id(mobject)
            if not entering:
                visiting.remove(marker)
                continue
            if not isinstance(mobject, _BridgeMobject):
                raise TypeError("submobjects must be Mobject instances")
            if marker in visiting:
                raise _FamilyCycleError("submobjects would create a family cycle")
            visiting.add(marker)
            family.append(mobject)
            stack.append((False, mobject))
            stack.extend((True, child) for child in reversed(list(mobject.submobjects)))
        return family

    def family_members_with_points(self):
        return [mobject for mobject in self.get_family() if mobject.has_points()]

    def get_all_points(self):
        # Reference Mobject.get_all_points keeps path-wise family traversal:
        # a shared descendant contributes once per path, and every member's
        # get_points override remains observable.  get_points also bakes the
        # engine's retained placement before exposing world-space records.
        if self.submobjects:
            return _np.vstack([mob.get_points() for mob in self.get_family()])
        return self.get_points()

    # ----------------------------------------------------------------------
    # Positional surface (fm-d3gt): Reference signatures and bodies over the
    # engine's Stage primitives. Defined here, before the schema placeholder
    # pass, so the parity placeholders skip these names. Every transform and
    # bounding-box read routes through the ONE Stage code path (the scene's
    # stage when bound, the proxy's private nursery Stage when detached).

    def _each_stage_target(self):
        # One engine call per Stage: a bound proxy's scene stage recurses
        # the family natively; a detached proxy's nursery holds exactly one
        # root, so the transform distributes over the Python family list.
        if self._is_bound():
            return [self]
        return _family_preorder(self)

    def _bbox_rows(self):
        # The family bounding box as the Reference's [min, mid, max] rows.
        if self._is_bound():
            low, mid, high = self._get_bbox()
            return _np.array([low, mid, high])
        # `works_on_bounding_box=True` transforms each member's cached three
        # rows directly. Preserve the root's transformed family box only while
        # every member still carries the matching native cache. A later point
        # or structural mutation invalidates at least one member and returns
        # this detached graph to ordinary extrema aggregation.
        family = _family_preorder(self)
        installed = [member._get_installed_bbox() for member in family]
        if installed and all(box is not None for box in installed):
            return _np.array(installed[0])
        lows = []
        highs = []
        for member in family:
            if not member._has_points():
                continue
            low, _mid, high = member._get_bbox()
            lows.append(low)
            highs.append(high)
        if not lows:
            return _np.zeros((3, 3))
        low = _np.min(_np.array(lows), axis=0)
        high = _np.max(_np.array(highs), axis=0)
        return _np.array([low, (low + high) / 2.0, high])

    def _resolve_pivot(self, about_point, about_edge):
        # The Reference's `apply_points_function` pivot rule. Both None
        # applies the (linear) map in place, i.e. about the coordinate
        # origin.
        if about_point is None and about_edge is not None:
            about_point = self.get_bounding_box_point(about_edge)
        if about_point is None:
            return (0.0, 0.0, 0.0)
        return _vec3(about_point)

    def apply_points_function(
        self,
        func,
        about_point=None,
        about_edge=_ORIGIN,
        works_on_bounding_box=False,
    ):
        if about_point is None and about_edge is not None:
            about_point = self.get_bounding_box_point(about_edge)
        pivot = None if about_point is None else _np.array(_vec3(about_point))
        cached_boxes = []

        def install_cached_boxes():
            # Install only after all writes observed so far, so each entry is
            # keyed to the final subtree signature. This also preserves the
            # Reference's member-by-member partial state if a later callback
            # raises; the callback exception itself is never translated.
            for cached_mob, cached_box in cached_boxes:
                cached_mob._cache_bbox_rows(
                    [
                        tuple(float(component) for component in row)
                        for row in cached_box
                    ]
                )

        try:
            for mob in self.get_family():
                box = (
                    mob.get_bounding_box().copy()
                    if works_on_bounding_box
                    else None
                )
                keys = mob.pointlike_data_keys if mob.has_points() else ()
                for key in keys:
                    # RecordBuffer fields are f32, matching the Reference's
                    # structured data arrays. Callbacks may legitimately
                    # branch on dtype, and NumPy arithmetic precision is part
                    # of their observable Python semantics.
                    array = _np.array(mob._field_rows(key), dtype=_np.float32)
                    if pivot is None:
                        try:
                            array[:] = func(array)
                        except BaseException:
                            # The Reference passes the live record array in
                            # this branch, so in-place edits made before a
                            # callback failure remain observable.
                            mob._set_field_rows(key, array.tolist())
                            raise
                    else:
                        array[:] = func(array - pivot) + pivot
                    mob._set_field_rows(key, array.tolist())
                if works_on_bounding_box:
                    try:
                        if pivot is None:
                            box[:] = func(box)
                        else:
                            box[:] = func(box - pivot) + pivot
                    except BaseException:
                        # As above, the no-pivot branch may have mutated the
                        # directly supplied cache array before raising.
                        cached_boxes.append((mob, box))
                        raise
                    cached_boxes.append((mob, box))
        except BaseException:
            install_cached_boxes()
            raise

        install_cached_boxes()
        return self

    def apply_function(self, function, **kwargs):
        if len(kwargs) == 0:
            kwargs["about_point"] = _ORIGIN
        self.apply_points_function(
            lambda points: _np.array([function(point) for point in points]),
            **kwargs,
        )
        return self

    def apply_function_to_position(self, function):
        self.move_to(function(self.get_center()))
        return self

    def apply_function_to_submobject_positions(self, function):
        for submobject in self.submobjects:
            submobject.apply_function_to_position(function)
        return self

    def apply_matrix(self, matrix, **kwargs):
        if "about_point" not in kwargs and "about_edge" not in kwargs:
            kwargs["about_point"] = _ORIGIN
        full_matrix = _np.identity(self.dim)
        matrix = _np.array(matrix)
        full_matrix[: matrix.shape[0], : matrix.shape[1]] = matrix

        point_mapper_overridden = (
            getattr(self.apply_points_function, "__func__", None)
            is not Mobject.apply_points_function
        )
        family_walker_overridden = (
            getattr(self.get_family, "__func__", None) is not Mobject.get_family
        )
        if (
            kwargs.get("works_on_bounding_box", False)
            or point_mapper_overridden
            or family_walker_overridden
        ):
            self.apply_points_function(
                lambda points: _np.dot(points, full_matrix.T), **kwargs
            )
            return self

        family = self.get_family()
        custom_point_fields = any(
            tuple(mob.pointlike_data_keys) != ("point",) for mob in family
        )
        shared_family_paths = len({id(mob) for mob in family}) != len(family)
        if custom_point_fields or shared_family_paths:
            self.apply_points_function(
                lambda points: _np.dot(points, full_matrix.T), **kwargs
            )
            return self

        about_point = kwargs.pop("about_point", None)
        about_edge = kwargs.pop("about_edge", _ORIGIN)
        kwargs.pop("works_on_bounding_box", None)
        if kwargs:
            return self.apply_points_function(
                lambda points: _np.dot(points, full_matrix.T), **kwargs
            )
        native_matrix = [
            [float(component) for component in row] for row in full_matrix
        ]
        native_point = None if about_point is None else _vec3(about_point)
        native_edge = None if about_edge is None else _vec3(about_edge)
        for target in self._each_stage_target():
            target._apply_matrix(native_matrix, native_point, native_edge)
        return self

    def apply_complex_function(self, function, **kwargs):
        def real_function(point):
            x, y, z = point
            xy_complex = function(complex(x, y))
            return [xy_complex.real, xy_complex.imag, z]

        return self.apply_function(real_function, **kwargs)

    def shift(self, vector):
        vector = _vec3(vector)
        for target in self._each_stage_target():
            target._shift(vector)
        return self

    def scale(
        self,
        scale_factor,
        min_scale_factor=1e-8,
        about_point=None,
        about_edge=_ORIGIN,
    ):
        pivot = self._resolve_pivot(about_point, about_edge)
        factor = _np.array(scale_factor, dtype=float)
        if factor.ndim == 0:
            value = max(float(factor), float(min_scale_factor))
            for target in self._each_stage_target():
                target._scale_about(value, pivot)
            side_effect_factor = value
        else:
            values = factor.clip(min=float(min_scale_factor))
            for target in self._each_stage_target():
                for dim, value in enumerate(values[:3]):
                    target._stretch_about(float(value), dim, pivot)
            side_effect_factor = values
        # The Reference's per-member hook (DecimalNumber tracks font_size
        # through it, mobject.py:955).
        for mob in _family_preorder(self):
            mob._handle_scale_side_effects(side_effect_factor)
        return self

    def _handle_scale_side_effects(self, scale_factor):
        del scale_factor
        return self

    def stretch(self, factor, dim, **kwargs):
        pivot = self._resolve_pivot(
            kwargs.pop("about_point", None), kwargs.pop("about_edge", _ORIGIN)
        )
        if kwargs:
            raise TypeError(
                "stretch() got unexpected keyword arguments: "
                + ", ".join(sorted(kwargs))
            )
        for target in self._each_stage_target():
            target._stretch_about(float(factor), int(dim), pivot)
        return self

    def stretch_about_point(self, factor, dim, point):
        return self.stretch(factor, dim, about_point=point)

    def stretch_in_place(self, factor, dim):
        return self.stretch(factor, dim)

    def rotate(self, angle, axis=_OUT, about_point=None, **kwargs):
        pivot = self._resolve_pivot(about_point, kwargs.pop("about_edge", _ORIGIN))
        if kwargs:
            raise TypeError(
                "rotate() got unexpected keyword arguments: "
                + ", ".join(sorted(kwargs))
            )
        axis = _vec3(axis)
        for target in self._each_stage_target():
            target._rotate_about(float(angle), axis, pivot)
        return self

    def rotate_about_origin(self, angle, axis=_OUT):
        return self.rotate(angle, axis, about_point=_ORIGIN)

    def flip(self, axis=_UP, **kwargs):
        return self.rotate(_math.tau / 2, axis, **kwargs)

    def center(self):
        self.shift(-self.get_center())
        return self

    def align_on_border(self, direction, buff=_DEFAULT_MOBJECT_TO_EDGE_BUFF):
        if self._is_bound():
            self._to_edge(_vec3(direction), float(buff))
            return self
        direction = _np.array(_vec3(direction))
        x_radius, y_radius = type(self)._frame_radii()
        target_point = _np.sign(direction) * _np.array([x_radius, y_radius, 0.0])
        point_to_align = self.get_bounding_box_point(direction)
        shift_val = target_point - point_to_align - buff * direction
        shift_val = shift_val * abs(_np.sign(direction))
        self.shift(shift_val)
        return self

    def to_corner(self, corner=_DL, buff=_DEFAULT_MOBJECT_TO_EDGE_BUFF):
        return self.align_on_border(corner, buff)

    def to_edge(self, edge=_LEFT, buff=_DEFAULT_MOBJECT_TO_EDGE_BUFF):
        return self.align_on_border(edge, buff)

    def shift_onto_screen(self, **kwargs):
        space_lengths = [_FRAME_X_RADIUS, _FRAME_Y_RADIUS]
        for vector in (_UP, _DOWN, _LEFT, _RIGHT):
            dim = int(_np.argmax(_np.abs(vector)))
            buff = kwargs.get("buff", _DEFAULT_MOBJECT_TO_EDGE_BUFF)
            max_value = space_lengths[dim] - buff
            edge_center = self.get_edge_center(vector)
            if _np.dot(edge_center, vector) > max_value:
                self.to_edge(vector, **kwargs)
        return self

    def is_off_screen(self):
        if self.get_left()[0] > _FRAME_X_RADIUS:
            return True
        if self.get_right()[0] < -_FRAME_X_RADIUS:
            return True
        if self.get_bottom()[1] > _FRAME_Y_RADIUS:
            return True
        if self.get_top()[1] < -_FRAME_Y_RADIUS:
            return True
        return False

    def next_to(
        self,
        mobject_or_point,
        direction=_RIGHT,
        buff=_DEFAULT_MOBJECT_TO_MOBJECT_BUFF,
        aligned_edge=_ORIGIN,
        submobject_to_align=None,
        index_of_submobject_to_align=None,
        coor_mask=_ONES,
    ):
        direction = _np.array(_vec3(direction))
        aligned_edge = _np.array(_vec3(aligned_edge))
        if isinstance(mobject_or_point, _BridgeMobject):
            mob = mobject_or_point
            if index_of_submobject_to_align is not None:
                target_aligner = mob[index_of_submobject_to_align]
            else:
                target_aligner = mob
            target_point = target_aligner.get_bounding_box_point(
                aligned_edge + direction
            )
        else:
            target_point = _np.array(_vec3(mobject_or_point))
        if submobject_to_align is not None:
            aligner = submobject_to_align
        elif index_of_submobject_to_align is not None:
            aligner = self[index_of_submobject_to_align]
        else:
            aligner = self
        point_to_align = aligner.get_bounding_box_point(aligned_edge - direction)
        self.shift(
            (target_point - point_to_align + buff * direction)
            * _np.array(_vec3(coor_mask))
        )
        return self

    def move_to(self, point_or_mobject, aligned_edge=_ORIGIN, coor_mask=_ONES):
        aligned_edge = _np.array(_vec3(aligned_edge))
        if isinstance(point_or_mobject, _BridgeMobject):
            target = point_or_mobject.get_bounding_box_point(aligned_edge)
        else:
            target = _np.array(_vec3(point_or_mobject))
        point_to_align = self.get_bounding_box_point(aligned_edge)
        self.shift((target - point_to_align) * _np.array(_vec3(coor_mask)))
        return self

    def align_to(self, mobject_or_point, direction=_ORIGIN):
        if isinstance(mobject_or_point, _BridgeMobject):
            point = mobject_or_point.get_bounding_box_point(direction)
        else:
            point = _np.array(_vec3(mobject_or_point))
        direction = _np.array(_vec3(direction))
        for dim in range(3):
            if direction[dim] != 0:
                self.set_coord(point[dim], dim, direction)
        return self

    def set_coord(self, value, dim, direction=_ORIGIN):
        curr = self.get_coord(dim, direction)
        shift_vect = _np.zeros(3)
        shift_vect[dim] = value - curr
        self.shift(shift_vect)
        return self

    def set_x(self, x, direction=_ORIGIN):
        return self.set_coord(x, 0, direction)

    def set_y(self, y, direction=_ORIGIN):
        return self.set_coord(y, 1, direction)

    def set_z(self, z, direction=_ORIGIN):
        return self.set_coord(z, 2, direction)

    def rescale_to_fit(self, length, dim, stretch=False, **kwargs):
        old_length = self.length_over_dim(dim)
        if old_length == 0:
            return self
        if stretch:
            self.stretch(length / old_length, dim, **kwargs)
        else:
            self.scale(length / old_length, **kwargs)
        return self

    def replace(self, mobject, dim_to_match=0, stretch=False):
        if not mobject.has_points() and not mobject.submobjects:
            self.scale(0)
            return self
        if stretch:
            for dim in range(3):
                self.rescale_to_fit(mobject.length_over_dim(dim), dim, stretch=True)
        else:
            self.rescale_to_fit(
                mobject.length_over_dim(dim_to_match), dim_to_match, stretch=False
            )
        self.shift(mobject.get_center() - self.get_center())
        return self

    def stretch_to_fit_width(self, width, **kwargs):
        return self.rescale_to_fit(width, 0, stretch=True, **kwargs)

    def stretch_to_fit_height(self, height, **kwargs):
        return self.rescale_to_fit(height, 1, stretch=True, **kwargs)

    def stretch_to_fit_depth(self, depth, **kwargs):
        return self.rescale_to_fit(depth, 2, stretch=True, **kwargs)

    def set_width(self, width, stretch=False, **kwargs):
        return self.rescale_to_fit(width, 0, stretch=stretch, **kwargs)

    def set_height(self, height, stretch=False, **kwargs):
        return self.rescale_to_fit(height, 1, stretch=stretch, **kwargs)

    def set_depth(self, depth, stretch=False, **kwargs):
        return self.rescale_to_fit(depth, 2, stretch=stretch, **kwargs)

    def set_shape(self, width=None, height=None, depth=None, **kwargs):
        if width is not None:
            self.set_width(width, stretch=True, **kwargs)
        if height is not None:
            self.set_height(height, stretch=True, **kwargs)
        if depth is not None:
            self.set_depth(depth, stretch=True, **kwargs)
        return self

    def space_out_submobjects(self, factor=1.5, **kwargs):
        self.scale(factor, **kwargs)
        for submobject in self.submobjects:
            submobject.scale(1.0 / factor)
        return self

    def set_max_width(self, max_width, **kwargs):
        if self.get_width() > max_width:
            self.set_width(max_width, **kwargs)
        return self

    def set_max_height(self, max_height, **kwargs):
        if self.get_height() > max_height:
            self.set_height(max_height, **kwargs)
        return self

    def set_submobjects(self, submobject_list):
        # Reference Mobject.set_submobjects (mobject.py:508) through the
        # live-list seam.  The clear-then-add order is observable on a later
        # Python error, and add owns Reference identity deduplication.
        if self.submobjects == submobject_list:
            return self
        self.clear()
        self.add(*submobject_list)
        return self

    def reverse_submobjects(self):
        self.submobjects.reverse()
        return self

    def set_z_index(self, z_index, recurse=True):
        # State-real: the engine's scene-list sort key (§8.5), written per
        # family member in both proxy states.
        for mob in _family_preorder(self) if recurse else [self]:
            mob._set_z_index(int(z_index))
        return self

    def sort(self, point_to_num_func=lambda p: p[0], submob_func=None):
        # _LiveSubmobjects commits the final stable Python ordering into the
        # Stage, so draw order and the compatibility-facing list cannot drift.
        if submob_func is not None:
            self.submobjects.sort(key=submob_func)
        else:
            self.submobjects.sort(
                key=lambda mobject: point_to_num_func(mobject.get_center())
            )
        return self

    def shuffle(self, recurse=False):
        if recurse:
            for submobject in self.submobjects:
                submobject.shuffle(recurse=True)
        candidate = list(self.submobjects)
        getattr(_FMN_ROOT, "random").shuffle(candidate)
        self.set_submobjects(candidate)
        return self

    def arrange_to_fit_dim(self, length, dim, about_edge=_ORIGIN):
        # Reference Mobject.arrange_to_fit_dim (mobject.py:583) verbatim,
        # including its bare `return` for the trivial-count branch.
        ref_point = self.get_bounding_box_point(about_edge)
        n_submobs = len(self.submobjects)
        if n_submobs <= 1:
            return
        total_length = sum(sm.length_over_dim(dim) for sm in self.submobjects)
        buff = (length - total_length) / (n_submobs - 1)
        vect = _np.zeros(3)
        vect[dim] = 1
        x = 0
        for submob in self.submobjects:
            submob.set_coord(x, dim, -vect)
            x += submob.length_over_dim(dim) + buff
        self.move_to(ref_point, about_edge)
        return self

    def arrange_to_fit_width(self, width, about_edge=_ORIGIN):
        return self.arrange_to_fit_dim(width, 0, about_edge)

    def arrange_to_fit_height(self, height, about_edge=_ORIGIN):
        return self.arrange_to_fit_dim(height, 1, about_edge)

    def arrange_to_fit_depth(self, depth, about_edge=_ORIGIN):
        return self.arrange_to_fit_dim(depth, 2, about_edge)

    def apply_depth_test(self, recurse=True):
        # State-real: `depth_test` is a typed engine uniform (§8.4).
        for mob in _family_preorder(self) if recurse else [self]:
            mob.uniforms["depth_test"] = True
        return self

    def deactivate_depth_test(self, recurse=True):
        for mob in _family_preorder(self) if recurse else [self]:
            mob.uniforms["depth_test"] = False
        return self

    def fix_in_frame(self, recurse=True):
        # Reference Mobject.fix_in_frame is exactly this typed-uniform write.
        # Keep it on the engine-owned Uniforms record so the same value flows
        # into Lumen's camera projection and snapshot/provenance paths.
        for mob in _family_preorder(self) if recurse else [self]:
            mob.uniforms["is_fixed_in_frame"] = 1.0
        return self

    def unfix_from_frame(self, recurse=True):
        for mob in _family_preorder(self) if recurse else [self]:
            mob.uniforms["is_fixed_in_frame"] = 0.0
        return self

    def is_fixed_in_frame(self):
        return bool(self.uniforms["is_fixed_in_frame"])

    def get_continuous_bounding_box_point(self, direction):
        # Reference Mobject.get_continuous_bounding_box_point verbatim.
        dl, center, ur = self.get_bounding_box()
        del dl
        corner_vect = ur - center
        direction = _np.array(_vec3(direction))
        return center + direction / _np.max(
            _np.abs(
                _np.true_divide(
                    direction,
                    corner_vect,
                    out=_np.zeros(len(direction)),
                    where=(corner_vect != 0),
                )
            )
        )

    def replicate(self, n):
        group_class = self.get_group_class()
        return group_class(*(self.copy() for _ in range(n)))

    def get_grid(
        self,
        n_rows,
        n_cols,
        height=None,
        width=None,
        group_by_rows=False,
        group_by_cols=False,
        **kwargs,
    ):
        # Reference Mobject.get_grid (mobject.py:786), with Appendix-C C-14's
        # correction: the public `width` argument sizes the width rather than
        # accidentally calling set_height.
        total = n_rows * n_cols
        grid = self.replicate(total)
        if group_by_cols:
            kwargs["fill_rows_first"] = False
        grid.arrange_in_grid(n_rows, n_cols, **kwargs)
        if height is not None:
            grid.set_height(height)
        if width is not None:
            grid.set_width(width)
        group_class = self.get_group_class()
        if group_by_rows:
            return group_class(*(grid[n : n + n_cols] for n in range(0, total, n_cols)))
        elif group_by_cols:
            return group_class(*(grid[n : n + n_rows] for n in range(0, total, n_rows)))
        return grid

    def match_dim_size(self, mobject, dim, **kwargs):
        return self.rescale_to_fit(mobject.length_over_dim(dim), dim, **kwargs)

    def match_width(self, mobject, **kwargs):
        return self.match_dim_size(mobject, 0, **kwargs)

    def match_height(self, mobject, **kwargs):
        return self.match_dim_size(mobject, 1, **kwargs)

    def match_depth(self, mobject, **kwargs):
        return self.match_dim_size(mobject, 2, **kwargs)

    def match_coord(self, mobject_or_point, dim, direction=_ORIGIN):
        if isinstance(mobject_or_point, _BridgeMobject):
            coord = mobject_or_point.get_coord(dim, direction)
        else:
            coord = mobject_or_point[dim]
        return self.set_coord(coord, dim=dim, direction=direction)

    def match_x(self, mobject_or_point, direction=_ORIGIN):
        return self.match_coord(mobject_or_point, 0, direction)

    def match_y(self, mobject_or_point, direction=_ORIGIN):
        return self.match_coord(mobject_or_point, 1, direction)

    def match_z(self, mobject_or_point, direction=_ORIGIN):
        return self.match_coord(mobject_or_point, 2, direction)

    def put_start_and_end_on(self, start, end):
        start = _np.array(_vec3(start))
        end = _np.array(_vec3(end))
        if self._is_bound():
            # A bound Stage owns the real family graph, so its native
            # implementation applies the affine transform recursively.
            self._put_start_and_end_on(_vec3(start), _vec3(end))
            return self

        # Reference Mobject.put_start_and_end_on (mobject.py:1299). The
        # ordinary Python transforms distribute over a detached proxy's
        # private family, whereas a direct nursery-Stage call can only see
        # the root and would leave its children behind.
        curr_start, curr_end = self.get_start_and_end()
        curr_vect = curr_end - curr_start
        if _np.all(curr_vect == 0):
            raise Exception("Cannot position endpoints of closed loop")
        target_vect = end - start
        self.scale(
            _np.linalg.norm(target_vect) / _np.linalg.norm(curr_vect),
            about_point=curr_start,
        )
        self.rotate(
            _math.atan2(target_vect[1], target_vect[0])
            - _math.atan2(curr_vect[1], curr_vect[0])
        )
        self.rotate(
            _math.atan2(curr_vect[2], _np.linalg.norm(curr_vect[:2]))
            - _math.atan2(target_vect[2], _np.linalg.norm(target_vect[:2])),
            axis=_np.array([-target_vect[1], target_vect[0], 0.0]),
        )
        self.shift(start - self.get_start())
        return self

    def arrange(self, direction=_RIGHT, center=True, **kwargs):
        for m1, m2 in zip(self.submobjects, self.submobjects[1:]):
            m2.next_to(m1, direction, **kwargs)
        if center:
            self.center()
        return self

    def arrange_in_grid(
        self,
        n_rows=None,
        n_cols=None,
        buff=None,
        h_buff=None,
        v_buff=None,
        buff_ratio=None,
        h_buff_ratio=0.5,
        v_buff_ratio=0.5,
        aligned_edge=_ORIGIN,
        fill_rows_first=True,
    ):
        submobs = self.submobjects
        n_submobs = len(submobs)
        if n_rows is None:
            n_rows = (
                int(_np.sqrt(n_submobs)) if n_cols is None else n_submobs // n_cols
            )
        if n_cols is None:
            n_cols = n_submobs // n_rows

        if buff is not None:
            h_buff = buff
            v_buff = buff
        else:
            if buff_ratio is not None:
                v_buff_ratio = buff_ratio
                h_buff_ratio = buff_ratio
            if h_buff is None:
                h_buff = h_buff_ratio * self[0].get_width()
            if v_buff is None:
                v_buff = v_buff_ratio * self[0].get_height()

        x_unit = h_buff + max(sm.get_width() for sm in submobs)
        y_unit = v_buff + max(sm.get_height() for sm in submobs)

        for index, sm in enumerate(submobs):
            if fill_rows_first:
                x, y = index % n_cols, index // n_cols
            else:
                x, y = index // n_rows, index % n_rows
            sm.move_to(_ORIGIN, aligned_edge)
            sm.shift(x * x_unit * _RIGHT + y * y_unit * _DOWN)
        self.center()
        return self

    # Positional getters over the same Stage bounding box.

    def get_bounding_box(self):
        # Dynamic dispatch matters: DotCloud expands the point hull by its
        # radius, while CameraFrame supplies renderer-owned frame state.
        return self.compute_bounding_box()

    def compute_bounding_box(self):
        # Marionette already maintains the revisioned family box over world-
        # space points.  Use that retained result for the ordinary family,
        # but preserve class-specific extent rules (notably DotCloud radius)
        # when one appears below this root.  The slow branch is the Reference
        # aggregation itself and stores no competing cache.
        family = self.get_family()
        if not any(
            type(mob).compute_bounding_box is not Mobject.compute_bounding_box
            for mob in family[1:]
        ):
            return self._bbox_rows()
        all_points = _np.vstack(
            [
                self.get_points(),
                *(
                    mob.get_bounding_box()
                    for mob in family[1:]
                    if mob.has_points()
                ),
            ]
        )
        if len(all_points) == 0:
            return _np.zeros((3, self.dim))
        mins = all_points.min(0)
        maxs = all_points.max(0)
        return _np.array([mins, (mins + maxs) / 2, maxs])

    def refresh_bounding_box(self, recurse_down=False, recurse_up=True):
        # RecordBuffer writes, live writable views, family edits, and retained
        # placements all invalidate Marionette's box through their revision
        # channels.  Accept the Reference cache-control surface while leaving
        # invalidation to that existing authority.
        del recurse_down, recurse_up
        return self

    def get_bounding_box_point(self, direction):
        bb = self.get_bounding_box()
        direction = _np.array(_vec3(direction))
        indices = (_np.sign(direction) + 1).astype(int)
        return _np.array([bb[indices[i]][i] for i in range(3)])

    def get_edge_center(self, direction):
        return self.get_bounding_box_point(direction)

    def get_corner(self, direction):
        return self.get_bounding_box_point(direction)

    def get_all_corners(self):
        bb = self.get_bounding_box()
        return _np.array(
            [
                [bb[indices[-i + 1]][i] for i in range(3)]
                for indices in _itertools.product([0, 2], repeat=3)
            ]
        )

    def get_center(self):
        return self.get_bounding_box()[1]

    def get_center_of_mass(self):
        return self.get_all_points().mean(0)

    def get_boundary_point(self, direction):
        all_points = self.get_all_points()
        boundary_directions = all_points - self.get_center()
        norms = _np.linalg.norm(boundary_directions, axis=1)
        boundary_directions /= _np.repeat(norms, 3).reshape((len(norms), 3))
        index = _np.argmax(_np.dot(boundary_directions, _np.array(direction).T))
        return all_points[index]

    def get_top(self):
        return self.get_edge_center(_UP)

    def get_bottom(self):
        return self.get_edge_center(_DOWN)

    def get_right(self):
        return self.get_edge_center(_RIGHT)

    def get_left(self):
        return self.get_edge_center(_LEFT)

    def get_zenith(self):
        return self.get_edge_center(_OUT)

    def get_nadir(self):
        return self.get_edge_center(_IN)

    def length_over_dim(self, dim):
        bb = self.get_bounding_box()
        return abs((bb[2] - bb[0])[dim])

    def are_points_touching(self, points, buff=0):
        bb = self.get_bounding_box()
        mins = bb[0] - buff
        maxs = bb[2] + buff
        return ((points >= mins) * (points <= maxs)).all(1)

    def is_point_touching(self, point, buff=0):
        return self.are_points_touching(_np.array(point, ndmin=2), buff)[0]

    def is_touching(self, mobject, buff=0.01):
        bb1 = self.get_bounding_box()
        bb2 = mobject.get_bounding_box()
        return not any(
            (
                (bb2[2] < bb1[0] - buff).any(),
                (bb2[0] > bb1[2] + buff).any(),
            )
        )

    def get_width(self):
        return self.length_over_dim(0)

    def get_height(self):
        return self.length_over_dim(1)

    def get_depth(self):
        return self.length_over_dim(2)

    def get_coord(self, dim, direction=_ORIGIN):
        return float(self.get_bounding_box_point(direction)[dim])

    def get_x(self, direction=_ORIGIN):
        return self.get_coord(0, direction)

    def get_y(self, direction=_ORIGIN):
        return self.get_coord(1, direction)

    def get_z(self, direction=_ORIGIN):
        return self.get_coord(2, direction)

    def get_start(self):
        return _np.array(self._get_start())

    def get_end(self):
        return _np.array(self._get_end())

    def get_start_and_end(self):
        return (self.get_start(), self.get_end())

    def get_points(self):
        # Stage keeps transforms in an independent placement. Bake that
        # representation first so the familiar writable NumPy view is in
        # world coordinates, exactly like the Reference's point records.
        self._bake_placement()
        return self.points

    def resize_points(self, new_length, resize_func=resize_array):
        # Reference Mobject.resize_points over the live RecordBuffer.  Python
        # chooses the public resize policy; Marionette owns the fallible
        # allocation, generation swap, and revision invalidation.  Copying the
        # resulting structured rows back preserves every subclass-specific
        # lane without teaching this surface about any particular schema.
        data = self.data
        defaults = self.__dict__.get("_data_defaults")
        if defaults is None:
            defaults = _np.ones(1, dtype=data.dtype)
            self.__dict__["_data_defaults"] = defaults
        if new_length == 0:
            if len(data) > 0:
                defaults[:1] = data[:1]
            source = data
        elif len(data) == 0:
            source = defaults.copy()
        else:
            source = data
        resized = resize_func(source, new_length)
        if new_length == len(data):
            data[:] = resized
            return self
        self.resize(new_length)
        if new_length > 0:
            self.data[:] = resized
        return self

    def set_points(self, points):
        self.resize_points(len(points), resize_func=resize_preserving_order)
        self.data["point"][:] = points
        return self

    def append_points(self, new_points):
        n = self.get_num_points()
        self.resize_points(n + len(new_points))
        data = self.data
        data[n:] = data[n - 1]
        data["point"][n:] = new_points
        return self

    def clear_points(self):
        self.resize_points(0)
        return self

    def get_num_points(self):
        return len(self.get_points())

    def reverse_points(self):
        self._reverse_points(True)
        return self

    def has_points(self):
        return self._has_points()

    def match_points(self, mobject):
        # Reference mobject.py:311. Placements are a FrankenManim-owned
        # representation detail, so bake both endpoints before copying the
        # world-space pointlike columns. RecordBuffer owns the exact
        # order-preserving resize primitive used by the Reference.
        self._bake_placement()
        mobject._bake_placement()
        self._resize_preserving_order(mobject.n_records())
        source = mobject.data
        target = self.data
        for key in self.pointlike_data_keys:
            target[key][:] = source[key]
        return self

    # Color surface (fm-d3gt): Reference bodies over the live record views.
    # Style written to a point-free mobject lands in `_data_defaults` (the
    # Reference's ones-row), so it round-trips through the getters before
    # any points exist.

    def _style_data(self):
        if self.n_records() > 0:
            return self.data
        defaults = self.__dict__.get("_data_defaults")
        if defaults is None:
            defaults = _np.ones(1, dtype=self.data.dtype)
            self.__dict__["_data_defaults"] = defaults
        return defaults

    def set_rgba_array(self, rgba_array, name="rgba", recurse=False):
        rgba_array = _np.asarray(rgba_array, dtype=float)
        for mob in _family_preorder(self) if recurse else [self]:
            mob._style_data()[name][:] = rgba_array
        return self

    def set_rgba_array_by_color(
        self, color=None, opacity=None, name="rgba", recurse=True
    ):
        for mob in _family_preorder(self) if recurse else [self]:
            data = mob._style_data()
            if color is not None:
                rgbs = _np.array([_color_to_rgb(c) for c in _listify(color)])
                if 1 < len(rgbs):
                    rgbs = _resize_with_interpolation(rgbs, len(data))
                data[name][:, :3] = rgbs
            if opacity is not None:
                if not isinstance(opacity, (float, int, _np.floating)):
                    opacity = _resize_with_interpolation(
                        _np.array(opacity), len(data)
                    )
                data[name][:, 3] = opacity
        return self

    def set_color(self, color, opacity=None, recurse=True):
        self.set_rgba_array_by_color(color, opacity, recurse=False)
        # Recurse to submobjects differently from how set_rgba_array_by_color
        # does, in case they implement set_color differently.
        if recurse:
            for submob in self.submobjects:
                submob.set_color(color, recurse=True)
        return self

    def set_opacity(self, opacity, recurse=True):
        self.set_rgba_array_by_color(color=None, opacity=opacity, recurse=False)
        if recurse:
            for submob in self.submobjects:
                submob.set_opacity(opacity, recurse=True)
        return self

    def get_color(self):
        return _rgb_to_hex(self._style_data()["rgba"][0, :3])

    def get_opacity(self):
        return float(self._style_data()["rgba"][0, 3])

    def get_opacities(self):
        return self._style_data()["rgba"][:, 3]

    def set_color_by_gradient(self, *colors):
        if self.has_points():
            self.set_color(colors)
        else:
            self.set_submobject_colors_by_gradient(*colors)
        return self

    def set_submobject_colors_by_gradient(self, *colors, interp_by_hsl=False):
        if len(colors) == 0:
            raise Exception("Need at least one color")
        if len(colors) == 1:
            return self.set_color(*colors)

        new_colors = _color_gradient(
            colors, len(self.submobjects), interp_by_hsl=interp_by_hsl
        )
        for submobject, color in zip(self.submobjects, new_colors):
            submobject.set_color(color)
        return self

    def add_background_rectangle(self, color=None, opacity=1.0, **kwargs):
        self.background_rectangle = BackgroundRectangle(
            self, color=color, fill_opacity=opacity, **kwargs
        )
        self.add_to_back(self.background_rectangle)
        return self

    def add_background_rectangle_to_submobjects(self, **kwargs):
        for submobject in self.submobjects:
            submobject.add_background_rectangle(**kwargs)
        return self

    def add_background_rectangle_to_family_members_with_points(self, **kwargs):
        for mobject in self.family_members_with_points():
            mobject.add_background_rectangle(**kwargs)
        return self

    def get_shading(self):
        return _np.array(self.uniforms["shading"])

    def set_shading(self, reflectiveness=None, gloss=None, shadow=None, recurse=True):
        for mob in _family_preorder(self) if recurse else [self]:
            shading = list(mob.uniforms["shading"])
            for index, value in enumerate([reflectiveness, gloss, shadow]):
                if value is not None:
                    shading[index] = float(value)
            mob.uniforms["shading"] = shading
        return self

    def fade(self, darkness=0.5, recurse=True):
        # The base Reference method intentionally has no fluent return. Its
        # VMobject override remains fluent and scales existing opacities.
        self.set_opacity(1.0 - darkness, recurse=recurse)

    def match_color(self, mobject):
        return self.set_color(mobject.get_color())

    def match_style(self, mobject):
        self.set_color(mobject.get_color())
        self.set_opacity(mobject.get_opacity())
        self.set_shading(*mobject.get_shading())
        return self

    # The Reference's container protocol (manimlib/mobject/mobject.py):
    # a mobject iterates, indexes, and measures as its submobject list;
    # slicing regroups through the family's group class.
    def split(self):
        return self.submobjects

    def get_group_class(self):
        return getattr(_FMN_ROOT, "Group", Mobject)

    def invisible_copy(self):
        result = self.copy()
        result.set_opacity(0)
        return result

    def add_n_more_submobjects(self, n):
        n = _operator.index(n)
        if n <= 0:
            return self
        current = len(self.submobjects)
        target = self._aligned_submobject_target(current, n)
        if current == 0:
            center = self.get_center()
            children = []
            for _ in range(target):
                child = self.copy()
                child.set_points([center])
                children.append(child)
            self.set_submobjects(children)
            return self

        split_factors = [0] * current
        for index in range(target):
            split_factors[index * current // target] += 1
        children = []
        for child, split_factor in zip(self.submobjects, split_factors):
            children.append(child)
            children.extend(
                child.invisible_copy() for _ in range(1, split_factor)
            )
        self.set_submobjects(children)
        return self

    def align_family(self, mobject):
        if not isinstance(mobject, _BridgeMobject):
            raise TypeError("align_family expects a Mobject")
        n1 = len(self.submobjects)
        n2 = len(mobject.submobjects)
        if n1 != n2:
            self.add_n_more_submobjects(max(0, n2 - n1))
            mobject.add_n_more_submobjects(max(0, n1 - n2))
        for child1, child2 in zip(self.submobjects, mobject.submobjects):
            child1.align_family(child2)
        return self

    def __getitem__(self, value):
        if isinstance(value, slice):
            return self.get_group_class()(*self.split()[value])
        return self.split()[value]

    def __iter__(self):
        return iter(self.split())

    def __len__(self):
        return len(self.split())

    def remove(self, *to_remove, reassemble=True, recurse=True):
        # Reference Mobject.remove (mobject.py:469) snapshots the family before
        # editing it, then visits parents in family order and requested
        # children in argument order.  _LiveSubmobjects owns both detach and
        # reverse-edge maintenance.  Marionette has no stale family cache to
        # defer, so `reassemble` remains the accepted batching hint while the
        # live graph is kept coherent immediately.
        del reassemble
        parents = self.get_family(recurse)
        for parent in parents:
            for child in to_remove:
                if not isinstance(child, _BridgeMobject):
                    raise TypeError("submobjects must be Mobject instances")
                if child in parent.submobjects:
                    parent.submobjects.remove(child)
        return self

    def clear(self):
        return self.remove(*list(self.submobjects), recurse=False)

    def add_updater(self, updater, index=None, call=True):
        if not callable(updater):
            raise TypeError(
                "Mobject.add_updater requires a callable updater; got "
                + type(updater).__name__
            )
        if index is None:
            self.updaters.append(updater)
        else:
            self.updaters.insert(index, updater)
        if call:
            # BN-07: one canonical child-first update pass, rather than the
            # Reference's accidental double pass or a new-updater-only call.
            self.update(0.0)
        return self

    def remove_updater(self, updater):
        self.updaters = [item for item in self.updaters if item is not updater]
        return self

    def clear_updaters(self, recurse=True):
        recurse = bool(recurse)
        targets = _family_preorder(self) if recurse else [self]
        for target in targets:
            target.updaters.clear()
        self._clear_native_updaters(recurse)
        return self

    def _update_python_family(self, dt, recurse):
        # Reference Mobject.update is child-first and snapshots each updater
        # list at that node's turn. Suspension prunes the whole subtree.
        if self._is_updating_suspended():
            return
        if recurse:
            for submobject in list(self.submobjects):
                submobject._update_python_family(dt, True)
        for updater in list(self.updaters):
            self._dispatch_updater(updater, dt)

    def update(self, dt=0, recurse=True):
        dt = float(dt)
        recurse = bool(recurse)
        self._update_python_family(dt, recurse)
        # Native updaters run only after host callbacks have returned, so the
        # Stage RefCell is never held across Python code.
        self._update_native_mobject(dt, recurse)
        return self

    def suspend_updating(self, recurse=True):
        self._suspend_updating(bool(recurse))
        return self

    def resume_updating(self, recurse=True, call_updater=True):
        recurse = bool(recurse)
        self._resume_updating(recurse)
        if call_updater:
            self.update(0.0, recurse=recurse)
        return self

    def generate_target(self, use_deepcopy=False):
        self.target = self.copy(deep=use_deepcopy)
        self.target.saved_state = self.saved_state
        return self.target

    def become(self, mobject, match_updaters=False):
        # Reference mobject.py:721: per-member data/uniform assignment
        # across zipped families, in both proxy states.  Reconcile the Python
        # and native family graphs first, then remap named attributes which
        # identify members of the source family onto the matching receiver.
        if not isinstance(mobject, _BridgeMobject):
            raise TypeError("become expects a Mobject")
        if (
            self._is_bound()
            and mobject._is_bound()
            and self._scene is not mobject._scene
        ):
            raise _ForeignStageError("become endpoints must belong to one Scene")
        if self._is_bound() and not mobject._is_bound():
            self._scene._adopt(mobject)
        self.align_family(mobject)
        receiver_family = self.get_family()
        source_family = mobject.get_family()
        self._become(mobject, match_updaters)
        for name, value in list(mobject.__dict__.items()):
            if not isinstance(value, _BridgeMobject):
                continue
            for index, member in enumerate(source_family):
                if value is member:
                    setattr(self, name, receiver_family[index])
                    break
        return self

    def save_state(self, use_deepcopy=False):
        # Keep the Reference-visible family copy. This is also the exact
        # pre-adoption state Restore needs when scene code calls save_state
        # on a detached graph and mutates it before its first play.
        self.saved_state = self.copy(deep=use_deepcopy)
        self.saved_state.target = self.target
        if self._is_bound():
            self._link_saved_state(self.saved_state)
        return self

    def restore(self):
        if self.saved_state is None:
            raise Exception("Trying to restore without having saved")
        saved_state = self.saved_state
        saved_family = _family_preorder(saved_state)
        if self._is_bound():
            self.become(saved_state)
            current_family = _family_preorder(self)
        else:
            # Detached parent/child relationships live in Python until scene
            # adoption. save_state created an isomorphic family copy, so
            # restore each paired nursery root without pretending the
            # detached parent nursery already owns the whole graph.
            current_family = _family_preorder(self)
            if len(current_family) != len(saved_family) or any(
                len(current.submobjects) != len(saved.submobjects)
                for current, saved in zip(current_family, saved_family)
            ):
                raise RuntimeError(
                    "become between families of different shapes "
                    "(family alignment lands with fm-cye)"
                )
            if any(
                current._engine_state()["fields"] != saved._engine_state()["fields"]
                for current, saved in zip(current_family, saved_family)
            ):
                raise RuntimeError("become between records of different schemas")
            for current, saved in zip(current_family, saved_family):
                current._become(saved, False)
        # Reference become remaps named family-member attributes from the
        # source family onto the corresponding members of the destination.
        for name, value in list(saved_state.__dict__.items()):
            if isinstance(value, _BridgeMobject) and value in saved_family:
                setattr(self, name, current_family[saved_family.index(value)])
        return self

    @property
    def animate(self):
        """Methods called with mobject.animate.method(...) build a
        MoveToTarget-shaped play spec against a generated target."""
        return _AnimationBuilder(self)

    @property
    def always(self):
        """Methods called with mobject.always.method(*args, **kwargs) run
        mobject.method(*args, **kwargs) on every frame."""
        return _UpdaterBuilder(self)

    @property
    def f_always(self):
        """Like `always`, but the arguments are functions returning the
        per-frame values."""
        return _FunctionalUpdaterBuilder(self)

    def _dispatch_updater(self, updater, dt):
        # The Reference's updater protocol is name-based: only a callback
        # whose code declares ``dt`` receives the frame delta.  Counting
        # positional parameters is observably wrong for common helpers such
        # as ``update(mob, vertical=False, horizontal=False)`` because the
        # delta is then mistaken for the helper's first semantic option.
        try:
            accepts_dt = "dt" in updater.__code__.co_varnames
        except AttributeError:
            # Callable objects and extension callables have no ``__code__``.
            # Use a named parameter when Python exposes one, and preserve the
            # two-argument native rung for opaque/variadic PyO3 callables.
            try:
                signature = _inspect.signature(updater)
            except (TypeError, ValueError):
                accepts_dt = True
            else:
                accepts_dt = "dt" in signature.parameters or any(
                    parameter.kind is parameter.VAR_POSITIONAL
                    for parameter in signature.parameters.values()
                )
        if accepts_dt:
            return updater(self, dt=dt)
        return updater(self)

    def copy(self, deep=False):
        return _copy_mobject_graph(self, bool(deep), {})

    def deepcopy(self):
        return _copy.deepcopy(self)

    def __copy__(self):
        return _copy_mobject_graph(self, False, {})

    def __deepcopy__(self, memo):
        return _copy_mobject_graph(self, True, memo)

    def __reduce_ex__(self, protocol):
        del protocol
        internal = {"submobjects", "uniforms", "updaters", "_scene"}
        attributes = {
            key: value
            for key, value in self.__dict__.items()
            if key not in internal
        }
        return (
            _restore_mobject,
            (
                type(self),
                self._engine_state(),
                attributes,
                list(self.submobjects),
                dict(self.uniforms._extras),
                list(self.updaters),
            ),
        )


class Group(Mobject):
    """The Reference's heterogeneous Mobject group.

    This class is explicit rather than schema-synthesized because its
    variadic member constructor is real behavior, not Mobject's ordinary
    constructor defaults under a different public name.
    """

    def __init__(self, *mobjects, **kwargs):
        super().__init__(**kwargs)
        self._ingest_args(*mobjects)

    def _ingest_args(self, *args):
        if len(args) == 0:
            return
        if all(isinstance(mob, _BridgeMobject) for mob in args):
            self.add(*args)
        elif isinstance(args[0], _collections_abc.Iterable):
            self.add(*args[0])
        else:
            raise Exception(f"Invalid argument to Group of type {type(args[0])}")


class Point(Mobject):
    """The Reference's Point (manimlib/mobject/mobject.py at the pin): one
    location record with artificial box extents. State-real — the location
    lives in the engine record, so every positional operation and read
    round-trips through the one Stage code path."""

    def __init__(
        self,
        location=_ORIGIN,
        artificial_width=1e-6,
        artificial_height=1e-6,
        **kwargs,
    ):
        self.artificial_width = artificial_width
        self.artificial_height = artificial_height
        super().__init__(**kwargs)
        self.set_location(location)

    def get_width(self):
        return self.artificial_width

    def get_height(self):
        return self.artificial_height

    def get_location(self):
        return _np.array(self.get_field("point", 0), dtype=float)

    def get_bounding_box_point(self, *args, **kwargs):
        del args, kwargs
        return self.get_location()

    def set_location(self, new_loc):
        self.resize(1)
        self.set_field("point", 0, list(_vec3(new_loc)))
        return self


class _EventTypeMeta(_enum.EnumMeta):
    """Permit the schema census's empty subclass probe, and nothing broader."""

    @classmethod
    def _check_for_existing_members_(metacls, class_name, bases):
        if class_name.startswith("_SubclassProbe_"):
            return
        return super()._check_for_existing_members_(class_name, bases)

    def __new__(metacls, class_name, bases, classdict, **kwargs):
        if class_name.startswith("_SubclassProbe_"):
            member_names = getattr(
                classdict,
                "member_names",
                getattr(classdict, "_member_names", ()),
            )
            if member_names:
                raise TypeError("EventType probe subclasses must be empty")
        return super().__new__(
            metacls,
            class_name,
            bases,
            classdict,
            **kwargs,
        )


class EventType(_enum.Enum, metaclass=_EventTypeMeta):
    """The Reference's event-handler token set."""

    MouseMotionEvent = "mouse_motion_event"
    MouseDragEvent = "mouse_drag_event"
    MousePressEvent = "mouse_press_event"
    MouseReleaseEvent = "mouse_release_event"
    MouseScrollEvent = "mouse_scroll_event"
    KeyPressEvent = "key_press_event"
    KeyReleaseEvent = "key_release_event"


class EventListener:
    """A mobject callback registered with an EventDispatcher."""

    def __init__(self, mobject, event_type, event_callback):
        if not isinstance(mobject, Mobject):
            raise TypeError("EventListener mobject must be a Mobject")
        if not callable(event_callback):
            raise TypeError("EventListener event_callback must be callable")
        self.mobject = mobject
        self.event_type = event_type
        self.callback = event_callback


class EventDispatcher:
    """In-process event fan-out; Studio owns the window, this owns listeners."""

    def __init__(self):
        self.event_listners = {event_type: [] for event_type in EventType}
        self.mouse_point = Point()
        self.mouse_drag_point = Point()
        self.pressed_keys = set()
        # The Reference exposes the corrected spelling as the typo's exact
        # alias. Cache one bound method under both names so instance identity
        # (`dispatcher.add_listener is dispatcher.add_listner`) is stable.
        listener_adder = self.add_listner
        self.add_listner = listener_adder
        self.add_listener = listener_adder

    def add_listner(self, event_listner):
        if not isinstance(event_listner, EventListener):
            raise TypeError("add_listner expects an EventListener")
        bucket = self.event_listners.setdefault(event_listner.event_type, [])
        if event_listner not in bucket:
            bucket.append(event_listner)

    add_listener = add_listner

    def remove_listner(self, event_listner):
        bucket = self.event_listners.get(event_listner.event_type)
        if not bucket:
            return
        self.event_listners[event_listner.event_type] = [
            item for item in bucket if item is not event_listner
        ]

    def dispatch(self, event_type, **event_data):
        point = event_data.get("point")
        if point is not None:
            if event_type == EventType.MouseDragEvent:
                self.mouse_drag_point.move_to(point)
            else:
                self.mouse_point.move_to(point)
        symbol = event_data.get("symbol")
        if event_type == EventType.KeyPressEvent and symbol is not None:
            self.pressed_keys.add(int(symbol))
        elif event_type == EventType.KeyReleaseEvent and symbol is not None:
            self.pressed_keys.discard(int(symbol))
        for listener in list(self.event_listners.get(event_type, ())):
            if listener.callback(listener.mobject, event_data) is False:
                return False
        return None

    def get_listners_count(self):
        return sum(len(bucket) for bucket in self.event_listners.values())

    def get_mouse_point(self):
        return self.mouse_point

    def get_mouse_drag_point(self):
        return self.mouse_drag_point

    def is_key_pressed(self, symbol):
        return int(symbol) in self.pressed_keys


class VMobject(Mobject):
    pre_function_handle_to_anchor_scale_factor = 0.01
    make_smooth_after_applying_functions = False
    tolerance_for_point_equality = 1e-8
    long_lines = False
    joint_type_map = {
        "no_joint": 0,
        "auto": 1,
        "bevel": 2,
        "miter": 3,
    }

    data_dtype = [
        ("point", 3),
        ("stroke_rgba", 4),
        ("stroke_width", 1),
        ("joint_angle", 1),
        ("fill_rgba", 4),
        ("base_normal", 3),
        ("fill_border_width", 1),
    ]

    def __init__(
        self,
        color=None,
        fill_color=None,
        fill_opacity=0.0,
        stroke_color=None,
        stroke_opacity=1.0,
        stroke_width=4.0,
        stroke_behind=False,
        background_image_file=None,
        long_lines=False,
        joint_type="auto",
        flat_stroke=False,
        scale_stroke_with_zoom=False,
        use_simple_quadratic_approx=False,
        anti_alias_width=1.5,
        fill_border_width=0.0,
        **kwargs,
    ):
        config = _pinned_manim_config().vmobject
        self.fill_color = (
            fill_color
            if fill_color is not None
            else color if color is not None else config.default_fill_color
        )
        self.fill_opacity = fill_opacity
        self.stroke_color = (
            stroke_color
            if stroke_color is not None
            else color if color is not None else config.default_stroke_color
        )
        self.stroke_opacity = stroke_opacity
        self.stroke_width = stroke_width
        self.stroke_behind = bool(stroke_behind)
        self.background_image_file = background_image_file
        self.long_lines = bool(long_lines)
        self.joint_type = joint_type
        self.flat_stroke = bool(flat_stroke)
        self.scale_stroke_with_zoom = bool(scale_stroke_with_zoom)
        self.use_simple_quadratic_approx = bool(use_simple_quadratic_approx)
        self.anti_alias_width = float(anti_alias_width)
        self.fill_border_width = fill_border_width
        self.needs_new_joint_angles = True
        self.needs_new_unit_normal = True
        self.subpath_end_indices = None
        self.outer_vert_indices = _np.zeros(0, dtype=int)
        self.shader_program_type = None

        super().__init__(**kwargs)
        try:
            joint_code = self.joint_type_map[joint_type]
        except KeyError as error:
            raise ValueError(f"unknown VMobject joint type: {joint_type}") from error
        self.uniforms["anti_alias_width"] = self.anti_alias_width
        self.uniforms["joint_type"] = joint_code
        self.uniforms["flat_stroke"] = self.flat_stroke
        self.uniforms["scale_stroke_with_zoom"] = self.scale_stroke_with_zoom
        self.set_stroke(
            color=self.stroke_color,
            width=self.stroke_width,
            opacity=self.stroke_opacity,
            behind=self.stroke_behind,
        )
        self.set_fill(
            color=self.fill_color,
            opacity=self.fill_opacity,
            border_width=self.fill_border_width,
        )

    def get_group_class(self):
        return getattr(_FMN_ROOT, "VGroup", VMobject)

    def set_points(self, points):
        if len(points) != 0 and len(points) % 2 != 1:
            raise AssertionError
        Mobject.set_points(self, points)
        self._refresh_vmobject_path_metadata()
        self.needs_new_unit_normal = True
        return self

    def append_points(self, points):
        if len(points) % 2 != 0:
            raise AssertionError
        final_length = self.get_num_points() + len(points)
        if final_length != 0 and final_length % 2 != 1:
            raise AssertionError
        Mobject.append_points(self, points)
        self._refresh_vmobject_path_metadata()
        self.needs_new_unit_normal = True
        return self

    def set_anchors_and_handles(self, anchors, handles):
        points = self._set_anchors_and_handles_points(
            [_vec3(anchor) for anchor in anchors],
            [_vec3(handle) for handle in handles],
        )
        self.set_points(points)
        return self

    def start_new_path(self, point):
        was_empty = self.get_num_points() == 0
        points = self._start_new_path_points(_vec3(point))
        if was_empty:
            self.set_points(points)
        else:
            self.append_points(points)
        return self

    def add_cubic_bezier_curve(
        self,
        anchor1,
        handle1,
        handle2,
        anchor2,
    ):
        was_empty = self.get_num_points() == 0
        points = self._add_cubic_bezier_curve_points(
            _vec3(anchor1),
            _vec3(handle1),
            _vec3(handle2),
            _vec3(anchor2),
        )
        if was_empty:
            self.set_points(points)
        else:
            self.append_points(points)
        return self

    def add_cubic_bezier_curve_to(self, handle1, handle2, anchor):
        points = self._add_cubic_bezier_curve_to_points(
            _vec3(handle1),
            _vec3(handle2),
            _vec3(anchor),
        )
        if len(points) > 0:
            self.append_points(points)
        return self

    def add_quadratic_bezier_curve_to(
        self,
        handle,
        anchor,
        allow_null_curve=True,
    ):
        points = self._add_quadratic_bezier_curve_to_points(
            _vec3(handle),
            _vec3(anchor),
            bool(allow_null_curve),
        )
        if len(points) > 0:
            self.append_points(points)
        return self

    def add_line_to(self, point, allow_null_line=True):
        points = self._add_line_to_points(
            _vec3(point),
            bool(allow_null_line),
        )
        if len(points) > 0:
            self.append_points(points)
        return self

    def add_smooth_curve_to(self, point):
        points = self._add_smooth_curve_to_points(_vec3(point))
        if len(points) > 0:
            self.append_points(points)
        return self

    def add_smooth_cubic_curve_to(self, handle, point):
        points = self._add_smooth_cubic_curve_to_points(
            _vec3(handle),
            _vec3(point),
        )
        if len(points) > 0:
            self.append_points(points)
        return self

    def add_arc_to(self, point, angle, n_components=None, threshold=1e-3):
        points = self._add_arc_to_points(
            _vec3(point),
            float(angle),
            n_components,
            float(threshold),
        )
        if len(points) > 0:
            self.append_points(points)
        return self

    def add_points_as_corners(self, points):
        new_points = self._add_points_as_corners_points(
            [_vec3(point) for point in points]
        )
        if len(new_points) > 0:
            self.append_points(new_points)
        return self

    def add_subpath(self, points):
        was_empty = self.get_num_points() == 0
        new_points = self._add_subpath_points([_vec3(point) for point in points])
        if len(new_points) > 0:
            if was_empty:
                self.set_points(new_points)
            else:
                self.append_points(new_points)
        return self

    def append_vectorized_mobject(self, vmobject):
        # Reference order is observable for aliases: splice the source's
        # current path first, then ask for its current record count and copy
        # every structured lane over the appended tail.
        self.add_subpath(vmobject.get_points())
        n_points = vmobject.get_num_points()
        self.data[-n_points:] = vmobject.data
        return self

    def has_new_path_started(self):
        return self._has_new_path_started()

    def get_last_point(self):
        return self.get_points()[-1]

    def get_reflection_of_last_handle(self):
        points = self.get_points()
        return 2 * points[-1] - points[-2]

    def close_path(self, smooth=False):
        points = self._close_path_points(bool(smooth))
        if len(points) > 0:
            self.append_points(points)
        return self

    def is_closed(self):
        return self._is_path_closed()

    def consider_points_equal(self, p0, p1):
        return self._consider_path_points_equal(_vec3(p0), _vec3(p1))

    def set_points_as_corners(self, points):
        # Stage::set_points preserves existing records, but the first resize
        # of an empty RecordBuffer is necessarily zero-filled.  The Reference
        # seeds that first run from `_data_defaults`, whose VMobject defaults
        # carry transparent fill and opaque stroke.  Retain those non-geometry
        # lanes while Chisel remains the sole owner of corner-path geometry and
        # joint-angle derivation.
        defaults = self._style_data().copy() if self.get_num_points() == 0 else None
        self._set_points_as_corners([_vec3(point) for point in points])
        if defaults is not None and self.get_num_points() > 0:
            data = self.data
            for field in (
                "stroke_rgba",
                "stroke_width",
                "fill_rgba",
                "base_normal",
                "fill_border_width",
            ):
                data[field][:] = defaults[field]
        return self

    def set_points_smoothly(self, points, approx=True):
        self.set_points_as_corners(points)
        self.make_smooth(approx=approx)
        return self

    def get_num_curves(self):
        return self.get_num_points() // 2

    def get_anchors_and_handles(self):
        points = self.get_points()
        return [points[0:-1:2], points[1::2], points[2::2]]

    def get_start_anchors(self):
        return self.get_points()[0:-1:2]

    def get_end_anchors(self):
        return self.get_points()[2::2]

    def get_anchors(self):
        return self.get_points()[::2]

    def get_bezier_tuples_from_points(self, points):
        n_curves = (len(points) - 1) // 2
        return (points[2 * index : 2 * index + 3] for index in range(n_curves))

    def get_bezier_tuples(self):
        return self.get_bezier_tuples_from_points(self.get_points())

    def get_nth_curve_points(self, n):
        assert n < self.get_num_curves()
        return self.get_points()[2 * n : 2 * n + 3]

    def get_nth_curve_function(self, n):
        points = self.get_nth_curve_points(n)

        def curve(alpha):
            return (
                (1.0 - alpha) ** 2 * points[0]
                + 2.0 * (1.0 - alpha) * alpha * points[1]
                + alpha**2 * points[2]
            )

        return curve

    def get_subpath_end_indices_from_points(self, points):
        points = _np.asarray(points)
        tolerance = 1e-4
        starts, handles, ends = points[0:-1:2], points[1::2], points[2::2]
        is_end = (starts == handles).all(1) & (
            _np.abs(handles - ends) > tolerance
        ).any(1)
        end_indices = (2 * index for index, end in enumerate(is_end) if end)
        return _np.array([*end_indices, len(points) - 1])

    def get_subpath_end_indices(self):
        # Recompute from the live RecordBuffer view. The Reference caches this
        # derived list behind explicit dirty flags; the portal's writable
        # NumPy views can change at any instant, so caching here would make a
        # direct view write invisibly stale.
        return self.get_subpath_end_indices_from_points(self.get_points())

    def get_subpaths_from_points(self, points):
        if len(points) == 0:
            return []
        end_indices = self.get_subpath_end_indices_from_points(points)
        start_indices = [0, *(end_indices[:-1] + 2)]
        return [
            points[start : end + 1]
            for start, end in zip(start_indices, end_indices)
        ]

    def get_subpaths(self):
        return self.get_subpaths_from_points(self.get_points())

    def get_points_without_null_curves(self, atol=1e-9):
        return _np.array(
            self._get_points_without_null_curves(float(atol)), dtype=float
        )

    def insert_n_curves_to_point_list(self, n, points):
        return _np.array(
            self._insert_n_curves_to_point_list(
                n,
                [_vec3(point) for point in points],
                self.tolerance_for_point_equality,
            )
        )

    def insert_n_curves(self, n, recurse=True):
        for mob in _family_preorder(self) if recurse else [self]:
            if mob.get_num_curves() > 0:
                mob.set_points(
                    mob.insert_n_curves_to_point_list(n, mob.get_points())
                )
        return self

    def align_points(self, vmobject):
        aligned = self._align_vmobject_points(
            vmobject, float(self.tolerance_for_point_equality)
        )
        if aligned is not None:
            self_points, other_points = aligned
            self.set_points(self_points)
            vmobject.set_points(other_points)
        self.needs_new_unit_normal = True
        vmobject.needs_new_unit_normal = True
        return self

    def subdivide_curves_by_condition(
        self, tuple_to_subdivisions, recurse=True
    ):
        targets = _family_preorder(self) if recurse else [self]
        planned = []
        for mob in targets:
            if not mob.has_points():
                continue
            counts = []
            for curve in mob.get_bezier_tuples():
                count = tuple_to_subdivisions(*curve)
                counts.append(_operator.index(count) if count > 0 else 0)
            planned.append(
                (mob, mob._subdivide_curve_points_by_counts(counts))
            )
        # Callback exceptions, count coercion errors, and Chisel budget
        # refusals above leave every requested family member untouched.
        for mob, points in planned:
            mob.set_points(points)
        return self

    def subdivide_sharp_curves(
        self, angle_threshold=30 * _DEG, recurse=True
    ):
        targets = _family_preorder(self) if recurse else [self]
        # Plan the complete requested family before the first RecordBuffer
        # write. Chisel's threshold/budget refusal is therefore atomic even
        # when a later descendant is the malformed member.
        planned = [
            (mob, mob._subdivide_sharp_curve_points(float(angle_threshold)))
            for mob in targets
            if mob.has_points()
        ]
        for mob, points in planned:
            mob.set_points(points)
        return self

    def subdivide_intersections(self, recurse=True, n_subdivisions=1):
        path = [_vec3(point) for point in self.get_anchors()]
        targets = _family_preorder(self) if recurse else [self]
        planned = [
            (
                mob,
                mob._subdivide_intersection_curve_points(
                    path, n_subdivisions
                ),
            )
            for mob in targets
            if mob.has_points()
        ]
        for mob, points in planned:
            mob.set_points(points)
        return self

    def quick_point_from_proportion(self, alpha):
        # Reference VMobject's equal-curve-count approximation. This remains
        # distinct from FrankenManim's deliberately improved true-arclength
        # `point_from_proportion`, and is needed by CoordinateSystem's pinned
        # function-less graph search.
        num_curves = self.get_num_curves()
        if num_curves == 0:
            return self.get_center()
        alpha = float(alpha)
        if alpha >= 1.0:
            curve_index, residue = num_curves - 1, 1.0
        elif alpha <= 0.0:
            curve_index, residue = 0, 0.0
        else:
            value = num_curves * alpha
            curve_index, residue = int(value), value % 1.0
        points = self.get_points()[2 * curve_index : 2 * curve_index + 3]
        return (
            (1.0 - residue) ** 2 * points[0]
            + 2.0 * (1.0 - residue) * residue * points[1]
            + residue**2 * points[2]
        )

    def make_smooth(self, approx=True, recurse=True):
        for submob in _family_preorder(self) if recurse else [self]:
            submob._make_smooth(bool(approx))
        return self

    def is_smooth(self, angle_tol=1 * _DEG):
        return self._is_path_smooth(float(angle_tol))

    def change_anchor_mode(self, mode):
        self._change_anchor_mode(mode)
        return self

    def make_approximately_smooth(self, recurse=True):
        return self.make_smooth(approx=True, recurse=recurse)

    def make_jagged(self, recurse=True):
        for submob in _family_preorder(self) if recurse else [self]:
            submob._change_anchor_mode("jagged")
        return self

    def reverse_points(self, recurse=True):
        recurse = bool(recurse)
        if self._is_bound():
            # One bound Stage owns the complete native family and preserves
            # the Reference's separate repair and base-row reversal scopes.
            self._reverse_points(recurse)
            return self

        # Detached proxies own one nursery apiece, so Python owns the family
        # distribution just as it does for positional operations. These are
        # the Reference's two exact phases over live RecordBuffer views.
        family = _family_preorder(self)
        for mob in family if recurse else [self]:
            if not mob.has_points():
                continue
            inner_ends = mob.get_subpath_end_indices()[:-1]
            mob.data["point"][inner_ends + 1] = mob.data["point"][inner_ends + 2]
            mob.data["base_normal"][1::2] *= -1
        for mob in family:
            mob.data[:] = mob.data[::-1]
        return self

    def apply_function(self, function, make_smooth=False, **kwargs):
        Mobject.apply_function(self, function, **kwargs)
        if getattr(self, "make_smooth_after_applying_functions", False) or make_smooth:
            self.make_smooth(approx=True)
        return self

    def apply_matrix(self, *args, **kwargs):
        return Mobject.apply_matrix(self, *args, **kwargs)

    def get_arc_length(self, n_sample_points=None):
        # BN-03: Chisel's error-bounded true length is the definition. The
        # Reference sampling knob remains accepted for source compatibility,
        # but cannot weaken the answer back to a chord approximation.
        del n_sample_points
        return self._get_arc_length()

    def point_from_proportion(self, alpha):
        return _np.array(self._point_from_proportion(float(alpha)))

    def get_area_vector(self):
        return _np.array(self._path_area_vector())

    def get_unit_normal(self, refresh=False):
        if self.get_num_points() < 3:
            return _np.array((0.0, 0.0, 1.0))
        if not self.needs_new_unit_normal and not refresh:
            return self.data["base_normal"][1, :]
        normal = _np.array(self._path_unit_normal())
        self.data["base_normal"][1::2] = normal
        self.needs_new_unit_normal = False
        return normal

    def pointwise_become_partial(self, vmobject, a, b):
        if not isinstance(vmobject, VMobject):
            raise AssertionError("pointwise_become_partial expects a VMobject")
        self._pointwise_become_partial(vmobject, float(a), float(b))
        return self

    def get_subcurve(self, a, b):
        vmobject = self.copy()
        vmobject.pointwise_become_partial(self, a, b)
        return vmobject

    # Style surface (fm-d3gt): Reference bodies (vectorized_mobject.py at
    # the pin) over the live engine record views (`stroke_rgba`,
    # `stroke_width`, `fill_rgba`, `fill_border_width`) and the typed
    # engine uniforms (`stroke_behind`, `flat_stroke`, `shading`,
    # `anti_alias_width`). Family recursion is the Python family list —
    # identical in both proxy states.

    def set_fill(self, color=None, opacity=None, border_width=None, recurse=True):
        self.set_rgba_array_by_color(color, opacity, "fill_rgba", recurse)
        if border_width is not None:
            self.border_width = border_width
            for mob in _family_preorder(self) if recurse else [self]:
                mob._style_data()["fill_border_width"] = border_width
        return self

    def set_stroke(
        self,
        color=None,
        width=None,
        opacity=None,
        behind=None,
        flat=None,
        recurse=True,
        background=None,
    ):
        # R13's pinned era shim: pre-2023 manimgl called this knob
        # `background`; the Reference pin renamed it to `behind`.
        if background is not None:
            if behind is not None and bool(behind) != bool(background):
                raise TypeError("set_stroke received conflicting behind/background values")
            behind = background
        self.set_rgba_array_by_color(color, opacity, "stroke_rgba", recurse)

        if width is not None:
            for mob in _family_preorder(self) if recurse else [self]:
                data = mob._style_data()
                if isinstance(width, (float, int, _np.floating)):
                    data["stroke_width"][:, 0] = width
                else:
                    data["stroke_width"][:, 0] = _resize_with_interpolation(
                        _np.array(width), len(data)
                    ).flatten()

        if behind is not None:
            for mob in _family_preorder(self) if recurse else [self]:
                mob.uniforms["stroke_behind"] = bool(behind)

        if flat is not None:
            self.set_flat_stroke(flat)

        return self

    def set_backstroke(self, color="#000000", width=3, background=None):
        behind = True if background is None else bool(background)
        self.set_stroke(color, width, behind=behind)
        return self

    def set_flat_stroke(self, flat_stroke=True, recurse=True):
        for mob in _family_preorder(self) if recurse else [self]:
            mob.uniforms["flat_stroke"] = bool(flat_stroke)
        return self

    def get_flat_stroke(self):
        return bool(self.uniforms["flat_stroke"])

    def set_joint_type(self, joint_type, recurse=True):
        joint_code = self.joint_type_map[joint_type]
        for mob in _family_preorder(self) if recurse else [self]:
            mob.uniforms["joint_type"] = joint_code
        return self

    def get_joint_type(self):
        return self.uniforms["joint_type"]

    def set_anti_alias_width(self, anti_alias_width, recurse=True):
        for mob in _family_preorder(self) if recurse else [self]:
            mob.uniforms["anti_alias_width"] = float(anti_alias_width)
        return self

    def get_anti_alias_width(self):
        return float(self.uniforms["anti_alias_width"])

    def set_style(
        self,
        fill_color=None,
        fill_opacity=None,
        fill_rgba=None,
        fill_border_width=None,
        stroke_color=None,
        stroke_opacity=None,
        stroke_rgba=None,
        stroke_width=None,
        stroke_behind=None,
        flat_stroke=None,
        shading=None,
        recurse=True,
    ):
        for mob in _family_preorder(self) if recurse else [self]:
            if fill_rgba is not None:
                data = mob._style_data()
                data["fill_rgba"][:] = _resize_with_interpolation(
                    _np.asarray(fill_rgba, dtype=float), len(data["fill_rgba"])
                )
            else:
                mob.set_fill(
                    color=fill_color,
                    opacity=fill_opacity,
                    border_width=fill_border_width,
                    recurse=False,
                )

            if stroke_rgba is not None:
                data = mob._style_data()
                data["stroke_rgba"][:] = _resize_with_interpolation(
                    _np.asarray(stroke_rgba, dtype=float), len(data["stroke_rgba"])
                )
                mob.set_stroke(
                    width=stroke_width,
                    behind=stroke_behind,
                    flat=flat_stroke,
                    recurse=False,
                )
            else:
                mob.set_stroke(
                    color=stroke_color,
                    width=stroke_width,
                    opacity=stroke_opacity,
                    flat=flat_stroke,
                    behind=stroke_behind,
                    recurse=False,
                )

            if shading is not None:
                mob.set_shading(*shading, recurse=False)
        return self

    def get_style(self):
        data = self._style_data()
        return {
            "fill_rgba": data["fill_rgba"].copy(),
            "fill_border_width": data["fill_border_width"].copy(),
            "stroke_rgba": data["stroke_rgba"].copy(),
            "stroke_width": data["stroke_width"].copy(),
            "stroke_behind": bool(self.uniforms["stroke_behind"]),
            "flat_stroke": self.get_flat_stroke(),
            "shading": tuple(self.get_shading()),
        }

    def match_style(self, vmobject, recurse=True):
        self.set_style(**vmobject.get_style(), recurse=False)
        if recurse:
            # Does its best to match up submobject lists, and match styles
            # accordingly.
            submobs1, submobs2 = self.submobjects, vmobject.submobjects
            if len(submobs1) == 0:
                return self
            elif len(submobs2) == 0:
                submobs2 = [vmobject]
            for sm1, sm2 in zip(*_make_even(submobs1, submobs2)):
                sm1.match_style(sm2)
        return self

    def set_color(self, color, opacity=None, recurse=True):
        self.set_fill(color, opacity=opacity, recurse=recurse)
        self.set_stroke(color, opacity=opacity, recurse=recurse)
        return self

    def set_opacity(self, opacity, recurse=True):
        self.set_fill(opacity=opacity, recurse=recurse)
        self.set_stroke(opacity=opacity, recurse=recurse)
        return self

    def fade(self, darkness=0.5, recurse=True):
        mobs = _family_preorder(self) if recurse else [self]
        for mob in mobs:
            factor = 1.0 - darkness
            mob.set_fill(opacity=factor * mob.get_fill_opacity(), recurse=False)
            mob.set_stroke(
                opacity=factor * mob.get_stroke_opacity(), recurse=False
            )
        return self

    def get_fill_colors(self):
        return [
            _rgb_to_hex(rgba[:3]) for rgba in self._style_data()["fill_rgba"]
        ]

    def get_fill_opacities(self):
        return self._style_data()["fill_rgba"][:, 3]

    def get_stroke_colors(self):
        return [
            _rgb_to_hex(rgba[:3]) for rgba in self._style_data()["stroke_rgba"]
        ]

    def get_stroke_opacities(self):
        return self._style_data()["stroke_rgba"][:, 3]

    def get_stroke_widths(self):
        return self._style_data()["stroke_width"][:, 0]

    def get_fill_color(self):
        return _rgb_to_hex(self._style_data()["fill_rgba"][0, :3])

    def get_fill_opacity(self):
        return float(self._style_data()["fill_rgba"][0, 3])

    def get_stroke_color(self):
        return _rgb_to_hex(self._style_data()["stroke_rgba"][0, :3])

    def get_stroke_width(self):
        return float(self._style_data()["stroke_width"][0, 0])

    def get_stroke_opacity(self):
        return float(self._style_data()["stroke_rgba"][0, 3])

    def get_color(self):
        if self.has_fill():
            return self.get_fill_color()
        return self.get_stroke_color()

    def get_opacity(self):
        if self.has_fill():
            return self.get_fill_opacity()
        return self.get_stroke_opacity()

    def has_stroke(self):
        data = self._style_data()
        return bool(any(data["stroke_width"].flatten())) and bool(
            any(data["stroke_rgba"][:, 3])
        )

    def has_fill(self):
        return bool(any(self._style_data()["fill_rgba"][:, 3]))


# ---------------------------------------------------------------------------
# The native-builder seam (fm-d3gt): the classes below construct by calling
# an fmn-library builder through the engine, which installs the built family
# across proxy nurseries — the root's records replace the constructing
# proxy's nursery and every descendant arrives as a fresh VMobject shell in
# the nested specs `_hang_native_children` walks. Native geometry is the ONE
# implementation (D4); no point math is re-derived here.


def _native_shell_factory():
    shell = VMobject.__new__(VMobject)
    _install_live_state(shell)
    return shell


def _hang_native_children(parent, specs):
    shells = []
    for shell, child_specs in specs:
        _hang_native_children(shell, child_specs)
        shells.append(shell)
    if shells:
        parent.submobjects.extend(shells)


def _apply_vmobject_style_kwargs(mob, kwargs, recurse=True):
    """The Reference's VMobject style constructor keywords, applied after a
    native build (its init_colors pass). Unknown keywords refuse."""
    _preflight_vmobject_style_kwargs(kwargs)
    color = kwargs.pop("color", None)
    opacity = kwargs.pop("opacity", None)
    fill_color = kwargs.pop("fill_color", None)
    fill_opacity = kwargs.pop("fill_opacity", None)
    stroke_color = kwargs.pop("stroke_color", None)
    stroke_width = kwargs.pop("stroke_width", None)
    stroke_opacity = kwargs.pop("stroke_opacity", None)
    stroke_behind = kwargs.pop("stroke_behind", None)
    flat_stroke = kwargs.pop("flat_stroke", None)
    fill_border_width = kwargs.pop("fill_border_width", None)
    shading = kwargs.pop("shading", None)
    if kwargs:
        raise TypeError(
            "unexpected keyword arguments: " + ", ".join(sorted(kwargs))
        )
    # `VMobject(color=...)` is the one-shot public override for both channels.
    # Constructor-specific defaults (notably Dot's white fill and black
    # zero-width stroke) must not mask that shorthand when it is supplied.
    if color is not None:
        fill_color = color
        stroke_color = color
    if fill_opacity is None:
        fill_opacity = opacity
    if stroke_opacity is None:
        stroke_opacity = opacity
    if any(
        value is not None
        for value in (stroke_color, stroke_width, stroke_opacity, stroke_behind, flat_stroke)
    ):
        mob.set_stroke(
            color=stroke_color,
            width=stroke_width,
            opacity=stroke_opacity,
            behind=stroke_behind,
            flat=flat_stroke,
            recurse=recurse,
        )
    if any(value is not None for value in (fill_color, fill_opacity, fill_border_width)):
        mob.set_fill(
            color=fill_color,
            opacity=fill_opacity,
            border_width=fill_border_width,
            recurse=recurse,
        )
    if shading is not None:
        mob.set_shading(*shading, recurse=recurse)
    return mob


_NATIVE_VMOBJECT_STYLE_KEYS = frozenset(
    {
        "color",
        "opacity",
        "fill_color",
        "fill_opacity",
        "stroke_color",
        "stroke_width",
        "stroke_opacity",
        "stroke_behind",
        "flat_stroke",
        "fill_border_width",
    }
)


def _preflight_vmobject_style_kwargs(kwargs):
    unknown = set(kwargs) - _NATIVE_VMOBJECT_STYLE_KEYS - {"shading"}
    if unknown:
        raise TypeError(
            "unexpected keyword arguments: " + ", ".join(sorted(unknown))
        )


def _split_native_vgroup3d_kwargs(class_name, kwargs, default_shading):
    """Preflight the shared vectorized-solid kwargs before native install."""
    style = dict(kwargs)
    depth_test = bool(style.pop("depth_test", True))
    shading = tuple(style.pop("shading", default_shading))
    joint_type = style.pop("joint_type", "no_joint")
    if joint_type not in VMobject.joint_type_map:
        raise KeyError(joint_type)
    unknown = sorted(set(style) - _NATIVE_VMOBJECT_STYLE_KEYS)
    _refuse_unrouted(class_name, [(name, True) for name in unknown])
    return style, depth_test, shading, joint_type


def _apply_vgroup3d_config(mob, depth_test, shading, joint_type):
    mob.set_shading(*shading)
    mob.set_joint_type(joint_type)
    if depth_test:
        mob.apply_depth_test()
    else:
        mob.deactivate_depth_test()
    return mob


class VGroup(VMobject):
    """The Reference's VGroup: a VMobject holding children. Defined here
    (rather than schema-synthesized) so the native coordinate classes can
    keep the Reference MRO `Axes < VGroup < VMobject`."""

    def __init__(self, *vmobjects, **kwargs):
        super().__init__(**kwargs)
        if any(
            isinstance(vmob, _BridgeMobject) and not isinstance(vmob, VMobject)
            for vmob in vmobjects
        ):
            raise Exception("Only VMobjects can be passed into VGroup")
        self._ingest_args(*vmobjects)


class VectorizedPoint(Point, VMobject):
    """The Reference's invisible one-record VMobject location marker."""

    def __init__(
        self,
        location=_ORIGIN,
        color=_BLACK,
        fill_opacity=0.0,
        stroke_width=0.0,
        **kwargs,
    ):
        self.artificial_width = kwargs.pop("artificial_width", 1e-6)
        self.artificial_height = kwargs.pop("artificial_height", 1e-6)
        _install_live_state(self)
        specs = self._build_vectorized_point(
            _native_shell_factory, _vec3(location)
        )
        _hang_native_children(self, specs)
        kwargs.setdefault("color", color)
        kwargs.setdefault("fill_opacity", fill_opacity)
        kwargs.setdefault("stroke_width", stroke_width)
        _apply_vmobject_style_kwargs(self, kwargs)


class CurvesAsSubmobjects(VGroup):
    """One native shared-anchor child for each source quadratic curve."""

    def __init__(self, vmobject, **kwargs):
        if not isinstance(vmobject, VMobject):
            raise TypeError("CurvesAsSubmobjects expects a VMobject")
        _install_live_state(self)
        specs = self._build_curves_as_submobjects(
            _native_shell_factory, vmobject
        )
        _hang_native_children(self, specs)
        _apply_vmobject_style_kwargs(self, kwargs)
        for part in self.submobjects:
            part.match_style(vmobject)


class DashedVMobject(VMobject):
    """Atlas's true-arclength dash placement over native partial slices."""

    def __init__(
        self,
        vmobject,
        num_dashes=15,
        positive_space_ratio=0.5,
        **kwargs,
    ):
        if not isinstance(vmobject, VMobject):
            raise TypeError("DashedVMobject expects a VMobject")
        super().__init__(**kwargs)
        num_dashes = _operator.index(num_dashes)
        if num_dashes > 0:
            intervals = vmobject._dash_curve_intervals(
                num_dashes, float(positive_space_ratio)
            )
            self.add(
                *(vmobject.get_subcurve(start, end) for start, end in intervals)
            )
        self.match_style(vmobject, recurse=False)


class VHighlight(VGroup):
    """Reference full-family outline copies over native portal state."""

    def __init__(
        self,
        vmobject,
        n_layers=5,
        color_bounds=(_GREY_C, _GREY_E),
        max_stroke_addition=5.0,
    ):
        if not isinstance(vmobject, VMobject):
            raise TypeError("VHighlight expects a VMobject")
        n_layers = _operator.index(n_layers)
        if n_layers < 0:
            raise ValueError("n_layers must be non-negative")
        outline = vmobject.replicate(n_layers)
        outline.set_fill(opacity=0)
        added_widths = _np.linspace(0, max_stroke_addition, n_layers + 1)[1:]
        colors = _color_gradient(color_bounds, n_layers)
        for part, added_width, color in zip(
            reversed(outline), added_widths, colors
        ):
            for submob in part.family_members_with_points():
                submob.set_stroke(
                    width=submob.get_stroke_width() + added_width,
                    color=color,
                )
        super().__init__(*outline)


class ParametricCurve(VMobject):
    """Atlas sampling over a construction-time Python parameter function."""

    def __init__(
        self,
        t_func,
        t_range=(0, 1, 0.1),
        epsilon=1e-8,
        discontinuities=[],
        use_smoothing=True,
        **kwargs,
    ):
        if not callable(t_func):
            raise TypeError("t_func must be callable")
        _install_live_state(self)
        self.t_func = t_func
        self.t_range = tuple(float(value) for value in t_range)
        self.epsilon = float(epsilon)
        self.discontinuities = tuple(float(value) for value in discontinuities)
        self.use_smoothing = bool(use_smoothing)
        specs = self._build_parametric_curve(
            _native_shell_factory,
            self.t_func,
            self.t_range,
            self.epsilon,
            list(self.discontinuities),
            self.use_smoothing,
        )
        _hang_native_children(self, specs)
        _apply_vmobject_style_kwargs(self, kwargs)

    def get_point_from_function(self, t):
        return _np.array(self.t_func(t))

    def get_t_func(self):
        return self.t_func

    def get_function(self):
        if hasattr(self, "underlying_function"):
            return self.underlying_function
        if hasattr(self, "function"):
            return self.function

    def get_x_range(self):
        if hasattr(self, "x_range"):
            return self.x_range


class FunctionGraph(ParametricCurve):
    """Atlas's bounded graph of ``y = function(x)`` in scene coordinates."""

    def __init__(
        self,
        function,
        x_range=(-8, 8, 0.25),
        color=_YELLOW,
        **kwargs,
    ):
        if not callable(function):
            raise TypeError("function must be callable")
        self.function = function
        self.x_range = tuple(float(value) for value in x_range)
        self.epsilon = float(kwargs.pop("epsilon", 1e-8))
        self.discontinuities = tuple(
            float(value) for value in kwargs.pop("discontinuities", ())
        )
        self.use_smoothing = bool(kwargs.pop("use_smoothing", True))
        kwargs.setdefault("color", color)
        _preflight_vmobject_style_kwargs(kwargs)

        def parametric_function(t):
            return [t, function(t), 0.0]

        self.t_func = parametric_function
        self.t_range = self.x_range
        _install_live_state(self)
        specs = self._build_function_graph(
            _native_shell_factory,
            self.function,
            self.x_range,
            self.epsilon,
            list(self.discontinuities),
            self.use_smoothing,
        )
        _hang_native_children(self, specs)
        _apply_vmobject_style_kwargs(self, kwargs)


class ImplicitFunction(VMobject):
    """Chisel's bounded zero-set extraction under the Reference surface."""

    def __init__(
        self,
        func,
        x_range=(-_FRAME_X_RADIUS, _FRAME_X_RADIUS),
        y_range=(-_FRAME_Y_RADIUS, _FRAME_Y_RADIUS),
        min_depth=5,
        max_quads=1500,
        use_smoothing=False,
        joint_type="no_joint",
        **kwargs,
    ):
        if not callable(func):
            raise TypeError("func must be callable")
        if joint_type not in VMobject.joint_type_map:
            raise ValueError(f"unknown VMobject joint type: {joint_type}")
        _preflight_vmobject_style_kwargs(kwargs)
        self.func = func
        self.x_range = tuple(float(value) for value in x_range)
        self.y_range = tuple(float(value) for value in y_range)
        if len(self.x_range) != 2 or len(self.y_range) != 2:
            raise ValueError("implicit-function ranges must contain two entries")
        self.min_depth = _operator.index(min_depth)
        self.max_quads = _operator.index(max_quads)
        self.use_smoothing = bool(use_smoothing)
        self.joint_type = joint_type
        _install_live_state(self)
        specs = self._build_implicit_function(
            _native_shell_factory,
            self.func,
            self.x_range,
            self.y_range,
            self.min_depth,
            self.max_quads,
            self.use_smoothing,
        )
        _hang_native_children(self, specs)
        self.set_joint_type(self.joint_type)
        _apply_vmobject_style_kwargs(self, kwargs)


class CubicBezier(VMobject):
    def __init__(self, a0, h0, h1, a1, **kwargs):
        _install_live_state(self)
        specs = self._build_cubic_bezier(
            _native_shell_factory,
            _vec3(a0),
            _vec3(h0),
            _vec3(h1),
            _vec3(a1),
        )
        _hang_native_children(self, specs)
        _apply_vmobject_style_kwargs(self, kwargs)


class Polygon(VMobject):
    """Atlas's closed shared-anchor polygon over caller-supplied vertices."""

    def __init__(self, *vertices, **kwargs):
        if not vertices:
            # Reference geometry.py indexes vertices[0] before any mutation.
            raise IndexError("tuple index out of range")
        _install_live_state(self)
        specs = self._build_polygon(
            _native_shell_factory, [_vec3(vertex) for vertex in vertices]
        )
        _hang_native_children(self, specs)
        _apply_vmobject_style_kwargs(self, kwargs)

    def get_vertices(self):
        return self.get_start_anchors()

    def round_corners(self, radius=None):
        vertices = [_vec3(vertex) for vertex in self.get_vertices()]
        self._round_polygon_corners(
            vertices, None if radius is None else float(radius)
        )
        return self


class Polyline(VMobject):
    """Atlas's open shared-anchor path through caller-supplied vertices."""

    def __init__(self, *vertices, **kwargs):
        _install_live_state(self)
        specs = self._build_polyline(
            _native_shell_factory, [_vec3(vertex) for vertex in vertices]
        )
        _hang_native_children(self, specs)
        _apply_vmobject_style_kwargs(self, kwargs)


def _boolean_from_operands(self, operation, operands, kwargs):
    if len(operands) < 2:
        raise ValueError("At least 2 mobjects needed for " + type(self).__name__)
    if any(not isinstance(mobject, VMobject) for mobject in operands):
        raise TypeError(type(self).__name__ + " operands must be VMobjects")
    _preflight_vmobject_style_kwargs(kwargs)
    _install_live_state(self)
    specs = self._build_boolean(_native_shell_factory, operation, list(operands))
    _hang_native_children(self, specs)
    self.match_style(operands[0], recurse=False)
    _apply_vmobject_style_kwargs(self, kwargs)


class Union(VMobject):
    """Filled-set union over Chisel's certified path-boolean kernel."""

    def __init__(self, *vmobjects, **kwargs):
        _boolean_from_operands(self, "union", vmobjects, kwargs)


class Difference(VMobject):
    """Filled-set difference over Chisel's certified path-boolean kernel."""

    def __init__(self, subject, clip, **kwargs):
        _boolean_from_operands(self, "difference", (subject, clip), kwargs)


class Intersection(VMobject):
    """Filled-set intersection over Chisel's certified path-boolean kernel."""

    def __init__(self, *vmobjects, **kwargs):
        _boolean_from_operands(self, "intersection", vmobjects, kwargs)


class Exclusion(VMobject):
    """Filled-set exclusive-or over Chisel's certified path-boolean kernel."""

    def __init__(self, *vmobjects, **kwargs):
        _boolean_from_operands(self, "exclusion", vmobjects, kwargs)


class RegularPolygon(Polygon):
    """The bounded native regular-polygon compass construction."""

    def __init__(self, n=6, radius=1.0, start_angle=None, **kwargs):
        n = _operator.index(n)
        if n == 0:
            # Reference compass_directions divides TAU by n first.
            raise ZeroDivisionError("float division by zero")
        if n < 0:
            # range(n) produces no vertices and Polygon then indexes [0].
            raise IndexError("tuple index out of range")
        _install_live_state(self)
        specs = self._build_regular_polygon(
            _native_shell_factory,
            n,
            float(radius),
            None if start_angle is None else float(start_angle),
        )
        _hang_native_children(self, specs)
        _apply_vmobject_style_kwargs(self, kwargs)


class Triangle(RegularPolygon):
    def __init__(self, **kwargs):
        super().__init__(n=3, **kwargs)


class ArrowTip(Triangle):
    """Atlas's native tip geometry with live Reference query methods."""

    def __init__(
        self,
        angle=0,
        width=0.35,
        length=0.35,
        fill_opacity=1.0,
        fill_color="#FFFFFF",
        stroke_width=0.0,
        tip_style=0,
        **kwargs,
    ):
        _install_live_state(self)
        specs = self._build_arrow_tip(
            _native_shell_factory,
            float(angle),
            float(width),
            float(length),
            1 if tip_style == 1 else 2 if tip_style == 2 else 0,
        )
        _hang_native_children(self, specs)
        kwargs.setdefault("fill_opacity", fill_opacity)
        kwargs.setdefault("fill_color", fill_color)
        kwargs.setdefault("stroke_width", stroke_width)
        _apply_vmobject_style_kwargs(self, kwargs)

    def get_base(self):
        return self.point_from_proportion(0.5)

    def get_tip_point(self):
        return self.get_points()[0]

    def get_vector(self):
        return self.get_tip_point() - self.get_base()

    def get_angle(self):
        return _math.atan2(self.get_vector()[1], self.get_vector()[0])

    def get_length(self):
        vector = self.get_vector()
        return float(_np.sqrt(_np.dot(vector, vector)))


class Rectangle(Polygon):
    def __init__(self, width=4.0, height=2.0, **kwargs):
        _install_live_state(self)
        specs = self._build_rectangle(
            _native_shell_factory, float(width), float(height)
        )
        _hang_native_children(self, specs)
        _apply_vmobject_style_kwargs(self, kwargs)

    def surround(self, mobject, buff=_SMALL_BUFF):
        self.set_shape(
            mobject.get_width() + 2 * float(buff),
            mobject.get_height() + 2 * float(buff),
        )
        self.move_to(mobject)
        return self


class RoundedRectangle(Rectangle):
    def __init__(self, width=4.0, height=2.0, corner_radius=0.5, **kwargs):
        _install_live_state(self)
        specs = self._build_rounded_rectangle(
            _native_shell_factory,
            float(width),
            float(height),
            float(corner_radius),
        )
        _hang_native_children(self, specs)
        _apply_vmobject_style_kwargs(self, kwargs)


class Square(Rectangle):
    def __init__(self, side_length=2.0, **kwargs):
        self.side_length = side_length
        super().__init__(side_length, side_length, **kwargs)


class ScreenRectangle(Rectangle):
    def __init__(self, aspect_ratio=_ASPECT_RATIO, height=4, **kwargs):
        _install_live_state(self)
        specs = self._build_screen_rectangle(
            _native_shell_factory, float(aspect_ratio), float(height)
        )
        _hang_native_children(self, specs)
        _apply_vmobject_style_kwargs(self, kwargs)


class FullScreenRectangle(ScreenRectangle):
    def __init__(
        self,
        height=_FRAME_HEIGHT,
        fill_color=_GREY_E,
        fill_opacity=1,
        stroke_width=0,
        **kwargs,
    ):
        super().__init__(
            height=height,
            fill_color=fill_color,
            fill_opacity=fill_opacity,
            stroke_width=stroke_width,
            **kwargs,
        )


class FullScreenFadeRectangle(FullScreenRectangle):
    def __init__(
        self,
        stroke_width=0.0,
        fill_color=_BLACK,
        fill_opacity=0.7,
        **kwargs,
    ):
        # The pinned Reference accepts but does not forward `kwargs` here.
        # Preserve that source-level quirk until it receives a Behavior Note.
        del kwargs
        super().__init__(
            stroke_width=stroke_width,
            fill_color=fill_color,
            fill_opacity=fill_opacity,
        )


class SurroundingRectangle(Rectangle):
    """Atlas's native shape matcher over Marionette's live family extent."""

    def __init__(self, mobject, buff=0.1, color="#FFFF00", **kwargs):
        if not isinstance(mobject, _BridgeMobject):
            raise TypeError("SurroundingRectangle expects a Mobject")
        _install_live_state(self)
        self.mobject = mobject
        self.buff = float(buff)
        rows = mobject._bbox_rows()
        has_extent = any(member._has_points() for member in _family_preorder(mobject))
        specs = self._build_surrounding_rectangle(
            _native_shell_factory,
            _vec3(rows[0]),
            _vec3(rows[2]),
            has_extent,
            self.buff,
        )
        _hang_native_children(self, specs)
        kwargs.setdefault("color", color)
        _apply_vmobject_style_kwargs(self, kwargs)
        if mobject.is_fixed_in_frame():
            self.fix_in_frame()

    def surround(self, mobject, buff=None):
        if not isinstance(mobject, _BridgeMobject):
            raise TypeError("SurroundingRectangle.surround expects a Mobject")
        style = self.get_style()
        self.mobject = mobject
        self.buff = self.buff if buff is None else float(buff)
        rows = mobject._bbox_rows()
        has_extent = any(member._has_points() for member in _family_preorder(mobject))
        if self._is_bound():
            self._rebuild_surrounding_rectangle(
                _vec3(rows[0]),
                _vec3(rows[2]),
                has_extent,
                self.buff,
            )
        else:
            specs = self._build_surrounding_rectangle(
                _native_shell_factory,
                _vec3(rows[0]),
                _vec3(rows[2]),
                has_extent,
                self.buff,
            )
            self.submobjects.clear()
            _hang_native_children(self, specs)
        self.set_style(**style)
        return self

    def set_buff(self, buff):
        return self.surround(self.mobject, buff)


class BackgroundRectangle(SurroundingRectangle):
    """The Reference's camera-colored plate over Atlas matcher geometry."""

    def __init__(
        self,
        mobject,
        color=None,
        stroke_width=0,
        stroke_opacity=0,
        fill_opacity=0.75,
        buff=0,
        **kwargs,
    ):
        if color is None:
            color = _pinned_manim_config().camera.background_color
        super().__init__(
            mobject,
            color=color,
            stroke_width=stroke_width,
            stroke_opacity=stroke_opacity,
            fill_opacity=fill_opacity,
            buff=buff,
            **kwargs,
        )
        self.color = _rgb_to_hex(_color_to_rgb(color))
        self.original_fill_opacity = fill_opacity

    def pointwise_become_partial(self, mobject, a, b):
        del mobject, a
        self.set_fill(opacity=b * self.original_fill_opacity)
        return self

    def set_style(
        self,
        stroke_color=None,
        stroke_width=None,
        fill_color=None,
        fill_opacity=None,
        family=True,
        **kwargs,
    ):
        del stroke_color, stroke_width, fill_color, family, kwargs
        VMobject.set_style(
            self,
            stroke_color="#000000",
            stroke_width=0,
            fill_color="#000000",
            fill_opacity=fill_opacity,
        )
        return self

    def get_fill_color(self):
        return self.color


class TipableVMobject(VMobject):
    """Reference tip ownership over Atlas's native placement algebra."""

    tip_config = dict(
        fill_opacity=1.0,
        stroke_width=0.0,
        tip_style=0.0,
    )

    def add_tip(self, at_start=False, **kwargs):
        tip = self.create_tip(at_start, **kwargs)
        self.reset_endpoints_based_on_tip(tip, at_start)
        self.asign_tip_attr(tip, at_start)
        tip.set_color(self.get_stroke_color())
        self.add(tip)
        return self

    def create_tip(self, at_start=False, **kwargs):
        tip = self.get_unpositioned_tip(**kwargs)
        self.position_tip(tip, at_start)
        return tip

    def get_unpositioned_tip(self, **kwargs):
        config = dict(self.tip_config)
        config.update(kwargs)
        return ArrowTip(**config)

    def position_tip(self, tip, at_start=False):
        tip.set_points(self._position_tip_points(tip, bool(at_start)))
        return tip

    def reset_endpoints_based_on_tip(self, tip, at_start):
        if self.get_length() == 0:
            return self
        self._trim_to_tip(tip, bool(at_start))
        return self

    def asign_tip_attr(self, tip, at_start):
        if at_start:
            self.start_tip = tip
        else:
            self.tip = tip
        return self

    def has_tip(self):
        return hasattr(self, "tip") and self.tip in self

    def has_start_tip(self):
        return hasattr(self, "start_tip") and self.start_tip in self

    def pop_tips(self):
        start, end = self.get_start_and_end()
        result = VGroup()
        if self.has_tip():
            result.add(self.tip)
            self.remove(self.tip)
        if self.has_start_tip():
            result.add(self.start_tip)
            self.remove(self.start_tip)
        self.put_start_and_end_on(start, end)
        return result

    def get_tips(self):
        result = VGroup()
        if hasattr(self, "tip"):
            result.add(self.tip)
        if hasattr(self, "start_tip"):
            result.add(self.start_tip)
        return result

    def get_tip(self):
        tips = self.get_tips()
        if len(tips) == 0:
            raise Exception("tip not found")
        return tips[0]

    def get_default_tip_length(self):
        return self.tip_length

    def get_first_handle(self):
        return self.get_points()[1]

    def get_last_handle(self):
        return self.get_points()[-2]

    def get_end(self):
        if self.has_tip():
            return self.tip.get_start()
        return VMobject.get_end(self)

    def get_start(self):
        if self.has_start_tip():
            return self.start_tip.get_start()
        return VMobject.get_start(self)

    def get_length(self):
        start, end = self.get_start_and_end()
        return float(_np.linalg.norm(start - end))


class Arc(TipableVMobject):
    def __init__(
        self,
        start_angle=0,
        angle=_math.tau / 4,
        radius=1.0,
        n_components=None,
        arc_center=_ORIGIN,
        **kwargs,
    ):
        _install_live_state(self)
        self.start_angle = float(start_angle)
        self.angle = float(angle)
        specs = self._build_arc(
            _native_shell_factory,
            self.start_angle,
            self.angle,
            float(radius),
            _vec3(arc_center),
            None if n_components is None else int(n_components),
        )
        _hang_native_children(self, specs)
        _apply_vmobject_style_kwargs(self, kwargs)

    def get_arc_center(self):
        return _np.array(self._arc_center())

    def get_start_angle(self):
        return self._arc_start_angle()

    def get_stop_angle(self):
        return self._arc_stop_angle()

    def move_arc_center_to(self, point):
        self.shift(_np.array(_vec3(point)) - self.get_arc_center())
        return self


class ArcBetweenPoints(Arc):
    def __init__(self, start, end, angle=_math.tau / 4, **kwargs):
        n_components = kwargs.pop("n_components", None)
        # The Reference builds a provisional Arc and then fits it to these
        # endpoints, so these inherited knobs cancel exactly.
        kwargs.pop("start_angle", 0)
        kwargs.pop("radius", 1.0)
        kwargs.pop("arc_center", _ORIGIN)
        _install_live_state(self)
        self.angle = float(angle)
        specs = self._build_arc_between_points(
            _native_shell_factory,
            _vec3(start),
            _vec3(end),
            self.angle,
            None if n_components is None else _operator.index(n_components),
        )
        _hang_native_children(self, specs)
        _apply_vmobject_style_kwargs(self, kwargs)


class CurvedArrow(ArcBetweenPoints):
    def __init__(self, start_point, end_point, **kwargs):
        angle = float(kwargs.pop("angle", _math.tau / 4))
        n_components = kwargs.pop("n_components", None)
        kwargs.pop("start_angle", 0)
        kwargs.pop("radius", 1.0)
        kwargs.pop("arc_center", _ORIGIN)
        _install_live_state(self)
        self.angle = angle
        specs = self._build_curved_arrow(
            _native_shell_factory,
            _vec3(start_point),
            _vec3(end_point),
            angle,
            None if n_components is None else _operator.index(n_components),
            False,
        )
        _hang_native_children(self, specs)
        self.tip = self.submobjects[-1]
        _apply_vmobject_style_kwargs(self, kwargs)


class CurvedDoubleArrow(CurvedArrow):
    def __init__(self, start_point, end_point, **kwargs):
        angle = float(kwargs.pop("angle", _math.tau / 4))
        n_components = kwargs.pop("n_components", None)
        kwargs.pop("start_angle", 0)
        kwargs.pop("radius", 1.0)
        kwargs.pop("arc_center", _ORIGIN)
        _install_live_state(self)
        self.angle = angle
        specs = self._build_curved_arrow(
            _native_shell_factory,
            _vec3(start_point),
            _vec3(end_point),
            angle,
            None if n_components is None else _operator.index(n_components),
            True,
        )
        _hang_native_children(self, specs)
        self.tip, self.start_tip = self.submobjects
        _apply_vmobject_style_kwargs(self, kwargs)


class Circle(Arc):
    def __init__(self, start_angle=0, stroke_color=_RED, **kwargs):
        radius = kwargs.pop("radius", 1.0)
        arc_center = kwargs.pop("arc_center", _ORIGIN)
        _install_live_state(self)
        self.start_angle = float(start_angle)
        self.angle = _math.tau
        specs = self._build_circle(
            _native_shell_factory,
            self.start_angle,
            float(radius),
            _vec3(arc_center),
        )
        _hang_native_children(self, specs)
        kwargs.setdefault("stroke_color", stroke_color)
        _apply_vmobject_style_kwargs(self, kwargs)

    def surround(self, mobject, dim_to_match=0, stretch=False, buff=_MED_SMALL_BUFF):
        self.replace(mobject, dim_to_match, stretch)
        self.stretch((self.get_width() + 2 * buff) / self.get_width(), 0)
        self.stretch((self.get_height() + 2 * buff) / self.get_height(), 1)
        return self

    def get_radius(self):
        return self._circle_radius()

    def point_at_angle(self, angle):
        return _np.array(self._circle_point_at_angle(float(angle)))


class Dot(Circle):
    def __init__(
        self,
        point=_ORIGIN,
        radius=0.08,
        stroke_color=_BLACK,
        stroke_width=0.0,
        fill_opacity=1.0,
        fill_color=_WHITE,
        **kwargs,
    ):
        _install_live_state(self)
        specs = self._build_dot(
            _native_shell_factory, _vec3(point), float(radius)
        )
        _hang_native_children(self, specs)
        # Route the public values through the ordinary style path even though
        # Atlas already installs the same Reference defaults.
        for name, value in (
            ("stroke_color", stroke_color),
            ("stroke_width", stroke_width),
            ("fill_opacity", fill_opacity),
            ("fill_color", fill_color),
        ):
            kwargs.setdefault(name, value)
        _apply_vmobject_style_kwargs(self, kwargs)


class SmallDot(Dot):
    def __init__(self, point=_ORIGIN, radius=0.04, **kwargs):
        super().__init__(point, radius, **kwargs)


class Ellipse(Circle):
    def __init__(self, width=2.0, height=1.0, **kwargs):
        arc_center = kwargs.pop("arc_center", _ORIGIN)
        start_angle = float(kwargs.pop("start_angle", 0))
        _install_live_state(self)
        specs = self._build_ellipse(
            _native_shell_factory,
            float(width),
            float(height),
            _vec3(arc_center),
            start_angle,
        )
        _hang_native_children(self, specs)
        _apply_vmobject_style_kwargs(self, kwargs)


class AnnularSector(VMobject):
    def __init__(
        self,
        angle=_math.tau / 4,
        start_angle=0.0,
        inner_radius=1.0,
        outer_radius=2.0,
        arc_center=_ORIGIN,
        fill_color="#BBBBBB",
        fill_opacity=1.0,
        stroke_width=0.0,
        **kwargs,
    ):
        _install_live_state(self)
        specs = self._build_annular_sector(
            _native_shell_factory,
            float(angle),
            float(start_angle),
            float(inner_radius),
            float(outer_radius),
            _vec3(arc_center),
        )
        _hang_native_children(self, specs)
        kwargs.setdefault("fill_color", fill_color)
        kwargs.setdefault("fill_opacity", fill_opacity)
        kwargs.setdefault("stroke_width", stroke_width)
        _apply_vmobject_style_kwargs(self, kwargs)


class Sector(AnnularSector):
    def __init__(self, angle=_math.tau / 4, radius=1.0, **kwargs):
        super().__init__(angle, inner_radius=0.0, outer_radius=radius, **kwargs)


class Annulus(VMobject):
    def __init__(
        self,
        inner_radius=1.0,
        outer_radius=2.0,
        fill_opacity=1.0,
        stroke_width=0.0,
        fill_color="#BBBBBB",
        center=_ORIGIN,
        **kwargs,
    ):
        _install_live_state(self)
        self.radius = float(outer_radius)
        specs = self._build_annulus(
            _native_shell_factory,
            float(inner_radius),
            self.radius,
            _vec3(center),
        )
        _hang_native_children(self, specs)
        kwargs.setdefault("fill_color", fill_color)
        kwargs.setdefault("fill_opacity", fill_opacity)
        kwargs.setdefault("stroke_width", stroke_width)
        _apply_vmobject_style_kwargs(self, kwargs)


def _resolve_raster_image_path(filename):
    """Resolve the Reference's local image search without its URL downloader.

    Exact paths win. Extension-less names additionally search the current
    working directory and its ``raster_images`` directory for the pinned
    JPEG/PNG suffix catalog. Network input is a capability refusal: the
    sovereign engine never downloads assets implicitly.
    """
    raw = str(filename)
    if raw.lower().startswith(("http://", "https://")):
        raise _CapabilityError(
            "ImageMobject URL fetching is unavailable; provide a local file "
            "or supply bytes through the host AssetFetcher boundary"
        )
    supplied = _pathlib.Path(raw)
    directories = (_pathlib.Path(""), _pathlib.Path("raster_images"))
    suffixes = (".jpg", ".jpeg", ".png", "")
    candidates = [supplied, *( _pathlib.Path(f"{raw}{suffix}") for suffix in suffixes)]
    if not supplied.is_absolute():
        candidates.extend(
            directory / f"{raw}{suffix}"
            for directory in directories
            for suffix in suffixes
        )
    for candidate in dict.fromkeys(candidates):
        if candidate.is_file():
            return candidate
    raise OSError(f"{filename} not Found")


class ImageMobject(Mobject):
    """Atlas's decoded image resource on Marionette's live image-quad rows."""

    shader_folder = "image"
    data_dtype = [
        ("point", _np.float32, (3,)),
        ("im_coords", _np.float32, (2,)),
        ("opacity", _np.float32, (1,)),
    ]
    render_primitive = 4  # moderngl.TRIANGLES, without importing a renderer
    pointlike_data_keys = ["point"]

    def __init__(self, filename, height=4.0, **kwargs):
        opacity = float(kwargs.pop("opacity", 1.0))
        z_index = int(kwargs.pop("z_index", 0))
        fixed_in_frame = bool(kwargs.pop("is_fixed_in_frame", False))
        depth_test = bool(kwargs.pop("depth_test", False))
        color = kwargs.pop("color", None)
        if kwargs:
            raise TypeError(
                "ImageMobject() got unexpected keyword arguments: "
                + ", ".join(sorted(kwargs))
            )
        path = _resolve_raster_image_path(filename)
        payload = path.read_bytes()
        _install_live_state(self)
        self.height = float(height)
        self.opacity = opacity
        self.image_path = str(path)
        specs = self._build_image(
            _native_shell_factory,
            payload,
            self.height,
            self.opacity,
            z_index,
        )
        _hang_native_children(self, specs)
        self.pixel_width, self.pixel_height = self._image_dimensions()
        if fixed_in_frame:
            self.fix_in_frame()
        if depth_test:
            self.apply_depth_test()
        if color is not None:
            self.set_color(color)

    def init_data(self):
        self.data["point"][:] = [_UP + _LEFT, _DOWN + _LEFT, _UP + _RIGHT,
                                 _DOWN + _RIGHT, _UP + _RIGHT, _DOWN + _LEFT]
        self.data["im_coords"][:] = [
            (0, 0), (0, 1), (1, 0), (1, 1), (1, 0), (0, 1)
        ]
        self.data["opacity"][:] = self.opacity
        return self

    def init_points(self):
        self.set_width(2 * self.pixel_width / self.pixel_height, stretch=True)
        self.set_height(self.height)
        return self

    def set_opacity(self, opacity, recurse=True):
        values = _resize_with_interpolation(
            _np.array(_listify(opacity), dtype=float), self.get_num_points()
        )
        self.data["opacity"][:, 0] = values
        if recurse:
            for submob in self.submobjects:
                submob.set_opacity(opacity, recurse=True)
        return self

    def set_color(self, color, opacity=None, recurse=None):
        del color, opacity, recurse
        return self

    def point_to_rgb(self, point):
        result = self._image_point_to_rgb(_vec3(point))
        if result is None:
            raise ValueError("Cannot sample color from outside an image")
        return _np.array(result)


class Elbow(VMobject):
    def __init__(self, width=0.2, angle=0, **kwargs):
        _install_live_state(self)
        specs = self._build_elbow(
            _native_shell_factory, float(width), float(angle)
        )
        _hang_native_children(self, specs)
        _apply_vmobject_style_kwargs(self, kwargs)


class Line(TipableVMobject):
    def __init__(self, start=_LEFT, end=_RIGHT, buff=0.0, path_arc=0.0, **kwargs):
        _install_live_state(self)
        self.path_arc = float(path_arc)
        self.buff = float(buff)
        self.set_start_and_end_attrs(start, end)
        specs = self._build_line(
            _native_shell_factory,
            _vec3(self.start),
            _vec3(self.end),
            self.buff,
            self.path_arc,
        )
        _hang_native_children(self, specs)
        _apply_vmobject_style_kwargs(self, kwargs)

    def _replace_line_geometry(self, start, end, buff=0.0, path_arc=0.0):
        """Commit one Atlas-built point run while preserving portal state."""
        start = _vec3(start)
        end = _vec3(end)
        style = self.get_style()
        uniforms = self.uniforms.copy()
        if self._is_bound():
            self._rebuild_line(start, end, float(buff), float(path_arc))
        else:
            specs = self._build_line(
                _native_shell_factory,
                start,
                end,
                float(buff),
                float(path_arc),
            )
            _hang_native_children(self, specs)
        self.set_style(**style, recurse=False)
        self.uniforms.update(uniforms)
        return self

    def set_points_by_ends(self, start, end, buff=0, path_arc=0):
        return self._replace_line_geometry(start, end, buff, path_arc)

    def reset_points_around_ends(self):
        return self.set_points_by_ends(
            self.get_start().copy(),
            self.get_end().copy(),
            path_arc=self.path_arc,
        )

    def set_path_arc(self, path_arc):
        path_arc = float(path_arc)
        start = self.get_start().copy()
        end = self.get_end().copy()
        self.set_points_by_ends(start, end, path_arc=path_arc)
        self.path_arc = path_arc
        return self

    # Reference-verbatim endpoint resolution (geometry.py:718): mobject
    # endpoints resolve to continuous boundary points along the rough
    # center-to-center direction.
    def set_start_and_end_attrs(self, start, end):
        rough_start = self.pointify(start)
        rough_end = self.pointify(end)
        diff = rough_end - rough_start
        norm = float(_np.sqrt((diff * diff).sum()))
        vect = diff / norm if norm > 0 else _np.zeros(3)
        self.start = self.pointify(start, vect)
        self.end = self.pointify(end, -vect)

    def pointify(self, mob_or_point, direction=None):
        if isinstance(mob_or_point, _BridgeMobject):
            mob = mob_or_point
            if direction is None:
                return mob.get_center()
            return mob.get_continuous_bounding_box_point(direction)
        point = mob_or_point
        result = _np.zeros(3)
        arr = _np.array(point, dtype=float).flatten()
        result[: len(arr)] = arr[:3]
        return result

    def get_vector(self):
        return _np.array(self._line_vector(_vec3(self.get_start()), _vec3(self.get_end())))

    def get_unit_vector(self):
        return _np.array(
            self._line_unit_vector(_vec3(self.get_start()), _vec3(self.get_end()))
        )

    def get_angle(self):
        return self._line_angle(_vec3(self.get_start()), _vec3(self.get_end()))

    def get_slope(self):
        return self._line_slope(_vec3(self.get_start()), _vec3(self.get_end()))

    def set_angle(self, angle, about_point=None):
        # Reference geometry.py:723-730: preserve the start by default and
        # rotate only the delta from the line's current planar bearing.
        if about_point is None:
            about_point = self.get_start()
        self.rotate(float(angle) - self.get_angle(), about_point=about_point)
        return self

    def get_length(self):
        vect = self.get_vector()
        return float(_np.sqrt((vect * vect).sum()))

    def get_arc_length(self):
        return self._get_arc_length()

    def get_projection(self, point):
        return _np.array(
            self._line_projection(
                _vec3(self.get_start()),
                _vec3(self.get_end()),
                _vec3(point),
            )
        )

    def put_start_and_end_on(self, start, end):
        curr_start, curr_end = self.get_start_and_end()
        if _np.isclose(curr_start, curr_end).all():
            return self.set_points_by_ends(
                start,
                end,
                buff=0,
                path_arc=self.path_arc,
            )
        return super().put_start_and_end_on(start, end)

    def set_length(self, length, **kwargs):
        self.scale(float(length) / self.get_length(), **kwargs)
        return self


class DashedLine(Line):
    def __init__(
        self,
        start=_LEFT,
        end=_RIGHT,
        dash_length=0.05,
        positive_space_ratio=0.5,
        **kwargs,
    ):
        buff = float(kwargs.pop("buff", 0.0))
        path_arc = kwargs.pop("path_arc", 0.0)
        unknown = sorted(
            set(kwargs) - _NATIVE_VMOBJECT_STYLE_KEYS - {"shading"}
        )
        _refuse_unrouted(
            "DashedLine()", [(name, True) for name in unknown]
        )
        _install_live_state(self)
        self.path_arc = float(path_arc)
        self.buff = buff
        self.set_start_and_end_attrs(start, end)
        specs = self._build_dashed_line(
            _native_shell_factory,
            _vec3(self.start),
            _vec3(self.end),
            float(dash_length),
            float(positive_space_ratio),
            self.buff,
            self.path_arc,
        )
        _hang_native_children(self, specs)
        _apply_vmobject_style_kwargs(self, kwargs)

    def calculate_num_dashes(self, dash_length, positive_space_ratio):
        return self._calculate_dashed_line_num_dashes(
            _vec3(self.get_start()),
            _vec3(self.get_end()),
            float(dash_length),
            float(positive_space_ratio),
            self.path_arc,
        )

    def get_start(self):
        if self.submobjects:
            return self.submobjects[0].get_start()
        return super().get_start()

    def get_end(self):
        if self.submobjects:
            return self.submobjects[-1].get_end()
        return super().get_end()

    def get_start_and_end(self):
        return self.get_start(), self.get_end()

    def get_first_handle(self):
        return self.submobjects[0].get_points()[1]

    def get_last_handle(self):
        return self.submobjects[-1].get_points()[-2]


class TangentLine(Line):
    def __init__(self, vmob, alpha, length=2, d_alpha=1e-6, **kwargs):
        _install_live_state(self)
        self.path_arc = 0.0
        self.buff = 0.0
        specs = self._build_tangent_line(
            _native_shell_factory,
            vmob,
            float(alpha),
            float(length),
            float(d_alpha),
        )
        _hang_native_children(self, specs)
        if self.has_points():
            self.start = self.get_start().copy()
            self.end = self.get_end().copy()
        else:
            self.start = _np.zeros(3)
            self.end = _np.zeros(3)
        _apply_vmobject_style_kwargs(self, kwargs)


class StrokeArrow(Line):
    """The pinned one-path stroke taper over Atlas's BN-03 geometry."""

    def __init__(
        self,
        start,
        end,
        stroke_color=_DEFAULT_LIGHT_COLOR,
        stroke_width=5,
        buff=0.25,
        tip_width_ratio=5,
        tip_len_to_width=0.0075,
        max_tip_length_to_length_ratio=0.3,
        max_width_to_length_ratio=8.0,
        **kwargs,
    ):
        _install_live_state(self)
        self.path_arc = float(kwargs.pop("path_arc", 0.0))
        self.buff = float(buff)
        self.tip_width_ratio = float(tip_width_ratio)
        self.tip_len_to_width = float(tip_len_to_width)
        self.max_tip_length_to_length_ratio = float(
            max_tip_length_to_length_ratio
        )
        self.max_width_to_length_ratio = float(max_width_to_length_ratio)
        self.n_tip_points = 3
        self.original_stroke_width = float(stroke_width)
        self.set_start_and_end_attrs(start, end)
        specs = self._build_stroke_arrow(
            _native_shell_factory,
            _vec3(self.start),
            _vec3(self.end),
            self.original_stroke_width,
            self.buff,
            self.path_arc,
            self.tip_width_ratio,
            self.tip_len_to_width,
            self.max_tip_length_to_length_ratio,
            self.max_width_to_length_ratio,
        )
        _hang_native_children(self, specs)
        kwargs.setdefault("stroke_color", stroke_color)
        kwargs.setdefault("stroke_width", stroke_width)
        _apply_vmobject_style_kwargs(self, kwargs, recurse=False)

    def _replace_stroke_arrow_geometry(self, start, end, buff=0, path_arc=0):
        self._rebuild_stroke_arrow(
            _vec3(start),
            _vec3(end),
            self.original_stroke_width,
            float(buff),
            float(path_arc),
            self.tip_width_ratio,
            self.tip_len_to_width,
            self.max_tip_length_to_length_ratio,
            self.max_width_to_length_ratio,
        )
        return self

    def set_points_by_ends(self, start, end, buff=0, path_arc=0):
        return self._replace_stroke_arrow_geometry(start, end, buff, path_arc)

    def insert_tip_anchor(self):
        # Atlas performs the complete operation idempotently: reset from the
        # live endpoints, true-length trim, then append the terminal segment.
        return self._replace_stroke_arrow_geometry(
            self.get_start(), self.get_end(), path_arc=self.path_arc
        )

    def create_tip_with_stroke_width(self):
        if self.get_num_points() < self.n_tip_points:
            return self
        tip_width = self.tip_width_ratio * min(
            float(self.original_stroke_width),
            self.max_width_to_length_ratio * self.get_length(),
        )
        widths = self.get_stroke_widths()
        widths[: -self.n_tip_points] = widths[0]
        widths[-self.n_tip_points :] = tip_width * _np.linspace(
            1.0, 0.0, self.n_tip_points
        )
        return self

    def reset_tip(self):
        return self.set_points_by_ends(
            self.get_start(), self.get_end(), path_arc=self.path_arc
        )

    def set_stroke(self, color=None, width=None, *args, **kwargs):
        super().set_stroke(color=color, width=width, *args, **kwargs)
        self.original_stroke_width = self.get_stroke_width()
        if self.has_points():
            self.reset_tip()
        return self

    def _handle_scale_side_effects(self, scale_factor):
        if _np.any(_np.asarray(scale_factor) != 1.0):
            self.reset_tip()
        return self


class Arrow(Line):
    tickness_multiplier = 0.015

    def __init__(
        self,
        start=_LEFT,
        end=_LEFT,
        buff=0.25,
        path_arc=0.0,
        fill_color=_DEFAULT_LIGHT_COLOR,
        fill_opacity=1.0,
        stroke_width=0.0,
        thickness=3.0,
        tip_width_ratio=5,
        tip_angle=_math.pi / 3,
        max_tip_length_to_length_ratio=0.5,
        max_width_to_length_ratio=0.1,
        **kwargs,
    ):
        _install_live_state(self)
        self.path_arc = float(path_arc)
        self.buff = float(buff)
        self.thickness = float(thickness)
        self.tip_width_ratio = float(tip_width_ratio)
        self.tip_angle = float(tip_angle)
        self.max_tip_length_to_length_ratio = float(
            max_tip_length_to_length_ratio
        )
        self.max_width_to_length_ratio = float(max_width_to_length_ratio)
        self.set_start_and_end_attrs(start, end)
        specs, self.tip_index = self._build_arrow(
            _native_shell_factory,
            _vec3(self.start),
            _vec3(self.end),
            self.buff,
            self.path_arc,
            self.thickness,
            self.tip_width_ratio,
            self.tip_angle,
            self.max_tip_length_to_length_ratio,
            self.max_width_to_length_ratio,
        )
        _hang_native_children(self, specs)
        kwargs.setdefault("fill_color", fill_color)
        kwargs.setdefault("fill_opacity", fill_opacity)
        kwargs.setdefault("stroke_width", stroke_width)
        _apply_vmobject_style_kwargs(self, kwargs)

    def get_key_dimensions(self, length):
        return self._arrow_key_dimensions(
            float(length),
            self.thickness,
            self.tip_width_ratio,
            self.tip_angle,
            self.max_tip_length_to_length_ratio,
            self.max_width_to_length_ratio,
        )

    def set_points_by_ends(self, start, end, buff=0, path_arc=0):
        start = _vec3(start)
        end = _vec3(end)
        style = self.get_style()
        uniforms = self.uniforms.copy()
        params = (
            float(buff),
            float(path_arc),
            self.thickness,
            self.tip_width_ratio,
            self.tip_angle,
            self.max_tip_length_to_length_ratio,
            self.max_width_to_length_ratio,
        )
        if self._is_bound():
            self.tip_index = self._rebuild_arrow(start, end, *params)
        else:
            specs, self.tip_index = self._build_arrow(
                _native_shell_factory,
                start,
                end,
                *params,
            )
            _hang_native_children(self, specs)
        self.set_style(**style, recurse=False)
        self.uniforms.update(uniforms)
        self.start = _np.array(start)
        self.end = _np.array(end)
        return self

    def get_start(self):
        points = self.get_points()
        return 0.5 * (points[0] + points[-3])

    def get_end(self):
        return self.get_points()[self.tip_index]

    def get_start_and_end(self):
        return self.get_start(), self.get_end()

    def put_start_and_end_on(self, start, end):
        # Reference Arrow.put_start_and_end_on explicitly rebuilds with
        # `buff=0`, independent of the constructor's initial trim.
        self.set_points_by_ends(start, end, buff=0, path_arc=self.path_arc)
        return self

    def scale(self, *args, **kwargs):
        super().scale(*args, **kwargs)
        self.reset_points_around_ends()
        return self

    def set_thickness(self, thickness):
        self.thickness = float(thickness)
        self.reset_points_around_ends()
        return self


class Vector(Arrow):
    def __init__(self, direction=_RIGHT, buff=0.0, **kwargs):
        if len(direction) == 2:
            direction = _np.hstack([_np.array(direction, dtype=float), 0])
        super().__init__(_ORIGIN, direction, buff=buff, **kwargs)


class NumberLine(VMobject):
    def __init__(self, x_range=(-8, 8, 1), **kwargs):
        _install_live_state(self)
        parts = [float(v) for v in x_range]
        if len(parts) == 2:
            parts.append(1.0)
        self.x_range = tuple(parts)
        self.x_min, self.x_max, self.x_step = parts
        self._number_line_params = (self.x_range, dict(kwargs))
        specs = self._build_number_line(
            _native_shell_factory, self.x_range, dict(kwargs)
        )
        _hang_native_children(self, specs)

    # The coordinate mapping reads the proxy's LIVE line geometry (its own
    # first/last points), so a rescaled, moved, or even stretched line maps
    # exactly where its ticks actually sit.
    def number_to_point(self, number):
        return _axis_number_to_point(self, self.x_min, self.x_max, number)

    def point_to_number(self, point):
        return _axis_point_to_number(self, self.x_min, self.x_max, point)

    def n2p(self, number):
        """Abbreviation for number_to_point"""
        return self.number_to_point(number)

    def p2n(self, point):
        """Abbreviation for point_to_number"""
        return self.point_to_number(point)

    def get_vector(self):
        return self.get_end() - self.get_start()

    def get_angle(self):
        vect = self.get_vector()
        return _math.atan2(vect[1], vect[0])

    def get_unit_vector(self):
        vect = self.get_vector()
        norm = float(_np.sqrt((vect * vect).sum()))
        return vect / norm if norm > 0 else _np.zeros(3)

    def add_numbers(self, x_values=None, excluding=None, font_size=24, **kwargs):
        # The Reference forwards **kwargs to get_number_mobject; the native
        # label builder owns the supported placement/precision overrides.
        direction = kwargs.pop("direction", None)
        buff = kwargs.pop("buff", None)
        num_decimal_places = kwargs.pop("num_decimal_places", None)
        _refuse_unrouted(
            "NumberLine.add_numbers()",
            [(name, True) for name in sorted(kwargs)],
        )
        x_range, config = self._number_line_params
        specs = _BridgeMobject._number_line_label_shells(
            _native_shell_factory,
            x_range,
            config,
            float(font_size),
            float(self.get_width()),
            _vec3(self.get_center()),
            None if x_values is None else [float(v) for v in x_values],
            None if excluding is None else [float(v) for v in excluding],
            None if direction is None else _vec3(direction),
            None if buff is None else float(buff),
            None if num_decimal_places is None else int(num_decimal_places),
        )
        _hang_native_children(self, specs)
        self.numbers = self.submobjects[-1]
        return self.numbers


class UnitInterval(NumberLine):
    def __init__(
        self,
        x_range=(0, 1, 0.1),
        unit_size=10,
        big_tick_numbers=(0, 1),
        decimal_number_config=None,
        **kwargs,
    ):
        if decimal_number_config is None:
            decimal_number_config = dict(num_decimal_places=1)
        super().__init__(
            x_range,
            unit_size=unit_size,
            big_tick_numbers=list(big_tick_numbers),
            decimal_number_config=decimal_number_config,
            **kwargs,
        )


class Slider(VGroup):
    """The exported tracker slider over Atlas's one native builder."""

    def __init__(
        self,
        value_tracker,
        x_range=(-5, 5),
        var_name=None,
        width=3,
        unit_size=1,
        arrow_width=0.15,
        arrow_length=0.15,
        arrow_color=_YELLOW,
        font_size=24,
        label_buff=0.1,
        num_decimal_places=2,
        tick_size=0.05,
        number_line_config=dict(),
        arrow_tip_config=dict(),
        decimal_config=dict(),
        angle=0,
        label_direction=None,
        add_tick_labels=True,
        tick_label_font_size=16,
    ):
        if not isinstance(value_tracker, ValueTracker):
            raise TypeError("Slider value_tracker must be a ValueTracker")
        line_config = dict(number_line_config)
        tip_config = dict(arrow_tip_config)
        x_range = line_config.pop("x_range", x_range)
        width = line_config.pop("width", width)
        tick_size = line_config.pop("tick_size", tick_size)
        arrow_width = tip_config.pop("width", arrow_width)
        arrow_length = tip_config.pop("length", arrow_length)
        arrow_color = tip_config.pop("fill_color", arrow_color)
        _refuse_unrouted(
            "Slider()",
            [(f"number_line_config.{name}", True) for name in line_config]
            + [(f"arrow_tip_config.{name}", True) for name in tip_config],
        )
        parts = tuple(float(value) for value in x_range)
        if len(parts) != 2:
            raise ValueError("Slider x_range must contain exactly two values")
        if var_name is not None and not isinstance(var_name, str):
            raise TypeError("Slider var_name must be a string or None")
        if int(num_decimal_places) < 0:
            raise ValueError("Slider num_decimal_places must be non-negative")
        # The pinned Reference accepts decimal_config but never reads it.
        self.decimal_config = dict(decimal_config)
        self.value_tracker = value_tracker
        self.x_range = parts
        self.var_name = var_name
        self.unit_size = float(unit_size)
        self.angle = float(angle)
        if label_direction is None:
            label_direction = _np.round(
                [-_math.sin(self.angle), _math.cos(self.angle), 0.0], 2
            )
        else:
            label_direction = _vec3(label_direction)

        _install_live_state(self)
        specs = self._build_slider(
            _native_shell_factory,
            float(value_tracker.get_value()),
            parts,
            var_name,
            float(width),
            self.unit_size,
            float(arrow_width),
            float(arrow_length),
            arrow_color,
            float(font_size),
            float(label_buff),
            int(num_decimal_places),
            float(tick_size),
            self.angle,
            label_direction,
            bool(add_tick_labels),
            float(tick_label_font_size),
        )
        _hang_native_children(self, specs)
        if len(self.submobjects) != 3:
            raise RuntimeError(
                "native Slider family contract drift: expected 3 children, got "
                + str(len(self.submobjects))
            )
        number_line, tip, label = self.submobjects
        number_line.__class__ = NumberLine
        number_line.x_range = (*parts, 1.0)
        number_line.x_min, number_line.x_max, number_line.x_step = number_line.x_range
        number_line._number_line_params = (
            number_line.x_range,
            dict(width=float(width), tick_size=float(tick_size)),
        )
        tip.__class__ = ArrowTip
        decimal = label if var_name is None else label.submobjects[1]
        _decorate_matrix_decimal_entry(
            decimal,
            float(value_tracker.get_value()),
            float(font_size),
            int(num_decimal_places),
            {},
        )
        get_value = value_tracker.get_value
        tip.add_updater(
            lambda mob: mob.move_to(number_line.n2p(get_value()))
        )
        decimal.add_updater(lambda mob: mob.set_value(get_value()))
        label.add_updater(
            lambda mob: mob.next_to(tip, label_direction, float(label_buff))
        )
        self.number_line = number_line
        self.tip = tip
        self.label = label
        self.decimal = decimal
        self.set_stroke(behind=True)


class SampleSpace(Rectangle):
    """The probability shelf's sample-space frame over native Rectangle."""

    def __init__(
        self,
        width=3,
        height=3,
        fill_color=_GREY_D,
        fill_opacity=1,
        stroke_width=0.5,
        stroke_color=_GREY_B,
        default_label_scale_val=1,
        **kwargs,
    ):
        texture_paths = kwargs.pop("texture_paths", None)
        fixed_in_frame = bool(kwargs.pop("is_fixed_in_frame", False))
        depth_test = bool(kwargs.pop("depth_test", False))
        z_index = int(kwargs.pop("z_index", 0))
        _preflight_vmobject_style_kwargs(dict(kwargs))
        super().__init__(
            width,
            height,
            fill_color=fill_color,
            fill_opacity=fill_opacity,
            stroke_width=stroke_width,
            stroke_color=stroke_color,
            **kwargs,
        )
        self.texture_paths = texture_paths
        self.depth_test = depth_test
        self.z_index = z_index
        self.set_z_index(z_index)
        if depth_test:
            self.apply_depth_test()
        if fixed_in_frame:
            self.fix_in_frame()
        self.default_label_scale_val = default_label_scale_val


class BarChart(VGroup):
    """The pinned probability chart composed from native Atlas primitives."""

    def __init__(
        self,
        values,
        height=4,
        width=6,
        n_ticks=4,
        include_x_ticks=False,
        tick_width=0.2,
        tick_height=0.15,
        label_y_axis=True,
        y_axis_label_height=0.25,
        max_value=1,
        bar_colors=[_BLUE, _YELLOW],
        bar_fill_opacity=0.8,
        bar_stroke_width=3,
        bar_names=[],
        bar_label_scale_val=0.75,
        **kwargs,
    ):
        values = tuple(values)
        super().__init__(**kwargs)
        self.height = height
        self.width = width
        self.n_ticks = n_ticks
        self.include_x_ticks = include_x_ticks
        self.tick_width = tick_width
        self.tick_height = tick_height
        self.label_y_axis = label_y_axis
        self.y_axis_label_height = y_axis_label_height
        self.max_value = max(values) if max_value is None else max_value
        self.bar_colors = bar_colors
        self.bar_fill_opacity = bar_fill_opacity
        self.bar_stroke_width = bar_stroke_width
        self.bar_names = bar_names
        self.bar_label_scale_val = bar_label_scale_val
        self.n_ticks_x = len(values)
        self.add_axes()
        self.add_bars(values)
        self.center()

    def add_axes(self):
        x_axis = Line(self.tick_width * _LEFT / 2, self.width * _RIGHT)
        y_axis = Line(_MED_LARGE_BUFF * _DOWN, self.height * _UP)
        y_ticks = VGroup()
        heights = _np.linspace(0, self.height, self.n_ticks + 1)
        values = _np.linspace(0, self.max_value, self.n_ticks + 1)
        for y, _value in zip(heights, values):
            y_tick = Line(_LEFT, _RIGHT)
            y_tick.set_width(self.tick_width)
            y_tick.move_to(y * _UP)
            y_ticks.add(y_tick)
        y_axis.add(y_ticks)

        if self.include_x_ticks is True:
            x_ticks = VGroup()
            widths = _np.linspace(0, self.width, self.n_ticks_x + 1)
            for x in widths:
                x_tick = Line(_UP, _DOWN)
                x_tick.set_height(self.tick_height)
                x_tick.move_to(x * _RIGHT)
                x_ticks.add(x_tick)
            x_axis.add(x_ticks)

        self.add(x_axis, y_axis)
        self.x_axis, self.y_axis = x_axis, y_axis
        if self.label_y_axis:
            labels = VGroup()
            for y_tick, value in zip(y_ticks, values):
                label = Tex(str(_np.round(value, 2)))
                label.set_height(self.y_axis_label_height)
                label.next_to(y_tick, _LEFT, _SMALL_BUFF)
                labels.add(label)
            self.y_axis_labels = labels
            self.add(labels)

    def add_bars(self, values):
        buff = float(self.width) / (2 * len(values))
        bars = VGroup()
        for index, value in enumerate(values):
            bar = Rectangle(
                height=(value / self.max_value) * self.height,
                width=buff,
                stroke_width=self.bar_stroke_width,
                fill_opacity=self.bar_fill_opacity,
            )
            bar.move_to(
                (2 * index + 0.5) * buff * _RIGHT,
                _DOWN + _LEFT * 5,
            )
            bars.add(bar)
        bars.set_color_by_gradient(*self.bar_colors)

        bar_labels = VGroup()
        for bar, name in zip(bars, self.bar_names):
            label = Tex(str(name))
            label.scale(self.bar_label_scale_val)
            label.next_to(bar, _DOWN, _SMALL_BUFF)
            bar_labels.add(label)

        self.add(bars, bar_labels)
        self.bars = bars
        self.bar_labels = bar_labels

    def change_bar_values(self, values):
        for bar, value in zip(self.bars, values):
            bar_bottom = bar.get_bottom()
            bar.stretch_to_fit_height((value / self.max_value) * self.height)
            bar.move_to(bar_bottom, _DOWN)


class CoordinateSystem:
    """The Reference's coordinate-system mixin over live axis geometry.

    Concrete axes own their construction in Atlas; this layer owns the
    familiar Python callable boundary and delegates every sampled curve to
    the already-native ``ParametricCurve`` builder.
    """

    dimension = 2

    def __init__(
        self,
        x_range=(-8.0, 8.0, 1.0),
        y_range=(-4.0, 4.0, 1.0),
        num_sampled_graph_points_per_tick=5,
    ):
        self.x_range = tuple(x_range) if len(x_range) == 3 else (*x_range, 1)
        self.y_range = tuple(y_range) if len(y_range) == 3 else (*y_range, 1)
        self.num_sampled_graph_points_per_tick = num_sampled_graph_points_per_tick

    def coords_to_point(self, *coords):
        del coords
        raise NotImplementedError("CoordinateSystem.coords_to_point is abstract")

    def point_to_coords(self, point):
        del point
        raise NotImplementedError("CoordinateSystem.point_to_coords is abstract")

    def c2p(self, *coords):
        return self.coords_to_point(*coords)

    def p2c(self, point):
        return self.point_to_coords(point)

    def get_origin(self):
        return self.c2p(*[0] * self.dimension)

    def get_axes(self):
        raise NotImplementedError("CoordinateSystem.get_axes is abstract")

    def get_all_ranges(self):
        raise NotImplementedError("CoordinateSystem.get_all_ranges is abstract")

    def get_axis(self, index):
        return self.get_axes()[index]

    def get_x_axis(self):
        return self.get_axis(0)

    def get_y_axis(self):
        return self.get_axis(1)

    def get_graph(self, function, x_range=None, bind=False, **kwargs):
        x_range = x_range or self.x_range
        t_range = _np.ones(3)
        t_range[: len(x_range)] = x_range
        t_range[2] /= self.num_sampled_graph_points_per_tick

        graph = ParametricCurve(
            lambda t: self.c2p(t, function(t)),
            t_range=tuple(t_range),
            **kwargs,
        )
        graph.underlying_function = function
        graph.x_range = x_range
        if bind:
            self.bind_graph_to_func(graph, function)
        return graph

    def get_parametric_curve(self, function, **kwargs):
        dimension = self.dimension
        graph = ParametricCurve(
            lambda t: self.coords_to_point(*function(t)[:dimension]),
            **kwargs,
        )
        graph.underlying_function = function
        return graph

    def input_to_graph_point(self, x, graph):
        if hasattr(graph, "underlying_function"):
            return self.coords_to_point(x, graph.underlying_function(x))
        alpha = _binary_search(
            function=lambda value: self.point_to_coords(
                graph.quick_point_from_proportion(value)
            )[0],
            target=x,
            lower_bound=self.x_range[0],
            upper_bound=self.x_range[1],
        )
        if alpha is None:
            return None
        return graph.quick_point_from_proportion(alpha)

    def i2gp(self, x, graph):
        return self.input_to_graph_point(x, graph)

    def bind_graph_to_func(
        self,
        graph,
        func,
        jagged=False,
        get_discontinuities=None,
    ):
        x_values = _np.array(
            [
                _axis_point_to_number(
                    self.x_axis,
                    self.x_range[0],
                    self.x_range[1],
                    point,
                )
                for point in graph.get_points()
            ]
        )

        def get_graph_points():
            xs = x_values
            if get_discontinuities:
                epsilon = 1e-6
                added_xs = [
                    value
                    for discontinuity in get_discontinuities()
                    for value in (discontinuity - epsilon, discontinuity + epsilon)
                ]
                xs[:] = sorted([*x_values, *added_xs])[: len(x_values)]
            return self.c2p(xs, func(xs))

        graph.add_updater(lambda current: current.set_points_as_corners(get_graph_points()))
        if not jagged:
            graph.add_updater(lambda current: current.make_smooth(approx=True))
        return graph


class Axes(VGroup, CoordinateSystem):
    def __init__(
        self,
        x_range=(-8.0, 8.0, 1.0),
        y_range=(-4.0, 4.0, 1.0),
        axis_config=None,
        x_axis_config=None,
        y_axis_config=None,
        height=None,
        width=None,
        unit_size=1.0,
        **kwargs,
    ):
        # The one remaining Reference keyword configures graph sampling
        # (`get_graph`), not built geometry; it is stored for that binding.
        self.num_sampled_graph_points_per_tick = kwargs.pop(
            "num_sampled_graph_points_per_tick", 5
        )
        if kwargs:
            raise TypeError(
                "Axes() got unexpected keyword arguments: "
                + ", ".join(sorted(kwargs))
            )
        _install_live_state(self)
        self._axes_params = (
            tuple(float(v) for v in x_range),
            tuple(float(v) for v in y_range),
            dict(axis_config or {}),
            dict(x_axis_config or {}),
            dict(y_axis_config or {}),
            None if height is None else float(height),
            None if width is None else float(width),
            float(unit_size),
        )
        specs = self._build_axes(_native_shell_factory, *self._axes_params)
        _hang_native_children(self, specs)
        self.x_axis = self.submobjects[0]
        self.y_axis = self.submobjects[1]
        self.axes = VGroup(self.x_axis, self.y_axis)
        self.x_range = self._axes_params[0]
        self.y_range = self._axes_params[1]
        self.dimension = 2

    def get_axes(self):
        return self.axes

    def get_x_axis(self):
        return self.x_axis

    def get_y_axis(self):
        return self.y_axis

    def get_all_ranges(self):
        return [self.x_range, self.y_range]

    # CoordinateSystem mapping (coordinate_systems.py:501), verbatim over
    # each axis proxy's LIVE line geometry — exact after rescale/move.
    def coords_to_point(self, *coords):
        axes = list(self.axes)
        ranges = self.get_all_ranges()
        origin = _axis_number_to_point(axes[0], ranges[0][0], ranges[0][1], 0)
        result = _np.array(origin, dtype=float)
        for axis, rng, coord in zip(axes, ranges, coords):
            result = result + (
                _axis_number_to_point(axis, rng[0], rng[1], coord) - origin
            )
        return result

    def point_to_coords(self, point):
        axes = list(self.axes)
        ranges = self.get_all_ranges()
        return tuple(
            _axis_point_to_number(axis, rng[0], rng[1], point)
            for axis, rng in zip(axes, ranges)
        )

    def c2p(self, *coords):
        """Abbreviation for coords_to_point"""
        return self.coords_to_point(*coords)

    def p2c(self, point):
        """Abbreviation for point_to_coords"""
        return self.point_to_coords(point)

    def get_origin(self):
        return self.c2p(*[0] * self.dimension)

    def add_coordinate_labels(self, x_values=None, y_values=None, excluding=(0,), **kwargs):
        font_size = kwargs.pop("font_size", None)
        if kwargs:
            raise TypeError(
                "add_coordinate_labels() got unexpected keyword arguments: "
                + ", ".join(sorted(kwargs))
            )
        (x_range, y_range, axis_config, x_axis_config, y_axis_config, _h, _w, unit) = (
            self._axes_params
        )
        x_specs, y_specs = _BridgeMobject._axes_label_shells(
            _native_shell_factory,
            x_range,
            y_range,
            axis_config,
            x_axis_config,
            y_axis_config,
            unit,
            float(self.get_width()),
            float(self.get_height()),
            _vec3(self.get_center()),
            None if x_values is None else [float(v) for v in x_values],
            None if y_values is None else [float(v) for v in y_values],
            [float(v) for v in excluding],
            None if font_size is None else float(font_size),
        )
        _hang_native_children(self.x_axis, x_specs)
        _hang_native_children(self.y_axis, y_specs)
        return self


class ThreeDAxes(Axes):
    def __init__(
        self,
        x_range=(-6.0, 6.0, 1.0),
        y_range=(-5.0, 5.0, 1.0),
        z_range=(-4.0, 4.0, 1.0),
        z_axis_config=None,
        z_normal=None,
        depth=None,
        **kwargs,
    ):
        if z_normal is None:
            z_normal_vect = None
        else:
            # planes.rs: the normal reorients the z-axis tick construction
            # by angle_of_vector(z_normal) about OUT; the axis line itself
            # stays along z (the Reference's exact behaviour).
            try:
                z_normal_vect = tuple(float(v) for v in _vec3(z_normal))
            except (TypeError, ValueError, IndexError) as error:
                raise TypeError(
                    "ThreeDAxes z_normal must be a 3-vector; got "
                    + repr(z_normal)
                ) from error
        axis_config = kwargs.pop("axis_config", None)
        x_axis_config = kwargs.pop("x_axis_config", None)
        y_axis_config = kwargs.pop("y_axis_config", None)
        height = kwargs.pop("height", None)
        width = kwargs.pop("width", None)
        unit_size = kwargs.pop("unit_size", 1.0)
        self.num_sampled_graph_points_per_tick = kwargs.pop(
            "num_sampled_graph_points_per_tick", 5
        )
        if kwargs:
            raise TypeError(
                "ThreeDAxes() got unexpected keyword arguments: "
                + ", ".join(sorted(kwargs))
            )
        _install_live_state(self)
        self._axes_params = (
            tuple(float(v) for v in x_range),
            tuple(float(v) for v in y_range),
            dict(axis_config or {}),
            dict(x_axis_config or {}),
            dict(y_axis_config or {}),
            None if height is None else float(height),
            None if width is None else float(width),
            float(unit_size),
        )
        specs = self._build_three_d_axes(
            _native_shell_factory,
            self._axes_params[0],
            self._axes_params[1],
            tuple(float(v) for v in z_range),
            self._axes_params[2],
            self._axes_params[3],
            self._axes_params[4],
            dict(z_axis_config or {}),
            self._axes_params[5],
            self._axes_params[6],
            None if depth is None else float(depth),
            z_normal_vect,
            self._axes_params[7],
        )
        _hang_native_children(self, specs)
        self.x_axis, self.y_axis, self.z_axis = self.submobjects[:3]
        self.axes = VGroup(self.x_axis, self.y_axis, self.z_axis)
        self.x_range = self._axes_params[0]
        self.y_range = self._axes_params[1]
        self.z_range = tuple(float(v) for v in z_range)
        self.dimension = 3

    def get_z_axis(self):
        return self.z_axis

    def get_all_ranges(self):
        return [self.x_range, self.y_range, self.z_range]


class NumberPlane(Axes):
    def __init__(
        self,
        x_range=(-8.0, 8.0, 1.0),
        y_range=(-4.0, 4.0, 1.0),
        background_line_style=None,
        faded_line_style=None,
        faded_line_ratio=4,
        make_smooth_after_applying_functions=True,
        **kwargs,
    ):
        axis_config = kwargs.pop("axis_config", None)
        x_axis_config = kwargs.pop("x_axis_config", None)
        y_axis_config = kwargs.pop("y_axis_config", None)
        height = kwargs.pop("height", None)
        width = kwargs.pop("width", None)
        unit_size = kwargs.pop("unit_size", 1.0)
        self.num_sampled_graph_points_per_tick = kwargs.pop(
            "num_sampled_graph_points_per_tick", 5
        )
        if kwargs:
            raise TypeError(
                type(self).__name__
                + "() got unexpected keyword arguments: "
                + ", ".join(sorted(kwargs))
            )
        _install_live_state(self)
        self.make_smooth_after_applying_functions = make_smooth_after_applying_functions
        self._plane_params = (
            tuple(float(v) for v in x_range),
            tuple(float(v) for v in y_range),
            dict(axis_config or {}),
            dict(x_axis_config or {}),
            dict(y_axis_config or {}),
            None if background_line_style is None else dict(background_line_style),
            None if faded_line_style is None else dict(faded_line_style),
            int(faded_line_ratio),
            None if height is None else float(height),
            None if width is None else float(width),
            float(unit_size),
        )
        # Axes.add_coordinate_labels rebuilds only the two native axes.
        # Preserve NumberPlane's class defaults in that shared shell path,
        # then layer the caller's ordinary axis configs over them in the
        # same order as the native NumberPlane builder.
        label_axis_config = {
            "color": _WHITE,
            "stroke_width": 2.0,
            "include_ticks": False,
            "include_tip": False,
            "line_to_number_buff": _SMALL_BUFF,
            "line_to_number_direction": _DL,
        }
        label_axis_config.update(self._plane_params[2])
        label_y_axis_config = {"line_to_number_direction": _DL}
        label_y_axis_config.update(self._plane_params[4])
        self._axes_params = (
            self._plane_params[0],
            self._plane_params[1],
            label_axis_config,
            self._plane_params[3],
            label_y_axis_config,
            self._plane_params[8],
            self._plane_params[9],
            self._plane_params[10],
        )
        specs = self._native_plane_specs()
        _hang_native_children(self, specs)
        self.faded_lines, self.background_lines, self.x_axis, self.y_axis = (
            self.submobjects[:4]
        )
        self.axes = VGroup(self.x_axis, self.y_axis)
        self.x_range = self._plane_params[0]
        self.y_range = self._plane_params[1]
        self.dimension = 2

    def _native_plane_specs(self):
        (x_range, y_range, axis_config, x_axis_config, y_axis_config,
         background, faded, ratio, height, width, unit) = self._plane_params
        return self._build_number_plane(
            _native_shell_factory,
            x_range,
            y_range,
            axis_config,
            x_axis_config,
            y_axis_config,
            background,
            faded,
            ratio,
            height,
            width,
            unit,
        )

    def add_coordinate_labels(self, x_values=None, y_values=None, excluding=(0,), **kwargs):
        if not isinstance(self, NumberPlane):
            raise TypeError(
                "NumberPlane.add_coordinate_labels requires a NumberPlane; got "
                + type(self).__name__
            )
        try:
            excluding = tuple(float(value) for value in excluding)
        except (TypeError, ValueError) as error:
            raise TypeError(
                "NumberPlane.add_coordinate_labels excluding must be an "
                "iterable of real numbers"
            ) from error
        return Axes.add_coordinate_labels(
            self,
            x_values=x_values,
            y_values=y_values,
            excluding=excluding,
            **kwargs,
        )


class ComplexPlane(NumberPlane):
    # Reference ComplexPlane: complex numbers map through the 2D grid.
    def number_to_point(self, number):
        number = complex(number)
        return self.coords_to_point(number.real, number.imag)

    def n2p(self, number):
        return self.number_to_point(number)

    def point_to_number(self, point):
        x, y = self.point_to_coords(point)
        return complex(x, y)

    def p2n(self, point):
        return self.point_to_number(point)

    def _native_plane_specs(self):
        (x_range, y_range, axis_config, x_axis_config, y_axis_config,
         background, faded, ratio, height, width, unit) = self._plane_params
        return self._build_complex_plane(
            _native_shell_factory,
            x_range,
            y_range,
            axis_config,
            x_axis_config,
            y_axis_config,
            background,
            faded,
            ratio,
            height,
            width,
            unit,
        )

    def add_coordinate_labels(self, numbers=None, skip_first=True, font_size=36, **kwargs):
        del skip_first  # accepted, never used — the Reference's own contract
        if kwargs:
            raise TypeError(
                "add_coordinate_labels() got unexpected keyword arguments: "
                + ", ".join(sorted(kwargs))
            )
        values = None
        if numbers is not None:
            values = [
                (float(z.real), float(z.imag))
                if isinstance(z, complex)
                else (float(z[0]), float(z[1]))
                if not isinstance(z, (int, float))
                else (float(z), 0.0)
                for z in numbers
            ]
        (x_range, y_range, axis_config, x_axis_config, y_axis_config,
         background, faded, ratio, _height, _width, unit) = self._plane_params
        specs = _BridgeMobject._complex_plane_label_shells(
            _native_shell_factory,
            x_range,
            y_range,
            axis_config,
            x_axis_config,
            y_axis_config,
            background,
            faded,
            ratio,
            unit,
            float(self.get_width()),
            float(self.get_height()),
            _vec3(self.get_center()),
            values,
            float(font_size),
        )
        _hang_native_children(self, specs)
        self.coordinate_labels = self.submobjects[-1]
        return self


def _refuse_unrouted(class_name, entries):
    """Precise refusal for ledger keywords whose native routing has not
    landed: never silently dropped."""
    unrouted = [name for name, off_default in entries if off_default]
    if unrouted:
        raise NotImplementedError(
            class_name
            + " keyword(s) not yet routed to the native builder: "
            + ", ".join(sorted(unrouted))
        )


class SVGMobject(VMobject):
    """User SVG files through Chisel's hardened document processor
    (fm-5wq.4.50, G2 criterion 4).

    The native side reads the file under the processor's byte budget and
    parses it under the full accept/reject matrix — bombs, deep nesting,
    DOCTYPE, external references, and unsupported features are all *named*
    refusals, never hangs or silent drops. The built family is the
    Reference's: one child per rendered shape in document order, each with
    its resolved SVG style; this constructor then applies the Reference's
    own post-passes (y-flip, style overrides, centring, height 2.0).

    Scribe-backed text subclasses keep their native layout provenance and
    never route through this constructor.
    """

    file_name = ""
    height = 2.0
    width = None

    def __init__(
        self,
        file_name: str = "",
        svg_string: str = "",
        should_center: bool = True,
        height: float | None = None,
        width: float | None = None,
        color=None,
        fill_color=None,
        fill_opacity: float | None = None,
        stroke_width: float | None = 0.0,
        stroke_color=None,
        stroke_opacity: float | None = None,
        svg_default: dict | None = None,
        path_string_config: dict | None = None,
        **kwargs,
    ):
        _refuse_unrouted(
            type(self).__name__ + "()",
            [
                (
                    "svg_default",
                    bool(svg_default)
                    and any(value is not None for value in svg_default.values()),
                ),
                ("path_string_config", bool(path_string_config)),
            ],
        )
        _install_live_state(self)
        if svg_string:
            specs = self._build_svg_mobject(_native_shell_factory, "", svg_string)
        else:
            name = file_name or type(self).file_name
            if not name:
                # Reference svg_mobject.py raises this exact bare Exception.
                raise Exception(
                    "Must specify either a file_name or svg_string SVGMobject"
                )
            specs = self._build_svg_mobject(_native_shell_factory, name, None)
        _hang_native_children(self, specs)
        self.flip(_RIGHT)  # SVG y-down becomes scene y-up, the Reference's flip
        self.set_style(
            fill_color=color or fill_color,
            fill_opacity=fill_opacity,
            stroke_color=color or stroke_color,
            stroke_width=stroke_width,
            stroke_opacity=stroke_opacity,
        )
        _apply_vmobject_style_kwargs(self, kwargs)
        height = height if height is not None else type(self).height
        width = width if width is not None else type(self).width
        if should_center:
            self.center()
        if height is not None:
            self.set_height(height)
        if width is not None:
            self.set_width(width)


class VMobjectFromSVGPath(VMobject):
    """One svgelements-compatible path over Chisel's native SVG parser."""

    def __init__(self, path_obj, **kwargs):
        background_image_file = kwargs.pop("background_image_file", None)
        long_lines = bool(kwargs.pop("long_lines", False))
        joint_type = kwargs.pop("joint_type", "auto")
        scale_stroke_with_zoom = bool(
            kwargs.pop("scale_stroke_with_zoom", False)
        )
        use_simple_quadratic_approx = bool(
            kwargs.pop("use_simple_quadratic_approx", False)
        )
        anti_alias_width = float(kwargs.pop("anti_alias_width", 1.5))
        _refuse_unrouted(
            "VMobjectFromSVGPath()",
            [("background_image_file", background_image_file is not None)],
        )
        if joint_type not in self.joint_type_map:
            raise ValueError(f"unknown VMobject joint type: {joint_type}")
        style = dict(kwargs)
        config = _pinned_manim_config().vmobject
        color = style.get("color")
        style.setdefault(
            "fill_color", color if color is not None else config.default_fill_color
        )
        style.setdefault("fill_opacity", 0.0)
        style.setdefault(
            "stroke_color",
            color if color is not None else config.default_stroke_color,
        )
        style.setdefault("stroke_opacity", 1.0)
        style.setdefault("stroke_width", 4.0)
        style.setdefault("fill_border_width", 0.0)
        _preflight_vmobject_style_kwargs(style)
        path_data = path_obj.d()
        if not isinstance(path_data, str):
            raise TypeError("VMobjectFromSVGPath path_obj.d() must return str")
        _install_live_state(self)
        self.transform_cache = None
        self.path_obj = path_obj
        self.long_lines = long_lines
        self.joint_type = joint_type
        self.scale_stroke_with_zoom = scale_stroke_with_zoom
        self.use_simple_quadratic_approx = use_simple_quadratic_approx
        self.anti_alias_width = anti_alias_width
        specs = self._build_svg_path(_native_shell_factory, path_data)
        _hang_native_children(self, specs)
        self.set_joint_type(joint_type)
        self.set_anti_alias_width(anti_alias_width)
        self.uniforms["scale_stroke_with_zoom"] = scale_stroke_with_zoom
        _apply_vmobject_style_kwargs(self, style)


class StringMobject(SVGMobject, _abc.ABC):
    """Shared substring-selection surface for Tex and MarkupText.

    The Reference discovers submobject labels with a second colored SVG and
    spatial matching. Scribe and fmd-math retain the source byte span of every
    glyph, so the portal can preserve the same selection result and object
    identity directly, including Unicode, without rendering twice.
    """

    height = None

    def find_spans_by_selector(self, selector):
        def one(value):
            if isinstance(value, str):
                return [match.span() for match in _re.finditer(_re.escape(value), self.string)]
            if isinstance(value, _re.Pattern):
                return [match.span() for match in value.finditer(self.string)]
            if (
                isinstance(value, tuple)
                and len(value) == 2
                and all(index is None or isinstance(index, int) for index in value)
            ):
                length = len(self.string)
                span = tuple(
                    default
                    if index is None
                    else min(index, length)
                    if index >= 0
                    else max(index + length, 0)
                    for index, default in zip(value, (0, length))
                )
                return [span]
            return None

        spans = one(selector)
        if spans is None:
            spans = []
            try:
                selectors = iter(selector)
            except TypeError as error:
                raise TypeError(f"Invalid selector: {selector!r}") from error
            for value in selectors:
                selected = one(value)
                if selected is None:
                    raise TypeError(f"Invalid selector: {value!r}")
                spans.extend(selected)
        return [span for span in spans if span[0] <= span[1]]

    @staticmethod
    def span_contains(span_0, span_1):
        return span_0[0] <= span_1[0] and span_0[1] >= span_1[1]

    def _byte_span(self, span):
        start, end = span
        return (
            len(self.string[:start].encode("utf-8")),
            len(self.string[:end].encode("utf-8")),
        )

    def _selector_byte_spans(self, selector):
        return [self._byte_span(span) for span in self.find_spans_by_selector(selector)]

    def _string_submobject(self, ordinal):
        current = self
        for index in self._string_sub_paths[ordinal]:
            current = current.submobjects[index]
        return current

    def get_submob_indices_list_by_span(self, arbitrary_span):
        start, end = self._byte_span(arbitrary_span)
        return [
            ordinal
            for ordinal, (sub_start, sub_end) in enumerate(self._string_sub_spans)
            if start <= sub_start and sub_end <= end
        ]

    def _selected_string_ordinals(self, selector):
        return [
            [
                ordinal
                for ordinal, (sub_start, sub_end) in enumerate(self._string_sub_spans)
                if start <= sub_start and sub_end <= end
            ]
            for start, end in self._selector_byte_spans(selector)
        ]

    def get_submob_indices_lists_by_selector(self, selector):
        return [
            ordinals
            for ordinals in self._selected_string_ordinals(selector)
            if ordinals
        ]

    def build_parts_from_indices_lists(self, indices_lists):
        return VGroup(
            *(
                VGroup(*(self._string_submobject(ordinal) for ordinal in ordinals))
                for ordinals in indices_lists
            )
        )

    def select_parts(self, selector):
        return self.build_parts_from_indices_lists(
            self.get_submob_indices_lists_by_selector(selector)
        )

    def __getitem__(self, value):
        if isinstance(value, (int, slice)):
            return super().__getitem__(value)
        return self.select_parts(value)

    def select_part(self, selector, index=0):
        return self.select_parts(selector)[index]

    def select_unisolated_substring(self, pattern):
        return self.select_parts(pattern)

    def set_parts_color(self, selector, color):
        self.select_parts(selector).set_color(color)
        return self

    def set_parts_color_by_dict(self, color_map):
        for selector, color in color_map.items():
            self.set_parts_color(selector, color)
        return self

    def get_string(self):
        return self.string


class MarkupText(StringMobject):
    """The Reference's MarkupText over the Scribe bridge (fmn-library
    text.rs): one glyph per child, shaped by the bundled FontBook. The
    native span map powers isolate, t2c, and substring selection without
    an SVG labelling pass. Font/weight/gradient maps beyond the bundled
    tier remain precise refusals."""

    _native_markup = True
    _hoist_descendant_records = False

    def __init__(
        self,
        text: str,
        font_size: int = 48,
        height: float | None = None,
        justify: bool = False,
        indent: float = 0,
        alignment: str = "",
        line_width: float | None = None,
        font: str = "",
        slant: str = "NORMAL",
        weight: str = "NORMAL",
        gradient: Iterable[ManimColor] | None = None,
        line_spacing_height: float | None = None,
        text2color: dict = {},
        text2font: dict = {},
        text2gradient: dict = {},
        text2slant: dict = {},
        text2weight: dict = {},
        lsh: float | None = None,
        t2c: dict = {},
        t2f: dict = {},
        t2g: dict = {},
        t2s: dict = {},
        t2w: dict = {},
        global_config: dict = {},
        local_configs: dict = {},
        disable_ligatures: bool = True,
        isolate: Selector = _re.compile(r"\w+", _re.U),
        **kwargs,
    ):
        self.use_labelled_svg = bool(kwargs.pop("use_labelled_svg", False))
        kwargs.pop("path_string_config", None)
        self.base_color = kwargs.pop("base_color", "#FFFFFF")
        self.protect = kwargs.pop("protect", ())
        _refuse_unrouted(
            type(self).__name__ + "()",
            [
                ("alignment", alignment != ""),
                ("font", font != ""),
                ("slant", slant != "NORMAL"),
                ("weight", weight != "NORMAL"),
                ("gradient", gradient is not None),
                ("line_spacing_height", line_spacing_height is not None),
                ("lsh", lsh is not None),
                ("text2font", bool(text2font)),
                ("text2gradient", bool(text2gradient)),
                ("text2slant", bool(text2slant)),
                ("text2weight", bool(text2weight)),
                ("t2f", bool(t2f)),
                ("t2g", bool(t2g)),
                ("t2s", bool(t2s)),
                ("t2w", bool(t2w)),
                ("global_config", bool(global_config)),
                ("local_configs", bool(local_configs)),
            ],
        )
        _install_live_state(self)
        self.text = str(text)
        self.string = self.text
        self.font_size = float(font_size)
        self.justify = bool(justify)
        self.indent = float(indent)
        self.alignment = alignment
        self.line_width = line_width
        self.font = font
        self.slant = slant
        self.weight = weight
        self.lsh = line_spacing_height or lsh
        self.t2c = dict(text2color or t2c)
        self.t2f = dict(text2font or t2f)
        self.t2g = dict(text2gradient or t2g)
        self.t2s = dict(text2slant or t2s)
        self.t2w = dict(text2weight or t2w)
        self.global_config = dict(global_config)
        self.local_configs = dict(local_configs)
        self.disable_ligatures = bool(disable_ligatures)
        self.isolate = isolate
        specs = self._build_text(
            _native_shell_factory,
            self.text,
            type(self)._native_markup,
            self.font_size,
            bool(justify),
            float(indent),
            None if line_width is None else float(line_width),
            bool(disable_ligatures),
            type(self)._hoist_descendant_records,
        )
        _hang_native_children(self, specs)
        _apply_vmobject_style_kwargs(self, kwargs)
        self.set_color_by_text_to_color_map(self.t2c)
        if height is not None:
            self.set_height(height)

    def get_parts_by_text(self, selector):
        return self.select_parts(selector)

    def get_part_by_text(self, selector, **kwargs):
        return self.select_part(selector, **kwargs)

    def set_color_by_text(self, selector, color):
        return self.set_parts_color(selector, color)

    def set_color_by_text_to_color_map(self, color_map):
        return self.set_parts_color_by_dict(color_map)

    def get_text(self):
        return self.get_string()


class _Alignment:
    """Pango alignment token used by MarkupText's Reference canvas."""

    VAL_DICT = {"LEFT": 0, "CENTER": 1, "RIGHT": 2}

    def __init__(self, s):
        self.value = self.VAL_DICT[str(s).upper()]


class Text(MarkupText):
    _native_markup = False

    def __init__(
        self,
        text: str,
        isolate: Selector = (
            _re.compile(r"\w+", _re.U),
            _re.compile(r"\S+", _re.U),
        ),
        use_labelled_svg: bool = True,
        path_string_config: dict = {"use_simple_quadratic_approx": True},
        **kwargs,
    ):
        super().__init__(
            text,
            isolate=isolate,
            use_labelled_svg=use_labelled_svg,
            path_string_config=path_string_config,
            **kwargs,
        )


class Code(MarkupText):
    """The Reference's Code over Scribe MarkupText as literal source.

    Pygments highlighting is not in the governed closure; off-default
    language / code_style / font / lsh stay named refusals until the fmd
    highlighter is wired. Default Consolas/monokai attributes are stored
    while the bundled text face typesets the source.
    """

    _native_markup = False
    _hoist_descendant_records = True

    def __init__(
        self,
        code,
        font="Consolas",
        font_size=24,
        lsh=1.0,
        fill_color=None,
        stroke_color=None,
        language="python",
        code_style="monokai",
        **kwargs,
    ):
        _refuse_unrouted(
            "Code()",
            [
                ("font", font != "Consolas"),
                ("lsh", lsh != 1.0),
                ("language", language != "python"),
                ("code_style", code_style != "monokai"),
            ],
        )
        style = dict(kwargs)
        if fill_color is not None:
            style["fill_color"] = fill_color
        if stroke_color is not None:
            style["stroke_color"] = stroke_color
        super().__init__(str(code), font_size=font_size, **style)
        self.code = str(code)
        self.font = font
        self.lsh = lsh
        self.language = language
        self.code_style = code_style


class Tex(StringMobject):
    """The Reference's Tex over fmd-math (fmn-library tex.rs). When a
    source exceeds fmd-math's current tier, the engine's refusal names
    the unsupported constructs and is surfaced VERBATIM — the fm-rqc
    corpus ratchet consumes those names from this exact message."""

    _native_text_mode = False
    _native_group_single_part = False
    tex_environment = "align*"

    def __init__(
        self,
        *tex_strings: str,
        font_size: int = 48,
        alignment: str = "\\centering",
        template: str = "",
        additional_preamble: str = "",
        tex_to_color_map: dict = {},
        t2c: dict = {},
        isolate: Selector = [],
        use_labelled_svg: bool = True,
        **kwargs,
    ):
        self.base_color = kwargs.pop("base_color", "#FFFFFF")
        self.protect = kwargs.pop("protect", ())
        _refuse_unrouted(
            type(self).__name__ + "()",
            [
                ("alignment", alignment != "\\centering"),
                ("template", template != ""),
                ("additional_preamble", additional_preamble != ""),
            ],
        )
        color_map = dict(t2c or {})
        color_map.update(tex_to_color_map or {})
        isolate = [] if isolate is None else isolate
        if len(tex_strings) > 1:
            if isinstance(isolate, (str, _re.Pattern, tuple)):
                isolate = [isolate]
            isolate = [*(isolate or []), *tex_strings]
        _install_live_state(self)
        self.alignment = alignment
        self.template = template
        self.additional_preamble = additional_preamble
        self.tex_to_color_map = color_map
        self.use_labelled_svg = bool(use_labelled_svg)
        self.isolate = isolate
        separator = getattr(self, "_tex_arg_separator", " ")
        self.tex_strings = [str(part) for part in tex_strings]
        if self.tex_strings:
            self.tex_strings[0] = self.tex_strings[0].lstrip()
            self.tex_strings[-1] = self.tex_strings[-1].rstrip()
        self.tex_string = separator.join(self.tex_strings).strip()
        if not self.tex_string:
            self.tex_strings = [r"\\"]
            self.tex_string = r"\\"
        self.string = self.tex_string
        self.font_size = float(font_size)
        # Multiple parts regroup glyph children per part via the typeset's
        # native source spans — the Reference's SingleStringTex structure.
        # `isolate=` partitions a single-part source the same way, and
        # `tex_to_color_map` string keys ride the native t2c surface; both
        # consume the span map, never a labelled second render.
        build_parts, build_separator = self._isolate_segments(separator)
        native_t2c = {
            key: value for key, value in color_map.items() if isinstance(key, str)
        }
        specs = self._build_tex(
            _native_shell_factory,
            build_parts,
            build_separator,
            bool(self._native_text_mode),
            self.font_size,
            native_t2c or None,
            bool(self._native_group_single_part),
        )
        _hang_native_children(self, specs)
        self._validate_isolate_spans()
        _apply_vmobject_style_kwargs(self, kwargs)
        self.set_color_by_tex_to_color_map(color_map)

    def _isolate_segments(self, separator):
        """Partition a single-part source at `isolate=` occurrence
        boundaries (source identity over the native span map), so each
        isolated piece becomes its own submobject group.  Multi-part
        construction already owns its part grouping and is left as-is."""
        if len(self.tex_strings) != 1 or not self.isolate:
            return self.tex_strings, separator
        cuts = {0, len(self.string)}
        for start, end in self.find_spans_by_selector(self.isolate):
            if start != end:
                cuts.update((start, end))
        ordered = sorted(cuts)
        segments = [
            self.string[start:end] for start, end in zip(ordered, ordered[1:])
        ]
        merged = []
        for segment in segments:
            if merged and (not segment.strip() or not merged[-1].strip()):
                merged[-1] += segment
            else:
                merged.append(segment)
        if len(merged) <= 1:
            return self.tex_strings, separator
        return merged, ""

    def _validate_isolate_spans(self):
        """A present isolate occurrence that resolves to no span-map
        primitive is a named error, never a silent no-op selection."""
        entries = self.isolate
        if not entries:
            return
        if isinstance(entries, (str, _re.Pattern, tuple)):
            entries = [entries]
        for entry in entries:
            for span in self.find_spans_by_selector(entry):
                if span[0] == span[1]:
                    continue
                start, end = self._byte_span(span)
                if not any(
                    start <= sub_start and sub_end <= end
                    for sub_start, sub_end in self._string_sub_spans
                ):
                    raise _TexError(
                        "isolate "
                        + repr(self.string[span[0] : span[1]])
                        + " is not in the native span map of "
                        + repr(self.string)
                    )

    def get_parts_by_tex(self, selector):
        return self.select_parts(selector)

    def get_part_by_tex(self, selector, index=0):
        return self.select_part(selector, index)

    def set_color_by_tex(self, selector, color):
        return self.set_parts_color(selector, color)

    def set_color_by_tex_to_color_map(self, color_map):
        return self.set_parts_color_by_dict(color_map)

    def get_tex(self):
        return self.tex_string

    def _handle_scale_side_effects(self, scale_factor):
        self.font_size *= scale_factor
        return self

    def make_number_changeable(self, value, index=0, replace_all=False, **config):
        substr = str(value)
        occurrences = [
            ordinals for ordinals in self._selected_string_ordinals(substr) if ordinals
        ]
        if not occurrences or index >= len(occurrences):
            return VMobject()
        selected = occurrences if replace_all else [occurrences[index]]
        # fm-5wq.4.85: two-level families (multi-part Tex, sub-paths
        # [part, glyph]) replace inside the owning part, keeping the part
        # structure. What is genuinely missing refuses by name: deeper or
        # mixed nesting, and a number whose glyphs cross a part boundary.
        nested = any(len(path) != 1 for path in self._string_sub_paths)
        if nested and not all(
            len(path) == 2 for path in self._string_sub_paths
        ):
            raise NotImplementedError(
                "Tex.make_number_changeable over glyph families nested "
                "deeper than one part level awaits a general grouped-span "
                "replacement seam"
            )
        if nested:
            for ordinals in selected:
                touched = {
                    self._string_sub_paths[ordinal][0]
                    for ordinal in ordinals
                }
                if len(touched) != 1:
                    raise NotImplementedError(
                        "Tex.make_number_changeable across part "
                        "boundaries awaits the grouped-span replacement "
                        "seam"
                    )

        if "num_decimal_places" not in config:
            config["num_decimal_places"] = (
                len(substr.split(".", 1)[1]) if "." in substr else 0
            )
        replacements = []
        for ordinals in selected:
            part = VGroup(*(self._string_submobject(ordinal) for ordinal in ordinals))
            decimal = DecimalNumber(float(value), **config)
            decimal.replace(part)
            decimal.match_style(part)
            replacements.append((ordinals, decimal))

        by_first = {ordinals[0]: (set(ordinals), decimal) for ordinals, decimal in replacements}
        removed = {ordinal for ordinals, _ in replacements for ordinal in ordinals[1:]}
        if nested:
            # Rebuild inside each owning part: replaced runs collapse to
            # their DecimalNumber, later glyphs in the same part re-index,
            # and the root's part list never changes.
            parts = list(self.submobjects)
            part_children = {index: [] for index in range(len(parts))}
            spans = []
            paths = []
            for ordinal, span in enumerate(self._string_sub_spans):
                part_index = self._string_sub_paths[ordinal][0]
                bucket = part_children[part_index]
                replacement = by_first.get(ordinal)
                if replacement is not None:
                    selected_ordinals, decimal = replacement
                    bucket.append(decimal)
                    spans.append(
                        (
                            min(self._string_sub_spans[item][0] for item in selected_ordinals),
                            max(self._string_sub_spans[item][1] for item in selected_ordinals),
                        )
                    )
                    paths.append([part_index, len(bucket) - 1])
                elif ordinal not in removed:
                    bucket.append(self._string_submobject(ordinal))
                    spans.append(span)
                    paths.append([part_index, len(bucket) - 1])
            for part_index, part in enumerate(parts):
                part.set_submobjects(part_children[part_index])
            self._string_sub_spans = spans
            self._string_sub_paths = paths
        else:
            children = []
            spans = []
            paths = []
            for ordinal, span in enumerate(self._string_sub_spans):
                replacement = by_first.get(ordinal)
                if replacement is not None:
                    selected_ordinals, decimal = replacement
                    children.append(decimal)
                    spans.append(
                        (
                            min(self._string_sub_spans[item][0] for item in selected_ordinals),
                            max(self._string_sub_spans[item][1] for item in selected_ordinals),
                        )
                    )
                    paths.append([len(children) - 1])
                elif ordinal not in removed:
                    children.append(self._string_submobject(ordinal))
                    spans.append(span)
                    paths.append([len(children) - 1])
            self.set_submobjects(children)
            self._string_sub_spans = spans
            self._string_sub_paths = paths
        self.tex_string = self.tex_string.replace(substr, "\\decimalmob", len(replacements))
        if not nested:
            self.tex_strings = [self.tex_string]
        self.string = self.tex_string
        decimal_mobs = [decimal for _, decimal in replacements]
        return VGroup(*decimal_mobs) if replace_all else decimal_mobs[0]


class TexText(Tex):
    _native_text_mode = True
    tex_environment = ""


class BulletedList(VGroup):
    """Atlas's native, bundled-font replacement for the Reference's
    implicit ``itemize``/``enumerate`` TeX composition."""

    def __init__(
        self,
        *items: str,
        buff: float = _MED_LARGE_BUFF,
        aligned_edge=_LEFT,
        numbered: bool = False,
        **kwargs,
    ):
        font_size = float(kwargs.pop("font_size", 48))
        style_kwargs = dict(kwargs)
        _preflight_vmobject_style_kwargs(style_kwargs)
        if not all(isinstance(item, str) for item in items):
            raise TypeError("BulletedList items must be strings")
        aligned_edge = _vec3(aligned_edge)
        buff = float(buff)

        _install_live_state(self)
        self.items = tuple(items)
        self.buff = buff
        self.aligned_edge = _np.array(aligned_edge)
        self.numbered = bool(numbered)
        self.font_size = font_size
        specs = self._build_bulleted_list(
            _native_shell_factory,
            list(items),
            buff,
            aligned_edge,
            self.numbered,
            font_size,
        )
        _hang_native_children(self, specs)
        _apply_vmobject_style_kwargs(self, style_kwargs)

    def fade_all_but(self, index: int, opacity: float = 0.25, scale_factor=0.7):
        # Reference special_tex.py:41-47 verbatim over the live Marionette
        # family. Keeping this as ordinary portal composition preserves the
        # identity of every caller-visible item while native Atlas remains
        # the sole construction/layout authority.
        max_dot_height = max(item[0].get_height() for item in self.submobjects)
        for item_index, part in enumerate(self.submobjects):
            target_height = (
                1.0 if item_index == index else float(scale_factor)
            ) * max_dot_height
            part.set_fill(
                opacity=(1.0 if item_index == index else float(opacity))
            )
            current_height = part[0].get_height()
            if current_height > 0:
                part.scale(target_height / current_height, about_edge=_LEFT)


class Title(TexText):
    """Atlas's native title composition with Scribe part provenance and a
    real Line underline, retaining the Reference's TexText-facing shape."""

    def __init__(
        self,
        *text_parts: str,
        font_size: int = 72,
        include_underline: bool = True,
        underline_width: float = _FRAME_SHAPE[0] - 2,
        match_underline_width_to_text: bool = False,
        underline_buff: float = _SMALL_BUFF,
        underline_style: dict = dict(stroke_width=2, stroke_color=_GREY_C),
        **kwargs,
    ):
        if not all(isinstance(part, str) for part in text_parts):
            raise TypeError("Title parts must be strings")
        parts = list(text_parts)
        if parts:
            parts[0] = parts[0].lstrip()
            parts[-1] = parts[-1].rstrip()
        style_kwargs = dict(kwargs)
        underline_style = dict(underline_style)
        _preflight_vmobject_style_kwargs(style_kwargs)
        _preflight_vmobject_style_kwargs(underline_style)
        font_size = float(font_size)
        underline_width = float(underline_width)
        underline_buff = float(underline_buff)

        _install_live_state(self)
        self.tex_strings = parts
        self.tex_string = " ".join(parts)
        self.string = self.tex_string
        self.font_size = font_size
        self.use_labelled_svg = False
        self.isolate = list(parts)
        spans = []
        paths = []
        byte_offset = 0
        for index, part in enumerate(parts):
            end = byte_offset + len(part.encode("utf-8"))
            spans.append((byte_offset, end))
            paths.append([index])
            byte_offset = end + 1
        self._string_sub_spans = spans
        self._string_sub_paths = paths

        specs = self._build_title(
            _native_shell_factory,
            parts,
            font_size,
            bool(include_underline),
            underline_width,
            bool(match_underline_width_to_text),
            underline_buff,
        )
        _hang_native_children(self, specs)
        expected_children = len(parts) + int(bool(include_underline))
        if len(self.submobjects) != expected_children:
            raise RuntimeError(
                "native Title family contract drift: "
                f"expected {expected_children} children, got {len(self.submobjects)}"
            )

        # TexText kwargs style only the text. The underline is constructed
        # afterwards in the Reference and therefore receives its own Line
        # defaults plus underline_style, never the title's text color.
        _apply_vmobject_style_kwargs(self, dict(style_kwargs), recurse=False)
        for part in self.submobjects[: len(parts)]:
            _apply_vmobject_style_kwargs(part, dict(style_kwargs))
        if include_underline:
            self.underline = self.submobjects[-1]
            self.underline.set_fill(color=_WHITE, opacity=0)
            self.underline.set_stroke(color=_WHITE, width=2, opacity=1)
            _apply_vmobject_style_kwargs(self.underline, underline_style)


_MAX_MATRIX_ENTRIES = 4096


def _bounded_matrix_rows(matrix):
    """Materialize at most Atlas's public matrix-entry budget.

    Generators are bounded before an untrusted row can force an unbounded
    copy. Shape equality remains the native builder's typed, atomic check.
    """
    try:
        outer = iter(matrix)
    except TypeError as error:
        raise TypeError("matrix must be an iterable of row iterables") from error
    rows = []
    total = 0
    for row in _itertools.islice(outer, _MAX_MATRIX_ENTRIES + 1):
        if len(rows) == _MAX_MATRIX_ENTRIES:
            raise ValueError(
                f"matrix rows exceed the declared limit of {_MAX_MATRIX_ENTRIES}"
            )
        try:
            values = list(
                _itertools.islice(iter(row), _MAX_MATRIX_ENTRIES + 1)
            )
        except TypeError as error:
            raise TypeError("matrix rows must be iterable") from error
        if len(values) > _MAX_MATRIX_ENTRIES:
            raise ValueError(
                f"matrix columns exceed the declared limit of {_MAX_MATRIX_ENTRIES}"
            )
        total += len(values)
        if total > _MAX_MATRIX_ENTRIES:
            raise ValueError(
                f"matrix entries require {total} items, above the declared "
                f"limit of {_MAX_MATRIX_ENTRIES}"
            )
        rows.append(values)
    return rows


def _matrix_common_config(config):
    """Consume the base Matrix constructor keys from a typed variant."""
    config = dict(config)
    common = dict(
        v_buff=float(config.pop("v_buff", 0.5)),
        h_buff=float(config.pop("h_buff", 0.5)),
        bracket_h_buff=float(config.pop("bracket_h_buff", 0.2)),
        bracket_v_buff=float(config.pop("bracket_v_buff", 0.25)),
        height=config.pop("height", None),
        element_alignment_corner=config.pop(
            "element_alignment_corner", _DOWN
        ),
        ellipses_row=config.pop("ellipses_row", None),
        ellipses_col=config.pop("ellipses_col", None),
    )
    if config:
        raise TypeError(
            "unexpected keyword arguments: " + ", ".join(sorted(config))
        )
    if common["height"] is not None:
        common["height"] = float(common["height"])
    return common


def _matrix_entry_config(config, *, decimal_places=None):
    """Split the native entry-size knob from style applied to live entries."""
    config = dict(config)
    font_size = float(config.pop("font_size", 48))
    if decimal_places is None:
        places = config.pop("num_decimal_places", None)
        if places is not None:
            places = int(places)
    else:
        if "num_decimal_places" in config:
            raise TypeError("num_decimal_places was supplied twice")
        places = int(decimal_places)
    unsupported = sorted(set(config) - _NATIVE_VMOBJECT_STYLE_KEYS - {"shading"})
    _refuse_unrouted(
        "Matrix element configuration",
        [(name, True) for name in unsupported],
    )
    _preflight_vmobject_style_kwargs(config)
    if not _math.isfinite(font_size) or font_size <= 0:
        raise ValueError("matrix entry font_size must be positive and finite")
    if places is not None and places < 0:
        raise ValueError("num_decimal_places must be non-negative")
    return font_size, places, config


def _matrix_ellipsis_indices(n_rows, n_cols, row_index, col_index):
    def normalized(index, length):
        if index is None or not -length <= index < length:
            return None
        return index % length

    row_index = normalized(row_index, n_rows)
    col_index = normalized(col_index, n_cols)
    indices = []
    if row_index is not None:
        indices.extend(row_index * n_cols + col for col in range(n_cols))
    if col_index is not None:
        indices.extend(
            row * n_cols + col_index
            for row in range(n_rows)
            if row * n_cols + col_index not in indices
        )
    return indices


def _decorate_matrix_tex_entry(entry, source, font_size):
    entry.__class__ = Tex
    source = str(source).strip() or r"\\"
    entry.alignment = r"\centering"
    entry.template = ""
    entry.additional_preamble = ""
    entry.tex_to_color_map = {}
    entry.use_labelled_svg = False
    entry.isolate = []
    entry.tex_strings = [source]
    entry.tex_string = source
    entry.string = source
    entry.font_size = font_size
    # The Matrix builder retains geometry rather than fmd-math's complete
    # per-primitive span table. The whole entry remains selectable as one
    # source unit; substring-granular selection is deliberately not claimed.
    entry._string_sub_spans = [(0, len(source.encode("utf-8")))]
    entry._string_sub_paths = [[]]


def _decorate_matrix_decimal_entry(entry, value, font_size, places, style):
    entry.__class__ = DecimalNumber
    entry.number = float(value)
    entry.font_size = font_size
    entry.edge_to_fix = _np.array(_LEFT)
    # fm-5wq.4.80's complex-mode flags, which __init__ normally sets: a
    # reclassed native shell is a real value display, always in real mode.
    entry.hide_zero_components_on_complex = True
    entry._complex_imag_mode = False
    entry._decimal_params = (
        places,
        0,
        False,
        True,
        0.001,
        False,
        None,
        False,
        _vec3(_LEFT),
        font_size,
        style.get("color"),
        float(style.get("stroke_width", 0)),
        float(style.get("fill_opacity", 1.0)),
        float(style.get("fill_border_width", 0.5)),
    )


def _initialize_scalar_matrix(
    matrix,
    rows,
    *,
    kind,
    entry_sources,
    font_size,
    decimal_places,
    entry_style,
    v_buff,
    h_buff,
    bracket_h_buff,
    bracket_v_buff,
    height,
    element_alignment_corner,
    ellipses_row,
    ellipses_col,
    ellipses_height_ratio=0.65,
    ellipses_width_ratio=0.4,
):
    _install_live_state(matrix)
    n_rows = len(rows)
    n_cols = len(rows[0]) if rows else 0
    common = (
        float(v_buff),
        float(h_buff),
        float(bracket_h_buff),
        float(bracket_v_buff),
        None if height is None else float(height),
        _vec3(element_alignment_corner),
        None if ellipses_row is None else int(ellipses_row),
        None if ellipses_col is None else int(ellipses_col),
        float(ellipses_height_ratio),
        float(ellipses_width_ratio),
        float(font_size),
    )
    if kind == "tex":
        specs = matrix._build_tex_matrix(
            _native_shell_factory,
            [[str(value) for value in row] for row in rows],
            *common,
        )
    elif kind == "mixed":
        specs = matrix._build_mixed_matrix(
            _native_shell_factory,
            [
                [
                    (
                        isinstance(value, (float, _np.floating))
                        and not isinstance(value, bool),
                        float(value)
                        if isinstance(value, (float, _np.floating))
                        and not isinstance(value, bool)
                        else 0.0,
                        str(value),
                    )
                    for value in row
                ]
                for row in rows
            ],
            int(decimal_places),
            *common,
        )
    else:
        specs = matrix._build_decimal_matrix(
            _native_shell_factory,
            [[float(value) for value in row] for row in rows],
            kind == "integer",
            int(decimal_places),
            *common,
        )
    _hang_native_children(matrix, specs)
    n_entries = n_rows * n_cols
    if len(matrix.submobjects) != n_entries + 2:
        raise RuntimeError(
            "native Matrix family contract drift: "
            f"expected {n_entries + 2} children, got {len(matrix.submobjects)}"
        )
    entries = list(matrix.submobjects[:n_entries])
    flat_sources = [value for row in entry_sources for value in row]
    ellipse_indices = _matrix_ellipsis_indices(
        n_rows, n_cols, ellipses_row, ellipses_col
    )
    for index, (entry, source) in enumerate(zip(entries, flat_sources)):
        # IntegerMatrix is DecimalMatrix's zero-places route in the
        # Reference, so its entries are DecimalNumbers too — the native
        # builder already renders the rounded integer glyphs; only the
        # Python-side class decoration selects here.
        is_decimal = kind in ("decimal", "integer") or (
            kind == "mixed"
            and isinstance(source, (float, _np.floating))
            and not isinstance(source, bool)
        )
        if is_decimal:
            _decorate_matrix_decimal_entry(
                entry, source, font_size, decimal_places, entry_style
            )
        else:
            _decorate_matrix_tex_entry(entry, source, font_size)
        # Reference Matrix styles the source entry first, then `become`s a
        # freshly constructed default-style dot. The native builder has
        # already performed that replacement, so only surviving entries
        # receive the caller's entry style here.
        if index not in ellipse_indices:
            _apply_vmobject_style_kwargs(entry, dict(entry_style))
    matrix.mob_matrix = [
        entries[row * n_cols : (row + 1) * n_cols]
        for row in range(n_rows)
    ]
    matrix.rows = VGroup(*(VGroup(*row) for row in matrix.mob_matrix))
    matrix.columns = VGroup(
        *(
            VGroup(*(row[column] for row in matrix.mob_matrix))
            for column in range(n_cols)
        )
    )
    matrix.brackets = list(matrix.submobjects[n_entries:])
    matrix.ellipses = [entries[index] for index in ellipse_indices]
    matrix.elements = [
        entry for index, entry in enumerate(entries) if index not in ellipse_indices
    ]
    matrix._matrix_kind = kind
    matrix._matrix_shape = (n_rows, n_cols)
    matrix._matrix_entry_font_size = float(font_size)
    matrix._matrix_decimal_places = (
        2 if decimal_places is None else int(decimal_places)
    )
    matrix._matrix_entry_style = dict(entry_style)
    return matrix


class Matrix(VMobject):
    """Atlas's native scalar Matrix grid and bundled-math delimiters.

    Real entries take the DecimalNumber route and other scalars take the
    Reference's Tex(str(value)) route, including heterogeneous grids and
    explicit complex element conversion. Complex values and caller-owned
    VMobjects remain constructor refusals until the native grid accepts them.
    """

    def __init__(
        self,
        matrix,
        v_buff=0.5,
        h_buff=0.5,
        bracket_h_buff=0.2,
        bracket_v_buff=0.25,
        height=None,
        element_config=dict(),
        element_alignment_corner=_DOWN,
        ellipses_row=None,
        ellipses_col=None,
    ):
        rows = _bounded_matrix_rows(matrix)
        flat = [value for row in rows for value in row]
        _refuse_unrouted(
            "Matrix()",
            [
                ("complex entries", any(isinstance(value, complex) for value in flat)),
                ("VMobject entries", any(isinstance(value, VMobject) for value in flat)),
            ],
        )
        real_flags = [
            isinstance(value, (float, _np.floating)) and not isinstance(value, bool)
            for value in flat
        ]
        font_size, places, style = _matrix_entry_config(
            element_config, decimal_places=None
        )
        if real_flags and all(real_flags):
            kind = "decimal"
        elif any(real_flags):
            kind = "mixed"
        else:
            kind = "tex"
        if kind in {"decimal", "mixed"} and places is None:
            places = 2
        if kind == "tex" and places is not None:
            raise TypeError("num_decimal_places applies only to real Matrix entries")
        _initialize_scalar_matrix(
            self,
            rows,
            kind=kind,
            entry_sources=rows,
            font_size=font_size,
            decimal_places=places,
            entry_style=style,
            v_buff=v_buff,
            h_buff=h_buff,
            bracket_h_buff=bracket_h_buff,
            bracket_v_buff=bracket_v_buff,
            height=height,
            element_alignment_corner=element_alignment_corner,
            ellipses_row=ellipses_row,
            ellipses_col=ellipses_col,
        )

    def copy(self, deep=False):
        result = super().copy(deep)
        source_family = self.get_family()
        copied_family = result.get_family()
        mapping = dict(zip(source_family, copied_family))
        result.mob_matrix = [
            [mapping[entry] for entry in row] for row in self.mob_matrix
        ]
        result.rows = VGroup(*(VGroup(*row) for row in result.mob_matrix))
        result.columns = VGroup(
            *(
                VGroup(*(row[column] for row in result.mob_matrix))
                for column in range(result._matrix_shape[1])
            )
        )
        result.elements = [mapping[entry] for entry in self.elements]
        result.ellipses = [mapping[entry] for entry in self.ellipses]
        result.brackets = [mapping[bracket] for bracket in self.brackets]
        return result

    def create_mobject_matrix(self, *args, **kwargs):
        del args, kwargs
        raise NotImplementedError(
            "Matrix.create_mobject_matrix is constructor-owned by the native "
            "grid engine; use Matrix(...)"
        )

    def element_to_mobject(self, element):
        if not isinstance(self, Matrix):
            raise TypeError(
                "Matrix.element_to_mobject requires a Matrix instance"
            )
        if isinstance(element, VMobject):
            return element
        style = dict(self._matrix_entry_style)
        if isinstance(element, (complex, _np.complexfloating)):
            # matrix.py routes complex entries through DecimalNumber
            # (fm-5wq.4.108): the degenerate-component reductions ride the
            # native formatter (fm-5wq.4.80), and a general complex entry
            # inherits BN-08's named refusal from DecimalNumber itself —
            # never Tex(str(...))'s silent "(1+2j)" fallthrough.
            return DecimalNumber(
                complex(element),
                num_decimal_places=self._matrix_decimal_places,
                font_size=self._matrix_entry_font_size,
                **style,
            )
        if isinstance(element, (float, _np.floating)) and not isinstance(
            element, bool
        ):
            return DecimalNumber(
                float(element),
                num_decimal_places=self._matrix_decimal_places,
                font_size=self._matrix_entry_font_size,
                **style,
            )
        return Tex(
            str(element),
            font_size=self._matrix_entry_font_size,
            **style,
        )

    def create_brackets(self, *args, **kwargs):
        del args, kwargs
        raise NotImplementedError(
            "Matrix.create_brackets is owned by the bundled native delimiter engine"
        )

    def get_column(self, index):
        n_cols = self._matrix_shape[1]
        if not 0 <= index < n_cols:
            raise IndexError(
                f"Index {index} out of bound for matrix with {n_cols} columns"
            )
        return self.columns[index]

    def get_row(self, index):
        n_rows = self._matrix_shape[0]
        if not 0 <= index < n_rows:
            raise IndexError(
                f"Index {index} out of bound for matrix with {n_rows} rows"
            )
        return self.rows[index]

    def get_columns(self):
        return self.columns

    def get_rows(self):
        return self.rows

    def set_column_colors(self, *colors):
        for column, color in enumerate(colors[: self._matrix_shape[1]]):
            for row in self.mob_matrix:
                row[column].set_color(color)
        return self

    def add_background_to_entries(self):
        for entry in list(self.elements):
            entry.add_background_rectangle()
        return self

    def swap_entry_for_dots(self, entry, dots):
        del entry, dots
        raise NotImplementedError(
            "Matrix ellipsis replacement is native and constructor-time; "
            "pass ellipses_row or ellipses_col"
        )

    def swap_entries_for_ellipses(
        self,
        row_index=None,
        col_index=None,
        height_ratio=0.65,
        width_ratio=0.4,
    ):
        del row_index, col_index, height_ratio, width_ratio
        raise NotImplementedError(
            "Matrix ellipsis replacement is native and constructor-time; "
            "pass ellipses_row or ellipses_col"
        )

    def get_mob_matrix(self):
        return self.mob_matrix

    def get_entries(self):
        return VGroup(*self.elements)

    def get_brackets(self):
        return VGroup(*self.brackets)

    def get_ellipses(self):
        return VGroup(*self.ellipses)


class DecimalMatrix(Matrix):
    def __init__(
        self,
        matrix,
        num_decimal_places=2,
        decimal_config=dict(),
        **config,
    ):
        rows = _bounded_matrix_rows(matrix)
        common = _matrix_common_config(config)
        font_size, places, style = _matrix_entry_config(
            decimal_config, decimal_places=num_decimal_places
        )
        numeric = [[float(value) for value in row] for row in rows]
        self.float_matrix = matrix
        _initialize_scalar_matrix(
            self,
            numeric,
            kind="decimal",
            entry_sources=numeric,
            font_size=font_size,
            decimal_places=places,
            entry_style=style,
            **common,
        )

    def element_to_mobject(self, *args, **kwargs):
        del args, kwargs
        raise NotImplementedError(
            "DecimalMatrix.element_to_mobject is constructor-owned by Atlas"
        )


class IntegerMatrix(DecimalMatrix):
    def __init__(
        self,
        matrix,
        num_decimal_places=0,
        decimal_config=dict(),
        **config,
    ):
        rows = _bounded_matrix_rows(matrix)
        common = _matrix_common_config(config)
        font_size, places, style = _matrix_entry_config(
            decimal_config, decimal_places=num_decimal_places
        )
        numeric = [[float(value) for value in row] for row in rows]
        self.float_matrix = matrix
        _initialize_scalar_matrix(
            self,
            numeric,
            kind="integer",
            entry_sources=numeric,
            font_size=font_size,
            decimal_places=places,
            entry_style=style,
            **common,
        )


class TexMatrix(Matrix):
    def __init__(self, matrix, tex_config=dict(), **config):
        rows = _bounded_matrix_rows(matrix)
        common = _matrix_common_config(config)
        font_size, places, style = _matrix_entry_config(
            tex_config, decimal_places=None
        )
        if places is not None:
            raise TypeError("num_decimal_places is not a TexMatrix option")
        sources = [[str(value) for value in row] for row in rows]
        _initialize_scalar_matrix(
            self,
            sources,
            kind="tex",
            entry_sources=sources,
            font_size=font_size,
            decimal_places=None,
            entry_style=style,
            **common,
        )


class MobjectMatrix(Matrix):
    """Atlas's native matrix layout applied to caller-owned VMobjects."""

    def __init__(
        self,
        group,
        n_rows=None,
        n_cols=None,
        height=4.0,
        element_alignment_corner=_ORIGIN,
        **config,
    ):
        if not isinstance(group, VGroup):
            raise TypeError("MobjectMatrix group must be a VGroup")
        entries = list(group)
        count = len(entries)
        if n_rows is not None:
            n_rows = _operator.index(n_rows)
        if n_cols is not None:
            n_cols = _operator.index(n_cols)
        if n_rows is None:
            n_rows = int(_math.sqrt(count)) if n_cols is None else count // n_cols
        if n_cols is None:
            n_cols = count // n_rows
        if n_rows <= 0 or n_cols <= 0:
            raise ValueError("MobjectMatrix dimensions must be positive")
        required = n_rows * n_cols
        if required > _MAX_MATRIX_ENTRIES:
            raise ValueError(
                f"matrix entries require {required} items, above the declared "
                f"limit of {_MAX_MATRIX_ENTRIES}"
            )
        if count < required:
            raise Exception(
                "Input to MobjectMatrix must have at least n_rows * n_cols entries"
            )

        common = _matrix_common_config(config)
        common["height"] = float(height)
        common["element_alignment_corner"] = _vec3(element_alignment_corner)
        used_entries = entries[:required]
        extents = []
        for entry in used_entries:
            box = entry.get_bounding_box()
            extents.append((_vec3(box[0]), _vec3(box[2])))

        _install_live_state(self)
        specs = self._build_mobject_matrix(
            _native_shell_factory,
            extents,
            n_rows,
            n_cols,
            common["v_buff"],
            common["h_buff"],
            common["bracket_h_buff"],
            common["bracket_v_buff"],
            common["height"],
            common["element_alignment_corner"],
            common["ellipses_row"],
            common["ellipses_col"],
        )
        _hang_native_children(self, specs)
        if len(self.submobjects) != required + 2:
            raise RuntimeError(
                "native MobjectMatrix family contract drift: "
                f"expected {required + 2} children, got {len(self.submobjects)}"
            )
        targets = list(self.submobjects[:required])
        brackets = list(self.submobjects[required:])
        ellipse_indices = _matrix_ellipsis_indices(
            n_rows,
            n_cols,
            common["ellipses_row"],
            common["ellipses_col"],
        )
        for index, (entry, target) in enumerate(zip(used_entries, targets)):
            if index in ellipse_indices:
                entry.become(target)
                continue
            lengths = [entry.length_over_dim(dim) for dim in range(3)]
            scale_dim = max(range(3), key=lengths.__getitem__)
            if lengths[scale_dim] > 0:
                entry.scale(
                    target.length_over_dim(scale_dim) / lengths[scale_dim]
                )
            entry.shift(target.get_center() - entry.get_center())
        self.set_submobjects([*used_entries, *brackets])
        self.group = group
        self.mob_matrix = [
            used_entries[row * n_cols : (row + 1) * n_cols]
            for row in range(n_rows)
        ]
        self.rows = VGroup(*(VGroup(*row) for row in self.mob_matrix))
        self.columns = VGroup(
            *(
                VGroup(*(row[column] for row in self.mob_matrix))
                for column in range(n_cols)
            )
        )
        self.brackets = brackets
        self.ellipses = [used_entries[index] for index in ellipse_indices]
        self.elements = [
            entry
            for index, entry in enumerate(used_entries)
            if index not in ellipse_indices
        ]
        self._matrix_kind = "mobject"
        self._matrix_shape = (n_rows, n_cols)
        self._matrix_entry_font_size = 48.0
        self._matrix_decimal_places = 2
        self._matrix_entry_style = {}

    def element_to_mobject(self, element, **config):
        del config
        return element


class TexTextFromPresetString(TexText):
    """The Reference preset-string base over the native Scribe TexText.

    Concrete presets ordinarily inherit this constructor. The two pifont
    marks below preserve that MRO but override construction because Atlas
    owns their de-TeX'd paths under BN-08.
    """

    tex = ""
    default_color = _WHITE

    def __init__(self, **kwargs):
        super().__init__(
            self.tex,
            color=kwargs.pop("color", self.default_color),
            **kwargs,
        )


def _init_native_drawn_mark(mark, build, kwargs):
    """Install one Atlas mark while retaining TexText's one-glyph surface."""
    kwargs = dict(kwargs)
    color = kwargs.pop("color", mark.default_color)
    font_size = float(kwargs.pop("font_size", 48))
    alignment = kwargs.pop("alignment", "\\centering")
    template = kwargs.pop("template", "")
    additional_preamble = kwargs.pop("additional_preamble", "")
    color_map = dict(kwargs.pop("t2c", {}) or {})
    color_map.update(dict(kwargs.pop("tex_to_color_map", {}) or {}))
    isolate = kwargs.pop("isolate", [])
    use_labelled_svg = bool(kwargs.pop("use_labelled_svg", True))
    base_color = kwargs.pop("base_color", _WHITE)
    protect = kwargs.pop("protect", ())
    _refuse_unrouted(
        type(mark).__name__ + "()",
        [
            ("alignment", alignment != "\\centering"),
            ("template", template != ""),
            ("additional_preamble", additional_preamble != ""),
        ],
    )
    if not _math.isfinite(font_size) or font_size <= 0:
        raise ValueError("drawn-mark font_size must be positive and finite")

    _install_live_state(mark)
    mark.alignment = alignment
    mark.template = template
    mark.additional_preamble = additional_preamble
    mark.tex_to_color_map = color_map
    mark.use_labelled_svg = use_labelled_svg
    mark.isolate = isolate
    mark.base_color = base_color
    mark.protect = protect
    mark.tex_strings = [mark.tex]
    mark.tex_string = mark.tex
    mark.string = mark.tex
    mark.font_size = font_size
    specs = build(_native_shell_factory, color)
    _hang_native_children(mark, specs)
    # The native tree is an empty TexText-family root with one drawn child.
    # Its selector span therefore points at that child exactly as a one-glyph
    # Scribe typeset would.
    mark._string_sub_spans = [(0, len(mark.tex.encode("utf-8")))]
    mark._string_sub_paths = [[0]]
    kwargs["color"] = color
    _apply_vmobject_style_kwargs(mark, kwargs)
    if font_size != 48:
        mark.scale(font_size / 48)
    mark.set_color_by_tex_to_color_map(color_map)


class Checkmark(TexTextFromPresetString):
    tex = r"\ding{51}"
    default_color = _GREEN

    def __init__(self, **kwargs):
        _init_native_drawn_mark(self, self._build_checkmark, kwargs)


class Exmark(TexTextFromPresetString):
    tex = r"\ding{55}"
    default_color = _RED

    def __init__(self, **kwargs):
        _init_native_drawn_mark(self, self._build_exmark, kwargs)


class Cross(VGroup):
    """Atlas's native tapered cross over a live family extent."""

    def __init__(
        self,
        mobject,
        stroke_color=_RED,
        stroke_width=[0, 6, 0],
        **kwargs,
    ):
        if not isinstance(mobject, _BridgeMobject):
            raise TypeError("Cross expects a Mobject")
        _install_live_state(self)
        rows = mobject._bbox_rows()
        has_extent = any(member._has_points() for member in _family_preorder(mobject))
        specs = self._build_cross(
            _native_shell_factory,
            (_vec3(rows[0]), _vec3(rows[2])) if has_extent else None,
            stroke_color,
        )
        _hang_native_children(self, specs)
        _apply_vmobject_style_kwargs(self, kwargs)
        self.set_stroke(stroke_color, width=stroke_width)


class Underline(Line):
    """Atlas's native tapered rule beneath a live family extent."""

    def __init__(
        self,
        mobject,
        buff=_SMALL_BUFF,
        stroke_color=_WHITE,
        stroke_width=[0, 3, 3, 0],
        stretch_factor=1.2,
        **kwargs,
    ):
        if not isinstance(mobject, _BridgeMobject):
            raise TypeError("Underline expects a Mobject")
        path_arc = float(kwargs.pop("path_arc", 0.0))
        _install_live_state(self)
        self.path_arc = 0.0
        self.buff = float(buff)
        rows = mobject._bbox_rows()
        has_extent = any(member._has_points() for member in _family_preorder(mobject))
        specs = self._build_underline(
            _native_shell_factory,
            (_vec3(rows[0]), _vec3(rows[2])) if has_extent else None,
            stroke_color,
            self.buff,
            float(stretch_factor),
        )
        _hang_native_children(self, specs)
        # Atlas owns the extent-derived endpoints; inherited Line owns the
        # native path_arc rebuild, so curvature does not rederive placement.
        if path_arc != 0.0:
            self.set_path_arc(path_arc)
        _apply_vmobject_style_kwargs(self, kwargs)
        self.set_stroke(stroke_color, width=stroke_width)


class Brace(Tex):
    """Atlas's parametric brace with Reference-compatible live tip helpers."""

    def __init__(
        self,
        mobject,
        direction=_DOWN,
        buff=0.2,
        tex_string=r"\underbrace{\qquad}",
        **kwargs,
    ):
        _install_live_state(self)
        # Brace keeps the Reference's Tex MRO even though Atlas supplies its
        # points directly. Preserve Tex's public scale bookkeeping so an
        # inherited group transform cannot reach a half-initialized object.
        self.font_size = float(kwargs.pop("font_size", 48))
        self.tex_string = str(tex_string)
        self.tex_strings = [self.tex_string]
        specs, tip_index = self._build_brace(
            _native_shell_factory,
            mobject,
            _vec3(direction),
            float(buff),
        )
        self.tip_point_index = int(tip_index)
        _hang_native_children(self, specs)
        _apply_vmobject_style_kwargs(self, kwargs)

    def set_initial_width(self, width):
        self.set_width(float(width), stretch=True)
        return self

    def get_tip(self):
        return self.get_points()[self.tip_point_index].copy()

    def get_direction(self):
        vector = self.get_tip() - self.get_center()
        norm = float(_np.linalg.norm(vector))
        return vector / norm if norm else _np.array(_DOWN)

    def put_at_tip(self, mob, use_next_to=True, **kwargs):
        if use_next_to:
            mob.next_to(self.get_tip(), _np.round(self.get_direction()), **kwargs)
        else:
            mob.move_to(self.get_tip())
            buff = float(kwargs.get("buff", _DEFAULT_MOBJECT_TO_MOBJECT_BUFF))
            mob.shift(self.get_direction() * (mob.get_width() / 2.0 + buff))
        return self

    def get_text(self, text, **kwargs):
        buff = kwargs.pop("buff", _SMALL_BUFF)
        result = Text(text, **kwargs)
        self.put_at_tip(result, buff=buff)
        return result

    def get_tex(self, *tex, **kwargs):
        buff = kwargs.pop("buff", _SMALL_BUFF)
        result = Tex(*tex, **kwargs)
        self.put_at_tip(result, buff=buff)
        return result


class LineBrace(Brace):
    def __init__(self, line, direction=_UP, **kwargs):
        buff = float(kwargs.pop("buff", 0.2))
        tex_string = kwargs.pop("tex_string", r"\underbrace{\qquad}")
        _install_live_state(self)
        self.font_size = float(kwargs.pop("font_size", 48))
        self.tex_string = str(tex_string)
        self.tex_strings = [self.tex_string]
        specs, tip_index = self._build_line_brace(
            _native_shell_factory,
            _vec3(line.get_start()),
            _vec3(line.get_end()),
            _vec3(direction),
            buff,
        )
        self.tip_point_index = int(tip_index)
        _hang_native_children(self, specs)
        _apply_vmobject_style_kwargs(self, kwargs)


class SingleStringTex(SVGMobject):
    """The legacy one-string TeX leaf over Scribe's native layout."""

    height = None

    def __init__(
        self,
        tex_string,
        height=None,
        fill_color=_WHITE,
        fill_opacity=1.0,
        stroke_width=0,
        svg_default={"fill_color": _WHITE},
        path_string_config={},
        font_size=48,
        alignment=r"\centering",
        math_mode=True,
        organize_left_to_right=False,
        template="",
        additional_preamble="",
        **kwargs,
    ):
        _refuse_unrouted(
            "SingleStringTex()",
            [
                ("alignment", alignment != r"\centering"),
                ("template", template != ""),
                ("additional_preamble", additional_preamble != ""),
            ],
        )
        style = dict(kwargs)
        style.update(
            fill_color=fill_color,
            fill_opacity=fill_opacity,
            stroke_width=stroke_width,
        )
        _preflight_vmobject_style_kwargs(style)
        _install_live_state(self)
        self.tex_string = str(tex_string)
        self.svg_default = dict(svg_default)
        self.path_string_config = dict(path_string_config)
        self.font_size = font_size
        self.alignment = alignment
        self.math_mode = bool(math_mode)
        self.organize_left_to_right = bool(organize_left_to_right)
        self.template = template
        self.additional_preamble = additional_preamble
        source = self.get_modified_expression(self.tex_string)
        specs = self._build_tex(
            _native_shell_factory,
            [source],
            "",
            not self.math_mode,
            float(font_size),
            None,
            False,
        )
        _hang_native_children(self, specs)
        _apply_vmobject_style_kwargs(self, style)
        if height is not None:
            self.set_height(float(height))
        if self.organize_left_to_right:
            self.sort(lambda point: point[0])

    @property
    def hash_seed(self):
        return (
            type(self).__name__,
            self.svg_default,
            self.path_string_config,
            self.tex_string,
            self.alignment,
            self.math_mode,
            self.template,
            self.additional_preamble,
        )

    def get_tex(self):
        return self.tex_string

    def get_modified_expression(self, tex_string):
        return self.modify_special_strings(str(tex_string).strip())

    def modify_special_strings(self, tex):
        tex = str(tex).strip()
        if tex in {r"\over", r"\overline", r"\sqrt", r"\sqrt{"} or tex.endswith(
            ("_", "^", "dot")
        ):
            tex += r"{\quad}"
        if tex == r"\overset":
            tex += r"{\quad}{\quad}"
        if tex == r"\substack":
            tex = r"\quad"
        if not tex:
            tex = r"\quad"
        if tex.startswith(r"\\"):
            tex = tex.replace(r"\\", r"\quad\\")
        tex = self.balance_braces(tex)
        for marker in (r"\left", r"\right"):
            count = sum(
                bool(suffix) and suffix[0] in "(){}[]|.\\"
                for suffix in tex.split(marker)[1:]
            )
            if marker == r"\left":
                left_count = count
            else:
                right_count = count
        if left_count != right_count:
            tex = tex.replace(r"\left", r"\big").replace(r"\right", r"\big")
        begin = r"\begin{array}" in tex
        end = r"\end{array}" in tex
        return "" if begin != end else tex

    def balance_braces(self, tex):
        unclosed = 0
        for index, char in enumerate(tex):
            if index > 0 and tex[index - 1] == "\\":
                continue
            if char == "{":
                unclosed += 1
            elif char == "}":
                if unclosed == 0:
                    tex = "{" + tex
                else:
                    unclosed -= 1
        return tex + unclosed * "}"

    def get_tex_file_body(self, tex_string):
        expression = self.get_modified_expression(tex_string)
        if self.math_mode:
            expression = "\\begin{align*}\n" + expression + "\n\\end{align*}"
        return self.alignment + "\n" + expression

    def organize_submobjects_left_to_right(self):
        self.sort(lambda point: point[0])
        return self


class OldTex(Tex):
    """The Reference's legacy Tex interface (old_tex_mobject.py at the
    pin): joins `tex_strings` with `arg_separator` and typesets in math
    mode over the same fmd-math engine."""

    _native_group_single_part = True

    def __init__(
        self,
        *tex_strings,
        arg_separator="",
        isolate=None,
        tex_to_color_map=None,
        **kwargs,
    ):
        self._tex_arg_separator = str(arg_separator)
        super().__init__(
            *tex_strings,
            isolate=isolate,
            tex_to_color_map=tex_to_color_map,
            **kwargs,
        )


class OldTexText(OldTex):
    _native_text_mode = True

    def __init__(self, *tex_strings, math_mode=False, arg_separator="", **kwargs):
        # The Reference's math_mode=True flips back to Tex semantics —
        # per instance, never on the class.
        self._native_text_mode = not math_mode
        super().__init__(*tex_strings, arg_separator=arg_separator, **kwargs)


class DecimalNumber(VMobject):
    """The Reference's DecimalNumber over the de-TeX'd native numbers
    shelf. `set_value` uses the rebuild pattern: the split-family proxy
    representation must re-split its glyph children anyway, which IS a
    rebuild — the native shelf's live glyph-recycling `set_value` is the
    upgrade path once live-state cores land (fm-p107)."""

    # Class-level defaults for the fm-5wq.4.80 complex-mode flags, so a
    # construction path that bypasses __init__ (reclassed native shells,
    # e.g. Matrix's decorated entries) can never hit AttributeError in
    # _reduced_build_value — a real display in real mode is the default.
    hide_zero_components_on_complex = True
    _complex_imag_mode = False

    def __init__(
        self,
        number=0,
        color=None,
        stroke_width=0,
        fill_opacity=1.0,
        fill_border_width=0.5,
        num_decimal_places=2,
        min_total_width=0,
        include_sign=False,
        group_with_commas=True,
        digit_buff_per_font_unit=0.001,
        show_ellipsis=False,
        unit=None,
        include_background_rectangle=False,
        hide_zero_components_on_complex=True,
        edge_to_fix=_LEFT,
        font_size=48,
        text_config=None,
        **kwargs,
    ):
        _refuse_unrouted(
            "DecimalNumber()", [("text_config", bool(text_config))]
        )
        _install_live_state(self)
        # fm-5wq.4.80: the Reference's hide_zero_components_on_complex
        # reductions are pure component selection, so they ride the native
        # f64 formatter exactly — a zero-imag complex is the real path, a
        # zero-real complex is the imaginary component with the "i" unit
        # glyph. The general complex formatter is BN-08's deliberate
        # native exclusion and refuses by that name in
        # _reduced_build_value.
        self.hide_zero_components_on_complex = bool(
            hide_zero_components_on_complex
        )
        self._complex_imag_mode = (
            isinstance(number, complex)
            and self.hide_zero_components_on_complex
            and number.real == 0
            and number.imag != 0
        )
        if self._complex_imag_mode:
            if unit is not None:
                raise NotImplementedError(
                    "DecimalNumber imaginary values with a unit await the "
                    "native complex formatter (BN-08 excludes it)"
                )
            unit = "i"
        build_value = self._reduced_build_value(number)
        self.number = number if isinstance(number, complex) else build_value
        self.font_size = float(font_size)
        self.edge_to_fix = _np.array(_vec3(edge_to_fix))
        self._decimal_params = (
            int(num_decimal_places),
            0 if min_total_width is None else int(min_total_width),
            bool(include_sign),
            bool(group_with_commas),
            float(digit_buff_per_font_unit),
            bool(show_ellipsis),
            None if unit is None else str(unit),
            bool(include_background_rectangle),
            _vec3(edge_to_fix),
            float(font_size),
            color,
            float(stroke_width),
            float(fill_opacity),
            float(fill_border_width),
        )
        specs = self._build_decimal_number(
            _native_shell_factory, build_value, *self._decimal_params
        )
        _hang_native_children(self, specs)
        _apply_vmobject_style_kwargs(self, kwargs)

    def _reduced_build_value(self, value):
        """Map a value onto the native f64 formatter (fm-5wq.4.80). The
        hide-zero complex reductions are exact; everything else refuses by
        the name of what is actually missing or wrong."""
        if isinstance(value, complex):
            hide = self.hide_zero_components_on_complex
            if hide and value.imag == 0:
                if self._complex_imag_mode:
                    raise NotImplementedError(
                        "DecimalNumber cannot switch an imaginary display "
                        "to a real value; the native complex formatter is "
                        "deliberately absent (BN-08)"
                    )
                reduced = float(value.real)
            elif hide and value.real == 0:
                if not self._complex_imag_mode:
                    raise NotImplementedError(
                        "DecimalNumber cannot switch a real display to an "
                        "imaginary value; the native complex formatter is "
                        "deliberately absent (BN-08)"
                    )
                reduced = float(value.imag)
            else:
                raise NotImplementedError(
                    "DecimalNumber over a general complex value awaits "
                    "the native complex formatter; fmn-library's numbers "
                    "shelf deliberately formats f64 only (BN-08)"
                )
        else:
            try:
                reduced = float(value)
            except (TypeError, ValueError) as error:
                raise TypeError(
                    "DecimalNumber requires a real or complex number; "
                    "got " + type(value).__name__
                ) from error
            if self._complex_imag_mode:
                raise NotImplementedError(
                    "DecimalNumber cannot switch an imaginary display to "
                    "a real value; the native complex formatter is "
                    "deliberately absent (BN-08)"
                )
        if not _math.isfinite(reduced):
            raise ValueError(
                "DecimalNumber requires a finite value; got " + repr(value)
            )
        return reduced

    def get_value(self):
        return self.number

    def set_value(self, number):
        # Reference set_value (numbers.py:207): style from the first
        # pointful member, fresh glyphs, re-seat the fixed edge. Works
        # LIVE in both proxy states: the fresh glyph shells build in a
        # scratch nursery and set_submobjects adopts them — the same
        # adoption-on-attach seam Scene-bound families already use — so a
        # bound number mutates in place, digit-count changes included.
        original = number
        number = self._reduced_build_value(number)
        move_to_point = self.get_edge_center(self.edge_to_fix)
        donor = next(
            (sm for sm in self.submobjects if sm.has_points()), None
        )
        style = donor.get_style() if donor is not None else None
        params = list(self._decimal_params)
        params[9] = float(self.font_size)  # scale side effects track here
        scratch = VMobject.__new__(VMobject)
        _install_live_state(scratch)
        specs = scratch._build_decimal_number(
            _native_shell_factory, number, *params
        )
        _hang_native_children(scratch, specs)
        if scratch.n_records() != 0:
            # fm-5wq.4.92: root-level records (the background rectangle's
            # geometry lives on the root) cannot ride set_submobjects, but
            # the live-state become seam replaces them: align_family pads
            # the two families, then per-member data assignment rewrites
            # the root's own records and every glyph child in place, in
            # both proxy states — the fm-p107 upgrade this branch used to
            # await.
            self.become(scratch)
        else:
            self.set_submobjects(list(scratch.submobjects))
        self.move_to(move_to_point, self.edge_to_fix)
        if style is not None:
            # fm-5wq.4.92: the donor style is a GLYPH's; restyle the glyph
            # children only, so the root's background-rectangle records
            # keep their camera-background paint. A rect-less number is
            # unchanged (its root carries no records to spare).
            for child in self.submobjects:
                child.set_style(**style)
        self.number = original if isinstance(original, complex) else number
        return self

    def _handle_scale_side_effects(self, scale_factor):
        self.font_size *= scale_factor
        return self

    def increment_value(self, delta_t=1):
        self.set_value(self.get_value() + delta_t)
        return self


class Integer(DecimalNumber):
    def __init__(self, number=0, num_decimal_places=0, **kwargs):
        inherited = {
            "color",
            "stroke_width",
            "fill_opacity",
            "fill_border_width",
            "min_total_width",
            "include_sign",
            "group_with_commas",
            "digit_buff_per_font_unit",
            "show_ellipsis",
            "unit",
            "include_background_rectangle",
            "hide_zero_components_on_complex",
            "edge_to_fix",
            "font_size",
            "text_config",
            *_NATIVE_VMOBJECT_STYLE_KEYS,
        }
        _refuse_unrouted(
            "Integer()",
            [(name, True) for name in sorted(set(kwargs) - inherited)],
        )
        super().__init__(number, num_decimal_places=num_decimal_places, **kwargs)

    def get_value(self):
        return int(_np.round(super().get_value()))


class PMobject(Mobject):
    """The point-cloud base over Marionette's live RecordBuffer."""

    def set_points(self, points):
        if len(points) == 0:
            points = _np.zeros((0, 3))
        Mobject.set_points(self, points)
        self.resize_points(len(points))
        return self

    def add_points(
        self,
        points,
        rgbas=None,
        color=None,
        opacity=None,
    ):
        points = _np.asarray(points, dtype=float).reshape((-1, 3))
        self.append_points(points)
        data = self.data
        if color is not None:
            if opacity is None:
                opacity = float(data["rgba"][-1, 3])
            rgbas = _np.repeat(
                [_color_to_rgba(color, opacity)], len(points), axis=0
            )
        if rgbas is not None:
            rgbas = _np.asarray(rgbas, dtype=float).reshape((-1, 4))
            data["rgba"][-len(rgbas):] = rgbas
        return self

    def add_point(self, point, rgba=None, color=None, opacity=None):
        rgbas = None if rgba is None else [rgba]
        return self.add_points([point], rgbas, color, opacity)

    def point_from_proportion(self, alpha):
        points = self.get_points()
        return points[int(float(alpha) * (len(points) - 1))]


class PGroup(PMobject):
    """The Reference's variadic PMobject-only family container."""

    def __init__(self, *pmobs, **kwargs):
        if not all(isinstance(mob, PMobject) for mob in pmobs):
            raise Exception("All submobjects must be of type PMobject")
        _install_live_state(self)
        for key, value in kwargs.items():
            setattr(self, key, value)
        specs = self._build_p_group(_native_shell_factory)
        _hang_native_children(self, specs)
        self.add(*pmobs)


class DotCloud(PMobject):
    """The Reference's DotCloud over the pointcloud shelf: the native
    point/radius/rgba/glow_factor record schema (NOT a VMobject — the
    VMobject stroke/fill surface does not exist here; Mobject-level
    set_color/set_opacity write the real rgba records)."""

    def __init__(
        self,
        points=None,
        color=None,
        opacity=1.0,
        radius=0.05,
        glow_factor=0.0,
        anti_alias_width=2.0,
        **kwargs,
    ):
        _install_live_state(self)
        for key, val in kwargs.items():
            setattr(self, key, val)
        self.radius = float(radius)
        self.glow_factor = float(glow_factor)
        specs = self._build_dot_cloud(
            _native_shell_factory,
            [] if points is None else [_vec3(p) for p in points],
            None if color is None else color,
            float(opacity),
            float(radius),
            float(glow_factor),
            float(anti_alias_width),
        )
        _hang_native_children(self, specs)

    def get_radius(self):
        return self.radius

    def compute_bounding_box(self):
        # Point-cloud radii are geometry, not merely a render uniform.  Keep
        # the Reference's class-specific extent rule on top of Marionette's
        # point-family box so inherited placement helpers see the full dots.
        bb = super().compute_bounding_box()
        radius = self.get_radius()
        bb[0] += _np.full((3,), -radius)
        bb[2] += _np.full((3,), radius)
        return bb

    def get_glow_factor(self):
        return self.glow_factor

    def make_3d(self, reflectiveness=0.5, gloss=0.1, shadow=0.2):
        # Reference dot_cloud.py:149 — uniforms-only, both engine-real.
        self.set_shading(reflectiveness, gloss, shadow)
        self.apply_depth_test()
        return self


class TrueDot(DotCloud):
    def __init__(self, center=_ORIGIN, **kwargs):
        super().__init__([center], **kwargs)


class GlowDots(DotCloud):
    def __init__(
        self,
        points=None,
        color="#FFFF00",
        radius=0.2,
        glow_factor=2.0,
        **kwargs,
    ):
        super().__init__(
            points, color=color, radius=radius, glow_factor=glow_factor, **kwargs
        )


class GlowDot(GlowDots):
    def __init__(self, center=_ORIGIN, **kwargs):
        super().__init__([center], **kwargs)


def _vector_field_sample_coords(coordinate_system, density):
    density = float(density)
    if not _math.isfinite(density) or density <= 0:
        raise ValueError("VectorField density must be positive and finite")
    axes = []
    total = 1
    for start, stop, step in coordinate_system.get_all_ranges():
        sample_step = float(step) / density
        if not _math.isfinite(sample_step) or sample_step <= 0:
            raise ValueError("VectorField axis steps must be positive and finite")
        span = (float(stop) + sample_step - float(start)) / sample_step
        count_bound = max(0, _math.ceil(span))
        total *= count_bound
        if total > 65_536:
            raise ValueError(
                "VectorField sample grid exceeds the 65536-point resource budget"
            )
        axes.append(_np.arange(float(start), float(stop) + sample_step, sample_step))
    mesh = _np.meshgrid(*axes, indexing="ij")
    coords = _np.stack(mesh, axis=-1).reshape((-1, len(axes)))
    return coords


def _vector_field_rows(values, context):
    rows = _np.asarray(values, dtype=float)
    if rows.ndim != 2 or not 1 <= rows.shape[1] <= 3:
        raise ValueError(
            f"{context} must be a two-dimensional array with one to three columns"
        )
    return rows


def _vector_field_c2p_rows(coordinate_system, rows):
    rows = _vector_field_rows(rows, "VectorField coordinates")
    return _np.asarray(
        coordinate_system.c2p(*rows.T),
        dtype=float,
    ).reshape((-1, 3))


def _vector_field_default_color_map(values):
    values = _np.asarray(values, dtype=float).reshape(-1)
    rgb = _np.asarray(
        _BridgeMobject._vector_field_gradient(0.0, 1.0, values.tolist()),
        dtype=float,
    )
    return _np.column_stack([rgb, _np.ones(len(rgb))])


class VectorField(VMobject):
    """Reference VectorField with Atlas-owned arrow geometry.

    Portal callbacks run only in Python's released updater window. The
    evaluated coordinate/output rows cross once into Rust, where the native
    field kernel owns tanh length mapping, tip geometry, and stroke records.
    """

    def __init__(
        self,
        func,
        coordinate_system,
        sample_coords=None,
        density=2.0,
        magnitude_range=None,
        color=None,
        color_map_name="3b1b_colormap",
        color_map=None,
        stroke_opacity=1.0,
        stroke_width=3.0,
        tip_width_ratio=4.0,
        tip_len_to_width=0.01,
        max_vect_len=None,
        max_vect_len_to_step_size=0.8,
        flat_stroke=False,
        norm_to_opacity_func=None,
        **kwargs,
    ):
        if color is None and color_map is None and color_map_name not in (
            None,
            "3b1b_colormap",
        ):
            raise NotImplementedError(
                "VectorField color_map_name requires a non-bundled matplotlib "
                f"map: {color_map_name!r}; pass color=, color_map=, or "
                "color_map_name=None"
            )
        style_kwargs = dict(kwargs)
        _preflight_vmobject_style_kwargs(style_kwargs)
        _install_live_state(self)
        self.func = func
        self.coordinate_system = coordinate_system
        self.sample_coords = (
            _vector_field_sample_coords(coordinate_system, density)
            if sample_coords is None
            else _vector_field_rows(sample_coords, "VectorField sample_coords")
        )
        self.stroke_width = float(stroke_width)
        self.stroke_opacity = float(stroke_opacity)
        self.tip_width_ratio = float(tip_width_ratio)
        self.tip_len_to_width = float(tip_len_to_width)
        self.flat_stroke = bool(flat_stroke)
        self.color = color
        self.norm_to_opacity_func = norm_to_opacity_func
        self._native_default_color_map = (
            color is None
            and color_map is None
            and color_map_name == "3b1b_colormap"
        )
        if color is not None:
            self.color_map = None
        elif color_map is not None:
            if not callable(color_map):
                raise TypeError("VectorField color_map must be callable")
            self.color_map = color_map
        elif color_map_name == "3b1b_colormap":
            self.color_map = _vector_field_default_color_map
        else:
            self.color_map = None
        self.update_sample_points()
        if len(self.sample_points) < 2:
            raise ValueError("VectorField needs at least two sample points")
        if max_vect_len is None:
            self.max_displayed_vect_len = float(max_vect_len_to_step_size) * float(
                _np.linalg.norm(self.sample_points[1] - self.sample_points[0])
            )
        else:
            dimension = int(getattr(coordinate_system, "dimension", 2))
            unit = _np.zeros((1, 3))
            unit[0, 0] = 1.0
            origin = _np.asarray(
                coordinate_system.c2p(*([0.0] * dimension)), dtype=float
            )
            unit_size = _np.linalg.norm(
                _vector_field_c2p_rows(coordinate_system, unit)[0] - origin
            )
            self.max_displayed_vect_len = float(max_vect_len) * float(unit_size)
        outputs = self._evaluate_outputs()
        self.magnitude_range = (
            (0.0, float(_np.linalg.norm(outputs, axis=1).max(initial=0.0)))
            if magnitude_range is None
            else (float(magnitude_range[0]), float(magnitude_range[1]))
        )
        self.init_base_stroke_width_array(len(self.sample_coords))
        specs = self._build_geometry(_native_shell_factory, outputs)
        _hang_native_children(self, specs)
        _apply_vmobject_style_kwargs(self, style_kwargs)
        self.set_stroke(
            color=color,
            width=stroke_width,
            opacity=stroke_opacity,
            flat=flat_stroke,
        )
        self._apply_callback_style(outputs)

    def _evaluate_outputs(self):
        outputs = _vector_field_rows(
            self.func(self.sample_coords), "VectorField callback output"
        )
        if len(outputs) != len(self.sample_coords):
            raise ValueError(
                "VectorField callback must return one vector per sample; "
                f"got {len(outputs)} vectors for {len(self.sample_coords)} samples"
            )
        return outputs

    def _geometry_inputs(self, outputs):
        output_norms = _np.linalg.norm(outputs, axis=1)
        dimension = int(getattr(self.coordinate_system, "dimension", 2))
        origin = _np.asarray(
            self.coordinate_system.c2p(*([0.0] * dimension)), dtype=float
        )
        out_vects = _vector_field_c2p_rows(self.coordinate_system, outputs) - origin
        return out_vects, output_norms

    def _build_geometry(self, factory, outputs, target=None):
        out_vects, output_norms = self._geometry_inputs(outputs)
        if target is None:
            target = self
        return target._build_vector_field_samples(
            factory,
            [tuple(row) for row in self.sample_points],
            [tuple(row) for row in out_vects],
            [float(value) for value in output_norms],
            self.max_displayed_vect_len,
            self.stroke_width,
            self.stroke_opacity,
            self.tip_width_ratio,
            self.tip_len_to_width,
            self.flat_stroke,
            self._native_default_color_map,
            self.color,
            self.magnitude_range,
        )

    def _apply_callback_style(self, outputs):
        output_norms = _np.linalg.norm(outputs, axis=1)
        repeated_norms = _np.repeat(output_norms, 8)[:-1]
        if self.color_map is not None:
            low, high = self.magnitude_range
            alphas = _np.true_divide(repeated_norms - low, high - low)
            rgba = _np.asarray(self.color_map(alphas), dtype=float)
            if rgba.ndim != 2 or rgba.shape[0] != len(repeated_norms) or rgba.shape[1] < 3:
                raise ValueError(
                    "VectorField color_map must return one RGB or RGBA row per point"
                )
            self.data["stroke_rgba"][:, :3] = rgba[:, :3]
        if self.norm_to_opacity_func is not None:
            self.get_stroke_opacities()[:] = self.norm_to_opacity_func(
                repeated_norms
            )

    def init_points(self):
        n_samples = len(self.sample_coords)
        self.set_points(_np.zeros((8 * n_samples - 1, 3)))
        self.set_joint_type("no_joint")

    def get_sample_points(
        self,
        center,
        width,
        height,
        depth,
        x_density,
        y_density,
        z_density,
    ):
        return _np.asarray(
            self._vector_field_grid_sample_points(
                _vec3(center),
                float(width),
                float(height),
                float(depth),
                float(x_density),
                float(y_density),
                float(z_density),
            ),
            dtype=float,
        )

    def init_base_stroke_width_array(self, n_sample_points):
        arr = _np.ones(8 * _operator.index(n_sample_points) - 1)
        arr[4::8] = self.tip_width_ratio
        arr[5::8] = self.tip_width_ratio * 0.5
        arr[6::8] = 0
        arr[7::8] = 0
        self.base_stroke_width_array = arr

    def set_stroke(
        self,
        color=None,
        width=None,
        opacity=None,
        behind=None,
        flat=None,
        recurse=True,
    ):
        VMobject.set_stroke(
            self,
            color,
            None,
            opacity,
            behind,
            flat,
            recurse,
        )
        if width is not None:
            self.set_stroke_width(float(width))
        return self

    def set_stroke_width(self, width):
        if self.get_num_points() > 0:
            self.get_stroke_widths()[:] = (
                float(width) * self.base_stroke_width_array
            )
            self.stroke_width = float(width)
        return self

    def update_sample_points(self):
        self.sample_points = _vector_field_c2p_rows(
            self.coordinate_system, self.sample_coords
        )

    def set_sample_coords(self, sample_coords):
        self.sample_coords = _vector_field_rows(
            sample_coords, "VectorField sample_coords"
        )
        return self

    def update_vectors(self):
        outputs = self._evaluate_outputs()
        scratch = VMobject.__new__(VMobject)
        _install_live_state(scratch)
        specs = self._build_geometry(
            _native_shell_factory, outputs, target=scratch
        )
        # The field kernel is a single VMobject root; keep an explicit guard
        # so a future native family expansion cannot be silently discarded.
        if specs:
            raise RuntimeError("native VectorField unexpectedly returned children")
        self.match_points(scratch)
        self.data["stroke_width"][:] = scratch.data["stroke_width"]
        self._apply_callback_style(outputs)
        return self


class TimeVaryingVectorField(VectorField):
    def __init__(self, time_func, coordinate_system, **kwargs):
        self.time = 0.0
        self.time_func = time_func
        super().__init__(
            lambda coords: self.time_func(coords, self.time),
            coordinate_system,
            **kwargs,
        )
        self.add_updater(lambda m, dt: m.increment_time(dt))
        self.always.update_vectors()

    def increment_time(self, dt):
        self.time += float(dt)


class StreamLines(VGroup):
    """Reference StreamLines over the ONE native RK45 integrator
    (fm-5wq.4.74): the Python field callback and coordinate system are
    consulted at construction only, seed jitter comes from the named
    STREAM_LINES_SUBSTREAM, and each child is one integrated flow line
    re-spaced by true arc length (BN-03)."""

    def __init__(
        self,
        func,
        coordinate_system,
        density=1.0,
        n_repeats=1,
        noise_factor=None,
        solution_time=3,
        dt=0.05,
        arc_len=3,
        max_time_steps=200,
        n_samples_per_line=10,
        cutoff_norm=15,
        stroke_width=1.0,
        stroke_color=None,
        stroke_opacity=1,
        color_by_magnitude=True,
        magnitude_range=(0, 2.0),
        taper_stroke_width=False,
        color_map="3b1b_colormap",
        **kwargs,
    ):
        if not callable(func):
            raise TypeError(
                "StreamLines func must be a callable vector field; got "
                + type(func).__name__
            )
        if not (
            hasattr(coordinate_system, "c2p")
            and hasattr(coordinate_system, "p2c")
            and hasattr(coordinate_system, "get_all_ranges")
        ):
            raise TypeError(
                "StreamLines requires a coordinate system with "
                "c2p/p2c/get_all_ranges; got "
                + type(coordinate_system).__name__
            )
        if color_map not in (None, "3b1b_colormap"):
            raise NotImplementedError(
                "StreamLines color_map requires a non-bundled matplotlib "
                f"map: {color_map!r}; use the bundled 3b1b_colormap"
            )
        style_kwargs = dict(kwargs)
        _preflight_vmobject_style_kwargs(style_kwargs)
        _install_live_state(self)
        self.func = func
        self.coordinate_system = coordinate_system
        self.density = float(density)
        self.solution_time = float(solution_time)

        def _field_rows(rows):
            outputs = _vector_field_rows(
                func(_np.asarray(rows, dtype=float)),
                "StreamLines func output",
            )
            return [tuple(float(value) for value in row) for row in outputs]

        def _adapter_c2p(coords):
            point = _np.asarray(
                coordinate_system.c2p(*coords), dtype=float
            ).reshape(3)
            return tuple(float(value) for value in point)

        def _adapter_p2c(point):
            coords = _np.asarray(
                coordinate_system.p2c(_np.asarray(point, dtype=float)),
                dtype=float,
            ).reshape(-1)
            padded = [0.0, 0.0, 0.0]
            for index in range(min(3, len(coords))):
                padded[index] = float(coords[index])
            return tuple(padded)

        # A coordinate system may spell a range as (min, max) — NumberPlane
        # keeps the caller's 2-vectors verbatim — while the native stream
        # builder wants the full (min, max, step) row. The pad is the
        # axis's default unit STEP (the NumberLine default), never a fake
        # z-axis; a malformed shorter row stays the native builder's own
        # named refusal.
        ranges = []
        for row in coordinate_system.get_all_ranges():
            values = [float(value) for value in row[:3]]
            if len(values) == 2:
                values.append(1.0)
            ranges.append(tuple(values))
        # Detached construction rides the standard portal scene seed (0);
        # certified seed plumbing follows the scene-owned construction seam.
        specs, virtual_times, rng_draws = self._build_stream_lines(
            _native_shell_factory,
            _field_rows,
            _adapter_c2p,
            _adapter_p2c,
            ranges,
            int(getattr(coordinate_system, "dimension", 2)),
            0,
            float(density),
            int(n_repeats),
            None if noise_factor is None else float(noise_factor),
            float(solution_time),
            float(dt),
            float(arc_len),
            int(max_time_steps),
            int(n_samples_per_line),
            float(cutoff_norm),
            float(stroke_width),
            stroke_color,
            float(stroke_opacity),
            bool(color_by_magnitude),
            (float(magnitude_range[0]), float(magnitude_range[1])),
            bool(taper_stroke_width),
        )
        _hang_native_children(self, specs)
        self._stream_virtual_times = [float(t) for t in virtual_times]
        self._stream_rng_draws = int(rng_draws)
        _apply_vmobject_style_kwargs(self, style_kwargs)


class AnimatedStreamLines(VGroup):
    """vector_field.py:445 as a Python dt-updater over the native lines:
    the per-line gaussian VShowPassingFlash sweep (σ = time_width/6, swept
    −tw/2 → 1+tw/2, zeroed outside 3σ) modulates each line's end-tapered
    base stroke profile, paced by virtual_time/rate_multiple and lagged by
    the named substream's continuous U[0,1) draws — the exact formula the
    native stage updater applies."""

    _FLASH_TAPER_WIDTH = 0.05

    def __init__(
        self,
        stream_lines,
        lag_range=4,
        rate_multiple=1.0,
        line_anim_config=None,
        **kwargs,
    ):
        if not isinstance(stream_lines, StreamLines):
            raise TypeError(
                "AnimatedStreamLines requires a StreamLines instance; got "
                + type(stream_lines).__name__
            )
        config = dict(line_anim_config or {})
        time_width = float(config.pop("time_width", 1.0))
        rate_func = config.pop("rate_func", None)
        _refuse_unrouted(
            "AnimatedStreamLines()",
            [(name, True) for name in sorted(config)]
            + [
                (
                    "rate_func",
                    rate_func is not None
                    and getattr(rate_func, "__name__", "") != "linear",
                )
            ],
        )
        super().__init__(stream_lines)
        _preflight_vmobject_style_kwargs(dict(kwargs))
        self.stream_lines = stream_lines
        self.lag_range = float(lag_range)
        self.rate_multiple = float(rate_multiple)
        self.time_width = time_width
        lines = list(stream_lines.submobjects)
        uniforms = _BridgeMobject._stream_line_lag_uniforms(
            0, stream_lines._stream_rng_draws, len(lines)
        )
        self._line_times = [-self.lag_range * value for value in uniforms]
        self._line_run_times = [
            virtual_time / self.rate_multiple
            for virtual_time in stream_lines._stream_virtual_times
        ]
        self._line_xs = []
        self._line_base_profiles = []
        taper = self._FLASH_TAPER_WIDTH

        def taper_kernel(x):
            if x < taper:
                return x
            if x > 1.0 - taper:
                return 1.0 - x
            return 1.0

        for line in lines:
            widths = _np.asarray(line.data["stroke_width"], dtype=float).reshape(-1)
            count = len(widths)
            xs = (
                _np.arange(count, dtype=float) / (count - 1)
                if count > 1
                else _np.zeros(count)
            )
            kernel = _np.asarray([taper_kernel(x) for x in xs])
            self._line_xs.append(xs)
            self._line_base_profiles.append(widths * kernel)
        self.add_updater(lambda mob, dt: mob.update(dt))

    def update(self, dt=0):
        sigma = self.time_width / 6.0
        for index, line in enumerate(self.stream_lines.submobjects):
            self._line_times[index] += float(dt)
            run_time = self._line_run_times[index]
            if not _math.isfinite(run_time) or run_time <= 0.0:
                continue
            # Reference vector_field.py: `alpha = time_ratio % 1` on the
            # SIGNED time — Python's modulo wraps a negative lag into
            # [0, 1), so a lagged line is a phase offset already mid-cycle,
            # not a line frozen at alpha 0. The old max(time, 0.0) clamp
            # froze every still-lagged line, which also made a wait tick a
            # visible no-op after the construction-time updater pass
            # (fm-5wq.4.74, bridge.py:11655).
            alpha = (self._line_times[index] / run_time) % 1.0
            mu = (1.0 - alpha) * (-self.time_width / 2.0) + alpha * (
                1.0 + self.time_width / 2.0
            )
            base = self._line_base_profiles[index]
            xs = self._line_xs[index]
            if sigma <= 0.0:
                widths = _np.zeros_like(base)
            else:
                z = (xs - mu) / sigma
                widths = _np.where(
                    _np.abs(xs - mu) > 3.0 * sigma,
                    0.0,
                    base * _np.exp(-0.5 * z * z),
                )
            column = line.data["stroke_width"]
            column[:] = widths.reshape(column.shape)
        return self


class MotionMobject(Mobject):
    """A draggable wrapper whose composition comes from Atlas's native
    ``MotionMobject`` builder.

    The separately owned event gateway has not landed, so schema installation
    deliberately retains the precise ``mob_on_mouse_drag`` capability refusal.
    Constructor identity, grouping, and the Reference's no-op updater are real.
    """

    def __init__(self, mobject, **kwargs):
        assert isinstance(mobject, Mobject)
        _install_live_state(self)
        for key, value in kwargs.items():
            setattr(self, key, value)
        specs = self._build_motion_mobject(_native_shell_factory, mobject)
        _hang_native_children(self, specs)
        self.mobject = mobject
        self.mobject.add_updater(lambda mob: None)
        self.add(mobject)


class Button(Mobject):
    """Reference-compatible clickable composition over Atlas's native
    arbitrary-mobject button builder.

    Event dispatch remains separately owned, so schema installation keeps the
    precise ``mob_on_mouse_press`` capability refusal while construction,
    identity, and callback storage are real.
    """

    def __init__(self, mobject, on_click, **kwargs):
        assert isinstance(mobject, Mobject)
        _install_live_state(self)
        for key, value in kwargs.items():
            setattr(self, key, value)
        specs = self._build_button(_native_shell_factory, mobject)
        _hang_native_children(self, specs)
        self.on_click = on_click
        self.mobject = mobject
        self.add(mobject)


class Surface(Mobject):
    """The Surface-family MRO anchor (Reference Surface(Mobject), NOT a
    VMobject — VGroup's only-VMobjects refusal stays correct for it).
    Concrete construction routes through the solid subclasses; the
    remaining Surface surface stays precise schema placeholders."""

    def pointwise_become_partial(self, smobject, a, b, axis=None):
        assert isinstance(smobject, Surface)
        if axis is None:
            axis = self.preferred_creation_axis
        self._surface_pointwise_become_partial(
            smobject,
            tuple(smobject.resolution),
            _operator.index(axis),
            float(a),
            float(b),
        )
        return self

    def _apply_surface_style(self, color, opacity, shading, depth_test):
        # Reference Surface defaults land in the native builders; explicit
        # values reapply through the state-real surfaces.
        if color is not None or opacity is not None:
            self.set_color(color, opacity)
        if shading is not None:
            self.set_shading(*shading)
        if depth_test:
            self.apply_depth_test()
        else:
            self.deactivate_depth_test()
        return self


class SGroup(Surface):
    # Reference SGroup(*parametric_surfaces): the group init is inherited
    # (Mobject's ingest rule); only-Surface membership is the schema's
    # concern once real Surface parents exist.
    pass


class TexturedSurface(Surface):
    """Light/dark textured UV surface. Marionette cannot yet retain both
    textures without collapsing the grid into an ImageQuad."""

    shader_folder = "textured_surface"
    data_dtype = [
        ("point", _np.float32, (3,)),
        ("d_normal_point", _np.float32, (3,)),
        ("im_coords", _np.float32, (2,)),
        ("opacity", _np.float32, (1,)),
    ]

    def __init__(self, uv_surface, image_file, dark_image_file=None, **kwargs):
        if kwargs:
            raise TypeError(
                "unexpected keyword arguments: " + ", ".join(sorted(kwargs))
            )
        if not isinstance(uv_surface, Surface):
            raise TypeError("TexturedSurface uv_surface must be a Surface")
        del image_file, dark_image_file
        raise _CapabilityError(
            "TexturedSurface is unavailable until Marionette can retain a "
            "light/dark texture pair without changing the surface grid into "
            "an ImageQuad"
        )


class TexturedGeometry(TexturedSurface):
    """Trimesh-backed textured surface. Same Marionette texture-pair gap."""

    def __init__(self, geometry, texture_file, **kwargs):
        if kwargs:
            raise TypeError(
                "unexpected keyword arguments: " + ", ".join(sorted(kwargs))
            )
        del geometry, texture_file
        raise _CapabilityError(
            "TexturedGeometry is unavailable until Marionette can retain a "
            "light/dark texture pair without changing the surface grid into "
            "an ImageQuad"
        )


def _native_surface_shell_factory():
    # Surface-family children are NOT VMobjects: their shells must carry
    # the Mobject-level color surface (rgba records), never stroke/fill.
    shell = Surface.__new__(Surface)
    _install_live_state(shell)
    return shell


class ThreeDModel(Group):
    """Atlas's owned OBJ-subset reader under the Reference Group surface."""

    def __init__(self, obj_file: str, height=3):
        super().__init__()
        path = _pathlib.Path(obj_file)
        payload = path.read_bytes()
        specs = self._build_three_d_model(
            _native_surface_shell_factory,
            payload,
            float(height),
        )
        _hang_native_children(self, specs)
        self.obj_file = str(path)
        self.height = float(height)

    def copy(self, deep=False):
        return _copy_mobject_graph(
            self,
            bool(deep),
            {},
            detach_bound=True,
        )


# The module postpones annotations globally, while the pinned Reference
# signature exposes the concrete builtin rather than the string ``'str'``.
ThreeDModel.__init__.__annotations__["obj_file"] = str


class ParametricSurface(Surface):
    def __init__(self, uv_func, u_range=(0, 1), v_range=(0, 1), **kwargs):
        color = kwargs.pop("color", None)
        opacity = kwargs.pop("opacity", None)
        shading = kwargs.pop("shading", None)
        depth_test = kwargs.pop("depth_test", True)
        resolution = kwargs.pop("resolution", (101, 101))
        preferred_creation_axis = kwargs.pop("preferred_creation_axis", 1)
        epsilon = kwargs.pop("epsilon", 0.001)
        normal_nudge = kwargs.pop("normal_nudge", 0.001)
        _refuse_unrouted(
            "ParametricSurface()", [(name, True) for name in sorted(kwargs)]
        )
        _install_live_state(self)
        self.u_range = tuple(u_range)
        self.v_range = tuple(v_range)
        self.resolution = (int(resolution[0]), int(resolution[1]))
        self.preferred_creation_axis = int(preferred_creation_axis)
        self.epsilon = float(epsilon)
        self.normal_nudge = float(normal_nudge)
        specs = self._build_parametric_surface(
            _native_surface_shell_factory,
            uv_func,
            (float(u_range[0]), float(u_range[1])),
            (float(v_range[0]), float(v_range[1])),
            self.resolution,
            self.epsilon,
            self.normal_nudge,
        )
        _hang_native_children(self, specs)
        self._apply_surface_style(color, opacity, shading, depth_test)


class Sphere(Surface):
    def __init__(
        self,
        u_range=(0, _math.tau),
        v_range=(0, _math.pi),
        resolution=(101, 51),
        radius=1.0,
        true_normals=True,
        clockwise=False,
        **kwargs,
    ):
        color = kwargs.pop("color", None)
        opacity = kwargs.pop("opacity", None)
        shading = kwargs.pop("shading", None)
        depth_test = kwargs.pop("depth_test", True)
        preferred_creation_axis = kwargs.pop("preferred_creation_axis", 1)
        epsilon = kwargs.pop("epsilon", 0.001)
        normal_nudge = kwargs.pop("normal_nudge", 0.001)
        _refuse_unrouted("Sphere()", [(name, True) for name in sorted(kwargs)])
        _install_live_state(self)
        self.radius = float(radius)
        self.clockwise = bool(clockwise)
        self.true_normals = bool(true_normals)
        self.u_range = tuple(u_range)
        self.v_range = tuple(v_range)
        self.resolution = (int(resolution[0]), int(resolution[1]))
        self.preferred_creation_axis = int(preferred_creation_axis)
        self.epsilon = float(epsilon)
        self.normal_nudge = float(normal_nudge)
        self._solid_params = ("sphere", self.radius)
        specs = self._build_sphere(
            _native_surface_shell_factory,
            self.radius,
            (float(u_range[0]), float(u_range[1])),
            (float(v_range[0]), float(v_range[1])),
            self.resolution,
            self.true_normals,
            self.clockwise,
            self.preferred_creation_axis,
            self.epsilon,
            self.normal_nudge,
        )
        _hang_native_children(self, specs)
        self._apply_surface_style(color, opacity, shading, depth_test)

    def uv_func(self, u, v):
        return _np.array(
            self._sphere_uv(self.radius, bool(self.clockwise), float(u), float(v))
        )


class Torus(Surface):
    def __init__(
        self,
        u_range=(0, _math.tau),
        v_range=(0, _math.tau),
        r1=3.0,
        r2=1.0,
        **kwargs,
    ):
        color = kwargs.pop("color", None)
        opacity = kwargs.pop("opacity", None)
        shading = kwargs.pop("shading", None)
        depth_test = kwargs.pop("depth_test", True)
        resolution = kwargs.pop("resolution", (101, 101))
        preferred_creation_axis = kwargs.pop("preferred_creation_axis", 1)
        epsilon = kwargs.pop("epsilon", 0.001)
        normal_nudge = kwargs.pop("normal_nudge", 0.001)
        z_index = int(kwargs.pop("z_index", 0))
        _refuse_unrouted("Torus()", [(name, True) for name in sorted(kwargs)])
        _install_live_state(self)
        self.r1 = float(r1)
        self.r2 = float(r2)
        self.u_range = tuple(u_range)
        self.v_range = tuple(v_range)
        self.resolution = (int(resolution[0]), int(resolution[1]))
        self.preferred_creation_axis = int(preferred_creation_axis)
        self.epsilon = float(epsilon)
        self.normal_nudge = float(normal_nudge)
        specs = self._build_torus(
            _native_surface_shell_factory,
            self.r1,
            self.r2,
            (float(u_range[0]), float(u_range[1])),
            (float(v_range[0]), float(v_range[1])),
            self.resolution,
            self.preferred_creation_axis,
            self.epsilon,
            self.normal_nudge,
            z_index,
        )
        _hang_native_children(self, specs)
        self._apply_surface_style(color, opacity, shading, depth_test)
        self._solid_params = ("torus", self.r1, self.r2)
        self._solid_native_height = self.get_height()

    def uv_func(self, u, v):
        return _np.array(
            self._torus_uv(self.r1, self.r2, float(u), float(v))
        )


class Cylinder(Surface):
    """The Reference cylinder over Atlas's native sampled solid."""

    def __init__(
        self,
        u_range=(0, _math.tau),
        v_range=(-1, 1),
        resolution=(101, 11),
        height=2,
        radius=1,
        axis=_OUT,
        **kwargs,
    ):
        color = kwargs.pop("color", None)
        opacity = kwargs.pop("opacity", None)
        shading = kwargs.pop("shading", None)
        depth_test = kwargs.pop("depth_test", True)
        preferred_creation_axis = kwargs.pop("preferred_creation_axis", 1)
        epsilon = kwargs.pop("epsilon", 0.001)
        normal_nudge = kwargs.pop("normal_nudge", 0.001)
        z_index = int(kwargs.pop("z_index", 0))
        _refuse_unrouted("Cylinder()", [(name, True) for name in sorted(kwargs)])
        _install_live_state(self)
        self.height = float(height)
        self.radius = float(radius)
        self.axis = _np.array(_vec3(axis), dtype=float)
        self.u_range = tuple(u_range)
        self.v_range = tuple(v_range)
        self.resolution = (int(resolution[0]), int(resolution[1]))
        self.preferred_creation_axis = int(preferred_creation_axis)
        self.epsilon = float(epsilon)
        self.normal_nudge = float(normal_nudge)
        specs = self._build_cylinder(
            _native_surface_shell_factory,
            self.height,
            self.radius,
            _vec3(self.axis),
            (float(u_range[0]), float(u_range[1])),
            (float(v_range[0]), float(v_range[1])),
            self.resolution,
            self.preferred_creation_axis,
            self.epsilon,
            self.normal_nudge,
            z_index,
        )
        _hang_native_children(self, specs)
        self._apply_surface_style(color, opacity, shading, depth_test)
        self._solid_params = (
            "cylinder",
            self.height,
            self.radius,
            tuple(float(component) for component in self.axis),
        )
        self._solid_native_height = self.get_height()

    def uv_func(self, u, v):
        return _np.array(self._cylinder_uv(float(u), float(v)))


class Cone(Cylinder):
    def __init__(
        self,
        u_range=(0, _math.tau),
        v_range=(0, 1),
        *args,
        **kwargs,
    ):
        if args:
            raise TypeError(
                "Cylinder.__init__() got multiple values for argument 'u_range'"
            )
        color = kwargs.pop("color", None)
        opacity = kwargs.pop("opacity", None)
        shading = kwargs.pop("shading", None)
        depth_test = kwargs.pop("depth_test", True)
        resolution = kwargs.pop("resolution", (101, 11))
        height = kwargs.pop("height", 2)
        radius = kwargs.pop("radius", 1)
        axis = kwargs.pop("axis", _OUT)
        preferred_creation_axis = kwargs.pop("preferred_creation_axis", 1)
        epsilon = kwargs.pop("epsilon", 0.001)
        normal_nudge = kwargs.pop("normal_nudge", 0.001)
        z_index = int(kwargs.pop("z_index", 0))
        _refuse_unrouted("Cone()", [(name, True) for name in sorted(kwargs)])
        _install_live_state(self)
        self.height = float(height)
        self.radius = float(radius)
        self.axis = _np.array(_vec3(axis), dtype=float)
        self.u_range = tuple(u_range)
        self.v_range = tuple(v_range)
        self.resolution = (int(resolution[0]), int(resolution[1]))
        self.preferred_creation_axis = int(preferred_creation_axis)
        self.epsilon = float(epsilon)
        self.normal_nudge = float(normal_nudge)
        specs = self._build_cone(
            _native_surface_shell_factory,
            self.height,
            self.radius,
            _vec3(self.axis),
            (float(u_range[0]), float(u_range[1])),
            (float(v_range[0]), float(v_range[1])),
            self.resolution,
            self.preferred_creation_axis,
            self.epsilon,
            self.normal_nudge,
            z_index,
        )
        _hang_native_children(self, specs)
        self._apply_surface_style(color, opacity, shading, depth_test)
        self._solid_params = (
            "cone",
            self.height,
            self.radius,
            tuple(float(component) for component in self.axis),
        )
        self._solid_native_height = self.get_height()

    def uv_func(self, u, v):
        return _np.array(self._cone_uv(float(u), float(v)))


class Line3D(Cylinder):
    def __init__(
        self,
        start,
        end,
        width=0.05,
        resolution=(21, 25),
        **kwargs,
    ):
        color = kwargs.pop("color", None)
        opacity = kwargs.pop("opacity", None)
        shading = kwargs.pop("shading", None)
        depth_test = kwargs.pop("depth_test", True)
        u_range = kwargs.pop("u_range", (0, _math.tau))
        v_range = kwargs.pop("v_range", (-1, 1))
        preferred_creation_axis = kwargs.pop("preferred_creation_axis", 1)
        epsilon = kwargs.pop("epsilon", 0.001)
        normal_nudge = kwargs.pop("normal_nudge", 0.001)
        z_index = int(kwargs.pop("z_index", 0))
        _refuse_unrouted("Line3D()", [(name, True) for name in sorted(kwargs)])
        _install_live_state(self)
        start_point = _np.array(_vec3(start), dtype=float)
        end_point = _np.array(_vec3(end), dtype=float)
        self.height = float(_np.linalg.norm(end_point - start_point))
        self.radius = float(width) / 2.0
        self.axis = end_point - start_point
        self.u_range = tuple(u_range)
        self.v_range = tuple(v_range)
        self.resolution = (int(resolution[0]), int(resolution[1]))
        self.preferred_creation_axis = int(preferred_creation_axis)
        self.epsilon = float(epsilon)
        self.normal_nudge = float(normal_nudge)
        specs = self._build_line3d(
            _native_surface_shell_factory,
            _vec3(start_point),
            _vec3(end_point),
            float(width),
            (float(u_range[0]), float(u_range[1])),
            (float(v_range[0]), float(v_range[1])),
            self.resolution,
            self.preferred_creation_axis,
            self.epsilon,
            self.normal_nudge,
            z_index,
        )
        _hang_native_children(self, specs)
        self._apply_surface_style(color, opacity, shading, depth_test)
        self._solid_params = (
            "cylinder",
            self.height,
            self.radius,
            tuple(float(component) for component in self.axis),
        )
        self._solid_native_height = self.get_height()


class Disk3D(Surface):
    def __init__(
        self,
        radius=1,
        u_range=(0, 1),
        v_range=(0, _math.tau),
        resolution=(2, 100),
        **kwargs,
    ):
        color = kwargs.pop("color", None)
        opacity = kwargs.pop("opacity", None)
        shading = kwargs.pop("shading", None)
        depth_test = kwargs.pop("depth_test", True)
        preferred_creation_axis = kwargs.pop("preferred_creation_axis", 1)
        epsilon = kwargs.pop("epsilon", 0.001)
        normal_nudge = kwargs.pop("normal_nudge", 0.001)
        z_index = int(kwargs.pop("z_index", 0))
        _refuse_unrouted("Disk3D()", [(name, True) for name in sorted(kwargs)])
        _install_live_state(self)
        self.radius = float(radius)
        self.u_range = tuple(u_range)
        self.v_range = tuple(v_range)
        self.resolution = (int(resolution[0]), int(resolution[1]))
        self.preferred_creation_axis = int(preferred_creation_axis)
        self.epsilon = float(epsilon)
        self.normal_nudge = float(normal_nudge)
        specs = self._build_disk3d(
            _native_surface_shell_factory,
            self.radius,
            (float(u_range[0]), float(u_range[1])),
            (float(v_range[0]), float(v_range[1])),
            self.resolution,
            self.preferred_creation_axis,
            self.epsilon,
            self.normal_nudge,
            z_index,
        )
        _hang_native_children(self, specs)
        self._apply_surface_style(color, opacity, shading, depth_test)
        self._solid_params = ("disk", self.radius)
        self._solid_native_height = self.get_height()

    def uv_func(self, u, v):
        return _np.array(self._disk3d_uv(float(u), float(v)))


class Square3D(Surface):
    def __init__(
        self,
        side_length=2.0,
        u_range=(-1, 1),
        v_range=(-1, 1),
        resolution=(2, 2),
        **kwargs,
    ):
        color = kwargs.pop("color", None)
        opacity = kwargs.pop("opacity", None)
        shading = kwargs.pop("shading", None)
        depth_test = kwargs.pop("depth_test", True)
        preferred_creation_axis = kwargs.pop("preferred_creation_axis", 1)
        epsilon = kwargs.pop("epsilon", 0.001)
        normal_nudge = kwargs.pop("normal_nudge", 0.001)
        z_index = int(kwargs.pop("z_index", 0))
        _refuse_unrouted("Square3D()", [(name, True) for name in sorted(kwargs)])
        _install_live_state(self)
        self.side_length = float(side_length)
        self.u_range = tuple(u_range)
        self.v_range = tuple(v_range)
        self.resolution = (int(resolution[0]), int(resolution[1]))
        self.preferred_creation_axis = int(preferred_creation_axis)
        self.epsilon = float(epsilon)
        self.normal_nudge = float(normal_nudge)
        specs = self._build_square3d(
            _native_surface_shell_factory,
            self.side_length,
            (float(u_range[0]), float(u_range[1])),
            (float(v_range[0]), float(v_range[1])),
            self.resolution,
            self.preferred_creation_axis,
            self.epsilon,
            self.normal_nudge,
            z_index,
        )
        _hang_native_children(self, specs)
        self._apply_surface_style(color, opacity, shading, depth_test)
        self._solid_params = ("square", self.side_length)
        self._solid_native_height = self.get_height()

    def uv_func(self, u, v):
        return _np.array(self._square3d_uv(float(u), float(v)))


class Cube(SGroup):
    def __init__(
        self,
        color=None,
        opacity=1,
        shading=(0.1, 0.5, 0.1),
        square_resolution=(2, 2),
        side_length=2,
        **kwargs,
    ):
        depth_test = kwargs.pop("depth_test", True)
        z_index = int(kwargs.pop("z_index", 0))
        _refuse_unrouted(
            type(self).__name__ + "()",
            [(name, True) for name in sorted(kwargs)],
        )
        _install_live_state(self)
        self.resolution = (
            int(square_resolution[0]),
            int(square_resolution[1]),
        )
        self.preferred_creation_axis = 1
        specs = self._build_cube(
            _native_surface_shell_factory,
            float(side_length),
            self.resolution,
            z_index,
        )
        _hang_native_children(self, specs)
        self._apply_surface_style(color, opacity, shading, depth_test)


class Prism(Cube):
    def __init__(self, width=3.0, height=2.0, depth=1.0, **kwargs):
        color = kwargs.pop("color", None)
        opacity = kwargs.pop("opacity", None)
        shading = kwargs.pop("shading", None)
        depth_test = kwargs.pop("depth_test", True)
        z_index = int(kwargs.pop("z_index", 0))
        _refuse_unrouted("Prism()", [(name, True) for name in sorted(kwargs)])
        _install_live_state(self)
        self.resolution = (2, 2)
        self.preferred_creation_axis = 1
        specs = self._build_prism(
            _native_surface_shell_factory,
            float(width),
            float(height),
            float(depth),
            z_index,
        )
        _hang_native_children(self, specs)
        self._apply_surface_style(color, opacity, shading, depth_test)


class VGroup3D(VGroup):
    def __init__(
        self,
        *vmobjects,
        depth_test=True,
        shading=(0.2, 0.2, 0.2),
        joint_type="no_joint",
        **kwargs,
    ):
        if any(not isinstance(vmob, VMobject) for vmob in vmobjects):
            raise Exception("Only VMobjects can be passed into VGroup")
        if joint_type not in VMobject.joint_type_map:
            raise KeyError(joint_type)
        super().__init__(*vmobjects, **kwargs)
        _apply_vgroup3d_config(self, depth_test, shading, joint_type)


class VCube(VGroup3D):
    def __init__(
        self,
        side_length=2.0,
        fill_color=_BLUE_D,
        fill_opacity=1,
        stroke_width=0,
        **kwargs,
    ):
        z_index = int(kwargs.pop("z_index", 0))
        style, depth_test, shading, joint_type = _split_native_vgroup3d_kwargs(
            "VCube()", kwargs, (0.2, 0.2, 0.2)
        )
        _install_live_state(self)
        specs = self._build_vcube(
            _native_shell_factory,
            float(side_length),
            fill_color,
            float(fill_opacity),
            float(stroke_width),
            z_index,
        )
        _hang_native_children(self, specs)
        style.setdefault("fill_color", fill_color)
        style.setdefault("fill_opacity", fill_opacity)
        style.setdefault("stroke_width", stroke_width)
        _apply_vmobject_style_kwargs(self, style)
        _apply_vgroup3d_config(self, depth_test, shading, joint_type)


class VPrism(VCube):
    def __init__(self, width=3.0, height=2.0, depth=1.0, **kwargs):
        side_length = kwargs.pop("side_length", 2.0)
        z_index = int(kwargs.pop("z_index", 0))
        _refuse_unrouted(
            "VPrism()", [("side_length", float(side_length) != 2.0)]
        )
        fill_color = kwargs.pop("fill_color", _BLUE_D)
        fill_opacity = kwargs.pop("fill_opacity", 1)
        stroke_width = kwargs.pop("stroke_width", 0)
        style, depth_test, shading, joint_type = _split_native_vgroup3d_kwargs(
            "VPrism()", kwargs, (0.2, 0.2, 0.2)
        )
        _install_live_state(self)
        specs = self._build_vprism(
            _native_shell_factory,
            float(width),
            float(height),
            float(depth),
            fill_color,
            float(fill_opacity),
            float(stroke_width),
            z_index,
        )
        _hang_native_children(self, specs)
        style.setdefault("fill_color", fill_color)
        style.setdefault("fill_opacity", fill_opacity)
        style.setdefault("stroke_width", stroke_width)
        _apply_vmobject_style_kwargs(self, style)
        _apply_vgroup3d_config(self, depth_test, shading, joint_type)


class Tetrahedron(VGroup3D):
    """The regular tetrahedron under the wider manim ecosystem's spelling
    (fm-5wq.4.128) — not a pinned-Reference class (three_dimensions.py has
    only Dodecahedron among the polyhedra), the same provenance posture as
    Delay/AddTextLetterByLetter. Four native Polygon faces on the
    even-parity (±1, ±1, ±1) corners scaled to the requested edge, styled
    and depth-tested through the shared VGroup3D config path; unknown
    kwargs stay _split_native_vgroup3d_kwargs' named refusal."""

    def __init__(
        self,
        edge_length=2.0,
        fill_color=_BLUE_E,
        fill_opacity=1,
        stroke_color=_BLUE_E,
        stroke_width=1,
        **kwargs,
    ):
        edge_length = float(edge_length)
        if not (edge_length > 0.0 and _math.isfinite(edge_length)):
            raise ValueError(
                "Tetrahedron edge_length must be a positive finite "
                "length; got " + repr(edge_length)
            )
        style, depth_test, shading, joint_type = (
            _split_native_vgroup3d_kwargs(
                "Tetrahedron()", kwargs, (0.2, 0.2, 0.2)
            )
        )
        unit = edge_length / (2.0 * _math.sqrt(2.0))
        corners = (
            (unit, unit, unit),
            (unit, -unit, -unit),
            (-unit, unit, -unit),
            (-unit, -unit, unit),
        )
        faces = [
            Polygon(corners[0], corners[1], corners[2]),
            Polygon(corners[0], corners[1], corners[3]),
            Polygon(corners[0], corners[2], corners[3]),
            Polygon(corners[1], corners[2], corners[3]),
        ]
        super().__init__(
            *faces,
            depth_test=depth_test,
            shading=shading,
            joint_type=joint_type,
        )
        style.setdefault("fill_color", fill_color)
        style.setdefault("fill_opacity", fill_opacity)
        style.setdefault("stroke_color", stroke_color)
        style.setdefault("stroke_width", stroke_width)
        _apply_vmobject_style_kwargs(self, style)


class Dodecahedron(VGroup3D):
    def __init__(
        self,
        fill_color=_BLUE_E,
        fill_opacity=1,
        stroke_color=_BLUE_E,
        stroke_width=1,
        shading=(0.2, 0.2, 0.2),
        **kwargs,
    ):
        z_index = int(kwargs.pop("z_index", 0))
        style, depth_test, resolved_shading, joint_type = (
            _split_native_vgroup3d_kwargs(
                "Dodecahedron()", kwargs, tuple(shading)
            )
        )
        _install_live_state(self)
        specs = self._build_dodecahedron(
            _native_shell_factory,
            fill_color,
            float(fill_opacity),
            stroke_color,
            float(stroke_width),
            tuple(float(value) for value in resolved_shading),
            z_index,
        )
        _hang_native_children(self, specs)
        style.setdefault("fill_color", fill_color)
        style.setdefault("fill_opacity", fill_opacity)
        style.setdefault("stroke_color", stroke_color)
        style.setdefault("stroke_width", stroke_width)
        _apply_vmobject_style_kwargs(self, style)
        _apply_vgroup3d_config(
            self, depth_test, resolved_shading, joint_type
        )


class Prismify(VGroup3D):
    def __init__(self, vmobject, depth=1.0, direction=_IN, **kwargs):
        if not isinstance(vmobject, VMobject):
            raise TypeError("Prismify source must be a VMobject")
        direction = tuple(float(value) for value in _vec3(direction))
        if not _math.isfinite(float(depth)) or not all(
            _math.isfinite(value) for value in direction
        ):
            raise ValueError("Prismify depth and direction must be finite")
        style, depth_test, shading, joint_type = _split_native_vgroup3d_kwargs(
            "Prismify()", kwargs, (0.2, 0.2, 0.2)
        )
        _install_live_state(self)
        if vmobject.submobjects:
            # One native extrusion per pointful family member, in family
            # order (fm-5wq.4.81); each piece matches its own source's
            # style, the Reference's per-child reading of match_style.
            family_sources = [
                member
                for member in vmobject.family_members_with_points()
                if isinstance(member, VMobject)
            ]
            if not family_sources:
                raise ValueError(
                    "Prismify family has no pointful VMobject members to "
                    "extrude"
                )
            specs = self._build_prismify_family(
                _native_shell_factory,
                vmobject,
                float(depth),
                direction,
            )
            _hang_native_children(self, specs)
            for piece, family_source in zip(self.submobjects, family_sources):
                piece.match_style(family_source)
        else:
            specs = self._build_prismify(
                _native_shell_factory,
                vmobject,
                float(depth),
                direction,
            )
            _hang_native_children(self, specs)
            for piece in self.submobjects:
                piece.match_style(vmobject)
        _apply_vmobject_style_kwargs(self, style, recurse=False)
        _apply_vgroup3d_config(self, depth_test, shading, joint_type)


class SurfaceMesh(VGroup):
    """The wireframe over a native surface — a VMobject family (Reference
    MRO SurfaceMesh(VGroup)), built by the native mesher through the
    rebuild oracle and re-seated onto the source's current geometry."""

    def __init__(
        self,
        uv_surface,
        resolution=(21, 11),
        stroke_width=1,
        stroke_color=None,
        normal_nudge=0.01,
        depth_test=True,
        joint_type="no_joint",
        **kwargs,
    ):
        if joint_type not in VMobject.joint_type_map:
            raise KeyError(joint_type)
        _refuse_unrouted(
            "SurfaceMesh()",
            [(name, True) for name in sorted(kwargs)],
        )
        params = getattr(uv_surface, "_solid_params", None)
        if params is None:
            raise NotImplementedError(
                "SurfaceMesh needs a native-rebuildable source surface "
                "(Sphere is native); "
                + type(uv_surface).__name__
                + " does not carry solid params yet"
            )
        source_kind = params[0]
        source_axis = (0.0, 0.0, 1.0)
        if source_kind == "torus":
            source_radius = float(params[1])
            source_minor_radius = float(params[2])
        elif source_kind in ("cylinder", "cone"):
            source_radius = float(params[2])
            source_minor_radius = float(params[1])
            source_axis = tuple(float(component) for component in params[3])
        else:
            source_radius = float(params[1])
            source_minor_radius = 0.0
        _install_live_state(self)
        specs = self._build_surface_mesh(
            _native_shell_factory,
            source_kind,
            source_radius,
            source_minor_radius,
            source_axis,
            (int(resolution[0]), int(resolution[1])),
            float(normal_nudge),
            float(stroke_width),
            stroke_color,
            bool(depth_test),
            float(VMobject.joint_type_map[joint_type]),
        )
        _hang_native_children(self, specs)
        # Re-seat onto the source's CURRENT geometry (the rebuild is at
        # native scale/origin) — exact for uniform rescales and moves.
        native_height = getattr(
            uv_surface, "_solid_native_height", 2.0 * float(params[1])
        )
        current_height = uv_surface.get_height()
        if current_height > 0 and abs(current_height - native_height) > 1e-12:
            self.scale(current_height / native_height)
        self.move_to(uv_surface.get_center())
        if depth_test:
            self.apply_depth_test()
        else:
            self.deactivate_depth_test()
        self.set_joint_type(joint_type)


class TracingTail(VMobject):
    """The Reference's TracingTail (changing.py:151) over the native
    tracer (fmn-library fields.rs): a bound stage entry whose NATIVE
    dt-updater follows the traced mobject's center with the kept width
    and opacity tapers — no Python crossing per frame, and no
    parent-child relation to the traced mobject (it only reads the
    center, the correct Reference reading)."""

    def __init__(
        self,
        mobject_or_func,
        time_traced=1.0,
        stroke_color=None,
        stroke_width=(0, 3),
        stroke_opacity=(0, 1),
        time_per_anchor=1.0 / 15,
        **kwargs,
    ):
        # fm-5wq.4.123: the schema's kwargs chain (TracingTail → TracedPath
        # → VMobject) carries time_per_anchor and VMobject style keys;
        # unknown keys stay named TypeErrors via the style preflight.
        style_kwargs = dict(kwargs)
        _preflight_vmobject_style_kwargs(style_kwargs)
        self.time_per_anchor = float(time_per_anchor)
        if not isinstance(mobject_or_func, _BridgeMobject) and not callable(
            mobject_or_func
        ):
            raise TypeError(
                "TracingTail traces a Mobject or a point-returning "
                "callable; got " + type(mobject_or_func).__name__
            )

        def taper(value):
            if hasattr(value, "__len__"):
                return [float(v) for v in value]
            return [float(value), float(value)]

        if not isinstance(mobject_or_func, _BridgeMobject):
            # changing.py:151's other half (fm-5wq.4.86): a point function
            # is a TracedPath with the tail's finite window and tapers,
            # grown in the released Python-updater window — the same
            # verbatim update_path the TracedPath bind (fm-5wq.4.67) runs.
            # time_per_anchor stays stored-but-inert exactly as TracedPath
            # keeps it at the pin (update_path never consults it).
            VMobject.__init__(self, **style_kwargs)
            self.traced_point_func = mobject_or_func
            self.stroke_config = dict(
                color=stroke_color if stroke_color is not None else "#FFFFFF",
                width=taper(stroke_width),
                opacity=taper(stroke_opacity),
            )
            self.time_traced = float(time_traced)
            self.time = 0.0
            self.traced_points = []
            self.add_updater(
                lambda mob, dt: TracedPath.update_path(mob, dt)
            )
            return
        if not mobject_or_func._is_bound():
            raise NotImplementedError(
                "TracingTail traces a scene-bound mobject; add it to the "
                "Scene before tracing (fm-p107)"
            )

        if self.time_per_anchor != 1.0 / 15:
            # The native tail's prefill cadence is fixed at the Reference
            # default; a custom cadence has no native setter yet.
            raise NotImplementedError(
                "TracingTail time_per_anchor is not yet routed to the "
                "native tracer's prefill; the 1/15 default works"
            )
        _install_live_state(self)
        self._init_native_tracer(
            mobject_or_func._scene,
            mobject_or_func,
            float(time_traced),
            stroke_color,
            taper(stroke_width),
            taper(stroke_opacity),
        )
        if style_kwargs:
            _apply_vmobject_style_kwargs(self, style_kwargs)


class ValueTracker(Mobject):
    """The Reference's ValueTracker over the native tracker entries
    (§8.6, Stage::add_value_tracker): the value is real engine state in
    both proxy states and survives scene adoption."""

    value_type = _np.float64
    _tracker_kind = 0  # Plain

    def __init__(self, value=0, **kwargs):
        _install_live_state(self)
        for key, val in kwargs.items():
            setattr(self, key, val)
        if isinstance(value, complex):
            raise TypeError(
                type(self).__name__
                + " holds a scalar; use ComplexValueTracker for complex values"
            )
        self._init_value_tracker(type(self)._tracker_kind, float(value), 0.0)

    def get_value(self):
        return self.value_type(self._tracker_value())

    def set_value(self, value):
        self._set_tracker_value(float(value))
        return self

    def increment_value(self, d_value):
        self.set_value(self.get_value() + d_value)


class ExponentialValueTracker(ValueTracker):
    _tracker_kind = 1


class ComplexValueTracker(ValueTracker):
    value_type = _np.complex128
    _tracker_kind = 2

    def __init__(self, value=0, **kwargs):
        _install_live_state(self)
        for key, val in kwargs.items():
            setattr(self, key, val)
        value = complex(value)
        self._init_value_tracker(2, value.real, value.imag)

    def get_value(self):
        re, im = self._tracker_complex_value()
        return self.value_type(complex(re, im))

    def set_value(self, value):
        value = complex(value)
        self._set_tracker_complex_value(value.real, value.imag)
        return self


class ControlMobject(ValueTracker):
    """The interactive-control base as a live native value tracker.

    Concrete controls own ``assert_value`` and ``set_value_anim``; this base
    preserves the Reference's tracker-first MRO and fixed-frame composition.
    """

    def __init__(self, value, *mobjects, **kwargs):
        if kwargs:
            raise TypeError(
                "unexpected keyword arguments: " + ", ".join(sorted(kwargs))
            )
        if not all(isinstance(mobject, Mobject) for mobject in mobjects):
            raise TypeError("ControlMobject children must be Mobject instances")
        _install_live_state(self)
        self._init_value_tracker(0, float(value), 0.0)
        self.add(*mobjects)
        self.add_updater(lambda mob: None)
        self.fix_in_frame()

    def assert_value(self, value):
        del value

    def set_value_anim(self, value):
        del value

    def set_value(self, value):
        self.assert_value(value)
        self.set_value_anim(value)
        return ValueTracker.set_value(self, value)


class Checkbox(ControlMobject):
    """Atlas's native checkbox composition over a live bool tracker."""

    def __init__(
        self,
        value=True,
        value_type=_np.dtype(bool),
        rect_kwargs=dict(width=0.5, height=0.5, fill_opacity=0.0),
        checkmark_kwargs=dict(stroke_color=_GREEN, stroke_width=6),
        cross_kwargs=dict(stroke_color=_RED, stroke_width=6),
        box_content_buff=0.1,
        **kwargs,
    ):
        if kwargs:
            raise TypeError(
                "unexpected keyword arguments: " + ", ".join(sorted(kwargs))
            )
        if not isinstance(value, bool):
            raise AssertionError("Checkbox value must be bool")

        rect_config = dict(rect_kwargs)
        rect_unknown = sorted(
            set(rect_config) - {"width", "height", "fill_opacity"}
        )
        if rect_unknown:
            raise TypeError(
                "unexpected keyword arguments: "
                + ", ".join("rect_kwargs." + name for name in rect_unknown)
            )
        box_width = float(rect_config.get("width", 0.5))
        box_height = float(rect_config.get("height", 0.5))
        fill_opacity = float(rect_config.get("fill_opacity", 0.0))

        check_config = dict(checkmark_kwargs)
        check_unknown = sorted(
            set(check_config) - {"stroke_color", "stroke_width"}
        )
        cross_config = dict(cross_kwargs)
        cross_unknown = sorted(
            set(cross_config) - {"stroke_color", "stroke_width"}
        )
        nested_unknown = [
            *("checkmark_kwargs." + name for name in check_unknown),
            *("cross_kwargs." + name for name in cross_unknown),
        ]
        if nested_unknown:
            raise TypeError(
                "unexpected keyword arguments: " + ", ".join(nested_unknown)
            )

        self.value_type = _np.dtype(value_type).type
        self.rect_kwargs = rect_config
        self.checkmark_kwargs = check_config
        self.cross_kwargs = cross_config
        self.box_content_buff = float(box_content_buff)
        self._checkbox_native_config = (
            box_width,
            box_height,
            fill_opacity,
            tuple(_color_to_rgb(check_config.get("stroke_color", _GREEN))),
            float(check_config.get("stroke_width", 6)),
            tuple(_color_to_rgb(cross_config.get("stroke_color", _RED))),
            float(cross_config.get("stroke_width", 6)),
        )
        box, content = self._native_checkbox_parts(value)
        super().__init__(value, box, content)
        self.box = box
        self.box_content = content

    def _native_checkbox_parts(self, value):
        specs = self._build_checkbox(
            _native_shell_factory,
            bool(value),
            *self._checkbox_native_config,
        )
        parts = []
        for shell, child_specs in specs:
            _hang_native_children(shell, child_specs)
            parts.append(shell)
        if len(parts) != 2:
            raise RuntimeError(
                "native Checkbox family contract drift: expected box + content"
            )
        return parts

    def assert_value(self, value):
        if not isinstance(value, bool):
            raise AssertionError("Checkbox value must be bool")

    def set_value_anim(self, value):
        box, content = self._native_checkbox_parts(value)
        self.box.become(box)
        self.box_content.become(content)

    def toggle_value(self):
        self.set_value(not bool(self.get_value()))


class EnableDisableButton(ControlMobject):
    """Atlas's native enabled/disabled box over a live bool tracker."""

    def __init__(
        self,
        value=True,
        value_type=_np.dtype(bool),
        rect_kwargs=dict(width=0.5, height=0.5, fill_opacity=1.0),
        enable_color=_GREEN,
        disable_color=_RED,
        **kwargs,
    ):
        if kwargs:
            raise TypeError(
                "unexpected keyword arguments: " + ", ".join(sorted(kwargs))
            )
        if not isinstance(value, bool):
            raise AssertionError("EnableDisableButton value must be bool")

        rect_config = dict(rect_kwargs)
        rect_unknown = sorted(
            set(rect_config) - {"width", "height", "fill_opacity"}
        )
        if rect_unknown:
            raise TypeError(
                "unexpected keyword arguments: "
                + ", ".join("rect_kwargs." + name for name in rect_unknown)
            )
        width = float(rect_config.get("width", 0.5))
        height = float(rect_config.get("height", 0.5))
        fill_opacity = float(rect_config.get("fill_opacity", 1.0))

        self.value_type = _np.dtype(value_type).type
        self.rect_kwargs = rect_config
        self.enable_color = enable_color
        self.disable_color = disable_color
        self._enable_disable_native_config = (
            width,
            height,
            fill_opacity,
            tuple(_color_to_rgb(enable_color)),
            tuple(_color_to_rgb(disable_color)),
        )
        (box,) = self._native_enable_disable_parts(value, colored=False)
        super().__init__(value, box)
        self.box = box

    def _native_enable_disable_parts(self, value, *, colored):
        specs = self._build_enable_disable_button(
            _native_shell_factory,
            bool(value),
            bool(colored),
            *self._enable_disable_native_config,
        )
        parts = []
        for shell, child_specs in specs:
            _hang_native_children(shell, child_specs)
            parts.append(shell)
        if len(parts) != 1:
            raise RuntimeError(
                "native EnableDisableButton family contract drift: expected box"
            )
        return parts

    def assert_value(self, value):
        if not isinstance(value, bool):
            raise AssertionError("EnableDisableButton value must be bool")

    def set_value_anim(self, value):
        (box,) = self._native_enable_disable_parts(value, colored=True)
        self.box.become(box)

    def toggle_value(self):
        self.set_value(not bool(self.get_value()))


class LinearNumberSlider(ControlMobject):
    """Atlas's native linear-number control over a live value tracker."""

    def __init__(
        self,
        value=0,
        value_type=_np.float64,
        min_value=-10.0,
        max_value=10.0,
        step=1.0,
        rounded_rect_kwargs=dict(
            height=0.075,
            width=2,
            corner_radius=0.0375,
        ),
        circle_kwargs=dict(
            radius=0.1,
            stroke_color=_GREY_A,
            fill_color=_GREY_A,
            fill_opacity=1.0,
        ),
        **kwargs,
    ):
        rect_config = dict(rounded_rect_kwargs)
        rect_unknown = sorted(
            set(rect_config) - {"height", "width", "corner_radius"}
        )
        if rect_unknown:
            raise TypeError(
                "unexpected keyword arguments: "
                + ", ".join(
                    "rounded_rect_kwargs." + name for name in rect_unknown
                )
            )
        corner_radius = float(rect_config.get("corner_radius", 0.0375))

        handle_config = dict(circle_kwargs)
        handle_unknown = sorted(
            set(handle_config)
            - {"radius", "stroke_color", "fill_color", "fill_opacity"}
        )
        if handle_unknown:
            raise TypeError(
                "unexpected keyword arguments: "
                + ", ".join(
                    "circle_kwargs." + name for name in handle_unknown
                )
            )
        handle_radius = float(handle_config.get("radius", 0.1))
        handle_fill_opacity = float(handle_config.get("fill_opacity", 1.0))
        stroke_color = tuple(
            _color_to_rgb(handle_config.get("stroke_color", _GREY_A))
        )
        fill_color = tuple(
            _color_to_rgb(handle_config.get("fill_color", _GREY_A))
        )

        self.value_type = _np.dtype(value_type).type
        self.min_value = float(min_value)
        self.max_value = float(max_value)
        self.step = float(step)
        self.rounded_rect_kwargs = rect_config
        self.circle_kwargs = handle_config
        specs = self._build_linear_number_slider(
            _native_shell_factory,
            float(value),
            self.min_value,
            self.max_value,
            self.step,
            float(rect_config.get("width", 2.0)),
            float(rect_config.get("height", 0.075)),
            corner_radius,
            handle_radius,
            handle_fill_opacity,
            stroke_color,
            fill_color,
        )
        parts = []
        for shell, child_specs in specs:
            _hang_native_children(shell, child_specs)
            parts.append(shell)
        if len(parts) != 3:
            raise RuntimeError(
                "native LinearNumberSlider family contract drift: expected "
                "bar + handle + axis"
            )
        super().__init__(value, *parts)
        for key, item in kwargs.items():
            setattr(self, key, item)
        self.bar, self.slider, self.slider_axis = parts


class ColorSliders(Group):
    """Atlas's four-channel slider bank and checkerboard swatch."""

    def __init__(
        self,
        sliders_kwargs=dict(),
        rect_kwargs=dict(width=2.0, height=0.5, stroke_opacity=1.0),
        background_grid_kwargs=dict(
            colors=[_GREY_A, _GREY_C],
            single_square_len=0.1,
        ),
        sliders_buff=_MED_LARGE_BUFF,
        default_rgb_value=255,
        default_a_value=1,
        **kwargs,
    ):
        if kwargs:
            raise TypeError(
                "unexpected keyword arguments: " + ", ".join(sorted(kwargs))
            )
        if sliders_kwargs:
            raise NotImplementedError(
                "ColorSliders sliders_kwargs are not routed to the native builder"
            )
        rect_config = dict(rect_kwargs)
        rect_unknown = sorted(
            set(rect_config) - {"width", "height", "stroke_opacity"}
        )
        if rect_unknown:
            raise TypeError(
                "unexpected keyword arguments: "
                + ", ".join("rect_kwargs." + name for name in rect_unknown)
            )
        if (
            float(rect_config.get("width", 2.0)) != 2.0
            or float(rect_config.get("height", 0.5)) != 0.5
            or float(rect_config.get("stroke_opacity", 1.0)) != 1.0
        ):
            raise NotImplementedError(
                "ColorSliders rect_kwargs are not routed to the native builder"
            )
        grid_config = dict(background_grid_kwargs)
        grid_unknown = sorted(
            set(grid_config) - {"colors", "single_square_len"}
        )
        if grid_unknown:
            raise TypeError(
                "unexpected keyword arguments: "
                + ", ".join(
                    "background_grid_kwargs." + name for name in grid_unknown
                )
            )
        grid_colors = [
            tuple(_color_to_rgb(color))
            for color in grid_config.get("colors", [_GREY_A, _GREY_C])
        ]
        if grid_colors != [
            tuple(_color_to_rgb(_GREY_A)),
            tuple(_color_to_rgb(_GREY_C)),
        ] or float(grid_config.get("single_square_len", 0.1)) != 0.1:
            raise NotImplementedError(
                "ColorSliders background_grid_kwargs are not routed to the native builder"
            )

        self.sliders_kwargs = dict(sliders_kwargs)
        self.rect_kwargs = rect_config
        self.background_grid_kwargs = grid_config
        self.sliders_buff = float(sliders_buff)
        self.default_rgb_value = float(default_rgb_value)
        self.default_a_value = float(default_a_value)
        if self.default_rgb_value != 255.0 or self.default_a_value != 1.0:
            raise NotImplementedError(
                "ColorSliders custom default values are not routed to the native builder"
            )
        self._color_slider_components = (
            self.default_rgb_value,
            self.default_rgb_value,
            self.default_rgb_value,
            self.default_a_value,
        )
        swatch, sliders = self._native_color_slider_parts(
            self._color_slider_components,
            apply_value=False,
        )
        super().__init__(swatch, sliders)
        self.swatch = swatch
        self.background, self.selected_color_box = swatch.submobjects
        self.sliders = sliders
        (
            self.r_slider,
            self.g_slider,
            self.b_slider,
            self.a_slider,
        ) = sliders.submobjects

    def _native_color_slider_parts(self, components, *, apply_value):
        specs = self._build_color_sliders(
            _native_shell_factory,
            *components,
            self.sliders_buff,
            bool(apply_value),
        )
        parts = []
        for shell, child_specs in specs:
            _hang_native_children(shell, child_specs)
            parts.append(shell)
        if (
            len(parts) != 2
            or len(parts[0].submobjects) != 2
            or len(parts[1].submobjects) != 4
        ):
            raise RuntimeError(
                "native ColorSliders family contract drift: expected swatch + four sliders"
            )
        return parts

    def get_value(self):
        red, green, blue, alpha = self._color_slider_components
        return _np.array([red / 255.0, green / 255.0, blue / 255.0, alpha])

    def set_value(self, r, g, b, a):
        components = (float(r), float(g), float(b), float(a))
        swatch, sliders = self._native_color_slider_parts(
            components,
            apply_value=True,
        )
        next_background, next_color_box = swatch.submobjects
        self.background.become(next_background)
        self.selected_color_box.become(next_color_box)
        self.sliders.become(sliders)
        self._color_slider_components = components

    def get_picked_color(self):
        return _rgb_to_hex(self.get_value()[:3])

    def get_picked_opacity(self):
        return float(self.get_value()[3])


class Textbox(ControlMobject):
    """Atlas's native FontBook-backed string control.

    Strings cannot inhabit Marionette's scalar tracker lane, so this authored
    ControlMobject subclass keeps its value as portal string state while Atlas
    owns every box and text candidate. Candidate construction completes before
    the live text proxy or stored value changes.
    """

    def __init__(
        self,
        value="",
        value_type=_np.dtype(object),
        box_kwargs=dict(
            width=2.0,
            height=1.0,
            fill_color=_WHITE,
            fill_opacity=1.0,
        ),
        text_kwargs=dict(color=_BLUE),
        text_buff=0.25,
        isInitiallyActive=False,
        active_color=_BLUE,
        deactive_color=_RED,
        **kwargs,
    ):
        if kwargs:
            raise TypeError(
                "unexpected keyword arguments: " + ", ".join(sorted(kwargs))
            )
        if not isinstance(value, str):
            raise TypeError("Textbox value must be a string")

        box_config = dict(box_kwargs)
        box_unknown = sorted(
            set(box_config)
            - {"width", "height", "fill_color", "fill_opacity"}
        )
        if box_unknown:
            raise TypeError(
                "unexpected keyword arguments: "
                + ", ".join("box_kwargs." + name for name in box_unknown)
            )
        text_config = dict(text_kwargs)
        text_unknown = sorted(set(text_config) - {"color"})
        if text_unknown:
            raise TypeError(
                "unexpected keyword arguments: "
                + ", ".join("text_kwargs." + name for name in text_unknown)
            )

        self.value_type = _np.dtype(value_type).type
        self.box_kwargs = box_config
        self.text_kwargs = text_config
        self.text_buff = float(text_buff)
        self.isInitiallyActive = bool(isInitiallyActive)
        self.active_color = active_color
        self.deactive_color = deactive_color
        self.isActive = self.isInitiallyActive
        self._textbox_value = value
        box, text = self._native_textbox_parts(value, None)
        super().__init__(0.0, box, text)
        self.box = box
        self.text = text
        self.text.add_updater(lambda mob: mob.move_to(self.box))

    def _native_textbox_parts(self, current, replacement):
        box_config = self.box_kwargs
        text_config = self.text_kwargs
        specs = self._build_textbox(
            _native_shell_factory,
            current,
            replacement,
            self.isActive,
            (
                float(box_config.get("width", 2.0)),
                float(box_config.get("height", 1.0)),
                tuple(_color_to_rgb(box_config.get("fill_color", _WHITE))),
                float(box_config.get("fill_opacity", 1.0)),
            ),
            tuple(_color_to_rgb(text_config.get("color", _BLUE))),
            float(self.text_buff),
            tuple(_color_to_rgb(self.active_color)),
            tuple(_color_to_rgb(self.deactive_color)),
        )
        parts = []
        for shell, child_specs in specs:
            _hang_native_children(shell, child_specs)
            parts.append(shell)
        if len(parts) != 2:
            raise RuntimeError(
                "native Textbox family contract drift: expected box + text"
            )
        return parts

    def get_value(self):
        return self.value_type(self._textbox_value)

    def set_value(self, value):
        if not isinstance(value, str):
            raise TypeError("Textbox value must be a string")
        _, candidate = self._native_textbox_parts(
            self._textbox_value,
            value,
        )
        self.text.become(candidate)
        self._textbox_value = value
        return self

    def set_value_anim(self, value):
        self.update_text(value)

    def update_text(self, value):
        if not isinstance(value, str):
            raise TypeError("Textbox value must be a string")
        _, candidate = self._native_textbox_parts(
            self._textbox_value,
            value,
        )
        self.text.become(candidate)


class ControlPanel(Group):
    """Atlas's native GREY_C panel, opener tab, and controls column."""

    def __init__(
        self,
        *controls,
        panel_kwargs=dict(
            width=_FRAME_SHAPE[0] / 4.0,
            height=_MED_SMALL_BUFF + _FRAME_HEIGHT,
            fill_color=_GREY_C,
            fill_opacity=1.0,
            stroke_width=0.0,
        ),
        opener_kwargs=dict(
            width=_FRAME_SHAPE[0] / 8.0,
            height=0.5,
            fill_color=_GREY_C,
            fill_opacity=1.0,
        ),
        opener_text_kwargs=dict(text="Control Panel", font_size=20),
        **kwargs,
    ):
        if kwargs:
            raise TypeError(
                "unexpected keyword arguments: " + ", ".join(sorted(kwargs))
            )
        if not all(isinstance(control, ControlMobject) for control in controls):
            raise TypeError(
                "ControlPanel controls must be ControlMobject instances"
            )

        panel_config = dict(panel_kwargs)
        panel_unknown = sorted(
            set(panel_config)
            - {"width", "height", "fill_color", "fill_opacity", "stroke_width"}
        )
        if panel_unknown:
            raise TypeError(
                "unexpected keyword arguments: "
                + ", ".join("panel_kwargs." + name for name in panel_unknown)
            )

        opener_config = dict(opener_kwargs)
        opener_unknown = sorted(
            set(opener_config) - {"width", "height", "fill_color", "fill_opacity"}
        )
        if opener_unknown:
            raise TypeError(
                "unexpected keyword arguments: "
                + ", ".join("opener_kwargs." + name for name in opener_unknown)
            )

        text_config = dict(opener_text_kwargs)
        text_unknown = sorted(set(text_config) - {"text", "font_size"})
        if text_unknown:
            raise TypeError(
                "unexpected keyword arguments: "
                + ", ".join("opener_text_kwargs." + name for name in text_unknown)
            )
        opener_text = str(text_config.get("text", "Control Panel"))
        opener_font_size = float(text_config.get("font_size", 20))

        self.panel_kwargs = panel_config
        self.opener_kwargs = opener_config
        self.opener_text_kwargs = text_config
        self._control_panel_open = False
        panel, opener, controls_group = self._native_control_panel_parts(
            opener_text,
            opener_font_size,
            controls,
            open=False,
        )
        super().__init__(panel, opener, controls_group)
        self.panel = panel
        self.panel_opener = opener
        self.controls = controls_group

    def _native_control_panel_parts(
        self,
        opener_text,
        opener_font_size,
        controls,
        *,
        open,
    ):
        control_extents = []
        for control in controls:
            box = control.get_bounding_box()
            control_extents.append((_vec3(box[0]), _vec3(box[2])))
        panel_config = self.panel_kwargs
        opener_config = self.opener_kwargs
        specs = self._build_control_panel(
            _native_shell_factory,
            control_extents,
            opener_text,
            opener_font_size,
            bool(open),
            (
                float(panel_config.get("width", _FRAME_SHAPE[0] / 4.0)),
                float(panel_config.get("height", _MED_SMALL_BUFF + _FRAME_HEIGHT)),
                tuple(_color_to_rgb(panel_config.get("fill_color", _GREY_C))),
                float(panel_config.get("fill_opacity", 1.0)),
                float(panel_config.get("stroke_width", 0.0)),
            ),
            (
                float(opener_config.get("width", _FRAME_SHAPE[0] / 8.0)),
                float(opener_config.get("height", 0.5)),
                tuple(_color_to_rgb(opener_config.get("fill_color", _GREY_C))),
                float(opener_config.get("fill_opacity", 1.0)),
            ),
        )
        parts = []
        for shell, child_specs in specs:
            _hang_native_children(shell, child_specs)
            parts.append(shell)
        if (
            len(parts) != 3
            or len(parts[1].submobjects) != 2
        ):
            raise RuntimeError(
                "native ControlPanel family contract drift: expected "
                "panel + opener + controls"
            )
        targets = list(parts[2].submobjects)
        if len(targets) != len(controls):
            raise RuntimeError(
                "native ControlPanel controls contract drift: "
                f"expected {len(controls)} controls, got {len(targets)}"
            )
        for control, target in zip(controls, targets):
            control.shift(target.get_center() - control.get_center())
        parts[2].set_submobjects(controls)
        return parts

    def _rebuild_control_panel(self, *, open):
        text_config = self.opener_text_kwargs
        panel, opener, controls_group = self._native_control_panel_parts(
            str(text_config.get("text", "Control Panel")),
            float(text_config.get("font_size", 20)),
            tuple(self.controls.submobjects),
            open=open,
        )
        self.panel.shift(panel.get_center() - self.panel.get_center())
        self.panel_opener.shift(
            opener.get_center() - self.panel_opener.get_center()
        )
        del controls_group
        self._control_panel_open = bool(open)
        return self

    def _layout_controls_against_opener(self):
        panel_box = self.panel.get_bounding_box()
        opener_box = self.panel_opener.submobjects[0].get_bounding_box()
        control_extents = []
        controls = tuple(self.controls.submobjects)
        for control in controls:
            box = control.get_bounding_box()
            control_extents.append((_vec3(box[0]), _vec3(box[2])))
        specs = self._layout_control_panel_against_opener(
            _native_shell_factory,
            (_vec3(panel_box[0]), _vec3(panel_box[2])),
            (_vec3(opener_box[0]), _vec3(opener_box[2])),
            control_extents,
        )
        parts = []
        for shell, child_specs in specs:
            _hang_native_children(shell, child_specs)
            parts.append(shell)
        if len(parts) != 2:
            raise RuntimeError(
                "native ControlPanel layout contract drift: expected "
                "panel + controls"
            )
        panel_target, controls_target = parts
        targets = list(controls_target.submobjects)
        if len(targets) != len(controls):
            raise RuntimeError(
                "native ControlPanel controls contract drift: "
                f"expected {len(controls)} controls, got {len(targets)}"
            )
        # Matcher targets are unstyled extents; shift keeps panel_kwargs.
        self.panel.shift(panel_target.get_center() - self.panel.get_center())
        for control, target in zip(controls, targets):
            control.shift(target.get_center() - control.get_center())

    def add_controls(self, *new_controls):
        self.controls.add(*new_controls)
        self._layout_controls_against_opener()

    def remove_controls(self, *controls_to_remove):
        self.controls.remove(*controls_to_remove)
        self._layout_controls_against_opener()

    def open_panel(self):
        return self._rebuild_control_panel(open=True)

    def close_panel(self):
        return self._rebuild_control_panel(open=False)


class CameraFrame(Mobject):
    """The Reference's camera frame (manimlib/camera/camera_frame.py) as a
    real Mobject whose authoritative state lives in one engine
    `_CameraFrameCore` (Lumen's `fmn_render::CameraFrame`, fm-0gy).

    State-real by construction (D5): orientation, center, shape, and field
    of view round-trip exactly through the engine value — never an inert
    stub. The positional primitives (`shift`/`scale`/`stretch`/`_bbox_rows`)
    are overridden to read and write that same state, so every inherited
    Mobject method (`move_to`, `set_x`, `set_height`, `center`, ...)
    operates on the camera frame exactly as the Reference's point-backed
    implementation does. `self._core` is the renderer-binding seam: final
    native PNG capture hands this same engine value to Lumen's `Camera`.

    Divergence note: without a SciPy dependency, `set_orientation` accepts
    a scipy-order `(x, y, z, w)` quaternion sequence (or any object with an
    `as_quat()` method) and `get_orientation` returns that quaternion as a
    numpy array rather than a `scipy.spatial.transform.Rotation`.
    """

    def __init__(
        self,
        frame_shape=_FRAME_SHAPE,
        center_point=_ORIGIN,
        fovy=45 * _DEG,
        euler_axes="zxz",
        z_index=-1,
        **kwargs,
    ):
        super().__init__(z_index=z_index, **kwargs)
        self._core = _CameraFrameCore(
            (float(frame_shape[0]), float(frame_shape[1])),
            _vec3(center_point),
            float(fovy),
            euler_axes,
        )

    def copy(self, deep=False):
        # The generic shallow copy shares non-mobject attributes by
        # reference; the camera core must never be shared, or .animate's
        # target would mutate the live frame.
        result = super().copy(deep)
        if getattr(result, "_core", None) is self._core:
            result.__dict__["_core"] = _copy.copy(self._core)
        return result

    # -- the positional primitives, routed to the engine camera state

    def _bbox_rows(self):
        center = _np.array(self._core.center())
        width, height = self._core.shape()
        half = _np.array([width / 2.0, height / 2.0, 0.0])
        return _np.array([center - half, center, center + half])

    def shift(self, vector):
        vector = _vec3(vector)
        center = self._core.center()
        self._core.set_center(
            (
                center[0] + vector[0],
                center[1] + vector[1],
                center[2] + vector[2],
            )
        )
        return self

    def scale(
        self,
        scale_factor,
        min_scale_factor=1e-8,
        about_point=None,
        about_edge=_ORIGIN,
    ):
        pivot = _np.array(self._resolve_pivot(about_point, about_edge))
        factor = max(float(scale_factor), float(min_scale_factor))
        center = _np.array(self._core.center())
        width, height = self._core.shape()
        self._core.set_shape((width * factor, height * factor))
        self._core.set_center(tuple(pivot + factor * (center - pivot)))
        return self

    def stretch(self, factor, dim, **kwargs):
        pivot = _np.array(
            self._resolve_pivot(
                kwargs.pop("about_point", None), kwargs.pop("about_edge", _ORIGIN)
            )
        )
        if kwargs:
            raise TypeError(
                "stretch() got unexpected keyword arguments: "
                + ", ".join(sorted(kwargs))
            )
        factor = float(factor)
        dim = int(dim)
        center = _np.array(self._core.center())
        center[dim] = pivot[dim] + factor * (center[dim] - pivot[dim])
        if dim in (0, 1):
            width, height = self._core.shape()
            if dim == 0:
                width *= factor
            else:
                height *= factor
            self._core.set_shape((width, height))
        self._core.set_center(tuple(center))
        return self

    # -- orientation

    def set_orientation(self, rotation):
        if hasattr(rotation, "as_quat"):
            rotation = rotation.as_quat()
        self._core.set_orientation(
            (
                float(rotation[0]),
                float(rotation[1]),
                float(rotation[2]),
                float(rotation[3]),
            )
        )
        return self

    def get_orientation(self):
        return _np.array(self._core.orientation())

    def make_orientation_default(self):
        self._core.make_orientation_default()
        return self

    def to_default_state(self):
        self._core.to_default_state()
        return self

    def get_euler_angles(self):
        return _np.array(self._core.euler_angles())

    def get_theta(self):
        return self.get_euler_angles()[0]

    def get_phi(self):
        return self.get_euler_angles()[1]

    def get_gamma(self):
        return self.get_euler_angles()[2]

    def get_scale(self):
        return self._core.scale()

    def get_inverse_camera_rotation_matrix(self):
        return _np.array(self._core.view_matrix())[:3, :3] * self.get_scale()

    def get_view_matrix(self, refresh=False):
        del refresh  # the engine value is always current
        return _np.array(self._core.view_matrix())

    def get_inv_view_matrix(self):
        return _np.linalg.inv(self.get_view_matrix())

    def rotate(self, angle, axis=_OUT, **kwargs):
        del kwargs  # the Reference ignores point-function kwargs here too
        self._core.rotate(float(angle), _vec3(axis))
        return self

    def set_euler_angles(self, theta=None, phi=None, gamma=None, units=_RADIANS):
        self._core.set_euler_angles(
            None if theta is None else float(theta) * units,
            None if phi is None else float(phi) * units,
            None if gamma is None else float(gamma) * units,
        )
        return self

    def increment_euler_angles(self, dtheta=0, dphi=0, dgamma=0, units=_RADIANS):
        self._core.increment_euler_angles(
            float(dtheta) * units, float(dphi) * units, float(dgamma) * units
        )
        return self

    def set_euler_axes(self, seq):
        self._core.set_euler_axes(seq)

    def reorient(
        self,
        theta_degrees=None,
        phi_degrees=None,
        gamma_degrees=None,
        center=None,
        height=None,
    ):
        self.set_euler_angles(theta_degrees, phi_degrees, gamma_degrees, units=_DEG)
        if center is not None:
            self.move_to(_np.array(center))
        if height is not None:
            self.set_height(height)
        return self

    def set_theta(self, theta):
        return self.set_euler_angles(theta=theta)

    def set_phi(self, phi):
        return self.set_euler_angles(phi=phi)

    def set_gamma(self, gamma):
        return self.set_euler_angles(gamma=gamma)

    def increment_theta(self, dtheta, units=_RADIANS):
        self.increment_euler_angles(dtheta=dtheta, units=units)
        return self

    def increment_phi(self, dphi, units=_RADIANS):
        self.increment_euler_angles(dphi=dphi, units=units)
        return self

    def increment_gamma(self, dgamma, units=_RADIANS):
        self.increment_euler_angles(dgamma=dgamma, units=units)
        return self

    def add_ambient_rotation(self, angular_speed=1 * _DEG):
        self.add_updater(lambda m, dt: m.increment_theta(angular_speed * dt))
        return self

    def set_focal_distance(self, focal_distance):
        self._core.set_focal_distance(float(focal_distance))
        return self

    def set_field_of_view(self, field_of_view):
        self._core.set_field_of_view(float(field_of_view))
        return self

    def get_shape(self):
        return self._core.shape()

    def get_aspect_ratio(self):
        return self._core.aspect_ratio()

    def get_center(self):
        return _np.array(self._core.center())

    def get_width(self):
        return self._core.shape()[0]

    def get_height(self):
        return self._core.shape()[1]

    def get_focal_distance(self):
        return self._core.focal_distance()

    def get_field_of_view(self):
        return self._core.field_of_view()

    def get_implied_camera_location(self):
        return _np.array(self._core.implied_camera_location())

    def to_fixed_frame_point(self, point, relative=False):
        return _np.array(self._core.to_fixed_frame_point(_vec3(point), bool(relative)))

    def from_fixed_frame_point(self, point, relative=False):
        return _np.array(
            self._core.from_fixed_frame_point(_vec3(point), bool(relative))
        )


class Camera:
    """The Reference capture-camera surface over Lumen's native builder.

    Lumen owns validated resolution, frame-rate, color, clipping, sample, and
    light configuration. The Python ``CameraFrame`` remains the live identity
    scenes mutate, initialized to the exact aspect-corrected shape returned by
    that native camera.
    """

    def __init__(
        self,
        window=None,
        background_image=None,
        frame_config=dict(),
        resolution=(1920, 1080),
        fps=30,
        background_color=_BLACK,
        background_opacity=1.0,
        max_allowable_norm=14.222222222222221,
        image_mode="RGBA",
        n_channels=4,
        pixel_array_dtype=_np.uint8,
        light_source_position=(-10.0, 10.0, 10.0),
        samples=0,
    ):
        _refuse_unrouted(
            "Camera()",
            [
                ("window", window is not None),
                ("background_image", background_image is not None),
                ("image_mode", image_mode != "RGBA"),
                ("n_channels", n_channels != 4),
                ("pixel_array_dtype", pixel_array_dtype is not _np.uint8),
            ],
        )
        if not isinstance(frame_config, dict):
            raise TypeError("Camera frame_config must be a dict")

        frame = CameraFrame(**frame_config)
        background_rgb = tuple(_color_to_rgb(background_color))
        core = _CameraCore(
            frame._core,
            resolution,
            fps,
            background_rgb,
            background_opacity,
            max_allowable_norm,
            _vec3(light_source_position),
            samples,
        )
        frame._core.set_shape(core.frame_shape())
        light_source = Point(light_source_position)

        self._core = core
        self.window = None
        self.background_image = None
        self.default_pixel_shape = tuple(core.pixel_shape())
        self.fps = core.fps()
        self.max_allowable_norm = core.max_allowable_norm()
        self.image_mode = "RGBA"
        self.n_channels = 4
        self.pixel_array_dtype = _np.uint8
        self.light_source_position = _np.array(core.light_source_position())
        self.samples = core.samples()
        self.rgb_max_val = float(_np.iinfo(self.pixel_array_dtype).max)
        self.background_rgba = list(
            _color_to_rgba(background_color, background_opacity)
        )
        self.uniforms = {}
        self.frame = frame
        self.light_source = light_source

    def get_pixel_size(self):
        return self.frame.get_width() / self.get_pixel_width()

    def get_pixel_shape(self):
        return tuple(self._core.pixel_shape())

    def get_pixel_width(self):
        return self.get_pixel_shape()[0]

    def get_pixel_height(self):
        return self.get_pixel_shape()[1]

    def get_aspect_ratio(self):
        width, height = self.get_pixel_shape()
        return width / height

    def get_frame_height(self):
        return self.frame.get_height()

    def get_frame_width(self):
        return self.frame.get_width()

    def get_frame_shape(self):
        return (self.get_frame_width(), self.get_frame_height())

    def get_frame_center(self):
        return self.frame.get_center()

    def get_location(self):
        return tuple(self.frame.get_implied_camera_location())

    def resize_frame_shape(self, fixed_dimension=False):
        width, height = self.get_frame_shape()
        if fixed_dimension:
            width = self.get_aspect_ratio() * height
        else:
            height = width / self.get_aspect_ratio()
        self.frame._core.set_shape((width, height))

    def refresh_uniforms(self):
        frame = self.frame
        self.uniforms.update(
            view=tuple(frame.get_view_matrix().T.flatten()),
            frame_scale=frame.get_scale(),
            frame_rescale_factors=(
                2.0 / (_FRAME_HEIGHT * _ASPECT_RATIO),
                2.0 / _FRAME_HEIGHT,
                frame.get_scale() / frame.get_focal_distance(),
            ),
            pixel_size=self.get_pixel_size(),
            camera_position=tuple(frame.get_implied_camera_location()),
            light_position=tuple(self.light_source.get_location()),
        )


class ThreeDCamera(Camera):
    """Camera with Lumen's Reference four-sample constructor default."""

    def __init__(self, samples=4, **kwargs):
        super().__init__(samples=samples, **kwargs)


class Scene(_SceneCore):
    def __init__(self, *args, **kwargs):
        self.args = args
        self.kwargs = kwargs
        # Reference Scene.__init__: deterministic scenes are the default.
        # The compatibility portal seeds the two RNG modules source-unedited
        # scenes actually call; engine-owned randomness remains Marionette's
        # named PCG64DXSM streams.
        self.random_seed = kwargs.get(
            "random_seed", getattr(type(self), "random_seed", 0)
        )
        if self.random_seed is not None:
            getattr(_FMN_ROOT, "random").seed(self.random_seed)
            _np.random.seed(self.random_seed)
        # Reference Scene.__init__ constructs the camera and puts its frame
        # first in the update list immediately.  The portal keeps that frame
        # out of the drawable Stage, but it must still exist before the first
        # update crossing so initialization never contaminates frame work.
        self.frame = CameraFrame()
        self.num_plays = 0
        self.undo_stack = []

    @property
    def frame(self):
        # Lazily-created per-scene camera frame, the same object scenes
        # reach through `self.camera.frame` (the Reference wires
        # `self.frame = self.camera.frame` in Scene.__init__).
        frame = self.__dict__.get("_camera_frame")
        if frame is None:
            frame = CameraFrame()
            self.__dict__["_camera_frame"] = frame
        return frame

    @frame.setter
    def frame(self, value):
        self.__dict__["_camera_frame"] = value
        camera = self.__dict__.get("_camera")
        if camera is not None:
            camera.frame = value

    @property
    def camera(self):
        # Scene and Camera share one live CameraFrame identity, matching the
        # Reference while capture configuration is built by Lumen.
        camera = self.__dict__.get("_camera")
        if camera is None:
            camera = Camera()
            camera.frame = self.frame
            self.__dict__["_camera"] = camera
        return camera

    def setup(self):
        pass

    def construct(self):
        pass

    def tear_down(self):
        pass

    @property
    def mobjects(self):
        return self._engine_roots()

    def get_time(self):
        return self.time()

    def get_top_level_mobjects(self):
        mobjects = self.get_mobjects()
        families = [mobject.get_family() for mobject in mobjects]
        return [
            mobject
            for mobject in mobjects
            if sum(mobject in family for family in families) == 1
        ]

    def get_mobject_family_members(self):
        return [
            member
            for mobject in self.mobjects
            for member in mobject.get_family()
        ]

    def add_mobjects_among(self, values):
        self.add(*(value for value in values if isinstance(value, Mobject)))
        return self

    def get_mobject_copies(self):
        return [mobject.copy() for mobject in self.mobjects]

    def run(self):
        return self._run_lifecycle()

    render = run

    def play(self, *proto_animations, run_time=None, rate_func=None, lag_ratio=None):
        # Mobject.animate builders and explicit native Animation classes drive
        # the engine's six-step contract. When live Python mobject updaters are
        # present, the Rust bridge uses Choreo's step-4 release window: no
        # Scene/Stage borrow survives the Python callback phase.
        if not proto_animations:
            return
        def rate_payload(value, where, run_time_hint):
            if value is None or isinstance(value, str):
                return value
            name = _RATE_FUNC_NAMES.get(value)
            if name is not None:
                return name
            if not callable(value):
                raise TypeError(
                    where + ": rate_func must be a callable or a catalog name"
                )
            # A custom callable (pure by the rate-func contract): pre-sample
            # it on the segment's frame grid BEFORE the segment runs, so it
            # evaluates natively with zero mid-segment interpreter
            # crossings — exact at every unlagged capture boundary; lagged
            # or time-spanned members see the linear interpolant between
            # grid points.
            frames = max(2, int(round(float(run_time_hint) * 30.0)))
            return [float(value(k / frames)) for k in range(frames + 1)]

        def build_spec(proto, nested):
            if isinstance(proto, _AnimationBuilder):
                if proto.overridden_animation is not None:
                    return build_spec(proto.build(), nested)
                spec_args = {}
                spec_params = {}
                for key, value in proto.anim_args.items():
                    if key in ("run_time", "rate_func", "lag_ratio"):
                        spec_args[key] = value
                    elif key == "path_arc":
                        # fm-5wq.4.99: the builder's Transform rides the
                        # native path_arc surface, same as Transform(...,
                        # path_arc=...).
                        spec_params["path_arc"] = float(value)
                    elif key == "path_arc_axis":
                        spec_params["path_arc_axis"] = _vec3(value)
                    else:
                        raise NotImplementedError(
                            "anim arg `" + key + "` is not yet routed to "
                            "the engine play"
                        )
                mobject = proto.mobject
                if isinstance(mobject, CameraFrame):
                    raise NotImplementedError(
                        "the camera-frame lerp cannot nest inside a "
                        "composition; play frame.animate at the top level"
                    )
                if not mobject._is_bound():
                    self.add(mobject)
                target = mobject.target
                if not target._is_bound():
                    self._adopt(target)
                return (
                    "transform",
                    mobject,
                    target,
                    spec_args.get("run_time"),
                    rate_payload(
                        spec_args.get("rate_func"),
                        "animate",
                        spec_args.get("run_time") or run_time or 1.0,
                    ),
                    spec_args.get("lag_ratio"),
                    spec_params,
                )
            if isinstance(proto, Animation) and getattr(proto, "_native_kind", None):
                params = dict(proto._native_params())
                params["suspend_mobject_updating"] = bool(
                    proto.suspend_mobject_updating
                )
                if proto.time_span is not None:
                    params["time_span"] = proto.time_span
                if isinstance(proto, AnimationGroup):
                    params["members"] = [
                        build_spec(member, True) for member in proto.animations
                    ]
                    params["lag_ratio"] = float(
                        type(proto)._default_lag_ratio
                        if proto.lag_ratio is None
                        else proto.lag_ratio
                    )
                    return (
                        proto._native_kind,
                        None,
                        None,
                        None if proto.run_time is None else float(proto.run_time),
                        rate_payload(
                            proto.rate_func,
                            type(proto).__name__,
                            proto.run_time or run_time or 1.0,
                        ),
                        None,
                        params,
                    )
                mobject = proto.mobject
                if isinstance(mobject, CameraFrame):
                    raise NotImplementedError(
                        "explicit animations of the camera frame await the "
                        "camera track; use frame.animate"
                    )
                if not mobject._is_bound():
                    self.add(mobject)
                if proto._native_kind == "restore":
                    saved_state = getattr(mobject, "saved_state", None)
                    if saved_state is not None:
                        if not saved_state._is_bound():
                            self._adopt(saved_state)
                        mobject._link_saved_state(saved_state)
                target = proto._native_target()
                if target is not None and not target._is_bound():
                    self._adopt(target)
                for extra in getattr(proto, "_native_extra_mobjects", ()):
                    if not extra._is_bound():
                        self.add(extra)
                return (
                    proto._native_kind,
                    mobject,
                    target,
                    None if proto.run_time is None else float(proto.run_time),
                    rate_payload(
                        proto.rate_func,
                        type(proto).__name__,
                        proto.run_time or run_time or 1.0,
                    ),
                    None if proto.lag_ratio is None else float(proto.lag_ratio),
                    params,
                )
            if isinstance(proto, Animation):
                # fm-5wq.4.88: nested Python-driven members build the same
                # python_callback placeholder slot as top-level ones — the
                # enclosing composition's _CompositionCallbackDriver runs
                # them at the mirrored sub-alphas in the release window.
                mobject = proto.mobject
                if not isinstance(mobject, _BridgeMobject):
                    raise TypeError(
                        type(proto).__name__ + " must animate a Mobject"
                    )
                if not mobject._is_bound():
                    self.add(mobject)
                params = {"remover": bool(getattr(proto, "remover", False))}
                if getattr(proto, "time_span", None) is not None:
                    params["time_span"] = proto.time_span
                return (
                    "python_callback",
                    mobject,
                    None,
                    float(proto.run_time),
                    None,
                    float(getattr(proto, "lag_ratio", 0.0)),
                    params,
                )
            raise NotImplementedError(
                ("a composition member" if nested else "Scene.play")
                + " accepts mobject.animate builders and the bound "
                "Animation classes; got " + type(proto).__name__
            )

        specs = []
        callbacks = []
        camera_pair = None
        for proto in proto_animations:
            if isinstance(proto, _AnimationBuilder) and isinstance(
                proto.mobject, CameraFrame
            ):
                # Cut T3: the camera lerp rides the same segment; its
                # state lives in the engine camera core, not records.
                #
                # fm-5wq.4.93: several camera-frame builders in one play
                # merge exactly the Reference's way. Same-mobject
                # transforms interpolate in play order, so the later one
                # overwrites the earlier every frame — and each builder's
                # generate_target starts from the live frame, so the last
                # builder's target already IS the play-order outcome. One
                # engine lerp to that target reproduces it; no second
                # camera clock exists or is needed.
                camera_pair = (proto.mobject._core, proto.mobject.target._core)
                continue
            if isinstance(proto, Animation) and not getattr(
                proto, "_native_kind", None
            ):
                if run_time is not None:
                    proto.run_time = float(run_time)
                if rate_func is not None:
                    proto.rate_func = rate_func
                if lag_ratio is not None:
                    proto.lag_ratio = float(lag_ratio)
                callbacks.append(proto)
            elif isinstance(proto, AnimationGroup) and _python_composition_members(
                proto
            ):
                # fm-5wq.4.88: a composition carrying Python-driven leaves
                # gets one driver callback in the same release window.
                callbacks.append(_CompositionCallbackDriver(proto))
            else:
                callbacks.append(None)
            specs.append(build_spec(proto, False))
        return self._play_animations(
            specs,
            callbacks,
            camera_pair,
            None if run_time is None else float(run_time),
            rate_payload(rate_func, "play", run_time or 1.0),
            None if lag_ratio is None else float(lag_ratio),
        )

    def wait(self, duration=None, **kwargs):
        stop_condition = kwargs.pop("stop_condition", None)
        ignore_presenter_mode = bool(kwargs.pop("ignore_presenter_mode", False))
        if kwargs:
            raise NotImplementedError(
                "Scene.wait unsupported keyword(s): "
                + ", ".join(sorted(kwargs))
            )
        if stop_condition is not None and not callable(stop_condition):
            raise TypeError(
                "Scene.wait stop_condition must be callable or None; got "
                + type(stop_condition).__name__
            )
        self._wait(
            None if duration is None else float(duration),
            stop_condition,
            ignore_presenter_mode,
        )

    def wait_until(self, stop_condition, max_time=60):
        self.wait(max_time, stop_condition=stop_condition)

    def add_sound(
        self,
        sound_file,
        time_offset=0,
        gain=None,
        gain_to_background=None,
    ):
        # Native Scene owns both exact call-site time and validation. Keep the
        # relative float offset separate so Reel performs the only conversion
        # from the rational frame grid to the output sample grid (BN-14).
        self._add_sound(
            sound_file,
            float(time_offset),
            None if gain is None else float(gain),
            None
            if gain_to_background is None
            else float(gain_to_background),
        )

    @staticmethod
    def _dispatch_updater_batch(mobjects, dt):
        # fm-zoi rung 1: one native→Python crossing per frame. Iterates the
        # engine's target list in order and snapshots each mobject's updater
        # list at that mobject's turn — identical ordering and observable
        # state to rung 0's per-updater dispatch.
        for mobject in mobjects:
            for updater in list(mobject.updaters):
                mobject._dispatch_updater(updater, dt)

    def get_state(self):
        return SceneState(self)

    def restore_state(self, scene_state):
        if not isinstance(scene_state, SceneState):
            raise TypeError("restore_state expects a SceneState")
        scene_state.restore_scene(self)
        return self


def _mobject_looks_identical(mobject, other):
    if type(mobject) is not type(other):
        return False
    count = mobject.get_num_points()
    if count != other.get_num_points():
        return False
    if count == 0:
        return True
    return bool(_np.allclose(mobject.get_points(), other.get_points()))


class SceneState:
    """Reference identities over Proscenium's canonical native checkpoint."""

    def __init__(self, scene, ignore=None):
        if not isinstance(scene, Scene):
            raise TypeError("SceneState scene must be a Scene")
        self._scene = scene
        self.time = scene.get_time()
        self.num_plays = int(getattr(scene, "num_plays", 0))
        skip = set() if ignore is None else set(ignore)
        last = {}
        if getattr(scene, "undo_stack", None):
            last = scene.undo_stack[-1].mobjects_to_copies
        copies = {}
        for mobject in list(scene.mobjects):
            if mobject in skip:
                continue
            prior = last.get(mobject)
            if prior is not None and _mobject_looks_identical(mobject, prior):
                copies[mobject] = prior
            else:
                copies[mobject] = mobject.copy()
        self.mobjects_to_copies = copies
        # Bound mobject.copy() allocates the copied family in the native Stage.
        # Capture only after that identity map is complete so the checkpoint
        # is byte-identical to the next read of an otherwise unchanged scene.
        # An explicitly empty ignore collection still captures native state.
        self._checkpoint = None if skip else bytes(scene._checkpoint_bytes())

    def mobjects_match(self, state):
        if not isinstance(state, SceneState):
            raise TypeError("mobjects_match expects a SceneState")
        return self.mobjects_to_copies == state.mobjects_to_copies

    def n_changes(self, state):
        if not isinstance(state, SceneState):
            raise TypeError("n_changes expects a SceneState")
        other = state.mobjects_to_copies
        return sum(
            1
            - int(
                mobject in other
                and _mobject_looks_identical(mobject, other[mobject])
            )
            for mobject in self.mobjects_to_copies
        )

    def restore_scene(self, scene):
        if not isinstance(scene, Scene):
            raise TypeError("restore_scene expects a Scene")
        if scene is self._scene and self._checkpoint is not None:
            scene._restore_checkpoint_bytes(self._checkpoint)
            scene._reseat_engine_roots(*self.mobjects_to_copies)
            scene.num_plays = self.num_plays
            return
        restored = [
            mobject.become(copy, match_updaters=True)
            for mobject, copy in self.mobjects_to_copies.items()
        ]
        scene.clear()
        if restored:
            scene.add(*restored)
        scene.num_plays = self.num_plays


class ThreeDScene(Scene):
    """Scene with the Reference three-d camera default and add-time depth/stroke."""

    samples = 4
    default_frame_orientation = (-30, 70)
    always_depth_test = True

    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.frame.reorient(*self.default_frame_orientation)
        self.frame.make_orientation_default()

    @property
    def camera(self):
        camera = self.__dict__.get("_camera")
        if camera is None:
            camera = Camera(samples=int(self.samples))
            camera.frame = self.frame
            self.__dict__["_camera"] = camera
        return camera

    def add(self, *mobjects, set_depth_test=True, perp_stroke=True):
        for mobject in mobjects:
            if (
                set_depth_test
                and not mobject.is_fixed_in_frame()
                and self.always_depth_test
            ):
                mobject.apply_depth_test()
            if isinstance(mobject, VMobject) and mobject.has_stroke() and perp_stroke:
                mobject.set_flat_stroke(False)
        return super().add(*mobjects)


class InteractiveScene(Scene):
    def embed(self, namespace=None):
        return _portal_embed(self, namespace)

    def checkpoint_paste(self):
        return _portal_checkpoint_paste(self)


class BlankScene(InteractiveScene):
    """extract_scene's empty InteractiveScene; construct enters embed."""

    def construct(self):
        self.embed()


class ModuleLoader:
    """In-process scene-file loader. Never locates or spawns a second CPython."""

    @staticmethod
    def get_module(file_name, is_during_reload=False):
        if file_name is None:
            return None
        path = _pathlib.Path(file_name).expanduser()
        if not path.is_file():
            raise FileNotFoundError(
                f"ModuleLoader cannot read {str(file_name)!r}"
            )
        resolved = path.resolve()
        module_name = "manimlib_loaded_" + _re.sub(
            r"[^0-9A-Za-z_]", "_", str(resolved)
        )
        if is_during_reload:
            _sys.modules.pop(module_name, None)
        elif module_name in _sys.modules:
            return _sys.modules[module_name]
        util = _importlib.import_module("importlib.util")
        spec = util.spec_from_file_location(module_name, resolved)
        if spec is None or spec.loader is None:
            raise ImportError(f"ModuleLoader cannot load {str(file_name)!r}")
        module = util.module_from_spec(spec)
        _sys.modules[module_name] = module
        spec.loader.exec_module(module)
        return module


def get_indent(code_lines, line_number):
    line = code_lines[int(line_number)]
    return line[: len(line) - len(line.lstrip())]


# BlankScene and ModuleLoader are not manimlib root exports, so the
# post-install namespace cleanup deletes those names from these
# functions' globals.
_BLANK_SCENE = BlankScene
_MODULE_LOADER = ModuleLoader


def is_child_scene(obj, module):
    if module is None or not _inspect.isclass(obj):
        return False
    try:
        if not issubclass(obj, Scene):
            return False
    except TypeError:
        return False
    if obj in (Scene, InteractiveScene, ThreeDScene, _BLANK_SCENE):
        return False
    module_name = getattr(module, "__name__", "")
    if not module_name:
        return False
    return str(getattr(obj, "__module__", "")).startswith(module_name)


def get_scene_classes(module):
    if module is None:
        return []
    return [
        member
        for member in vars(module).values()
        if _is_child_scene(member, module)
    ]


def get_module(run_config):
    if not isinstance(run_config, dict):
        raise TypeError("get_module run_config must be a dict")
    file_name = run_config.get("file_name")
    if file_name is None:
        file_name = run_config.get("module")
    return _MODULE_LOADER.get_module(
        file_name,
        is_during_reload=bool(run_config.get("is_during_reload", False)),
    )


_is_child_scene = is_child_scene


def scene_from_class(scene_class, scene_config, run_config):
    if not _inspect.isclass(scene_class):
        raise TypeError("scene_from_class scene_class must be a Scene subclass")
    try:
        if not issubclass(scene_class, Scene):
            raise TypeError(
                "scene_from_class scene_class must be a Scene subclass"
            )
    except TypeError as error:
        raise TypeError(
            "scene_from_class scene_class must be a Scene subclass"
        ) from error
    if not isinstance(scene_config, dict):
        raise TypeError("scene_from_class scene_config must be a dict")
    if not isinstance(run_config, dict):
        raise TypeError("scene_from_class run_config must be a dict")
    del run_config
    return scene_class(**dict(scene_config))


def note_missing_scenes(arg_names, module_names):
    missing = ", ".join(str(name) for name in arg_names)
    available = ", ".join(str(name) for name in module_names)
    raise ValueError(f"No scenes named {missing}; available: {available}")


def prompt_user_for_choice(scene_classes):
    del scene_classes
    raise _CapabilityError(
        "prompt_user_for_choice is unavailable; pass scene_names or write_all"
    )


def get_scenes_to_render(all_scene_classes, scene_config, run_config):
    if not isinstance(all_scene_classes, (list, tuple)):
        raise TypeError("get_scenes_to_render all_scene_classes must be a list")
    if not isinstance(scene_config, dict):
        raise TypeError("get_scenes_to_render scene_config must be a dict")
    if not isinstance(run_config, dict):
        raise TypeError("get_scenes_to_render run_config must be a dict")
    classes = list(all_scene_classes)
    if bool(run_config.get("write_all", False)):
        return [
            _scene_from_class(cls, scene_config, run_config) for cls in classes
        ]
    names = [str(name) for name in (run_config.get("scene_names") or [])]
    if names:
        by_name = {cls.__name__: cls for cls in classes}
        missing = [name for name in names if name not in by_name]
        if missing:
            _note_missing_scenes(missing, [cls.__name__ for cls in classes])
        return [
            _scene_from_class(by_name[name], scene_config, run_config)
            for name in names
        ]
    if len(classes) == 1:
        return [_scene_from_class(classes[0], scene_config, run_config)]
    if not classes:
        return []
    raise _CapabilityError(
        "get_scenes_to_render needs scene_names or write_all; "
        "interactive prompt_user_for_choice is unavailable"
    )


_scene_from_class = scene_from_class
_note_missing_scenes = note_missing_scenes
_get_indent = get_indent


def insert_embed_line_to_module(module, run_config):
    if not isinstance(run_config, dict):
        raise TypeError("insert_embed_line_to_module run_config must be a dict")
    embed_line = run_config.get("embed_line")
    if embed_line is None:
        return module
    file_name = getattr(module, "__file__", None)
    if not file_name:
        raise TypeError("insert_embed_line_to_module module must have __file__")
    path = _pathlib.Path(file_name)
    lines = path.read_text(encoding="utf-8").splitlines(keepends=True)
    index = int(embed_line) - 1
    if index < 0 or index > len(lines):
        raise ValueError(
            f"insert_embed_line_to_module embed_line {embed_line} is out of range"
        )
    indent_source = lines[index] if index < len(lines) else ""
    indent = _get_indent([indent_source], 0)
    lines.insert(index, indent + "self.embed()\n")
    path.write_text("".join(lines), encoding="utf-8")
    return _MODULE_LOADER.get_module(str(path), is_during_reload=True)


def compute_total_frames(scene_class, scene_config):
    if not isinstance(scene_config, dict):
        raise TypeError("compute_total_frames scene_config must be a dict")
    kwargs = dict(scene_config)
    kwargs["skip_animations"] = True
    scene = _scene_from_class(scene_class, kwargs, {})
    scene.run()
    fps = 30.0
    camera = getattr(scene, "camera", None)
    if camera is not None:
        fps = float(getattr(camera, "frame_rate", fps) or fps)
    return int(scene.get_time() * fps)


_get_module = get_module
_get_scene_classes = get_scene_classes
_insert_embed_line_to_module = insert_embed_line_to_module
_get_scenes_to_render = get_scenes_to_render
_compute_total_frames = compute_total_frames


def main(scene_config, run_config):
    if not isinstance(scene_config, dict):
        raise TypeError("main scene_config must be a dict")
    if not isinstance(run_config, dict):
        raise TypeError("main run_config must be a dict")
    module = _get_module(run_config)
    if module is None:
        raise ValueError("main run_config needs file_name")
    module = _insert_embed_line_to_module(module, run_config)
    classes = _get_scene_classes(module)
    scenes = _get_scenes_to_render(classes, scene_config, run_config)
    for scene in scenes:
        scene.run()
    return scenes


class CheckpointManager:
    """Named SceneState snapshots for interactive re-run of a comment-keyed block."""

    def __init__(self):
        self.checkpoint_states = {}

    def checkpoint_paste(self, shell, scene):
        try:
            pyperclip = _importlib.import_module("pyperclip")
        except ImportError as error:
            raise _CapabilityError(
                "pyperclip is not installed; CheckpointManager.checkpoint_paste "
                "needs a clipboard, or call handle_checkpoint_key directly"
            ) from error
        code_string = _textwrap.dedent(
            "\n".join(line.rstrip() for line in str(pyperclip.paste()).splitlines())
        )
        self.handle_checkpoint_key(scene, self.get_leading_comment(code_string))
        if shell is not None:
            shell.run_cell(code_string)

    @staticmethod
    def get_leading_comment(code_string):
        leading_line = str(code_string).partition("\n")[0].lstrip()
        if leading_line.startswith("#"):
            return leading_line
        return ""

    def handle_checkpoint_key(self, scene, key):
        if not key:
            return
        if key in self.checkpoint_states:
            scene.restore_state(self.checkpoint_states[key])
            all_keys = list(self.checkpoint_states.keys())
            index = all_keys.index(key)
            for later_key in all_keys[index + 1 :]:
                self.checkpoint_states.pop(later_key)
        else:
            self.checkpoint_states[key] = scene.get_state()

    def clear_checkpoints(self):
        self.checkpoint_states = {}


class InteractiveSceneEmbed:
    """IPython embed surface over Scene + CheckpointManager.

    Construction is headless. Launching the shell and GUI hooks refuse
    with CapabilityError when IPython or a pyglet window is absent;
    Studio owns interactive windows.
    """

    def __init__(self, scene):
        if not isinstance(scene, Scene):
            raise TypeError("InteractiveSceneEmbed scene must be a Scene")
        self.scene = scene
        self.checkpoint_manager = CheckpointManager()
        self.shell = None

    def launch(self):
        self.shell = self.get_ipython_shell_for_embedded_scene()
        return self.shell

    def get_ipython_shell_for_embedded_scene(self):
        return _portal_embed(self.scene)

    def get_shortcuts(self):
        scene = self.scene
        shortcuts = {
            "play": scene.play,
            "wait": scene.wait,
            "add": scene.add,
            "remove": scene.remove,
            "remove_all_except": scene.remove_all_except,
            "clear": scene.clear,
            "checkpoint_paste": self.checkpoint_paste,
            "clear_checkpoints": self.checkpoint_manager.clear_checkpoints,
        }
        for name in (
            "focus",
            "save_state",
            "undo",
            "redo",
            "i2g",
            "i2m",
        ):
            if hasattr(scene, name):
                shortcuts[name] = getattr(scene, name)
        return shortcuts

    def checkpoint_paste(self, skip=False, record=False, progress_bar=True):
        del skip, record, progress_bar
        return self.checkpoint_manager.checkpoint_paste(self.shell, self.scene)

    def enable_gui(self):
        raise _CapabilityError(
            "the Reference IPython GUI hook is unavailable; "
            "FrankenManim Studio owns interactive windows"
        )

    def ensure_frame_update_post_cell(self):
        raise _CapabilityError(
            "the Reference IPython GUI hook is unavailable; "
            "FrankenManim Studio owns interactive windows"
        )

    def ensure_flash_on_error(self):
        raise _CapabilityError(
            "the Reference IPython GUI hook is unavailable; "
            "FrankenManim Studio owns interactive windows"
        )

    def auto_reload(self):
        raise _CapabilityError(
            "auto_reload requires the manimgl IPython embed loop"
        )

    def reload_scene(self, embed_line=None):
        del embed_line
        raise _CapabilityError(
            "reload_scene requires the manimgl IPython embed loop"
        )

    def validate_syntax(self, file_path):
        try:
            source = _pathlib.Path(file_path).read_text(encoding="utf-8")
            compile(source, file_path, "exec")
        except (OSError, SyntaxError, UnicodeError):
            return False
        return True


class Animation:
    def __init__(
        self,
        mobject=None,
        run_time=1.0,
        rate_func=None,
        time_span=None,
        lag_ratio=0.0,
        name="",
        remover=False,
        final_alpha_value=1.0,
        suspend_mobject_updating=False,
        **kwargs,
    ):
        self.mobject = mobject
        self.run_time = float(run_time)
        self.rate_func = (
            rate_func
            if rate_func is not None
            else getattr(_FMN_ROOT, "smooth", lambda alpha: alpha)
        )
        self.time_span = time_span
        self.lag_ratio = float(lag_ratio)
        self.name = name or type(self).__name__
        self.remover = bool(remover)
        self.final_alpha_value = float(final_alpha_value)
        self.suspend_mobject_updating = bool(suspend_mobject_updating)
        self.__dict__.update(kwargs)

    def begin(self):
        if self.time_span is not None:
            self.run_time = max(float(self.time_span[1]), self.run_time)
        self.interpolate(0.0)

    def finish(self):
        self.interpolate(self.final_alpha_value)

    def interpolate(self, alpha):
        self.interpolate_mobject(float(alpha))
        return self

    def time_spanned_alpha(self, alpha):
        if self.time_span is None:
            return float(alpha)
        start, end = self.time_span
        return _np.clip(float(alpha) * self.run_time - start, 0.0, end - start) / (
            end - start
        )

    def interpolate_mobject(self, alpha):
        del alpha

    def copy(self):
        return _copy.copy(self)


class UpdateFromFunc(Animation):
    def __init__(
        self,
        mobject,
        update_function,
        suspend_mobject_updating=False,
        **kwargs,
    ):
        self.update_function = update_function
        super().__init__(
            mobject,
            suspend_mobject_updating=suspend_mobject_updating,
            **kwargs,
        )

    def interpolate_mobject(self, alpha):
        del alpha
        self.update_function(self.mobject)


class UpdateFromAlphaFunc(Animation):
    def __init__(
        self,
        mobject,
        update_function,
        suspend_mobject_updating=False,
        **kwargs,
    ):
        self.update_function = update_function
        super().__init__(
            mobject,
            suspend_mobject_updating=suspend_mobject_updating,
            **kwargs,
        )

    def interpolate_mobject(self, alpha):
        true_alpha = self.rate_func(self.time_spanned_alpha(alpha))
        self.update_function(self.mobject, true_alpha)


class AnimatedBoundary(VGroup):
    """mobject/changing.py:18 (fm-5wq.4.67): two stroke copies of the
    outlined VMobject cycle a growing/fading boundary through the live
    updater seam — verbatim Reference mechanics over the portal's
    `pointwise_become_partial` and stroke surfaces."""

    def __init__(
        self,
        vmobject,
        colors=None,
        max_stroke_width=3.0,
        cycle_rate=0.5,
        back_and_forth=True,
        draw_rate_func=_smooth_rate,
        fade_rate_func=_smooth_rate,
        **kwargs,
    ):
        if not isinstance(vmobject, VMobject):
            raise TypeError(
                "AnimatedBoundary requires a VMobject to outline; got "
                + type(vmobject).__name__
            )
        super().__init__(**kwargs)
        self.vmobject = vmobject
        # The Reference's default cycle: BLUE_D, BLUE_B, BLUE_E, GREY_BROWN.
        self.colors = (
            list(colors)
            if colors is not None
            else [_BLUE_D, "#9CDCEB", _BLUE_E, "#736357"]
        )
        self.max_stroke_width = max_stroke_width
        self.cycle_rate = cycle_rate
        self.back_and_forth = back_and_forth
        self.draw_rate_func = draw_rate_func
        self.fade_rate_func = fade_rate_func
        self.boundary_copies = [
            vmobject.copy().set_style(stroke_width=0, fill_opacity=0)
            for _ in range(2)
        ]
        self.add(*self.boundary_copies)
        self.total_time = 0.0
        self.add_updater(lambda m, dt: self.update_boundary_copies(dt))

    def update_boundary_copies(self, dt):
        # changing.py:52, verbatim: an altered-rate clock drives the
        # grow/fade pair through the color cycle.
        time = self.total_time * self.cycle_rate
        growing, fading = self.boundary_copies
        colors = self.colors
        msw = self.max_stroke_width
        vmobject = self.vmobject

        index = int(time % len(colors))
        alpha = time % 1
        draw_alpha = self.draw_rate_func(alpha)
        fade_alpha = self.fade_rate_func(alpha)

        if self.back_and_forth and int(time) % 2 == 1:
            bounds = (1 - draw_alpha, 1)
        else:
            bounds = (0, draw_alpha)
        self.full_family_become_partial(growing, vmobject, *bounds)
        growing.set_stroke(colors[index], width=msw)

        if time >= 1:
            self.full_family_become_partial(fading, vmobject, 0, 1)
            fading.set_stroke(
                color=colors[index - 1], width=(1 - fade_alpha) * msw
            )

        self.total_time += dt
        return self

    def full_family_become_partial(self, mob1, mob2, a, b):
        family1 = mob1.family_members_with_points()
        family2 = mob2.family_members_with_points()
        for sm1, sm2 in zip(family1, family2):
            sm1.pointwise_become_partial(sm2, a, b)
        return self


class TracedPath(VMobject):
    """mobject/changing.py:98 (fm-5wq.4.67): a live path grown from a
    traced-point callable every frame, verbatim over the portal's
    smooth-points and stroke surfaces. A non-callable trace source is a
    named refusal instead of the Reference's mid-play crash."""

    def __init__(
        self,
        traced_point_func,
        time_traced=None,
        time_per_anchor=1.0 / 15,
        stroke_color=None,
        stroke_width=2.0,
        stroke_opacity=1.0,
        **kwargs,
    ):
        if not callable(traced_point_func):
            raise TypeError(
                "TracedPath requires a callable returning the traced "
                "point; got " + type(traced_point_func).__name__
            )
        self.stroke_config = dict(
            color=stroke_color if stroke_color is not None else "#FFFFFF",
            width=stroke_width,
            opacity=stroke_opacity,
        )
        super().__init__(**kwargs)
        self.traced_point_func = traced_point_func
        self.time_traced = (
            float(time_traced) if time_traced is not None else _np.inf
        )
        self.time_per_anchor = time_per_anchor
        self.time = 0.0
        self.traced_points = []
        self.add_updater(lambda m, dt: m.update_path(dt))

    def update_path(self, dt):
        # changing.py:122, verbatim (including the finite-window resample
        # and the every-now-and-then list refresh).
        if dt == 0:
            return self
        point = _np.array(self.traced_point_func(), dtype=float).copy()
        self.traced_points.append(point)

        if self.time_traced < _np.inf:
            n_relevant_points = int(self.time_traced / dt + 0.5)
            n_tps = len(self.traced_points)
            if n_tps < n_relevant_points:
                points = self.traced_points + [point] * (
                    n_relevant_points - n_tps
                )
            else:
                points = self.traced_points[n_tps - n_relevant_points:]
            if n_tps > 10 * n_relevant_points:
                self.traced_points = self.traced_points[-n_relevant_points:]
        else:
            points = self.traced_points

        if points:
            self.set_points_smoothly(points)

        self.set_stroke(**self.stroke_config)

        self.time += dt
        return self


class ChangeSpeed(Animation):
    """ManimCE's `animation/speed.py` time remap — not a pinned-Reference
    class. The native seam it needs (a Choreo child-segment clock remap
    driven by a piecewise `speedinfo` curve, including updater `dt`
    scaling) has not landed, so construction refuses by name instead of
    playing the wrapped animation at the wrong speed (fm-5wq.4.62)."""

    def __init__(self, anim=None, speedinfo=None, rate_func=None, **kwargs):
        del anim, speedinfo, rate_func, kwargs
        raise NotImplementedError(
            "ChangeSpeed requires Choreo's child-segment clock-remap seam "
            "(piecewise speedinfo over a wrapped animation, with updater "
            "dt scaling); that seam has not landed"
        )


class Delay(Animation):
    """A timed hold (fm-5wq.4.66): `run_time` seconds pass on the engine
    clock and nothing mutates. The held mobject is an internal empty
    `Mobject`, so no user state is ever touched. Not a pinned-Reference
    class (specialized.py has no Delay) — compatibility surface for
    Succession/LaggedStart galleries under the wider ecosystem's spelling.
    """

    def __init__(self, run_time=1.0, **kwargs):
        run_time = float(run_time)
        if not (run_time >= 0.0 and _math.isfinite(run_time)):
            raise ValueError(
                "Delay run_time must be a finite non-negative duration; "
                "got " + repr(run_time)
            )
        super().__init__(Mobject(), run_time=run_time, **kwargs)

    def interpolate_mobject(self, alpha):
        del alpha  # a hold mutates nothing


class ChangingDecimal(Animation):
    """The Reference's update mechanism pointed at `DecimalNumber.set_value`
    (fm-5wq.4.55, animation/numbers.py): each frame feeds the time-spanned
    alpha to the update callable and rebuilds the displayed number natively.

    The Reference's `interpolate_mobject` bypasses `get_sub_alpha`, so
    `rate_func` deliberately does not shape the value track — kept exactly.
    """

    def __init__(
        self,
        decimal_mob,
        number_update_func,
        suspend_mobject_updating=False,
        **kwargs,
    ):
        # The Reference's exact refusal for a non-DecimalNumber target.
        assert isinstance(decimal_mob, DecimalNumber)
        self.number_update_func = number_update_func
        super().__init__(
            decimal_mob,
            suspend_mobject_updating=suspend_mobject_updating,
            **kwargs,
        )
        self.mobject = decimal_mob

    def interpolate_mobject(self, alpha):
        true_alpha = self.time_spanned_alpha(float(alpha))
        self.mobject.set_value(self.number_update_func(true_alpha))


class ChangeDecimalToValue(ChangingDecimal):
    """Linear track from the current displayed number to `target_number`."""

    def __init__(self, decimal_mob, target_number, **kwargs):
        start_number = decimal_mob.number
        super().__init__(
            decimal_mob,
            lambda a: start_number + (target_number - start_number) * a,
            **kwargs,
        )


class CountInFrom(ChangingDecimal):
    """Count from `source_number` up to the number already displayed."""

    def __init__(self, decimal_mob, source_number=0, **kwargs):
        start_number = decimal_mob.get_value()
        super().__init__(
            decimal_mob,
            lambda a: source_number
            + (start_number - source_number) * min(max(a, 0), 1),
            **kwargs,
        )


def _string_word_groups(string_mobject):
    """`StringMobject.build_groups()`' word boundaries from native span maps
    (fm-5wq.4.54): partition the glyph ordinals, in order, into runs that
    share a whitespace-delimited word of the source string. A glyph whose
    span starts outside every word (an isolated command glyph) stays with
    the current run, so the runs always partition the ordinals."""
    spans = getattr(string_mobject, "_string_sub_spans", None)
    string = getattr(string_mobject, "string", None)
    if not spans or string is None:
        return []
    word_bytes = [
        string_mobject._byte_span(match.span())
        for match in _re.finditer(r"\S+", string)
    ]
    groups = []
    current = []
    current_word = -1
    for ordinal, (start, _end) in enumerate(spans):
        word = next(
            (
                index
                for index, (word_start, word_end) in enumerate(word_bytes)
                if word_start <= start < word_end
            ),
            current_word,
        )
        if word != current_word and current:
            groups.append(current)
            current = []
        current_word = word
        current.append(ordinal)
    if current:
        groups.append(current)
    return groups


def _string_glyph_reveal_plan(string_mobject):
    """fm-5wq.4.82: how the first N glyphs of a nested StringMobject
    reveal. Two-level families (multi-part Tex: sub-paths ``[part,
    glyph]``) reveal parts progressively — fully passed parts whole, the
    boundary part sliced to its own glyph prefix — through the same live
    `set_submobjects` seam the flat reveal uses. Parts out of reading
    order or nesting deeper than one part level refuse by name."""
    paths = [list(path) for path in string_mobject._string_sub_paths]
    if not all(len(path) == 2 for path in paths):
        raise NotImplementedError(
            "glyph families nested deeper than one part level await a "
            "general flattening seam"
        )
    parts = list(string_mobject.submobjects)
    part_children = [list(part.submobjects) for part in parts]
    ranges = {}
    last_part = -1
    for ordinal, (part_index, _glyph_index) in enumerate(paths):
        if part_index < last_part:
            raise NotImplementedError(
                "span-map parts out of reading order await a general "
                "flattening seam"
            )
        last_part = part_index
        ranges.setdefault(part_index, [ordinal, ordinal])[1] = ordinal + 1
    return (paths, parts, part_children, ranges)


def _apply_glyph_reveal(string_mobject, plan, count):
    """Show the first `count` glyphs of a two-level family in place; the
    full count re-seats every part's original child list, so the finished
    frame is the untouched original structure."""
    paths, parts, part_children, ranges = plan
    if count >= len(paths):
        for part_index, part in enumerate(parts):
            part.set_submobjects(part_children[part_index])
        string_mobject.set_submobjects(parts)
        return
    shown = []
    for part_index, part in enumerate(parts):
        bounds = ranges.get(part_index)
        if bounds is None:
            continue
        start, end = bounds
        if end <= count:
            part.set_submobjects(part_children[part_index])
            shown.append(part)
        elif start < count:
            # The first hidden glyph's own child index bounds the slice.
            part.set_submobjects(
                part_children[part_index][: paths[count][1]]
            )
            shown.append(part)
    string_mobject.set_submobjects(shown)


class AddTextWordByWord(Animation):
    """The Reference's word-by-word reveal (creation.py:210) over native
    span-map word groups (fm-5wq.4.54): `ShowIncreasingSubsets` semantics —
    rate applied to the raw alpha, banker's rounding, `run_time` derived as
    `time_per_word * n_words` when negative, linear rate — with the word
    boundaries coming from the glyph children's retained source spans
    instead of the Reference's second labelled-SVG render. The native
    constructor is fmn-anim's `add_text_word_by_word`; this skin drives the
    same prefix reveal through the live `set_submobjects` seam."""

    def __init__(
        self,
        string_mobject,
        time_per_word=0.2,
        run_time=-1.0,
        rate_func=None,
        **kwargs,
    ):
        # The Reference's exact refusal for a non-StringMobject target.
        assert isinstance(string_mobject, StringMobject)
        groups = _string_word_groups(string_mobject)
        if not groups:
            raise ValueError(
                "AddTextWordByWord requires span-map word groups; "
                "the string mobject produced none"
            )
        if any(
            len(path) != 1
            for path in getattr(string_mobject, "_string_sub_paths", [])
        ):
            # fm-5wq.4.82: nested (multi-part) families reveal through
            # the per-part flattening plan; boundaries stay glyph counts.
            self._nested_plan = _string_glyph_reveal_plan(string_mobject)
            self._all_submobs = []
            self._boundaries = [0]
            for group in groups:
                self._boundaries.append(group[-1] + 1)
        else:
            self._nested_plan = None
            self._all_submobs = list(string_mobject.submobjects)
            # Each word group ends at its last glyph ordinal; the final
            # boundary covers trailing non-glyph children (decorations),
            # so the finished frame is the whole family.
            self._boundaries = [0]
            for group in groups[:-1]:
                self._boundaries.append(group[-1] + 1)
            self._boundaries.append(len(self._all_submobs))
        if run_time < 0:
            run_time = time_per_word * len(groups)
        super().__init__(
            string_mobject,
            run_time=run_time,
            rate_func=(
                rate_func
                if rate_func is not None
                else getattr(_FMN_ROOT, "linear", lambda alpha: alpha)
            ),
            **kwargs,
        )
        self.mobject = string_mobject
        self.string_mobject = string_mobject

    def interpolate_mobject(self, alpha):
        # ShowIncreasingSubsets' exact indexing: rate on the raw alpha,
        # np.round's ties-to-even, Python-slice clamping.
        words = len(self._boundaries) - 1
        index = int(
            min(max(_np.round(self.rate_func(float(alpha)) * words), 0), words)
        )
        if self._nested_plan is not None:
            _apply_glyph_reveal(
                self.mobject, self._nested_plan, self._boundaries[index]
            )
            return
        self.mobject.set_submobjects(self._all_submobs[: self._boundaries[index]])


class AddTextLetterByLetter(Animation):
    """The letter-granularity sibling of `AddTextWordByWord`
    (fm-5wq.4.59): every span-map letter group is one non-whitespace glyph
    in reading order, revealed with the same `ShowIncreasingSubsets`
    semantics (linear rate on the raw alpha, banker's rounding, `run_time`
    derived as `time_per_char * n_letters` when negative). The native
    constructor is fmn-anim's `add_text_letter_by_letter`. The pinned
    Reference's creation.py has no letter-granularity class — this is
    compatibility surface for the wider manim ecosystem's spelling, built
    from the same native span maps."""

    def __init__(
        self,
        string_mobject,
        time_per_char=0.1,
        run_time=-1.0,
        rate_func=None,
        **kwargs,
    ):
        # The word sibling's exact refusal shape for a non-StringMobject.
        assert isinstance(string_mobject, StringMobject)
        spans = getattr(string_mobject, "_string_sub_spans", None) or []
        if not spans:
            raise ValueError(
                "AddTextLetterByLetter requires span-map letter groups; "
                "the string mobject produced none"
            )
        letters = len(spans)
        if any(
            len(path) != 1
            for path in getattr(string_mobject, "_string_sub_paths", [])
        ):
            # fm-5wq.4.82: the word sibling's flattening plan covers the
            # letter grain too — boundaries are plain glyph counts.
            self._nested_plan = _string_glyph_reveal_plan(string_mobject)
            self._all_submobs = []
            self._boundaries = list(range(letters + 1))
        else:
            self._nested_plan = None
            self._all_submobs = list(string_mobject.submobjects)
            # One boundary per glyph; the final boundary covers trailing
            # non-glyph children (decorations), so the finished frame is
            # the whole family.
            self._boundaries = list(range(letters)) + [
                len(self._all_submobs)
            ]
        if run_time < 0:
            run_time = time_per_char * letters
        super().__init__(
            string_mobject,
            run_time=run_time,
            rate_func=(
                rate_func
                if rate_func is not None
                else getattr(_FMN_ROOT, "linear", lambda alpha: alpha)
            ),
            **kwargs,
        )
        self.mobject = string_mobject
        self.string_mobject = string_mobject

    def interpolate_mobject(self, alpha):
        # ShowIncreasingSubsets' exact indexing, letter-grained.
        letters = len(self._boundaries) - 1
        index = int(
            min(max(_np.round(self.rate_func(float(alpha)) * letters), 0), letters)
        )
        if self._nested_plan is not None:
            _apply_glyph_reveal(
                self.mobject, self._nested_plan, self._boundaries[index]
            )
            return
        self.mobject.set_submobjects(self._all_submobs[: self._boundaries[index]])


# ---------------------------------------------------------------------------
# Explicit animation classes (fm-d3gt): thin specs over fmn-anim's native
# five-mechanisms shelf. Scene.play builds one native segment animation per
# spec; classes without a 1:1 native mechanism refuse precisely at play.


class _NativeAnimation(Animation):
    _native_kind = None
    _target_attr = None

    def __init__(
        self,
        mobject,
        run_time=None,
        rate_func=None,
        lag_ratio=None,
        time_span=None,
        final_alpha_value=1.0,
        suspend_mobject_updating=False,
        **kwargs,
    ):
        _refuse_unrouted(
            type(self).__name__ + "()", [(name, True) for name in sorted(kwargs)]
        )
        self.mobject = mobject
        self.run_time = run_time
        self.rate_func = rate_func
        self.lag_ratio = lag_ratio
        self.time_span = (
            None if time_span is None else (float(time_span[0]), float(time_span[1]))
        )
        self.final_alpha_value = float(final_alpha_value)
        self.suspend_mobject_updating = bool(suspend_mobject_updating)

    def _native_params(self):
        return {}

    def _native_target(self):
        return getattr(self, self._target_attr) if self._target_attr else None


class ShowPartial(_NativeAnimation, _abc.ABC):
    """The abstract partial-reveal mechanism (creation.py:25): a concrete
    subclass supplies ``get_bounds``.  The two native bounds vocabularies —
    creation's ``(0, alpha)`` (creation.py:52) and indication.py:179's
    sliding passing-flash window — are recognized by sampling the
    subclass's rule; any other rule refuses precisely at play."""

    _native_kind = "show_partial"
    _BOUNDS_PROBES = (0.125, 0.375, 0.625, 0.875)

    def __init__(self, mobject, should_match_start=False, **kwargs):
        if not isinstance(mobject, (VMobject, Surface)):
            raise TypeError(
                type(self).__name__
                + " requires a VMobject or Surface family; "
                + type(mobject).__name__
                + " has no pointwise_become_partial plane"
            )
        super().__init__(mobject, **kwargs)
        # Stored-but-never-read in the pinned Reference (creation.py:30);
        # kept as inert constructor surface, matching the native shelf.
        self.should_match_start = bool(should_match_start)

    @_abc.abstractmethod
    def get_bounds(self, alpha):
        raise NotImplementedError

    def _native_params(self):
        probes = self._BOUNDS_PROBES
        bounds = [
            tuple(float(value) for value in self.get_bounds(alpha))
            for alpha in probes
        ]
        if all(
            abs(lower) <= 1e-12 and abs(upper - alpha) <= 1e-12
            for (lower, upper), alpha in zip(bounds, probes)
        ):
            return {"bounds_kind": "creation"}
        # The sliding window: upper = alpha * (1 + tw), lower = upper - tw,
        # both clipped to [0, 1]; tw solves from the first (uncapped) probe.
        time_width = bounds[0][1] / probes[0] - 1.0
        if time_width > 0.0 and all(
            abs(lower - max(alpha * (1.0 + time_width) - time_width, 0.0))
            <= 1e-9
            and abs(upper - min(alpha * (1.0 + time_width), 1.0)) <= 1e-9
            for (lower, upper), alpha in zip(bounds, probes)
        ):
            return {"bounds_kind": "passing_flash", "time_width": time_width}
        raise NotImplementedError(
            type(self).__name__
            + ".get_bounds is not a native reveal rule (creation or "
            "passing-flash bounds)"
        )


class ShowCreation(ShowPartial):
    _native_kind = "show_creation"

    def __init__(self, mobject, lag_ratio=1.0, **kwargs):
        super().__init__(mobject, lag_ratio=lag_ratio, **kwargs)
        if (
            isinstance(mobject, Surface)
            and hasattr(mobject, "resolution")
            and hasattr(mobject, "preferred_creation_axis")
        ):
            self._native_kind = "show_surface_creation"

    def get_bounds(self, alpha):
        # creation.py:52
        return (0.0, alpha)

    def _native_params(self):
        if self._native_kind != "show_surface_creation":
            return {}
        return {
            "surface_resolution": tuple(self.mobject.resolution),
            "surface_axis": int(self.mobject.preferred_creation_axis),
        }


class Uncreate(ShowCreation):
    # creation.py:56's full signature (fm-5wq.4.94): rate_func=None keeps
    # the native smooth(1 − t) default, remover is structurally True in
    # the native uncreate kind (False refuses by name), and the inert
    # should_match_start surface passes through to ShowPartial.
    _native_kind = "uncreate"

    def __init__(
        self,
        mobject,
        rate_func=None,
        remover=True,
        should_match_start=True,
        **kwargs,
    ):
        _refuse_unrouted("Uncreate()", [("remover", remover is not True)])
        super().__init__(
            mobject,
            rate_func=rate_func,
            should_match_start=should_match_start,
            **kwargs,
        )
        if (
            isinstance(mobject, Surface)
            and hasattr(mobject, "resolution")
            and hasattr(mobject, "preferred_creation_axis")
        ):
            self._native_kind = "uncreate_surface"

    def _native_params(self):
        if self._native_kind != "uncreate_surface":
            return {}
        return {
            "surface_resolution": tuple(self.mobject.resolution),
            "surface_axis": int(self.mobject.preferred_creation_axis),
        }


class DrawBorderThenFill(_NativeAnimation):
    """creation.py:75 — reveal a stroke outline over the first half, then
    cross-fade outline → start over the second (native
    fmn-anim::DrawBorderThenFill owns the mechanism)."""

    _native_kind = "draw_border_then_fill"

    def __init__(
        self,
        vmobject,
        run_time=2.0,
        rate_func=None,
        stroke_width=2.0,
        stroke_color=None,
        draw_border_animation_config=None,
        fill_animation_config=None,
        **kwargs,
    ):
        if not isinstance(vmobject, VMobject):
            raise TypeError(
                "DrawBorderThenFill requires a VMobject; "
                + type(vmobject).__name__
                + " has no border/fill planes (creation.py:88)"
            )
        super().__init__(
            vmobject, run_time=run_time, rate_func=rate_func, **kwargs
        )
        self.stroke_width = float(stroke_width)
        self.stroke_color = stroke_color
        # Stored-but-never-read in the pinned Reference; inert surface.
        self.draw_border_animation_config = draw_border_animation_config or {}
        self.fill_animation_config = fill_animation_config or {}

    def _native_params(self):
        params = {"stroke_width": self.stroke_width}
        if self.stroke_color is not None:
            params["stroke_color"] = tuple(_color_to_rgb(self.stroke_color))
        return params


class Write(DrawBorderThenFill):
    _native_kind = "write"

    def __init__(
        self,
        vmobject,
        run_time=-1,
        lag_ratio=-1,
        rate_func=None,
        stroke_color=None,
        **kwargs,
    ):
        # -1 keeps the native auto values (family-size-derived timing).
        super().__init__(
            vmobject,
            run_time=None if run_time == -1 else run_time,
            lag_ratio=None if lag_ratio == -1 else lag_ratio,
            rate_func=rate_func,
            stroke_color=stroke_color,
            **kwargs,
        )

    def _native_params(self):
        if self.stroke_color is None:
            return {}
        return {"stroke_color": tuple(_color_to_rgb(self.stroke_color))}


class ShowIncreasingSubsets(Animation):
    """creation.py:176 with the native rounding rules (fm-5wq.4.58): the
    group's child list is rewritten each frame from the construction-time
    snapshot, through the live `set_submobjects` seam.

    The seam matters: a bound proxy's Reference-visible `submobjects` is
    the Python family list, and the native arena-only rewiring the spec
    kind used to drive never reached it — the reveal was invisible to
    scene code. `set_submobjects` mutates both worlds in step, exactly the
    AddTextWordByWord pattern; the Rust `show_increasing_subsets` /
    `show_submobjects_one_by_one` kinds stay the native-user surface."""

    _int_round_default = "round"
    _one_by_one = False

    def __init__(
        self,
        group,
        int_func=None,
        suspend_mobject_updating=False,
        **kwargs,
    ):
        if not isinstance(group, Mobject):
            raise TypeError(
                type(self).__name__
                + " requires a Mobject family; "
                + type(group).__name__
                + " has no submobject list to reveal"
            )
        if not group.submobjects:
            raise ValueError(
                type(self).__name__
                + " requires a family with submobjects; the group is empty"
            )
        self._int_round = self._classify_int_func(int_func)
        self.all_submobs = list(group.submobjects)
        super().__init__(
            group,
            suspend_mobject_updating=suspend_mobject_updating,
            **kwargs,
        )

    def _classify_int_func(self, int_func):
        # The two Reference rounding rules the native shelf keeps as data
        # (np.round is ties-to-even, np.ceil is ceiling); anything else
        # refuses precisely instead of silently rounding differently.
        if int_func is None:
            return self._int_round_default
        if int_func is _np.round:
            return "round"
        if int_func is _np.ceil:
            return "ceil"
        raise NotImplementedError(
            type(self).__name__
            + " int_func must be np.round or np.ceil, the native "
            "rounding rules"
        )

    def _native_params(self):
        return {"int_round": self._int_round}

    def interpolate_mobject(self, alpha):
        # The Reference overrides interpolate_mobject (creation.py:190):
        # the rate function applies to the RAW alpha, then the rounding
        # rule turns it into a child count.
        rated = self.rate_func(float(alpha))
        count = len(self.all_submobs)
        value = rated * count
        if self._int_round == "ceil":
            index = int(_np.ceil(value))
        else:
            index = int(_np.round(value))  # ties-to-even, IntRound::Round
        self._update_submobject_list(index)

    def _update_submobject_list(self, index):
        # creation.py:196 / creation.py:207, with Python-slice clamping
        # and set_submobjects' identity short-circuit.
        count = len(self.all_submobs)
        if count == 0:
            return
        if self._one_by_one:
            clipped = int(min(max(index, 0), count - 1))
            desired = [] if clipped == 0 else [self.all_submobs[clipped - 1]]
        else:
            clipped = int(min(max(index, 0), count))
            desired = self.all_submobs[:clipped]
        if list(self.mobject.submobjects) != desired:
            self.mobject.set_submobjects(list(desired))


class ShowSubmobjectsOneByOne(ShowIncreasingSubsets):
    """creation.py:200: ceiling rounding, one visible child at a time."""

    _int_round_default = "ceil"
    _one_by_one = True


class MaintainPositionRelativeTo(_NativeAnimation):
    """update.py:53 through the native tracker (fm-5wq.4.62): the
    follower's offset from the tracked mobject is captured at construction
    and re-imposed every frame, so the follower rides along while the
    tracked mobject moves in the same play call."""

    _native_kind = "maintain_position_relative_to"
    _target_attr = "tracked_mobject"

    def __init__(self, mobject, tracked_mobject=None, **kwargs):
        if not isinstance(mobject, Mobject) or not isinstance(
            tracked_mobject, Mobject
        ):
            raise TypeError(
                "MaintainPositionRelativeTo requires a follower Mobject and "
                "a tracked Mobject; got "
                + type(mobject).__name__
                + " and "
                + type(tracked_mobject).__name__
            )
        super().__init__(mobject, **kwargs)
        self.tracked_mobject = tracked_mobject


class VFadeIn(_NativeAnimation):
    _native_kind = "v_fade_in"

    def __init__(self, vmobject, suspend_mobject_updating=False, **kwargs):
        if not isinstance(vmobject, VMobject):
            raise TypeError(
                type(self).__name__
                + " requires a VMobject; got "
                + type(vmobject).__name__
            )
        super().__init__(
            vmobject,
            suspend_mobject_updating=suspend_mobject_updating,
            **kwargs,
        )

    def _native_params(self):
        return {"final_alpha_value": self.final_alpha_value}


class VFadeOut(_NativeAnimation):
    _native_kind = "v_fade_out"

    def __init__(self, vmobject, remover=True, final_alpha_value=0.0, **kwargs):
        if not isinstance(vmobject, VMobject):
            raise TypeError(
                "VFadeOut requires a VMobject; got "
                + type(vmobject).__name__
            )
        super().__init__(
            vmobject,
            final_alpha_value=final_alpha_value,
            **kwargs,
        )
        self.remover = bool(remover)

    def _native_params(self):
        return {
            "remover": self.remover,
            "final_alpha_value": self.final_alpha_value,
        }


class VFadeInThenOut(VFadeIn):
    _native_kind = "v_fade_in_then_out"

    def __init__(
        self,
        vmobject,
        rate_func=_there_and_back_rate,
        remover=True,
        final_alpha_value=0.5,
        **kwargs,
    ):
        super().__init__(
            vmobject,
            rate_func=rate_func,
            final_alpha_value=final_alpha_value,
            **kwargs,
        )
        self.remover = bool(remover)

    def _native_params(self):
        return {
            "remover": self.remover,
            "final_alpha_value": self.final_alpha_value,
        }


class Rotating(Animation):
    _native_kind = "rotating"

    def __init__(
        self,
        mobject,
        angle=_math.tau,
        axis=_OUT,
        about_point=None,
        about_edge=None,
        run_time=5.0,
        rate_func=_linear_rate,
        suspend_mobject_updating=False,
        **kwargs,
    ):
        super().__init__(
            mobject,
            run_time=run_time,
            rate_func=rate_func,
            suspend_mobject_updating=suspend_mobject_updating,
            **kwargs,
        )
        self.angle = float(angle)
        self.axis = _vec3(axis)
        self.about_point = None if about_point is None else _vec3(about_point)
        self.about_edge = None if about_edge is None else _vec3(about_edge)

    def begin(self):
        if self.time_span is not None:
            self.run_time = max(float(self.time_span[1]), self.run_time)
        self.starting_mobject = self.mobject.copy()
        if self.suspend_mobject_updating:
            self.mobject_was_updating = not self.mobject._is_updating_suspended()
            self.mobject.suspend_updating()
        self.interpolate(0.0)

    def finish(self):
        self.interpolate(self.final_alpha_value)
        if self.suspend_mobject_updating and self.mobject_was_updating:
            self.mobject.resume_updating()

    def interpolate_mobject(self, alpha):
        for current, starting in zip(
            self.mobject.family_members_with_points(),
            self.starting_mobject.family_members_with_points(),
        ):
            current.match_points(starting)
        self.mobject.rotate(
            self.rate_func(self.time_spanned_alpha(float(alpha))) * self.angle,
            axis=self.axis,
            about_point=self.about_point,
            about_edge=self.about_edge,
        )

    def _native_params(self):
        params = {"angle": self.angle, "axis": self.axis}
        if self.about_point is not None:
            params["about_point"] = self.about_point
        if self.about_edge is not None:
            params["about_edge"] = self.about_edge
        return params

    def _native_target(self):
        return None


class Rotate(Rotating):
    _native_kind = "rotate"

    def __init__(
        self,
        mobject,
        angle=_math.pi,
        axis=_OUT,
        run_time=1,
        rate_func=_smooth_rate,
        about_edge=_ORIGIN,
        **kwargs,
    ):
        super().__init__(
            mobject,
            angle=angle,
            axis=axis,
            run_time=run_time,
            rate_func=rate_func,
            about_edge=about_edge,
            **kwargs,
        )


class Homotopy(Animation):
    """movement.py:17 through the Python-callback segment slot: the
    (x, y, z, t) map is user Python, so it rides interpolate_mobject
    per frame (the native fmn-anim Homotopy serves the Rust front
    door's provable closures)."""

    apply_function_config = dict()

    def __init__(self, homotopy, mobject, run_time=3.0, **kwargs):
        self.homotopy = homotopy
        super().__init__(mobject, run_time=run_time, **kwargs)

    def begin(self):
        if self.time_span is not None:
            self.run_time = max(float(self.time_span[1]), self.run_time)
        self.starting_mobject = self.mobject.copy()
        self.interpolate(0.0)

    def function_at_time_t(self, t):
        return lambda point: self.homotopy(point[0], point[1], point[2], t)

    def interpolate_submobject(self, submob, start, alpha):
        submob.match_points(start)
        submob.apply_function(
            self.function_at_time_t(alpha), **self.apply_function_config
        )

    def interpolate_mobject(self, alpha):
        rated = self.rate_func(self.time_spanned_alpha(float(alpha)))
        for submob, start in zip(
            self.mobject.family_members_with_points(),
            self.starting_mobject.family_members_with_points(),
        ):
            self.interpolate_submobject(submob, start, rated)


class SmoothedVectorizedHomotopy(Homotopy):
    apply_function_config = dict(make_smooth=True)


class ComplexHomotopy(Homotopy):
    def __init__(self, complex_homotopy, mobject, **kwargs):
        def homotopy(x, y, z, t):
            c = complex_homotopy(complex(x, y), t)
            return (c.real, c.imag, z)

        super().__init__(homotopy, mobject, **kwargs)


class PhaseFlow(Animation):
    """movement.py:75, ported verbatim: forward-Euler advection whose
    state is path-dependent by design (the `last_alpha` memo), driven
    through the Python-callback slot; the overridden interpolate never
    consults rate_func or time_span (the linear default makes it moot)."""

    def __init__(
        self,
        function,
        mobject,
        virtual_time=None,
        suspend_mobject_updating=False,
        rate_func=_linear_rate,
        run_time=3.0,
        **kwargs,
    ):
        self.function = function
        self.virtual_time = virtual_time or run_time
        super().__init__(
            mobject,
            run_time=run_time,
            rate_func=rate_func,
            suspend_mobject_updating=suspend_mobject_updating,
            **kwargs,
        )

    def interpolate_mobject(self, alpha):
        alpha = float(alpha)
        if hasattr(self, "last_alpha"):
            dt = self.virtual_time * (alpha - self.last_alpha)
            self.mobject.apply_function(
                lambda point: point + dt * self.function(point)
            )
        self.last_alpha = alpha


class MoveAlongPath(_NativeAnimation):
    # BN-03: under the Reference's name, the native sampler takes the
    # point by TRUE arc length (constant speed), and the rate function
    # applies to the raw alpha exactly as movement.py:115's override does.
    _native_kind = "move_along_path"
    _target_attr = "path"

    def __init__(self, mobject, path, suspend_mobject_updating=False, **kwargs):
        if not isinstance(path, VMobject):
            raise TypeError(
                "MoveAlongPath requires a VMobject path; "
                + type(path).__name__
                + " has no curve to sample"
            )
        if not path.has_points():
            raise ValueError(
                "MoveAlongPath path has no points to move along"
            )
        super().__init__(
            mobject,
            suspend_mobject_updating=suspend_mobject_updating,
            **kwargs,
        )
        self.path = path

    def interpolate_mobject(self, alpha):
        # Python fallback mirroring the native rule (raw-alpha rate).
        point = self.path.point_from_proportion(self.rate_func(float(alpha)))
        self.mobject.move_to(point)


class Restore(_NativeAnimation):
    _native_kind = "restore"

    def __init__(
        self,
        mobject,
        path_arc=0.0,
        path_arc_axis=_OUT,
        path_func=None,
        **kwargs,
    ):
        # The Reference's Restore consumes mobject.saved_state at
        # construction; a never-saved mobject fails here with the exact
        # Mobject.restore() exception rather than deep in the native
        # segment (fm-5wq.4.70).
        if getattr(mobject, "saved_state", None) is None:
            raise Exception("Trying to restore without having saved")
        # fm-5wq.4.102: the portal arc factories route onto the native
        # restore path_arc surface exactly as Transform's do (fm-5wq.4.63);
        # an arbitrary user path function stays a precise refusal — Choreo
        # never samples Python mid-segment.
        if path_func is not None:
            routed_arc = getattr(path_func, "_fmn_path_arc", None)
            if routed_arc is None:
                _refuse_unrouted("Restore()", [("path_func", True)])
            path_arc = float(routed_arc)
            path_arc_axis = getattr(path_func, "_fmn_path_axis", path_arc_axis)
        self.path_func = path_func
        super().__init__(mobject, **kwargs)
        self.path_arc = float(path_arc)
        self.path_arc_axis = _vec3(path_arc_axis)

    def _native_params(self):
        return {"path_arc": self.path_arc, "path_arc_axis": self.path_arc_axis}


class Transform(_NativeAnimation):
    _native_kind = "transform"
    _target_attr = "target_mobject"

    def __init__(
        self,
        mobject,
        target_mobject=None,
        path_arc=0.0,
        path_arc_axis=_OUT,
        path_func=None,
        **kwargs,
    ):
        # The portal's own arc factories (utils.paths) carry their scalar
        # arc as metadata, so clockwise_path()/counterclockwise_path()/
        # path_along_arc(θ) route onto the native path_arc surface
        # (fm-5wq.4.63); an arbitrary user path function stays a precise
        # refusal — Choreo never samples Python mid-segment.
        if path_func is not None:
            routed_arc = getattr(path_func, "_fmn_path_arc", None)
            if routed_arc is None:
                _refuse_unrouted("Transform()", [("path_func", True)])
            path_arc = float(routed_arc)
            path_arc_axis = getattr(path_func, "_fmn_path_axis", path_arc_axis)
        self.path_func = path_func
        if not isinstance(mobject, Mobject):
            raise TypeError(
                "Transform expects a Mobject; got " + type(mobject).__name__
            )
        super().__init__(mobject, **kwargs)
        self.target_mobject = target_mobject
        self.path_arc = float(path_arc)
        self.path_arc_axis = _vec3(path_arc_axis)

    def _native_params(self):
        return {"path_arc": self.path_arc, "path_arc_axis": self.path_arc_axis}

    def _allows_deferred_target(self):
        return True

    def create_target(self):
        # Reference create_target hands back the stored target; the
        # in-place reading (fm-5wq.4.83): a None target resolves to a copy
        # of the mobject's play-time state, so `Transform(mob)` after
        # in-place mutation snaps records to that state through the same
        # native transform kind.
        if getattr(self, "target_mobject", None) is not None:
            return self.target_mobject
        return self.mobject.copy()

    def _native_target(self):
        # Subclasses that carry no target at all (CyclicReplace/Swap set
        # `_target_attr = None`) stay target-less rather than resolving
        # the in-place identity.
        if self._target_attr is None:
            return None
        self.target_mobject = self.create_target()
        return self.target_mobject

    def create_starting_mobject(self):
        return self.mobject.copy()

    def begin(self):
        # fm-5wq.4.91: the Python updater-driving fallback (never touched
        # by native Scene.play, which builds Choreo segments instead).
        if self.time_span is not None:
            self.run_time = max(float(self.time_span[1]), self.run_time)
        self.target_mobject = self.create_target()
        self.starting_mobject = self.create_starting_mobject()
        self.target_copy = self.target_mobject.copy()
        self.interpolate(0.0)

    def interpolate_mobject(self, alpha):
        # Straight-path, same-structure record lerp only: family alignment
        # and arc paths are native Transform machinery, so those cases
        # refuse by the missing seam's name instead of drifting.
        if self.path_arc != 0.0:
            raise NotImplementedError(
                "Python-driven Transform interpolation with a path arc "
                "awaits Choreo's persistent-updater seam"
            )
        rated = self.rate_func(self.time_spanned_alpha(float(alpha)))
        subs = self.mobject.family_members_with_points()
        starts = self.starting_mobject.family_members_with_points()
        targets = self.target_copy.family_members_with_points()
        if len(subs) != len(starts) or len(subs) != len(targets):
            raise NotImplementedError(
                "Python-driven Transform interpolation across differing "
                "family structures awaits Choreo's persistent-updater seam"
            )
        for sub, start, target in zip(subs, starts, targets):
            live, from_data, to_data = sub.data, start.data, target.data
            if (
                len(live) != len(from_data)
                or len(live) != len(to_data)
                or live.dtype != from_data.dtype
                or live.dtype != to_data.dtype
            ):
                raise NotImplementedError(
                    "Python-driven Transform interpolation across differing "
                    "record shapes awaits Choreo's persistent-updater seam"
                )
            for name in live.dtype.names:
                live[name][:] = (1.0 - rated) * from_data[name] + (
                    rated * to_data[name]
                )


class Fade(Transform):
    """fading.py's shared base at the pin (fm-5wq.4.75): a Transform that
    stores the shift/scale the concrete fades compose.  The Reference's
    bare ``Fade(mobject)`` has no target and dies at begin; here it
    constructs (base surface) and refuses by name at play instead."""

    def __init__(self, mobject, shift=_ORIGIN, scale=1.0, **kwargs):
        if not isinstance(mobject, Mobject):
            raise TypeError(
                "Fade expects a Mobject; got " + type(mobject).__name__
            )
        super().__init__(mobject, None, **kwargs)
        self.shift_vect = _vec3(shift)
        self.scale_factor = float(scale)

    def _allows_deferred_target(self):
        return True

    def _native_target(self):
        # The fade_in/fade_out kinds carry no target, and the Reference's
        # bare Fade dies at begin — so a None target stays None here and
        # bare Fade keeps its named play-time refusal instead of silently
        # resolving to the in-place identity (fm-5wq.4.83).
        return self.target_mobject

    def _native_params(self):
        return {"shift": self.shift_vect, "scale": self.scale_factor}


class FadeIn(Fade):
    _native_kind = "fade_in"

    # fading.py:41 — the Python updater-driving halves (fm-5wq.4.91);
    # native Scene.play keeps the fade_in kind and never calls these.
    def create_target(self):
        return self.mobject.copy()

    def create_starting_mobject(self):
        start = super().create_starting_mobject()
        start.set_opacity(0)
        start.scale(1.0 / self.scale_factor)
        start.shift([-value for value in self.shift_vect])
        return start


class FadeOut(Fade):
    _native_kind = "fade_out"

    def __init__(self, mobject, shift=_ORIGIN, remover=True, final_alpha_value=0.0, **kwargs):
        scale = kwargs.pop("scale", 1.0)
        _refuse_unrouted(
            "FadeOut()",
            [
                ("remover", remover is not True),
                ("final_alpha_value", final_alpha_value != 0.0),
            ],
        )
        super().__init__(mobject, shift=shift, scale=scale, **kwargs)
        # fading.py:76: finish() restores the pre-fade state.
        self.final_alpha_value = 0.0

    def create_target(self):
        result = self.mobject.copy()
        result.set_opacity(0)
        result.shift(self.shift_vect)
        result.scale(self.scale_factor)
        return result


class FadeInFromLarge(FadeIn):
    """Compatibility spelling for a fade from an enlarged start state."""

    def __init__(self, mobject, scale_factor=2, **kwargs):
        self.scale_factor = float(scale_factor)
        if not _math.isfinite(self.scale_factor) or self.scale_factor <= 0:
            raise ValueError(
                "FadeInFromLarge scale_factor must be positive and finite"
            )
        super().__init__(mobject, scale=self.scale_factor, **kwargs)


class GrowFromPoint(Transform):
    """The Reference grow family over Choreo's native start-prep Transform.

    The anchor is sampled when the animation object is constructed, while the
    target remains the mobject's state when play begins.  This distinction is
    observable when callers move a mobject between those two operations.
    """

    _native_kind = "grow_from_point"

    def __init__(self, mobject, point, point_color=None, **kwargs):
        self.point = _np.array(_vec3(point))
        self.point_color = point_color
        super().__init__(mobject, None, **kwargs)

    def create_target(self):
        return self.mobject.copy()

    def create_starting_mobject(self):
        start = self.mobject.copy()
        start.scale(0)
        start.move_to(self.point)
        if self.point_color is not None:
            start.set_color(self.point_color)
        return start

    def _allows_deferred_target(self):
        return True

    def _native_target(self):
        # Choreo copies the current source at play begin.  Returning another
        # Python target here would adopt a redundant family into the Stage and
        # make the retained target lifetime depend on portal bookkeeping.
        return None

    def _native_params(self):
        params = super()._native_params()
        params["point"] = self.point
        if self.point_color is not None:
            params["point_color"] = tuple(_color_to_rgb(self.point_color))
        return params


class GrowFromCenter(GrowFromPoint):
    _native_kind = "grow_from_center"

    def __init__(self, mobject, **kwargs):
        super().__init__(mobject, mobject.get_center(), **kwargs)


class GrowFromEdge(GrowFromPoint):
    _native_kind = "grow_from_edge"

    def __init__(self, mobject, edge, **kwargs):
        super().__init__(mobject, mobject.get_bounding_box_point(edge), **kwargs)


class GrowArrow(GrowFromPoint):
    _native_kind = "grow_arrow"

    def __init__(self, arrow, **kwargs):
        super().__init__(arrow, arrow.get_start(), **kwargs)


class SpinInFromNothing(GrowFromCenter):
    """Historical grow-from-center spelling with a half-turn arc path."""

    _native_kind = "spin_in_from_nothing"

    def __init__(self, mobject, **kwargs):
        if not isinstance(mobject, VMobject):
            raise TypeError(
                "SpinInFromNothing requires a VMobject with points; got "
                + type(mobject).__name__
            )
        if not mobject.family_members_with_points():
            raise ValueError(
                "SpinInFromNothing requires a VMobject with points; target is empty"
            )
        kwargs.setdefault("path_arc", _math.pi)
        super().__init__(mobject, **kwargs)


class FocusOn(Transform):
    _native_kind = "focus_on"

    def __init__(
        self,
        focus_point,
        opacity=0.2,
        color=_GREY_C,
        run_time=2,
        remover=True,
        **kwargs,
    ):
        if isinstance(focus_point, _BridgeMobject):
            # fm-5wq.4.77: a Mobject focus point follows live. The follow
            # case is Python-driven end to end (the instance drops its
            # native kind, so Scene.play routes it through the 4.97
            # python-callback slot): each frame rebuilds the shrinking
            # dot at the target's CURRENT centre via become — the
            # Reference's move_to updater semantics, with the radius and
            # opacity lerp inlined — so co-playing with the target's own
            # .animate stays an ordinary mixed play.
            self._focus_target = focus_point
            self.focus_point = _np.array(_vec3(focus_point.get_center()))
        else:
            self._focus_target = None
            try:
                self.focus_point = _np.array(_vec3(focus_point))
            except Exception as error:
                raise TypeError(
                    "FocusOn focus_point must be a 3D point or a "
                    "Mobject; got " + type(focus_point).__name__
                ) from error
        self.opacity = float(opacity)
        self.color = color
        self.remover = bool(remover)
        super().__init__(
            self.create_starting_mobject(),
            self.create_target(),
            run_time=run_time,
            **kwargs,
        )
        if self._focus_target is not None:
            self._native_kind = None
            # The python-callback slot floats these; the native spec path
            # tolerates None but the callback path must not crash on it.
            if self.rate_func is None:
                self.rate_func = getattr(
                    _FMN_ROOT, "smooth", lambda value: value
                )
            if self.lag_ratio is None:
                self.lag_ratio = 0.0

    def begin(self):
        if self._focus_target is None:
            return super().begin()
        self.interpolate(0.0)

    def interpolate_mobject(self, alpha):
        if self._focus_target is None:
            return super().interpolate_mobject(alpha)
        rated = self.rate_func(self.time_spanned_alpha(float(alpha)))
        radius = (1.0 - rated) * (_FRAME_X_RADIUS + _FRAME_Y_RADIUS)
        self.mobject.become(
            Dot(
                self._focus_target.get_center(),
                radius=max(radius, 0.0),
                stroke_width=0,
                fill_color=self.color,
                fill_opacity=rated * self.opacity,
            )
        )

    def create_target(self):
        return Dot(
            self.focus_point,
            radius=0,
            stroke_width=0,
            fill_color=self.color,
            fill_opacity=self.opacity,
        )

    def create_starting_mobject(self):
        return Dot(
            self.focus_point,
            radius=_FRAME_X_RADIUS + _FRAME_Y_RADIUS,
            stroke_width=0,
            fill_color=self.color,
            fill_opacity=0,
        )

    def _native_params(self):
        params = super()._native_params()
        params["remover"] = self.remover
        return params


class Indicate(Transform):
    _native_kind = "indicate"

    def __init__(
        self,
        mobject,
        scale_factor=1.2,
        color=_YELLOW,
        rate_func=_there_and_back_rate,
        **kwargs,
    ):
        self.scale_factor = float(scale_factor)
        self.color = color
        super().__init__(mobject, None, rate_func=rate_func, **kwargs)

    def create_target(self):
        return self.mobject.copy().scale(self.scale_factor).set_color(self.color)

    def _allows_deferred_target(self):
        return True

    def _native_target(self):
        return None

    def _native_params(self):
        return {
            "scale_factor": self.scale_factor,
            "color": tuple(_color_to_rgb(self.color)),
        }


class CircleIndicate(Transform):
    _native_kind = "circle_indicate"

    def __init__(
        self,
        mobject,
        scale_factor=1.2,
        rate_func=_there_and_back_rate,
        stroke_color=_YELLOW,
        stroke_width=3.0,
        remover=True,
        **kwargs,
    ):
        if not isinstance(mobject, _BridgeMobject):
            raise TypeError(
                "CircleIndicate expects a Mobject; got "
                + type(mobject).__name__
            )
        self.scale_factor = float(scale_factor)
        if not _math.isfinite(self.scale_factor) or self.scale_factor <= 0:
            raise ValueError("CircleIndicate scale_factor must be positive and finite")
        self.stroke_color = stroke_color
        self.stroke_width = float(stroke_width)
        if not _math.isfinite(self.stroke_width) or self.stroke_width < 0:
            raise ValueError("CircleIndicate stroke_width must be non-negative and finite")
        self.remover = bool(remover)
        circle = Circle(
            stroke_color=self.stroke_color,
            stroke_width=self.stroke_width,
        ).surround(mobject)
        pre_circle = circle.copy().set_stroke(width=0)
        pre_circle.scale(1.0 / self.scale_factor)
        self.circle = circle
        super().__init__(
            pre_circle,
            circle,
            rate_func=rate_func,
            **kwargs,
        )

    def _native_params(self):
        params = super()._native_params()
        params["remover"] = self.remover
        return params


class TurnInsideOut(Transform):
    _native_kind = "turn_inside_out"

    def __init__(self, mobject, path_arc=0.5 * _math.pi, **kwargs):
        super().__init__(mobject, None, path_arc=path_arc, **kwargs)

    def create_target(self):
        return self.mobject.copy().reverse_points()

    def _allows_deferred_target(self):
        return True

    def _native_target(self):
        return None


class WiggleOutThenIn(Animation):
    _native_kind = "wiggle_out_then_in"

    def __init__(
        self,
        mobject,
        scale_value=1.1,
        rotation_angle=0.01 * _math.tau,
        n_wiggles=6,
        scale_about_point=None,
        rotate_about_point=None,
        run_time=2,
        **kwargs,
    ):
        super().__init__(mobject, run_time=run_time, **kwargs)
        self.scale_value = float(scale_value)
        self.rotation_angle = float(rotation_angle)
        self.n_wiggles = float(n_wiggles)
        self.scale_about_point = (
            None if scale_about_point is None else _vec3(scale_about_point)
        )
        self.rotate_about_point = (
            None if rotate_about_point is None else _vec3(rotate_about_point)
        )

    def get_scale_about_point(self):
        if self.scale_about_point is not None:
            return _np.array(self.scale_about_point)
        return self.mobject.get_center()

    def get_rotate_about_point(self):
        if self.rotate_about_point is not None:
            return _np.array(self.rotate_about_point)
        return self.mobject.get_center()

    def _native_params(self):
        params = {
            "scale_value": self.scale_value,
            "rotation_angle": self.rotation_angle,
            "n_wiggles": self.n_wiggles,
        }
        if self.scale_about_point is not None:
            params["scale_about_point"] = self.scale_about_point
        if self.rotate_about_point is not None:
            params["rotate_about_point"] = self.rotate_about_point
        return params

    def _native_target(self):
        return None


class ShowPassingFlash(_NativeAnimation):
    _native_kind = "show_passing_flash"

    def __init__(self, mobject, time_width=0.1, remover=True, **kwargs):
        _refuse_unrouted(
            "ShowPassingFlash()", [("remover", remover is not True)]
        )
        super().__init__(mobject, **kwargs)
        self.time_width = float(time_width)

    def get_bounds(self, alpha):
        upper = _interpolate(0.0, 1.0 + self.time_width, float(alpha))
        return (max(upper - self.time_width, 0.0), min(upper, 1.0))

    def _native_params(self):
        return {"time_width": self.time_width, "remover": True}


class ShowCreationThenDestruction(ShowPassingFlash):
    _native_kind = "show_creation_then_destruction"

    def __init__(self, vmobject, time_width=2.0, **kwargs):
        super().__init__(vmobject, time_width=time_width, **kwargs)


class VShowPassingFlash(Animation):
    _native_kind = "v_show_passing_flash"

    def __init__(
        self,
        vmobject,
        time_width=0.3,
        taper_width=0.05,
        remover=True,
        **kwargs,
    ):
        _refuse_unrouted(
            "VShowPassingFlash()", [("remover", remover is not True)]
        )
        super().__init__(vmobject, remover=True, **kwargs)
        self.time_width = float(time_width)
        self.taper_width = float(taper_width)

    def taper_kernel(self, x):
        if x < self.taper_width:
            return x
        if x > 1.0 - self.taper_width:
            return 1.0 - x
        return 1.0

    def _native_params(self):
        return {
            "time_width": self.time_width,
            "taper_width": self.taper_width,
            "remover": True,
        }

    def _native_target(self):
        return None


class FlashAround(VShowPassingFlash):
    """Sweep a tapered native stroke around Atlas matcher geometry."""

    def __init__(
        self,
        mobject,
        time_width=1.0,
        taper_width=0.0,
        stroke_width=4.0,
        color=_YELLOW,
        buff=_SMALL_BUFF,
        n_inserted_curves=100,
        **kwargs,
    ):
        if not isinstance(mobject, _BridgeMobject):
            raise TypeError(f"{type(self).__name__} expects a Mobject")
        path = self.get_path(mobject, buff)
        if mobject.is_fixed_in_frame():
            path.fix_in_frame()
        path.insert_n_curves(n_inserted_curves)
        path.set_points(path.get_points_without_null_curves())
        path.set_stroke(color, stroke_width)
        super().__init__(
            path,
            time_width=time_width,
            taper_width=taper_width,
            **kwargs,
        )

    def get_path(self, mobject, buff):
        return SurroundingRectangle(mobject, buff=buff)


class FlashUnder(FlashAround):
    def get_path(self, mobject, buff):
        return Underline(mobject, buff=buff, stretch_factor=1.0)


class ShowPassingFlashAround(VShowPassingFlash):
    """Track a native surrounding rectangle while its stroke sweeps."""

    def __init__(
        self,
        mobject,
        stroke_width=2.0,
        stroke_color=_YELLOW,
        buff=_SMALL_BUFF,
        **kwargs,
    ):
        if not isinstance(mobject, _BridgeMobject):
            raise TypeError(f"{type(self).__name__} expects a Mobject")
        rect = SurroundingRectangle(
            mobject,
            stroke_width=stroke_width,
            color=stroke_color,
            buff=buff,
        )
        # The Reference's ShowPassingFlash works on continuous path bounds.
        # Our native VShowPassingFlash samples per-record stroke widths, so
        # densify the same rectangle before sweeping it to keep the wrapper
        # visually continuous rather than exposing four coarse edge samples.
        rect.insert_n_curves(100)
        rect.set_points(rect.get_points_without_null_curves())
        rect.add_updater(lambda surrounding: surrounding.move_to(mobject))
        kwargs.setdefault("time_width", 0.1)
        kwargs.setdefault("taper_width", 0.0)
        super().__init__(rect, **kwargs)


class ApplyWave(Animation):
    _native_kind = "apply_wave"

    def __init__(
        self,
        mobject,
        direction=_UP,
        amplitude=0.2,
        run_time=1.0,
        **kwargs,
    ):
        super().__init__(mobject, run_time=run_time, **kwargs)
        self.direction = _vec3(direction)
        self.amplitude = float(amplitude)

    def _native_params(self):
        return {"direction": self.direction, "amplitude": self.amplitude}

    def _native_target(self):
        return None


class ApplyMethod(Transform):
    def __init__(self, method, *args, **kwargs):
        self.check_validity_of_input(method)
        self.method = method
        self.method_args = args
        super().__init__(method.__self__, None, **kwargs)

    def check_validity_of_input(self, method):
        if not _inspect.ismethod(method):
            raise Exception(
                "Whoops, looks like you accidentally invoked "
                "the method you want to animate"
            )
        assert isinstance(method.__self__, Mobject)

    def create_target(self):
        method = self.method
        args = list(self.method_args)
        if args and isinstance(args[-1], dict):
            method_kwargs = args.pop()
        else:
            method_kwargs = {}
        target = method.__self__.copy()
        method.__func__(target, *args, **method_kwargs)
        return target

    def _allows_deferred_target(self):
        return True


class ApplyPointwiseFunction(ApplyMethod):
    def __init__(self, function, mobject, run_time=3.0, **kwargs):
        if not callable(function):
            raise TypeError(
                "ApplyPointwiseFunction function must be callable; got "
                + type(function).__name__
            )
        super().__init__(mobject.apply_function, function, run_time=run_time, **kwargs)


class ApplyPointwiseFunctionToCenter(Transform):
    def __init__(self, function, mobject, **kwargs):
        if not callable(function):
            raise TypeError(
                "ApplyPointwiseFunctionToCenter function must be callable; got "
                + type(function).__name__
            )
        self.function = function
        super().__init__(mobject, None, **kwargs)

    def create_target(self):
        return self.mobject.copy().move_to(self.function(self.mobject.get_center()))

    def _allows_deferred_target(self):
        return True


class FadeToColor(ApplyMethod):
    def __init__(self, mobject, color, **kwargs):
        super().__init__(mobject.set_color, color, **kwargs)


class ScaleInPlace(ApplyMethod):
    def __init__(self, mobject, scale_factor, **kwargs):
        super().__init__(mobject.scale, scale_factor, **kwargs)


class ShrinkToCenter(ScaleInPlace):
    def __init__(self, mobject, **kwargs):
        super().__init__(mobject, 0, **kwargs)


class ApplyFunction(Transform):
    def __init__(self, function, mobject, **kwargs):
        self.function = function
        super().__init__(mobject, None, **kwargs)

    def create_target(self):
        target = self.function(self.mobject.copy())
        if not isinstance(target, Mobject):
            raise Exception(
                "Functions passed to ApplyFunction must return object of type Mobject"
            )
        return target

    def _allows_deferred_target(self):
        return True


class ApplyMatrix(ApplyPointwiseFunction):
    def __init__(self, matrix, mobject, **kwargs):
        matrix = self.initialize_matrix(matrix)

        def func(point):
            return _np.dot(point, matrix.T)

        super().__init__(func, mobject, **kwargs)

    def initialize_matrix(self, matrix):
        try:
            matrix = _np.array(matrix)
        except (TypeError, ValueError) as error:
            raise TypeError(
                "ApplyMatrix matrix must be a rectangular 2x2 or 3x3 array"
            ) from error
        if matrix.shape == (2, 2):
            new_matrix = _np.identity(3)
            new_matrix[:2, :2] = matrix
            matrix = new_matrix
        elif matrix.shape != (3, 3):
            raise ValueError(
                "ApplyMatrix matrix must have shape (2, 2) or (3, 3); "
                f"got {matrix.shape}"
            )
        return matrix


class ApplyComplexFunction(ApplyMethod):
    def __init__(self, function, mobject, **kwargs):
        # fm-5wq.4.104: the class already rides the ApplyMethod/Transform
        # native kinds; what was missing were the named refusals in front
        # of the Reference's bare probe crashes (function(1+0j) at
        # construction derives path_arc, transform.py:308).
        if not callable(function):
            raise TypeError(
                "ApplyComplexFunction requires a callable complex "
                "function; got " + type(function).__name__
            )
        if not isinstance(mobject, Mobject):
            raise TypeError(
                "ApplyComplexFunction requires a Mobject; got "
                + type(mobject).__name__
            )
        self.function = function
        try:
            probe = complex(function(complex(1)))
        except (TypeError, ValueError) as error:
            raise TypeError(
                "ApplyComplexFunction's function must map complex to "
                "complex; probing f(1+0j) failed: " + str(error)
            ) from error
        kwargs["path_arc"] = float(_np.log(probe).imag)
        super().__init__(mobject.apply_complex_function, function, **kwargs)

    def init_path_func(self):
        self.path_arc = float(_np.log(complex(self.function(complex(1)))).imag)


class MoveToTarget(Transform):
    def __init__(self, mobject, **kwargs):
        target = getattr(mobject, "target", None)
        if target is None:
            raise Exception("MoveToTarget called on mobject without attribute 'target'")
        super().__init__(mobject, target, **kwargs)


class _MethodAnimation(MoveToTarget):
    def __init__(self, mobject, methods, **kwargs):
        self.methods = methods
        super().__init__(mobject, **kwargs)


def prepare_animation(anim):
    if isinstance(anim, _AnimationBuilder):
        return anim.build()
    if isinstance(anim, Animation):
        return anim
    raise TypeError(f"Object {anim} cannot be converted to an animation")


class ReplacementTransform(Transform):
    _native_kind = "replacement_transform"


class TransformFromCopy(Transform):
    _native_kind = "transform_from_copy"

    def __init__(self, mobject, target_mobject, **kwargs):
        super().__init__(mobject, target_mobject, **kwargs)


class CyclicReplace(Transform):
    """transform.py:316 through native per-mobject arc transforms
    (fm-5wq.4.63): each mobject rides a path-arc Transform onto the next
    mobject's center, cyclically."""

    _native_kind = "cyclic_replace"
    _target_attr = None

    def __init__(self, *mobjects, path_arc=0.5 * _math.pi, **kwargs):
        if len(mobjects) < 2:
            raise ValueError(
                type(self).__name__
                + " needs at least two mobjects to cycle; got "
                + str(len(mobjects))
            )
        if not all(isinstance(mobject, Mobject) for mobject in mobjects):
            raise TypeError(type(self).__name__ + " cycles Mobjects only")
        _NativeAnimation.__init__(self, mobjects[0], **kwargs)
        self.mobjects = list(mobjects)
        self.path_arc = float(path_arc)
        self._native_extra_mobjects = tuple(self.mobjects[1:])

    def _native_params(self):
        return {"mobs": self.mobjects, "path_arc": self.path_arc}


class Swap(CyclicReplace):
    _native_kind = "swap"


class FadeTransform(_NativeAnimation):
    _native_kind = "fade_transform"
    _target_attr = "target_mobject"

    def __init__(self, mobject, target_mobject, stretch=True, dim_to_match=1, **kwargs):
        # fm-5wq.4.98: stretch and dim_to_match route to the native
        # builder's own knobs (fading.py:91 semantics); the unrouted
        # refusal retires. A non-Mobject endpoint refuses by name here
        # instead of failing anonymously at spec build.
        if not isinstance(mobject, _BridgeMobject) or not isinstance(
            target_mobject, _BridgeMobject
        ):
            raise TypeError(
                "FadeTransform requires a source Mobject and a target "
                "Mobject; got "
                + type(mobject).__name__
                + " and "
                + type(target_mobject).__name__
            )
        super().__init__(mobject, **kwargs)
        self.target_mobject = target_mobject
        self.stretch = bool(stretch)
        self.dim_to_match = int(dim_to_match)
        if self.dim_to_match not in (0, 1, 2):
            raise ValueError(
                "FadeTransform dim_to_match must be 0, 1, or 2; got "
                + repr(dim_to_match)
            )

    def _native_params(self):
        return {"stretch": self.stretch, "dim_to_match": self.dim_to_match}


class FadeTransformPieces(FadeTransform):
    _native_kind = "fade_transform_pieces"

    def __init__(self, mobject, target_mobject, **kwargs):
        # The per-piece native path does not consume stretch/dim_to_match
        # yet; refuse non-defaults by the unrouted-keyword name rather
        # than silently dropping them (fm-5wq.4.98).
        _refuse_unrouted(
            "FadeTransformPieces()",
            [
                ("stretch", kwargs.get("stretch", True) is not True),
                ("dim_to_match", kwargs.get("dim_to_match", 1) != 1),
            ],
        )
        for role, value in (("source", mobject), ("target", target_mobject)):
            if not isinstance(value, VMobject):
                raise TypeError(
                    "FadeTransformPieces requires a non-empty VMobject pair; "
                    + role
                    + " is "
                    + type(value).__name__
                )
            if not value.family_members_with_points():
                raise ValueError(
                    "FadeTransformPieces requires a non-empty VMobject pair; "
                    + role
                    + " has no points"
                )
        super().__init__(mobject, target_mobject, **kwargs)


class TransformMatchingParts(_NativeAnimation):
    """transform_matching_parts.py:21 through the native shape matcher
    (fm-5wq.4.68): user matched_pairs claim first, then the same-shape
    product over the remaining pieces, then directional fades for the
    leftovers — all built natively, no Python mid-segment."""

    _native_kind = "transform_matching_parts"
    _target_attr = "target_mobject"

    def __init__(
        self,
        source,
        target,
        matched_pairs=(),
        match_animation=Transform,
        mismatch_animation=Transform,
        run_time=2,
        lag_ratio=0,
        **kwargs,
    ):
        if not isinstance(source, Mobject) or not isinstance(target, Mobject):
            raise TypeError(
                type(self).__name__ + " expects two Mobject families"
            )
        _refuse_unrouted(
            type(self).__name__ + "()",
            [
                ("match_animation", match_animation is not Transform),
                ("mismatch_animation", mismatch_animation is not Transform),
            ],
        )
        pairs = [tuple(pair) for pair in matched_pairs]
        if not all(
            len(pair) == 2
            and isinstance(pair[0], Mobject)
            and isinstance(pair[1], Mobject)
            for pair in pairs
        ):
            raise TypeError(
                type(self).__name__ + " matched_pairs must pair Mobjects"
            )
        if (
            not source.family_members_with_points()
            or not target.family_members_with_points()
        ):
            raise ValueError(
                type(self).__name__
                + " requires point-bearing families on both sides"
            )
        self.target_mobject = target
        self.matched_pairs = pairs
        super().__init__(
            source, run_time=run_time, lag_ratio=lag_ratio, **kwargs
        )

    def _native_params(self):
        return {"matched_pairs": self.matched_pairs}


class TransformMatchingShapes(TransformMatchingParts):
    _native_kind = "transform_matching_shapes"


class TransformMatchingStrings(_NativeAnimation):
    """Match native string primitives by their source-span identity.

    Plain-text span maps are one glyph per part, so bare key equality would
    pair every ``e`` with any other ``e``.  This class therefore matches by
    string identity over the longest matching blocks of the two key
    sequences (``difflib.SequenceMatcher``, ``autojunk=False`` — pure
    deterministic sequence data), the pinned Reference's matching-blocks
    discipline: a glyph pairs only inside a genuinely shared substring run.
    ``TransformMatchingTex`` opts back into bare key equality, because Tex
    span parts are semantic isolate units rather than glyphs."""

    _native_kind = "transform_matching_strings"
    _target_attr = "target_mobject"
    _match_by_blocks = True

    def __init__(
        self,
        source,
        target,
        matched_keys=(),
        key_map=None,
        matched_pairs=(),
        run_time=2,
        lag_ratio=0,
        **kwargs,
    ):
        if not isinstance(source, StringMobject) or not isinstance(
            target, StringMobject
        ):
            raise TypeError(
                type(self).__name__
                + " expects two StringMobject instances"
            )
        if not source._string_sub_spans or not target._string_sub_spans:
            raise _TexError(
                type(self).__name__
                + " requires non-empty native span maps"
            )
        pairs = [tuple(pair) for pair in matched_pairs]
        if not all(
            len(pair) == 2
            and isinstance(pair[0], Mobject)
            and isinstance(pair[1], Mobject)
            for pair in pairs
        ):
            raise TypeError(
                type(self).__name__ + " matched_pairs must pair Mobjects"
            )
        self.matched_pairs = pairs
        self.target_mobject = target
        self.matched_keys = tuple(matched_keys)
        self.key_map = dict(key_map or {})
        super().__init__(
            source,
            run_time=run_time,
            lag_ratio=lag_ratio,
            **kwargs,
        )

    def _native_span_keys(self, mobject):
        encoded = mobject.get_string().encode("utf-8")
        keys = []
        for ordinal, (start, end) in enumerate(mobject._string_sub_spans):
            if not 0 <= start < end <= len(encoded):
                raise _TexError(
                    type(self).__name__
                    + " received an invalid native span"
                )
            try:
                key = encoded[start:end].decode("utf-8")
            except UnicodeDecodeError as error:
                raise _TexError(
                    type(self).__name__
                    + " native span split a UTF-8 code point"
                ) from error
            keys.append((mobject._string_submobject(ordinal), key))
        return keys

    @staticmethod
    def _matching_block_keys(source_keys, target_keys, pinned):
        """Scope keys to `SequenceMatcher` matching blocks: parts inside a
        shared block keep pairwise-aligned block-scoped keys, pinned keys
        (`matched_keys` / `key_map` sources) stay raw everywhere, and
        everything else becomes unpairable so it takes the native fades."""
        source_sequence = [key for _, key in source_keys]
        target_sequence = [key for _, key in target_keys]
        matcher = _difflib.SequenceMatcher(
            None, source_sequence, target_sequence, autojunk=False
        )
        source_block = {}
        target_block = {}
        for block_ordinal, block in enumerate(matcher.get_matching_blocks()):
            for offset in range(block.size):
                source_block[block.a + offset] = block_ordinal
                target_block[block.b + offset] = block_ordinal

        def scope(keys, blocks, side):
            scoped = []
            for index, (part, key) in enumerate(keys):
                if key in pinned:
                    scoped.append((part, key))
                elif index in blocks:
                    scoped.append((part, f"block:{blocks[index]}:{key}"))
                else:
                    scoped.append((part, f"{side}:{index}:{key}"))
            return scoped

        return (
            scope(source_keys, source_block, "source-only"),
            scope(target_keys, target_block, "target-only"),
        )

    def _claim_matched_pairs(self, source_keys, target_keys):
        """The explicit-pair native key override (fm-5wq.4.76): each user
        pair claims its two parts with a unique shared key BEFORE the
        matching-blocks pass, so the pair always transforms together and
        the block matcher only sees the remainder. A pair member that is
        not a live span-map part, or a part claimed twice, refuses by
        name."""
        source_claims = {}
        target_claims = {}
        for ordinal, (source_part, target_part) in enumerate(
            self.matched_pairs
        ):
            key = "matched-pair:" + str(ordinal)
            if (
                id(source_part) in source_claims
                or id(target_part) in target_claims
            ):
                raise ValueError(
                    type(self).__name__
                    + " matched_pairs claims the same part twice"
                )
            source_claims[id(source_part)] = key
            target_claims[id(target_part)] = key

        def claim(keys, claims, side):
            remaining = []
            claimed = []
            for part, key in keys:
                pair_key = claims.pop(id(part), None)
                if pair_key is not None:
                    claimed.append((part, pair_key))
                else:
                    remaining.append((part, key))
            if claims:
                raise ValueError(
                    type(self).__name__
                    + " matched_pairs member is not a live span-map part "
                    "of the " + side + " family"
                )
            return remaining, claimed

        source_remaining, source_claimed = claim(
            source_keys, source_claims, "source"
        )
        target_remaining, target_claimed = claim(
            target_keys, target_claims, "target"
        )
        return (
            source_remaining,
            source_claimed,
            target_remaining,
            target_claimed,
        )

    def _native_params(self):
        source_keys = self._native_span_keys(self.mobject)
        target_keys = self._native_span_keys(self.target_mobject)
        source_claimed = []
        target_claimed = []
        if self.matched_pairs:
            (
                source_keys,
                source_claimed,
                target_keys,
                target_claimed,
            ) = self._claim_matched_pairs(source_keys, target_keys)
        if self.matched_keys and not self._match_by_blocks:
            admitted = set(self.matched_keys)
            source_keys = [
                (part, key if key in admitted else f"source-only:{index}:{key}")
                for index, (part, key) in enumerate(source_keys)
            ]
            target_keys = [
                (part, key if key in admitted else f"target-only:{index}:{key}")
                for index, (part, key) in enumerate(target_keys)
            ]
        if self.key_map:
            reverse = {target: source for source, target in self.key_map.items()}
            target_keys = [
                (part, reverse.get(key, key)) for part, key in target_keys
            ]
        if self._match_by_blocks:
            pinned = set(self.matched_keys) | set(self.key_map)
            source_keys, target_keys = self._matching_block_keys(
                source_keys, target_keys, pinned
            )
        return {
            "source_keys": source_keys + source_claimed,
            "target_keys": target_keys + target_claimed,
        }


class TransformMatchingTex(TransformMatchingStrings):
    _native_kind = "transform_matching_tex"
    _match_by_blocks = False


class FadeInFromPoint(_NativeAnimation):
    # Reference fading.py:71 — routed to the native fade_in_from_point,
    # which encodes the same shift/scale composition.
    _native_kind = "fade_in_from_point"

    def __init__(self, mobject, point, **kwargs):
        super().__init__(mobject, **kwargs)
        self.point = _vec3(point)

    def _native_params(self):
        return {"point": self.point}


class FadeOutToPoint(FadeInFromPoint):
    _native_kind = "fade_out_to_point"


class AnimationGroup(_NativeAnimation):
    """Reference composition.py: members share one segment; the native
    module owns the group timing derivation (build_timings — the
    Reference's `interpolate(start, end, lag_ratio)` rule)."""

    _native_kind = "animation_group"
    _default_lag_ratio = 0.0

    def __init__(self, *animations, run_time=-1, lag_ratio=None, group=None, group_type=None, **kwargs):
        _refuse_unrouted(
            type(self).__name__ + "()",
            [("group", group is not None), ("group_type", group_type is not None)],
        )
        if (
            len(animations) == 1
            and not isinstance(animations[0], (Animation, _AnimationBuilder))
            and isinstance(animations[0], _collections_abc.Iterable)
        ):
            animations = tuple(animations[0])
        super().__init__(
            None,
            run_time=None if run_time == -1 else run_time,
            lag_ratio=type(self)._default_lag_ratio if lag_ratio is None else lag_ratio,
            **kwargs,
        )
        self.animations = list(animations)


class LaggedStart(AnimationGroup):
    _native_kind = "lagged_start"
    _default_lag_ratio = 0.05


class Succession(AnimationGroup):
    _native_kind = "succession"
    _default_lag_ratio = 1.0


def _python_composition_members(group):
    """fm-5wq.4.88: every Python-driven leaf animation inside a
    composition, recursively (builders and native-kind classes are the
    native module's; nested groups recurse)."""
    members = []
    for member in group.animations:
        if isinstance(member, AnimationGroup):
            members.extend(_python_composition_members(member))
        elif isinstance(member, Animation) and not getattr(
            member, "_native_kind", None
        ):
            members.append(member)
    return members


def _composition_member_run_time(member):
    run_time = getattr(member, "run_time", None)
    return 1.0 if run_time is None else float(run_time)


def _composition_timings(group):
    """fmn-anim composition.rs `build_timings`, mirrored: member k spans
    [start, start + run_time_k] and the next member starts at
    `start + run_time_k * lag_ratio`."""
    lag = float(
        type(group)._default_lag_ratio
        if group.lag_ratio is None
        else group.lag_ratio
    )
    timings = []
    curr = 0.0
    for member in group.animations:
        start = curr
        end = start + _composition_member_run_time(member)
        timings.append((member, start, end))
        curr = start + (end - start) * lag
    max_end = max((end for _, _, end in timings), default=0.0)
    return timings, max_end


def _composition_timeline_position(group, alpha, max_end):
    """fmn-anim composition.rs `timeline_position`: time_span re-window,
    then the rate curve, scaled onto the timeline. The scale is the
    group's EXPLICIT run_time when one was given (fm-5wq.4.88: the group
    alpha the native slot hands the driver is already t / run_time, so
    rate(alpha) * run_time is absolute seconds and a python leaf's
    nominal window spans exactly its share of the actual play), falling
    back to max_end_time — the native group_config default — when the
    group derives its run_time from the members."""
    span = getattr(group, "time_span", None)
    if span is not None:
        start, end = span
        run_time = _composition_member_run_time(group)
        alpha = min(max(alpha * run_time - start, 0.0), end - start) / (
            end - start
        )
    rate = getattr(group, "rate_func", None)
    if rate is None:
        rate = getattr(_FMN_ROOT, "smooth", lambda value: value)
    scale = max_end
    explicit = getattr(group, "run_time", None)
    try:
        explicit = None if explicit is None else float(explicit)
    except (TypeError, ValueError):
        explicit = None
    if explicit is not None and explicit > 0:
        scale = explicit
    return rate(float(alpha)) * scale


def _drive_python_composition(group, alpha):
    """Interpolate every Python-driven leaf at the exact sub-alpha its
    native placeholder slot occupies: the native group interpolates its
    members at `Interval.sub_alpha(timeline_position(alpha))`, and this
    mirror computes the same windows for the Python leaves only."""
    timings, max_end = _composition_timings(group)
    time = _composition_timeline_position(group, alpha, max_end)
    for member, start, end in timings:
        duration = end - start
        sub = 1.0 if duration <= 0 else min(max((time - start) / duration, 0.0), 1.0)
        if isinstance(member, AnimationGroup):
            _drive_python_composition(member, sub)
        elif isinstance(member, Animation) and not getattr(
            member, "_native_kind", None
        ):
            member.interpolate(sub)


class _CompositionCallbackDriver:
    """The one top-level release-window callback for a composition that
    carries Python-driven members (fm-5wq.4.88): the native group drives
    its native members and the python_callback placeholder slots; this
    driver runs the Python leaves at the mirrored sub-alphas in the same
    release window."""

    def __init__(self, group):
        self.group = group
        self.members = _python_composition_members(group)

    def begin(self):
        for member in self.members:
            member.begin()

    def interpolate(self, alpha):
        _drive_python_composition(self.group, float(alpha))

    def finish(self):
        for member in self.members:
            member.finish()


class Flash(AnimationGroup):
    _native_kind = "flash"

    def __init__(
        self,
        point,
        color=_YELLOW,
        line_length=0.2,
        num_lines=12,
        flash_radius=0.3,
        line_stroke_width=3.0,
        run_time=1.0,
        **kwargs,
    ):
        if isinstance(point, _BridgeMobject):
            self._flash_target = point
            self.point = _np.array(_vec3(point.get_center()))
        else:
            self._flash_target = None
            try:
                self.point = _np.array(_vec3(point))
            except Exception as error:
                raise TypeError(
                    "Flash point must be a 3D point or a Mobject; got "
                    + type(point).__name__
                ) from error
        self.color = color
        self.line_length = float(line_length)
        self.flash_radius = float(flash_radius)
        self.line_stroke_width = float(line_stroke_width)
        try:
            self.num_lines = _operator.index(num_lines)
        except TypeError:
            raise TypeError("Flash num_lines must be an integer") from None
        if self.num_lines <= 0:
            raise ValueError("Flash num_lines must be greater than zero")
        for name, value in (
            ("line_length", self.line_length),
            ("flash_radius", self.flash_radius),
            ("line_stroke_width", self.line_stroke_width),
        ):
            if not _math.isfinite(value) or value < 0:
                raise ValueError(
                    "Flash " + name + " must be non-negative and finite"
                )
        self.lines = self.create_lines()
        super().__init__(
            *self.create_line_anims(),
            run_time=run_time,
            **kwargs,
        )
        if self._flash_target is not None:
            # fm-5wq.4.78: the Mobject-point case is Python-driven end to
            # end — the same architecture whose co-play pin runs green for
            # FocusOn (fm-5wq.4.77): the instance drops its native kind and
            # rides the 4.97 python-callback slot, so each release-window
            # frame re-applies every line's passing-flash window from its
            # begin copy and re-centres the radial group on the target's
            # CURRENT centre. The numeric-point case keeps the native
            # flash composition untouched.
            self._native_kind = None
            self.mobject = self.lines
            self.remover = True
            # The python-callback slot floats these; the native spec path
            # tolerates None but the callback path must not crash on it —
            # the same normalization FocusOn's follow case carries
            # (fm-5wq.4.77).
            if self.rate_func is None:
                self.rate_func = getattr(
                    _FMN_ROOT, "smooth", lambda value: value
                )
            if self.lag_ratio is None:
                self.lag_ratio = 0.0

    # The Python-driven follow lifecycle (only the Mobject-point case is
    # ever driven; the numeric case plays natively and never calls these).
    _FLASH_WINDOW_TIME_WIDTH = 2.0  # ShowCreationThenDestruction's default

    def begin(self):
        if self._flash_target is None:
            return Animation.begin(self)
        self._follow_sources = [line.copy() for line in self.lines]
        self.interpolate(0.0)
        return self

    def interpolate_mobject(self, alpha):
        if self._flash_target is None:
            # Numeric points animate through the native flash composition;
            # a Python drive of that case (turn_animation_into_updater)
            # refuses by name rather than silently holding still.
            raise NotImplementedError(
                "Flash with a numeric point animates through the native "
                "flash composition; Python-driven interpolation serves "
                "only the Mobject-follow case"
            )
        rated = self.rate_func(self.time_spanned_alpha(float(alpha)))
        # RevealBounds::PassingFlash, formula for formula (indication.py:179):
        # the lower bound derives from the UNCAPPED upper, so the window
        # closes to zero width at alpha 1 instead of leaving a sliver.
        time_width = self._FLASH_WINDOW_TIME_WIDTH
        raw_upper = rated * (1.0 + time_width)
        lower = max(raw_upper - time_width, 0.0)
        upper = min(raw_upper, 1.0)
        for line, source in zip(self.lines, self._follow_sources):
            line.pointwise_become_partial(source, lower, upper)
        self.lines.move_to(self._flash_target)

    def create_lines(self):
        lines = VGroup()
        for index in range(self.num_lines):
            angle = index * _math.tau / self.num_lines
            line = Line(_ORIGIN, self.line_length * _RIGHT)
            line.shift((self.flash_radius - self.line_length) * _RIGHT)
            line.rotate(angle, about_point=_ORIGIN)
            lines.add(line)
        lines.set_stroke(color=self.color, width=self.line_stroke_width)
        lines.move_to(self.point)
        return lines

    def create_line_anims(self):
        return [ShowCreationThenDestruction(line) for line in self.lines]


class ShowCreationThenFadeOut(Succession):
    _native_kind = "show_creation_then_fade_out"

    def __init__(self, mobject, remover=True, **kwargs):
        # fm-5wq.4.103: name the composite in the refusal instead of
        # letting the ShowCreation member's error speak for it.
        if not isinstance(mobject, (VMobject, Surface)):
            raise TypeError(
                "ShowCreationThenFadeOut requires a VMobject or Surface "
                "family; got " + type(mobject).__name__
            )
        self.remover = bool(remover)
        super().__init__(
            ShowCreation(mobject),
            FadeOut(mobject),
            **kwargs,
        )

    def _native_params(self):
        return {"remover": self.remover}


class _BroadcastRestore(Restore):
    """Restore one Broadcast ring and apply the outer remover contract."""

    def __init__(self, mobject, remover=True, **kwargs):
        self.remover = bool(remover)
        super().__init__(mobject, **kwargs)

    def _native_params(self):
        params = super()._native_params()
        params["remover"] = self.remover
        return params


class Broadcast(LaggedStart):
    _native_kind = "broadcast"

    def __init__(
        self,
        focal_point,
        small_radius=0.0,
        big_radius=5.0,
        n_circles=5,
        start_stroke_width=8.0,
        color=_WHITE,
        run_time=3.0,
        lag_ratio=0.2,
        remover=True,
        **kwargs,
    ):
        if isinstance(focal_point, _BridgeMobject):
            self.focal_point = focal_point
        else:
            try:
                self.focal_point = _np.array(_vec3(focal_point))
            except (IndexError, KeyError, TypeError) as error:
                raise TypeError(
                    "manimlib.animation.specialized.Broadcast expects "
                    "a Mobject or 3D point; got " + type(focal_point).__name__
                ) from error
        self.small_radius = float(small_radius)
        self.big_radius = float(big_radius)
        self.start_stroke_width = float(start_stroke_width)
        try:
            self.n_circles = _operator.index(n_circles)
        except TypeError:
            raise TypeError("Broadcast n_circles must be an integer") from None
        if self.n_circles <= 0:
            raise ValueError("Broadcast n_circles must be greater than zero")
        for name, value in (
            ("small_radius", self.small_radius),
            ("big_radius", self.big_radius),
            ("start_stroke_width", self.start_stroke_width),
        ):
            if not _math.isfinite(value) or value < 0:
                raise ValueError(
                    "Broadcast " + name + " must be non-negative and finite"
                )
        self.color = color
        self.remover = bool(remover)
        self.circles = VGroup()
        for _ in range(self.n_circles):
            circle = Circle(
                radius=self.big_radius,
                stroke_color=_BLACK,
                stroke_width=0,
            )
            circle.add_updater(lambda ring: ring.move_to(self.focal_point))
            circle.save_state()
            circle.set_width(2.0 * self.small_radius)
            circle.set_stroke(self.color, self.start_stroke_width)
            self.circles.add(circle)
        super().__init__(
            *(
                _BroadcastRestore(circle, remover=self.remover)
                for circle in self.circles
            ),
            run_time=run_time,
            lag_ratio=lag_ratio,
            **kwargs,
        )

    def _native_params(self):
        return {"remover": self.remover}


class FlashyFadeIn(AnimationGroup):
    _native_kind = "flashy_fade_in"

    def __init__(
        self,
        vmobject,
        stroke_width=2.0,
        fade_lag=0.0,
        time_width=1.0,
        **kwargs,
    ):
        if not isinstance(vmobject, VMobject):
            raise TypeError("FlashyFadeIn expects a VMobject")
        self.stroke_width = float(stroke_width)
        self.fade_lag = float(fade_lag)
        self.time_width = float(time_width)
        self.outline = vmobject.copy()
        self.outline.set_fill(opacity=0)
        self.outline.set_stroke(width=self.stroke_width, opacity=1)
        rate_func = kwargs.get("rate_func", _smooth_rate)
        fade_rate = _FMN_ROOT.squish_rate_func(
            rate_func,
            self.fade_lag,
            1.0,
        )
        super().__init__(
            FadeIn(vmobject, rate_func=fade_rate),
            VShowPassingFlash(self.outline, time_width=self.time_width),
            **kwargs,
        )


class AnimationOnSurroundingRectangle(AnimationGroup):
    RectAnimationType = Animation

    def __init__(
        self,
        mobject,
        stroke_width=2.0,
        stroke_color=_YELLOW,
        buff=_SMALL_BUFF,
        **kwargs,
    ):
        if not isinstance(mobject, _BridgeMobject):
            raise TypeError(
                type(self).__name__ + " expects a Mobject to surround"
            )
        if self.RectAnimationType is Animation:
            raise NotImplementedError(
                "manimlib.animation.indication.AnimationOnSurroundingRectangle "
                "has no native RectAnimationType; use a concrete subclass"
            )
        self.mobject_to_surround = mobject
        self.rectangle = SurroundingRectangle(
            mobject,
            stroke_width=stroke_width,
            stroke_color=stroke_color,
            buff=buff,
        )
        self.rectangle.add_updater(
            lambda rectangle: rectangle.move_to(self.mobject_to_surround)
        )
        super().__init__(self.RectAnimationType(self.rectangle, **kwargs))


class ShowCreationThenDestructionAround(AnimationOnSurroundingRectangle):
    _native_kind = "show_creation_then_destruction_around"
    RectAnimationType = ShowCreationThenDestruction


class ShowCreationThenFadeAround(AnimationOnSurroundingRectangle):
    _native_kind = "show_creation_then_fade_around"
    RectAnimationType = ShowCreationThenFadeOut


class LaggedStartMap(LaggedStart):
    def __init__(self, anim_func, group, run_time=2.0, lag_ratio=0.05, **kwargs):
        # Reference composition.py:166 verbatim: one animation per member,
        # through the native lagged_start composition. Named refusals for
        # a non-callable constructor and a non-Mobject family beat the
        # Reference's bare mid-map crashes (fm-5wq.4.95).
        if not callable(anim_func):
            raise TypeError(
                "LaggedStartMap requires an animation constructor to map; "
                "got " + type(anim_func).__name__
            )
        if not isinstance(group, Mobject):
            raise TypeError(
                "LaggedStartMap requires a Mobject family to map over; "
                "got " + type(group).__name__
            )
        anim_kwargs = dict(kwargs)
        anim_kwargs.pop("lag_ratio", None)
        super().__init__(
            *(anim_func(submob, **anim_kwargs) for submob in group),
            run_time=run_time,
            lag_ratio=lag_ratio,
        )


class BraceLabel(VMobject):
    """A live native brace and Scribe label under the Reference API.

    Atlas owns the parametric brace geometry through :class:`Brace`, while
    ``label_constructor`` selects Scribe's bundled-font ``Tex`` or
    ``TexText`` route. Keeping the two caller-visible children explicit is
    important: Reference scenes retain and mutate ``brace`` and ``label`` by
    identity after construction.
    """

    label_constructor = Tex

    def __init__(
        self,
        obj,
        text,
        brace_direction=_DOWN,
        label_scale=1.0,
        label_buff=_DEFAULT_MOBJECT_TO_MOBJECT_BUFF,
        **kwargs,
    ):
        super().__init__(**kwargs)
        self.brace_direction = _np.array(_vec3(brace_direction))
        self.label_scale = float(label_scale)
        self.label_buff = float(label_buff)

        if isinstance(obj, list):
            obj = VGroup(*obj)
        self.brace = Brace(obj, self.brace_direction, **kwargs)
        self.label = self.label_constructor(*_listify(text), **kwargs)
        self.label.scale(self.label_scale)
        self.brace.put_at_tip(self.label, buff=self.label_buff)
        self.set_submobjects([self.brace, self.label])

    def creation_anim(self, label_anim=FadeIn, brace_anim=GrowFromCenter):
        return AnimationGroup(brace_anim(self.brace), label_anim(self.label))

    def shift_brace(self, obj, **kwargs):
        if isinstance(obj, list):
            obj = VGroup(*obj)
        self.brace = Brace(obj, self.brace_direction, **kwargs)
        self.brace.put_at_tip(self.label)
        self.submobjects[0] = self.brace
        return self

    def change_label(self, *text, **kwargs):
        self.label = self.label_constructor(*text, **kwargs)
        if self.label_scale != 1:
            self.label.scale(self.label_scale)
        self.brace.put_at_tip(self.label)
        self.submobjects[1] = self.label
        return self

    def change_brace_label(self, obj, *text):
        self.shift_brace(obj)
        self.change_label(*text)
        return self

    def copy(self):
        # The shared graph copier remaps direct Mobject-valued attributes to
        # their copied family members. Defining the method here preserves the
        # Reference owner/signature and makes that brace/label remap explicit.
        return super().copy()


class BraceText(BraceLabel):
    label_constructor = TexText



def _portal_embed(scene=None, namespace=None):
    """Enter IPython with an explicit scene/namespace capability boundary."""

    try:
        ipython = _importlib.import_module("IPython")
    except ImportError as error:
        raise _CapabilityError(
            "IPython is not installed; use checkpoint_paste(scene) for a "
            "dependency-free Studio handoff"
        ) from error
    user_ns = {} if namespace is None else dict(namespace)
    if scene is not None:
        user_ns.setdefault("scene", scene)
    return ipython.embed(user_ns=user_ns)


def _portal_checkpoint_paste(scene):
    """Return the canonical SceneState bytes consumed by Studio checkpoints."""

    if not isinstance(scene, _SceneCore):
        raise TypeError("checkpoint_paste expects a Scene")
    return scene._checkpoint_bytes()


def _placeholder_function(module_name, name):
    def unavailable(*args, **kwargs):
        del args, kwargs
        raise NotImplementedError(
            f"{module_name}.{name} is present in the parity surface but its "
            "semantic binding has not landed"
        )

    unavailable.__name__ = name
    unavailable.__qualname__ = name
    unavailable.__module__ = module_name
    return unavailable


def _placeholder_method(module_name, owner, name):
    def unavailable(self, *args, **kwargs):
        del self, args, kwargs
        raise NotImplementedError(
            f"{module_name}.{owner}.{name} is present in the parity surface "
            "but its semantic binding has not landed"
        )

    unavailable.__name__ = name
    unavailable.__qualname__ = f"{owner}.{name}"
    unavailable.__module__ = module_name
    return unavailable


def _surface_init(self, *args, **kwargs):
    self.args = args
    self.__dict__.update(kwargs)


def _schema_init_refusal(module_name, class_name):
    """Refuse a generated core class whose own constructor is unbound.

    Inheriting a usable ancestor constructor is not import compatibility: it
    silently applies the wrong defaults and often interprets positional
    arguments as unrelated fields. Keep the class subclassable, but make
    construction name the exact missing semantic seam.
    """

    def unavailable(self, *args, **kwargs):
        del self, args, kwargs
        raise NotImplementedError(
            f"{module_name}.{class_name} declares class-specific constructor "
            "semantics that have not landed; refusing to inherit an "
            "ancestor's incompatible defaults"
        )

    unavailable.__name__ = "__init__"
    unavailable.__qualname__ = f"{class_name}.__init__"
    unavailable.__module__ = module_name
    return unavailable


class _UnavailablePygletWindow:
    """Import-compatible base for the deliberately absent pyglet gateway."""

    def __init__(self, *args, **kwargs):
        del args, kwargs
        raise _CapabilityError(
            "the Reference's PygletWindow gateway is unavailable; "
            "FrankenManim Studio owns interactive windows"
        )


_UnavailablePygletWindow.__name__ = "PygletWindow"
_UnavailablePygletWindow.__qualname__ = "PygletWindow"
_UnavailablePygletWindow.__module__ = "manimlib.window"


class Window(_UnavailablePygletWindow):
    """Reference pyglet/moderngl_window surface.

    Construction is import-compatible and names the Studio window owner.
    GUI methods stay schema placeholders until Studio binds a host window.
    """

    cursor = True
    fullscreen = False
    gl_version = (3, 3)
    resizable = True
    vsync = True

    def __init__(
        self,
        scene=None,
        position_string="UR",
        monitor_index=1,
        full_screen=False,
        size=None,
        position=None,
        samples=0,
    ):
        del scene, position_string, monitor_index, full_screen, size, position, samples
        raise _CapabilityError(
            "the Reference window gateway is unavailable; "
            "FrankenManim Studio owns interactive windows"
        )


class SceneFileWriter:
    """Reference write-surface constructor over Reel path/config knobs.

    Construction records the scene and encode knobs without mkdir or ffmpeg.
    Path queries are pure. Movie encode/mux stays the ffmpeg Reel boundary.
    """

    def __init__(
        self,
        scene,
        write_to_movie=False,
        subdivide_output=False,
        png_mode="RGBA",
        save_last_frame=False,
        movie_file_extension=".mp4",
        output_directory=".",
        file_name=None,
        open_file_upon_completion=False,
        show_file_location_upon_completion=False,
        quiet=False,
        total_frames=0,
        progress_description_len=40,
        ffmpeg_bin="ffmpeg",
        video_codec="libx264",
        pixel_format="yuv420p",
        saturation=1.0,
        gamma=1.0,
        **kwargs,
    ):
        if kwargs:
            raise TypeError(
                "unexpected keyword arguments: " + ", ".join(sorted(kwargs))
            )
        if not isinstance(scene, Scene):
            raise TypeError("SceneFileWriter scene must be a Scene")
        self.scene = scene
        self.write_to_movie = bool(write_to_movie)
        self.subdivide_output = bool(subdivide_output)
        self.png_mode = str(png_mode)
        self.save_last_frame = bool(save_last_frame)
        self.movie_file_extension = str(movie_file_extension)
        self.output_directory = str(output_directory)
        self.file_name = None if file_name is None else str(file_name)
        self.open_file_upon_completion = bool(open_file_upon_completion)
        self.show_file_location_upon_completion = bool(
            show_file_location_upon_completion
        )
        self.quiet = bool(quiet)
        self.total_frames = int(total_frames)
        self.progress_description_len = int(progress_description_len)
        self.ffmpeg_bin = str(ffmpeg_bin)
        self.video_codec = str(video_codec)
        self.pixel_format = str(pixel_format)
        self.saturation = float(saturation)
        self.gamma = float(gamma)
        self.image_file_path = self.get_image_file_path()
        self.movie_file_path = self.get_movie_file_path()

    def _movie_extension(self):
        extension = self.movie_file_extension
        if not extension.startswith("."):
            return "." + extension
        return extension

    def get_output_file_name(self):
        if self.file_name:
            return self.file_name
        return type(self.scene).__name__

    def get_output_file_rootname(self):
        return str(_pathlib.Path(self.output_directory) / self.get_output_file_name())

    def get_image_file_path(self):
        return self.get_output_file_rootname() + ".png"

    def get_movie_file_path(self):
        return self.get_output_file_rootname() + self._movie_extension()

    def get_insert_file_path(self, index):
        return (
            self.get_output_file_rootname()
            + f"_{int(index)}"
            + self._movie_extension()
        )

    def get_next_partial_movie_path(self):
        plays = int(getattr(self.scene, "num_plays", 0))
        directory = (
            _pathlib.Path(self.output_directory)
            / "partial_movie_files"
            / self.get_output_file_name()
        )
        return str(directory / f"{plays:05}{self._movie_extension()}")

    def should_open_file(self):
        return bool(self.open_file_upon_completion)

    def has_progress_display(self):
        return bool(self.write_to_movie) and not bool(self.quiet)

    def use_fast_encoding(self):
        return self.pixel_format != "yuv444p"

    def open_movie_pipe(self, file_path):
        del file_path
        raise _CapabilityError(
            "SceneFileWriter movie encode is the ffmpeg Reel boundary; "
            "use native PNG/y4m output or the fmn CLI"
        )

    def close_movie_pipe(self):
        raise _CapabilityError(
            "SceneFileWriter movie encode is the ffmpeg Reel boundary; "
            "use native PNG/y4m output or the fmn CLI"
        )

    def write_frame(self, camera):
        del camera
        raise _CapabilityError(
            "SceneFileWriter movie encode is the ffmpeg Reel boundary; "
            "use native PNG/y4m output or the fmn CLI"
        )


class LatexError(Exception):
    """Reference-compatible native-typesetting failure signal."""


def _refuse_shader_wrapper():
    raise _CapabilityError(
        "the Reference OpenGL ShaderWrapper is excluded; "
        "Lumen owns rasterization and custom GLSL is outside the "
        "compatibility claim"
    )


class ShaderWrapper:
    """Reference moderngl program wrapper. Construction names the Lumen owner."""

    def __init__(
        self,
        ctx,
        vert_data,
        shader_folder=None,
        mobject_uniforms=None,
        texture_paths=None,
        depth_test=False,
        render_primitive=5,
        code_replacements=None,
    ):
        del (
            ctx,
            vert_data,
            shader_folder,
            mobject_uniforms,
            texture_paths,
            depth_test,
            render_primitive,
            code_replacements,
        )
        _refuse_shader_wrapper()


class VShaderWrapper(ShaderWrapper):
    """Reference VMobject fill/stroke shader wrapper. Same Lumen exclusion."""

    def __init__(
        self,
        ctx,
        vert_data,
        shader_folder=None,
        mobject_uniforms=None,
        texture_paths=None,
        depth_test=False,
        render_primitive=4,
        code_replacements=None,
        program_type=None,
        stroke_behind=False,
    ):
        del (
            ctx,
            vert_data,
            shader_folder,
            mobject_uniforms,
            texture_paths,
            depth_test,
            render_primitive,
            code_replacements,
            program_type,
            stroke_behind,
        )
        _refuse_shader_wrapper()


def _constant_expression(detail, env):
    """Evaluate only the closed expression grammar used by schema constants.

    This deliberately is not Python evaluation. Unknown names, private
    attributes, calls outside the three listed constructors, and every other
    syntax form refuse so runtime-shaped schema entries remain symbolic.
    """

    def visit(node):
        if isinstance(node, _ast.Constant):
            return node.value
        if isinstance(node, _ast.List):
            return [visit(item) for item in node.elts]
        if isinstance(node, _ast.Tuple):
            return tuple(visit(item) for item in node.elts)
        if isinstance(node, _ast.Dict):
            return {visit(key): visit(value) for key, value in zip(node.keys, node.values)}
        if isinstance(node, _ast.Name):
            if node.id not in env:
                raise ValueError(f"unknown schema constant name: {node.id}")
            return env[node.id]
        if isinstance(node, _ast.Attribute):
            if node.attr.startswith("_"):
                raise ValueError("private schema constant attributes are forbidden")
            return getattr(visit(node.value), node.attr)
        if isinstance(node, _ast.Subscript):
            return visit(node.value)[visit(node.slice)]
        if isinstance(node, _ast.UnaryOp):
            operand = visit(node.operand)
            if isinstance(node.op, _ast.UAdd):
                return +operand
            if isinstance(node.op, _ast.USub):
                return -operand
            raise ValueError("unsupported schema constant unary operator")
        if isinstance(node, _ast.BinOp):
            left = visit(node.left)
            right = visit(node.right)
            if isinstance(node.op, _ast.Add):
                return left + right
            if isinstance(node.op, _ast.Sub):
                return left - right
            if isinstance(node.op, _ast.Mult):
                return left * right
            if isinstance(node.op, _ast.Div):
                return left / right
            if isinstance(node.op, _ast.BitOr):
                return left | right
            raise ValueError("unsupported schema constant binary operator")
        if isinstance(node, _ast.BoolOp) and isinstance(node.op, _ast.Or):
            result = None
            for item in node.values:
                result = visit(item)
                if result:
                    return result
            return result
        if isinstance(node, _ast.Call) and not node.keywords:
            args = [visit(arg) for arg in node.args]
            if isinstance(node.func, _ast.Name):
                if node.func.id == "dict":
                    return dict(*args)
                if node.func.id == "list":
                    return list(*args)
                if node.func.id == "version" and "version" in env:
                    return env["version"](*args)
            if (
                isinstance(node.func, _ast.Attribute)
                and node.func.attr == "values"
                and not args
            ):
                owner = visit(node.func.value)
                if isinstance(owner, dict):
                    return owner.values()
            raise ValueError("unsupported schema constant call")
        raise ValueError(f"unsupported schema constant syntax: {type(node).__name__}")

    return visit(_ast.parse(detail, mode="eval").body)


def _constant(detail):
    if detail == "-":
        return None
    if detail == "True":
        return True
    if detail == "False":
        return False
    try:
        return int(detail)
    except ValueError:
        pass
    try:
        return float(detail)
    except ValueError:
        pass
    # Materialize the ledger's literal forms. The direction/coordinate
    # constants MUST be real NumPy arrays — the Reference's are, and the
    # corpus multiplies and adds them (`0.25 * DOWN`); a string here
    # turns every corpus import into a TypeError.
    if detail.startswith("np.array(") and detail.endswith(")"):
        numpy = _importlib.import_module("numpy")
        return numpy.array(_constant_expression(detail[len("np.array(") : -1], {}))
    if detail == "np.pi":
        return _math.pi
    try:
        # Quoted strings, tuples, dicts, and other pure literals.
        return _constant_expression(detail, {})
    except (AttributeError, KeyError, TypeError, ValueError, SyntaxError):
        # Symbolic defaults (config references, derived expressions) keep
        # their declared spelling until the constants environment lands.
        return detail


def _schema_rows():
    rows = []
    in_symbols = False
    for raw_line in _API_SCHEMA_TSV.splitlines():
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("[") and line.endswith("]"):
            in_symbols = line == "[symbols]"
            continue
        if not in_symbols:
            continue
        columns = raw_line.split("\t", 5)
        if len(columns) == 6:
            rows.append(tuple(columns))
    return rows


def _ensure_module(name):
    existing = _sys.modules.get(name)
    if existing is not None:
        return existing
    module = _types.ModuleType(name)
    module.__package__ = name
    module.__path__ = []
    _sys.modules[name] = module
    if "." in name:
        parent_name, child_name = name.rsplit(".", 1)
        parent = _ensure_module(parent_name)
        setattr(parent, child_name, module)
    return module


def _base_names(detail):
    result = []
    for raw in detail.split(","):
        name = raw.strip().split("[", 1)[0].split(".")[-1]
        if not name or name in {"-", "object", "ABC", "Generic", "Protocol"}:
            continue
        result.append(name)
    return result


def _pinned_manim_config():
    """The Reference's default_config.yml at the pin, as namespaces.

    Only the sections the ledger's symbolic constant defaults consult;
    values are the pinned file's, verbatim (fm-d3gt).
    """
    ns = _types.SimpleNamespace
    return ns(
        camera=ns(resolution=(1920, 1080), fps=30, background_color="#333333", background_opacity=1.0),
        sizes=ns(
            frame_height=8.0,
            small_buff=0.1,
            med_small_buff=0.25,
            med_large_buff=0.5,
            large_buff=1.0,
            default_mobject_to_edge_buff=0.5,
            default_mobject_to_mobject_buff=0.25,
        ),
        key_bindings=ns(
            pan_3d="d", pan="f", reset="r", quit="q", select="s", unselect="u",
            grab="g", x_grab="h", y_grab="v", z_grab="z", resize="t", color="c",
            information="i", cursor="k",
        ),
        colors=ns(
            blue_e="#1C758A", blue_d="#29ABCA", blue_c="#58C4DD", blue_b="#9CDCEB", blue_a="#C7E9F1",
            teal_e="#49A88F", teal_d="#55C1A7", teal_c="#5CD0B3", teal_b="#76DDC0", teal_a="#ACEAD7",
            green_e="#699C52", green_d="#77B05D", green_c="#83C167", green_b="#A6CF8C", green_a="#C9E2AE",
            yellow_e="#E8C11C", yellow_d="#F4D345", yellow_c="#FFFF00", yellow_b="#FFEA94", yellow_a="#FFF1B6",
            gold_e="#C78D46", gold_d="#E1A158", gold_c="#F0AC5F", gold_b="#F9B775", gold_a="#F7C797",
            red_e="#CF5044", red_d="#E65A4C", red_c="#FC6255", red_b="#FF8080", red_a="#F7A1A3",
            maroon_e="#94424F", maroon_d="#A24D61", maroon_c="#C55F73", maroon_b="#EC92AB", maroon_a="#ECABC1",
            purple_e="#644172", purple_d="#715582", purple_c="#9A72AC", purple_b="#B189C6", purple_a="#CAA3E8",
            grey_e="#222222", grey_d="#444444", grey_c="#888888", grey_b="#BBBBBB", grey_a="#DDDDDD",
            white="#FFFFFF", black="#000000", grey_brown="#736357", dark_brown="#8B4513",
            light_brown="#CD853F", pink="#D147BD", light_pink="#DC75CD", green_screen="#00FF00",
            orange="#FF862F", pure_red="#FF0000", pure_green="#00FF00", pure_blue="#0000FF",
        ),
        vmobject=ns(default_stroke_width=4.0, default_stroke_color="#DDDDDD", default_fill_color="#888888"),
        mobject=ns(default_mobject_color="#FFFFFF", default_light_color="#BBBBBB"),
        text=ns(font="Consolas", alignment="LEFT", font_size_for_unit_height=144),
        tex=ns(template="default", font_size_for_unit_height=144),
    )


def _resolve_symbolic_constants(rows):
    """Fixpoint-evaluate the ledger's symbolic constant defaults.

    The corpus does arithmetic on these at import time (FRAME_WIDTH - 1,
    FRAME_Y_RADIUS * DOWN), so the derived spellings must become real
    values. Literals resolve through _constant; symbolic spellings
    evaluate in a closed environment seeded with the pinned
    manim_config, NumPy, and every constant already resolved — repeated
    until no pass makes progress. Spellings that never resolve (runtime
    machinery like EventDispatcher()) keep their declared string, the
    pre-existing behavior.
    """
    numpy = _importlib.import_module("numpy")
    env = {
        "manim_config": _pinned_manim_config(),
        "np": numpy,
        "version": lambda _name: "1.7.2",
    }
    pending = {}
    resolved = {}
    # Schema rows are (module, name, kind, origin, exported, detail).
    for module_name, qualified, kind, _origin, _exported, detail in rows:
        if kind != "constant" or "." in qualified:
            continue
        value = _constant(detail)
        if isinstance(value, str) and value == detail and detail != "-":
            pending[(module_name, qualified)] = detail
        else:
            resolved[(module_name, qualified)] = value
            env.setdefault(qualified, value)
    progressed = True
    while progressed and pending:
        progressed = False
        for key in list(pending):
            detail = pending[key]
            try:
                value = _constant_expression(detail, env)
            except (AttributeError, KeyError, TypeError, ValueError, SyntaxError):
                continue
            resolved[key] = value
            env.setdefault(key[1], value)
            del pending[key]
            progressed = True
    for key, detail in pending.items():
        resolved[key] = detail
    return resolved


def _install_schema_surface():
    root = _FMN_MODULE
    _sys.modules.setdefault(__name__, root)
    root.__path__ = []
    rows = _schema_rows()
    for module_name, *_ in rows:
        _ensure_module(module_name)
    _resolved_constants = _resolve_symbolic_constants(rows)

    specials = {
        (
            "manimlib.mobject.mobject",
            "_AnimationBuilder",
        ): _AnimationBuilder,
        (
            "manimlib.mobject.mobject",
            "_UpdaterBuilder",
        ): _UpdaterBuilder,
        (
            "manimlib.mobject.mobject",
            "_FunctionalUpdaterBuilder",
        ): _FunctionalUpdaterBuilder,
        ("manimlib.mobject.mobject", "Mobject"): Mobject,
        ("manimlib.mobject.mobject", "Group"): Group,
        ("manimlib.mobject.mobject", "Point"): Point,
        ("manimlib.event_handler.event_type", "EventType"): EventType,
        ("manimlib.event_handler.event_listner", "EventListener"): EventListener,
        (
            "manimlib.event_handler.event_dispatcher",
            "EventDispatcher",
        ): EventDispatcher,
        ("manimlib.mobject.interactive", "MotionMobject"): MotionMobject,
        ("manimlib.mobject.interactive", "Button"): Button,
        ("manimlib.mobject.interactive", "ControlMobject"): ControlMobject,
        ("manimlib.mobject.interactive", "Checkbox"): Checkbox,
        (
            "manimlib.mobject.interactive",
            "EnableDisableButton",
        ): EnableDisableButton,
        (
            "manimlib.mobject.interactive",
            "LinearNumberSlider",
        ): LinearNumberSlider,
        ("manimlib.mobject.interactive", "ColorSliders"): ColorSliders,
        ("manimlib.mobject.interactive", "Textbox"): Textbox,
        ("manimlib.mobject.interactive", "ControlPanel"): ControlPanel,
        ("manimlib.mobject.types.vectorized_mobject", "VMobject"): VMobject,
        ("manimlib.mobject.svg.svg_mobject", "SVGMobject"): SVGMobject,
        (
            "manimlib.mobject.svg.string_mobject",
            "StringMobject",
        ): StringMobject,
        ("manimlib.camera.camera_frame", "CameraFrame"): CameraFrame,
        ("manimlib.camera.camera", "Camera"): Camera,
        ("manimlib.camera.camera", "ThreeDCamera"): ThreeDCamera,
        ("manimlib.mobject.types.vectorized_mobject", "VGroup"): VGroup,
        (
            "manimlib.mobject.types.vectorized_mobject",
            "VectorizedPoint",
        ): VectorizedPoint,
        (
            "manimlib.mobject.types.vectorized_mobject",
            "CurvesAsSubmobjects",
        ): CurvesAsSubmobjects,
        (
            "manimlib.mobject.types.vectorized_mobject",
            "DashedVMobject",
        ): DashedVMobject,
        (
            "manimlib.mobject.types.vectorized_mobject",
            "VHighlight",
        ): VHighlight,
        ("manimlib.mobject.functions", "ParametricCurve"): ParametricCurve,
        ("manimlib.mobject.functions", "FunctionGraph"): FunctionGraph,
        ("manimlib.mobject.functions", "ImplicitFunction"): ImplicitFunction,
        ("manimlib.mobject.geometry", "CubicBezier"): CubicBezier,
        ("manimlib.mobject.boolean_ops", "Union"): Union,
        ("manimlib.mobject.boolean_ops", "Difference"): Difference,
        ("manimlib.mobject.boolean_ops", "Intersection"): Intersection,
        ("manimlib.mobject.boolean_ops", "Exclusion"): Exclusion,
        ("manimlib.mobject.geometry", "Polygon"): Polygon,
        ("manimlib.mobject.geometry", "Polyline"): Polyline,
        ("manimlib.mobject.geometry", "RegularPolygon"): RegularPolygon,
        ("manimlib.mobject.geometry", "Triangle"): Triangle,
        ("manimlib.mobject.geometry", "ArrowTip"): ArrowTip,
        ("manimlib.mobject.geometry", "Rectangle"): Rectangle,
        ("manimlib.mobject.geometry", "RoundedRectangle"): RoundedRectangle,
        ("manimlib.mobject.geometry", "Square"): Square,
        ("manimlib.mobject.frame", "ScreenRectangle"): ScreenRectangle,
        ("manimlib.mobject.frame", "FullScreenRectangle"): FullScreenRectangle,
        (
            "manimlib.mobject.frame",
            "FullScreenFadeRectangle",
        ): FullScreenFadeRectangle,
        ("manimlib.mobject.shape_matchers", "SurroundingRectangle"): SurroundingRectangle,
        ("manimlib.mobject.shape_matchers", "BackgroundRectangle"): BackgroundRectangle,
        ("manimlib.mobject.shape_matchers", "Cross"): Cross,
        ("manimlib.mobject.shape_matchers", "Underline"): Underline,
        (
            "manimlib.mobject.svg.special_tex",
            "TexTextFromPresetString",
        ): TexTextFromPresetString,
        ("manimlib.mobject.svg.special_tex", "BulletedList"): BulletedList,
        ("manimlib.mobject.svg.special_tex", "Title"): Title,
        ("manimlib.mobject.svg.drawings", "Checkmark"): Checkmark,
        ("manimlib.mobject.svg.drawings", "Exmark"): Exmark,
        ("manimlib.animation.update", "UpdateFromFunc"): UpdateFromFunc,
        ("manimlib.animation.update", "UpdateFromAlphaFunc"): UpdateFromAlphaFunc,
        (
            "manimlib.animation.update",
            "MaintainPositionRelativeTo",
        ): MaintainPositionRelativeTo,
        ("manimlib.animation.speed", "ChangeSpeed"): ChangeSpeed,
        ("manimlib.animation.specialized", "Delay"): Delay,
        ("manimlib.mobject.changing", "AnimatedBoundary"): AnimatedBoundary,
        ("manimlib.mobject.changing", "TracedPath"): TracedPath,
        ("manimlib.animation.numbers", "ChangingDecimal"): ChangingDecimal,
        ("manimlib.animation.numbers", "ChangeDecimalToValue"): ChangeDecimalToValue,
        ("manimlib.animation.numbers", "CountInFrom"): CountInFrom,
        ("manimlib.animation.creation", "AddTextWordByWord"): AddTextWordByWord,
        (
            "manimlib.animation.creation",
            "AddTextLetterByLetter",
        ): AddTextLetterByLetter,
        ("manimlib.mobject.geometry", "TipableVMobject"): TipableVMobject,
        ("manimlib.mobject.geometry", "Arc"): Arc,
        ("manimlib.mobject.geometry", "ArcBetweenPoints"): ArcBetweenPoints,
        ("manimlib.mobject.geometry", "CurvedArrow"): CurvedArrow,
        ("manimlib.mobject.geometry", "CurvedDoubleArrow"): CurvedDoubleArrow,
        ("manimlib.mobject.geometry", "Circle"): Circle,
        ("manimlib.mobject.geometry", "Dot"): Dot,
        ("manimlib.mobject.geometry", "SmallDot"): SmallDot,
        ("manimlib.mobject.geometry", "Ellipse"): Ellipse,
        ("manimlib.mobject.geometry", "AnnularSector"): AnnularSector,
        ("manimlib.mobject.geometry", "Sector"): Sector,
        ("manimlib.mobject.geometry", "Annulus"): Annulus,
        ("manimlib.mobject.types.point_cloud_mobject", "PMobject"): PMobject,
        ("manimlib.mobject.types.point_cloud_mobject", "PGroup"): PGroup,
        ("manimlib.mobject.numbers", "DecimalNumber"): DecimalNumber,
        ("manimlib.mobject.numbers", "Integer"): Integer,
        ("manimlib.mobject.matrix", "Matrix"): Matrix,
        ("manimlib.mobject.matrix", "DecimalMatrix"): DecimalMatrix,
        ("manimlib.mobject.matrix", "IntegerMatrix"): IntegerMatrix,
        ("manimlib.mobject.matrix", "TexMatrix"): TexMatrix,
        ("manimlib.mobject.matrix", "MobjectMatrix"): MobjectMatrix,
        ("manimlib.mobject.types.dot_cloud", "DotCloud"): DotCloud,
        ("manimlib.mobject.types.dot_cloud", "TrueDot"): TrueDot,
        ("manimlib.mobject.types.dot_cloud", "GlowDots"): GlowDots,
        ("manimlib.mobject.types.dot_cloud", "GlowDot"): GlowDot,
        (
            "manimlib.mobject.types.image_mobject",
            "ImageMobject",
        ): ImageMobject,
        (
            "manimlib.mobject.svg.svg_mobject",
            "VMobjectFromSVGPath",
        ): VMobjectFromSVGPath,
        ("manimlib.mobject.vector_field", "VectorField"): VectorField,
        ("manimlib.mobject.vector_field", "StreamLines"): StreamLines,
        (
            "manimlib.mobject.vector_field",
            "AnimatedStreamLines",
        ): AnimatedStreamLines,
        (
            "manimlib.mobject.vector_field",
            "TimeVaryingVectorField",
        ): TimeVaryingVectorField,
        ("manimlib.mobject.types.surface", "Surface"): Surface,
        ("manimlib.mobject.types.surface", "SGroup"): SGroup,
        ("manimlib.mobject.types.surface", "ParametricSurface"): ParametricSurface,
        ("manimlib.mobject.types.surface", "ThreeDModel"): ThreeDModel,
        ("manimlib.mobject.types.surface", "TexturedSurface"): TexturedSurface,
        ("manimlib.mobject.types.surface", "TexturedGeometry"): TexturedGeometry,
        ("manimlib.mobject.three_dimensions", "Sphere"): Sphere,
        ("manimlib.mobject.three_dimensions", "Torus"): Torus,
        ("manimlib.mobject.three_dimensions", "Cylinder"): Cylinder,
        ("manimlib.mobject.three_dimensions", "Cone"): Cone,
        ("manimlib.mobject.three_dimensions", "Line3D"): Line3D,
        ("manimlib.mobject.three_dimensions", "Disk3D"): Disk3D,
        ("manimlib.mobject.three_dimensions", "Square3D"): Square3D,
        ("manimlib.mobject.three_dimensions", "Cube"): Cube,
        ("manimlib.mobject.three_dimensions", "Prism"): Prism,
        ("manimlib.mobject.three_dimensions", "VGroup3D"): VGroup3D,
        ("manimlib.mobject.three_dimensions", "VCube"): VCube,
        ("manimlib.mobject.three_dimensions", "VPrism"): VPrism,
        ("manimlib.mobject.three_dimensions", "Dodecahedron"): Dodecahedron,
        ("manimlib.mobject.three_dimensions", "Tetrahedron"): Tetrahedron,
        ("manimlib.mobject.three_dimensions", "Prismify"): Prismify,
        ("manimlib.mobject.three_dimensions", "SurfaceMesh"): SurfaceMesh,
        ("manimlib.mobject.geometry", "Elbow"): Elbow,
        ("manimlib.mobject.geometry", "Line"): Line,
        ("manimlib.mobject.geometry", "DashedLine"): DashedLine,
        ("manimlib.mobject.geometry", "TangentLine"): TangentLine,
        ("manimlib.mobject.geometry", "StrokeArrow"): StrokeArrow,
        ("manimlib.mobject.geometry", "Arrow"): Arrow,
        ("manimlib.mobject.geometry", "Vector"): Vector,
        ("manimlib.mobject.number_line", "NumberLine"): NumberLine,
        ("manimlib.mobject.number_line", "UnitInterval"): UnitInterval,
        ("manimlib.mobject.number_line", "Slider"): Slider,
        ("manimlib.mobject.probability", "SampleSpace"): SampleSpace,
        ("manimlib.mobject.probability", "BarChart"): BarChart,
        ("manimlib.mobject.coordinate_systems", "CoordinateSystem"): CoordinateSystem,
        ("manimlib.mobject.coordinate_systems", "Axes"): Axes,
        ("manimlib.mobject.coordinate_systems", "ThreeDAxes"): ThreeDAxes,
        ("manimlib.mobject.coordinate_systems", "NumberPlane"): NumberPlane,
        ("manimlib.mobject.coordinate_systems", "ComplexPlane"): ComplexPlane,
        ("manimlib.mobject.svg.text_mobject", "MarkupText"): MarkupText,
        ("manimlib.mobject.svg.text_mobject", "Text"): Text,
        ("manimlib.mobject.svg.text_mobject", "_Alignment"): _Alignment,
        ("manimlib.mobject.svg.text_mobject", "Code"): Code,
        ("manimlib.mobject.svg.tex_mobject", "Tex"): Tex,
        ("manimlib.mobject.svg.tex_mobject", "TexText"): TexText,
        (
            "manimlib.mobject.svg.old_tex_mobject",
            "SingleStringTex",
        ): SingleStringTex,
        ("manimlib.mobject.svg.old_tex_mobject", "OldTex"): OldTex,
        ("manimlib.mobject.svg.old_tex_mobject", "OldTexText"): OldTexText,
        ("manimlib.mobject.changing", "TracingTail"): TracingTail,
        ("manimlib.mobject.value_tracker", "ValueTracker"): ValueTracker,
        ("manimlib.mobject.value_tracker", "ExponentialValueTracker"): ExponentialValueTracker,
        ("manimlib.mobject.value_tracker", "ComplexValueTracker"): ComplexValueTracker,
        ("manimlib.animation.creation", "ShowPartial"): ShowPartial,
        ("manimlib.animation.creation", "ShowCreation"): ShowCreation,
        ("manimlib.animation.creation", "Uncreate"): Uncreate,
        (
            "manimlib.animation.creation",
            "DrawBorderThenFill",
        ): DrawBorderThenFill,
        ("manimlib.animation.creation", "Write"): Write,
        (
            "manimlib.animation.creation",
            "ShowIncreasingSubsets",
        ): ShowIncreasingSubsets,
        (
            "manimlib.animation.creation",
            "ShowSubmobjectsOneByOne",
        ): ShowSubmobjectsOneByOne,
        ("manimlib.animation.fading", "Fade"): Fade,
        ("manimlib.animation.fading", "FadeIn"): FadeIn,
        ("manimlib.animation.fading", "FadeOut"): FadeOut,
        ("manimlib.animation.fading", "FadeInFromLarge"): FadeInFromLarge,
        ("manimlib.animation.fading", "VFadeIn"): VFadeIn,
        ("manimlib.animation.fading", "VFadeOut"): VFadeOut,
        ("manimlib.animation.fading", "VFadeInThenOut"): VFadeInThenOut,
        ("manimlib.animation.fading", "FadeTransform"): FadeTransform,
        (
            "manimlib.animation.fading",
            "FadeTransformPieces",
        ): FadeTransformPieces,
        ("manimlib.animation.fading", "FadeInFromPoint"): FadeInFromPoint,
        ("manimlib.animation.fading", "FadeOutToPoint"): FadeOutToPoint,
        ("manimlib.animation.composition", "AnimationGroup"): AnimationGroup,
        ("manimlib.animation.composition", "LaggedStart"): LaggedStart,
        ("manimlib.animation.composition", "LaggedStartMap"): LaggedStartMap,
        ("manimlib.animation.composition", "Succession"): Succession,
        ("manimlib.animation.rotation", "Rotate"): Rotate,
        ("manimlib.animation.rotation", "Rotating"): Rotating,
        ("manimlib.animation.movement", "Homotopy"): Homotopy,
        (
            "manimlib.animation.movement",
            "SmoothedVectorizedHomotopy",
        ): SmoothedVectorizedHomotopy,
        ("manimlib.animation.movement", "ComplexHomotopy"): ComplexHomotopy,
        ("manimlib.animation.movement", "PhaseFlow"): PhaseFlow,
        ("manimlib.animation.movement", "MoveAlongPath"): MoveAlongPath,
        ("manimlib.animation.growing", "GrowFromPoint"): GrowFromPoint,
        ("manimlib.animation.growing", "GrowFromCenter"): GrowFromCenter,
        ("manimlib.animation.growing", "GrowFromEdge"): GrowFromEdge,
        ("manimlib.animation.growing", "GrowArrow"): GrowArrow,
        (
            "manimlib.animation.growing",
            "SpinInFromNothing",
        ): SpinInFromNothing,
        ("manimlib.animation.indication", "FocusOn"): FocusOn,
        ("manimlib.animation.indication", "Indicate"): Indicate,
        ("manimlib.animation.indication", "Flash"): Flash,
        ("manimlib.animation.indication", "CircleIndicate"): CircleIndicate,
        ("manimlib.animation.indication", "TurnInsideOut"): TurnInsideOut,
        ("manimlib.animation.indication", "WiggleOutThenIn"): WiggleOutThenIn,
        ("manimlib.animation.indication", "ShowPassingFlash"): ShowPassingFlash,
        (
            "manimlib.animation.indication",
            "ShowCreationThenDestruction",
        ): ShowCreationThenDestruction,
        (
            "manimlib.animation.indication",
            "ShowCreationThenFadeOut",
        ): ShowCreationThenFadeOut,
        (
            "manimlib.animation.indication",
            "VShowPassingFlash",
        ): VShowPassingFlash,
        ("manimlib.animation.indication", "FlashAround"): FlashAround,
        ("manimlib.animation.indication", "FlashUnder"): FlashUnder,
        (
            "manimlib.animation.indication",
            "ShowPassingFlashAround",
        ): ShowPassingFlashAround,
        (
            "manimlib.animation.indication",
            "AnimationOnSurroundingRectangle",
        ): AnimationOnSurroundingRectangle,
        (
            "manimlib.animation.indication",
            "ShowCreationThenDestructionAround",
        ): ShowCreationThenDestructionAround,
        (
            "manimlib.animation.indication",
            "ShowCreationThenFadeAround",
        ): ShowCreationThenFadeAround,
        ("manimlib.animation.indication", "FlashyFadeIn"): FlashyFadeIn,
        ("manimlib.animation.indication", "ApplyWave"): ApplyWave,
        ("manimlib.animation.specialized", "Broadcast"): Broadcast,
        ("manimlib.animation.transform", "Transform"): Transform,
        ("manimlib.animation.transform", "CyclicReplace"): CyclicReplace,
        ("manimlib.animation.transform", "Swap"): Swap,
        ("manimlib.animation.transform", "ApplyMethod"): ApplyMethod,
        (
            "manimlib.animation.transform",
            "ApplyPointwiseFunction",
        ): ApplyPointwiseFunction,
        (
            "manimlib.animation.transform",
            "ApplyPointwiseFunctionToCenter",
        ): ApplyPointwiseFunctionToCenter,
        ("manimlib.animation.transform", "FadeToColor"): FadeToColor,
        ("manimlib.animation.transform", "ScaleInPlace"): ScaleInPlace,
        ("manimlib.animation.transform", "ShrinkToCenter"): ShrinkToCenter,
        ("manimlib.animation.transform", "ApplyFunction"): ApplyFunction,
        ("manimlib.animation.transform", "ApplyMatrix"): ApplyMatrix,
        (
            "manimlib.animation.transform",
            "ApplyComplexFunction",
        ): ApplyComplexFunction,
        ("manimlib.animation.transform", "MoveToTarget"): MoveToTarget,
        (
            "manimlib.animation.transform",
            "_MethodAnimation",
        ): _MethodAnimation,
        ("manimlib.animation.transform", "ReplacementTransform"): ReplacementTransform,
        ("manimlib.animation.transform", "TransformFromCopy"): TransformFromCopy,
        ("manimlib.animation.transform", "Restore"): Restore,
        (
            "manimlib.animation.transform_matching_parts",
            "TransformMatchingParts",
        ): TransformMatchingParts,
        (
            "manimlib.animation.transform_matching_parts",
            "TransformMatchingShapes",
        ): TransformMatchingShapes,
        (
            "manimlib.animation.transform_matching_parts",
            "TransformMatchingStrings",
        ): TransformMatchingStrings,
        (
            "manimlib.animation.transform_matching_parts",
            "TransformMatchingTex",
        ): TransformMatchingTex,
        ("manimlib.mobject.svg.brace", "Brace"): Brace,
        ("manimlib.mobject.svg.brace", "BraceLabel"): BraceLabel,
        ("manimlib.mobject.svg.brace", "BraceText"): BraceText,
        ("manimlib.mobject.svg.brace", "LineBrace"): LineBrace,
        ("manimlib.scene.scene", "Scene"): Scene,
        ("manimlib.scene.scene", "SceneState"): SceneState,
        ("manimlib.scene.scene", "EndScene"): _EndScene,
        ("manimlib.scene.scene", "ThreeDScene"): ThreeDScene,
        ("manimlib.scene.scene_embed", "CheckpointManager"): CheckpointManager,
        (
            "manimlib.scene.scene_embed",
            "InteractiveSceneEmbed",
        ): InteractiveSceneEmbed,
        ("manimlib.scene.interactive_scene", "InteractiveScene"): InteractiveScene,
        ("manimlib.extract_scene", "BlankScene"): BlankScene,
        ("manimlib.module_loader", "ModuleLoader"): ModuleLoader,
        ("manimlib.window", "Window"): Window,
        ("manimlib.scene.scene_file_writer", "SceneFileWriter"): SceneFileWriter,
        ("manimlib.utils.tex_file_writing", "LatexError"): LatexError,
        ("manimlib.shader_wrapper", "ShaderWrapper"): ShaderWrapper,
        ("manimlib.shader_wrapper", "VShaderWrapper"): VShaderWrapper,
        ("manimlib.animation.animation", "Animation"): Animation,
    }
    pyglet_module = _ensure_module("manimlib.window")
    pyglet_module.PygletWindow = _UnavailablePygletWindow
    classes_by_name = {
        "Enum": _enum.Enum,
        "Exception": Exception,
        "PygletWindow": _UnavailablePygletWindow,
    }
    for (module_name, name), cls in specials.items():
        cls.__module__ = module_name
        setattr(_ensure_module(module_name), name, cls)
        classes_by_name.setdefault(name, cls)
    _ensure_module("manimlib.event_handler").EVENT_DISPATCHER = EventDispatcher()

    special_functions = {
        (
            "manimlib.mobject.mobject",
            "override_animate",
        ): override_animate,
        (
            "manimlib.animation.animation",
            "prepare_animation",
        ): prepare_animation,
        ("manimlib.extract_scene", "get_indent"): get_indent,
        ("manimlib.extract_scene", "is_child_scene"): is_child_scene,
        ("manimlib.extract_scene", "get_scene_classes"): get_scene_classes,
        ("manimlib.extract_scene", "get_module"): get_module,
        ("manimlib.extract_scene", "scene_from_class"): scene_from_class,
        ("manimlib.extract_scene", "note_missing_scenes"): note_missing_scenes,
        ("manimlib.extract_scene", "prompt_user_for_choice"): prompt_user_for_choice,
        ("manimlib.extract_scene", "get_scenes_to_render"): get_scenes_to_render,
        (
            "manimlib.extract_scene",
            "insert_embed_line_to_module",
        ): insert_embed_line_to_module,
        ("manimlib.extract_scene", "compute_total_frames"): compute_total_frames,
        ("manimlib.extract_scene", "main"): main,
    }
    for (module_name, name), function in special_functions.items():
        function.__module__ = module_name
        setattr(_ensure_module(module_name), name, function)

    class_rows = [
        row for row in rows if row[2] == "class" and "." not in row[1]
    ]
    declared_initializers = {
        (module_name, qualified.rsplit(".", 1)[0])
        for module_name, qualified, kind, _origin, _exported, _detail in rows
        if kind == "method" and qualified.endswith(".__init__")
    }
    pending = [
        row for row in class_rows if (row[0], row[1]) not in specials
    ]
    while pending:
        progress = False
        for row in list(pending):
            module_name, name, _kind, _origin, _exported, detail = row
            names = _base_names(detail)
            if any(base not in classes_by_name for base in names):
                continue
            bases = tuple(classes_by_name[base] for base in names) or (object,)
            attributes = {"__module__": module_name}
            is_core_surface = any(
                isinstance(base, type)
                and issubclass(base, (Mobject, Scene, Animation))
                for base in bases
            )
            if not is_core_surface:
                attributes["__init__"] = _surface_init
            elif (module_name, name) in declared_initializers:
                attributes["__init__"] = _schema_init_refusal(
                    module_name, name
                )
            try:
                if bases == (_enum.Enum,):
                    cls = _enum.Enum(name, {}, module=module_name)
                else:
                    cls = type(name, bases, attributes)
            except TypeError as error:
                base_names = ", ".join(base.__name__ for base in bases)
                raise RuntimeError(
                    f"API schema MRO for {module_name}.{name} "
                    f"cannot preserve bases ({base_names})"
                ) from error
            setattr(_ensure_module(module_name), name, cls)
            classes_by_name.setdefault(name, cls)
            pending.remove(row)
            progress = True
        if progress:
            continue
        unresolved = "; ".join(
            f"{module_name}.{name}: {_base_names(detail)}"
            for module_name, name, _kind, _origin, _exported, detail in pending
        )
        raise RuntimeError(f"unresolved API schema class bases: {unresolved}")

    lifecycle = {
        "__init__",
        "init_data",
        "init_points",
        "init_uniforms",
        "setup",
        "construct",
        "tear_down",
        "begin",
        "finish",
        "interpolate",
        "interpolate_mobject",
    }
    for module_name, qualified, kind, _origin, _exported, _detail in rows:
        module = _ensure_module(module_name)
        if kind == "method" and "." in qualified:
            owner_name, method_name = qualified.rsplit(".", 1)
            owner = getattr(module, owner_name, None)
            if (
                isinstance(owner, type)
                and method_name not in lifecycle
                and not hasattr(owner, method_name)
            ):
                setattr(
                    owner,
                    method_name,
                    _placeholder_method(module_name, owner_name, method_name),
                )
        elif kind == "function" and "." not in qualified:
            if not hasattr(module, qualified):
                setattr(
                    module,
                    qualified,
                    _placeholder_function(module_name, qualified),
                )
        elif kind == "constant" and "." not in qualified:
            if not hasattr(module, qualified):
                setattr(
                    module,
                    qualified,
                    _resolved_constants.get(
                        (module_name, qualified), _constant(_detail)
                    ),
                )
        elif kind == "leaked_import" and "." not in qualified:
            if hasattr(module, qualified):
                continue
            if _origin.split(".", 1)[0] in _REFUSED_REFERENCE_RENDER_IMPORT_ROOTS:
                value = _placeholder_function(module_name, qualified)
            else:
                try:
                    origin = _importlib.import_module(_origin)
                    # A leaked module imported under its own basename (for
                    # example `import random`) is the module, not a same-named
                    # attribute such as `random.random`.
                    value = (
                        origin
                        if qualified == _origin.rsplit(".", 1)[-1]
                        else getattr(origin, qualified, origin)
                    )
                except (ImportError, ValueError):
                    value = _placeholder_function(module_name, qualified)
            setattr(module, qualified, value)

    in_canonical = False
    for raw_line in _API_OVERLAY_TSV.splitlines():
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("[") and line.endswith("]"):
            in_canonical = line == "[canonical]"
            continue
        if not in_canonical:
            continue
        columns = raw_line.split("\t")
        if len(columns) < 2 or ":" not in columns[0]:
            continue
        reference, canonical = columns[0], columns[1]
        module_name, qualified = reference.split(":", 1)
        module = _ensure_module(module_name)
        if "." in qualified:
            owner_name, reference_name = qualified.rsplit(".", 1)
            owner = getattr(module, owner_name, None)
            if isinstance(owner, type):
                value = getattr(
                    owner,
                    reference_name,
                    _placeholder_method(module_name, owner_name, reference_name),
                )
                setattr(owner, reference_name, value)
                setattr(owner, canonical, value)
        elif hasattr(module, qualified):
            value = getattr(module, qualified)
            setattr(module, canonical, value)

    for module_name, name, _kind, _origin, exported, _detail in rows:
        if exported != "1" or "." in name:
            continue
        module = _ensure_module(module_name)
        if hasattr(module, name):
            setattr(root, name, getattr(module, name))

    root.__reference_commit__ = (
        "6199a00d4c1b1127ebe45cb629c3f22538b10e13"
    )
    exception_module = _types.ModuleType("manimlib.exceptions")
    exception_module.StaleHandleError = _StaleHandleError
    exception_module.ForeignStageError = _ForeignStageError
    exception_module.FamilyCycleError = _FamilyCycleError
    exception_module.CapabilityError = _CapabilityError
    exception_module.TexError = _TexError
    _sys.modules["manimlib.exceptions"] = exception_module
    root._exceptions = exception_module

    expected_exports = {
        name
        for _module, name, _kind, _origin, exported, _detail in rows
        if exported == "1" and "." not in name
    }
    for name in list(root.__dict__):
        if not name.startswith("_") and name not in expected_exports:
            del root.__dict__[name]
    # The pinned Reference intentionally has no root __all__; wildcard import
    # therefore exposes every non-underscore name in the assembled namespace.
    root.__dict__.pop("__all__", None)


def _install_rate_functions():
    """The Reference's pure easing formulas, verbatim (fm-d3gt).

    manimlib/utils/rate_functions.py at the Reference pin is pure
    arithmetic; the corpus calls these at import time (custom/** builds
    squished rate functions at module scope), so they must be real
    callables before the placeholder pass runs. Engine-side easing stays
    fmn-anim's; these are the API-semantic Python definitions.
    """
    _math_local = _math

    def _bezier(points):
        degree = len(points) - 1

        def result(t):
            return sum(
                ((1 - t) ** (degree - k))
                * (t**k)
                * _math_local.comb(degree, k)
                * point
                for k, point in enumerate(points)
            )

        return result

    linear = _linear_rate
    smooth = _smooth_rate

    def rush_into(t):
        return 2 * smooth(0.5 * t)

    def rush_from(t):
        return 2 * smooth(0.5 * (t + 1)) - 1

    def slow_into(t):
        return _math_local.sqrt(1 - (1 - t) * (1 - t))

    def double_smooth(t):
        if t < 0.5:
            return 0.5 * smooth(2 * t)
        return 0.5 * (1 + smooth(2 * t - 1))

    there_and_back = _there_and_back_rate

    def there_and_back_with_pause(t, pause_ratio=1.0 / 3):
        a = 2.0 / (1.0 - pause_ratio)
        if t < 0.5 - pause_ratio / 2:
            return smooth(a * t)
        if t < 0.5 + pause_ratio / 2:
            return 1
        return smooth(a - a * t)

    def running_start(t, pull_factor=-0.5):
        return _bezier([0, 0, pull_factor, pull_factor, 1, 1, 1])(t)

    def overshoot(t, pull_factor=1.5):
        return _bezier([0, 0, pull_factor, pull_factor, 1, 1])(t)

    def not_quite_there(func=smooth, proportion=0.7):
        def result(t):
            return proportion * func(t)

        return result

    def wiggle(t, wiggles=2):
        return there_and_back(t) * _math_local.sin(wiggles * _math_local.pi * t)

    def squish_rate_func(func, a=0.4, b=0.6):
        def result(t):
            if a == b:
                return a
            if t < a:
                return func(0)
            if t > b:
                return func(1)
            return func((t - a) / (b - a))

        return result

    def lingering(t):
        return squish_rate_func(lambda t: t, 0, 0.8)(t)

    def exponential_decay(t, half_life=0.1):
        return 1 - _math_local.exp(-t / half_life)

    functions = {
        "linear": linear,
        "smooth": smooth,
        "rush_into": rush_into,
        "rush_from": rush_from,
        "slow_into": slow_into,
        "double_smooth": double_smooth,
        "there_and_back": there_and_back,
        "there_and_back_with_pause": there_and_back_with_pause,
        "running_start": running_start,
        "overshoot": overshoot,
        "not_quite_there": not_quite_there,
        "wiggle": wiggle,
        "squish_rate_func": squish_rate_func,
        "lingering": lingering,
        "exponential_decay": exponential_decay,
    }
    native_catalog = {
        "linear",
        "smooth",
        "rush_into",
        "rush_from",
        "slow_into",
        "double_smooth",
        "there_and_back",
        "lingering",
    }
    module = _ensure_module("manimlib.utils.rate_functions")
    for name, function in functions.items():
        # Only names implemented directly by fmn-core cross as names. The
        # other Reference callables take the already-landed sampled-curve
        # path, preserving default/parameterized semantics without a Python
        # crossing during the segment.
        if name in native_catalog:
            _RATE_FUNC_NAMES[function] = name
        setattr(module, name, function)
        if not hasattr(_FMN_MODULE, name):
            setattr(_FMN_MODULE, name, function)


_install_rate_functions()


def _install_bezier_functions():
    """Bind the high-frequency deterministic Bézier utility surface.

    Array broadcasting remains ordinary NumPy behavior, while Chisel owns
    the clamped integer interpolation shared with the animation engine.
    """

    def bezier(points):
        if len(points) == 0:
            raise Exception("bezier cannot be calld on an empty list")
        degree = len(points) - 1

        def result(t):
            return sum(
                ((1 - t) ** (degree - k))
                * (t**k)
                * _math.comb(degree, k)
                * point
                for k, point in enumerate(points)
            )

        return result

    def interpolate(start, end, alpha):
        return _interpolate(start, end, alpha)

    def inverse_interpolate(start, end, value):
        return _np.true_divide(value - start, end - start)

    def integer_interpolate(start, end, alpha):
        return _BridgeMobject._integer_interpolate(
            int(start), int(end), float(alpha)
        )

    functions = {
        "bezier": bezier,
        "interpolate": interpolate,
        "inverse_interpolate": inverse_interpolate,
        "integer_interpolate": integer_interpolate,
    }
    module = _ensure_module("manimlib.utils.bezier")
    for name, function in functions.items():
        setattr(module, name, function)
        if not hasattr(_FMN_MODULE, name):
            setattr(_FMN_MODULE, name, function)


_install_bezier_functions()


def _install_color_functions():
    """Expose fmn-core's one color model through the Reference utilities."""

    def color_to_rgb(color):
        return _color_to_rgb(color)

    def color_to_rgba(color, alpha=1.0):
        return _color_to_rgba(color, alpha)

    def rgb_to_color(rgb):
        values = _np.asarray(rgb, dtype=float)
        if values.shape != (3,) or not _np.isfinite(values).all() or (
            (values < 0) | (values > 1)
        ).any():
            return _ColorValue((1.0, 1.0, 1.0))
        return _ColorValue(values)

    def rgba_to_color(rgba):
        return rgb_to_color(rgba[:3])

    def rgb_to_hex(rgb):
        return _rgb_to_hex(rgb)

    def hex_to_rgb(hex_code):
        return _np.array(_BridgeMobject._hex_to_rgb(hex_code))

    def invert_color(color):
        return rgb_to_color(1.0 - color_to_rgb(color))

    def color_to_int_rgb(color):
        return (255 * color_to_rgb(color)).astype("uint8")

    def color_to_int_rgba(color, opacity=1.0):
        alpha = int(255 * opacity)
        return _np.array([*color_to_int_rgb(color), alpha], dtype=_np.uint8)

    def color_to_hex(color):
        return rgb_to_hex(color_to_rgb(color))

    def hex_to_int(rgb_hex):
        return int(rgb_hex[1:], 16)

    def int_to_hex(rgb_int):
        return f"#{rgb_int:06x}".upper()

    def color_gradient(reference_colors, length_of_output, interp_by_hsl=False):
        if length_of_output == 0:
            return []
        length_of_output = _operator.index(length_of_output)
        if length_of_output < 0:
            raise ValueError(
                f"Number of samples, {length_of_output}, must be non-negative."
            )
        n_reference_colors = len(reference_colors)
        if n_reference_colors < 2:
            if n_reference_colors == 1 and length_of_output == 1:
                return [_ColorValue(color_to_rgb(reference_colors[0]))]
            raise IndexError("list index out of range")
        return _color_gradient(
            list(reference_colors), length_of_output, bool(interp_by_hsl)
        )

    def interpolate_color(color1, color2, alpha, interp_by_hsl=False):
        rgb = _BridgeMobject._interpolate_color(
            _vec3(color_to_rgb(color1)),
            _vec3(color_to_rgb(color2)),
            float(alpha),
            bool(interp_by_hsl),
        )
        return _ColorValue(rgb)

    def interpolate_color_by_hsl(color1, color2, alpha):
        return interpolate_color(color1, color2, alpha, interp_by_hsl=True)

    def average_color(*colors):
        if not colors:
            # The pinned Reference reaches ``tuple(numpy.float64(nan))`` in
            # this case. Preserve its public exception instead of silently
            # turning an undefined average into a color.
            raise TypeError("'numpy.float64' object is not iterable")
        rgbs = [_vec3(color_to_rgb(color)) for color in colors]
        return _ColorValue(_BridgeMobject._average_color(rgbs))

    functions = {
        "average_color": average_color,
        "color_gradient": color_gradient,
        "color_to_hex": color_to_hex,
        "color_to_int_rgb": color_to_int_rgb,
        "color_to_int_rgba": color_to_int_rgba,
        "color_to_rgb": color_to_rgb,
        "color_to_rgba": color_to_rgba,
        "hex_to_int": hex_to_int,
        "hex_to_rgb": hex_to_rgb,
        "int_to_hex": int_to_hex,
        "interpolate_color": interpolate_color,
        "interpolate_color_by_hsl": interpolate_color_by_hsl,
        "invert_color": invert_color,
        "rgb_to_color": rgb_to_color,
        "rgb_to_hex": rgb_to_hex,
        "rgba_to_color": rgba_to_color,
    }
    module = _ensure_module("manimlib.utils.color")
    for name, function in functions.items():
        setattr(module, name, function)
        if not hasattr(_FMN_MODULE, name):
            setattr(_FMN_MODULE, name, function)


_install_color_functions()


def _install_simple_functions():
    """Reference scalar/array helpers with no optional dependency surface."""

    def sigmoid(x):
        return 1.0 / (1 + _np.exp(-x))

    def choose(n, k):
        return _math.comb(n, k)

    def clip(a, min_a, max_a):
        if a < min_a:
            return min_a
        if a > max_a:
            return max_a
        return a

    def fdiv(a, b, zero_over_zero_value=None):
        if zero_over_zero_value is not None:
            out = _np.full_like(a, zero_over_zero_value)
            where = _np.logical_or(a != 0, b != 0)
        else:
            out = None
            where = True
        return _np.true_divide(a, b, out=out, where=where)

    functions = {
        "binary_search": _binary_search,
        "choose": choose,
        "clip": clip,
        "fdiv": fdiv,
        "sigmoid": sigmoid,
    }
    module = _ensure_module("manimlib.utils.simple_functions")
    for name, function in functions.items():
        setattr(module, name, function)
        if not hasattr(_FMN_MODULE, name):
            setattr(_FMN_MODULE, name, function)


_install_simple_functions()


def _install_space_ops():
    """The pure-arithmetic space_ops (manimlib/utils/space_ops.py at the
    pin), Reference-verbatim. Rotation/quaternion members stay precise
    placeholders until they bind to the engine's camera math."""

    def get_norm(vect):
        return sum(x**2 for x in vect) ** 0.5

    def get_dist(vect1, vect2):
        return get_norm(_np.array(vect2) - _np.array(vect1))

    def normalize(vect, fall_back=None):
        norm = get_norm(vect)
        if norm > 0:
            return _np.array(vect) / norm
        elif fall_back is not None:
            return _np.array(fall_back)
        else:
            return _np.zeros(len(vect))

    def normalize_along_axis(array, axis):
        norms = _np.sqrt((array * array).sum(axis))
        norms[norms == 0] = 1
        return array / norms[:, _np.newaxis]

    def center_of_mass(points):
        return _np.array(points).sum(0) / len(points)

    def midpoint(point1, point2):
        return center_of_mass([point1, point2])

    def rotate_vector(vector, angle, axis=_OUT):
        # The ONE rotation implementation: fmn-geom's scipy-exact
        # quaternion rotation_matrix, through the engine seam.
        return _np.array(
            _BridgeMobject._rotate_vector(_vec3(vector), float(angle), _vec3(axis))
        )

    def rotate_vector_2d(vector, angle):
        # Reference-verbatim complex-arithmetic 2D rotation.
        z = complex(*vector) * _np.exp(complex(0, angle))
        return _np.array([z.real, z.imag])

    def cross(v1, v2, out=None):
        is_2d = isinstance(v1, _np.ndarray) and len(v1.shape) == 2
        if is_2d:
            x1, y1, z1 = v1[:, 0], v1[:, 1], v1[:, 2]
            x2, y2, z2 = v2[:, 0], v2[:, 1], v2[:, 2]
        else:
            x1, y1, z1 = v1
            x2, y2, z2 = v2
        if out is None:
            out = _np.empty(_np.shape(v1))
        out.T[:] = [
            y1 * z2 - z1 * y2,
            z1 * x2 - x1 * z2,
            x1 * y2 - y1 * x2,
        ]
        return out

    def angle_of_vector(vector):
        values = _np.asarray(vector, dtype=float)
        if values.size < 2:
            raise ValueError("angle_of_vector needs at least two components")
        return _BridgeMobject._angle_of_vector(
            (float(values[0]), float(values[1]), 0.0)
        )

    def angle_between_vectors(v1, v2):
        left = _np.asarray(v1, dtype=float)
        right = _np.asarray(v2, dtype=float)
        if left.shape == right.shape == (3,):
            return _BridgeMobject._angle_between_vectors(
                _vec3(left), _vec3(right)
            )
        n1 = get_norm(left)
        n2 = get_norm(right)
        if n1 == 0 or n2 == 0:
            return 0.0
        cosine = _np.dot(left, right) / _np.float64(n1 * n2)
        return _math.acos(max(-1.0, min(1.0, cosine)))

    def line_intersects_path(start, end, path):
        def xy_point(value):
            point = _np.asarray(value, dtype=float)
            if point.shape == (2,):
                return (float(point[0]), float(point[1]), 0.0)
            if point.shape == (3,):
                return _vec3(point)
            raise ValueError("line_intersects_path points need two or three components")

        return _BridgeMobject._line_intersects_path(
            xy_point(start),
            xy_point(end),
            [xy_point(point) for point in path],
        )

    def compass_directions(n=4, start_vect=_RIGHT):
        angle = _math.tau / n
        return _np.array(
            [rotate_vector(start_vect, k * angle) for k in range(n)]
        )

    functions = {
        "get_norm": get_norm,
        "get_dist": get_dist,
        "normalize": normalize,
        "normalize_along_axis": normalize_along_axis,
        "center_of_mass": center_of_mass,
        "midpoint": midpoint,
        "rotate_vector": rotate_vector,
        "rotate_vector_2d": rotate_vector_2d,
        "cross": cross,
        "angle_of_vector": angle_of_vector,
        "angle_between_vectors": angle_between_vectors,
        "line_intersects_path": line_intersects_path,
        "compass_directions": compass_directions,
    }
    module = _ensure_module("manimlib.utils.space_ops")
    for name, function in functions.items():
        setattr(module, name, function)
        if not hasattr(_FMN_MODULE, name):
            setattr(_FMN_MODULE, name, function)


_install_space_ops()


def _install_path_functions():
    """Reference path factories over NumPy arrays and native rotation axes."""

    straight_path_threshold = 0.01

    def straight_path(start_points, end_points, alpha):
        return _interpolate(start_points, end_points, alpha)

    # Scalar-arc factories carry their arc as metadata so Transform's
    # path_func= surface can route them onto the native path_arc plane
    # (fm-5wq.4.63) instead of refusing.
    straight_path._fmn_path_arc = 0.0
    straight_path._fmn_path_axis = tuple(float(v) for v in _OUT)

    def path_along_arc(arc_angle, axis=_OUT):
        if isinstance(arc_angle, (float, int)) and abs(arc_angle) < straight_path_threshold:
            return straight_path
        axis = _np.asarray(axis, dtype=float)
        if sum(value**2 for value in axis) ** 0.5 == 0:
            axis = _OUT
        unit_axis = axis / (sum(value**2 for value in axis) ** 0.5)

        def path(start_points, end_points, alpha):
            start_points = _np.asarray(start_points)
            end_points = _np.asarray(end_points)
            if isinstance(arc_angle, (float, int)):
                theta = arc_angle
            else:
                if isinstance(arc_angle, _np.ndarray) and len(arc_angle) == len(start_points):
                    # Reference behavior: its zero-avoidance assignment is
                    # visible through the caller-owned ndarray.
                    theta_range = arc_angle
                else:
                    theta_range = _np.linspace(
                        arc_angle[0], arc_angle[-1], len(start_points)
                    )
                theta_range[_np.abs(theta_range) < straight_path_threshold] = (
                    straight_path_threshold
                )
                theta = theta_range[:, _np.newaxis] * _np.ones(
                    start_points.shape[1]
                )
            start_to_end = end_points - start_points
            with _np.errstate(divide="ignore", invalid="ignore"):
                adjustments = _np.nan_to_num(
                    _np.cross(unit_axis, start_to_end / 2.0) / _np.tan(theta / 2)
                )
                arc_centers = start_points + 0.5 * start_to_end + adjustments
            center_to_start = start_points - arc_centers
            center_to_perpendicular = _np.cross(unit_axis, center_to_start)
            return (
                arc_centers
                + _np.cos(alpha * theta) * center_to_start
                + _np.sin(alpha * theta) * center_to_perpendicular
            )

        if isinstance(arc_angle, (float, int)):
            path._fmn_path_arc = float(arc_angle)
            path._fmn_path_axis = tuple(float(value) for value in unit_axis)
        return path

    def clockwise_path():
        return path_along_arc(-_math.pi)

    def counterclockwise_path():
        return path_along_arc(_math.pi)

    functions = {
        "clockwise_path": clockwise_path,
        "counterclockwise_path": counterclockwise_path,
        "path_along_arc": path_along_arc,
        "straight_path": straight_path,
    }
    module = _ensure_module("manimlib.utils.paths")
    for name, function in functions.items():
        setattr(module, name, function)
        if not hasattr(_FMN_MODULE, name):
            setattr(_FMN_MODULE, name, function)


_install_path_functions()


def _install_iterable_functions():
    """Bind the two record-resize utilities used by Mobject mutation."""

    functions = {
        "resize_array": resize_array,
        "resize_preserving_order": resize_preserving_order,
    }
    module = _ensure_module("manimlib.utils.iterables")
    for name, function in functions.items():
        setattr(module, name, function)
        if not hasattr(_FMN_MODULE, name):
            setattr(_FMN_MODULE, name, function)


_install_iterable_functions()


def _install_mobject_functions():
    """Free functions from manimlib/mobject/mobject.py, Reference-verbatim
    over the bound surfaces."""

    def always_redraw(func, *args, **kwargs):
        # A named refusal beats the bare "'NoneType' object is not
        # callable" the Reference lets escape (fm-5wq.4.69).
        if not callable(func):
            raise TypeError(
                "always_redraw requires a callable returning a Mobject; "
                "got " + type(func).__name__
            )
        mob = func(*args, **kwargs)
        if not isinstance(mob, Mobject):
            raise TypeError(
                "always_redraw's callable must return a Mobject; got "
                + type(mob).__name__
            )
        mob.add_updater(lambda m: m.become(func(*args, **kwargs)))
        return mob

    def assert_is_mobject_method(method):
        # mobject_update_utils.py:20, verbatim (bare asserts included).
        assert _inspect.ismethod(method)
        mobject = method.__self__
        assert isinstance(mobject, Mobject)

    def always(method, *args, **kwargs):
        # mobject_update_utils.py:26, verbatim over the live updater seam.
        assert_is_mobject_method(method)
        mobject = method.__self__
        func = method.__func__
        mobject.add_updater(lambda m: func(m, *args, **kwargs))
        return mobject

    def f_always(method, *arg_generators, **kwargs):
        # mobject_update_utils.py:34: `always` with per-frame argument
        # generators instead of fixed arguments.
        assert_is_mobject_method(method)
        mobject = method.__self__
        func = method.__func__

        def updater(mob):
            args = [arg_generator() for arg_generator in arg_generators]
            func(mob, *args, **kwargs)

        mobject.add_updater(updater)
        return mobject

    def always_shift(mobject, direction=_RIGHT, rate=0.1):
        if not isinstance(mobject, Mobject):
            raise TypeError(
                "always_shift requires a Mobject; got "
                + type(mobject).__name__
            )
        direction = _np.asarray(_vec3(direction), dtype=float)
        rate = float(rate)
        mobject.add_updater(
            lambda mob, dt: mob.shift(rate * dt * direction)
        )
        return mobject

    def always_rotate(mobject, rate=20 * _DEG, **kwargs):
        if not isinstance(mobject, Mobject):
            raise TypeError(
                "always_rotate requires a Mobject; got "
                + type(mobject).__name__
            )
        rate = float(rate)
        rotation_kwargs = dict(kwargs)
        mobject.add_updater(
            lambda mob, dt: mob.rotate(rate * dt, **rotation_kwargs)
        )
        return mobject

    def turn_animation_into_updater(animation, cycle=False, **kwargs):
        # mobject_update_utils.py:83 over Python-driven animations: the
        # updater re-applies the animation's own interpolate each frame.
        # The Transform family carries a straight-path record-lerp
        # fallback for exactly this driver (fm-5wq.4.91); a spec class
        # with no per-frame Python interpolate at all still refuses by
        # the missing seam's name rather than silently holding still.
        if not isinstance(animation, Animation):
            raise TypeError(
                "turn_animation_into_updater requires an Animation; got "
                + type(animation).__name__
            )
        if (
            type(animation).interpolate_mobject
            is Animation.interpolate_mobject
        ):
            raise NotImplementedError(
                "turn_animation_into_updater requires a Python-driven "
                "animation (one implementing interpolate_mobject); "
                "native-segment classes await Choreo's persistent-updater "
                "seam"
            )
        run_time = kwargs.pop("run_time", None)
        rate_func = kwargs.pop("rate_func", None)
        lag_ratio = kwargs.pop("lag_ratio", None)
        if kwargs:
            raise TypeError(
                "unexpected keyword arguments: " + ", ".join(sorted(kwargs))
            )
        if run_time is not None:
            animation.run_time = float(run_time)
        if rate_func is not None:
            animation.rate_func = rate_func
        if lag_ratio is not None:
            animation.lag_ratio = float(lag_ratio)
        mobject = animation.mobject
        animation.suspend_mobject_updating = False
        animation.begin()
        animation.total_time = 0.0

        def update(m, dt):
            run_time = animation.run_time
            time_ratio = animation.total_time / run_time
            if cycle:
                alpha = time_ratio % 1
            else:
                alpha = min(max(time_ratio, 0.0), 1.0)
                if alpha >= 1:
                    animation.finish()
                    m.remove_updater(update)
                    return
            animation.interpolate(alpha)
            animation.total_time += dt

        mobject.add_updater(update)
        return mobject

    functions = {
        "always_redraw": always_redraw,
        "assert_is_mobject_method": assert_is_mobject_method,
        "always": always,
        "f_always": f_always,
        "always_shift": always_shift,
        "always_rotate": always_rotate,
        "turn_animation_into_updater": turn_animation_into_updater,
    }
    module = _ensure_module("manimlib.mobject.mobject_update_utils")
    for name, function in functions.items():
        setattr(module, name, function)
        if not hasattr(_FMN_MODULE, name):
            setattr(_FMN_MODULE, name, function)


_install_mobject_functions()


def _install_vector_field_functions():
    """Bind the Reference's field helpers over native geometry and live
    updater surfaces without importing SciPy or matplotlib."""

    def get_vectorized_rgb_gradient_function(min_value, max_value, color_map):
        if color_map != "3b1b_colormap":
            raise NotImplementedError(
                "field gradients support the bundled '3b1b_colormap'; "
                f"{color_map!r} requires non-bundled matplotlib data"
            )

        def gradient(values):
            flat = _np.asarray(values, dtype=float).reshape(-1)
            return _np.asarray(
                _BridgeMobject._vector_field_gradient(
                    float(min_value), float(max_value), flat.tolist()
                ),
                dtype=float,
            )

        return gradient

    def get_rgb_gradient_function(min_value, max_value, color_map):
        gradient = get_vectorized_rgb_gradient_function(
            min_value, max_value, color_map
        )
        return lambda value: gradient(_np.array([value]))[0]

    def get_sample_coords(coordinate_system, density=1.0):
        return _vector_field_sample_coords(coordinate_system, density)

    def move_along_vector_field(mobject, func):
        mobject.add_updater(
            lambda current, dt: current.shift(func(current.get_center()) * dt)
        )
        return mobject

    def move_submobjects_along_vector_field(mobject, func):
        def apply_nudge(current, dt):
            for submobject in current:
                x, y = submobject.get_center()[:2]
                if abs(x) < 2.0 * _FRAME_X_RADIUS and abs(y) < 2.0 * _FRAME_Y_RADIUS:
                    submobject.shift(func(submobject.get_center()) * dt)

        mobject.add_updater(apply_nudge)
        return mobject

    def move_points_along_vector_field(mobject, func, coordinate_system):
        origin = coordinate_system.get_origin()

        def apply_nudge(current, dt):
            current.apply_function(
                lambda point: point
                + (
                    coordinate_system.c2p(
                        *func(*coordinate_system.p2c(point))
                    )
                    - origin
                )
                * dt
            )

        mobject.add_updater(apply_nudge)
        return mobject

    def vectorize(pointwise_function):
        def vectorized(coords_array):
            return _np.array(
                [pointwise_function(*coords) for coords in coords_array]
            )

        return vectorized

    functions = {
        "get_rgb_gradient_function": get_rgb_gradient_function,
        "get_sample_coords": get_sample_coords,
        "get_vectorized_rgb_gradient_function": get_vectorized_rgb_gradient_function,
        "move_along_vector_field": move_along_vector_field,
        "move_points_along_vector_field": move_points_along_vector_field,
        "move_submobjects_along_vector_field": move_submobjects_along_vector_field,
        "vectorize": vectorize,
    }
    module = _ensure_module("manimlib.mobject.vector_field")
    for name, function in functions.items():
        setattr(module, name, function)
        if not hasattr(_FMN_MODULE, name):
            setattr(_FMN_MODULE, name, function)


_install_vector_field_functions()


_install_schema_surface()


def _portal_cli_emit(code, identity, kind, message, robot, **fields):
    """Emit one bounded human or robot result and return its exit code."""

    import json as _json

    message = str(message)[:1024]
    if robot:
        payload = {
            "schema": "fmn-python.cli",
            "version": 1,
            "kind": kind,
            "status": "success" if code == 0 else "error",
            "exit": {"code": int(code), "identity": identity},
            "message": message,
        }
        payload.update(fields)
        print(_json.dumps(payload, sort_keys=True, separators=(",", ":")))
    elif code == 0:
        if message:
            print(message)
    else:
        print(f"fmn-python: {identity}/{kind}: {message}", file=_sys.stderr)
    return int(code)


def _portal_cli_help():
    return """usage: fmn-python [--robot] --version
       fmn-python [--robot] --list-scenes SOURCE.py
       fmn-python [--robot] --construct-only SOURCE.py [SCENE]
       fmn-python [--robot] SOURCE.py [SCENE] [--format png|png_sequence]
                  [--resolution WIDTHxHEIGHT] [--fps FPS] [--threads N]
                  [--video_dir DIRECTORY]
       fmn-python studio SOURCE.py [SCENE]

The wheel renders standard-mode final-state PNGs and PNG sequences through the
same retained Lumen CPU renderer and ordered Reel sink as the native front
door. Certified output, video containers, opener flags, write-all, and Studio
remain precise capability refusals until their complete contracts are
connected."""


def _portal_cli_scene_types(source):
    """Execute one user source and return its locally declared Scene types."""

    import pathlib as _pathlib
    import runpy as _runpy

    path = _pathlib.Path(source)
    if path.suffix.lower() not in (".py", ".pyw"):
        raise ValueError("the Python portal accepts only .py or .pyw scene sources")
    if not path.is_file():
        raise FileNotFoundError(f"scene source does not exist: {source}")
    module_name = "__fmn_scene_source__"
    namespace = _runpy.run_path(str(path), run_name=module_name)
    scenes = {}
    for name, value in namespace.items():
        if (
            isinstance(value, type)
            and value is not Scene
            and issubclass(value, Scene)
            and value.__module__ == module_name
        ):
            scenes[name] = value
    return scenes


def _portal_cli_render_arguments(arguments):
    """Parse the deliberately narrow, actually shipped portal render surface."""

    import os as _os

    values = {
        "format": "png_sequence",
        "resolution": "1920x1080",
        "fps": "60",
        "threads": str(max(1, min(_os.cpu_count() or 1, 96))),
        "video_dir": None,
    }
    positionals = []
    index = 0
    while index < len(arguments):
        argument = arguments[index]
        if argument == "--reproducible":
            raise RuntimeError(
                "CAPABILITY: certified portal rendering awaits the complete "
                "content-hashed input closure and provenance sidecar"
            )
        if argument in ("-o", "-so", "--write_all", "--autoreload"):
            raise RuntimeError(
                f"CAPABILITY: {argument} is not connected in the Python portal"
            )
        if argument in ("--format", "--resolution", "--fps", "--threads", "--video_dir"):
            if index + 1 >= len(arguments):
                raise ValueError(f"{argument} requires a value")
            values[argument[2:]] = arguments[index + 1]
            index += 2
            continue
        if argument.startswith("--") and "=" in argument:
            name, value = argument.split("=", 1)
            if name in ("--format", "--resolution", "--fps", "--threads", "--video_dir"):
                if not value:
                    raise ValueError(f"{name} requires a value")
                values[name[2:]] = value
                index += 1
                continue
        if argument.startswith("-"):
            raise ValueError(f"unsupported portal render flag: {argument}")
        positionals.append(argument)
        index += 1

    if values["format"] not in ("png", "png_sequence"):
        raise RuntimeError(
            f"CAPABILITY: portal output format {values['format']!r} is not connected; "
            "use --format png or --format png_sequence"
        )
    if len(positionals) not in (1, 2):
        raise ValueError("render requires SOURCE.py and accepts one optional SCENE")
    try:
        width_text, height_text = values["resolution"].lower().split("x", 1)
        width = int(width_text)
        height = int(height_text)
        fps = int(values["fps"])
        threads = int(values["threads"])
    except (TypeError, ValueError) as error:
        raise ValueError(
            "--resolution must be WIDTHxHEIGHT and --fps/--threads must be integers"
        ) from error
    if width <= 0 or height <= 0 or fps <= 0 or threads <= 0:
        raise ValueError("resolution, fps, and threads must all be positive")
    return positionals, values, width, height, fps, threads


def _console_main():
    """The wheel's `fmn-python` entry point.

    The standard PNG routes are production composition: Scene captures cross
    into Lumen and are published through Reel. Other output surfaces remain
    precise refusals instead of lifecycle-only fake success.
    """

    import platform as _platform

    arguments = list(_sys.argv[1:])
    robot = False
    if "--robot" in arguments:
        arguments.remove("--robot")
        robot = True

    if arguments in (["--help"], ["-h"]):
        return _portal_cli_emit(0, "success", "help", _portal_cli_help(), robot)
    if not arguments:
        return _portal_cli_emit(
            2,
            "usage",
            "usage-error",
            "missing command or scene source; use --help",
            robot,
        )
    if "--version" in arguments:
        if arguments != ["--version"]:
            return _portal_cli_emit(
                2,
                "usage",
                "usage-error",
                "--version cannot be combined with a scene or another flag",
                robot,
            )
        version = getattr(_FMN_ROOT, "__version__", "unknown")
        numpy_version = getattr(_np, "__version__", "unknown")
        message = (
            f"fmn-python {version} "
            f"({_platform.python_implementation()} {_platform.python_version()}, "
            f"NumPy {numpy_version})"
        )
        return _portal_cli_emit(
            0,
            "success",
            "version",
            message,
            robot,
            program="fmn-python",
            program_version=version,
            distribution="franken-manim",
            abi_policy="cpython-3.13-full-abi",
            python_implementation=_platform.python_implementation(),
            python_version=_platform.python_version(),
            numpy_version=numpy_version,
        )

    if arguments[0] == "studio":
        return _portal_cli_emit(
            4,
            "capability",
            "studio-unavailable",
            "the Python portal Studio supervisor/worker route is not yet connected",
            robot,
        )

    construct_only = "--construct-only" in arguments
    list_scenes = "--list-scenes" in arguments
    if construct_only and list_scenes:
        return _portal_cli_emit(
            2,
            "usage",
            "usage-error",
            "choose exactly one of --construct-only or --list-scenes",
            robot,
        )
    if not construct_only and not list_scenes:
        try:
            positionals, render_values, width, height, fps, threads = (
                _portal_cli_render_arguments(arguments)
            )
        except RuntimeError as error:
            message = str(error)
            if message.startswith("CAPABILITY: "):
                message = message[len("CAPABILITY: ") :]
            return _portal_cli_emit(
                4, "capability", "render-capability-unavailable", message, robot
            )
        except (TypeError, ValueError) as error:
            return _portal_cli_emit(
                2, "usage", "usage-error", str(error), robot
            )

        source = positionals[0]
        selected = positionals[1] if len(positionals) == 2 else None
        try:
            scenes = _portal_cli_scene_types(source)
            names = sorted(scenes)
            if selected is None:
                if len(names) != 1:
                    raise ValueError(
                        "select one scene explicitly; discovered: "
                        + (", ".join(names) if names else "none")
                    )
                selected = names[0]
            scene_type = scenes.get(selected)
            if scene_type is None:
                raise ValueError(
                    f"scene {selected!r} was not declared by {source}; discovered: "
                    + (", ".join(names) if names else "none")
                )
            scene = scene_type()
        except Exception as error:
            return _portal_cli_emit(
                5,
                "scene",
                "scene-load-failed",
                f"{type(error).__name__}: {error}",
                robot,
                source=source,
            )

        import pathlib as _pathlib

        destination = render_values["video_dir"]
        if destination is None:
            output_root = (
                _pathlib.Path("media") / "videos" / _pathlib.Path(source).stem
            )
            if render_values["format"] == "png":
                destination = str(output_root / f"{selected}.png")
            else:
                destination = str(output_root / selected / "frames")
        try:
            begin_render = (
                scene._begin_png
                if render_values["format"] == "png"
                else scene._begin_png_sequence
            )
            begin_render(
                destination,
                width,
                height,
                fps,
                threads,
                int(scene.random_seed or 0),
            )
        except Exception as error:
            return _portal_cli_emit(
                6,
                "render",
                "render-start-failed",
                f"{type(error).__name__}: {error}",
                robot,
                source=source,
                scene=selected,
                destination=destination,
            )
        try:
            scene.run()
        except Exception as error:
            try:
                scene._abort_render()
            except Exception:
                pass
            message = f"{type(error).__name__}: {error}"
            output_error = any(
                marker in str(error)
                for marker in ("lumen:", "reel:", "portal-render:")
            )
            return _portal_cli_emit(
                6 if output_error else 5,
                "render" if output_error else "scene",
                "render-failed" if output_error else "scene-execution-failed",
                message,
                robot,
                source=source,
                scene=selected,
                destination=destination,
            )
        try:
            path, frame_count, byte_count, digest, engine, used_threads = (
                scene._finish_render(
                    scene.frame._core,
                    _vec3(scene.camera.light_source.get_center()),
                )
            )
        except Exception as error:
            try:
                scene._abort_render()
            except Exception:
                pass
            return _portal_cli_emit(
                6,
                "render",
                "render-finish-failed",
                f"{type(error).__name__}: {error}",
                robot,
                source=source,
                scene=selected,
                destination=destination,
            )
        return _portal_cli_emit(
            0,
            "success",
            "render",
            f"rendered {frame_count} PNG frames to {path}",
            robot,
            source=source,
            scene=selected,
            format=render_values["format"],
            resolution=[width, height],
            fps=fps,
            destination=path,
            frame_count=int(frame_count),
            bytes=int(byte_count),
            digest=digest,
            engine=engine,
            threads=int(used_threads),
            rendered=True,
        )

    control = "--construct-only" if construct_only else "--list-scenes"
    arguments.remove(control)
    if any(argument.startswith("-") for argument in arguments):
        return _portal_cli_emit(
            2,
            "usage",
            "usage-error",
            f"{control} accepts only SOURCE.py and an optional scene name",
            robot,
        )
    expected = (1, 2) if construct_only else (1,)
    if len(arguments) not in expected:
        detail = (
            "requires SOURCE.py and accepts one optional SCENE"
            if construct_only
            else "requires exactly one SOURCE.py"
        )
        return _portal_cli_emit(2, "usage", "usage-error", f"{control} {detail}", robot)

    source = arguments[0]
    try:
        scenes = _portal_cli_scene_types(source)
        names = sorted(scenes)
        if list_scenes:
            message = "\n".join(names) if names else "no Scene subclasses found"
            return _portal_cli_emit(
                0,
                "success",
                "scene-list",
                message,
                robot,
                source=source,
                scenes=names,
            )

        selected = arguments[1] if len(arguments) == 2 else None
        if selected is None:
            if len(names) != 1:
                raise ValueError(
                    "select one scene explicitly; discovered: "
                    + (", ".join(names) if names else "none")
                )
            selected = names[0]
        scene_type = scenes.get(selected)
        if scene_type is None:
            raise ValueError(
                f"scene {selected!r} was not declared by {source}; discovered: "
                + (", ".join(names) if names else "none")
            )
        scene = scene_type()
        scene.run()
        roots, family, low, high = scene._engine_facts()
        scene_time = float(scene.time())
    except Exception as error:
        return _portal_cli_emit(
            5,
            "scene",
            "construct-failed",
            f"{type(error).__name__}: {error}",
            robot,
            source=source,
        )

    return _portal_cli_emit(
        0,
        "success",
        "construct-only",
        f"constructed {selected} without rendering pixels",
        robot,
        source=source,
        scene=selected,
        scene_time=scene_time,
        root_count=int(roots),
        family_count=int(family),
        bounds_low=[float(value) for value in low],
        bounds_high=[float(value) for value in high],
        rendered=False,
    )
