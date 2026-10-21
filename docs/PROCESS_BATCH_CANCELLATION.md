# Caller-controlled cancellation of Python process batches

A host application can stop source discovery or a rendering batch without
sending a signal to its own process. This belongs to the optional Python portal;
it does not add a Python dependency or another scheduler to native `fmn`.

```python
from threading import Timer
from fmn_python import render_source_scenes
from fmn_python.cancellation import BatchCancelled, CancellationToken

if __name__ == "__main__":
    cancellation = CancellationToken()
    timer = Timer(60, cancellation.cancel)
    timer.start()
    try:
        report = render_source_scenes(
            "lessons.py", "recordings", ["FirstScene", "SecondScene"],
            format="fmtl", workers=2, bundle_audio=True,
            checkpoint="progress.json", resume_key="lessons-and-assets-v1",
            cancellation=cancellation,
        )
        print(report.as_dict())
    except BatchCancelled as error:
        report = getattr(error, "render_batch_result", None)
        if report is not None:
            print(report.as_dict())
    finally:
        timer.cancel()
        timer.join()
```

The timer is merely an example of a caller. An application stop button, an
ordinary thread, or the parent's `on_result` callback may call `cancel()`.
`cancel()` returns immediately; the rendering/discovery call returns or raises
only after its owned workers have been reaped. The token is thread-safe,
idempotent and one-way. There is no reset operation; use a fresh token for a
later invocation or checkpoint resume. Tokens are parent-only and are never
included in JSON jobs, passed into scene constructors, or hashed into a plan.

## Ownership and shutdown

`render_source_scenes(..., cancellation=token)` and
`discover_source_scenes(..., cancellation=token)` accept the same token. A
request observed between discovery and render admission prevents rendering.
`discovery_source` forwards it as well. The existing `_process_batch` supervisor
owns admission, result transport, cooperative interruption and escalation; this
feature does not install a parent signal handler or launch another controller.

The supervisor checks before each worker admission, while polling active work,
and around completion observers. Once it observes the request, it stops new
admission and uses the existing concurrent shutdown path. On POSIX, the owned
worker or its acknowledged private process group receives SIGINT, then SIGTERM
and SIGKILL if needed. Normal children in that group are covered; deliberately
detached children are not. Windows retains direct-worker termination, not a
process-tree guarantee. A parent `on_result` callback or filesystem operation
must return before cancellation can be observed. This is not a hard wall-clock
bound on arbitrary host operations. `job_timeout` remains per worker, including
startup, and is independent of caller cancellation.

## Results are not rolled back

`BatchCancelled` subclasses `KeyboardInterrupt`, preserving the portal's
interruption handling rather than converting a stop request into an ordinary
scene failure. Catch it specifically when caller cancellation should be handled
separately from a real keyboard interrupt.

The scheduler attaches ordered `process_batch_outcomes`; after a scene plan is
admitted, `render_source_scenes` also attaches its typed `render_batch_result`.
Pre-admission cancellation may have no report: a pre-cancelled scene/discovery
call does not read its source, acquire its journal or start workers.

* Complete success and failure receipts survive shutdown, including receipts
  returned during cooperative cancellation or before stalled interpreter exit.
* Cooperative partial receipts remain attached to the interrupted outcome.
  A forcibly stopped worker with no receipt is cancelled, not a fabricated
  rendering error. Already observed timeouts and protocol failures keep their
  original classifications. Workers not started remain `not_run`.
* Once cancellation is observed, no further user completion callbacks run.
  Cleanup receipts still appear in the attached report and durable journal.
  An exception raised by an observer remains primary even if that observer
  also requested cancellation.

A successful output is never removed or overwritten to simulate rollback.
Cancellation can race with atomic publication: an output with no complete
receipt may already exist and still fails create-only preflight on retry.

## Recovery

The parent records retained results using the existing checkpoint journal
before propagating cancellation. It also releases the journal lock. A reporting
failure is attached as a note to the original cancellation; the typed partial
report is retained even when the last checkpoint write fails.

Resume with the same source, constructor inputs, names, output settings and
`resume_key`, adding `resume=True` and a fresh token (or no token). Completed
outputs are hash-verified and reused; only unfinished scenes run. Cancellation
is a scheduling choice and does not change checkpoint identity. No new journal
schema, artifact-adoption policy or certification claim is introduced.

If cancellation happens in an observer of an already-complete resumed batch,
the exception can legitimately carry an all-success report. Cancellation is
not permission to relabel intact outputs as failed. A late request is not a
retroactive revocation of a successful call that already returned.

## Evidence boundaries

`test_caller_cancellation.py` uses actual spawned workers, socket transport,
process exit, signals and (on Linux) descendant-liveness checks, without a
native wheel or mocked IPC. It checks bounded concurrent escalation, admission
races, large receipts, first-error retention, and parent-handler preservation.

`test_parallel_cancellation_protocol.py` exercises the complete production batch
coordinator and discovery wrapper with real workers, filesystem checkpoints,
locking, hashing and resume. The existing explicit byte-artifact fixture stands
in for native capture; its files are NOT FMTL. These tests do not claim native
renderer/mixer acceptance, an installed CLI pass, or cross-platform execution.
No CLI cancellation flag is added: CLI users retain Ctrl-C and `--job-timeout`.
