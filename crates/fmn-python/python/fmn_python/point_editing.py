"""Validate authored point tables before changing Marionette record generations.

Native RecordBuffer resize, view lifetimes, paint lanes and path metadata remain
owned by the existing Mobject/VMobject protocol. Only input admission lives here:
empty Rust vector returns are valid empty point tables, not malformed (0,) rows.
"""
from __future__ import annotations


def _point_table(np, points):
    values = np.asarray(points)
    if values.ndim == 1 and values.shape == (0,):
        values = values.reshape((0, 3))
    if values.ndim != 2 or values.shape[1] != 3:
        raise ValueError("points must be an (N, 3) array of real coordinates")
    if values.dtype.kind == "c":
        raise TypeError("points must be real coordinates")
    # Own the input before resizing or invoking authored resize hooks. A view
    # can alias the live destination, including a reversed/strided field view.
    values = np.array(values, dtype=np.float64, copy=True)
    if not np.isfinite(values).all():
        raise ValueError("points must be finite")
    if np.any(np.abs(values) > np.finfo(np.float32).max):
        raise ValueError("points must be f32-representable")
    return values


def install_point_editing(native):
    """Keep native classes and VMobject's odd-count/path-metadata dispatch."""
    g = vars(native)
    if g.get("_FMN_POINT_EDITING_INSTALLED", False):
        return
    Mobject, np = g["Mobject"], g["_np"]

    def set_points(self, points):
        values = _point_table(np, points)
        self.resize_points(len(values), resize_func=g["resize_preserving_order"])
        self.data["point"][:] = values
        return self

    def append_points(self, new_points):
        values = _point_table(np, new_points)
        if not len(values):
            return self
        count = self.get_num_points()
        self.resize_points(count + len(values))
        data = self.data
        # Appended records inherit the last existing record's non-point lanes.
        # An empty source instead uses resize_points' retained style defaults.
        if count:
            data[count:] = data[count - 1]
        data["point"][count:] = values
        return self

    for name, method in (("set_points", set_points), ("append_points", append_points)):
        method.__name__ = name
        method.__qualname__ = Mobject.__qualname__ + "." + name
        method.__module__ = Mobject.__module__
        setattr(Mobject, name, method)
    g["_FMN_POINT_EDITING_INSTALLED"] = True
