# Recover completed subdivided scenes in a batch

The Python front door can checkpoint completed per-scene clip collections.
Use `--subdivide` with the existing batch journal, either sequentially or with
`--workers` for a process batch:

```bash
fmn-python lessons.py Intro Derivation Summary \
  --subdivide --format gif --save-last-frame \
  --resolution 1280x720 --fps 30 --video_dir recordings \
  --checkpoint recordings-progress.json --resume-key lessons-assets-v1 \
  --keep-going
```

Repeat the same command with `--resume` after interruption or a transient
failure. Every completed scene's clips and optional final PNG are verified
before any render retry starts. Completed scenes do not run again; the robot
batch receipt's `reused` field names them. Discovery still runs in its ordinary
isolated process for `--workers` batches. `--write_all` uses the selected batch
mode; `--jobs-file` retains its process-batch owner. For a process batch, add
`--workers 4` to both the initial and recovery commands.

The sequential programmatic API is:

```python
from fmn_python import RenderJob, render_scenes

report = render_scenes(
    [RenderJob("intro", Intro), RenderJob("derivation", Derivation)],
    "recordings",
    subdivide=True, format="gif", save_last_frame=True,
    checkpoint="recordings-progress.json", resume_key="lessons-assets-v1",
    continue_on_error=True,
    # Add resume=True when retrying this same plan.
)
```

Process batches use `render_source_scenes(..., subdivide=True,
checkpoint=..., resume_key=..., resume=True)`. Both APIs verify completed
collections before constructing or dispatching any unfinished scene.

## Exactly what is recovered

Recovery is at **whole-scene granularity**. A complete collection contains all
of that scene's selected `play()` / `wait()` clips, and its final PNG when
requested. Valid empty selections are complete too. Render workers do not
receive the batch journal: its lock, inventory and atomic writes remain owned
by the coordinator. A successful scene is journaled before observer callbacks.

An unfinished scene can run afresh only when its destination is absent.
**Partial collections and unreceipted files remain no-clobber refusals.**
The batch journal does not delete those files, infer completion from names,
adopt them, or replay a partial clip prefix. For recovery *within* one scene,
use the existing single-scene `--clip-checkpoint` mode instead; its command
replay and input contract are distinct.

Each collection lives at `OUTPUT/JobName/clips/`, with original segment-index
filenames. A companion image is `OUTPUT/JobName/clips.png`. File-based GIF,
Y4M, WAV, MP4 and MOV clips have individually checked byte counts and SHA-256
digests. PNG-sequence clips have exact frame inventories, the native
`FMN_COMPLETE` marker, and hashes for every file. Missing/altered clips, extra
files or directories, links, missing stills, inconsistent receipts, and file
or segment budget violations refuse reuse. Checks do not promise an atomic
snapshot against concurrent filesystem mutation after verification.

## Input and mode contract

Keep entry-source bytes, working directory, selected names/constructor inputs,
output paths and render options unchanged. The journal binds subdivision,
segment budget, selection and final-image mode explicitly. `-n START,END` keeps
its **exclusive end**: a clip at `END` is not part of the admitted selection.
Keep the same API and execution mode when resuming. Within process batches,
worker count and supervision timeouts may change. The `resume_key` remains the
caller's explicit version for helpers/assets/runtime inputs; this is not
source-closure certification. Change the key and use new destinations when
those inputs change.

No new CLI flags or renderer are introduced. This feature is standard-only;
certified output and still-only `-s` do not combine with subdivision. Legacy
non-subdivided batch plans keep their version-2 shape and existing inventories.
Ordinary scene failure remains exit 5; successful recovery is exit 0. Deadline,
interruption and reporting-error behavior retain their existing owners.

## Validation boundaries

`test_subdivision_batch_checkpoint.py` exercises receipt/inventory validation,
real files, journal locking, real process scheduling and retries. The byte
writer explicitly emits `NOT-MEDIA`, not an encoded artifact.
Its sequential coordinator tests cover interrupted/failed scenes, observers,
empty selections, paired outputs, and plan identity before any retry starts.
`test_subdivision_batch_cli_protocol.py` runs both shipping dispatch wrappers
through those real process and checkpoint owners; native common-option parsing,
discovery and scene rendering remain explicit fixtures.
`test_subdivision_batch_recovery_native.py` separately requires a matching built
wheel and tests real GIF/PNG output through both batch APIs and the installed
console, including sequential recovery without `--workers`. The registered
Gauntlet batch-checkpoint scenario also exercises real sequential Y4M/PNG
collections, empty selections and modified-clip refusal. Protocol passes alone
are not native rendering acceptance.
