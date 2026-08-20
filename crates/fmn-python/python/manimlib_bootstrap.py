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
import enum as _enum
import importlib as _importlib
import inspect as _inspect
import itertools as _itertools
import math as _math
import operator as _operator
import pathlib as _pathlib
import re as _re
import sys as _sys
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
_BLACK = "#000000"
_WHITE = "#FFFFFF"
_GREY_C = "#888888"
_GREY_E = "#222222"
_GREEN = "#83C167"
_RED = "#FC6255"

# Function object -> catalog name, filled by _install_rate_functions;
# Scene.play maps rate_func callables into the engine's named catalog.
_RATE_FUNC_NAMES = {}

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


def _copy_mobject_graph(root, deep, memo=None):
    if memo is None:
        memo = {}
    existing = memo.get(id(root))
    if existing is not None:
        return existing

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
        if index is None:
            self.updaters.append(updater)
        else:
            self.updaters.insert(index, updater)
        if call:
            self._dispatch_updater(updater, 0.0)
        return self

    def remove_updater(self, updater):
        self.updaters = [item for item in self.updaters if item is not updater]
        return self

    def clear_updaters(self, recurse=True):
        targets = _family_preorder(self) if recurse else [self]
        for target in targets:
            target.updaters.clear()
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


def _apply_vmobject_style_kwargs(mob, kwargs):
    """The Reference's VMobject style constructor keywords, applied after a
    native build (its init_colors pass). Unknown keywords refuse."""
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
    if fill_color is None:
        fill_color = color
    if stroke_color is None:
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
        )
    if any(value is not None for value in (fill_color, fill_opacity, fill_border_width)):
        mob.set_fill(
            color=fill_color, opacity=fill_opacity, border_width=fill_border_width
        )
    if shading is not None:
        mob.set_shading(*shading)
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
        discontinuities=(),
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


class Rectangle(VMobject):
    def __init__(self, width=4.0, height=2.0, **kwargs):
        _install_live_state(self)
        specs = self._build_rectangle(
            _native_shell_factory, float(width), float(height)
        )
        _hang_native_children(self, specs)
        _apply_vmobject_style_kwargs(self, kwargs)


class Square(Rectangle):
    def __init__(self, side_length=2.0, **kwargs):
        self.side_length = side_length
        super().__init__(side_length, side_length, **kwargs)


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


class Arc(VMobject):
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
    def __init__(self, start_angle=0, stroke_color=None, **kwargs):
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
        # The native circle carries the Reference's RED stroke default; an
        # explicit stroke_color reapplies.
        if stroke_color is not None:
            kwargs.setdefault("stroke_color", stroke_color)
        _apply_vmobject_style_kwargs(self, kwargs)

    def get_radius(self):
        return self._circle_radius()

    def point_at_angle(self, angle):
        return _np.array(self._circle_point_at_angle(float(angle)))


class Dot(VMobject):
    def __init__(
        self,
        point=_ORIGIN,
        radius=0.08,
        stroke_color=None,
        stroke_width=None,
        fill_opacity=None,
        fill_color=None,
        **kwargs,
    ):
        _install_live_state(self)
        specs = self._build_dot(
            _native_shell_factory, _vec3(point), float(radius)
        )
        _hang_native_children(self, specs)
        # The native dot carries the Reference defaults (white fill at 1,
        # black zero-width stroke); explicit values reapply.
        for name, value in (
            ("stroke_color", stroke_color),
            ("stroke_width", stroke_width),
            ("fill_opacity", fill_opacity),
            ("fill_color", fill_color),
        ):
            if value is not None:
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


class Line(VMobject):
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
        return self.get_end() - self.get_start()

    def get_unit_vector(self):
        vect = self.get_vector()
        norm = float(_np.sqrt((vect * vect).sum()))
        return vect / norm if norm > 0 else _np.zeros(3)

    def get_angle(self):
        vect = self.get_vector()
        return _math.atan2(vect[1], vect[0])

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
        unit_vect = self.get_unit_vector()
        start = self.get_start()
        return start + _np.dot(_np.asarray(point, dtype=float) - start, unit_vect) * unit_vect


class DashedLine(Line):
    def __init__(
        self,
        start=_LEFT,
        end=_RIGHT,
        dash_length=0.05,
        positive_space_ratio=0.5,
        **kwargs,
    ):
        buff = kwargs.pop("buff", 0.0)
        path_arc = kwargs.pop("path_arc", 0.0)
        _refuse_unrouted("DashedLine()", [("buff", bool(buff))])
        _install_live_state(self)
        self.path_arc = float(path_arc)
        self.buff = 0.0
        self.set_start_and_end_attrs(start, end)
        specs = self._build_dashed_line(
            _native_shell_factory,
            _vec3(self.start),
            _vec3(self.end),
            float(dash_length),
            float(positive_space_ratio),
            self.path_arc,
        )
        _hang_native_children(self, specs)
        _apply_vmobject_style_kwargs(self, kwargs)

    def get_start(self):
        if self.submobjects:
            return self.submobjects[0].get_start()
        return super().get_start()

    def get_end(self):
        if self.submobjects:
            return self.submobjects[-1].get_end()
        return super().get_end()


