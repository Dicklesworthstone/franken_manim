"""Job-file media dispatch through full production CLI and process owners.

The real source-job reader, media option helpers, worker dispatch, spawn/JSON
transport and checkpoint I/O execute. Native parsing/capture, scene loading and
paired/subdivided render owners are explicit fixtures. NOT-MEDIA artifacts are
not evidence of native codec, pixel, PCM or installed-console acceptance.
"""
from contextlib import contextmanager, nullcontext, redirect_stdout, redirect_stderr
from dataclasses import dataclass
import hashlib
import io
import json
import os
from pathlib import Path
import sys
import types
import unittest
from unittest.mock import Mock, patch

import test_parallel_bundle_cli_protocol as prior
from test_parallel_bundle_protocol import host_fixture, Outcome, Result, load_module
from test_source_job_variants_protocol import SOURCE, RenderJobFixture


@dataclass(frozen=True)
class MediaReceipt:
    data: dict

    def __getattr__(self, key):
        value = self.data[key]
        return Path(value) if key == "destination" else value

    def as_dict(self):
        return dict(self.data)


def restore_media(row):
    data = row["result"]
    if data["destination"] != row["destination"] or data["certified"] is not False:
        raise ValueError("invalid media fixture receipt")
    return Outcome(row["name"], Path(row["destination"]), "succeeded", MediaReceipt(data))


def execute_media_variant(job):
    with host_fixture() as host:
        host.batch.RenderJob = RenderJobFixture

        @contextmanager
        def load(source, base, **options):
            source = Path(source)
            raw = source.read_bytes()
            namespace = {"__name__": "media_fixture", "__file__": str(source)}
            exec(compile(raw, str(source), "exec"), namespace)
            scenes = {name: cls for name, cls in namespace.items()
                      if isinstance(cls, type) and issubclass(cls, base)
                      and cls.__module__ == namespace["__name__"]}
            yield types.SimpleNamespace(scenes=scenes, _source_bytes={source: raw})

        def render(entries, directory, **options):
            if isinstance(entries, dict):
                entries = [RenderJobFixture(name, cls) for name, cls in entries.items()]
            entry, = entries
            scene = entry.scene(**(entry.scene_kwargs or {}))
            scene.run()
            format = options["format"]
            path = (Path(directory) / entry.name / "frames" if format == "png_sequence"
                    else Path(directory) / (entry.name + "." + format))
            raw = b"NOT-MEDIA:" + json.dumps({"values": scene.values, "options": options}, sort_keys=True).encode()
            if format == "png_sequence":
                path.mkdir(parents=True, exist_ok=False)
                (path / "000000.png").write_bytes(raw)
            else:
                path.parent.mkdir(parents=True, exist_ok=True)
                with path.open("xb") as stream:
                    stream.write(raw)
            receipt = MediaReceipt(dict(destination=str(path), format=format, certified=False,
                bytes=len(raw), digest=hashlib.sha256(raw).hexdigest(), engine="NOT-MEDIA-fixture",
                frame_count=None if format == "wav" else 1,
                sample_frames=8 if format == "wav" else None, fps=options["fps"] or 24,
                threads=options["threads"], resolution=options["resolution"] or [64, 36], seed=0))
            return Result((Outcome(entry.name, path, "succeeded", receipt),))

        host.batch.render_scenes = render
        loader = types.ModuleType("fmn_python.scene_loading")
        loader.SceneSource = load
        effects = types.ModuleType("fmn_python.effect_audit")
        effects.recording = lambda enabled: nullcontext()
        with patch.dict(sys.modules, {loader.__name__: loader, effects.__name__: effects}):
            return host.coordinator._render_source_job(job)


def module(name, **members):
    value = types.ModuleType("fmn_python." + name)
    vars(value).update(members)
    return value


def take_switch(arguments, flags, name):
    forwarded, selected, index = [], False, 0
    while index < len(arguments):
        token = arguments[index]
        if token == name:
            if selected:
                raise ValueError(name + " repeated")
            selected = True
        else:
            forwarded.append(token)
            if token in flags and index + 1 < len(arguments):
                index += 1
                forwarded.append(arguments[index])
        index += 1
    return forwarded, selected


