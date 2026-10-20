"""Host policy tests for the real adapter, independent of a native build.

The backend is a protocol recorder here, not a substitute typesetter. Native
layout/cache behavior is tested by fmn-tex/tests/cache_binding.rs and the
installed-extension typeset_cache.py suite.
"""
from contextlib import redirect_stderr
from copy import deepcopy
import importlib.util
import io
import json
import os
from pathlib import Path
import sys
from threading import Thread
from types import SimpleNamespace
import unittest
from unittest.mock import patch


SOURCE = Path(__file__).resolve().parents[1] / "python/fmn_python/typesetting.py"
spec = importlib.util.spec_from_file_location("fmn_typesetting_test_subject", SOURCE)
runtime = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runtime)


def protocol(error=None):
    calls = []
    ceilings, maintenance = [], [0]
    policy = {"max_bytes": None, "persistent": False}
    class Scene:
        def __init__(self, *args, **kwargs):
            calls.append(("scene", args, kwargs))
            if kwargs.get("fail"):
                raise ValueError("scene constructor refused")
            self.received = (args, kwargs)
        def _run_lifecycle(self):
            calls.append(("lifecycle",))
            if hasattr(self, "lifecycle_error"):
                raise self.lifecycle_error
            return "rendered"
    def backend(directory, *, max_bytes=None):
        calls.append(("cache", directory))
        ceilings.append(max_bytes)
        policy.update(max_bytes=max_bytes, persistent=directory is not None and error is None)
        return {"schema": "fmn-python.typeset-cache", "version": 1,
                "requested": directory is not None, "directory": directory,
                "templates": {name: {
                    "persistent": directory is not None and error is None,
                    "memory_hits": 0, "memory_misses": 0,
                    "memory_entries": 0, "memory_bytes": 0,
                    "disk_hits": 0, "layout_computations": 0,
                    "persistent_ceiling_bytes": max_bytes if policy["persistent"] else None,
                    "error": error,
                } for name in ("default", "basic", "empty")}}
    def validate(template, preamble, text_mode):
        calls.append(("validate", template, preamble, text_mode))
        if template == "invalid":
            raise ValueError("unknown template")
        return "validated"
    def info():
        return {name: {"memory_hits": 7,
                       "persistent_ceiling_bytes": policy["max_bytes"] if policy["persistent"] else None,
                       "maintenance": {"runs": maintenance[0], "evicted_entries": maintenance[0],
                                       "evicted_bytes": maintenance[0] * 100,
                                       "retained_bytes": 250 if maintenance[0] else None}}
                for name in ("default", "basic", "empty")}
    def maintain():
        calls.append(("maintenance",))
        maintenance[0] += 1
    native = SimpleNamespace(Scene=Scene, _configure_tex_cache=backend,
                             _tex_cache_info=info, _validate_tex_options=validate,
                             _maintain_tex_cache=maintain, ceiling_calls=ceilings)
    return native, calls


