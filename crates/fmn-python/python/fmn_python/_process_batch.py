"""Bounded fresh-interpreter jobs for the optional host-CPython portal.

Only JSON receipts cross the return channel, never Scenes, native proxies, or
exceptions. This scheduler owns processes, not rendering or artifact cleanup.
"""
from __future__ import annotations

import json
import math
import multiprocessing as mp
from multiprocessing.connection import wait
import operator
import os
import signal
import sys
import time

_MAX_RECEIPT_BYTES = 8 * 1024 * 1024


def positive_integer(value, name, maximum):
    if isinstance(value, bool):
        raise TypeError(name + " must be a positive integer, not bool")
    value = operator.index(value)
    if not 1 <= value <= maximum:
        raise ValueError(f"{name} must be between 1 and {maximum}")
    return value


def validate_job_timeout(job_timeout):
    """Validate even when a resumed batch has no jobs left to launch."""
    if job_timeout is not None:
        if isinstance(job_timeout, bool):
            raise TypeError("job_timeout must be a positive finite number")
        job_timeout = float(job_timeout)
        if not math.isfinite(job_timeout) or job_timeout <= 0:
            raise ValueError("job_timeout must be a positive finite number")
    return job_timeout


def failure(error, status="failed"):
    try:
        message = str(error)
    except BaseException:
        message = "exception message could not be formatted"
    return {"status": status, "result": None,
            "error": {"type": type(error).__name__[:128], "message": message[:4096], "notes": []}}


def _child(connection, worker, job):
    try:
        try:
            # The result travels over its private pipe. Keep fd-level output
            # from native libraries and authored os.write/subprocess calls off
            # the coordinator's robot stdout too, not only Python print().
            # Redirect before flushing any buffered startup message too.
            os.dup2(2, 1)
            if sys.stdout is not None:
                sys.stdout.flush()
            row = worker(job)
            if not isinstance(row, dict) or row.get("status") not in {"succeeded", "failed", "cancelled"}:
                raise ValueError("worker must return a terminal outcome")
            data = json.dumps(row, allow_nan=False, ensure_ascii=True).encode("utf-8")
            if len(data) > _MAX_RECEIPT_BYTES:
                raise ValueError("worker receipt exceeds its 8 MiB budget")
        except BaseException as error:
            row = failure(error, "cancelled" if isinstance(error, KeyboardInterrupt) else "failed")
            # Publication may have preceded a reporting error. Never claim that
            # no artifact exists or remove it to make the failure look atomic.
            row["error"]["notes"].append("Worker failed before returning a complete receipt; outputs were not removed.")
            data = json.dumps(row).encode("utf-8")
        connection.send_bytes(data)
    finally:
        connection.close()


def _receive(connection):
    try:
        row = json.loads(connection.recv_bytes(_MAX_RECEIPT_BYTES))
        if not isinstance(row, dict) or row.get("status") not in {"succeeded", "failed", "cancelled"}:
            raise ValueError("invalid worker outcome")
        return row
    except (EOFError, OSError, ValueError) as error:
        return failure(RuntimeError("worker exited without a complete receipt: " + str(error)))


def _stop(active):
    """Give native session contexts a chance to cancel before forced shutdown."""
    for process, _, _ in active.values():
        if process.is_alive():
            try:
                if os.name == "posix":
                    os.kill(process.pid, signal.SIGINT)
                else:
                    process.terminate()
            except ProcessLookupError:
                pass
    deadline = time.monotonic() + 2.0
    while time.monotonic() < deadline and any(item[0].is_alive() for item in active.values()):
        # Drain large receipts while workers shut down; joining before reading
        # can deadlock a sender whose receipt exceeds the pipe buffer.
        for item in active.values():
            if item[2] is None and item[1].poll():
                item[2] = _receive(item[1])
        time.sleep(0.01)
    for item in active.values():
        process, connection, _ = item
        if process.is_alive():
            process.terminate()
        process.join(timeout=0.5)
        if process.is_alive():
            process.kill()
            process.join()
        if item[2] is None and connection.poll():
            item[2] = _receive(connection)


