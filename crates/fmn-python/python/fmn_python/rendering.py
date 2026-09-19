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
import sys
import threading
from dataclasses import dataclass, replace
from pathlib import Path
from typing import Any

from .render_selection import animation_range as _animation_range, apply_animation_range

_FORMATS = frozenset({"png", "png_sequence", "gif", "y4m", "wav", "svg", "mp4", "mov"})
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
    animation_range: tuple[int, int | None] | None = None
    audio_inputs: tuple[dict[str, Any], ...] = ()
    manifest: Path | None = None
    closure_digest: str | None = None

    def as_dict(self) -> dict[str, Any]:
        result = {
            "destination": str(self.destination), "format": self.format,
            "resolution": list(self.resolution), "fps": self.fps,
            "threads": self.threads, "engine": self.engine, "bytes": self.bytes,
            "digest": self.digest, "frame_count": self.frame_count,
            "sample_frames": self.sample_frames, "seed": self.seed,
            "certified": self.certified,
            "ffmpeg_invocations": copy.deepcopy(list(self.ffmpeg_invocations)),
            "audio_inputs": copy.deepcopy(list(self.audio_inputs)),
        }
        if self.animation_range is not None:
            result["animation_range"] = list(self.animation_range)
        if self.format == "wav":
            result.update(sample_rate=48000, channels=2)
        if self.manifest is not None:
            result["manifest"] = {
                "path": str(self.manifest),
                "closure_digest": self.closure_digest,
            }
        if self.closure_digest is not None:
            result["closure_digest"] = self.closure_digest
        if self.certified:
            result["artifact_digest"] = self.digest
        return result


def _runtime_identities(native: Any = None) -> dict[str, str]:
    import platform as _platform
    try:
        import numpy as _np
        numpy_id = f"NumPy {_np.__version__}"
    except Exception:
        numpy_id = "NumPy unknown"
    native_mod = importlib.import_module("manimlib") if native is None else native
    version = getattr(native_mod, "__version__", "unknown")
    abi = getattr(native_mod, "__abi_policy__", "cpython-3.13-full-abi")
    cpython = f"{_platform.python_implementation()} {_platform.python_version()}"
    wheel = f"franken-manim {version}"
    return {
        "cpython": cpython,
        "abi": abi,
        "wheel": wheel,
        "numpy": numpy_id,
    }


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
        animation_range: tuple[int, int | None] | None = None,
        reproducible: bool = False,
        sources: dict[str, bytes] | None = None,
        runtime_identities: dict[str, str] | None = None,
        _native: Any = None,
    ) -> None:
        native = importlib.import_module("manimlib") if _native is None else _native
        if not isinstance(reproducible, bool):
            raise TypeError("reproducible must be bool")
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
            raise ValueError("render format must be png, png_sequence, gif, y4m, wav, svg, mp4, or mov")
        if reproducible:
            if format not in ("png", "png_sequence", "wav"):
                error_type = getattr(native, "_CapabilityError", RuntimeError)
                raise error_type(
                    f"CAPABILITY: certified reproducibility excludes format {format!r}; "
                    "use --format png, png_sequence, or wav"
                )
            if sys.platform == "win32":
                error_type = getattr(native, "_CapabilityError", RuntimeError)
                raise error_type(
                    "CAPABILITY: windows-x86-64 is excluded from certified "
                    "reproducibility by ADR-0019"
                )
        _validate_writer_options(scene, format, native)
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
        self.animation_range = _animation_range(animation_range)
        self.reproducible = reproducible
        self.sources = sources
        self.runtime_identities = runtime_identities
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
        if self.reproducible:
            sidecar = self.destination.parent / (self.destination.name + ".manifest")
            if os.path.lexists(sidecar):
                raise FileExistsError(
                    f"manifest destination {sidecar} already exists; sidecars are no-clobber generations"
                )
        # The native start validates pristine Stage ownership, format budgets,
        # and sink capabilities. Do not abort if it refuses: another caller
        # (notably the CLI) might own the existing generation.
        scene._begin_native_output(
            str(self.destination), self.format, *self.resolution,
            self.fps, self.threads, self.seed, self.reproducible,
        )
        self._state = "active"
        try:
            scene.__dict__["_fmn_owned_render_session"] = self
            if self.animation_range is not None:
                apply_animation_range(scene, self.animation_range)
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
        manifest_path = None
        closure_digest = None
        if self.reproducible:
            publish_fn = getattr(self._native, "_portal_publish_manifest", None)
            if publish_fn is None:
                error_type = getattr(self._native, "_CapabilityError", RuntimeError)
                raise error_type(
                    "CAPABILITY: certified portal rendering awaits the complete "
                    "content-hashed input closure and provenance sidecar"
                )
            artifact_report = {
                "path": str(path),
                "digest": digest,
            }
            identities = self.runtime_identities or _runtime_identities(self._native)
            sources = self.sources or {}
            cue_assets = None
            audio_inputs = getattr(self.scene, "_render_audio_inputs", None)
            if audio_inputs:
                cue_assets = []
                for entry in audio_inputs:
                    cpath = entry.get("path")
                    if cpath:
                        p = Path(cpath)
                        try:
                            cue_assets.append((p.name, p.read_bytes()))
                        except OSError:
                            pass
            try:
                manifest_file_str, closure_digest_hex = publish_fn(
                    Path(self.destination),
                    self.format,
                    self.resolution,
                    self.fps,
                    self.threads,
                    self.seed,
                    artifact_report,
                    sources,
                    identities,
                    cue_assets,
                )
                manifest_path = Path(manifest_file_str)
                closure_digest = closure_digest_hex
            except BaseException as error:
                error.add_note(
                    f"native artifact is already published at {path}; provenance manifest could not be written"
                )
                raise

        self.result = RenderResult(
            destination=Path(path), format=self.format, resolution=self.resolution,
            fps=self.fps, threads=int(threads), engine=engine, bytes=int(size),
            digest=digest, frame_count=None if self.format == "wav" else int(count),
            sample_frames=int(count) if self.format == "wav" else None,
            seed=self.seed, animation_range=self.animation_range,
            certified=self.reproducible,
            manifest=manifest_path,
            closure_digest=closure_digest,
        )
        if self.format in _VIDEO_FORMATS or self.format == "wav":
            try:
                self.result = replace(
                    self.result,
                    ffmpeg_invocations=tuple(copy.deepcopy(self.scene._render_invocations)),
                    audio_inputs=tuple(copy.deepcopy(self.scene._render_audio_inputs)),
                )
            except BaseException as error:
                error.add_note(f"native artifact is already published at {path}; media provenance could not be read")
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
    animation_range: tuple[int, int | None] | None = None,
    reproducible: bool = False,
    sources: dict[str, bytes] | None = None,
    runtime_identities: dict[str, str] | None = None,
) -> RenderSession:
    """Record imperative scene operations without a CLI or temporary script."""
    return RenderSession(scene, destination, format=format, resolution=resolution,
                         fps=fps, threads=threads, animation_range=animation_range,
                         reproducible=reproducible, sources=sources,
                         runtime_identities=runtime_identities)


