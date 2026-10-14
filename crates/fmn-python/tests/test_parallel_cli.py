"""Lexer and real console-dispatch tests against a reduced native table.

These test the Python boundary, not native rendering. Process behavior is
covered independently by test_process_batch.py with real spawned interpreters.
"""
import ast
import contextlib
from functools import partial
import hashlib
import os
from pathlib import Path
import sys
import tempfile
import types
import unittest
from unittest.mock import patch

PYTHON = Path(__file__).resolve().parents[1] / "python"
sys.path.insert(0, str(PYTHON))
from fmn_python.parallel_cli_options import PARALLEL_VALUES, take_parallel_options


class ParallelOptionsTests(unittest.TestCase):
    def test_space_and_inline_forms(self):
        self.assertEqual(take_parallel_options(["--workers", "4", "--job-timeout=1.5", "--robot"]),
                         (["--robot"], {"workers": 4, "job_timeout": 1.5}))
        self.assertEqual(take_parallel_options(["--workers=1"]), ([], {"workers": 1}))

    def test_preserves_legacy_values_that_look_like_parallel_flags(self):
        args = ["--video_dir", "--workers", "--workers=3", "--ffmpeg_bin", "--job-timeout=2"]
        self.assertEqual(take_parallel_options(args, {"--video_dir", "--ffmpeg_bin"}),
                         (["--video_dir", "--workers", "--ffmpeg_bin", "--job-timeout=2"], {"workers": 3}))

    def test_rejects_bad_repeated_and_missing_values(self):
        invalid = [["--workers"], ["--workers=0"], ["--workers=65"], ["--workers=-1"],
                   ["--workers=1.0"], ["--workers=true"], ["--workers=2", "--workers", "3"],
                   ["--job-timeout=1"], ["--workers=2", "--job-timeout=nan"],
                   ["--workers=2", "--job-timeout=inf"], ["--workers=2", "--job-timeout=0"],
                   ["--workers=2", "--job-timeout=1", "--job-timeout=2"]]
        for args in invalid:
            with self.subTest(args=args), self.assertRaises(ValueError):
                take_parallel_options(args)


