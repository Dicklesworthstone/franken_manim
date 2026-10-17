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
without explicit resume. In the CLI, source imports run in a disposable discovery
worker, never in the coordinator; reused scenes do not start render workers or
run their constructors. `--job-timeout` also bounds discovery, including native
initialization. Discovery crashes and import errors become structured scene-load
errors; a child interrupt retains exit 130. Dynamic locally declared classes and
package-relative imports still use the ordinary `SceneSource` loader. Entry-source
bytes are checked before compilation so an edit during package import cannot
silently execute a different entry point. This is robustness isolation, not a
sandbox: authored filesystem effects and spawned processes retain host authority.

## Recovery contract

The parent holds the exclusive checkpoint lock for the batch. Before starting
any pending worker it validates the complete saved plan and hash-verifies every
completed artifact, including the entire inventory of a PNG-sequence directory.
Only jobs without a completed receipt are scheduled. An all-completed resume
starts no render workers.

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

## Multiple outputs per scene

`--save-last-frame` adds a native final RGBA PNG without running the scene
again. A file primary uses its stem plus `.png`; a PNG-sequence directory uses
an adjacent `frames.png`. All primary and companion destinations are checked
before any render worker starts. Non-subdivided pairs support checkpoint/resume:
**both** publications must still match, and the journal/lock must be outside
both outputs. A late PNG collision retains an already published primary and
returns an incomplete pair, not a reusable success.

`--subdivide` emits one native clip per selected top-level `play()` or `wait()`
into `<Scene>/clips/`. Clip names retain their original play indices, including
with `-n` range selection. Completed clips survive later errors or interrupts;
the failed/cancelled outcome contains their typed partial collection receipt.
`--save-last-frame` can also add one adjacent `clips.png` to that collection.
No extra scene execution, frame clock, renderer or publication mechanism is
introduced. Each native artifact remains independently atomic, not the pair
or collection as a whole.

```sh
fmn-python scenes.py First Second --workers 2 --format gif \
  --subdivide --save-last-frame --video_dir rendered --keep-going --robot
```

The programmatic equivalents are `subdivide=True`, `save_last_frame=True`,
and `max_segments=N` on `render_source_scenes`. The segment limit applies per
scene and defaults to 10,000. Still-only formats cannot be subdivided or paired.
Clip collections cannot use checkpoint/resume; a partial collection is evidence
of published work, not permission to rerun into its existing directory.

A process crash or forced timeout before a receipt returns may leave unreceipted
files; those are never silently adopted.

## Reproducible parallel renders

`--reproducible` now supports fresh PNG stills, PNG sequences and native WAV
outputs in isolated workers, through the same guarded native sessions as the
sequential renderer. This is a separate mode from checkpoint reuse:

```sh
fmn-python scenes.py First Second --workers 2 --threads 4 \
  --reproducible --format png_sequence --video_dir certified-frames \
  --keep-going --robot
```

`--write_all` works as well. The API equivalent is
`render_source_scenes(source, directory, names, workers=2, reproducible=True)`;
source providers are constructed inside the workers, not passed from the host.
Each scene receives an independent adjacent `<destination>.manifest/` directory
containing native `manifest.fmnp` and its human-readable `manifest.txt`.

Before any render worker starts, the parent checks every output and sidecar
path and measures the actual installed CPython, NumPy and portal payloads.
Each worker must match those identities before importing scene source. Native
runtime checks also surround output/sidecar publication. An observer that edits
the installed runtime stops further admission, without relabeling a previously
verified publication as failed. No missing certificate is replaced by a flag
claim, and no failed batch silently switches to standard output.

The worker's effect audit begins before source import, not only before
`construct`. Its source provider retains the loader's exact compilation bytes
and lazy imports; supported asset reads and WAV cues use the existing native
closure mechanism. A reserved C1 input, `__fmn_parallel__/invocation.json`, also
binds the selected class and output-affecting invocation (format, explicit
resolution/FPS, animation range and writer options). Two different classes in
one module must not alias one closure merely because their source bytes match.
An authored source with that reserved name is refused, never overwritten.
Worker count and timeout are scheduling choices, not invocation identity.

Receipts cross processes as bounded JSON. The parent reconstructs the existing
`RenderResult`, checks the planned name/output/format, matching native digests,
measured runtime and entry-source identity, and hashes the exact published
FMNP bytes against the worker's transport receipt. That last hash is an
integrity check, **not a second FMNP parser or a replacement for the native
publisher's semantic validation**.

The batch JSON keeps `certified: false`: there is no newly certified aggregate.
`reproducible_requested` records the requested mode; `all_scenes_certified`
requires every selected scene to succeed with complete provenance. Mixed
success/failure under `--keep-going` retains the successful per-scene certificates
but returns a failing batch and exit 5. Fail-fast, timeout and interrupt behavior
remain unchanged; partial progress never implies an absent artifact.

This mode inherits the existing portal closure's limits, not a sandbox or a
certificate for arbitrary Python programs. Directory enumeration, network and
process effects observed by the audit are rejected; supported file reads are
captured and checked. Environment, clock, entropy and arbitrary native-extension
side effects are not made observable by spawning workers. Helpers and assets
may differ between workers; each scene records its own observed closure rather
than claiming a frozen batch-wide source snapshot.

Certification cannot combine with checkpoint/resume, paired PNG companions or
subdivision. GIF, Y4M, SVG, MP4/MOV and ffmpeg products are not certified by this
parallel mode. Unsupported combinations refuse before CLI source discovery.
Default sequential behavior and all existing standard parallel outputs remain
unchanged.

## Validation scope

`test_parallel_checkpoint.py` runs real spawned processes and the production
checkpoint/coordinator against byte-artifact worker fixtures; it does not claim
native raster validation. `test_parallel_cli.py` checks the production console
functions and checkpoint parser with a reduced native table.
`test_parallel_batch_native.py` separately exercises actual native PNG receipts,
fresh scene state and checkpoint recovery when the FrankenManim wheel is built.

`test_parallel_outputs_native.py` exercises real GIF/PNG pairs, PNG-sequence
recovery, per-play clips, range selection, cancellation and late collisions.
It compares parallel and sequential PNG bytes from the same native engine.
`test_parallel_output_receipts.py` separately rejects malformed or misbound
transport receipts; transport admission alone is not artifact verification.

`test_parallel_provenance.py` checks transport and invocation binding with explicit
runtime doubles. `test_parallel_certified_native.py` renders real PNG/WAV outputs,
checks moving frames at 1/4/16 threads, manifests across worker counts, import
and asset effects, and partial results. `test_parallel_certified_cli_native.py`
executes the real installed CLI, including negative pre-discovery controls.
`test_parallel_certified_worker_native.py` verifies actual runtime mismatch and
post-publication runtime edits. These are same-host tests, not a claim that a
new cross-platform certification matrix or performance gate has passed.
