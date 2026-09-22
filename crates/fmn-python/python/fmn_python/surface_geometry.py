"""Live UV-surface regeneration through Atlas, with native geometry publication.

Only geometry changes: handles, child identities, materials, scene membership,
updaters and live record views keep their existing owners. Grid-changing morphs
are separate from resampling a fixed topology and are never silently flattened.
"""
from __future__ import annotations

import math
import operator
from types import SimpleNamespace

_BUSY = "_fmn_surface_regenerating"
_METADATA = ("u_range", "v_range", "epsilon", "normal_nudge", "preferred_creation_axis")


def _controls(surface):
    resolution = tuple(operator.index(v) for v in surface.resolution)
    if len(resolution) != 2 or any(v < 2 for v in resolution) or math.prod(resolution) > 65_536:
        raise ValueError("surface regeneration requires a UV grid of 2..65536 points with both axes >= 2")
    domains = []
    for name in ("u_range", "v_range"):
        pair = tuple(float(v) for v in getattr(surface, name))
        if len(pair) != 2 or not all(math.isfinite(v) for v in pair) or pair[0] == pair[1]:
            raise ValueError(name + " must contain two distinct finite bounds")
        domains.append(pair)
    epsilon, nudge = float(surface.epsilon), float(surface.normal_nudge)
    if not math.isfinite(epsilon) or epsilon <= 0 or not math.isfinite(nudge) or nudge < 0:
        raise ValueError("surface epsilon must be positive; normal_nudge must be nonnegative; both must be finite")
    if any(not math.isfinite(value + epsilon) for pair in domains for value in pair):
        raise ValueError("surface derivative samples must remain finite")
    return resolution, *domains, epsilon, nudge


def install_surface_geometry(native):
    g = vars(native)
    if g.get("_FMN_SURFACE_GEOMETRY_INSTALLED", False):
        return
    for name in ("_surface_grid_resolution", "_copy_surface_geometry"):
        if not callable(g.get(name)):
            raise ImportError("native surface geometry seam is missing: " + name)
    Surface, Textured, Geometry, Mobject, np = (g[name] for name in
        ("Surface", "TexturedSurface", "TexturedGeometry", "Mobject", "_np"))
    original_become = Mobject.become
    triangles = Surface.compute_triangle_indices

    def resolution(self):
        result = g["_surface_grid_resolution"](self)
        if result is None:
            raise TypeError("surface regeneration requires an actual native UV grid")
        return result

    def idle(self):
        if vars(self).get("_is_animating", False) or getattr(self, "locked_data_keys", ()):
            raise RuntimeError("release the surface's active animation before regenerating its geometry")

    def init_points(self):
        """Resample the current UV recipe, preserving native identity and style.

        Designed for explicit rebuilds and ValueTracker-driven updaters. Like
        construction, sampling evaluates the current UV function in its own
        coordinates; previous affine geometry edits are not reapplied. Native
        topology must remain unchanged. Callback errors publish no new geometry;
        arbitrary side effects made by the callback itself are not rolled back.
        """
        if vars(self).get(_BUSY, False):
            raise RuntimeError("surface regeneration is already in progress")
        idle(self)
        controls = _controls(self)
        if controls[0] != resolution(self):
            raise ValueError("surface resolution changed; construct a new surface and use become() for topology replacement")
        if isinstance(self, Geometry):
            raise TypeError("indexed TexturedGeometry is not a UV-grid surface")
        vars(self)[_BUSY] = True
        try:
            # Source records are retained before callbacks; a callback that edits
            # the destination must not have its changes overwritten by this build.
            before = self.data.copy()
            owner = self._scene
            family = tuple(self.get_family())
            if isinstance(self, Textured):
                candidate = self.uv_surface
                if not isinstance(candidate, Surface) or candidate is self:
                    raise TypeError("TexturedSurface requires a distinct source Surface")
                if resolution(candidate) != controls[0]:
                    raise ValueError("textured surface and source require matching native UV topology")
            else:
                function = self.uv_func
                recipe = getattr(self, "passed_uv_func", None)
                if getattr(function, "__func__", None) is g["ParametricSurface"].uv_func:
                    function = recipe
                if not callable(function):
                    raise TypeError("surface uv_func must be callable")
                failure = None
                def sample(u, v):
                    nonlocal failure
                    if failure is not None:
                        raise failure
                    try:
                        value = np.asarray(function(u, v))
                        if value.shape != (3,) or value.dtype.kind not in "fiu":
                            raise ValueError("surface uv_func must return three real coordinates")
                        value = np.asarray(value, dtype=float)
                        if not np.isfinite(value).all() or np.any(np.abs(value) > np.finfo(np.float32).max):
                            raise ValueError("surface uv_func must return finite f32-representable coordinates")
                        return tuple(value)
                    except BaseException as error:
                        failure = error
                        raise
                # Bypass Python constructors, not Atlas: its existing native
                # sampler owns the finite-difference normal convention and grid.
                candidate = Surface.__new__(Surface)
                g["_install_live_state"](candidate)
                shape, u_range, v_range, epsilon, nudge = controls
                specs = candidate._build_parametric_surface(g["_native_surface_shell_factory"],
                            sample, u_range, v_range, shape, epsilon, nudge)
                g["_hang_native_children"](candidate, specs)
            idle(self)
            if not isinstance(self, Textured) and getattr(self, "passed_uv_func", None) is not recipe:
                raise RuntimeError("surface UV function changed during regeneration")
            if (self._scene is not owner or tuple(self.get_family()) != family
                    or _controls(self) != controls or resolution(self) != controls[0]
                    or not np.array_equal(self.data, before)):
                raise RuntimeError("surface changed during regeneration; sampled geometry was not published")
            g["_copy_surface_geometry"](self, candidate)
        finally:
            vars(self).pop(_BUSY, None)
        return None

    def become(self, mobject, match_updaters=False):
        # Marionette already transfers durable primitive topology. Publish its
        # Python grid projection as well: UV queries and triangle-index helpers
        # must not reshape the new native records using the old dimensions.
        other = mobject
        planned = {}
        if isinstance(other, Mobject):
            for source in other.get_family():
                if isinstance(source, Surface):
                    shape = g["_surface_grid_resolution"](source)
                    if shape is not None:
                        metadata = {key: getattr(source, key) for key in _METADATA if hasattr(source, key)}
                        for key in ("u_range", "v_range"):
                            if key in metadata:
                                metadata[key] = tuple(metadata[key])
                        metadata["resolution"] = tuple(shape)
                        holder = SimpleNamespace(resolution=shape)
                        metadata["triangle_indices"] = triangles(holder)
                        planned[id(source)] = metadata
        result = original_become(self, other, match_updaters=match_updaters)
        for receiver, source in zip(self.get_family(), other.get_family()):
            metadata = planned.get(id(source))
            if isinstance(receiver, Surface) and metadata is not None:
                vars(receiver).update(metadata)
        return result

    for cls, name, method in ((Surface, "init_points", init_points),
                               (Mobject, "become", become)):
        method.__name__, method.__qualname__, method.__module__ = name, cls.__qualname__ + "." + name, cls.__module__
        setattr(cls, name, method)
    g["_FMN_SURFACE_GEOMETRY_INSTALLED"] = True
