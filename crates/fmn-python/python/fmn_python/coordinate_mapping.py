"""Invert the shipped affine axes chart using its live native line geometry.

Independent axis projections are only an inverse for orthogonal axes. The
scaled dual basis handles shears, reflections and 2D charts embedded in 3D.
This is the host's existing coordinate adapter, not a geometry/renderer owner.
"""
from __future__ import annotations

import math
import sys
import itertools
import inspect


_MAX_LABELS = 4096
_MAX_LABEL_RECORDS = 1_048_576


def _label_values(values, name, convert=float):
    result = []
    for value in itertools.islice(iter(values), _MAX_LABELS + 1):
        if len(result) == _MAX_LABELS:
            raise ValueError(name + " exceeds the 4096-label budget")
        value = convert(value)
        if not (math.isfinite(value.real) and math.isfinite(value.imag)):
            raise ValueError(name + " must contain finite numbers")
        result.append(value)
    return result


def _label_ticks(np, terms, include_tip):
    low, high, step = (float(v) for v in terms)
    if not all(math.isfinite(v) for v in (low, high, step)) or step <= 0 or high < low:
        raise ValueError("coordinate label range must be finite, ordered and have a positive step")
    stop = high if include_tip else high + step
    count = (stop - low) / step
    if not math.isfinite(count) or count > _MAX_LABELS:
        raise ValueError("coordinate label range exceeds the 4096-label budget")
    return [float(v) for v in np.arange(low, stop, step) if v <= high]


def install_coordinate_labels(native):
    """Compose Scribe numbers at live coordinates, never reconstructed boxes.

    Preparation is separate from family publication. No glyph layout, axis
    geometry, record generation or scene clock is implemented in this adapter.
    """
    g = vars(native)
    if g.get("_FMN_COORDINATE_LABELS_INSTALLED", False):
        return
    Line, Group, np = (g[name] for name in ("NumberLine", "VGroup", "_np"))
    stock_label, stock_ticks = Line.get_number_mobject, Line.get_tick_range
    active = set()

    def geometry_state(axis):
        return (axis.get_points().copy(), tuple(axis.submobjects),
                vars(axis).get("_scene"), axis._is_bound())

    def check_geometry(axis, before):
        points, children, owner, bound = before
        if (tuple(axis.submobjects) != children or vars(axis).get("_scene") is not owner
                or axis._is_bound() != bound or not np.array_equal(axis.get_points(), points)):
            raise RuntimeError("coordinate geometry or ownership changed while preparing labels")

    def options(kwargs):
        out = dict(kwargs)
        for name in ("font_size", "buff", "unit"):
            if name in out and out[name] is not None:
                value = float(out[name])
                if not math.isfinite(value) or (name == "font_size" and value <= 0) or (name == "unit" and value == 0):
                    raise ValueError("coordinate label " + name + " must be finite and valid")
                out[name] = value
        if out.get("direction") is not None:
            direction = np.asarray(out["direction"], dtype=float)
            if direction.shape != (3,) or not np.isfinite(direction).all():
                raise ValueError("coordinate label direction must be a finite three-vector")
            out["direction"] = direction.copy()
        return out

    def stock_options(kwargs):
        # Keep the existing precise refusal for genuinely unknown keywords,
        # while routing the actual DecimalNumber and unit-label surface.
        allowed = set(inspect.signature(g["DecimalNumber"].__init__).parameters)
        allowed.update(("direction", "buff", "unit", "unit_tex", "color", "opacity",
                        "fill_color", "fill_opacity", "stroke_color", "stroke_width", "stroke_opacity"))
        g["_refuse_unrouted"]("NumberLine.add_numbers()",
                              [(name, True) for name in sorted(kwargs) if name not in allowed])

    def planned_labels(formatter, values, kwargs):
        labels, records, seen = [], 0, set()
        for value in values:
            label = formatter(value, **kwargs)
            if not isinstance(label, g["VMobject"]):
                raise TypeError("coordinate label formatter must return a VMobject")
            for member in label.get_family():
                if id(member) in seen:
                    continue
                seen.add(id(member))
                records += member.get_num_points()
                if records > _MAX_LABEL_RECORDS:
                    raise ValueError("coordinate labels exceed their native record budget")
                points = member.get_points()
                if not np.isfinite(points).all():
                    raise ValueError("coordinate label geometry must be finite")
            labels.append(label)
        return Group(*labels)

    def add_numbers(self, x_values=None, excluding=None, font_size=24, **kwargs):
        if id(self) in active:
            raise RuntimeError("coordinate labeling cannot reenter itself")
        active.add(id(self))
        try:
            before = geometry_state(self)
            parameters = (self.x_min, self.x_max, self.x_step)
            formatter = self.get_number_mobject
            if getattr(formatter, "__func__", None) is stock_label:
                stock_options(kwargs)
            kwargs = options(dict(kwargs, font_size=font_size))
            if x_values is None:
                x_values = (_label_ticks(np, parameters, self.include_tip)
                            if getattr(self.get_tick_range, "__func__", None) is stock_ticks
                            else self.get_tick_range())
            values = _label_values(x_values, "number-line labels")
            excluded = self.numbers_to_exclude if excluding is None else excluding
            excluded = set(() if excluded is None else _label_values(excluded, "excluded labels"))
            group = planned_labels(formatter, [v for v in values if v not in excluded], kwargs)
            check_geometry(self, before)
            if (self.x_min, self.x_max, self.x_step) != parameters:
                raise RuntimeError("number-line range changed while preparing labels")
            self.add(group)
            self.numbers = group
            return group
        finally:
            active.remove(id(self))

    add_numbers.__module__ = Line.__module__
    add_numbers.__qualname__ = Line.__qualname__ + ".add_numbers"
    Line.add_numbers = add_numbers
    g["_FMN_COORDINATE_LABELS_INSTALLED"] = True


