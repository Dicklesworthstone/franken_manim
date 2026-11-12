"""Three-dimensional plotting in the axes' actual coordinate chart.

Sampling and finite-difference normals stay in Atlas. The host maps authored
coordinate triples through c2p, freezing the ordinary affine chart per rebuild.
"""
from __future__ import annotations

from .graphing import _bind_method
from .surface_admission import sampling_options as _sampling_options


class _ChartRecipe:
    def __init__(self, axes, function, np, c2p, coords_to_point):
        self.axes, self.function, self.np = axes, function, np
        self.c2p, self.coords_to_point = c2p, coords_to_point

    def __deepcopy__(self, memo):
        # Match a Python function closure: keep its callable and external axes,
        # never deepcopy a live native module or silently substitute a chart.
        return self

    def __getstate__(self):
        # NumPy is a runtime dependency, not scene state. Preserve normal pickle
        # memo handling for axes and authored callables without serializing it.
        return self.axes, self.function, self.c2p, self.coords_to_point

    def __setstate__(self, state):
        import numpy as np
        self.axes, self.function, self.c2p, self.coords_to_point = state
        self.np = np

    def point(self, value, *, world=False, allow_undefined=False):
        np = self.np
        point = np.asarray(value)
        if point.shape != (3,) or point.dtype.kind not in "biuf":
            raise ValueError("surface plotting requires three real coordinates")
        if point.dtype.kind == "f" and point.dtype.itemsize > 8:
            # A finite extended-precision input must not narrow to inf and
            # acquire the semantics of an authored undefined sample. World
            # coordinates also retain their f32 bound before that rounding.
            limit = np.finfo(np.float32 if world else np.float64).max
            if np.any(np.isfinite(point) & (np.abs(point) > limit)):
                raise ValueError("surface plotting coordinates exceed their finite representation")
        point = point.astype(float, copy=True)
        finite = np.isfinite(point)
        if ((not allow_undefined and not finite.all())
                or (world and np.any(finite & (np.abs(point) > np.finfo(np.float32).max)))):
            raise ValueError("surface plotting requires finite, float32-representable world coordinates")
        return point

    def __call__(self, u, v):
        return self.point(self.axes.c2p(*self.point(self.function(u, v))), world=True)

    def prepare(self, *, allow_undefined=False):
        """One immutable affine chart, or explicit authored mapping dispatch."""
        np, axes = self.np, self.axes
        methods = axes.c2p, axes.coords_to_point
        affine = (getattr(methods[0], "__func__", None) is self.c2p
                  and getattr(methods[1], "__func__", None) is self.coords_to_point)

        def chart():
            return np.array([self.point(axes.c2p(*p)) for p in
                            ((0, 0, 0), (1, 0, 0), (0, 1, 0), (0, 0, 1))])

        before = chart() if affine else None
        if affine:
            origin = before[0]
            basis = before[1:] - origin

            def sample(u, v):
                point = self.point(self.function(u, v), allow_undefined=allow_undefined)
                if not np.isfinite(point).all():
                    # An undefined coordinate has no image under this chart.
                    # Preserve it for Atlas's mask instead of evaluating 0*inf
                    # and fabricating finite geometry or emitting warnings.
                    return self.point(point, world=True, allow_undefined=True)
                x, y, z = point
                # Fixed-order arithmetic; no BLAS or second geometry sampler.
                return self.point(origin + x * basis[0] + y * basis[1] + z * basis[2],
                                  world=True)
        else:
            # A custom chart may be nonlinear. Do not replace it by a basis
            # inferred at four points; keep its actual public c2p dispatch.
            def sample(u, v):
                point = self.point(self.function(u, v), allow_undefined=allow_undefined)
                if not np.isfinite(point).all():
                    return self.point(point, world=True, allow_undefined=True)
                return self.point(axes.c2p(*point), world=True, allow_undefined=allow_undefined)

        def verify():
            if (axes.c2p, axes.coords_to_point) != methods or (affine and not np.array_equal(chart(), before)):
                raise RuntimeError("surface axes changed during sampling; geometry was not published")

        return sample, verify


def install_surface_plotting(native):
    g = vars(native)
    if g.get("_FMN_SURFACE_PLOTTING_INSTALLED", False):
        return
    ThreeDAxes, ParametricSurface, np = (g[name] for name in
                                        ("ThreeDAxes", "ParametricSurface", "_np"))
    default_c2p, default_coords = ThreeDAxes.c2p, g["Axes"].coords_to_point
    original_init = ParametricSurface.__init__

    def initialize(self, uv_func, u_range=(0, 1), v_range=(0, 1), **kwargs):
        # The native constructor allocates the complete surface before invoking
        # callbacks. All public parametric entry points must bound that request,
        # not just the chart helpers. Keep the original callable as the recipe.
        if not callable(uv_func):
            raise TypeError("surface function must be callable")
        options = _sampling_options(dict(kwargs, u_range=u_range, v_range=v_range))
        original_init(self, uv_func, **options)

    _bind_method(ParametricSurface, "__init__", initialize)

    def get_parametric_surface(self, func, color=g["BLUE_E"], opacity=.9, **kwargs):
        if not callable(func):
            raise TypeError("surface function must be callable")
        kwargs = _sampling_options(kwargs)
        recipe = _ChartRecipe(self, func, np, default_c2p, default_coords)
        sample, verify = recipe.prepare(allow_undefined=kwargs["allow_undefined"])
        surface = ParametricSurface(sample, color=color, opacity=opacity, **kwargs)
        verify()
        # Keep the recipe, not this construction's frozen basis: a later
        # init_points() pass prepares the then-current chart exactly once.
        surface.passed_uv_func = recipe
        return surface

    def get_graph(self, func, color=g["BLUE_E"], opacity=.9, u_range=None, v_range=None, **kwargs):
        if not callable(func):
            raise TypeError("surface graph function must be callable")

        def coordinates(u, v):
            value = np.asarray(func(u, v))
            if value.ndim != 0 or value.dtype.kind not in "biuf":
                raise ValueError("surface graph function must return a real scalar")
            # Preserve extended floating-point values until chart admission
            # can distinguish authored infinities from finite overflow.
            return u, v, value[()]

        return get_parametric_surface(self, coordinates, color=color, opacity=opacity,
                                      u_range=self.x_range[:2] if u_range is None else u_range,
                                      v_range=self.y_range[:2] if v_range is None else v_range, **kwargs)

    def prepare(function, *, allow_undefined=False):
        return (function.prepare(allow_undefined=allow_undefined)
                if isinstance(function, _ChartRecipe) else (function, None))

    _bind_method(ThreeDAxes, "get_parametric_surface", get_parametric_surface)
    _bind_method(ThreeDAxes, "get_graph", get_graph)
    g["_fmn_prepare_surface_function"] = prepare
    g["_FMN_SURFACE_PLOTTING_INSTALLED"] = True
