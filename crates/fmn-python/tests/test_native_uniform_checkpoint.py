"""Native SceneState acceptance; FMN_TEST_NATIVE=1 requires a matching wheel."""
import unittest
from unittest.mock import patch

import numpy as np
from test_parallel_batch_native import native_wheel_available


@unittest.skipUnless(native_wheel_available(), "requires the built FrankenManim native wheel")
class NativeUniformCheckpoint(unittest.TestCase):
    def test_native_restore_preserves_owned_uniform_data_and_aliases(self):
        import manimlib as m
        scene = m.Scene()
        left, right = m.Square(), m.Square().shift(m.RIGHT)
        scene.add(left, right)
        payload = {"samples": np.array([1., 2., 3.])}
        values = np.empty(1, dtype=object)
        values[0] = payload
        left.payload = payload
        left.uniforms._extras["checkpoint_values"] = values
        right.uniforms._extras["checkpoint_values"] = values
        scene.frame.uniforms._extras["checkpoint_values"] = values
        record = np.zeros(1, dtype=[("counter", "i8")])
        left.record = record[0]
        expected = left.get_points().copy()
        saved = scene.get_state()
        for counter in (8, 12, 16):
            left.uniforms._extras["checkpoint_values"][0]["samples"][:] = counter
            record["counter"] = counter
            left.shift(m.UP)
            scene.restore_state(saved)
            np.testing.assert_array_equal(left.get_points(), expected)
            np.testing.assert_array_equal(left.payload["samples"], [1, 2, 3])
            self.assertIs(left.payload, left.uniforms._extras["checkpoint_values"][0])
            self.assertIs(left.uniforms._extras["checkpoint_values"], right.uniforms._extras["checkpoint_values"])
            self.assertIs(left.uniforms._extras["checkpoint_values"], scene.frame.uniforms._extras["checkpoint_values"])
            self.assertEqual(left.record["counter"], 0)
            self.assertEqual(record["counter"][0], counter)

    def test_native_history_tracks_bits_and_preserves_state_on_projection_refusal(self):
        import manimlib as m
        from fmn_python import scene_attributes
        scene = m.Scene()
        square = m.Square()
        scene.add(square)
        square.uniforms._extras["checkpoint_zero"] = np.array([0.])
        square.uniforms._extras["checkpoint_nan"] = np.array([0x7ff8000000000001], dtype="u8").view("f8")
        scene.save_state()
        size = len(scene.undo_stack)
        scene.save_state()
        self.assertEqual(len(scene.undo_stack), size)
        saved = scene.undo_stack[-1]
        square.uniforms._extras["checkpoint_zero"][0] = -0.
        self.assertGreater(saved.n_changes(saved), 0)
        square.shift(m.UP)
        moved = square.get_points().copy()
        with patch.object(scene_attributes, "_MAX_BYTES", 1):
            with self.assertRaisesRegex(ValueError, "budget"):
                scene.restore_state(saved)
        np.testing.assert_array_equal(square.get_points(), moved)
        self.assertTrue(np.signbit(square.uniforms._extras["checkpoint_zero"][0]))
        scene.restore_state(saved)
        self.assertFalse(np.signbit(square.uniforms._extras["checkpoint_zero"][0]))
        self.assertEqual(square.uniforms._extras["checkpoint_nan"].view("u8")[0], 0x7ff8000000000001)


if __name__ == "__main__":
    unittest.main()
