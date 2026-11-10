"""Streamline callback admission and live paint over the native RK45 builder.

Atlas integrates and smooths the paths; Chisel measures their true arc lengths;
Atlas's bundled gradient or an authored color map supplies RGB values. This
adapter only dispatches Python fields and color callbacks, then publishes
validated style columns through Marionette's live records.
"""
from __future__ import annotations

from .invocation import InvocationGuard
from functools import wraps
import inspect
import math
import operator

_MAX_POINTS = 65_536
_LINE_EDITS = InvocationGuard()


def _finite(value, name, *, nonnegative=False):
    result = float(value)
    if not math.isfinite(result) or (nonnegative and result < 0):
        raise ValueError(name + " must be finite" + (" and nonnegative" if nonnegative else ""))
    return result


def _magnitude_gradient(np, native_gradient, min_value, max_value, color_map):
    """Map bounded magnitudes to owned RGB rows without a host palette library.

    The named bundled map retains Atlas's interpolation. An authored callable
    receives clipped normalized magnitudes and returns RGB or RGBA rows, like
    VectorField's color-map protocol. StreamLines controls opacity separately,
    as the Reference's gradient helpers do, so the fourth column is ignored.
    """
    low = _finite(min_value, "field gradient minimum")
    high = _finite(max_value, "field gradient maximum")
    if high < low:
        raise ValueError("field gradient range must be ordered")
    custom = callable(color_map)
    if not custom and not (isinstance(color_map, str) and color_map == "3b1b_colormap"):
        raise NotImplementedError(
            "field gradients support the bundled '3b1b_colormap' or a callable "
            "color map; other names require non-bundled matplotlib data"
        )

    def gradient(values):
        array = np.asarray(values)
        if array.size > _MAX_POINTS:
            raise ValueError("field gradient exceeds the 65536-value resource budget")
        if array.dtype.kind not in "biuf":
            raise TypeError("field gradient values must be real numeric magnitudes")
        flat = np.array(array, dtype=float, copy=True).reshape(-1)
        if not np.isfinite(flat).all():
            raise ValueError("field gradient values must be finite")
        if not len(flat):
            return np.empty((0, 3))
        if custom:
            if high == low:
                alphas = np.zeros(len(flat))
            else:
                # A finite range can span opposite near-limit floats. Halve
                # before subtracting in that case to keep normalization finite.
                span = high - low
                with np.errstate(over="ignore", invalid="ignore"):
                    alphas = ((flat - low) / span if math.isfinite(span) else
                              (flat * .5 - low * .5) / (high * .5 - low * .5))
                alphas = np.clip(alphas, 0., 1.)
            colors = np.asarray(color_map(alphas))
            if (colors.ndim != 2 or colors.shape[0] != len(flat)
                    or colors.shape[1] not in (3, 4)):
                raise ValueError("field color map must return one RGB or RGBA row per magnitude")
            if colors.dtype.kind not in "biuf":
                raise TypeError("field color map must return real numeric RGB or RGBA rows")
            colors = colors[:, :3]
        else:
            colors = native_gradient(low, high, flat.tolist())
        # A callback may reuse its scratch array on the next line. Freeze its
        # RGB now so a later call cannot rewrite an earlier line's paint plan.
        rgb = np.array(colors, dtype=float, copy=True).reshape((len(flat), 3))
        if not np.isfinite(rgb).all() or np.any(np.abs(rgb) > np.finfo(np.float32).max):
            raise ValueError("field gradient colors must be finite and f32-representable")
        return rgb

    return gradient


def _rows(np, values, name, *, count=None):
    array = np.asarray(values)
    if (array.ndim != 2 or not 1 <= array.shape[1] <= 3
            or len(array) > _MAX_POINTS or (count is not None and len(array) != count)):
        raise ValueError(name + " must contain one 1..3-component vector per sample (at most 65536)")
    if array.dtype.kind not in "biuf":
        raise TypeError(name + " must contain real numeric coordinates")
    array = np.array(array, dtype=float, copy=True)
    if not np.isfinite(array).all():
        raise ValueError(name + " must be finite")
    return array


