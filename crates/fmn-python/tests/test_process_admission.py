"""Failure-atomic admission into the real disposable-worker supervisor.

Faults are injected only at parent resource-acquisition boundaries. Workers,
private result sockets, acknowledgements, process groups and reaping are real;
no native renderer is involved. Emergency test cleanup exposes baseline leaks
without leaving the test runner's own children behind.
"""
from contextlib import contextmanager
import multiprocessing as mp
from multiprocessing.util import Finalize
import os
from pathlib import Path
import signal
import socket
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "python"))
from fmn_python import _process_batch as scheduler
from fmn_python.cancellation import BatchCancelled
from fmn_python._process_receipt import ReceiptReader


def wait_for(predicate, timeout=12):
    end = time.monotonic() + timeout
    while not predicate():
        if time.monotonic() >= end:
            raise AssertionError("worker admission synchronization timed out")
        time.sleep(0.01)


def blocked_teardown(root, name):
    signal.signal(signal.SIGINT, signal.SIG_IGN)
    (Path(root) / (name + ".published")).write_text("receipt sent")
    time.sleep(30)


def admission_worker(job):
    root, name = Path(job["root"]), job["name"]
    mode = job.get("mode", "cooperative")
    if mode == "large":
        signal.signal(signal.SIGINT, signal.SIG_IGN)
    row = {"status": "succeeded", "result": {"name": name}, "error": None}
    (root / (name + ".ready")).write_text(str(os.getpid()))
    if mode in {"success", "failure", "large", "complete", "failed_complete"}:
        if mode in {"failure", "failed_complete"}:
            row.update(status="failed", result=None,
                       error={"type": "ValueError", "message": "authored failure", "notes": []})
        if mode == "large":
            row["result"]["payload"] = "x" * (2 * 1024 * 1024)
        if mode in {"success", "failure"}:
            Finalize(None, blocked_teardown, args=(str(root), name), exitpriority=20)
        return row
    try:
        time.sleep(30)
    except KeyboardInterrupt:
        (root / (name + ".cleaned")).write_text("cooperative cleanup")
        row.update(status="cancelled", result={"partial": [name]},
                   error={"type": "KeyboardInterrupt", "message": "stopped", "notes": []})
        return row


class ProcessAdmissionTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.children = {child.pid for child in mp.active_children()}
        self.handler = signal.getsignal(signal.SIGINT)
        self.processes, self.channels = [], []
        self.real_context = mp.get_context("spawn")
        self.real_channel = scheduler.receipt_channel

    def tearDown(self):
        self.assertEqual({child.pid for child in mp.active_children()}, self.children)
        self.assertIs(signal.getsignal(signal.SIGINT), self.handler)

    def job(self, name, mode="cooperative"):
        return {"name": name, "mode": mode, "root": str(self.root)}

    def capture_channel(self):
        pair = self.real_channel()
        self.channels.append(pair)
        return pair

    @contextmanager
    def fault_context(self, *, before_start=None, after_start=None, construct=None, after_close=None):
        owner = self
        class Process:
            def __init__(self, **options):
                self.index = len(owner.processes)
                if construct is not None:
                    construct(self.index)
                self.process = owner.real_context.Process(**options)
                owner.processes.append(self)
            def __getattr__(self, name):
                return getattr(self.process, name)
            def start(self):
                if before_start is not None:
                    before_start(self.index)
                self.process.start()
                if after_start is not None:
                    after_start(self.index)
            def close(self):
                self.process.close()
                if after_close is not None:
                    after_close(self.index)
        class Context:
            pass
        context = Context()
        context.Process = Process
        try:
            with patch.object(scheduler.mp, "get_context", return_value=context), \
                    patch.object(scheduler, "receipt_channel", side_effect=self.capture_channel):
                yield
        finally:
            # Run assertions before this safety net: baseline orphans must fail.
            for wrapped in self.processes:
                process = wrapped.process
                try:
                    if process.pid is not None:
                        if process.is_alive():
                            process.kill()
                        process.join(5)
                    process.close()
                except ValueError:
                    pass
            for receive, send in self.channels:
                receive.close()
                send.close()

    def assert_released(self):
        self.assertEqual({child.pid for child in mp.active_children()}, self.children)
        for receive, send in self.channels:
            self.assertEqual(receive.fileno(), -1)
            self.assertEqual(send.fileno(), -1)
        for wrapped in self.processes:
            with self.assertRaises(ValueError):
                wrapped.process.is_alive()

    def run_jobs(self, jobs):
        return scheduler.run_process_jobs(jobs, admission_worker, workers=3,
                                          continue_on_error=True)

    def test_constructor_refusal_releases_both_socket_endpoints(self):
        primary = OSError("process constructor refused")
        def refuse(_):
            raise primary
        with self.fault_context(construct=refuse):
            with self.assertRaises(OSError) as caught:
                self.run_jobs([self.job("first"), self.job("later")])
            self.assertIs(caught.exception, primary)
            self.assertEqual([row["status"] for row in primary.process_batch_outcomes],
                             ["not_run", "not_run"])
            self.assert_released()

    def test_start_refusal_without_a_pid_does_not_join_an_unstarted_process(self):
        primary = OSError("spawn refused")
        def refuse(_):
            raise primary
        with self.fault_context(before_start=refuse):
            with self.assertRaises(OSError) as caught:
                self.run_jobs([self.job("first"), self.job("later")])
            self.assertIs(caught.exception, primary)
            self.assertEqual([row["status"] for row in primary.process_batch_outcomes],
                             ["not_run", "not_run"])
            self.assert_released()

    @unittest.skipUnless(os.name == "posix", "cooperative cleanup uses POSIX signals")
    def test_interrupt_after_spawn_reaps_worker_and_keeps_its_partial_receipt(self):
        primary = KeyboardInterrupt("interrupt during Process.start")
        def interrupt(_):
            wait_for((self.root / "first.ready").exists)
            raise primary
        with self.fault_context(after_start=interrupt):
            with self.assertRaises(KeyboardInterrupt) as caught:
                self.run_jobs([self.job("first"), self.job("later")])
            self.assertIs(caught.exception, primary)
            rows = primary.process_batch_outcomes
            self.assertEqual([row["status"] for row in rows], ["cancelled", "not_run"])
            self.assertEqual(rows[0]["result"], {"partial": ["first"]})
            self.assertTrue((self.root / "first.cleaned").exists())
            self.assertFalse((self.root / "later.ready").exists())
            self.assert_released()

    @unittest.skipUnless(os.name == "posix", "private-group cleanup uses POSIX signals")
    def test_second_spawn_error_retains_first_publication_and_reaps_both_workers(self):
        primary = RuntimeError("failure after second spawn")
        def interrupt(index):
            name = ("done", "running")[index]
            wait_for((self.root / (name + (".published" if index == 0 else ".ready"))).exists)
            if index == 1:
                raise primary
        with self.fault_context(after_start=interrupt):
            with self.assertRaises(RuntimeError) as caught:
                self.run_jobs([self.job("done", "success"), self.job("running"), self.job("later")])
            self.assertIs(caught.exception, primary)
            rows = primary.process_batch_outcomes
            self.assertEqual([row["status"] for row in rows], ["succeeded", "cancelled", "not_run"])
            self.assertEqual(rows[0]["result"], {"name": "done"})
            self.assertEqual(rows[1]["result"], {"partial": ["running"]})
            self.assert_released()

    @unittest.skipUnless(os.name == "posix", "private-group cleanup uses POSIX signals")
    def test_unstarted_second_candidate_does_not_prevent_first_worker_cleanup(self):
        primary = OSError("second spawn refused")
        def after(_):
            wait_for((self.root / "first.ready").exists)
        def before(index):
            if index == 1:
                raise primary
        with self.fault_context(before_start=before, after_start=after):
            with self.assertRaises(OSError) as caught:
                self.run_jobs([self.job("first"), self.job("second"), self.job("later")])
            self.assertIs(caught.exception, primary)
            self.assertEqual([row["status"] for row in primary.process_batch_outcomes],
                             ["cancelled", "not_run", "not_run"])
            self.assertTrue((self.root / "first.cleaned").exists())
            self.assert_released()

    @unittest.skipUnless(os.name == "posix", "private-group cleanup uses POSIX signals")
    def test_endpoint_close_failure_after_spawn_still_owns_the_process(self):
        primary = OSError("endpoint close interrupted after spawn")
        real_close = socket.socket.close
        injected = []
        def close(connection):
            real_close(connection)
            if self.channels and connection is self.channels[0][1] and not injected:
                injected.append(True)
                raise primary
        def after(_):
            wait_for((self.root / "first.ready").exists)
        with self.fault_context(after_start=after):
            with patch.object(socket.socket, "close", close):
                with self.assertRaises(OSError) as caught:
                    self.run_jobs([self.job("first"), self.job("later")])
            self.assertIs(caught.exception, primary)
            self.assertEqual([row["status"] for row in primary.process_batch_outcomes],
                             ["cancelled", "not_run"])
            self.assert_released()

    def test_cleanup_close_error_does_not_replace_original_start_refusal(self):
        primary = OSError("original spawn failure")
        real_close = ReceiptReader.close
        injected = []
        def close(reader):
            real_close(reader)
            if not injected:
                injected.append(True)
                raise RuntimeError("secondary descriptor close failure")
        def before(_):
            raise primary
        with self.fault_context(before_start=before):
            with patch.object(ReceiptReader, "close", close):
                with self.assertRaises(OSError) as caught:
                    self.run_jobs([self.job("first")])
            self.assertIs(caught.exception, primary)
            self.assertEqual(primary.process_batch_outcomes[0]["status"], "not_run")
            self.assertTrue(any("RuntimeError" in note for note in primary.__notes__))
            self.assert_released()

    @unittest.skipUnless(os.name == "posix", "private-group cleanup uses POSIX signals")
    def test_reported_failure_survives_cancellation_raised_after_spawn(self):
        primary = BatchCancelled("caller interrupted admission")
        def after(_):
            wait_for((self.root / "failed.published").exists)
            raise primary
        with self.fault_context(after_start=after):
            with self.assertRaises(BatchCancelled) as caught:
                self.run_jobs([self.job("failed", "failure"), self.job("later")])
            self.assertIs(caught.exception, primary)
            rows = primary.process_batch_outcomes
            self.assertEqual([row["status"] for row in rows], ["failed", "not_run"])
            self.assertEqual(rows[0]["error"]["type"], "ValueError")
            self.assert_released()

    @unittest.skipUnless(os.name == "posix", "receipt recovery uses POSIX cleanup")
    def test_large_receipt_is_drained_after_spawn_raises(self):
        primary = OSError("spawn reporting failure")
        def after(_):
            wait_for((self.root / "large.ready").exists)
            raise primary
        with self.fault_context(after_start=after):
            with self.assertRaises(OSError) as caught:
                self.run_jobs([self.job("large", "large")])
            self.assertIs(caught.exception, primary)
            row = primary.process_batch_outcomes[0]
            self.assertEqual(row["status"], "succeeded")
            self.assertEqual(len(row["result"]["payload"]), 2 * 1024 * 1024)
            self.assert_released()

    @unittest.skipUnless(os.name == "posix", "sibling cleanup uses POSIX signals")
    def test_retirement_interrupt_keeps_success_and_reaps_live_sibling(self):
        primary = KeyboardInterrupt("interrupted after completed Process.close")
        raised = []
        def after(index):
            name = ("done", "running")[index]
            wait_for((self.root / (name + ".ready")).exists)
        def after_close(index):
            if index == 0 and not raised:
                raised.append(True)
                raise primary
        with self.fault_context(after_start=after, after_close=after_close):
            with self.assertRaises(KeyboardInterrupt) as caught:
                self.run_jobs([self.job("done", "complete"), self.job("running")])
            self.assertIs(caught.exception, primary)
            rows = primary.process_batch_outcomes
            self.assertEqual([row["status"] for row in rows], ["succeeded", "cancelled"])
            self.assertEqual(rows[0]["result"], {"name": "done"})
            self.assertEqual(rows[1]["result"], {"partial": ["running"]})
            self.assert_released()

    def test_last_worker_close_error_keeps_completed_failure_receipt(self):
        primary = OSError("completed process handle close error")
        raised = []
        def after_close(_):
            if not raised:
                raised.append(True)
                raise primary
        with self.fault_context(after_close=after_close):
            with self.assertRaises(OSError) as caught:
                self.run_jobs([self.job("failed", "failed_complete")])
            self.assertIs(caught.exception, primary)
            row = primary.process_batch_outcomes[0]
            self.assertEqual(row["status"], "failed")
            self.assertEqual(row["error"]["type"], "ValueError")
            self.assert_released()

    def test_retirement_closes_process_even_when_receipt_close_also_raises(self):
        primary = OSError("completed receipt close error")
        secondary = RuntimeError("completed process close error")
        real_close, raised = ReceiptReader.close, []
        def close(reader):
            real_close(reader)
            if not raised:
                raised.append(True)
                raise primary
        def after_close(_):
            raise secondary
        with self.fault_context(after_close=after_close):
            with patch.object(ReceiptReader, "close", close):
                with self.assertRaises(OSError) as caught:
                    self.run_jobs([self.job("done", "complete")])
            self.assertIs(caught.exception, primary)
            self.assertEqual(primary.process_batch_outcomes[0]["status"], "succeeded")
            self.assertTrue(any("RuntimeError" in note for note in primary.__notes__))
            self.assert_released()

    def test_normal_retirement_still_reports_each_receipt_once(self):
        observed = []
        jobs = [self.job(name, "complete") for name in ("one", "two", "three")]
        with self.fault_context():
            rows = scheduler.run_process_jobs(jobs, admission_worker, workers=2,
                                              on_result=lambda index, row: observed.append((index, row)))
            self.assertEqual([row["status"] for row in rows], ["succeeded"] * 3)
            self.assertEqual(sorted(index for index, _ in observed), [0, 1, 2])
            for index, row in observed:
                self.assertIs(row, rows[index])
            self.assert_released()



if __name__ == "__main__":
    unittest.main()