class TypesettingRuntime(unittest.TestCase):
    def setUp(self):
        self.native, self.calls = protocol()
        runtime.install_typesetting(self.native)

    def cache_calls(self):
        return [call for call in self.calls if call[0] == "cache"]

    def test_installation_is_io_free_and_preserves_class_identity(self):
        native, calls = protocol()
        Scene = native.Scene
        runtime.install_typesetting(native)
        self.assertIs(native.Scene, Scene)
        self.assertEqual(calls, [])

    def test_first_scene_configures_once_before_authored_construct(self):
        scene = self.native.Scene(5, camera_config={"fps": 24})
        self.assertEqual(scene.received, ((5,), {"camera_config": {"fps": 24}}))
        self.assertEqual(self.cache_calls(), [("cache", "")])
        self.native.Scene()
        self.assertEqual(len(self.cache_calls()), 1)

    def test_direct_tex_validation_also_activates_the_real_shared_engines(self):
        self.assertEqual(self.native._validate_tex_options("basic", "", False), "validated")
        self.assertEqual(self.cache_calls(), [("cache", "")])
        self.native.Scene()
        self.assertEqual(len(self.cache_calls()), 1)

    def test_bad_scene_or_template_does_not_create_a_cache(self):
        with self.assertRaisesRegex(ValueError, "constructor"):
            self.native.Scene(fail=True)
        with self.assertRaisesRegex(ValueError, "template"):
            self.native._validate_tex_options("invalid", "", False)
        self.assertEqual(self.cache_calls(), [])

    def test_explicit_disable_survives_scene_and_tex_construction(self):
        result = self.native._fmn_configure_tex_cache(enabled=False)
        self.assertFalse(result["requested"])
        self.native.Scene()
        self.native._validate_tex_options("default", "", False)
        self.assertEqual(self.cache_calls(), [("cache", None)])

    def test_explicit_reconfigure_reopens_even_the_same_path(self):
        for _ in range(2):
            self.native._fmn_configure_tex_cache("cache-leaf")
        self.assertEqual(self.cache_calls(), [("cache", os.path.abspath("cache-leaf"))] * 2)
        self.native.Scene()
        self.assertEqual(len(self.cache_calls()), 2)

    def test_relative_paths_are_anchored_and_symlinks_are_not_resolved(self):
        with patch.object(runtime.os.path, "abspath", return_value="/chosen/link/cache") as absolute:
            with patch.object(runtime.Path, "resolve", side_effect=AssertionError("must not hide symlinks")):
                self.native._fmn_configure_tex_cache("cache")
        absolute.assert_called_once_with("cache")
        self.assertEqual(self.cache_calls(), [("cache", "/chosen/link/cache")])

    def test_pathlike_conversion_is_single_and_accepts_path_objects(self):
        class Location:
            calls = 0
            def __fspath__(self):
                self.calls += 1
                return "cache"
        location = Location()
        self.native._fmn_configure_tex_cache(location)
        self.assertEqual(location.calls, 1)
        self.native._fmn_configure_tex_cache(Path("cache"))

    def test_hostile_paths_and_flags_refuse_before_native_work(self):
        for value in (b"cache", "\0", "x/../cache", "λ" * 3000, 12):
            with self.subTest(value=repr(value)[:40]):
                with self.assertRaises((TypeError, ValueError)):
                    self.native._fmn_configure_tex_cache(value)
        for flag in (0, 1, "yes", None):
            with self.assertRaises(TypeError):
                self.native._fmn_configure_tex_cache(enabled=flag)
        self.assertEqual(self.cache_calls(), [])

    def test_unavailable_storage_reports_once_and_scene_keeps_running(self):
        native, calls = protocol("ownership_missing: foreign cache directory")
        runtime.install_typesetting(native)
        output = io.StringIO()
        with redirect_stderr(output):
            scene = native.Scene()
            native.Scene()
        self.assertIsNotNone(scene)
        records = output.getvalue().splitlines()
        self.assertEqual(len(records), 1)
        event = json.loads(records[0])
        self.assertEqual(event["fallback"], "memory-and-layout")
        self.assertEqual(len(event["errors"]), 1)
        self.assertFalse(native._fmn_tex_cache_info()["templates"]["default"]["persistent"])
        self.assertEqual(len([call for call in calls if call[0] == "cache"]), 1)

    def test_receipts_are_snapshots_not_mutable_runtime_state(self):
        result = self.native._fmn_configure_tex_cache(enabled=False)
        result["templates"]["default"]["error"] = "injected"
        before = self.native._fmn_tex_cache_info()
        self.assertIsNone(before["templates"]["default"]["error"])
        self.assertEqual(before["templates"]["default"]["memory_hits"], 7)
        before["templates"].clear()
        self.assertEqual(len(self.native._fmn_tex_cache_info()["templates"]), 3)

    def test_independent_native_modules_do_not_share_policy(self):
        self.native._fmn_configure_tex_cache(enabled=False)
        other, other_calls = protocol()
        runtime.install_typesetting(other)
        other.Scene()
        self.assertEqual([call for call in other_calls if call[0] == "cache"], [("cache", "")])
        self.assertEqual(self.cache_calls(), [("cache", None)])

    def test_each_thread_binds_its_own_native_engine_slots(self):
        self.native._fmn_configure_tex_cache(enabled=False)
        errors = []
        def worker():
            try:
                self.native.Scene()
                self.native.Scene()
            except BaseException as error:
                errors.append(error)
        for _ in range(2):
            thread = Thread(target=worker)
            thread.start()
            thread.join()
        self.assertEqual(errors, [])
        self.assertEqual(self.cache_calls(), [("cache", None), ("cache", ""), ("cache", "")])
        self.assertFalse(self.native._fmn_tex_cache_info()["requested"])

    def test_installation_is_idempotent(self):
        init = self.native.Scene.__init__
        validate = self.native._validate_tex_options
        runtime.install_typesetting(self.native)
        self.assertIs(init, self.native.Scene.__init__)
        self.assertIs(validate, self.native._validate_tex_options)

    def test_engine_failures_are_not_mislabeled_as_optional_storage_failures(self):
        failure = RuntimeError("bundled math faces are corrupt")
        native, _ = protocol()
        def broken_backend(directory, *, max_bytes=None):
            raise failure
        native._configure_tex_cache = broken_backend
        runtime.install_typesetting(native)
        with self.assertRaises(RuntimeError) as caught:
            native.Scene()
        self.assertIs(caught.exception, failure)

    def test_public_functions_use_the_installed_production_module(self):
        with patch.dict(sys.modules, {"manimlib": self.native}):
            result = runtime.configure_tex_cache(enabled=False)
            self.assertFalse(result["requested"])
            self.assertEqual(runtime.tex_cache_info()["templates"]["basic"]["memory_hits"], 7)

    def test_configured_and_explicit_byte_ceilings_reach_the_native_owner(self):
        self.native.manim_config = {"directories": {"cache": "configured-cache"}, "cache": {"max_bytes": 4096}}
        self.native.Scene()
        self.assertEqual(self.cache_calls(), [("cache", os.path.abspath("configured-cache"))])
        self.assertEqual(self.native.ceiling_calls, [4096])
        with patch.dict(sys.modules, {"manimlib": self.native}):
            runtime.configure_tex_cache("manual-cache", max_bytes=0)
        self.native.Scene()
        self.assertEqual(self.native.ceiling_calls, [4096, 0])
        self.assertEqual(self.native._fmn_tex_cache_info()["templates"]["default"]["persistent_ceiling_bytes"], 0)

    def test_scoped_configuration_change_rebinds_once_and_restoring_it_rebinds_again(self):
        self.native.manim_config = {"directories": {"cache": "first"}, "cache": {"max_bytes": 4000}}
        self.native.Scene()
        self.native.manim_config["directories"]["cache"] = "second"
        self.native.manim_config["cache"]["max_bytes"] = 2000
        self.native.Scene()
        self.native.Scene()
        self.native.manim_config["directories"]["cache"] = "first"
        self.native.manim_config["cache"]["max_bytes"] = 4000
        self.native._validate_tex_options("default", "", False)
        self.assertEqual(self.native.ceiling_calls, [4000, 2000, 4000])
        self.assertEqual(self.cache_calls(), [("cache", os.path.abspath(path)) for path in ("first", "second", "first")])

    def test_invalid_explicit_ceilings_refuse_before_native_work(self):
        for value in (True, False, -1, 1 << 64, 1.5, "10", {}):
            with self.subTest(value=value), self.assertRaises((TypeError, ValueError)):
                self.native._fmn_configure_tex_cache("leaf", max_bytes=value)
        self.assertEqual(self.cache_calls(), [])

    def test_invalid_configured_ceiling_falls_back_once_without_failing_the_scene(self):
        self.native.manim_config = {"cache": {"max_bytes": 1}}
        self.native.Scene()
        self.native.manim_config["cache"]["max_bytes"] = True
        output = io.StringIO()
        with redirect_stderr(output):
            self.native.Scene()
            self.native.Scene()
        self.assertEqual(self.cache_calls(), [("cache", ""), ("cache", None)])
        self.assertEqual(len(output.getvalue().splitlines()), 1)
        self.assertIn("max_bytes", output.getvalue())

    def test_maintenance_runs_after_success_failure_and_cancellation(self):
        self.native._fmn_set_static_tex_preflight(False)
        for error in (None, ValueError("authored error"), KeyboardInterrupt(), SystemExit(0)):
            with self.subTest(error=error):
                scene = self.native.Scene()
                if error is None:
                    self.assertEqual(scene._run_lifecycle(), "rendered")
                else:
                    scene.lifecycle_error = error
                    with self.assertRaises(type(error)) as caught:
                        scene._run_lifecycle()
                    self.assertIs(caught.exception, error)
                self.assertEqual(self.calls[-2:], [("lifecycle",), ("maintenance",)])

    def test_render_receipt_reports_per_run_maintenance_without_tripling_retained_bytes(self):
        self.native._fmn_set_static_tex_preflight(False)
        self.native._fmn_configure_tex_cache("cache", max_bytes=1000)
        with patch.dict(sys.modules, {"manimlib": self.native}):
            for _ in range(2):
                scene = self.native.Scene()
                scene._run_lifecycle()
                receipt = runtime.typesetting_receipt(scene)
                self.assertEqual(receipt["persistent_ceiling_bytes"], 1000)
                self.assertEqual(receipt["maintenance"], {
                    "runs": 3, "evicted_entries": 3, "evicted_bytes": 300,
                    "retained_bytes": 250, "skipped_lock_held": 0,
                    "skipped_pinned": 0, "errors": 0, "last_error": None})
                human = runtime.describe_receipt(receipt)
                self.assertIn("1000 byte disk ceiling", human)
                self.assertIn("3 entries evicted (300 bytes)", human)
                self.assertIn("250 bytes retained", human)


if __name__ == "__main__":
    unittest.main()
