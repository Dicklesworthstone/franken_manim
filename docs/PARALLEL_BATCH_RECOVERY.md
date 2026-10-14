# Resumable process-isolated scene batches

The optional host-CPython portal can render source-file batches in fresh
interpreters and retain completed work across failed or interrupted runs. It
uses the existing native render-session owner for each scene and the same
`BatchCheckpoint` artifact verifier as sequential batches. Native `fmn` does
not acquire a Python runtime dependency.

## CLI

Start a batch with a checkpoint outside its published outputs:

```sh
fmn-python scenes.py --write_all --workers 4 --threads 1 \
  --format png_sequence --video_dir rendered \
  --checkpoint progress.json --resume-key scene-assets-v1 --keep-going
```

Resume from the same working directory, entry source, scene selection and
native render options. Worker count and per-job timeout may change:

```sh
fmn-python scenes.py --write_all --workers 2 --threads 1 \
  --format png_sequence --video_dir rendered \
  --checkpoint progress.json --resume-key scene-assets-v1 --resume \
  --job-timeout 300 --keep-going --robot
```

Multiple explicit scene names also work. Their order must remain unchanged.
Without `--workers`, the existing sequential batch route is unchanged.
Sequential and parallel checkpoint plans are distinct and cannot be reused
interchangeably.

## Programmatic API

Use a spawn-safe entry point; workers receive names and JSON options, never
live scene objects:

```python
from fmn_python import render_source_scenes

if __name__ == "__main__":
    report = render_source_scenes(
        "scenes.py", "rendered", ["Introduction", "Derivation"],
        workers=4, threads=1, format="png_sequence",
        checkpoint="progress.json", resume_key="scene-assets-v1",
        resume=True, continue_on_error=True,
    )
    print(report.reused)
    print(report.as_dict())
```

Use `resume=False` on the first run. A preexisting checkpoint is an error
without explicit resume. In the CLI, source imports still run for discovery;
reused scenes do not start workers or run their constructors.

## Recovery contract

The parent holds the exclusive checkpoint lock for the batch. Before starting
any pending worker it validates the complete saved plan and hash-verifies every
completed artifact, including the entire inventory of a PNG-sequence directory.
Only jobs without a completed receipt are scheduled. An all-completed resume
starts no workers.

The plan binds entry-source SHA-256, absolute source/output paths, initial
working directory, scene names/order and native render options. The caller's
`resume_key` identifies helper, asset and runtime versions not automatically
captured by the entry-source digest. Change that key when inputs of completed
scenes change, and use a new output directory and checkpoint. Recovery is
explicit artifact reuse, **not certification or a complete source-code cache**.

Each new receipt is recorded durably before its progress callback. Reused
receipts reach `on_result` in input order before pending jobs start; new results
arrive in completion order. The final outcome list always retains input order.
`report.reused` and the robot receipt's `reused` list identify skipped scenes.

Fail-fast stops new admission while preserving running jobs' results.
Interruptions and observer errors retain a partial `render_batch_result`,
including receipts collected while stopping active workers. Failed, cancelled
and unattempted scenes can be retried when they have not published an output.

A crash between artifact publication and journal publication may leave an
output without a completed receipt. Such an output is **not** reused, removed
or overwritten. Missing, modified or extra files also cause a refusal before
workers start. Keep the journal and its persistent lock file outside all
published artifacts. A journal write failure preserves existing outputs but
does not imply that they are recoverable from the saved checkpoint.

Parallel batches still exclude `--reproducible`, paired output and subdivision.
Those capabilities remain on their supported sequential routes.

## Validation scope

`test_parallel_checkpoint.py` runs real spawned processes and the production
checkpoint/coordinator against byte-artifact worker fixtures; it does not claim
native raster validation. `test_parallel_cli.py` checks the production console
functions and checkpoint parser with a reduced native table.
`test_parallel_batch_native.py` separately exercises actual native PNG receipts,
fresh scene state and checkpoint recovery when the FrankenManim wheel is built.
