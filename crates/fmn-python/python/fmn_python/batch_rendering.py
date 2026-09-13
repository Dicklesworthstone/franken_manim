"""Ordered multi-scene export over the existing native render-session owner.

Scenes run sequentially on the calling thread; each native renderer still uses
its requested thread budget. This is not the native ``fmn batch`` farm and
never moves a Python scene, its proxies, or a live render across threads.
"""
from __future__ import annotations

import importlib
import os
import unicodedata
from collections.abc import Callable, Iterable, Mapping
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from .rendering import RenderResult, _FORMATS, _positive_integer, render_scene


@dataclass(frozen=True)
class RenderJob:
    """A named Scene class or pristine instance, with optional class arguments."""

    name: str
    scene: Any
    scene_kwargs: Mapping[str, Any] | None = None


@dataclass(frozen=True)
class SceneRenderOutcome:
    """One planned destination and its actual execution/publication outcome."""

    name: str
    destination: Path
    status: str
    result: RenderResult | None = None
    error_type: str | None = None
    message: str | None = None
    notes: tuple[str, ...] = ()

    def as_dict(self) -> dict[str, Any]:
        return {
            "name": self.name, "destination": str(self.destination),
            "status": self.status,
            "result": None if self.result is None else self.result.as_dict(),
            "error": None if self.error_type is None else {
                "type": self.error_type, "message": self.message, "notes": list(self.notes),
            },
        }


@dataclass(frozen=True)
class BatchRenderResult:
    """Ordered receipts, errors, and unattempted jobs; never all-or-nothing."""

    outcomes: tuple[SceneRenderOutcome, ...]

    @property
    def ok(self) -> bool:
        return bool(self.outcomes) and all(item.status == "succeeded" for item in self.outcomes)

    @property
    def counts(self) -> dict[str, int]:
        return {status: sum(item.status == status for item in self.outcomes)
                for status in ("succeeded", "failed", "cancelled", "not_run")}

    def as_dict(self) -> dict[str, Any]:
        return {
            "schema": "fmn-python.render-batch", "version": 1,
            "ok": self.ok, "execution": "sequential", "certified": False,
            "counts": self.counts,
            "outcomes": [item.as_dict() for item in self.outcomes],
        }


class BatchRenderError(RuntimeError):
    """Fail-fast execution stopped; ``result`` retains every completed receipt."""

    def __init__(self, result: BatchRenderResult):
        self.result = result
        failed = next(item for item in reversed(result.outcomes) if item.status == "failed")
        super().__init__(f"render batch stopped at {failed.name}: {failed.error_type}: {failed.message}")


def _name_key(name: Any) -> str:
    if not isinstance(name, str) or not name.isidentifier() or len(name.encode("utf-8")) > 128:
        raise ValueError("render job names must be identifiers of at most 128 UTF-8 bytes")
    # Reject names that collide on common case-insensitive/normalizing file
    # systems, including Windows device names, before starting any scene.
    key = unicodedata.normalize("NFKC", name).casefold()
    if key in {"con", "prn", "aux", "nul", "conin$", "conout$"} or (
        len(key) == 4 and key[:3] in {"com", "lpt"} and key[3] in "123456789"
    ):
        raise ValueError(f"render job name {name!r} is a reserved filesystem name")
    return key


def _plan(scenes: Any, directory: Any, format: str, native: Any, maximum: int):
    text = os.fspath(directory)
    if not isinstance(text, str):
        raise TypeError("batch output directory must be a text path")
    if not text or "\0" in text:
        raise ValueError("batch output directory must be nonempty and contain no NUL")
    # Freeze relative paths before authored constructors/run hooks can chdir.
    root = Path(text).resolve()
    if root.exists() and not root.is_dir():
        raise NotADirectoryError(str(root))
    if isinstance(scenes, Mapping):
        entries = (RenderJob(name, scene) for name, scene in scenes.items())
    elif isinstance(scenes, (str, bytes)) or isinstance(scenes, native.Scene) or isinstance(scenes, type):
        raise TypeError("render_scenes expects a mapping or an iterable of scenes/RenderJob entries")
    else:
        entries = iter(scenes)
    jobs, destinations, names, instances = [], [], set(), set()
    for index, entry in enumerate(entries):
        if index >= maximum:
            raise ValueError(f"render batch exceeds max_jobs={maximum}")
        if isinstance(entry, RenderJob):
            job = entry
        else:
            job = RenderJob(entry.__name__ if isinstance(entry, type) else type(entry).__name__, entry)
        key = _name_key(job.name)
        if key in names:
            raise ValueError(f"render job name collision: {job.name!r}")
        names.add(key)
        is_class = isinstance(job.scene, type) and issubclass(job.scene, native.Scene)
        if not is_class and not isinstance(job.scene, native.Scene):
            raise TypeError(f"render job {job.name!r} must contain a Scene class or instance")
        if not is_class:
            if id(job.scene) in instances:
                raise ValueError("a Scene instance cannot be used by two render jobs; supply its class instead")
            instances.add(id(job.scene))
        kwargs = job.scene_kwargs
        if kwargs is not None:
            if not is_class:
                raise TypeError("scene_kwargs is valid only for a Scene class")
            if not isinstance(kwargs, Mapping) or any(not isinstance(key, str) for key in kwargs):
                raise TypeError("scene_kwargs must be a mapping with string keys")
            kwargs = dict(kwargs)
        destination = root / job.name / "frames" if format == "png_sequence" else root / (job.name + "." + format)
        # This is an early diagnostic, not a replacement for Reel's atomic
        # no-clobber check (another process may create a destination later).
        if os.path.lexists(destination):
            raise FileExistsError(f"render destination already exists: {destination}")
        for parent in destination.parents:
            if parent.exists() and not parent.is_dir():
                raise NotADirectoryError(str(parent))
        jobs.append(RenderJob(job.name, job.scene, kwargs))
        destinations.append(destination)
    if not jobs:
        raise ValueError("render batch must contain at least one scene")
    return jobs, destinations


