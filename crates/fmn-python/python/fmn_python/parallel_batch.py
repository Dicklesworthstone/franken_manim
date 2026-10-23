"""Process-isolated source-file batches over the existing native render owner.

Each scene is imported and rendered in a fresh host CPython interpreter. This
is an optional Python-portal API, not a dependency of native fmn/Studio. Only
names, options and JSON receipts cross processes; no live proxy is transferred.
"""
from __future__ import annotations

import contextlib
from dataclasses import dataclass
import hashlib
import importlib
import json
import os
from pathlib import Path
import sys

from ._process_batch import positive_integer, run_process_jobs, validate_job_timeout
from .batch_checkpoint import BatchCheckpoint
from .cancellation import _check_cancelled, _validate_cancellation
from .batch_rendering import (
    BatchRenderError, BatchRenderResult, SceneRenderOutcome, _name_key, _restored_outcome,
)
from .render_selection import animation_range as _animation_range
from .rendering import _FORMATS


@dataclass(frozen=True)
class ParallelBatchRenderResult(BatchRenderResult):
    workers: int = 1
    reused: tuple[str, ...] = ()

    def as_dict(self):
        result = super().as_dict()
        result.update(execution="spawn", workers=self.workers, reused=list(self.reused))
        return result


def _source_digest(path):
    from .parallel_discovery import _entry_digest

    return _entry_digest(path)


def _render_source_job(job):
    # Include native initialization and source import in redirection. Scene
    # output is streamed, never retained as an unbounded in-memory log.
    with contextlib.redirect_stdout(sys.stderr):
        os.chdir(job["working_directory"])
        native = importlib.import_module("manimlib")
        from .scene_loading import SceneSource
        from .batch_rendering import render_scenes

        options = dict(job["options"])
        reproducible = options.get("reproducible", False)
        from .effect_audit import recording
        snapshot = invocation = None
        if reproducible:
            from .parallel_provenance import freeze_invocation, worker_runtime

            invocation = freeze_invocation(job)
            snapshot = worker_runtime(native, job["runtime_identities"])
        source = Path(job["source"])
        if _source_digest(source) != job["source_digest"]:
            raise RuntimeError("scene entry source changed after batch planning")
        # Capture import-time effects too. The parent does NOT record its
        # orchestration subprocesses as though they were authored scene effects.
        with (recording(reproducible),
              SceneSource(source, native.Scene, entry_digest=job["source_digest"],
                          configuration=options.get("_configuration")) as loaded):
            # SceneSource owns exact compilation bytes. Check those, not a
            # second disk read that could miss an edit during source loading.
            compiled = loaded._source_bytes.get(source)
            if compiled is None or hashlib.sha256(compiled).hexdigest() != job["source_digest"]:
                raise RuntimeError("scene entry source changed during batch loading")
            name = job["name"]
            scene_name = job.get("scene_name", name)
            if scene_name not in loaded.scenes:
                raise ValueError(f"no locally declared Scene named {scene_name!r} in {source}")
            if "clip_recovery" in job:
                from .parallel_clip_recovery import render_clip_job

                return render_clip_job(job, loaded.scenes[scene_name])
            selected = {name: loaded.scenes[scene_name]}
            if "scene_name" in job:
                from .batch_rendering import RenderJob

                selected = [RenderJob(name, loaded.scenes[scene_name], job["scene_kwargs"])]
            if reproducible:
                from .parallel_provenance import executed_sources

                options.update(sources=lambda: executed_sources(loaded, job, invocation=invocation),
                               runtime_identities=snapshot.identities)
            try:
                report = render_scenes(
                    selected, job["directory"],
                    continue_on_error=True, **options,
                )
            except BaseException as error:
                # Cancellation can follow already published clips. Return the
                # owned batch's data-only outcome rather than discarding its
                # partial receipts in the generic process exception envelope.
                report = getattr(error, "render_batch_result", None)
                if not isinstance(report, BatchRenderResult) or len(report.outcomes) != 1:
                    raise
            row = report.outcomes[0].as_dict()
            if reproducible and row["status"] == "succeeded":
                from .parallel_provenance import worker_proof

                row["provenance"] = worker_proof(
                    snapshot, job["source_digest"], report.outcomes[0].result,
                    **({"invocation": invocation} if "invocation_sha256" in job else {}),
                )
            return row