class MediaJobFileTests(unittest.TestCase):
    emit = prior.ParallelBundleCliTests.emit
    discover = prior.ParallelBundleCliTests.discover
    behavior = prior.ParallelBundleCliTests.behavior

    def setUp(self):
        prior.ParallelBundleCliTests.setUp(self)
        self.source.write_text(SOURCE)
        self.names = ("Lesson", "Other")
        self.h.coordinator._render_source_job = execute_media_variant
        self.h.coordinator._FORMATS.update({"gif", "y4m", "svg", "mp4", "mov"})
        self.h.batch._restored_outcome = restore_media
        self.h.coordinator._restored_outcome = restore_media
        self.h.batch.RenderJob = RenderJobFixture
        self.job_file = self.root / "jobs.json"
        self.jobs = [{"name": "seed7", "scene": "Lesson", "scene_kwargs": {"value": 7}},
                     {"name": "seed11", "scene": "Lesson", "scene_kwargs": {"value": 11}}]
        self.write_jobs()
        self.effects = []
        emitter = sys.modules["fmn_python.batch_cli"]
        emitter._BATCH_HELP = "existing batch help"
        emitter._VALUE_FLAGS = sys.modules["fmn_python.console_rendering"]._VALUE_FLAGS
        rendering = sys.modules["fmn_python.rendering"]
        rendering._apply_output_options = Mock(side_effect=AssertionError("single-scene path entered"))
        rendering._runtime_identities = lambda native: {}
        selection = load_module("fmn_python._media_selection_test", "render_selection.py")
        selection.__name__ = "fmn_python.render_selection"
        pairs = module("paired_output", PAIRED_HELP="paired help", PairedRenderResult=type("Pair", (), {}),
            PairedRenderSession=type("PairSession", (), {}), companion_png_path=lambda p, *a, **k: p.with_suffix(".png"),
            take_last_frame_option=lambda a, f: take_switch(a, f, "--save-last-frame"),
            validate_paired_mode=Mock())
        clips = module("subdivision_rendering", SUBDIVISION_HELP="subdivision help",
            subdivided_render_session=Mock(side_effect=AssertionError("single-scene path entered")),
            take_subdivision_option=lambda a, f: take_switch(a, f, "--subdivide"),
            validate_subdivided_mode=Mock())
        replacements = [selection, pairs, clips,
            module("batch_provenance", validate_batch_mode=Mock()),
            module("effect_audit", recording=lambda enabled: (self.effects.append(enabled) or nullcontext())),
            module("typesetting", describe_receipt=lambda receipt: "", typesetting_receipt=lambda scene: None),
            module("subdivision", SubdividedRenderResult=type("Clips", (), {})),
            module("studio", _HELP="existing studio help")]
        absent = object()
        saved = {m.__name__: sys.modules.get(m.__name__, absent) for m in replacements}
        sys.modules.update({m.__name__: m for m in replacements})
        def restore():
            for key, old in saved.items():
                if old is absent:
                    sys.modules.pop(key, None)
                else:
                    sys.modules[key] = old
        self.addCleanup(restore)
        self.media = load_module("fmn_python._media_job_cli_test", "console_rendering.py")
        self.addCleanup(sys.modules.pop, "fmn_python._media_job_cli_test", None)
        self.addCleanup(sys.modules.pop, "fmn_python._media_selection_test", None)
        self.h.native._portal_cli_help = lambda: "existing help"
        self.h.native.EndScene = type("EndScene", (Exception,), {})

    def write_jobs(self):
        self.job_file.write_text(json.dumps(dict(schema="fmn-python.source-jobs", version=1, jobs=self.jobs)))

    def parse(self, arguments):
        self.parses.append(list(arguments))
        names, options, index = [], {"format": "png_sequence"}, 0
        flags = {"--format": "format", "--video_dir": "video_dir", "--output": "output",
                 "--file_name": "file_name", "--resolution": "resolution", "-r": "resolution",
                 "--fps": "fps", "--threads": "threads", "--config_file": "config_file",
                 "--vcodec": "vcodec", "--pix_fmt": "pix_fmt", "--ffmpeg_bin": "ffmpeg_bin"}
        while index < len(arguments):
            token = arguments[index]
            flag, equal, inline = token.partition("=")
            if flag in flags:
                if equal:
                    text = inline
                else:
                    index += 1
                    if index == len(arguments):
                        raise ValueError(flag + " requires a value")
                    text = arguments[index]
                options[flags[flag]] = text
            elif token in {"--quiet", "-q"}:
                options["quiet"] = True
            elif token in {"--transparent", "-t"}:
                options["transparent"] = True
            elif token == "--reproducible":
                options["reproducible"] = True
            elif token in {"-w", "--write_file"}:
                options["format"] = "mp4"
            elif token in {"-i", "--gif"}:
                options["format"] = "gif"
            elif token.startswith("-"):
                raise ValueError("unknown native fixture option: " + token)
            else:
                names.append(token)
            index += 1
        if not names:
            raise ValueError("source required")
        width, height = map(int, options.get("resolution", "64x36").split("x"))
        return names, options, width, height, int(options.get("fps", "24")), int(options.get("threads", "8"))

    def invoke(self, *extra, format="png", workers=True, robot=True, job_file=True, names=()):
        args = (["--robot"] if robot else []) + [str(self.source), *names]
        if format is not None:
            args += ["--format", format]
        args += ["--video_dir", str(self.output)]
        if workers:
            args += ["--workers=2"]
        if job_file:
            args += ["--jobs-file", str(self.job_file)]
        self.stdout, self.stderr = io.StringIO(), io.StringIO()
        with redirect_stdout(self.stdout), redirect_stderr(self.stderr):
            code = self.cli.try_bundle_cli(self.h.native, args + list(extra))
            return self.media.try_render_cli(self.h.native, args + list(extra)) if code is None else code

    def payload(self, name="seed7", format="png"):
        path = self.output / name / "frames" / "000000.png" if format == "png_sequence" else self.output / (name + "." + format)
        return json.loads(path.read_bytes().removeprefix(b"NOT-MEDIA:"))

    def assert_not_started(self):
        self.assertEqual(self.discoveries, [])
        self.assertEqual(self.process_calls, [])
        self.loading.SceneSource.assert_not_called()
        self.assertFalse(self.output.exists())

    def test_all_standard_media_formats_keep_job_names_kwargs_and_spawn_isolation(self):
        for format in ("png", "png_sequence", "gif", "y4m", "wav", "svg", "mp4", "mov"):
            with self.subTest(format=format):
                self.output = self.root / format
                self.assertEqual(self.invoke(format=format), 0, self.records[-1])
                rows = self.records[-1]["batch"]["outcomes"]
                self.assertEqual([row["name"] for row in rows], ["seed7", "seed11"])
                self.assertEqual([self.payload(name, format)["values"]["value"] for name in ("seed7", "seed11")], [7, 11])
                self.assertTrue(all(row["result"]["format"] == format for row in rows))
                self.assertTrue(all(row["result"]["threads"] == 1 for row in rows))
        pids = (self.root / "imports").read_text().splitlines()
        self.assertEqual(len(pids), 16)
        self.assertNotIn(str(os.getpid()), pids)
        self.loading.SceneSource.assert_not_called()
        self.assertTrue(all(enabled is False for enabled in self.effects))

    def test_inline_jobs_file_and_one_job_default_format_remain_batches(self):
        self.jobs = self.jobs[:1]
        self.write_jobs()
        self.assertEqual(self.invoke("--jobs-file=" + str(self.job_file), job_file=False, format=None), 0)
        self.assertEqual(self.records[-1]["kind"], "render-batch")
        self.assertEqual(self.records[-1]["batch"]["execution"], "spawn")
        self.assertEqual(self.payload(format="png_sequence")["values"]["value"], 7)

    def test_real_media_helpers_forward_quality_resource_range_and_thread_options(self):
        encoder = self.root / "encoder with spaces"
        self.assertEqual(self.invoke("--vcodec", "libx264", "--pix_fmt", "rgba", "--ffmpeg_bin", str(encoder),
            "--crf=0", "--preset", "slow", "--tune", "animation", "--audio-bitrate", "192000",
            "--ffmpeg-timeout=73", "--ffmpeg-max-log-bytes", "2048", "--ffmpeg-max-input-bytes", "8192",
            "--ffmpeg-max-artifact-bytes=4096", "--threads", "4", "-n", "2,5", format="mp4"), 0)
        options = self.payload(format="mp4")["options"]
        self.assertEqual(options["threads"], 4)
        self.assertEqual(options["animation_range"], [2, 5])
        self.assertEqual(options["_output_options"], dict(vcodec="libx264", pix_fmt="rgba", ffmpeg_bin=str(encoder),
            video_crf=0, video_preset="slow", video_tune="animation", audio_bitrate=192000,
            ffmpeg_timeout=73, ffmpeg_max_log_bytes=2048, ffmpeg_max_input_bytes=8192, ffmpeg_max_artifact_bytes=4096))

    def test_transparent_mov_and_open_range_keep_their_native_profiles(self):
        self.assertEqual(self.invoke("--transparent", "--vcodec", "qtrle", "--pix_fmt", "bgra", "-n2", format="mov"), 0)
        options = self.payload(format="mov")["options"]
        self.assertEqual(options["animation_range"], [2, None])
        self.assertEqual(options["_output_options"], dict(transparent=True, vcodec="qtrle", pix_fmt="bgra"))

    def test_final_still_selection_uses_existing_png_mode(self):
        self.assertEqual(self.invoke("-s", "-n", "1,3", format=None), 0)
        self.assertEqual(self.payload()["options"]["animation_range"], [1, 3])
        self.assertEqual(self.records[-1]["batch"]["outcomes"][0]["result"]["format"], "png")

    def test_quality_and_format_conflicts_refuse_before_discovery(self):
        for format, extra in (("png", ["--crf", "20"]), ("wav", ["--transparent"]),
                              ("mov", ["-t", "--crf", "20"]), ("mp4", ["-s"]),
                              ("mov", ["-t", "--vcodec", "h264"]), ("mp4", ["-n", "3,3"])):
            with self.subTest(format=format, extra=extra):
                self.assertEqual(self.invoke(*extra, format=format), 2)
        self.assert_not_started()

    def test_jobs_file_requires_workers_and_exclusive_scene_selection(self):
        self.assertEqual(self.invoke(workers=False), 2)
        self.assertEqual(self.invoke(names=("Lesson",)), 2)
        for extra in (["--write_all"], ["--output", str(self.root / "single.png")],
                      ["--file_name", "single"], ["--jobs-file", str(self.job_file)]):
            self.assertEqual(self.invoke(*extra), 2)
        self.assert_not_started()

    def test_variant_certification_refuses_before_any_discovery(self):
        self.assertEqual(self.invoke("--reproducible"), 4)
        self.assertIn("constructor variants", self.records[-1]["message"])
        self.assert_not_started()

    def test_missing_jobs_value_retains_robot_and_help_never_reads_file(self):
        self.assertEqual(self.invoke("--jobs-file", "--robot", robot=False, job_file=False), 2)
        self.assertTrue(self.records[-1]["robot"])
        self.assertEqual(self.invoke("--jobs-file", str(self.root / "absent"), "--help", job_file=False), 0)
        self.assertIn("fmn-python.source-jobs", self.records[-1]["help"])
        self.assertIn("mp4", self.records[-1]["help"])
        self.assert_not_started()

    def test_file_values_in_other_flags_do_not_select_a_jobs_document(self):
        self.assertEqual(self.invoke("--config_file", "--jobs-file", job_file=False, names=("Lesson", "Other")), 0)
        self.assertEqual(self.process_calls[0][1]["scene_names"], ["Lesson", "Other"])

    def test_malformed_document_refuses_before_source_import(self):
        for raw in ('{"schema":"fmn-python.source-jobs","version":1,"jobs":[]}',
                    '{"schema":"fmn-python.source-jobs","schema":"other","version":1,"jobs":[]}',
                    'not json'):
            self.job_file.write_text(raw)
            self.assertEqual(self.invoke(), 2)
        self.assert_not_started()

    def test_unknown_variant_class_refuses_the_entire_batch(self):
        self.jobs[1]["scene"] = "Missing"
        self.write_jobs()
        self.assertEqual(self.invoke(), 2)
        self.assertEqual(len(self.discoveries), 1)
        self.assertEqual(self.process_calls, [])
        self.assertFalse((self.root / "imports").exists())

    def test_discovery_receives_configuration_and_entry_identity_is_bound(self):
        self.configuration = {"values": {}, "overrides": []}
        self.assertEqual(self.invoke("--job-timeout=12", "--resolution", "128x72", "--fps", "30"), 0)
        self.assertEqual(self.discoveries[0][1], dict(working_directory=Path.cwd(), job_timeout=12,
                                                     configuration=self.configuration))
        options = self.process_calls[0][1]
        self.assertEqual(options["_configuration"], self.configuration)
        self.assertEqual(options["_expected_source_digest"], hashlib.sha256(self.source.read_bytes()).hexdigest())
        self.assertEqual(options["resolution"], (128, 72))
        self.assertEqual(options["fps"], 30)

    def test_resume_reuses_success_after_failure_and_rejects_modified_arguments(self):
        self.jobs[1]["scene_kwargs"]["transient"] = True
        self.write_jobs()
        args = ("--checkpoint", str(self.checkpoint), "--resume-key", "assets-v1", "--keep-going")
        self.assertEqual(self.invoke(*args), 5)
        first = self.output / "seed7.png"
        saved = (first.read_bytes(), first.stat().st_mtime_ns)
        (self.root / "retry").write_text("repaired")
        self.assertEqual(self.invoke(*args, "--resume"), 0)
        self.assertEqual(self.records[-1]["batch"]["reused"], ["seed7"])
        self.assertEqual((first.read_bytes(), first.stat().st_mtime_ns), saved)
        before = self.checkpoint.read_bytes()
        self.jobs[0]["scene_kwargs"]["value"] = 99
        self.write_jobs()
        self.assertEqual(self.invoke(*args, "--resume"), 2)
        self.assertEqual(self.checkpoint.read_bytes(), before)

    def test_tampered_media_cannot_be_resumed_or_silently_overwritten(self):
        args = ("--checkpoint", str(self.checkpoint), "--resume-key", "assets-v1")
        self.assertEqual(self.invoke(*args, format="wav"), 0)
        target = self.output / "seed7.wav"
        target.write_bytes(b"tampered")
        imports = (self.root / "imports").read_text()
        self.assertEqual(self.invoke(*args, "--resume", format="wav"), 2)
        self.assertEqual(target.read_bytes(), b"tampered")
        self.assertEqual((self.root / "imports").read_text(), imports)

    def test_later_destination_collision_prevents_all_workers(self):
        self.output.mkdir()
        target = self.output / "seed11.png"
        target.write_bytes(b"preexisting")
        self.assertEqual(self.invoke(), 6)
        self.assertFalse((self.root / "imports").exists())
        self.assertFalse((self.output / "seed7.png").exists())
        self.assertEqual(target.read_bytes(), b"preexisting")

    def test_job_snapshot_survives_file_mutation_during_discovery(self):
        original = self.discover
        @contextmanager
        def changed(*args, **kwargs):
            self.jobs[0]["scene_kwargs"]["value"] = 99
            self.write_jobs()
            with original(*args, **kwargs) as loaded:
                yield loaded
        with patch("fmn_python.parallel_discovery.discovery_source", changed):
            self.assertEqual(self.invoke(), 0)
        self.assertEqual(self.payload()["values"]["value"], 7)

    def test_worker_interrupt_reports_cancelled_progress(self):
        self.jobs[0]["scene_kwargs"]["cancel"] = True
        self.write_jobs()
        self.assertEqual(self.invoke("--keep-going"), 130)
        rows = self.records[-1]["batch"]["outcomes"]
        self.assertEqual(rows[0]["status"], "cancelled")
        self.assertEqual(rows[1]["status"], "succeeded")
        self.assertTrue((self.output / "seed11.png").exists())

    def test_collection_modes_delegate_to_existing_batch_owner(self):
        calls = []
        def capture(*args, **options):
            calls.append((args, options))
            return Result((Outcome("seed7", self.output / "seed7" / "clips", "succeeded"),))
        with patch("fmn_python.parallel_batch.render_source_scenes", capture):
            self.assertEqual(self.invoke("--subdivide", "--save-last-frame", "-n", "2,5", format="mp4"), 0)
        args, options = calls[0]
        self.assertTrue(options["subdivide"])
        self.assertTrue(options["save_last_frame"])
        self.assertEqual(options["animation_range"], (2, 5))
        self.assertEqual([job.name for job in options["scene_names"]], ["seed7", "seed11"])
        self.assertEqual(options["scene_names"][1].scene_kwargs, {"value": 11})
        # Parallel collections now support recovery; the sequential refusal
        # remains, and dedicated CLI recovery tests exercise the positive path.
        self.assertEqual(self.invoke("--subdivide", "--checkpoint", str(self.checkpoint), "--resume-key", "v1",
                                     workers=False, job_file=False, names=("Lesson", "Other"), format="mp4"), 2)
        self.assertEqual(len(self.discoveries), 1)

    def test_quiet_keeps_robot_receipt_and_legacy_single_mode_is_not_a_job_batch(self):
        self.assertEqual(self.invoke("--quiet"), 0)
        self.assertEqual(self.stderr.getvalue(), "")
        self.assertTrue(self.records[-1]["robot"])
        self.assertEqual(self.cli.try_bundle_cli(self.h.native, ["source.py", "Scene", "--format", "png"]), None)
        self.assertIsNone(self.media.try_render_cli(self.h.native, ["--version"]))


if __name__ == "__main__":
    unittest.main()
