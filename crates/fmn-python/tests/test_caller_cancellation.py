"""Caller cancellation through real spawned processes and the production wire.

No native wheel or renderer is involved. Workers below model cooperative work,
published receipts and blocked teardown; the supervisor and process groups are
production code. No global parent signals, fake clocks or mock IPC are used.
"""
from contextlib import contextmanager
import json
import multiprocessing as mp
from multiprocessing.util import Finalize
import os
from pathlib import Path
import signal
import struct
import subprocess
import sys
import tempfile
from threading import Thread
import time
import unittest
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "python"))
from fmn_python import _process_batch as scheduler
from fmn_python.cancellation import BatchCancelled, CancellationToken


def wait_for(predicate, timeout=12):
    deadline = time.monotonic() + timeout
    while not predicate():
        if time.monotonic() >= deadline:
            raise AssertionError("worker synchronization deadline exceeded")
        time.sleep(0.01)


@contextmanager
def request_when(token, predicate):
    failures = []
    def request():
        try:
            wait_for(predicate)
        except BaseException as error:
            failures.append(error)
        finally:
            token.cancel()
    thread = Thread(target=request, daemon=True)
    thread.start()
    try:
        yield
    finally:
        thread.join(14)
        if thread.is_alive():
            raise AssertionError("cancellation requester did not finish")
        if failures:
            raise failures[0]


def _teardown(root, name):
    signal.signal(signal.SIGINT, signal.SIG_IGN)
    (Path(root) / (name + ".teardown")).write_text("waiting")
    time.sleep(30)


def cancellable_worker(job):
    root, name = Path(job["root"]), job["name"]
    mode = job.get("mode", "immediate")
    # A token must never be pickled into a child or smuggled into its JSON job.
    assert "cancellation" not in job
    if mode in {"ignore", "timeout", "protocol_failure"}:
        def ignore(signum, frame):
            (root / (name + ".interrupted")).write_text("SIGINT")
        signal.signal(signal.SIGINT, ignore)
    if mode == "descendant":
        ready = root / (name + ".descendant-ready")
        child = subprocess.Popen([sys.executable, "-S", "-c", "import signal,time,sys; "
                                 "from pathlib import Path; "
                                 "signal.signal(signal.SIGINT,signal.SIG_IGN); "
                                 "Path(sys.argv[1]).write_text('ready'); time.sleep(30)", str(ready)])
        (root / (name + ".descendant")).write_text(str(child.pid))
        wait_for(ready.exists)
    (root / (name + ".ready")).write_text(str(os.getpid()))
    result = {"name": name, "status": "succeeded", "error": None,
              "result": {"value": job.get("value", name)}}
    if mode == "protocol_failure":
        def malformed(connection, data):
            connection.sendall(struct.pack("!I", 0))
            time.sleep(30)
        scheduler.send_receipt = malformed
        return result
    if mode == "failure":
        return {**result, "status": "failed", "result": None,
                "error": {"type": "ValueError", "message": "authored failure", "notes": []}}
    if mode in {"success_teardown", "failure_teardown", "large_receipt"}:
        if mode == "failure_teardown":
            result.update(status="failed", result=None,
                          error={"type": "ValueError", "message": "authored failure", "notes": []})
        if mode == "large_receipt":
            result["result"]["value"] = "x" * (2 * 1024 * 1024)
        Finalize(None, _teardown, args=(str(root), name), exitpriority=20)
        return result
    if mode == "publish_then_wait":
        (root / (name + ".output")).write_bytes(b"complete publication")
    if mode in {"cooperate", "publish_then_wait", "ignore", "timeout", "descendant"}:
        try:
            time.sleep(30)
        except KeyboardInterrupt:
            (root / (name + ".cleaned")).write_text("cleanup")
            if mode == "publish_then_wait":
                return result
            return {**result, "status": "cancelled",
                    "result": {"partial": ["already-published-clip"]},
                    "error": {"type": "KeyboardInterrupt", "message": "worker interrupted", "notes": []}}
    return result


class CallerCancellationTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name).resolve()
        self.token = CancellationToken()
        self.before_children = {child.pid for child in mp.active_children()}
        self.sigint = signal.getsignal(signal.SIGINT)

    def tearDown(self):
        self.assertEqual({child.pid for child in mp.active_children()}, self.before_children)
        self.assertIs(signal.getsignal(signal.SIGINT), self.sigint)

    def job(self, name, mode="immediate"):
        return {"name": name, "mode": mode, "root": str(self.root)}

    def run_jobs(self, jobs, **options):
        return scheduler.run_process_jobs(jobs, cancellable_worker,
                                         **{"workers": 1, "cancellation": self.token, **options})

    def test_pre_cancelled_plan_has_only_not_run_rows_and_launches_nothing(self):
        self.token.cancel()
        observer = Mock()
        with patch.object(scheduler.mp, "get_context", side_effect=AssertionError("no startup")):
            with self.assertRaises(BatchCancelled) as caught:
                self.run_jobs([self.job("one"), self.job("two")], on_result=observer)
        self.assertEqual([row["status"] for row in caught.exception.process_batch_outcomes],
                         ["not_run", "not_run"])
        observer.assert_not_called()
        self.assertEqual(list(self.root.iterdir()), [])

    def test_token_is_one_way_thread_safe_and_independent_of_other_runs(self):
        threads = [Thread(target=self.token.cancel) for _ in range(16)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join()
        self.assertTrue(self.token.cancelled)
        with self.assertRaises(BatchCancelled):
            self.token.raise_if_cancelled()
        fresh = CancellationToken()
        rows = self.run_jobs([self.job("fresh")], cancellation=fresh)
        self.assertEqual(rows[0]["status"], "succeeded")
        self.assertFalse(fresh.cancelled)

    def test_invalid_tokens_refuse_before_process_startup(self):
        with patch.object(scheduler.mp, "get_context", side_effect=AssertionError("no startup")):
            for token in (False, True, 1, object(), lambda: True):
                with self.subTest(token=token), self.assertRaises(TypeError):
                    self.run_jobs([self.job("one")], cancellation=token)

    def test_observer_cancels_before_later_admission_even_with_keep_going(self):
        seen = []
        def observer(index, row):
            seen.append((index, row["status"]))
            self.token.cancel()
        with self.assertRaises(BatchCancelled) as caught:
            self.run_jobs([self.job("one"), self.job("two"), self.job("three")],
                          continue_on_error=True, on_result=observer)
        self.assertEqual(seen, [(0, "succeeded")])
        self.assertEqual([row["status"] for row in caught.exception.process_batch_outcomes],
                         ["succeeded", "not_run", "not_run"])
        self.assertFalse((self.root / "two.ready").exists())

    def test_request_during_admission_reaps_the_new_worker_before_next_launch(self):
        real = mp.get_context("spawn")
        started = []
        token = self.token
        class Process:
            def __init__(self, **options):
                self.owner = real.Process(**options)
            def __getattr__(self, name):
                return getattr(self.owner, name)
            def start(self):
                self.owner.start()
                started.append(self.owner.pid)
                token.cancel()
        class Context:
            pass
        context = Context()
        context.Process = Process
        with patch.object(scheduler.mp, "get_context", return_value=context):
            with self.assertRaises(BatchCancelled) as caught:
                self.run_jobs([self.job("one", "cooperate"), self.job("two")], workers=2)
        self.assertEqual(len(started), 1)
        self.assertEqual(caught.exception.process_batch_outcomes[1]["status"], "not_run")
        self.assertFalse((self.root / "two.ready").exists())

    def test_final_observer_request_still_returns_cancellation_with_completed_receipts(self):
        with self.assertRaises(BatchCancelled) as caught:
            self.run_jobs([self.job("one")], on_result=lambda *_: self.token.cancel())
        self.assertEqual(caught.exception.process_batch_outcomes[0]["status"], "succeeded")

    def test_observer_exception_is_not_replaced_by_simultaneous_stop_request(self):
        primary = LookupError("keep the first failure")
        def observer(*_):
            self.token.cancel()
            raise primary
        with self.assertRaises(LookupError) as caught:
            self.run_jobs([self.job("one"), self.job("two")], on_result=observer)
        self.assertIs(caught.exception, primary)
        self.assertEqual([row["status"] for row in primary.process_batch_outcomes],
                         ["succeeded", "not_run"])

    def test_plain_keyboard_interrupt_uses_existing_cleanup_without_becoming_token_cancellation(self):
        primary = KeyboardInterrupt("user interrupt")
        def observer(*_):
            raise primary
        with self.assertRaises(KeyboardInterrupt) as caught:
            self.run_jobs([self.job("one"), self.job("two")], on_result=observer)
        self.assertIs(caught.exception, primary)
        self.assertFalse(isinstance(caught.exception, BatchCancelled))
        self.assertFalse(self.token.cancelled)

    @unittest.skipUnless(os.name == "posix", "cooperative worker SIGINT is POSIX-specific")
    def test_cooperative_stop_retains_partial_receipt_and_reaps_before_raising(self):
        with request_when(self.token, lambda: (self.root / "slow.ready").exists()):
            with self.assertRaises(BatchCancelled) as caught:
                self.run_jobs([self.job("slow", "cooperate"), self.job("later")])
        row = caught.exception.process_batch_outcomes[0]
        self.assertEqual(row["status"], "cancelled")
        self.assertEqual(row["result"]["partial"], ["already-published-clip"])
        self.assertTrue((self.root / "slow.cleaned").exists())
        self.assertFalse((self.root / "later.ready").exists())

    @unittest.skipUnless(os.name == "posix", "cooperative worker SIGINT is POSIX-specific")
    def test_success_returned_during_cleanup_remains_a_success(self):
        with request_when(self.token, lambda: (self.root / "done.output").exists()):
            with self.assertRaises(BatchCancelled) as caught:
                self.run_jobs([self.job("done", "publish_then_wait")])
        self.assertEqual(caught.exception.process_batch_outcomes[0]["status"], "succeeded")
        self.assertEqual((self.root / "done.output").read_bytes(), b"complete publication")

    @unittest.skipUnless(os.name == "posix", "signals and native process teardown are POSIX-specific")
    def test_forced_stops_escalate_concurrently_without_fabricating_render_failures(self):
        jobs = [self.job("a", "ignore"), self.job("b", "ignore"), self.job("later")]
        with request_when(self.token, lambda: all((self.root / (name + ".ready")).exists()
                                                  for name in ("a", "b"))):
            before = time.monotonic()
            with self.assertRaises(BatchCancelled) as caught:
                self.run_jobs(jobs, workers=2, continue_on_error=True)
            elapsed = time.monotonic() - before
        rows = caught.exception.process_batch_outcomes
        self.assertEqual([row["status"] for row in rows], ["cancelled", "cancelled", "not_run"])
        self.assertTrue(all(row["error"]["type"] == "BatchCancelled" for row in rows[:2]))
        self.assertLess(elapsed, 8, "cleanup must not multiply per-worker grace periods")
        self.assertFalse((self.root / "later.ready").exists())

    @unittest.skipUnless(os.name == "posix", "blocked interpreter teardown uses POSIX signals")
    def test_received_success_and_failure_survive_forced_teardown(self):
        for mode, status in (("success_teardown", "succeeded"), ("failure_teardown", "failed")):
            token = CancellationToken()
            with self.subTest(mode=mode), request_when(token, lambda: (self.root / (mode + ".teardown")).exists()):
                with self.assertRaises(BatchCancelled) as caught:
                    self.run_jobs([self.job(mode, mode)], cancellation=token, continue_on_error=True)
            row = caught.exception.process_batch_outcomes[0]
            self.assertEqual(row["status"], status)
            if status == "failed":
                self.assertEqual(row["error"]["type"], "ValueError")
                self.assertEqual(row["error"]["message"], "authored failure")

    @unittest.skipUnless(os.name == "posix", "blocked interpreter teardown uses POSIX signals")
    def test_large_complete_receipt_is_not_discarded_during_cancellation(self):
        with request_when(self.token, lambda: (self.root / "large.teardown").exists()):
            with self.assertRaises(BatchCancelled) as caught:
                self.run_jobs([self.job("large", "large_receipt")])
        row = caught.exception.process_batch_outcomes[0]
        self.assertEqual(row["status"], "succeeded")
        self.assertEqual(row["result"]["value"], "x" * (2 * 1024 * 1024))

    @unittest.skipUnless(os.name == "posix", "timeout/interrupt ordering uses POSIX signals")
    def test_previously_observed_timeout_keeps_its_error_identity(self):
        with request_when(self.token, lambda: (self.root / "slow.interrupted").exists()):
            with self.assertRaises(BatchCancelled) as caught:
                self.run_jobs([self.job("slow", "timeout")], job_timeout=2)
        row = caught.exception.process_batch_outcomes[0]
        self.assertEqual(row["status"], "failed")
        self.assertEqual(row["error"]["type"], "TimeoutError")

    @unittest.skipUnless(os.name == "posix", "protocol-failure cleanup uses POSIX signals")
    def test_protocol_failure_observed_before_cancellation_remains_a_failure(self):
        with request_when(self.token, lambda: (self.root / "bad.interrupted").exists()):
            with self.assertRaises(BatchCancelled) as caught:
                self.run_jobs([self.job("bad", "protocol_failure")], continue_on_error=True)
        row = caught.exception.process_batch_outcomes[0]
        self.assertEqual(row["status"], "failed")
        self.assertEqual(row["error"]["type"], "RuntimeError")
        self.assertIn("receipt length", row["error"]["message"])

    @unittest.skipUnless(os.name == "posix" and Path("/proc").is_dir(),
                         "private process-group liveness check uses Linux procfs")
    def test_owned_descendants_are_stopped_without_signalling_parent_group(self):
        with request_when(self.token, lambda: (self.root / "tree.ready").exists()):
            with self.assertRaises(BatchCancelled):
                self.run_jobs([self.job("tree", "descendant")])
        pid = int((self.root / "tree.descendant").read_text())
        def no_longer_running():
            try:
                state = Path(f"/proc/{pid}/stat").read_text().rpartition(")")[2].split()[0]
                return state == "Z"
            except FileNotFoundError:
                return True
        wait_for(no_longer_running)


if __name__ == "__main__":
    unittest.main()