def _dot(left, right):
    return left[0] * right[0] + left[1] * right[1] + left[2] * right[2]


def _cross(left, right):
    return (left[1] * right[2] - left[2] * right[1],
            left[2] * right[0] - left[0] * right[2],
            left[0] * right[1] - left[1] * right[0])


def _unit(vector):
    length = math.hypot(*vector)
    if not math.isfinite(length) or length == 0:
        raise ValueError("axes coordinate mapping requires finite nonzero axis directions")
    return tuple(value / length for value in vector), length


def _inverse(basis, offset):
    """Fixed-order dual-basis solve, with no BLAS or unscaled Gram matrix."""
    normalized = [_unit(column) for column in basis]
    vectors = [item[0] for item in normalized]
    if len(vectors) == 2:
        # The third direction makes the inverse an orthogonal projection onto
        # the actual chart plane, rather than a projection onto screen x/y.
        vectors.append(_unit(_cross(*vectors))[0])
    a, b, c = vectors
    duals = (_cross(b, c), _cross(c, a), _cross(a, b))
    determinant = _dot(a, duals[0])
    if not math.isfinite(determinant) or abs(determinant) <= 64 * sys.float_info.epsilon:
        raise ValueError("axes coordinate mapping is singular or numerically degenerate")
    result = tuple((_dot(offset, dual) / determinant) / scale
                   for dual, (_, scale) in zip(duals, normalized))
    return result


def install_coordinate_mapping(native):
    g = vars(native)
    if g.get("_FMN_COORDINATE_MAPPING_INSTALLED", False):
        return
    Axes, np = g["Axes"], g["_np"]
    original = Axes.point_to_coords
    forward, abbreviated = Axes.coords_to_point, Axes.c2p

    def vector(value, name):
        array = np.asarray(value)
        if array.shape != (3,) or np.iscomplexobj(array):
            raise ValueError(name + " must contain exactly three real coordinates")
        values = tuple(float(v) for v in array)
        if not all(math.isfinite(v) for v in values):
            raise ValueError(name + " must contain finite coordinates")
        return values

    def point_to_coords(self, point):
        # Do not fit an affine model to an authored nonlinear chart. Explicit
        # inverse overrides keep normal Python dispatch; an authored forward
        # override inheriting this method keeps its previous inverse behavior.
        if (getattr(self.coords_to_point, "__func__", None) is not forward
                or getattr(self.c2p, "__func__", None) is not abbreviated):
            return original(self, point)
        target = np.asarray(point)
        if target.ndim == 0 or target.shape[-1] != 3 or np.iscomplexobj(target):
            raise ValueError("axes points must have a final dimension of three real coordinates")
        target = target.astype(float, copy=False)
        if not np.isfinite(target).all():
            raise ValueError("axes points must contain finite coordinates")
        axes, ranges = tuple(self.axes), tuple(self.get_all_ranges())
        if len(axes) not in (2, 3) or len(ranges) != len(axes):
            raise ValueError("affine coordinate mapping requires two or three axes and matching ranges")
        basis, origins = [], []
        for axis, domain in zip(axes, ranges):
            low, high = float(domain[0]), float(domain[1])
            span = high - low
            if not math.isfinite(low) or not math.isfinite(high) or not math.isfinite(span) or span == 0:
                raise ValueError("axes coordinate ranges must have distinct finite bounds")
            start, end = vector(axis._get_start(), "axis start"), vector(axis._get_end(), "axis end")
            column = tuple((b - a) / span for a, b in zip(start, end))
            alpha = -low / span
            origin = tuple((1 - alpha) * a + alpha * b for a, b in zip(start, end))
            basis.append(column)
            origins.append(origin)
        # Match coords_to_point's full-coordinate origin even when individual
        # axes were moved independently. Do not assume they still intersect.
        origin = tuple(origins[0][d] + sum(row[d] - origins[0][d] for row in origins)
                       for d in range(3))
        offset = tuple(target[..., d] - origin[d] for d in range(3))
        if not all(np.isfinite(v).all() for v in offset):
            raise ValueError("axes point displacement must be finite")
        result = _inverse(basis, offset)
        if not all(np.isfinite(v).all() for v in result):
            raise ValueError("axes inverse coordinates are not finite")
        return tuple(float(v) for v in result) if target.ndim == 1 else result

    point_to_coords.__name__ = "point_to_coords"
    point_to_coords.__qualname__ = Axes.__qualname__ + ".point_to_coords"
    point_to_coords.__module__ = Axes.__module__
    Axes.point_to_coords = point_to_coords
    g["_FMN_COORDINATE_MAPPING_INSTALLED"] = True
