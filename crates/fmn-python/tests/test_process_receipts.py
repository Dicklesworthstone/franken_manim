"""Framing and real-process supervision, including incomplete result writes.

Fault cases run in an outer process with a hard deadline, so a regression in
job_timeout cannot hang the test runner. No renderer is substituted here.
"""
import json
import multiprocessing as mp
import os
from pathlib import Path
import signal
import socket
import struct
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

PYTHON = Path(__file__).resolve().parents[1] / "python"
sys.path.insert(0, str(PYTHON))
from fmn_python import _process_batch as batch
from fmn_python._process_receipt import MAX_RECEIPT_BYTES, receipt_channel, send_receipt

_ORIGINAL_CHILD = batch._child
_SUCCESS = {"status": "succeeded", "result": {"value": 7}, "error": None}


def ordinary_job(job):
    return _SUCCESS


def _wire(connection, data):
    # The legacy implementation used Connection; the negative-control probe
    # also runs against it. Production never depends on its internal framing.
    if hasattr(connection, "sendall"):
        connection.sendall(data)
    else:
        os.write(connection.fileno(), data)


def faulty_child(connection, worker, job):
    fault = job.get("fault")
    if fault is None:
        return _ORIGINAL_CHILD(connection, worker, job)
    signal.signal(signal.SIGINT, signal.SIG_IGN)
    if fault == "dead_holder":
        child = os.fork()
        if child == 0:
            time.sleep(20)
            os._exit(0)
        Path(job["pid_file"]).write_text(str(child))
        os._exit(17)  # Its child still owns the writer: no EOF is forthcoming.
    if fault == "fragmented_header":
        _wire(connection, struct.pack("!I", 128)[:2])
    elif fault in {"partial", "dead_partial"}:
        _wire(connection, struct.pack("!I", 128) + b"{")
        if fault == "dead_partial":
            os._exit(17)
    elif fault == "invalid_length":
        _wire(connection, struct.pack("!I", MAX_RECEIPT_BYTES + 1))
        os._exit(17)
    else:
        raise AssertionError(fault)
    time.sleep(20)


def fault_case(case):
    with tempfile.TemporaryDirectory() as directory:
        pid_file = Path(directory) / "descendant.pid"
        seen = []
        started = time.monotonic()
        try:
            jobs = [{"fault": case, "pid_file": str(pid_file)}, {}, {}]
            with patch.object(batch, "_child", faulty_child):
                rows = batch.run_process_jobs(jobs, ordinary_job, workers=2,
                    job_timeout=0.8, continue_on_error=True,
                    on_result=lambda i, row: seen.append(i))
            assert not mp.active_children(), "scheduler leaked a direct worker"
            print(json.dumps({"rows": rows, "seen": seen, "elapsed": time.monotonic() - started}))
        finally:
            if pid_file.exists():
                try:
                    os.kill(int(pid_file.read_text()), signal.SIGKILL)
                except ProcessLookupError:
                    pass


