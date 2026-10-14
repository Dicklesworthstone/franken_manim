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
from itertools import islice
import os
from pathlib import Path
import sys

from ._process_batch import positive_integer, run_process_jobs, validate_job_timeout
from .batch_checkpoint import BatchCheckpoint
from .batch_rendering import (
    BatchRenderError, BatchRenderResult, SceneRenderOutcome, _name_key, _restored_outcome,
)
from .render_selection import animation_range as _animation_range
from .rendering import _FORMATS

_MAX_SOURCE_BYTES = 16 * 1024 * 1024


@dataclass(frozen=True)
class ParallelBatchRenderResult(BatchRenderResult):
    workers: int = 1
    reused: tuple[str, ...] = ()

    def as_dict(self):
        result = super().as_dict()
        result.update(execution="spawn", workers=self.workers, reused=list(self.reused))
        return result


def _source_digest(path):
    with path.open("rb") as stream:
        data = stream.read(_MAX_SOURCE_BYTES + 1)
    if len(data) > _MAX_SOURCE_BYTES:
        raise ValueError("scene entry source exceeds its 16 MiB budget")
    return hashlib.sha256(data).hexdigest()


def _render_source_job(job):
    # Include native initialization and source import in redirection. Scene
    # output is streamed, never retained as an unbounded in-memory log.
    with contextlib.redirect_stdout(sys.stderr):
        os.chdir(job["working_directory"])
        native = importlib.import_module("manimlib")
        from .scene_loading import SceneSource
        from .batch_rendering import render_scenes

        source = Path(job["source"])
        if _source_digest(source) != job["source_digest"]:
            raise RuntimeError("scene entry source changed after batch planning")
        with SceneSource(source, native.Scene) as loaded:
            # SceneSource owns exact compilation bytes. Check those, not a
            # second disk read that could miss an edit during source loading.
            compiled = loaded._source_bytes.get(source)
            if compiled is None or hashlib.sha256(compiled).hexdigest() != job["source_digest"]:
                raise RuntimeError("scene entry source changed during batch loading")
            name = job["name"]
            if name not in loaded.scenes:
                raise ValueError(f"no locally declared Scene named {name!r} in {source}")
            report = render_scenes(
                {name: loaded.scenes[name]}, job["directory"],
                continue_on_error=True, **job["options"],
            )
            return report.outcomes[0].as_dict()


