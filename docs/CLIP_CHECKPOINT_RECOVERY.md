# Checkpointed rendering within one scene

The optional Python portal can resume an interrupted scene from its completed
native clips. It reexecutes the scene's Python and animation updates to rebuild
state, but does not rasterize or encode the verified prefix again. Only the
missing suffix opens new native recording generations. This is different from
batch recovery, which skips whole completed scenes.

## CLI

Start one explicitly named scene with a journal outside its clip directory:

```sh
fmn-python scenes.py Derivation --subdivide --format png_sequence \
  --video_dir rendered-clips --fps 30 --threads 4 \
  --clip-checkpoint progress.json --resume-key scene-assets-v1 --robot
```

Retry with the same source, working directory, selected scene and output options:

```sh
fmn-python scenes.py Derivation --subdivide --format png_sequence \
  --video_dir rendered-clips --fps 30 --threads 4 \
  --clip-checkpoint progress.json --resume-key scene-assets-v1 --resume --robot
```

`--clip-checkpoint=progress.json` also works. `--resume-key` is required, and
`--resume` is explicit: an existing journal is an error on a fresh run. The
journal and its persistent lock must be outside the collection and every clip.
The CLI freezes publication and journal paths before importing scene code.
Constructor changes to the working directory cannot redirect those paths.

Supported output formats are PNG sequences, GIF, Y4M, WAV, MP4 and MOV, subject
to their ordinary native/ffmpeg capabilities. Native video-quality options and
`-n START,END` range selection retain their existing meanings. Names keep the
original zero-based play/wait indices; skipped preroll and empty `play()` calls
do not consume a saved selected clip. Nested animation groups remain one clip.

This opt-in supports **one explicitly named scene**, not `--workers`,
`--write_all`, `--keep-going`, `--save-last-frame` or `--reproducible`. It also
cannot combine with the existing batch `--checkpoint`. Batch journals and clip
journals are distinct: a partial clip collection is not a completed scene.
Incompatible flags refuse before importing source. Ordinary CLI rendering and
existing batch recovery remain on their original routes.

Robot stdout contains one result with `subdivision`, `checkpoint` and
`resume_requested`. Import/construct diagnostics go to stderr through the
ordinary single-scene redirection, not a disposable worker or security sandbox.
Exit 0 means the scene and journal completed; 2 is invalid usage/preflight;
5 reports source/scene/replay failures; 6 reports output/journal I/O failure;
Ctrl-C remains 130. Errors retain typed receipts for known publications.
A missing receipt never implies that a file is absent.

## Python API

```python
from fmn_python import render_subdivided_scene

result = render_subdivided_scene(
    Derivation, "rendered-clips", format="png_sequence",
    fps=30, threads=4,
    checkpoint="progress.json", resume_key="scene-assets-v1", resume=True,
)
```

Use `resume=False` on the first run. Constructor arguments under `scene_kwargs`
must be plain JSON values when checkpointing. They are copied before invocation,
so a constructor cannot mutate the caller's inputs or the recorded plan.

For an imperative program, `subdivided_render_session` accepts the same recovery
options around commands on a pristine scene. Supply a fresh Scene and repeat
the same commands on each invocation. The returned session's `reused` property
lists the original play indices actually replayed during this invocation. The
ordinary immutable `SubdividedRenderResult` format remains unchanged.

## What is checked, and what is not

Before replay, the session matches the complete saved plan and hash-verifies
all recorded files. The plan includes class identity, resolved output geometry,
FPS, seed, threads, range, segment budget, writer options and supplied inputs.
The CLI additionally binds the canonical source path, selected class, initial
working directory and exact entry-source SHA-256. The loader verifies those
entry bytes before compilation. It is not a frozen snapshot of helper modules,
assets, installed libraries or arbitrary Python effects.

Every saved selected call must match its original index, kind, start frame and
end frame. A changed interval fails before new rendering starts. Early scene
termination cannot mark an unreplayed prefix complete, and a completed journal
cannot silently accept an extra selected call. Existing files are rechecked
before the first new native generation and when completing the collection.

**Python side effects repeat.** Constructors, imports, callbacks, updaters,
file reads and authored host actions run again. The supplied key is the
caller's assertion that code, helpers, assets and runtime have the same meaning.
Matching timing is not a proof that arbitrary Python rebuilt identical geometry:
changes in environment, randomness, external state or pixel-dependent callbacks
can change it. This feature is explicit artifact reuse, not a certified cache,
a generic interpreter checkpoint or a claim that arbitrary programs are replayable.
Change the key and use a new journal/output directory when relevant inputs change.

Audio cues are authored again during replay; later clips use the existing
native sample-exact audio window, including a cue crossing a saved boundary.
Old clips are immutable: a later newly authored cue never rewrites their audio.

## Failure and publication ownership

Reel still owns each independent atomic, create-only clip publication. The
session records each completed clip durably before returning from its play/wait
boundary, using the same exclusive lock and fsync/replace implementation as
batch checkpoints. PNG inventories include every canonical frame and the native
`FMN_COMPLETE` marker. Extra files, empty subdirectories, missing/changed files,
symlinks, forged paths and mismatched plans refuse reuse.

Failure or interruption cancels only the active native generation. Completed
clips and the last durable prefix remain available. A subsequent invocation
replays that prefix and retries the unpublished suffix. No scene objects are
pickled, no animation or frame clock is reimplemented, and no old output is
deleted or overwritten to permit recovery.

A crash or journal I/O failure between native publication and journal replacement
can leave an **unrecorded output**. Resume refuses it; it does not adopt it merely
because a similarly named file exists. The original error and partial receipt
remain available for diagnosis. After a complete run, an explicit resume still
executes Python and validates timing, but opens no render generations.

## Validation boundary

`test_clip_checkpoint_native.py` exercises actual moving PNG clips, exact
resumed-versus-fresh bytes and updater state, retained inode/mtime, zero native
render generations for complete replay, timing/range controls, tamper/link/file
refusals, exclusive locks and post-publication journal errors.

`test_clip_recovery_cli_native.py` invokes the real CLI in separate interpreters,
checks error and interrupt receipts, source binding, flag-shaped values, path
freezing and ordinary-render delegation. Native edge cases cover mid-clip
cancellation and byte-identical stereo 48 kHz WAV recovery across a cue boundary.
The video case uses actual ffmpeg/libx264 and retains encoder provenance.

These are Linux same-host checks using the unchanged d862adfb CI extension with
the affected Python sources, CPython 3.13.5 and NumPy 2.3.5. They are not a
rebuilt whole wheel, the full Rust/portal gate, a cross-platform matrix or a
qualified performance result. The feature needs no new Rust runtime dependency
and does not add Python to standalone native `fmn`.
