"""FMTL process orchestration with real spawned workers and disk checkpoints.

The scheduler, JSON transport, bundle validators, typed receipt restoration and
checkpoint inventory/locking run from production modules. Native capture and
render-result classes are explicit fixtures; byte artifacts are NOT FMTL files.
No native rendering, PCM mixing or installed-wheel acceptance is claimed here.
"""
from __future__ import annotations

from contextlib import contextmanager, nullcontext
from dataclasses import dataclass
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import time
import types
import unittest
from unittest.mock import Mock, patch

PYTHON = Path(__file__).resolve().parents[1] / "python"
sys.path.insert(0, str(PYTHON))
from fmn_python._process_batch import positive_integer


@dataclass(frozen=True)
class Outcome:
    name: str
    destination: Path
    status: str
    result: object = None
    error_type: str | None = None
    message: str | None = None
    notes: tuple = ()

    def as_dict(self):
        return {"name": self.name, "destination": str(self.destination), "status": self.status,
                "result": self.result.as_dict() if self.result is not None else None,
                "error": None if self.error_type is None else
                {"type": self.error_type, "message": self.message, "notes": list(self.notes)}}


@dataclass(frozen=True)
class Result:
    outcomes: tuple
    reproducible: bool = False

    @property
    def ok(self):
        return bool(self.outcomes) and all(row.status == "succeeded" for row in self.outcomes)

    def as_dict(self):
        return {"ok": self.ok, "outcomes": [row.as_dict() for row in self.outcomes]}


class BatchError(RuntimeError):
    def __init__(self, result):
        self.result = result
        super().__init__("fixture batch failed")


class CapabilityError(RuntimeError):
    pass


def name_key(name):
    if not isinstance(name, str) or not name.isidentifier():
        raise ValueError("invalid fixture scene name")
    return name.casefold()


def load_module(name, filename):
    spec = importlib.util.spec_from_file_location(name, PYTHON / "fmn_python" / filename)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


@contextmanager
def host_fixture():
    # Restore only these entries; restoring all sys.modules would evict lazy
    # multiprocessing imports and damage spawn's class identities.
    names = ["manimlib", *["fmn_python." + name for name in (
        "rendering", "scene_execution", "batch_rendering", "render_selection",
        "bundle_export", "bundle_checkpoint", "_parallel_bundle_test")]]
    absent = object()
    saved = {name: sys.modules.get(name, absent) for name in names}
    try:
        native = types.ModuleType("manimlib")
        native._CapabilityError = CapabilityError
        native.Scene = type("Scene", (), {})
        for name in ("_portal_begin_bundle", "_portal_begin_camera_bundle",
                     "_portal_begin_soundtrack_bundle", "_portal_bundle_segment",
                     "_portal_finish_bundle", "_portal_finish_soundtrack_bundle"):
            setattr(native, name, lambda *args: None)
        rendering = types.ModuleType("fmn_python.rendering")
        rendering.RenderSession = object
        rendering._FORMATS = {"png", "wav", "png_sequence"}
        rendering._positive_integer = lambda v, n, m=(1 << 32) - 1: positive_integer(v, n, m)
        execution = types.ModuleType("fmn_python.scene_execution")
        execution._note = lambda error, text: error.add_note(text)
        selection = types.ModuleType("fmn_python.render_selection")
        selection.animation_range = lambda value: value
        batch = types.ModuleType("fmn_python.batch_rendering")
        batch.BatchRenderError, batch.BatchRenderResult = BatchError, Result
        batch.SceneRenderOutcome, batch._name_key = Outcome, name_key
        batch._restored_outcome = Mock(side_effect=AssertionError("FMTL must use its typed restorer"))
        for module in (native, rendering, execution, selection, batch):
            sys.modules[module.__name__] = module
        bundle = load_module("fmn_python.bundle_export", "bundle_export.py")
        receipt = load_module("fmn_python.bundle_checkpoint", "bundle_checkpoint.py")
        coordinator = load_module("fmn_python._parallel_bundle_test", "parallel_batch.py")
        yield types.SimpleNamespace(native=native, bundle=bundle, receipt=receipt,
                                    batch=batch, coordinator=coordinator)
    finally:
        for name, old in saved.items():
            if old is absent:
                sys.modules.pop(name, None)
            else:
                sys.modules[name] = old