def _outcome(name, destination, row, *, format=None, subdivide=False,
             save_last_frame=False, max_segments=10_000, bundle_options=None):
    if subdivide or save_last_frame:
        from .parallel_output import restore_output

        data = row.get("result")
        receipt = None
        if data is not None:
            if row.get("name") != name or row.get("destination") != str(destination):
                raise ValueError("worker receipt does not identify its planned scene destination")
            receipt = restore_output(data, destination=destination, format=format,
                                     subdivide=subdivide, save_last_frame=save_last_frame,
                                     max_segments=max_segments)
            if receipt.completed != (row["status"] == "succeeded"):
                raise ValueError("worker output completeness disagrees with its scene outcome")
        elif row["status"] == "succeeded":
            raise ValueError("successful worker omitted its output collection receipt")
        error = row.get("error") or {}
        return SceneRenderOutcome(name, destination, row["status"], result=receipt,
                                  error_type=error.get("type"), message=error.get("message"),
                                  notes=tuple(error.get("notes", ())))
    if row["status"] == "succeeded":
        # Restore the same native receipt type used by sequential batches;
        # never unpickle a worker-supplied Scene or arbitrary exception.
        if row.get("name") != name or row.get("destination") != str(destination):
            raise ValueError("worker receipt does not identify its planned scene destination")
        if format == "fmtl":
            from .bundle_checkpoint import restore_bundle_receipt

            if bundle_options is None:
                raise ValueError("FMTL worker receipt requires its admitted bundle options")
            receipt = restore_bundle_receipt(row.get("result"), destination=destination,
                                             options=bundle_options)
            return SceneRenderOutcome(name, destination, "succeeded", result=receipt)
        result = _restored_outcome(row)
        if result.destination != destination:
            raise ValueError("worker publication receipt changed destination")
        return result
    error = row.get("error") or {}
    return SceneRenderOutcome(
        name, destination, row["status"], error_type=error.get("type"),
        message=error.get("message"), notes=tuple(error.get("notes", ())),
    )


