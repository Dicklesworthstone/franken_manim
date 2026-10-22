"""Owned NumPy state and complete SceneState orchestration.

The structural tests execute production code with real NumPy. The SceneState
cases reuse the existing explicit native-storage fixture; they do not establish
native rendering, RNG, checkpoint-byte compatibility or installed-wheel parity.
"""
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import numpy as np
from fmn_python import scene_attributes as data
from fmn_python import scene_state as state
from test_scene_state_protocol import environment, Mob, Uniforms


def objects(*values):
    array = np.empty(len(values), dtype=object)
    for index, value in enumerate(values):
        array[index] = value
    return array


class Opaque:
    def __eq__(self, other):
        raise AssertionError("authored equality must not execute")

    def __deepcopy__(self, memo):
        raise AssertionError("opaque resources retain identity")


class UniformDataOwnership(unittest.TestCase):
    def test_numeric_record_views_detach_and_restore_repeatedly(self):
        array = np.array([(3., [4, 5])], dtype=[("scale", "f8"), ("xy", "i8", 2)])
        record = array[0]
        member = SimpleNamespace(record=record, again=record)
        snapshot = data.capture([member], np)
        array["scale"] = 99
        array["xy"] = -1
        self.assertEqual(snapshot[id(member)]["record"]["scale"], 3.)
        for _ in range(3):
            data.restore([member], data.prepare_restore(snapshot, np, [member]))
            self.assertIs(member.record, member.again)
            self.assertFalse(np.shares_memory(np.asarray(member.record), array))
            np.testing.assert_array_equal(member.record["xy"], [4, 5])
            member.record["xy"][0] = 88

    def test_self_referential_structured_scalar_is_registered_before_slots(self):
        record = np.zeros(1, dtype=[("count", "i4"), ("link", object)])[0]
        record["count"], record["link"] = 7, record
        copied = data.capture([SimpleNamespace(record=record)], np)
        saved = next(iter(copied.values()))["record"]
        self.assertIsNot(saved, record)
        self.assertIs(saved["link"], saved)
        self.assertTrue(state._same_value(np, record, saved))
        record["count"] = 12
        self.assertEqual(saved["count"], 7)
        self.assertFalse(state._same_value(np, record, saved))

    def test_record_array_container_cycle_keeps_aliases_and_opaque_identity(self):
        record = np.zeros(1, dtype=[("link", object), ("value", "i8")])[0]
        opaque, items = Opaque(), []
        array = objects(record, items, opaque)
        record["link"] = array
        items.extend([record, array])
        saved = state._freeze_value(np, {"record": record, "array": array})
        self.assertIs(saved["record"]["link"], saved["array"])
        self.assertIs(saved["array"][0], saved["record"])
        self.assertIs(saved["array"][1][1], saved["array"])
        self.assertIs(saved["array"][2], opaque)
        self.assertTrue(state._same_value(np, saved, {"record": record, "array": array}))

    def test_readonly_records_and_raw_void_payloads_keep_bytes(self):
        array = np.zeros(1, dtype=[("value", "u8")])
        array["value"] = 0xffffffffffffffff
        array.flags.writeable = False
        record = array[0]
        copied = state._freeze_value(np, record)
        self.assertFalse(copied.flags.writeable)
        self.assertIsNot(copied, record)
        self.assertFalse(np.shares_memory(np.asarray(record), np.asarray(copied)))
        self.assertTrue(state._same_value(np, record, copied))
        raw = np.void(b"\x00\x80\xff\x01")
        self.assertEqual(state._freeze_value(np, raw).tobytes(), raw.tobytes())

    def test_structured_scalar_counts_toward_buffer_budget(self):
        record = np.zeros(1, dtype=[("samples", "f8", 16)])[0]
        with patch.object(data, "_MAX_BYTES", record.nbytes - 1):
            with self.assertRaisesRegex(ValueError, "budget"):
                data.capture([SimpleNamespace(record=record)], np)

    def test_object_uniforms_copy_nested_values_and_keep_repeated_arrays(self):
        inner, raw = [1, bytearray(b"ab")], np.arange(6).reshape(2, 3)
        values = {"objects": objects(inner, inner, raw), "raw": raw}
        copied = state._freeze_value(np, values)
        inner[1][:] = b"zz"
        raw[:] = 99
        self.assertEqual(copied["objects"][0], [1, bytearray(b"ab")])
        self.assertIs(copied["objects"][0], copied["objects"][1])
        self.assertIs(copied["objects"][2], copied["raw"])
        np.testing.assert_array_equal(copied["raw"], np.arange(6).reshape(2, 3))

    def test_ndarray_subclass_copy_and_equality_hooks_do_not_run(self):
        class Hostile(np.ndarray):
            def copy(self, *args, **kwargs):
                raise AssertionError("authored array copy must not execute")
            def __deepcopy__(self, memo):
                raise AssertionError("authored deepcopy must not execute")
            def __eq__(self, other):
                raise AssertionError("authored array equality must not execute")
        array = np.arange(8)[::2].view(Hostile)
        array.flags.writeable = False
        copied = state._freeze_value(np, array)
        self.assertIs(type(copied), np.ndarray)
        self.assertFalse(copied.flags.writeable)
        np.testing.assert_array_equal(copied, [0, 2, 4, 6])

    def test_object_array_comparison_uses_resource_identity_not_eq(self):
        opaque = Opaque()
        self.assertTrue(state._same_value(np, objects(opaque), objects(opaque)))
        self.assertFalse(state._same_value(np, objects(opaque), objects(Opaque())))

    def test_uniform_comparison_preserves_signed_zero_nan_bits_and_aliases(self):
        nan = np.array([0x7ff8000000000001, 0x7ff8000000000002], dtype="u8").view("f8")
        self.assertTrue(state._same_value(np, nan, nan.copy()))
        self.assertFalse(state._same_value(np, nan[:1], nan[1:]))
        for a, b in ((0., -0.), (np.array([0.]), np.array([-0.])),
                     (True, 1), (np.complex128(0. + 1j), np.complex128(complex(-0., 1.)))):
            self.assertFalse(state._same_value(np, a, b))
        shared = [1]
        self.assertFalse(state._same_value(np, {"a": shared, "b": shared}, {"a": [1], "b": [1]}))
        readonly = np.arange(4); readonly.flags.writeable = False
        self.assertFalse(state._same_value(np, readonly, readonly.copy()))

    def test_projection_shares_one_alias_memo_and_aggregate_buffer_budget(self):
        raw = np.arange(4, dtype="i8")
        a = SimpleNamespace(value=raw, uniforms=SimpleNamespace(_extras={"x": raw}))
        b = SimpleNamespace(uniforms=SimpleNamespace(_extras={"y": raw}))
        with patch.object(data, "_MAX_BYTES", raw.nbytes):
            attrs, uniforms = data.capture_projection([a, b], np)
            self.assertIs(attrs[id(a)]["value"], uniforms[id(a)]["x"])
            self.assertIs(uniforms[id(a)]["x"], uniforms[id(b)]["y"])
            b.uniforms._extras["y"] = raw.copy()
            with self.assertRaisesRegex(ValueError, "budget"):
                data.capture_projection([a, b], np)

    def test_uniform_depth_and_invalid_projection_refuse_before_restoration(self):
        member = SimpleNamespace(uniforms=SimpleNamespace(_extras={"x": [[[[1]]]]}))
        with patch.object(data, "_MAX_DEPTH", 4):
            with self.assertRaisesRegex(ValueError, "budget"):
                data.capture_projection([member], np)
        attrs, uniforms = data.capture_projection([member], np)
        for invalid in ({}, {id(member): []}, {0: {}}):
            with self.assertRaisesRegex(ValueError, "captured family"):
                data.prepare_projection(attrs, invalid, np, [member])
        a, u = data.prepare_projection(attrs, uniforms, np, [member])
        self.assertTrue(data.same(np, (a, u), (attrs, uniforms)))