class ConsoleParallelDispatchTests(unittest.TestCase):
    def setUp(self):
        self.cwd = Path.cwd()
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.source = self.root / "scenes.py"
        self.source.write_text("# stable source\n", encoding="utf-8")
        self.loads, self.calls, self.parsed, self.emitted = [], [], [], []
        self.chdir_on_load = False
        case = self

        class Source:
            def __init__(self, path, scene_type):
                self._source_bytes = {path: path.read_bytes()}
                self.scenes = {"Second": type("Second", (), {}), "First": type("First", (), {})}
            def __enter__(self):
                case.loads.append(self)
                if case.chdir_on_load:
                    os.chdir(case.root)
                return self
            def __exit__(self, *args):
                return False

        class Result:
            def as_dict(self):
                return {"execution": "contract-test"}
        self.Result = Result

        def render(mode):
            def invoke(*args, **kwargs):
                case.calls.append((mode, args, kwargs))
                return Result()
            return invoke

        def parse(args):
            case.parsed.append(args)
            options = {"format": "png", "video_dir": str(case.root / "out"),
                       "reproducible": "--reproducible" in args}
            for flag, name in (("--format", "format"), ("--vcodec", "vcodec"),
                               ("--pix_fmt", "pix_fmt"), ("--ffmpeg_bin", "ffmpeg_bin")):
                if flag in args:
                    options[name] = args[args.index(flag) + 1]
            threads = int(args[args.index("--threads") + 1]) if "--threads" in args else 8
            return [args[0]], options, 64, 48, 30, threads

        def emit(code, identity, kind, message, robot, **kwargs):
            case.emitted.append((code, identity, kind, message, robot, kwargs))
            return code

        def switch(args, name):
            return [value for value in args if value != name], name in args

        def recovery(args, flags):
            args = list(args)
            if "--checkpoint" not in args:
                return args, {}
            index = args.index("--checkpoint")
            return args[:index] + args[index + 2:], {"checkpoint": args[index + 1]}

        self.native = types.SimpleNamespace(Scene=object, _portal_cli_render_arguments=parse,
                                            _portal_cli_emit=emit)
        values = {"--threads", "--format", "--fps", "--resolution", "--video_dir", "--vcodec",
                  "--pix_fmt", "--ffmpeg_bin", "--checkpoint", "-n"} | PARALLEL_VALUES
        self.g = {
            "__name__": "fmn_python._console_contract_test", "__package__": "fmn_python",
            "Any": object, "Path": Path, "os": os, "sys": sys, "contextlib": contextlib,
            "_partial_call": partial, "hashlib": hashlib,
            "_VALUE_FLAGS": values,
            "_CONTROL_FLAGS": {"--version", "--list-scenes", "--construct-only", "--audit-parity"},
            "_SELECTION_FLAGS": {"--robot", "--write_all", "-a", "--keep-going"},
            "_MAX_SELECTED_SCENES": 1024, "take_parallel_options": take_parallel_options,
            "take_video_options": lambda args, flags: (args, {}),
            "take_last_frame_option": lambda args, flags: switch(args, "--save-last-frame"),
            "take_subdivision_option": lambda args, flags: switch(args, "--subdivide"),
            "take_checkpoint_options": recovery,
            "take_playback_options": lambda args: (args, None, False),
            "select_still_format": lambda options, args, still: options,
            "_output_overrides": lambda options: {key: value for key, value in options.items()
                                                  if key in {"vcodec", "pix_fmt", "ffmpeg_bin"}},
            "_positive_integer": lambda value, name: value,
            "_name_key": lambda name: name.casefold(),
            "validate_batch_mode": lambda *args: None,
            "_error_fields": lambda error: (type(error).__name__, str(error)),
            "_error_notes": lambda error: (), "_partial_output": lambda *args: {},
            "recording_effects": lambda enabled: contextlib.nullcontext(),
            "SceneSource": Source, "render_scenes": render("sequential"),
            "BatchRenderResult": Result, "BatchRenderError": type("BatchError", (Exception,), {}),
            "_emit_result": lambda native, report, robot, source, destination, **kwargs:
                emit(0, "success", "render-batch", "done", robot, batch=report.as_dict()),
        }
        tree = ast.parse((PYTHON / "fmn_python/console_rendering.py").read_text())
        tree.body = [node for node in tree.body if isinstance(node, ast.FunctionDef)
                     and node.name in {"_tokens", "_message", "try_render_cli"}]
        exec(compile(tree, "console_rendering.py", "exec"), self.g)
        self.parallel = types.ModuleType("fmn_python.parallel_batch")
        self.parallel.render_source_scenes = render("parallel")
        self.modules = patch.dict(sys.modules, {"fmn_python.parallel_batch": self.parallel})
        self.modules.start()

    def tearDown(self):
        self.modules.stop()
        os.chdir(self.cwd)
        self.temp.cleanup()

    def run_cli(self, *args):
        return self.g["try_render_cli"](self.native, [str(self.source), *args])

    def test_write_all_selects_parallel_and_preserves_native_output_options(self):
        code = self.run_cli("--write_all", "--workers", "2", "--robot", "--keep-going",
                            "--format", "mp4", "--vcodec", "libx264", "--pix_fmt", "rgba")
        self.assertEqual(code, 0)
        mode, args, options = self.calls[0]
        self.assertEqual(mode, "parallel")
        self.assertEqual(options["scene_names"], ["First", "Second"])
        self.assertEqual(options["workers"], 2)
        self.assertEqual(options["threads"], 1)
        self.assertEqual(options["_output_options"], {"vcodec": "libx264", "pix_fmt": "rgba"})
        self.assertTrue(options["continue_on_error"])
        self.assertNotIn("--workers", self.parsed[0])
        self.assertEqual(args, (self.source, self.root / "out"))

    def test_named_order_explicit_threads_and_timeout_survive_dispatch(self):
        self.assertEqual(self.run_cli("Second", "First", "--workers=3", "--threads", "4",
                                     "--job-timeout=20"), 0)
        options = self.calls[0][2]
        self.assertEqual(options["scene_names"], ["Second", "First"])
        self.assertEqual((options["threads"], options["job_timeout"]), (4, 20.0))

    def test_default_sequential_path_is_unchanged(self):
        self.assertEqual(self.run_cli("--write_all"), 0)
        mode, args, options = self.calls[0]
        self.assertEqual(mode, "sequential")
        self.assertEqual(list(args[0]), ["First", "Second"])
        self.assertEqual(options["threads"], 8)
        self.assertNotIn("workers", options)

    def test_incompatible_modes_refuse_before_source_load(self):
        for flags in (("--reproducible",), ("--subdivide",), ("--save-last-frame",),
                      ("--checkpoint", "journal.json")):
            with self.subTest(flags=flags):
                self.assertEqual(self.run_cli("--write_all", "--workers=2", *flags), 4)
                self.assertFalse(self.loads)
                self.assertFalse(self.calls)

    def test_single_scene_and_invalid_workers_refuse_before_source_load(self):
        self.assertEqual(self.run_cli("First", "--workers=2"), 2)
        self.assertEqual(self.run_cli("--write_all", "--workers=65"), 2)
        self.assertFalse(self.loads)

    def test_discovery_cannot_change_worker_starting_directory_or_source_identity(self):
        self.chdir_on_load = True
        self.assertEqual(self.run_cli("-a", "--workers=2"), 0)
        options = self.calls[0][2]
        self.assertEqual(options["_working_directory"], self.cwd)
        self.assertEqual(options["_expected_source_digest"], hashlib.sha256(b"# stable source\n").hexdigest())


if __name__ == "__main__":
    unittest.main()
