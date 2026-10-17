"""Real worker cancellation with independent grace clocks and owned groups.

The Linux process-state assertions distinguish stopped/zombie descendants from
running ones; only direct workers can be reaped by the coordinator. Deliberately
detached descendants are outside this process-group contract, not a sandbox.
"""
import json
import multiprocessing as mp
import os
from pathlib import Path
import signal
import struct
import subprocess
import sys
import tempfile
import threading
import time
import types
import unittest
from unittest.mock import patch

PYTHON = Path(__file__).resolve().parents[1] / "python"
sys.path.insert(0, str(PYTHON))
from fmn_python import _process_batch as batch
from fmn_python._process_receipt import MAX_RECEIPT_BYTES, receipt_channel, send_group_ready, send_receipt


def _running(pid):
    try:
        status = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()[0]
    except FileNotFoundError:
        return False
    return status not in {"Z", "X"}


def _wait_file(path):
    deadline = time.monotonic() + 5
    while not path.exists():
        if time.monotonic() >= deadline:
            raise RuntimeError("test worker did not publish its ready marker")
        time.sleep(0.01)


def _bad_writer(connection, data):
    connection.sendall(struct.pack("!I", MAX_RECEIPT_BYTES + 1))
    signal.signal(signal.SIGINT, signal.SIG_IGN)
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    time.sleep(30)


def worker(job):
    root, index = Path(job["root"]), job["index"]
    mode = job.get("mode", "success")
    if mode == "stubborn":
        signal.signal(signal.SIGINT, signal.SIG_IGN)
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
    (root / f"owner-{index}.json").write_text(json.dumps({"pid": os.getpid(), "pgid": os.getpgrp()}))
    if mode in {"crash", "descendant", "background"}:
        child = os.fork()
        if child == 0:
            signal.signal(signal.SIGINT, signal.SIG_IGN)
            signal.signal(signal.SIGTERM, signal.SIG_IGN)
            (root / f"child-{index}.pid").write_text(str(os.getpid()))
            time.sleep(30)
            os._exit(0)
        _wait_file(root / f"child-{index}.pid")
        if mode == "crash":
            os._exit(19)
    if mode == "bad_writer":
        batch.send_receipt = _bad_writer
    if mode == "teardown_success":
        import atexit
        signal.signal(signal.SIGINT, signal.SIG_IGN)
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        atexit.register(time.sleep, 30)
    if job.get("wait_for"):
        _wait_file(root / job["wait_for"])
    try:
        time.sleep(job.get("delay", 30 if mode in {"stubborn", "descendant"} else 0))
    except KeyboardInterrupt:
        # A scene can publish some output before cooperative cancellation.
        return {"status": "cancelled", "result": {"published": ["completed-clip"]},
                "error": {"type": "KeyboardInterrupt", "message": "stopped", "notes": []}}
    return {"status": "succeeded", "result": {"pid": os.getpid(), "pgid": os.getpgrp()}, "error": None}


def _cleanup(root):
    # Test watchdog only: these PIDs were written by this invocation's workers.
    for path in root.glob("owner-*.json"):
        pid = json.loads(path.read_text())["pid"]
        try:
            if os.getpgid(pid) == pid and pid != os.getpgrp():
                os.killpg(pid, signal.SIGKILL)
            else:
                os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
    for path in root.glob("child-*.pid"):
        try:
            os.kill(int(path.read_text()), signal.SIGKILL)
        except ProcessLookupError:
            pass


