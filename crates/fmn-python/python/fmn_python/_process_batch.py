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

from .cancellation import BatchCancelled, _check_cancelled, _validate_cancellation
from ._process_receipt import (
    MAX_RECEIPT_BYTES as _MAX_RECEIPT_BYTES, receipt_channel, send_receipt,
    send_group_ready,
)


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
            # A disposable worker owns its ordinary descendants, never the
            # caller's process group. Acknowledgement precedes authored code.
            # Windows retains direct-worker termination; this is not a sandbox
            # against descendants deliberately creating another session/group.
            if os.name == "posix":
                os.setsid()
                send_group_ready(connection)
            # The result travels over its private result channel. Keep fd-level output
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
        send_receipt(connection, data)
    finally:
        connection.close()


def _receive(connection, *, exited=False):
    try:
        data = connection.drain()
        if data is None:
            if exited and not connection.budget_exhausted:
                raise EOFError("worker exited before sending its full receipt")
            return None
        row = json.loads(data, parse_constant=_invalid_constant)
        if (not isinstance(row, dict) or not isinstance(row.get("status"), str)
                or row["status"] not in {"succeeded", "failed", "cancelled"}):
            raise ValueError("invalid worker outcome")
        return row
    except (EOFError, OSError, ValueError, RecursionError) as error:
        connection.failed = True
        connection.incomplete = isinstance(error, EOFError)
        return failure(RuntimeError("worker exited without a complete receipt: " + str(error)))


def _invalid_constant(value):
    raise ValueError("worker receipt contains nonfinite JSON: " + value)


def _signal_worker(item, action):
    """Signal only a created worker, or its acknowledged private POSIX group."""
    process, reader, _ = item
    try:
        if os.name == "posix":
            sig = {"interrupt": signal.SIGINT, "terminate": signal.SIGTERM,
                   "kill": signal.SIGKILL}[action]
            if reader.group_started:
                # The id comes from Process, not JSON. Never fall back to the
                # caller's group when a child has not completed setsid.
                if process.pid <= 1 or process.pid == os.getpgrp():
                    raise RuntimeError("refusing to signal the caller's process group")
                os.killpg(process.pid, sig)
            elif process.is_alive():
                os.kill(process.pid, sig)
        elif process.is_alive():
            (process.kill if action == "kill" else process.terminate)()
    except ProcessLookupError:
        pass  # The owned process/group has already gone away.


def _begin_cancel(item):
    _signal_worker(item, "interrupt")
    return [time.monotonic() + 2.0, 0]


def _advance_cancel(item, state):
    """Advance a nonblocking escalation clock shared by normal/error cleanup."""
    if not item[0].is_alive():
        # The owner is gone and cannot clean up any remaining descendants.
        _signal_worker(item, "kill")
        return True
    if time.monotonic() >= state[0]:
        action = "interrupt" if state[1] < 0 else "terminate" if state[1] == 0 else "kill"
        _signal_worker(item, action)
        state[:] = [time.monotonic() + (2.0 if state[1] < 0 else 0.5), state[1] + 1]
    return False


def _drain_exited(item):
    # Never await EOF from an inherited descriptor after its owner exits.
    # Continue only when a bounded read left bytes; at most 8 MiB in total.
    while item[2] is None:
        item[2] = _receive(item[1], exited=True)
        if not item[1].budget_exhausted:
            break


def _stop(active, *, cancelling=None):
    """Cancel concurrently while continuing to collect published receipts.

    Grace periods belong to each worker, not a blocking join inside a loop:
    64 uncooperative jobs must not multiply a two-second cleanup allowance.
    """
    cancelling = {} if cancelling is None else dict(cancelling)
    for index in active:
        cancelling.setdefault(index, [0.0, -1])
    while cancelling:
        try:
            for index, state in list(cancelling.items()):
                item = active[index]
                if item[2] is None:
                    item[2] = _receive(item[1])
                if _advance_cancel(item, state):
                    item[0].join()
                    _drain_exited(item)
                    del cancelling[index]
            if cancelling:
                time.sleep(0.01)
        except KeyboardInterrupt:
            # A second Ctrl-C escalates all remaining workers instead of
            # abandoning cleanup and masking the original exception with a
            # Process.close() error. No caller signal handler is replaced.
            for state in cancelling.values():
                state[:] = [0.0, 1]


def _timeout_outcome(item, duration):
    if item[2] is not None and item[2]["status"] == "succeeded":
        return  # A complete native publication remains a complete publication.
    timeout = failure(TimeoutError(f"scene job exceeded {duration:g} seconds"))
    timeout["error"]["notes"].append("Timed-out outputs were not removed; inspect before retrying.")
    if item[2] is None:
        item[2] = timeout
    else:
        # Cooperative cancellation may return valuable partial publications.
        item[2].update(status=timeout["status"], error=timeout["error"])


