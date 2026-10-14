"""Installed CLI certification witnesses using real subprocesses/native outputs.

No parser, renderer, receipt or manifest publisher is substituted. These checks
require the actual wheel inventory and run separately from reduced-table tests.
"""
from __future__ import annotations

import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from test_parallel_certified_native import native_available

_SOURCE = '''import os
from manimlib import Scene, Square, Dot, LEFT, RIGHT
print("authored discovery/render log")
os.write(1, b"authored fd log\\n")
class First(Scene):
    def construct(self):
        self.add(Square().shift(LEFT))
class Second(Scene):
    def construct(self):
        self.add(Dot().shift(RIGHT))
'''


@unittest.skipUnless(native_available(), "requires the installed native FrankenManim wheel")
class CertifiedParallelCliTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="fmn-certified-cli-"))
        self.source = self.root / "scenes.py"
        self.source.write_text(_SOURCE, encoding="utf-8")
        self.number = 0

    def command(self, *selection, workers=2, format="png", extra=(), expected=0):
        self.number += 1
        destination = self.root / f"output-{self.number}"
        args = [sys.executable, "-m", "fmn_python", str(self.source), *selection,
                "--format", format, "--resolution", "64x48", "--fps", "4", "--threads", "1",
                "--video_dir", str(destination), "--reproducible", "--robot"]
        if workers is not None:
            args += ["--workers", str(workers)]
        result = subprocess.run([*args, *extra], cwd=self.root, capture_output=True, text=True, timeout=40)
        self.assertEqual(result.returncode, expected, result.stderr + result.stdout)
        # json.loads rejects trailing output; exactly one receipt is required.
        receipt = json.loads(result.stdout)
        self.assertEqual(receipt["exit"]["code"], expected)
        return receipt, result, destination

    def publications(self, receipt):
        batch = receipt["batch"]
        self.assertTrue(batch["reproducible_requested"])
        self.assertFalse(batch["certified"])
        for row in batch["outcomes"]:
            if row["status"] != "succeeded":
                continue
            result = row["result"]
            self.assertIs(result["certified"], True)
            destination = Path(result["destination"])
            manifest = Path(result["manifest"]["path"])
            self.assertEqual(manifest, Path(str(destination) + ".manifest") / "manifest.fmnp")
            self.assertTrue(manifest.read_bytes().startswith(b"FMNP"))
            self.assertEqual(result["manifest"]["closure_digest"], result["closure_digest"])
            self.assertEqual(hashlib.sha256(destination.read_bytes()).hexdigest(), result["artifact_digest"])
        return batch

    def test_named_selection_keeps_order_and_robot_stdout_clean(self):
        receipt, process, _ = self.command("Second", "First")
        batch = self.publications(receipt)
        self.assertTrue(batch["all_scenes_certified"])
        self.assertEqual(batch["execution"], "spawn")
        self.assertEqual(batch["workers"], 2)
        self.assertEqual([row["name"] for row in batch["outcomes"]], ["Second", "First"])
        self.assertIn("authored discovery/render log", process.stderr)
        self.assertIn("authored fd log", process.stderr)

    def test_write_all_and_sequential_path_render_identical_native_pngs(self):
        parallel, _, _ = self.command("--write_all")
        # The existing sequential robot boundary redirects print, not fd-level
        # authored writes. Keep this comparison about unchanged render semantics.
        self.source.write_text(_SOURCE.replace('os.write(1, b"authored fd log\\n")\n', ''))
        sequential, _, _ = self.command("--write_all", workers=None)
        left, right = self.publications(parallel), self.publications(sequential)
        self.assertTrue(left["all_scenes_certified"])
        self.assertTrue(right["all_scenes_certified"])
        self.assertEqual(right["execution"], "sequential")
        self.assertEqual([row["name"] for row in left["outcomes"]], ["First", "Second"])
        for a, b in zip(left["outcomes"], right["outcomes"]):
            self.assertEqual(Path(a["destination"]).read_bytes(), Path(b["destination"]).read_bytes())

    def test_unsupported_combinations_refuse_before_discovery_imports(self):
        marker = self.root / "imported"
        self.source.write_text(f'from pathlib import Path\nPath({str(marker)!r}).write_text("yes")\n' + _SOURCE)
        for format, options in (
            ("gif", ()), ("mp4", ()), ("png_sequence", ("--subdivide",)),
            ("png_sequence", ("--save-last-frame",)),
            ("png", ("--checkpoint", str(self.root / "progress.json"), "--resume-key", "inputs")),
        ):
            with self.subTest(format=format, options=options):
                receipt, _, destination = self.command("--write_all", format=format, extra=options, expected=4)
                self.assertEqual(receipt["kind"], "render-capability-unavailable")
                self.assertFalse(marker.exists())
                self.assertFalse(destination.exists())
        self.assertFalse((self.root / "progress.json").exists())

    def test_keep_going_keeps_certified_successes_without_certifying_failed_batch(self):
        self.source.write_text(_SOURCE + '''class Broken(Scene):
    def construct(self):
        raise RuntimeError("intentional construction failure")
''')
        receipt, _, _ = self.command("Second", "Broken", "First", extra=("--keep-going",), expected=5)
        batch = self.publications(receipt)
        self.assertFalse(batch["all_scenes_certified"])
        self.assertEqual([row["status"] for row in batch["outcomes"]], ["succeeded", "failed", "succeeded"])
        self.assertIn("intentional construction failure", batch["outcomes"][1]["error"]["message"])

    def test_import_effects_are_audited_in_render_workers_not_lost_in_discovery(self):
        self.source.write_text('from pathlib import Path\nlist(Path(__file__).parent.iterdir())\n' + _SOURCE)
        receipt, _, _ = self.command("--write_all", extra=("--keep-going",), expected=5)
        batch = receipt["batch"]
        self.assertFalse(batch["all_scenes_certified"])
        for row in batch["outcomes"]:
            self.assertEqual(row["status"], "failed")
            self.assertIn("directory listing", row["error"]["message"])
            self.assertFalse(Path(row["destination"]).exists())

    def test_job_timeout_does_not_misreport_a_certificate(self):
        self.source.write_text('''import time
from manimlib import Scene, Square
class Slow(Scene):
    def construct(self):
        time.sleep(20)
        self.add(Square())
''')
        receipt, _, _ = self.command("--write_all", extra=("--job-timeout", "5"), expected=5)
        batch = receipt["batch"]
        self.assertFalse(batch["all_scenes_certified"])
        self.assertIs(batch["reproducible_requested"], True)
        row = batch["outcomes"][0]
        self.assertEqual(row["status"], "failed")
        self.assertEqual(row["error"]["type"], "TimeoutError")
        self.assertIsNone(row["result"])
        self.assertFalse(Path(row["destination"]).exists())


if __name__ == "__main__":
    unittest.main()