class Arrow(Line):
    def __init__(
        self,
        start=_LEFT,
        end=_LEFT,
        buff=0.25,
        path_arc=0.0,
        fill_color=None,
        fill_opacity=None,
        stroke_width=None,
        thickness=3.0,
        tip_width_ratio=5,
        tip_angle=_math.pi / 3,
        max_tip_length_to_length_ratio=0.5,
        max_width_to_length_ratio=0.1,
        **kwargs,
    ):
        # The ratio caps are the Reference defaults baked into the native
        # builder; off-default values refuse precisely.
        _refuse_unrouted(
            "Arrow()",
            [
                (
                    "max_tip_length_to_length_ratio",
                    max_tip_length_to_length_ratio != 0.5,
                ),
                ("max_width_to_length_ratio", max_width_to_length_ratio != 0.1),
            ],
        )
        _install_live_state(self)
        self.path_arc = float(path_arc)
        self.buff = float(buff)
        self.set_start_and_end_attrs(start, end)
        self._arrow_params = (
            self.buff,
            self.path_arc,
            float(thickness),
            float(tip_width_ratio),
            float(tip_angle),
        )
        specs = self._build_arrow(
            _native_shell_factory,
            _vec3(self.start),
            _vec3(self.end),
            *self._arrow_params,
        )
        _hang_native_children(self, specs)
        # The explicit style parameters default to the native builder's
        # (Reference) style; only caller-supplied values reapply.
        if fill_color is not None:
            kwargs.setdefault("fill_color", fill_color)
        if fill_opacity is not None:
            kwargs.setdefault("fill_opacity", fill_opacity)
        if stroke_width is not None:
            kwargs.setdefault("stroke_width", stroke_width)
        _apply_vmobject_style_kwargs(self, kwargs)

    def put_start_and_end_on(self, start, end):
        # An Arrow is ONE filled path whose tip proportions are functions
        # of its length; the generic family affine would stretch the tip.
        # Rebuild natively at the new endpoints instead, carrying style and
        # every engine uniform.  The detached builder replaces the nursery
        # root with a fresh native Arrow, so without the uniform snapshot an
        # updater's immediate call silently drops fix_in_frame/depth/camera
        # state before the Arrow ever enters a Scene.  The bound rebuild only
        # replaces points and already preserves these fields.
        # Reference Arrow.put_start_and_end_on explicitly rebuilds with
        # `buff=0`, independent of the constructor's initial trim.
        style = self.get_style()
        uniforms = self.uniforms.copy()
        rebuild_params = (0.0, *self._arrow_params[1:])
        if self._is_bound():
            self._rebuild_arrow(
                _vec3(start),
                _vec3(end),
                *rebuild_params,
            )
        else:
            specs = self._build_arrow(
                _native_shell_factory,
                _vec3(start),
                _vec3(end),
                *rebuild_params,
            )
            _hang_native_children(self, specs)
        self.set_style(**style, recurse=False)
        self.uniforms.update(uniforms)
        self.start = _np.array(_vec3(start))
        self.end = _np.array(_vec3(end))
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
        # The Reference forwards **kwargs to get_number_mobject; the two
        # positional overrides the corpus uses route natively.
        direction = kwargs.pop("direction", None)
        buff = kwargs.pop("buff", None)
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
        if font_size is not None:
            raise NotImplementedError(
                "Axes.add_coordinate_labels(font_size=...) awaits the "
                "numbers-shelf font-size passthrough; the default size works"
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
        if z_normal is not None:
            raise NotImplementedError(
                "ThreeDAxes(z_normal=...) is not yet routed to the native "
                "builder; the default DOWN normal works"
            )
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
        del x_values, y_values, excluding, kwargs
        raise NotImplementedError(
            "NumberPlane.add_coordinate_labels awaits the merged "
            "plane-axis-config rebuild; ComplexPlane's labeler is native"
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
    """Structural portal base for native SVG-derived mobjects.

    The schema-owned SVG parser methods remain precise unavailable callables;
    Scribe-backed text subclasses use native layout provenance instead.
    """


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
        specs = self._build_tex(
            _native_shell_factory,
            self.tex_strings,
            separator,
            bool(self._native_text_mode),
            self.font_size,
            None,
            bool(self._native_group_single_part),
        )
        _hang_native_children(self, specs)
        _apply_vmobject_style_kwargs(self, kwargs)
        self.set_color_by_tex_to_color_map(color_map)

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
        if any(
            len(self._string_sub_paths[ordinal]) != 1
            for ordinals in selected
            for ordinal in ordinals
        ):
            raise NotImplementedError(
                "Tex.make_number_changeable across nested part groups awaits "
                "the grouped-span replacement seam"
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
        self.tex_strings = [self.tex_string]
        self.string = self.tex_string
        decimal_mobs = [decimal for _, decimal in replacements]
        return VGroup(*decimal_mobs) if replace_all else decimal_mobs[0]


class TexText(Tex):
    _native_text_mode = True
    tex_environment = ""


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
        specs = self._build_cross(_native_shell_factory, mobject, stroke_color)
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
        _refuse_unrouted("Underline()", [("path_arc", path_arc != 0.0)])
        _install_live_state(self)
        self.path_arc = 0.0
        self.buff = float(buff)
        specs = self._build_underline(
            _native_shell_factory,
            mobject,
            stroke_color,
            self.buff,
            float(stretch_factor),
        )
        _hang_native_children(self, specs)
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
        del hide_zero_components_on_complex  # only observable for complex
        if isinstance(number, complex):
            raise NotImplementedError(
                "DecimalNumber over complex values awaits the native "
                "complex-formatting path; real values are native"
            )
        _refuse_unrouted(
            "DecimalNumber()", [("text_config", bool(text_config))]
        )
        _install_live_state(self)
        self.number = float(number)
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
            _native_shell_factory, self.number, *self._decimal_params
        )
        _hang_native_children(self, specs)
        _apply_vmobject_style_kwargs(self, kwargs)

    def get_value(self):
        return self.number

    def set_value(self, number):
        # Reference set_value (numbers.py:207): style from the first
        # pointful member, fresh glyphs, re-seat the fixed edge. Works
        # LIVE in both proxy states: the fresh glyph shells build in a
        # scratch nursery and set_submobjects adopts them — the same
        # adoption-on-attach seam Scene-bound families already use — so a
        # bound number mutates in place, digit-count changes included.
        number = float(number)
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
            raise NotImplementedError(
                "DecimalNumber.set_value with root-level records (background "
                "rectangle) awaits the live-state core (fm-p107)"
            )
        self.set_submobjects(list(scratch.submobjects))
        self.move_to(move_to_point, self.edge_to_fix)
        if style is not None:
            self.set_style(**style)
        self.number = number
        return self

    def _handle_scale_side_effects(self, scale_factor):
        self.font_size *= scale_factor
        return self

    def increment_value(self, delta_t=1):
        self.set_value(self.get_value() + delta_t)
        return self


class Integer(DecimalNumber):
    def __init__(self, number=0, num_decimal_places=0, **kwargs):
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
    for start, stop, step in coordinate_system.get_all_ranges():
        sample_step = float(step) / density
        axes.append(_np.arange(float(start), float(stop) + sample_step, sample_step))
    mesh = _np.meshgrid(*axes, indexing="ij")
    coords = _np.stack(mesh, axis=-1).reshape((-1, len(axes)))
    if coords.shape[1] < 3:
        coords = _np.pad(coords, ((0, 0), (0, 3 - coords.shape[1])))
    return coords[:, :3]


def _vector_field_c2p_rows(coordinate_system, rows):
    rows = _np.asarray(rows, dtype=float).reshape((-1, 3))
    dimension = int(getattr(coordinate_system, "dimension", 2))
    return _np.asarray(
        coordinate_system.c2p(*(rows[:, index] for index in range(dimension))),
        dtype=float,
    ).reshape((-1, 3))


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
        _refuse_unrouted(
            "VectorField()",
            [
                ("color_map", color_map is not None),
                (
                    "color_map_name",
                    color is None and color_map_name not in (None, "3b1b_colormap"),
                ),
                ("norm_to_opacity_func", norm_to_opacity_func is not None),
                *((name, True) for name in sorted(kwargs)),
            ],
        )
        _install_live_state(self)
        self.func = func
        self.coordinate_system = coordinate_system
        self.sample_coords = (
            _vector_field_sample_coords(coordinate_system, density)
            if sample_coords is None
            else _np.asarray(sample_coords, dtype=float).reshape((-1, 3))
        )
        self.stroke_width = float(stroke_width)
        self.stroke_opacity = float(stroke_opacity)
        self.tip_width_ratio = float(tip_width_ratio)
        self.tip_len_to_width = float(tip_len_to_width)
        self.flat_stroke = bool(flat_stroke)
        self.color = color
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
        specs = self._build_geometry(_native_shell_factory, outputs)
        _hang_native_children(self, specs)

    def _evaluate_outputs(self):
        outputs = _np.asarray(self.func(self.sample_coords), dtype=float)
        if outputs.shape != self.sample_coords.shape:
            raise ValueError(
                "VectorField callback must return one three-component vector "
                f"per sample; got {outputs.shape} for {self.sample_coords.shape}"
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
            self.color,
            self.magnitude_range,
        )

    def update_sample_points(self):
        self.sample_points = _vector_field_c2p_rows(
            self.coordinate_system, self.sample_coords
        )
        return self

    def set_sample_coords(self, sample_coords):
        self.sample_coords = _np.asarray(sample_coords, dtype=float).reshape((-1, 3))
        return self.update_sample_points()

    def update_vectors(self):
        scratch = VMobject.__new__(VMobject)
        _install_live_state(scratch)
        specs = self._build_geometry(
            _native_shell_factory, self._evaluate_outputs(), target=scratch
        )
        # The field kernel is a single VMobject root; keep an explicit guard
        # so a future native family expansion cannot be silently discarded.
        if specs:
            raise RuntimeError("native VectorField unexpectedly returned children")
        self.match_points(scratch)
        self.data[:] = scratch.data
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


def _native_surface_shell_factory():
    # Surface-family children are NOT VMobjects: their shells must carry
    # the Mobject-level color surface (rgba records), never stroke/fill.
    shell = Surface.__new__(Surface)
    _install_live_state(shell)
    return shell


class ParametricSurface(Surface):
    def __init__(self, uv_func, u_range=(0, 1), v_range=(0, 1), **kwargs):
        color = kwargs.pop("color", None)
        opacity = kwargs.pop("opacity", None)
        shading = kwargs.pop("shading", None)
        depth_test = kwargs.pop("depth_test", True)
        resolution = kwargs.pop("resolution", (101, 101))
        preferred_creation_axis = kwargs.pop("preferred_creation_axis", 1)
        _refuse_unrouted(
            "ParametricSurface()", [(name, True) for name in sorted(kwargs)]
        )
        _install_live_state(self)
        self.u_range = tuple(u_range)
        self.v_range = tuple(v_range)
        self.resolution = (int(resolution[0]), int(resolution[1]))
        self.preferred_creation_axis = int(preferred_creation_axis)
        specs = self._build_parametric_surface(
            _native_surface_shell_factory,
            uv_func,
            (float(u_range[0]), float(u_range[1])),
            (float(v_range[0]), float(v_range[1])),
            self.resolution,
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
        _refuse_unrouted("Sphere()", [(name, True) for name in sorted(kwargs)])
        _install_live_state(self)
        self.radius = float(radius)
        self.clockwise = bool(clockwise)
        self.true_normals = bool(true_normals)
        self.u_range = tuple(u_range)
        self.v_range = tuple(v_range)
        self.resolution = (int(resolution[0]), int(resolution[1]))
        self.preferred_creation_axis = 1
        self._solid_params = ("sphere", self.radius)
        specs = self._build_sphere(
            _native_surface_shell_factory,
            self.radius,
            (float(u_range[0]), float(u_range[1])),
            (float(v_range[0]), float(v_range[1])),
            self.resolution,
            self.true_normals,
            self.clockwise,
        )
        _hang_native_children(self, specs)
        self._apply_surface_style(color, opacity, shading, depth_test)

    def uv_func(self, u, v):
        return _np.array(
            self._sphere_uv(self.radius, bool(self.clockwise), float(u), float(v))
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
        )
        _hang_native_children(self, specs)
        self._apply_surface_style(color, opacity, shading, depth_test)

    def uv_func(self, u, v):
        return _np.array(self._cylinder_uv(float(u), float(v)))


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
        _refuse_unrouted(
            type(self).__name__ + "()",
            [("square_resolution", tuple(square_resolution) != (2, 2))]
            + [(name, True) for name in sorted(kwargs)],
        )
        _install_live_state(self)
        self.resolution = (2, 2)
        self.preferred_creation_axis = 1
        specs = self._build_cube(_native_surface_shell_factory, float(side_length))
        _hang_native_children(self, specs)
        self._apply_surface_style(color, opacity, shading, depth_test)


class Prism(Cube):
    def __init__(self, width=3.0, height=2.0, depth=1.0, **kwargs):
        color = kwargs.pop("color", None)
        opacity = kwargs.pop("opacity", None)
        shading = kwargs.pop("shading", None)
        depth_test = kwargs.pop("depth_test", True)
        _refuse_unrouted("Prism()", [(name, True) for name in sorted(kwargs)])
        _install_live_state(self)
        self.resolution = (2, 2)
        self.preferred_creation_axis = 1
        specs = self._build_prism(
            _native_surface_shell_factory, float(width), float(height), float(depth)
        )
        _hang_native_children(self, specs)
        self._apply_surface_style(color, opacity, shading, depth_test)


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
        _refuse_unrouted(
            "SurfaceMesh()",
            [("joint_type", joint_type != "no_joint")]
            + [(name, True) for name in sorted(kwargs)],
        )
        params = getattr(uv_surface, "_solid_params", None)
        if params is None:
            raise NotImplementedError(
                "SurfaceMesh needs a native-rebuildable source surface "
                "(Sphere is native); "
                + type(uv_surface).__name__
                + " does not carry solid params yet"
            )
        _install_live_state(self)
        specs = self._build_surface_mesh(
            _native_shell_factory,
            params[0],
            float(params[1]),
            (int(resolution[0]), int(resolution[1])),
            float(normal_nudge),
            float(stroke_width),
            stroke_color,
        )
        _hang_native_children(self, specs)
        # Re-seat onto the source's CURRENT geometry (the rebuild is at
        # native scale/origin) — exact for uniform rescales and moves.
        native_height = 2.0 * float(params[1])
        current_height = uv_surface.get_height()
        if current_height > 0 and abs(current_height - native_height) > 1e-12:
            self.scale(current_height / native_height)
        self.move_to(uv_surface.get_center())
        if depth_test:
            self.apply_depth_test()


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
        **kwargs,
    ):
        _refuse_unrouted(
            "TracingTail()", [(name, True) for name in sorted(kwargs)]
        )
        if not isinstance(mobject_or_func, _BridgeMobject):
            raise NotImplementedError(
                "function-traced tails await the native point-function "
                "tracer; pass the traced mobject itself"
            )
        if not mobject_or_func._is_bound():
            raise NotImplementedError(
                "TracingTail traces a scene-bound mobject; add it to the "
                "Scene before tracing (fm-p107)"
            )

        def taper(value):
            if hasattr(value, "__len__"):
                return [float(v) for v in value]
            return [float(value), float(value)]

        _install_live_state(self)
        self._init_native_tracer(
            mobject_or_func._scene,
            mobject_or_func,
            float(time_traced),
            stroke_color,
            taper(stroke_width),
            taper(stroke_opacity),
        )


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
        # The minimal camera surface: the schema's Camera class (whose
        # unbound methods stay precise placeholders) holding this scene's
        # frame. State-real for `frame`; nothing else is silently stubbed.
        camera = self.__dict__.get("_camera")
        if camera is None:
            camera_class = getattr(_FMN_ROOT, "Camera", None)
            if isinstance(camera_class, type):
                camera = camera_class.__new__(camera_class)
                camera.args = ()
            else:
                camera = _types.SimpleNamespace()
            camera.frame = self.frame
            # The Reference's Camera.init_light_source: a real Point at the
            # camera config's light_source_position default (the same
            # [-10, 10, 10] Lumen's CameraConfig declares). State-real:
            # scenes move it and read it back through the Stage.
            camera.light_source = Point((-10.0, 10.0, 10.0))
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
                    raise NotImplementedError(
                        "overridden animations are not yet routed to the engine"
                    )
                spec_args = {}
                for key, value in proto.anim_args.items():
                    if key not in ("run_time", "rate_func", "lag_ratio"):
                        raise NotImplementedError(
                            "anim arg `" + key + "` is not yet routed to "
                            "the engine play"
                        )
                    spec_args[key] = value
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
                    {},
                )
            if isinstance(proto, Animation) and getattr(proto, "_native_kind", None):
                params = dict(proto._native_params())
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
                if nested:
                    raise NotImplementedError(
                        type(proto).__name__
                        + " inside a composition awaits the nested Python "
                        "animation release window"
                    )
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
                if camera_pair is not None:
                    raise NotImplementedError(
                        "one camera-frame builder per play; merge the "
                        "reorient chain into a single .animate"
                    )
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
        if kwargs:
            raise NotImplementedError(
                "Scene.wait keyword(s) not yet routed: "
                + ", ".join(sorted(kwargs))
            )
        self._wait(None if duration is None else float(duration))

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


class InteractiveScene(Scene):
    def embed(self, namespace=None):
        return _portal_embed(self, namespace)

    def checkpoint_paste(self):
        return _portal_checkpoint_paste(self)


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

    def _native_params(self):
        return {}

    def _native_target(self):
        return getattr(self, self._target_attr) if self._target_attr else None


class ShowCreation(_NativeAnimation):
    _native_kind = "show_creation"

    def __init__(self, mobject, lag_ratio=1.0, **kwargs):
        super().__init__(mobject, lag_ratio=lag_ratio, **kwargs)
        if (
            isinstance(mobject, Surface)
            and hasattr(mobject, "resolution")
            and hasattr(mobject, "preferred_creation_axis")
        ):
            self._native_kind = "show_surface_creation"

    def _native_params(self):
        if self._native_kind != "show_surface_creation":
            return {}
        return {
            "surface_resolution": tuple(self.mobject.resolution),
            "surface_axis": int(self.mobject.preferred_creation_axis),
        }


class Uncreate(_NativeAnimation):
    _native_kind = "uncreate"

    def __init__(self, mobject, **kwargs):
        super().__init__(mobject, **kwargs)
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


class Write(_NativeAnimation):
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
            **kwargs,
        )
        self.stroke_color = stroke_color

    def _native_params(self):
        if self.stroke_color is None:
            return {}
        return {"stroke_color": tuple(_color_to_rgb(self.stroke_color))}


