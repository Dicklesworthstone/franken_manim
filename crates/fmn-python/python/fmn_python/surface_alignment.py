"""Keep Surface topology coherent across the shared Transform protocol.

Marionette owns all UV resampling, field alignment and generation publication.
This adapter owns Python-visible metadata and callback/native route admission;
it neither evaluates authored UV functions nor implements a second resampler.
"""
from __future__ import annotations

from contextvars import ContextVar
from functools import wraps
from inspect import getattr_static
from types import SimpleNamespace
from weakref import ref

from .native_playback import _plain_state

from .surface_admission import GRID_BUDGET


def install_surface_alignment(native):
    g = vars(native)
    if g.get("_FMN_SURFACE_ALIGNMENT_INSTALLED", False):
        return
    if not callable(g.get("_align_surface_grids")) or not callable(g.get("_surface_grid_resolution")):
        raise ImportError("native UV-grid alignment seam is missing")
    Surface, Mobject, Transform = g["Surface"], g["Mobject"], g["Transform"]
    original_align, original_aligned = Mobject.align_points, Mobject.is_aligned_with
    triangles = Surface.compute_triangle_indices

    def pair_shapes(left, right):
        resolution = g["_surface_grid_resolution"]
        a, b = resolution(left), resolution(right)
        if (a is None) != (b is None):
            raise TypeError("surface alignment requires two native UV-grid surfaces")
        return a, b

    def empty_pair(left, right):
        # SGroup is a Surface by MRO, but its root owns no UV grid. Likewise,
        # a Group may wrap the same surface family. Only their drawable leaves
        # need UV alignment; never invent a grid for empty container records.
        return isinstance(right, Mobject) and not left.has_points() and not right.has_points()

    def align_points(self, mobject):
        if not isinstance(self, Surface) and not isinstance(mobject, Surface):
            return original_align(self, mobject)
        if empty_pair(self, mobject):
            return original_align(self, mobject)
        if not isinstance(self, Surface) or not isinstance(mobject, Surface):
            raise TypeError("a UV-grid surface cannot align with an unstructured mobject")
        a, b = pair_shapes(self, mobject)
        if a is None:  # Both are unstructured meshes, not sampled UV grids.
            return original_align(self, mobject)
        shape = (max(a[0], b[0]), max(a[1], b[1]))
        if shape[0] * shape[1] > GRID_BUDGET:
            raise ValueError(f"surface alignment exceeds its {GRID_BUDGET}-point UV-grid budget")
        # Allocate derived Python indices before native publication. Invoke the
        # shipped constructor on inert holders, not callbacks on either object.
        # Independent arrays prevent one endpoint's face ordering affecting its peer.
        indices = [
            vars(member)["triangle_indices"]
            if old == shape and tuple(vars(member).get("resolution", ())) == shape
            and "triangle_indices" in vars(member)
            else triangles(SimpleNamespace(resolution=shape))
            for member, old in ((self, a), (mobject, b))
        ]
        g["_align_surface_grids"](self, mobject)
        for member, data in zip((self, mobject), indices):
            vars(member).update(resolution=shape, triangle_indices=data)
        return self

    def is_aligned_with(self, mobject):
        if isinstance(self, Surface) or isinstance(mobject, Surface):
            if empty_pair(self, mobject):
                return original_aligned(self, mobject)
            if not isinstance(self, Surface) or not isinstance(mobject, Surface):
                return False
            a, b = pair_shapes(self, mobject)
            if a is None:
                return original_aligned(self, mobject)
            if a != b or self.data.dtype != mobject.data.dtype:
                return False
        return original_aligned(self, mobject)

    for cls, name, method in ((Mobject, "align_points", align_points),
                              (Surface, "align_points", align_points),
                              (Mobject, "is_aligned_with", is_aligned_with)):
        method.__name__ = name
        method.__qualname__ = cls.__qualname__ + "." + name
        method.__module__ = cls.__module__
        setattr(cls, name, method)

    original_indices = Surface.get_triangle_indices

    def get_triangle_indices(self):
        # Atlas-backed subclasses need not call Surface.__init__. Derive a
        # missing public index array lazily; retain authored face order when it
        # already exists. Alignment installs fresh arrays before any frame runs.
        if "triangle_indices" not in vars(self):
            shape = g["_surface_grid_resolution"](self)
            if shape is not None:
                vars(self)["triangle_indices"] = triangles(SimpleNamespace(resolution=shape))
        return original_indices(self)

    get_triangle_indices.__name__ = "get_triangle_indices"
    get_triangle_indices.__qualname__ = Surface.__qualname__ + ".get_triangle_indices"
    get_triangle_indices.__module__ = Surface.__module__
    Surface.get_triangle_indices = get_triangle_indices

    previous_requires = g["_requires_python_animation"]
    Scene, Builder = g["Scene"], g["_AnimationBuilder"]
    previous_play = Scene.play
    # A preceding sibling's authored begin can regrid a surface after route
    # admission. Keep mixed plays and compositions on their ordered callback
    # lifecycle; a single stock Transform has no intervening authored begin.
    # Its mobject may still be a bounded nested Surface/SGroup family.
    single_transform = ContextVar("fmn_single_surface_transform", default=None)
    # Native builtins own their module through __self__. Weak identities keep
    # an embedded module releasable when its globals are cleared.
    bindings = {}
    for name in ("_surface_grid_resolution", "_become_schema_compatible"):
        try:
            bindings[name] = ref(g.get(name))
        except TypeError:
            pass  # An older/incomplete boundary keeps the callback route.
    native_has_points = Mobject._has_points
    accessors = tuple((name, getattr_static(Mobject, name)) for name in
                      ("data", "_data_array", "_has_points", "n_records", "field_names"))

    def unchanged_topology(source, target):
        if len(bindings) != 2:
            return False
        grid_resolution = bindings["_surface_grid_resolution"]()
        schema_compatible = bindings["_become_schema_compatible"]()
        if (not callable(grid_resolution) or not callable(schema_compatible)
                or g.get("_surface_grid_resolution") is not grid_resolution
                or g.get("_become_schema_compatible") is not schema_compatible):
            return False
        pending, seen = [(source, target)], set()
        left_peers, right_peers, edges = {}, {}, 0
        while pending:
            left, right = pending.pop()
            pair = (id(left), id(right))
            if pair in seen:
                continue
            # A shared leaf cannot correspond to two independent leaves:
            # native and Python family walks must see the same alias graph.
            if (left_peers.setdefault(pair[0], pair[1]) != pair[1]
                    or right_peers.setdefault(pair[1], pair[0]) != pair[0]):
                return False
            seen.add(pair)
            if len(seen) > 65536:
                return False
            if not isinstance(left, Mobject) or not isinstance(right, Mobject):
                return False
            states = (_plain_state(left), _plain_state(right))
            if any(state is None for state in states):
                return False
            # The earlier protocol guard handles copy/alignment/interpolation
            # overrides. These storage accessors are also observed by the
            # Python alignment path; never bypass a custom property or an
            # instance-level replacement merely to avoid exporting a view.
            if any(getattr_static(member, name) is not expected
                   for member in (left, right) for name, expected in accessors):
                return False
            children = [state.get("submobjects") for state in states]
            if any(type(items) not in (list, tuple, g["_LiveSubmobjects"]) for items in children):
                return False
            if len(children[0]) != len(children[1]):
                return False
            edges += len(children[0])
            if edges > 65536:
                return False
            if isinstance(left, Surface) != isinstance(right, Surface):
                return False
            if isinstance(left, Surface):
                a, b = grid_resolution(left), grid_resolution(right)
                if a != b:
                    return False
                if a is None:
                    # Empty SGroup roots need no UV metadata. Unstructured
                    # meshes retain their existing callback protocol.
                    if native_has_points(left) or native_has_points(right):
                        return False
                else:
                    for member, state in zip((left, right), states):
                        shape, indices = state.get("resolution"), state.get("triangle_indices")
                        if (getattr_static(member, "resolution", None) is not shape
                                or getattr_static(member, "triangle_indices", None) is not indices):
                            return False
                        if (type(shape) not in (tuple, list) or len(shape) != 2
                                or any(type(value) is not int for value in shape)
                                or tuple(shape) != a):
                            return False
                        if (type(indices) is not g["_np"].ndarray or indices.ndim != 1
                                or indices.dtype.kind not in "iu"
                                or len(indices) != 6 * (a[0] - 1) * (a[1] - 1)):
                            return False
            if not schema_compatible(left, right):
                return False
            pending.extend(zip(children[0], children[1]))
        return True

    def requires(animation):
        if previous_requires(animation):
            return True
        if not isinstance(animation, Transform):
            return False
        # The preceding shipped-protocol classifier has already refused
        # authored attribute/family dispatch. Resolve no deferred target here.
        roots = tuple(getattr(animation, name, None) for name in
                      ("mobject", "target_mobject"))
        if not any(isinstance(root, Mobject) and any(
                isinstance(member, Surface) for member in root.get_family()) for root in roots):
            return False
        if (single_transform.get() is not animation
                or getattr_static(animation, "_native_kind", None)
                not in ("transform", "replacement_transform", "transform_from_copy")
                or not all(isinstance(root, Mobject) for root in roots)):
            return True
        # Atlas validates the real grid and field schema without materializing
        # placement or exporting writable records. No Python topology needs
        # publication when this proof holds. Existing native admission still
        # rejects live record views and authored per-frame callbacks.
        try:
            return not unchanged_topology(*roots)
        except (ValueError, TypeError, RuntimeError):
            # Preserve the established alignment/lifecycle error channel for
            # malformed grids instead of failing merely while choosing a route.
            return True

    @wraps(previous_play)
    def play(self, *animations, **kwargs):
        if len(animations) == 1 and isinstance(animations[0], Builder):
            animation = g["prepare_animation"](animations[0])
            if not isinstance(animation, g["Animation"]):
                raise TypeError("AnimationBuilder.build must return an Animation")
            animations = (animation,)
        candidate = (animations[0] if len(animations) == 1
                     and isinstance(animations[0], Transform) else None)
        token = single_transform.set(candidate)
        try:
            return previous_play(self, *animations, **kwargs)
        finally:
            single_transform.reset(token)

    g["_requires_python_animation"] = requires
    Scene.play = play
    g["_FMN_SURFACE_ALIGNMENT_INSTALLED"] = True
