"""Native record/family copying with authored ndarray state; no storage doubles."""
from __future__ import annotations

import copy
import unittest

import numpy as np
import manimlib as m
from fmn_python.copying import install_mobject_copying


class DataSquare(m.Square):
    def __init__(self, **kwargs):
        super().__init__(**kwargs)
        self.controls = np.array([0., 1., 2.])

    def set_control(self, value):
        self.controls[0] = value
        self.set_x(value)
        return self


class NativeCopyingTests(unittest.TestCase):
    def root(self, bound):
        root = m.Group(DataSquare(), DataSquare())
        scene = m.Scene()
        if bound:
            scene.add(root)
        return scene, root

    def test_shallow_copy_entrypoints_detach_all_family_arrays(self):
        for bound in (False, True):
            for copier in (lambda obj: obj.copy(), copy.copy):
                with self.subTest(bound=bound, copier=copier):
                    scene, root = self.root(bound)
                    root.authored = np.arange(6).reshape(2, 3)
                    original_roots = tuple(scene.mobjects)
                    other = copier(root)
                    for old, new in zip(root.get_family(), other.get_family()):
                        for name, value in vars(old).items():
                            if isinstance(value, np.ndarray):
                                saved = getattr(new, name)
                                self.assertFalse(np.shares_memory(value, saved), name)
                                np.testing.assert_array_equal(saved, value)
                    other[0].controls[0] = 99
                    self.assertEqual(root[0].controls[0], 0.)
                    root[1].controls[1] = -5
                    self.assertEqual(other[1].controls[1], 1.)
                    self.assertEqual(tuple(scene.mobjects), original_roots)

    def test_animate_target_cannot_edit_source_or_saved_state(self):
        for bound in (False, True):
            with self.subTest(bound=bound):
                scene, root = self.root(bound)
                obj = root[0]
                obj.save_state()
                builder = obj.animate.set_control(8.)
                self.assertEqual(obj.controls[0], 0.)
                self.assertEqual(obj.saved_state.controls[0], 0.)
                self.assertEqual(obj.target.controls[0], 8.)
                self.assertAlmostEqual(obj.get_x(), 0.)
                self.assertIs(builder.mobject, obj)

    def test_array_aliases_are_preserved_across_members_not_with_source(self):
        _, root = self.root(True)
        shared = root[0].controls
        root.shared = root[1].shared = shared
        other = root.copy()
        self.assertIs(other.shared, other[0].controls)
        self.assertIs(other.shared, other[1].shared)
        self.assertIsNot(other.shared, shared)

    def test_object_array_remaps_family_members_but_not_external_objects(self):
        _, root = self.root(True)
        outsider, payload = m.Circle(), {"shared": True}
        root.lookup = np.empty(4, dtype=object)
        root.lookup[:] = [root, root[0], outsider, payload]
        other = root.copy()
        self.assertIs(other.lookup[0], other)
        self.assertIs(other.lookup[1], other[0])
        self.assertIs(other.lookup[2], outsider)
        self.assertIs(other.lookup[3], payload)
        other.lookup[1] = outsider
        self.assertIs(root.lookup[1], root[0])

    def test_structured_object_subarrays_use_the_same_family_map(self):
        _, root = self.root(False)
        root.lookup = np.zeros(2, dtype=[("points", "f8", 3), ("objects", "O", 2)])
        root.lookup["points"] = [[1, 2, 3], [4, 5, 6]]
        root.lookup["objects"][0, 0] = root
        root.lookup["objects"][0, 1] = root[0]
        root.lookup["objects"][1, 0] = root[1]
        root.lookup["objects"][1, 1] = root[0].controls
        other = root.copy()
        np.testing.assert_array_equal(other.lookup["points"], root.lookup["points"])
        self.assertIs(other.lookup["objects"][0, 0], other)
        self.assertIs(other.lookup["objects"][0, 1], other[0])
        self.assertIs(other.lookup["objects"][1, 0], other[1])
        self.assertIs(other.lookup["objects"][1, 1], other[0].controls)

    def test_array_cycles_and_zero_dimensional_arrays_remain_coherent(self):
        _, root = self.root(False)
        root.lookup = np.empty((), dtype=object)
        nested = np.empty(2, dtype=object)
        root.lookup[()] = nested
        nested[0], nested[1] = root.lookup, root[0]
        other = root.copy()
        self.assertIs(other.lookup[()][0], other.lookup)
        self.assertIs(other.lookup[()][1], other[0])

    def test_noncontiguous_and_readonly_arrays_own_their_storage(self):
        _, root = self.root(True)
        root.values = np.arange(40, dtype=np.int16).reshape(5, 8)[::-1, ::2]
        root.values.setflags(write=False)
        other = root.copy()
        self.assertEqual(root.values.dtype, other.values.dtype)
        self.assertEqual(root.values.shape, other.values.shape)
        self.assertFalse(np.shares_memory(root.values, other.values))
        np.testing.assert_array_equal(root.values, other.values)
        # Like ndarray.copy, the materialized array is writable.
        self.assertTrue(other.values.flags.writeable)

    def test_array_bits_are_not_normalized(self):
        _, root = self.root(False)
        root.values = np.array([0, 0x8000000000000000,
                                0x7ff8000000000001, 0x7ff8000000000002], dtype="u8").view("f8")
        other = root.copy()
        self.assertEqual(root.values.tobytes(), other.values.tobytes())
        self.assertIsNot(root.values, other.values)

    def test_shallow_python_data_updaters_and_link_rules_remain_unchanged(self):
        _, root = self.root(True)
        root.named_child = root[0]
        root.payload = [root[0], {"value": 1}]
        root.uniforms["application_state"] = {"values": [1]}
        callback = lambda obj, dt: None
        root.add_updater(callback, call=False)
        root.target = m.Circle()
        root.saved_state = m.Circle()
        other = root.copy()
        self.assertIs(other.named_child, other[0])
        self.assertIs(other.payload, root.payload)
        self.assertIs(other.uniforms["application_state"], root.uniforms["application_state"])
        self.assertIsNot(other.updaters, root.updaters)
        self.assertIs(other.updaters[0], callback)
        self.assertIsNone(other.target)
        self.assertIsNone(other.saved_state)

    def test_deepcopy_still_uses_one_memo_for_containers_arrays_and_family(self):
        _, root = self.root(True)
        root.payload = [root[0], root[0].controls]
        other = copy.deepcopy(root)
        self.assertIs(other.payload[0], other[0])
        self.assertIs(other.payload[1], other[0].controls)
        self.assertIsNot(other.payload, root.payload)
        self.assertIsNot(other.payload[1], root.payload[1])

    def test_existing_memo_identity_is_not_overwritten(self):
        native = getattr(m, "_native", m)
        _, root = self.root(False)
        previous = root.copy()
        previous[0].controls[:] = 99
        self.assertIs(native._copy_mobject_graph(root, False, {id(root): previous}), previous)
        np.testing.assert_array_equal(previous[0].controls, [99, 99, 99])

    def test_writable_native_view_attribute_becomes_an_owned_array(self):
        _, root = self.root(True)
        root.view = root[0].get_points()
        before = root.view.copy()
        other = root.copy()
        self.assertFalse(np.shares_memory(root.view, other.view))
        other.view[:] = 99
        np.testing.assert_array_equal(root[0].get_points(), before)
        np.testing.assert_array_equal(other[0].get_points(), before)

    def test_shared_child_family_still_has_one_copied_identity(self):
        child = DataSquare()
        root = m.Group(m.Group(child), m.Group(child))
        scene = m.Scene(); scene.add(root)
        other = root.copy()
        self.assertIs(other[0][0], other[1][0])
        self.assertIsNot(other[0][0], child)
        self.assertIsNot(other[0][0].controls, child.controls)

    def test_deep_array_chains_do_not_use_python_recursion(self):
        _, root = self.root(False)
        nested = np.empty(1, dtype=object)
        nested[0] = root[0]
        for _ in range(1500):
            parent = np.empty(1, dtype=object)
            parent[0] = nested
            nested = parent
        root.lookup = nested
        other = root.copy()
        left, right = root.lookup, other.lookup
        for _ in range(1501):
            self.assertIsNot(left, right)
            left, right = left[0], right[0]
        self.assertIs(left, root[0])
        self.assertIs(right, other[0])

    def test_reinstall_preserves_native_and_public_callable_identities(self):
        native = getattr(m, "_native", m)
        before = (native._copy_mobject_graph, m.Mobject.copy, m.Mobject.__copy__, m.Mobject.__deepcopy__)
        install_mobject_copying(native)
        after = (native._copy_mobject_graph, m.Mobject.copy, m.Mobject.__copy__, m.Mobject.__deepcopy__)
        self.assertTrue(all(a is b for a, b in zip(before, after)))


suite = unittest.defaultTestLoader.loadTestsFromTestCase(NativeCopyingTests)
assert suite.countTestCases() == 15
result = unittest.TextTestRunner(verbosity=2).run(suite)
if not result.wasSuccessful():
    raise AssertionError("native mobject array-copy acceptance failed")
