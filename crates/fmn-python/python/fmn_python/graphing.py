"""Live function graphs composed from Chisel's existing native path operations.

Python owns callable evaluation and coordinate-system dispatch. Corner paths,
subpath joins, smoothing, record resizing and rendering retain their existing
native owners. No second curve kernel or frame clock is introduced here.
"""
from __future__ import annotations

from contextvars import ContextVar
import itertools
import math
from typing import Any

_MAX_SAMPLES = 65_536
_MAX_DISCONTINUITIES = 1_024
_MAX_RECORDS = 4 * _MAX_SAMPLES + 2 * _MAX_DISCONTINUITIES
_EPSILON = 1e-6
_BINDING = "_fmn_function_graph_binding"
_BUSY = "_fmn_function_graph_busy"
# Keep the public bind hook's original callable identity. A get_graph() call
# supplies a scalar function; direct bind_graph_to_func() supplies an array one.
_SCALAR_BIND = ContextVar("fmn_scalar_graph_binding", default=None)


def _finite(value: Any, name: str) -> float:
    result = float(value)
    if not math.isfinite(result):
        raise ValueError(name + " must be finite")
    return result


def _discontinuities(values, low, high):
    result = []
    for value in itertools.islice(iter(values), _MAX_DISCONTINUITIES + 1):
        if len(result) == _MAX_DISCONTINUITIES:
            raise ValueError("graph discontinuities exceed their 1024-item budget")
        result.append(_finite(value, "graph discontinuity"))
    return sorted(set(value for value in result if low <= value <= high))


def _segments(samples, discontinuities, epsilon, np):
    """Subtract excluded intervals without ever editing the baseline grid.

    Exclusion bands may overlap, touch either endpoint, or cover the domain.
    Discontinuity neighbors are added rather than replacing the rightmost
    samples. Work is bounded before allocating evaluation/record buffers.
    """
    low, high = samples[0], samples[-1]
    intervals, cursor = [], low
    for value in discontinuities:
        left, right = max(low, value - epsilon), min(high, value + epsilon)
        if left > cursor:
            intervals.append((cursor, left))
        cursor = max(cursor, right)
    if cursor < high:
        intervals.append((cursor, high))
    result, total = [], 0
    for start, stop in intervals:
        first, last = np.searchsorted(samples, [start, stop])
        values = np.unique(np.concatenate(([start], samples[first:last], [stop])))
        total += len(values)
        if total > _MAX_SAMPLES:
            raise ValueError("live graph samples exceed their 65536-point budget")
        result.append(values)
    return result


def _evaluate(function, xs, scalar, np):
    # Give authored code a detached array; mutation must not change sampling.
    values = [function(float(x)) for x in xs] if scalar else function(xs.copy())
    array = np.asarray(values)
    if array.dtype.kind not in "biuf":
        raise TypeError("graph function must return real numeric values")
    if array.ndim == 0:
        array = np.full(len(xs), float(array))
    elif array.shape == xs.shape:
        array = array.astype(float, copy=False)
    else:
        raise ValueError("graph function must return a scalar or one value per sample")
    if not np.isfinite(array).all():
        raise ValueError("graph function returned nonfinite values; declare its discontinuities")
    return array


class _Binding:
    """Immutable updater ownership, copied like the callback's closure."""
    def __init__(self, updater, axes, samples):
        self.updater, self.axes, self.samples = updater, axes, samples

    def __deepcopy__(self, memo):
        return self


class _GraphFunction:
    """Scalar/array query view of a bound callable, without guessing its mode."""
    def __init__(self, function, scalar, np):
        self.function, self.scalar, self.np = function, scalar, np

    def __call__(self, x):
        np = self.np
        values = np.asarray(x, dtype=float)
        if not np.isfinite(values).all() or values.size > _MAX_SAMPLES:
            raise ValueError("graph query coordinates must be finite and bounded")
        result = _evaluate(self.function, values.reshape(-1), self.scalar, np)
        return float(result[0]) if values.ndim == 0 else result.reshape(values.shape)

    def __deepcopy__(self, memo):
        # Like a Reference closure, keep its author callable and coordinate
        # dependencies. Never deepcopy the native module or NumPy namespace.
        return self


