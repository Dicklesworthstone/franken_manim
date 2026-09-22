"""Coordinate-aware calculus visuals over existing native path operations.

Python owns graph queries and coordinate dispatch. Chisel and Marionette own
curve construction, clipping, record buffers, placement and rendered geometry.
"""
from __future__ import annotations

import itertools
import math

from fmn_python.graphing import _MAX_SAMPLES, _bind_method, _finite


def _riemann_grid(values, default_step):
    """Bound the request before evaluating authored functions or allocating paths."""
    values = tuple(itertools.islice(iter(values), 4))
    if len(values) not in (2, 3):
        raise ValueError("Riemann x_range requires two or three values")
    start, stop = (_finite(value, "Riemann range endpoint") for value in values[:2])
    step = _finite(values[2] if len(values) == 3 else default_step, "Riemann dx")
    if stop < start or step <= 0:
        raise ValueError("Riemann range must not decrease and dx must be positive")
    if start == stop:
        return ()
    ratio = (stop - start) / step
    if not math.isfinite(ratio) or ratio > _MAX_SAMPLES:
        raise ValueError("Riemann rectangles exceed their 65536-item budget")
    # An exact number of steps may round one ulp above an integer. Do not add
    # an almost-zero last rectangle, but always retain the authored endpoint.
    count = max(1, math.ceil(math.nextafter(ratio, -math.inf)))
    boundaries = [start + index * step for index in range(count)] + [stop]
    if any(right <= left for left, right in zip(boundaries, boundaries[1:])):
        raise ValueError("Riemann dx is too small to advance the range")
    return tuple(zip(boundaries, boundaries[1:]))


def install_graph_calculus(native):
    g = vars(native)
    if g.get("_FMN_GRAPH_CALCULUS_INSTALLED"):
        return
    Coordinates, np = g["CoordinateSystem"], g["_np"]

    def point(value):
        result = np.asarray(value)
        if (result.shape != (3,) or result.dtype.kind not in "biuf"
                or not np.isfinite(result).all()
                or np.any(np.abs(result) > np.finfo(np.float32).max)):
            raise ValueError("calculus geometry requires three finite float32-representable coordinates")
        return result.astype(float, copy=True)

    def get_riemann_rectangles(
        self, graph, x_range=None, dx=None, input_sample_type="left",
        stroke_width=1, stroke_color=None, fill_opacity=1, colors=None,
        negative_color=None, stroke_background=True, show_signed_area=True,
    ):
        if input_sample_type not in ("left", "right", "center"):
            raise ValueError("input_sample_type must be 'left', 'right', or 'center'")
        step = self.x_range[2] if dx is None else dx
        grid = _riemann_grid(self.x_range[:2] if x_range is None else x_range, step)
        width = _finite(stroke_width, "Riemann stroke_width")
        opacity = _finite(fill_opacity, "Riemann fill_opacity")
        if not 0 <= width <= np.finfo(np.float32).max or not 0 <= opacity <= 1:
            raise ValueError("Riemann stroke_width must be nonnegative and fill_opacity in [0,1]")
        palette = tuple(itertools.islice(
            iter((g["BLUE_D"], g["BLUE_B"]) if colors is None else colors),
            _MAX_SAMPLES + 1))
        if not palette or len(palette) > _MAX_SAMPLES:
            raise ValueError("Riemann colors must be a nonempty bounded iterable")
        stroke_color = g["BLACK"] if stroke_color is None else stroke_color
        negative_color = g["RED"] if negative_color is None else negative_color
        # Validate colors before any graph callback; keep the actual palette
        # for the existing linear-light native style/gradient owner.
        for color in (*palette, stroke_color, negative_color):
            rgba = np.asarray(g["color_to_rgb"](color))
            if not np.isfinite(rgba).all():
                raise ValueError("Riemann colors must be finite")

        rectangles = []
        for left, right in grid:
            sample = (left if input_sample_type == "left" else right
                      if input_sample_type == "right" else left + (right - left) / 2)
            queried = self.i2gp(sample, graph)
            if queried is None:
                raise ValueError("Riemann sample lies outside the graph or in a discontinuity")
            coordinates = np.asarray(self.p2c(point(queried)))
            if (coordinates.ndim != 1 or len(coordinates) < 2
                    or coordinates.dtype.kind not in "biuf" or not np.isfinite(coordinates).all()):
                raise ValueError("Riemann graph inverse coordinates must contain finite x and y")
            # Exact baseline queries stay zero even when the inverse of a
            # rotated chart has a small floating-point residual at y=0.
            height = (0. if np.array_equal(queried, self.c2p(sample, 0))
                      else float(coordinates[1]))
            corners = [point(self.c2p(x, y)) for x, y in (
                (left, 0), (right, 0), (right, height), (left, height), (left, 0))]
            # Preserve Rectangle identity, but build its path in the axes'
            # chart, not a screen-aligned bounding box. Chisel still owns
            # corner conversion, records, placement and rasterization.
            rectangle = g["Rectangle"]()
            rectangle.set_points_as_corners(corners)
            rectangle.positive = height >= 0
            rectangles.append(rectangle)
        result = g["VGroup"](*rectangles)
        result.set_submobject_colors_by_gradient(*palette)
        result.set_style(stroke_width=width, stroke_color=stroke_color,
                         fill_opacity=opacity, stroke_behind=stroke_background)
        if show_signed_area:
            for rectangle in rectangles:
                if not rectangle.positive:
                    rectangle.set_fill(negative_color)
        return result

    _bind_method(Coordinates, "get_riemann_rectangles", get_riemann_rectangles)
    g["_FMN_GRAPH_CALCULUS_INSTALLED"] = True