def run_case(case, directory):
    root = Path(directory)
    def job(index, **options):
        return {"root": str(root), "index": index, **options}
    started = time.monotonic()
    observer = watcher = None
    interrupted = False
    workers, timeout, keep = 1, 1.2, False
    if case == "many":
        jobs = [job(i, mode="stubborn") for i in range(5)]
        workers = 4
    elif case in {"crash", "descendant", "background", "bad_writer"}:
        jobs = [job(0, mode=case)]
        timeout = None if case == "bad_writer" else timeout
    elif case == "observer":
        jobs = [job(0, wait_for="child-1.pid"), job(1, mode="descendant")]
        workers, timeout = 2, 10
        def observer(index, row):
            raise LookupError("observer stopped the batch")
    elif case in {"interrupt", "double_interrupt"}:
        jobs = [job(0, mode="stubborn"), job(1, mode="descendant")]
        workers, timeout = 2, 10
        def interrupt():
            _wait_file(root / "owner-0.json")
            _wait_file(root / "child-1.pid")
            os.kill(os.getpid(), signal.SIGINT)
            if case == "double_interrupt":
                time.sleep(.2)
                os.kill(os.getpid(), signal.SIGINT)
        watcher = threading.Thread(target=interrupt)
        watcher.start()
    elif case == "timeout_observer":
        jobs = [job(0, mode="stubborn"), job(1, delay=.45), job(2, delay=.9)]
        workers, timeout, keep = 2, 1.1, True
        def observer(index, row):
            if index == 2:
                raise LookupError("observer failed while another job was timing out")
    elif case == "teardown_success":
        jobs = [job(0, mode="teardown_success"), job(1)]
    elif case == "identity":
        jobs = [job(0), job(1)]
        workers = 2
    else:
        raise AssertionError(case)
    try:
        try:
            rows = batch.run_process_jobs(jobs, worker, workers=workers, job_timeout=timeout,
                                          on_result=observer, continue_on_error=keep)
        except (LookupError, KeyboardInterrupt) as error:
            interrupted = isinstance(error, KeyboardInterrupt)
            rows = error.process_batch_outcomes
        if watcher is not None:
            watcher.join()
        children = [int(path.read_text()) for path in root.glob("child-*.pid")]
        # SIGKILL has been issued; allow the scheduler's return-to-host race,
        # not a renderer/supervisor grace period, for the OS to stop a child.
        deadline = time.monotonic() + 1
        while case != "background" and any(map(_running, children)) and time.monotonic() < deadline:
            time.sleep(.01)
        print(json.dumps({"rows": rows, "elapsed": time.monotonic() - started,
                          "children_running": [pid for pid in children if _running(pid)],
                          "direct_workers": [child.pid for child in mp.active_children()],
                          "owners_started": len(list(root.glob("owner-*.json"))),
                          "parent_group": os.getpgrp(), "interrupted": interrupted}), flush=True)
    finally:
        _cleanup(root)


class GroupControlTests(unittest.TestCase):
    def test_group_acknowledgement_is_not_a_terminal_receipt(self):
        reader, writer = receipt_channel()
        try:
            send_group_ready(writer)
            self.assertIsNone(batch._receive(reader))
            self.assertTrue(reader.group_started)
            send_receipt(writer, b'{"status":"succeeded"}')
            self.assertEqual(batch._receive(reader)["status"], "succeeded")
        finally:
            reader.close()
            writer.close()

    def test_complete_receipt_can_be_retried_after_an_interrupted_decode(self):
        reader, writer = receipt_channel()
        try:
            payload = b'{"status":"succeeded"}'
            send_receipt(writer, payload)
            self.assertEqual(reader.drain(), payload)
            self.assertEqual(reader.drain(), payload)
            self.assertEqual(batch._receive(reader)["status"], "succeeded")
        finally:
            reader.close()
            writer.close()

    def test_duplicate_group_acknowledgement_fails(self):
        reader, writer = receipt_channel()
        try:
            send_group_ready(writer)
            send_group_ready(writer)
            self.assertEqual(batch._receive(reader)["status"], "failed")
            self.assertTrue(reader.failed)
        finally:
            reader.close()
            writer.close()

    @unittest.skipUnless(os.name == "posix", "POSIX group signals")
    def test_unacknowledged_startup_signals_only_the_created_pid(self):
        process = types.SimpleNamespace(pid=99999, is_alive=lambda: True)
        reader = types.SimpleNamespace(group_started=False)
        with patch.object(batch.os, "kill") as kill, patch.object(batch.os, "killpg") as killpg:
            batch._signal_worker([process, reader, None], "interrupt")
        kill.assert_called_once_with(99999, signal.SIGINT)
        killpg.assert_not_called()

    @unittest.skipUnless(os.name == "posix", "POSIX group signals")
    def test_caller_group_can_never_be_a_cleanup_target(self):
        process = types.SimpleNamespace(pid=os.getpgrp(), is_alive=lambda: True)
        reader = types.SimpleNamespace(group_started=True)
        with patch.object(batch.os, "killpg") as killpg, self.assertRaises(RuntimeError):
            batch._signal_worker([process, reader, None], "kill")
        killpg.assert_not_called()