def _bind_method(cls, name, function):
    function.__name__ = name
    function.__qualname__ = cls.__qualname__ + "." + name
    function.__module__ = cls.__module__
    setattr(cls, name, function)


def install_graphing(native):
    g = vars(native)
    if g.get("_FMN_GRAPHING_INSTALLED", False):
        return
    Coordinates, VMobject, np = g["CoordinateSystem"], g["VMobject"], g["_np"]

    def bind(self, graph, func, jagged=False, get_discontinuities=None):
        if not isinstance(graph, VMobject):
            raise TypeError("bind_graph_to_func requires a VMobject graph")
        if not callable(func):
            raise TypeError("graph function must be callable")
        if get_discontinuities is not None and not callable(get_discontinuities):
            raise TypeError("get_discontinuities must be callable or None")
        if not isinstance(jagged, bool):
            raise TypeError("jagged must be bool")
        if vars(graph).get(_BUSY, False):
            raise RuntimeError("cannot rebind a graph during its update")
        # A custom pointlike schema cannot be copied from a plain native path.
        # Refuse before the native resize rather than partly copying fields.
        if tuple(graph.pointlike_data_keys) != ("point",):
            raise TypeError("live function graphs require the VMobject pointlike schema")
        previous = vars(graph).get(_BINDING)
        if previous is not None and previous.axes is self:
            samples = previous.samples
        else:
            points = graph.get_points()
            if not 2 <= len(points) <= _MAX_SAMPLES:
                raise ValueError("binding requires 2..65536 graph point samples")
            # Public coordinate conversion handles transformed and custom axes;
            # projecting onto a hard-coded x_axis does not do so in general.
            samples = np.array([_finite(self.p2c(point)[0], "graph sample x")
                                for point in points], dtype=float)
            samples = np.unique(samples)
            if len(samples) < 2:
                raise ValueError("function graph samples must span a nonzero x interval")
            samples.setflags(write=False)
        low, high = float(samples[0]), float(samples[-1])
        epsilon = _finite(getattr(graph, "epsilon", _EPSILON), "graph epsilon")
        if epsilon <= 0:
            raise ValueError("graph epsilon must be positive")
        epsilon = max(epsilon, _EPSILON)
        fixed_values = getattr(graph, "discontinuities", ())
        fixed = _discontinuities(() if fixed_values is None else fixed_values, low, high)
        context = _SCALAR_BIND.get()
        scalar = context is not None and context[0] is graph and context[1] is func
        query = _GraphFunction(func, scalar, np)

        def update(current):
            if vars(current).get(_BUSY, False):
                raise RuntimeError("live graph update cannot reenter itself")
            vars(current)[_BUSY] = True
            try:
                ds = (fixed if get_discontinuities is None else
                      _discontinuities(get_discontinuities(), low, high))
                segments = _segments(samples, ds, epsilon, np)
                candidate = VMobject()
                if segments:
                    xs = np.concatenate(segments)
                    ys = _evaluate(func, xs, scalar, np)
                    # Keep authored c2p dispatch, with detached inputs for the
                    # same reason as callable evaluation above.
                    points = np.asarray(self.c2p(xs.copy(), ys.copy()))
                    if (points.dtype.kind not in "biuf" or points.shape != (len(xs), 3)
                            or not np.isfinite(points).all()
                            or np.any(np.abs(points) > np.finfo(np.float32).max)):
                        raise ValueError("graph coordinates must be finite f32-representable (N, 3) points")
                    offset = 0
                    for segment in segments:
                        part = VMobject()
                        part.set_points_as_corners(points[offset:offset + len(segment)])
                        if not jagged:
                            part.make_smooth(approx=True, recurse=False)
                        candidate.add_subpath(part.get_points())
                        if candidate.get_num_points() > _MAX_RECORDS:
                            raise ValueError("live graph exceeds its native path-record budget")
                        offset += len(segment)
                # Only native geometry is copied. Existing style columns,
                # uniforms, family members and updater identities remain live.
                # All user evaluation and native smoothing already succeeded.
                if not np.isfinite(candidate.get_points()).all():
                    raise ValueError("native graph construction produced nonfinite points")
                # set_points uses the existing Python/native resize protocol:
                # when the whole domain vanishes it saves the current style
                # defaults, then reseeds those defaults when samples return.
                # Raw match_points would resize an empty native buffer without
                # that protocol and could bring back invisible zero-style rows.
                current.get_points()  # Bake any live placement before writing world points.
                current.set_points(candidate.get_points())
            finally:
                vars(current).pop(_BUSY, None)

        # A binding owns one updater, not the author's entire updater list.
        # Rebinding replaces that one callback and retains the original grid.
        graph.add_updater(update, call=False)
        if previous is not None:
            graph.remove_updater(previous.updater)
        vars(graph)[_BINDING] = _Binding(update, self, samples)
        graph.underlying_function = func if scalar else query
        return graph

    def unbind(self, graph):
        if not isinstance(graph, VMobject):
            raise TypeError("unbind_graph_from_func requires a VMobject graph")
        if vars(graph).get(_BUSY, False):
            raise RuntimeError("cannot unbind a graph during its update")
        binding = vars(graph).get(_BINDING)
        if binding is not None:
            if binding.axes is not self:
                raise ValueError("graph binding belongs to another coordinate system")
            graph.remove_updater(binding.updater)
            vars(graph).pop(_BINDING)
            # An unbound graph is now geometry, not a promise to keep following
            # a changing callable. Queries must inspect that frozen geometry.
            vars(graph).pop("underlying_function", None)
        return graph

    _bind_method(Coordinates, "bind_graph_to_func", bind)
    _bind_method(Coordinates, "unbind_graph_from_func", unbind)
    _install_graph_queries(native)
    g["_FMN_GRAPHING_INSTALLED"] = True


