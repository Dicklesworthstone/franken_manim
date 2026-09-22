"""Invert the shipped affine axes chart using its live native line geometry.

Independent axis projections are only an inverse for orthogonal axes. The
scaled dual basis handles shears, reflections and 2D charts embedded in 3D.
This is the host's existing coordinate adapter, not a geometry/renderer owner.
"""
from __future__ import annotations

import math
import sys


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
