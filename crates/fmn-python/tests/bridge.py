"""Permanent W10 acceptance tests executed inside fmn-python's Rust test.

The file is Python rather than Rust because the contract under test is ordinary
Python behavior: MRO, live NumPy arrays, weakrefs, descriptors, copy/pickle,
module imports, and callback reentrancy.
"""

import copy
import enum
import gc
import importlib
import inspect
import math
import pickle
import re
import sys
import threading
import types
import weakref

import numpy as np

import manimlib
from manimlib import Animation, InteractiveScene, Mobject, Scene, VMobject

bridge_errors = importlib.import_module("manimlib.exceptions")

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
payload = pickle.dumps(pickled_parent, protocol=pickle.HIGHEST_PROTOCOL)
restored = pickle.loads(payload)  # ubs:ignore -- trusted round-trip created immediately above
assert restored.label is restored.submobjects[0]
assert restored.get_field("point", 0) == [3.0, 4.0, 5.0]
Scene().add(restored)
assert restored.is_alive()


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

# Shape matchers consume the real family bounding box and retain the native
# empty-target rule.  A generated schema shell used to fail here before any
# geometry reached Atlas.
shape_matchers = importlib.import_module("manimlib.mobject.shape_matchers")
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

bound_empty_target = Mobject()
matcher_scene.add(bound_empty_target, empty_surround)
empty_surround.surround(bound_empty_target)
assert not empty_surround.has_points()
empty_surround.surround(box_group, buff=0.2)
assert empty_surround.has_points()
assert np.allclose(
    empty_surround.get_bounding_box()[2][:2], live_target_box[2][:2] + 0.2
)

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
assert len(release_observations) == 1
assert np.allclose(release_observations[0], [1.0 / 30.0, 1.0 / 30.0, 1.0])
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
field_axes = manimlib.Axes(
    x_range=(0.0, 2.0, 1.0),
    y_range=(-1.0, 1.0, 1.0),
    width=2.0,
    height=2.0,
)
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

# Fixed-frame state is the renderer-consumed typed uniform, including the
# Reference's recursive family default and non-recursive override.
fixed_child = geometry.Square()
fixed_group = manimlib.Group(fixed_child)
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
dashed = geometry.DashedLine([-2.0, 1.0, 0.0], [2.0, 1.0, 0.0])
dashed_start = dashed.get_start().copy()
assert dashed.set_angle(math.pi / 2.0) is dashed
assert np.allclose(dashed.get_start(), dashed_start)
assert np.allclose(dashed.get_unit_vector(), [0.0, 1.0, 0.0], atol=1e-9)

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

# Existing Chisel/Scribe semantics are live through the portal, rather than
# being shadowed by schema placeholders. Arc length remains true for either
# curvature sign (BN-03), and Tex selectors consume the native UTF-8 span map.
line = geometry.Line((-1.0, 0.0, 0.0), (1.0, 0.0, 0.0))
assert math.isclose(line.get_arc_length(), 2.0, rel_tol=0.0, abs_tol=1e-9)
curved_line = geometry.Line(
    (-1.0, 0.0, 0.0),
    (1.0, 0.0, 0.0),
    path_arc=-math.pi / 2.0,
)
assert curved_line.get_arc_length() > curved_line.get_length()
assert curved_line.get_arc_length() == VMobject.get_arc_length(curved_line, 2)
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
line.set_stroke(manimlib.BLACK, 3, background=True)
assert line.uniforms["stroke_behind"] is True
try:
    line.set_stroke(behind=True, background=False)
except TypeError as error:
    assert "conflicting behind/background" in str(error)
else:
    raise AssertionError("conflicting era-shim kwargs must be rejected")

tex = manimlib.Tex("E = mc^2", isolate=["mc"])
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
assert curve.has_points()
assert Mobject().get_num_points() == 0
assert curve.get_num_points() == len(curve.get_points())
assert curve.get_num_curves() == curve.get_num_points() // 2
assert np.allclose(curve.get_point_from_function(0.5), [0.5, 0.25, 0.0])
assert curve.get_t_func() is curve.t_func
assert curve.get_arc_length() > math.sqrt(2.0)

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
assert hasattr(Mobject, "add_event_listner")
assert Mobject.add_event_listener is Mobject.add_event_listner
assert not hasattr(manimlib, "__all__")

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
assert rung1["method_dispatch"] == 1
assert rung1["dirty_propagation"] == 1
# Field writes are inherent to the updaters and equal across rungs.
assert rung0["field_write"] == rung1["field_write"] == N_MOBS * N_UPDATERS
assert rung1["total"] < rung0["total"]
# Exact deterministic counts: rung 0 pays 24 updater crossings + 6 updater
# list snapshots on top of the 48 inherent field-I/O crossings; rung 1 pays
# one batch dispatch + one batched dirty-propagation return.
assert rung0["other"] == N_MOBS + N_MOBS * N_UPDATERS
assert rung0["total"] == 2 * N_MOBS * N_UPDATERS + N_MOBS + N_MOBS * N_UPDATERS
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