def render_scene(
    scene: Any, destination: os.PathLike[str] | str, *, format: str | None = None,
    resolution: tuple[int, int] | None = None, fps: int | None = None,
    threads: int | None = None, scene_kwargs: dict[str, Any] | None = None,
    animation_range: tuple[int, int | None] | None = None,
    reproducible: bool = False,
    sources: dict[str, bytes] | None = None,
    runtime_identities: dict[str, str] | None = None,
    _output_options: dict[str, Any] | None = None,
) -> RenderResult:
    """Render a Scene instance or class and return its native artifact receipt.

    A class is constructed once with ``scene_kwargs``. An existing instance
    must still be pristine when rendering starts. ``EndScene`` is normal
    early completion; all other failures cancel without publishing a partial
    artifact. The host interpreter executes source directly, never a child
    Python process or a subprocess wrapper around the command-line program.
    """
    animation_range = _animation_range(animation_range)
    native = importlib.import_module("manimlib")
    if isinstance(scene, type) and issubclass(scene, native.Scene):
        scene = scene(**({} if scene_kwargs is None else dict(scene_kwargs)))
    elif scene_kwargs is not None:
        raise TypeError("scene_kwargs is valid only when rendering a Scene class")
    if _output_options:
        _apply_output_options(scene, _output_options)
    session = RenderSession(scene, destination, format=format, resolution=resolution,
                            fps=fps, threads=threads, animation_range=animation_range,
                            reproducible=reproducible, sources=sources,
                            runtime_identities=runtime_identities, _native=native)
    with session:
        try:
            scene.run()
        except native.EndScene:
            pass
    if session.result is None:
        raise RuntimeError("scene execution ended without publishing its render generation")
    return session.result


