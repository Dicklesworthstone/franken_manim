"""Narrated bundle CLI dispatch with native/session boundaries observed by spies.

Batch planning, option validation and checkpoint I/O are real. Published bytes
are protocol fixtures, not native audio or FMTL decoding acceptance evidence.
"""
from contextlib import contextmanager, redirect_stderr
import hashlib
import io
import json
from pathlib import Path
import sys
import tempfile
import types
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "python"))
from fmn_python import batch_rendering as batch
from fmn_python import bundle_cli as cli
from fmn_python.bundle_export import BundleExportResult, BundleSoundtrackInfo


class CapabilityError(RuntimeError):
    pass


class EndScene(Exception):
    pass


class AudioCliProtocol(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.directory = self.root / "output"
        self.source = self.root / "scenes.py"
        self.source.write_text("# Source loading is observed at its boundary.\n")
        self.loads, self.constructions, self.runs = [], [], []
        self.sessions, self.exports, self.records, self.parses = [], [], [], []
        self.run_error, self.finish_error, self.silent = None, None, False
        owner = self

        class Scene:
            def __init__(self, **kwargs):
                owner.constructions.append((type(self).__name__, kwargs))

            def run(self):
                name = type(self).__name__
                owner.runs.append(name)
                if name == "Second" and owner.run_error is not None:
                    raise owner.run_error

        class First(Scene):
            pass

        class Second(Scene):
            pass

        self.scenes = {"Second": Second, "First": First}
        self.native = types.SimpleNamespace(
            Scene=Scene, EndScene=EndScene, _CapabilityError=CapabilityError,
            _portal_begin_bundle=lambda *args: None,
            _portal_begin_camera_bundle=lambda *args: None,
            _portal_begin_soundtrack_bundle=lambda *args: None,
            _portal_bundle_segment=lambda *args: None,
            _portal_finish_bundle=lambda *args: None,
            _portal_finish_soundtrack_bundle=lambda *args: None,
            _portal_cli_render_arguments=self.parse, _portal_cli_emit=self.emit,
        )

        class Session:
            def __init__(self, scene, destination, **options):
                self.scene, self.destination, self.options = scene, Path(destination), options
                self.result, self.artifact_published = None, False
                owner.sessions.append(self)

            def __enter__(self):
                return self

            def __exit__(self, kind, error, traceback):
                if error is not None:
                    return False
                if owner.finish_error is not None:
                    raise owner.finish_error
                self.destination.parent.mkdir(parents=True, exist_ok=True)
                data = ("audio CLI protocol fixture: " + type(self.scene).__name__).encode().ljust(256, b".")
                with self.destination.open("xb") as stream:
                    stream.write(data)
                self.artifact_published = True
                soundtrack = None
                if self.options.get("audio") and not owner.silent:
                    soundtrack = BundleSoundtrackInfo(
                        self.options["audio_sample_rate"], self.options["audio_channels"], 8)
                self.result = BundleExportResult(
                    self.destination, 12, 2, len(data), hashlib.sha256(data).hexdigest(),
                    self.options.get("fps") or 24, self.options.get("resolution") or (64, 36),
                    self.options.get("camera", False), soundtrack)
                return False

        self.session_type = Session
        for context in (
            patch.dict(sys.modules, {"manimlib": self.native}),
            patch.object(cli, "SceneSource", self.load),
            patch.object(cli, "resolve_render_configuration",
                         lambda native, values, width, height, fps: (None, width, height, fps)),
            patch.object(cli, "BundleExportSession", Session),
            patch.object(batch, "export_bundle", self.export),
        ):
            context.start()
            self.addCleanup(context.stop)

    @contextmanager
    def load(self, source, base, **options):
        self.loads.append((source, base, options))
        yield types.SimpleNamespace(scenes=self.scenes)

    def export(self, scene, destination, **options):
        self.exports.append((scene.__name__, dict(options)))
        instance = scene(**(options.get("scene_kwargs") or {}))
        with self.session_type(instance, destination, **options) as session:
            try:
                instance.run()
            except EndScene:
                pass
        return session.result

    def parse(self, arguments):
        self.parses.append(list(arguments))
        positionals, values, index = [], {}, 0
        flags = {"--format": "format", "--resolution": "resolution", "-r": "resolution",
                 "--fps": "fps", "--video_dir": "video_dir", "--output": "output",
                 "--file_name": "file_name", "-c": "color", "--color": "color",
                 "--config_file": "config_file"}
        while index < len(arguments):
            token = arguments[index]
            if token in flags:
                if index + 1 == len(arguments):
                    raise ValueError(token + " requires a value")
                values[flags[token]] = arguments[index + 1]
                index += 2
            elif token in ("--quiet", "-q"):
                values["quiet"] = True
                index += 1
            elif token.startswith("-"):
                raise AssertionError("unsupported option reached the native parser: " + token)
            else:
                positionals.append(token)
                index += 1
        if not positionals:
            raise ValueError("a source is required")
        if values.get("format") != "png_sequence":
            raise AssertionError("only shared picture syntax should reach this parser")
        width, height = map(int, values.get("resolution", "64x36").split("x"))
        fps = int(values.get("fps", 24))
        return positionals, values, width, height, fps, None

    def emit(self, code, identity, kind, message, robot, **fields):
        self.records.append(dict(code=code, identity=identity, kind=kind,
                                 message=message, robot=robot, **fields))
        return code

    def invoke(self, *extra, names=("First",), audio=True, robot=True):
        arguments = [str(self.source), *names, "--format", "fmtl",
                     "--video_dir", str(self.directory)]
        if audio:
            arguments.append("--bundle-audio")
        if robot:
            arguments.append("--robot")
        with redirect_stderr(io.StringIO()):
            return cli.try_bundle_cli(self.native, [*arguments, *extra])

    def assert_no_execution(self):
        self.assertEqual(self.loads, [])
        self.assertEqual(self.constructions, [])
        self.assertEqual(self.sessions, [])
        self.assertEqual(self.exports, [])
        self.assertFalse(self.directory.exists())

    def test_single_audio_camera_export_forwards_bounded_options_and_receipt(self):
        code = self.invoke("--bundle-camera", "--bundle-audio-sample-rate", "32000",
                           "--bundle-audio-channels=1", "--bundle-audio-threads", "4",
                           "--bundle-audio-max-bytes=8192", "--resolution", "128x72", "--fps=30")
        self.assertEqual(code, 0)
        self.assertEqual(self.constructions, [("First", {})])
        self.assertEqual(self.runs, ["First"])
        self.assertEqual(self.exports, [])
        options = self.sessions[0].options
        self.assertEqual({key: options[key] for key in (
            "audio", "audio_sample_rate", "audio_channels", "audio_threads", "max_audio_bytes", "camera")},
            {"audio": True, "audio_sample_rate": 32000, "audio_channels": 1,
             "audio_threads": 4, "max_audio_bytes": 8192, "camera": True})
        receipt = self.records[-1]["bundle"]
        self.assertEqual(receipt["soundtrack"], {"sample_rate": 32000, "channels": 1, "sample_frames": 8})
        self.assertEqual(receipt["fmtl_minor"], 2)
        self.assertEqual(receipt["replay_resolution"], [128, 72])
        self.assertEqual(receipt["fps"], 30)
        self.assertTrue(receipt["camera_track"])
        self.assertFalse(receipt["certified_source"])
        self.assertTrue(self.records[-1]["robot"])
        self.assertFalse(any(token.startswith("--bundle") for token in self.parses[0]))

    def test_explicit_batch_order_uses_real_audio_batch_dispatch(self):
        self.assertEqual(self.invoke("--bundle-audio-sample-rate=44100", names=("Second", "First")), 0)
        self.assertEqual(self.runs, ["Second", "First"])
        self.assertEqual([name for name, _ in self.exports], ["Second", "First"])
        for _, options in self.exports:
            self.assertIs(options["audio"], True)
            self.assertEqual(options["audio_sample_rate"], 44100)
            self.assertEqual(options["audio_channels"], 2)
            self.assertEqual(options["audio_threads"], 1)
            self.assertEqual(options["max_audio_bytes"], 64 * 1024 * 1024)
        rows = self.records[-1]["batch"]["outcomes"]
        self.assertEqual([row["name"] for row in rows], ["Second", "First"])
        self.assertTrue(all(row["result"]["fmtl_minor"] == 2 for row in rows))

    def test_write_all_sorts_names_and_binds_audio_to_checkpoint_recovery(self):
        checkpoint = self.root / "progress.json"
        arguments = ("--write_all", "--checkpoint", str(checkpoint), "--resume-key", "source-v1")
        self.run_error = ValueError("decode failed")
        self.assertEqual(self.invoke(*arguments, names=()), 5)
        first = self.records[-1]["batch"]["outcomes"][0]["result"]
        saved = json.loads(checkpoint.read_text())
        self.assertIs(saved["plan"]["options"]["bundle_audio"], True)
        self.assertEqual(self.runs, ["First", "Second"])
        self.run_error = None
        self.runs.clear()
        self.constructions.clear()
        self.exports.clear()
        self.assertEqual(self.invoke(*arguments, "--resume", names=()), 0)
        self.assertEqual(self.runs, ["Second"])
        self.assertEqual(self.constructions, [("Second", {})])
        self.assertEqual(self.records[-1]["batch"]["outcomes"][0]["result"], first)
        before = checkpoint.read_bytes()
        self.exports.clear()
        self.assertEqual(self.invoke(*arguments, "--resume", "--bundle-audio-channels=1", names=()), 2)
        self.assertEqual(self.exports, [])
        self.assertEqual(checkpoint.read_bytes(), before)

    def test_legacy_no_audio_exports_keep_the_previous_session_arguments(self):
        self.assertEqual(self.invoke(audio=False), 0)
        self.assertNotIn("audio", self.sessions[0].options)
        self.assertNotIn("soundtrack", self.records[-1]["bundle"])
        self.assertNotIn("fmtl_minor", self.records[-1]["bundle"])
        self.directory = self.root / "legacy-batch"
        self.assertEqual(self.invoke(names=("First", "Second"), audio=False), 0)
        self.assertTrue(all("audio" not in options for _, options in self.exports))

    def test_malformed_numeric_options_fail_before_parsing_or_importing_source(self):
        cases = []
        for flag, values in {
            "--bundle-audio-sample-rate": ("0", "-1", "384001", "1.5", "True", "４８０００", "9" * 10000),
            "--bundle-audio-channels": ("0", "3", "+1"),
            "--bundle-audio-threads": ("0", "65"),
            "--bundle-audio-max-bytes": ("0", "7", "268435457"),
        }.items():
            cases.extend((flag, value) for value in values)
        for flag, value in cases:
            for extra in ((flag, value), (flag + "=" + value,)):
                with self.subTest(flag=flag, value=value[:30], equals=len(extra) == 1):
                    self.assertEqual(self.invoke(*extra), 2)
                    self.assertEqual(self.records[-1]["phase"], "options")
                    self.assertTrue(self.records[-1]["robot"])
        self.assertEqual(self.parses, [])
        self.assert_no_execution()

    def test_duplicate_flags_and_switch_values_are_usage_errors(self):
        cases = [("--bundle-audio",), ("--bundle-audio=true",), ("--bundle-audio=false",)]
        for flag in cli._AUDIO_VALUE_FLAGS:
            cases.append((flag, "1", flag + "=1"))
        for arguments in cases:
            with self.subTest(arguments=arguments):
                self.assertEqual(self.invoke(*arguments), 2)
                self.assertFalse(self.records[-1]["exported"])
        self.assert_no_execution()

    def test_missing_numeric_value_does_not_consume_robot_or_help_switch(self):
        for flag in cli._AUDIO_VALUE_FLAGS:
            with self.subTest(flag=flag):
                self.assertEqual(self.invoke(flag, "--robot", robot=False), 2)
                self.assertTrue(self.records[-1]["robot"])
                self.assertEqual(self.invoke(flag), 2)
        self.assert_no_execution()
        self.assertEqual(cli.try_bundle_cli(self.native, ["--bundle-audio", "--bundle-audio-threads", "--help"]), 0)
        self.assertEqual(self.records[-1]["kind"], "help")

    def test_audio_settings_require_explicit_audio_and_exact_fmtl_format(self):
        for flag in cli._AUDIO_VALUE_FLAGS:
            with self.subTest(flag=flag):
                self.assertEqual(self.invoke(flag, "1", audio=False), 2)
        for arguments in (["--bundle-audio"], ["--bundle-audio", "--format", "mp4"],
                          ["--bundle-audio-sample-rate", "48000"],
                          ["--bundle-audio", "--format", "fmtl", "--format=fmtl"]):
            with self.subTest(arguments=arguments):
                self.assertEqual(cli.try_bundle_cli(self.native, ["--robot", *arguments]), 2)
                self.assertTrue(self.records[-1]["robot"])
        self.assert_no_execution()

    def test_option_values_that_spell_audio_flags_do_not_activate_bundle_mode(self):
        for value in ("--bundle-audio", "--bundle-audio-sample-rate", "--bundle-audio=true", "fmtl"):
            for args in (["--file_name", value], ["--file_name=" + value]):
                with self.subTest(args=args):
                    self.assertIsNone(cli.try_bundle_cli(self.native, [str(self.source), *args]))
        self.assertEqual(self.records, [])
        self.assert_no_execution()
        self.assertEqual(self.invoke("--file_name", "--bundle-audio", audio=False), 0)
        self.assertNotIn("audio", self.sessions[0].options)
        self.assertEqual(self.sessions[0].destination.name, "--bundle-audio.fmtl")

    def test_missing_native_audio_capability_is_reported_before_source_loading(self):
        for name in ("_portal_begin_soundtrack_bundle", "_portal_finish_soundtrack_bundle"):
            for camera in (False, True):
                with self.subTest(name=name, camera=camera), patch.object(self.native, name, None):
                    self.assertEqual(self.invoke(*(["--bundle-camera"] if camera else [])), 4)
                    self.assertEqual(self.records[-1]["identity"], "capability")
        self.assert_no_execution()

    def test_help_needs_no_native_audio_support_and_explains_refusals(self):
        with patch.object(self.native, "_portal_begin_soundtrack_bundle", None):
            self.assertEqual(cli.try_bundle_cli(self.native, ["--bundle-audio", "--robot", "--help"]), 0)
        text = self.records[-1]["help"]
        for flag in cli._AUDIO_VALUE_FLAGS:
            self.assertIn(flag, text)
        self.assertIn("Compressed", text)
        self.assertIn("picture-only", text)
        self.assert_no_execution()

    def test_finish_error_never_reports_success_or_published_artifact(self):
        self.finish_error = ValueError("native WAV decode failed")
        self.assertEqual(self.invoke(), 6)
        self.assertEqual(self.records[-1]["phase"], "finish")
        self.assertFalse(self.records[-1]["exported"])
        self.assertFalse(self.records[-1]["artifact_published"])
        self.assertIsNone(self.sessions[0].result)
        self.assertFalse(self.sessions[0].destination.exists())

    def test_keep_going_and_interrupts_retain_completed_soundtrack_receipts(self):
        self.run_error = ValueError("scene failure")
        self.assertEqual(self.invoke("--keep-going", names=("Second", "First")), 5)
        rows = self.records[-1]["batch"]["outcomes"]
        self.assertEqual([row["status"] for row in rows], ["failed", "succeeded"])
        self.assertIn("soundtrack", rows[1]["result"])
        self.directory = self.root / "interrupted"
        self.run_error = KeyboardInterrupt()
        self.assertEqual(self.invoke("--keep-going", names=("First", "Second")), 130)
        self.assertEqual(self.records[-1]["kind"], "render-batch-interrupted")
        rows = self.records[-1]["batch"]["outcomes"]
        self.assertEqual([row["status"] for row in rows], ["succeeded", "cancelled"])
        self.assertIn("soundtrack", rows[0]["result"])

    def test_quiet_suppresses_human_success_but_never_robot_receipt(self):
        self.assertEqual(self.invoke("--quiet", robot=False), 0)
        self.assertEqual(self.records, [])
        self.directory = self.root / "robot"
        self.assertEqual(self.invoke("--quiet"), 0)
        self.assertEqual(len(self.records), 1)
        self.assertIn("soundtrack", self.records[0]["bundle"])

    def test_no_cue_scenes_keep_picture_only_receipts_through_audio_cli(self):
        self.silent = True
        self.assertEqual(self.invoke("--bundle-camera"), 0)
        receipt = self.records[-1]["bundle"]
        self.assertNotIn("soundtrack", receipt)
        self.assertEqual(receipt["fmtl_minor"], 1)
        self.assertTrue(receipt["camera_track"])
        self.assertIs(self.sessions[0].options["audio"], True)

    def test_later_batch_collision_prevents_all_constructors(self):
        self.directory.mkdir()
        occupied = self.directory / "Second.fmtl"
        occupied.write_bytes(b"another writer")
        self.assertEqual(self.invoke(names=("First", "Second")), 6)
        self.assertEqual(self.constructions, [])
        self.assertEqual(self.exports, [])
        self.assertEqual(occupied.read_bytes(), b"another writer")


if __name__ == "__main__":
    unittest.main()
