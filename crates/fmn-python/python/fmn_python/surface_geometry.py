"""Live UV-surface regeneration through Atlas, with native geometry publication.

Only geometry changes: handles, child identities, materials, scene membership,
updaters and live record views keep their existing owners. Grid-changing morphs
are separate from resampling a fixed topology and are never silently flattened.
"""
from __future__ import annotations

import itertools
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



def _solid_recipe(g, surface, controls):
    """Reuse specialized Atlas constructors, including analytic sphere normals.

    Unit UV functions do not encode cylinder radius/height/axis or disk/square
    dimensions. Sampling those functions alone would silently discard shape
    parameters. An authored UV override instead retains the generic callback path.
    """
    common = dict(resolution=controls[0], u_range=controls[1], v_range=controls[2],
                  epsilon=controls[3], normal_nudge=controls[4],
                  preferred_creation_axis=operator.index(surface.preferred_creation_axis))
    function = getattr(surface.uv_func, "__func__", None)
    for name in ("Sphere", "Torus", "Cone", "Cylinder", "Disk3D", "Square3D"):
        cls = g[name]
        if not isinstance(surface, cls) or function is not cls.uv_func:
            continue
        if name == "Sphere":
            values = dict(radius=float(surface.radius), true_normals=bool(surface.true_normals),
                          clockwise=bool(surface.clockwise))
        elif name == "Torus":
            values = dict(r1=float(surface.r1), r2=float(surface.r2))
        elif name in ("Cone", "Cylinder"):
            axis = tuple(float(value) for value in surface.axis)
            if len(axis) != 3 or not all(math.isfinite(value) for value in axis):
                raise ValueError("surface axis must contain three finite coordinates")
            values = dict(height=float(surface.height), radius=float(surface.radius), axis=axis)
        elif name == "Disk3D":
            values = dict(radius=float(surface.radius))
        else:
            values = dict(side_length=float(surface.side_length))
        if any(type(value) is float and not math.isfinite(value) for value in values.values()):
            raise ValueError("surface shape parameters must be finite")
        return cls, dict(common, **values)
    return None


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
        if isinstance(self, Geometry):
            raise TypeError("indexed TexturedGeometry is not a UV-grid surface")
        shape = tuple(operator.index(v) for v in itertools.islice(iter(self.resolution), 3))
        if len(shape) == 2 and 0 in shape and all(0 <= v <= 65_536 for v in shape):
            # Empty Surface construction calls this hook through _engine_init.
            # There are no samples to regenerate, and no triangle grid to copy.
            if self.n_records():
                raise ValueError("surface resolution no longer matches its native records")
            return None
        idle(self)
        controls = _controls(self)
        if controls[0] != resolution(self):
            raise ValueError("surface resolution changed; construct a new surface and use become() for topology replacement")
        vars(self)[_BUSY] = True
        try:
            # Source records are retained before callbacks; a callback that edits
            # the destination must not have its changes overwritten by this build.
            before = self.data.copy()
            owner = vars(self).get("_scene")
            bound = self._is_bound()
            family = tuple(self.get_family())
            uv_method = self.uv_func
            uv_identity = (getattr(uv_method, "__func__", uv_method),
                           getattr(uv_method, "__self__", None))
            solid = None if isinstance(self, Textured) else _solid_recipe(g, self, controls)
            recipe = getattr(self, "passed_uv_func", None)
            if solid is not None:
                cls, options = solid
                candidate = cls(**options)
            elif isinstance(self, Textured):
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
            if solid is not None and _solid_recipe(g, self, _controls(self)) != solid:
                raise RuntimeError("surface shape parameters changed during regeneration")
            if not isinstance(self, Textured) and getattr(self, "passed_uv_func", None) is not recipe:
                raise RuntimeError("surface UV function changed during regeneration")
            current_uv = self.uv_func
            if (getattr(current_uv, "__func__", current_uv) is not uv_identity[0]
                    or getattr(current_uv, "__self__", None) is not uv_identity[1]):
                raise RuntimeError("surface UV function changed during regeneration")
            # Detached proxies have no _scene attribute. Check native admission
            # as well as its host mirror: a callback may bind this very object.
            if (vars(self).get("_scene") is not owner or self._is_bound() != bound
                    or tuple(self.get_family()) != family
                    or _controls(self) != controls or resolution(self) != controls[0]
                    or not np.array_equal(self.data, before)):
                raise RuntimeError("surface changed during regeneration; sampled geometry was not published")
            g["_copy_surface_geometry"](self, candidate)
            if solid is not None:
                for key in ("_solid_params", "_solid_native_height"):
                    if key in vars(candidate):
                        vars(self)[key] = vars(candidate)[key]
        finally:
            vars(self).pop(_BUSY, None)
        return None

    def become(self, mobject, match_updaters=False):
        other = mobject
        # Marionette already transfers durable primitive topology. Publish its
        # Python grid projection as well: UV queries and triangle-index helpers
        # must not reshape the new native records using the old dimensions.
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
    # Cylinder historically overrides the Surface initializer with a no-op.
    # Cone and Line3D inherit it; route the same method without another sampler.
    g["Cylinder"].init_points = Surface.init_points
    g["_FMN_SURFACE_GEOMETRY_INSTALLED"] = True
