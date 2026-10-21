"""Installed-wheel acceptance; protocol fixtures are not a substitute."""
import json
import subprocess
import sys
from pathlib import Path
import tempfile
import unittest

from test_parallel_batch_native import native_wheel_available


@unittest.skipUnless(native_wheel_available(), "requires the built native wheel")
class NativeSourceVariantTests(unittest.TestCase):
    def test_native_variants_match_sequential_constructor_exports_and_resume(self):
        from fmn_python import RenderJob, render_scenes, render_source_scenes
        from fmn_python.source_jobs import SourceRenderJob
        from fmn_python.scene_loading import SceneSource
        import manimlib

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            source = root / "scenes.py"
            source.write_text('''from manimlib import Scene, Square
class Lesson(Scene):
    def __init__(self, side=1.0, **kwargs):
        super().__init__(**kwargs)
        self.side = side
    def construct(self):
        self.add(Square(side_length=self.side, fill_opacity=1))
        self.wait(0.25)
''')
            jobs = [SourceRenderJob("small", "Lesson", {"side": 0.5, "random_seed": 7}),
                    SourceRenderJob("large", "Lesson", {"side": 2.0, "random_seed": 11})]
            options = dict(format="fmtl", bundle_camera=True, resolution=(64, 36), fps=8)
            with SceneSource(source, manimlib.Scene) as loaded:
                sequential = render_scenes([RenderJob(job.name, loaded.scenes[job.scene], job.scene_kwargs)
                                            for job in jobs], root / "sequential", **options)
            parallel = render_source_scenes(source, root / "parallel", jobs, workers=2,
                checkpoint=root / "progress.json", resume_key="native-variants-v1", **options)
            self.assertTrue(sequential.ok and parallel.ok)
            for expected, actual in zip(sequential.outcomes, parallel.outcomes):
                self.assertEqual(actual.destination.read_bytes(), expected.destination.read_bytes())
                self.assertEqual(actual.result.digest, expected.result.digest)
                self.assertEqual(actual.result.frame_count, 2)
            before = [row.destination.stat().st_mtime_ns for row in parallel.outcomes]
            recovered = render_source_scenes(source, root / "parallel", jobs, workers=1,
                checkpoint=root / "progress.json", resume_key="native-variants-v1", resume=True, **options)
            self.assertEqual(recovered.reused, ("small", "large"))
            self.assertEqual([row.destination.stat().st_mtime_ns for row in recovered.outcomes], before)

    def test_installed_bundle_cli_renders_json_variants_and_reuses_outputs(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            source = root / "scenes.py"
            source.write_text("from manimlib import Scene, Square\n"
                "class Lesson(Scene):\n"
                "    def __init__(self, side=1.0, **kwargs):\n"
                "        super().__init__(**kwargs)\n"
                "        self.side = side\n"
                "    def construct(self):\n"
                "        self.add(Square(side_length=self.side, fill_opacity=1))\n"
                "        self.wait(0.25)\n")
            job_file = root / "jobs.json"
            job_file.write_text(json.dumps({"schema": "fmn-python.source-jobs", "version": 1,
                "jobs": [{"name": "small", "scene": "Lesson", "scene_kwargs": {"side": 0.5}},
                         {"name": "large", "scene": "Lesson", "scene_kwargs": {"side": 2.0}}]}))
            command = [sys.executable, "-m", "fmn_python", "--robot", str(source),
                       "--format", "fmtl", "--workers", "2", "--jobs-file", str(job_file),
                       "--bundle-camera", "--resolution", "64x36", "--fps", "8",
                       "--video_dir", str(root / "out"), "--checkpoint", str(root / "progress.json"),
                       "--resume-key", "native-cli-v1"]
            first = subprocess.run(command, capture_output=True, text=True, timeout=120, check=False)
            self.assertEqual(first.returncode, 0, first.stdout + first.stderr)
            rows = json.loads(first.stdout)["batch"]["outcomes"]
            self.assertEqual([row["name"] for row in rows], ["small", "large"])
            data = [Path(row["destination"]).read_bytes() for row in rows]
            self.assertTrue(all(item.startswith(b"FMTL") for item in data))
            self.assertNotEqual(data[0], data[1])
            second = subprocess.run(command + ["--resume"], capture_output=True, text=True,
                                    timeout=120, check=False)
            self.assertEqual(second.returncode, 0, second.stdout + second.stderr)
            self.assertEqual(json.loads(second.stdout)["batch"]["reused"], ["small", "large"])


if __name__ == "__main__":
    unittest.main()