@unittest.skipUnless(sys.platform == "linux", "requires Linux process-state inspection")
class ConcurrentCancellationTests(unittest.TestCase):
    def case(self, name):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            process = subprocess.Popen([sys.executable, __file__, "--case", name, directory],
                env=dict(os.environ, PYTHONPATH=str(PYTHON)), start_new_session=True,
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            try:
                out, err = process.communicate(timeout=10)
            except subprocess.TimeoutExpired:
                _cleanup(root)
                os.killpg(process.pid, signal.SIGKILL)
                out, err = process.communicate()
                self.fail("cancellation did not complete under its watchdog: " + err[-1000:])
            finally:
                _cleanup(root)
            self.assertEqual(process.returncode, 0, err)
            result = json.loads(out)
            self.assertEqual(result["direct_workers"], [])
            return result

    def test_many_stubborn_workers_share_grace_time_and_stop_admission(self):
        result = self.case("many")
        self.assertEqual(result["owners_started"], 4)
        self.assertEqual([row["status"] for row in result["rows"]], ["failed"] * 4 + ["not_run"])
        self.assertTrue(all(row["error"]["type"] == "TimeoutError" for row in result["rows"][:4]))
        self.assertLess(result["elapsed"], 7)  # Serial grace would take > 11 s.

    def test_crash_stops_the_descriptor_holding_descendant(self):
        result = self.case("crash")
        self.assertEqual(result["rows"][0]["status"], "failed")
        self.assertEqual(result["children_running"], [])

    def test_timeout_stops_descendants_and_retains_partial_publications(self):
        result = self.case("descendant")
        row = result["rows"][0]
        self.assertEqual(row["error"]["type"], "TimeoutError")
        self.assertEqual(row["result"], {"published": ["completed-clip"]})
        self.assertEqual(result["children_running"], [])

    def test_observer_failure_reaps_groups_but_keeps_prior_success(self):
        result = self.case("observer")
        self.assertEqual(result["rows"][0]["status"], "succeeded")
        self.assertEqual(result["rows"][1]["status"], "cancelled")
        self.assertEqual(result["children_running"], [])

    def test_parent_sigint_preserves_the_interrupt_and_stops_owned_groups(self):
        result = self.case("interrupt")
        self.assertTrue(result["interrupted"])
        self.assertEqual(result["children_running"], [])
        self.assertTrue(all(row["status"] in {"failed", "cancelled"} for row in result["rows"]))

    def test_second_sigint_forces_cleanup_without_losing_the_original_interrupt(self):
        result = self.case("double_interrupt")
        self.assertTrue(result["interrupted"])
        self.assertEqual(result["children_running"], [])
        self.assertLess(result["elapsed"], 2)

    def test_timeout_identity_survives_an_unrelated_observer_error(self):
        result = self.case("timeout_observer")
        self.assertEqual(result["rows"][0]["error"]["type"], "TimeoutError")
        self.assertEqual(result["rows"][2]["status"], "succeeded")

    def test_broken_transport_cancels_even_without_a_job_timeout(self):
        result = self.case("bad_writer")
        self.assertEqual(result["rows"][0]["status"], "failed")
        self.assertIn("8388608", result["rows"][0]["error"]["message"])

    def test_successful_author_background_process_is_not_relabelled_as_cancelled(self):
        result = self.case("background")
        self.assertEqual(result["rows"][0]["status"], "succeeded")
        self.assertEqual(len(result["children_running"]), 1)

    def test_published_success_with_stalled_teardown_does_not_strand_later_jobs(self):
        result = self.case("teardown_success")
        self.assertEqual([row["status"] for row in result["rows"]], ["succeeded", "succeeded"])
        self.assertEqual(result["owners_started"], 2)

    def test_worker_groups_are_private_and_not_the_coordinator_group(self):
        result = self.case("identity")
        for row in result["rows"]:
            self.assertEqual(row["status"], "succeeded")
            self.assertEqual(row["result"]["pgid"], row["result"]["pid"])
            self.assertNotEqual(row["result"]["pgid"], result["parent_group"])


if __name__ == "__main__":
    if len(sys.argv) == 4 and sys.argv[1] == "--case":
        run_case(sys.argv[2], sys.argv[3])
    else:
        unittest.main()
