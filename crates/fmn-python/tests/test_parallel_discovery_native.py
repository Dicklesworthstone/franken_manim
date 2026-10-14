"""Real wheel/source/CLI checks for isolated discovery, not render substitutes."""
import builtins
import hashlib
import json
import multiprocessing as mp
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

from test_parallel_batch_native import native_wheel_available

PYTHON = Path(__file__).resolve().parents[1] / "python"
sys.path.insert(0, str(PYTHON))
from fmn_python.parallel_discovery import discover_source_scenes, SceneDiscoveryError
from fmn_python.scene_loading import SceneSource


class DiscoveryAdmissionTests(unittest.TestCase):
    def test_invalid_inputs_never_launch_a_worker(self):
        with tempfile.TemporaryDirectory() as directory, patch(
                "fmn_python.parallel_discovery.run_process_jobs") as launch:
            source = Path(directory) / "scene.py"
            source.write_text("", encoding="utf-8")
            for options in ({"max_scenes": 0}, {"max_scenes": True},
                            {"job_timeout": float("inf")}, {"job_timeout": False}):
                with self.subTest(options=options), self.assertRaises((TypeError, ValueError)):
                    discover_source_scenes(source, **options)
            with self.assertRaises(FileNotFoundError):
                discover_source_scenes(source.with_name("missing.py"))
            with self.assertRaises(ValueError):
                discover_source_scenes(source.with_suffix(".txt"))
            launch.assert_not_called()

    @unittest.skipUnless(hasattr(os, "mkfifo"), "requires POSIX FIFOs")
    def test_fifo_source_refuses_before_blocking_open(self):
        with tempfile.TemporaryDirectory() as directory, patch(
                "fmn_python.parallel_discovery.run_process_jobs") as launch:
            source = Path(directory) / "pipe.py"
            os.mkfifo(source)
            with self.assertRaisesRegex(ValueError, "regular file"):
                discover_source_scenes(source, job_timeout=1)
            launch.assert_not_called()

    def test_changed_entry_is_rejected_before_its_top_level_effects(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "scene.py"
            original = b"# original\n"
            source.write_bytes(original)
            digest = hashlib.sha256(original).hexdigest()
            marker = source.with_name("executed")
            source.write_text(f"from pathlib import Path\nPath({str(marker)!r}).touch()\n")
            with self.assertRaisesRegex(RuntimeError, "entry source changed"):
                with SceneSource(source, object, entry_digest=digest):
                    self.fail("mismatched source was admitted")
            self.assertFalse(marker.exists())
            for value in ("not a hash", "A" * 64, 1, True):
                with self.assertRaises(ValueError):
                    SceneSource(source, object, entry_digest=value)

    def test_malformed_discovery_receipt_is_not_a_scene_inventory(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "scene.py"
            source.write_text("")
            valid = {"schema": "fmn-python.scene-discovery", "version": 1,
                     "source": str(source.resolve()), "source_digest": hashlib.sha256(b"").hexdigest(),
                     "scenes": ["First"]}
            for change in ({"version": True}, {"source_digest": "0" * 64},
                           {"scenes": ["First", "First"]}, {"scenes": [1]},
                           {"scenes": ["Second", "First"]}):
                with self.subTest(change=change), patch(
                        "fmn_python.parallel_discovery.run_process_jobs",
                        return_value=[{"status": "succeeded", "result": valid | change}]):
                    with self.assertRaises(ValueError):
                        discover_source_scenes(source)


@unittest.skipUnless(native_wheel_available(), "requires the built FrankenManim native wheel")
class NativeDiscoveryTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name).resolve()
        self.source = self.root / "scenes.py"
        self.children = {child.pid for child in mp.active_children()}

    def tearDown(self):
        self.assertEqual({child.pid for child in mp.active_children()}, self.children)

    def write(self, source):
        self.source.write_text(source, encoding="utf-8")

    def cli(self, *options):
        environment = dict(os.environ, PYTHONPATH=str(PYTHON))
        return subprocess.run(
            [sys.executable, "-m", "fmn_python", str(self.source), "--write_all",
             "--workers", "2", "--robot", "--format", "png", "--resolution", "64x48",
             "--fps", "10", "--video_dir", str(self.root / "outputs"), *options],
            env=environment, cwd=self.root, text=True, capture_output=True, timeout=30,
        )

    def test_dynamic_scene_classes_and_helpers_use_the_actual_loader(self):
        package = self.root / "example"
        package.mkdir()
        (package / "__init__.py").write_text("")
        (package / "helper.py").write_text("from manimlib import Scene\nclass Base(Scene): pass\n")
        self.source = package / "scenes.py"
        self.write('''from .helper import Base
class First(Base):
    def __init__(self):
        raise AssertionError("discovery constructed a scene")
Alias = First
Generated = type("Generated", (Base,), {"__module__": __name__})
''')
        result = discover_source_scenes(self.source)
        self.assertEqual(result.scenes, ("Alias", "First", "Generated"))
        self.assertEqual(result.source_digest, hashlib.sha256(self.source.read_bytes()).hexdigest())
        self.assertFalse(any(name == "example" or name.startswith("example.") for name in sys.modules))

    def test_import_effects_stay_out_of_the_parent_interpreter(self):
        cwd = Path.cwd()
        old_env = os.environ.get("FMN_DISCOVERY_TEST")
        self.write('''import os, builtins
from pathlib import Path
from manimlib import Scene
Path(__file__).with_suffix(".pid").write_text(str(os.getpid()))
os.environ["FMN_DISCOVERY_TEST"] = "worker-only"
builtins._fmn_discovery_test = True
os.chdir(Path(__file__).parent)
class First(Scene): pass
''')
        self.assertEqual(discover_source_scenes(self.source).scenes, ("First",))
        self.assertEqual(Path.cwd(), cwd)
        self.assertEqual(os.environ.get("FMN_DISCOVERY_TEST"), old_env)
        self.assertFalse(hasattr(builtins, "_fmn_discovery_test"))
        self.assertNotEqual(int(self.source.with_suffix(".pid").read_text()), os.getpid())

    def test_import_crash_becomes_a_cli_error_not_a_coordinator_exit(self):
        self.write("import os\nos._exit(17)\n")
        process = self.cli()
        self.assertEqual(process.returncode, 5, process.stderr)
        receipt = json.loads(process.stdout)
        self.assertEqual(receipt["kind"], "scene-load-failed")
        self.assertIn("receipt", receipt["message"])
        self.assertFalse((self.root / "outputs").exists())

    def test_import_timeout_returns_without_starting_render_jobs(self):
        self.write('''import time
from pathlib import Path
Path(__file__).with_suffix(".started").touch()
time.sleep(60)
''')
        process = self.cli("--job-timeout", "3")
        self.assertTrue(self.source.with_suffix(".started").exists())
        self.assertEqual(process.returncode, 5, process.stderr)
        self.assertIn("TimeoutError", json.loads(process.stdout)["message"])
        self.assertFalse((self.root / "outputs").exists())

    def test_child_interrupt_retains_cli_interruption_identity(self):
        self.write("raise KeyboardInterrupt()\n")
        process = self.cli()
        self.assertEqual(process.returncode, 130, process.stderr)
        self.assertEqual(json.loads(process.stdout)["kind"], "render-interrupted")

    def test_robot_stdout_is_one_receipt_even_with_fd_level_scene_output(self):
        self.write('''import os
from manimlib import Scene, Square
os.write(1, b"fd-level scene output\\n")
print("python scene output")
class First(Scene):
    def construct(self):
        self.add(Square())
class Second(First): pass
''')
        process = self.cli()
        self.assertEqual(process.returncode, 0, process.stderr)
        report = json.loads(process.stdout)["batch"]
        self.assertEqual(report["counts"]["succeeded"], 2)
        self.assertEqual(process.stderr.count("fd-level scene output"), 3)
        self.assertEqual(process.stderr.count("python scene output"), 3)
        for name in ("First", "Second"):
            self.assertEqual((self.root / "outputs" / (name + ".png")).read_bytes()[:8],
                             b"\x89PNG\r\n\x1a\n")

    def test_discovery_scene_count_limit_is_a_worker_failure(self):
        self.write("from manimlib import Scene\nclass First(Scene): pass\nclass Second(Scene): pass\n")
        with self.assertRaisesRegex(SceneDiscoveryError, "scene-count budget"):
            discover_source_scenes(self.source, max_scenes=1)

    def test_package_cannot_replace_entry_between_plan_and_compile(self):
        package = self.root / "rewriter"
        package.mkdir()
        self.source = package / "scenes.py"
        self.write("from manimlib import Scene\nclass First(Scene): pass\n")
        marker = self.root / "unexpected-execution"
        replacement = f"from pathlib import Path\nPath({str(marker)!r}).touch()\n"
        (package / "__init__.py").write_text(
            f"from pathlib import Path\nPath(__file__).with_name('scenes.py').write_text({replacement!r})\n")
        with self.assertRaisesRegex(SceneDiscoveryError, "entry source changed"):
            discover_source_scenes(self.source)
        self.assertFalse(marker.exists())


if __name__ == "__main__":
    unittest.main()
