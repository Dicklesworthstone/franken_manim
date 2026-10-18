"""Real process + disk recovery tests with a reduced render-owner fixture.

The production scheduler, journal, inventory hashing and parallel coordinator
run unchanged. The worker writes byte artifacts instead of rasterizing; this
suite makes no native rendering claim. test_parallel_batch_native.py exercises
the separately built native wheel.
"""
from __future__ import annotations

import dataclasses
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
from unittest.mock import patch

PYTHON = Path(__file__).resolve().parents[1] / "python"
sys.path.insert(0, str(PYTHON))
from fmn_python.batch_checkpoint import BatchCheckpoint


@dataclasses.dataclass(frozen=True)
class Receipt:
    data: dict

    def __getattr__(self, name):
        return self.data[name]

    def as_dict(self):
        return dict(self.data)


@dataclasses.dataclass(frozen=True)
class Outcome:
    name: str
    destination: Path
    status: str
    result: Receipt | None = None
    error_type: str | None = None
    message: str | None = None
    notes: tuple = ()

    def as_dict(self):
        return {"name": self.name, "destination": str(self.destination), "status": self.status,
                "result": None if self.result is None else self.result.as_dict(),
                "error": None if self.error_type is None else
                {"type": self.error_type, "message": self.message, "notes": list(self.notes)}}


@dataclasses.dataclass(frozen=True)
class Result:
    outcomes: tuple
    reproducible: bool = False

    @property
    def ok(self):
        return bool(self.outcomes) and all(item.status == "succeeded" for item in self.outcomes)

    def as_dict(self):
        return {"ok": self.ok, "certified": False,
                "outcomes": [item.as_dict() for item in self.outcomes]}


class BatchError(RuntimeError):
    def __init__(self, result):
        self.result = result
        super().__init__("byte-artifact batch failed")


def restore_outcome(row):
    result = row["result"]
    if (not isinstance(result, dict) or result["destination"] != row["destination"]
            or result["certified"] is not False):
        raise ValueError("invalid fixture publication receipt")
    return Outcome(row["name"], Path(row["destination"]), "succeeded", Receipt(result))


def publish_worker(job):
    """Importable, genuinely spawned worker; no native code or mock IPC."""
    root = Path(job["source"]).parent
    attempts = root / (job["name"] + ".attempts")
    with attempts.open("a") as stream:
        stream.write(str(os.getpid()) + "\n")
    # This mutable helper stands in for a repaired external transient failure,
    # not a change to the automatically bound entry-source bytes.
    mode = json.loads((root / "behavior.json").read_text()).get(job["name"], "ok")
    if mode == "fail":
        raise RuntimeError("transient render fixture failure")
    if mode == "crash":
        os._exit(23)
    if mode == "sleep":
        time.sleep(30)
    format = job["options"]["format"]
    destination = Path(job["directory"]) / (job["name"] + "." + format)
    data = ("artifact:" + job["name"]).encode("utf-8")
    if format == "png_sequence":
        destination = Path(job["directory"]) / job["name"] / "frames"
        destination.mkdir(parents=True, exist_ok=False)
        for index in range(2):
            with (destination / f"{index:06d}.png").open("xb") as stream:
                stream.write(data)
        data *= 2
    else:
        destination.parent.mkdir(parents=True, exist_ok=True)
        with destination.open("xb") as stream:
            stream.write(data)
    if mode == "publish_then_crash":
        os._exit(24)
    return {"name": job["name"], "destination": str(destination), "status": "succeeded", "error": None,
            "result": {"destination": str(destination), "format": format, "certified": False,
                       "bytes": len(data), "digest": hashlib.sha256(data).hexdigest(),
                       "engine": "fixture-no-raster", "frame_count": 2 if format == "png_sequence" else 1,
                       "fps": job["options"]["fps"] or 30, "threads": job["options"]["threads"],
                       "resolution": job["options"]["resolution"] or [64, 48], "seed": 0}}