class FadeIn(_NativeAnimation):
    _native_kind = "fade_in"

    def __init__(self, mobject, shift=_ORIGIN, scale=1.0, **kwargs):
        super().__init__(mobject, **kwargs)
        self.shift_vect = _vec3(shift)
        self.scale_factor = float(scale)

    def _native_params(self):
        return {"shift": self.shift_vect, "scale": self.scale_factor}


class FadeOut(FadeIn):
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


class VFadeIn(_NativeAnimation):
    _native_kind = "v_fade_in"

    def __init__(self, vmobject, suspend_mobject_updating=False, **kwargs):
        _refuse_unrouted(
            "VFadeIn()",
            [("suspend_mobject_updating", bool(suspend_mobject_updating))],
        )
        super().__init__(vmobject, **kwargs)


class VFadeOut(_NativeAnimation):
    _native_kind = "v_fade_out"

    def __init__(self, vmobject, remover=True, final_alpha_value=0.0, **kwargs):
        _refuse_unrouted(
            "VFadeOut()",
            [
                ("remover", remover is not True),
                ("final_alpha_value", final_alpha_value != 0.0),
            ],
        )
        super().__init__(vmobject, **kwargs)


class Rotate(_NativeAnimation):
    _native_kind = "rotate"

    def __init__(
        self,
        mobject,
        angle=_math.pi,
        axis=_OUT,
        run_time=1,
        rate_func=None,
        about_edge=_ORIGIN,
        **kwargs,
    ):
        about_point = kwargs.pop("about_point", None)
        super().__init__(mobject, run_time=run_time, rate_func=rate_func, **kwargs)
        self.angle = float(angle)
        self.axis = _vec3(axis)
        self.about_edge = _vec3(about_edge)
        self.about_point = None if about_point is None else _vec3(about_point)

    def _native_params(self):
        params = {
            "angle": self.angle,
            "axis": self.axis,
            "about_edge": self.about_edge,
        }
        if self.about_point is not None:
            params["about_point"] = self.about_point
        return params