def run_process_jobs(jobs, worker, *, workers, continue_on_error=False,
                     on_result=None, job_timeout=None, cancellation=None):
    """Run bounded JSON jobs in fresh spawned interpreters, in input order.

    on_result(index, row) runs in completion order in the parent. Fail-fast
    stops admission, but lets already running jobs finish and keeps receipts.
    Timeout is per job (including interpreter startup), not a batch deadline.
    Interrupt/observer exceptions carry process_batch_outcomes after cleanup.
    Cancellation escalates concurrently while other workers remain supervised.
    On POSIX, crashed/cancelled workers' acknowledged private groups are stopped;
    deliberately detached descendants and Windows process trees are not covered.
    cancellation is a parent-owned CancellationToken; observing it stops new
    admission and raises BatchCancelled after the same bounded cleanup as Ctrl-C.
    Complete receipts survive shutdown, including successes collected during it.
    Unstarted jobs stay not_run; forced stops without a receipt are cancelled.
    This function is for importable worker functions, not lambdas or closures.
    """
    _validate_cancellation(cancellation)
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
    rows = [{"status": "not_run", "result": None, "error": None} for _ in jobs]
    active, started, cancelling = {}, {}, {}
    timed_out = set()
    next_job, stopped = 0, False
    try:
        _check_cancelled(cancellation)
        # Freeze plain job data before launching anything, including nested values.
        jobs = json.loads(json.dumps(jobs, allow_nan=False))
        context = mp.get_context("spawn")
        while active or (not stopped and next_job < len(jobs)):
            _check_cancelled(cancellation)
            while (not stopped and (continue_on_error or not timed_out)
                   and next_job < len(jobs) and len(active) < workers):
                _check_cancelled(cancellation)
                index = next_job
                receive, send = receipt_channel()
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
            _check_cancelled(cancellation)
            handles = [handle for process, connection, row in active.values()
                       for handle in ([process.sentinel, connection] if row is None else [process.sentinel])]
            ready = wait(handles, timeout=0.05)
            completed = []
            for index, item in list(active.items()):
                process, connection, row = item
                if row is None and connection in ready:
                    item[2] = row = _receive(connection)
                # A timeout's own cleanup SIGINT may return a cancelled row.
                # It becomes a timeout failure after cleanup, not a user request
                # to stop unrelated work. Genuine worker cancellation still stops
                # admission even under keep-going.
                if row is not None and ((row["status"] == "cancelled" and index not in timed_out)
                                        or (row["status"] != "succeeded" and not continue_on_error)):
                    stopped = True
                if (index not in cancelling and job_timeout is not None
                        and time.monotonic() - started[index] >= job_timeout and process.is_alive()):
                    cancelling[index] = _begin_cancel(item)
                    # Pause fail-fast admission while cleanup determines the
                    # outcome. A successful publication followed by stalled
                    # interpreter teardown must not strand later jobs as
                    # not_run with no failed outcome for BatchRenderError.
                    timed_out.add(index)
                elif index not in cancelling and connection.failed:
                    # Invalid framing is a broken worker protocol, not an
                    # invitation to wait indefinitely for interpreter teardown.
                    cancelling[index] = _begin_cancel(item)
                if index in cancelling and not _advance_cancel(item, cancelling[index]):
                    continue
                if not process.is_alive():
                    process.join()
                    if item[2] is None:
                        item[2] = _receive(connection, exited=True)
                        if item[2] is None:
                            continue  # A bounded drain left buffered bytes.
                    if index in timed_out:
                        _timeout_outcome(item, job_timeout)
                    if item[2]["status"] != "succeeded" and index not in cancelling:
                        _signal_worker(item, "kill")
                    rows[index] = item[2]
                    if rows[index]["status"] == "cancelled" or (
                            rows[index]["status"] != "succeeded" and not continue_on_error):
                        stopped = True
                    completed.append(index)
                    connection.close()
                    process.close()
                    del active[index]
                    cancelling.pop(index, None)
                    timed_out.discard(index)
            # First collect every ready receipt, then call observers. A failing
            # observer cannot discard another job's already reported success.
            for index in completed:
                _check_cancelled(cancellation)
                if on_result is not None:
                    on_result(index, rows[index])
            # Also observe requests made by the final observer, when no next
            # loop iteration would otherwise run. Rows already collected stay.
            _check_cancelled(cancellation)
    except BaseException as error:
        awaiting = {index for index, item in active.items() if item[2] is None}
        try:
            _stop(active, cancelling=cancelling)
        finally:
            for index, item in active.items():
                if index in timed_out:
                    _timeout_outcome(item, job_timeout)
                process, connection, row = item
                if (isinstance(error, BatchCancelled) and index in awaiting
                        and index not in timed_out and connection.incomplete):
                    # EOF caused by stopping an unreceipted worker is not a
                    # reported rendering failure. Preserve every complete row,
                    # earlier protocol failure and already-observed timeout.
                    row = failure(BatchCancelled("worker stopped by caller"), "cancelled")
                    row["error"]["notes"].append(
                        "Worker stopped without a complete receipt; outputs were not removed.")
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
