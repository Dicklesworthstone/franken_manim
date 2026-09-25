"""Export a Python-authored scene through the native FMTL/1 recorder.

Only the host lifecycle/ownership adapter lives here. Proscenium captures real
post-updater snapshots; its existing writer, reader and frame clock remain the
format authority. Exported bundles contain no Python executable or callback.
"""
from __future__ import annotations

from contextlib import contextmanager
from dataclasses import dataclass
import importlib
import os
from pathlib import Path
from typing import Any

from .rendering import RenderSession, _positive_integer
from .scene_execution import _note

_CAP = 256 * 1024 * 1024
_OWNER = "_fmn_owned_render_session"
_SEGMENTS = "_fmn_subdivision_session"  # Existing play/wait segment-observer protocol.


@dataclass(frozen=True)
class BundleExportResult:
    destination: Path
    frame_count: int
    segment_count: int
    bytes: int
    digest: str
    fps: int
    resolution: tuple[int, int]

    def as_dict(self) -> dict[str, Any]:
        return {"schema": "fmn.python-bundle-export", "version": 1,
                "format": "fmtl", "destination": str(self.destination),
                "frame_count": self.frame_count, "segment_count": self.segment_count,
                "bytes": self.bytes, "sha256": self.digest, "fps": self.fps,
                "replay_resolution": list(self.resolution), "certified_source": False}


def require_bundle_capability(native):
    for name in ("_portal_begin_bundle", "_portal_bundle_segment", "_portal_finish_bundle"):
        if not callable(getattr(native, name, None)):
            error = getattr(native, "_CapabilityError", RuntimeError)
            raise error("FMTL export requires a matching native wheel with SceneBundleRecorder support")


class _Segment:
    def __init__(self, owner, kind):
        self.owner, self.kind, self.started = owner, kind, False

    def select(self, selected):
        self.owner._validate_playback()
        if selected:
            self.owner._native._portal_bundle_segment(self.owner.scene, self.kind, True)
            self.started = True


