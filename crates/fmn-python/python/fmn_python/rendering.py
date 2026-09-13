"""Programmatic output through the portal's existing Lumen/Reel generation.

A generation must start before its Scene has adopted mobjects or advanced the
clock. Put construction in ``construct``, or use ``render_session`` around
imperative ``add``/``play``/``wait`` calls. Publication, no-clobber behavior,
frame budgets, codecs, and ffmpeg remain native responsibilities.
"""

from __future__ import annotations

import copy
import importlib
import operator
import os
import threading
from dataclasses import dataclass, replace
from pathlib import Path
from typing import Any

_FORMATS = frozenset({"png", "png_sequence", "gif", "y4m", "wav", "mp4", "mov"})
_VIDEO_FORMATS = frozenset({"mp4", "mov"})


def _positive_integer(value: Any, name: str, maximum: int = (1 << 32) - 1) -> int:
    if isinstance(value, bool):
        raise TypeError(f"{name} must be a positive integer, not bool")
    try:
        result = operator.index(value)
    except TypeError:
        raise TypeError(f"{name} must be a positive integer") from None
    if not 0 < result <= maximum:
        raise ValueError(f"{name} must lie between 1 and {maximum}")
    return result


def _seed(value: Any) -> int:
    # Same seed interpretation as the existing portal CLI; Python's optional
    # unseeded random streams are not a certified input closure.
    if value is None:
        return 0
    if isinstance(value, bool):
        raise TypeError("Scene.random_seed must be an integer or None")
    try:
        result = operator.index(value)
    except TypeError:
        raise TypeError("Scene.random_seed must be an integer or None") from None
    if not 0 <= result < 1 << 64:
        raise ValueError("Scene.random_seed must fit an unsigned 64-bit integer")
    return result


@dataclass(frozen=True)
class RenderResult:
    """A successful native publication receipt, not a render intent."""

    destination: Path
    format: str
    resolution: tuple[int, int]
    fps: int
    threads: int
    engine: str
    bytes: int
    digest: str
    frame_count: int | None
    sample_frames: int | None
    seed: int
    ffmpeg_invocations: tuple[dict[str, Any], ...] = ()
    certified: bool = False

    def as_dict(self) -> dict[str, Any]:
        result = {
            "destination": str(self.destination), "format": self.format,
            "resolution": list(self.resolution), "fps": self.fps,
            "threads": self.threads, "engine": self.engine, "bytes": self.bytes,
            "digest": self.digest, "frame_count": self.frame_count,
            "sample_frames": self.sample_frames, "seed": self.seed,
            "certified": self.certified,
            "ffmpeg_invocations": copy.deepcopy(list(self.ffmpeg_invocations)),
        }
        if self.format == "wav":
            result.update(sample_rate=48000, channels=2)
        return result