def _graph_range(values, density):
    """Validate only the sampling request; native ParametricCurve samples it."""
    values = tuple(itertools.islice(iter(values), 4))
    if len(values) not in (2, 3):
        raise ValueError("graph x_range requires two or three values")
    start, stop = (_finite(value, "graph range endpoint") for value in values[:2])
    step = _finite(values[2] if len(values) == 3 else 1., "graph range step")
    density = _finite(density, "graph sampling density")
    if start >= stop or step <= 0 or density <= 0:
        raise ValueError("graph range must increase with positive step and sampling density")
    spacing = step / density
    if not math.isfinite(spacing) or spacing <= 0:
        raise ValueError("graph sample spacing must be positive and finite")
    count = (stop - start) / spacing
    if not math.isfinite(count) or math.ceil(count) + 1 > _MAX_SAMPLES:
        raise ValueError("graph sampling request exceeds its 65536-point budget")
    return (start, stop) if len(values) == 2 else (start, stop, step)


def _install_graph_queries(native):
    g = vars(native)
    Coordinates, VMobject, np = g["CoordinateSystem"], g["VMobject"], g["_np"]
    original_graph = Coordinates.get_graph

    def get_graph(self, function, x_range=None, bind=False, **kwargs):
        if not callable(function):
            raise TypeError("graph function must be callable")
        if not isinstance(bind, bool):
            raise TypeError("graph bind must be bool")
        # Avoid NumPy's ambiguous truth test in the original x_range-or-default
        # expression, and stop invalid/huge requests before authored effects.
        domain = _graph_range(self.x_range if x_range is None else x_range,
                              self.num_sampled_graph_points_per_tick)
        options = dict(kwargs)
        if "discontinuities" in options:
            values = options["discontinuities"]
            options["discontinuities"] = tuple(_discontinuities(
                () if values is None else values, domain[0], domain[1]))
        if "epsilon" in options and _finite(options["epsilon"], "graph epsilon") <= 0:
            raise ValueError("graph epsilon must be positive")
        # Keep native sampling, style handling and constructor identity. Do not
        # vectorize scalar callbacks by trial, swallow exceptions, or probe a
        # function twice to guess whether its author supports arrays.
        graph = original_graph(self, function, x_range=domain, bind=False, **options)
        if bind:
            token = _SCALAR_BIND.set((graph, function))
            try:
                # Preserve public override dispatch and the exact callable the
                # author supplied. The context applies only to this graph.
                self.bind_graph_to_func(graph, function)
            finally:
                _SCALAR_BIND.reset(token)
        return graph

    def point(value):
        result = np.asarray(value)
        if result.dtype.kind not in "biuf" or result.shape != (3,) or not np.isfinite(result).all():
            raise ValueError("graph point must have three finite real coordinates")
        return result.astype(float, copy=True)

    def input_to_graph_point(self, x, graph):
        x = _finite(x, "graph query x")
        function = getattr(graph, "underlying_function", None)
        if function is not None:
            if not callable(function):
                raise TypeError("graph underlying_function must be callable")
            # Analytic graphs keep their original public callable semantics;
            # direct vectorized bindings supply an explicit scalar query view.
            return point(self.c2p(x, function(x)))
        if not isinstance(graph, VMobject):
            raise TypeError("geometric graph lookup requires a VMobject")
        n_points = graph.get_num_points()
        if n_points == 0:
            return None
        if n_points > _MAX_RECORDS or n_points % 2 != 1:
            raise ValueError("geometric graph lookup requires a bounded shared-anchor path")

        def coordinate(p):
            coordinates = np.asarray(self.p2c(p.copy()))
            if (coordinates.ndim != 1 or not len(coordinates)
                    or coordinates.dtype.kind not in "biuf" or not np.isfinite(coordinates).all()):
                raise ValueError("graph inverse coordinates must be a finite real vector")
            return float(coordinates[0])

        if n_points == 1:
            only = point(graph.get_points()[0])
            return only if coordinate(only) == x else None
        ends = np.asarray(graph.get_subpath_end_indices())
        if (ends.ndim != 1 or not len(ends) or ends.dtype.kind not in "iu"
                or ends[-1] != n_points - 1 or np.any(ends % 2)
                or np.any(ends < 0) or np.any(ends >= n_points)
                or np.any(np.diff(ends.astype(np.int64)) <= 0)):
            raise ValueError("graph subpath boundaries do not describe its shared-anchor path")
        # A null curve is a topological break, not a drawable chord. Searching
        # it would return invented points across a pole or missing interval.
        breaks = {int(end) // 2 for end in ends[:-1]}
        for index in range(n_points // 2):
            if index in breaks:
                continue
            curve = graph.get_nth_curve_function(index)
            left_point, right_point = point(curve(0.)), point(curve(1.))
            left_x, right_x = coordinate(left_point), coordinate(right_point)
            if left_x == x:
                return left_point
            if right_x == x:
                return right_point
            if not min(left_x, right_x) < x < max(left_x, right_x):
                continue
            # Invert a continuous, x-monotone function-graph curve using its
            # public native-backed evaluator. Axis ranges are data coordinates,
            # NEVER the allowed [0,1] parameter range. Direction may decrease.
            lower, upper = 0., 1.
            increasing = left_x < right_x
            best_point = left_point if abs(left_x - x) < abs(right_x - x) else right_point
            best_error = min(abs(left_x - x), abs(right_x - x))
            tolerance = 8 * np.finfo(np.float32).eps * max(1., abs(x), abs(left_x), abs(right_x))
            for _ in range(64):
                middle = (lower + upper) * .5
                if middle == lower or middle == upper:
                    break
                middle_point = point(curve(middle))
                middle_x = coordinate(middle_point)
                error = abs(middle_x - x)
                if error < best_error:
                    best_point, best_error = middle_point, error
                if error == 0:
                    return middle_point
                if (middle_x < x) == increasing:
                    lower = middle
                else:
                    upper = middle
            if best_error <= tolerance:
                return best_point
            raise ValueError("graph curve did not resolve the requested x coordinate within f32 precision")
        return None

    _bind_method(Coordinates, "get_graph", get_graph)
    _bind_method(Coordinates, "input_to_graph_point", input_to_graph_point)
