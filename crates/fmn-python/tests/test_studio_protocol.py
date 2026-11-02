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
    def __init__(self, builder, scene, token, port, timeout, startup_timeout):
        self.builder = builder
        self.timeout, self.startup_timeout = timeout, startup_timeout
        self.artifact = builder()
        self.url = f"http://127.0.0.1:{port}/?cap={token}"
        self.alive = True

    def close(self):
        self.alive = False

    @staticmethod
    def watch_sources(roots, debounce_ms):
        from test_studio_inputs_protocol import fake_snapshot
        return fake_snapshot(roots, debounce_ms)


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

    def test_cold_start_and_operation_deadlines_are_independent(self):
        with Studio(self.source, "Lesson", timeout=3, startup_timeout=40) as host:
            self.assertEqual(host._host.timeout, 3)
            self.assertEqual(host._host.startup_timeout, 40)
            first = host._host.artifact[-1]
            with patch("fmn_python.studio._reload_request", return_value={}) as reload_request:
                host.reload()
            self.assertEqual(reload_request.call_args.args[1], 91)
            self.assertEqual(host._timeout, 8, "ordinary requests retain their short client deadline")
        with Studio(self.source, "Lesson", timeout=3, startup_timeout=2) as host:
            self.assertEqual(host._host.artifact[-1], first,
                             "process deadlines do not change the captured scene identity")

    def test_worker_waits_for_a_native_request_before_capturing_source(self):
        from fmn_python import scene_console, studio
        with Studio(self.source, "Lesson") as host:
            encoded = host._host.artifact[1][-1]
        observed = []
        recording = object()
        self.native._fmn_ensure_tex_cache = lambda: observed.append("runtime ready")

        class DeferredBoundary:
            @staticmethod
            def serve_deferred(build, scene, capture):
                capture_scene.assert_not_called()
                self.assertEqual(observed, ["runtime ready"])
                observed.append((build, scene))
                with capture() as prepared:
                    self.assertIs(prepared, recording)

        def capture_once(request, native):
            self.assertIs(native, self.native)
            self.assertTrue(scene_console._STUDIO_WORKER.get())
            return contextlib.nullcontext(recording)

        self.native._StudioRecording = DeferredBoundary
        with patch("fmn_python.studio._capture", side_effect=capture_once) as capture_scene:
            self.assertEqual(studio._worker(encoded), 0)
        capture_scene.assert_called_once()
        self.assertEqual(observed, ["runtime ready", (json.loads(encoded)["build_id"], "Lesson")])
        self.assertFalse(scene_console._STUDIO_WORKER.get())

    def test_live_capture_prepares_one_service_and_retains_its_source_context(self):
        from fmn_python import studio
        with Studio(self.source, "Lesson", interactive=True) as host:
            request = json.loads(host._host.artifact[1][-1])
        events, instances = [], []
        recording = object()
        case = self

        class Scene:
            random_seed = 0
            def __init__(self):
                self.camera = SimpleNamespace(_core=SimpleNamespace(set_pixel_shape=lambda *args: None))
                instances.append(self)
            def _begin_studio_capture(self, *args):
                events.append("begin")
            def run(self):
                events.append("construct")
            def _prepare_studio_live(self):
                case.assertTrue(self.__dict__["_fmn_studio_live_input"])
                events.append("prepare")
                return recording
            def _serve_studio_live(self):
                case.fail("capture must not open a second protocol loop")
            def _abort_render(self):
                events.append("abort")

        class Source:
            scenes = {"Lesson": Scene}
            source_digests = {self.source: request["source_sha256"]}
            def __enter__(self):
                events.append("enter")
                return self
            def __exit__(self, *args):
                events.append("exit")

        self.native.Scene = Scene
        self.native.EndScene = type("EndScene", (Exception,), {})
        inputs = SimpleNamespace(files={}, fingerprint="12" * 32,
                                 require_source=lambda *args: None, verify=lambda *args: None)
        with patch("fmn_python.scene_loading.SceneSource", return_value=Source()), \
             patch.object(studio.StudioInputs, "from_request", return_value=inputs):
            context = studio._capture(request, self.native)
            self.assertEqual(events, [])
            with context as prepared:
                self.assertIs(prepared, recording)
                self.assertEqual(events, ["enter", "begin", "construct", "prepare"])
                self.assertIn("_fmn_studio_validate_inputs", instances[0].__dict__)
            self.assertEqual(events, ["enter", "begin", "construct", "prepare", "abort", "exit"])
            self.assertNotIn("_fmn_studio_live_input", instances[0].__dict__)
            self.assertNotIn("_fmn_studio_validate_inputs", instances[0].__dict__)

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

    def test_interactive_authority_is_explicit_and_part_of_the_worker_identity(self):
        with Studio(self.source, "Lesson") as recorded:
            first = recorded._host.artifact[-1]
            self.assertFalse(recorded.interactive)
        with Studio(self.source, "Lesson", interactive=True) as live:
            request = json.loads(live._host.artifact[1][-1])
            self.assertTrue(request["interactive"])
            self.assertTrue(live.interactive)
            self.assertNotEqual(first, request["build_id"])
        with self.assertRaisesRegex(ValueError, "interactive must be a bool"):
            Studio(self.source, "Lesson", interactive=1)

    def test_invalid_resource_profiles_fail_before_launch(self):
        for options in ({"fps": True}, {"fps": 0}, {"threads": 97}, {"max_frames": 0},
                        {"max_bytes": 2**31}, {"resolution": (16384, 16384)},
                        {"resolution": (20,)}, {"port": 65536}, {"timeout": 0},
                        {"startup_timeout": 0}, {"startup_timeout": 901},
                        {"startup_timeout": True}, {"startup_timeout": 1.5}):
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

    def test_programmatic_reload_receipt_and_failure_status_are_detached(self):
        with Studio(self.source, "Lesson") as host:
            with patch("fmn_python.studio._reload_request", return_value={"sha256": "a" * 64, "frame_index": 0}) as reload:
                receipt = host.reload()
                reload.assert_called_once_with(host.url, 485)
                receipt["sha256"] = "mutated"
                status = host.reload_status
                self.assertEqual(status["completed"], 1)
                self.assertEqual(status["result"]["sha256"], "a" * 64)
                status["result"]["sha256"] = "mutated again"
                self.assertEqual(host.reload_status["result"]["sha256"], "a" * 64)
            with patch("fmn_python.studio._reload_request", side_effect=RuntimeError("broken edit")):
                with self.assertRaisesRegex(RuntimeError, "broken edit"):
                    host.reload()
            self.assertEqual(host.reload_status["completed"], 1)
            self.assertEqual(host.reload_status["error"], "broken edit")
            self.assertTrue(host.alive)
        with self.assertRaisesRegex(RuntimeError, "closed"):
            host.reload()

    def test_autoreload_owns_one_native_watcher_and_joins_on_close(self):
        asset = self.root / "values.csv"
        with Studio(self.source, "Lesson", autoreload=True, watch_paths=[asset], debounce_ms=50) as host:
            self.assertTrue(host.autoreload)
            self.assertEqual(host._watch.watched, ([str(self.source), str(self.root), str(asset)], 50))
            thread = host._watch_thread
            self.assertTrue(thread.is_alive())
        self.assertFalse(thread.is_alive())
        self.assertFalse(host.alive)
        host.close()

    def test_autoreload_retries_changed_inputs_not_the_same_failed_edit(self):
        from fmn_python.studio import _watch_loop
        import weakref
        with Studio(self.source, "Lesson") as host:
            # Native SourceWatch owns stabilization/one-shot behavior. Verify
            # this caller records a failed attempt without manufacturing retries.
            polls = iter([True, False])
            def poll():
                result = next(polls)
                if not result:
                    host._stop.set()
                return result
            with patch("fmn_python.studio._reload_request", side_effect=RuntimeError("failed")) as reload:
                _watch_loop(weakref.ref(host), host._stop, SimpleNamespace(poll=poll))
                self.assertEqual(reload.call_count, 1)
            self.assertEqual(host.reload_status["revision"], 1)
            self.assertEqual(host.reload_status["error"], "failed")

    def test_invalid_watch_options_fail_before_launch(self):
        for options in ({"autoreload": 1}, {"watch_paths": [self.source]},
                        {"autoreload": True, "watch_paths": str(self.source)},
                        {"debounce_ms": -1}, {"debounce_ms": True}):
            with self.subTest(options=options), self.assertRaises(ValueError):
                Studio(self.source, "Lesson", **options)

    def test_watch_snapshot_precedes_initial_worker_execution(self):
        events = []
        class OrderedHost(NativeHost):
            @staticmethod
            def watch_sources(roots, debounce_ms):
                events.append("snapshot")
                return NativeHost.watch_sources(roots, debounce_ms)
            def __init__(self, *args):
                events.append("worker")
                super().__init__(*args)
        self.native._StudioHost = OrderedHost
        with Studio(self.source, "Lesson", autoreload=True):
            self.assertEqual(events, ["snapshot", "worker", "snapshot"])

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
