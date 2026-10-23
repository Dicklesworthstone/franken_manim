"""Compose production completion and clip journals over real process supervision.

The existing importable byte/camera fixtures replace native rendering only.
These checks prove worker admission, receipt verification, filesystem ownership
and retry behavior, not native media output or rendered pixels.
"""
from __future__ import annotations

import json
import os
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import test_parallel_clip_recovery as clips
import test_parallel_paired_clip_recovery as pairs
from test_paired_clip_recovery_protocol import CaptureFixture
from fmn_python import parallel_batch
from fmn_python.batch_checkpoint import BatchCheckpoint
from fmn_python.batch_rendering import BatchRenderError
from fmn_python.cancellation import BatchCancelled, CancellationToken
from fmn_python.parallel_clip_recovery import ParallelClipPlan
from fmn_python.source_jobs import SourceRenderJob


def two_level_worker_fixture(job):
    paired = job["options"].get("save_last_frame", False)
    result = pairs.paired_worker_fixture(job) if paired else clips.byte_clip_job(job)
    behavior = json.loads((Path(job["source"]).parent / "behavior.json").read_text())
    if behavior.get(job["name"]) == "terminal-crash":
        os._exit(41)
    return result


class TwoLevelBatchRecoveryTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="fmn-two-level-"))
        self.source = self.root / "source.py"
        self.source.write_text("# explicit native byte fixture, not media\n")
        self.output = self.root / "out"
        self.state = self.root / "prefix-state"
        self.checkpoint = self.root / "completed.json"
        self.native = SimpleNamespace(_portal_prepare_recording_scene=lambda: None,
            _portal_begin_audio_clip=lambda: None, _CameraCapture=CaptureFixture)
        self.behavior()

    def behavior(self, **values):
        (self.root / "behavior.json").write_text(json.dumps(values))

    def render(self, names=("A", "B"), **overrides):
        options = dict(format="y4m", subdivide=True, resolution=(64, 36), fps=8, workers=2,
            checkpoint=self.checkpoint, clip_checkpoint_dir=self.state,
            resume_key="inputs-v1", continue_on_error=True)
        options.update(overrides)
        with patch.dict(sys.modules, {"manimlib": self.native}), \
                patch.object(parallel_batch, "_render_source_job", two_level_worker_fixture):
            return parallel_batch.render_source_scenes(self.source, self.output, names, **options)

    def calls(self, name):
        path = self.root / (name + ".calls")
        return len(path.read_text().splitlines()) if path.exists() else 0

    def signature(self, path):
        return path.read_bytes(), path.stat().st_mtime_ns

    def completed_document(self):
        return json.loads(self.checkpoint.read_text())

    def test_completed_jobs_skip_and_unfinished_jobs_replay_only_the_prefix(self):
        self.behavior(B="fail")
        first = self.render()
        self.assertEqual([row.status for row in first.outcomes], ["succeeded", "failed"])
        saved = [self.output / "A/clips/segment-000000.y4m",
                 self.output / "B/clips/segment-000000.y4m"]
        before = list(map(self.signature, saved))
        self.behavior()
        observed = []
        second = self.render(resume=True, workers=1, job_timeout=8,
                             on_result=lambda outcome: observed.append(outcome.name))
        self.assertTrue(second.ok, second.as_dict())
        self.assertEqual(second.reused, ("A",))
        self.assertEqual(observed, ["A", "B"])
        self.assertEqual((self.calls("A"), self.calls("B")), (1, 2))
        self.assertEqual(list(map(self.signature, saved)), before)
        self.assertEqual(len(second.outcomes[1].result.segments), 3)
        job = json.loads((self.root / "B.job.json").read_text())
        self.assertTrue(job["clip_recovery"]["resume"])
        self.assertNotIn("checkpoint", job["options"])
        self.assertNotIn("clip_checkpoint_dir", job["options"])
        self.assertEqual(self.completed_document()["plan"]["options"]["clip_checkpoint_dir"], str(self.state))
        plan = json.loads((self.state / "plan.json").read_text())
        self.assertEqual(plan["plan"]["completion_checkpoint"], str(self.checkpoint))

    def test_completed_pairs_skip_capture_and_unfinished_pairs_finish_both_outputs(self):
        self.behavior(B="fail")
        first = self.render(save_last_frame=True)
        self.assertFalse(first.ok)
        image = self.output / "A/clips.png"
        clip = self.output / "B/clips/segment-000000.y4m"
        saved = self.signature(image), self.signature(clip)
        self.behavior()
        result = self.render(resume=True, save_last_frame=True)
        self.assertTrue(result.ok, result.as_dict())
        self.assertEqual(result.reused, ("A",))
        self.assertEqual((self.calls("A"), self.calls("B")), (1, 2))
        self.assertEqual((self.signature(image), self.signature(clip)), saved)
        self.assertTrue(result.outcomes[1].result.completed)
        self.assertTrue((self.output / "B/clips.png").is_file())

    def test_complete_batch_starts_no_workers_and_reports_reuse_in_input_order(self):
        self.assertTrue(self.render(("B", "A")).ok)
        observed = []
        with patch.object(parallel_batch, "run_process_jobs") as workers:
            result = self.render(("B", "A"), resume=True,
                                 on_result=lambda outcome: observed.append(outcome.name))
        workers.assert_not_called()
        self.assertEqual(result.reused, ("B", "A"))
        self.assertEqual(observed, ["B", "A"])
        self.assertEqual((self.calls("A"), self.calls("B")), (1, 1))
        self.assertTrue(result.ok)

    def test_fail_fast_retry_keeps_completed_partial_and_unstarted_jobs_distinct(self):
        self.behavior(B="fail")
        with self.assertRaises(BatchRenderError) as failed:
            self.render(("A", "B", "C"), workers=1, continue_on_error=False)
        self.assertEqual([row.status for row in failed.exception.result.outcomes],
                         ["succeeded", "failed", "not_run"])
        self.assertEqual(self.calls("C"), 0)
        self.behavior()
        resumed = self.render(("A", "B", "C"), workers=2, resume=True)
        self.assertTrue(resumed.ok)
        self.assertEqual(resumed.reused, ("A",))
        self.assertEqual([self.calls(name) for name in ("A", "B", "C")], [1, 2, 1])
        for name, saved in (("B", True), ("C", False)):
            job = json.loads((self.root / (name + ".job.json")).read_text())
            self.assertIs(job["clip_recovery"]["resume"], saved)

    def test_worker_death_without_terminal_receipt_does_not_guess_scene_success(self):
        self.behavior(B="terminal-crash")
        first = self.render(save_last_frame=True)
        self.assertEqual([row.status for row in first.outcomes], ["succeeded", "failed"])
        image = self.output / "B/clips.png"
        saved = self.signature(image)
        self.behavior()
        result = self.render(save_last_frame=True, resume=True)
        self.assertTrue(result.ok)
        self.assertEqual(result.reused, ("A",))
        self.assertEqual((self.calls("A"), self.calls("B")), (1, 2))
        self.assertEqual(self.signature(image), saved)
        self.assertNotIn("capture", json.loads((self.root / "B.events.json").read_text()))

    def test_completed_artifact_tampering_stops_all_pending_admission(self):
        self.behavior(B="fail")
        self.render()
        path = self.output / "A/clips/segment-000000.y4m"
        path.write_bytes(b"tampered")
        before = self.signature(self.checkpoint), self.signature(self.state / "plan.json")
        self.behavior()
        with patch.object(parallel_batch, "run_process_jobs") as workers, self.assertRaises(ValueError):
            self.render(resume=True)
        workers.assert_not_called()
        self.assertEqual((self.calls("A"), self.calls("B")), (1, 1))
        self.assertEqual((self.signature(self.checkpoint), self.signature(self.state / "plan.json")), before)
        self.assertEqual(path.read_bytes(), b"tampered")

    def test_completed_image_tampering_stops_all_pending_admission(self):
        self.behavior(B="fail")
        self.render(save_last_frame=True)
        path = self.output / "A/clips.png"
        path.write_bytes(b"tampered")
        self.behavior()
        with patch.object(parallel_batch, "run_process_jobs") as workers, self.assertRaises(ValueError):
            self.render(resume=True, save_last_frame=True)
        workers.assert_not_called()
        self.assertEqual((self.calls("A"), self.calls("B")), (1, 1))
        self.assertEqual(path.read_bytes(), b"tampered")

    def test_unjournaled_artifacts_are_not_adopted_in_either_level(self):
        self.behavior(B="fail")
        self.render()
        path = self.output / "B/clips/foreign.y4m"
        path.write_bytes(b"keep foreign output")
        self.behavior()
        result = self.render(resume=True)
        self.assertEqual(result.reused, ("A",))
        self.assertEqual([row.status for row in result.outcomes], ["succeeded", "failed"])
        self.assertEqual(path.read_bytes(), b"keep foreign output")
        self.assertEqual(self.calls("A"), 1)

    def test_completion_observer_sees_durable_success_before_raising(self):
        def observe(row):
            document = self.completed_document()
            stored = next(item for item in document["outcomes"] if item["name"] == row.name)
            self.assertEqual(stored["status"], "succeeded")
            self.assertTrue((self.state / "jobs" / (row.name + ".json")).is_file())
            raise LookupError("observer failed")
        with self.assertRaisesRegex(LookupError, "observer failed") as error:
            self.render(workers=1, on_result=observe)
        self.assertEqual(error.exception.render_batch_result.outcomes[0].status, "succeeded")
        result = self.render(resume=True)
        self.assertTrue(result.ok)
        self.assertEqual(result.reused, ("A",))
        self.assertEqual((self.calls("A"), self.calls("B")), (1, 1))

    def test_cancellation_in_reused_observer_preserves_success_and_releases_both_locks(self):
        self.behavior(B="fail")
        self.render()
        self.behavior()
        token = CancellationToken()
        with patch.object(parallel_batch, "run_process_jobs") as workers, self.assertRaises(BatchCancelled) as error:
            self.render(resume=True, cancellation=token, on_result=lambda row: token.cancel())
        workers.assert_not_called()
        self.assertEqual(error.exception.render_batch_result.reused, ("A",))
        self.assertEqual([row.status for row in error.exception.render_batch_result.outcomes],
                         ["succeeded", "not_run"])
        result = self.render(resume=True, cancellation=CancellationToken())
        self.assertTrue(result.ok)
        self.assertEqual((self.calls("A"), self.calls("B")), (1, 2))

    def test_completion_write_failure_keeps_prefix_and_does_not_fabricate_reuse(self):
        publish = BatchCheckpoint._publish
        def refuse_success(owner, payload):
            if any(row["status"] == "succeeded" for row in json.loads(payload)["outcomes"]):
                raise OSError("completion journal unavailable")
            return publish(owner, payload)
        with patch.object(BatchCheckpoint, "_publish", refuse_success), \
                self.assertRaisesRegex(OSError, "completion journal unavailable") as error:
            self.render(("A",), workers=1)
        self.assertEqual(error.exception.render_batch_result.outcomes[0].status, "succeeded")
        self.assertEqual(self.completed_document()["outcomes"][0]["status"], "not_run")
        clip = self.output / "A/clips/segment-000000.y4m"
        saved = self.signature(clip)
        result = self.render(("A",), resume=True)
        self.assertTrue(result.ok)
        self.assertEqual(result.reused, ())
        self.assertEqual(self.calls("A"), 2)
        self.assertEqual(self.signature(clip), saved)

    def test_recovery_namespaces_are_disjoint_before_either_owner_opens(self):
        paths = [self.state, self.state / "plan.json", self.state / "plan.json.lock",
                 self.state / "jobs/A.json", self.state / "other.json",
                 self.output / "A/clips/journal.json", self.root]
        for path in paths:
            with self.subTest(path=path), patch.object(parallel_batch, "run_process_jobs") as workers, \
                    self.assertRaises((ValueError, OSError)):
                self.render(checkpoint=path)
            workers.assert_not_called()
        self.assertFalse(self.state.exists())
        self.assertFalse(self.checkpoint.exists())
        self.assertFalse(self.output.exists())

    def test_copied_or_removed_recovery_mode_is_refused(self):
        self.render()
        copied = self.root / "copy.json"
        copied.write_bytes(self.checkpoint.read_bytes())
        for options in ({"checkpoint": copied}, {"checkpoint": None}, {"clip_checkpoint_dir": None}):
            with self.subTest(options=options), patch.object(parallel_batch, "run_process_jobs") as workers, \
                    self.assertRaises(ValueError):
                self.render(resume=True, **options)
            workers.assert_not_called()
        self.assertEqual((self.calls("A"), self.calls("B")), (1, 1))

    def test_redirected_prefix_binding_is_refused_even_when_all_outputs_match(self):
        self.render()
        document = self.completed_document()
        document["plan"]["options"]["clip_checkpoint_dir"] = str(self.root / "other-state")
        self.checkpoint.write_text(json.dumps(document))
        with patch.object(parallel_batch, "run_process_jobs") as workers, self.assertRaisesRegex(ValueError, "plan"):
            self.render(resume=True)
        workers.assert_not_called()
        self.assertEqual(self.calls("A"), 1)

    def test_missing_completion_journal_is_not_reconstructed_from_directory_names(self):
        self.render()
        saved = self.root / "retained-completion.json"
        self.checkpoint.rename(saved)
        with patch.object(parallel_batch, "run_process_jobs") as workers, self.assertRaises(FileNotFoundError):
            self.render(resume=True)
        workers.assert_not_called()
        self.assertFalse(self.checkpoint.exists())
        self.assertTrue(saved.is_file())
        with ParallelClipPlan(self.state, resume=True, key="inputs-v1"):
            pass

    def test_both_journal_locks_exclude_a_second_coordinator(self):
        self.render()
        owners = [BatchCheckpoint(self.checkpoint, resume=True, key="inputs-v1"),
                  ParallelClipPlan(self.state, resume=True, key="inputs-v1")]
        for owner in owners:
            with self.subTest(owner=type(owner).__name__), owner, \
                    patch.object(parallel_batch, "run_process_jobs") as workers, self.assertRaises(OSError):
                self.render(resume=True)
            workers.assert_not_called()
        self.assertEqual(self.render(resume=True).reused, ("A", "B"))

    def test_variants_and_type_sensitive_constructor_inputs_keep_both_plans(self):
        jobs = [SourceRenderJob("A", "Lesson", {"value": True}),
                SourceRenderJob("B", "Lesson", {"value": 2})]
        self.behavior(B="fail")
        self.render(jobs)
        self.behavior()
        changed = [SourceRenderJob("A", "Lesson", {"value": 1}), jobs[1]]
        with patch.object(parallel_batch, "run_process_jobs") as workers, self.assertRaises(ValueError):
            self.render(changed, resume=True)
        workers.assert_not_called()
        result = self.render(jobs, resume=True)
        self.assertTrue(result.ok)
        self.assertEqual(result.reused, ("A",))
        self.assertEqual((self.calls("A"), self.calls("B")), (1, 2))

    def test_changed_source_and_render_options_are_refused_before_reuse_or_workers(self):
        self.render()
        original = self.source.read_text()
        self.source.write_text(original + "# changed\n")
        with patch.object(parallel_batch, "run_process_jobs") as workers, self.assertRaises(ValueError):
            self.render(resume=True)
        workers.assert_not_called()
        self.source.write_text(original)
        with patch.object(parallel_batch, "run_process_jobs") as workers, self.assertRaises(ValueError):
            self.render(resume=True, fps=9)
        workers.assert_not_called()
        self.assertEqual((self.calls("A"), self.calls("B")), (1, 1))


if __name__ == "__main__":
    unittest.main(verbosity=2)