class BundleExportSession:
    """Own one complete, atomically published, code-free scene generation.

    A pristine Scene is required. Setup/construct/tear_down are not invoked by
    this context itself; export_bundle runs them once. FMTL/1 admits captured
    planar vectors (including text outlines and Python updaters), not camera,
    lighting, background or audio tracks. Unsupported content fails closed.
    The resolution is a replay setting, not a field serialized by FMTL/1.
    Arbitrary Python source effects are not certified by exporting snapshots.
    """
    def __init__(self, scene, destination, *, resolution=None, fps=None,
                 max_frames=1_000_000, max_capture_bytes=_CAP, max_output_bytes=_CAP,
                 _native=None):
        native = importlib.import_module("manimlib") if _native is None else _native
        require_bundle_capability(native)
        # Reuse the existing output validation, without entering a pixel sink.
        self._configuration = RenderSession(
            scene, destination, format="png_sequence", resolution=resolution,
            fps=fps, threads=1, _native=native,
        )
        self.destination = Path(os.path.abspath(os.fspath(destination)))
        self.scene, self._native = scene, native
        self.resolution, self.fps = self._configuration.resolution, self._configuration.fps
        self.max_frames = _positive_integer(max_frames, "max_frames")
        self.max_capture_bytes = _positive_integer(max_capture_bytes, "max_capture_bytes", _CAP)
        self.max_output_bytes = _positive_integer(max_output_bytes, "max_output_bytes", _CAP)
        self.result = None
        self._state, self._failure = "new", None
        self._current = None
        self._segment_active = False
        self._artifact_published = False

    @property
    def artifact_published(self):
        return self._artifact_published

    def _validate_playback(self):
        scene = self.scene
        if (getattr(scene, "skip_animations", False)
                or getattr(scene, "presenter_mode", False)
                or getattr(scene, "start_at_animation_number", None) not in (None, 0)
                or getattr(scene, "end_at_animation_number", None) is not None):
            raise self._native._CapabilityError(
                "FMTL export requires complete offline playback; skip/range/presenter modes are unsupported"
            )
        if scene.camera.fps != self.fps:
            raise ValueError("bundle FPS cannot change during an export")

    def __enter__(self):
        self._configuration._check_owner()
        if self._state != "new":
            raise RuntimeError("a BundleExportSession can be entered only once")
        self._configuration._check_output_owner()
        if vars(self.scene).get("_fmn_scene_execution") is not None:
            raise RuntimeError("bundle export must start outside a play/wait call")
        # Freeze configuration before native generation acquisition. Pixel
        # shape affects camera aspect, so it must be set before validation.
        self._native._portal_begin_bundle(
            self.scene, str(self.destination), *self.resolution, self.fps,
            self._configuration.seed, self.max_frames, self.max_capture_bytes,
            self.max_output_bytes,
        )
        self._state = "active"
        namespace = vars(self.scene)
        namespace[_OWNER] = namespace[_SEGMENTS] = self
        self._current = self
        try:
            self.scene.camera._core.set_pixel_shape(*self.resolution)
            self.scene.camera.fps = self.fps
            self._validate_playback()
        except BaseException as error:
            self._abort_preserving(error)
            raise
        return self

    def _release(self):
        for key in (_OWNER, _SEGMENTS):
            if vars(self.scene).get(key) is self:
                vars(self.scene).pop(key)
        self._current = None

    def abort(self):
        self._configuration._check_owner()
        if self._state != "active":
            return
        self._state = "aborted"
        try:
            self.scene._abort_render()
        finally:
            self._release()

    def _abort_preserving(self, error):
        try:
            self.abort()
        except BaseException as cleanup:
            _note(error, "bundle cancellation also failed: " + type(cleanup).__name__)

    @contextmanager
    def _segment(self, kind):
        self._configuration._check_owner()
        if self._state != "active" or self._segment_active:
            raise RuntimeError("bundle export is inactive or already executing a segment")
        if self._failure is not None:
            raise self._failure
        self._segment_active = True
        segment = _Segment(self, kind)
        try:
            yield segment
            if segment.started:
                self._native._portal_bundle_segment(self.scene, kind, False)
        except BaseException as error:
            # EndScene before pre_play selected a segment is normal range/end
            # termination. Any in-flight failure is sticky, even if caught by
            # authored construct code; never publish a truncated animation.
            if segment.started or not isinstance(error, self._native.EndScene):
                if self._failure is None:
                    self._failure = error
            raise
        finally:
            self._segment_active = False

    def __exit__(self, error_type, error, traceback):
        self._configuration._check_owner()
        if error is not None:
            self._abort_preserving(error)
            return False
        if self._state != "active":
            raise RuntimeError("bundle export is no longer active")
        try:
            if self._failure is not None:
                raise self._failure
            self._validate_playback()
            path, frames, segments, size, digest = self._native._portal_finish_bundle(self.scene)
            self._artifact_published = True
            self.result = BundleExportResult(Path(path), frames, segments, size, digest,
                                             self.fps, self.resolution)
            self._state = "finished"
        except BaseException as primary:
            self._abort_preserving(primary)
            raise
        finally:
            self._release()
            self._failure = None
        return False


def export_bundle(scene, destination, *, scene_kwargs=None, **options):
    """Run a Scene class/instance once and return its immutable FMTL receipt."""
    native = importlib.import_module("manimlib")
    require_bundle_capability(native)
    if isinstance(scene, type) and issubclass(scene, native.Scene):
        scene = scene(**({} if scene_kwargs is None else dict(scene_kwargs)))
    elif scene_kwargs is not None:
        raise TypeError("scene_kwargs is valid only when exporting a Scene class")
    with BundleExportSession(scene, destination, _native=native, **options) as session:
        try:
            scene.run()
        except native.EndScene:
            pass
    if session.result is None:
        raise RuntimeError("scene did not publish its bundle")
    return session.result
