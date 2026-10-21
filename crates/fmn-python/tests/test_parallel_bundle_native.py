"""Installed-wheel acceptance for process-isolated camera/audio bundle export.

Run with FMN_TEST_NATIVE=1 so missing native tooling is an error, not a skip.
No fixture renderer or alternate bundle writer is used in these tests.
"""
import hashlib
import json
import subprocess
import sys
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


    def test_installed_cli_uses_disposable_discovery_and_resumable_bundle_workers(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            source = root / "scenes.py"
            source.write_text('''import os
from pathlib import Path
from manimlib import Scene, Square
root = Path(__file__).parent
with (root / "imports.log").open("a") as log:
    log.write(str(os.getpid()) + "\\n")

class First(Scene):
    def construct(self):
        self.add(Square())
        self.wait(0.25)

class Second(Scene):
    def construct(self):
        self.add(Square().shift((1, 0, 0)))
        self.wait(0.25)
''', encoding="utf-8")
            argv = [sys.executable, "-m", "fmn_python", "--robot", str(source), "--write_all",
                    "--format", "fmtl", "--workers", "2", "--job-timeout", "90",
                    "--bundle-camera", "--bundle-audio", "--resolution", "64x36", "--fps", "8",
                    "--video_dir", str(root / "out"), "--checkpoint", str(root / "progress.json"),
                    "--resume-key", "source-and-assets-v1"]
            with subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                  text=True, cwd=root) as process:
                try:
                    stdout, stderr = process.communicate(timeout=120)
                except BaseException:
                    process.kill()
                    process.communicate()
                    raise
                self.assertEqual(process.returncode, 0, stderr)
                parent = process.pid
            report = json.loads(stdout)["batch"]
            self.assertTrue(report["ok"])
            self.assertEqual(report["execution"], "spawn")
            self.assertEqual([row["name"] for row in report["outcomes"]], ["First", "Second"])
            imports = [int(pid) for pid in (root / "imports.log").read_text().splitlines()]
            self.assertEqual(len(imports), 3)  # one discovery and two render imports
            self.assertNotIn(parent, imports)
            for row in report["outcomes"]:
                data = Path(row["destination"]).read_bytes()
                self.assertEqual(data[:4], b"FMTL")
                self.assertEqual(row["result"]["sha256"], hashlib.sha256(data).hexdigest())
                self.assertTrue(row["result"]["camera_track"])
                self.assertNotIn("soundtrack", row["result"])  # audio enabled, no cues
            resumed = subprocess.run([*argv, "--resume"], stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                     text=True, cwd=root, timeout=120)
            self.assertEqual(resumed.returncode, 0, resumed.stderr)
            self.assertEqual(json.loads(resumed.stdout)["batch"]["reused"], ["First", "Second"])
            self.assertEqual(len((root / "imports.log").read_text().splitlines()), 4)


if __name__ == "__main__":
    unittest.main()