def _dimension(coordinates):
    dimension = operator.index(getattr(coordinates, "dimension", 2))
    if not 1 <= dimension <= 3:
        raise ValueError("StreamLines coordinate dimension must be 1, 2 or 3")
    return dimension


def _field(np, function, coordinates, dimension):
    # Native integration uses padded Vec3 rows; authored 2D fields must see
    # their actual two coordinates, not an undocumented third dimension.
    rows = _rows(np, coordinates, "StreamLines field inputs")
    result = _rows(np, function(rows[:, :dimension].copy()),
                   "StreamLines field output", count=len(rows))
    padded = np.zeros((len(result), 3))
    padded[:, :result.shape[1]] = result
    return padded


def _editing(lines):
    return _LINE_EDITS.hold(lines, message="StreamLines authoring cannot reenter itself")


def _widths(g, points, width, taper):
    np = g["_np"]
    if not taper or not len(points):
        return np.full(len(points), width)
    # Measure each actual quadratic with Chisel, never chord lengths or point
    # indices. Handles sit at their curve's half-length, as in Atlas's
    # taper_by_true_length. No quadrature/curve kernel is duplicated in Python.
    probe = g["VMobject"]()
    lengths = []
    for offset in range(0, len(points) - 2, 2):
        probe.set_points(points[offset:offset + 3])
        lengths.append(probe.get_arc_length())
    total = sum(lengths)
    if not math.isfinite(total):
        raise ValueError("StreamLines path length must be finite")
    if total <= 0:
        return np.zeros(len(points))
    stations, distance = [0.0], 0.0
    for length in lengths:
        stations.append((distance + .5 * length) / total)
        distance += length
        stations.append(distance / total)
    stations.extend([1.0] * (len(points) - len(stations)))
    return width * (1.0 - np.abs(2.0 * np.clip(stations, 0.0, 1.0) - 1.0))


def _style_plan(g, owner):
    np, coordinates = g["_np"], owner.coordinate_system
    dimension = _dimension(coordinates)
    width = _finite(owner.stroke_width, "StreamLines stroke_width", nonnegative=True)
    opacity = _finite(owner.stroke_opacity, "StreamLines stroke_opacity", nonnegative=True)
    if opacity > 1:
        raise ValueError("StreamLines stroke_opacity must be at most 1")
    low, high = (_finite(value, "StreamLines magnitude_range") for value in owner.magnitude_range)
    if high < low:
        raise ValueError("StreamLines magnitude_range must be ordered")
    color_map = owner.color_map
    gradient = _magnitude_gradient(
        np, g["_BridgeMobject"]._vector_field_gradient, low, high,
        "3b1b_colormap" if color_map is None else color_map,
    )
    function, by_magnitude = owner.func, bool(owner.color_by_magnitude)
    taper = bool(owner.taper_stroke_width)
    members, plan, total = tuple(owner.submobjects), [], 0
    for line in members:
        if not isinstance(line, g["VMobject"]):
            raise TypeError("StreamLines children must be VMobjects")
        points = np.array(line.get_points(), copy=True)
        total += len(points)
        if total > _MAX_POINTS:
            raise ValueError("StreamLines restyling exceeds the 65536-point resource budget")
        if not np.isfinite(points).all():
            raise ValueError("StreamLines path points must be finite")
        if by_magnitude and len(points):
            # Use the public scalar p2c hook: authored/rotated axes need not
            # implement the Reference's optional vectorized conversion.
            coords = _rows(np, [coordinates.p2c(point.copy()) for point in points],
                           "StreamLines coordinate conversion", count=len(points))
            outputs = _field(np, function, coords, dimension)
            with np.errstate(over="ignore", invalid="ignore"):
                norms = [g["get_norm"](row) for row in outputs]
            if not all(math.isfinite(value) for value in norms):
                raise ValueError("StreamLines field magnitudes must be finite")
            rgb = gradient(norms)
        else:
            color = owner.stroke_color
            rgb = np.tile(g["color_to_rgb"](g["WHITE"] if color is None else color), (len(points), 1))
        rgba = np.empty((len(points), 4))
        rgba[:, :3], rgba[:, 3] = np.asarray(rgb).reshape((-1, 3)), opacity
        widths = _widths(g, points, width, taper)
        if (not np.isfinite(rgba).all() or not np.isfinite(widths).all()
                or np.any(np.abs(rgba) > np.finfo(np.float32).max)
                or np.any(np.abs(widths) > np.finfo(np.float32).max)):
            raise ValueError("StreamLines paint must be finite and f32-representable")
        plan.append((line, points, rgba, widths))
    # A callback may inspect scene state, but changing the family/geometry
    # invalidates this entire plan. Never partially apply an obsolete paint plan.
    if tuple(owner.submobjects) != members:
        raise RuntimeError("StreamLines family changed while evaluating its style")
    for line, points, _, _ in plan:
        if not np.array_equal(line.get_points(), points):
            raise RuntimeError("StreamLines geometry changed while evaluating its style")
    return plan