def _error_fields(error: BaseException) -> tuple[str, str]:
    try:
        message = str(error)
    except BaseException:
        message = "exception message could not be formatted"
    return type(error).__name__[:128], message[:4096]


def _error_notes(error: BaseException) -> tuple[str, ...]:
    # RenderSession notes distinguish a failed cancellation from the primary
    # error, and publication succeeded but provenance retrieval failed.
    try:
        notes = vars(error).get("__notes__", ())
    except BaseException:
        return ()
    if not isinstance(notes, (tuple, list)):
        return ()
    return tuple(note[:1024] for note in notes[:8] if isinstance(note, str))


def _attach_result(error: BaseException, outcomes: list[SceneRenderOutcome]) -> None:
    # Preserve KeyboardInterrupt/SystemExit and authored observer exceptions
    # rather than translating cancellation into an ordinary failed scene.
    try:
        error.render_batch_result = BatchRenderResult(tuple(outcomes))
    except BaseException:
        pass


def render_scenes(
    scenes: Mapping[str, Any] | Iterable[Any], directory: os.PathLike[str] | str, *,
    format: str = "png_sequence", resolution: tuple[int, int] | None = None,
    fps: int | None = None, threads: int | None = None,
    continue_on_error: bool = False,
    on_result: Callable[[SceneRenderOutcome], Any] | None = None,
    max_jobs: int = 1024,
) -> BatchRenderResult:
    """Render named scenes in input order using independent native sessions.

    Mappings provide names; iterables accept Scene classes, pristine instances,
    or RenderJob(name, scene, scene_kwargs). All jobs and common options are
    validated before constructing a scene. Native publication remains atomic
    per artifact, NOT for the batch. Completed artifacts are never rolled back.

    By default the first ordinary failure raises BatchRenderError with partial
    receipts and the original cause. continue_on_error returns a report with
    failed outcomes instead. KeyboardInterrupt/SystemExit always propagate;
    render_batch_result on the exception preserves progress. on_result runs
    after each outcome and cannot turn a published artifact into a failure.
    Observer exceptions stop the batch and carry the same progress attribute.
    """
    if not isinstance(format, str) or format not in _FORMATS:
        raise ValueError("render format must be png, png_sequence, gif, y4m, wav, mp4, or mov")
    if not isinstance(continue_on_error, bool):
        raise TypeError("continue_on_error must be bool")
    if on_result is not None and not callable(on_result):
        raise TypeError("on_result must be callable or None")
    maximum = _positive_integer(max_jobs, "max_jobs", 65536)
    if resolution is not None:
        try:
            width, height = resolution
        except (TypeError, ValueError):
            raise ValueError("render resolution must contain width and height") from None
        resolution = (_positive_integer(width, "width"), _positive_integer(height, "height"))
    fps = None if fps is None else _positive_integer(fps, "fps")
    threads = None if threads is None else _positive_integer(threads, "threads")
    native = importlib.import_module("manimlib")
    jobs, destinations = _plan(scenes, directory, format, native, maximum)
    outcomes = [SceneRenderOutcome(job.name, path, "not_run") for job, path in zip(jobs, destinations)]
    for index, (job, path) in enumerate(zip(jobs, destinations)):
        failure = None
        try:
            receipt = render_scene(job.scene, path, format=format, resolution=resolution,
                                   fps=fps, threads=threads, scene_kwargs=job.scene_kwargs)
        except Exception as error:
            failure = error
            kind, message = _error_fields(error)
            outcomes[index] = SceneRenderOutcome(job.name, path, "failed", error_type=kind, message=message, notes=_error_notes(error))
        except BaseException as error:
            kind, message = _error_fields(error)
            outcomes[index] = SceneRenderOutcome(job.name, path, "cancelled", error_type=kind, message=message, notes=_error_notes(error))
            _attach_result(error, outcomes)
            raise
        else:
            outcomes[index] = SceneRenderOutcome(job.name, receipt.destination, "succeeded", result=receipt)
        if on_result is not None:
            try:
                on_result(outcomes[index])
            except BaseException as error:
                _attach_result(error, outcomes)
                raise
        if failure is not None and not continue_on_error:
            raise BatchRenderError(BatchRenderResult(tuple(outcomes))) from failure
    return BatchRenderResult(tuple(outcomes))
