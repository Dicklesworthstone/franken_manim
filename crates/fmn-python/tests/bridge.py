"""Permanent W10 acceptance tests executed inside fmn-python's Rust test.

The file is Python rather than Rust because the contract under test is ordinary
Python behavior: MRO, live NumPy arrays, weakrefs, descriptors, copy/pickle,
module imports, and callback reentrancy.
"""

import abc
import contextlib
import copy
import enum
import gc
import hmac
import importlib
import inspect
import io
import json
import math
import pickle
import pathlib
import re
import struct
import sys
import tempfile
import threading
import types
import weakref
import zlib

import numpy as np

import manimlib
from manimlib import Animation, InteractiveScene, Mobject, Scene, VMobject

bridge_errors = importlib.import_module("manimlib.exceptions")


def png_rgba8_rows(path):
    """Decode the portal's non-interlaced RGBA8 PNGs with stdlib only."""
    payload = pathlib.Path(path).read_bytes()
    assert payload.startswith(b"\x89PNG\r\n\x1a\n")
    offset = 8
    width = height = None
    compressed = bytearray()
    while offset < len(payload):
        length = struct.unpack(">I", payload[offset : offset + 4])[0]
        kind = payload[offset + 4 : offset + 8]
        data = payload[offset + 8 : offset + 8 + length]
        offset += 12 + length
        if kind == b"IHDR":
            width, height, depth, color, compression, filtering, interlace = (
                struct.unpack(">IIBBBBB", data)
            )
            assert (depth, color, compression, filtering, interlace) == (8, 6, 0, 0, 0)
        elif kind == b"IDAT":
            compressed.extend(data)
        elif kind == b"IEND":
            break
    assert width is not None and height is not None
    raw = zlib.decompress(compressed)
    stride = width * 4
    assert len(raw) == height * (stride + 1)
    rows = []
    previous = bytearray(stride)

    def paeth(left, above, upper_left):
        prediction = left + above - upper_left
        distances = (
            abs(prediction - left),
            abs(prediction - above),
            abs(prediction - upper_left),
        )
        return (left, above, upper_left)[distances.index(min(distances))]

    for y in range(height):
        start = y * (stride + 1)
        filter_kind = raw[start]
        encoded = raw[start + 1 : start + 1 + stride]
        row = bytearray(stride)
        for index, value in enumerate(encoded):
            left = row[index - 4] if index >= 4 else 0
            above = previous[index]
            upper_left = previous[index - 4] if index >= 4 else 0
            predictor = {
                0: 0,
                1: left,
                2: above,
                3: (left + above) // 2,
                4: paeth(left, above, upper_left),
            }[filter_kind]
            row[index] = (value + predictor) & 0xFF
        rows.append(row)
        previous = row
    return width, height, rows


def assert_red_orientation_witness_is_above_origin(path):
    width, height, rows = png_rgba8_rows(path)
    red_pixels = [
        (x, y)
        for y, row in enumerate(rows)
        for x in range(width)
        if row[4 * x] > 160
        and row[4 * x + 1] < 100
        and row[4 * x + 2] < 100
        and row[4 * x + 3] > 160
    ]
    assert len(red_pixels) >= 4, f"missing red orientation witness in {path}"
    centroid_y = sum(y for _x, y in red_pixels) / len(red_pixels)
    assert centroid_y < height / 2, (path, centroid_y, height)


def assert_white_stroke_witness(path, minimum_pixels=8):
    _width, _height, rows = png_rgba8_rows(path)
    white_pixels = sum(
        1
        for row in rows
        for offset in range(0, len(row), 4)
        if min(row[offset : offset + 3]) >= 180 and row[offset + 3] > 160
    )
    assert white_pixels >= minimum_pixels, (path, white_pixels, minimum_pixels)

# Schema constants use a deliberately closed AST grammar: ordinary literal
# and arithmetic expressions resolve, while executable/private forms refuse.
assert manimlib._constant_expression("1 + 2 * 3", {}) == 7
assert manimlib._constant_expression("value[1]", {"value": (4, 9)}) == 9
for forbidden_constant in ("__import__('os')", "(1).__class__", "lambda: 1"):
    try:
        manimlib._constant_expression(forbidden_constant, {})
    except (AttributeError, ValueError):
        pass
    else:
        raise AssertionError(f"executable schema constant accepted: {forbidden_constant}")


class TracingMobject(Mobject):
    def __init__(self):
        self.calls = []
        super().__init__()

    def init_data(self):
        self.calls.append("init_data")

    def init_points(self):
        self.calls.append("init_points")
        self.resize(1)
        self.set_field("point", 0, [1.0, 2.0, 3.0])

    def init_uniforms(self):
        self.calls.append("init_uniforms")
        super().init_uniforms()
        self.uniforms["glow"] = 0.25


class PointMixin:
    def init_points(self):
        self.resize(1)
        self.set_field("point", 0, [7.0, 8.0, 9.0])
        self.mixin_ran = True


class MixedMobject(PointMixin, Mobject):
    pass


class CustomDtype(Mobject):
    data_dtype = [
        ("point", np.float32, (3,)),
        ("rgba", np.float32, (4,)),
        ("wobble", np.float32, (2,)),
    ]

    def init_points(self):
        self.resize(2)
        self.set_field("wobble", 1, [0.5, -0.5])


class CustomPointFields(Mobject):
    data_dtype = [
        ("point", np.float32, (3,)),
        ("control", np.float32, (3,)),
    ]
    pointlike_data_keys = ["point", "control"]

    def init_points(self):
        self.resize(1)
        self.set_field("point", 0, [1.0, 2.0, 0.0])
        self.set_field("control", 0, [3.0, 4.0, 0.0])


class MatrixPointMapperOverride(CustomPointFields):
    def apply_points_function(self, *args, **kwargs):
        self.matrix_point_mapper_calls += 1
        return super().apply_points_function(*args, **kwargs)


class MatrixFamilyWalkerOverride(CustomPointFields):
    def get_family(self, recurse=True):
        self.matrix_family_walker_calls += 1
        return super().get_family(recurse)


class UnsupportedDtype(Mobject):
    data_dtype = [("point", np.float64, (3,))]


# Lifecycle and ordinary MRO.
traced = TracingMobject()
assert traced.calls == ["init_data", "init_points", "init_uniforms"]
assert traced.get_field("point", 0) == [1.0, 2.0, 3.0]
assert traced.uniforms["glow"] == 0.25
mixed = MixedMobject()
assert mixed.mixin_ran
assert mixed.get_field("point", 0) == [7.0, 8.0, 9.0]
assert [base.__name__ for base in type(mixed).__mro__[:4]] == [
    "MixedMobject",
    "PointMixin",
    "Mobject",
    "_BridgeMobject",
]
try:
    UnsupportedDtype()
except TypeError as error:
    assert "native-endian float32" in str(error)
else:
    raise AssertionError("a non-f32 RecordBuffer schema was accepted")


# Subclass dtype → RecordSchema → zero-copy structured NumPy view.
custom = CustomDtype()
assert custom.field_names() == ["point", "rgba", "wobble"]
assert custom.data.dtype.names == ("point", "rgba", "wobble")
view = custom.data
assert view.shape == (2,)
assert view.dtype.itemsize == 9 * 4
view["point"][0] = [4.0, 5.0, 6.0]
assert custom.get_field("point", 0) == [4.0, 5.0, 6.0]
custom.set_field("wobble", 1, [2.5, -2.5])
assert view["wobble"][1].tolist() == [2.5, -2.5]
revision = custom.revision()
view["rgba"][0] = [0.1, 0.2, 0.3, 0.4]
del view
gc.collect()
assert custom.revision() > revision

old_view = custom.data
old_view["point"][1] = [11.0, 12.0, 13.0]
custom.resize(4)
assert old_view.shape == (2,)
assert old_view["point"][1].tolist() == [11.0, 12.0, 13.0]
old_view["point"][1] = [21.0, 22.0, 23.0]
assert custom.get_field("point", 1) == [11.0, 12.0, 13.0]
readonly = custom._data_array(False)
assert not readonly.flags.writeable
try:
    custom.resize(sys.maxsize)
except OverflowError:
    pass
else:
    raise AssertionError("an overflowing RecordBuffer resize was accepted")


# Deterministic utility functions are public semantic bindings, not schema
# placeholders. Core color interpolation and Chisel's integer/angle kernels
# remain the authorities behind the NumPy-compatible portal surface.
bezier_utils = importlib.import_module("manimlib.utils.bezier")
color_utils = importlib.import_module("manimlib.utils.color")
simple_utils = importlib.import_module("manimlib.utils.simple_functions")
space_utils = importlib.import_module("manimlib.utils.space_ops")
path_utils = importlib.import_module("manimlib.utils.paths")

utility_signatures = {
    bezier_utils.bezier: "(points)",
    bezier_utils.interpolate: "(start, end, alpha)",
    bezier_utils.inverse_interpolate: "(start, end, value)",
    bezier_utils.integer_interpolate: "(start, end, alpha)",
    color_utils.average_color: "(*colors)",
    color_utils.color_gradient: (
        "(reference_colors, length_of_output, interp_by_hsl=False)"
    ),
    color_utils.color_to_hex: "(color)",
    color_utils.color_to_int_rgb: "(color)",
    color_utils.color_to_int_rgba: "(color, opacity=1.0)",
    color_utils.color_to_rgb: "(color)",
    color_utils.color_to_rgba: "(color, alpha=1.0)",
    color_utils.hex_to_int: "(rgb_hex)",
    color_utils.hex_to_rgb: "(hex_code)",
    color_utils.int_to_hex: "(rgb_int)",
    color_utils.interpolate_color: (
        "(color1, color2, alpha, interp_by_hsl=False)"
    ),
    color_utils.interpolate_color_by_hsl: "(color1, color2, alpha)",
    color_utils.invert_color: "(color)",
    color_utils.rgb_to_color: "(rgb)",
    color_utils.rgb_to_hex: "(rgb)",
    color_utils.rgba_to_color: "(rgba)",
    simple_utils.binary_search: (
        "(function, target, lower_bound, upper_bound, tolerance=0.0001)"
    ),
    simple_utils.choose: "(n, k)",
    simple_utils.clip: "(a, min_a, max_a)",
    simple_utils.fdiv: "(a, b, zero_over_zero_value=None)",
    simple_utils.sigmoid: "(x)",
    space_utils.angle_between_vectors: "(v1, v2)",
    space_utils.angle_of_vector: "(vector)",
    space_utils.compass_directions: "(n=4, start_vect=array([1., 0., 0.]))",
    space_utils.cross: "(v1, v2, out=None)",
    space_utils.line_intersects_path: "(start, end, path)",
    path_utils.clockwise_path: "()",
    path_utils.counterclockwise_path: "()",
    path_utils.path_along_arc: "(arc_angle, axis=array([0., 0., 1.]))",
    path_utils.straight_path: "(start_points, end_points, alpha)",
}
assert len(utility_signatures) == 34
for function, declared_call_shape in utility_signatures.items():
    actual_call_shape = str(inspect.signature(function))
    assert actual_call_shape == declared_call_shape
    assert getattr(manimlib, function.__name__) is function
    assert function.__code__.co_name != "unavailable"

quadratic = bezier_utils.bezier(
    [np.array([0.0, 0.0]), np.array([2.0, 4.0]), np.array([4.0, 0.0])]
)
assert np.allclose(quadratic(0.25), [1.0, 1.5])
assert np.array_equal(
    bezier_utils.interpolate(
        np.array([0.0, 2.0]), np.array([4.0, 6.0]), 0.25
    ),
    [1.0, 3.0],
)
assert np.allclose(
    bezier_utils.inverse_interpolate(1.0, 5.0, np.array([1.0, 3.0, 5.0])),
    [0.0, 0.5, 1.0],
)
integer_value, integer_residue = bezier_utils.integer_interpolate(0, 10, 0.46)
assert integer_value == 4 and math.isclose(integer_residue, 0.6)
assert bezier_utils.integer_interpolate(3, 8, -1.0) == (3, 0.0)
assert bezier_utils.integer_interpolate(3, 8, 2.0) == (7, 1.0)
try:
    bezier_utils.bezier([])
except Exception as error:
    assert "empty list" in str(error)
else:
    raise AssertionError("empty Bezier controls were accepted")

black_to_white = color_utils.color_gradient(["#000000", "#FFFFFF"], 3)
assert [color.get_hex_l() for color in black_to_white] == [
    "#000000",
    "#B4B4B4",
    "#FFFFFF",
]
assert np.allclose(black_to_white[1].get_rgb(), [2**-0.5] * 3)
assert [color.get_hex_l() for color in color_utils.color_gradient(["#123456"], 1)] == [
    "#123456"
]
assert color_utils.color_gradient(iter(["#000000"]), 0) == []
assert color_utils.interpolate_color("#000000", "#FFFFFF", 0.25).get_hex_l() == (
    "#7F7F7F"
)
hsl_midpoint = color_utils.interpolate_color_by_hsl("#FF0000", "#0000FF", 0.5)
assert np.allclose(color_utils.color_to_rgb(hsl_midpoint), [0.0, 1.0, 0.0])
assert color_utils.average_color("#000000", "#FFFFFF").get_hex_l() == "#B4B4B4"
assert np.array_equal(color_utils.color_to_rgba("#123456", 0.25), [18 / 255, 52 / 255, 86 / 255, 0.25])
assert np.array_equal(color_utils.color_to_int_rgb("#123456"), [18, 52, 86])
assert np.array_equal(color_utils.color_to_int_rgba("#123456", 0.5), [18, 52, 86, 127])
assert color_utils.rgb_to_hex([18 / 255, 52 / 255, 86 / 255]) == "#123456"
assert color_utils.color_to_hex(color_utils.rgba_to_color([1.0, 0.0, 0.0, 0.2])) == "#FF0000"
assert color_utils.color_to_hex(color_utils.invert_color("#00FF00")) == "#FF00FF"
assert color_utils.hex_to_int("#123456") == 0x123456
assert color_utils.int_to_hex(0x123456) == "#123456"
assert np.allclose(color_utils.hex_to_rgb("#123456"), [18 / 255, 52 / 255, 86 / 255])
try:
    color_utils.color_gradient(["#000000"], 2)
except IndexError as error:
    assert str(error) == "list index out of range"
else:
    raise AssertionError("one-stop multi-sample gradient was accepted")
try:
    color_utils.color_gradient(["#000000", "#FFFFFF"], -1)
except ValueError as error:
    assert str(error) == "Number of samples, -1, must be non-negative."
else:
    raise AssertionError("negative-length gradient was accepted")
try:
    color_utils.average_color()
except TypeError as error:
    assert str(error) == "'numpy.float64' object is not iterable"
else:
    raise AssertionError("undefined empty color average was accepted")
try:
    color_utils.hex_to_rgb("not-a-color")
except ValueError as error:
    assert "invalid color" in str(error)
else:
    raise AssertionError("malformed color text was accepted")

assert simple_utils.choose(8, 3) == 56
assert simple_utils.clip(-2.0, -1.0, 1.0) == -1.0
assert simple_utils.clip(2.0, -1.0, 1.0) == 1.0
assert simple_utils.clip(0.25, -1.0, 1.0) == 0.25
assert np.allclose(simple_utils.sigmoid(np.array([-1.0, 0.0, 1.0])), [
    1 / (1 + math.e),
    0.5,
    1 / (1 + math.exp(-1)),
])
assert np.allclose(
    simple_utils.fdiv(
        np.array([0.0, 2.0]), np.array([0.0, 4.0]), zero_over_zero_value=7.0
    ),
    [7.0, 0.5],
)
assert math.isclose(
    simple_utils.binary_search(lambda value: value * value, 4.0, 0.0, 4.0),
    4.0,
    abs_tol=1e-4,
)
assert simple_utils.binary_search(lambda value: value * value, -1.0, 0.0, 4.0) is None

assert math.isclose(space_utils.angle_of_vector([0.0, 1.0]), math.pi / 2)
assert math.isclose(
    space_utils.angle_between_vectors([1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
    math.pi / 2,
)
assert space_utils.angle_between_vectors([0.0] * 4, [1.0] * 4) == 0.0
assert space_utils.line_intersects_path(
    [0.0, -1.0],
    [0.0, 1.0],
    np.array([[-1.0, 0.0], [1.0, 0.0]]),
)
assert space_utils.line_intersects_path(
    [0.0, -1.0, 8.0],
    [0.0, 1.0, -3.0],
    np.array([[-1.0, 0.0, 12.0], [1.0, 0.0, -5.0]]),
)
assert not space_utils.line_intersects_path(
    [0.0, 0.0],
    [0.0, 1.0],
    np.array([[-1.0, 0.0], [1.0, 0.0]]),
)
try:
    space_utils.line_intersects_path([0.0], [0.0, 1.0], [[-1.0, 0.0]])
except ValueError as error:
    assert "two or three components" in str(error)
else:
    raise AssertionError("line_intersects_path accepted a one-dimensional point")
out_vector = np.zeros(3)
assert space_utils.cross([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], out_vector) is out_vector
assert np.array_equal(out_vector, [0.0, 0.0, 1.0])
assert np.array_equal(
    space_utils.cross(
        np.array([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]),
        np.array([[0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]),
    ),
    [[0.0, 0.0, 1.0], [1.0, 0.0, 0.0]],
)
assert np.allclose(
    space_utils.compass_directions(),
    [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [-1.0, 0.0, 0.0], [0.0, -1.0, 0.0]],
    atol=1e-15,
)
try:
    space_utils.angle_of_vector([1.0])
except ValueError as error:
    assert "at least two" in str(error)
else:
    raise AssertionError("one-dimensional polar vector was accepted")

path_start = np.array([[0.0, 0.0, 0.0], [2.0, 1.0, 0.0]])
path_end = np.array([[2.0, 0.0, 0.0], [4.0, 1.0, 0.0]])
assert np.allclose(
    path_utils.straight_path(path_start, path_end, 0.25),
    [[0.5, 0.0, 0.0], [2.5, 1.0, 0.0]],
)
assert path_utils.path_along_arc(0.0) is path_utils.straight_path
assert np.allclose(
    path_utils.counterclockwise_path()(path_start[:1], path_end[:1], 0.5),
    [[1.0, -1.0, 0.0]],
)
assert np.allclose(
    path_utils.clockwise_path()(path_start[:1], path_end[:1], 0.5),
    [[1.0, 1.0, 0.0]],
)
assert np.allclose(
    path_utils.path_along_arc(math.pi, axis=np.zeros(3))(
        path_start[:1], path_end[:1], 0.5
    ),
    [[1.0, -1.0, 0.0]],
)
per_point_arcs = np.array([0.0, math.pi])
path_utils.path_along_arc(per_point_arcs)(path_start, path_end, 0.5)
assert np.array_equal(per_point_arcs, [0.01, math.pi])


# Reference point mutation composes the public NumPy resize policies over the
# live Marionette RecordBuffer.  All subclass lanes move together, while a
# size-changing write detaches old views through the generation protocol.
iterables = importlib.import_module("manimlib.utils.iterables")
assert manimlib.resize_array is iterables.resize_array
assert manimlib.resize_preserving_order is iterables.resize_preserving_order
assert str(inspect.signature(manimlib.resize_array)) == "(nparray, length)"
assert str(inspect.signature(manimlib.resize_preserving_order)) == (
    "(nparray, length)"
)
resize_probe = np.array([[1.0], [2.0], [3.0]])
assert np.array_equal(
    manimlib.resize_array(resize_probe, 5),
    [[1.0], [2.0], [3.0], [1.0], [2.0]],
)
assert np.array_equal(
    manimlib.resize_preserving_order(resize_probe, 5),
    [[1.0], [1.0], [2.0], [2.0], [3.0]],
)
assert manimlib.resize_preserving_order(np.zeros((0, 3)), 2).shape == (2,)

assert list(inspect.signature(Mobject.resize_points).parameters) == [
    "self",
    "new_length",
    "resize_func",
]
assert (
    inspect.signature(Mobject.resize_points).parameters["resize_func"].default
    is manimlib.resize_array
)
assert str(inspect.signature(Mobject.set_points)) == "(self, points)"
assert str(inspect.signature(Mobject.append_points)) == "(self, new_points)"
assert str(inspect.signature(Mobject.clear_points)) == "(self)"
assert str(inspect.signature(manimlib.PMobject.add_points)) == (
    "(self, points, rgbas=None, color=None, opacity=None)"
)
assert str(inspect.signature(manimlib.PMobject.add_point)) == (
    "(self, point, rgba=None, color=None, opacity=None)"
)
assert str(inspect.signature(manimlib.PMobject.set_points)) == "(self, points)"
assert str(inspect.signature(VMobject.set_points)) == "(self, points)"
assert str(inspect.signature(VMobject.append_points)) == "(self, points)"
assert str(inspect.signature(VMobject.set_anchors_and_handles)) == (
    "(self, anchors, handles)"
)
assert str(inspect.signature(VMobject.set_points_as_corners)) == "(self, points)"
assert str(inspect.signature(VMobject.start_new_path)) == "(self, point)"
assert str(inspect.signature(VMobject.add_cubic_bezier_curve)) == (
    "(self, anchor1, handle1, handle2, anchor2)"
)
assert str(inspect.signature(VMobject.add_cubic_bezier_curve_to)) == (
    "(self, handle1, handle2, anchor)"
)
assert str(inspect.signature(VMobject.add_quadratic_bezier_curve_to)) == (
    "(self, handle, anchor, allow_null_curve=True)"
)
assert str(inspect.signature(VMobject.add_line_to)) == (
    "(self, point, allow_null_line=True)"
)
assert str(inspect.signature(VMobject.add_smooth_curve_to)) == "(self, point)"
assert str(inspect.signature(VMobject.add_smooth_cubic_curve_to)) == (
    "(self, handle, point)"
)
assert str(inspect.signature(VMobject.add_arc_to)) == (
    "(self, point, angle, n_components=None, threshold=0.001)"
)
assert str(inspect.signature(VMobject.add_points_as_corners)) == "(self, points)"
assert str(inspect.signature(VMobject.add_subpath)) == "(self, points)"
assert str(inspect.signature(VMobject.append_vectorized_mobject)) == (
    "(self, vmobject)"
)
assert str(inspect.signature(VMobject.has_new_path_started)) == "(self)"
assert str(inspect.signature(VMobject.get_last_point)) == "(self)"
assert str(inspect.signature(VMobject.get_reflection_of_last_handle)) == "(self)"
assert str(inspect.signature(VMobject.close_path)) == "(self, smooth=False)"
assert str(inspect.signature(VMobject.is_closed)) == "(self)"
assert str(inspect.signature(VMobject.consider_points_equal)) == "(self, p0, p1)"
assert str(inspect.signature(VMobject.get_anchors_and_handles)) == "(self)"
assert str(inspect.signature(VMobject.get_start_anchors)) == "(self)"
assert str(inspect.signature(VMobject.get_end_anchors)) == "(self)"
assert str(inspect.signature(VMobject.get_anchors)) == "(self)"
assert str(inspect.signature(VMobject.get_bezier_tuples_from_points)) == (
    "(self, points)"
)
assert str(inspect.signature(VMobject.get_bezier_tuples)) == "(self)"
assert str(inspect.signature(VMobject.get_nth_curve_points)) == "(self, n)"
assert str(inspect.signature(VMobject.get_nth_curve_function)) == "(self, n)"
assert str(inspect.signature(VMobject.get_subpath_end_indices_from_points)) == (
    "(self, points)"
)
assert str(inspect.signature(VMobject.get_subpath_end_indices)) == "(self)"
assert str(inspect.signature(VMobject.get_subpaths_from_points)) == "(self, points)"
assert str(inspect.signature(VMobject.get_subpaths)) == "(self)"
assert str(inspect.signature(VMobject.insert_n_curves_to_point_list)) == (
    "(self, n, points)"
)
assert str(inspect.signature(VMobject.insert_n_curves)) == "(self, n, recurse=True)"
assert str(inspect.signature(VMobject.align_points)) == "(self, vmobject)"
assert list(inspect.signature(VMobject.subdivide_sharp_curves).parameters) == [
    "self",
    "angle_threshold",
    "recurse",
]
assert math.isclose(
    inspect.signature(VMobject.subdivide_sharp_curves)
    .parameters["angle_threshold"]
    .default,
    30 * manimlib.DEG,
    rel_tol=0.0,
    abs_tol=0.0,
)
assert (
    inspect.signature(VMobject.subdivide_sharp_curves)
    .parameters["recurse"]
    .default
    is True
)
assert list(inspect.signature(VMobject.is_smooth).parameters) == ["self", "angle_tol"]
assert math.isclose(
    inspect.signature(VMobject.is_smooth).parameters["angle_tol"].default,
    manimlib.DEG,
    rel_tol=0.0,
    abs_tol=0.0,
)
assert str(inspect.signature(VMobject.change_anchor_mode)) == "(self, mode)"
assert str(inspect.signature(VMobject.make_approximately_smooth)) == (
    "(self, recurse=True)"
)
assert str(inspect.signature(VMobject.make_jagged)) == "(self, recurse=True)"
assert str(inspect.signature(VMobject.reverse_points)) == "(self, recurse=True)"
assert str(inspect.signature(VMobject.get_area_vector)) == "(self)"
assert str(inspect.signature(VMobject.get_unit_normal)) == "(self, refresh=False)"
assert str(inspect.signature(VMobject.get_arc_length)) == "(self, n_sample_points=None)"
assert str(inspect.signature(VMobject.point_from_proportion)) == "(self, alpha)"
assert VMobject.tolerance_for_point_equality == 1e-8

mutation = CustomDtype()
mutation.data["point"][:] = [[-1.0, 0.0, 0.0], [1.0, 0.0, 0.0]]
mutation.data["rgba"][:] = [
    [0.1, 0.2, 0.3, 0.4],
    [0.5, 0.6, 0.7, 0.8],
]
mutation.data["wobble"][:] = [[10.0, 11.0], [20.0, 21.0]]
mutation_before = mutation.data.copy()
mutation_old_view = mutation.data
replacement_points = np.array(
    [
        [-4.0, 0.0, 0.0],
        [-2.0, 0.0, 0.0],
        [2.0, 0.0, 0.0],
        [4.0, 0.0, 0.0],
    ]
)
assert mutation.set_points(replacement_points) is mutation
mutation_expected = manimlib.resize_preserving_order(mutation_before, 4)
mutation_expected["point"][:] = replacement_points
assert np.array_equal(mutation.data, mutation_expected)
assert mutation_old_view.shape == (2,)
mutation_old_view["point"][0] = [99.0, 99.0, 99.0]
assert np.array_equal(mutation.data, mutation_expected)

# Same-length replacement keeps the current generation live.  Clearing saves
# the first complete record as the Reference default row, and growing again
# restores every non-point field from it.
same_generation = mutation.data
same_length_points = replacement_points + [0.0, 1.0, 0.0]
assert mutation.set_points(same_length_points) is mutation
assert np.array_equal(same_generation["point"], same_length_points)
saved_default = mutation.data[0].copy()
cleared_generation = mutation.data
assert mutation.clear_points() is mutation
assert mutation.get_points().shape == (0, 3)
assert cleared_generation.shape == (4,)
assert mutation.set_points([[7.0, 8.0, 9.0]]) is mutation
assert np.array_equal(mutation.data["point"], [[7.0, 8.0, 9.0]])
assert np.array_equal(mutation.data["rgba"][0], saved_default["rgba"])
assert np.array_equal(mutation.data["wobble"][0], saved_default["wobble"])

# append_points uses the Reference cyclic resize first, then copies the last
# retained record into the tail before installing the new point rows.
appended = CustomDtype()
appended.set_points(
    [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [2.0, 0.0, 0.0]]
)
appended.data["rgba"][:] = [
    [0.1, 0.0, 0.0, 1.0],
    [0.2, 0.0, 0.0, 1.0],
    [0.3, 0.0, 0.0, 1.0],
]
appended.data["wobble"][:] = [[1.0, 2.0], [3.0, 4.0], [5.0, 6.0]]
append_before = appended.data.copy()
append_tail = np.array([[3.0, 0.0, 0.0], [4.0, 0.0, 0.0]])
assert appended.append_points(append_tail) is appended
append_expected = manimlib.resize_array(append_before, 5)
append_expected[3:] = append_expected[2]
append_expected["point"][3:] = append_tail
assert np.array_equal(appended.data, append_expected)

point_cloud = manimlib.PMobject()
assert point_cloud.set_points([]) is point_cloud
assert point_cloud.get_points().shape == (0, 3)
assert point_cloud.set_points([[1.0, 2.0, 3.0]]) is point_cloud
assert np.array_equal(point_cloud.get_points(), [[1.0, 2.0, 3.0]])
point_cloud.set_points([[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [2.0, 0.0, 0.0]])
point_cloud.data["rgba"][:] = [
    [0.1, 0.0, 0.0, 1.0],
    [0.2, 0.0, 0.0, 1.0],
    [0.3, 0.0, 0.0, 1.0],
]
point_cloud_before = point_cloud.data.copy()
point_cloud_tail = np.array([[3.0, 0.0, 0.0], [4.0, 0.0, 0.0]])
point_cloud_rgbas = np.array(
    [[0.8, 0.1, 0.2, 0.5], [0.7, 0.2, 0.1, 0.25]]
)
assert point_cloud.add_points(point_cloud_tail, point_cloud_rgbas) is point_cloud
point_cloud_expected = manimlib.resize_array(point_cloud_before, 5)
point_cloud_expected[3:] = point_cloud_expected[2]
point_cloud_expected["point"][3:] = point_cloud_tail
point_cloud_expected["rgba"][3:] = point_cloud_rgbas
assert np.array_equal(point_cloud.data, point_cloud_expected)

# VMobject overrides enforce the shared-anchor odd/even contract before any
# mutation and refresh the native joint-angle column through Stage::set_points.
vm_source = VMobject().set_points_as_corners(
    [[0.0, 0.0, 0.0], [1.0, 1.0, 0.0], [2.0, 0.0, 0.0]]
)
# A bare VMobject starts with the Reference's visible-stroke/transparent-fill
# defaults, and the first Chisel corner-path allocation must retain them.
# SquareOnASphere constructs its three elbow marks through exactly this path
# and then changes only color/width; losing the default opacity leaves valid
# scene objects that are nevertheless invisible to Lumen.
assert np.allclose(vm_source.data["stroke_rgba"][:, :3], [0xDD / 255] * 3)
assert np.allclose(vm_source.data["stroke_rgba"][:, 3], 1.0)
assert np.allclose(vm_source.data["stroke_width"], 4.0)
assert np.allclose(vm_source.data["fill_rgba"][:, :3], [0x88 / 255] * 3)
assert np.allclose(vm_source.data["fill_rgba"][:, 3], 0.0)
assert vm_source.uniforms["joint_type"] == 1.0
assert vm_source.uniforms["anti_alias_width"] == 1.5
assert vm_source.set_stroke(manimlib.WHITE, 2) is vm_source
assert np.allclose(vm_source.data["stroke_rgba"][:, :3], 1.0)
assert np.allclose(vm_source.data["stroke_rgba"][:, 3], 1.0)
assert np.allclose(vm_source.data["stroke_width"], 2.0)
vm_mutation = VMobject()
assert vm_mutation.set_points(vm_source.get_points().copy()) is vm_mutation
assert np.array_equal(
    vm_mutation.data["joint_angle"], vm_source.data["joint_angle"]
)
vm_before_invalid = vm_mutation.data.copy()
try:
    vm_mutation.set_points([[0.0, 0.0, 0.0], [1.0, 0.0, 0.0]])
except AssertionError:
    pass
else:
    raise AssertionError("VMobject.set_points accepted an even point run")
assert np.array_equal(vm_mutation.data, vm_before_invalid)
try:
    vm_mutation.append_points([[3.0, 0.0, 0.0]])
except AssertionError:
    pass
else:
    raise AssertionError("VMobject.append_points accepted an odd append run")
assert np.array_equal(vm_mutation.data, vm_before_invalid)
assert vm_mutation.append_points(
    [[2.5, -0.5, 0.0], [3.0, 0.0, 0.0]]
) is vm_mutation
assert vm_mutation.get_num_points() == 7

# A bound write bakes a pending placement exactly once and then replaces the
# world-space records, rather than leaving an affine transform to double-apply.
bound_mutation = VMobject().set_points_as_corners(
    [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0]]
)
bound_mutation_scene = Scene()
bound_mutation_scene.add(bound_mutation)
bound_mutation.shift([10.0, 0.0, 0.0])
bound_replacement = np.array(
    [[-2.0, 0.0, 0.0], [-1.5, 0.0, 0.0], [-1.0, 0.0, 0.0]]
)
assert bound_mutation.set_points(bound_replacement) is bound_mutation
assert np.array_equal(bound_mutation.get_points(), bound_replacement)


# Live typed uniforms and open extension keys.
assert custom.uniforms["anti_alias_width"] == 1.5
custom.uniforms["anti_alias_width"] = 2.25
custom.uniforms["plugin_value"] = {"nested": [1, 2]}
assert custom.uniforms["anti_alias_width"] == 2.25
assert custom.uniforms["plugin_value"] == {"nested": [1, 2]}


# Detached family graphs bind transactionally and remain live afterwards.
scene = Scene()
parent = Mobject()
parent.resize(1)
parent.set_field("point", 0, [1.0, 1.0, 0.0])
child = Mobject()
child.resize(1)
child.set_field("point", 0, [2.0, 2.0, 0.0])
outsider = Mobject()
parent.add(child)
assert parent.family_size() == 2
scene.add(parent, outsider)
assert scene.root_count() == 2
assert scene.mobjects[0] is parent
assert parent.family_size() == 2
parent.submobjects.clear()
assert parent.family_size() == 1
parent.submobjects.append(child)
assert parent.family_size() == 2
try:
    parent.submobjects.append(child)
except ValueError:
    pass
else:
    raise AssertionError("duplicate family edge was accepted")
try:
    child.submobjects.append(parent)
except bridge_errors.FamilyCycleError:
    pass
else:
    raise AssertionError("family cycle was accepted")

# The public family-list methods preserve the Reference's ordered
# identity-set semantics while every accepted edit is mirrored into the
# detached nursery or bound Marionette graph.  In particular, add is not a
# raw list extension: repeated and already-present children are ignored.
assert str(inspect.signature(Mobject.add)) == "(self, *mobjects)"
assert str(inspect.signature(Mobject.remove)) == (
    "(self, *to_remove, reassemble=True, recurse=True)"
)
assert str(inspect.signature(Mobject.clear)) == "(self)"
assert str(inspect.signature(Mobject.set_submobjects)) == (
    "(self, submobject_list)"
)
assert str(inspect.signature(Mobject.reverse_submobjects)) == "(self)"
family_add_root = Mobject()
family_add_a = Mobject()
family_add_b = Mobject()
family_add_c = Mobject()
assert family_add_root.add(
    family_add_a,
    family_add_a,
    family_add_b,
    family_add_a,
) is family_add_root
assert family_add_root.submobjects == [family_add_a, family_add_b]
assert family_add_root.add(family_add_a) is family_add_root
assert family_add_root.submobjects == [family_add_a, family_add_b]

# Self-containment is rejected before any member of the batch mutates the
# family.  A later invalid ordinary Python value preserves the successfully
# attached Mobject prefix but is itself never admitted to the safe graph.
try:
    family_add_root.add(family_add_c, family_add_root)
except Exception as error:
    assert str(error) == "Mobject cannot contain self"
else:
    raise AssertionError("Mobject.add accepted self-containment")
assert family_add_root.submobjects == [family_add_a, family_add_b]
try:
    family_add_root.add(family_add_c, object())
except TypeError as error:
    assert str(error) == "submobjects must be Mobject instances"
else:
    raise AssertionError("Mobject.add accepted a non-Mobject")
assert family_add_root.submobjects == [family_add_a, family_add_b, family_add_c]

# set_submobjects is Reference clear-then-add, including identity dedup and
# its observable prefix on a later failure.
assert family_add_root.set_submobjects(
    [family_add_b, family_add_b, family_add_c]
) is family_add_root
assert family_add_root.submobjects == [family_add_b, family_add_c]
try:
    family_add_root.set_submobjects([family_add_a, object(), family_add_b])
except TypeError as error:
    assert str(error) == "submobjects must be Mobject instances"
else:
    raise AssertionError("Mobject.set_submobjects accepted a non-Mobject")
assert family_add_root.submobjects == [family_add_a]

# Recursive remove snapshots family order and detaches every matching edge;
# recurse=False touches only the selected root.  Shared descendants retain
# their Python identity while both parent edges are edited independently.
family_remove_shared = Mobject()
family_remove_left = Mobject(family_remove_shared)
family_remove_right = Mobject(family_remove_shared)
family_remove_root = Mobject(family_remove_left, family_remove_right)
assert family_remove_root.remove(family_remove_shared) is family_remove_root
assert family_remove_left.submobjects == []
assert family_remove_right.submobjects == []
family_remove_left.add(family_remove_shared)
family_remove_right.add(family_remove_shared)
assert (
    family_remove_root.remove(
        family_remove_shared,
        recurse=False,
        reassemble=False,
    )
    is family_remove_root
)
assert family_remove_left.submobjects == [family_remove_shared]
assert family_remove_right.submobjects == [family_remove_shared]

family_remove_other = Mobject()
family_remove_root.add(family_remove_other)
try:
    family_remove_root.remove(family_remove_other, object(), recurse=False)
except TypeError as error:
    assert str(error) == "submobjects must be Mobject instances"
else:
    raise AssertionError("Mobject.remove accepted a non-Mobject")
assert family_remove_other not in family_remove_root.submobjects

assert family_remove_root.reverse_submobjects() is family_remove_root
assert family_remove_root.submobjects == [family_remove_right, family_remove_left]
assert family_remove_root.clear() is family_remove_root
assert family_remove_root.submobjects == []
assert family_remove_left.submobjects == [family_remove_shared]
assert family_remove_right.submobjects == [family_remove_shared]

# The same surface commits real native child order after Scene adoption and
# keeps the typed foreign-stage and cycle refusals transactional.
family_mutation_scene = Scene()
bound_family_root = Mobject()
bound_family_a = Mobject()
bound_family_b = Mobject()
family_mutation_scene.add(bound_family_root)
assert bound_family_root.add(bound_family_a, bound_family_b, bound_family_a) is (
    bound_family_root
)
assert bound_family_root.submobjects == [bound_family_a, bound_family_b]
assert bound_family_root.family_size() == 3
assert bound_family_root.reverse_submobjects() is bound_family_root
assert bound_family_root.get_family() == [
    bound_family_root,
    bound_family_b,
    bound_family_a,
]

foreign_family_scene = Scene()
foreign_family_child = Mobject()
foreign_family_scene.add(foreign_family_child)
bound_before_foreign = list(bound_family_root.submobjects)
try:
    bound_family_root.add(foreign_family_child)
except bridge_errors.ForeignStageError:
    pass
else:
    raise AssertionError("Mobject.add accepted a foreign-stage child")
assert bound_family_root.submobjects == bound_before_foreign

try:
    bound_family_a.add(bound_family_root)
except bridge_errors.FamilyCycleError:
    pass
else:
    raise AssertionError("Mobject.add accepted a family cycle")
assert bound_family_a.submobjects == []

other_scene = Scene()
try:
    other_scene.add(parent)
except bridge_errors.ForeignStageError:
    pass
else:
    raise AssertionError("cross-Scene handle was accepted")


# Public Scene draw-order edits route through the native runtime. Promotion
# is the same stable add operation as the Reference; demotion prepends the
# whole argument batch without a z-sort and can adopt a detached mobject.
order_scene = Scene()
order_back = Mobject()
order_middle = Mobject()
order_front = Mobject()
order_scene.add(order_back, order_middle, order_front)
assert order_scene.bring_to_front(order_back) is order_scene
assert order_scene.mobjects == [order_middle, order_front, order_back]
assert order_scene.bring_to_back(order_back, order_front) is order_scene
assert order_scene.mobjects == [order_back, order_front, order_middle]

detached_back = Mobject()
assert order_scene.bring_to_back(detached_back) is order_scene
assert order_scene.mobjects == [detached_back, order_back, order_front, order_middle]
assert order_scene.mobjects[0] is detached_back

order_before_refusal = list(order_scene.mobjects)
try:
    order_scene.bring_to_front(object())
except TypeError:
    pass
else:
    raise AssertionError("Scene.bring_to_front accepted a non-Mobject")
assert order_scene.mobjects == order_before_refusal

foreign_order_scene = Scene()
foreign_order_mobject = Mobject()
foreign_order_scene.add(foreign_order_mobject)
try:
    order_scene.bring_to_back(foreign_order_mobject)
except bridge_errors.ForeignStageError:
    pass
else:
    raise AssertionError("Scene.bring_to_back accepted a foreign-stage Mobject")
assert order_scene.mobjects == order_before_refusal


# Scene.replace is an exact top-level splice. Its absent-source guard runs
# before replacement binding, matching the Reference's membership test.
replace_scene = Scene()
replace_back = Mobject()
replace_middle = Mobject()
replace_front = Mobject()
replace_scene.add(replace_back, replace_middle, replace_front)
replacement_left = Mobject()
replacement_right = Mobject()
assert replace_scene.replace(
    replace_middle,
    replacement_left,
    replacement_right,
) is replace_scene
assert replace_scene.mobjects == [
    replace_back,
    replacement_left,
    replacement_right,
    replace_front,
]
assert replace_scene.mobjects[1] is replacement_left
assert replace_scene.mobjects[2] is replacement_right

assert replace_scene.replace(replacement_left) is replace_scene
assert replace_scene.mobjects == [replace_back, replacement_right, replace_front]

detached_source = Mobject()
detached_replacement = Mobject()
assert replace_scene.replace(detached_source, detached_replacement) is replace_scene
assert replace_scene.mobjects == [replace_back, replacement_right, replace_front]
assert not detached_source._is_bound()
assert not detached_replacement._is_bound()

nonmobject_replacement = Mobject()
assert replace_scene.replace(object(), nonmobject_replacement) is replace_scene
assert replace_scene.mobjects == [replace_back, replacement_right, replace_front]
assert not nonmobject_replacement._is_bound()

family_scene = Scene()
family_root = Mobject()
family_child = Mobject()
family_root.submobjects.append(family_child)
family_scene.add(family_root)
nonroot_replacement = Mobject()
assert family_scene.replace(family_child, nonroot_replacement) is family_scene
assert family_scene.mobjects == [family_root]
assert not nonroot_replacement._is_bound()

replace_before_refusal = list(replace_scene.mobjects)
try:
    replace_scene.replace(replacement_right, object())
except TypeError:
    pass
else:
    raise AssertionError("Scene.replace accepted a non-Mobject replacement")
assert replace_scene.mobjects == replace_before_refusal

foreign_replace_scene = Scene()
foreign_replacement = Mobject()
foreign_replace_scene.add(foreign_replacement)
foreign_source_replacement = Mobject()
assert replace_scene.replace(
    foreign_replacement,
    foreign_source_replacement,
) is replace_scene
assert replace_scene.mobjects == replace_before_refusal
assert not foreign_source_replacement._is_bound()
try:
    replace_scene.replace(replacement_right, foreign_replacement)
except bridge_errors.ForeignStageError:
    pass
else:
    raise AssertionError("Scene.replace accepted a foreign-stage replacement")
assert replace_scene.mobjects == replace_before_refusal


# Scene.get_mobjects returns a fresh ordered snapshot of the native draw list.
# Scene.remove_all_except resets that list through native clear + stable add,
# preserving batch duplicates and adopting detached mobjects.
membership_scene = Scene()
membership_back = Mobject().set_z_index(-1)
membership_old = Mobject()
membership_front = Mobject().set_z_index(1)
membership_scene.add(membership_old, membership_front, membership_back)
membership_snapshot = membership_scene.get_mobjects()
assert membership_snapshot == [membership_back, membership_old, membership_front]
assert membership_snapshot is not membership_scene.get_mobjects()
assert membership_snapshot[0] is membership_back
membership_snapshot.clear()
assert membership_scene.mobjects == [membership_back, membership_old, membership_front]

membership_detached = Mobject()
assert membership_scene.remove_all_except(
    membership_front,
    membership_detached,
    membership_front,
    membership_back,
) is membership_scene
assert membership_scene.get_mobjects() == [
    membership_back,
    membership_detached,
    membership_front,
    membership_front,
]
assert membership_scene.get_mobjects()[1] is membership_detached
assert membership_detached._is_bound()

membership_before_refusal = membership_scene.get_mobjects()
try:
    membership_scene.remove_all_except(membership_back, object())
except TypeError:
    pass
else:
    raise AssertionError("Scene.remove_all_except accepted a non-Mobject")
assert membership_scene.get_mobjects() == membership_before_refusal

foreign_membership_scene = Scene()
foreign_membership_mobject = Mobject()
foreign_membership_scene.add(foreign_membership_mobject)
try:
    membership_scene.remove_all_except(foreign_membership_mobject)
except bridge_errors.ForeignStageError:
    pass
else:
    raise AssertionError("Scene.remove_all_except accepted a foreign-stage Mobject")
assert membership_scene.get_mobjects() == membership_before_refusal

assert membership_scene.remove_all_except() is membership_scene
assert membership_scene.get_mobjects() == []


# Python-owned Scene collection helpers filter arbitrary iterables before
# routing through native add, and copy each native top-level placement.
class AmongMobject(Mobject):
    pass


among_scene = Scene()
among_front = AmongMobject().set_z_index(1)
among_back = Mobject().set_z_index(-1)
among_seen = []


def among_values():
    for value in (object(), among_front, "ignored", among_back, None):
        among_seen.append(value)
        yield value


assert among_scene.add_mobjects_among(among_values()) is among_scene
assert len(among_seen) == 5
assert among_scene.mobjects == [among_back, among_front]
assert among_scene.mobjects[1] is among_front

among_failed = Mobject()


def failing_among_values():
    yield among_failed
    raise RuntimeError("iterator failure")


among_before_failure = among_scene.get_mobjects()
try:
    among_scene.add_mobjects_among(failing_among_values())
except RuntimeError as error:
    assert str(error) == "iterator failure"
else:
    raise AssertionError("Scene.add_mobjects_among swallowed an iterator failure")
assert among_scene.get_mobjects() == among_before_failure
assert not among_failed._is_bound()

try:
    among_scene.add_mobjects_among(1)
except TypeError:
    pass
else:
    raise AssertionError("Scene.add_mobjects_among accepted a non-iterable")
assert among_scene.get_mobjects() == among_before_failure

foreign_among_scene = Scene()
foreign_among_mobject = Mobject()
foreign_among_scene.add(foreign_among_mobject)
try:
    among_scene.add_mobjects_among([foreign_among_mobject])
except bridge_errors.ForeignStageError:
    pass
else:
    raise AssertionError("Scene.add_mobjects_among accepted a foreign-stage Mobject")
assert among_scene.get_mobjects() == among_before_failure

# Family introspection preserves Reference preorder and path-wise duplicates,
# while Scene helpers expose fresh snapshots over live engine roots.
family_shared = VMobject().set_points_as_corners(
    [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0]]
)
family_left = Mobject(family_shared)
family_right = Mobject(family_shared)
family_root = Mobject(family_left, family_right)
family_expected = [
    family_root,
    family_left,
    family_shared,
    family_right,
    family_shared,
]
family_actual = family_root.get_family()
assert len(family_actual) == len(family_expected)
assert all(actual is expected for actual, expected in zip(family_actual, family_expected))
assert family_root.get_family() is not family_actual
assert family_root.get_family(recurse=False) == [family_root]
assert family_root.get_family(recurse=False)[0] is family_root
family_with_points = family_root.family_members_with_points()
assert len(family_with_points) == 2
assert all(member is family_shared for member in family_with_points)
assert family_root.family_members_with_points() is not family_with_points

# Family spatial queries are ordinary Python compositions over Marionette's
# world-space points and retained bounding boxes. Shared descendants remain
# path-wise duplicates, matching the Reference compatibility surface.
assert str(inspect.signature(Mobject.get_all_points)) == "(self)"
assert str(inspect.signature(Mobject.get_bounding_box)) == "(self)"
assert str(inspect.signature(Mobject.compute_bounding_box)) == "(self)"
assert str(inspect.signature(Mobject.refresh_bounding_box)) == (
    "(self, recurse_down=False, recurse_up=True)"
)
assert str(inspect.signature(Mobject.are_points_touching)) == (
    "(self, points, buff=0)"
)
assert str(inspect.signature(Mobject.is_point_touching)) == (
    "(self, point, buff=0)"
)
assert str(inspect.signature(Mobject.is_touching)) == (
    "(self, mobject, buff=0.01)"
)
assert str(inspect.signature(Mobject.get_all_corners)) == "(self)"
assert str(inspect.signature(Mobject.get_center_of_mass)) == "(self)"
assert str(inspect.signature(Mobject.get_boundary_point)) == "(self, direction)"
assert str(inspect.signature(manimlib.DotCloud.compute_bounding_box)) == "(self)"

family_all_points = family_root.get_all_points()
shared_points = family_shared.get_points()
assert family_all_points.dtype == np.float32
assert family_all_points.shape == (2 * len(shared_points), 3)
assert np.array_equal(family_all_points[: len(shared_points)], shared_points)
assert np.array_equal(family_all_points[len(shared_points) :], shared_points)
assert np.allclose(
    family_root.compute_bounding_box(),
    [[0.0, 0.0, 0.0], [0.5, 0.0, 0.0], [1.0, 0.0, 0.0]],
)
assert family_root.refresh_bounding_box() is family_root
assert family_root.refresh_bounding_box(True, False) is family_root

empty_spatial = Mobject(Mobject())
assert empty_spatial.get_all_points().shape == (0, 3)
assert np.array_equal(empty_spatial.compute_bounding_box(), np.zeros((3, 3)))

mass_probe = Mobject()
mass_probe.resize(2)
mass_probe.set_field("point", 0, [-2.0, 0.0, 0.0])
mass_probe.set_field("point", 1, [1.0, 0.0, 0.0])
assert np.allclose(mass_probe.get_center_of_mass(), [-0.5, 0.0, 0.0])
assert np.allclose(mass_probe.get_boundary_point(manimlib.RIGHT), [1.0, 0.0, 0.0])
assert np.allclose(mass_probe.get_boundary_point(manimlib.LEFT), [-2.0, 0.0, 0.0])

touch_probe = Mobject()
touch_probe.resize(1)
touch_probe.set_field("point", 0, [0.0, 0.0, 0.0])
touch_points = np.array(
    [[0.0, 0.0, 0.0], [0.25, 0.0, 0.0], [-0.25, 0.0, 0.0]]
)
assert np.array_equal(
    touch_probe.are_points_touching(touch_points, buff=0.25),
    [True, True, True],
)
assert not touch_probe.are_points_touching(
    np.array([[0.2501, 0.0, 0.0]]), buff=0.25
)[0]
assert isinstance(touch_probe.is_point_touching([0.0, 0.0, 0.0]), np.bool_)
assert touch_probe.is_point_touching([0.25, 0.0, 0.0], buff=0.25)

other_touch_probe = Mobject()
other_touch_probe.resize(1)
other_touch_probe.set_field("point", 0, [1.0, 0.0, 0.0])
assert not touch_probe.is_touching(other_touch_probe, buff=0.999)
assert touch_probe.is_touching(other_touch_probe, buff=1.0)
assert isinstance(touch_probe.is_touching(other_touch_probe), bool)

dot_spatial = manimlib.DotCloud([[2.0, 3.0, 0.0]], radius=0.5)
dot_box = np.array(
    [[1.5, 2.5, -0.5], [2.0, 3.0, 0.0], [2.5, 3.5, 0.5]]
)
assert np.allclose(dot_spatial.compute_bounding_box(), dot_box)
assert np.allclose(dot_spatial.get_bounding_box(), dot_box)
assert np.allclose(dot_spatial.get_left(), [1.5, 3.0, 0.0])
assert dot_spatial.get_width() == 1.0
assert np.allclose(
    dot_spatial.get_all_corners(),
    [
        [1.5, 2.5, -0.5],
        [1.5, 2.5, 0.5],
        [2.5, 2.5, -0.5],
        [2.5, 2.5, 0.5],
        [1.5, 3.5, -0.5],
        [1.5, 3.5, 0.5],
        [2.5, 3.5, -0.5],
        [2.5, 3.5, 0.5],
    ],
)
dot_group = Mobject(dot_spatial)
assert np.allclose(dot_group.get_bounding_box(), dot_box)
dot_scene = Scene()
dot_scene.add(dot_group)
assert np.allclose(dot_group.get_bounding_box(), dot_box)

spatial_camera_frame = manimlib.CameraFrame(
    frame_shape=(10.0, 6.0), center_point=(1.0, 2.0, 0.0)
)
assert np.allclose(
    spatial_camera_frame.compute_bounding_box(),
    [[-4.0, -1.0, 0.0], [1.0, 2.0, 0.0], [6.0, 5.0, 0.0]],
)

live_box = manimlib.Square()
live_box_before = live_box.get_bounding_box().copy()
live_box.get_points()[:, 0] += 2.0
assert np.allclose(
    live_box.get_bounding_box(), live_box_before + [2.0, 0.0, 0.0]
)

family_scene = Scene()
family_isolated = Mobject()
family_duplicate = Mobject()
family_scene.add(
    family_root,
    family_shared,
    family_isolated,
    family_duplicate,
    family_duplicate,
)
family_roots_before = family_scene.get_mobjects()
top_level = family_scene.get_top_level_mobjects()
assert len(top_level) == 2
assert top_level[0] is family_root
assert top_level[1] is family_isolated
assert family_scene.get_top_level_mobjects() is not top_level

scene_family_expected = [
    family_root,
    family_left,
    family_shared,
    family_right,
    family_shared,
    family_shared,
    family_isolated,
    family_duplicate,
    family_duplicate,
]
scene_family_actual = family_scene.get_mobject_family_members()
assert len(scene_family_actual) == len(scene_family_expected)
assert all(
    actual is expected
    for actual, expected in zip(scene_family_actual, scene_family_expected)
)
assert family_scene.get_mobject_family_members() is not scene_family_actual
assert family_scene.get_time() == 0.0
family_scene.wait(1.0 / 30.0)
assert abs(family_scene.get_time() - (1.0 / 30.0)) < 1e-15
family_roots_after = family_scene.get_mobjects()
assert len(family_roots_after) == len(family_roots_before)
assert all(
    after is before
    for after, before in zip(family_roots_after, family_roots_before)
)

copy_source = VMobject().set_points_as_corners(
    [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0]]
)
copy_source.label = {"shared": True}
copy_scene = Scene()
copy_scene.add(copy_source, copy_source)
copy_roots_before = copy_scene.get_mobjects()
scene_copies = copy_scene.get_mobject_copies()
assert len(scene_copies) == 2
assert scene_copies[0] is not copy_source
assert scene_copies[1] is not copy_source
assert scene_copies[0] is not scene_copies[1]
assert type(scene_copies[0]) is type(copy_source)
assert scene_copies[0].label is copy_source.label
assert np.allclose(scene_copies[0].get_points(), copy_source.get_points())
scene_copies[0].shift([3.0, 0.0, 0.0])
assert not np.allclose(scene_copies[0].get_points(), copy_source.get_points())
assert copy_scene.get_mobjects() == copy_roots_before
assert all(copy not in copy_scene.get_mobjects() for copy in scene_copies)
assert copy_scene.get_mobject_copies() is not scene_copies


# Bound copy uses Marionette's CopyMap, then Python remaps __dict__ aliases.
parent.label = child
parent.buddy = outsider
parent.nested = [child]
parent.uniforms["plugin_value"] = {"nested": [1, 2]}
tick = lambda mob, dt: None
parent.updaters.append(tick)
clone = copy.copy(parent)
assert type(clone) is type(parent)
assert clone is not parent
assert clone.submobjects[0] is clone.label
assert clone.label is not child
assert clone.buddy is outsider
assert clone.nested is parent.nested
assert clone.updaters is not parent.updaters
assert clone.updaters[0] is tick
assert clone.uniforms["plugin_value"] == {"nested": [1, 2]}
clone.set_field("point", 0, [99.0, 0.0, 0.0])
assert parent.get_field("point", 0) == [1.0, 1.0, 0.0]

deep = copy.deepcopy(parent)
assert deep.label is deep.submobjects[0]
assert deep.nested is not parent.nested
assert deep.nested[0] is deep.label
assert deep.uniforms["plugin_value"] is not parent.uniforms["plugin_value"]


# High-frequency Mobject helpers compose the live Stage-backed organization,
# copy, state, transform, and style seams instead of schema placeholders.
method_deep = parent.deepcopy()
assert method_deep.label is method_deep.submobjects[0]
assert method_deep.nested is not parent.nested
assert method_deep.nested[0] is method_deep.label
assert method_deep.uniforms["plugin_value"] is not parent.uniforms["plugin_value"]


def utility_marker(x, name):
    marker = VMobject().set_points_as_corners(
        [[x - 0.25, 0.0, 0.0], [x + 0.25, 0.0, 0.0]]
    )
    marker.name = name
    return marker


utility_a = utility_marker(2.0, "a")
utility_b = utility_marker(-1.0, "b")
utility_c = utility_marker(1.0, "c")
utility_d = utility_marker(1.0, "d")
utility_group = Mobject(utility_a, utility_b)
utility_scene = Scene()
utility_scene.add(utility_group)

# list_update keeps the last occurrence, so already-present utility_a wins
# over the same object in the prepend list and every child remains unique.
assert utility_group.add_to_back(utility_c, utility_a) is utility_group
assert list(utility_group.submobjects) == [utility_c, utility_a, utility_b]
assert utility_group.add_to_back(utility_d, utility_d) is utility_group
assert list(utility_group.submobjects) == [utility_d, utility_c, utility_a, utility_b]

# Python's stable sort is committed into Marionette. Equal-center c/d order is
# preserved by the default branch; both callback forms remain supported.
assert utility_group.sort() is utility_group
assert list(utility_group.submobjects) == [utility_b, utility_d, utility_c, utility_a]
assert utility_group.sort(point_to_num_func=lambda point: -point[0]) is utility_group
assert list(utility_group.submobjects) == [utility_a, utility_d, utility_c, utility_b]
assert utility_group.sort(submob_func=lambda mob: mob.name) is utility_group
assert [mob.name for mob in utility_group.submobjects] == ["a", "b", "c", "d"]
# A bound copy is sourced from the Stage, independently proving native child
# order rather than merely re-reading the compatibility list we just mutated.
assert [mob.name for mob in utility_group.copy().submobjects] == ["a", "b", "c", "d"]

shuffle_child_members = [utility_marker(i, f"inner-{i}") for i in range(3)]
shuffle_child = Mobject(*shuffle_child_members)
shuffle_leaf = utility_marker(5.0, "outer-leaf")
shuffle_root = Mobject(shuffle_child, shuffle_leaf)
shuffle_seed = 1729
shuffle_rng = manimlib.random.Random(shuffle_seed)
expected_inner = list(shuffle_child_members)
expected_outer = [shuffle_child, shuffle_leaf]
shuffle_rng.shuffle(expected_inner)
shuffle_rng.shuffle(expected_outer)
manimlib.random.seed(shuffle_seed)
assert shuffle_root.shuffle(recurse=True) is shuffle_root
assert list(shuffle_child.submobjects) == expected_inner
assert list(shuffle_root.submobjects) == expected_outer

detached_restore_child = utility_marker(0.0, "detached-restore")
detached_restore_group = Mobject(detached_restore_child)
detached_restore_group.named_child = detached_restore_child
detached_restore_group.save_state()
detached_restore_group.named_child = utility_marker(8.0, "wrong-detached-alias")
detached_restore_group.shift([3.0, -2.0, 0.0])
assert detached_restore_group.restore() is detached_restore_group
assert np.allclose(detached_restore_child.get_center(), [0.0, 0.0, 0.0])
assert detached_restore_group.named_child is detached_restore_child

bound_restore_child = utility_marker(0.0, "bound-restore")
bound_restore_group = Mobject(bound_restore_child)
bound_restore_group.named_child = bound_restore_child
bound_restore_scene = Scene()
bound_restore_scene.add(bound_restore_group)
bound_restore_group.save_state()
bound_restore_group.named_child = utility_marker(8.0, "wrong-bound-alias")
bound_restore_group.shift([-4.0, 1.5, 0.0])
assert bound_restore_group.restore() is bound_restore_group
assert np.allclose(bound_restore_child.get_center(), [0.0, 0.0, 0.0])
assert bound_restore_group.named_child is bound_restore_child

try:
    Mobject().restore()
except Exception as error:
    assert type(error) is Exception
    assert str(error) == "Trying to restore without having saved"
else:
    raise AssertionError("restore without save_state did not refuse")

shape_restore_group = Mobject(Mobject())
shape_restore_group.resize(1)
shape_restore_group.save_state()
shape_restore_group.set_field("point", 0, [4.0, 0.0, 0.0])
shape_restore_group.add(Mobject())
try:
    shape_restore_group.restore()
except RuntimeError as error:
    assert str(error) == (
        "become between families of different shapes "
        "(family alignment lands with fm-cye)"
    )
else:
    raise AssertionError("detached restore silently accepted family-shape drift")
assert shape_restore_group.get_field("point", 0) == [4.0, 0.0, 0.0]

schema_restore_group = Mobject(Mobject())
schema_restore_group.resize(1)
schema_restore_group.save_state()
schema_restore_group.set_field("point", 0, [7.0, 0.0, 0.0])
schema_restore_group.set_submobjects([VMobject()])
try:
    schema_restore_group.restore()
except RuntimeError as error:
    assert str(error) == "become between records of different schemas"
else:
    raise AssertionError("detached restore silently accepted record-schema drift")
assert schema_restore_group.get_field("point", 0) == [7.0, 0.0, 0.0]

spacing_left = utility_marker(1.0, "spacing-left")
spacing_right = utility_marker(3.0, "spacing-right")
spacing_group = Mobject(spacing_left, spacing_right)
spacing_widths = [mob.get_width() for mob in spacing_group.submobjects]
assert (
    spacing_group.space_out_submobjects(2.0, about_point=[1.0, 0.0, 0.0])
    is spacing_group
)
assert np.allclose(spacing_left.get_center(), [1.0, 0.0, 0.0])
assert np.allclose(spacing_right.get_center(), [5.0, 0.0, 0.0])
assert np.allclose(
    [mob.get_width() for mob in spacing_group.submobjects], spacing_widths
)

# High-demand transform and frame-containment surface. These assertions cover
# both detached nursery graphs and a Scene-bound native family so a Python
# shell loop cannot accidentally double-apply the native Stage traversal.
assert str(inspect.signature(Mobject.apply_points_function)) == (
    "(self, func, about_point=None, about_edge=array([0., 0., 0.]), "
    "works_on_bounding_box=False)"
)
assert Mobject.dim == 3
assert str(inspect.signature(Mobject.apply_function)) == (
    "(self, function, **kwargs)"
)
assert str(inspect.signature(Mobject.apply_function_to_position)) == (
    "(self, function)"
)
assert str(inspect.signature(Mobject.apply_function_to_submobject_positions)) == (
    "(self, function)"
)
assert str(inspect.signature(Mobject.apply_matrix)) == "(self, matrix, **kwargs)"
assert str(inspect.signature(Mobject.apply_complex_function)) == (
    "(self, function, **kwargs)"
)
assert str(inspect.signature(Mobject.shift_onto_screen)) == "(self, **kwargs)"
assert str(inspect.signature(Mobject.is_off_screen)) == "(self)"
assert str(inspect.signature(VMobject.apply_function)) == (
    "(self, function, make_smooth=False, **kwargs)"
)
assert str(inspect.signature(VMobject.apply_matrix)) == "(self, *args, **kwargs)"
assert VMobject.make_smooth_after_applying_functions is False


def nonlinear_box_map(rows):
    result = rows.copy()
    result[:, 0] = rows[:, 0] + rows[:, 1] ** 2
    return result


box_mapped = manimlib.Square()
box_before = box_mapped.get_bounding_box().copy()
point_map_dtypes = []


def dtype_observing_box_map(rows):
    point_map_dtypes.append(rows.dtype)
    return nonlinear_box_map(rows)


assert (
    box_mapped.apply_points_function(
        dtype_observing_box_map,
        about_edge=None,
        works_on_bounding_box=True,
    )
    is box_mapped
)
assert np.allclose(box_mapped.get_bounding_box(), nonlinear_box_map(box_before))
assert point_map_dtypes == [np.dtype(np.float32), np.dtype(float)]

partial_box = manimlib.Square()
partial_box_points = partial_box.get_points().copy()
partial_box_before = partial_box.get_bounding_box().copy()
partial_box_calls = 0


def mutate_then_fail_on_box(rows):
    global partial_box_calls
    partial_box_calls += 1
    rows[:] += manimlib.RIGHT
    if partial_box_calls == 2:
        raise LookupError("box-map boom")
    return rows


try:
    partial_box.apply_points_function(
        mutate_then_fail_on_box,
        about_edge=None,
        works_on_bounding_box=True,
    )
except LookupError as error:
    assert str(error) == "box-map boom"
else:
    raise AssertionError("a mutating bounding-box callback did not propagate")
assert np.allclose(partial_box.get_points(), partial_box_points + manimlib.RIGHT)
assert np.allclose(
    partial_box.get_bounding_box(), partial_box_before + manimlib.RIGHT
)

# Every member keeps its directly transformed Reference box until a later
# member revision invalidates that family of caches.
box_child = manimlib.Square().shift(3 * manimlib.RIGHT)
box_family = Mobject(manimlib.Square(), box_child)
family_box_before = box_family.get_bounding_box().copy()
box_family.apply_points_function(
    nonlinear_box_map,
    about_edge=None,
    works_on_bounding_box=True,
)
assert np.allclose(
    box_family.get_bounding_box(), nonlinear_box_map(family_box_before)
)
box_child.shift(manimlib.RIGHT)
assert np.allclose(box_family.get_right(), [6.0, 0.0, 0.0])

detached_matrix_child = manimlib.Square().shift(manimlib.RIGHT)
detached_matrix_family = Mobject(detached_matrix_child)
detached_before = detached_matrix_child.get_center().copy()
assert detached_matrix_family.apply_matrix([[2.0, 0.0], [0.0, 3.0]]) is (
    detached_matrix_family
)
assert np.allclose(detached_matrix_child.get_center(), [2.0, 0.0, 0.0])
assert not np.allclose(detached_matrix_child.get_center(), 4.0 * detached_before)

bound_matrix_child = manimlib.Square().shift(manimlib.RIGHT)
bound_matrix_family = Mobject(bound_matrix_child)
bound_matrix_scene = Scene()
bound_matrix_scene.add(bound_matrix_family)
assert bound_matrix_family.apply_matrix(
    [[2.0, 0.0, 0.0], [0.0, 3.0, 0.0], [0.0, 0.0, 1.0]]
) is bound_matrix_family
assert np.allclose(bound_matrix_child.get_center(), [2.0, 0.0, 0.0])

custom_point_fields = CustomPointFields()
assert custom_point_fields.apply_matrix([[2.0, 0.0], [0.0, 3.0]]) is (
    custom_point_fields
)
assert custom_point_fields.get_field("point", 0) == [2.0, 6.0, 0.0]
assert custom_point_fields.get_field("control", 0) == [6.0, 12.0, 0.0]

matrix_override = MatrixPointMapperOverride()
matrix_override.matrix_point_mapper_calls = 0
matrix_override.apply_matrix([[2.0, 0.0], [0.0, 3.0]])
assert matrix_override.matrix_point_mapper_calls == 1
assert matrix_override.get_field("point", 0) == [2.0, 6.0, 0.0]

family_walker_override = MatrixFamilyWalkerOverride()
family_walker_override.matrix_family_walker_calls = 0
family_walker_override.apply_matrix([[2.0, 0.0], [0.0, 3.0]])
assert family_walker_override.matrix_family_walker_calls == 1

shared_matrix_leaf = manimlib.Square().shift(manimlib.RIGHT)
shared_matrix_family = Mobject(Mobject(shared_matrix_leaf), Mobject(shared_matrix_leaf))
shared_matrix_family.apply_matrix([[2.0, 0.0], [0.0, 1.0]])
assert np.allclose(shared_matrix_leaf.get_center(), [4.0, 0.0, 0.0])

matrix_refusal = manimlib.Square().shift([1.0, 2.0, 0.0])
matrix_refusal_points = matrix_refusal.get_points().copy()
try:
    matrix_refusal.apply_matrix(np.ones((4, 4)))
except ValueError:
    pass
else:
    raise AssertionError("an oversized matrix did not preserve NumPy refusal")
assert np.array_equal(matrix_refusal.get_points(), matrix_refusal_points)

function_mapped = manimlib.Square().shift([1.0, 2.0, 0.0])
function_width = function_mapped.get_width()
assert function_mapped.apply_function(lambda point: point + [2.0, -1.0, 0.0]) is (
    function_mapped
)
assert np.allclose(function_mapped.get_center(), [3.0, 1.0, 0.0])
assert np.isclose(function_mapped.get_width(), function_width)

position_mapped = manimlib.Square().shift([1.0, 2.0, 0.0])
position_width = position_mapped.get_width()
assert position_mapped.apply_function_to_position(lambda point: 2.0 * point) is (
    position_mapped
)
assert np.allclose(position_mapped.get_center(), [2.0, 4.0, 0.0])
assert np.isclose(position_mapped.get_width(), position_width)

positions_left = manimlib.Square().shift(manimlib.LEFT)
positions_right = manimlib.Square().shift(manimlib.RIGHT)
positions_group = Mobject(positions_left, positions_right)
assert positions_group.apply_function_to_submobject_positions(
    lambda point: point + [0.0, 2.0, 0.0]
) is positions_group
assert np.allclose(positions_left.get_center(), [-1.0, 2.0, 0.0])
assert np.allclose(positions_right.get_center(), [1.0, 2.0, 0.0])

complex_mapped = manimlib.Square().shift([1.0, 2.0, 3.0])
assert complex_mapped.apply_complex_function(lambda value: value * (1.0 + 1.0j)) is (
    complex_mapped
)
assert np.allclose(complex_mapped.get_center(), [-1.0, 3.0, 3.0])

callback_partial = Mobject(
    manimlib.Square().shift(manimlib.LEFT),
    manimlib.Square().shift(manimlib.RIGHT),
)
callback_before = [mob.get_points().copy() for mob in callback_partial.submobjects]
callback_calls = 0


def fail_on_second_member(rows):
    global callback_calls
    callback_calls += 1
    if callback_calls == 2:
        raise KeyError("point-map boom")
    return rows + manimlib.UP


try:
    callback_partial.apply_points_function(fail_on_second_member, about_edge=None)
except KeyError as error:
    assert error.args == ("point-map boom",)
else:
    raise AssertionError("a point-map callback exception did not propagate")
assert np.allclose(
    callback_partial.submobjects[0].get_points(), callback_before[0] + manimlib.UP
)
assert np.array_equal(
    callback_partial.submobjects[1].get_points(), callback_before[1]
)

smooth_probe = utility_marker(0.0, "smooth-transform")
smooth_calls = []
smooth_probe.make_smooth = lambda approx=True: smooth_calls.append(approx) or smooth_probe
assert smooth_probe.apply_function(lambda point: point, make_smooth=True) is smooth_probe
assert smooth_calls == [True]

screen_probe = manimlib.Square().shift([20.0, -20.0, 0.0])
assert screen_probe.is_off_screen()
assert screen_probe.shift_onto_screen() is screen_probe
assert not screen_probe.is_off_screen()
screen_box = screen_probe.get_bounding_box().copy()
screen_probe.shift_onto_screen()
assert np.array_equal(screen_probe.get_bounding_box(), screen_box)

boundary_probe = Mobject()
boundary_probe.resize(1)
inside_boundary = float(
    np.nextafter(np.float32(manimlib.FRAME_X_RADIUS), np.float32(-np.inf))
)
outside_boundary = float(
    np.nextafter(np.float32(manimlib.FRAME_X_RADIUS), np.float32(np.inf))
)
boundary_probe.set_field("point", 0, [inside_boundary, 0.0, 0.0])
assert not boundary_probe.is_off_screen()
boundary_probe.set_field("point", 0, [outside_boundary, 0.0, 0.0])
assert boundary_probe.is_off_screen()

oversized_screen_probe = manimlib.Rectangle(width=20.0, height=10.0)
assert oversized_screen_probe.shift_onto_screen(buff=0.25) is oversized_screen_probe
assert np.isclose(
    oversized_screen_probe.get_right()[0], manimlib.FRAME_X_RADIUS - 0.25
)
assert np.isclose(
    oversized_screen_probe.get_bottom()[1], -manimlib.FRAME_Y_RADIUS + 0.25
)

style_source = Mobject()
style_source.resize(1)
style_source.set_color("#336699").set_opacity(0.35).set_shading(0.2, 0.4, 0.6)
style_target = Mobject()
style_target.resize(1)
style_target.set_color("#ff0000").set_opacity(0.9).set_shading(0.9, 0.8, 0.7)
assert style_target.match_color(style_source) is style_target
assert style_target.get_color() == style_source.get_color()
assert np.isclose(style_target.get_opacity(), 0.9)
assert np.allclose(style_target.get_shading(), [0.9, 0.8, 0.7])
assert style_target.match_style(style_source) is style_target
assert style_target.get_color() == style_source.get_color()
assert np.isclose(style_target.get_opacity(), style_source.get_opacity())
assert np.allclose(style_target.get_shading(), style_source.get_shading())

fade_child = Mobject()
fade_child.resize(1)
fade_child.set_opacity(0.25)
fade_parent = Mobject(fade_child)
fade_parent.resize(1)
fade_parent.set_opacity(0.8, recurse=False)
assert fade_parent.fade(0.1, recurse=False) is None
assert np.isclose(fade_parent.get_opacity(), 0.9)
assert np.isclose(fade_child.get_opacity(), 0.25)
assert fade_parent.fade(0.4) is None
assert np.isclose(fade_parent.get_opacity(), 0.6)
assert np.isclose(fade_child.get_opacity(), 0.6)

vmobject_fade = utility_marker(0.0, "vmobject-fade").set_opacity(0.8)
assert vmobject_fade.fade(0.25) is vmobject_fade
assert np.isclose(vmobject_fade.get_opacity(), 0.6)

# The high-demand color helpers route both interpolation policies through
# fmn-core while preserving Reference branching and exact refusal text.
assert str(inspect.signature(Mobject.set_color_by_gradient)) == "(self, *colors)"
assert str(inspect.signature(Mobject.set_submobject_colors_by_gradient)) == (
    "(self, *colors, interp_by_hsl=False)"
)
gradient_group = Mobject(manimlib.Square(), manimlib.Square(), manimlib.Square())
assert (
    gradient_group.set_submobject_colors_by_gradient("#FF0000", "#0000FF")
    is gradient_group
)
assert [mob.get_color() for mob in gradient_group.submobjects] == [
    "#FF0000",
    "#B400B4",
    "#0000FF",
]
assert (
    gradient_group.set_submobject_colors_by_gradient(
        "#FF0000", "#0000FF", interp_by_hsl=True
    )
    is gradient_group
)
assert [mob.get_color() for mob in gradient_group.submobjects] == [
    "#FF0000",
    "#00FF00",
    "#0000FF",
]
gradient_colors_before_refusal = [
    mob.get_color() for mob in gradient_group.submobjects
]
try:
    gradient_group.set_submobject_colors_by_gradient()
except Exception as error:
    assert type(error) is Exception
    assert str(error) == "Need at least one color"
else:
    raise AssertionError("an empty submobject gradient did not refuse")
assert [mob.get_color() for mob in gradient_group.submobjects] == (
    gradient_colors_before_refusal
)
assert gradient_group.set_submobject_colors_by_gradient("#123456") is gradient_group
assert all(mob.get_color() == "#123456" for mob in gradient_group.submobjects)
empty_gradient_group = Mobject()
assert (
    empty_gradient_group.set_submobject_colors_by_gradient("#FF0000", "#0000FF")
    is empty_gradient_group
)

point_gradient = manimlib.Square()
assert point_gradient.set_color_by_gradient("#FF0000", "#0000FF") is point_gradient
point_gradient_colors = point_gradient.get_fill_colors()
assert point_gradient_colors[0] == "#FF0000"
assert point_gradient_colors[-1] == "#0000FF"

pointless_gradient = Mobject(*[manimlib.Square() for _ in range(3)])
assert pointless_gradient.set_color_by_gradient("#FF0000", "#0000FF") is pointless_gradient
assert [mob.get_color() for mob in pointless_gradient.submobjects] == [
    "#FF0000",
    "#B400B4",
    "#0000FF",
]


# Pickle restores detached state, preserves family aliases, and can rebind.
pickled_parent = Mobject()
pickled_child = Mobject()
pickled_parent.add(pickled_child)
pickled_parent.label = pickled_child
pickled_parent.resize(1)
pickled_parent.set_field("point", 0, [3.0, 4.0, 5.0])
pickled_parent.shift([2.0, 3.0, 0.0])
pickle_source_scene = Scene()
pickle_source_scene.add(pickled_parent)
payload = pickle.dumps(pickled_parent, protocol=pickle.HIGHEST_PROTOCOL)
restored = pickle.loads(payload)  # ubs:ignore -- trusted round-trip created immediately above
assert restored.label is restored.submobjects[0]
assert restored.get_field("point", 0) == [5.0, 7.0, 5.0]
assert restored.family_size() == 2
pickle_destination_scene = Scene()
pickle_destination_scene.add(restored)
assert restored.family_size() == 2
assert restored.is_alive()

# Native pickle state is a versioned, checksummed FMNA document. Decode is
# transactional: malformed bytes and unknown envelope versions cannot replace
# even a detached destination's already-live state.
pickle_state = restored._engine_state()
assert pickle_state["version"] == 1
assert pickle_state["snapshot"].startswith(b"FMNA")
unchanged = Mobject()
unchanged.resize(1)
unchanged.set_field("point", 0, [9.0, 8.0, 7.0])
malformed_state = dict(pickle_state)
malformed_state["snapshot"] = pickle_state["snapshot"][:-1]
try:
    unchanged._restore_engine_state(malformed_state)
except ValueError as error:
    assert "snapshot container refused" in str(error)
else:
    raise AssertionError("truncated native pickle state was accepted")
assert unchanged.get_field("point", 0) == [9.0, 8.0, 7.0]
unknown_version = dict(pickle_state)
unknown_version["version"] = 2
try:
    unchanged._restore_engine_state(unknown_version)
except ValueError as error:
    assert "unsupported Python portal pickle-state version 2" in str(error)
else:
    raise AssertionError("unknown native pickle-state version was accepted")
assert unchanged.get_field("point", 0) == [9.0, 8.0, 7.0]
mismatched_summary = dict(pickle_state)
mismatched_summary["fields"] = [("fabricated", 1)]
try:
    unchanged._restore_engine_state(mismatched_summary)
except ValueError as error:
    assert "field summary does not match" in str(error)
else:
    raise AssertionError("pickle state with a fabricated field summary was accepted")
assert unchanged.get_field("point", 0) == [9.0, 8.0, 7.0]


# Weakrefs, identity round trips, and explicit worker-thread confinement.
reference = weakref.ref(clone)
assert reference() is clone
identity_map = {parent: "parent", child: "child"}
assert identity_map[parent] == "parent"
assert identity_map[scene.mobjects[0]] == "parent"
del clone
gc.collect()
assert reference() is None


# Reentrant callbacks and Python exceptions cross the engine boundary intact.
updates = []


def updater(mobject, dt):
    point = mobject.get_field("point", 0)
    mobject.set_field("point", 0, [point[0] + dt, point[1], point[2]])
    updates.append(dt)


parent.add_updater(updater, call=False)
scene.update(0.5)
assert updates == [0.5]
assert parent.get_field("point", 0)[0] == 1.5

# Updater delta dispatch follows the Reference's named-``dt`` protocol, not
# callable arity.  Optional positional parameters are often semantic flags;
# passing the frame delta into one silently changes scene meaning.
semantic_default_calls = []
named_dt_calls = []
variadic_calls = []


def semantic_default_updater(mobject, vertical=False, horizontal=False):
    semantic_default_calls.append((mobject, vertical, horizontal))


def named_dt_updater(mobject, dt=0.0):
    named_dt_calls.append((mobject, dt))


def variadic_updater(mobject, *options):
    variadic_calls.append((mobject, options))


dispatch_probe = Mobject()
dispatch_scene = Scene().add(dispatch_probe)
dispatch_probe.add_updater(semantic_default_updater, call=False)
dispatch_probe.add_updater(named_dt_updater, call=False)
dispatch_probe.add_updater(variadic_updater, call=False)
dispatch_scene.update(0.25)
assert semantic_default_calls == [(dispatch_probe, False, False)]
assert named_dt_calls == [(dispatch_probe, 0.25)]
assert variadic_calls == [(dispatch_probe, ())]


# The Python updater surface shares Marionette's durable suspension flag.
# Scene traversal is child-first, a suspended parent prunes its subtree, and
# resuming a child clears the ancestor chain exactly like the Reference.
suspension_scene = Scene()
suspension_parent = Mobject()
suspension_child = Mobject()
suspension_parent.add(suspension_child)
suspension_order = []
suspension_parent.add_updater(
    lambda mob, dt: suspension_order.append(("parent", dt)), call=False
)
suspension_child.add_updater(
    lambda mob, dt: suspension_order.append(("child", dt)), call=False
)
suspension_scene.add(suspension_parent)
suspension_parent.suspend_updating(recurse=False)
suspension_scene.update(0.25)
assert suspension_order == []
suspension_child.resume_updating(recurse=False, call_updater=False)
suspension_scene.update(0.25)
assert suspension_order == [("child", 0.25), ("parent", 0.25)]
suspension_order.clear()
suspension_parent.suspend_updating()
suspension_parent.resume_updating()
assert suspension_order == [("child", 0.0), ("parent", 0.0)]
suspension_order.clear()
suspension_parent.update(0.5, recurse=False)
assert suspension_order == [("parent", 0.5)]


class ExplodingUpdater(Mobject):
    pass


exploding = ExplodingUpdater()
exploding.add_updater(
    lambda mob, dt: (_ for _ in ()).throw(KeyError("updater boom")),
    call=False,
)
scene.add(exploding)
try:
    scene.update(0.1)
except KeyError as error:
    assert "updater boom" in str(error)
else:
    raise AssertionError("Python updater exception did not propagate")
scene.remove(exploding)


class InterpolatingMobject(Mobject):
    def __init__(self, value):
        self.alphas = []
        super().__init__()
        self.resize(1)
        self.set_field("point", 0, [value, 0.0, 0.0])

    def interpolate(self, start, target, alpha):
        self.alphas.append(alpha)
        return super().interpolate(start, target, alpha)


interpolating = InterpolatingMobject(0.0)
interpolation_target = InterpolatingMobject(3.0)
scene.add(interpolating, interpolation_target)
scene.run_transform(interpolating, interpolation_target, 2)
assert interpolating.alphas == [0.0, 0.5, 1.0]
assert interpolating.get_field("point", 0) == [3.0, 0.0, 0.0]


class LifecycleAnimation(Animation):
    def __init__(self):
        super().__init__()
        self.calls = []

    def begin(self):
        self.calls.append("begin")

    def interpolate_mobject(self, alpha):
        self.calls.append(("interpolate_mobject", alpha))

    def finish(self):
        self.calls.append("finish")


lifecycle_animation = LifecycleAnimation()
lifecycle_animation.begin()
lifecycle_animation.interpolate(0.25)
lifecycle_animation.finish()
assert lifecycle_animation.calls == [
    "begin",
    ("interpolate_mobject", 0.25),
    "finish",
]


class ExplodingInit(Mobject):
    def init_points(self):
        raise ValueError("init boom")


try:
    ExplodingInit()
except ValueError as error:
    assert "init boom" in str(error)
else:
    raise AssertionError("Python lifecycle exception did not propagate")


class LifecycleScene(Scene):
    def __init__(self):
        super().__init__()
        self.calls = []

    def setup(self):
        self.calls.append("setup")

    def construct(self):
        self.calls.append("construct")

    def tear_down(self):
        self.calls.append("tear_down")


lifecycle_scene = LifecycleScene()
lifecycle_scene.run()
assert lifecycle_scene.calls == ["setup", "construct", "tear_down"]


class FailingLifecycleScene(LifecycleScene):
    def construct(self):
        super().construct()
        raise RuntimeError("construct boom")


failing_lifecycle_scene = FailingLifecycleScene()
try:
    failing_lifecycle_scene.run()
except RuntimeError as error:
    assert "construct boom" in str(error)
else:
    raise AssertionError("Scene.construct exception did not propagate")
assert failing_lifecycle_scene.calls == ["setup", "construct", "tear_down"]

# Scene.add_sound is a real native request boundary, not a schema placeholder.
# The call-site time stays on Choreo's exact rational grid while the relative
# offset remains separate for Reel's eventual sample-grid conversion (BN-14).
sound_scene = Scene()
assert sound_scene.add_sound("click.wav", -0.125, -6.0, -12.0) is None
assert sound_scene._sound_request_facts() == [
    ("click.wav", 0, 30, -0.125, -6.0, -12.0)
]
sound_scene.wait(0.25)
assert sound_scene.add_sound("tone.wav", gain=-3.0) is None
assert sound_scene._sound_request_facts()[1] == (
    "tone.wav",
    8,
    30,
    0.0,
    -3.0,
    None,
)
sound_requests_before_refusal = sound_scene._sound_request_facts()
try:
    sound_scene.add_sound("bad.wav", gain_to_background=-math.inf)
except ValueError as error:
    assert str(error) == (
        "invalid scene configuration: sound gain_to_background must be finite"
    )
else:
    raise AssertionError("a non-finite sound gain reached the native request list")
assert sound_scene._sound_request_facts() == sound_requests_before_refusal

interactive_scene = InteractiveScene()
assert isinstance(interactive_scene.checkpoint_paste(), bytes)


# The schema-generated import topology and exact-name aliases are present.
geometry = importlib.import_module("manimlib.mobject.geometry")
circle = geometry.Circle()
assert isinstance(circle, VMobject)

# Atlas already owns these curve, corner, and camera-frame primitives. Their
# public classes must drive those native builders rather than stop at the
# schema-generated constructor refusal which previously occupied each row.
frame_mobjects = importlib.import_module("manimlib.mobject.frame")
assert geometry.CubicBezier.__bases__ == (VMobject,)
assert geometry.Elbow.__bases__ == (VMobject,)
assert frame_mobjects.ScreenRectangle.__bases__ == (geometry.Rectangle,)
assert frame_mobjects.FullScreenRectangle.__bases__ == (
    frame_mobjects.ScreenRectangle,
)
assert frame_mobjects.FullScreenFadeRectangle.__bases__ == (
    frame_mobjects.FullScreenRectangle,
)
assert str(inspect.signature(geometry.CubicBezier)) == (
    "(a0, h0, h1, a1, **kwargs)"
)
assert str(inspect.signature(geometry.Elbow)) == (
    "(width=0.2, angle=0, **kwargs)"
)
assert str(inspect.signature(frame_mobjects.ScreenRectangle)) == (
    "(aspect_ratio=1.7777777777777777, height=4, **kwargs)"
)
assert str(inspect.signature(frame_mobjects.FullScreenRectangle)) == (
    "(height=8.0, fill_color='#222222', fill_opacity=1, "
    "stroke_width=0, **kwargs)"
)
assert str(inspect.signature(frame_mobjects.FullScreenFadeRectangle)) == (
    "(stroke_width=0.0, fill_color='#000000', fill_opacity=0.7, **kwargs)"
)

native_cubic = geometry.CubicBezier(
    (-2.0, 0.0, 0.0),
    (-1.0, 2.0, 0.0),
    (1.0, 2.0, 0.0),
    (2.0, 0.0, 0.0),
    stroke_color=manimlib.BLUE,
    stroke_width=2.5,
)
assert np.allclose(native_cubic.get_points()[0], [-2.0, 0.0, 0.0])
assert np.allclose(native_cubic.get_points()[-1], [2.0, 0.0, 0.0])
assert native_cubic.get_stroke_color() == manimlib.BLUE
assert np.isclose(native_cubic.get_stroke_width(), 2.5)

failed_cubic = geometry.CubicBezier.__new__(geometry.CubicBezier)
try:
    geometry.CubicBezier.__init__(
        failed_cubic,
        (0.0, 0.0, 0.0),
        (1.0, math.nan, 0.0),
        (2.0, 1.0, 0.0),
        (3.0, 0.0, 0.0),
    )
except ValueError as error:
    assert "tolerance" in str(error).lower()
else:
    raise AssertionError("CubicBezier published a non-finite native path")
assert failed_cubic.get_num_points() == 0

native_elbow = geometry.Elbow(width=2.0, angle=math.pi / 2, color=manimlib.RED)
assert np.isclose(native_elbow.get_height(), 2.0)
assert native_elbow.get_stroke_color() == manimlib.RED

screen_rectangle = frame_mobjects.ScreenRectangle()
assert np.isclose(screen_rectangle.get_width(), 4.0 * 16.0 / 9.0)
assert np.isclose(screen_rectangle.get_height(), 4.0)

full_screen_rectangle = frame_mobjects.FullScreenRectangle()
assert np.isclose(full_screen_rectangle.get_width(), manimlib.FRAME_WIDTH)
assert np.isclose(full_screen_rectangle.get_height(), manimlib.FRAME_HEIGHT)
assert full_screen_rectangle.get_fill_color() == manimlib.GREY_E
assert np.isclose(full_screen_rectangle.get_fill_opacity(), 1.0)
assert np.isclose(full_screen_rectangle.get_stroke_width(), 0.0)

custom_full_screen = frame_mobjects.FullScreenRectangle(
    height=3.0,
    fill_color=manimlib.BLUE,
    fill_opacity=0.25,
    stroke_width=1.5,
    stroke_color=manimlib.RED,
)
assert np.isclose(custom_full_screen.get_width(), 3.0 * 16.0 / 9.0)
assert np.isclose(custom_full_screen.get_height(), 3.0)
assert custom_full_screen.get_fill_color() == manimlib.BLUE
assert np.isclose(custom_full_screen.get_fill_opacity(), 0.25)
assert custom_full_screen.get_stroke_color() == manimlib.RED
assert np.isclose(custom_full_screen.get_stroke_width(), 1.5)

fade_rectangle = frame_mobjects.FullScreenFadeRectangle(height=2.0)
assert np.isclose(fade_rectangle.get_width(), manimlib.FRAME_WIDTH)
assert np.isclose(fade_rectangle.get_height(), manimlib.FRAME_HEIGHT)
assert fade_rectangle.get_fill_color() == manimlib.BLACK
assert np.isclose(fade_rectangle.get_fill_opacity(), 0.7)
assert np.isclose(fade_rectangle.get_stroke_width(), 0.0)

native_shelf_scene = Scene()
native_shelf_scene.add(
    native_cubic,
    native_elbow,
    screen_rectangle,
    full_screen_rectangle,
    custom_full_screen,
    fade_rectangle,
)
assert all(mobject._is_bound() for mobject in native_shelf_scene.mobjects)

# Shape matchers consume the real family bounding box and retain the native
# empty-target rule.  A generated schema shell used to fail here before any
# geometry reached Atlas.
shape_matchers = importlib.import_module("manimlib.mobject.shape_matchers")
assert str(inspect.signature(shape_matchers.SurroundingRectangle)) == (
    "(mobject, buff=0.1, color='#FFFF00', **kwargs)"
)
assert str(inspect.signature(shape_matchers.SurroundingRectangle.set_buff)) == (
    "(self, buff)"
)
assert str(inspect.signature(shape_matchers.SurroundingRectangle.surround)) == (
    "(self, mobject, buff=None)"
)
assert str(inspect.signature(shape_matchers.BackgroundRectangle)) == (
    "(mobject, color=None, stroke_width=0, stroke_opacity=0, "
    "fill_opacity=0.75, buff=0, **kwargs)"
)
assert str(inspect.signature(shape_matchers.BackgroundRectangle.set_style)) == (
    "(self, stroke_color=None, stroke_width=None, fill_color=None, "
    "fill_opacity=None, family=True, **kwargs)"
)
left_box = geometry.Rectangle(width=2.0, height=1.0).shift([-1.5, 0.5, 0.0])
right_box = geometry.Rectangle(width=1.0, height=2.0).shift([2.0, -0.5, 0.0])
box_group = manimlib.VGroup(left_box, right_box)
surround = shape_matchers.SurroundingRectangle(
    box_group, buff=0.25, color=manimlib.BLUE, stroke_width=2.0
)
target_box = box_group.get_bounding_box()
surround_box = surround.get_bounding_box()
assert np.allclose(surround_box[0][:2], target_box[0][:2] - 0.25)
assert np.allclose(surround_box[2][:2], target_box[2][:2] + 0.25)
assert surround.get_stroke_color() == manimlib.BLUE
assert np.allclose(surround.get_stroke_width(), 2.0)
surround.set_buff(0.5)
assert np.allclose(surround.get_bounding_box()[0][:2], target_box[0][:2] - 0.5)
empty_surround = shape_matchers.SurroundingRectangle(Mobject())
assert not empty_surround.has_points()
assert isinstance(surround, geometry.Rectangle)

# BackgroundRectangle uses the same Atlas extent matcher, the pinned camera
# background default, and the Reference's deliberately locked public style.
background_target = geometry.Rectangle(width=2.0, height=1.0)
background_target_box = background_target.get_bounding_box().copy()
background = shape_matchers.BackgroundRectangle(
    background_target, buff=0.2, fill_opacity=0.6
)
assert np.allclose(
    background.get_bounding_box()[0][:2], background_target_box[0][:2] - 0.2
)
assert np.allclose(
    background.get_bounding_box()[2][:2], background_target_box[2][:2] + 0.2
)
assert background.get_fill_color() == "#333333"
assert VMobject.get_fill_color(background) == "#333333"
assert np.isclose(background.get_fill_opacity(), 0.6)
assert np.isclose(background.get_stroke_width(), 0.0)
assert np.isclose(background.get_stroke_opacity(), 0.0)
assert background.pointwise_become_partial(background_target, 0.2, 0.5) is background
assert np.isclose(background.get_fill_opacity(), 0.3)
assert (
    background.set_style(
        stroke_color="#FF0000",
        stroke_width=9,
        fill_color="#00FF00",
        fill_opacity=0.25,
        family=False,
        ignored_reference_kwarg=True,
    )
    is background
)
assert background.get_fill_color() == "#333333"
assert VMobject.get_fill_color(background) == "#000000"
assert background.get_stroke_color() == "#000000"
assert np.isclose(background.get_stroke_width(), 0.0)
assert np.isclose(background.get_fill_opacity(), 0.25)

detached_plate_target = geometry.Rectangle(width=1.0, height=0.5)
detached_plate_box = detached_plate_target.get_bounding_box().copy()
assert (
    detached_plate_target.add_background_rectangle(
        color="#123456", opacity=0.8, buff=0.15
    )
    is detached_plate_target
)
detached_plate = detached_plate_target.background_rectangle
assert isinstance(detached_plate, shape_matchers.BackgroundRectangle)
assert detached_plate_target.submobjects[0] is detached_plate
assert detached_plate.get_fill_color() == "#123456"
assert np.isclose(detached_plate.get_fill_opacity(), 0.8)
assert np.allclose(
    detached_plate.get_bounding_box()[0][:2], detached_plate_box[0][:2] - 0.15
)

bound_plate_scene = Scene()
bound_plate_target = geometry.Rectangle(width=1.5, height=0.75)
bound_plate_scene.add(bound_plate_target)
assert bound_plate_target.add_background_rectangle(buff=0.1) is bound_plate_target
assert bound_plate_target.background_rectangle._is_bound()
assert bound_plate_target.submobjects[0] is bound_plate_target.background_rectangle
assert bound_plate_scene.get_mobjects()[0] is bound_plate_target

immediate_a = geometry.Square(side_length=0.5)
immediate_b = geometry.Square(side_length=0.75)
immediate_group = Mobject(immediate_a, immediate_b)
bound_plate_scene.add(immediate_group)
assert immediate_group.add_background_rectangle_to_submobjects(opacity=0.4) is immediate_group
for immediate in (immediate_a, immediate_b):
    assert immediate.submobjects[0] is immediate.background_rectangle
    assert immediate.background_rectangle._is_bound()
    assert np.isclose(immediate.background_rectangle.get_fill_opacity(), 0.4)

family_point_a = geometry.Square(side_length=0.5)
family_point_b = geometry.Square(side_length=0.75)
family_point_container = Mobject(family_point_b)
family_plate_root = Mobject(family_point_a, family_point_container)
bound_plate_scene.add(family_plate_root)
assert (
    family_plate_root.add_background_rectangle_to_family_members_with_points(
        opacity=0.35
    )
    is family_plate_root
)
assert not hasattr(family_plate_root, "background_rectangle")
assert not hasattr(family_point_container, "background_rectangle")
for point_member in (family_point_a, family_point_b):
    assert point_member.submobjects[0] is point_member.background_rectangle
    assert point_member.background_rectangle._is_bound()
    assert np.isclose(point_member.background_rectangle.get_fill_opacity(), 0.35)

# Retargeting after scene entry rebuilds the same arena object through the
# native matcher. Identity and style survive, and bad targets refuse before
# touching the live geometry.
matcher_scene = Scene()
matcher_scene.add(box_group, surround)
surround_identity = id(surround)
surround_before_refusal = surround.get_points().copy()
try:
    surround.surround([0.0, 0.0, 0.0])
except TypeError as error:
    assert str(error) == "SurroundingRectangle.surround expects a Mobject"
else:
    raise AssertionError("a non-Mobject surrounding target was accepted")
assert np.array_equal(surround.get_points(), surround_before_refusal)

right_box.shift([1.0, 1.5, 0.0])
live_target_box = box_group.get_bounding_box()
assert surround.surround(box_group, buff=0.75) is surround
assert id(surround) == surround_identity
assert np.allclose(
    surround.get_bounding_box()[0][:2], live_target_box[0][:2] - 0.75
)
assert np.allclose(
    surround.get_bounding_box()[2][:2], live_target_box[2][:2] + 0.75
)
assert surround.get_stroke_color() == manimlib.BLUE
assert np.allclose(surround.get_stroke_width(), 2.0)
surround.set_buff(0.1)
assert np.allclose(
    surround.get_bounding_box()[0][:2], live_target_box[0][:2] - 0.1
)

fixed_match_target = geometry.Rectangle(width=1.0, height=0.5).fix_in_frame()
fixed_surround = shape_matchers.SurroundingRectangle(fixed_match_target)
assert fixed_match_target.is_fixed_in_frame()
assert fixed_surround.is_fixed_in_frame()

bound_empty_target = Mobject()
matcher_scene.add(bound_empty_target, empty_surround)
empty_surround.surround(bound_empty_target)
assert not empty_surround.has_points()
empty_surround.surround(box_group, buff=0.2)
assert empty_surround.has_points()
assert np.allclose(
    empty_surround.get_bounding_box()[2][:2], live_target_box[2][:2] + 0.2
)

# The rest of Atlas's matcher shelf is routed through the same authoritative
# family-extent crossing. Cross and Underline retain their Reference MRO and
# tapered record profiles; the Python layer does not reconstruct either path.
cross_call_shape = str(inspect.signature(shape_matchers.Cross))
assert cross_call_shape == (
    "(mobject, stroke_color='#FC6255', stroke_width=[0, 6, 0], **kwargs)"
), cross_call_shape
underline_call_shape = str(inspect.signature(shape_matchers.Underline))
assert underline_call_shape == (
    "(mobject, buff=0.1, stroke_color='#FFFFFF', "
    "stroke_width=[0, 3, 3, 0], stretch_factor=1.2, **kwargs)"
), underline_call_shape
assert shape_matchers.Cross.__bases__ == (manimlib.VGroup,)
assert shape_matchers.Underline.__bases__ == (geometry.Line,)

native_match_target = manimlib.VGroup(
    geometry.Rectangle(width=2.0, height=1.0).shift([-1.0, 0.25, 0.0]),
    geometry.Rectangle(width=1.0, height=2.0).shift([1.75, -0.5, 0.0]),
)
native_match_box = native_match_target.get_bounding_box().copy()
native_cross = shape_matchers.Cross(native_match_target)
assert len(native_cross.submobjects) == 2, len(native_cross.submobjects)
assert np.allclose(
    native_cross.get_bounding_box()[[0, 2], :2],
    native_match_box[[0, 2], :2],
)
for arm in native_cross.submobjects:
    widths = arm.get_stroke_widths()
    assert len(widths) > 3
    assert np.isclose(widths[0], 0.0)
    assert np.isclose(widths[-1], 0.0)
    assert np.isclose(widths.max(), 6.0)
    assert arm.get_stroke_color() == manimlib.RED

scalar_cross = shape_matchers.Cross(
    native_match_target, stroke_color=manimlib.BLUE, stroke_width=2.5
)
for arm in scalar_cross.submobjects:
    assert np.allclose(arm.get_stroke_widths(), 2.5)
    assert arm.get_stroke_color() == manimlib.BLUE

native_underline = shape_matchers.Underline(native_match_target, buff=0.2)
underline_box = native_underline.get_bounding_box()
target_width = native_match_box[2, 0] - native_match_box[0, 0]
assert np.isclose(underline_box[2, 0] - underline_box[0, 0], 1.2 * target_width)
assert np.isclose(underline_box[0, 1], native_match_box[0, 1] - 0.2)
assert np.isclose(underline_box[2, 1], native_match_box[0, 1] - 0.2)
underline_widths = native_underline.get_stroke_widths()
assert np.isclose(underline_widths[0], 0.0)
assert np.isclose(underline_widths[-1], 0.0)
assert np.isclose(underline_widths.max(), 3.0)

scalar_underline = shape_matchers.Underline(
    native_match_target,
    stroke_color=manimlib.GREEN,
    stroke_width=1.75,
    stretch_factor=0.8,
)
assert np.allclose(scalar_underline.get_stroke_widths(), 1.75)
assert scalar_underline.get_stroke_color() == manimlib.GREEN
assert np.isclose(scalar_underline.get_width(), 0.8 * target_width)

empty_cross = shape_matchers.Cross(Mobject())
empty_underline = shape_matchers.Underline(Mobject())
assert not empty_cross.family_members_with_points()
assert not empty_underline.family_members_with_points()

bound_matcher_scene = Scene()
bound_match_target = geometry.Rectangle(width=1.5, height=0.75)
bound_matcher_scene.add(bound_match_target)
bound_cross = shape_matchers.Cross(bound_match_target)
bound_underline = shape_matchers.Underline(bound_match_target)
bound_matcher_scene.add(bound_cross, bound_underline)
assert bound_cross._is_bound()
assert all(arm._is_bound() for arm in bound_cross.submobjects)
assert bound_underline._is_bound()

for constructor, message in (
    (shape_matchers.Cross, "Cross expects a Mobject"),
    (shape_matchers.Underline, "Underline expects a Mobject"),
):
    try:
        constructor([0.0, 0.0, 0.0])
    except TypeError as error:
        assert str(error) == message
    else:
        raise AssertionError(f"{constructor.__name__} accepted a non-Mobject target")

for name, kwargs in (
    ("buff", {"buff": float("nan")}),
    ("stretch_factor", {"stretch_factor": float("inf")}),
):
    try:
        shape_matchers.Underline(native_match_target, **kwargs)
    except ValueError as error:
        assert name in str(error)
        assert "finite" in str(error)
    else:
        raise AssertionError(f"Underline accepted a non-finite {name}")

try:
    shape_matchers.Underline(native_match_target, path_arc=0.5)
except NotImplementedError as error:
    assert "path_arc" in str(error)
else:
    raise AssertionError("Underline silently ignored a curved path request")

# Checkmark and Exmark keep the Reference preset-string inheritance and one
# selectable glyph-family shape, but their visible contours are Atlas's
# paired BN-08 paths rather than a hidden LaTeX/pifont subprocess.
drawings = importlib.import_module("manimlib.mobject.svg.drawings")
special_tex = importlib.import_module("manimlib.mobject.svg.special_tex")
assert str(inspect.signature(special_tex.TexTextFromPresetString)) == ("(**kwargs)")
assert str(inspect.signature(drawings.Checkmark)) == ("(**kwargs)")
assert str(inspect.signature(drawings.Exmark)) == ("(**kwargs)")
assert drawings.Checkmark.__bases__ == (special_tex.TexTextFromPresetString,)
assert drawings.Exmark.__bases__ == (special_tex.TexTextFromPresetString,)
assert issubclass(special_tex.TexTextFromPresetString, manimlib.TexText)
assert special_tex.TexTextFromPresetString.tex == ""
assert special_tex.TexTextFromPresetString.default_color == manimlib.DEFAULT_MOBJECT_COLOR
assert drawings.Checkmark.tex == r"\ding{51}"
assert drawings.Checkmark.default_color == manimlib.GREEN
assert drawings.Exmark.tex == r"\ding{55}"
assert drawings.Exmark.default_color == manimlib.RED


class NativeTexTemplateProbe(special_tex.TexTextFromPresetString):
    tex = "x"
    default_color = manimlib.BLUE


native_tex_template_probe = NativeTexTemplateProbe()
assert native_tex_template_probe.get_tex() == "x"
assert native_tex_template_probe.get_fill_color() == manimlib.BLUE

checkmark = drawings.Checkmark()
exmark = drawings.Exmark()
assert checkmark.get_tex() == r"\ding{51}"
assert exmark.get_tex() == r"\ding{55}"
assert len(checkmark.submobjects) == 1
assert len(exmark.submobjects) == 1
assert checkmark.get_part_by_tex(checkmark.tex)[0] is checkmark[0]
assert exmark.get_part_by_tex(exmark.tex)[0] is exmark[0]
assert np.allclose(checkmark.get_bounding_box()[[0, 2], :2], [[-0.5, -0.5], [0.5, 0.5]])
assert np.allclose(exmark.get_bounding_box()[[0, 2], :2], [[-0.5, -0.5], [0.5, 0.5]])
assert checkmark[0].get_fill_color() == manimlib.GREEN
assert exmark[0].get_fill_color() == manimlib.RED
assert np.isclose(checkmark[0].get_stroke_width(), 0.0)
assert np.isclose(exmark[0].get_stroke_width(), 0.0)

large_checkmark = drawings.Checkmark(font_size=96, color=manimlib.BLUE)
assert np.isclose(large_checkmark.get_width(), 2.0)
assert np.isclose(large_checkmark.get_height(), 2.0)
assert large_checkmark[0].get_fill_color() == manimlib.BLUE

bound_matcher_scene.add(checkmark, exmark)
assert checkmark._is_bound() and checkmark[0]._is_bound()
assert exmark._is_bound() and exmark[0]._is_bound()

for constructor in (drawings.Checkmark, drawings.Exmark):
    try:
        constructor(font_size=0)
    except ValueError as error:
        assert "positive and finite" in str(error)
    else:
        raise AssertionError(f"{constructor.__name__} accepted zero font_size")

try:
    drawings.Checkmark(template="external-template")
except NotImplementedError as error:
    assert "template" in str(error)
else:
    raise AssertionError("Checkmark silently accepted an external TeX template")

# ValueTracker targets are native typed state, not record-buffer decoration.
# Detached copy/deepcopy/pickle must preserve that payload so the ordinary
# `.animate` builder can mutate its generated target before Scene adoption.
assert manimlib.ValueTracker.value_type is np.float64
assert manimlib.ComplexValueTracker.value_type is np.complex128
assert manimlib.ExponentialValueTracker.__bases__ == (manimlib.ValueTracker,)
assert manimlib.ComplexValueTracker.__bases__ == (manimlib.ValueTracker,)
tracker_cases = (
    (manimlib.ValueTracker(1.25), 1.25),
    (manimlib.ExponentialValueTracker(4.0), 4.0),
    (manimlib.ComplexValueTracker(1.0 - 2.0j), 1.0 - 2.0j),
)
for tracker, expected in tracker_cases:
    assert isinstance(tracker.get_value(), tracker.value_type)
    for clone in (
        copy.copy(tracker),
        copy.deepcopy(tracker),
        pickle.loads(  # ubs:ignore -- trusted round-trip created immediately here
            pickle.dumps(tracker, protocol=pickle.HIGHEST_PROTOCOL)
        ),
    ):
        assert type(clone) is type(tracker)
        assert clone.get_value() == expected
        assert isinstance(clone.get_value(), clone.value_type)
        clone.increment_value(0.5)
        assert clone.get_value() == expected + 0.5
        assert tracker.get_value() == expected

plain_tracker_scene = Scene()
plain_tracker = manimlib.ValueTracker(1.0)
plain_tracker_values = []
plain_tracker.add_updater(
    lambda mob: plain_tracker_values.append(float(mob.get_value())),
    call=False,
)
plain_tracker_scene.play(
    plain_tracker.animate.set_value(5.0),
    run_time=2.0 / 30.0,
    rate_func=manimlib.linear,
)
assert np.allclose(plain_tracker_values, [3.0, 5.0, 5.0])
assert plain_tracker.get_value() == 5.0

exponential_tracker_scene = Scene()
exponential_tracker = manimlib.ExponentialValueTracker(4.0)
exponential_tracker_values = []
exponential_tracker.add_updater(
    lambda mob: exponential_tracker_values.append(float(mob.get_value())),
    call=False,
)
exponential_tracker_scene.play(
    exponential_tracker.animate.set_value(16.0),
    run_time=2.0 / 30.0,
    rate_func=manimlib.linear,
)
assert np.allclose(exponential_tracker_values, [8.0, 16.0, 16.0])
assert math.isclose(
    exponential_tracker.get_value(), 16.0, rel_tol=0.0, abs_tol=1e-12
)

complex_tracker_scene = Scene()
complex_tracker = manimlib.ComplexValueTracker(1.0 - 2.0j)
complex_tracker_values = []
complex_tracker.add_updater(
    lambda mob: complex_tracker_values.append(complex(mob.get_value())),
    call=False,
)
complex_tracker_scene.play(
    complex_tracker.animate.set_value(5.0 + 6.0j),
    run_time=2.0 / 30.0,
    rate_func=manimlib.linear,
)
assert np.allclose(
    complex_tracker_values,
    [3.0 + 2.0j, 5.0 + 6.0j, 5.0 + 6.0j],
)
assert complex_tracker.get_value() == 5.0 + 6.0j

# Scene.finish_animations runs one final zero-dt updater traversal.  The
# display root intentionally precedes the derived root, so the ordinary frame
# leaves it one update behind after the source lands on its endpoint.  The
# finish pass must close that dependency in Reference root order before play
# returns (and before a final-state PNG is captured).
finish_scene = Scene()
finish_source = manimlib.ValueTracker(1.0)
finish_derived = manimlib.ValueTracker(1.0)
finish_display = manimlib.ValueTracker(-1.0)
finish_order = []


def update_finish_display(mob, dt):
    finish_order.append(("display", dt, float(finish_derived.get_value())))
    mob.set_value(finish_derived.get_value())


def update_finish_derived(mob, dt):
    finish_order.append(("derived", dt, float(finish_source.get_value())))
    mob.set_value(finish_source.get_value())


finish_display.add_updater(update_finish_display, call=False)
finish_derived.add_updater(update_finish_derived, call=False)
finish_scene.add(finish_display, finish_derived, finish_source)
finish_scene.play(
    finish_source.animate.set_value(7.0),
    run_time=1.0 / 30.0,
    rate_func=manimlib.linear,
)
assert finish_order == [
    ("display", 1.0 / 30.0, 1.0),
    ("derived", 1.0 / 30.0, 7.0),
    ("display", 0.0, 7.0),
    ("derived", 0.0, 7.0),
]
assert finish_display.get_value() == 7.0
assert finish_derived.get_value() == 7.0

# Engine-driven plays release the Scene RefCell after interpolation and the
# rational clock advance, then invoke Python scene updaters before native
# scene updaters and immutable capture. This used to refuse every such play.
release_scene = Scene()
release_mover = geometry.Rectangle(width=1.0, height=1.0)
release_observations = []
release_mover.add_updater(
    lambda mob, dt: release_observations.append(
        (dt, release_scene.time(), mob.get_x())
    ),
    call=False,
)
release_scene.add(release_mover)
release_scene.play(
    release_mover.animate.shift(manimlib.RIGHT),
    run_time=1.0 / 30.0,
    rate_func=manimlib.linear,
)
assert len(release_observations) == 2
assert np.allclose(release_observations[0], [1.0 / 30.0, 1.0 / 30.0, 1.0])
assert np.allclose(release_observations[1], [0.0, 1.0 / 30.0, 1.0])
release_observations.clear()
release_scene.wait(1.0 / 30.0)
assert len(release_observations) == 2
assert np.allclose([row[0] for row in release_observations], [0.0, 1.0 / 30.0])
assert np.allclose([row[1] for row in release_observations], [1.0 / 30.0, 2.0 / 30.0])

update_animation = importlib.import_module("manimlib.animation.update")
callback_scene = Scene()
callback_mover = geometry.Rectangle(width=1.0, height=1.0)
callback_alphas = []
callback_animation = update_animation.UpdateFromAlphaFunc(
    callback_mover,
    lambda mob, alpha: (
        callback_alphas.append(alpha),
        mob.set_x(alpha),
    )[-1],
    run_time=1.0 / 30.0,
    rate_func=manimlib.linear,
)
callback_scene.play(callback_animation)
assert np.allclose(callback_alphas, [0.0, 1.0, 1.0])
assert np.allclose(callback_mover.get_x(), 1.0)

# Restore is a Transform onto Marionette's saved-state copy, so its standard
# path-arc parameters route through the same native path function.
restore_scene = Scene()
restore_mover = geometry.Rectangle(width=1.0, height=1.0)
restore_scene.add(restore_mover)
restore_mover.save_state()
assert restore_mover.saved_state is not None
assert restore_mover.saved_state.target is restore_mover.target
restore_mover.shift([2.0, 1.0, 0.0])
restore_scene.play(
    manimlib.Restore(restore_mover, path_arc=math.pi / 3.0),
    run_time=1.0 / 30.0,
    rate_func=manimlib.linear,
)
assert np.allclose(restore_mover.get_center(), [0.0, 0.0, 0.0])

# A saved state is a real family copy even before scene adoption. The first
# animation may adopt the already-mutated source; Restore must still target
# the earlier detached state, including its child graph.
detached_restore_scene = Scene()
detached_restore_child = geometry.Rectangle(width=0.5, height=0.5)
detached_restore_mover = manimlib.VGroup(detached_restore_child)
detached_restore_mover.save_state()
detached_saved = detached_restore_mover.saved_state
detached_restore_mover.shift([3.0, -2.0, 0.0])
detached_restore_scene.play(
    manimlib.Restore(detached_restore_mover, path_arc=math.pi / 6.0),
    run_time=1.0 / 30.0,
    rate_func=manimlib.linear,
)
assert detached_restore_mover.saved_state is detached_saved
assert np.allclose(detached_restore_mover.get_center(), [0.0, 0.0, 0.0])
assert np.allclose(detached_restore_child.get_center(), [0.0, 0.0, 0.0])

state_links = geometry.Rectangle(width=0.25, height=0.25)
first_target = state_links.generate_target()
state_links.save_state()
assert state_links.saved_state.target is first_target
second_target = state_links.generate_target()
assert second_target.saved_state is state_links.saved_state
plain_state_copy = state_links.copy()
assert plain_state_copy.target is None
assert plain_state_copy.saved_state is None

explicit_target_scene = Scene()
explicit_target_mover = geometry.Rectangle(width=0.5, height=0.5)
explicit_target_scene.add(explicit_target_mover)
explicit_target_mover.generate_target().shift([1.25, 0.0, 0.0])
explicit_target_scene.play(
    manimlib.MoveToTarget(explicit_target_mover),
    run_time=1.0 / 30.0,
    rate_func=manimlib.linear,
)
assert np.allclose(explicit_target_mover.get_center(), [1.25, 0.0, 0.0])

# ApplyMethod is the Reference's method-to-target adapter over Transform.  It
# must keep the bound method and its arguments as public state, build a fresh
# independent target at play time, and route that target through the native
# transform mechanism rather than treating the Python method as a Mobject.
assert manimlib.ApplyMethod.__bases__ == (manimlib.Transform,)
assert str(inspect.signature(manimlib.ApplyMethod)) == "(method, *args, **kwargs)"
assert str(inspect.signature(manimlib.ApplyMethod.__init__)) == (
    "(self, method, *args, **kwargs)"
)
assert str(inspect.signature(manimlib.ApplyMethod.check_validity_of_input)) == (
    "(self, method)"
)
assert str(inspect.signature(manimlib.ApplyMethod.create_target)) == "(self)"

apply_target_source = geometry.Rectangle(width=1.0, height=0.5)
apply_scale = manimlib.ApplyMethod(
    apply_target_source.scale,
    2.0,
    {"about_point": manimlib.RIGHT},
    path_arc=math.pi / 6.0,
)
assert apply_scale.method.__self__ is apply_target_source
assert apply_scale.method.__func__ is type(apply_target_source).scale
assert apply_scale.method_args[0] == 2.0
assert list(apply_scale.method_args[1]) == ["about_point"]
assert np.allclose(apply_scale.method_args[1]["about_point"], manimlib.RIGHT)
assert apply_scale.target_mobject is None
apply_scale_target = apply_scale.create_target()
assert apply_scale_target is not apply_target_source
assert np.allclose(apply_target_source.get_center(), manimlib.ORIGIN)
assert np.allclose(apply_scale_target.get_center(), manimlib.LEFT)
assert np.isclose(apply_scale_target.get_width(), 2.0)

delayed_apply_scene = Scene()
delayed_apply_source = geometry.Rectangle(width=1.0, height=0.5)
delayed_apply = manimlib.ApplyMethod(delayed_apply_source.shift, manimlib.RIGHT)
delayed_apply_source.shift(manimlib.UP)
delayed_apply_samples = []
delayed_apply_source.add_updater(
    lambda mob: delayed_apply_samples.append(mob.get_center().copy()),
    call=False,
)
delayed_apply_scene.play(
    delayed_apply,
    run_time=2.0 / 30.0,
    rate_func=manimlib.linear,
)
assert np.allclose(
    delayed_apply_samples,
    [[0.5, 1.0, 0.0], [1.0, 1.0, 0.0], [1.0, 1.0, 0.0]],
)
assert np.allclose(delayed_apply_source.get_center(), [1.0, 1.0, 0.0])
assert delayed_apply.target_mobject is not None
assert delayed_apply.target_mobject is not delayed_apply_source

apply_tracker_scene = Scene()
apply_tracker = manimlib.ValueTracker(1.0)
apply_tracker_samples = []
apply_tracker.add_updater(
    lambda mob: apply_tracker_samples.append(float(mob.get_value())),
    call=False,
)
apply_tracker_scene.play(
    manimlib.ApplyMethod(apply_tracker.set_value, 5.0),
    run_time=2.0 / 30.0,
    rate_func=manimlib.linear,
)
assert np.allclose(apply_tracker_samples, [3.0, 5.0, 5.0])
assert apply_tracker.get_value() == 5.0

try:
    manimlib.ApplyMethod(lambda: None)
except Exception as error:
    assert str(error) == (
        "Whoops, looks like you accidentally invoked the method you want to animate"
    )
else:
    raise AssertionError("ApplyMethod accepted a free function")


class ForeignApplyTarget:
    def mutate(self):
        return self


try:
    manimlib.ApplyMethod(ForeignApplyTarget().mutate)
except AssertionError:
    pass
else:
    raise AssertionError("ApplyMethod accepted a bound non-Mobject method")

# The thin transform adapters are authored over the same play-time target
# seam.  Their documented argument order must not fall through to inherited
# ApplyMethod/Transform constructors, and each adapter must preserve the
# Reference target operation while Lumen's native Transform owns playback.
adapter_call_shapes = {
    manimlib.ApplyPointwiseFunction: "(function, mobject, run_time=3.0, **kwargs)",
    manimlib.ApplyPointwiseFunctionToCenter: "(function, mobject, **kwargs)",
    manimlib.FadeToColor: "(mobject, color, **kwargs)",
    manimlib.ScaleInPlace: "(mobject, scale_factor, **kwargs)",
    manimlib.ShrinkToCenter: "(mobject, **kwargs)",
    manimlib.ApplyFunction: "(function, mobject, **kwargs)",
    manimlib.ApplyMatrix: "(matrix, mobject, **kwargs)",
    manimlib.ApplyComplexFunction: "(function, mobject, **kwargs)",
}
for adapter, declared_call_shape in adapter_call_shapes.items():
    actual_call_shape = str(inspect.signature(adapter))
    assert actual_call_shape == declared_call_shape
adapter_method_call_shapes = {
    manimlib.ApplyPointwiseFunction.__init__: (
        "(self, function, mobject, run_time=3.0, **kwargs)"
    ),
    manimlib.ApplyPointwiseFunctionToCenter.__init__: (
        "(self, function, mobject, **kwargs)"
    ),
    manimlib.ApplyPointwiseFunctionToCenter.create_target: "(self)",
    manimlib.FadeToColor.__init__: "(self, mobject, color, **kwargs)",
    manimlib.ScaleInPlace.__init__: "(self, mobject, scale_factor, **kwargs)",
    manimlib.ShrinkToCenter.__init__: "(self, mobject, **kwargs)",
    manimlib.ApplyFunction.__init__: "(self, function, mobject, **kwargs)",
    manimlib.ApplyFunction.create_target: "(self)",
    manimlib.ApplyMatrix.__init__: "(self, matrix, mobject, **kwargs)",
    manimlib.ApplyMatrix.initialize_matrix: "(self, matrix)",
    manimlib.ApplyComplexFunction.__init__: "(self, function, mobject, **kwargs)",
    manimlib.ApplyComplexFunction.init_path_func: "(self)",
}
for adapter_method, declared_call_shape in adapter_method_call_shapes.items():
    actual_call_shape = str(inspect.signature(adapter_method))
    assert actual_call_shape == declared_call_shape
assert manimlib.ApplyPointwiseFunction.__bases__ == (manimlib.ApplyMethod,)
assert manimlib.ApplyPointwiseFunctionToCenter.__bases__ == (manimlib.Transform,)
assert manimlib.FadeToColor.__bases__ == (manimlib.ApplyMethod,)
assert manimlib.ScaleInPlace.__bases__ == (manimlib.ApplyMethod,)
assert manimlib.ShrinkToCenter.__bases__ == (manimlib.ScaleInPlace,)
assert manimlib.ApplyFunction.__bases__ == (manimlib.Transform,)
assert manimlib.ApplyMatrix.__bases__ == (manimlib.ApplyPointwiseFunction,)
assert manimlib.ApplyComplexFunction.__bases__ == (manimlib.ApplyMethod,)

pointwise_source = geometry.Rectangle(width=1.0, height=0.5)
pointwise_animation = manimlib.ApplyPointwiseFunction(
    lambda point: point + np.array([1.0, -0.5, 0.0]),
    pointwise_source,
)
assert pointwise_animation.run_time == 3.0
Scene().play(
    pointwise_animation,
    run_time=1.0 / 30.0,
    rate_func=manimlib.linear,
)
assert np.allclose(pointwise_source.get_center(), [1.0, -0.5, 0.0])

center_source = geometry.Rectangle(width=1.0, height=0.5)
center_animation = manimlib.ApplyPointwiseFunctionToCenter(
    lambda point: point + 2.0 * manimlib.RIGHT,
    center_source,
)
center_source.shift(manimlib.UP)
Scene().play(
    center_animation,
    run_time=1.0 / 30.0,
    rate_func=manimlib.linear,
)
assert np.allclose(center_source.get_center(), [2.0, 1.0, 0.0])

fade_source = geometry.Square().set_color(manimlib.BLUE)
Scene().play(
    manimlib.FadeToColor(fade_source, manimlib.RED),
    run_time=1.0 / 30.0,
    rate_func=manimlib.linear,
)
assert fade_source.get_color() == manimlib.RED

scale_source = geometry.Rectangle(width=1.0, height=0.5)
scale_scene = Scene()
scale_scene.play(
    manimlib.ScaleInPlace(scale_source, 2.0),
    run_time=1.0 / 30.0,
    rate_func=manimlib.linear,
)
assert np.isclose(scale_source.get_width(), 2.0)
scale_scene.play(
    manimlib.ShrinkToCenter(scale_source),
    run_time=1.0 / 30.0,
    rate_func=manimlib.linear,
)
assert np.isclose(scale_source.get_width(), 2.0e-8)

apply_function_calls = []


def build_apply_target(mobject):
    apply_function_calls.append(mobject.get_center().copy())
    return mobject.scale(2.0).shift(manimlib.RIGHT)


function_source = geometry.Rectangle(width=1.0, height=0.5)
function_animation = manimlib.ApplyFunction(build_apply_target, function_source)
function_scene = Scene()
function_source.shift(manimlib.UP)
assert apply_function_calls == []
function_scene.play(
    function_animation,
    run_time=1.0 / 30.0,
    rate_func=manimlib.linear,
)
assert len(apply_function_calls) == 1
assert np.allclose(apply_function_calls[0], manimlib.UP)
assert np.allclose(function_source.get_center(), [1.0, 1.0, 0.0])
assert np.isclose(function_source.get_width(), 2.0)
function_scene.play(
    function_animation,
    run_time=1.0 / 30.0,
    rate_func=manimlib.linear,
)
assert len(apply_function_calls) == 2
assert np.allclose(apply_function_calls[1], [1.0, 1.0, 0.0])
assert np.allclose(function_source.get_center(), [2.0, 1.0, 0.0])
assert np.isclose(function_source.get_width(), 4.0)

invalid_function_animation = manimlib.ApplyFunction(
    lambda _mobject: 7,
    geometry.Square(),
)
try:
    Scene().play(invalid_function_animation)
except Exception as error:
    assert str(error) == (
        "Functions passed to ApplyFunction must return object of type Mobject"
    )
else:
    raise AssertionError("ApplyFunction accepted a non-Mobject target")

matrix_source = geometry.Rectangle(width=1.0, height=0.5).shift(manimlib.RIGHT)
matrix_animation = manimlib.ApplyMatrix([[0.0, -1.0], [1.0, 0.0]], matrix_source)
assert np.array_equal(
    matrix_animation.initialize_matrix([[1.0, 0.0], [0.0, 1.0]]),
    np.identity(3),
)
Scene().play(
    matrix_animation,
    run_time=1.0 / 30.0,
    rate_func=manimlib.linear,
)
assert np.allclose(matrix_source.get_center(), manimlib.UP)
try:
    manimlib.ApplyMatrix([[1.0]], geometry.Square())
except Exception as error:
    assert str(error) == "Matrix has bad dimensions"
else:
    raise AssertionError("ApplyMatrix accepted a matrix with bad dimensions")

complex_source = geometry.Rectangle(width=1.0, height=0.5).shift(manimlib.RIGHT)
complex_animation = manimlib.ApplyComplexFunction(lambda value: 1j * value, complex_source)
assert np.isclose(complex_animation.path_arc, math.pi / 2.0)
complex_animation.path_arc = 0.0
assert complex_animation.init_path_func() is None
assert np.isclose(complex_animation.path_arc, math.pi / 2.0)
Scene().play(
    complex_animation,
    run_time=1.0 / 30.0,
    rate_func=manimlib.linear,
)
assert np.allclose(complex_source.get_center(), manimlib.UP)

sampled_rate_scene = Scene()
sampled_rate_mover = geometry.Rectangle(width=0.5, height=0.5)
sampled_rate_scene.play(
    sampled_rate_mover.animate.shift([2.0, 0.0, 0.0]),
    run_time=1.0 / 30.0,
    rate_func=manimlib.there_and_back_with_pause,
)
assert np.allclose(sampled_rate_mover.get_center(), [0.0, 0.0, 0.0])

abort_scene = Scene()
abort_mover = geometry.Rectangle(width=1.0, height=1.0)
abort_scene.add(abort_mover)


def explode_after_begin(mob, alpha):
    if alpha > 0.0:
        raise LookupError("callback release failure")
    mob.set_x(alpha)


try:
    abort_scene.play(
        update_animation.UpdateFromAlphaFunc(
            abort_mover,
            explode_after_begin,
            run_time=1.0 / 30.0,
            rate_func=manimlib.linear,
        )
    )
except LookupError as error:
    assert str(error) == "callback release failure"
else:
    raise AssertionError("a Python animation callback exception was swallowed")
abort_scene.play(
    abort_mover.animate.shift(manimlib.RIGHT),
    run_time=1.0 / 30.0,
    rate_func=manimlib.linear,
)
assert np.allclose(abort_mover.get_x(), 1.0)

# Reference Scene defaults random_seed to zero and resets both RNG modules
# used by source-unedited scene files.
Scene()
python_random = [manimlib.random.random() for _ in range(3)]
numpy_random = np.random.random(3)
Scene()
assert python_random == [manimlib.random.random() for _ in range(3)]
assert np.allclose(numpy_random, np.random.random(3))

# Custom callbacks can use the RecordBuffer-backed point-cloud and point
# matching surface without falling back to schema placeholders.
dot_cloud = manimlib.GlowDot([1.0, 2.0, 0.0], opacity=0.75)
opacities = dot_cloud.get_opacities()
assert np.allclose(opacities, [0.75])
dot_cloud.add_point([3.0, 4.0, 0.0], opacity=0.25, color=manimlib.BLUE)
assert np.allclose(dot_cloud.get_points(), [[1, 2, 0], [3, 4, 0]])
assert np.allclose(dot_cloud.get_opacities(), [0.75, 0.25])
assert np.allclose(dot_cloud.data["radius"], [0.2, 0.2])
assert np.allclose(dot_cloud.data["glow_factor"], [2.0, 2.0])
dot_cloud.set_opacity([0.5, 0.125])
assert np.allclose(dot_cloud.get_opacities(), [0.5, 0.125])

match_source = VMobject().set_points_as_corners(
    [[0.0, 0.0, 0.0], [2.0, 1.0, 0.0], [3.0, 0.0, 0.0]]
).shift([4.0, 0.0, 0.0])
match_target = VMobject().set_points_as_corners(
    [[-1.0, 0.0, 0.0], [0.0, 0.0, 0.0]]
)
match_target.match_points(match_source)
assert match_target.n_records() == match_source.n_records()
assert np.allclose(match_target.get_points(), match_source.get_points())

vector_field = importlib.import_module("manimlib.mobject.vector_field")
assert vector_field.VectorField.__bases__ == (VMobject,)
assert vector_field.TimeVaryingVectorField.__bases__ == (
    vector_field.VectorField,
)
assert list(inspect.signature(vector_field.VectorField).parameters) == [
    "func",
    "coordinate_system",
    "sample_coords",
    "density",
    "magnitude_range",
    "color",
    "color_map_name",
    "color_map",
    "stroke_opacity",
    "stroke_width",
    "tip_width_ratio",
    "tip_len_to_width",
    "max_vect_len",
    "max_vect_len_to_step_size",
    "flat_stroke",
    "norm_to_opacity_func",
    "kwargs",
]
assert inspect.signature(vector_field.VectorField).parameters[
    "density"
].default == 2.0
assert inspect.signature(vector_field.VectorField).parameters[
    "color_map_name"
].default == "3b1b_colormap"
assert list(
    inspect.signature(vector_field.TimeVaryingVectorField).parameters
) == ["time_func", "coordinate_system", "kwargs"]
assert str(inspect.signature(vector_field.VectorField.set_stroke)) == (
    "(self, color=None, width=None, opacity=None, behind=None, flat=None, "
    "recurse=True)"
)
assert str(inspect.signature(vector_field.VectorField.get_sample_points)) == (
    "(self, center, width, height, depth, x_density, y_density, z_density)"
)
assert str(inspect.signature(vector_field.get_sample_coords)) == (
    "(coordinate_system, density=1.0)"
)

field_axes = manimlib.Axes(
    x_range=(0.0, 2.0, 1.0),
    y_range=(-1.0, 1.0, 1.0),
    width=2.0,
    height=2.0,
)
sample_grid = vector_field.get_sample_coords(field_axes)
assert sample_grid.shape == (9, 2)
assert np.allclose(
    sample_grid,
    [
        [0.0, -1.0],
        [0.0, 0.0],
        [0.0, 1.0],
        [1.0, -1.0],
        [1.0, 0.0],
        [1.0, 1.0],
        [2.0, -1.0],
        [2.0, 0.0],
        [2.0, 1.0],
    ],
)
try:
    vector_field.get_sample_coords(field_axes, density=0.0)
except ValueError as error:
    assert str(error) == "VectorField density must be positive and finite"
else:
    raise AssertionError("VectorField accepted zero sampling density")
try:
    vector_field.get_sample_coords(field_axes, density=1000.0)
except ValueError as error:
    assert "65536-point resource budget" in str(error)
else:
    raise AssertionError("VectorField accepted an excessive sample grid")
try:
    vector_field.VectorField(
        np.zeros_like,
        field_axes,
        sample_coords=np.zeros((65_537, 2)),
        max_vect_len=0.5,
        color=manimlib.BLUE,
    )
except ValueError as error:
    assert "65536-point resource budget" in str(error)
else:
    raise AssertionError("VectorField accepted excessive explicit samples")

observed_field_shapes = []


def two_dimensional_field(coords):
    observed_field_shapes.append(coords.shape)
    return np.column_stack([np.ones(len(coords)), np.zeros(len(coords))])


static_field = vector_field.VectorField(
    two_dimensional_field,
    field_axes,
    sample_coords=np.array([[0.0, 0.0], [1.0, 0.0]]),
    max_vect_len=0.5,
    color=manimlib.BLUE,
)
assert observed_field_shapes == [(2, 2)]
assert static_field.sample_coords.shape == (2, 2)
assert static_field.get_num_points() == 15
assert static_field.get_joint_type() == 0
drawn_length = 0.5 * math.tanh(2.0)
assert math.isclose(
    np.linalg.norm(static_field.get_points()[6] - static_field.get_points()[0]),
    drawn_length,
    rel_tol=0.0,
    abs_tol=2e-7,
)
base_widths = np.array([1, 1, 1, 1, 4, 2, 0, 0, 1, 1, 1, 1, 4, 2, 0])
assert np.allclose(static_field.base_stroke_width_array, base_widths)
assert np.allclose(static_field.get_stroke_widths(), 3.0 * base_widths)
assert np.allclose(
    static_field.data["stroke_rgba"][:, :3],
    np.repeat([manimlib.color_to_rgb(manimlib.BLUE)], 15, axis=0),
)
assert static_field.set_stroke_width(2.0) is static_field
assert np.allclose(static_field.get_stroke_widths(), 2.0 * base_widths)
assert static_field.set_stroke(
    manimlib.RED,
    1.5,
    opacity=0.4,
    behind=True,
    flat=True,
) is static_field
assert np.allclose(static_field.get_stroke_widths(), 1.5 * base_widths)
assert np.allclose(static_field.get_stroke_opacities(), 0.4)
assert np.allclose(
    static_field.data["stroke_rgba"][:, :3], manimlib.color_to_rgb(manimlib.RED)
)
assert static_field.uniforms["stroke_behind"]
assert static_field.get_flat_stroke()

old_sample_points = static_field.sample_points.copy()
assert static_field.set_sample_coords([[0.5, 0.0], [1.5, 0.0]]) is static_field
assert np.array_equal(static_field.sample_points, old_sample_points)
assert static_field.update_sample_points() is None
assert not np.array_equal(static_field.sample_points, old_sample_points)
assert static_field.update_vectors() is static_field
assert np.allclose(
    static_field.data["stroke_rgba"][:, :3], manimlib.color_to_rgb(manimlib.RED)
)
assert np.allclose(static_field.get_stroke_opacities(), 0.4)

grid_points = static_field.get_sample_points(
    np.zeros(3), 2.0, 2.0, 0.0, 1.0, 1.0, 1.0
)
assert np.allclose(
    grid_points,
    [
        [-1.0, -1.0, 0.0],
        [-1.0, 0.0, 0.0],
        [-1.0, 1.0, 0.0],
        [0.0, -1.0, 0.0],
        [0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [1.0, -1.0, 0.0],
        [1.0, 0.0, 0.0],
        [1.0, 1.0, 0.0],
    ],
)
try:
    static_field.get_sample_points(
        np.zeros(3), 2.0, 2.0, 0.0, 1000.0, 1000.0, 1.0
    )
except ValueError as error:
    assert "65536-point resource budget" in str(error)
else:
    raise AssertionError("VectorField accepted an excessive point grid")
initialized_field = static_field.copy()
assert initialized_field.init_points() is None
assert np.array_equal(initialized_field.get_points(), np.zeros((15, 3)))
assert initialized_field.get_joint_type() == 0

custom_field = vector_field.VectorField(
    lambda coords: np.array([[0.0, 0.0], [2.0, 0.0]]),
    field_axes,
    sample_coords=[[0.0, 0.0], [1.0, 0.0]],
    magnitude_range=(0.0, 2.0),
    max_vect_len=0.5,
    color_map=lambda alphas: np.column_stack(
        [alphas, np.zeros(len(alphas)), 1.0 - alphas, np.ones(len(alphas))]
    ),
    norm_to_opacity_func=lambda norms: norms / 2.0,
)
assert np.allclose(custom_field.data["stroke_rgba"][:8, :3], [0.0, 0.0, 1.0])
assert np.allclose(custom_field.data["stroke_rgba"][8:, :3], [1.0, 0.0, 0.0])
assert np.allclose(custom_field.get_stroke_opacities()[:8], 0.0)
assert np.allclose(custom_field.get_stroke_opacities()[8:], 1.0)

native_mapped_field = vector_field.VectorField(
    lambda coords: np.array([[0.0, 0.0], [2.0, 0.0]]),
    field_axes,
    sample_coords=[[0.0, 0.0], [1.0, 0.0]],
    magnitude_range=(0.0, 2.0),
    max_vect_len=0.5,
    stroke_color=manimlib.GREEN,
)
assert np.allclose(
    native_mapped_field.data["stroke_rgba"][:8, :3],
    manimlib.color_to_rgb(manimlib.BLUE_E),
)
assert np.allclose(
    native_mapped_field.data["stroke_rgba"][8:, :3],
    manimlib.color_to_rgb(manimlib.RED),
)

three_dimensional_field = vector_field.VectorField(
    lambda coords: np.column_stack(
        [np.ones(len(coords)), np.zeros(len(coords)), coords[:, 2]]
    ),
    field_axes,
    sample_coords=[[0.0, 0.0, 0.25], [1.0, 0.0, 0.5]],
    max_vect_len=math.inf,
    color=manimlib.BLUE,
)
assert three_dimensional_field.sample_coords.shape == (2, 3)
assert np.allclose(
    np.linalg.norm(
        three_dimensional_field.get_points()[6]
        - three_dimensional_field.get_points()[0]
    ),
    1.0,
    atol=2e-7,
)
try:
    vector_field.VectorField(
        lambda coords: np.zeros((len(coords) + 1, 2)),
        field_axes,
        sample_coords=[[0.0, 0.0], [1.0, 0.0]],
    )
except ValueError as error:
    assert "one vector per sample" in str(error)
else:
    raise AssertionError("VectorField accepted a callback row-count mismatch")


def refuse_vector_field_callback(coords):
    del coords
    raise LookupError("vector field callback failed")


try:
    vector_field.VectorField(
        refuse_vector_field_callback,
        field_axes,
        sample_coords=[[0.0, 0.0], [1.0, 0.0]],
    )
except LookupError as error:
    assert str(error) == "vector field callback failed"
else:
    raise AssertionError("VectorField swallowed its Python callback error")
try:
    vector_field.VectorField(
        lambda coords: coords,
        field_axes,
        color_map_name="viridis",
    )
except NotImplementedError as error:
    assert "non-bundled matplotlib map" in str(error)
else:
    raise AssertionError("VectorField silently accepted an unavailable color map")
try:
    vector_field.VectorField(
        lambda coords: coords,
        field_axes,
        color_map=object(),
    )
except TypeError as error:
    assert str(error) == "VectorField color_map must be callable"
else:
    raise AssertionError("VectorField accepted a non-callable color map")

gradient = vector_field.get_vectorized_rgb_gradient_function(
    0.0, 3.0, "3b1b_colormap"
)
gradient_rows = gradient([0.0, 1.0, 2.0, 3.0])
assert np.allclose(gradient_rows[0], manimlib.color_to_rgb(manimlib.BLUE_E))
assert np.allclose(gradient_rows[1], manimlib.color_to_rgb(manimlib.GREEN))
assert np.allclose(gradient_rows[2], manimlib.color_to_rgb(manimlib.YELLOW))
assert np.allclose(gradient_rows[3], manimlib.color_to_rgb(manimlib.RED))
scalar_gradient = vector_field.get_rgb_gradient_function(
    0.0, 3.0, "3b1b_colormap"
)
assert np.allclose(scalar_gradient(1.0), manimlib.color_to_rgb(manimlib.GREEN))
try:
    vector_field.get_vectorized_rgb_gradient_function(0, 1, "viridis")
except NotImplementedError as error:
    assert "non-bundled matplotlib data" in str(error)
else:
    raise AssertionError("field gradient silently accepted unavailable map data")

pointwise_field = vector_field.vectorize(lambda x, y: (x + y, x - y))
assert np.allclose(pointwise_field([[1.0, 2.0], [3.0, 1.0]]), [[3, -1], [4, 2]])
moving = geometry.Square(side_length=0.2)
assert vector_field.move_along_vector_field(
    moving, lambda point: np.array([1.0, 0.0, 0.0])
) is moving
moving.update(0.5)
assert np.allclose(moving.get_center(), [0.5, 0.0, 0.0])
moving_children = manimlib.Group(
    geometry.Square(side_length=0.2), geometry.Square(side_length=0.2).shift(manimlib.UP)
)
assert vector_field.move_submobjects_along_vector_field(
    moving_children, lambda point: np.array([0.0, 1.0, 0.0])
) is moving_children
moving_children.update(0.25)
assert np.allclose(moving_children[0].get_center(), [0.0, 0.25, 0.0])
assert np.allclose(moving_children[1].get_center(), [0.0, 1.25, 0.0])
moving_points = geometry.Square(side_length=0.2)
assert vector_field.move_points_along_vector_field(
    moving_points,
    lambda x, y: (1.0, 0.0),
    field_axes,
) is moving_points
moving_points.update(0.5)
assert np.allclose(moving_points.get_center(), [0.5, 0.0, 0.0])

field = vector_field.TimeVaryingVectorField(
    lambda coords, time: np.column_stack(
        [np.full(len(coords), time), np.zeros(len(coords)), np.zeros(len(coords))]
    ),
    field_axes,
    sample_coords=np.array([[0.0, 0.0, 0.0], [1.0, 0.0, 0.0]]),
    max_vect_len=0.5,
    color=manimlib.BLUE,
)
field_before = field.get_points().copy()
field_scene = Scene()
field_scene.add(field_axes, field)
field_scene.wait(1.0 / 30.0)
assert math.isclose(field.time, 1.0 / 30.0, rel_tol=0.0, abs_tol=1e-12)
assert not np.allclose(field.get_points(), field_before)
assert field.increment_time(0.0) is None


def refuse_time_field(coords, time):
    if time > 0:
        raise RuntimeError("time field callback failed")
    return np.zeros_like(coords)


refusing_time_field = vector_field.TimeVaryingVectorField(
    refuse_time_field,
    field_axes,
    sample_coords=[[0.0, 0.0], [1.0, 0.0]],
    max_vect_len=0.5,
    color=manimlib.BLUE,
)
try:
    refusing_time_field.update(1.0 / 30.0)
except RuntimeError as error:
    assert str(error) == "time field callback failed"
else:
    raise AssertionError("TimeVaryingVectorField swallowed its callback error")

# Fixed-frame state is the renderer-consumed typed uniform, including the
# Reference's recursive family default and non-recursive override.
fixed_child = geometry.Square()
fixed_group = manimlib.Group(fixed_child)
assert "__init__" in manimlib.Group.__dict__
iterable_group_children = [geometry.Square(), geometry.Circle()]
iterable_group = manimlib.Group(iterable_group_children)
assert list(iterable_group) == iterable_group_children
deduplicated_group = manimlib.Group(
    iterable_group_children[0], iterable_group_children[0]
)
assert list(deduplicated_group) == iterable_group_children[:1]

# Appendix-C C-14: the Reference's width branch calls set_height(width).
# A 3x2 grid of 2x1 rectangles is already 6x2 with zero buffer, so this
# planted negative becomes 18x6 under the buggy branch and 6x2 here.
grid_source = geometry.Rectangle(width=2.0, height=1.0)
width_grid = grid_source.get_grid(2, 3, width=6.0, buff=0.0)
assert len(width_grid) == 6
assert np.allclose(
    [width_grid.get_width(), width_grid.get_height()], [6.0, 2.0]
)
height_grid = grid_source.get_grid(2, 3, height=4.0, buff=0.0)
assert np.allclose(
    [height_grid.get_width(), height_grid.get_height()], [12.0, 4.0]
)
row_grid = grid_source.get_grid(2, 3, group_by_rows=True, buff=0.0)
assert len(row_grid) == 2
assert [len(row) for row in row_grid] == [3, 3]
column_grid = grid_source.get_grid(2, 3, group_by_cols=True, buff=0.0)
assert len(column_grid) == 3
assert [len(column) for column in column_grid] == [2, 2, 2]
assert fixed_group.is_fixed_in_frame() is False
assert fixed_child.is_fixed_in_frame() is False
assert fixed_group.fix_in_frame() is fixed_group
assert fixed_group.uniforms["is_fixed_in_frame"] == 1.0
assert fixed_child.uniforms["is_fixed_in_frame"] == 1.0
fixed_group.unfix_from_frame(recurse=False)
assert fixed_group.is_fixed_in_frame() is False
assert fixed_child.is_fixed_in_frame() is True
fixed_group.unfix_from_frame()
assert fixed_child.is_fixed_in_frame() is False

# Sampled surfaces select Choreo's UV-grid partial mechanism rather than the
# VMobject-only quadratic operator, and finish on the original full grid.
surface = manimlib.ParametricSurface(
    lambda u, v: np.array([u, v, u + v]),
    resolution=(3, 3),
)
surface_points = surface.get_points().copy()
surface_scene = Scene()
surface_scene.play(
    manimlib.ShowCreation(surface),
    run_time=1.0 / 30.0,
    rate_func=manimlib.linear,
)
assert np.allclose(surface.get_points(), surface_points)

# The public Surface method reaches that same Marionette authority directly.
# Partial reveals replace only the point grid, preserve the target's normals
# and style lanes, and never mutate the source. Full-range reveal restores both
# pointlike columns, matching Mobject.match_points in the pinned Reference.
assert str(inspect.signature(manimlib.Surface.pointwise_become_partial)) == (
    "(self, smobject, a, b, axis=None)"
)
surface_source = manimlib.ParametricSurface(
    lambda u, v: np.array([u, v, u + 2.0 * v]),
    u_range=(0.0, 2.0),
    v_range=(0.0, 2.0),
    resolution=(3, 3),
    color=manimlib.BLUE,
    opacity=0.8,
)
surface_source_before = surface_source.data.copy()
surface_target = manimlib.ParametricSurface(
    lambda u, v: np.array([u - 4.0, v - 4.0, 0.0]),
    resolution=(3, 3),
    color=manimlib.RED,
    opacity=0.35,
)
surface_target_normals = surface_target.data["d_normal_point"].copy()
surface_target_rgba = surface_target.data["rgba"].copy()
expected_v_slice = surface_source_before["point"].reshape((3, 3, 3)).copy()
expected_v_slice[:, 0] = expected_v_slice[:, 1]
expected_v_slice[:, 2] = expected_v_slice[:, 1]
assert (
    surface_target.pointwise_become_partial(surface_source, 0.5, 0.5, axis=1)
    is surface_target
)
assert np.allclose(surface_target.data["point"], expected_v_slice.reshape((-1, 3)))
assert np.array_equal(surface_target.data["d_normal_point"], surface_target_normals)
assert np.array_equal(surface_target.data["rgba"], surface_target_rgba)
assert np.array_equal(surface_source.data, surface_source_before)

default_axis_target = manimlib.ParametricSurface(
    lambda u, v: np.array([u + 4.0, v + 4.0, 0.0]),
    resolution=(3, 3),
    preferred_creation_axis=0,
)
expected_u_slice = surface_source_before["point"].reshape((3, 3, 3)).copy()
expected_u_slice[0] = expected_u_slice[1]
expected_u_slice[2] = expected_u_slice[1]
default_axis_target.pointwise_become_partial(surface_source, 0.5, 0.5)
assert np.allclose(
    default_axis_target.data["point"], expected_u_slice.reshape((-1, 3))
)

full_surface_target = manimlib.ParametricSurface(
    lambda u, v: np.array([-u, -v, 0.0]),
    resolution=(3, 3),
    color=manimlib.GREEN,
    opacity=0.2,
)
full_surface_rgba = full_surface_target.data["rgba"].copy()
full_surface_target.pointwise_become_partial(surface_source, 0.0, 1.0)
assert np.array_equal(
    full_surface_target.data["point"], surface_source_before["point"]
)
assert np.array_equal(
    full_surface_target.data["d_normal_point"],
    surface_source_before["d_normal_point"],
)
assert np.array_equal(full_surface_target.data["rgba"], full_surface_rgba)

self_surface = surface_source.copy()
self_expected = self_surface.data["point"].reshape((3, 3, 3)).copy()
self_expected[:, 0] = self_expected[:, 1]
self_expected[:, 2] = self_expected[:, 1]
assert self_surface.pointwise_become_partial(self_surface, 0.5, 0.5) is self_surface
assert np.allclose(self_surface.data["point"], self_expected.reshape((-1, 3)))

bound_surface_scene = Scene()
bound_surface_source = surface_source.copy()
bound_surface_target = full_surface_target.copy()
bound_surface_scene.add(bound_surface_source, bound_surface_target)
bound_surface_target.pointwise_become_partial(
    bound_surface_source, 0.5, 0.5, axis=0
)
assert np.allclose(
    bound_surface_target.data["point"], expected_u_slice.reshape((-1, 3))
)

foreign_surface_scene = Scene()
foreign_surface_target = full_surface_target.copy()
foreign_surface_scene.add(foreign_surface_target)
foreign_surface_target.pointwise_become_partial(
    bound_surface_source, 0.5, 0.5, axis=1
)
assert np.allclose(
    foreign_surface_target.data["point"], expected_v_slice.reshape((-1, 3))
)

for invalid_axis in (-1, 2):
    refused_surface = full_surface_target.copy()
    refused_before = refused_surface.data.copy()
    try:
        refused_surface.pointwise_become_partial(
            surface_source, 0.25, 0.75, invalid_axis
        )
    except (OverflowError, RuntimeError):
        pass
    else:
        raise AssertionError(f"Surface accepted invalid partial axis {invalid_axis}")
    assert np.array_equal(refused_surface.data, refused_before)

nonfinite_surface = full_surface_target.copy()
nonfinite_before = nonfinite_surface.data.copy()
try:
    nonfinite_surface.pointwise_become_partial(surface_source, float("nan"), 0.5)
except ValueError as error:
    assert "finite" in str(error)
else:
    raise AssertionError("Surface accepted a non-finite partial bound")
assert np.array_equal(nonfinite_surface.data, nonfinite_before)

malformed_surface_source = surface_source.copy()
malformed_surface_source.resolution = (2, 4)
malformed_surface_target = full_surface_target.copy()
malformed_target_before = malformed_surface_target.data.copy()
try:
    malformed_surface_target.pointwise_become_partial(
        malformed_surface_source, 0.25, 0.75
    )
except RuntimeError as error:
    assert "schema" in str(error).lower()
else:
    raise AssertionError("Surface accepted resolution metadata inconsistent with its records")
assert np.array_equal(malformed_surface_target.data, malformed_target_before)

try:
    surface_target.pointwise_become_partial(VMobject(), 0.0, 1.0)
except AssertionError:
    pass
else:
    raise AssertionError("Surface accepted a non-Surface partial source")

# Filled Arrow and Vector are authored over Atlas rather than inherited Line
# approximations.  Exercise every public row together so the tip-record
# identity, Reference ratio caps, and rebuild side effects cannot drift apart.
assert manimlib.Arrow is geometry.Arrow
assert manimlib.Vector is geometry.Vector
assert geometry.Arrow.__bases__ == (geometry.Line,)
assert geometry.Vector.__bases__ == (geometry.Arrow,)
assert geometry.Arrow.tickness_multiplier == 0.015
assert list(inspect.signature(geometry.Arrow).parameters) == [
    "start",
    "end",
    "buff",
    "path_arc",
    "fill_color",
    "fill_opacity",
    "stroke_width",
    "thickness",
    "tip_width_ratio",
    "tip_angle",
    "max_tip_length_to_length_ratio",
    "max_width_to_length_ratio",
    "kwargs",
]
assert list(inspect.signature(geometry.Vector).parameters) == [
    "direction",
    "buff",
    "kwargs",
]

capped_arrow = geometry.Arrow(
    [0.0, 0.0, 0.0],
    [4.0, 0.0, 0.0],
    buff=0.0,
    thickness=4.0,
    tip_width_ratio=3.0,
    tip_angle=math.pi / 2.0,
    max_tip_length_to_length_ratio=0.005,
    max_width_to_length_ratio=0.005,
    fill_color=manimlib.RED,
    fill_opacity=0.65,
    stroke_color=manimlib.BLUE,
    stroke_width=1.25,
)
assert capped_arrow.thickness == 4.0
assert capped_arrow.tip_width_ratio == 3.0
assert capped_arrow.tip_angle == math.pi / 2.0
assert capped_arrow.max_tip_length_to_length_ratio == 0.005
assert capped_arrow.max_width_to_length_ratio == 0.005
# width=min(4*0.015, 4*0.005)=0.02; the 0.005 head-length cap then
# scales the initial (tip width, tip length) pair from (0.06, 0.03).
assert np.allclose(capped_arrow.get_key_dimensions(4.0), [0.02, 0.04, 0.02])
assert np.allclose(capped_arrow.get_start(), [0.0, 0.0, 0.0], atol=1e-9)
assert np.allclose(capped_arrow.get_end(), [4.0, 0.0, 0.0], atol=1e-9)
start, end = capped_arrow.get_start_and_end()
assert np.allclose(start, capped_arrow.get_start())
assert np.allclose(end, capped_arrow.get_end())

capped_identity = id(capped_arrow)
capped_style = (
    capped_arrow.get_fill_color(),
    capped_arrow.get_fill_opacity(),
    capped_arrow.get_stroke_color(),
    capped_arrow.get_stroke_opacity(),
    capped_arrow.get_stroke_width(),
)
capped_arrow.uniforms["depth_test"] = True
capped_arrow.uniforms["anti_alias_width"] = 2.75
assert capped_arrow.set_points_by_ends(
    [-1.0, -1.0, 0.0],
    [2.0, 2.0, 0.0],
    buff=0.0,
    path_arc=math.pi / 3.0,
) is capped_arrow
assert id(capped_arrow) == capped_identity
assert np.allclose(capped_arrow.get_start(), [-1.0, -1.0, 0.0], atol=1e-8)
curved_end = capped_arrow.get_end()
assert np.allclose(curved_end, [2.0, 2.0, 0.0], atol=1e-4), curved_end
assert capped_arrow.uniforms["depth_test"] is True
assert capped_arrow.uniforms["anti_alias_width"] == 2.75
assert capped_style == (
    capped_arrow.get_fill_color(),
    capped_arrow.get_fill_opacity(),
    capped_arrow.get_stroke_color(),
    capped_arrow.get_stroke_opacity(),
    capped_arrow.get_stroke_width(),
)

transformed_arrow = geometry.Arrow(
    [0.0, 0.0, 0.0], [2.0, 0.0, 0.0], buff=0.0
)
transformed_identity = id(transformed_arrow)
assert transformed_arrow.rotate(
    math.pi / 2.0, about_point=manimlib.ORIGIN
) is transformed_arrow
assert np.allclose(transformed_arrow.get_start(), [0.0, 0.0, 0.0], atol=1e-8)
assert np.allclose(transformed_arrow.get_end(), [0.0, 2.0, 0.0], atol=1e-8)
assert transformed_arrow.scale(
    0.5, about_point=manimlib.ORIGIN
) is transformed_arrow
assert id(transformed_arrow) == transformed_identity
assert np.allclose(transformed_arrow.get_end(), [0.0, 1.0, 0.0], atol=1e-8)
assert transformed_arrow.set_thickness(6.0) is transformed_arrow
assert transformed_arrow.thickness == 6.0
assert np.allclose(transformed_arrow.get_end(), [0.0, 1.0, 0.0], atol=1e-8)
assert transformed_arrow.put_start_and_end_on(
    [1.0, 2.0, 3.0], [2.0, 4.0, 6.0]
) is transformed_arrow
assert np.allclose(transformed_arrow.get_start(), [1.0, 2.0, 3.0], atol=1e-8)
assert np.allclose(transformed_arrow.get_end(), [2.0, 4.0, 6.0], atol=1e-8)

vector_2d = geometry.Vector([3.0, 4.0])
assert np.allclose(vector_2d.get_start(), manimlib.ORIGIN, atol=1e-9)
assert np.allclose(vector_2d.get_end(), [3.0, 4.0, 0.0], atol=1e-9)
vector_3d = geometry.Vector([1.0, -2.0, 3.0])
assert np.allclose(vector_3d.get_start(), manimlib.ORIGIN, atol=1e-9)
assert np.allclose(vector_3d.get_end(), [1.0, -2.0, 3.0], atol=1e-8)

# Filled Arrow endpoint updates rebuild Atlas's length-dependent tip geometry
# in place, so scene-bound updater callbacks preserve arena identity.
bound_arrow = manimlib.Vector(manimlib.RIGHT)
arrow_scene = Scene()
arrow_scene.add(bound_arrow)
arrow_identity = id(bound_arrow)
bound_arrow.put_start_and_end_on([1.0, 2.0, 0.0], [4.0, 2.0, 0.0])
assert id(bound_arrow) == arrow_identity
bound_arrow_points = bound_arrow.get_points()
assert np.allclose(
    0.5 * (bound_arrow_points[0] + bound_arrow_points[-3]),
    [1.0, 2.0, 0.0],
)
assert np.min(np.linalg.norm(bound_arrow_points - [4.0, 2.0, 0.0], axis=1)) < 1e-6

# A detached fixed-frame Arrow is rebuilt by the immediate updater call that
# add_updater performs by default.  Rebuilding geometry must not replace the
# engine-owned camera/depth uniforms with fresh Arrow defaults; BeamSplitter's
# three overlay vectors all pass through this exact path before scene adoption.
detached_fixed_arrow = manimlib.Vector(manimlib.RIGHT).fix_in_frame()
detached_fixed_arrow.uniforms["depth_test"] = True
detached_fixed_arrow.uniforms["anti_alias_width"] = 2.5
detached_fixed_arrow.add_updater(
    lambda arrow: arrow.put_start_and_end_on(
        [-2.0, 3.0, 0.0],
        [-3.0, 3.0, 0.0],
    )
)
assert detached_fixed_arrow.is_fixed_in_frame() is True
assert detached_fixed_arrow.uniforms["depth_test"] is True
assert detached_fixed_arrow.uniforms["anti_alias_width"] == 2.5
assert np.allclose(detached_fixed_arrow.start, [-2.0, 3.0, 0.0])
assert np.allclose(detached_fixed_arrow.end, [-3.0, 3.0, 0.0])
arrow_scene.add(detached_fixed_arrow)
assert detached_fixed_arrow.is_fixed_in_frame() is True

# Generic endpoint placement applies one affine transform to the complete
# family in both proxy states. Detached families distribute the Reference
# algorithm over their private nursery roots; bound families recurse through
# the scene Stage.
detached_endpoint_parent = VMobject().set_points_as_corners(
    [[0.0, 0.0, 0.0], [2.0, 0.0, 0.0]]
)
detached_endpoint_child = geometry.Square(side_length=0.5).shift([1.0, 1.0, 0.0])
detached_endpoint_parent.add(detached_endpoint_child)
assert detached_endpoint_parent.put_start_and_end_on(
    [0.0, 0.0, 0.0], [0.0, 4.0, 0.0]
) is detached_endpoint_parent
assert np.allclose(detached_endpoint_parent.get_start(), [0.0, 0.0, 0.0])
assert np.allclose(detached_endpoint_parent.get_end(), [0.0, 4.0, 0.0])
assert np.allclose(detached_endpoint_child.get_center(), [-2.0, 2.0, 0.0])

bound_endpoint_parent = VMobject().set_points_as_corners(
    [[0.0, 0.0, 0.0], [2.0, 0.0, 0.0]]
)
bound_endpoint_child = geometry.Square(side_length=0.5).shift([1.0, 1.0, 0.0])
bound_endpoint_parent.add(bound_endpoint_child)
endpoint_scene = Scene()
endpoint_scene.add(bound_endpoint_parent)
bound_endpoint_parent.put_start_and_end_on([0.0, 0.0, 0.0], [0.0, 4.0, 0.0])
assert np.allclose(bound_endpoint_parent.get_start(), [0.0, 0.0, 0.0])
assert np.allclose(bound_endpoint_parent.get_end(), [0.0, 4.0, 0.0])
assert np.allclose(bound_endpoint_child.get_center(), [-2.0, 2.0, 0.0])

# Closed-loop refusal happens before any member moves.
closed_endpoint_parent = VMobject().set_points_as_corners(
    [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 0.0]]
)
closed_endpoint_child = geometry.Square(side_length=0.5).shift([2.0, 1.0, 0.0])
closed_endpoint_parent.add(closed_endpoint_child)
closed_parent_before = closed_endpoint_parent.get_points().copy()
closed_child_before = closed_endpoint_child.get_points().copy()
try:
    closed_endpoint_parent.put_start_and_end_on(
        [0.0, 0.0, 0.0], [0.0, 4.0, 0.0]
    )
except Exception as error:
    assert str(error) == "Cannot position endpoints of closed loop"
else:
    raise AssertionError("closed-loop endpoint placement did not refuse")
assert np.array_equal(closed_endpoint_parent.get_points(), closed_parent_before)
assert np.array_equal(closed_endpoint_child.get_points(), closed_child_before)

# A slice aligner made from scene-bound children adopts into that scene and
# remains usable by the ordinary Reference next_to algorithm.
bound_slice_source = manimlib.VGroup(
    geometry.Square(side_length=1.0).shift([-1.0, 0.0, 0.0]),
    geometry.Square(side_length=1.0).shift([1.0, 0.0, 0.0]),
    geometry.Square(side_length=1.0).shift([3.0, 0.0, 0.0]),
)
bound_slice_target = manimlib.VGroup(
    geometry.Square(side_length=1.0).shift([4.0, 0.0, 0.0]),
    geometry.Square(side_length=1.0).shift([6.0, 0.0, 0.0]),
    geometry.Square(side_length=1.0).shift([8.0, 0.0, 0.0]),
)
slice_scene = Scene()
slice_scene.add(bound_slice_source, bound_slice_target)
bound_slice_source.next_to(
    bound_slice_target,
    manimlib.RIGHT,
    buff=0.25,
    index_of_submobject_to_align=slice(0, 2),
)
assert np.allclose(bound_slice_source[0].get_center(), [7.25, 0.0, 0.0])
assert np.allclose(bound_slice_source[1].get_center(), [9.25, 0.0, 0.0])

# Line.set_angle keeps its start fixed by default. DashedLine reads endpoints
# from its first/last native dash children, matching the Reference override.
assert geometry.DashedLine.__mro__[:3] == (
    geometry.DashedLine,
    geometry.Line,
    geometry.TipableVMobject,
)
assert list(inspect.signature(geometry.DashedLine).parameters) == [
    "start",
    "end",
    "dash_length",
    "positive_space_ratio",
    "kwargs",
]
dashed = geometry.DashedLine([-2.0, 1.0, 0.0], [2.0, 1.0, 0.0])
dashed_start = dashed.get_start().copy()
assert dashed.set_angle(math.pi / 2.0) is dashed
assert np.allclose(dashed.get_start(), dashed_start)
assert np.allclose(dashed.get_unit_vector(), [0.0, 1.0, 0.0], atol=1e-9)
dash_start, dash_end = dashed.get_start_and_end()
assert np.allclose(dash_start, dashed.get_start())
assert np.allclose(dash_end, dashed.get_end())
assert np.allclose(
    dashed.get_first_handle(), dashed.submobjects[0].get_points()[1]
)
assert np.allclose(
    dashed.get_last_handle(), dashed.submobjects[-1].get_points()[-2]
)
assert dashed.calculate_num_dashes(0.5, 0.5) == 4
for dash_args, refusal in (
    ((0.0, 0.5), "dash length must be positive and finite"),
    ((0.5, 0.0), "positive-space ratio must be finite and in (0, 1]"),
    ((1e-6, 0.5), "above the 4096 cap"),
):
    try:
        dashed.calculate_num_dashes(*dash_args)
    except ValueError as error:
        assert refusal in str(error)
    else:
        raise AssertionError("invalid native dash count parameters succeeded")

# TangentLine is the Atlas true-arclength construction, not a generated Line
# shell. On this 1+3 unit corner path, alpha=0.5 is one unit up the long
# vertical segment; curve-index interpolation would incorrectly stop at the
# corner. The requested length remains exact in detached and bound states.
assert geometry.TangentLine.__mro__[:3] == (
    geometry.TangentLine,
    geometry.Line,
    geometry.TipableVMobject,
)
assert list(inspect.signature(geometry.TangentLine).parameters) == [
    "vmob",
    "alpha",
    "length",
    "d_alpha",
    "kwargs",
]
tangent_source = VMobject().set_points_as_corners(
    ([0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 3.0, 0.0])
)
tangent = geometry.TangentLine(
    tangent_source,
    0.5,
    length=2.0,
    d_alpha=1e-4,
    stroke_color=manimlib.RED,
    stroke_width=3.0,
)
assert np.allclose(tangent.get_center(), [1.0, 1.0, 0.0], atol=1e-6)
assert np.allclose(tangent.get_unit_vector(), [0.0, 1.0, 0.0], atol=1e-6)
assert math.isclose(tangent.get_length(), 2.0, abs_tol=1e-6)
assert tangent.get_stroke_color() == manimlib.RED
assert tangent.get_stroke_width() == 3.0

bound_tangent_source = tangent_source.copy()
bound_tangent_scene = Scene()
bound_tangent_scene.add(bound_tangent_source)
bound_tangent_source.shift([3.0, -2.0, 0.0])
bound_tangent = geometry.TangentLine(
    bound_tangent_source, 0.5, length=3.0, d_alpha=1e-4
)
bound_tangent_identity = id(bound_tangent)
bound_tangent_scene.add(bound_tangent)
assert id(bound_tangent) == bound_tangent_identity
assert np.allclose(bound_tangent.get_center(), [4.0, -1.0, 0.0], atol=1e-6)
assert math.isclose(bound_tangent.get_length(), 3.0, abs_tol=1e-6)

empty_tangent = geometry.TangentLine(VMobject(), 0.5)
assert empty_tangent.get_num_points() == 0

# StrokeArrow is the authored Line subclass and Atlas now emits the pinned
# one-path, three-record stroke taper directly. There is no synthetic tip
# child for Python family traversal to observe.
assert geometry.StrokeArrow.__mro__[:4] == (
    geometry.StrokeArrow,
    geometry.Line,
    geometry.TipableVMobject,
    VMobject,
)
assert list(inspect.signature(geometry.StrokeArrow).parameters) == [
    "start",
    "end",
    "stroke_color",
    "stroke_width",
    "buff",
    "tip_width_ratio",
    "tip_len_to_width",
    "max_tip_length_to_length_ratio",
    "max_width_to_length_ratio",
    "kwargs",
]
stroke_arrow = geometry.StrokeArrow(
    [0.0, 0.0, 0.0], [4.0, 0.0, 0.0], stroke_color=manimlib.RED
)
assert stroke_arrow.submobjects == []
assert stroke_arrow.get_num_points() == 7
assert np.allclose(stroke_arrow.get_start(), [0.25, 0.0, 0.0])
assert np.allclose(stroke_arrow.get_end(), [3.75, 0.0, 0.0])
assert np.allclose(
    stroke_arrow.get_stroke_widths(), [5.0, 5.0, 5.0, 5.0, 25.0, 12.5, 0.0]
)
assert stroke_arrow.get_stroke_color() == manimlib.RED

ratio_arrow = geometry.StrokeArrow(
    [0.0, 0.0, 0.0],
    [2.0, 0.0, 0.0],
    stroke_width=2.0,
    buff=0.0,
    tip_width_ratio=3.0,
    tip_len_to_width=0.1,
    max_tip_length_to_length_ratio=0.25,
    max_width_to_length_ratio=0.4,
)
assert np.allclose(ratio_arrow.get_stroke_widths()[-3:], [2.4, 1.2, 0.0])
assert np.allclose(ratio_arrow.get_points()[-1], [2.0, 0.0, 0.0])
ratio_points = ratio_arrow.get_points().copy()
assert ratio_arrow.insert_tip_anchor() is ratio_arrow
assert ratio_arrow.submobjects == []
assert ratio_arrow.get_num_points() == 7
assert np.allclose(ratio_arrow.get_points(), ratio_points, atol=1e-9)

# The width-profile method is a real live RecordBuffer update, while reset,
# endpoint changes, and scale route geometry back through Atlas.
ratio_arrow.get_stroke_widths()[:] = 7.0
assert ratio_arrow.create_tip_with_stroke_width() is ratio_arrow
assert np.allclose(ratio_arrow.get_stroke_widths()[:4], 7.0)
assert np.allclose(ratio_arrow.get_stroke_widths()[-3:], [2.4, 1.2, 0.0])
ratio_identity = id(ratio_arrow)
assert ratio_arrow.set_stroke(color=manimlib.BLUE, width=1.5) is ratio_arrow
assert id(ratio_arrow) == ratio_identity
assert ratio_arrow.get_stroke_color() == manimlib.BLUE
assert np.allclose(ratio_arrow.get_stroke_widths()[-3:], [2.4, 1.2, 0.0])
assert ratio_arrow.set_points_by_ends(
    [-3.0, 1.0, 0.0], [3.0, 1.0, 0.0], buff=0.5, path_arc=0.0
) is ratio_arrow
assert np.allclose(ratio_arrow.get_start(), [-2.5, 1.0, 0.0])
assert np.allclose(ratio_arrow.get_end(), [2.5, 1.0, 0.0])
length_before_scale = ratio_arrow.get_length()
assert ratio_arrow.scale(2.0) is ratio_arrow
assert math.isclose(ratio_arrow.get_length(), 2.0 * length_before_scale, abs_tol=1e-6)
assert ratio_arrow.get_num_points() == 7
assert ratio_arrow.reset_tip() is ratio_arrow

bound_stroke_scene = Scene()
bound_stroke_scene.add(ratio_arrow)
assert ratio_arrow.set_stroke(width=2.0) is ratio_arrow
assert id(ratio_arrow) == ratio_identity
assert np.allclose(ratio_arrow.get_stroke_widths()[-3:], [6.0, 3.0, 0.0])
assert ratio_arrow.set_points_by_ends(
    [-1.0, -1.0, 0.0], [1.0, -1.0, 0.0], path_arc=math.pi / 3.0
) is ratio_arrow
assert id(ratio_arrow) == ratio_identity
assert ratio_arrow.get_num_points() > 7
assert ratio_arrow.submobjects == []

degenerate_stroke = geometry.StrokeArrow([1.0, 2.0, 0.0], [1.0, 2.0, 0.0])
assert degenerate_stroke.submobjects == []
assert np.isfinite(degenerate_stroke.get_points()).all()
assert np.isfinite(degenerate_stroke.get_stroke_widths()).all()
assert np.allclose(degenerate_stroke.get_stroke_widths()[-3:], 0.0)
try:
    geometry.StrokeArrow([0.0, 0.0, 0.0], [1.0, 0.0, 0.0], path_arc=math.nan)
except ValueError as error:
    assert "arc angle must be finite" in str(error)
else:
    raise AssertionError("StrokeArrow accepted a non-finite path_arc")

# `become` must keep the Python proxy family and Marionette family aligned
# when a live redraw changes a generated child's multiplicity.  This is the
# exact varying-dash-count shape exercised by MaxProcess's always_redraw line.
many_dashes = geometry.DashedLine(
    [0.0, 0.0, 0.0], [4.0, 0.0, 0.0], dash_length=0.12
)
few_dashes = geometry.DashedLine(
    [0.0, 0.0, 0.0], [1.0, 0.0, 0.0], dash_length=0.12
)
assert len(many_dashes.submobjects) > len(few_dashes.submobjects)
dash_scene = Scene()
dash_scene.add(many_dashes)
many_dashes_identity = id(many_dashes)
assert many_dashes.become(few_dashes) is many_dashes
assert id(many_dashes) == many_dashes_identity
assert len(many_dashes.submobjects) == len(few_dashes.submobjects)
assert len(many_dashes.get_family()) == 1 + len(many_dashes.submobjects)
assert np.allclose(
    many_dashes.submobjects[0].get_points(),
    few_dashes.submobjects[0].get_points(),
)

named_source = manimlib.VGroup(geometry.Dot(), geometry.Dot())
named_source.focus = named_source.submobjects[1]
named_receiver = manimlib.VGroup(geometry.Dot())
named_receiver.become(named_source)
assert named_receiver.focus is named_receiver.submobjects[1]

foreign_dash_scene = Scene()
foreign_dashes = geometry.DashedLine(
    [0.0, 0.0, 0.0], [2.0, 0.0, 0.0], dash_length=0.12
)
foreign_dash_scene.add(foreign_dashes)
many_family_before = list(many_dashes.submobjects)
foreign_family_before = list(foreign_dashes.submobjects)
try:
    many_dashes.become(foreign_dashes)
except bridge_errors.ForeignStageError as error:
    assert "become endpoints must belong to one Scene" in str(error)
else:
    raise AssertionError("become aligned foreign-stage families before refusal")
assert list(many_dashes.submobjects) == many_family_before
assert list(foreign_dashes.submobjects) == foreign_family_before

alignment_leaf = geometry.Dot(fill_opacity=1.0)
assert alignment_leaf.add_n_more_submobjects(2) is alignment_leaf
assert len(alignment_leaf.submobjects) == 2
assert all(child.get_num_points() == 1 for child in alignment_leaf.submobjects)
visible_child = geometry.Dot(fill_opacity=1.0)
alignment_group = manimlib.VGroup(visible_child)
alignment_group.add_n_more_submobjects(2)
assert alignment_group.submobjects[0] is visible_child
assert len(alignment_group.submobjects) == 3
assert all(
    math.isclose(child.get_fill_opacity(), 0.0)
    for child in alignment_group.submobjects[1:]
)
assert math.isclose(visible_child.invisible_copy().get_fill_opacity(), 0.0)

alignment_before = list(alignment_group.submobjects)
try:
    alignment_group.add_n_more_submobjects(1 << 16)
except RuntimeError as error:
    assert "family alignment requested 65539 direct submobjects" in str(error)
    assert "maximum is 65536" in str(error)
else:
    raise AssertionError("an over-budget family alignment reached allocation")
assert list(alignment_group.submobjects) == alignment_before

try:
    alignment_group.align_family(object())
except TypeError as error:
    assert "align_family expects a Mobject" in str(error)
else:
    raise AssertionError("align_family accepted a non-Mobject")
assert list(alignment_group.submobjects) == alignment_before

# The polygon lineage is native Atlas geometry, not schema-generated empty
# shells.  Constructor topology, rounded-corner mutation, and ArrowTip's live
# queries all remain valid after scene adoption and ordinary transforms.
assert manimlib.Polygon is geometry.Polygon
assert manimlib.RegularPolygon is geometry.RegularPolygon
assert manimlib.Triangle is geometry.Triangle
assert manimlib.ArrowTip is geometry.ArrowTip
assert issubclass(geometry.RegularPolygon, geometry.Polygon)
assert issubclass(geometry.Triangle, geometry.RegularPolygon)
assert issubclass(geometry.ArrowTip, geometry.Triangle)
assert str(inspect.signature(geometry.Polygon)) == "(*vertices, **kwargs)"
assert str(inspect.signature(geometry.RegularPolygon)) == (
    "(n=6, radius=1.0, start_angle=None, **kwargs)"
)
assert str(inspect.signature(geometry.Triangle)) == "(**kwargs)"
assert list(inspect.signature(geometry.ArrowTip).parameters) == [
    "angle",
    "width",
    "length",
    "fill_opacity",
    "fill_color",
    "stroke_width",
    "tip_style",
    "kwargs",
]

polygon = geometry.Polygon(
    [0.0, 0.0, 0.0],
    [2.0, 0.0, 0.0],
    [0.0, 2.0, 0.0],
    color=manimlib.BLUE,
)
assert polygon.get_num_curves() == 3
assert np.allclose(
    polygon.get_vertices(),
    [[0.0, 0.0, 0.0], [2.0, 0.0, 0.0], [0.0, 2.0, 0.0]],
)
assert polygon.get_fill_color() == manimlib.BLUE
assert polygon.get_stroke_color() == manimlib.BLUE

rounded = polygon.copy()
rounded_style = (
    rounded.get_fill_color(),
    rounded.get_fill_opacity(),
    rounded.get_stroke_color(),
    rounded.get_stroke_opacity(),
    rounded.get_stroke_width(),
)
rounded_before = rounded.get_points().copy()
assert rounded.round_corners(0.2) is rounded
assert rounded.get_num_curves() == 9
assert not np.array_equal(rounded.get_points(), rounded_before)
assert np.isfinite(rounded.get_points()).all()
assert (
    rounded.get_fill_color(),
    rounded.get_fill_opacity(),
    rounded.get_stroke_color(),
    rounded.get_stroke_opacity(),
    rounded.get_stroke_width(),
) == rounded_style

default_rounded = geometry.Polygon(
    [-1.0, -1.0, 0.0],
    [1.0, -1.0, 0.0],
    [1.0, 1.0, 0.0],
    [-1.0, 1.0, 0.0],
)
default_rounded.round_corners()
assert default_rounded.get_num_curves() == 12

concave_rounded = geometry.Polygon(
    [-1.0, -1.0, 0.0],
    [1.0, -1.0, 0.0],
    [1.0, 1.0, 0.0],
    [-1.0, 1.0, 0.0],
).round_corners(-0.1)
assert np.isfinite(concave_rounded.get_points()).all()

bound_polygon = geometry.Polygon(
    [0.0, 0.0, 0.0], [3.0, 0.0, 0.0], [0.0, 2.0, 0.0]
)
polygon_scene = Scene()
polygon_scene.add(bound_polygon)
bound_polygon_identity = id(bound_polygon)
bound_polygon_points = bound_polygon.get_points().copy()
bound_polygon.round_corners(0.15)
assert id(bound_polygon) == bound_polygon_identity
assert not np.array_equal(bound_polygon.get_points(), bound_polygon_points)
assert bound_polygon.get_num_curves() == 9

hexagon = geometry.RegularPolygon(n=6, radius=2.0)
assert len(hexagon.get_vertices()) == 6
assert np.allclose(np.linalg.norm(hexagon.get_vertices(), axis=1), 2.0)
assert np.allclose(hexagon.get_vertices()[0], [2.0, 0.0, 0.0])
triangle = geometry.Triangle()
assert len(triangle.get_vertices()) == 3
assert np.allclose(triangle.get_vertices()[0], [0.0, 1.0, 0.0], atol=1e-12)

tip = geometry.ArrowTip(
    angle=math.pi / 3.0,
    width=0.4,
    length=0.7,
    fill_color=manimlib.RED,
)
assert np.allclose(tip.get_tip_point(), tip.get_points()[0])
assert np.allclose(tip.get_vector(), tip.get_tip_point() - tip.get_base())
assert math.isclose(tip.get_angle(), math.pi / 3.0, abs_tol=1e-6)
assert math.isclose(tip.get_length(), 0.7, abs_tol=1e-6)
assert tip.get_fill_color() == manimlib.RED
for tip_style in (0, 1, 2, 99):
    styled_tip = geometry.ArrowTip(tip_style=tip_style)
    assert styled_tip.has_points()
    assert np.isfinite(styled_tip.get_points()).all()
assert np.allclose(
    geometry.ArrowTip(tip_style=1.5).get_points(),
    geometry.ArrowTip(tip_style=0).get_points(),
)
inner_smooth_tip = geometry.ArrowTip(tip_style=1)
assert np.allclose(
    inner_smooth_tip.get_base(), inner_smooth_tip.point_from_proportion(0.5)
)
assert not np.allclose(
    inner_smooth_tip.get_base(),
    inner_smooth_tip.quick_point_from_proportion(0.5),
    atol=1e-3,
)
assert np.allclose(
    inner_smooth_tip.get_vector(),
    inner_smooth_tip.get_tip_point() - inner_smooth_tip.get_base(),
)
assert math.isclose(
    inner_smooth_tip.get_length(),
    np.linalg.norm(inner_smooth_tip.get_vector()),
)

# TipableVMobject is an authored portal base, not a generated import shell.
# The Python layer owns the Reference lifecycle and object identity; Atlas
# owns the terminal tangent, placement, and BN-03 true-length shaft trim.
assert manimlib.TipableVMobject is geometry.TipableVMobject
assert geometry.TipableVMobject.__bases__ == (VMobject,)
assert geometry.Arc.__bases__ == (geometry.TipableVMobject,)
assert geometry.Line.__bases__ == (geometry.TipableVMobject,)
assert geometry.TipableVMobject.tip_config == dict(
    fill_opacity=1.0,
    stroke_width=0.0,
    tip_style=0.0,
)

untipped = geometry.Line([-2.0, -2.0, 0.0], [2.0, -2.0, 0.0])
assert not untipped.has_tip()
assert not untipped.has_start_tip()
try:
    untipped.get_tip()
except Exception as error:
    assert str(error) == "tip not found"
else:
    raise AssertionError("get_tip accepted a tipless TipableVMobject")
try:
    untipped.get_default_tip_length()
except AttributeError:
    pass
else:
    raise AssertionError("get_default_tip_length hid a missing public attribute")
untipped.tip_length = 0.625
assert untipped.get_default_tip_length() == 0.625
assert np.array_equal(untipped.get_first_handle(), untipped.get_points()[1])
assert np.array_equal(untipped.get_last_handle(), untipped.get_points()[-2])

manual_line = geometry.Line([-1.0, -3.0, 0.0], [1.0, -3.0, 0.0])
manual_logical_end = manual_line.get_end().copy()
manual_tip = manual_line.get_unpositioned_tip(length=0.2, width=0.3)
manual_tip_identity = id(manual_tip)
assert isinstance(manual_tip, geometry.ArrowTip)
assert manual_tip not in manual_line
assert manual_line.position_tip(manual_tip) is manual_tip
assert id(manual_tip) == manual_tip_identity
assert np.allclose(manual_tip.get_tip_point(), manual_logical_end, atol=1e-9)
assert manual_line.reset_endpoints_based_on_tip(manual_tip, False) is manual_line
assert VMobject.get_end(manual_line)[0] < manual_logical_end[0]
assert manual_line.asign_tip_attr(manual_tip, False) is manual_line
assert not manual_line.has_tip()
manual_line.add(manual_tip)
assert manual_line.has_tip()

tipped_line = geometry.Line(
    [-2.0, -1.0, 0.0],
    [2.0, -1.0, 0.0],
    stroke_color=manimlib.BLUE,
)
tipped_identity = id(tipped_line)
logical_start = tipped_line.get_start().copy()
logical_end = tipped_line.get_end().copy()
assert tipped_line.add_tip(length=0.5, width=0.4) is tipped_line
assert id(tipped_line) == tipped_identity
end_tip = tipped_line.tip
assert isinstance(end_tip, geometry.ArrowTip)
assert tipped_line.has_tip()
assert not tipped_line.has_start_tip()
assert tipped_line.submobjects == [end_tip]
assert np.allclose(end_tip.get_tip_point(), logical_end, atol=1e-9)
assert np.allclose(tipped_line.get_end(), logical_end, atol=1e-9)
assert VMobject.get_end(tipped_line)[0] < logical_end[0]
assert end_tip.get_fill_color() == manimlib.BLUE
assert math.isclose(tipped_line.get_length(), 4.0, abs_tol=1e-9)

assert tipped_line.add_tip(at_start=True, length=0.25) is tipped_line
start_tip = tipped_line.start_tip
assert isinstance(start_tip, geometry.ArrowTip)
assert tipped_line.has_start_tip()
assert tipped_line.submobjects == [end_tip, start_tip]
assert np.allclose(start_tip.get_tip_point(), logical_start, atol=1e-9)
assert np.allclose(tipped_line.get_start(), logical_start, atol=1e-9)
assert VMobject.get_start(tipped_line)[0] > logical_start[0]
assert tipped_line.get_tip() is end_tip
assert tipped_line.get_tips().submobjects == [end_tip, start_tip]

popped_tips = tipped_line.pop_tips()
assert isinstance(popped_tips, manimlib.VGroup)
assert popped_tips.submobjects == [end_tip, start_tip]
assert not tipped_line.has_tip()
assert not tipped_line.has_start_tip()
assert tipped_line.submobjects == []
assert np.allclose(tipped_line.get_start(), logical_start, atol=1e-9)
assert np.allclose(tipped_line.get_end(), logical_end, atol=1e-9)
# Reference get_tips is attribute-based, whereas has_tip also requires current
# family membership. Popping deliberately leaves the attributes in place.
assert tipped_line.get_tips().submobjects == [end_tip, start_tip]

curved_tippable = geometry.ArcBetweenPoints(
    [-2.0, 0.0, 0.0],
    [2.0, 0.0, 0.0],
    angle=math.pi,
)
curved_logical_end = curved_tippable.get_end().copy()
curved_points_before = curved_tippable.get_points().copy()
terminal_tangent = curved_points_before[-1] - curved_points_before[-2]
terminal_tangent /= np.linalg.norm(terminal_tangent)
assert curved_tippable.add_tip(length=0.35) is curved_tippable
curved_tip_vector = curved_tippable.tip.get_vector()
curved_tip_vector /= np.linalg.norm(curved_tip_vector)
assert np.dot(curved_tip_vector, terminal_tangent) > 1.0 - 1e-9
assert np.allclose(curved_tippable.get_end(), curved_logical_end, atol=1e-9)
assert not np.array_equal(curved_tippable.get_points(), curved_points_before)
assert curved_tippable.get_arc_length() < geometry.ArcBetweenPoints(
    [-2.0, 0.0, 0.0],
    [2.0, 0.0, 0.0],
    angle=math.pi,
).get_arc_length()

bound_tippable = geometry.Line([0.0, 2.0, 0.0], [3.0, 2.0, 0.0])
bound_tip_scene = Scene()
bound_tip_scene.add(bound_tippable)
bound_tippable_identity = id(bound_tippable)
assert bound_tippable.add_tip(length=0.3) is bound_tippable
assert id(bound_tippable) == bound_tippable_identity
assert bound_tip_scene.get_mobjects()[0] is bound_tippable
assert bound_tippable.tip._is_bound()
assert bound_tippable.submobjects == [bound_tippable.tip]
assert bound_tippable.family_size() == 2

try:
    geometry.Polygon()
except IndexError as error:
    assert "tuple index out of range" in str(error)
else:
    raise AssertionError("an empty Polygon did not preserve the Reference refusal")

try:
    geometry.RegularPolygon(1 << 20)
except ValueError as error:
    assert "compass directions" in str(error).lower()
else:
    raise AssertionError("an over-budget RegularPolygon reached allocation")

for invalid_n, error_type in ((0, ZeroDivisionError), (-1, IndexError), (3.5, TypeError)):
    try:
        geometry.RegularPolygon(invalid_n)
    except error_type:
        pass
    else:
        raise AssertionError(
            f"RegularPolygon({invalid_n!r}) did not preserve the Reference refusal"
        )

quarter_arc = geometry.Arc(
    start_angle=0.0,
    angle=math.pi / 2.0,
    radius=2.0,
    arc_center=[1.0, -1.0, 0.0],
)
assert np.allclose(
    quarter_arc.pfp(0.5),
    [1.0 + math.sqrt(2.0), -1.0 + math.sqrt(2.0), 0.0],
    atol=2e-3,
)

# The rest of Atlas's Arc lineage must be authored portal classes over the
# existing native builders, not generated constructor refusals. Geometry
# queries read current Stage points so transforms cannot leave stale answers.
for name in (
    "ArcBetweenPoints",
    "CurvedArrow",
    "CurvedDoubleArrow",
    "Ellipse",
    "AnnularSector",
    "Sector",
    "Annulus",
):
    assert getattr(manimlib, name) is getattr(geometry, name)
assert issubclass(geometry.ArcBetweenPoints, geometry.Arc)
assert issubclass(geometry.CurvedArrow, geometry.ArcBetweenPoints)
assert issubclass(geometry.CurvedDoubleArrow, geometry.CurvedArrow)
assert issubclass(geometry.Ellipse, geometry.Circle)
assert issubclass(geometry.Sector, geometry.AnnularSector)
assert list(inspect.signature(geometry.ArcBetweenPoints).parameters) == [
    "start",
    "end",
    "angle",
    "kwargs",
]
assert list(inspect.signature(geometry.CurvedArrow).parameters) == [
    "start_point",
    "end_point",
    "kwargs",
]
assert list(inspect.signature(geometry.AnnularSector).parameters) == [
    "angle",
    "start_angle",
    "inner_radius",
    "outer_radius",
    "arc_center",
    "fill_color",
    "fill_opacity",
    "stroke_width",
    "kwargs",
]

assert np.allclose(quarter_arc.get_arc_center(), [1.0, -1.0, 0.0], atol=1e-9)
quarter_start = quarter_arc.get_start_angle()
assert min(abs(quarter_start), abs(quarter_start - math.tau)) < 1e-6
assert math.isclose(quarter_arc.get_stop_angle(), math.pi / 2.0, abs_tol=1e-6)
quarter_arc.shift([2.0, 3.0, 0.0])
assert np.allclose(quarter_arc.get_arc_center(), [3.0, 2.0, 0.0], atol=1e-9)
quarter_arc.rotate(math.pi / 2.0, about_point=quarter_arc.get_arc_center())
assert math.isclose(quarter_arc.get_start_angle(), math.pi / 2.0, abs_tol=1e-6)
assert math.isclose(quarter_arc.get_stop_angle(), math.pi, abs_tol=1e-6)

between = geometry.ArcBetweenPoints(
    [-2.0, -0.5, 0.0],
    [2.0, -0.5, 0.0],
    angle=math.pi / 2.0,
    n_components=5,
    color=manimlib.BLUE,
)
assert between.get_num_curves() == 5
assert np.allclose(between.get_start(), [-2.0, -0.5, 0.0], atol=1e-9)
assert np.allclose(between.get_end(), [2.0, -0.5, 0.0], atol=1e-9)
assert between.get_stroke_color() == manimlib.BLUE

curved_arrow = geometry.CurvedArrow(
    [-2.0, 0.0, 0.0],
    [2.0, 0.0, 0.0],
    angle=-math.pi / 2.0,
    color=manimlib.BLUE,
)
assert curved_arrow.submobjects == [curved_arrow.tip]
assert curved_arrow.tip.has_points()
assert np.min(
    np.linalg.norm(curved_arrow.tip.get_points() - [2.0, 0.0, 0.0], axis=1)
) < 1e-9
curved_double = geometry.CurvedDoubleArrow(
    [-2.0, 0.0, 0.0],
    [2.0, 0.0, 0.0],
    angle=math.pi / 2.0,
    color=manimlib.RED,
)
assert curved_double.submobjects == [curved_double.tip, curved_double.start_tip]
assert all(tip.has_points() for tip in curved_double.submobjects)
assert np.min(
    np.linalg.norm(curved_double.tip.get_points() - [2.0, 0.0, 0.0], axis=1)
) < 1e-9
assert np.min(
    np.linalg.norm(curved_double.start_tip.get_points() - [-2.0, 0.0, 0.0], axis=1)
) < 1e-9

# The highest-demand primitive shelf is authored over Atlas rather than
# schema-generated shells.  Lock its declared hierarchy and signatures before
# checking the live construction and mutation semantics together.
assert geometry.Circle.__bases__ == (geometry.Arc,)
assert geometry.Dot.__bases__ == (geometry.Circle,)
assert geometry.SmallDot.__bases__ == (geometry.Dot,)
assert geometry.Rectangle.__bases__ == (geometry.Polygon,)
assert geometry.Square.__bases__ == (geometry.Rectangle,)
assert geometry.RoundedRectangle.__bases__ == (geometry.Rectangle,)
assert geometry.Polyline.__bases__ == (VMobject,)
assert list(inspect.signature(geometry.Circle).parameters) == [
    "start_angle",
    "stroke_color",
    "kwargs",
]
assert inspect.signature(geometry.Circle).parameters["stroke_color"].default == manimlib.RED
assert list(inspect.signature(geometry.Dot).parameters) == [
    "point",
    "radius",
    "stroke_color",
    "stroke_width",
    "fill_opacity",
    "fill_color",
    "kwargs",
]
dot_parameters = inspect.signature(geometry.Dot).parameters
assert dot_parameters["radius"].default == geometry.DEFAULT_DOT_RADIUS == 0.08
assert dot_parameters["stroke_color"].default == manimlib.BLACK
assert dot_parameters["stroke_width"].default == 0.0
assert dot_parameters["fill_opacity"].default == 1.0
assert dot_parameters["fill_color"].default == manimlib.DEFAULT_MOBJECT_COLOR
assert str(inspect.signature(geometry.SmallDot)) == (
    "(point=array([0., 0., 0.]), radius=0.04, **kwargs)"
)
assert geometry.DEFAULT_SMALL_DOT_RADIUS == 0.04
assert str(inspect.signature(geometry.Rectangle)) == (
    "(width=4.0, height=2.0, **kwargs)"
)
assert str(inspect.signature(geometry.Square)) == (
    "(side_length=2.0, **kwargs)"
)
assert str(inspect.signature(geometry.RoundedRectangle)) == (
    "(width=4.0, height=2.0, corner_radius=0.5, **kwargs)"
)
assert str(inspect.signature(geometry.Polyline)) == "(*vertices, **kwargs)"

arc_center_mover = geometry.Arc(
    start_angle=0.3,
    angle=1.2,
    radius=1.4,
    arc_center=[-0.5, 0.25, 0.0],
    stroke_color=manimlib.BLUE,
    stroke_width=2.0,
)
arc_center_identity = id(arc_center_mover)
arc_center_style = arc_center_mover.get_style()
arc_center_mover.uniforms["anti_alias_width"] = 2.5
assert arc_center_mover.move_arc_center_to([1.25, -0.75, 0.0]) is arc_center_mover
assert id(arc_center_mover) == arc_center_identity
assert np.allclose(arc_center_mover.get_arc_center(), [1.25, -0.75, 0.0])
arc_center_style_after = arc_center_mover.get_style()
assert all(
    np.array_equal(arc_center_style_after[key], value)
    for key, value in arc_center_style.items()
)
assert arc_center_mover.uniforms["anti_alias_width"] == 2.5

circle_target = geometry.Rectangle(width=2.0, height=1.0).shift([0.5, -0.25, 0.0])
circle_surround = geometry.Circle(stroke_width=3.0)
circle_surround_identity = id(circle_surround)
circle_surround.uniforms["depth_test"] = True
assert circle_surround.surround(
    circle_target, dim_to_match=0, stretch=True, buff=0.2
) is circle_surround
assert id(circle_surround) == circle_surround_identity
assert np.allclose(circle_surround.get_center(), circle_target.get_center())
assert np.allclose(
    [circle_surround.get_width(), circle_surround.get_height()],
    [2.4, 1.4],
    atol=1e-6,
)
assert circle_surround.get_stroke_width() == 3.0
assert circle_surround.uniforms["depth_test"] is True

rectangle_surround = geometry.Rectangle(
    width=0.5,
    height=0.25,
    stroke_color=manimlib.BLUE,
    stroke_width=2.0,
)
rectangle_identity = id(rectangle_surround)
rectangle_surround.uniforms["anti_alias_width"] = 3.25
assert rectangle_surround.surround(circle_target, buff=0.15) is rectangle_surround
assert id(rectangle_surround) == rectangle_identity
assert np.allclose(rectangle_surround.get_center(), circle_target.get_center())
assert np.allclose(
    [rectangle_surround.get_width(), rectangle_surround.get_height()],
    [2.3, 1.3],
    atol=1e-6,
)
assert rectangle_surround.get_stroke_color() == manimlib.BLUE
assert rectangle_surround.get_stroke_width() == 2.0
assert rectangle_surround.uniforms["anti_alias_width"] == 3.25

point_target = manimlib.VectorizedPoint([2.0, 3.0, 0.0])
degenerate_surround = geometry.Rectangle().surround(point_target, buff=0.1)
assert np.allclose(degenerate_surround.get_center(), [2.0, 3.0, 0.0])
assert np.allclose(
    [degenerate_surround.get_width(), degenerate_surround.get_height()],
    [0.2, 0.2],
    atol=1e-6,
)

bound_primitive_scene = Scene()
bound_rectangle = geometry.Rectangle(width=0.5, height=0.5)
bound_primitive_scene.add(bound_rectangle)
bound_rectangle_identity = id(bound_rectangle)
assert bound_rectangle._is_bound()
assert bound_rectangle.surround(circle_target, buff=0.05) is bound_rectangle
assert id(bound_rectangle) == bound_rectangle_identity
assert bound_rectangle._is_bound()
assert bound_primitive_scene.mobjects == [bound_rectangle]
assert np.allclose(
    [bound_rectangle.get_width(), bound_rectangle.get_height()],
    [2.1, 1.1],
    atol=1e-6,
)

dot = geometry.Dot([1.0, -2.0, 0.0], radius=0.2)
assert isinstance(dot, geometry.Circle)
assert np.allclose(dot.get_center(), [1.0, -2.0, 0.0])
assert math.isclose(dot.get_radius(), 0.2, abs_tol=1e-6)
assert dot.get_stroke_color() == manimlib.BLACK
assert dot.get_stroke_width() == 0.0
assert dot.get_fill_opacity() == 1.0
assert dot.get_fill_color() == manimlib.DEFAULT_MOBJECT_COLOR
small_dot = geometry.SmallDot([-1.0, 2.0, 0.0])
assert isinstance(small_dot, geometry.Dot)
assert math.isclose(small_dot.get_radius(), 0.04, abs_tol=1e-6)

rectangle = geometry.Rectangle(
    width=3.0,
    height=1.5,
    stroke_color=manimlib.BLUE,
    stroke_width=1.5,
)
assert isinstance(rectangle, geometry.Polygon)
assert np.allclose([rectangle.get_width(), rectangle.get_height()], [3.0, 1.5])
assert len(rectangle.get_vertices()) == 4
square = geometry.Square(side_length=1.25)
assert np.allclose([square.get_width(), square.get_height()], [1.25, 1.25])
rounded = geometry.RoundedRectangle(
    width=3.0,
    height=1.5,
    corner_radius=0.25,
    stroke_color=manimlib.RED,
)
assert np.allclose([rounded.get_width(), rounded.get_height()], [3.0, 1.5])
assert rounded.get_num_curves() > rectangle.get_num_curves()
assert rounded.get_stroke_color() == manimlib.RED
try:
    geometry.RoundedRectangle(corner_radius=math.nan)
except ValueError:
    pass
else:
    raise AssertionError("RoundedRectangle accepted a non-finite corner radius")

polyline = geometry.Polyline(
    [-1.0, 0.0, 0.0],
    [0.0, 1.0, 0.0],
    [1.0, 0.0, 0.0],
    stroke_color=manimlib.BLUE,
    stroke_width=2.0,
)
assert np.allclose(polyline.get_start(), [-1.0, 0.0, 0.0])
assert np.allclose(polyline.get_end(), [1.0, 0.0, 0.0])
assert not np.allclose(polyline.get_start(), polyline.get_end())
assert polyline.get_stroke_color() == manimlib.BLUE
assert geometry.Polyline().get_num_points() == 0

circle_query = geometry.Circle(radius=2.0, arc_center=[1.0, -1.0, 0.0])
assert math.isclose(circle_query.get_radius(), 2.0, abs_tol=1e-6)
assert np.allclose(circle_query.point_at_angle(math.pi / 2.0), [1.0, 1.0, 0.0])
circle_query.scale(1.5)
assert math.isclose(circle_query.get_radius(), 3.0, abs_tol=1e-6)
assert np.allclose(circle_query.point_at_angle(math.pi), [-2.0, -1.0, 0.0])

ellipse = geometry.Ellipse(
    width=4.0,
    height=1.5,
    arc_center=[0.5, -0.25, 0.0],
    start_angle=math.pi / 2.0,
    color=manimlib.BLUE,
)
assert np.allclose([ellipse.get_width(), ellipse.get_height()], [4.0, 1.5], atol=1e-6)
assert np.allclose(ellipse.get_center(), [0.5, -0.25, 0.0], atol=1e-6)
assert np.allclose(ellipse.get_points()[0], [0.5, 0.5, 0.0], atol=1e-6)

annular_sector = geometry.AnnularSector(
    angle=math.pi,
    start_angle=math.pi / 2.0,
    inner_radius=0.5,
    outer_radius=1.5,
    arc_center=[1.0, 0.0, 0.0],
    fill_color=manimlib.RED,
)
assert annular_sector.has_points()
assert annular_sector.get_fill_color() == manimlib.RED
assert math.isclose(annular_sector.get_left()[0], -0.5, abs_tol=2e-3)
assert math.isclose(annular_sector.get_right()[0], 1.0, abs_tol=2e-3)
sector = geometry.Sector(angle=math.pi / 2.0, radius=2.0)
assert sector.has_points()
assert np.min(np.linalg.norm(sector.get_points(), axis=1)) < 1e-9
annulus = geometry.Annulus(
    inner_radius=0.75,
    outer_radius=1.5,
    center=[-0.5, 0.25, 0.0],
    fill_color=manimlib.BLUE,
)
assert math.isclose(annulus.radius, 1.5)
assert len(annulus.get_subpaths()) == 2
assert annulus.get_fill_color() == manimlib.BLUE
assert np.allclose(annulus.get_center(), [-0.5, 0.25, 0.0], atol=1e-6)

try:
    geometry.ArcBetweenPoints([0, 0, 0], [1, 0, 0], n_components=0)
except ValueError as error:
    assert "component" in str(error).lower()
else:
    raise AssertionError("ArcBetweenPoints accepted a zero component budget")
try:
    geometry.Annulus(unrecognized_style=True)
except TypeError as error:
    assert "unrecognized_style" in str(error)
else:
    raise AssertionError("Annulus silently ignored an unknown keyword")

selector_tex = manimlib.Tex(r"\cos(\theta) + \sin(\theta)")
selected_thetas = selector_tex[r"\theta"]
assert len(selected_thetas) == 2
assert all(len(part) > 0 for part in selected_thetas)

brace_module = importlib.import_module("manimlib.mobject.svg.brace")
brace_line = geometry.Line([-1.0, -1.0, 0.0], [1.0, 1.0, 0.0])
line_brace = brace_module.LineBrace(brace_line, buff=0.1)
assert isinstance(line_brace, brace_module.Brace)
brace_tip = line_brace.get_tip()
brace_label = line_brace.get_tex("1")
assert np.dot(brace_label.get_center() - brace_tip, line_brace.get_direction()) > 0

# BraceLabel/BraceText are authored live compositions over Atlas's parametric
# Brace and Scribe's bundled Tex/TexText constructors. The two public children
# keep stable identity across mutation, copying remaps those aliases, and the
# familiar creation animation is a real Choreo composition.
assert brace_module.BraceLabel.__bases__ == (VMobject,)
assert brace_module.BraceText.__bases__ == (brace_module.BraceLabel,)
brace_label_signature = inspect.signature(brace_module.BraceLabel)
assert tuple(brace_label_signature.parameters) == (
    "obj",
    "text",
    "brace_direction",
    "label_scale",
    "label_buff",
    "kwargs",
)
assert np.array_equal(
    brace_label_signature.parameters["brace_direction"].default,
    manimlib.DOWN,
)
assert brace_label_signature.parameters["label_scale"].default == 1.0
assert (
    brace_label_signature.parameters["label_buff"].default
    == manimlib.DEFAULT_MOBJECT_TO_MOBJECT_BUFF
)
assert str(inspect.signature(brace_module.BraceLabel.creation_anim)) == (
    "(self, label_anim=<class 'manimlib.animation.fading.FadeIn'>, "
    "brace_anim=<class 'manimlib.animation.growing.GrowFromCenter'>)"
)
assert str(inspect.signature(brace_module.BraceLabel.shift_brace)) == (
    "(self, obj, **kwargs)"
)
assert str(inspect.signature(brace_module.BraceLabel.change_label)) == (
    "(self, *text, **kwargs)"
)
assert str(inspect.signature(brace_module.BraceLabel.change_brace_label)) == (
    "(self, obj, *text)"
)
assert str(inspect.signature(brace_module.BraceLabel.copy)) == "(self)"
assert brace_module.BraceLabel.label_constructor is manimlib.Tex
assert brace_module.BraceText.label_constructor is manimlib.TexText

label_target = geometry.Rectangle(width=2.0, height=0.75).shift((0.25, 0.5, 0.0))
native_brace_label = brace_module.BraceLabel(
    label_target,
    ("x", "+", "y"),
    brace_direction=manimlib.DOWN,
    label_scale=0.6,
    label_buff=0.15,
    color=manimlib.BLUE,
)
assert native_brace_label.submobjects == [
    native_brace_label.brace,
    native_brace_label.label,
]
assert isinstance(native_brace_label.brace, brace_module.Brace)
assert isinstance(native_brace_label.label, manimlib.Tex)
assert native_brace_label.label.get_tex() == "x + y"
assert np.dot(
    native_brace_label.label.get_center() - native_brace_label.brace.get_tip(),
    native_brace_label.brace.get_direction(),
) > 0
assert all(
    member.get_fill_color() == manimlib.BLUE
    for member in native_brace_label.family_members_with_points()
)
brace_font_size = native_brace_label.brace.font_size
label_font_size = native_brace_label.label.font_size
native_brace_label.scale(0.8)
assert np.isclose(native_brace_label.brace.font_size, 0.8 * brace_font_size)
assert np.isclose(native_brace_label.label.font_size, 0.8 * label_font_size)

creation = native_brace_label.creation_anim()
assert isinstance(creation, manimlib.AnimationGroup)
assert len(creation.animations) == 2
assert isinstance(creation.animations[0], manimlib.GrowFromCenter)
assert creation.animations[0].mobject is native_brace_label.brace
assert isinstance(creation.animations[1], manimlib.FadeIn)
assert creation.animations[1].mobject is native_brace_label.label

native_brace_label_copy = native_brace_label.copy()
assert native_brace_label_copy is not native_brace_label
assert native_brace_label_copy.brace is native_brace_label_copy[0]
assert native_brace_label_copy.label is native_brace_label_copy[1]
assert native_brace_label_copy.brace is not native_brace_label.brace
assert native_brace_label_copy.label is not native_brace_label.label
native_brace_label_copy.shift((1.0, 0.0, 0.0))
assert not np.allclose(
    native_brace_label_copy.brace.get_center(),
    native_brace_label.brace.get_center(),
)

original_brace = native_brace_label.brace
original_label = native_brace_label.label
replacement_target = geometry.Circle(radius=0.5).shift((-1.0, 0.25, 0.0))
assert native_brace_label.shift_brace(replacement_target) is native_brace_label
assert native_brace_label.brace is native_brace_label[0]
assert native_brace_label.brace is not original_brace
assert native_brace_label.label is original_label
assert native_brace_label.label is native_brace_label[1]

shifted_brace = native_brace_label.brace
assert native_brace_label.change_label("z", color=manimlib.RED) is native_brace_label
assert native_brace_label.brace is shifted_brace
assert native_brace_label.label is native_brace_label[1]
assert native_brace_label.label is not original_label
assert native_brace_label.label.get_tex() == "z"
assert all(
    member.get_fill_color() == manimlib.RED
    for member in native_brace_label.label.family_members_with_points()
)

changed_label = native_brace_label.label
assert native_brace_label.change_brace_label(label_target, "q") is native_brace_label
assert native_brace_label.brace is native_brace_label[0]
assert native_brace_label.label is native_brace_label[1]
assert native_brace_label.label is not changed_label
assert native_brace_label.label.get_tex() == "q"

native_brace_text = brace_module.BraceText(
    [geometry.Circle(radius=0.2), geometry.Circle(radius=0.2).shift((1, 0, 0))],
    "native text",
    label_scale=0.75,
)
assert isinstance(native_brace_text.label, manimlib.TexText)
assert native_brace_text.submobjects == [
    native_brace_text.brace,
    native_brace_text.label,
]
try:
    brace_module.BraceLabel(object(), "invalid target")
except TypeError:
    pass
else:
    raise AssertionError("BraceLabel accepted a non-Mobject target")

# Existing Chisel/Scribe semantics are live through the portal, rather than
# being shadowed by schema placeholders. Arc length remains true for either
# curvature sign (BN-03), and Tex selectors consume the native UTF-8 span map.
line = geometry.Line((-1.0, 0.0, 0.0), (1.0, 0.0, 0.0))
assert math.isclose(line.get_arc_length(), 2.0, rel_tol=0.0, abs_tol=1e-9)
assert np.array_equal(line.get_vector(), [2.0, 0.0, 0.0])
assert np.array_equal(line.get_unit_vector(), [1.0, 0.0, 0.0])
assert line.get_angle() == 0.0
assert line.get_slope() == 0.0
assert np.array_equal(line.get_projection([0.25, 7.0, 3.0]), [0.25, 0.0, 0.0])
assert np.array_equal(line.pointify([2.0, 3.0]), [2.0, 3.0, 0.0])
assert line.set_length(4.0) is line
assert math.isclose(line.get_length(), 4.0, rel_tol=0.0, abs_tol=1e-9)
line_start = line.get_start().copy()
assert line.set_angle(math.pi / 2.0) is line
assert np.allclose(line.get_start(), line_start, rtol=0.0, atol=1e-9)
assert np.allclose(line.get_unit_vector(), [0.0, 1.0, 0.0], rtol=0.0, atol=1e-9)

line_style = geometry.Line(
    (-2.0, -1.0, 0.0),
    (2.0, -1.0, 0.0),
    stroke_color=manimlib.RED,
    stroke_width=3.0,
).fix_in_frame()
style_before = line_style.get_style()
uniforms_before = line_style.uniforms.copy()
line_style_identity = id(line_style)
assert line_style.set_points_by_ends(
    (-1.0, 1.0, 0.0),
    (1.0, 1.0, 0.0),
    buff=0.25,
) is line_style
assert id(line_style) == line_style_identity
assert np.allclose(line_style.get_start(), [-0.75, 1.0, 0.0])
assert np.allclose(line_style.get_end(), [0.75, 1.0, 0.0])
style_after = line_style.get_style()
assert style_after.keys() == style_before.keys()
assert all(np.array_equal(style_after[key], value) for key, value in style_before.items())
assert line_style.uniforms.keys() == uniforms_before.keys()
assert all(
    np.array_equal(line_style.uniforms[key], value)
    for key, value in uniforms_before.items()
)

assert line_style.set_path_arc(-math.pi / 2.0) is line_style
assert line_style.path_arc == -math.pi / 2.0
assert line_style.get_num_curves() == 4
assert line_style.get_arc_length() > line_style.get_length()
curved_points_before = line_style.get_points().copy()
curved_path_arc_before = line_style.path_arc
try:
    line_style.set_path_arc(float("nan"))
except ValueError as error:
    assert "arc angle must be finite" in str(error)
else:
    raise AssertionError("non-finite path_arc did not refuse")
assert line_style.path_arc == curved_path_arc_before
assert np.array_equal(line_style.get_points(), curved_points_before)

null_line = geometry.Line((0.0, 0.0, 0.0), (0.0, 0.0, 0.0))
assert null_line.put_start_and_end_on(
    (-1.0, 2.0, 0.0),
    (2.0, 2.0, 0.0),
) is null_line
assert np.array_equal(null_line.get_start(), [-1.0, 2.0, 0.0])
assert np.array_equal(null_line.get_end(), [2.0, 2.0, 0.0])

bound_line = geometry.Line((0.0, -2.0, 0.0), (1.0, -2.0, 0.0))
bound_line_scene = Scene()
bound_line_scene.add(bound_line)
bound_line_identity = id(bound_line)
assert bound_line.set_points_by_ends(
    (-2.0, -2.0, 0.0),
    (2.0, -2.0, 0.0),
    path_arc=math.pi / 3.0,
) is bound_line
assert id(bound_line) == bound_line_identity
assert bound_line_scene.get_mobjects()[0] is bound_line
assert bound_line.get_arc_length() > bound_line.get_length()
assert bound_line.reset_points_around_ends() is bound_line

curved_line = geometry.Line(
    (-1.0, 0.0, 0.0),
    (1.0, 0.0, 0.0),
    path_arc=-math.pi / 2.0,
)
assert curved_line.get_arc_length() > curved_line.get_length()
assert curved_line.get_arc_length() == VMobject.get_arc_length(curved_line, 2)
arclength_curve = VMobject().set_anchors_and_handles(
    [[-1.0, 0.0, 0.0], [1.0, 0.0, 0.0]],
    [[0.0, 1.0, 0.0]],
)
assert np.allclose(arclength_curve.point_from_proportion(0.0), [-1.0, 0.0, 0.0])
assert np.allclose(arclength_curve.point_from_proportion(0.5), [0.0, 0.5, 0.0])
assert np.allclose(arclength_curve.point_from_proportion(1.0), [1.0, 0.0, 0.0])
rotated_line = line.copy().rotate(math.pi / 2.0)
assert np.allclose(rotated_line.get_points()[0], rotated_line.get_start())
rotated_line.get_points()[0] = [3.0, 4.0, 0.0]
assert np.allclose(rotated_line.get_start(), [3.0, 4.0, 0.0])
corners = VMobject().set_points_as_corners(
    [[0.0, 0.0, 0.0], [1.0, 1.0, 0.0], [2.0, 0.0, 0.0]]
)
assert np.allclose(corners.get_points()[::2], [[0, 0, 0], [1, 1, 0], [2, 0, 0]])
corners.reverse_points()
assert np.allclose(corners.get_points()[::2], [[2, 0, 0], [1, 1, 0], [0, 0, 0]])

# VMobject's construction surface now crosses one bounded Chisel seam. Native
# QuadPath computes the complete result before Python commits any structured
# record rows, so refusal is atomic and style lanes keep Reference resize rules.
path = VMobject(
    stroke_color=manimlib.RED,
    stroke_width=7.0,
    fill_color=manimlib.BLUE,
    fill_opacity=0.25,
)
path_defaults = path._style_data().copy()
assert path.start_new_path([0.0, 0.0, 0.0]) is path
assert path.has_new_path_started()
assert np.allclose(path.get_last_point(), [0.0, 0.0, 0.0])
for field in (
    "stroke_rgba",
    "stroke_width",
    "fill_rgba",
    "base_normal",
    "fill_border_width",
):
    assert np.array_equal(path.data[field], path_defaults[field])

terminal_style = path.data.copy()[-1]
assert path.add_line_to([2.0, 0.0, 0.0]) is path
assert np.allclose(
    path.get_points(),
    [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [2.0, 0.0, 0.0]],
)
assert not path.has_new_path_started()
assert np.allclose(path.get_reflection_of_last_handle(), [3.0, 0.0, 0.0])
for field in ("stroke_rgba", "stroke_width", "fill_rgba", "base_normal"):
    assert np.array_equal(path.data[field][1], terminal_style[field])
    assert np.array_equal(path.data[field][2], terminal_style[field])

nth_curve = path.get_nth_curve_points(0)
assert np.shares_memory(nth_curve, path.get_points())
curve_function = path.get_nth_curve_function(0)
nth_curve[1] = [1.0, 1.0, 0.0]
assert np.allclose(curve_function(0.5), [1.0, 0.5, 0.0])
try:
    path.get_nth_curve_points(path.get_num_curves())
except AssertionError:
    pass
else:
    raise AssertionError("get_nth_curve_points accepted an out-of-range curve")

long_line = VMobject(long_lines=True).start_new_path([0.0, 0.0, 0.0])
long_line.add_line_to([2.0, 0.0, 0.0])
assert np.allclose(long_line.get_points()[:, 0], [0.0, 0.5, 1.0, 1.5, 2.0])

null_path = VMobject().start_new_path([1.0, 2.0, 0.0])
null_path.tolerance_for_point_equality = 1e-3
null_before = null_path.data.copy()
null_path.add_line_to([1.0005, 2.0, 0.0], allow_null_line=False)
null_path.add_quadratic_bezier_curve_to(
    [1.0, 2.0, 0.0],
    [1.0005, 2.0, 0.0],
    allow_null_curve=False,
)
assert np.array_equal(null_path.data, null_before)

quadratic = VMobject().start_new_path([0.0, 0.0, 0.0])
quadratic.add_quadratic_bezier_curve_to(
    [0.0, 0.0, 0.0], [2.0, 0.0, 0.0]
)
assert np.allclose(quadratic.get_points()[1], [1.0, 0.0, 0.0])

cubic = VMobject()
assert (
    cubic.add_cubic_bezier_curve(
        [0.0, 0.0, 0.0],
        [0.0, 2.0, 0.0],
        [2.0, 2.0, 0.0],
        [2.0, 0.0, 0.0],
    )
    is cubic
)
assert cubic.get_num_curves() >= 1
assert np.allclose(cubic.get_last_point(), [2.0, 0.0, 0.0])
cubic_before = cubic.data.copy()
try:
    cubic.add_cubic_bezier_curve_to(
        [float("nan"), 0.0, 0.0],
        [3.0, 1.0, 0.0],
        [4.0, 0.0, 0.0],
    )
except ValueError as error:
    assert "quadratics" in str(error).lower() and "cap" in str(error).lower()
else:
    raise AssertionError("cubic construction accepted non-finite geometry")
assert np.array_equal(cubic.data, cubic_before)

smooth = VMobject().start_new_path([0.0, 0.0, 0.0])
assert smooth.add_smooth_curve_to([1.0, 0.0, 0.0]) is smooth
assert smooth.add_smooth_cubic_curve_to([1.5, 1.0, 0.0], [2.0, 0.0, 0.0]) is smooth
assert np.allclose(smooth.get_last_point(), [2.0, 0.0, 0.0])

multi_path = VMobject().start_new_path([0.0, 0.0, 0.0])
multi_path.add_points_as_corners([[1.0, 0.0, 0.0], [1.0, 1.0, 0.0]])
multi_path.start_new_path([2.0, 0.0, 0.0]).add_line_to([3.0, 0.0, 0.0])
first_subpath = multi_path.get_subpaths()[0].copy()
assert not multi_path.is_closed()
assert multi_path.close_path() is multi_path
assert multi_path.is_closed()
assert np.array_equal(multi_path.get_subpaths()[0], first_subpath)
closed_before = multi_path.data.copy()
multi_path.close_path()
assert np.array_equal(multi_path.data, closed_before)

smooth_closed = VMobject().start_new_path([0.0, 0.0, 0.0])
smooth_closed.add_line_to([1.0, 0.0, 0.0]).add_line_to([1.0, 1.0, 0.0])
smooth_closed.close_path(smooth=True)
assert smooth_closed.is_closed()

square_path = VMobject().start_new_path([0.0, 0.0, 0.0])
square_path.add_points_as_corners(
    [[1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0]]
).close_path()
assert np.allclose(square_path.get_area_vector(), [0.0, 0.0, 1.0])
assert np.allclose(square_path.get_unit_normal(), [0.0, 0.0, 1.0])
assert np.allclose(square_path.data["base_normal"][1::2], [0.0, 0.0, 1.0])
square_path.get_points()[2] = [1.0, -1.0, 0.0]
assert np.allclose(
    square_path.get_unit_normal(refresh=True),
    square_path._path_unit_normal(),
)

anchors_path = VMobject().set_points_as_corners(
    [[-1.0, 0.0, 0.0], [1.0, 0.0, 0.0]]
)
anchors_before = anchors_path.data.copy()
try:
    anchors_path.set_anchors_and_handles(
        [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0]],
        [],
    )
except ValueError as error:
    assert "anchor" in str(error).lower() and "handle" in str(error).lower()
else:
    raise AssertionError("mismatched anchors and handles were accepted")
assert np.array_equal(anchors_path.data, anchors_before)
assert (
    anchors_path.set_anchors_and_handles(
        [[0.0, 0.0, 0.0], [1.0, 1.0, 0.0], [2.0, 0.0, 0.0]],
        [[0.5, 1.0, 0.0], [1.5, 1.0, 0.0]],
    )
    is anchors_path
)
assert np.allclose(
    anchors_path.get_points(),
    [
        [0.0, 0.0, 0.0],
        [0.5, 1.0, 0.0],
        [1.0, 1.0, 0.0],
        [1.5, 1.0, 0.0],
        [2.0, 0.0, 0.0],
    ],
)

empty_path = VMobject()
empty_before = empty_path.data.copy()
try:
    empty_path.add_line_to([1.0, 0.0, 0.0])
except ValueError as error:
    assert "path with at least one point" in str(error).lower()
else:
    raise AssertionError("line construction accepted an empty path")
assert np.array_equal(empty_path.data, empty_before)

bound_path_scene = Scene()
bound_path = VMobject()
bound_path_scene.add(bound_path)
bound_path.start_new_path([-1.0, 0.0, 0.0]).add_line_to([1.0, 0.0, 0.0])
bound_path.start_new_path([0.0, 1.0, 0.0]).add_line_to([0.0, 2.0, 0.0])
assert bound_path.get_num_curves() == 3
assert np.allclose(bound_path.get_last_point(), [0.0, 2.0, 0.0])

# The remaining topology operations also route through QuadPath. The public
# threshold controls only the line-vs-arc decision; BN-09 controls density.
equality_path = VMobject()
equality_path.tolerance_for_point_equality = 1e-3
assert equality_path.consider_points_equal([0, 0, 0], [0.0009, -0.0009, 0])
assert not equality_path.consider_points_equal([0, 0, 0], [0.001, 0, 0])

threshold_line = VMobject().start_new_path([0.0, 0.0, 0.0])
assert (
    threshold_line.add_arc_to(
        [1.0, 0.0, 0.0], 5e-4, n_components=1, threshold=1e-3
    )
    is threshold_line
)
assert np.allclose(threshold_line.get_points()[1], [0.5, 0.0, 0.0])

threshold_arc = VMobject().start_new_path([0.0, 0.0, 0.0])
threshold_arc.add_arc_to(
    [1.0, 0.0, 0.0], 5e-4, n_components=1, threshold=1e-4
)
assert not np.allclose(threshold_arc.get_points()[1], [0.5, 0.0, 0.0])

dense_arc = VMobject().start_new_path([0.0, 0.0, 0.0])
dense_arc.add_arc_to([1.0, 1.0, 0.0], math.pi / 2.0)
assert dense_arc.get_num_curves() == 4
explicit_arc = VMobject().start_new_path([0.0, 0.0, 0.0])
explicit_arc.add_arc_to([1.0, 1.0, 0.0], math.pi / 2.0, n_components=2)
assert explicit_arc.get_num_curves() == 2
for bad_kwargs, message in (
    ({"n_components": 0}, "component"),
    ({"threshold": float("nan")}, "threshold"),
    ({"threshold": -1.0}, "threshold"),
):
    refusal_arc = VMobject().start_new_path([0.0, 0.0, 0.0])
    refusal_before = refusal_arc.data.copy()
    try:
        refusal_arc.add_arc_to([1.0, 0.0, 0.0], 5e-4, **bad_kwargs)
    except ValueError as error:
        assert message in str(error).lower()
    else:
        raise AssertionError(f"add_arc_to accepted {bad_kwargs}")
    assert np.array_equal(refusal_arc.data, refusal_before)

subpath = VMobject()
first_run = np.array(
    [[0.0, 0.0, 0.0], [0.5, 0.5, 0.0], [1.0, 0.0, 0.0]]
)
assert subpath.add_subpath(first_run) is subpath
assert np.allclose(subpath.get_points(), first_run)
subpath.add_subpath(
    [[1.0, 0.0, 0.0], [1.5, -0.5, 0.0], [2.0, 0.0, 0.0]]
)
assert len(subpath.get_subpaths()) == 1
subpath.add_subpath(
    [[3.0, 0.0, 0.0], [3.5, 0.5, 0.0], [4.0, 0.0, 0.0]]
)
assert len(subpath.get_subpaths()) == 2
subpath_before = subpath.data.copy()
assert subpath.add_subpath([]) is subpath
assert np.array_equal(subpath.data, subpath_before)
try:
    subpath.add_subpath([[5.0, 0.0, 0.0], [6.0, 0.0, 0.0]])
except ValueError as error:
    assert "odd length" in str(error).lower()
else:
    raise AssertionError("add_subpath accepted an even shared-anchor run")
assert np.array_equal(subpath.data, subpath_before)

anchor_modes = VMobject().set_points_as_corners(
    [[0.0, 0.0, 0.0], [1.0, 1.0, 0.0], [2.0, 0.0, 0.0]]
)
anchor_modes.data["stroke_width"][:, 0] = [1.0, 2.0, 3.0, 4.0, 5.0]
anchor_mode_widths = anchor_modes.data["stroke_width"].copy()
assert not anchor_modes.is_smooth()
assert anchor_modes.change_anchor_mode("approx_smooth") is anchor_modes
assert anchor_modes.is_smooth()
assert np.array_equal(anchor_modes.data["stroke_width"], anchor_mode_widths)
assert anchor_modes.make_jagged(recurse=False) is anchor_modes
assert not anchor_modes.is_smooth()
assert anchor_modes.make_approximately_smooth(recurse=False) is anchor_modes
assert anchor_modes.is_smooth()
anchor_before = anchor_modes.data.copy()
try:
    anchor_modes.change_anchor_mode("mystery")
except ValueError as error:
    assert "anchor mode" in str(error).lower()
else:
    raise AssertionError("change_anchor_mode accepted an unknown mode")
assert np.array_equal(anchor_modes.data, anchor_before)

mode_parent = VMobject().set_points_as_corners(
    [[0.0, 0.0, 0.0], [1.0, 1.0, 0.0], [2.0, 0.0, 0.0]]
)
mode_child = VMobject().set_points_as_corners(
    [[0.0, 2.0, 0.0], [1.0, 3.0, 0.0], [2.0, 2.0, 0.0]]
)
mode_parent.add(mode_child)
mode_child.make_approximately_smooth(recurse=False)
child_smooth_points = mode_child.get_points().copy()
mode_parent.make_jagged(recurse=False)
assert np.array_equal(mode_child.get_points(), child_smooth_points)
mode_parent.make_jagged(recurse=True)
assert not np.array_equal(mode_child.get_points(), child_smooth_points)

# VMobject's pre-pass honors recurse, while the Reference base reversal still
# reverses every family row. The odd normal rows reveal the phase boundary.
reverse_parent = VMobject().set_points_as_corners(
    [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0]]
)
reverse_child = VMobject().set_points_as_corners(
    [[2.0, 0.0, 0.0], [3.0, 0.0, 0.0]]
)
reverse_parent.add(reverse_child)
reverse_parent.data["base_normal"][:] = [0.0, 0.0, 1.0]
reverse_child.data["base_normal"][:] = [0.0, 0.0, 1.0]
reverse_parent.data["stroke_width"][:, 0] = [1.0, 2.0, 3.0]
reverse_child.data["stroke_width"][:, 0] = [4.0, 5.0, 6.0]
assert reverse_parent.reverse_points(recurse=False) is reverse_parent
assert np.allclose(reverse_parent.get_points()[::2, 0], [1.0, 0.0])
assert np.allclose(reverse_child.get_points()[::2, 0], [3.0, 2.0]), (
    reverse_child.get_points()
)
assert np.allclose(reverse_parent.data["base_normal"][1], [0.0, 0.0, -1.0])
assert np.allclose(reverse_child.data["base_normal"][1], [0.0, 0.0, 1.0])
assert np.allclose(reverse_parent.data["stroke_width"][:, 0], [3.0, 2.0, 1.0])
assert np.allclose(reverse_child.data["stroke_width"][:, 0], [6.0, 5.0, 4.0])

# The scene-bound branch must expose the same two-phase semantics through one
# shared native Stage, not merely through the detached Python fallback above.
bound_reverse_scene = Scene()
bound_reverse_parent = VMobject().set_points_as_corners(
    [[0.0, 1.0, 0.0], [1.0, 1.0, 0.0]]
)
bound_reverse_child = VMobject().set_points_as_corners(
    [[2.0, 1.0, 0.0], [3.0, 1.0, 0.0]]
)
bound_reverse_parent.add(bound_reverse_child)
bound_reverse_parent.data["base_normal"][:] = [0.0, 0.0, 1.0]
bound_reverse_child.data["base_normal"][:] = [0.0, 0.0, 1.0]
bound_reverse_scene.add(bound_reverse_parent)
bound_reverse_parent.reverse_points(recurse=False)
assert np.allclose(bound_reverse_parent.get_points()[::2, 0], [1.0, 0.0])
assert np.allclose(bound_reverse_child.get_points()[::2, 0], [3.0, 2.0])
assert np.allclose(
    bound_reverse_parent.data["base_normal"][1], [0.0, 0.0, -1.0]
)
assert np.allclose(
    bound_reverse_child.data["base_normal"][1], [0.0, 0.0, 1.0]
)

# Marionette's production partial-reveal operation is the one portal
# VMobjects use too. A subcurve remains a full-size, independent record copy:
# only its point/joint-angle lanes change, while gradient/style data survives.
partial_source = VMobject().set_points(
    np.array(
        [
            [0.0, 0.0, 0.0],
            [0.5, 1.0, 0.0],
            [1.0, 0.0, 0.0],
            [2.0, -1.0, 0.0],
            [3.0, 0.0, 0.0],
        ],
        dtype=np.float32,
    )
)
partial_source.data["stroke_width"][:, 0] = np.linspace(1.0, 5.0, 5)
partial_source.data["fill_rgba"][:] = np.linspace(
    [1.0, 0.0, 0.0, 0.2], [0.0, 0.0, 1.0, 0.8], 5
)
partial_style = {
    "stroke_width": partial_source.data["stroke_width"].copy(),
    "fill_rgba": partial_source.data["fill_rgba"].copy(),
}
partial_start = partial_source.quick_point_from_proportion(0.25)
partial_end = partial_source.quick_point_from_proportion(0.75)
partial_copy = partial_source.get_subcurve(0.25, 0.75)
assert partial_copy is not partial_source
assert partial_copy.get_num_points() == partial_source.get_num_points()
assert np.allclose(partial_copy.get_points()[0], partial_start)
assert np.allclose(partial_copy.get_points()[-1], partial_end)
assert np.array_equal(partial_copy.data["stroke_width"], partial_style["stroke_width"])
assert np.array_equal(partial_copy.data["fill_rgba"], partial_style["fill_rgba"])
partial_copy.get_points()[0] = [-9.0, -9.0, 0.0]
assert not np.allclose(partial_source.get_points()[0], partial_copy.get_points()[0])

full_partial = VMobject().set_points_as_corners(
    [[-1.0, 0.0, 0.0], [1.0, 0.0, 0.0]]
)
assert full_partial.pointwise_become_partial(partial_source, 0.0, 1.0) is full_partial
assert np.allclose(full_partial.get_points(), partial_source.get_points())
same_partial = partial_source.copy()
assert same_partial.pointwise_become_partial(same_partial, 0.1, 0.9) is same_partial
try:
    partial_source.pointwise_become_partial(Mobject(), 0.0, 1.0)
except AssertionError as error:
    assert "VMobject" in str(error)
else:
    raise AssertionError("partial slicing accepted a non-VMobject source")
try:
    partial_source.get_subcurve(float("nan"), 0.5)
except ValueError as error:
    assert "finite" in str(error)
else:
    raise AssertionError("partial slicing accepted a non-finite bound")

partial_scene = Scene()
bound_partial_source = partial_source.copy()
bound_partial_target = partial_source.copy()
partial_scene.add(bound_partial_source, bound_partial_target)
bound_start = bound_partial_source.quick_point_from_proportion(0.125)
bound_end = bound_partial_source.quick_point_from_proportion(0.625)
assert (
    bound_partial_target.pointwise_become_partial(
        bound_partial_source, 0.125, 0.625
    )
    is bound_partial_target
)
assert np.allclose(bound_partial_target.get_points()[0], bound_start)
assert np.allclose(bound_partial_target.get_points()[-1], bound_end)

vectorized = importlib.import_module("manimlib.mobject.types.vectorized_mobject")
assert vectorized.VectorizedPoint is manimlib.VectorizedPoint
assert vectorized.CurvesAsSubmobjects is manimlib.CurvesAsSubmobjects
assert vectorized.DashedVMobject is manimlib.DashedVMobject
assert vectorized.VHighlight is manimlib.VHighlight
assert vectorized.VectorizedPoint.__mro__[:4] == (
    vectorized.VectorizedPoint,
    manimlib.Point,
    VMobject,
    Mobject,
)
assert vectorized.CurvesAsSubmobjects.__mro__[:3] == (
    vectorized.CurvesAsSubmobjects,
    manimlib.VGroup,
    VMobject,
)
assert list(inspect.signature(vectorized.VectorizedPoint).parameters) == [
    "location",
    "color",
    "fill_opacity",
    "stroke_width",
    "kwargs",
]
assert list(inspect.signature(vectorized.CurvesAsSubmobjects).parameters) == [
    "vmobject",
    "kwargs",
]
assert list(inspect.signature(vectorized.DashedVMobject).parameters) == [
    "vmobject",
    "num_dashes",
    "positive_space_ratio",
    "kwargs",
]
assert list(inspect.signature(vectorized.VHighlight).parameters) == [
    "vmobject",
    "n_layers",
    "color_bounds",
    "max_stroke_addition",
]
assert list(inspect.signature(VMobject.get_subcurve).parameters) == ["self", "a", "b"]
assert list(inspect.signature(VMobject.pointwise_become_partial).parameters) == [
    "self",
    "vmobject",
    "a",
    "b",
]

vectorized_point = vectorized.VectorizedPoint(
    [1.5, -2.0, 0.25], color=manimlib.RED
)
assert vectorized_point.get_num_points() == 1
assert np.allclose(vectorized_point.get_location(), [1.5, -2.0, 0.25])
assert np.allclose(vectorized_point.get_start(), vectorized_point.get_end())
assert vectorized_point.get_stroke_width() == 0.0
assert vectorized_point.get_fill_opacity() == 0.0
vectorized_point.shift([0.5, 1.0, -0.25])
assert np.allclose(vectorized_point.get_location(), [2.0, -1.0, 0.0])

curve_parts = vectorized.CurvesAsSubmobjects(partial_source)
assert len(curve_parts) == partial_source.get_num_curves()
assert all(type(part) is VMobject for part in curve_parts)
assert all(part.get_num_points() == 3 for part in curve_parts)
assert all(part.get_stroke_width() == partial_source.get_stroke_width() for part in curve_parts)

# BN-03: four dashes on a path whose two curves have very different lengths
# still measure equally. Each child is a full native partial copy, not a
# Python-reconstructed path, and the root retains the source's own style.
uneven_dash_source = VMobject(stroke_color=manimlib.BLUE, stroke_width=3.0).set_points(
    np.array(
        [
            [0.0, 0.0, 0.0],
            [0.05, 0.0, 0.0],
            [0.1, 0.0, 0.0],
            [3.0, 0.0, 0.0],
            [6.0, 0.0, 0.0],
        ]
    )
)
dashed = vectorized.DashedVMobject(
    uneven_dash_source, num_dashes=4, positive_space_ratio=0.5
)
assert len(dashed) == 4
dash_lengths = [dash.get_arc_length() for dash in dashed]
assert np.allclose(dash_lengths, dash_lengths[0], rtol=0.0, atol=1e-6), dash_lengths
assert dashed.get_stroke_color() == manimlib.BLUE
assert all(dash.get_stroke_width() == 3.0 for dash in dashed)
assert len(vectorized.DashedVMobject(uneven_dash_source, num_dashes=0)) == 0
try:
    vectorized.DashedVMobject(uneven_dash_source, num_dashes=4097)
except ValueError as error:
    assert "4096" in str(error)
else:
    raise AssertionError("DashedVMobject exceeded the native child budget")
try:
    vectorized.DashedVMobject(
        uneven_dash_source, num_dashes=4, positive_space_ratio=0.0
    )
except ValueError as error:
    assert "positive-space ratio" in str(error)
else:
    raise AssertionError("DashedVMobject accepted a zero drawn ratio")

highlight_source = manimlib.VGroup(
    manimlib.Circle(radius=0.4, stroke_width=2.0, fill_opacity=0.5),
    manimlib.Square(side_length=0.5, stroke_width=3.0, fill_opacity=0.75),
)
highlight = vectorized.VHighlight(highlight_source, n_layers=5)
assert len(highlight) == 5
assert all(len(layer) == 2 for layer in highlight)
assert all(
    member.get_fill_opacity() == 0.0
    for layer in highlight
    for member in layer.family_members_with_points()
)
assert highlight[0][0].get_stroke_width() == 7.0
assert highlight[-1][0].get_stroke_width() == 3.0
assert highlight[0][0].get_stroke_color() == manimlib.GREY_E
assert highlight[-1][0].get_stroke_color() == manimlib.GREY_C
assert len(vectorized.VHighlight(highlight_source, n_layers=0)) == 0

# VMobject topology is read directly from the writable shared-anchor record
# view, while insertion delegates its distribution/subdivision work to
# Chisel and commits through the ordinary RecordBuffer point-write seam.
topology = VMobject().set_points(
    np.array(
        [
            [0.0, 0.0, 0.0],
            [0.5, 1.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [3.0, 0.0, 0.0],
            [3.5, -1.0, 0.0],
            [4.0, 0.0, 0.0],
        ]
    )
)
anchors_and_handles = topology.get_anchors_and_handles()
assert len(anchors_and_handles) == 3
assert all(np.shares_memory(part, topology.get_points()) for part in anchors_and_handles)
assert np.allclose(topology.get_start_anchors(), topology.get_points()[0:-1:2])
assert np.allclose(topology.get_end_anchors(), topology.get_points()[2::2])
live_anchors = topology.get_anchors()
assert np.shares_memory(live_anchors, topology.get_points())
live_anchors[0] = [-1.0, 0.0, 0.0]
assert np.allclose(topology.get_points()[0], [-1.0, 0.0, 0.0])
bezier_tuples = list(topology.get_bezier_tuples())
assert len(bezier_tuples) == 3
assert all(np.shares_memory(curve, topology.get_points()) for curve in bezier_tuples)
assert np.array_equal(topology.get_subpath_end_indices(), [2, 6])
subpaths = topology.get_subpaths()
assert [len(path) for path in subpaths] == [3, 3]
assert all(np.shares_memory(path, topology.get_points()) for path in subpaths)
assert np.array_equal(
    topology.get_subpath_end_indices_from_points(topology.get_points()),
    [2, 6],
)
assert len(topology.get_subpaths_from_points(np.zeros((0, 3)))) == 0

single_insert = topology.insert_n_curves_to_point_list(
    2, np.array([[2.0, 3.0, 0.0]], dtype=np.float32)
)
assert single_insert.dtype == np.float64
assert single_insert.shape == (5, 3)
assert np.allclose(single_insert, [[2.0, 3.0, 0.0]] * 5)

insertion = VMobject().set_points(
    np.array(
        [
            [0.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [2.0, 0.0, 0.0],
            [2.25, 0.5, 0.0],
            [2.5, 0.0, 0.0],
        ]
    )
)
insertion.data["stroke_width"][:, 0] = [1.0, 2.0, 3.0, 4.0, 5.0]
insertion_old_view = insertion.get_points()
insertion_arc_length = insertion.get_arc_length()
assert insertion.insert_n_curves(2) is insertion
assert insertion.get_num_curves() == 4
assert insertion_old_view.shape == (5, 3)
insertion_old_view[0] = [99.0, 99.0, 99.0]
assert not np.allclose(insertion.get_points()[0], insertion_old_view[0])
assert np.allclose(
    insertion.data["stroke_width"][:, 0],
    [1.0, 1.0, 2.0, 2.0, 3.0, 3.0, 4.0, 4.0, 5.0],
)
assert math.isclose(
    insertion.get_arc_length(), insertion_arc_length, rel_tol=0.0, abs_tol=2e-7
)
assert np.isfinite(insertion.data["joint_angle"]).all()

same_length_view = insertion.get_points()
insertion.insert_n_curves(0)
same_length_view[0] = [-2.0, 0.0, 0.0]
assert np.allclose(insertion.get_points()[0], [-2.0, 0.0, 0.0])

family_curve_a = VMobject().set_points_as_corners([[0, 0, 0], [1, 0, 0]])
family_curve_b = VMobject().set_points_as_corners([[0, 1, 0], [2, 1, 0]])
curve_family = manimlib.VGroup(family_curve_a, family_curve_b)
curve_family.insert_n_curves(1, recurse=False)
assert [mob.get_num_curves() for mob in curve_family] == [1, 1]
curve_family.insert_n_curves(1)
assert [mob.get_num_curves() for mob in curve_family] == [2, 2]

bound_insertion = VMobject().set_points_as_corners([[0, 0, 0], [3, 0, 0]])
bound_insertion_scene = Scene()
bound_insertion_scene.add(bound_insertion)
bound_insertion.insert_n_curves(2)
assert bound_insertion.get_num_curves() == 3

insertion_before_refusal = insertion.get_points().copy()
try:
    insertion.insert_n_curves_to_point_list(1, np.zeros((2, 3)))
except ValueError as error:
    assert "odd" in str(error).lower() or "even" in str(error).lower()
else:
    raise AssertionError("an even shared-anchor run reached Chisel insertion")
try:
    insertion.insert_n_curves(sys.maxsize)
except ValueError as error:
    assert "above the 65536 cap" in str(error).lower()
else:
    raise AssertionError("an over-budget curve insertion mutated the portal")
assert np.array_equal(insertion.get_points(), insertion_before_refusal)

# Alignment, whole-record append, and sharp subdivision reuse the existing
# Marionette/Chisel authorities rather than schema-generated placeholders.
# Appending copies every source lane onto the exact appended topology tail.
append_source = VMobject().set_points_as_corners(
    [[2.0, 1.0, 0.0], [3.0, 2.0, 0.0]]
)
append_source.data["stroke_width"][:, 0] = [7.0, 8.0, 9.0]
append_source.data["stroke_rgba"][:] = [0.2, 0.4, 0.6, 0.8]
append_source.data["fill_rgba"][:] = [0.7, 0.5, 0.3, 0.1]
append_target = VMobject().set_points_as_corners(
    [[-2.0, -1.0, 0.0], [-1.0, -1.0, 0.0]]
)
append_old_view = append_target.get_points()
assert append_target.append_vectorized_mobject(append_source) is append_target
assert append_target.get_num_points() == 7
assert np.array_equal(
    append_target.data[-append_source.get_num_points() :], append_source.data
)
assert append_old_view.shape == (3, 3)
append_old_view[0] = [99.0, 99.0, 99.0]
assert not np.allclose(append_target.get_points()[0], append_old_view[0])

# Detached alignment mutates both endpoints, preserves every non-geometry
# lane by proportional record resize, and invalidates only resized views.
align_small = VMobject().set_points(
    [[0.0, 0.0, 0.0], [1.0, 1.0, 0.0], [2.0, 0.0, 0.0]]
)
align_large = VMobject().set_points_as_corners(
    [[0.0, 2.0, 0.0], [1.0, 2.0, 0.0], [2.0, 2.0, 0.0], [3.0, 2.0, 0.0]]
)
align_small.data["stroke_width"][:, 0] = [1.0, 2.0, 3.0]
align_small_before = align_small.data.copy()
align_small_old_view = align_small.get_points()
assert align_small.align_points(align_large) is align_small
assert align_small.get_num_points() == align_large.get_num_points() == 7
assert np.allclose(align_small.get_points()[0], [0.0, 0.0, 0.0])
assert np.allclose(align_small.get_points()[-1], [2.0, 0.0, 0.0])
assert np.array_equal(
    align_small.data["stroke_width"],
    manimlib.resize_preserving_order(align_small_before, 7)["stroke_width"],
)
assert align_small_old_view.shape == (3, 3)
assert np.isfinite(align_small.data["joint_angle"]).all()
assert np.isfinite(align_large.data["joint_angle"]).all()

# The receiver's public tolerance controls which near-null curve may receive
# an inserted split. This is the parameterized native seam, not Python math.
def aligned_with_tolerance(tolerance):
    left = VMobject().set_points(
        [
            [0.0, 0.0, 0.0],
            [0.01, 0.0, 0.0],
            [10.0, 0.0, 0.0],
            [10.5, 0.0, 0.0],
            [11.0, 0.0, 0.0],
        ]
    )
    right = VMobject().set_points_as_corners(
        [[0.0, 1.0, 0.0], [1.0, 1.0, 0.0], [2.0, 1.0, 0.0], [3.0, 1.0, 0.0]]
    )
    left.tolerance_for_point_equality = tolerance
    left.align_points(right)
    return left.get_points().copy()


default_tolerance_alignment = aligned_with_tolerance(1e-8)
wide_tolerance_alignment = aligned_with_tolerance(0.1)
assert not np.array_equal(default_tolerance_alignment, wide_tolerance_alignment)
assert np.allclose(
    wide_tolerance_alignment[:3],
    [[0.0, 0.0, 0.0], [0.01, 0.0, 0.0], [10.0, 0.0, 0.0]],
)

# Same-Scene alignment uses the shared Stage directly; cross-Scene alignment
# uses native temporary entries and commits each proven point run back to its
# own live Stage. Both retain the public operation's scene independence.
bound_align_scene = Scene()
bound_align_small = VMobject().set_points_as_corners(
    [[0.0, 3.0, 0.0], [1.0, 3.0, 0.0]]
)
bound_align_large = VMobject().set_points_as_corners(
    [[0.0, 4.0, 0.0], [1.0, 4.0, 0.0], [2.0, 4.0, 0.0]]
)
bound_align_scene.add(bound_align_small, bound_align_large)
bound_align_small.align_points(bound_align_large)
assert bound_align_small.get_num_points() == bound_align_large.get_num_points() == 5

cross_align_left = VMobject().set_points_as_corners(
    [[0.0, 5.0, 0.0], [1.0, 5.0, 0.0]]
)
cross_align_right = VMobject().set_points_as_corners(
    [[0.0, 6.0, 0.0], [1.0, 6.0, 0.0], [2.0, 6.0, 0.0]]
)
cross_align_left_scene = Scene().add(cross_align_left)
cross_align_right_scene = Scene().add(cross_align_right)
cross_align_left.align_points(cross_align_right)
assert cross_align_left.get_num_points() == cross_align_right.get_num_points() == 5
assert cross_align_left in cross_align_left_scene.mobjects
assert cross_align_right in cross_align_right_scene.mobjects

align_refusal = VMobject().set_points_as_corners(
    [[0.0, 7.0, 0.0], [1.0, 7.0, 0.0]]
)
align_refusal_before = align_refusal.data.copy()
try:
    align_refusal.align_points(Mobject().set_points([[0.0, 0.0, 0.0]]))
except RuntimeError as error:
    assert "schema" in str(error).lower()
else:
    raise AssertionError("VMobject alignment accepted a base-record peer")
assert np.array_equal(align_refusal.data, align_refusal_before)

sharp = VMobject().set_points(
    [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0]]
)
sharp.data["stroke_width"][:, 0] = [2.0, 4.0, 6.0]
sharp_before = sharp.data.copy()
sharp_old_view = sharp.get_points()
sharp_length = sharp.get_arc_length()
assert sharp.subdivide_sharp_curves(30 * manimlib.DEG, recurse=False) is sharp
assert sharp.get_num_curves() == 4
assert sharp_old_view.shape == (3, 3)
assert np.allclose(sharp.get_points()[0], [0.0, 0.0, 0.0])
assert np.allclose(sharp.get_points()[-1], [1.0, 1.0, 0.0])
assert np.array_equal(
    sharp.data["stroke_width"],
    manimlib.resize_preserving_order(sharp_before, 9)["stroke_width"],
)
assert math.isclose(sharp.get_arc_length(), sharp_length, rel_tol=0.0, abs_tol=2e-7)

sharp_parent = VMobject().set_points(
    [[0.0, 2.0, 0.0], [1.0, 2.0, 0.0], [1.0, 3.0, 0.0]]
)
sharp_child = VMobject().set_points(
    [[2.0, 2.0, 0.0], [3.0, 2.0, 0.0], [3.0, 3.0, 0.0]]
)
sharp_parent.add(sharp_child)
sharp_parent.subdivide_sharp_curves(recurse=False)
assert sharp_parent.get_num_curves() == 4
assert sharp_child.get_num_curves() == 1
sharp_parent.subdivide_sharp_curves(recurse=True)
assert sharp_child.get_num_curves() == 4

# Plan-before-commit matters when a later descendant breaches the native
# subdivision budget: an earlier valid family member must remain unchanged.
atomic_parent = VMobject().set_points_as_corners(
    [[0.0, 8.0, 0.0], [1.0, 8.0, 0.0]]
)
atomic_child = VMobject().set_points(
    [[2.0, 8.0, 0.0], [3.0, 8.0, 0.0], [3.0, 9.0, 0.0]]
)
atomic_parent.add(atomic_child)
atomic_parent_before = atomic_parent.data.copy()
atomic_child_before = atomic_child.data.copy()
try:
    atomic_parent.subdivide_sharp_curves(float.fromhex("0x0.0000000000001p-1022"))
except ValueError as error:
    assert "65536" in str(error) or "subdivision" in str(error).lower()
else:
    raise AssertionError("sharp subdivision exceeded the native curve budget")
assert np.array_equal(atomic_parent.data, atomic_parent_before)
assert np.array_equal(atomic_child.data, atomic_child_before)

bound_sharp_scene = Scene()
bound_sharp = VMobject().set_points(
    [[-1.0, 5.0, 0.0], [0.0, 5.0, 0.0], [0.0, 6.0, 0.0]]
)
bound_sharp_scene.add(bound_sharp)
bound_sharp.subdivide_sharp_curves(recurse=False)
assert bound_sharp.get_num_curves() == 4

# Smooth construction and condition/intersection subdivision are the sibling
# Chisel-backed VMobject operations. Callback arguments retain the Reference's
# live float32 NumPy row shape, callbacks run exactly once per original curve,
# and the whole family is planned before the first RecordBuffer mutation.
smooth_points = np.array(
    [[-2.0, 0.0, 0.0], [-1.0, 1.0, 0.0], [0.0, 0.0, 0.0], [1.0, 1.0, 0.0]]
)
for approx in (True, False):
    corner_points = VMobject().set_points_as_corners(smooth_points).get_points().copy()
    manual_smooth = VMobject().set_points_as_corners(smooth_points)
    manual_smooth.make_smooth(approx=approx)
    smooth = VMobject(stroke_width=7.0)
    assert smooth.set_points_smoothly(smooth_points, approx=approx) is smooth
    assert np.array_equal(smooth.get_points(), manual_smooth.get_points())
    assert np.allclose(smooth.data["stroke_width"], 7.0)
    assert not np.array_equal(smooth.get_points()[1::2], corner_points[1::2])

assert str(inspect.signature(VMobject.set_points_smoothly)) == (
    "(self, points, approx=True)"
)
assert str(inspect.signature(VMobject.subdivide_curves_by_condition)) == (
    "(self, tuple_to_subdivisions, recurse=True)"
)
assert str(inspect.signature(VMobject.subdivide_intersections)) == (
    "(self, recurse=True, n_subdivisions=1)"
)

conditional = VMobject().set_points(
    [
        [0.0, 0.0, 0.0],
        [0.5, 1.0, 0.0],
        [1.0, 0.0, 0.0],
        [1.5, -1.0, 0.0],
        [2.0, 0.0, 0.0],
    ]
)
conditional.data["stroke_width"][:, 0] = [1.0, 2.0, 3.0, 4.0, 5.0]
conditional_before = conditional.data.copy()
conditional_length = conditional.get_arc_length()
callback_rows = []


def conditional_counts(b0, b1, b2):
    callback_rows.append((b0.copy(), b1.copy(), b2.copy()))
    assert all(isinstance(point, np.ndarray) for point in (b0, b1, b2))
    assert all(point.dtype == np.float32 for point in (b0, b1, b2))
    return np.int64(len(callback_rows))


assert (
    conditional.subdivide_curves_by_condition(conditional_counts, recurse=False)
    is conditional
)
assert len(callback_rows) == 2
assert conditional.get_num_curves() == 5
assert np.array_equal(
    conditional.data["stroke_width"],
    manimlib.resize_preserving_order(conditional_before, 11)["stroke_width"],
)
assert math.isclose(
    conditional.get_arc_length(), conditional_length, rel_tol=0.0, abs_tol=2e-7
)

condition_parent = VMobject().set_points_as_corners(
    [[0.0, 10.0, 0.0], [1.0, 10.0, 0.0]]
)
condition_child = VMobject().set_points_as_corners(
    [[2.0, 10.0, 0.0], [3.0, 10.0, 0.0]]
)
condition_parent.add(condition_child)
condition_parent.subdivide_curves_by_condition(lambda *curve: 1, recurse=False)
assert condition_parent.get_num_curves() == 2
assert condition_child.get_num_curves() == 1
condition_parent.subdivide_curves_by_condition(lambda *curve: 1)
assert condition_parent.get_num_curves() == 4
assert condition_child.get_num_curves() == 2

atomic_condition_parent = VMobject().set_points_as_corners(
    [[0.0, 11.0, 0.0], [1.0, 11.0, 0.0]]
)
atomic_condition_child = VMobject().set_points_as_corners(
    [[2.0, 11.0, 0.0], [3.0, 11.0, 0.0]]
)
atomic_condition_parent.add(atomic_condition_child)
atomic_condition_parent_before = atomic_condition_parent.data.copy()
atomic_condition_child_before = atomic_condition_child.data.copy()
atomic_callback_calls = 0


def failing_condition(*curve):
    global atomic_callback_calls
    atomic_callback_calls += 1
    if atomic_callback_calls == 2:
        raise LookupError("second family callback refusal")
    return 1


try:
    atomic_condition_parent.subdivide_curves_by_condition(failing_condition)
except LookupError as error:
    assert "second family callback refusal" in str(error)
else:
    raise AssertionError("a callback failure partially subdivided a family")
assert atomic_callback_calls == 2
assert np.array_equal(atomic_condition_parent.data, atomic_condition_parent_before)
assert np.array_equal(atomic_condition_child.data, atomic_condition_child_before)

for refused_count, exception in [(1.5, TypeError), (10**100, ValueError)]:
    refused = VMobject().set_points_as_corners(
        [[0.0, 12.0, 0.0], [1.0, 12.0, 0.0]]
    )
    refused_before = refused.data.copy()
    try:
        refused.subdivide_curves_by_condition(lambda *curve: refused_count)
    except exception:
        pass
    else:
        raise AssertionError(f"invalid subdivision count {refused_count!r} succeeded")
    assert np.array_equal(refused.data, refused_before)

negative_count = VMobject().set_points_as_corners(
    [[0.0, 13.0, 0.0], [1.0, 13.0, 0.0]]
)
negative_view = negative_count.get_points()
negative_count.subdivide_curves_by_condition(lambda *curve: -3)
negative_view[0] = [-1.0, 13.0, 0.0]
assert np.allclose(negative_count.get_points()[0], [-1.0, 13.0, 0.0])

intersection = VMobject().set_points(
    [
        [-2.0, -1.0, 0.0],
        [1.0, 1.0, 0.0],
        [-2.0, 1.0, 0.0],
        [-1.0, 2.0, 0.0],
        [2.0, -1.0, 0.0],
    ]
)
intersection_path = intersection.get_anchors().copy()
intersection_flags = [
    space_utils.line_intersects_path(b0, b1, intersection_path)
    for b0, b1, b2 in intersection.get_bezier_tuples()
]
assert any(intersection_flags)
intersection_curves = intersection.get_num_curves()
assert intersection.subdivide_intersections(False, 2) is intersection
assert intersection.get_num_curves() == intersection_curves + 2 * sum(
    intersection_flags
)

non_intersection = VMobject().set_points_as_corners(
    [[0.0, 15.0, 0.0], [1.0, 15.0, 0.0]]
)
non_intersection_view = non_intersection.get_points()
assert non_intersection.subdivide_intersections(False, 1.5) is non_intersection
non_intersection_view[0] = [-1.0, 15.0, 0.0]
assert np.allclose(non_intersection.get_points()[0], [-1.0, 15.0, 0.0])

for refused_count, exception in [(1.5, TypeError), (10**100, ValueError)]:
    refused_intersection = VMobject().set_points(
        [
            [-2.0, -1.0, 0.0],
            [1.0, 1.0, 0.0],
            [-2.0, 1.0, 0.0],
            [-1.0, 2.0, 0.0],
            [2.0, -1.0, 0.0],
        ]
    )
    refused_intersection_before = refused_intersection.data.copy()
    try:
        refused_intersection.subdivide_intersections(False, refused_count)
    except exception:
        pass
    else:
        raise AssertionError(
            f"invalid intersection subdivision count {refused_count!r} succeeded"
        )
    assert np.array_equal(
        refused_intersection.data, refused_intersection_before
    )

bound_condition_scene = Scene()
bound_condition = VMobject().set_points_as_corners(
    [[-1.0, 14.0, 0.0], [1.0, 14.0, 0.0]]
)
bound_condition_scene.add(bound_condition)
bound_condition.subdivide_curves_by_condition(lambda *curve: 1, recurse=False)
assert bound_condition.get_num_curves() == 2
assert bound_condition in bound_condition_scene.mobjects

line.set_stroke(manimlib.BLACK, 3, background=True)
assert line.uniforms["stroke_behind"] is True
try:
    line.set_stroke(behind=True, background=False)
except TypeError as error:
    assert "conflicting behind/background" in str(error)
else:
    raise AssertionError("conflicting era-shim kwargs must be rejected")

# Tex and native text share the Reference's StringMobject selector surface.
# The implementation uses Scribe's retained UTF-8 source spans directly,
# preserving object identity without the Reference's render-twice SVG labels.
string_module = importlib.import_module("manimlib.mobject.svg.string_mobject")
svg_module = importlib.import_module("manimlib.mobject.svg.svg_mobject")
text_module = importlib.import_module("manimlib.mobject.svg.text_mobject")
StringMobject = string_module.StringMobject
SVGMobject = svg_module.SVGMobject
assert issubclass(manimlib.Tex, StringMobject)
assert issubclass(manimlib.MarkupText, StringMobject)
assert issubclass(StringMobject, SVGMobject)
assert issubclass(SVGMobject, VMobject)
assert StringMobject.__bases__ == (SVGMobject, abc.ABC)
assert manimlib.Tex.__mro__[:4] == (
    manimlib.Tex,
    StringMobject,
    SVGMobject,
    VMobject,
)
assert manimlib.MarkupText.__mro__[:4] == (
    manimlib.MarkupText,
    StringMobject,
    SVGMobject,
    VMobject,
)

plain_text = manimlib.Text("café café")
assert isinstance(plain_text, StringMobject)
assert plain_text.get_string() == plain_text.get_text() == "café café"
assert plain_text.find_spans_by_selector("café") == [(0, 4), (5, 9)]
assert plain_text.find_spans_by_selector(re.compile(r"caf.")) == [(0, 4), (5, 9)]
assert plain_text.find_spans_by_selector((-4, None)) == [(5, 9)]
assert plain_text.find_spans_by_selector("") == [
    (index, index) for index in range(len(plain_text.string) + 1)
]
assert plain_text.find_spans_by_selector(["café", (5, None)]) == [
    (0, 4),
    (5, 9),
    (5, 9),
]
assert StringMobject.span_contains((0, 4), (1, 3))
assert not StringMobject.span_contains((1, 3), (0, 4))
plain_groups = plain_text.get_parts_by_text("café")
assert len(plain_groups) == 2
assert all(len(group) == 4 for group in plain_groups)
assert plain_groups[0][0] is plain_text.submobjects[0]
assert plain_text["café"][1][0] is plain_text.submobjects[4]
assert plain_text.get_part_by_text("café", index=1)[0] is plain_text.submobjects[4]
assert plain_text.get_submob_indices_list_by_span((0, 4)) == [0, 1, 2, 3]
assert plain_text.get_submob_indices_lists_by_selector("café") == [
    [0, 1, 2, 3],
    [4, 5, 6, 7],
]
assert len(plain_text.get_parts_by_text("absent")) == 0
assert plain_text.set_color_by_text("café", manimlib.BLUE) is plain_text
assert all(
    leaf.get_fill_color() == manimlib.BLUE
    for group in plain_text.get_parts_by_text("café")
    for leaf in group
)
assert plain_text.set_color_by_text_to_color_map(
    {re.compile(r"caf."): manimlib.YELLOW}
) is plain_text
assert all(
    leaf.get_fill_color() == manimlib.YELLOW
    for group in plain_text.get_parts_by_text("café")
    for leaf in group
)
try:
    plain_text.find_spans_by_selector(3.5)
except TypeError as error:
    assert "Invalid selector" in str(error)
else:
    raise AssertionError("StringMobject accepted an invalid selector")

markup_text = manimlib.MarkupText(
    "<b>β</b> + β",
    t2c={"β": manimlib.RED},
    isolate=re.compile(r"β"),
)
assert markup_text.string == markup_text.text == "<b>β</b> + β"
assert markup_text.isolate.pattern == r"β"
markup_betas = markup_text.get_parts_by_text("β")
assert len(markup_betas) == 2
assert all(len(group) == 1 for group in markup_betas)
assert all(
    group[0].get_fill_color() == manimlib.RED
    for group in markup_betas
)
assert markup_text.get_part_by_text("β", index=1)[0] is markup_text.submobjects[2]

markup_signature = inspect.signature(manimlib.MarkupText)
assert markup_signature.parameters["font_size"].default == 48
assert markup_signature.parameters["text2color"].default == {}
assert markup_signature.parameters["t2c"].default == {}
assert markup_signature.parameters["isolate"].default.pattern == r"\w+"
text_signature = inspect.signature(manimlib.Text)
assert list(text_signature.parameters) == [
    "text",
    "isolate",
    "use_labelled_svg",
    "path_string_config",
    "kwargs",
]
assert text_signature.parameters["use_labelled_svg"].default is True
assert text_signature.parameters["path_string_config"].default == {
    "use_simple_quadratic_approx": True
}

tex = manimlib.Tex("E = mc^2", isolate=["mc"])
tex_signature = inspect.signature(manimlib.Tex)
assert tex_signature.parameters["tex_strings"].annotation == "str"
assert tex_signature.parameters["font_size"].default == 48
assert tex_signature.parameters["tex_to_color_map"].default == {}
assert tex_signature.parameters["t2c"].default == {}
assert tex_signature.parameters["isolate"].default == []
assert tex.string == tex.tex_string == "E = mc^2"
assert tex.alignment == r"\centering"
assert tex.template == ""
assert tex.additional_preamble == ""
assert tex.tex_to_color_map == {}
assert tex.isolate == ["mc"]
assert tex.tex_environment == "align*"
mc_parts = tex.get_parts_by_tex("mc")
assert len(mc_parts) == 1 and len(mc_parts[0]) == 2
assert tex.get_part_by_tex(re.compile(r"m.")) is not None
assert len(tex.get_parts_by_tex((0, 1))) == 1
assert len(tex.get_parts_by_tex("not present")) == 0
tex.set_color_by_tex("mc", manimlib.BLUE)
assert all(
    leaf.get_fill_color() == manimlib.BLUE
    for leaf in tex.get_part_by_tex("mc")
)
tex.set_color_by_tex_to_color_map({re.compile(r"E"): manimlib.RED})
assert all(
    leaf.get_fill_color() == manimlib.RED
    for leaf in tex.get_part_by_tex("E")
)
assert tex.get_tex() == "E = mc^2"

tex.scale(0.5)
assert math.isclose(tex.font_size, 24.0)

multi_tex = manimlib.Tex("x", "+", "y", isolate="=")
assert multi_tex.get_tex() == "x + y"
assert multi_tex.string == "x + y"
assert multi_tex.isolate == ["=", "x", "+", "y"]
assert len(multi_tex.get_parts_by_tex(["x", "y"])) == 2

trimmed_tex = manimlib.Tex("  x  ")
assert trimmed_tex.get_tex() == "x"
assert trimmed_tex.isolate == []
assert manimlib.Tex("").get_tex() == r"\\"

unicode_tex = manimlib.Tex("α+α")
unicode_parts = unicode_tex.get_parts_by_tex("α")
assert len(unicode_parts) == 2
unicode_tex.set_color_by_tex("α", manimlib.YELLOW)
assert all(
    leaf.get_fill_color() == manimlib.YELLOW
    for group in unicode_parts
    for leaf in group
)

tex_text = manimlib.TexText("words")
assert tex_text.tex_environment == ""

mapped_tex = manimlib.Tex(
    "x+y",
    t2c={"x": manimlib.RED},
    tex_to_color_map={"x": manimlib.BLUE},
)
assert mapped_tex.tex_to_color_map == {"x": manimlib.BLUE}
assert all(
    leaf.get_fill_color() == manimlib.BLUE
    for leaf in mapped_tex.get_part_by_tex("x")
)

for unsupported_tex_kwargs, named_knob in [
    ({"alignment": r"\raggedright"}, "alignment"),
    ({"template": "legacy"}, "template"),
    ({"additional_preamble": r"\usepackage{foo}"}, "additional_preamble"),
]:
    try:
        manimlib.Tex("x", **unsupported_tex_kwargs)
    except NotImplementedError as error:
        assert named_knob in str(error)
    else:
        raise AssertionError(f"unsupported Tex {named_knob} silently succeeded")


class ReadmeHello(Scene):
    def construct(self):
        title = manimlib.Text("FrankenManim", font_size=72)
        formula = manimlib.Tex(r"e^{i\pi} + 1 = 0")
        formula.next_to(title, manimlib.DOWN)
        self.play(
            manimlib.Write(title),
            manimlib.FadeIn(formula, shift=manimlib.UP),
        )
        self.play(formula.animate.set_color_by_tex("i", manimlib.YELLOW))
        self.wait()
        self.formula = formula


readme_hello = ReadmeHello()
readme_hello.run()
assert math.isclose(readme_hello.time(), 3.0)
readme_i_parts = readme_hello.formula.get_parts_by_tex("i")
assert len(readme_i_parts) == 1
assert all(
    leaf.get_fill_color() == manimlib.YELLOW
    for group in readme_i_parts
    for leaf in group
)

changeable = manimlib.Tex("x = 0.00").make_number_changeable("0.00")
assert isinstance(changeable, manimlib.DecimalNumber)
changeable.set_value(1.25)
assert math.isclose(changeable.get_value(), 1.25)

repeated_tex = manimlib.Tex("x", "+", "x")
assert len(repeated_tex.get_parts_by_tex("x")) == 2

functions = importlib.import_module("manimlib.mobject.functions")
curve = functions.ParametricCurve(
    lambda t: np.array([t, t * t, 0.0]),
    t_range=(0.0, 1.0, 0.25),
)
parametric_curve_signature = inspect.signature(functions.ParametricCurve)
assert tuple(parametric_curve_signature.parameters) == (
    "t_func",
    "t_range",
    "epsilon",
    "discontinuities",
    "use_smoothing",
    "kwargs",
)
assert parametric_curve_signature.parameters["t_range"].default == (0, 1, 0.1)
assert parametric_curve_signature.parameters["epsilon"].default == 1e-8
assert parametric_curve_signature.parameters["discontinuities"].default == []
assert parametric_curve_signature.parameters["use_smoothing"].default is True
assert curve.has_points()
assert Mobject().get_num_points() == 0
assert curve.get_num_points() == len(curve.get_points())
assert curve.get_num_curves() == curve.get_num_points() // 2
assert np.allclose(curve.get_point_from_function(0.5), [0.5, 0.25, 0.0])
assert curve.get_t_func() is curve.t_func
assert curve.get_arc_length() > math.sqrt(2.0)

# FunctionGraph and ImplicitFunction are authored portal classes over Atlas's
# already-native graph builders, not schema-generated import shells. Scalar
# callbacks are construction-only, metadata remains Reference-visible, and
# Chisel owns the bounded zero-set extraction.
assert functions.FunctionGraph.__bases__ == (functions.ParametricCurve,)
assert functions.ImplicitFunction.__bases__ == (VMobject,)
function_graph_signature = inspect.signature(functions.FunctionGraph)
assert tuple(function_graph_signature.parameters) == (
    "function",
    "x_range",
    "color",
    "kwargs",
)
assert function_graph_signature.parameters["x_range"].default == (-8, 8, 0.25)
assert function_graph_signature.parameters["color"].default == manimlib.YELLOW
implicit_signature = inspect.signature(functions.ImplicitFunction)
assert tuple(implicit_signature.parameters) == (
    "func",
    "x_range",
    "y_range",
    "min_depth",
    "max_quads",
    "use_smoothing",
    "joint_type",
    "kwargs",
)
assert np.allclose(
    implicit_signature.parameters["x_range"].default,
    (-manimlib.FRAME_X_RADIUS, manimlib.FRAME_X_RADIUS),
)
assert np.allclose(
    implicit_signature.parameters["y_range"].default,
    (-manimlib.FRAME_Y_RADIUS, manimlib.FRAME_Y_RADIUS),
)
assert implicit_signature.parameters["min_depth"].default == 5
assert implicit_signature.parameters["max_quads"].default == 1500
assert implicit_signature.parameters["use_smoothing"].default is False
assert implicit_signature.parameters["joint_type"].default == "no_joint"

function_graph_samples = []


def portal_parabola(x):
    function_graph_samples.append(float(x))
    return x * x - 0.25


function_graph = functions.FunctionGraph(
    portal_parabola,
    x_range=(-1.0, 1.0, 0.25),
    color=manimlib.RED,
    epsilon=1e-6,
    discontinuities=(0.0,),
    use_smoothing=False,
    stroke_width=3.0,
)
assert function_graph.function is portal_parabola
assert function_graph.get_function() is portal_parabola
assert function_graph.get_t_func() is function_graph.t_func
assert function_graph.get_x_range() == (-1.0, 1.0, 0.25)
assert function_graph.t_range == function_graph.x_range
assert function_graph.epsilon == 1e-6
assert function_graph.discontinuities == (0.0,)
assert function_graph.use_smoothing is False
assert function_graph.get_stroke_color() == manimlib.RED
assert math.isclose(function_graph.get_stroke_width(), 3.0)
assert np.allclose(function_graph.get_start(), [-1.0, 0.75, 0.0])
assert np.allclose(function_graph.get_end(), [1.0, 0.75, 0.0])
assert function_graph_samples[0] == -1.0
assert function_graph_samples[-1] == 1.0
assert any(np.isclose(value, -1e-6) for value in function_graph_samples)
assert any(np.isclose(value, 1e-6) for value in function_graph_samples)
assert np.allclose(
    function_graph.get_point_from_function(0.5),
    [0.5, 0.0, 0.0],
)

default_function_graph = functions.FunctionGraph(
    lambda x: x,
    x_range=(0.0, 1.0, 0.5),
    use_smoothing=False,
)
assert default_function_graph.get_stroke_color() == manimlib.YELLOW

implicit_calls = []


def portal_circle_field(x, y):
    implicit_calls.append((float(x), float(y)))
    return x * x + y * y - 1.0


implicit_circle = functions.ImplicitFunction(
    portal_circle_field,
    x_range=(-1.5, 1.5),
    y_range=(-1.5, 1.5),
    min_depth=3,
    max_quads=512,
    use_smoothing=False,
    joint_type="bevel",
    stroke_color=manimlib.BLUE,
    stroke_width=2.5,
)
assert implicit_circle.func is portal_circle_field
assert implicit_circle.x_range == (-1.5, 1.5)
assert implicit_circle.y_range == (-1.5, 1.5)
assert implicit_circle.min_depth == 3
assert implicit_circle.max_quads == 512
assert implicit_circle.use_smoothing is False
assert implicit_circle.joint_type == "bevel"
assert implicit_circle.get_joint_type() == VMobject.joint_type_map["bevel"]
assert implicit_circle.get_stroke_color() == manimlib.BLUE
assert math.isclose(implicit_circle.get_stroke_width(), 2.5)
assert implicit_circle.has_points()
assert implicit_calls
implicit_box = implicit_circle.get_bounding_box()
assert np.allclose(implicit_box[0, :2], [-1.0, -1.0], atol=2e-2)
assert np.allclose(implicit_box[2, :2], [1.0, 1.0], atol=2e-2)

empty_implicit = functions.ImplicitFunction(
    lambda x, y: x * x + y * y + 1.0,
    x_range=(-1.0, 1.0),
    y_range=(-1.0, 1.0),
    min_depth=2,
    max_quads=64,
)
assert not empty_implicit.has_points()


class PortalGraphCallbackError(RuntimeError):
    pass


def refuse_graph_callback(_x):
    raise PortalGraphCallbackError("function graph callback sentinel")


try:
    functions.FunctionGraph(
        refuse_graph_callback,
        x_range=(0.0, 1.0, 0.5),
        use_smoothing=False,
    )
except PortalGraphCallbackError as error:
    assert str(error) == "function graph callback sentinel"
else:
    raise AssertionError("FunctionGraph swallowed its Python callback error")


def refuse_implicit_callback(_x, _y):
    raise PortalGraphCallbackError("implicit callback sentinel")


try:
    functions.ImplicitFunction(
        refuse_implicit_callback,
        x_range=(-1.0, 1.0),
        y_range=(-1.0, 1.0),
        min_depth=1,
        max_quads=16,
    )
except PortalGraphCallbackError as error:
    assert str(error) == "implicit callback sentinel"
else:
    raise AssertionError("ImplicitFunction swallowed its Python callback error")

preflight_graph_calls = []
for invalid_kwargs, invalid_name in (
    ({"unknown_style": True}, "unknown_style"),
    ({"joint_type": "not-a-joint"}, "not-a-joint"),
):
    try:
        functions.ImplicitFunction(
            lambda x, y: preflight_graph_calls.append((x, y)) or x + y,
            x_range=(-1.0, 1.0),
            y_range=(-1.0, 1.0),
            **invalid_kwargs,
        )
    except (TypeError, ValueError) as error:
        assert invalid_name in str(error)
    else:
        raise AssertionError(f"ImplicitFunction accepted {invalid_name}")
assert preflight_graph_calls == []

# CoordinateSystem is the Python callable seam over Atlas's native graph
# sampler. The concrete axes keep their live coordinate transform, graph
# metadata/style remain Reference-visible, and the tick step is divided by
# num_sampled_graph_points_per_tick before native construction.
coordinate_systems = importlib.import_module("manimlib.mobject.coordinate_systems")
assert issubclass(manimlib.Axes, coordinate_systems.CoordinateSystem)
graph_axes = manimlib.Axes(
    x_range=(0.0, 2.0, 1.0),
    y_range=(0.0, 4.0, 1.0),
    width=4.0,
    height=4.0,
    num_sampled_graph_points_per_tick=2,
)
assert graph_axes.get_axis(0) is graph_axes.x_axis
assert graph_axes.get_x_axis() is graph_axes.x_axis
assert graph_axes.get_y_axis() is graph_axes.y_axis
assert np.allclose(graph_axes.get_origin(), graph_axes.c2p(0.0, 0.0))
assert np.allclose(graph_axes.p2c(graph_axes.c2p(0.5, 0.25)), [0.5, 0.25])
sampled_xs = []


def sampled_square(x):
    sampled_xs.append(float(x))
    return x * x


static_graph = graph_axes.get_graph(
    sampled_square,
    x_range=(0.0, 2.0, 1.0),
    color=manimlib.RED,
    stroke_width=7.0,
    use_smoothing=False,
)
assert isinstance(static_graph, functions.ParametricCurve)
assert static_graph.underlying_function is sampled_square
assert static_graph.x_range == (0.0, 2.0, 1.0)
assert static_graph.t_range == (0.0, 2.0, 0.5)
assert np.allclose(sampled_xs, [0.0, 0.5, 1.0, 1.5, 2.0])
assert static_graph.get_stroke_color() == manimlib.RED
assert math.isclose(static_graph.get_stroke_width(), 7.0)
assert np.allclose(static_graph.get_start(), graph_axes.c2p(0.0, 0.0))
assert np.allclose(static_graph.get_end(), graph_axes.c2p(2.0, 4.0))
assert np.allclose(
    graph_axes.input_to_graph_point(0.5, static_graph),
    graph_axes.c2p(0.5, 0.25),
)
assert np.allclose(graph_axes.i2gp(1.5, static_graph), graph_axes.c2p(1.5, 2.25))

parametric_graph = graph_axes.get_parametric_curve(
    lambda t: np.array([t, 2.0 * t]),
    t_range=(0.0, 1.0, 0.5),
    use_smoothing=False,
)
assert np.allclose(parametric_graph.get_start(), graph_axes.c2p(0.0, 0.0))
assert np.allclose(parametric_graph.get_end(), graph_axes.c2p(1.0, 2.0))

# The pinned function-less fallback deliberately uses quick curve-count
# interpolation and the Reference binary search, not true arclength.
unit_axes = manimlib.Axes(
    x_range=(0.0, 1.0, 0.25),
    y_range=(0.0, 1.0, 0.25),
    width=2.0,
    height=2.0,
)
functionless_graph = functions.ParametricCurve(
    lambda t: unit_axes.c2p(t, t),
    t_range=(0.0, 1.0, 0.05),
    use_smoothing=False,
)
fallback_point = unit_axes.input_to_graph_point(0.4, functionless_graph)
assert math.isclose(
    unit_axes.point_to_coords(fallback_point)[0],
    0.4,
    rel_tol=0.0,
    abs_tol=2e-4,
)

# bind=True retains the Reference's vectorized-function contract and routes
# smoothing into Chisel after each corner refresh. The updater changes the
# same graph object and remains usable by ordinary graph-point queries.
bound_scale = [1.0]
bound_graph = unit_axes.get_graph(
    lambda x: bound_scale[0] * np.asarray(x),
    bind=True,
    use_smoothing=False,
)
assert len(bound_graph.updaters) == 2
bound_before = bound_graph.get_points().copy()
bound_scale[0] = 0.5
bound_graph.update(0.0)
assert not np.allclose(bound_graph.get_points(), bound_before)
assert np.allclose(
    unit_axes.i2gp(0.75, bound_graph),
    unit_axes.c2p(0.75, 0.375),
)

discontinuous_graph = unit_axes.get_graph(
    lambda x: np.asarray(x),
    use_smoothing=False,
)
unit_axes.bind_graph_to_func(
    discontinuous_graph,
    lambda x: np.asarray(x),
    jagged=True,
    get_discontinuities=lambda: [0.5],
)
assert len(discontinuous_graph.updaters) == 1
discontinuous_xs = unit_axes.point_to_coords(discontinuous_graph.get_points())[0]
assert np.any(np.isclose(discontinuous_xs, 0.5 - 1e-6, atol=1e-12))
assert np.any(np.isclose(discontinuous_xs, 0.5 + 1e-6, atol=1e-12))


def refuse_vectorized_graph_callback(x):
    if np.ndim(x) != 0:
        raise LookupError("vectorized graph callback failed")
    return x


try:
    unit_axes.get_graph(
        refuse_vectorized_graph_callback,
        bind=True,
        use_smoothing=False,
    )
except LookupError as error:
    assert str(error) == "vectorized graph callback failed"
else:
    raise AssertionError("a bound graph callback exception was swallowed")

# Family recursion is Python-owned while each smoothing operation is native,
# so recurse=False cannot accidentally rewrite a child path.
smooth_root = VMobject().set_points_as_corners(
    [[0.0, 0.0, 0.0], [1.0, 1.0, 0.0], [2.0, 0.0, 0.0]]
)
smooth_child = VMobject().set_points_as_corners(
    [[0.0, 0.0, 0.0], [0.5, -1.0, 0.0], [1.0, 0.0, 0.0]]
)
smooth_root.add(smooth_child)
smooth_root_before = smooth_root.get_points().copy()
smooth_child_before = smooth_child.get_points().copy()
assert smooth_root.make_smooth(approx=True, recurse=False) is smooth_root
assert not np.allclose(smooth_root.get_points(), smooth_root_before)
assert np.allclose(smooth_child.get_points(), smooth_child_before)
assert smooth_child.make_smooth(approx=False) is smooth_child
assert smooth_child.has_points()

three_dimensions = importlib.import_module("manimlib.mobject.three_dimensions")
sphere = three_dimensions.Sphere(radius=2.0, clockwise=True, resolution=(5, 3))
assert np.allclose(sphere.uv_func(0.0, 0.0), [0.0, 0.0, -2.0])
assert np.allclose(sphere.uv_func(0.0, math.pi / 2.0), [2.0, 0.0, 0.0])

# Atlas owns the complete parametric solid shelf. Every authored portal class
# preserves the pinned MRO/signature while construction and public uv_func
# calls share the exact native parameterization.
cylinder = three_dimensions.Cylinder(
    u_range=(0.0, math.tau),
    v_range=(-1.0, 1.0),
    resolution=(5, 3),
    height=4.0,
    radius=2.0,
    axis=manimlib.OUT,
    color=manimlib.BLUE,
    depth_test=False,
)
assert cylinder.n_records() == 15
assert cylinder.u_range == (0.0, math.tau)
assert cylinder.v_range == (-1.0, 1.0)
assert cylinder.resolution == (5, 3)
assert cylinder.height == 4.0
assert cylinder.radius == 2.0
assert np.array_equal(cylinder.axis, manimlib.OUT)
assert np.allclose(cylinder.uv_func(0.0, 0.25), [1.0, 0.0, 0.25])
assert np.allclose(
    [cylinder.get_width(), cylinder.get_height(), cylinder.get_depth()],
    [4.0, 4.0, 4.0],
    atol=1e-6,
)
assert cylinder.uniforms["depth_test"] is False
assert cylinder.get_color() == manimlib.BLUE

right_cylinder = three_dimensions.Cylinder(
    resolution=(5, 3), height=6.0, radius=1.0, axis=manimlib.RIGHT
)
assert np.allclose(
    [right_cylinder.get_width(), right_cylinder.get_height(), right_cylinder.get_depth()],
    [6.0, 2.0, 2.0],
    atol=1e-6,
)

assert three_dimensions.Torus.__bases__ == (manimlib.Surface,)
assert three_dimensions.Cone.__bases__ == (three_dimensions.Cylinder,)
assert three_dimensions.Line3D.__bases__ == (three_dimensions.Cylinder,)
assert three_dimensions.Disk3D.__bases__ == (manimlib.Surface,)
assert three_dimensions.Square3D.__bases__ == (manimlib.Surface,)
assert str(inspect.signature(three_dimensions.Torus)) == (
    "(u_range=(0, 6.283185307179586), v_range=(0, 6.283185307179586), "
    "r1=3.0, r2=1.0, **kwargs)"
)
assert str(inspect.signature(three_dimensions.Cone)) == (
    "(u_range=(0, 6.283185307179586), v_range=(0, 1), *args, **kwargs)"
)
assert str(inspect.signature(three_dimensions.Line3D)) == (
    "(start, end, width=0.05, resolution=(21, 25), **kwargs)"
)
assert str(inspect.signature(three_dimensions.Disk3D)) == (
    "(radius=1, u_range=(0, 1), v_range=(0, 6.283185307179586), "
    "resolution=(2, 100), **kwargs)"
)
assert str(inspect.signature(three_dimensions.Square3D)) == (
    "(side_length=2.0, u_range=(-1, 1), v_range=(-1, 1), "
    "resolution=(2, 2), **kwargs)"
)

torus = three_dimensions.Torus(
    r1=2.0,
    r2=0.5,
    resolution=(5, 5),
    color=manimlib.BLUE,
    opacity=0.6,
    shading=(0.1, 0.2, 0.3),
    depth_test=False,
)
assert torus.n_records() == 25
assert np.allclose(torus.uv_func(0.0, 0.0), [1.5, 0.0, 0.0])
assert np.allclose(torus.uv_func(0.0, math.pi), [2.5, 0.0, 0.0])
assert np.allclose(
    [torus.get_width(), torus.get_height(), torus.get_depth()],
    [5.0, 5.0, 1.0],
    atol=1e-6,
)
assert torus.get_color() == manimlib.BLUE
assert np.isclose(torus.get_opacity(), 0.6)
assert np.allclose(torus.uniforms["shading"], [0.1, 0.2, 0.3])
assert torus.uniforms["depth_test"] is False

cone = three_dimensions.Cone(
    resolution=(5, 3),
    height=3.0,
    radius=2.0,
    axis=manimlib.RIGHT,
)
assert cone.n_records() == 15
assert cone.u_range == (0, math.tau)
assert cone.v_range == (0, 1)
assert np.allclose(cone.uv_func(0.0, 0.0), [1.0, 0.0, 0.0])
assert np.allclose(cone.uv_func(0.0, 1.0), [0.0, 0.0, 1.0])
assert np.allclose(
    [cone.get_width(), cone.get_height(), cone.get_depth()],
    [3.0, 4.0, 4.0],
    atol=1e-6,
)
assert np.allclose(cone.get_center(), [1.5, 0.0, 0.0], atol=1e-6)

line3d = three_dimensions.Line3D(
    [-2.0, 0.0, 0.0],
    [2.0, 0.0, 0.0],
    width=0.4,
    resolution=(5, 3),
)
assert line3d.n_records() == 15
assert np.isclose(line3d.height, 4.0)
assert np.isclose(line3d.radius, 0.2)
assert np.array_equal(line3d.axis, [4.0, 0.0, 0.0])
assert np.allclose(line3d.uv_func(0.0, 0.25), [1.0, 0.0, 0.25])
assert np.allclose(
    [line3d.get_width(), line3d.get_height(), line3d.get_depth()],
    [4.0, 0.4, 0.4],
    atol=1e-6,
)

disk3d = three_dimensions.Disk3D(radius=2.0, resolution=(2, 5))
assert disk3d.n_records() == 10
assert np.allclose(disk3d.uv_func(0.5, 0.0), [0.5, 0.0, 0.0])
assert np.allclose(
    [disk3d.get_width(), disk3d.get_height(), disk3d.get_depth()],
    [4.0, 4.0, 0.0],
    atol=1e-6,
)

square3d = three_dimensions.Square3D(
    side_length=3.0,
    u_range=(-2.0, 2.0),
    v_range=(-1.0, 1.0),
    resolution=(3, 2),
)
assert square3d.n_records() == 6
assert np.allclose(square3d.uv_func(-2.0, 1.0), [-2.0, 1.0, 0.0])
assert np.allclose(
    [square3d.get_width(), square3d.get_height(), square3d.get_depth()],
    [6.0, 3.0, 0.0],
    atol=1e-6,
)

solid_scene = Scene().add(torus, cone, line3d, disk3d, square3d)
assert solid_scene.get_mobjects() == [torus, cone, line3d, disk3d, square3d]

# Atlas's vectorized 3D shelf is authored rather than schema-generated: the
# group keeps caller proxy identity while the four concrete solids install
# Atlas geometry and recurse 3D uniforms through the live family.
assert three_dimensions.VGroup3D.__bases__ == (manimlib.VGroup,)
assert three_dimensions.VCube.__bases__ == (three_dimensions.VGroup3D,)
assert three_dimensions.VPrism.__bases__ == (three_dimensions.VCube,)
assert three_dimensions.Dodecahedron.__bases__ == (three_dimensions.VGroup3D,)
assert three_dimensions.Prismify.__bases__ == (three_dimensions.VGroup3D,)
assert str(inspect.signature(three_dimensions.VGroup3D)) == (
    "(*vmobjects, depth_test=True, shading=(0.2, 0.2, 0.2), "
    "joint_type='no_joint', **kwargs)"
)
assert str(inspect.signature(three_dimensions.VCube)) == (
    "(side_length=2.0, fill_color='#29ABCA', fill_opacity=1, "
    "stroke_width=0, **kwargs)"
)
assert str(inspect.signature(three_dimensions.VPrism)) == (
    "(width=3.0, height=2.0, depth=1.0, **kwargs)"
)
assert str(inspect.signature(three_dimensions.Dodecahedron)) == (
    "(fill_color='#1C758A', fill_opacity=1, stroke_color='#1C758A', "
    "stroke_width=1, shading=(0.2, 0.2, 0.2), **kwargs)"
)
prismify_signature = inspect.signature(three_dimensions.Prismify)
assert tuple(prismify_signature.parameters) == (
    "vmobject", "depth", "direction", "kwargs"
)
assert prismify_signature.parameters["depth"].default == 1.0
assert np.array_equal(
    prismify_signature.parameters["direction"].default, manimlib.IN
)

group_left = manimlib.Square(side_length=0.5)
group_right = manimlib.Square(side_length=0.75).shift(manimlib.RIGHT)
joint_probe = manimlib.VGroup(
    manimlib.Square(side_length=0.2),
    manimlib.Square(side_length=0.2).shift(manimlib.RIGHT),
)
assert joint_probe.get_joint_type() == 1
assert joint_probe.set_joint_type("bevel") is joint_probe
assert all(member.get_joint_type() == 2 for member in joint_probe.get_family())
try:
    joint_probe.set_joint_type("rounded")
except KeyError as error:
    assert error.args == ("rounded",)
else:
    raise AssertionError("an unknown joint type was silently accepted")
assert all(member.get_joint_type() == 2 for member in joint_probe.get_family())

vector_group = three_dimensions.VGroup3D(
    group_left,
    group_right,
    depth_test=False,
    shading=(0.3, 0.4, 0.5),
    joint_type="miter",
)
assert vector_group.submobjects == [group_left, group_right]
assert vector_group.submobjects[0] is group_left
for member in vector_group.submobjects:
    assert member.uniforms["depth_test"] is False
    assert np.allclose(member.uniforms["shading"], [0.3, 0.4, 0.5])
    assert member.uniforms["joint_type"] == 3

vcube = three_dimensions.VCube(
    side_length=2.5,
    fill_color=manimlib.RED,
    fill_opacity=0.4,
    stroke_color=manimlib.BLUE,
    stroke_width=1.5,
    depth_test=False,
    shading=(0.1, 0.2, 0.3),
)
assert len(vcube.submobjects) == 6
assert np.allclose(
    [vcube.get_width(), vcube.get_height(), vcube.get_depth()],
    [2.5, 2.5, 2.5],
    atol=1e-6,
)
for face in vcube.submobjects:
    assert face.get_fill_color() == manimlib.RED
    assert np.isclose(face.get_fill_opacity(), 0.4)
    assert face.get_stroke_color() == manimlib.BLUE
    assert np.isclose(face.get_stroke_width(), 1.5)
    assert face.uniforms["depth_test"] is False
    assert np.allclose(face.uniforms["shading"], [0.1, 0.2, 0.3])
    assert face.get_joint_type() == 0

vprism = three_dimensions.VPrism(
    width=4.0,
    height=3.0,
    depth=2.0,
    fill_color=manimlib.BLUE,
    fill_opacity=0.7,
)
assert len(vprism.submobjects) == 6
assert np.allclose(
    [vprism.get_width(), vprism.get_height(), vprism.get_depth()],
    [4.0, 3.0, 2.0],
    atol=1e-6,
)
assert all(face.get_fill_color() == manimlib.BLUE for face in vprism.submobjects)

dodecahedron = three_dimensions.Dodecahedron(
    fill_color=manimlib.RED,
    fill_opacity=0.6,
    stroke_color=manimlib.BLUE,
    stroke_width=2.0,
)
assert len(dodecahedron.submobjects) == 12
assert all(face.n_records() == 11 for face in dodecahedron.submobjects)
phi = (1.0 + math.sqrt(5.0)) / 2.0
assert np.allclose(
    [dodecahedron.get_width(), dodecahedron.get_height(), dodecahedron.get_depth()],
    [2.0 * phi, 2.0 * phi, 2.0 * phi],
    atol=1e-6,
)

prismify_source = manimlib.Polygon(
    [-0.5, -0.5, 0.0],
    [0.75, -0.5, 0.0],
    [0.0, 0.75, 0.0],
    fill_color=manimlib.RED,
    fill_opacity=0.5,
    stroke_color=manimlib.BLUE,
    stroke_width=2.5,
)
prismify_source_points = prismify_source.get_points().copy()
prismified = three_dimensions.Prismify(
    prismify_source, depth=0.75, direction=manimlib.OUT
)
assert len(prismified.submobjects) == 5
assert np.array_equal(prismify_source.get_points(), prismify_source_points)
assert np.allclose(prismified.submobjects[0].get_points(), prismify_source_points)
assert np.isclose(prismified.submobjects[-1].get_depth(), 0.0)
assert np.allclose(
    prismified.submobjects[-1].get_center()
    - prismify_source.get_center(),
    [0.0, 0.0, 0.75],
    atol=1e-6,
)
for piece in prismified.submobjects:
    assert piece.get_fill_color() == manimlib.RED
    assert np.isclose(piece.get_fill_opacity(), 0.5)
    assert piece.get_stroke_color() == manimlib.BLUE
    assert np.isclose(piece.get_stroke_width(), 2.5)

vector_solid_scene = Scene().add(
    vector_group, vcube, vprism, dodecahedron, prismified
)
assert vector_solid_scene.get_mobjects() == [
    vector_group, vcube, vprism, dodecahedron, prismified
]

failed_vcube = three_dimensions.VCube.__new__(three_dimensions.VCube)
try:
    three_dimensions.VCube.__init__(failed_vcube, unrouted_option=True)
except NotImplementedError as error:
    assert str(error) == (
        "VCube() keyword(s) not yet routed to the native builder: "
        "unrouted_option"
    )
else:
    raise AssertionError("an unrouted VCube keyword reached the native builder")
assert not hasattr(failed_vcube, "submobjects")

try:
    three_dimensions.Prismify(manimlib.VGroup(prismify_source))
except NotImplementedError as error:
    assert str(error) == (
        "Prismify over a VMobject family awaits native family-value extraction"
    )
else:
    raise AssertionError("Prismify silently discarded a source family")

# The two de-TeX'd special-text compositions are authored portal classes over
# Atlas/Scribe, not import-only schema shells. Construction owns layout in
# Rust; the live Python methods retain ordinary manim object identity.
special_tex = importlib.import_module("manimlib.mobject.svg.special_tex")
assert special_tex.BulletedList.__bases__ == (manimlib.VGroup,)
assert special_tex.Title.__bases__ == (manimlib.TexText,)
bullets_signature = inspect.signature(special_tex.BulletedList)
assert tuple(bullets_signature.parameters) == (
    "items", "buff", "aligned_edge", "numbered", "kwargs"
)
assert bullets_signature.parameters["items"].kind is inspect.Parameter.VAR_POSITIONAL
assert bullets_signature.parameters["buff"].kind is inspect.Parameter.KEYWORD_ONLY
assert bullets_signature.parameters["numbered"].default is False
title_signature = inspect.signature(special_tex.Title)
assert tuple(title_signature.parameters) == (
    "text_parts",
    "font_size",
    "include_underline",
    "underline_width",
    "match_underline_width_to_text",
    "underline_buff",
    "underline_style",
    "kwargs",
)
assert title_signature.parameters["text_parts"].kind is inspect.Parameter.VAR_POSITIONAL
assert title_signature.parameters["font_size"].default == 72
assert title_signature.parameters["include_underline"].default is True

bulleted = special_tex.BulletedList(
    "First",
    "A much longer second item",
    "Third",
    buff=0.35,
    font_size=30,
    color=manimlib.BLUE,
)
assert len(bulleted) == 3
assert all(len(item) == 2 for item in bulleted)
assert all(
    member.get_fill_color() == manimlib.BLUE
    for member in bulleted.family_members_with_points()
)
item_tops = [item.get_top()[1] for item in bulleted]
assert item_tops[0] > item_tops[1] > item_tops[2]
item_lefts = [item.get_left()[0] for item in bulleted]
assert np.allclose(item_lefts, item_lefts[0], atol=1e-6)
assert bulleted[1][1].get_width() > bulleted[0][1].get_width()

numbered = special_tex.BulletedList(
    "First", "Second", numbered=True, font_size=30
)
assert len(numbered) == 2
assert numbered[0][0].get_width() > bulleted[0][0].get_width()

item_identities = [id(item) for item in bulleted]
max_label_height = max(item[0].get_height() for item in bulleted)
assert bulleted.fade_all_but(1, opacity=0.2, scale_factor=0.6) is None
assert [id(item) for item in bulleted] == item_identities
assert np.isclose(bulleted[1][0].get_height(), max_label_height, atol=1e-6)
for index, item in enumerate(bulleted):
    expected_opacity = 1.0 if index == 1 else 0.2
    assert all(
        np.isclose(member.get_fill_opacity(), expected_opacity)
        for member in item.family_members_with_points()
    )

failed_bullets = special_tex.BulletedList.__new__(special_tex.BulletedList)
try:
    special_tex.BulletedList.__init__(failed_bullets, "item", unsupported=True)
except TypeError as error:
    assert str(error) == "unexpected keyword arguments: unsupported"
else:
    raise AssertionError("BulletedList silently discarded an unknown option")
assert not hasattr(failed_bullets, "submobjects")

title = special_tex.Title(
    "Native",
    "Title",
    font_size=36,
    underline_width=4.0,
    underline_buff=0.2,
    underline_style=dict(stroke_width=3, stroke_color=manimlib.RED),
    color=manimlib.BLUE,
)
assert len(title) == 3
assert title[0] is title.submobjects[0]
assert title[1] is title.submobjects[1]
assert title.underline is title[2]
assert title.tex_strings == ["Native", "Title"]
assert title.tex_string == "Native Title"
assert title._string_sub_paths == [[0], [1]]
assert np.isclose(title.underline.get_width(), 4.0, atol=1e-6)
assert title.underline.get_stroke_color() == manimlib.RED
assert np.isclose(title.underline.get_stroke_width(), 3.0)
assert all(
    member.get_fill_color() == manimlib.BLUE
    for part in title[:2]
    for member in part.family_members_with_points()
)
text_bottom = min(part.get_bottom()[1] for part in title[:2])
assert np.isclose(
    title.underline.get_top()[1], text_bottom - 0.2, atol=1e-6
)
assert np.isclose(
    max(part.get_top()[1] for part in title[:2]),
    manimlib.FRAME_Y_RADIUS - manimlib.MED_SMALL_BUFF,
    atol=1e-6,
)

matched_title = special_tex.Title(
    "Matched",
    "Underline",
    font_size=30,
    match_underline_width_to_text=True,
)
matched_text_left = min(part.get_left()[0] for part in matched_title[:-1])
matched_text_right = max(part.get_right()[0] for part in matched_title[:-1])
assert np.isclose(
    matched_title.underline.get_width(),
    matched_text_right - matched_text_left,
    atol=1e-6,
)
plain_title = special_tex.Title("No underline", include_underline=False)
assert len(plain_title) == 1
assert not hasattr(plain_title, "underline")

failed_title = special_tex.Title.__new__(special_tex.Title)
try:
    special_tex.Title.__init__(
        failed_title, "Title", underline_style={"unsupported": True}
    )
except TypeError as error:
    assert str(error) == "unexpected keyword arguments: unsupported"
else:
    raise AssertionError("Title silently discarded an unknown underline option")
assert not hasattr(failed_title, "submobjects")

# The scalar Matrix family is an authored portal over Atlas's one grid engine
# and the bundled fmd-math delimiter builder. Python keeps caller-visible
# entry identities and accessors; no placement or bracket math is duplicated.
matrix_module = importlib.import_module("manimlib.mobject.matrix")
assert matrix_module.Matrix.__bases__ == (manimlib.VMobject,)
assert matrix_module.DecimalMatrix.__bases__ == (matrix_module.Matrix,)
assert matrix_module.IntegerMatrix.__bases__ == (matrix_module.DecimalMatrix,)
assert matrix_module.TexMatrix.__bases__ == (matrix_module.Matrix,)
matrix_signature = inspect.signature(matrix_module.Matrix)
assert tuple(matrix_signature.parameters) == (
    "matrix",
    "v_buff",
    "h_buff",
    "bracket_h_buff",
    "bracket_v_buff",
    "height",
    "element_config",
    "element_alignment_corner",
    "ellipses_row",
    "ellipses_col",
)
assert tuple(inspect.signature(matrix_module.DecimalMatrix).parameters) == (
    "matrix", "num_decimal_places", "decimal_config", "config"
)
assert tuple(inspect.signature(matrix_module.IntegerMatrix).parameters) == (
    "matrix", "num_decimal_places", "decimal_config", "config"
)
assert tuple(inspect.signature(matrix_module.TexMatrix).parameters) == (
    "matrix", "tex_config", "config"
)

integer_tex_matrix = matrix_module.Matrix(
    [[1, 22], [333, 4]],
    v_buff=0.7,
    h_buff=0.8,
    element_config=dict(font_size=24, color=manimlib.BLUE),
)
assert len(integer_tex_matrix) == 6
assert integer_tex_matrix._matrix_shape == (2, 2)
assert all(
    isinstance(entry, manimlib.Tex)
    for row in integer_tex_matrix.get_mob_matrix()
    for entry in row
)
assert [
    entry.get_tex()
    for row in integer_tex_matrix.get_mob_matrix()
    for entry in row
] == ["1", "22", "333", "4"]
assert integer_tex_matrix.get_row(0)[0] is integer_tex_matrix.mob_matrix[0][0]
assert integer_tex_matrix.get_column(1)[1] is integer_tex_matrix.mob_matrix[1][1]
assert integer_tex_matrix.get_rows() is integer_tex_matrix.rows
assert integer_tex_matrix.get_columns() is integer_tex_matrix.columns
assert len(integer_tex_matrix.get_rows()) == 2
assert len(integer_tex_matrix.get_columns()) == 2
assert len(integer_tex_matrix.get_entries()) == 4
assert len(integer_tex_matrix.get_brackets()) == 2
assert len(integer_tex_matrix.get_ellipses()) == 0
assert integer_tex_matrix.mob_matrix[0][0].get_center()[0] < (
    integer_tex_matrix.mob_matrix[0][1].get_center()[0]
)
assert integer_tex_matrix.mob_matrix[0][0].get_center()[1] > (
    integer_tex_matrix.mob_matrix[1][0].get_center()[1]
)
assert all(
    member.get_fill_color() == manimlib.BLUE
    for entry in integer_tex_matrix.elements
    for member in entry.family_members_with_points()
)

integer_tex_matrix.set_column_colors(manimlib.RED, manimlib.YELLOW)
assert all(
    member.get_fill_color() == manimlib.RED
    for row in integer_tex_matrix.mob_matrix
    for member in row[0].family_members_with_points()
)
assert all(
    member.get_fill_color() == manimlib.YELLOW
    for row in integer_tex_matrix.mob_matrix
    for member in row[1].family_members_with_points()
)
matrix_copy = integer_tex_matrix.copy()
assert matrix_copy is not integer_tex_matrix
assert matrix_copy.mob_matrix[0][0] is matrix_copy.submobjects[0]
assert matrix_copy.mob_matrix[0][0] is not integer_tex_matrix.mob_matrix[0][0]
assert matrix_copy.brackets[-1] is matrix_copy.submobjects[-1]
background_matrix = matrix_module.Matrix([["x"]])
assert background_matrix.add_background_to_entries() is background_matrix
assert background_matrix.elements[0].background_rectangle is (
    background_matrix.elements[0].submobjects[0]
)

float_matrix = matrix_module.Matrix(
    [[1.25, 2.5]],
    element_config=dict(num_decimal_places=1, font_size=30),
)
assert all(isinstance(entry, manimlib.DecimalNumber) for entry in float_matrix.elements)
assert np.allclose(
    [entry.get_value() for entry in float_matrix.elements], [1.25, 2.5]
)
first_float_entry = float_matrix.elements[0]
first_float_edge = first_float_entry.get_edge_center(manimlib.LEFT).copy()
assert first_float_entry.set_value(9.5) is first_float_entry
assert float_matrix.elements[0] is first_float_entry
assert np.isclose(first_float_entry.get_value(), 9.5)
assert np.allclose(
    first_float_entry.get_edge_center(manimlib.LEFT), first_float_edge, atol=1e-6
)
decimal_matrix = matrix_module.DecimalMatrix(
    [[1, 2.5], [3.75, 4]],
    num_decimal_places=2,
    decimal_config=dict(font_size=22, color=manimlib.GREEN),
    height=2.5,
)
assert decimal_matrix.float_matrix == [[1, 2.5], [3.75, 4]]
assert all(
    isinstance(entry, manimlib.DecimalNumber)
    for entry in decimal_matrix.elements
)
assert np.allclose(
    [entry.get_value() for entry in decimal_matrix.elements],
    [1.0, 2.5, 3.75, 4.0],
)
assert decimal_matrix.get_height() <= 2.5 + 1e-6
integer_matrix = matrix_module.IntegerMatrix(
    [[1.2, 2.8]], decimal_config=dict(font_size=20)
)
assert all(isinstance(entry, manimlib.DecimalNumber) for entry in integer_matrix.elements)
tex_matrix = matrix_module.TexMatrix(
    [["x", "y"], ["z", "w"]],
    tex_config=dict(font_size=20, color=manimlib.RED),
    ellipses_row=0,
    ellipses_col=1,
)
assert len(tex_matrix.ellipses) == 3
assert len(tex_matrix.elements) == 1
assert tex_matrix.get_ellipses()[0] is tex_matrix.mob_matrix[0][0]
assert all(
    member.get_fill_color() == manimlib.WHITE
    for ellipse in tex_matrix.ellipses
    for member in ellipse.family_members_with_points()
)
assert all(
    member.get_fill_color() == manimlib.RED
    for entry in tex_matrix.elements
    for member in entry.family_members_with_points()
)

for invalid_matrix, expected_text in [
    ([], "at least one row"),
    ([[1], [2, 3]], "ragged matrix"),
]:
    try:
        matrix_module.Matrix(invalid_matrix)
    except ValueError as error:
        assert expected_text in str(error)
    else:
        raise AssertionError("an invalid Matrix shape reached live state")
for unsupported_matrix, expected_text in [
    ([[1.0, "x"]], "mixed float"),
    ([[1 + 2j]], "complex entries"),
    ([[manimlib.Circle()]], "VMobject entries"),
]:
    try:
        matrix_module.Matrix(unsupported_matrix)
    except NotImplementedError as error:
        assert expected_text in str(error)
    else:
        raise AssertionError("an unsupported Matrix entry silently succeeded")
try:
    matrix_module.TexMatrix([["x"]], tex_config={"template": "legacy"})
except NotImplementedError as error:
    assert "template" in str(error)
else:
    raise AssertionError("TexMatrix silently discarded an unsupported entry option")
try:
    tex_matrix.swap_entries_for_ellipses(0, 0)
except NotImplementedError as error:
    assert "constructor-time" in str(error)
else:
    raise AssertionError("post-construction Matrix ellipses silently succeeded")
try:
    integer_tex_matrix.get_row(-1)
except IndexError as error:
    assert "2 rows" in str(error)
else:
    raise AssertionError("Matrix.get_row accepted a negative index")

failed_torus = three_dimensions.Torus.__new__(three_dimensions.Torus)
try:
    three_dimensions.Torus.__init__(failed_torus, unrouted_option=True)
except NotImplementedError as error:
    assert str(error) == (
        "Torus() keyword(s) not yet routed to the native builder: "
        "unrouted_option"
    )
else:
    raise AssertionError("an unrouted Torus keyword reached the native builder")
assert not hasattr(failed_torus, "submobjects")

try:
    three_dimensions.Cone((0, math.tau), (0, 1), 7)
except TypeError as error:
    assert str(error) == (
        "Cylinder.__init__() got multiple values for argument 'u_range'"
    )
else:
    raise AssertionError("Cone accepted impossible Reference positional forwarding")

old_tex = importlib.import_module("manimlib.mobject.svg.old_tex_mobject").OldTex
quadratic_label = old_tex(
    "-{b \\over 2} \\pm \\sqrt{{b^2 \\over 4} - c}",
    font_size=30,
)[0]
assert len(quadratic_label) > 12, len(quadratic_label)
assert issubclass(InteractiveScene, Scene)
assert issubclass(Animation, object)
assert issubclass(manimlib.Group, Mobject)
assert issubclass(manimlib.VGroup, Mobject)
assert issubclass(manimlib.Axes, manimlib.VGroup)
assert issubclass(manimlib.EventType, enum.Enum)
assert issubclass(manimlib.EndScene, Exception)
assert manimlib.np is np

# Installing the Reference's optional OpenGL/window packages beside the wheel
# must not make the Rust portal initialize a second renderer or require an X11
# display.  Their leaked names remain import-compatible, but calling one names
# the exact unsupported semantic binding.
for refused_module, refused_name in (
    ("manimlib.camera.camera", "gl"),
    ("manimlib.camera.camera", "moderngl"),
    ("manimlib.mobject.interactive", "PygletWindowKeys"),
    ("manimlib.window", "Timer"),
    ("manimlib.window", "mglw"),
    ("manimlib.window", "screeninfo"),
):
    refused_value = getattr(importlib.import_module(refused_module), refused_name)
    try:
        refused_value()
    except NotImplementedError as error:
        assert str(error) == (
            f"{refused_module}.{refused_name} is present in the parity surface "
            "but its semantic binding has not landed"
        )
    else:
        raise AssertionError(
            f"Reference renderer leak {refused_module}.{refused_name} became live"
        )
pyglet_window = importlib.import_module("manimlib.window").PygletWindow
assert isinstance(pyglet_window, type)
try:
    pyglet_window()
except bridge_errors.CapabilityError as error:
    assert str(error) == (
        "the Reference's PygletWindow gateway is unavailable; "
        "FrankenManim Studio owns interactive windows"
    )
else:
    raise AssertionError("the Reference PygletWindow gateway became live")
assert not any(
    name == root or name.startswith(root + ".")
    for name in sys.modules
    for root in ("OpenGL", "moderngl", "moderngl_window", "pyglet", "screeninfo")
), sorted(
    name
    for name in sys.modules
    if name.split(".", 1)[0]
    in {"OpenGL", "moderngl", "moderngl_window", "pyglet", "screeninfo"}
)
assert hasattr(Mobject, "add_event_listner")
assert Mobject.add_event_listener is Mobject.add_event_listner
assert not hasattr(manimlib, "__all__"), repr(getattr(manimlib, "__all__", None))

exported = set()
classes = set()
in_symbols = False
for raw_line in manimlib._API_SCHEMA_TSV.splitlines():
    line = raw_line.strip()
    if line.startswith("[") and line.endswith("]"):
        in_symbols = line == "[symbols]"
        continue
    if not in_symbols or not line or line.startswith("#"):
        continue
    columns = raw_line.split("\t", 5)
    if len(columns) != 6:
        continue
    module_name, name, kind, _origin, is_exported, _detail = columns
    if kind == "class" and "." not in name:
        classes.add((module_name, name))
    if is_exported == "1" and "." not in name:
        exported.add(name)
missing = sorted(name for name in exported if not hasattr(manimlib, name))
assert not missing, missing[:20]
assert len(classes) >= 250
assert len(exported) >= 600
for module_name, name in classes:
    candidate = getattr(importlib.import_module(module_name), name)
    assert isinstance(candidate, type), (module_name, name, candidate)
    types.new_class(f"_SubclassProbe_{name}", (candidate,))
actual = {name for name in vars(manimlib) if not name.startswith("_")}
assert actual == exported, sorted(actual ^ exported)[:20]


# ---------------------------------------------------------------------------
# fm-zoi (W10 binding tax, §15.2 Rev 4): method-resolution cache, batched
# crossings (rung 1), CrossingStats instrumentation, and GIL release.
# All assertions are count/state based — no wall-clock thresholds.
# ---------------------------------------------------------------------------

# The opt-in rung-1 API exists beside the always-correct rung 0.
assert callable(Scene.update)
assert callable(Scene.update_batched)


# METHOD-RESOLUTION CACHE — correct under Python subclass mutation.
class CacheProbe(Mobject):
    def init_points(self):
        self.seen = []
        self.resize(1)
        self.set_field("point", 0, [0.0, 0.0, 0.0])

    def _dispatch_updater(self, updater, dt):
        self.seen.append("original")
        return super()._dispatch_updater(updater, dt)


cache_scene = Scene()
probe = CacheProbe()
cache_scene.add(probe)
probe_ticks = []
probe.add_updater(lambda mob, dt: probe_ticks.append(dt), call=False)
manimlib._method_cache_reset()
cache_scene.update(0.25)  # first dispatch resolves and caches
assert probe.seen == ["original"]
assert probe_ticks == [0.25]
cache_stats_first = manimlib._method_cache_stats()
assert cache_stats_first["misses"] >= 1
cache_scene.update(0.25)  # second dispatch hits the cache
assert probe.seen == ["original", "original"]
cache_stats_second = manimlib._method_cache_stats()
assert cache_stats_second["hits"] > cache_stats_first["hits"]


def patched_dispatch(self, updater, dt):
    self.seen.append("patched")
    return Mobject._dispatch_updater(self, updater, dt)


CacheProbe._dispatch_updater = patched_dispatch  # class mutation
cache_scene.update(0.25)
assert probe.seen == ["original", "original", "patched"]
cache_stats_patched = manimlib._method_cache_stats()
assert cache_stats_patched["invalidations"] > cache_stats_second["invalidations"]
assert probe_ticks == [0.25, 0.25, 0.25]


# Base-class mutation recursively invalidates subclass cache entries.
class BaseProbe(Mobject):
    def init_points(self):
        self.resize(1)
        self.set_field("point", 0, [0.0, 0.0, 0.0])


base_scene = Scene()
base_probe = BaseProbe()
base_scene.add(base_probe)
base_ticks = []
base_probe.add_updater(lambda mob, dt: base_ticks.append(dt), call=False)
base_scene.update(0.5)  # caches (BaseProbe, _dispatch_updater) → Mobject's
assert base_ticks == [0.5]
saved_mobject_dispatch = Mobject._dispatch_updater
base_calls = []


def base_patched(self, updater, dt):
    base_calls.append(dt)
    return saved_mobject_dispatch(self, updater, dt)


Mobject._dispatch_updater = base_patched
try:
    base_scene.update(0.5)
    assert base_calls == [0.5]
    assert base_ticks == [0.5, 0.5]
finally:
    Mobject._dispatch_updater = saved_mobject_dispatch


# Instance-__dict__ shadowing still wins over the class-level cache.
shadow = BaseProbe()
shadow_scene = Scene()
shadow_scene.add(shadow)
shadow_ticks = []
shadow.add_updater(lambda mob, dt: shadow_ticks.append(dt), call=False)
shadow._dispatch_updater = lambda updater, dt: shadow_ticks.append("shadow")
shadow_scene.update(1.0)
assert shadow_ticks == ["shadow"]


# BATCHED CROSSINGS (rung 1) — identical ordering and observable state with
# measurably fewer crossings than rung 0 on the same callback corpus.
def make_updater_scene(count, updaters_each):
    scene = Scene()
    mobjects = []
    for index in range(count):
        mob = Mobject()
        mob.resize(1)
        mob.set_field("point", 0, [float(index), 0.0, 0.0])

        def make_tick(step):
            def tick(m, dt):
                point = m.get_field("point", 0)
                m.set_field("point", 0, [point[0] + step * dt, point[1], point[2]])

            return tick

        for k in range(updaters_each):
            mob.add_updater(make_tick(k + 1), call=False)
        mobjects.append(mob)
    scene.add(*mobjects)
    return scene, mobjects


N_MOBS, N_UPDATERS = 6, 4
scene_rung0, mobs_rung0 = make_updater_scene(N_MOBS, N_UPDATERS)
scene_rung1, mobs_rung1 = make_updater_scene(N_MOBS, N_UPDATERS)

manimlib._crossing_stats_reset()
scene_rung0.update(1.0)
rung0 = manimlib._crossing_stats_snapshot()

manimlib._crossing_stats_reset()
scene_rung1.update_batched(1.0)
rung1 = manimlib._crossing_stats_snapshot()

assert rung0["updater_call"] == N_MOBS * N_UPDATERS
assert rung1["updater_call"] == 0
assert rung1["method_dispatch"] == 1, rung1
assert rung1["dirty_propagation"] == 1
# Field writes are inherent to the updaters and equal across rungs.
assert rung0["field_write"] == rung1["field_write"] == N_MOBS * N_UPDATERS
assert rung1["total"] < rung0["total"]
# Exact deterministic counts: rung 0 pays 24 updater crossings + 7 updater
# list snapshots (the camera frame first, then 6 drawable roots) on top of
# the 48 inherent field-I/O crossings; rung 1 pays one batch dispatch + one
# batched dirty-propagation return.
assert rung0["other"] == 1 + N_MOBS + N_MOBS * N_UPDATERS
assert rung0["total"] == (
    2 * N_MOBS * N_UPDATERS + 1 + N_MOBS + N_MOBS * N_UPDATERS
)
assert rung1["other"] == N_MOBS * N_UPDATERS
assert rung1["total"] == 2 * N_MOBS * N_UPDATERS + 2
assert rung0["python_callback_ns"] > 0
assert rung1["python_callback_ns"] > 0
# Bit-equality vs rung 0 after the frame, and updater ordering preserved:
# updater k adds (k + 1) * dt to lane 0, so the sum is 1+2+3+4 = 10.
for left, right in zip(mobs_rung0, mobs_rung1):
    assert left.get_field("point", 0) == right.get_field("point", 0)
assert mobs_rung1[0].get_field("point", 0)[0] == 10.0


# Exceptions still propagate intact through the single batched crossing.
def boom(mob, dt):
    raise KeyError("batched updater boom")


batch_exploding = Mobject()
batch_exploding.resize(1)
batch_exploding.set_field("point", 0, [0.0, 0.0, 0.0])
batch_exploding.add_updater(boom, call=False)
batch_scene = Scene()
batch_scene.add(batch_exploding)
try:
    batch_scene.update_batched(0.1)
except KeyError as error:
    assert "batched updater boom" in str(error)
else:
    raise AssertionError("batched updater exception did not propagate")


# GIL RELEASE — a Python thread makes progress (counter increments) during a
# long detached native kernel; the pass/fail signal is the counter alone.
gil_probe = manimlib._GilProbe()
gil_stop = []


def gil_spin():
    while not gil_probe.native_started():
        pass
    while not gil_stop:
        gil_probe.tick()


gil_worker = threading.Thread(target=gil_spin)
gil_worker.start()
gil_observed = gil_probe.run_native(40_000_000)
gil_stop.append(True)
gil_worker.join()
assert gil_observed > 0, "no Python progress during the detached native kernel"
assert gil_probe.observed() >= gil_observed


# W11 PORTAL CONSOLE — package/version identity, stable robot records, source
# discovery, explicit construct-only diagnostics, and a real standard-mode PNG
# sequence through Lumen + Reel. Unsupported certified/Studio contracts still
# refuse before source access; lifecycle-only success is never called render.
def run_portal_console(*arguments):
    previous = sys.argv
    stdout = io.StringIO()
    stderr = io.StringIO()
    sys.argv = ["fmn-python", *arguments]
    try:
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            code = manimlib._console_main()
    finally:
        sys.argv = previous
    return code, stdout.getvalue(), stderr.getvalue()


assert manimlib.__version__ == _expected_package_version
assert manimlib.__distribution__ == "franken-manim"
assert manimlib.__franken_manim__ is True
assert manimlib.__abi_policy__ == "cpython-3.13-full-abi"

console_code, console_out, console_err = run_portal_console("--version")
assert console_code == 0
assert console_out.startswith(
    f"fmn-python {_expected_package_version} (CPython 3.13."
), console_out
assert console_err == ""

console_code, console_out, console_err = run_portal_console("--robot", "--version")
assert console_code == 0
console_version = json.loads(console_out)
assert console_version["schema"] == "fmn-python.cli"
assert console_version["version"] == 1
assert console_version["kind"] == "version"
assert console_version["program_version"] == _expected_package_version
assert console_version["abi_policy"] == "cpython-3.13-full-abi"
assert console_version["exit"] == {"code": 0, "identity": "success"}
assert console_err == ""

# Certified refusal precedes even source-file access: partial certified pixels
# without the content closure and provenance sidecar are not certification.
console_code, console_out, console_err = run_portal_console(
    "--reproducible", "missing-source.py", "MissingScene"
)
assert console_code == 4
assert console_out == ""
assert "render-capability-unavailable" in console_err
assert "input closure" in console_err

def verify_portal_console_scene():
    source = pathlib.Path(__file__).with_name("console_scene.py")
    console_code, console_out, console_err = run_portal_console(
        "--robot", "--list-scenes", str(source)
    )
    assert console_code == 0
    scene_list = json.loads(console_out)
    assert scene_list["kind"] == "scene-list"
    assert scene_list["scenes"] == ["Hello"]
    assert console_err == ""

    console_code, console_out, console_err = run_portal_console(
        "--robot", "--construct-only", str(source), "Hello"
    )
    assert console_code == 0
    constructed = json.loads(console_out)
    assert constructed["kind"] == "construct-only"
    assert constructed["scene"] == "Hello"
    assert constructed["root_count"] == 3
    assert constructed["family_count"] > constructed["root_count"]
    assert constructed["scene_time"] == 3.0
    assert constructed["rendered"] is False
    assert constructed["exit"] == {"code": 0, "identity": "success"}
    assert console_err == ""

    output_root = pathlib.Path(tempfile.mkdtemp(prefix="fmn-portal-render-"))
    destination = output_root / "frames"
    blocked_still_destination = output_root / "blocked.png"
    blocked_still_destination.mkdir()
    console_code, console_out, console_err = run_portal_console(
        "--robot",
        str(source),
        "Hello",
        "--format",
        "png_sequence",
        "--resolution",
        "96x54",
        "--fps",
        "30",
        "--threads",
        "1",
        "--video_dir",
        str(destination),
    )
    assert console_code == 0, console_err
    rendered = json.loads(console_out)
    assert rendered["kind"] == "render"
    assert rendered["scene"] == "Hello"
    assert rendered["format"] == "png_sequence"
    assert rendered["resolution"] == [96, 54]
    assert rendered["fps"] == 30
    assert rendered["rendered"] is True
    assert rendered["frame_count"] == 90
    assert rendered["bytes"] > 0
    assert len(rendered["digest"]) == 64
    engine_parts = rendered["engine"].split(":")
    assert len(engine_parts) == 3
    assert engine_parts[0] == "fast-cpu"
    assert engine_parts[2].isdigit()
    assert rendered["threads"] == 1
    assert pathlib.Path(rendered["destination"]) == destination
    frames = sorted(destination.glob("frame_*.png"))
    assert len(frames) == rendered["frame_count"]
    assert sum(frame.stat().st_size for frame in frames) == rendered["bytes"]
    for frame in frames:
        assert frame.read_bytes().startswith(b"\x89PNG\r\n\x1a\n")
    assert_red_orientation_witness_is_above_origin(frames[0])
    assert console_err == ""

    # Atlas already owns PNG/JPEG decode and ImageQuad construction; the
    # portal must preserve that resource across detached copy, Scene
    # adoption, and the production Lumen/Reel render rather than exposing a
    # schema shell or decoding a second Python-side raster.
    source_width, source_height, source_rows = png_rgba8_rows(frames[0])
    image = manimlib.ImageMobject(str(frames[0]), height=2.0)
    assert image.data.dtype.names == ("point", "im_coords", "opacity")
    assert image.get_num_points() == 6
    assert math.isclose(image.get_height(), 2.0, rel_tol=0.0, abs_tol=1e-6)
    assert math.isclose(
        image.get_width(),
        2.0 * source_width / source_height,
        rel_tol=0.0,
        abs_tol=1e-6,
    )
    assert np.array_equal(
        image.data["im_coords"],
        np.array([(0, 0), (0, 1), (1, 0), (1, 1), (1, 0), (0, 1)]),
    )
    source_top_left = np.array(source_rows[0][:3], dtype=float) / 255.0
    assert np.allclose(
        image.point_to_rgb(image.get_corner(manimlib.UL)),
        source_top_left,
        rtol=0.0,
        atol=1e-12,
    )
    try:
        image.point_to_rgb(image.get_right() + manimlib.RIGHT)
    except ValueError as error:
        assert "outside an image" in str(error)
    else:
        raise AssertionError("ImageMobject sampled outside its live quad")
    before_color = image.data.copy()
    assert image.set_color(manimlib.RED, opacity=0.1) is image
    assert np.array_equal(image.data, before_color)
    assert image.set_opacity([0.25, 0.75]) is image
    assert math.isclose(image.data["opacity"][0, 0], 0.25)
    assert math.isclose(image.data["opacity"][-1, 0], 0.75)
    image.shift([0.25, -0.5, 0.0])
    pickled_image = pickle.loads(  # ubs:ignore -- trusted round-trip created immediately here
        pickle.dumps(image, protocol=pickle.HIGHEST_PROTOCOL)
    )
    for image_clone in (image.copy(), copy.deepcopy(image), pickled_image):
        assert type(image_clone) is type(image)
        assert image_clone._image_dimensions() == (source_width, source_height)
        assert np.array_equal(image_clone.data, image.data)
        assert np.allclose(image_clone.get_center(), image.get_center())
        assert np.allclose(
            image_clone.point_to_rgb(image_clone.get_corner(manimlib.UL)),
            image.point_to_rgb(image.get_corner(manimlib.UL)),
            rtol=0.0,
            atol=1e-12,
        )

    # Extension-less local lookup reaches the same native decoder. A URL is
    # a named capability refusal and a non-image file fails in Atlas before a
    # usable object can escape construction.
    extensionless = manimlib.ImageMobject(
        str(frames[0].with_suffix("")), height=1.0
    )
    assert (extensionless.pixel_width, extensionless.pixel_height) == (
        source_width,
        source_height,
    )
    try:
        manimlib.ImageMobject("https://example.invalid/image.png")
    except bridge_errors.CapabilityError as error:
        assert "AssetFetcher" in str(error)
    else:
        raise AssertionError("ImageMobject silently accepted a network URL")
    try:
        manimlib.ImageMobject(str(source))
    except ValueError as error:
        assert "not a recognized image" in str(error)
    else:
        raise AssertionError("ImageMobject accepted non-image bytes")

    image_destination = output_root / "image-mobject.png"
    image_scene = InteractiveScene()
    image_scene._begin_png(str(image_destination), 160, 90, 30, 1, 0)
    # Fill the camera height so nearest-neighbour sampling cannot legitimately
    # skip the source's deliberately small orientation witness.
    rendered_image = image.copy().set_opacity(1.0).set_height(8.0)
    assert rendered_image._image_dimensions() == (source_width, source_height)
    image_scene.add(rendered_image)
    image_receipt = image_scene._finish_render(
        image_scene.frame._core,
        image_scene.camera.light_source.get_center(),
    )
    assert pathlib.Path(image_receipt[0]) == image_destination
    assert image_receipt[1] == 1
    assert image_receipt[2] == image_destination.stat().st_size
    assert len(image_receipt[3]) == 64
    assert image_receipt[4].split(":")[0] == "fast-cpu"
    rendered_width, rendered_height, rendered_rows = png_rgba8_rows(
        image_destination
    )
    assert (rendered_width, rendered_height) == (160, 90)
    assert sum(
        tuple(row[offset : offset + 3]) != (51, 51, 51)
        for row in rendered_rows
        for offset in range(0, len(row), 4)
    ) >= 8

    # The independently unpickled image carries the same ImageResource,
    # ImageQuad primitive, placement, and opacity field into production
    # composition. Canonical PNG equality is the end-to-end witness.
    pickled_image_destination = output_root / "pickled-image-mobject.png"
    pickled_image_scene = InteractiveScene()
    pickled_image_scene._begin_png(
        str(pickled_image_destination), 160, 90, 30, 1, 0
    )
    pickled_rendered_image = pickled_image.copy().set_opacity(1.0).set_height(8.0)
    pickled_image_scene.add(pickled_rendered_image)
    pickled_image_receipt = pickled_image_scene._finish_render(
        pickled_image_scene.frame._core,
        pickled_image_scene.camera.light_source.get_center(),
    )
    assert pathlib.Path(pickled_image_receipt[0]) == pickled_image_destination
    assert pickled_image_receipt[1] == 1
    assert pickled_image_destination.read_bytes() == image_destination.read_bytes()

    # Same build, policy, seed, and thread count reproduce the exact ordered
    # PNG-tree digest through an independently published generation.
    repeated_destination = output_root / "repeated"
    console_code, console_out, console_err = run_portal_console(
        "--robot",
        str(source),
        "Hello",
        "--resolution=96x54",
        "--fps=30",
        "--threads=1",
        f"--video_dir={repeated_destination}",
    )
    assert console_code == 0, console_err
    repeated = json.loads(console_out)
    assert hmac.compare_digest(repeated["digest"], rendered["digest"])
    assert repeated["bytes"] == rendered["bytes"]
    assert repeated["frame_count"] == rendered["frame_count"]

    # Final-state PNG uses the same production composition root but skips
    # intermediate raster captures. It must publish exactly one atomic file.
    still_destination = output_root / "final.png"
    console_code, console_out, console_err = run_portal_console(
        "--robot",
        str(source),
        "Hello",
        "--format",
        "png",
        "--resolution",
        "96x54",
        "--fps",
        "30",
        "--threads",
        "1",
        "--video_dir",
        str(still_destination),
    )
    assert console_code == 0, console_err
    still = json.loads(console_out)
    assert still["kind"] == "render"
    assert still["format"] == "png"
    assert still["frame_count"] == 1
    assert still["bytes"] == still_destination.stat().st_size
    assert len(still["digest"]) == 64
    assert still["engine"].split(":")[0] == "fast-cpu"
    assert still["threads"] == 1
    assert pathlib.Path(still["destination"]) == still_destination
    still_bytes = still_destination.read_bytes()
    assert still_bytes.startswith(b"\x89PNG\r\n\x1a\n")
    assert_red_orientation_witness_is_above_origin(still_destination)
    assert console_err == ""

    repeated_still_destination = output_root / "repeated-final.png"
    console_code, console_out, console_err = run_portal_console(
        "--robot",
        str(source),
        "Hello",
        "--format=png",
        "--resolution=96x54",
        "--fps=30",
        "--threads=1",
        f"--video_dir={repeated_still_destination}",
    )
    assert console_code == 0, console_err
    repeated_still = json.loads(console_out)
    assert repeated_still["frame_count"] == 1
    assert hmac.compare_digest(repeated_still["digest"], still["digest"])
    assert repeated_still_destination.read_bytes() == still_bytes

    # Direct public Surface partial reveal is production behavior, not only a
    # structural bridge assertion: its collapsed native UV grid must survive
    # Scene adoption and reach visible Lumen/Reel pixels.
    surface_destination = output_root / "surface-partial.png"
    surface_render_scene = InteractiveScene()
    surface_render_scene._begin_png(
        str(surface_destination), 160, 90, 30, 1, 0
    )
    surface_render_source = manimlib.ParametricSurface(
        lambda u, v: np.array([u, v, 0.0]),
        u_range=(-3.0, 3.0),
        v_range=(-2.0, 2.0),
        resolution=(11, 9),
        color=manimlib.RED,
        opacity=1.0,
    )
    surface_render_target = surface_render_source.copy()
    assert (
        surface_render_target.pointwise_become_partial(
            surface_render_source, 0.2, 0.8, axis=0
        )
        is surface_render_target
    )
    surface_render_scene.add(surface_render_target)
    surface_receipt = surface_render_scene._finish_render(
        surface_render_scene.frame._core,
        surface_render_scene.camera.light_source.get_center(),
    )
    assert pathlib.Path(surface_receipt[0]) == surface_destination
    assert surface_receipt[1] == 1
    assert surface_receipt[2] == surface_destination.stat().st_size
    assert len(surface_receipt[3]) == 64
    assert surface_receipt[4].split(":")[0] == "fast-cpu"
    surface_width, surface_height, surface_rows = png_rgba8_rows(
        surface_destination
    )
    assert (surface_width, surface_height) == (160, 90)
    assert sum(
        tuple(row[offset : offset + 3]) != (51, 51, 51)
        for row in surface_rows
        for offset in range(0, len(row), 4)
    ) >= 32

    # A generic open VMobject remains visibly stroked after ShowCreation,
    # subsequent camera animation, and an ambient-rotation wait.  This is the
    # minimal production form of SquareOnASphere's three elbow marks: the
    # object survives structurally either way, so only decoded pixels catch a
    # zero-filled first-allocation opacity lane.
    elbow_destination = output_root / "camera-elbow.png"
    elbow_scene = InteractiveScene()
    elbow_scene._begin_png(str(elbow_destination), 160, 90, 30, 1, 0)
    elbow_frame = elbow_scene.frame
    elbow_frame.reorient(12, 70, 0, manimlib.ORIGIN, 4)
    elbow = VMobject().set_points_as_corners(
        [[-1.0, -0.5, 0.0], [0.0, -0.5, 0.0], [0.0, 0.75, 0.0]]
    )
    elbow.set_stroke(manimlib.WHITE, 4)
    elbow_scene.play(
        manimlib.ShowCreation(elbow, time_span=(0, 1 / 30)),
        elbow_frame.animate.reorient(20, 60, 0),
        run_time=2 / 30,
        rate_func=manimlib.linear,
    )
    elbow_scene.play(
        elbow_frame.animate.reorient(6, 84, 0),
        run_time=1 / 30,
        rate_func=manimlib.linear,
    )
    elbow_frame.add_ambient_rotation(2 * manimlib.DEG)
    elbow_scene.wait(1 / 30)
    assert np.isclose(
        elbow_frame.get_theta(),
        (6 + 2 / 30) * manimlib.DEG,
        rtol=0.0,
        atol=1e-12,
    )
    assert np.allclose(elbow.data["stroke_rgba"][:, 3], 1.0)
    elbow_receipt = elbow_scene._finish_render(
        elbow_frame._core,
        elbow_scene.camera.light_source.get_center(),
    )
    assert pathlib.Path(elbow_receipt[0]) == elbow_destination
    assert elbow_receipt[1] == 1
    assert elbow_receipt[2] == elbow_destination.stat().st_size
    assert len(elbow_receipt[3]) == 64
    assert elbow_receipt[4].split(":")[0] == "fast-cpu"
    assert elbow_receipt[5] == 1
    assert_white_stroke_witness(elbow_destination)

    # The camera frame is intentionally not a drawable Stage root, but the
    # Reference places it first in Scene.mobjects for updater dispatch.  Both
    # crossing rungs, wait, ordinary play, and camera-only play must preserve
    # frame-before-root ordering.  The camera-only observation also proves
    # frame interpolation lands before the updater phase rather than inside
    # the later capture callback.
    updater_scene = InteractiveScene()
    updater_frame = updater_scene.frame
    updater_root = manimlib.Dot()
    updater_scene.add(updater_root)
    updater_events = []

    def record_frame_updater(mobject, dt):
        del mobject
        updater_events.append(("frame", dt))

    def record_root_updater(mobject, dt):
        del mobject
        updater_events.append(("root", dt))

    updater_frame.add_updater(record_frame_updater)
    updater_root.add_updater(record_root_updater)
    updater_events.clear()
    updater_scene.update_batched(0.125)
    assert updater_events == [("frame", 0.125), ("root", 0.125)]

    updater_events.clear()
    updater_scene.wait(1 / 30)
    assert [kind for kind, _ in updater_events] == [
        "frame",
        "root",
        "frame",
        "root",
    ]
    assert np.allclose(
        [dt for _, dt in updater_events],
        [0.0, 0.0, 1 / 30, 1 / 30],
        rtol=0.0,
        atol=1e-15,
    )

    updater_events.clear()
    updater_scene.play(
        updater_root.animate.shift(manimlib.RIGHT),
        run_time=1 / 30,
        rate_func=manimlib.linear,
    )
    assert [kind for kind, _ in updater_events] == [
        "frame",
        "root",
        "frame",
        "root",
    ]
    assert np.allclose(
        [dt for _, dt in updater_events],
        [1 / 30, 1 / 30, 0.0, 0.0],
        rtol=0.0,
        atol=1e-15,
    )

    camera_only_scene = InteractiveScene()
    camera_only_frame = camera_only_scene.frame
    camera_observations = []

    def observe_camera_updater(mobject, dt):
        camera_observations.append((mobject.get_theta(), dt))

    camera_only_frame.add_updater(observe_camera_updater)
    camera_observations.clear()
    camera_only_scene.play(
        camera_only_frame.animate.reorient(30, 70, 0),
        run_time=1 / 30,
        rate_func=manimlib.linear,
    )
    assert len(camera_observations) == 2
    assert np.allclose(
        [theta for theta, _ in camera_observations],
        [30 * manimlib.DEG, 30 * manimlib.DEG],
        rtol=0.0,
        atol=1e-12,
    )
    assert np.allclose(
        [dt for _, dt in camera_observations],
        [1 / 30, 0.0],
        rtol=0.0,
        atol=1e-15,
    )

    # LineBrace follows the Reference's two-stage transform: flatten the
    # target around its own centre, build the brace, then restore both around
    # that same centre.  Rotating the flattened anchor around the world origin
    # instead sends translated annotations off-screen, as BeamSplitter's
    # amplitude brace demonstrated.  Keep the translated, angled geometry and
    # the adopted fixed-frame pixels in one production regression.
    brace_destination = output_root / "fixed-line-brace.png"
    brace_scene = InteractiveScene()
    brace_scene._begin_png(str(brace_destination), 160, 90, 30, 1, 0)
    brace_frame = brace_scene.frame
    brace_frame.reorient(-55, 68, 0, [1.0, -1.0, 0.0], 6)
    brace_start = np.array([-2.5, -0.5, 0.0])
    brace_end = np.array([0.5, 1.0, 0.0])
    brace_line = geometry.Line(brace_start, brace_end)
    unit_tangent = (brace_end - brace_start) / np.linalg.norm(brace_end - brace_start)
    unit_normal = np.array([-unit_tangent[1], unit_tangent[0], 0.0])
    line_brace = manimlib.LineBrace(brace_line, buff=0.1).fix_in_frame()
    brace_label = line_brace.get_tex("1").fix_in_frame()
    relative_points = line_brace.get_points() - brace_line.get_center()
    tangent_projections = relative_points @ unit_tangent
    normal_projections = relative_points @ unit_normal
    assert np.isclose(tangent_projections.min(), -brace_line.get_length() / 2)
    assert np.isclose(tangent_projections.max(), brace_line.get_length() / 2)
    assert np.isclose(normal_projections.min(), 0.1, rtol=0, atol=1e-6), (
        normal_projections.min(),
        normal_projections.max(),
        line_brace.get_direction(),
    )
    assert np.dot(line_brace.get_direction(), unit_normal) > 1 - 1e-8
    assert np.dot(brace_label.get_center() - line_brace.get_tip(), unit_normal) > 0
    brace_scene.play(
        manimlib.GrowFromCenter(line_brace),
        manimlib.Write(brace_label),
        brace_frame.animate.reorient(-25, 76, 0),
        run_time=1 / 30,
        rate_func=manimlib.linear,
    )
    brace_family = {
        id(member)
        for root in brace_scene.get_mobjects()
        for member in root.get_family()
    }
    assert id(line_brace) in brace_family
    assert id(brace_label) in brace_family
    brace_receipt = brace_scene._finish_render(
        brace_frame._core,
        brace_scene.camera.light_source.get_center(),
    )
    assert pathlib.Path(brace_receipt[0]) == brace_destination
    assert brace_receipt[1] == 1
    assert brace_receipt[2] == brace_destination.stat().st_size
    assert len(brace_receipt[3]) == 64
    assert brace_receipt[4].split(":")[0] == "fast-cpu"
    assert brace_receipt[5] == 1
    assert_white_stroke_witness(brace_destination, minimum_pixels=20)

    console_code, console_out, console_err = run_portal_console(
        "--robot",
        str(source),
        "Hello",
        "--format",
        "png",
        "--resolution",
        "96x54",
        "--fps",
        "30",
        "--threads",
        "1",
        "--video_dir",
        str(blocked_still_destination),
    )
    assert console_code == 6
    blocked_still = json.loads(console_out)
    assert blocked_still["exit"] == {"code": 6, "identity": "render"}
    assert blocked_still["kind"] in ("render-failed", "render-finish-failed")
    assert blocked_still_destination.is_dir()
    assert still_destination.read_bytes() == still_bytes
    assert console_err == ""

    # Reel's atomic-directory publication is no-clobber. A second generation
    # targeting an already published root fails as render/output, and the
    # first generation remains byte-for-byte intact.
    original_frames = {frame.name: frame.read_bytes() for frame in frames}
    console_code, console_out, console_err = run_portal_console(
        "--robot",
        str(source),
        "Hello",
        "--resolution",
        "96x54",
        "--fps",
        "30",
        "--threads",
        "1",
        "--video_dir",
        str(destination),
    )
    assert console_code == 6
    clobber_error = json.loads(console_out)
    assert clobber_error["exit"] == {"code": 6, "identity": "render"}
    assert clobber_error["kind"] in ("render-failed", "render-finish-failed")
    assert {frame.name: frame.read_bytes() for frame in frames} == original_frames
    assert console_err == ""

    # The native composition method has no certified-mode parameter at all;
    # an extra mode argument cannot bypass the console's fail-closed boundary.
    direct_certified = manimlib.Scene()
    try:
        direct_certified._begin_png_sequence(
            str(output_root / "forbidden-certified"), 96, 54, 30, 1, True, 0
        )
    except TypeError as error:
        assert "positional arguments" in str(error)
    else:
        raise AssertionError("direct certified portal rendering did not refuse")

    console_code, console_out, console_err = run_portal_console(
        "--robot", "studio", "missing-source.py", "MissingScene"
    )
    assert console_code == 4
    studio_error = json.loads(console_out)
    assert studio_error["kind"] == "studio-unavailable"
    assert studio_error["exit"] == {"code": 4, "identity": "capability"}
    assert console_err == ""

    console_code, console_out, console_err = run_portal_console(
        "--robot", "--construct-only", str(source), "NoSuchScene"
    )
    assert console_code == 5
    scene_error = json.loads(console_out)
    assert scene_error["kind"] == "construct-failed"
    assert scene_error["exit"] == {"code": 5, "identity": "scene"}
    assert "NoSuchScene" in scene_error["message"]
    assert console_err == ""


verify_portal_console_scene()