def run_process_jobs(jobs, worker, *, workers, continue_on_error=False,
                     on_result=None, job_timeout=None):
    """Run bounded JSON jobs in fresh spawned interpreters, in input order.

    on_result(index, row) runs in completion order in the parent. Fail-fast
    stops admission, but lets already running jobs finish and keeps receipts.
    Timeout is per job (including interpreter startup), not a batch deadline.
    Interrupt/observer exceptions carry process_batch_outcomes after cleanup.
    This function is for importable worker functions, not lambdas or closures.
    """
    workers = positive_integer(workers, "workers", 64)
    if not isinstance(jobs, (list, tuple)) or not 1 <= len(jobs) <= 65536:
        raise ValueError("process batch requires 1..65536 jobs")
    if not isinstance(continue_on_error, bool):
        raise TypeError("continue_on_error must be bool")
    if on_result is not None and not callable(on_result):
        raise TypeError("on_result must be callable or None")
    if not callable(worker):
        raise TypeError("worker must be callable")
    job_timeout = validate_job_timeout(job_timeout)
    # Freeze plain job data before launching anything, including nested values.
    jobs = json.loads(json.dumps(jobs, allow_nan=False))
    context = mp.get_context("spawn")
    rows = [{"status": "not_run", "result": None, "error": None} for _ in jobs]
    active, started = {}, {}
    next_job, stopped = 0, False
    try:
        while active or (not stopped and next_job < len(jobs)):
            while not stopped and next_job < len(jobs) and len(active) < workers:
                index = next_job
                receive, send = context.Pipe(duplex=False)
                process = context.Process(target=_child, args=(send, worker, jobs[index]))
                try:
                    process.start()
                except BaseException:
                    receive.close()
                    send.close()
                    process.close()
                    raise
                send.close()
                active[index] = [process, receive, None]
                started[index] = time.monotonic()
                next_job += 1
            handles = [handle for process, connection, row in active.values()
                       for handle in ([process.sentinel, connection] if row is None else [process.sentinel])]
            ready = wait(handles, timeout=0.05)
            completed = []
            for index, item in list(active.items()):
                process, connection, row = item
                if row is None and connection in ready:
                    item[2] = row = _receive(connection)
                if row is not None and (row["status"] == "cancelled" or
                                        (row["status"] != "succeeded" and not continue_on_error)):
                    stopped = True
                if job_timeout is not None and time.monotonic() - started[index] >= job_timeout and process.is_alive():
                    # Timeout is an explicit caller policy. It cannot roll back
                    # native publication or imply a force-stopped output is absent.
                    _stop({index: item})
                    if item[2] is None or item[2]["status"] != "succeeded":
                        timeout = failure(TimeoutError(f"scene job exceeded {job_timeout:g} seconds"))
                        timeout["error"]["notes"].append("Timed-out outputs were not removed; inspect before retrying.")
                        # Cooperative cancellation may have returned valuable
                        # partial publications. Retain that data while making
                        # the timeout (not success/cancellation) authoritative.
                        if item[2] is None:
                            item[2] = timeout
                        else:
                            item[2].update(status=timeout["status"], error=timeout["error"])
                if not process.is_alive():
                    process.join()
                    if item[2] is None:
                        item[2] = _receive(connection)
                    rows[index] = item[2]
                    if rows[index]["status"] == "cancelled" or (
                            rows[index]["status"] != "succeeded" and not continue_on_error):
                        stopped = True
                    completed.append(index)
                    connection.close()
                    process.close()
                    del active[index]
            # First collect every ready receipt, then call observers. A failing
            # observer cannot discard another job's already reported success.
            for index in completed:
                if on_result is not None:
                    on_result(index, rows[index])
    except BaseException as error:
        try:
            _stop(active)
        finally:
            for index, (process, connection, row) in active.items():
                rows[index] = row if row is not None else failure(
                    RuntimeError("worker stopped without a receipt; outputs may exist"), "cancelled")
                connection.close()
                process.close()
            try:
                error.process_batch_outcomes = rows
            except BaseException:
                pass
        raise
    return rows
