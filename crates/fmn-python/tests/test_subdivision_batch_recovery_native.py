"""Installed-wheel acceptance for native subdivided batch recovery.

Run against a wheel built from the candidate source, never protocol byte
fixtures. These cases require native rendering and the installed CLI.
"""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from test_parallel_batch_native import native_wheel_available

SOURCE = '''from pathlib import Path
from manimlib import Scene, Square, RIGHT
class First(Scene):
    def __init__(self, **kwargs):
        root = Path(__file__).parent
        with (root / (type(self).__name__ + ".calls")).open("a") as stream:
            stream.write("construct\\n")
        if type(self).__name__ == "Second" and (root / "fail").exists():
            raise RuntimeError("transient constructor failure before output ownership")
        super().__init__(**kwargs)
    def construct(self):
        square = Square()
        self.add(square)
        self.wait(0.125)
        self.play(square.animate.shift(RIGHT), run_time=0.25)
        self.wait(0.125)
class Second(First): pass
'''


@unittest.skipUnless(native_wheel_available(), "requires a matching installed native wheel")
class NativeSubdivisionRecoveryTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="fmn-native-collection-recovery-"))
        self.source = self.root / "scenes.py"
        self.source.write_text(SOURCE)
        self.output = self.root / "output"
        self.checkpoint = self.root / "progress.json"

    def command(self, *extra):
        environment = dict(os.environ)
        environment.pop("PYTHONPATH", None)
        return subprocess.run([sys.executable, "-m", "fmn_python", str(self.source),
            "First", "Second", "--workers", "2", "--subdivide", "--save-last-frame",
            "--format", "gif", "--resolution", "64x48", "--fps", "8", "--robot",
            "--video_dir", str(self.output), "--checkpoint", str(self.checkpoint),
            "--resume-key", "native-inputs-v1", "--keep-going", *extra],
            cwd=self.root, env=environment, capture_output=True, text=True, timeout=120)

    def test_cli_retry_hashes_every_native_clip_and_does_not_reconstruct_completed_scene(self):
        (self.root / "fail").touch()
        first = self.command()
        self.assertEqual(first.returncode, 5, first.stderr + first.stdout)
        report = json.loads(first.stdout)["batch"]
        self.assertEqual([row["status"] for row in report["outcomes"]], ["succeeded", "failed"])
        pair = report["outcomes"][0]["result"]
        self.assertEqual([clip["play_index"] for clip in pair["primary"]["segments"]], [0, 1, 2])
        samples = [clip["render"] for clip in pair["primary"]["segments"]] + [pair["still"]]
        saved = {}
        for item in samples:
            path = Path(item["destination"])
            raw = path.read_bytes()
            self.assertEqual(hashlib.sha256(raw).hexdigest(), item["digest"])
            self.assertEqual(len(raw), item["bytes"])
            self.assertTrue(raw.startswith(b"GIF") if item["format"] == "gif"
                            else raw.startswith(b"\x89PNG\r\n\x1a\n"))
            saved[path] = (raw, path.stat().st_mtime_ns)
        (self.root / "fail").rename(self.root / "repaired")
        second = self.command("--resume")
        self.assertEqual(second.returncode, 0, second.stderr + second.stdout)
        self.assertEqual(json.loads(second.stdout)["batch"]["reused"], ["First"])
        self.assertEqual((self.root / "First.calls").read_text().splitlines(), ["construct"])
        self.assertEqual(len((self.root / "Second.calls").read_text().splitlines()), 2)
        for path, (raw, modified) in saved.items():
            self.assertEqual((path.read_bytes(), path.stat().st_mtime_ns), (raw, modified))
        Path(samples[1]["destination"]).write_bytes(b"corrupted saved native clip")
        refused = self.command("--resume")
        self.assertNotEqual(refused.returncode, 0, refused.stdout)
        self.assertEqual(len((self.root / "Second.calls").read_text().splitlines()), 2)

    def test_native_png_collections_and_still_resume_without_opening_workers(self):
        from unittest.mock import patch
        from fmn_python.parallel_batch import render_source_scenes
        options = dict(workers=2, format="png_sequence", subdivide=True,
                       save_last_frame=True, resolution=(64, 48), fps=8, threads=1,
                       animation_range=(1, 3), checkpoint=self.checkpoint, resume_key="native-inputs-v1")
        first = render_source_scenes(self.source, self.output, ["First", "Second"], **options)
        self.assertTrue(first.ok)
        for outcome in first.outcomes:
            self.assertEqual([s.play_index for s in outcome.result.primary.segments], [1, 2])
            for segment in outcome.result.primary.segments:
                root = segment.render.destination
                self.assertTrue((root / "FMN_COMPLETE").is_file())
                frames = sorted(root.glob("*.png"))
                self.assertEqual(len(frames), segment.render.frame_count)
                self.assertTrue(all(p.read_bytes().startswith(b"\x89PNG\r\n\x1a\n") for p in frames))
        with patch("fmn_python.parallel_batch.run_process_jobs", side_effect=AssertionError("no retry workers")):
            resumed = render_source_scenes(self.source, self.output, ["First", "Second"], resume=True, **options)
        self.assertEqual(resumed.reused, ("First", "Second"))
        self.assertEqual([row.result.as_dict() for row in first.outcomes],
                         [row.result.as_dict() for row in resumed.outcomes])


if __name__ == "__main__":
    unittest.main(verbosity=2)