def _outcome(name, destination, row):
    if row["status"] == "succeeded":
        # Restore the same native receipt type used by sequential batches;
        # never unpickle a worker-supplied Scene or arbitrary exception.
        if row.get("name") != name or row.get("destination") != str(destination):
            raise ValueError("worker receipt does not identify its planned scene destination")
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
                         threads=1, continue_on_error=False, on_result=None,
                         max_jobs=1024, animation_range=None, job_timeout=None,
                         checkpoint=None, resume=False, resume_key=None,
                         _output_options=None, _expected_source_digest=None,
                         _working_directory=None):
    """Render explicitly named local Scene classes in bounded fresh processes.

    Use this API under ``if __name__ == '__main__':`` in Python scripts. The
    CLI supplies that spawn-safe entry point automatically. Unlike an iterable
    of live Scenes, a source path and class names can be reconstructed safely.
    Source module side effects run once PER scene; interpreter globals, cwd,
    native proxies and random state are not shared between jobs. Imported
    helpers/assets are live inputs, not a frozen or certified input closure.

    Receipts retain input order; on_result runs in completion order in the
    parent. Fail-fast stops new admission and lets in-flight scenes finish.
    Ctrl-C requests worker cleanup and preserves every returned receipt. After
    a crash/timeout outputs may exist without a receipt; they are never deleted
    or silently reused. Native atomic publication/no-clobber remains decisive.

    workers is bounded to 64. threads defaults to ONE native thread per worker
    to avoid multiplying two automatic CPU budgets. job_timeout optionally
    bounds each worker, including startup. This path deliberately does not
    claim certification or accept live instances, paired output or subdivision;
    those remain available through render_scenes.

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
    """
    workers = positive_integer(workers, "workers", 64)
    maximum = positive_integer(max_jobs, "max_jobs", 65536)
    job_timeout = validate_job_timeout(job_timeout)
    if not isinstance(resume, bool):
        raise TypeError("resume must be bool")
    if checkpoint is None and (resume or resume_key is not None):
        raise ValueError("resume and resume_key require a checkpoint path")
    if not isinstance(continue_on_error, bool):
        raise TypeError("continue_on_error must be bool")
    if on_result is not None and not callable(on_result):
        raise TypeError("on_result must be callable or None")
    if not isinstance(format, str) or format not in _FORMATS:
        raise ValueError("unsupported parallel batch render format")
    selection = _animation_range(animation_range)
    if resolution is not None:
        try:
            width, height = resolution
        except (TypeError, ValueError):
            raise ValueError("render resolution must contain width and height") from None
        resolution = [positive_integer(width, "width", (1 << 32) - 1),
                      positive_integer(height, "height", (1 << 32) - 1)]
    fps = None if fps is None else positive_integer(fps, "fps", (1 << 32) - 1)
    threads = positive_integer(1 if threads is None else threads, "threads", 96)
    if isinstance(scene_names, (str, bytes)):
        raise TypeError("scene_names must be an iterable of class names, not a string")
    names = list(islice(iter(scene_names), maximum + 1))
    if not names or len(names) > maximum:
        raise ValueError(f"parallel batch requires 1..{maximum} scene names")
    keys = [_name_key(name) for name in names]
    if len(set(keys)) != len(keys):
        raise ValueError("parallel batch scene names collide")
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
    destinations = [root / name / "frames" if format == "png_sequence"
                    else root / (name + "." + format) for name in names]
    for destination in destinations:
        for parent in destination.parents:
            if parent.exists() and not parent.is_dir():
                raise NotADirectoryError(str(parent))
    options = dict(format=format, resolution=resolution, fps=fps, threads=threads,
                   animation_range=selection, _output_options=_output_options)
    # Freeze exactly the JSON data that the process scheduler will transmit,
    # before opening a journal or calling an observer that can mutate options.
    options = json.loads(json.dumps(options, allow_nan=False))
    jobs = [dict(source=str(source), source_digest=digest, directory=str(root),
                 name=name, options=options, working_directory=str(working_directory)) for name in names]
    owner = (contextlib.nullcontext(None) if checkpoint is None else
             BatchCheckpoint(checkpoint, resume=resume, key=resume_key))
    if checkpoint is not None:
        owner.validate_destinations(destinations)
    with owner as journal:
        existing = set() if journal is None else journal.existing_destinations
        for destination in destinations:
            if os.path.lexists(destination) and destination not in existing:
                raise FileExistsError(f"render destination already exists: {destination}")
        if journal is not None:
            journal.prepare_sources(names, destinations, options, source=source,
                                    source_digest=digest, working_directory=working_directory)
        completed = {} if journal is None else journal.completed
        outcomes = [_outcome(name, path, completed[name]) if name in completed else
                    SceneRenderOutcome(name, path, "not_run")
                    for name, path in zip(names, destinations)]
        pending = [index for index, name in enumerate(names) if name not in completed]
        reused = tuple(name for name in names if name in completed)

        def report():
            return ParallelBatchRenderResult(tuple(outcomes), workers=workers, reused=reused)

        def record():
            if journal is not None:
                journal.record(outcomes)

        def accept(local_index, row):
            index = pending[local_index]
            outcomes[index] = _outcome(names[index], destinations[index], row)
            # Checkpoint publication precedes arbitrary observer code. Only the
            # parent hashes artifacts and writes the journal; children never
            # acquire its lock or receive its path.
            record()
            if on_result is not None:
                on_result(outcomes[index])

        try:
            record()
            if on_result is not None:
                for index, name in enumerate(names):
                    if name in completed:
                        on_result(outcomes[index])
            if pending:
                rows = run_process_jobs([jobs[index] for index in pending], _render_source_job,
                                        workers=workers, continue_on_error=continue_on_error,
                                        on_result=accept, job_timeout=job_timeout)
                for index, row in zip(pending, rows):
                    outcomes[index] = _outcome(names[index], destinations[index], row)
            record()
        except BaseException as error:
            # The scheduler drains in-flight workers on interruption or a failed
            # observer. Merge those receipts through the pending-index map, not
            # the full scene list: resumed successes must never be overwritten.
            for index, row in zip(pending, getattr(error, "process_batch_outcomes", ())):
                try:
                    outcomes[index] = _outcome(names[index], destinations[index], row)
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
