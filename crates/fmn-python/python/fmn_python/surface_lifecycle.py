"""Authored Surface initialization over Atlas sampling and native UV topology.

Initial construction and live regeneration have different admission contracts:
empty grids/strips are legal initially; live regridding keeps its existing rules.
No callback, geometry sampler, frame clock or material is emulated here.
"""
from __future__ import annotations

from .surface_admission import sampling_options


def install_surface_lifecycle(native):
    g = vars(native)
    if g.get("_FMN_SURFACE_LIFECYCLE_INSTALLED", False):
        return
    if not callable(g.get("_initialize_surface_grid")):
        raise ImportError("native surface initialization seam is missing")
    Surface, Parametric = (g[name] for name in ("Surface", "ParametricSurface"))
    regenerate = Surface.init_points
    # Invocation state, not copied/pickled attributes. A callback may create a
    # distinct nested surface, but cannot recursively initialize its own owner.
    constructing, sampling = set(), set()

    def idle(self):
        if vars(self).get("_is_animating", False) or getattr(self, "locked_data_keys", ()):
            raise RuntimeError("release the surface's active animation before initializing its geometry")

    def controls(self):
        return sampling_options({name: getattr(self, name) for name in
            ("resolution", "u_range", "v_range", "epsilon", "normal_nudge",
             "preferred_creation_axis")})

    def initialize(self, color=g["GREY"], shading=(.3, .2, .4), depth_test=True,
                   u_range=(0, 1), v_range=(0, 1), resolution=(101, 101),
                   preferred_creation_axis=1, epsilon=.001, normal_nudge=.001,
                   **kwargs):
        if self._is_bound():
            raise RuntimeError("a surface constructor requires a detached target; use init_points or become")
        if id(self) in constructing:
            raise RuntimeError("surface initialization is already in progress")
        opacity = kwargs.pop("opacity", None)
        z_index = int(kwargs.pop("z_index", 0))
        g["_refuse_unrouted"](type(self).__name__ + "()",
                              [(key, True) for key in sorted(kwargs)])
        options = sampling_options(dict(resolution=resolution, u_range=u_range,
            v_range=v_range, preferred_creation_axis=preferred_creation_axis,
            epsilon=epsilon, normal_nudge=normal_nudge))
        options.update(color=g["GREY"] if color is None else color,
                       opacity=1.0 if opacity is None else opacity,
                       shading=(.3, .2, .4) if shading is None else shading,
                       depth_test=bool(depth_test), z_index=z_index)
        constructing.add(id(self))
        try:
            # Mobject installs live fields, then the native engine dispatches
            # init_data, init_points and init_uniforms through the actual MRO.
            # Seeds follow _install_live_state so it cannot reset their values.
            super(Surface, self).__init__(**options)
            # An authored hook may replace sampling entirely. Its actual table
            # must still gain durable native topology before entering a Scene.
            shape = controls(self)["resolution"]
            idle(self)
            g["_initialize_surface_grid"](self, shape)
            self.compute_triangle_indices()
            self.set_z_index(self.z_index)
            self.init_colors()
        finally:
            constructing.discard(id(self))

    def parametric(self, uv_func, u_range=(0, 1), v_range=(0, 1), **kwargs):
        if self._is_bound():
            raise RuntimeError("a surface constructor requires a detached target; use init_points or become")
        if id(self) in constructing:
            raise RuntimeError("surface initialization is already in progress")
        if not callable(uv_func):
            raise TypeError("surface function must be callable")
        self.passed_uv_func = uv_func
        super(Parametric, self).__init__(u_range=u_range, v_range=v_range, **kwargs)

    def init_uniforms(self):
        super(Surface, self).init_uniforms()
        # This is the Surface hook, not a post-hook overwrite: overrides which
        # call super() can deliberately replace these values afterwards.
        self.set_shading(*self.shading)
        (self.apply_depth_test if self.depth_test else self.deactivate_depth_test)()

    def init_points(self):
        if id(self) not in constructing:
            return regenerate(self)
        if id(self) in sampling:
            raise RuntimeError("surface initialization sampling is already in progress")
        sampling.add(id(self))
        try:
            options = controls(self)
            idle(self)
            before = self.data.copy()
            owner, bound = vars(self).get("_scene"), self._is_bound()
            family = tuple(id(member) for member in self.get_family())
            function = self.uv_func
            identity = (getattr(function, "__func__", function),
                        getattr(function, "__self__", None))
            recipe = getattr(self, "passed_uv_func", None)
            if identity[0] is Parametric.uv_func:
                function = recipe
            if not callable(function):
                raise TypeError("surface uv_func must be callable")
            verify = None
            prepare = g.get("_fmn_prepare_surface_function")
            if prepare is not None:
                function, verify = prepare(function)
            def sample():
                candidate = Surface.__new__(Surface)
                g["_install_live_state"](candidate)
                specs = candidate._build_parametric_surface(
                    g["_native_surface_shell_factory"], function,
                    options["u_range"], options["v_range"], options["resolution"],
                    options["epsilon"], options["normal_nudge"])
                if specs:
                    raise RuntimeError("a UV surface sampler returned unexpected children")
                return candidate

            # Stock solids keep their specialized Atlas kernels; authored UV
            # overrides use this same bounded sampler and publication contract.
            build_solid = g.get("_fmn_build_solid_candidate")
            candidate = sample() if build_solid is None else build_solid(self, options, sample)
            if verify is not None:
                verify()
            idle(self)
            current = self.uv_func
            if (vars(self).get("_scene") is not owner or self._is_bound() != bound
                    or tuple(id(member) for member in self.get_family()) != family
                    or controls(self) != options
                    or getattr(self, "passed_uv_func", None) is not recipe
                    or getattr(current, "__func__", current) is not identity[0]
                    or getattr(current, "__self__", None) is not identity[1]
                    or self.data.dtype != before.dtype
                    or self.data.tobytes() != before.tobytes()):
                raise RuntimeError("surface changed during initialization; sampled geometry was not published")
            g["_initialize_surface_grid"](self, options["resolution"], candidate)
        finally:
            sampling.discard(id(self))
        return None

    for cls, name, method in ((Surface, "__init__", initialize),
                              (Parametric, "__init__", parametric),
                              (Surface, "init_uniforms", init_uniforms),
                              (Surface, "init_points", init_points)):
        method.__name__, method.__qualname__, method.__module__ = (
            name, cls.__qualname__ + "." + name, cls.__module__)
        setattr(cls, name, method)
    _install_authored_solids(g, Surface)
    g["_FMN_SURFACE_LIFECYCLE_INSTALLED"] = True