def publish_bundle_job(job):
    """Importable byte writer, executed by the unchanged spawn/receipt owner."""
    root = Path(job["source"]).parent
    name, options = job["name"], job["options"]
    with (root / (name + ".attempts")).open("a") as stream:
        stream.write(str(os.getpid()) + "\n")
    (root / (name + ".job.json")).write_text(json.dumps(job))
    mode = json.loads((root / "behavior.json").read_text()).get(name, "ok")
    if mode == "fail":
        raise RuntimeError("fixture capture failed")
    if mode == "cancel":
        raise KeyboardInterrupt("fixture capture cancelled")
    if mode == "sleep":
        time.sleep(30)
    if mode == "crash":
        os._exit(23)
    destination = Path(job["directory"]) / (name + ".fmtl")
    payload = ("NOT-FMTL fixture " + name).encode().ljust(1024, b"\0")
    destination.parent.mkdir(parents=True, exist_ok=True)
    with destination.open("xb") as stream:
        stream.write(payload)
    if mode == "publish_then_crash":
        os._exit(24)
    receipt = {"schema": "fmn.python-bundle-export", "version": 1, "format": "fmtl",
               "destination": str(destination), "frame_count": 12, "segment_count": 2,
               "bytes": len(payload), "sha256": hashlib.sha256(payload).hexdigest(),
               "fps": options["fps"] or 30, "replay_resolution": options["resolution"] or [64, 36],
               "certified_source": False}
    if options["bundle_camera"]:
        receipt.update(camera_track=True, fmtl_minor=1)
    if options.get("bundle_audio") and mode != "no_cues":
        sound = options["bundle_audio_options"]
        receipt.update(fmtl_minor=2, soundtrack={"sample_rate": sound["audio_sample_rate"],
                       "channels": sound["audio_channels"], "sample_frames": 16})
    if mode == "wrong_camera":
        receipt.update(camera_track=True, fmtl_minor=2 if "soundtrack" in receipt else 1)
    if mode == "wrong_rate":
        receipt["soundtrack"]["sample_rate"] += 1
    return {"name": name, "destination": str(destination), "status": "succeeded",
            "result": receipt, "error": None}


class ParallelBundleTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.source = self.root / "scenes.py"
        self.source.write_text("# stable scene source; not compiled by the byte fixture\n")
        self.output = self.root / "output"
        self.journal = self.root / "progress.json"
        self.behavior()
        fixture = host_fixture()
        self.h = fixture.__enter__()
        self.addCleanup(fixture.__exit__, None, None, None)
        self.parallel = self.h.coordinator
        self.actual_worker = self.parallel._render_source_job
        self.parallel._render_source_job = publish_bundle_job

    def behavior(self, **modes):
        (self.root / "behavior.json").write_text(json.dumps(modes))

    def render(self, names=("Alpha", "Beta"), **overrides):
        options = dict(format="fmtl", workers=2, resolution=(64, 36), fps=24)
        options.update(overrides)
        return self.parallel.render_source_scenes(self.source, self.output, names, **options)

    def attempts(self, name):
        path = self.root / (name + ".attempts")
        return path.read_text().splitlines() if path.exists() else []

    def test_spawned_camera_audio_exports_return_typed_receipts_and_frozen_settings(self):
        limits = {"max_frames": 40, "max_capture_bytes": 4096, "max_output_bytes": 2048}
        audio = {"audio_sample_rate": 22050, "audio_channels": 1, "audio_threads": 2,
                 "max_audio_bytes": 128}
        def observer(outcome):
            self.assertEqual(type(outcome.result), self.h.bundle.BundleExportResult)
            limits["max_frames"] = 1
            audio["audio_sample_rate"] = 1
        result = self.render(bundle_camera=True, bundle_audio=True, bundle_limits=limits,
                             bundle_audio_options=audio, on_result=observer)
        self.assertTrue(result.ok)
        self.assertEqual([row.name for row in result.outcomes], ["Alpha", "Beta"])
        pids = [int(self.attempts(name)[0]) for name in ("Alpha", "Beta")]
        self.assertEqual(len(set([*pids, os.getpid()])), 3)
        for row in result.outcomes:
            self.assertEqual(row.result.soundtrack.sample_rate, 22050)
            self.assertEqual(row.result.soundtrack.channels, 1)
            self.assertTrue(row.result.camera_track)
            job = json.loads((self.root / (row.name + ".job.json")).read_text())
            self.assertEqual(job["options"]["bundle_limits"]["max_frames"], 40)
            self.assertIsNone(job["options"]["threads"])
            self.assertNotIn("checkpoint", job)
            self.assertNotIn("checkpoint", job["options"])
        self.assertEqual(result.as_dict()["execution"], "spawn")

    def test_picture_only_and_no_cue_audio_receipts_remain_valid(self):
        self.behavior(Alpha="no_cues", Beta="no_cues")
        result = self.render(bundle_audio=True)
        self.assertTrue(result.ok)
        self.assertTrue(all(row.result.soundtrack is None for row in result.outcomes))
        self.assertTrue(all("fmtl_minor" not in row.result.as_dict() for row in result.outcomes))

    def test_checkpoint_partial_failure_reuses_complete_bundles_only(self):
        self.behavior(Beta="fail")
        common = dict(bundle_camera=True, bundle_audio=True, checkpoint=self.journal,
                      resume_key="assets-v1", continue_on_error=True)
        first = self.render(**common)
        self.assertFalse(first.ok)
        preserved = (self.output / "Alpha.fmtl").read_bytes()
        self.behavior()
        seen = []
        second = self.render(**common, resume=True, workers=1, job_timeout=10,
                             on_result=lambda row: seen.append(row.name))
        self.assertTrue(second.ok)
        self.assertEqual(second.reused, ("Alpha",))
        self.assertEqual(seen, ["Alpha", "Beta"])
        self.assertEqual([len(self.attempts(n)) for n in ("Alpha", "Beta")], [1, 2])
        self.assertEqual((self.output / "Alpha.fmtl").read_bytes(), preserved)
        with patch.object(self.parallel, "run_process_jobs", side_effect=AssertionError("no workers")):
            third = self.render(**common, resume=True, workers=4)
        self.assertTrue(third.ok)
        self.assertEqual(third.reused, ("Alpha", "Beta"))

    def test_checkpoint_rejects_changed_capture_audio_and_source_before_launch(self):
        common = dict(bundle_audio=True, checkpoint=self.journal, resume_key="assets-v1")
        self.render(**common)
        with patch.object(self.parallel, "run_process_jobs", side_effect=AssertionError("no launch")):
            for change in ({"bundle_camera": True}, {"bundle_limits": {"max_frames": 20}},
                           {"bundle_audio_options": {"audio_sample_rate": 22050}},
                           {"bundle_audio_options": {"audio_threads": 2}}, {"fps": 30}):
                with self.subTest(change=change), self.assertRaisesRegex(ValueError, "plan|options"):
                    self.render(**{**common, **change, "resume": True})
            self.source.write_text("# changed entry\n")
            with self.assertRaisesRegex(ValueError, "plan"):
                self.render(**common, resume=True)

    def test_checkpoint_hashes_artifacts_and_rejects_tampering_before_launch(self):
        common = dict(checkpoint=self.journal, resume_key="assets-v1")
        self.render(**common)
        artifact = self.output / "Alpha.fmtl"
        artifact.write_bytes(b"x" * artifact.stat().st_size)
        with patch.object(self.parallel, "run_process_jobs", side_effect=AssertionError("no launch")):
            with self.assertRaisesRegex(ValueError, "modified"):
                self.render(**common, resume=True)

    def test_observer_failure_occurs_after_durable_receipt(self):
        def observer(_):
            raise LookupError("observer failed")
        with self.assertRaisesRegex(LookupError, "observer") as caught:
            self.render(workers=1, checkpoint=self.journal, resume_key="v1", on_result=observer)
        result = caught.exception.render_batch_result
        self.assertEqual([r.status for r in result.outcomes], ["succeeded", "not_run"])
        document = json.loads(self.journal.read_text())
        self.assertEqual(document["outcomes"][0]["status"], "succeeded")
        self.assertIn("Alpha", document["artifacts"])
        resumed = self.render(workers=1, checkpoint=self.journal, resume_key="v1", resume=True)
        self.assertTrue(resumed.ok)
        self.assertEqual(resumed.reused, ("Alpha",))

    def test_fail_fast_and_keep_going_preserve_previous_exports(self):
        self.behavior(Beta="fail")
        with self.assertRaises(BatchError) as caught:
            self.render(("Alpha", "Beta", "Later"), workers=1)
        self.assertEqual([r.status for r in caught.exception.result.outcomes],
                         ["succeeded", "failed", "not_run"])
        self.assertTrue((self.output / "Alpha.fmtl").exists())
        self.assertFalse(self.attempts("Later"))
        other = self.parallel.render_source_scenes(self.source, self.root / "keep", ("Beta", "Later"),
                                                  format="fmtl", workers=1, continue_on_error=True)
        self.assertEqual([r.status for r in other.outcomes], ["failed", "succeeded"])

    def test_cancelled_worker_stops_admission_and_preserves_completed_bundle(self):
        self.behavior(Beta="cancel")
        with self.assertRaises(KeyboardInterrupt) as caught:
            self.render(("Alpha", "Beta", "Later"), workers=1, continue_on_error=True)
        result = caught.exception.render_batch_result
        self.assertEqual([r.status for r in result.outcomes], ["succeeded", "cancelled", "not_run"])
        self.assertTrue((self.output / "Alpha.fmtl").exists())
        self.assertFalse(self.attempts("Later"))

    def test_timeout_is_bounded_and_keep_going_admits_later_scene(self):
        self.behavior(Alpha="sleep")
        result = self.render(workers=1, job_timeout=2, continue_on_error=True)
        self.assertEqual([r.status for r in result.outcomes], ["failed", "succeeded"])
        self.assertEqual(result.outcomes[0].error_type, "TimeoutError")

    def test_timeout_fail_fast_keeps_completed_bundle_and_unstarted_work(self):
        self.behavior(Beta="sleep")
        with self.assertRaises(BatchError) as caught:
            self.render(("Alpha", "Beta", "Later"), workers=1, job_timeout=2)
        rows = caught.exception.result.outcomes
        self.assertEqual([r.status for r in rows], ["succeeded", "failed", "not_run"])
        self.assertEqual(rows[1].error_type, "TimeoutError")
        self.assertTrue((self.output / "Alpha.fmtl").exists())
        self.assertFalse(self.attempts("Later"))

    def test_crash_after_publication_does_not_create_a_reusable_receipt(self):
        self.behavior(Alpha="publish_then_crash")
        common = dict(checkpoint=self.journal, resume_key="v1", continue_on_error=True)
        result = self.render(workers=1, **common)
        self.assertEqual([r.status for r in result.outcomes], ["failed", "succeeded"])
        self.assertTrue((self.output / "Alpha.fmtl").exists())
        self.assertNotIn("Alpha", json.loads(self.journal.read_text())["artifacts"])
        with patch.object(self.parallel, "run_process_jobs", side_effect=AssertionError("no launch")):
            with self.assertRaises(FileExistsError):
                self.render(**common, resume=True)

    def test_destination_collisions_refuse_before_any_spawn(self):
        self.output.mkdir()
        (self.output / "Beta.fmtl").write_bytes(b"keep")
        with patch.object(self.parallel, "run_process_jobs", side_effect=AssertionError("no launch")):
            with self.assertRaises(FileExistsError):
                self.render()
        self.assertEqual((self.output / "Beta.fmtl").read_bytes(), b"keep")
        self.assertFalse(self.attempts("Alpha"))

    def test_invalid_modes_and_limits_refuse_before_journal_or_spawn(self):
        invalid = [dict(threads=1), dict(animation_range=(1, 2)), dict(subdivide=True),
                   dict(save_last_frame=True), dict(reproducible=True), dict(fps=241),
                   dict(resolution=(4097, 4097)), dict(_output_options={"encoder": "x"}),
                   dict(bundle_camera=1), dict(bundle_audio=0), dict(bundle_limits=[]),
                   dict(bundle_limits={"max_frames": 0}), dict(bundle_limits={"surprise": 1}),
                   dict(bundle_audio_options={}), dict(bundle_audio=True, bundle_audio_options=[]),
                   dict(bundle_audio=True, bundle_audio_options={"audio_channels": 3}),
                   dict(bundle_audio=True, bundle_audio_options={"max_audio_bytes": 7})]
        with patch.object(self.parallel, "run_process_jobs", side_effect=AssertionError("no launch")):
            for kwargs in invalid:
                with self.subTest(kwargs=kwargs), self.assertRaises((ValueError, TypeError, CapabilityError)):
                    self.render(checkpoint=self.journal, resume_key="v1", **kwargs)
                self.assertFalse(self.journal.exists())
                self.assertFalse(self.journal.with_name("progress.json.lock").exists())

    def test_missing_native_bundle_capability_refuses_before_launch(self):
        self.h.native._portal_finish_soundtrack_bundle = None
        with patch.object(self.parallel, "run_process_jobs", side_effect=AssertionError("no launch")):
            with self.assertRaises(CapabilityError):
                self.render(bundle_audio=True)

    def test_parent_rejects_wrong_native_receipt_mode_and_audio_layout(self):
        for name, mode in (("WrongCamera", "wrong_camera"), ("WrongRate", "wrong_rate")):
            self.behavior(**{name: mode})
            with self.subTest(mode=mode), self.assertRaises(ValueError) as caught:
                self.render((name,), bundle_audio=True)
            self.assertEqual(caught.exception.render_batch_result.outcomes[0].status, "failed")
            self.assertTrue((self.output / (name + ".fmtl")).exists())

    def test_typed_receipt_checks_admitted_limits_even_without_checkpoint(self):
        with self.assertRaisesRegex(ValueError, "limits") as caught:
            self.render(("Alpha",), bundle_limits={"max_frames": 8})
        self.assertEqual(caught.exception.render_batch_result.outcomes[0].status, "failed")
        self.assertTrue((self.output / "Alpha.fmtl").exists())

    def test_raster_default_threads_and_checkpoint_shape_are_unchanged(self):
        captured = []
        def scheduler(jobs, worker, **kwargs):
            captured.extend(jobs)
            return [dict(status="failed", error={"type": "Fixture", "message": "no raster"}) for _ in jobs]
        with patch.object(self.parallel, "run_process_jobs", scheduler):
            result = self.render(format="png", continue_on_error=True)
        self.assertFalse(result.ok)
        self.assertEqual(captured[0]["options"], {"format": "png", "resolution": [64, 36],
                         "fps": 24, "threads": 1, "animation_range": None, "_output_options": None})
        with self.assertRaises(ValueError):
            self.render(format="png", bundle_audio=True)

    def test_existing_worker_forwards_bundle_options_to_shared_render_scenes(self):
        job = dict(source=str(self.source), source_digest=hashlib.sha256(self.source.read_bytes()).hexdigest(),
                   directory=str(self.output), name="Alpha", working_directory=str(self.root),
                   options={"format": "fmtl", "bundle_camera": True, "bundle_audio": True,
                            "bundle_audio_options": {"audio_channels": 1}})
        loaded = types.SimpleNamespace(scenes={"Alpha": self.h.native.Scene},
                                       _source_bytes={self.source: self.source.read_bytes()})
        loading = types.ModuleType("fmn_python.scene_loading")
        loading.SceneSource = Mock(return_value=nullcontext(loaded))
        audit = types.ModuleType("fmn_python.effect_audit")
        audit.recording = lambda _: nullcontext()
        self.h.batch.render_scenes = Mock(return_value=Result((Outcome("Alpha", self.output / "Alpha.fmtl", "failed"),)))
        cwd = Path.cwd()
        try:
            with patch.dict(sys.modules, {loading.__name__: loading, audit.__name__: audit}):
                row = self.actual_worker(job)
            self.assertEqual(row["status"], "failed")
            args, kwargs = self.h.batch.render_scenes.call_args
            self.assertEqual(args, ({"Alpha": self.h.native.Scene}, str(self.output)))
            self.assertEqual(kwargs, {**job["options"], "continue_on_error": True})
        finally:
            os.chdir(cwd)


if __name__ == "__main__":
    unittest.main()
