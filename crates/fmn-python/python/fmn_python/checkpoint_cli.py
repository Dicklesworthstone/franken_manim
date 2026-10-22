"""Checkpoint options for the existing ordered multi-scene console route."""
from pathlib import Path

CHECKPOINT_VALUES = frozenset({"--checkpoint", "--clip-checkpoint-dir", "--resume-key"})
CHECKPOINT_HELP = """Resumable batches:
  fmn-python SOURCE.py --write_all --checkpoint progress.json --resume-key INPUTS
  fmn-python SOURCE.py --write_all --checkpoint progress.json --resume-key INPUTS --resume

The same options work with multiple explicitly named scenes. Keep all render
options and the scene selection/order unchanged when resuming. INPUTS is your
version for scene code and assets: change it when inputs for completed scenes
change. The checkpoint is not a certified input closure or source-code cache.
Completed artifacts are hash-verified and reused without constructing their
scenes; only failed, cancelled and unattempted scenes render again. Progress is
saved before observers run. A changed/missing output or an unrecorded existing
output is an error, never permission to overwrite. Use a new output directory
and checkpoint for a new input version. Keep the journal outside frame folders.
With --save-last-frame, non-subdivided batches checkpoint the primary and final
PNG together. Both must still match before a completed scene is skipped. Keep
the journal and its lock outside BOTH outputs. Paired and single-output plans
are distinct; certification does not support recovery. With --workers,
--subdivide can recover completed whole-scene collections through this journal.
With --workers, the parent owns the same durable journal and verifies every
completed artifact before launching pending jobs. Entry-source bytes and cwd
must also match; worker count and timeout may change. Reused scene names appear
in the robot receipt. Sequential and parallel checkpoint plans are distinct.

To recover INSIDE unfinished scenes, choose --clip-checkpoint-dir DIRECTORY
instead of --checkpoint. It requires --workers and --subdivide, with the same
--resume-key INPUTS and explicit --resume on retries. Each job gets an existing
per-clip journal; its saved clips are verified and its commands replay to rebuild
state without rasterizing/encoding the saved prefix. Python side effects repeat,
including for completed scenes, so the whole-scene reused list stays empty.
Named scene batches, --write_all and --jobs-file are supported. Final-image pairs,
certification and FMTL do not combine with this mode. Keep the state directory
outside every clip collection; missing journals never authorize existing outputs.
A shared --batch-timeout or per-job --job-timeout retains durably journaled clips.
"""


def take_checkpoint_options(arguments, value_flags):
    """Consume only our switches, preserving native option/value token pairs."""
    remaining, found, index = [], {}, 0
    while index < len(arguments):
        value = arguments[index]
        if value in CHECKPOINT_VALUES or value == "--resume":
            if value in found:
                raise ValueError(value + " must not be repeated")
            if value == "--resume":
                found[value] = True
                index += 1
                continue
            if index + 1 >= len(arguments):
                raise ValueError(value + " requires a value")
            found[value] = arguments[index + 1]
            index += 2
        elif value in value_flags:
            remaining.extend(arguments[index:index + 2])
            index += 2
        else:
            remaining.append(value)
            index += 1
    if not found:
        return remaining, {}
    modes = [flag for flag in ("--checkpoint", "--clip-checkpoint-dir") if flag in found]
    if len(modes) != 1 or "--resume-key" not in found:
        raise ValueError("batch recovery requires exactly one of --checkpoint PATH or "
                         "--clip-checkpoint-dir DIRECTORY, and --resume-key INPUTS")
    mode = modes[0]
    path, key = found[mode], found["--resume-key"]
    if not path or "\0" in path:
        raise ValueError(mode + " must be a nonempty path without NUL")
    if not key or len(key.encode("utf-8")) > 4096:
        raise ValueError("--resume-key must contain 1 to 4096 UTF-8 bytes")
    # Source module code may chdir. Preserve the leaf so the journal owner can
    # reject a symlink rather than resolving it into an overwrite target.
    path = Path(path).absolute()
    return remaining, {mode[2:].replace("-", "_"): path, "resume_key": key,
                       "resume": bool(found.get("--resume"))}
