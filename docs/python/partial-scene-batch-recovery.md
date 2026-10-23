# Resume inside unfinished scenes in a process batch

`--clip-checkpoint-dir` connects the existing single-scene clip-prefix owner to
isolated process batches. It retains already journaled clips when a scene fails,
is cancelled, exceeds its worker timeout, or exits before sending a terminal
receipt. The next explicit resume reconstructs scene state and renders only the
missing suffix, rather than rejecting every partial collection as an existing
output or rerasterizing its completed prefix.

```bash
fmn-python lessons.py Intro Derivation Summary --workers 4 \
  --subdivide --format gif --save-last-frame --video_dir recordings \
  --checkpoint recording-progress.json --clip-checkpoint-dir recording-state \
  --resume-key lessons-assets-v1 \
  --job-timeout 600 --batch-timeout 1800 --keep-going
```

Repeat with `--resume`. Named scenes, `--write_all`, and `--jobs-file` use the
same mode. `render_source_scenes(..., subdivide=True, checkpoint=FILE,
clip_checkpoint_dir=PATH, resume_key=KEY, resume=True)` is its programmatic
equivalent. Omit `checkpoint` to replay every scene instead of skipping completed
ones. GIF, Y4M, native PNG-sequence, WAV, MP4 and MOV clips retain their
existing format/capability requirements. This feature introduces no renderer, mixer or encoder.

## Two recovery levels, independently usable and composable

The existing `--checkpoint FILE` skips **whole completed scenes**, and supports
optional final-image pairs. Partial collections still refuse no-clobber there.

`--clip-checkpoint-dir DIRECTORY` alone reexecutes each scene's Python
commands, including already completed scenes, while the existing
`ResumableSubdivisionSession` verifies and skips rasterization/encoding of its
saved clip prefix. Consequently Python side effects repeat and the whole-scene
`reused` list stays empty. It is not a live-state snapshot or certified replay.
Choose this mode before the initial render; it does not adopt outputs from a
previous run without clip journals. The single-scene `--clip-checkpoint` route
is unchanged.

Use **both options from the initial invocation** to get two-level recovery. The
parent completion journal hash-verifies all completed outputs and skips those
scene jobs entirely, including their constructor and `construct()` calls. Only
unfinished jobs start workers, replay their saved clips, and render the missing
suffix. The batch receipt's `reused` list contains whole-scene skips in input
order; clips reused inside pending workers are not counted as skipped scenes.
This also works with `--save-last-frame`: completed pairs are verified and
skipped, while unfinished pairs retain their clip-prefix and final-image recovery.

The CLI still performs isolated source discovery on each invocation. Module-level
code can therefore run during discovery and when unfinished workers import it.
Whole-scene reuse is not a promise to suppress every side effect in the source
module. Within unfinished scenes, authored commands and side effects repeat.

Keep the completion journal and its lock **outside the prefix-state directory**
and all output collections/images. Both plans bind the other journal's canonical
location. Retain both on resume: adding/removing a recovery level, copying the
completion journal to a different path, or redirecting a plan is refused. Existing
single-level plans remain compatible with their original mode; do not retrofit
another level onto them.

A worker that completed its child journal but died before delivering a terminal
success receipt remains unfinished to the parent. It replays its prefix on retry,
including a previously completed final PNG where applicable; file existence is
never promoted into proof of whole-scene success. Successful terminal receipts
are durably written before observer callbacks. Cancellation and observer errors
preserve received progress and release both parent locks.

All completed artifacts are verified before pending scene workers start. Tampering
with a skipped scene's clip or PNG refuses the invocation rather than allowing
other jobs to proceed. Each journal is atomic, but the two journals are not a
single transaction. A crash during initial plan admission can leave only one
journal: the invocation refuses recovery rather than reconstructing missing
ownership. No scene workers start until both initial plans are published.

FMTL, still-only outputs and certification are explicitly refused for prefix
recovery. It requires `--workers` and `--subdivide`. In the CLI,
select a named batch, `--write_all`, or a job file; use the single-scene route for
a lone named scene without a batch selector.

## Final PNGs compose with prefix recovery

Add `--save-last-frame` (or `save_last_frame=True` in the API) to publish the
collection's adjacent `OUTPUT/JobName/clips.png`. This uses the existing paired
capture/publication owner. The initial run prepares the PNG, completes and
journals the clips, commits the PNG, then writes a separate final-image receipt
at `DIRECTORY/jobs/JobName.still.json`. Its lock is held through paired completion.

