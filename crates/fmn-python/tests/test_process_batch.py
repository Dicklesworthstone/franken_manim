"""Real spawned-process tests; no native extension or render stubs required."""
import multiprocessing as mp
from functools import partial
import os
import signal
import socket
from pathlib import Path
import sys
import time
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


def slow_teardown(channel):
    channel.sendall(b"teardown\n")
    time.sleep(60)


def interrupted_job_with_teardown(options):
    channel = socket.create_connection(tuple(options["address"]))
    try:
        time.sleep(options.get("entry_delay", 0))
        channel.sendall(b"ready\n")
        time.sleep(60)
    except KeyboardInterrupt:
        if options.get("teardown_before_receipt"):
            slow_teardown(channel)
        if not _defer_until_receipted(partial(slow_teardown, channel)):
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
        self._assert_cancellation_receipt(entry_delay=0)

    @unittest.skipUnless(os.name == "posix", "requires cooperative SIGINT cancellation")
    def test_delayed_worker_entry_does_not_consume_receipt_shutdown_budget(self):
        self._assert_cancellation_receipt(entry_delay=6)

    @unittest.skipUnless(os.name == "posix", "requires cooperative SIGINT cancellation")
    def test_receipt_oracle_rejects_teardown_before_receipt(self):
        with self.assertRaisesRegex(AssertionError, "KeyboardInterrupt|owned receipt"):
            self._assert_cancellation_receipt(entry_delay=0, teardown_before_receipt=True)

    def _assert_cancellation_receipt(self, *, entry_delay, teardown_before_receipt=False):
        # Test coordination belongs on an IPC channel: rendering in parallel
        # must not delay creation of a filesystem sentinel past worker cleanup.
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            listener.listen(1)
            listener.settimeout(0.05)
            token, finished, errors = CancellationToken(), Event(), []
            cancelled_at, events = [], []
            def cancel_entered_worker():
                # Startup is a prerequisite for this receipt-after-cancellation
                # test. Bound it separately from the shutdown being measured.
                deadline = time.monotonic() + 30
                while time.monotonic() < deadline and not finished.is_set():
                    try:
                        channel, _ = listener.accept()
                        break
                    except TimeoutError:
                        continue
                else:
                    if finished.is_set():
                        return
                    errors.append("worker did not enter before the readiness watchdog")
                    token.cancel()
                    return
                with channel:
                    channel.settimeout(0.05)
                    buffered = b""
                    while time.monotonic() < deadline:
                        try:
                            data = channel.recv(64)
                        except TimeoutError:
                            if finished.is_set():
                                break
                            continue
                        if not data:
                            break
                        buffered += data
                        while b"\n" in buffered:
                            event, buffered = buffered.split(b"\n", 1)
                            events.append(event)
                            if event == b"ready":
                                cancelled_at.append(time.monotonic())
                                token.cancel()
                            elif event == b"teardown":
                                return
                    if b"ready" not in events:
                        errors.append("worker did not enter before the readiness watchdog")
                        token.cancel()
            observer = Thread(target=cancel_entered_worker)
            observer.start()
            try:
                with self.assertRaises(BatchCancelled) as caught:
                    run_process_jobs([{"address": listener.getsockname(), "entry_delay": entry_delay,
                                       "teardown_before_receipt": teardown_before_receipt}],
                                     interrupted_job_with_teardown, workers=1, cancellation=token)
                returned_at = time.monotonic()
            finally:
                finished.set()
                observer.join()
            self.assertEqual(errors, [])
            self.assertEqual(len(cancelled_at), 1)
            self.assertLess(returned_at - cancelled_at[0], 5,
                            "receipt shutdown exceeded its budget after worker readiness")
            rows = caught.exception.process_batch_outcomes
            self.assertEqual(events, [b"ready", b"teardown"],
                             "the real deferred callback did not execute")
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