def _apply_output_options(scene: Any, options: dict[str, Any]) -> None:
    """Apply parsed console overrides after construction, without replacing
    the scene's camera, pose, RGB background, or unrelated writer settings.
    """
    for option, attribute in (("vcodec", "video_codec"), ("pix_fmt", "pixel_format"),
                              ("ffmpeg_bin", "ffmpeg_bin")):
        value = options.get(option)
        if value is not None:
            setattr(scene.file_writer, attribute, value)
    if options.get("transparent", False):
        scene.camera.background_rgba[3] = 0.0


def _validate_writer_options(scene: Any, format: str, native: Any) -> None:
    writer = getattr(scene, "file_writer", None)
    if writer is None:
        return
    unsupported = []
    for name in ("subdivide_output", "open_file_upon_completion", "show_file_location_upon_completion"):
        if getattr(writer, name, False):
            unsupported.append(name)
    if format != "wav":
        for name in ("saturation", "gamma"):
            if getattr(writer, name, 1.0) != 1.0:
                unsupported.append(name)
    if format in {"png", "png_sequence"} and getattr(writer, "png_mode", "RGBA") != "RGBA":
        unsupported.append("png_mode")
    # Video codec, wire format and executable are validated and negotiated by
    # the native generation before it acquires a process/output reservation.
    if unsupported:
        error_type = getattr(native, "_CapabilityError", RuntimeError)
        raise error_type(
            "programmatic native output does not yet route these SceneFileWriter options: "
            + ", ".join(unsupported)
        )


def _configured_destination(scene: Any, destination: Any, format: str | None, native: Any):
    if destination is not None:
        # An explicit destination/format selects ONE output, independently of
        # the writer's movie-versus-still preference. RenderSession owns path
        # validation and suffix inference.
        return destination, format
    writer = getattr(scene, "file_writer", None)
    if writer is None:
        raise ValueError("Scene.render requires a destination when no SceneFileWriter is present")
    if format is None:
        movie = bool(writer.write_to_movie)
        still = bool(writer.save_last_frame)
        if movie and still:
            error_type = getattr(native, "_CapabilityError", RuntimeError)
            raise error_type(
                "Scene.render cannot publish a movie and a last-frame PNG in one generation; "
                "select one destination or format explicitly"
            )
        if movie:
            return writer.get_movie_file_path(), None
        format = "png" if still else "png_sequence"
    if not isinstance(format, str) or format not in _FORMATS:
        raise ValueError("render format must be png, png_sequence, gif, y4m, wav, svg, mp4, or mov")
    if format == "png":
        return writer.get_image_file_path(), format
    root = writer.get_output_file_rootname()
    if format == "png_sequence":
        return Path(root) / "frames", format
    return str(root) + "." + format, format


def install_scene_rendering(native: Any) -> None:
    """Bind the wheel's explicit output front door without changing Scene.run.

    ``run`` remains the engine lifecycle used by construct-only and externally
    owned CLI generations. ``render`` owns its generation and delegates to the
    same run hook. ``render_session`` exposes the same generation for imperative
    scenes without calling ``run`` or reconstructing the scene. Both bindings
    capture their owning native module, including in embedded interpreters.
    """
    if vars(native).get("_FMN_SCENE_RENDERING_INSTALLED", False):
        return
    Scene = native.Scene

    def render(
        self, destination=None, *, format=None, resolution=None, fps=None, threads=None,
        animation_range=None,
    ):
        destination, format = _configured_destination(self, destination, format, native)
        session = RenderSession(self, destination, format=format, resolution=resolution,
                                fps=fps, threads=threads, animation_range=animation_range, _native=native)
        with session:
            try:
                self.run()
            except native.EndScene:
                pass
        if session.result is None:
            raise RuntimeError("scene execution ended without publishing its render generation")
        self.render_result = session.result
        return session.result

    def scene_render_session(
        self, destination=None, *, format=None, resolution=None, fps=None, threads=None,
        animation_range=None,
    ):
        """Record imperative add/play/wait calls in one native output generation.

        Construction is side-effect free: the returned context acquires its
        generation only on entry. Read ``session.result`` after successful exit.
        Exceptions cancel the owned generation without publishing an artifact.
        """
        destination, format = _configured_destination(self, destination, format, native)
        return RenderSession(self, destination, format=format, resolution=resolution,
                             fps=fps, threads=threads, animation_range=animation_range, _native=native)

    scene_render_session.__name__ = "render_session"
    scene_render_session.__qualname__ = Scene.__qualname__ + ".render_session"
    scene_render_session.__module__ = Scene.__module__
    Scene.render_session = scene_render_session

    render.__name__ = "render"
    render.__qualname__ = Scene.__qualname__ + ".render"
    render.__module__ = Scene.__module__
    Scene.render = render
    native._FMN_SCENE_RENDERING_INSTALLED = True