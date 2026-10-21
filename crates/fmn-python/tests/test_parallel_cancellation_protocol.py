"""Caller cancellation, checkpoint recovery and isolated discovery composition.

Real process supervision, JSON receipts, journal locks and artifact hashes run.
The existing byte-artifact fixture replaces native capture/Scene classes; its
outputs are NOT FMTL. Discovery deliberately blocks in its own real process.
"""
from contextlib import nullcontext
import hashlib
import json
import os
from pathlib import Path
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

from test_caller_cancellation import request_when
from test_parallel_bundle_protocol import host_fixture, publish_bundle_job
from fmn_python import parallel_discovery as discovery
from fmn_python.batch_checkpoint import BatchCheckpoint
from fmn_python.cancellation import BatchCancelled, CancellationToken


def discovery_job(job):
    root = Path(job["source"]).parent
    (root / "discovery.ready").write_text(str(os.getpid()))
    if (root / "block-discovery").exists():
        time.sleep(30)
    return {"status": "succeeded", "error": None, "result": {
        "schema": "fmn-python.scene-discovery", "version": 1,
        "source": job["source"], "source_digest": job["source_digest"],
        "scenes": ["Alpha", "Beta", "Later"],
    }}


class ParallelCancellationTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name).resolve()
        self.source = self.root / "scenes.py"
        self.source.write_text("# stable entry source; byte fixture does not execute it\n")
        self.output = self.root / "output"
        self.journal = self.root / "progress.json"
        (self.root / "behavior.json").write_text("{}")
        fixture = host_fixture()
        self.h = fixture.__enter__()
        self.addCleanup(fixture.__exit__, None, None, None)
        self.parallel = self.h.coordinator
        self.parallel._render_source_job = publish_bundle_job
        self.token = CancellationToken()

    def render(self, names=("Alpha", "Beta"), **extra):
        options = dict(workers=1, format="fmtl", resolution=(64, 36), fps=24,
                       checkpoint=self.journal, resume_key="stable-assets",
                       cancellation=self.token)
        options.update(extra)
        return self.parallel.render_source_scenes(self.source, self.output, names, **options)

    def attempts(self, name):
        path = self.root / (name + ".attempts")
        return path.read_text().splitlines() if path.exists() else []

    def test_pre_cancelled_render_does_not_read_source_open_journal_or_start_workers(self):
        self.token.cancel()
        with (patch.object(self.parallel, "_source_digest", side_effect=AssertionError("no source read")),
              patch.object(self.parallel, "BatchCheckpoint", side_effect=AssertionError("no journal")),
              patch.object(self.parallel, "run_process_jobs", side_effect=AssertionError("no workers"))):
            with self.assertRaises(BatchCancelled):
                self.render()
        self.assertFalse(self.output.exists())
        self.assertFalse(self.journal.exists())

    def test_observer_cancellation_keeps_durable_receipt_and_resume_runs_only_pending(self):
        def observer(row):
            saved = json.loads(self.journal.read_text())
            self.assertEqual(saved["outcomes"][0]["status"], "succeeded")
            self.assertIn("Alpha", saved["artifacts"])
            self.token.cancel()
        with self.assertRaises(BatchCancelled) as caught:
            self.render(on_result=observer, continue_on_error=True)
        result = caught.exception.render_batch_result
        self.assertEqual([row.status for row in result.outcomes], ["succeeded", "not_run"])
        artifact = self.output / "Alpha.fmtl"
        previous = artifact.read_bytes(), artifact.stat().st_mtime_ns
        resumed = self.render(resume=True, workers=2, cancellation=CancellationToken())
        self.assertTrue(resumed.ok)
        self.assertEqual(resumed.reused, ("Alpha",))
        self.assertEqual([len(self.attempts(name)) for name in ("Alpha", "Beta")], [1, 1])
        self.assertEqual((artifact.read_bytes(), artifact.stat().st_mtime_ns), previous)
        document = json.loads(self.journal.read_text())
        self.assertNotIn("cancellation", document["plan"]["options"])
        for name in ("Alpha", "Beta"):
            job = json.loads((self.root / (name + ".job.json")).read_text())
            self.assertNotIn("cancellation", job)
            self.assertNotIn("cancellation", job["options"])

    def test_cancelled_active_worker_is_journalled_and_retry_uses_fresh_token(self):
        (self.root / "behavior.json").write_text('{"Beta":"sleep"}')
        with request_when(self.token, lambda: (self.root / "Beta.job.json").exists()):
            with self.assertRaises(BatchCancelled) as caught:
                self.render(("Alpha", "Beta", "Later"), continue_on_error=True)
        report = caught.exception.render_batch_result
        self.assertEqual([row.status for row in report.outcomes], ["succeeded", "cancelled", "not_run"])
        self.assertEqual([row["status"] for row in json.loads(self.journal.read_text())["outcomes"]],
                         ["succeeded", "cancelled", "not_run"])
        self.assertEqual(self.attempts("Later"), [])
        (self.root / "behavior.json").write_text("{}")
        resumed = self.render(("Alpha", "Beta", "Later"), resume=True, workers=2,
                              cancellation=CancellationToken())
        self.assertTrue(resumed.ok)
        self.assertEqual(resumed.reused, ("Alpha",))
        self.assertEqual([len(self.attempts(name)) for name in ("Alpha", "Beta", "Later")], [1, 2, 1])

    def test_cancelling_reuse_observer_preserves_all_verified_successes(self):
        initial = self.render(cancellation=None)
        previous = [(row.destination.read_bytes(), row.destination.stat().st_mtime_ns)
                    for row in initial.outcomes]
        seen = []
        def observer(row):
            seen.append(row.name)
            self.token.cancel()
        with patch.object(self.parallel, "run_process_jobs", side_effect=AssertionError("no new workers")):
            with self.assertRaises(BatchCancelled) as caught:
                self.render(resume=True, on_result=observer)
        self.assertEqual(seen, ["Alpha"])
        self.assertTrue(caught.exception.render_batch_result.ok)
        self.assertEqual(caught.exception.render_batch_result.reused, ("Alpha", "Beta"))
        self.assertEqual([(row.destination.read_bytes(), row.destination.stat().st_mtime_ns)
                          for row in caught.exception.render_batch_result.outcomes], previous)

    def test_checkpoint_write_failure_during_cancellation_preserves_primary_and_releases_lock(self):
        original = BatchCheckpoint.record
        def record(owner, outcomes):
            if self.token.cancelled:
                raise OSError("journal storage unavailable")
            return original(owner, outcomes)
        with patch.object(BatchCheckpoint, "record", record):
            with self.assertRaises(BatchCancelled) as caught:
                self.render(on_result=lambda _: self.token.cancel())
        self.assertEqual([row.status for row in caught.exception.render_batch_result.outcomes],
                         ["succeeded", "not_run"])
        self.assertTrue(any("journal storage unavailable" in note for note in caught.exception.__notes__))
        resumed = self.render(resume=True, cancellation=CancellationToken())
        self.assertTrue(resumed.ok)
        self.assertEqual(resumed.reused, ("Alpha",))

    def test_cancelled_run_does_not_weaken_checkpoint_artifact_verification(self):
        with self.assertRaises(BatchCancelled):
            self.render(on_result=lambda _: self.token.cancel())
        path = self.output / "Alpha.fmtl"
        with path.open("ab") as stream:
            stream.write(b"tampered")
        with patch.object(self.parallel, "run_process_jobs", side_effect=AssertionError("no new workers")):
            with self.assertRaisesRegex(ValueError, "missing or modified"):
                self.render(resume=True, cancellation=CancellationToken())
        self.assertEqual(self.attempts("Beta"), [])

    def test_invalid_token_is_rejected_even_on_an_all_completed_resume(self):
        self.render(cancellation=None)
        before = self.journal.read_bytes()
        with patch.object(self.parallel, "run_process_jobs", side_effect=AssertionError("no new workers")):
            with self.assertRaises(TypeError):
                self.render(resume=True, cancellation=lambda: True)
        self.assertEqual(self.journal.read_bytes(), before)

    def test_discovery_pre_cancellation_never_reads_entry_source(self):
        self.token.cancel()
        with patch.object(discovery, "_entry_digest", side_effect=AssertionError("no source read")):
            with self.assertRaises(BatchCancelled):
                discovery.discover_source_scenes(self.root / "absent.py", cancellation=self.token)
        self.assertFalse((self.root / "discovery.ready").exists())

    def test_discovery_worker_can_be_cancelled_and_fresh_discovery_succeeds(self):
        (self.root / "block-discovery").write_text("block")
        with patch.object(discovery, "_discover_source_job", discovery_job):
            with request_when(self.token, lambda: (self.root / "discovery.ready").exists()):
                with self.assertRaises(BatchCancelled) as caught:
                    discovery.discover_source_scenes(self.source, cancellation=self.token)
            self.assertEqual(caught.exception.process_batch_outcomes[0]["status"], "cancelled")
            (self.root / "block-discovery").rename(self.root / "discovery-unblocked")
            result = discovery.discover_source_scenes(self.source, cancellation=CancellationToken())
        self.assertEqual(result.scenes, ("Alpha", "Beta", "Later"))
        self.assertEqual(result.source_digest, hashlib.sha256(self.source.read_bytes()).hexdigest())
        self.assertNotEqual(int((self.root / "discovery.ready").read_text()), os.getpid())

    def test_same_token_cancels_render_after_successful_discovery_without_starting_any_scene(self):
        with patch.object(discovery, "_discover_source_job", discovery_job):
            with discovery.discovery_source(self.source, cancellation=self.token) as result:
                self.assertIn("Alpha", result.scenes)
                self.token.cancel()
                with self.assertRaises(BatchCancelled):
                    self.render(_expected_source_digest=result.source_digest)
        self.assertFalse(self.journal.exists())
        self.assertFalse(self.output.exists())
        self.assertEqual(self.attempts("Alpha"), [])


if __name__ == "__main__":
    unittest.main()