_HOOKS = ("uv_func", "init_points", "init_data", "init_uniforms", "init_colors")


def _install_authored_solids(g, Surface):
    """Construct solid subclasses with authored hooks in Reference order.

    Sphere, Torus, Cylinder, Cone, Line3D, Disk3D and Square3D build their
    stock geometry with specialized Atlas constructors, which never call the
    Surface hooks. A subclass that authors one (most often uv_func) is
    instead constructed as the Reference does it: shape attributes first,
    then Surface.__init__, whose hooks dispatch through the real MRO and
    whose sampling evaluates self.uv_func, then the Reference's post-steps.
    Unmodified classes keep the specialized constructors.
    """
    np, tau, pi, out = g["_np"], g["TAU"], g["PI"], g["OUT"]
    Cylinder = g["Cylinder"]
    # Ids of cylinders whose Reference-order construction is in progress.
    reference = set()

    def authored(cls, names=_HOOKS):
        # A hook is authored when the class that defines it is not part of
        # the portal (manimlib, its bootstrap namespace, fmn_python).
        for name in names:
            owner = next((klass for klass in cls.__mro__ if name in vars(klass)), None)
            module = getattr(owner, "__module__", "") or ""
            if owner is not None and module.partition(".")[0] not in ("manimlib", "fmn_python", "builtins"):
                return True
        return False

    def z_to_vector(axis):
        return np.array(g["_BridgeMobject"]._z_to_vector(g["_vec3"](axis)))

    def sphere(self, u_range=(0, tau), v_range=(0, pi), resolution=(101, 51), radius=1.0,
               true_normals=True, clockwise=False, **kwargs):
        self.radius, self.clockwise, self.true_normals = radius, clockwise, bool(true_normals)
        Surface.__init__(self, u_range=u_range, v_range=v_range, resolution=resolution, **kwargs)
        if true_normals:
            self.data["d_normal_point"] = self.data["point"] * ((radius + self.normal_nudge) / radius)

    def torus(self, u_range=(0, tau), v_range=(0, tau), r1=3.0, r2=1.0, **kwargs):
        self.r1, self.r2 = r1, r2
        Surface.__init__(self, u_range=u_range, v_range=v_range, **kwargs)

    def cylinder(self, u_range=(0, tau), v_range=(-1, 1), resolution=(101, 11), height=2, radius=1,
                 axis=out, **kwargs):
        self.height, self.radius, self.axis = height, radius, axis
        reference.add(id(self))
        try:
            Surface.__init__(self, u_range=u_range, v_range=v_range, resolution=resolution, **kwargs)
        finally:
            reference.discard(id(self))

    def cone(self, u_range=(0, tau), v_range=(0, 1), *args, **kwargs):
        cylinder(self, u_range, v_range, *args, **kwargs)

    def line3d(self, start, end, width=0.05, resolution=(21, 25), **kwargs):
        start, end = (np.array(g["_vec3"](point), dtype=float) for point in (start, end))
        axis = end - start
        cylinder(self, height=float(np.linalg.norm(axis)), radius=width / 2, axis=axis,
                 resolution=resolution, **kwargs)
        self.shift((start + end) / 2)

    def disk3d(self, radius=1, u_range=(0, 1), v_range=(0, tau), resolution=(2, 100), **kwargs):
        self.radius = radius
        Surface.__init__(self, u_range=u_range, v_range=v_range, resolution=resolution, **kwargs)
        self.scale(radius)

    def square3d(self, side_length=2.0, u_range=(-1, 1), v_range=(-1, 1), resolution=(2, 2), **kwargs):
        self.side_length = side_length
        Surface.__init__(self, u_range=u_range, v_range=v_range, resolution=resolution, **kwargs)
        self.scale(side_length / 2)

    regenerate = Cylinder.init_points

    def cylinder_points(self):
        """Reference Cylinder.init_points: sample the UV chart, then place it.

        The stock recipe regenerates through the native solid constructor,
        which already applies radius, height and axis. Only a chart sampled
        through the UV callback (Reference-order construction, or an
        authored uv_func) is placed here.
        """
        if id(self) in reference:
            Surface.init_points(self)
        else:
            regenerate(self)
            if not authored(type(self), ("uv_func",)):
                return None
        self.scale(self.radius)
        self.set_depth(self.height, stretch=True)
        self.apply_matrix(z_to_vector(self.axis))
        return None

    cylinder_points.__name__, cylinder_points.__qualname__ = "init_points", "Cylinder.init_points"
    cylinder_points.__module__ = Cylinder.__module__
    Cylinder.init_points = cylinder_points
    for name, construct in (("Sphere", sphere), ("Torus", torus), ("Cylinder", cylinder),
                            ("Cone", cone), ("Line3D", line3d), ("Disk3D", disk3d),
                            ("Square3D", square3d)):
        cls = g[name]
        native = vars(cls)["__init__"]

        def __init__(self, *args, _cls=cls, _native=native, _construct=construct, **kwargs):
            if type(self) is _cls or not authored(type(self)):
                return _native(self, *args, **kwargs)
            return _construct(self, *args, **kwargs)

        __init__.__name__, __init__.__qualname__ = "__init__", cls.__qualname__ + ".__init__"
        __init__.__module__, __init__.__doc__ = cls.__module__, native.__doc__
        __init__.__wrapped__ = native
        cls.__init__ = __init__
