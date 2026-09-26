"""Solid constructor hooks over Surface's native sampling/publication lifecycle.

The stock UV function selects Atlas's specialized solid builder, not a Python
sampler. An authored UV function instead uses Surface's bounded callback path.
In either case only pointlike columns are published into the actual subclass's
records, so custom schemas, children, and cooperative hooks keep their owners.
"""
from __future__ import annotations

import math

from .surface_admission import _finite


def install_solid_lifecycle(native):
    g = vars(native)
    if g.get("_FMN_SOLID_LIFECYCLE_INSTALLED", False):
        return
    if not g.get("_FMN_SURFACE_LIFECYCLE_INSTALLED", False):
        raise ImportError("solid construction requires the shared Surface lifecycle")
    Surface, Torus, Sphere, np = (g[name] for name in ("Surface", "Torus", "Sphere", "_np"))
    # Capture identities now. Later class/instance monkey patches are authored
    # functions, even if they retain a built-in method's name or signature.
    torus_uv, sphere_uv = Torus.uv_func, Sphere.uv_func
    # Invocation state, not copied/pickled attributes. A retained sphere
    # candidate exists only until the constructor finishes (also on failure).
    constructing = {}

    def admit(self):
        if self._is_bound():
            raise RuntimeError("a surface constructor requires a detached target; use init_points or become")
        if id(self) in constructing:
            raise RuntimeError("solid initialization is already in progress")

    def dimensions(self):
        keys = ("r1", "r2") if isinstance(self, Torus) else ("radius",)
        values = tuple(_finite(getattr(self, key), "surface " + key, record=True) for key in keys)
        if isinstance(self, Sphere):
            values += (bool(self.true_normals), bool(self.clockwise))
        return values

    def torus(self, u_range=(0, math.tau), v_range=(0, math.tau),
              r1=3.0, r2=1.0, **kwargs):
        admit(self)
        radii = tuple(_finite(value, "surface " + key, record=True)
                      for key, value in (("r1", r1), ("r2", r2)))
        constructing[id(self)] = {}
        try:
            self.r1, self.r2 = radii
            super(Torus, self).__init__(u_range=u_range, v_range=v_range, **kwargs)
            self._solid_params = ("torus", self.r1, self.r2)
            self._solid_native_height = self.get_height()
        finally:
            constructing.pop(id(self), None)

    def radial_normals(self, radius, nudge, preserved=None):
        if radius == 0:
            # Atlas's existing degenerate-sphere rule avoids division by zero.
            return
        nudge = _finite(nudge, "surface normal_nudge", record=True)
        if nudge < 0:
            raise ValueError("surface normal_nudge must be nonnegative")
        if preserved is not None:
            # Atlas computes radial normals before narrowing its sampled f64
            # points. Do not recompute untouched stock columns from f32 records:
            # that double rounding can change an otherwise identical frame.
            if all(self.data[key].tobytes() == preserved.data[key].tobytes()
                   for key in ("point", "d_normal_point")):
                return
        if vars(self).get("_is_animating", False) or getattr(self, "locked_data_keys", ()):
            raise RuntimeError("release the sphere's active animation before correcting its normals")
        factor = (radius + nudge) / radius
        with np.errstate(over="ignore", invalid="ignore"):
            normals = np.asarray(self.data['point'], dtype=np.float64) * factor
        if normals.shape != self.data['d_normal_point'].shape:
            raise ValueError("sphere normal points must match the native record shape")
        if (not np.isfinite(normals).all()
                or np.any(np.abs(normals) > np.finfo(np.float32).max)):
            raise ValueError("sphere normal points must remain finite and f32-representable")
        # Validate the complete column before replacing any of it. As in the
        # Reference this final correction also applies to an authored UV map or
        # a point-hook replacement, and happens after init_colors has run.
        self.data['d_normal_point'][:] = normals

    def sphere(self, u_range=(0, math.tau), v_range=(0, math.pi),
               resolution=(101, 51), radius=1.0, true_normals=True,
               clockwise=False, **kwargs):
        admit(self)
        radius = _finite(radius, "surface radius", record=True)
        true_normals, clockwise = bool(true_normals), bool(clockwise)
        context = dict(radius=radius, true_normals=true_normals)
        constructing[id(self)] = context
        try:
            self.radius, self.true_normals, self.clockwise = radius, true_normals, clockwise
            super(Sphere, self).__init__(u_range=u_range, v_range=v_range,
                                        resolution=resolution, **kwargs)
            if true_normals:
                preserved = context.get("candidate")
                if (context.get("sampled_radius") != radius
                        or context.get("sampled_nudge") != self.normal_nudge):
                    preserved = None
                radial_normals(self, radius, self.normal_nudge, preserved)
            self._solid_params = ("sphere", self.radius)
        finally:
            constructing.pop(id(self), None)

    def build_candidate(self, options, sample):
        if not isinstance(self, (Torus, Sphere)):
            return sample()
        values = dimensions(self)
        function = getattr(self.uv_func, "__func__", None)
        is_torus = isinstance(self, Torus)
        stock = function is (torus_uv if is_torus else sphere_uv)
        context = constructing.get(id(self))
        if stock:
            candidate = Surface.__new__(Surface)
            g["_install_live_state"](candidate)
            if is_torus:
                specs = candidate._build_torus(
                    g["_native_surface_shell_factory"], *values,
                    options["u_range"], options["v_range"], options["resolution"],
                    options["preferred_creation_axis"], options["epsilon"],
                    options["normal_nudge"], 0)
            else:
                radius, true_normals, clockwise = values
                if context is not None:
                    # The constructor's true_normals/radius arguments govern
                    # its final correction, even if init_data edits the recipe.
                    true_normals = (context["true_normals"]
                                    and radius == context["radius"])
                specs = candidate._build_sphere(
                    g["_native_surface_shell_factory"], radius,
                    options["u_range"], options["v_range"], options["resolution"],
                    true_normals, clockwise, options["preferred_creation_axis"],
                    options["epsilon"], options["normal_nudge"])
            if specs:
                raise RuntimeError("a solid sampler returned unexpected children")
        else:
            candidate = sample()
        if dimensions(self) != values:
            raise RuntimeError("solid shape parameters changed during sampling; geometry was not published")
        if not is_torus:
            if context is not None:
                context.update(candidate=candidate if stock else None,
                               sampled_radius=values[0], sampled_nudge=options["normal_nudge"])
            elif values[1] and not stock:
                # A live authored-UV rebuild has no constructor epilogue. Apply
                # the same correction to its detached candidate before publish.
                radial_normals(candidate, values[0], options["normal_nudge"])
        return candidate

    for cls, constructor in ((Torus, torus), (Sphere, sphere)):
        constructor.__name__, constructor.__qualname__, constructor.__module__ = (
            "__init__", cls.__qualname__ + ".__init__", cls.__module__)
        cls.__init__ = constructor
    g["_fmn_build_solid_candidate"] = build_candidate
    g["_FMN_SOLID_LIFECYCLE_INSTALLED"] = True
