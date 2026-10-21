"""Installed-wheel acceptance for process-isolated camera/audio bundle export.

Run with FMN_TEST_NATIVE=1 so missing native tooling is an error, not a skip.
No fixture renderer or alternate bundle writer is used in these tests.
"""
import hashlib
from pathlib import Path
import struct
import tempfile
import unittest
import wave

from test_parallel_batch_native import native_wheel_available


@unittest.skipUnless(native_wheel_available(), "requires the built FrankenManim native wheel")
class NativeParallelBundleTests(unittest.TestCase):
    def test_camera_audio_checkpoint_resume_and_worker_count_replay(self):
        from fmn_python import render_source_scenes
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            with wave.open(str(root / "tone.wav"), "wb") as stream:
                stream.setnchannels(1)
                stream.setsampwidth(2)
                stream.setframerate(8000)
                stream.writeframes(struct.pack("<2000h", *([8192] * 2000)))
            source = root / "scenes.py"
            source.write_text('''from pathlib import Path
from manimlib import Scene, Square, RIGHT
root = Path(__file__).parent
constructed = 0

def capture(scene, name):
    global constructed
    constructed += 1
    assert constructed == 1, "scene jobs shared an interpreter"
    with (root / (name + ".attempts")).open("a") as log:
        log.write("constructed\\n")
    if name == "Retry" and not (root / "allow-retry").exists():
        raise RuntimeError("transient capture failure")
    scene.add(Square(side_length=0.5, fill_opacity=1))
    scene.add_sound(str(root / "tone.wav"))
    scene.wait(0.25)
    scene.camera.frame.shift(RIGHT)
    scene.wait(0.25)

class First(Scene):
    def construct(self): capture(self, "First")

class Retry(Scene):
    def construct(self): capture(self, "Retry")
''', encoding="utf-8")
            settings = dict(format="fmtl", resolution=(64, 36), fps=8,
                            bundle_camera=True, bundle_audio=True,
                            bundle_audio_options={"audio_sample_rate": 8000, "audio_channels": 1})
            recovery = dict(checkpoint=root / "progress.json", resume_key="scene-assets-v1",
                            continue_on_error=True)
            first = render_source_scenes(source, root / "out", ["First", "Retry"], workers=2,
                                          **settings, **recovery)
            self.assertEqual([row.status for row in first.outcomes], ["succeeded", "failed"])
            preserved = first.outcomes[0].destination.read_bytes()
            stamp = first.outcomes[0].destination.stat().st_mtime_ns
            (root / "allow-retry").write_text("ready")
            result = render_source_scenes(source, root / "out", ["First", "Retry"], workers=1,
                                           resume=True, **settings, **recovery)
            self.assertTrue(result.ok)
            self.assertEqual(result.reused, ("First",))
            self.assertEqual(first.outcomes[0].destination.read_bytes(), preserved)
            self.assertEqual(first.outcomes[0].destination.stat().st_mtime_ns, stamp)
            self.assertEqual([(root / (name + ".attempts")).read_text().count("constructed")
                              for name in ("First", "Retry")], [1, 2])
            for row in result.outcomes:
                data = row.destination.read_bytes()
                self.assertEqual(data[:4], b"FMTL")
                self.assertEqual(row.result.bytes, len(data))
                self.assertEqual(row.result.digest, hashlib.sha256(data).hexdigest())
                self.assertTrue(row.result.camera_track)
                self.assertEqual(row.result.frame_count, 4)
                self.assertEqual(row.result.segment_count, 2)
                self.assertEqual(row.result.soundtrack.sample_rate, 8000)
                self.assertEqual(row.result.soundtrack.channels, 1)
                self.assertGreaterEqual(row.result.soundtrack.sample_frames, 2000)
                self.assertFalse(row.result.as_dict()["certified_source"])
            again = render_source_scenes(source, root / "again", ["First", "Retry"], workers=2,
                                          **settings)
            for left, right in zip(result.outcomes, again.outcomes):
                self.assertEqual(left.destination.read_bytes(), right.destination.read_bytes())
            bad = result.outcomes[0].destination
            bad.write_bytes(preserved + b"tampered")
            with self.assertRaisesRegex(ValueError, "modified"):
                render_source_scenes(source, root / "out", ["First", "Retry"], workers=2,
                                     resume=True, **settings, **recovery)


if __name__ == "__main__":
    unittest.main()
