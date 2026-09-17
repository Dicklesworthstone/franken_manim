"""Host-policy tests without executing a scene or requiring a native extension."""
import contextlib
import hashlib
import io
import json
import os
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from fmn_python.studio import Studio, _build_id, _integer, _source, try_studio_cli


class NativeHost:
    def __init__(self, builder, scene, token, port, timeout):
        self.builder = builder
        self.artifact = builder()
        self.url = f"http://127.0.0.1:{port}/?cap={token}"
        self.alive = True

    def close(self):
        self.alive = False


class StudioPolicyTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="studio-policy-")
        self.root = Path(self.temp.name)
        self.source = self.root / "lesson.py"
        self.source.write_text("raise RuntimeError('parent must not execute this')\n")
        self.extension = self.root / "native.so"
        self.extension.write_bytes(b"test engine bytes")
        self.native = SimpleNamespace(__file__=str(self.extension), _StudioHost=NativeHost)
        self.namespace = patch.dict(sys.modules, {"manimlib": SimpleNamespace(_native=self.native)})
        self.namespace.start()

    def tearDown(self):
        self.namespace.stop()
        self.temp.cleanup()

    def test_build_has_exact_host_runtime_but_does_not_execute_source(self):
        with Studio(self.source, "Lesson") as host:
            executable, argv, environment, cwd, build = host._host.artifact
            self.assertEqual(executable, os.path.abspath(sys.executable))
            self.assertEqual(argv[:4], ["-I", "-m", "fmn_python.studio", "--worker"])
            request = json.loads(argv[4])
            self.assertEqual(request["build_id"], _build_id(request))
            self.assertEqual(build, request["build_id"])
            self.assertEqual(request["source_sha256"], hashlib.sha256(self.source.read_bytes()).hexdigest())
            self.assertEqual(request["runtime"]["native"], hashlib.sha256(self.extension.read_bytes()).hexdigest())
            self.assertEqual(cwd, str(self.root))
            self.assertEqual(environment, sorted(os.environ.items()))
            self.assertEqual(len(host.url.split("?cap=")[1]), 64)
        self.assertFalse(host.alive)

    def test_reload_observes_new_source_but_retains_host_environment(self):
        with Studio(self.source, "Lesson") as host:
            first = host._host.artifact
            self.source.write_text("x = 1\n")
            with patch.dict(os.environ, {"FMN_STUDIO_POLICY_TEST": "new environment"}):
                second = host._host.builder()
            self.assertNotEqual(first[-1], second[-1])
            self.assertEqual(first[2], second[2])
            self.source.write_text("invalid(\n")
            with self.assertRaises(SyntaxError):
                host._host.builder()
            self.assertTrue(host.alive)

    def test_invalid_resource_profiles_fail_before_launch(self):
        for options in ({"fps": True}, {"fps": 0}, {"threads": 97}, {"max_frames": 0},
                        {"max_bytes": 2**31}, {"resolution": (16384, 16384)},
                        {"resolution": (20,)}, {"port": 65536}, {"timeout": 0}):
            with self.subTest(options=options), self.assertRaises(ValueError):
                Studio(self.source, "Lesson", **options)
        for value in (True, 1.0, -1, 500):
            with self.assertRaises(ValueError):
                _integer(value, "resource", 1, 100)

    def test_source_preflight_compiles_without_importing(self):
        self.assertEqual(_source(self.source), self.source.read_bytes())
        self.source.write_text("broken(\n")
        with self.assertRaises(SyntaxError):
            _source(self.source)

    def test_cli_studio_usage_refuses_extra_or_missing_options(self):
        receipts = []
        def emit(code, identity, kind, message, robot, **fields):
            receipts.append((code, identity, kind, message, robot, fields))
            return code
        native = SimpleNamespace(_portal_cli_emit=emit)
        for args in (["studio"], ["studio", "scene.py", "Example", "--format", "mp4"],
                     ["studio", "scene.py", "Example", "--fp", "24"]):
            with self.subTest(args=args), contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(try_studio_cli(native, ["--robot", *args]), 2)
                self.assertEqual(receipts[-1][1], "usage")
        self.assertIsNone(try_studio_cli(native, ["scene.py", "Example"]))
        self.assertEqual(try_studio_cli(native, ["studio", "--robot", "--help"]), 0)
        self.assertIn("disposable", receipts[-1][-1]["help"])


if __name__ == "__main__":
    unittest.main()