def render_source_scenes(source, directory, scene_names, *, workers=2,
                         format="png_sequence", resolution=None, fps=None,
                         threads=None, continue_on_error=False, on_result=None,
                         max_jobs=1024, animation_range=None, job_timeout=None,
                         cancellation=None,
                         checkpoint=None, resume=False, resume_key=None, clip_checkpoint_dir=None,
                         subdivide=False, max_segments=10_000, save_last_frame=False,
                         reproducible=False, bundle_camera=False, bundle_limits=None,
                         bundle_audio=False, bundle_audio_options=None,
                         _output_options=None, _expected_source_digest=None,
                         _working_directory=None, _configuration=None):
    """Render local Scene classes or named parameter variants in fresh processes.

    scene_names accepts strings or SourceRenderJob(name, scene, scene_kwargs).
    Distinct names can select the same locally declared class with independent
    constructor inputs. All names and plain JSON values are validated and
    snapshotted before source loading or observer callbacks. Outcomes and paths
    use job names; source lookup uses each job's scene identifier. Variants use
    the existing RenderJob path inside each child, never dynamic eval or pickle.
    Checkpoints bind both selected class and type-preserving constructor values;
    changing either refuses reuse, even with an unchanged resume_key. String-only
    calls retain their existing checkpoint format. Explicit parameter variants
    are standard-mode only until constructor-input certification is defined.

    Use this API under ``if __name__ == '__main__':`` in Python scripts. The
    CLI supplies that spawn-safe entry point automatically. Unlike an iterable
    of live Scenes, a source path and class names can be reconstructed safely.
    Source module side effects run once PER scene; interpreter globals, cwd,
    native proxies and random state are not shared between jobs. Imported
    helpers/assets are live inputs. In reproducible mode the existing guarded
    native session captures each scene's executed imports and supported reads.

    Receipts retain input order; on_result runs in completion order in the
    parent. Fail-fast stops new admission and lets in-flight scenes finish.
    Ctrl-C requests worker cleanup and preserves every returned receipt. After
    a crash/timeout outputs may exist without a receipt; they are never deleted
    or silently reused. Native atomic publication/no-clobber remains decisive.

    workers is bounded to 64. threads defaults to ONE native thread per worker
    to avoid multiplying two automatic CPU budgets. job_timeout optionally
    bounds each worker, including startup. This path does not accept live
    instances. reproducible=True supports fresh PNG stills/sequences and WAV:
    workers must match one measured installed runtime, each runs the existing
    effect-audited session, and each returns its own native FMNP sidecar. The
    aggregate stays uncertified; all_scenes_certified requires every scene's
    complete receipt. No checkpoint, paired or subdivided certification is
    implied. Runtime/sidecar checks precede admission; failures preserve outputs.

    subdivide=True emits one native clip per selected play/wait; completed
    clips survive later failures and interruptions in each outcome's result.
    save_last_frame=True adds a final PNG without executing a scene twice.
    Both compose in each worker through render_scenes, retaining the shared
    clock, recording and publication owners. Every primary/companion path is
    preflighted before spawning. Checkpoint/resume verifies every completed
    collection, including each clip and an optional final PNG, before skipping
    that scene. Empty completed selections are reusable. With checkpoint alone,
    partial collections remain create-only: that journal does not replay a saved
    clip prefix, and unfinished published files fail no-clobber preflight.
    Add clip_checkpoint_dir for within-scene recovery, as described below.

    checkpoint uses the same locked, atomic journal and artifact verifier as
    render_scenes, owned ONLY by the parent. Each completed receipt is recorded
    before on_result runs. resume=True verifies every completed artifact before
    spawning anything and runs only unfinished scenes. Reused receipts are
    delivered to on_result in input order before new jobs; report.reused names
    them explicitly. Entry-source digest, absolute paths, cwd, selected names
    and native render options must match. Worker count and timeout may change.
    Supply resume_key for helper/asset versions, as in sequential recovery:
    this is explicit reuse, not source-closure certification or a render cache.
    An output without a completed journal receipt still fails no-clobber.

    format="fmtl" exports complete portable recordings through each worker's
    existing render_scenes/BundleExportSession. bundle_camera, bundle_limits,
    bundle_audio and bundle_audio_options have the same meanings and defaults
    as sequential export. Explicit raster threads, ranges, subdivision, paired
    output and source certification are refused before launching a worker.
    Audio mixer workers are controlled separately by audio_threads. Parent-side
    receipt validation binds camera/audio mode, layout and capture limits to the
    admitted job. Checkpoint recovery preserves these settings and hash-verifies
    completed bundles; no-cue scenes may return ordinary picture-only receipts.

    clip_checkpoint_dir provides recovery WITHIN each subdivided scene.
    Each worker owns a distinct existing clip
    journal; the parent holds one locked source/variant/options plan. Resume
    reexecutes authored commands to reconstruct scene state, verifies saved
    clip indices and clocks, and renders only their missing suffix. An unstarted
    scene with no journal may start only when its destination is absent.
    Unrecorded outputs are never adopted or overwritten. Completed scenes also
    replay when used alone; this is not certified Python replay.
    It requires subdivide=True, excludes certification, and uses
    the same explicit resume/resume_key contract. save_last_frame=True composes
    the existing paired owner with an adjacent final-image journal: a completed
    PNG is verified against its exact clip collection and reused after replay;
    an absent image is captured only after the scene runs. An unjournaled PNG
    is refused, including after a publication-to-journal crash.
    Interrupted workers retain per-clip progress even without a terminal receipt.

    Supply BOTH checkpoint and clip_checkpoint_dir from the initial invocation
    for two-level recovery. The parent batch journal hash-verifies and skips
    completed scenes without importing or constructing them; only unfinished
    scenes replay their saved clip prefix. report.reused names the whole-scene
    skips, not the clips reused inside workers. Both journals bind each other's
    location and must be retained; changing or removing either mode refuses
    resume. The batch journal and its lock must be outside the clip state tree
    as well as all outputs. A completed child journal without a terminal batch
    receipt remains an unfinished job: it replays, never guesses whole-scene
    success. Each receipt is durably recorded before its observer is called.

    cancellation accepts a parent-owned CancellationToken. Call cancel() from
    another thread or on_result to stop admission and clean up in-flight workers.
    BatchCancelled (a KeyboardInterrupt subtype) carries render_batch_result
    once the scene plan is admitted; returned publications are checkpointed
    before it propagates. A pre-cancelled call acquires no worker or journal.
    Tokens are not serialized or included in checkpoint identity. Use a fresh
    token for resume; no completed artifact is removed or silently overwritten.
    """
    _validate_cancellation(cancellation)
    _check_cancelled(cancellation)
    workers = positive_integer(workers, "workers", 64)
    maximum = positive_integer(max_jobs, "max_jobs", 65536)
    job_timeout = validate_job_timeout(job_timeout)
    if not isinstance(resume, bool):
        raise TypeError("resume must be bool")
    if checkpoint is None and clip_checkpoint_dir is None and (resume or resume_key is not None):
        raise ValueError("resume and resume_key require a checkpoint path")
    if not isinstance(continue_on_error, bool):
        raise TypeError("continue_on_error must be bool")
    if on_result is not None and not callable(on_result):
        raise TypeError("on_result must be callable or None")
    if not isinstance(format, str) or format not in _FORMATS | {"fmtl"}:
        raise ValueError("unsupported parallel batch render format")
    if not isinstance(subdivide, bool) or not isinstance(save_last_frame, bool):
        raise TypeError("subdivide and save_last_frame must be bool")
    if subdivide:
        max_segments = positive_integer(max_segments, "max_segments", 100_000)
    elif max_segments != 10_000:
        raise ValueError("max_segments requires subdivide=True")
    if not isinstance(reproducible, bool):
        raise TypeError("reproducible must be bool")
    if clip_checkpoint_dir is not None:
        from .parallel_clip_recovery import validate_clip_recovery

        validate_clip_recovery(checkpoint=checkpoint, subdivide=subdivide,
                               save_last_frame=save_last_frame, reproducible=reproducible, format=format)
    from .source_jobs import prepare_source_jobs

    names, variants = prepare_source_jobs(scene_names, maximum)
    if variants is not None and reproducible:
        raise RuntimeError("CAPABILITY: parameterized source jobs do not certify constructor inputs")
    if type(bundle_camera) is not bool or type(bundle_audio) is not bool:
        raise TypeError("bundle_camera and bundle_audio must be bool")
    if format != "fmtl" and (bundle_camera or bundle_audio or bundle_limits is not None
                             or bundle_audio_options is not None):
        raise ValueError("bundle camera, audio and limits require format='fmtl'")
    if format == "fmtl" and (reproducible or subdivide or save_last_frame):
        native = importlib.import_module("manimlib")
        raise native._CapabilityError(
            "FMTL process batches exclude source certification, subdivision and paired output"
        )
    if reproducible:
        native = importlib.import_module("manimlib")
        from .batch_provenance import validate_batch_mode

        validate_batch_mode(native, format, True, checkpoint)
        if subdivide or save_last_frame:
            raise RuntimeError("CAPABILITY: reproducible parallel batches exclude paired output and subdivision")
    if subdivide or save_last_frame:
        native = importlib.import_module("manimlib")
        from .paired_output import companion_png_path, validate_paired_mode
        from .subdivision_rendering import validate_subdivided_mode

        if subdivide:
            validate_subdivided_mode(native, format)
        if save_last_frame:
            validate_paired_mode(native, format, checkpoint=checkpoint)
    selection = _animation_range(animation_range)
    if resolution is not None:
        try:
            width, height = resolution
        except (TypeError, ValueError):
            raise ValueError("render resolution must contain width and height") from None
        resolution = [positive_integer(width, "width", (1 << 32) - 1),
                      positive_integer(height, "height", (1 << 32) - 1)]
    if subdivide and resolution is not None and resolution[0] * resolution[1] > 16_777_216:
        raise ValueError("subdivision requires at most 16777216 pixels")
    fps = None if fps is None else positive_integer(fps, "fps", (1 << 32) - 1)
    bundle_options = None
    if format == "fmtl":
        from .parallel_bundle import prepare_bundle_options

        bundle_options = prepare_bundle_options(
            importlib.import_module("manimlib"), camera=bundle_camera, audio=bundle_audio,
            limits=bundle_limits, audio_options=bundle_audio_options, resolution=resolution,
            fps=fps, threads=threads, animation_range=selection, subdivide=subdivide,
            save_last_frame=save_last_frame, reproducible=reproducible,
            output_options=_output_options,
        )
    else:
        threads = positive_integer(1 if threads is None else threads, "threads", 96)
    for path, label in ((source, "source"), (directory, "directory")):
        value = os.fspath(path)
        if not isinstance(value, str) or not value or "\0" in value:
            raise ValueError(label + " must be a nonempty text path without NUL")
    source, root = Path(source).resolve(), Path(directory).resolve()
    digest = _source_digest(source)
    if _expected_source_digest is not None and digest != _expected_source_digest:
        raise RuntimeError("scene entry source changed after discovery")
    working_directory = Path.cwd() if _working_directory is None else Path(_working_directory).resolve()
    if not working_directory.is_dir():
        raise NotADirectoryError(str(working_directory))
    destinations = [root / name / "clips" if subdivide else
                    root / name / "frames" if format == "png_sequence" else
                    root / (name + "." + format) for name in names]
    still_destinations = ([companion_png_path(path, format, subdivide=subdivide) for path in destinations]
                          if save_last_frame else [])
    all_destinations = [*destinations, *still_destinations]
    for destination in all_destinations:
        for parent in destination.parents:
            if parent.exists() and not parent.is_dir():
                raise NotADirectoryError(str(parent))
    provenance = None
    options = dict(format=format, resolution=resolution, fps=fps, threads=threads,
                   animation_range=selection, _output_options=_output_options)
    if bundle_options is not None:
        options.update(bundle_options)
    if _configuration is not None:
        options["_configuration"] = _configuration
    if reproducible:
        options["reproducible"] = True
    # Keep existing single-artifact checkpoint plans byte-compatible. Only an
    # explicitly selected output mode adds new plan fields.
    if subdivide:
        options.update(subdivide=True, max_segments=max_segments)
    if save_last_frame:
        options["save_last_frame"] = True
    # Freeze exactly the JSON data that the process scheduler will transmit,
    # before opening a journal or calling an observer that can mutate options.
    options = json.loads(json.dumps(options, allow_nan=False))
    jobs = [dict(source=str(source), source_digest=digest, directory=str(root),
                 name=name, options=options, working_directory=str(working_directory)) for name in names]
    if variants is not None:
        for job, variant in zip(jobs, variants):
            job.update(scene_name=variant["scene"], scene_kwargs=variant["scene_kwargs"])
    if reproducible:
        from .parallel_provenance import ParallelProvenance, freeze_invocation

        invocation_digests = {job["name"]: hashlib.sha256(freeze_invocation(job)).hexdigest()
                              for job in jobs}
        provenance = ParallelProvenance(native, format, destinations, digest,
                                        invocation_digests=invocation_digests)
        for job in jobs:
            job["runtime_identities"] = provenance.identities
            job["invocation_sha256"] = invocation_digests[job["name"]]
    owner = (contextlib.nullcontext(None) if checkpoint is None else
             BatchCheckpoint(checkpoint, resume=resume, key=resume_key))
    if checkpoint is not None:
        owner.validate_destinations(all_destinations)
    clip_owner = contextlib.nullcontext(None)
    if clip_checkpoint_dir is not None:
        from .parallel_clip_recovery import ParallelClipPlan

        clip_owner = ParallelClipPlan(clip_checkpoint_dir, resume=resume, key=resume_key)
        clip_owner.validate_destinations(all_destinations)
        if checkpoint is not None:
            # Check namespaces before either owner creates directories/locks.
            # A completion journal inside the prefix tree is foreign state,
            # and aliasing plan.json(.lock) would acquire the same lock twice.
            clip_owner.validate_destinations([owner.path, owner.lock_path])
    journal_options = options
    if checkpoint is not None and clip_checkpoint_dir is not None:
        # Recovery identity is parent-only: do not send checkpoint paths as
        # renderer kwargs or change existing single-mode plan representations.
        journal_options = dict(options, clip_checkpoint_dir=str(clip_owner.directory))
    _check_cancelled(cancellation)
    with clip_owner as clip_journal, owner as journal:
        existing = set() if journal is None else journal.existing_destinations
        if clip_journal is not None:
            clip_journal.prepare(jobs, destinations,
                                 **({"completion_checkpoint": owner.path} if journal is not None else {}))
            existing.update(clip_journal.existing_destinations)
        for destination in all_destinations:
            if os.path.lexists(destination) and destination not in existing:
                raise FileExistsError(f"render destination already exists: {destination}")
        if journal is not None:
            journal.prepare_sources(names, destinations, journal_options, source=source,
                                    source_digest=digest, working_directory=working_directory,
                                    **({"variants": variants} if variants is not None else {}))
        if clip_journal is not None:
            _check_cancelled(cancellation)
            clip_journal.admit()
            for job in jobs:
                job["clip_recovery"] = clip_journal.recovery[job["name"]]
        completed = {} if journal is None else journal.completed

        def restore(name, path, row):
            if provenance is not None and row["status"] == "succeeded":
                return provenance.restore(name, path, row)
            return _outcome(name, path, row, format=format, subdivide=subdivide,
                            save_last_frame=save_last_frame, max_segments=max_segments,
                            bundle_options=options if format == "fmtl" else None)

        outcomes = [restore(name, path, completed[name]) if name in completed else
                    SceneRenderOutcome(name, path, "not_run")
                    for name, path in zip(names, destinations)]
        pending = [index for index, name in enumerate(names) if name not in completed]
        reused = tuple(name for name in names if name in completed)

        def report():
            return ParallelBatchRenderResult(tuple(outcomes), reproducible=reproducible,
                                             workers=workers, reused=reused)

        def record():
            if journal is not None:
                journal.record(outcomes)

        def accept(local_index, row):
            index = pending[local_index]
            outcomes[index] = restore(names[index], destinations[index], row)
            # Checkpoint publication precedes arbitrary observer code. Only the
            # parent hashes artifacts and writes the journal; children never
            # acquire its lock or receive its path.
            record()
            if on_result is not None:
                on_result(outcomes[index])
            if provenance is not None:
                # An observer may edit installed files. Stop before admitting
                # another worker, while preserving already published receipts.
                provenance.verify()

        try:
            _check_cancelled(cancellation)
            record()
            if on_result is not None:
                for index, name in enumerate(names):
                    if name in completed:
                        _check_cancelled(cancellation)
                        on_result(outcomes[index])
            _check_cancelled(cancellation)
            if pending:
                rows = run_process_jobs([jobs[index] for index in pending], _render_source_job,
                                        workers=workers, continue_on_error=continue_on_error,
                                        on_result=accept, job_timeout=job_timeout,
                                        **({"cancellation": cancellation} if cancellation is not None else {}))
                for index, row in zip(pending, rows):
                    outcomes[index] = restore(names[index], destinations[index], row)
            _check_cancelled(cancellation)
            record()
            _check_cancelled(cancellation)
        except BaseException as error:
            # The scheduler drains in-flight workers on interruption or a failed
            # observer. Merge those receipts through the pending-index map, not
            # the full scene list: resumed successes must never be overwritten.
            for index, row in zip(pending, getattr(error, "process_batch_outcomes", ())):
                if provenance is not None and outcomes[index].status == "succeeded":
                    # This receipt was already verified before its observer.
                    # A later runtime edit stops admission, not time-travels
                    # into a failure of an earlier certified publication.
                    continue
                try:
                    outcomes[index] = restore(names[index], destinations[index], row)
                except Exception as receipt_error:
                    outcomes[index] = SceneRenderOutcome(names[index], destinations[index], "failed",
                        error_type=type(receipt_error).__name__, message=str(receipt_error),
                        notes=("Invalid worker receipt; outputs may already exist.",))
            try:
                record()
            except BaseException as reporting_error:
                try:
                    error.add_note("parallel batch checkpoint also failed: " + str(reporting_error))
                except BaseException:
                    pass
            try:
                error.render_batch_result = report()
            except BaseException:
                pass
            raise
        result = report()
        if any(item.status == "cancelled" for item in result.outcomes):
            error = KeyboardInterrupt("parallel scene worker interrupted")
            error.render_batch_result = result
            raise error
        if not result.ok and not continue_on_error:
            raise BatchRenderError(result)
        return result