def load_coordinator():
    # Only renderer-facing imports are reduced. Execute the COMPLETE shipping
    # coordinator, not a copied function or a rewritten scheduler/checkpointer.
    batch = types.ModuleType("fmn_python.batch_rendering")
    batch.BatchRenderError, batch.BatchRenderResult = BatchError, Result
    batch.SceneRenderOutcome, batch._restored_outcome = Outcome, restore_outcome
    batch._name_key = lambda name: name.casefold()
    selection = types.ModuleType("fmn_python.render_selection")
    selection.animation_range = lambda value: value
    rendering = types.ModuleType("fmn_python.rendering")
    rendering._FORMATS = {"png", "png_sequence"}
    name = "fmn_python._checkpoint_coordinator_test"
    spec = importlib.util.spec_from_file_location(name, PYTHON / "fmn_python/parallel_batch.py")
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    replacements = {"fmn_python.batch_rendering": batch,
                    "fmn_python.render_selection": selection,
                    "fmn_python.rendering": rendering}
    absent = object()
    saved = {key: sys.modules.get(key, absent) for key in replacements}
    try:
        sys.modules.update(replacements)
        spec.loader.exec_module(module)
    finally:
        # Restore only these dependencies. Restoring all of sys.modules would
        # evict multiprocessing's lazy imports and break spawn class identity.
        for key, value in saved.items():
            if value is absent:
                sys.modules.pop(key, None)
            else:
                sys.modules[key] = value
    module._render_source_job = publish_worker
    return module


class ParallelCheckpointTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name).resolve()
        self.source = self.root / "scenes.py"
        self.source.write_text("# entry source is stable\n", encoding="utf-8")
        self.destination = self.root / "output"
        self.checkpoint = self.root / "batch.json"
        self.behavior()
        self.parallel = load_coordinator()
        self.names = ["Done", "Broken", "Later"]

    def tearDown(self):
        self.temp.cleanup()

    def behavior(self, **modes):
        (self.root / "behavior.json").write_text(json.dumps(modes), encoding="utf-8")

    def render(self, **overrides):
        options = dict(workers=2, format="png", checkpoint=self.checkpoint,
                       resume_key="helper-and-assets-v1", continue_on_error=True)
        options.update(overrides)
        return self.parallel.render_source_scenes(self.source, self.destination, self.names, **options)

    def counts(self):
        return [len((self.root / (name + ".attempts")).read_text().splitlines())
                if (self.root / (name + ".attempts")).exists() else 0 for name in self.names]

    def document(self):
        return json.loads(self.checkpoint.read_text())

    def test_partial_failure_resumes_only_missing_jobs_with_different_worker_budget(self):
        self.behavior(Broken="fail")
        first = self.render()
        self.assertEqual([o.status for o in first.outcomes], ["succeeded", "failed", "succeeded"])
        preserved = [(self.destination / (name + ".png")).stat().st_mtime_ns for name in ("Done", "Later")]
        self.behavior()
        events = []
        second = self.render(resume=True, workers=3, job_timeout=10, on_result=lambda o: events.append(o.name))
        self.assertTrue(second.ok)
        self.assertEqual(second.reused, ("Done", "Later"))
        self.assertEqual(events[:2], ["Done", "Later"])
        self.assertEqual(self.counts(), [1, 2, 1])
        self.assertEqual(preserved, [(self.destination / (name + ".png")).stat().st_mtime_ns
                                     for name in ("Done", "Later")])
        self.assertEqual(second.as_dict()["execution"], "spawn")
        self.assertEqual(second.as_dict()["reused"], ["Done", "Later"])
        self.assertFalse(second.as_dict()["certified"])

    def test_fail_fast_journal_keeps_unstarted_work_for_resume(self):
        self.behavior(Broken="fail")
        with self.assertRaises(BatchError) as caught:
            self.render(workers=1, continue_on_error=False)
        self.assertEqual([o.status for o in caught.exception.result.outcomes],
                         ["succeeded", "failed", "not_run"])
        self.assertEqual(self.counts(), [1, 1, 0])
        self.behavior()
        report = self.render(resume=True)
        self.assertTrue(report.ok)
        self.assertEqual(report.reused, ("Done",))
        self.assertEqual(self.counts(), [1, 2, 1])

    def test_checkpoint_write_failure_preserves_publication_without_claiming_reuse(self):
        self.names = ["Done"]
        original = BatchCheckpoint.record
        failure = OSError("journal storage unavailable")

        def broken_record(journal, outcomes):
            if any(outcome.status == "succeeded" for outcome in outcomes):
                raise failure
            return original(journal, outcomes)

        with patch.object(BatchCheckpoint, "record", broken_record):
            with self.assertRaises(OSError) as caught:
                self.render()
        self.assertIs(caught.exception, failure)
        self.assertTrue(failure.render_batch_result.ok)
        self.assertTrue(any("checkpoint also failed" in note for note in failure.__notes__))
        self.assertEqual(self.document()["outcomes"][0]["status"], "not_run")
        self.assertTrue((self.destination / "Done.png").exists())
        with self.assertRaises(FileExistsError):
            self.render(resume=True)
        self.assertEqual(self.counts(), [1])

    def test_complete_resume_does_not_start_any_process(self):
        self.render()
        with patch.object(self.parallel, "run_process_jobs", side_effect=AssertionError("must not launch")):
            report = self.render(resume=True, workers=1)
        self.assertEqual(report.reused, tuple(self.names))
        self.assertEqual(self.counts(), [1, 1, 1])

    def test_entry_source_change_refuses_before_worker_launch(self):
        self.render()
        before = self.checkpoint.read_bytes()
        self.source.write_text("# different entry bytes\n", encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "plan or render options"):
            self.render(resume=True)
        self.assertEqual(before, self.checkpoint.read_bytes())
        self.assertEqual(self.counts(), [1, 1, 1])

    def test_changed_semantic_plan_is_not_reusable(self):
        self.render(fps=30)
        for overrides in ({"fps": 60}, {"threads": 2}, {"resolution": (80, 48)},
                          {"animation_range": (1, 3)}, {"_working_directory": self.root},
                          {"_configuration": {"values": {"camera": {"background_color": "#112233"}}}},
                          {"_output_options": {"transparent": True}}, {"resume_key": "another-input-version"}):
            with self.subTest(overrides=overrides), self.assertRaises(ValueError):
                self.render(resume=True, **{**{"fps": 30}, **overrides})
        self.names.reverse()
        with self.assertRaises(ValueError):
            self.render(resume=True, fps=30)
        self.assertEqual(self.counts(), [1, 1, 1])

    def test_modified_artifact_refuses_without_rewriting_it(self):
        self.render()
        artifact = self.destination / "Done.png"
        artifact.write_bytes(b"tampered")
        with self.assertRaisesRegex(ValueError, "missing or modified"):
            self.render(resume=True)
        self.assertEqual(artifact.read_bytes(), b"tampered")
        self.assertEqual(self.counts(), [1, 1, 1])

    def test_sequence_inventory_detects_extra_files(self):
        self.render(format="png_sequence")
        extra = self.destination / "Done/frames/extra.png"
        extra.write_bytes(b"extra")
        with self.assertRaisesRegex(ValueError, "missing or modified"):
            self.render(resume=True, format="png_sequence")
        self.assertEqual(self.counts(), [1, 1, 1])

    def test_artifact_without_receipt_remains_no_clobber(self):
        self.behavior(Broken="publish_then_crash")
        first = self.render()
        self.assertEqual(first.outcomes[1].status, "failed")
        data = (self.destination / "Broken.png").read_bytes()
        self.behavior()
        with self.assertRaises(FileExistsError):
            self.render(resume=True)
        self.assertEqual((self.destination / "Broken.png").read_bytes(), data)
        self.assertEqual(self.counts(), [1, 1, 1])

    def test_resume_interrupt_merges_pending_indices_not_full_scene_indices(self):
        self.behavior(Broken="fail", Later="fail")
        self.render()
        self.behavior()

        def interrupt(outcome):
            row = next(row for row in self.document()["outcomes"] if row["name"] == outcome.name)
            self.assertEqual(row["status"], "succeeded")  # durable before callback
            if outcome.name == "Broken":
                raise KeyboardInterrupt("observer interruption")

        with self.assertRaises(KeyboardInterrupt) as caught:
            self.render(resume=True, workers=1, on_result=interrupt)
        report = caught.exception.render_batch_result
        self.assertEqual([o.status for o in report.outcomes], ["succeeded", "succeeded", "not_run"])
        self.assertEqual(report.reused, ("Done",))
        self.assertEqual(self.document()["outcomes"], [o.as_dict() for o in report.outcomes])
        final = self.render(resume=True)
        self.assertTrue(final.ok)
        self.assertEqual(final.reused, ("Done", "Broken"))
        self.assertEqual(self.counts(), [1, 2, 2])

    def test_reused_observer_failure_preserves_checkpoint_without_starting_jobs(self):
        self.behavior(Broken="fail")
        self.render()
        error = RuntimeError("observer failed")
        with self.assertRaises(RuntimeError) as caught:
            self.render(resume=True, on_result=lambda _: (_ for _ in ()).throw(error))
        self.assertIs(caught.exception, error)
        self.assertEqual(error.render_batch_result.reused, ("Done", "Later"))
        self.assertEqual(self.counts(), [1, 1, 1])
        self.behavior()
        self.assertTrue(self.render(resume=True).ok)

    def test_worker_crash_and_timeout_are_retryable_when_nothing_was_published(self):
        self.behavior(Broken="crash", Later="sleep")
        first = self.render(job_timeout=1.5)
        self.assertEqual([o.status for o in first.outcomes], ["succeeded", "failed", "failed"])
        self.assertEqual(first.outcomes[2].error_type, "TimeoutError")
        self.behavior()
        self.assertTrue(self.render(resume=True, job_timeout=20).ok)
        self.assertEqual(self.counts(), [1, 2, 2])

    def test_checkpoint_and_lock_cannot_overlap_artifacts(self):
        for path in (self.destination / "Done.png", self.destination / "Done.png/receipt.json"):
            with self.subTest(path=path), self.assertRaisesRegex(ValueError, "outside published artifacts"):
                self.render(checkpoint=path)
        self.assertFalse(self.destination.exists())
        self.assertEqual(self.counts(), [0, 0, 0])

    def test_concurrent_journal_owner_refuses_before_starting_jobs(self):
        self.render()
        with BatchCheckpoint(self.checkpoint, resume=True, key="helper-and-assets-v1"):
            with self.assertRaises(OSError):
                self.render(resume=True)
        self.assertEqual(self.counts(), [1, 1, 1])

    def test_invalid_resume_and_timeout_options_refuse_even_with_no_pending_work(self):
        self.render()
        for options in ({"job_timeout": 0}, {"job_timeout": float("nan")},
                        {"job_timeout": True}, {"resume": 1}, {"checkpoint": None},
                        {"resume_key": None}, {"_output_options": {"x": float("nan")}}):
            with self.subTest(options=options), self.assertRaises((ValueError, TypeError)):
                self.render(**{**{"resume": True}, **options})
        self.assertEqual(self.counts(), [1, 1, 1])

    def test_legacy_class_checkpoint_format_and_recovery_are_unchanged(self):
        class Scene:
            pass
        path = self.destination / "Done.png"
        self.destination.mkdir()
        path.write_bytes(b"legacy")
        row = Outcome("Done", path, "succeeded", Receipt({"format": "png", "destination": str(path),
            "certified": False, "bytes": 6, "digest": hashlib.sha256(b"legacy").hexdigest()}))
        jobs = [types.SimpleNamespace(name="Done", scene=Scene, scene_kwargs={"flag": True})]
        with BatchCheckpoint(self.checkpoint, resume=False, key="legacy") as journal:
            journal.prepare(jobs, [path], {"format": "png"})
            journal.record([row])
        plan = self.document()["plan"]
        self.assertEqual(plan, {"constructor_inputs": "json-values-v1", "options": {"format": "png"},
                               "jobs": [{"name": "Done", "destination": str(path), "scene_kwargs": {"flag": True},
                                         "scene": {"module": Scene.__module__, "qualname": Scene.__qualname__}}]})
        with BatchCheckpoint(self.checkpoint, resume=True, key="legacy") as journal:
            journal.prepare(jobs, [path], {"format": "png"})
            self.assertIn("Done", journal.completed)
            with self.assertRaisesRegex(ValueError, "plan or render options"):
                journal.prepare_sources(["Done"], [path], {"format": "png"}, source=self.source,
                    source_digest=hashlib.sha256(self.source.read_bytes()).hexdigest(), working_directory=Path.cwd())


if __name__ == "__main__":
    unittest.main()