class UniformSceneState(unittest.TestCase):
    def setUp(self):
        self.native = environment()
        self.left, self.right = Mob(x=1.), Mob(x=2.)
        self.scene = self.native.Scene(self.left, self.right)

    def snapshot(self, **options):
        return self.native.SceneState(self.scene, **options)

    def test_nested_object_uniforms_restore_across_repeated_live_mutations(self):
        payload = [1, {"samples": np.array([2., 3.])}]
        self.left.uniforms._extras = {"objects": objects(payload)}
        saved = self.snapshot()
        for _ in range(3):
            self.left.uniforms._extras["objects"][0][1]["samples"][:] = 99
            self.left.data["point"][:] = 8
            saved.restore_scene(self.scene)
            np.testing.assert_array_equal(self.left.uniforms._extras["objects"][0][1]["samples"], [2., 3.])
            np.testing.assert_array_equal(self.left.data["point"], [[1., 0., 0.]])
        self.assertIsNot(self.left.uniforms._extras["objects"][0], payload)

    def test_cross_root_attribute_uniform_and_mapping_aliases_roundtrip(self):
        payload = [bytearray(b"original")]
        self.left.value = self.right.value = payload
        self.left.uniforms._extras = {"payload": payload}
        self.right.uniforms._extras = self.left.uniforms._extras
        self.left.uniform_mapping = self.left.uniforms._extras
        saved = self.snapshot()
        payload[0][:] = b"changed!"
        saved.restore_scene(self.scene)
        self.assertEqual(self.left.value, [bytearray(b"original")])
        self.assertIs(self.left.value, self.right.value)
        self.assertIs(self.left.value, self.left.uniforms._extras["payload"])
        self.assertIs(self.left.uniforms._extras, self.right.uniforms._extras)
        self.assertIs(self.left.uniform_mapping, self.right.uniforms._extras)

    def test_alias_only_uniform_edit_changes_history_and_live_change_count(self):
        shared = [1]
        self.left.uniforms._extras = {"payload": shared}
        self.right.uniforms._extras = {"payload": shared}
        before = self.snapshot()
        self.scene.undo_stack.append(before)
        self.right.uniforms._extras["payload"] = [1]
        after = self.snapshot()
        self.assertFalse(before.mobjects_match(after))
        self.assertGreater(before.n_changes(after), 0)
        self.assertGreater(before.n_changes(before), 0)
        before.restore_scene(self.scene)
        self.assertIs(self.left.uniforms._extras["payload"], self.right.uniforms._extras["payload"])

    def test_shared_opaque_payload_survives_uniform_copy_and_comparison(self):
        opaque = Opaque()
        # The existing native-storage fixture deepcopies before making extras
        # shallow. Suppress only that fixture's copy, not production capture.
        original = Uniforms.__deepcopy__ if hasattr(Uniforms, "__deepcopy__") else None
        def shallow(uniform, memo):
            result = Uniforms()
            dict.clear(result); dict.update(result, dict(dict.items(uniform)))
            result._extras = dict(uniform._extras)
            return result
        self.assertIsNone(original)
        self.left.uniforms._extras = {"resource": objects(opaque)}
        with patch.object(Uniforms, "__deepcopy__", shallow, create=True):
            before, after = self.snapshot(), self.snapshot()
            self.assertTrue(before.mobjects_match(after))
            before.restore_scene(self.scene)
        self.assertIs(self.left.uniforms._extras["resource"][0], opaque)

    def test_nonroot_camera_uniforms_restore_and_register_in_change_detection(self):
        self.scene.frame.uniforms = Uniforms()
        values = [1]
        self.scene.frame.uniforms._extras = {"values": values}
        self.left.values = values
        before = self.snapshot()
        values[0] = 6
        self.assertFalse(before.mobjects_match(self.snapshot()))
        self.assertGreater(before.n_changes(before), 0)
        before.restore_scene(self.scene)
        self.assertEqual(self.scene.frame.uniforms._extras["values"], [1])
        self.assertIs(self.scene.frame.uniforms._extras["values"], self.left.values)

    def test_ignored_camera_uniforms_stay_live(self):
        self.scene.frame.uniforms = Uniforms()
        before = self.snapshot(ignore=[self.scene.frame])
        self.scene.frame.uniforms._extras["gain"][:] = 17
        before.restore_scene(self.scene)
        np.testing.assert_array_equal(self.scene.frame.uniforms._extras["gain"], [17, 17])

    def test_uniform_budget_refusal_does_not_mutate_native_state_or_history(self):
        self.left.uniforms._extras = {"payload": np.arange(20)}
        self.scene.save_state()
        saved = self.scene.undo_stack[-1]
        self.left.data["point"][:] = 17
        self.scene._time = 90
        live_uniforms = self.left.uniforms._extras
        with patch.object(data, "_MAX_BYTES", 16):
            with self.assertRaisesRegex(ValueError, "budget"):
                saved.restore_scene(self.scene)
        self.assertEqual(self.scene.restore_calls, 0)
        self.assertEqual(self.scene.get_time(), 90)
        np.testing.assert_array_equal(self.left.data["point"], [[17, 17, 17]])
        self.assertIs(self.left.uniforms._extras, live_uniforms)
        self.assertIs(self.scene.undo_stack[-1], saved)

    def test_tampered_uniform_projection_fails_before_native_restore(self):
        before = self.snapshot()
        before._uniforms[id(self.left)] = []
        self.left.data["point"][:] = 17
        with self.assertRaisesRegex(ValueError, "uniform projection"):
            before.restore_scene(self.scene)
        self.assertEqual(self.scene.restore_calls, 0)
        np.testing.assert_array_equal(self.left.data["point"], [[17, 17, 17]])

    def test_native_refusal_preserves_live_aliases_and_undo_stack(self):
        shared = [1]
        self.left.value = shared
        self.left.uniforms._extras = {"value": shared}
        self.scene.save_state()
        shared[0] = 2
        self.scene.refuse_restore = True
        with self.assertRaisesRegex(RuntimeError, "backend rejected"):
            self.scene.undo()
        self.assertIs(self.left.value, shared)
        self.assertIs(self.left.uniforms._extras["value"], shared)
        self.assertEqual(self.left.value, [2])
        self.assertEqual(len(self.scene.undo_stack), 1)
        self.assertEqual(self.scene.redo_stack, [])

    def test_undo_redo_roundtrips_object_uniform_values_and_cross_projection_aliases(self):
        payload = {"items": [1]}
        self.left.payload = payload
        self.left.uniforms._extras = {"payload": objects(payload)}
        self.scene.save_state()
        payload["items"].append(2)
        self.scene.undo()
        self.assertEqual(self.left.payload["items"], [1])
        self.assertIs(self.left.payload, self.left.uniforms._extras["payload"][0])
        self.scene.redo()
        self.assertEqual(self.left.payload["items"], [1, 2])
        self.assertIs(self.left.payload, self.left.uniforms._extras["payload"][0])
        self.scene.undo()
        self.assertEqual(self.left.payload["items"], [1])

    def test_signed_zero_and_nan_values_drive_history_without_losing_noop_reuse(self):
        nan = np.array([0x7ff8000000000001], dtype="u8").view("f8")
        self.left.uniforms._extras = {"zero": np.array([0.]), "nan": nan}
        self.scene.save_state()
        self.scene.save_state()
        self.assertEqual(len(self.scene.undo_stack), 1)
        self.left.uniforms._extras["zero"][0] = -0.
        self.scene.save_state()
        self.assertEqual(len(self.scene.undo_stack), 2)
        self.assertGreater(self.scene.undo_stack[0].n_changes(self.scene.undo_stack[1]), 0)
        self.scene.undo_stack[0].restore_scene(self.scene)
        self.assertFalse(np.signbit(self.left.uniforms._extras["zero"][0]))
        self.assertEqual(self.left.uniforms._extras["nan"].view("u8")[0], 0x7ff8000000000001)

    def test_unchanged_array_subclass_does_not_invalidate_saved_mirror(self):
        class Array(np.ndarray):
            pass
        self.left.uniforms._extras = {"values": np.arange(4).view(Array)}
        first = self.snapshot()
        self.scene.undo_stack.append(first)
        second = self.snapshot()
        self.assertIs(first.mobjects_to_copies[self.left], second.mobjects_to_copies[self.left])
        self.assertEqual(first.n_changes(first), 0)
        self.assertTrue(first.mobjects_match(second))

    def test_numeric_record_attribute_does_not_follow_its_original_storage(self):
        array = np.zeros(1, dtype=[("counter", "i8")])
        self.left.record = array[0]
        before = self.snapshot()
        array["counter"] = 18
        self.assertGreater(before.n_changes(before), 0)
        before.restore_scene(self.scene)
        self.assertEqual(self.left.record["counter"], 0)
        self.assertEqual(array["counter"][0], 18)


if __name__ == "__main__":
    unittest.main()
