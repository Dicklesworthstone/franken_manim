"""Real spawned-process tests; no native extension or render stubs required."""
import multiprocessing as mp
from functools import partial
import os
import signal
from pathlib import Path
import sys
import time
import tempfile
from threading import Event, Thread
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "python"))
from fmn_python._process_batch import run_process_jobs
from fmn_python._process_batch import _defer_until_receipted
from fmn_python.cancellation import BatchCancelled, CancellationToken

_GENERATION = 0


def job(options):
    global _GENERATION
    _GENERATION += 1
    start = time.monotonic()
    time.sleep(options.get("delay", 0))
    if options.get("crash"):
        os._exit(7)
    if options.get("fail"):
        raise ValueError("authored scene failed")
    if options.get("interrupt"):
        raise KeyboardInterrupt()
    return {"status": "succeeded", "result": {
        "pid": os.getpid(), "generation": _GENERATION,
        "start": start, "end": time.monotonic(), "payload": "x" * options.get("bytes", 0)},
        "error": None}


def malformed_job(options):
    return {"status": "succeeded", "result": float("nan")}


def slow_teardown(path):
    Path(path).write_text("teardown started")
    time.sleep(60)


def interrupted_job_with_teardown(options):
    try:
        Path(options["ready"]).write_text("worker entered")
        time.sleep(60)
    except KeyboardInterrupt:
        if not _defer_until_receipted(partial(slow_teardown, options["marker"])):
            raise RuntimeError("disposable worker has no receipt owner")
        return {"status": "cancelled", "result": {"retained": "owned receipt"},
                "error": {"type": "KeyboardInterrupt", "message": "", "notes": []}}


class ProcessBatchTests(unittest.TestCase):
    def test_parallel_overlap_limit_order_and_fresh_interpreters(self):
        seen = []
        rows = run_process_jobs([{"delay": d} for d in (.25, .08, .08, .08, .08)], job,
                                workers=2, on_result=lambda i, row: seen.append(i))
        self.assertEqual([row["status"] for row in rows], ["succeeded"] * 5)
        values = [row["result"] for row in rows]
        self.assertEqual(len({value["pid"] for value in values}), 5)
        self.assertEqual([value["generation"] for value in values], [1] * 5)
        self.assertEqual(seen[0], 1)
        events = sorted([(value["start"], 1) for value in values] +
                        [(value["end"], -1) for value in values])
        active = peak = 0
        for _, change in events:
            active += change
            peak = max(peak, active)
        self.assertEqual(peak, 2)
        self.assertEqual(active, 0)

    def test_fail_fast_retains_running_success_and_stops_admission(self):
        rows = run_process_jobs([{"fail": True}, {"delay": .2}, {}, {}], job, workers=2)
        self.assertEqual([row["status"] for row in rows], ["failed", "succeeded", "not_run", "not_run"])
        self.assertEqual(rows[0]["error"]["type"], "ValueError")

    def test_keep_going_and_abrupt_worker_exit(self):
        rows = run_process_jobs([{"crash": True}, {"fail": True}, {}], job,
                                workers=2, continue_on_error=True)
        self.assertEqual([row["status"] for row in rows], ["failed", "failed", "succeeded"])
        self.assertIn("receipt", rows[0]["error"]["message"])

    def test_large_receipt_does_not_deadlock_join(self):
        rows = run_process_jobs([{"bytes": 300_000}], job, workers=1)
        self.assertEqual(len(rows[0]["result"]["payload"]), 300_000)

    def test_observer_error_preserves_success_and_reaps_workers(self):
        before = {child.pid for child in mp.active_children()}
        def observer(index, row):
            raise LookupError("observer failed")
        with self.assertRaises(LookupError) as caught:
            run_process_jobs([{}, {"delay": 10}, {}], job, workers=2, on_result=observer)
        rows = caught.exception.process_batch_outcomes
        self.assertEqual(rows[0]["status"], "succeeded")
        self.assertIn(rows[1]["status"], {"cancelled", "failed"})
        self.assertEqual(rows[2]["status"], "not_run")
        self.assertEqual({child.pid for child in mp.active_children()}, before)

    def test_child_interrupt_stops_admission_even_when_keeping_going(self):
        rows = run_process_jobs([{"interrupt": True}, {}], job,
                                workers=1, continue_on_error=True)
        self.assertEqual([row["status"] for row in rows], ["cancelled", "not_run"])

    def test_timeout_is_reported_and_does_not_admit_next_job(self):
        rows = run_process_jobs([{"delay": 10}, {}], job, workers=1, job_timeout=1.5)
        self.assertEqual(rows[0]["status"], "failed")
        self.assertEqual(rows[0]["error"]["type"], "TimeoutError")
        self.assertEqual(rows[1]["status"], "not_run")

    @unittest.skipUnless(os.name == "posix", "requires cooperative SIGINT cancellation")
    def test_cancellation_keeps_receipt_before_stalled_optional_teardown(self):
        with tempfile.TemporaryDirectory() as directory:
            marker = Path(directory) / "teardown-started"
            ready = Path(directory) / "worker-ready"
            token, finished, errors = CancellationToken(), Event(), []
            def cancel_entered_worker():
                deadline = time.monotonic() + 5
                while not ready.exists() and time.monotonic() < deadline:
                    if finished.wait(0.01):
                        return
                if not ready.is_file():
                    errors.append("worker did not enter before the readiness watchdog")
                token.cancel()
            observer = Thread(target=cancel_entered_worker)
            observer.start()
            try:
                with self.assertRaises(BatchCancelled) as caught:
                    run_process_jobs([{"marker": str(marker), "ready": str(ready)}],
                                     interrupted_job_with_teardown, workers=1, cancellation=token)
            finally:
                finished.set()
                observer.join()
            self.assertEqual(errors, [])
            rows = caught.exception.process_batch_outcomes
            self.assertTrue(marker.is_file(), "the real deferred callback did not execute")
            self.assertEqual(rows[0]["status"], "cancelled")
            self.assertEqual(rows[0]["error"]["type"], "KeyboardInterrupt")
            self.assertEqual(rows[0]["result"], {"retained": "owned receipt"})

    @unittest.skipUnless(os.name == "posix", "requires cooperative SIGINT cancellation")
    def test_worker_cancellation_restores_only_its_inherited_ignored_sigint(self):
        previous = signal.getsignal(signal.SIGINT)
        try:
            signal.signal(signal.SIGINT, signal.SIG_IGN)
            self.test_cancellation_keeps_receipt_before_stalled_optional_teardown()
            self.assertEqual(signal.getsignal(signal.SIGINT), signal.SIG_IGN)
        finally:
            signal.signal(signal.SIGINT, previous)

    def test_non_json_receipt_is_failed_without_publication_claim(self):
        rows = run_process_jobs([{}], malformed_job, workers=1)
        self.assertEqual(rows[0]["status"], "failed")
        self.assertIn("outputs were not removed", rows[0]["error"]["notes"][0])

    def test_bad_admission_never_starts_worker(self):
        for workers in (0, 65, True, 1.5):
            with self.subTest(workers=workers), self.assertRaises((ValueError, TypeError)):
                run_process_jobs([{}], job, workers=workers)
        for duration in (0, -1, True, float("nan"), float("inf")):
            with self.subTest(duration=duration), self.assertRaises((ValueError, TypeError)):
                run_process_jobs([{}], job, workers=1, job_timeout=duration)
        with self.assertRaises(ValueError):
            run_process_jobs([{"value": float("nan")}], job, workers=1)


if __name__ == "__main__":
    unittest.main()