class Rotating(_NativeAnimation):
    _native_kind = "rotating"

    def __init__(
        self,
        mobject,
        angle=_math.tau,
        axis=_OUT,
        about_point=None,
        about_edge=None,
        run_time=5.0,
        rate_func=None,
        suspend_mobject_updating=False,
        **kwargs,
    ):
        _refuse_unrouted(
            "Rotating()",
            [("suspend_mobject_updating", bool(suspend_mobject_updating))],
        )
        super().__init__(mobject, run_time=run_time, rate_func=rate_func, **kwargs)
        self.angle = float(angle)
        self.axis = _vec3(axis)
        self.about_point = None if about_point is None else _vec3(about_point)
        self.about_edge = None if about_edge is None else _vec3(about_edge)

    def _native_params(self):
        params = {"angle": self.angle, "axis": self.axis}
        if self.about_point is not None:
            params["about_point"] = self.about_point
        if self.about_edge is not None:
            params["about_edge"] = self.about_edge
        return params


class GrowFromCenter(_NativeAnimation):
    _native_kind = "grow_from_center"


class GrowArrow(_NativeAnimation):
    _native_kind = "grow_arrow"


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
        _refuse_unrouted("Restore()", [("path_func", path_func is not None)])
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
        _refuse_unrouted("Transform()", [("path_func", path_func is not None)])
        super().__init__(mobject, **kwargs)
        if target_mobject is None and not self._allows_deferred_target():
            raise NotImplementedError(
                "Transform without a target (in-place) awaits its binding"
            )
        self.target_mobject = target_mobject
        self.path_arc = float(path_arc)
        self.path_arc_axis = _vec3(path_arc_axis)

    def _native_params(self):
        return {"path_arc": self.path_arc, "path_arc_axis": self.path_arc_axis}

    def _allows_deferred_target(self):
        return False

    def _native_target(self):
        if self._allows_deferred_target():
            self.target_mobject = self.create_target()
        return self.target_mobject


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
        super().__init__(mobject.apply_function, function, run_time=run_time, **kwargs)


