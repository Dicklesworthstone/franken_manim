"""Caller-owned, one-way cancellation for optional host-CPython process jobs.

The token stays in the coordinator. Only the existing supervisor signals its
owned workers; no token, callback or parent signal handler crosses processes.
"""
from threading import Event


class BatchCancelled(KeyboardInterrupt):
    """A caller stopped a process batch, after owned workers were reaped.

    This is an interruption, not an ordinary render failure. Once execution
    starts, process_batch_outcomes retains scheduler receipts. The scene batch
    owner additionally attaches render_batch_result and journals its progress.
    Pre-admission cancellation need not have either report.
    """


class CancellationToken:
    """Thread-safe stop request, reusable across discovery and scene admission.

    cancel() is idempotent and does not wait for workers. There is deliberately
    no reset: use a fresh token for a later run or checkpoint resume. Hosts may
    call cancel() from another thread or from a completion observer. Blocking
    host filesystem operations and observer callbacks must return before the
    supervisor can observe the request.
    """

    def __init__(self):
        self._event = Event()

    @property
    def cancelled(self):
        return self._event.is_set()

    def cancel(self):
        self._event.set()

    def raise_if_cancelled(self):
        if self._event.is_set():
            raise BatchCancelled("process batch cancelled by caller")


def _validate_cancellation(cancellation):
    if cancellation is not None and not isinstance(cancellation, CancellationToken):
        raise TypeError("cancellation must be a CancellationToken or None")


def _check_cancelled(cancellation):
    if cancellation is not None:
        cancellation.raise_if_cancelled()
