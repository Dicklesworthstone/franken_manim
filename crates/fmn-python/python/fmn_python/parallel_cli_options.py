"""Parallel portal syntax only; the shared console still owns render parsing."""
from __future__ import annotations

import math

PARALLEL_VALUES = frozenset({"--workers", "--job-timeout"})
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
bounds discovery and each render worker including startup. Crashed/timed-out
outputs are never removed or silently reused, and may exist without a complete receipt.
Add --checkpoint PATH --resume-key INPUTS to save durable per-scene progress;
add --resume to verify and reuse completed outputs without starting workers
for them. Keep source bytes, cwd, scene selection and native render options
unchanged. --workers and --job-timeout may change between attempts. Robot
receipts name reused scenes explicitly. Sequential and parallel checkpoint
plans are not interchangeable. --save-last-frame adds a final PNG from the
same scene execution; non-subdivided pairs support verified checkpoint reuse.
--subdivide emits per-play/wait clips, retaining partial collection receipts
on failure or interruption, and composes with --save-last-frame. Subdivided
collections do not support checkpoint recovery. --reproducible still requires
the sequential path; omit --workers for certified output.
"""


def take_parallel_options(arguments, value_flags=frozenset()):
    forwarded, options, index = [], {}, 0
    while index < len(arguments):
        token = arguments[index]
        flag, separator, text = token.partition("=")
        if flag in PARALLEL_VALUES:
            name = "workers" if flag == "--workers" else "job_timeout"
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
                    raise ValueError("--job-timeout requires positive finite seconds")
                try:
                    value = float(text)
                except ValueError:
                    raise ValueError("--job-timeout requires positive finite seconds") from None
                if not math.isfinite(value) or value <= 0:
                    raise ValueError("--job-timeout requires positive finite seconds")
            options[name] = value
        else:
            forwarded.append(token)
            if token in value_flags and index + 1 < len(arguments):
                index += 1
                forwarded.append(arguments[index])
        index += 1
    if "job_timeout" in options and "workers" not in options:
        raise ValueError("--job-timeout requires --workers")
    return forwarded, options