def install_streamline_authoring(native):
    g = vars(native)
    if g.get("_FMN_STREAMLINE_AUTHORING_INSTALLED", False):
        return
    Lines, np = g["StreamLines"], g["_np"]
    g["_rebind_default"](Lines, "stroke_color", g["DEFAULT_MOBJECT_COLOR"])
    previous = Lines.__init__
    signature = inspect.signature(previous)

    @wraps(previous)
    def initialize(self, *args, **kwargs):
        bound = signature.bind(self, *args, **kwargs)
        bound.apply_defaults()
        controls = dict(bound.arguments)
        controls.pop("self")
        extras = controls.pop("kwargs")
        function, coordinates = controls["func"], controls["coordinate_system"]
        if not callable(function):
            raise TypeError(
                "StreamLines func must be a callable vector field; got "
                + type(function).__name__
            )
        if not (
            hasattr(coordinates, "c2p")
            and hasattr(coordinates, "p2c")
            and hasattr(coordinates, "get_all_ranges")
        ):
            raise TypeError(
                "StreamLines requires a coordinate system with "
                "c2p/p2c/get_all_ranges; got "
                + type(coordinates).__name__
            )
        dimension = _dimension(coordinates)
        for name in ("n_repeats", "max_time_steps", "n_samples_per_line"):
            controls[name] = operator.index(controls[name])
        # Preserve the public callable identity. The temporary bridge closure
        # exists only during native integration and never replaces self.func.
        def field(rows):
            return _field(np, function, rows, dimension)
        native_controls = dict(controls, func=field)
        previous(self, **native_controls, **extras)
        for name, value in controls.items():
            setattr(self, name, value)
        for line, virtual_time in zip(self, self._stream_virtual_times):
            line.virtual_time = virtual_time
        self.init_style()
        # Preserve constructor shorthand/paint precedence after derived colors.
        g["_apply_vmobject_style_kwargs"](self, dict(extras))

    def init_style(self):
        with _editing(self):
            plan = _style_plan(g, self)
            for line, _, rgba, widths in plan:
                line.data["stroke_rgba"][:] = rgba
                line.data["stroke_width"][:, 0] = widths

    def point_func(self, points):
        array = np.asarray(points)
        single = array.shape == (3,)
        rows = _rows(np, array.reshape((1, 3)) if single else array, "StreamLines points")
        if rows.shape[1] != 3:
            raise ValueError("StreamLines scene points must have three components")
        coordinates = self.coordinate_system
        dimension = _dimension(coordinates)
        if not len(rows):
            return np.empty((0, 3))
        coords = _rows(np, [coordinates.p2c(point.copy()) for point in rows],
                       "StreamLines coordinate conversion", count=len(rows))
        outputs = _field(np, self.func, coords, dimension)
        origin = np.asarray(coordinates.get_origin(), dtype=float).reshape(3)
        vectors = _rows(np, [coordinates.c2p(*row[:dimension]) - origin for row in outputs],
                        "StreamLines scene vectors", count=len(rows))
        if vectors.shape[1] != 3:
            raise ValueError("StreamLines scene vectors must have three components")
        return vectors[0] if single else vectors

    for name, method in (("__init__", initialize), ("init_style", init_style), ("point_func", point_func)):
        method.__name__ = name
        method.__qualname__ = Lines.__qualname__ + "." + name
        method.__module__ = Lines.__module__
        setattr(Lines, name, method)
    g["_FMN_STREAMLINE_AUTHORING_INSTALLED"] = True
