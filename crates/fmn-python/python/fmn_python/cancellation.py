"""Parent-owned cancellation and shared monotonic budgets for process jobs.

Tokens stay in the coordinator. The existing supervisor alone signals owned
workers; no timer thread, parent signal handler or token crosses processes.
"""
import math
from threading import Lock
from time import monotonic


class BatchCancelled(KeyboardInterrupt):
    """A caller stopped a process batch, after owned workers were reaped.

    process_batch_outcomes retains scheduler receipts once supervision starts.
    The scene owner also attaches render_batch_result and journals progress.
    Pre-admission cancellation need not have either report.
    """


class BatchDeadlineExceeded(BatchCancelled):
    """The shared time budget expired; outputs are not rolled back."""

    def __init__(self, timeout):
        self.timeout = timeout
        super().__init__(f"process batch exceeded its {timeout:g}-second time budget")


class CancellationToken:
    """Thread-safe, one-way stop request, optionally with a shared time budget.

    timeout is positive finite seconds from token creation, measured on the
    monotonic clock. Reusing the token for discovery and rendering does not
    restart that clock. No timer thread is created. Expiry is observed at the
    same supervision checkpoints as caller cancellation, so blocking host I/O
    and observers must return and worker cleanup can exceed the time budget.
    Use a fresh token for a later run or checkpoint resume; there is no reset.
    """

    def __init__(self, *, timeout=None):
        if isinstance(timeout, bool):
            raise TypeError("timeout must be positive finite seconds, not bool")
        if timeout is not None:
            try:
                timeout = float(timeout)
            except (TypeError, ValueError, OverflowError):
                raise ValueError("timeout must be positive finite seconds") from None
            if not math.isfinite(timeout) or timeout <= 0:
                raise ValueError("timeout must be positive finite seconds")
        deadline = None if timeout is None else monotonic() + timeout
        if deadline is not None and not math.isfinite(deadline):
            raise ValueError("timeout exceeds the monotonic clock range")
        self._timeout, self._deadline = timeout, deadline
        self._lock, self._cause = Lock(), None

    def _status(self, requested=False):
        with self._lock:
            if self._cause is None:
                if self._deadline is not None and monotonic() >= self._deadline:
                    self._cause = "deadline"
                elif requested:
                    self._cause = "caller"
            return self._cause

    @property
    def cancelled(self):
        return self._status() is not None

    def cancel(self):
        # A manual request before expiry wins; after expiry it cannot disguise
        # a budget overrun. The first cause is latched even across later reads.
        self._status(requested=True)

    def raise_if_cancelled(self):
        cause = self._status()
        if cause == "deadline":
            raise BatchDeadlineExceeded(self._timeout)
        if cause is not None:
            raise BatchCancelled("process batch cancelled by caller")


def _validate_cancellation(cancellation):
    if cancellation is not None and not isinstance(cancellation, CancellationToken):
        raise TypeError("cancellation must be a CancellationToken or None")


def _check_cancelled(cancellation):
    if cancellation is not None:
        cancellation.raise_if_cancelled()
