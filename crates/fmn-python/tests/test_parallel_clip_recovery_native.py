"""Installed-wheel acceptance for parallel per-scene clip-prefix recovery."""
import hashlib
import json
import subprocess
import sys
from pathlib import Path
import tempfile
import unittest

from test_parallel_batch_native import native_wheel_available

SOURCE = '''from pathlib import Path
from manimlib import Scene, Square, RIGHT
class Lesson(Scene):
    def construct(self):
        root = Path(__file__).parent
        with (root / (type(self).__name__ + ".calls")).open("a") as stream:
            stream.write("construct\\n")
        square = Square()
        self.add(square)
        self.wait(0.125)
        if (root / "fail").exists():
            raise RuntimeError("interrupted after a published clip")
        self.play(square.animate.shift(RIGHT), run_time=0.25)
        self.wait(0.125)
class Other(Lesson): pass
'''


@unittest.skipUnless(native_wheel_available(), "requires a matching built native wheel")
class ParallelClipRecoveryNativeTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="fmn-native-prefix-"))
        self.source = self.root / "scenes.py"
        self.source.write_text(SOURCE)
        self.output, self.state = self.root / "out", self.root / "state"

    def render(self, **options):
        from fmn_python.parallel_batch import render_source_scenes
        return render_source_scenes(self.source, self.output, ["Lesson", "Other"],
            workers=2, format="gif", resolution=(64, 48), fps=8, threads=1,
            subdivide=True, clip_checkpoint_dir=self.state, resume_key="native-v1",
            continue_on_error=True, **options)

    def test_partial_native_collections_resume_without_reencoding_prefix(self):
        flag = self.root / "fail"
        flag.touch()
        first = self.render()
        self.assertEqual([row.status for row in first.outcomes], ["failed", "failed"])
        saved = {}
        for row in first.outcomes:
            self.assertEqual(len(row.result.segments), 1)
            receipt = row.result.segments[0].render
            data = receipt.destination.read_bytes()
            self.assertTrue(data.startswith(b"GIF8"))
            self.assertEqual(hashlib.sha256(data).hexdigest(), receipt.digest)
            saved[row.name] = (receipt.destination.stat().st_mtime_ns, data)
        flag.rename(self.root / "fixed")
        resumed = self.render(resume=True)
        self.assertTrue(resumed.ok)
        for row in resumed.outcomes:
            self.assertEqual(len(row.result.segments), 3)
            first_clip = row.result.segments[0].render.destination
            self.assertEqual((first_clip.stat().st_mtime_ns, first_clip.read_bytes()), saved[row.name])
            self.assertEqual((self.root / (row.name + ".calls")).read_text().splitlines(),
                             ["construct", "construct"])

    def test_native_prefix_tampering_refuses_before_rendering_the_suffix(self):
        (self.root / "fail").touch()
        first = self.render()
        path = first.outcomes[0].result.segments[0].render.destination
        path.write_bytes(b"tampered native clip")
        (self.root / "fail").rename(self.root / "fixed")
        resumed = self.render(resume=True)
        self.assertEqual(resumed.outcomes[0].status, "failed")
        self.assertFalse((self.output / "Lesson/clips/segment-000001.gif").exists())
        self.assertEqual(path.read_bytes(), b"tampered native clip")

    def test_installed_cli_recovers_each_partial_scene(self):
        (self.root / "fail").touch()
        command = [sys.executable, "-m", "fmn_python", str(self.source), "Lesson", "Other",
                   "--workers", "2", "--subdivide", "--format", "gif", "--resolution", "64x48",
                   "--fps", "8", "--video_dir", str(self.output), "--clip-checkpoint-dir", str(self.state),
                   "--resume-key", "native-v1", "--keep-going", "--robot"]
        first = subprocess.run(command, cwd=self.root, capture_output=True, text=True, timeout=90)
        self.assertEqual(first.returncode, 5, first.stderr + first.stdout)
        data = json.loads(first.stdout)["batch"]
        self.assertEqual(data["counts"]["failed"], 2)
        path = self.output / "Lesson/clips/segment-000000.gif"
        saved = path.stat().st_mtime_ns, path.read_bytes()
        (self.root / "fail").rename(self.root / "fixed")
        second = subprocess.run(command + ["--resume"], cwd=self.root,
                                capture_output=True, text=True, timeout=90)
        self.assertEqual(second.returncode, 0, second.stderr + second.stdout)
        self.assertTrue(json.loads(second.stdout)["batch"]["ok"])
        self.assertEqual((path.stat().st_mtime_ns, path.read_bytes()), saved)

    def test_native_paired_prefix_resume_reuses_png_and_clips_without_modification(self):
        (self.root / "fail").touch()
        first = self.render(save_last_frame=True)
        self.assertEqual([row.status for row in first.outcomes], ["failed", "failed"])
        saved = {}
        for row in first.outcomes:
            self.assertFalse(row.result.completed)
            self.assertIsNone(row.result.still)
            clip = row.result.primary.segments[0].render.destination
            self.assertTrue(clip.read_bytes().startswith(b"GIF8"))
            saved[row.name] = (clip.stat().st_mtime_ns, clip.read_bytes())
        (self.root / "fail").rename(self.root / "fixed")
        second = self.render(save_last_frame=True, resume=True)
        self.assertTrue(second.ok, second.as_dict())
        images = {}
        for row in second.outcomes:
            pair = row.result
            self.assertTrue(pair.completed)
            self.assertEqual(len(pair.primary.segments), 3)
            clip = pair.primary.segments[0].render.destination
            self.assertEqual((clip.stat().st_mtime_ns, clip.read_bytes()), saved[row.name])
            path = pair.still_destination
            data = path.read_bytes()
            self.assertTrue(data.startswith(b"\x89PNG\r\n\x1a\n"))
            self.assertEqual(hashlib.sha256(data).hexdigest(), pair.still.digest)
            images[row.name] = path.stat().st_mtime_ns, data
        third = self.render(save_last_frame=True, resume=True)
        self.assertTrue(third.ok, third.as_dict())
        for row in third.outcomes:
            path = row.result.still_destination
            self.assertEqual((path.stat().st_mtime_ns, path.read_bytes()), images[row.name])
        path = third.outcomes[0].result.still_destination
        path.write_bytes(b"modified final PNG")
        fourth = self.render(save_last_frame=True, resume=True)
        self.assertEqual(fourth.outcomes[0].status, "failed")
        self.assertEqual(path.read_bytes(), b"modified final PNG")

    def test_installed_cli_composes_final_image_with_partial_recovery(self):
        (self.root / "fail").touch()
        command = [sys.executable, "-m", "fmn_python", str(self.source), "Lesson", "Other",
            "--workers", "2", "--subdivide", "--save-last-frame", "--format", "gif",
            "--resolution", "64x48", "--fps", "8", "--video_dir", str(self.output),
            "--clip-checkpoint-dir", str(self.state), "--resume-key", "native-paired-v1",
            "--keep-going", "--robot"]
        first = subprocess.run(command, cwd=self.root, capture_output=True, text=True, timeout=90)
        self.assertEqual(first.returncode, 5, first.stderr + first.stdout)
        for row in json.loads(first.stdout)["batch"]["outcomes"]:
            self.assertFalse(row["result"]["completed"])
            self.assertEqual(len(row["result"]["primary"]["segments"]), 1)
        (self.root / "fail").rename(self.root / "fixed")
        second = subprocess.run(command + ["--resume"], cwd=self.root,
                                capture_output=True, text=True, timeout=90)
        self.assertEqual(second.returncode, 0, second.stderr + second.stdout)
        for row in json.loads(second.stdout)["batch"]["outcomes"]:
            self.assertTrue(row["result"]["completed"])
            image = Path(row["result"]["still_destination"]).read_bytes()
            self.assertTrue(image.startswith(b"\x89PNG\r\n\x1a\n"))
            self.assertEqual(hashlib.sha256(image).hexdigest(), row["result"]["still"]["digest"])


if __name__ == "__main__":
    unittest.main()