class RenderSession:
    """One explicitly owned, standard-mode native render generation.

    ``result`` is set only after the native sink finishes successfully. All
    exceptions, including KeyboardInterrupt, cancel an owned live generation.
    Failure to open a generation never cancels an existing external owner.
    """

    def __init__(
        self, scene: Any, destination: os.PathLike[str] | str, *,
        format: str | None = None, resolution: tuple[int, int] | None = None,
        fps: int | None = None, threads: int | None = None,
        _native: Any = None,
    ) -> None:
        native = importlib.import_module("manimlib") if _native is None else _native
        if not isinstance(scene, native.Scene):
            raise TypeError("render_session requires a Scene instance")
        path_text = os.fspath(destination)
        if not isinstance(path_text, str):
            raise TypeError("render destination must be a text path")
        if not path_text or "\0" in path_text:
            raise ValueError("render destination must be nonempty and contain no NUL")
        path = Path(path_text)
        if format is None:
            format = path.suffix.lower().lstrip(".") if path.suffix else "png_sequence"
        if not isinstance(format, str) or format not in _FORMATS:
            raise ValueError("render format must be png, png_sequence, gif, y4m, wav, mp4, or mov")
        camera = scene.camera
        dimensions = camera.get_pixel_shape() if resolution is None else resolution
        try:
            width, height = dimensions
        except (TypeError, ValueError):
            raise ValueError("render resolution must contain width and height") from None
        self.resolution = (_positive_integer(width, "width"), _positive_integer(height, "height"))
        self.fps = _positive_integer(camera.fps if fps is None else fps, "fps")
        self.threads = _positive_integer(
            max(1, min(os.cpu_count() or 1, 96)) if threads is None else threads,
            "threads",
        )
        self.seed = _seed(scene.random_seed)
        self.scene = scene
        self.destination = path
        self.format = format
        self.result: RenderResult | None = None
        self._state = "new"
        self._owner_thread = threading.get_ident()
        self._native = native

    def _check_owner(self) -> None:
        if threading.get_ident() != self._owner_thread:
            raise RuntimeError("a RenderSession is confined to its creating thread")

    def __enter__(self) -> RenderSession:
        self._check_owner()
        if self._state != "new":
            raise RuntimeError("a RenderSession can be entered only once")
        scene = self.scene
        if getattr(scene, "_fmn_owned_render_session", None) is not None:
            raise RuntimeError("this Scene already has an owned render generation")
        # The native start validates pristine Stage ownership, format budgets,
        # and sink capabilities. Do not abort if it refuses: another caller
        # (notably the CLI) might own the existing generation.
        scene._begin_native_output(
            str(self.destination), self.format, *self.resolution,
            self.fps, self.threads, self.seed,
        )
        self._state = "active"
        try:
            scene.__dict__["_fmn_owned_render_session"] = self
            camera = scene.camera
            # Resolution is output configuration, not a camera-pose edit.
            # Keep the exact scene frame identity and authored view/zoom.
            camera._core.set_pixel_shape(*self.resolution)
            camera.fps = self.fps
        except BaseException as error:
            self._cancel_preserving(error)
            raise
        return self

    def _release(self) -> None:
        if self.scene.__dict__.get("_fmn_owned_render_session") is self:
            self.scene.__dict__.pop("_fmn_owned_render_session")

    def abort(self) -> None:
        self._check_owner()
        if self._state != "active":
            return
        self._state = "aborted"
        try:
            self.scene._abort_render()
        finally:
            self._release()

    def _cancel_preserving(self, error: BaseException) -> None:
        try:
            self.abort()
        except BaseException as cleanup_error:
            error.add_note(f"native render cancellation also failed: {cleanup_error}")

    def finish(self) -> RenderResult:
        self._check_owner()
        if self._state == "finished":
            return self.result
        if self._state != "active":
            raise RuntimeError("only an active render generation can finish")
        try:
            # Native finish synchronizes the live camera/background/light,
            # supplies the static-scene final capture, joins ordered output,
            # and publishes atomically. No Python encoding or file copying.
            path, count, size, digest, engine, threads = self.scene._finish_render()
        except BaseException as error:
            self._cancel_preserving(error)
            raise
        self._state = "finished"
        self._release()
        self.result = RenderResult(
            destination=Path(path), format=self.format, resolution=self.resolution,
            fps=self.fps, threads=int(threads), engine=engine, bytes=int(size),
            digest=digest, frame_count=None if self.format == "wav" else int(count),
            sample_frames=int(count) if self.format == "wav" else None,
            seed=self.seed,
        )
        if self.format in _VIDEO_FORMATS:
            try:
                self.result = replace(self.result, ffmpeg_invocations=tuple(
                    copy.deepcopy(self.scene._render_invocations),
                ))
            except BaseException as error:
                error.add_note(f"native artifact is already published at {path}; ffmpeg provenance could not be read")
                raise
        return self.result

    def __exit__(self, exc_type, exc_value, traceback) -> bool:
        if exc_value is not None:
            self._cancel_preserving(exc_value)
        elif self._state == "active":
            self.finish()
        return False


def render_session(
    scene: Any, destination: os.PathLike[str] | str, *, format: str | None = None,
    resolution: tuple[int, int] | None = None, fps: int | None = None,
    threads: int | None = None,
) -> RenderSession:
    """Record imperative scene operations without a CLI or temporary script."""
    return RenderSession(scene, destination, format=format, resolution=resolution,
                         fps=fps, threads=threads)


def render_scene(
    scene: Any, destination: os.PathLike[str] | str, *, format: str | None = None,
    resolution: tuple[int, int] | None = None, fps: int | None = None,
    threads: int | None = None, scene_kwargs: dict[str, Any] | None = None,
) -> RenderResult:
    """Render a Scene instance or class and return its native artifact receipt.

    A class is constructed once with ``scene_kwargs``. An existing instance
    must still be pristine when rendering starts. ``EndScene`` is normal
    early completion; all other failures cancel without publishing a partial
    artifact. The host interpreter executes source directly, never a child
    Python process or a subprocess wrapper around the command-line program.
    """
    native = importlib.import_module("manimlib")
    if isinstance(scene, type) and issubclass(scene, native.Scene):
        scene = scene(**({} if scene_kwargs is None else dict(scene_kwargs)))
    elif scene_kwargs is not None:
        raise TypeError("scene_kwargs is valid only when rendering a Scene class")
    session = RenderSession(scene, destination, format=format, resolution=resolution,
                            fps=fps, threads=threads, _native=native)
    with session:
        try:
            scene.run()
        except native.EndScene:
            pass
    return session.result
