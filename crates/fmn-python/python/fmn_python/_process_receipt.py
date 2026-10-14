"""Bounded, nonblocking result frames for disposable scene workers.

A readable multiprocessing Connection can contain only part of its message;
recv_bytes would then block the coordinator, including its timeout checks.
Socket endpoints are transferred by multiprocessing's ordinary spawn support.
Only a four-byte length and bounded UTF-8 JSON travel on this private channel.
A startup control word acknowledges the worker's private POSIX process group;
no process identifier is ever accepted from the wire. No connection internals,
pickled results, reader threads or disk spool is used.
"""
from __future__ import annotations

import socket
import struct

MAX_RECEIPT_BYTES = 8 * 1024 * 1024
_HEADER = struct.Struct("!I")
_GROUP_READY = 0x80000000
_READ_CHUNK = 64 * 1024
_READ_BUDGET = 256 * 1024


def receipt_channel():
    """Return a nonblocking parent reader and its blocking worker endpoint."""
    receive, send = socket.socketpair()
    try:
        receive.setblocking(False)
        return ReceiptReader(receive), send
    except BaseException:
        receive.close()
        send.close()
        raise


def send_group_ready(connection):
    """Acknowledge setsid before any scene source or worker callback runs."""
    connection.sendall(_HEADER.pack(_GROUP_READY))


def send_receipt(connection, data):
    """Send one bounded frame. Only the worker may wait for channel space."""
    if not 0 < len(data) <= MAX_RECEIPT_BYTES:
        raise ValueError("worker receipt exceeds its 8 MiB budget")
    connection.sendall(_HEADER.pack(len(data)))
    connection.sendall(data)


class ReceiptReader:
    """Incremental frame parser; each drain has a fixed fairness budget.

    None means incomplete, not EOF. budget_exhausted asks the caller to poll
    again before declaring an exited worker's buffered message incomplete.
    A descendant holding an inherited endpoint cannot make drain() wait.
    """
    def __init__(self, connection):
        self.connection = connection
        self.buffer = bytearray()
        self.expected = None
        self.budget_exhausted = False
        self.group_started = False
        self.failed = False

    def fileno(self):
        return self.connection.fileno()

    def close(self):
        self.connection.close()

    def drain(self):
        self.budget_exhausted = False
        if self.expected is not None and len(self.buffer) == self.expected:
            return bytes(self.buffer)  # A signal may have interrupted JSON decoding.
        budget = _READ_BUDGET
        while budget:
            needed = (4 if self.expected is None else self.expected) - len(self.buffer)
            try:
                chunk = self.connection.recv(min(needed, _READ_CHUNK, budget))
            except BlockingIOError:
                return None
            if not chunk:
                raise EOFError("worker channel closed before its receipt was complete")
            self.buffer.extend(chunk)
            budget -= len(chunk)
            if self.expected is None and len(self.buffer) == 4:
                self.expected, = _HEADER.unpack(self.buffer)
                self.buffer.clear()
                if self.expected == _GROUP_READY:
                    if self.group_started:
                        raise ValueError("duplicate worker process-group acknowledgement")
                    self.group_started = True
                    self.expected = None
                    continue
                if not 0 < self.expected <= MAX_RECEIPT_BYTES:
                    raise ValueError("worker receipt length must be 1..8388608 bytes")
            elif self.expected is not None and len(self.buffer) == self.expected:
                return bytes(self.buffer)
        self.budget_exhausted = True
        return None
