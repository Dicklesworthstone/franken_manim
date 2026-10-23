"""Installed native acceptance, including a nonzero portal offload oracle."""
import tempfile
import unittest

from test_parallel_batch_native import native_wheel_available


@unittest.skipUnless(native_wheel_available(), "requires the built FrankenManim native wheel")
class NativePureOffloadTests(unittest.TestCase):
    def test_native_frames_match_serial_and_authored_observers_are_preserved(self):
        from native_pure_offload import run_native_pure_offload_acceptance
        with tempfile.TemporaryDirectory(prefix="fmn-native-pure-offload-") as directory:
            pure, frames, fallbacks = run_native_pure_offload_acceptance(directory)
        self.assertGreater(pure, 0)
        self.assertGreater(frames, pure)
        self.assertEqual(fallbacks, 5)


if __name__ == "__main__":
    unittest.main()