class ReceiptFramingTests(unittest.TestCase):
    def setUp(self):
        self.reader, self.writer = receipt_channel()
        self.addCleanup(self.reader.close)
        self.addCleanup(self.writer.close)

    def test_empty_channel_does_not_wait(self):
        self.assertIsNone(self.reader.drain())
        self.assertFalse(self.reader.budget_exhausted)

    def test_fragmented_header_and_body_wait_for_complete_frame(self):
        data = json.dumps(_SUCCESS).encode()
        frame = struct.pack("!I", len(data)) + data
        for char in frame[:-1]:
            self.writer.sendall(bytes([char]))
            self.assertIsNone(self.reader.drain())
        self.writer.sendall(frame[-1:])
        self.assertEqual(self.reader.drain(), data)

    def test_one_byte_receipt_is_framed_not_confused_with_header(self):
        send_receipt(self.writer, b"7")
        self.assertEqual(self.reader.drain(), b"7")

    def test_clean_and_partial_eof_are_terminal_failures(self):
        self.writer.sendall(struct.pack("!I", 20) + b"{")
        self.writer.close()
        result = batch._receive(self.reader)
        self.assertEqual(result["status"], "failed")
        self.assertIn("complete receipt", result["error"]["message"])

    def test_exited_writer_does_not_wait_for_an_inherited_open_endpoint(self):
        self.writer.sendall(struct.pack("!I", 20) + b"{")
        self.assertIsNone(batch._receive(self.reader))
        result = batch._receive(self.reader, exited=True)
        self.assertEqual(result["status"], "failed")

    def test_invalid_lengths_refuse_before_body_allocation(self):
        for length in (0, MAX_RECEIPT_BYTES + 1, 0xffffffff):
            reader, writer = receipt_channel()
            try:
                writer.sendall(struct.pack("!I", length))
                self.assertEqual(batch._receive(reader)["status"], "failed")
                self.assertEqual(len(reader.buffer), 0)
            finally:
                reader.close()
                writer.close()

    def test_malformed_json_status_and_nonfinite_values_are_failed(self):
        for raw in (b"{", b"[]", b'{"status":[]}', b'{"status":"not_run"}',
                    b'{"status":"succeeded","result":NaN}', b'\xff', b'{' * 10000):
            reader, writer = receipt_channel()
            try:
                send_receipt(writer, raw)
                self.assertEqual(batch._receive(reader)["status"], "failed")
            finally:
                reader.close()
                writer.close()

    def test_large_receipt_drain_yields_to_other_jobs(self):
        data = json.dumps({**_SUCCESS, "result": "x" * 5000}).encode()
        send_receipt(self.writer, data)
        with patch("fmn_python._process_receipt._READ_BUDGET", 1000):
            self.assertIsNone(batch._receive(self.reader, exited=True))
            self.assertTrue(self.reader.budget_exhausted)
            self.assertLessEqual(len(self.reader.buffer), 1000)
            row = None
            for _ in range(10):
                row = batch._receive(self.reader, exited=True)
                if row is not None:
                    break
        self.assertEqual(row["status"], "succeeded")
        self.assertEqual(len(row["result"]), 5000)

    def test_senders_refuse_empty_or_oversized_frames(self):
        for data in (b"", b"x" * (MAX_RECEIPT_BYTES + 1)):
            with self.assertRaises(ValueError):
                send_receipt(self.writer, data)
            self.assertIsNone(self.reader.drain())


@unittest.skipUnless(os.name == "posix", "outer process-group watchdog is POSIX-specific")
class ProcessTransportAcceptance(unittest.TestCase):
    def run_fault(self, case):
        environment = dict(os.environ, PYTHONPATH=str(PYTHON))
        process = subprocess.Popen([sys.executable, __file__, "--fault-case", case],
            env=environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            text=True, start_new_session=True)
        try:
            out, err = process.communicate(timeout=7)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            out, err = process.communicate()
            self.fail("result transport defeated the job deadline: " + err[-1000:])
        self.assertEqual(process.returncode, 0, err)
        return json.loads(out)

    def test_partial_body_does_not_defeat_timeout_or_other_workers(self):
        result = self.run_fault("partial")
        self.assertEqual([row["status"] for row in result["rows"]], ["failed", "succeeded", "succeeded"])
        self.assertEqual(result["rows"][0]["error"]["type"], "TimeoutError")
        self.assertEqual(result["seen"][0], 1)

    def test_partial_header_does_not_defeat_timeout(self):
        result = self.run_fault("fragmented_header")
        self.assertEqual(result["rows"][0]["error"]["type"], "TimeoutError")

    def test_dead_worker_with_partial_receipt_reports_failure(self):
        result = self.run_fault("dead_partial")
        self.assertEqual(result["rows"][0]["status"], "failed")
        self.assertIn("receipt", result["rows"][0]["error"]["message"])

    def test_dead_worker_does_not_wait_for_forked_descriptor_holder(self):
        result = self.run_fault("dead_holder")
        self.assertEqual(result["rows"][0]["status"], "failed")
        self.assertLess(result["elapsed"], 3)

    def test_oversized_wire_length_never_allocates_or_hangs(self):
        result = self.run_fault("invalid_length")
        self.assertEqual(result["rows"][0]["status"], "failed")
        self.assertIn("8388608", result["rows"][0]["error"]["message"])


if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] == "--fault-case":
        fault_case(sys.argv[2])
    else:
        unittest.main()