When a scene must replay, its worker validates that receipt against the exact completed clip
collection, resolved scene/camera/writer plan, and hashes of every clip and the
PNG. It reconstructs scene state as before. A completed, verified PNG does not
need another camera capture or publication. Its bytes and modification time
remain unchanged. An absent PNG with no saved image receipt is captured after
replay, so a scene that previously failed halfway can finish both outputs.
Empty clip selections can still have a final image. With prefix-only recovery,
the aggregate `reused` list remains empty: authored commands and their side
effects still execute. In combined mode, the parent instead skips completed
pairs, and `reused` names those skipped jobs.

A missing or altered previously receipted image refuses reuse, rather than
silently replacing it. An image that exists without a completed final-image
journal also refuses reuse. In particular, a crash after PNG publication but
before its journal is committed leaves an orphan that is preserved, not adopted.
An ordinary journal-write failure retains the in-memory PNG and primary receipts
in the failed paired outcome. Preparation failures and late image collisions
preserve the saved clips. An interruption during replay does not expose a saved
image as proof that the current scene invocation completed.

The paired mode is part of the parent plan from its first admission: it cannot
be added to or removed from an existing unpaired recovery plan. Keep the state
directory outside both the clip collections and final PNGs. Ordinary paired
renders without prefix recovery retain their fresh-output/no-clobber behavior.

## Ownership and input identity

The coordinator holds a kernel lock on `DIRECTORY/plan.json.lock` for the entire
invocation. The atomic plan binds entry-source bytes, invocation cwd, output
paths, scene order, selected classes, type-sensitive constructor inputs,
configuration and render options. Worker count and supervision deadlines may
change on resume, but native render settings must remain compatible. The
`resume_key` remains the caller's explicit version for helper modules, assets
and runtime inputs; it is not a content-hashed source closure.

Each worker receives its own `DIRECTORY/jobs/JobName.json` journal. Native clip
verification, exact replayed segment indices/kinds/clock intervals, prefix
inventory hashes, no-clobber publication and per-clip fsync remain with their
existing owners. The parent never shares a live scene or native handle between
workers. A missing child journal permits a fresh scene only when its destination
is absent, which lets fail-fast retries include previously unstarted jobs.

Keep the state directory outside all published clip collections. Links,
conflicting plans, foreign state and redirected journal destinations are refused.
Inert temporary files left by interrupted atomic journal writes are preserved,
not treated as receipts and not automatically deleted. No verification claims
an atomic snapshot against subsequent external filesystem mutation.

A crash **between artifact publication and journal publication** can still leave
an unrecorded clip. It is preserved and refused, never silently adopted or
overwritten. Durably journaled prefix progress survives a missing terminal worker
receipt; this does not make a filesystem write and its journal one transaction.

## Validation

`test_parallel_clip_recovery.py` exercises production parent-plan admission,
worker dispatch and delegation, real spawned processes, private receipt sockets,
locks, atomic files, hard exit, timeout, fail-fast retry and constructor variants.
Its durable byte writer is explicitly a native-rendering fixture, not encoded
media. `test_parallel_clip_recovery_cli.py` exercises both shipping CLI wrappers
through those process and journal owners, with native common-option parsing and
scene discovery supplied by explicit fixtures.

`test_parallel_clip_recovery_native.py` separately requires a matching installed
wheel. It checks real GIF prefix preservation and tamper refusal through the API,
and partial failure followed by resume through the installed console. Passing
protocol tests does not imply native rendering acceptance.

`test_paired_clip_recovery_protocol.py` and
`test_parallel_paired_clip_recovery.py` exercise the production paired owner,
final-image verifier, journal writes, worker dispatch and process cleanup with
explicit native playback/camera fixtures. `test_parallel_paired_clip_cli.py`
connects both CLI wrappers to those same owners. Paired cases in
`test_parallel_clip_recovery_native.py` require the built wheel and verify real
GIF/PNG output through the API and installed console separately.

`test_two_level_batch_recovery.py` and `test_two_level_batch_cli.py` exercise
combined completion/prefix recovery through the production parent journals,
worker admission, receipt transport and CLI wrappers with explicit native-byte
fixtures. They prove completed-job skipping, partial suffix retries, paired
images, terminal-receipt loss, observer/cancellation behavior, cross-bound plan
identity, and tamper/no-clobber refusal. Native two-level cases live in the same
installed-wheel acceptance module; fixture successes do not substitute for them.
