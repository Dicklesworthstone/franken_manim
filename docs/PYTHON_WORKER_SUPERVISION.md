# Python render-worker supervision

The optional portal's source discovery and parallel rendering use the same
`fmn_python._process_batch` supervisor. Native `fmn` does not acquire a Python
runtime dependency. Rendering, clip clocks, artifact publication, checkpoint
verification and certified provenance remain with their existing owners.

## Result-channel liveness

Each spawned interpreter receives one private socket endpoint. Its only result
is a length-prefixed UTF-8 JSON receipt, limited to 8 MiB. The parent validates
the length before allocating the body and reads incrementally in bounded
chunks. A readable channel is **not** evidence that the whole result is ready.
A partial header/body must never block the coordinator's worker deadlines.
No pickle is accepted from the worker, no reader thread can be stranded, and
no filesystem spool or persistent receipt format is introduced.

After a worker exits, the coordinator drains its buffered receipt without
waiting for EOF. A forked descendant can retain the endpoint, but cannot make
the coordinator wait for an owner which has already died. Oversized or malformed
wire data is a worker failure and triggers cancellation even when the caller
has not supplied a job timeout. Completed receipts from other scenes survive.

## Cancellation and descendants

Every POSIX worker calls `setsid()` and acknowledges its private process group
before invoking the worker function, importing scene source or rendering. The
acknowledgement contains no process identifier: cleanup targets only the PID
from the supervisor's own `multiprocessing.Process`. Before acknowledgement,
only that individual process may be signalled. The caller's process group is
never used as a fallback or accepted as a cleanup target.

Timeouts and interruption request SIGINT so native session contexts can finish
or cancel cooperatively. A worker still alive after two seconds receives
SIGTERM, followed by SIGKILL after another half second. These are independent,
nonblocking escalation clocks: other workers continue delivering results and
being checked against their deadlines. A batch of stubborn workers does not
multiply the grace periods by its worker count. Fail-fast pauses new admission
as soon as a deadline expires; a failed outcome stops admission permanently,
while a complete success recovered during cleanup allows it to continue.
Keep-going can run later jobs as slots become available. A second Ctrl-C during cleanup immediately escalates remaining
workers to SIGKILL, while preserving the original interruption and any complete
receipts. The caller's signal handler is not replaced. Progress callbacks remain
synchronous caller code; they are not preempted or covered by worker timeouts.

Failed/crashed or cancelled POSIX workers' acknowledged process groups are
also stopped, including ordinary descendants which outlive the group leader.
Direct workers are joined and their handles/channels closed. The coordinator
cannot reap processes for which it is not the OS parent; on Linux a terminated
orphan can temporarily remain a zombie until its adopter reaps it.

This is robustness isolation, **not a security sandbox or a universal process
tree guarantee**. Descendants which deliberately create a different process
group/session escape this group scope. Native ffmpeg jobs retain their own
process-management boundary. Windows uses direct-worker termination, not the
POSIX descendant-group guarantee. Successful scene programs are not retroactively
cancelled solely because they launched an authored background process.

## Publication remains authoritative

Cancellation never removes or overwrites native outputs. A complete successful
receipt stays successful even if the worker is stopped during interpreter
shutdown. A timed-out scene can return a partial clip collection during its
cooperative cleanup; that receipt is retained while TimeoutError describes the
job outcome. A simultaneous observer failure does not erase earlier successes
or change an already-expired job into a different outcome. Hard crashes can
still leave outputs without a complete receipt; those are not reusable by
assertion and remain subject to existing no-clobber and checkpoint rules.

## Evidence

At `470b22dd`, a partial body defeated a 0.5-second deadline: an outer watchdog
terminated the blocked coordinator after four seconds. The fixed transport's
fault tests use independent watchdogs, including partial headers, dead senders
and inherited open endpoints. With the first transport fix alone, four workers
ignoring SIGINT/SIGTERM remained in serial cancellation after six seconds for
0.8-second job timeouts. Concurrent escalation completed that same local probe
in about 3.3 seconds, including the grace periods. These are fault/liveness
observations, not renderer throughput or qualified performance-gate results.

`test_process_receipts.py` tests framing and real spawned-process failures.
`test_process_cancellation.py` checks real Linux descendant state, concurrent
escalation, caller-group protection and preservation of partial/error outcomes.
`test_process_rendering_native.py` renders actual moving PNG sequences, compares
them byte-for-byte with sequential output, and verifies an actual GIF clip's
bytes and digest after cooperative timeout. The unchanged d862adfb native CI
extension was used with the affected Python sources on Linux, CPython 3.13.5
and NumPy 2.3.5. These tests do not establish a rebuilt whole wheel, the full
workspace/portal gate, macOS/Windows behavior or cross-platform certification.