class ApplyPointwiseFunctionToCenter(Transform):
    def __init__(self, function, mobject, **kwargs):
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
        matrix = _np.array(matrix)
        if matrix.shape == (2, 2):
            new_matrix = _np.identity(3)
            new_matrix[:2, :2] = matrix
            matrix = new_matrix
        elif matrix.shape != (3, 3):
            raise Exception("Matrix has bad dimensions")
        return matrix


class ApplyComplexFunction(ApplyMethod):
    def __init__(self, function, mobject, **kwargs):
        self.function = function
        kwargs["path_arc"] = float(_np.log(function(complex(1))).imag)
        super().__init__(mobject.apply_complex_function, function, **kwargs)

    def init_path_func(self):
        self.path_arc = float(_np.log(self.function(complex(1))).imag)


class MoveToTarget(Transform):
    def __init__(self, mobject, **kwargs):
        target = getattr(mobject, "target", None)
        if target is None:
            raise Exception("MoveToTarget called on mobject without attribute 'target'")
        super().__init__(mobject, target, **kwargs)


class ReplacementTransform(Transform):
    _native_kind = "replacement_transform"


class TransformFromCopy(Transform):
    _native_kind = "transform_from_copy"

    def __init__(self, mobject, target_mobject, **kwargs):
        super().__init__(mobject, target_mobject, **kwargs)


