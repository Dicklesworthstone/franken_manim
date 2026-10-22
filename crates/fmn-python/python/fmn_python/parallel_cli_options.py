"""Parallel portal syntax only; the shared console still owns render parsing."""
from __future__ import annotations

import math

PARALLEL_VALUES = frozenset({"--workers", "--job-timeout", "--batch-timeout"})
PARALLEL_HELP = """Isolated parallel scene batches:
  fmn-python SOURCE.py --write_all --workers N [--job-timeout SECONDS]
  fmn-python SOURCE.py FirstScene SecondScene --workers N

--workers 1..64 opts into fresh spawned interpreters, one per scene. Without
it, the existing sequential path is unchanged. --threads defaults to 1 per
worker; an explicit --threads N still controls each native renderer. Source
code is imported in a disposable discovery worker and again in each render
worker; no scene code runs in the coordinator and global state is not shared.
Robot stdout remains one ordered batch receipt; scene output streams to stderr. --keep-going admits later jobs after ordinary failures. Fail-fast
stops new jobs while already running scenes finish. Optional --job-timeout
bounds discovery and each render worker including startup. --batch-timeout
SECONDS instead sets ONE shared wall-clock budget starting after parallel option
parsing, covering discovery, planning and all scene workers. It requires
--workers and composes with --job-timeout. Per-job timeouts still honor
--keep-going; the shared deadline always stops the entire invocation. Expiry
stops admission, reaps active workers, retains publications and journals partial
progress, then exits 6 with batch-deadline-exceeded (not user-interrupt 130).
Host I/O/observers must return before expiry is observed; cleanup grace is extra.
The batch timeout is not stored in checkpoints or certified scene identities.
Crashed/timed-out
outputs are never removed or silently reused, and may exist without a complete receipt.
Add --checkpoint PATH --resume-key INPUTS to save durable per-scene progress;
add --resume to verify and reuse completed outputs without starting workers
for them. Keep source bytes, cwd, scene selection and native render options
unchanged. --workers and --job-timeout may change between attempts. Robot
receipts name reused scenes explicitly. Sequential and parallel checkpoint
plans are not interchangeable. --save-last-frame adds a final PNG from the
same scene execution. --subdivide emits per-play/wait clips and composes
with --save-last-frame. Parallel checkpoint recovery verifies every clip and
optional final PNG before reusing a completed scene collection, including an
empty selection. Incomplete collections retain their files but are not reused;
retrying them still refuses no-clobber. This is whole-scene recovery, not the
single-scene --clip-checkpoint prefix-replay mode. Sequential subdivided
batches still refuse --checkpoint.
--reproducible supports fresh png, png_sequence and wav outputs with a native
manifest per scene. Each worker captures imports/reads through the guarded
session and must match the parent's installed runtime. The aggregate remains
uncertified; all_scenes_certified requires every scene's complete receipt.
Certification cannot combine with recovery, paired output or subdivision.
"""


def take_parallel_options(arguments, value_flags=frozenset()):
    forwarded, options, index = [], {}, 0
    while index < len(arguments):
        token = arguments[index]
        flag, separator, text = token.partition("=")
        if flag in PARALLEL_VALUES:
            name = flag[2:].replace("-", "_")
            if name in options:
                raise ValueError(flag + " must not be repeated")
            if not separator:
                index += 1
                if index >= len(arguments):
                    raise ValueError(flag + " requires a value")
                text = arguments[index]
            if flag == "--workers":
                if not text or len(text) > 2 or any(char not in "0123456789" for char in text):
                    raise ValueError("--workers requires an integer in 1..64")
                value = int(text)
                if not 1 <= value <= 64:
                    raise ValueError("--workers must lie between 1 and 64")
            else:
                if not text or len(text) > 32:
                    raise ValueError(flag + " requires positive finite seconds")
                try:
                    value = float(text)
                except ValueError:
                    raise ValueError(flag + " requires positive finite seconds") from None
                if not math.isfinite(value) or value <= 0:
                    raise ValueError(flag + " requires positive finite seconds")
            options[name] = value
        else:
            forwarded.append(token)
            if token in value_flags and index + 1 < len(arguments):
                index += 1
                forwarded.append(arguments[index])
        index += 1
    for name in ("job_timeout", "batch_timeout"):
        if name in options and "workers" not in options:
            raise ValueError("--" + name.replace("_", "-") + " requires --workers")
    return forwarded, options


def start_batch_budget(options):
    """Consume one scheduling option and share its token across CLI phases."""
    from .cancellation import CancellationToken

    duration = options.pop("batch_timeout", None)
    return None if duration is None else CancellationToken(timeout=duration)


def emit_deadline_failure(native, error, robot, *, source, destination, phase):
    """Report a budget stop without discarding already published outcomes."""
    details = {"source": None if source is None else str(source),
               "destination": None if destination is None else str(destination),
               "phase": phase, "timeout_seconds": error.timeout,
               "notes": list(getattr(error, "__notes__", ()))}
    report = getattr(error, "render_batch_result", None)
    if report is not None:
        details.update(batch=report.as_dict(), all_succeeded=report.ok)
    return native._portal_cli_emit(6, "render", "batch-deadline-exceeded",
                                  str(error), robot, **details)
