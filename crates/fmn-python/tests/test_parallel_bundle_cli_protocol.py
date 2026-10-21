"""FMTL CLI routing over real process orchestration and checkpoint publication.

The full bundle CLI, shared option consumers, production token/path/receipt
functions and parallel coordinator run unchanged. Native common-option parsing,
source discovery inventory and native capture are explicit boundary fixtures.
No installed console entry point or native renderer is claimed by this suite.
"""
from __future__ import annotations

import ast
from contextlib import contextmanager, nullcontext, redirect_stdout, redirect_stderr
import hashlib
import io
import json
import os
from pathlib import Path
import sys
import tempfile
import types
from typing import Any
import unittest
from unittest.mock import Mock, patch

from test_parallel_bundle_protocol import (
    PYTHON, Outcome, Result, host_fixture, load_module, publish_bundle_job,
)
from fmn_python.checkpoint_cli import CHECKPOINT_VALUES
from fmn_python.parallel_cli_options import PARALLEL_VALUES


def source_functions(filename, names, namespace):
    tree = ast.parse((PYTHON / "fmn_python" / filename).read_text())
    selected = [node for node in tree.body if isinstance(node, ast.FunctionDef) and node.name in names]
    if len(selected) != len(names):
        raise AssertionError("production console helper inventory changed")
    exec(compile(ast.Module(body=selected, type_ignores=[]), filename, "exec"), namespace)


class ParallelBundleCliTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.source = self.root / "lessons.py"
        self.source.write_text("# entry bytes belong to the explicit source fixture\n")
        self.output = self.root / "out"
        self.checkpoint = self.root / "progress.json"
        self.behavior()
        self.context = host_fixture()
        self.h = self.context.__enter__()
        self.addCleanup(self.context.__exit__, None, None, None)
        self.h.coordinator._render_source_job = publish_bundle_job
        self.h.batch._error_notes = lambda error: tuple(getattr(error, "__notes__", ()))
        self.h.batch.render_scenes = Mock(return_value=Result(()))
        self.h.batch._error_fields = lambda error: (type(error).__name__, str(error))
        # The actual emitter consumes the production count interface.
        previous_counts = getattr(Result, "counts", None)
        Result.counts = property(lambda self: {status: sum(row.status == status for row in self.outcomes)
                                              for status in ("succeeded", "failed", "cancelled", "not_run")})
        self.addCleanup(lambda: setattr(Result, "counts", previous_counts) if previous_counts is not None
                        else delattr(Result, "counts"))
        console = types.ModuleType("fmn_python.console_rendering")
        console.__dict__.update(Path=Path, os=os, _error_fields=self.h.batch._error_fields)
        tree = ast.parse((PYTHON / "fmn_python/batch_cli.py").read_text())
        flags = next(node for node in tree.body if isinstance(node, ast.Assign)
                     and any(isinstance(target, ast.Name) and target.id == "_VALUE_FLAGS" for target in node.targets))
        exec(compile(ast.Module(body=[flags], type_ignores=[]), "batch_cli.py", "exec"), console.__dict__)
        console._VALUE_FLAGS |= CHECKPOINT_VALUES | PARALLEL_VALUES
        console._MAX_SELECTED_SCENES = 1024
        source_functions("console_rendering.py", {"_tokens", "_output_paths", "_single_destination", "_message"},
                         console.__dict__)
        emitter = types.ModuleType("fmn_python.batch_cli")
        emitter.__dict__.update(Any=Any, BatchRenderResult=Result, Path=Path)
        source_functions("batch_cli.py", {"_emit_result"}, emitter.__dict__)
        loading = types.ModuleType("fmn_python.scene_loading")
        loading.SceneSource = Mock(side_effect=AssertionError("source imported in coordinator"))
        loading.resolve_render_configuration = lambda native, values, width, height, fps: (
            self.configuration, width, height, fps)
        self.configuration = None
        discovery = types.ModuleType("fmn_python.parallel_discovery")
        # Keep the actual bounded entry digest used by the coordinator.
        from fmn_python.parallel_discovery import _entry_digest
        discovery._entry_digest = _entry_digest
        discovery.discovery_source = self.discover
        self.discoveries, self.records, self.parses = [], [], []
        self.names = ("Beta", "Alpha")
        self.discovery_error = None
        self.discovery_digest = None
        self.process_calls = []
        parallel = types.ModuleType("fmn_python.parallel_batch")
        def render_source_scenes(*args, **kwargs):
            self.process_calls.append((args, kwargs))
            return self.h.coordinator.render_source_scenes(*args, **kwargs)
        parallel.render_source_scenes = render_source_scenes
        replacements = {mod.__name__: mod for mod in (console, emitter, loading, discovery, parallel)}
        # Do not restore the whole sys.modules mapping: multiprocessing imports lazily.
        absent = object()
        before = {key: sys.modules.get(key, absent) for key in replacements}
        sys.modules.update(replacements)
        def restore():
            for key, value in before.items():
                if value is absent:
                    sys.modules.pop(key, None)
                else:
                    sys.modules[key] = value
        self.addCleanup(restore)
        self.cli = load_module("fmn_python._parallel_bundle_cli_test", "bundle_cli.py")
        self.addCleanup(sys.modules.pop, "fmn_python._parallel_bundle_cli_test", None)
        self.loading = loading
        self.h.native._portal_cli_render_arguments = self.parse
        self.h.native._portal_cli_emit = self.emit

    def behavior(self, **modes):
        (self.root / "behavior.json").write_text(json.dumps(modes))

    @contextmanager
    def discover(self, source, **options):
        self.discoveries.append((source, options))
        if self.discovery_error:
            raise self.discovery_error
        yield types.SimpleNamespace(scenes=self.names,
            source_digest=self.discovery_digest or hashlib.sha256(self.source.read_bytes()).hexdigest())

    def parse(self, arguments):
        self.parses.append(list(arguments))
        names, options, index = [], {}, 0
        flags = {"--format": "format", "--video_dir": "video_dir", "--output": "output",
                 "--file_name": "file_name", "--fps": "fps", "--resolution": "resolution",
                 "-r": "resolution", "--config_file": "config_file"}
        while index < len(arguments):
            token = arguments[index]
            if token in flags:
                if index + 1 >= len(arguments):
                    raise ValueError(token + " requires a value")
                options[flags[token]] = arguments[index + 1]
                index += 2
            elif token in {"--quiet", "-q"}:
                options["quiet"] = True
                index += 1
            elif token.startswith("-"):
                raise AssertionError("bundle-specific flags leaked to common parser: " + token)
            else:
                names.append(token)
                index += 1
        if not names:
            raise ValueError("source required")
        self.assertEqual(options["format"], "png_sequence")
        width, height = map(int, options.get("resolution", "64x36").split("x"))
        return names, options, width, height, int(options.get("fps", "24")), None

    def emit(self, code, identity, kind, message, robot, **fields):
        self.records.append(dict(code=code, identity=identity, kind=kind, message=message, robot=robot, **fields))
        return code

    def invoke(self, *extra, names=("Alpha", "Beta"), workers=True, robot=True):
        args = (["--robot"] if robot else []) + [str(self.source), *names, "--format", "fmtl",
                "--video_dir", str(self.output)] + (["--workers=2"] if workers else []) + list(extra)
        self.stdout, self.stderr = io.StringIO(), io.StringIO()
        with redirect_stdout(self.stdout), redirect_stderr(self.stderr):
            return self.cli.try_bundle_cli(self.h.native, args)

    def test_parallel_camera_audio_options_discovery_and_typed_receipts(self):
        self.configuration = {"values": {}, "overrides": []}
        self.assertEqual(self.invoke("--bundle-camera", "--bundle-audio", "--bundle-audio-channels=1",
                                     "--bundle-audio-sample-rate", "22050", "--job-timeout=12",
                                     "--resolution=128x72", "--fps", "30"), 0)
        self.loading.SceneSource.assert_not_called()
        self.h.batch.render_scenes.assert_not_called()
        source, options = self.discoveries[0]
        self.assertEqual(source, str(self.source))
        self.assertEqual(options, {"working_directory": Path.cwd(), "job_timeout": 12,
                                   "configuration": self.configuration})
        args, options = self.process_calls[0]
        self.assertEqual(args, (str(self.source), self.output))
        self.assertEqual(options["scene_names"], ["Alpha", "Beta"])
        self.assertEqual(options["_expected_source_digest"], hashlib.sha256(self.source.read_bytes()).hexdigest())
        self.assertEqual(options["_working_directory"], Path.cwd())
        self.assertEqual(options["_configuration"], self.configuration)
        self.assertNotIn("threads", options)
        self.assertTrue(self.records[-1]["batch"]["ok"])
        self.assertEqual(self.records[-1]["batch"]["execution"], "spawn")
        for row in self.records[-1]["batch"]["outcomes"]:
            self.assertEqual(row["result"]["soundtrack"]["sample_rate"], 22050)
            self.assertEqual(row["result"]["soundtrack"]["channels"], 1)
            self.assertTrue(row["result"]["camera_track"])
            self.assertEqual(row["result"]["replay_resolution"], [128, 72])
        self.assertEqual(self.stdout.getvalue(), "")
        self.assertIn("succeeded", self.stderr.getvalue())

    def test_write_all_and_explicit_order_are_preserved(self):
        self.assertEqual(self.invoke("--write_all", names=()), 0)
        self.assertEqual(self.process_calls[0][1]["scene_names"], ["Alpha", "Beta"])
        self.output = self.root / "ordered"
        self.assertEqual(self.invoke(names=("Beta", "Alpha")), 0)
        self.assertEqual(self.process_calls[1][1]["scene_names"], ["Beta", "Alpha"])

    def test_parallel_checkpoint_resume_reuses_completed_without_native_import_in_parent(self):
        flags = ("--checkpoint", str(self.checkpoint), "--resume-key", "assets-v1", "--bundle-audio", "--keep-going")
        self.behavior(Beta="fail")
        self.assertEqual(self.invoke(*flags), 5)
        self.behavior()
        self.assertEqual(self.invoke(*flags, "--resume", "--workers=1", workers=False), 0)
        self.assertEqual(self.records[-1]["batch"]["reused"], ["Alpha"])
        self.assertEqual([len((self.root / (n + ".attempts")).read_text().splitlines())
                          for n in ("Alpha", "Beta")], [1, 2])
        saved = json.loads(self.checkpoint.read_text())
        self.assertEqual(saved["plan"]["constructor_inputs"], "source-file-v1")
        self.assertTrue(saved["plan"]["options"]["bundle_audio"])
        self.loading.SceneSource.assert_not_called()

    def test_invalid_parallel_options_refuse_before_discovery(self):
        for flags in (("--workers", "0"), ("--workers=65",), ("--workers", "no"),
                      ("--workers=2", "--workers=3"), ("--workers",),
                      ("--job-timeout=2",), ("--workers=2", "--job-timeout=nan"),
                      ("--workers=2", "--job-timeout=0"),
                      ("--workers=2", "--job-timeout=2", "--job-timeout=3")):
            with self.subTest(flags=flags):
                self.assertEqual(self.invoke(*flags, workers=False), 2)
                self.assertTrue(self.records[-1]["robot"])
                self.assertEqual(self.records[-1]["phase"], "options")
        self.assertEqual(self.discoveries, [])
        self.assertEqual(self.process_calls, [])

    def test_worker_mode_requires_batch_selection_and_rejects_exact_filename(self):
        self.assertEqual(self.invoke(names=("Alpha",)), 2)
        self.assertEqual(self.invoke("--file_name", "renamed"), 2)
        self.assertEqual(self.invoke("--output", str(self.root / "single.fmtl")), 2)
        self.assertEqual(self.discoveries, [])

    def test_unsupported_capture_modes_and_missing_capability_refuse_before_discovery(self):
        for flags in (("--threads", "1"), ("-n", "1,2"), ("--reproducible",),
                      ("--subdivide",), ("--save-last-frame",)):
            with self.subTest(flags=flags):
                self.assertEqual(self.invoke(*flags), 4)
        self.h.native._portal_finish_soundtrack_bundle = None
        self.assertEqual(self.invoke("--bundle-audio"), 4)
        self.assertEqual(self.discoveries, [])
        self.assertEqual(self.process_calls, [])

    def test_selected_names_are_checked_before_render_admission(self):
        self.assertEqual(self.invoke(names=("Alpha", "Missing")), 2)
        self.names = ()
        self.assertEqual(self.invoke("--write_all", names=()), 2)
        self.assertEqual(self.process_calls, [])

    def test_discovery_failure_and_interrupt_keep_process_exit_identity(self):
        self.discovery_error = RuntimeError("discovery failed")
        self.assertEqual(self.invoke(), 5)
        self.assertEqual(self.records[-1]["phase"], "load")
        self.discovery_error = KeyboardInterrupt("discovery interrupted")
        self.assertEqual(self.invoke(), 130)
        self.assertEqual(self.process_calls, [])

    def test_changed_entry_after_discovery_is_refused_before_worker_launch(self):
        self.discovery_digest = "a" * 64
        with patch.object(self.h.coordinator, "run_process_jobs", side_effect=AssertionError("no launch")):
            self.assertEqual(self.invoke(), 5)
        self.assertIn("changed after discovery", self.records[-1]["message"])
        self.assertFalse(self.output.exists())

    def test_process_cancellation_reports_completed_receipts_and_stops_later_jobs(self):
        self.behavior(Beta="cancel")
        self.names = ("Alpha", "Beta", "Later")
        self.assertEqual(self.invoke("--workers=1", "--keep-going", workers=False,
                                     names=self.names), 130)
        self.assertEqual(self.records[-1]["kind"], "render-batch-interrupted")
        self.assertEqual([row["status"] for row in self.records[-1]["batch"]["outcomes"]],
                         ["succeeded", "cancelled", "not_run"])
        self.assertTrue((self.output / "Alpha.fmtl").exists())

    def test_collisions_keep_existing_output_and_return_render_failure(self):
        self.output.mkdir()
        (self.output / "Alpha.fmtl").write_bytes(b"keep")
        self.assertEqual(self.invoke(), 6)
        self.assertEqual((self.output / "Alpha.fmtl").read_bytes(), b"keep")
        self.assertFalse((self.root / "Beta.attempts").exists())

    def test_quiet_hides_progress_but_preserves_robot_receipt(self):
        self.assertEqual(self.invoke("--quiet"), 0)
        self.assertEqual(self.stderr.getvalue(), "")
        self.assertTrue(self.records[-1]["robot"])
        self.assertIsNone(self.process_calls[0][1]["on_result"])

    def test_sequential_batch_dispatch_is_unchanged_without_workers(self):
        classes = {name: type(name, (), {}) for name in self.names}
        self.loading.SceneSource.side_effect = None
        self.loading.SceneSource.return_value = nullcontext(types.SimpleNamespace(scenes=classes))
        self.assertEqual(self.invoke(workers=False), 5)  # explicit fixture returns an empty batch
        self.loading.SceneSource.assert_called_once()
        args, options = self.h.batch.render_scenes.call_args
        self.assertEqual(args, ({"Alpha": classes["Alpha"], "Beta": classes["Beta"]}, self.output))
        self.assertNotIn("workers", options)
        self.assertEqual(self.discoveries, [])
        self.assertEqual(self.process_calls, [])

    def test_help_and_non_bundle_commands_do_not_launch_any_worker(self):
        self.assertEqual(self.invoke("--help"), 0)
        self.assertIn("--workers", self.records[-1]["help"])
        self.assertIn("--job-timeout", self.records[-1]["help"])
        self.assertIsNone(self.cli.try_bundle_cli(self.h.native, [str(self.source), "--format", "png", "--workers", "2"]))
        self.assertIsNone(self.cli.try_bundle_cli(self.h.native, [str(self.source), "--output", "fmtl"]))
        self.assertEqual(self.discoveries, [])

    def test_parallel_option_text_inside_another_value_is_not_reinterpreted(self):
        self.assertEqual(self.invoke("--config_file", "--workers=63"), 0)
        self.assertEqual(self.process_calls[0][1]["workers"], 2)
        self.assertIn("--workers=63", self.parses[0])


if __name__ == "__main__":
    unittest.main()