class FadeTransform(_NativeAnimation):
    _native_kind = "fade_transform"
    _target_attr = "target_mobject"

    def __init__(self, mobject, target_mobject, stretch=True, dim_to_match=1, **kwargs):
        _refuse_unrouted(
            "FadeTransform()",
            [("stretch", stretch is not True), ("dim_to_match", dim_to_match != 1)],
        )
        super().__init__(mobject, **kwargs)
        self.target_mobject = target_mobject


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


class LaggedStartMap(LaggedStart):
    def __init__(self, anim_func, group, run_time=2.0, lag_ratio=0.05, **kwargs):
        # Reference composition.py:166 verbatim: one animation per member.
        anim_kwargs = dict(kwargs)
        anim_kwargs.pop("lag_ratio", None)
        super().__init__(
            *(anim_func(submob, **anim_kwargs) for submob in group),
            run_time=run_time,
            lag_ratio=lag_ratio,
        )



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
        ("manimlib.mobject.mobject", "Mobject"): Mobject,
        ("manimlib.mobject.mobject", "Group"): Group,
        ("manimlib.mobject.mobject", "Point"): Point,
        ("manimlib.mobject.types.vectorized_mobject", "VMobject"): VMobject,
        ("manimlib.mobject.svg.svg_mobject", "SVGMobject"): SVGMobject,
        (
            "manimlib.mobject.svg.string_mobject",
            "StringMobject",
        ): StringMobject,
        ("manimlib.camera.camera_frame", "CameraFrame"): CameraFrame,
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
        ("manimlib.mobject.geometry", "Polygon"): Polygon,
        ("manimlib.mobject.geometry", "RegularPolygon"): RegularPolygon,
        ("manimlib.mobject.geometry", "Triangle"): Triangle,
        ("manimlib.mobject.geometry", "ArrowTip"): ArrowTip,
        ("manimlib.mobject.geometry", "Rectangle"): Rectangle,
        ("manimlib.mobject.geometry", "Square"): Square,
        ("manimlib.mobject.shape_matchers", "SurroundingRectangle"): SurroundingRectangle,
        ("manimlib.mobject.shape_matchers", "BackgroundRectangle"): BackgroundRectangle,
        ("manimlib.mobject.shape_matchers", "Cross"): Cross,
        ("manimlib.mobject.shape_matchers", "Underline"): Underline,
        (
            "manimlib.mobject.svg.special_tex",
            "TexTextFromPresetString",
        ): TexTextFromPresetString,
        ("manimlib.mobject.svg.drawings", "Checkmark"): Checkmark,
        ("manimlib.mobject.svg.drawings", "Exmark"): Exmark,
        ("manimlib.animation.update", "UpdateFromFunc"): UpdateFromFunc,
        ("manimlib.animation.update", "UpdateFromAlphaFunc"): UpdateFromAlphaFunc,
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
        ("manimlib.mobject.numbers", "DecimalNumber"): DecimalNumber,
        ("manimlib.mobject.numbers", "Integer"): Integer,
        ("manimlib.mobject.types.dot_cloud", "DotCloud"): DotCloud,
        ("manimlib.mobject.types.dot_cloud", "TrueDot"): TrueDot,
        ("manimlib.mobject.types.dot_cloud", "GlowDots"): GlowDots,
        ("manimlib.mobject.types.dot_cloud", "GlowDot"): GlowDot,
        (
            "manimlib.mobject.types.image_mobject",
            "ImageMobject",
        ): ImageMobject,
        ("manimlib.mobject.vector_field", "VectorField"): VectorField,
        (
            "manimlib.mobject.vector_field",
            "TimeVaryingVectorField",
        ): TimeVaryingVectorField,
        ("manimlib.mobject.types.surface", "Surface"): Surface,
        ("manimlib.mobject.types.surface", "SGroup"): SGroup,
        ("manimlib.mobject.types.surface", "ParametricSurface"): ParametricSurface,
        ("manimlib.mobject.three_dimensions", "Sphere"): Sphere,
        ("manimlib.mobject.three_dimensions", "Cylinder"): Cylinder,
        ("manimlib.mobject.three_dimensions", "Cube"): Cube,
        ("manimlib.mobject.three_dimensions", "Prism"): Prism,
        ("manimlib.mobject.three_dimensions", "SurfaceMesh"): SurfaceMesh,
        ("manimlib.mobject.geometry", "Line"): Line,
        ("manimlib.mobject.geometry", "DashedLine"): DashedLine,
        ("manimlib.mobject.geometry", "Arrow"): Arrow,
        ("manimlib.mobject.geometry", "Vector"): Vector,
        ("manimlib.mobject.number_line", "NumberLine"): NumberLine,
        ("manimlib.mobject.number_line", "UnitInterval"): UnitInterval,
        ("manimlib.mobject.coordinate_systems", "CoordinateSystem"): CoordinateSystem,
        ("manimlib.mobject.coordinate_systems", "Axes"): Axes,
        ("manimlib.mobject.coordinate_systems", "ThreeDAxes"): ThreeDAxes,
        ("manimlib.mobject.coordinate_systems", "NumberPlane"): NumberPlane,
        ("manimlib.mobject.coordinate_systems", "ComplexPlane"): ComplexPlane,
        ("manimlib.mobject.svg.text_mobject", "MarkupText"): MarkupText,
        ("manimlib.mobject.svg.text_mobject", "Text"): Text,
        ("manimlib.mobject.svg.tex_mobject", "Tex"): Tex,
        ("manimlib.mobject.svg.tex_mobject", "TexText"): TexText,
        ("manimlib.mobject.svg.old_tex_mobject", "OldTex"): OldTex,
        ("manimlib.mobject.svg.old_tex_mobject", "OldTexText"): OldTexText,
        ("manimlib.mobject.changing", "TracingTail"): TracingTail,
        ("manimlib.mobject.value_tracker", "ValueTracker"): ValueTracker,
        ("manimlib.mobject.value_tracker", "ExponentialValueTracker"): ExponentialValueTracker,
        ("manimlib.mobject.value_tracker", "ComplexValueTracker"): ComplexValueTracker,
        ("manimlib.animation.creation", "ShowCreation"): ShowCreation,
        ("manimlib.animation.creation", "Uncreate"): Uncreate,
        ("manimlib.animation.creation", "Write"): Write,
        ("manimlib.animation.fading", "FadeIn"): FadeIn,
        ("manimlib.animation.fading", "FadeOut"): FadeOut,
        ("manimlib.animation.fading", "VFadeIn"): VFadeIn,
        ("manimlib.animation.fading", "VFadeOut"): VFadeOut,
        ("manimlib.animation.fading", "FadeTransform"): FadeTransform,
        ("manimlib.animation.fading", "FadeInFromPoint"): FadeInFromPoint,
        ("manimlib.animation.fading", "FadeOutToPoint"): FadeOutToPoint,
        ("manimlib.animation.composition", "AnimationGroup"): AnimationGroup,
        ("manimlib.animation.composition", "LaggedStart"): LaggedStart,
        ("manimlib.animation.composition", "LaggedStartMap"): LaggedStartMap,
        ("manimlib.animation.composition", "Succession"): Succession,
        ("manimlib.animation.rotation", "Rotate"): Rotate,
        ("manimlib.animation.rotation", "Rotating"): Rotating,
        ("manimlib.animation.growing", "GrowFromCenter"): GrowFromCenter,
        ("manimlib.animation.growing", "GrowArrow"): GrowArrow,
        ("manimlib.animation.transform", "Transform"): Transform,
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
        ("manimlib.animation.transform", "ReplacementTransform"): ReplacementTransform,
        ("manimlib.animation.transform", "TransformFromCopy"): TransformFromCopy,
        ("manimlib.animation.transform", "Restore"): Restore,
        ("manimlib.mobject.svg.brace", "Brace"): Brace,
        ("manimlib.mobject.svg.brace", "LineBrace"): LineBrace,
        ("manimlib.scene.scene", "Scene"): Scene,
        ("manimlib.scene.interactive_scene", "InteractiveScene"): InteractiveScene,
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

    def linear(t):
        return t

    def smooth(t):
        s = 1 - t
        return (t**3) * (10 * s * s + 5 * s * t + t * t)

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

    def there_and_back(t):
        new_t = 2 * t if t < 0.5 else 2 * (1 - t)
        return smooth(new_t)

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
        mob = func(*args, **kwargs)
        mob.add_updater(lambda m: m.become(func(*args, **kwargs)))
        return mob

    functions = {"always_redraw": always_redraw}
    module = _ensure_module("manimlib.mobject.mobject_update_utils")
    for name, function in functions.items():
        setattr(module, name, function)
        if not hasattr(_FMN_MODULE, name):
            setattr(_FMN_MODULE, name, function)


_install_mobject_functions()


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
