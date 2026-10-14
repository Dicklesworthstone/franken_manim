"""Real spawned-process tests; no native extension or render stubs required."""
import multiprocessing as mp
import os
from pathlib import Path
import sys
import time
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "python"))
from fmn_python._process_batch import run_process_jobs

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
