"""Export a Python-authored scene through the native FMTL/1 recorder.

Only the host lifecycle/ownership adapter lives here. Proscenium captures real
post-updater snapshots; its existing writer, reader and frame clock remain the
format authority. Exported bundles contain no Python executable or callback.
"""
from __future__ import annotations

from contextlib import contextmanager
from collections.abc import Mapping
import copy
from dataclasses import dataclass
import importlib
import os
from pathlib import Path
from typing import Any

from .rendering import RenderSession, _apply_output_options, _positive_integer
from .scene_execution import _note

_CAP = 256 * 1024 * 1024
_COMPACT_DEFAULTS = {
    "max_retained_bytes": _CAP,
    "max_snapshot_bytes": 64 * 1024 * 1024,
    "max_snapshot_work_bytes": 16 * 1024 * 1024 * 1024,
}
_OWNER = "_fmn_owned_render_session"
_SEGMENTS = "_fmn_subdivision_session"  # Existing play/wait segment-observer protocol.


@dataclass(frozen=True)
class BundleSoundtrackInfo:
    sample_rate: int
    channels: int
    sample_frames: int

    def as_dict(self):
        return {"sample_rate": self.sample_rate, "channels": self.channels,
                "sample_frames": self.sample_frames}


@dataclass(frozen=True)
class BundleExportResult:
    destination: Path
    frame_count: int
    segment_count: int
    bytes: int
    digest: str
    fps: int
    resolution: tuple[int, int]
    camera_track: bool = False
    soundtrack: BundleSoundtrackInfo | None = None
    storage: str = "recorded"
    audio_inputs: tuple[dict[str, Any], ...] = ()
    ffmpeg_invocations: tuple[dict[str, Any], ...] = ()
    ffmpeg_limits: dict[str, Any] | None = None

    def as_dict(self) -> dict[str, Any]:
        result = {"schema": "fmn.python-bundle-export", "version": 1,
                "format": "fmtl", "destination": str(self.destination),
                "frame_count": self.frame_count, "segment_count": self.segment_count,
                "bytes": self.bytes, "sha256": self.digest, "fps": self.fps,
                "replay_resolution": list(self.resolution), "certified_source": False}
        if self.camera_track:
            result.update(camera_track=True, fmtl_minor=1)
        if self.storage == "compact":
            result.update(storage="compact", fmtl_minor=3)
        if self.soundtrack is not None:
            result.update(soundtrack=self.soundtrack.as_dict(), fmtl_minor=2)
            if self.storage == "compact":
                result["picture_fmtl_minor"] = 3
        if self.ffmpeg_limits is not None:
            result.update(audio_inputs=copy.deepcopy(list(self.audio_inputs)),
                          ffmpeg_invocations=copy.deepcopy(list(self.ffmpeg_invocations)),
                          ffmpeg_limits=copy.deepcopy(self.ffmpeg_limits))
        return result


def require_bundle_capability(native, *, camera=False, audio=False, audio_ffmpeg=False,
                              storage="recorded"):
    if type(camera) is not bool:
        raise TypeError("camera must be a bool")
    if type(audio) is not bool:
        raise TypeError("audio must be a bool")
    if type(audio_ffmpeg) is not bool:
        raise TypeError("audio_ffmpeg must be a bool")
    if audio_ffmpeg and not audio:
        raise ValueError("audio_ffmpeg requires audio=True")
    if storage not in ("recorded", "compact"):
        raise ValueError("bundle storage must be 'recorded' or 'compact'")
    begin = ("_portal_begin_compact_bundle" if storage == "compact" else
             "_portal_begin_soundtrack_bundle" if audio else
             "_portal_begin_camera_bundle" if camera else "_portal_begin_bundle")
    finish = "_portal_finish_soundtrack_bundle" if audio else "_portal_finish_bundle"
    for name in (begin, "_portal_bundle_segment", finish):
        if not callable(getattr(native, name, None)):
            error = getattr(native, "_CapabilityError", RuntimeError)
            raise error("FMTL export requires a matching native wheel with SceneBundleRecorder support")
    if audio_ffmpeg and getattr(native, "_PORTAL_BUNDLE_AUDIO_FFMPEG", False) is not True:
        error = getattr(native, "_CapabilityError", RuntimeError)
        raise error("FMTL compressed-audio export requires a matching native wheel with audio_ffmpeg support")


def _validated_limits(*, max_frames=1_000_000, max_capture_bytes=_CAP, max_output_bytes=_CAP):
    """Freeze the native recorder's bounds before constructing any batch job."""
    return {
        "max_frames": _positive_integer(max_frames, "max_frames"),
        "max_capture_bytes": _positive_integer(max_capture_bytes, "max_capture_bytes", _CAP),
        "max_output_bytes": _positive_integer(max_output_bytes, "max_output_bytes", _CAP),
    }


def _validated_storage(storage="recorded", compact_limits=None):
    """Compact limits do not redefine the legacy cumulative capture budget."""
    if storage not in ("recorded", "compact"):
        raise ValueError("bundle storage must be 'recorded' or 'compact'")
    if storage == "recorded":
        if compact_limits is not None:
            raise ValueError("compact_limits require storage='compact'")
        return None
    supplied = {} if compact_limits is None else dict(compact_limits)
    unknown = set(supplied) - _COMPACT_DEFAULTS.keys()
    if unknown:
        raise ValueError("unknown compact recording limit: " + ", ".join(sorted(map(repr, unknown))))
    return {
        key: _positive_integer(supplied.get(key, default), key, default)
        for key, default in _COMPACT_DEFAULTS.items()
    }


def _validated_batch_storage(*, storage="recorded", compact_limits=None, limits=None):
    """Freeze recording admission without changing legacy journal options."""
    if limits is not None and not isinstance(limits, Mapping):
        raise TypeError("bundle_limits must be a mapping or None")
    if compact_limits is not None and not isinstance(compact_limits, Mapping):
        raise TypeError("bundle_compact_limits must be a mapping or None")
    capture = _validated_limits(**({} if limits is None else dict(limits)))
    compact = _validated_storage(storage, compact_limits)
    if compact is None:
        return capture, {}
    if capture["max_capture_bytes"] != _CAP:
        raise ValueError("max_capture_bytes applies to recorded storage; use bundle_compact_limits")
    return capture, {"bundle_storage": "compact", "bundle_compact_limits": compact}


def _validated_audio_options(*, audio=False, audio_sample_rate=48_000, audio_channels=2,
                             audio_threads=1, max_audio_bytes=64 * 1024 * 1024,
                             audio_ffmpeg=False):
    if type(audio) is not bool:
        raise TypeError("audio must be a bool")
    if type(audio_ffmpeg) is not bool:
        raise TypeError("audio_ffmpeg must be a bool")
    if audio_ffmpeg and not audio:
        raise ValueError("audio_ffmpeg requires audio=True")
    rate = _positive_integer(audio_sample_rate, "audio_sample_rate", 384_000)
    channels = _positive_integer(audio_channels, "audio_channels", 2)
    threads = _positive_integer(audio_threads, "audio_threads", 64)
    budget = _positive_integer(max_audio_bytes, "max_audio_bytes", _CAP)
    if budget < 8:
        raise ValueError("max_audio_bytes must be at least 8")
    if not audio and (rate, channels, threads, budget) != (48_000, 2, 1, 64 * 1024 * 1024):
        raise ValueError("audio settings require audio=True")
    return rate, channels, threads, budget, audio_ffmpeg


def _validated_bundle_output_options(options=None, *, audio=False, audio_ffmpeg=False):
    """Freeze only the admitted decoder override before authored construction."""
    if options is None:
        return {}
    if not isinstance(options, Mapping):
        raise TypeError("bundle _output_options must be a mapping or None")
    frozen = dict(options)
    if set(frozen) - {"ffmpeg_bin"}:
        raise ValueError("bundle output options admit only ffmpeg_bin")
    if not frozen:
        return {}
    if audio is not True or audio_ffmpeg is not True:
        raise ValueError("bundle ffmpeg_bin overrides require audio=True and audio_ffmpeg=True")
    executable = frozen["ffmpeg_bin"]
    if not isinstance(executable, str):
        raise TypeError("bundle ffmpeg_bin must be a string")
    if not executable or "\0" in executable:
        raise ValueError("bundle ffmpeg_bin must be nonempty and contain no NUL")
    # A bare name remains a lazy search request. A relative executable path
    # must keep its meaning if source imports or constructors change cwd.
    if not os.path.isabs(executable) and (os.sep in executable or
            (os.altsep is not None and os.altsep in executable)):
        executable = os.path.abspath(executable)
    return {"ffmpeg_bin": executable}


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
    this context itself; export_bundle runs them once. By default, minor-0
    bundles admit planar vectors (including text and Python updaters) and work
    with the camera-less browser player. With camera=True, minor-1 bundles
    record each frame's view, lighting and background alongside 3D/raster/vector
    geometry for the native player. Replay needs no Python or scene code.
    Resolution controls capture aspect; native replay may change pixel size.
    With audio=True, native WAV cues are mixed into a self-contained minor-2
    soundtrack. audio_ffmpeg=True also admits recognized compressed cues
    through the existing governed decoder and frozen SceneFileWriter policy.
    Native WAV and no-cue exports never discover or run a decoder process.
    Arbitrary Python source effects and compressed decoding are not certified
    by exporting their observed pictures and PCM.
    storage="compact" records the same captures using lossless minor-3 blocks;
    compact_limits separately bounds retained bytes, one snapshot and cumulative
    snapshot work. The legacy max_capture_bytes option belongs to recorded storage.
    """
    def __init__(self, scene, destination, *, resolution=None, fps=None, camera=False,
                 max_frames=1_000_000, max_capture_bytes=_CAP, max_output_bytes=_CAP,
                 audio=False, audio_sample_rate=48_000, audio_channels=2,
                 audio_threads=1, max_audio_bytes=64 * 1024 * 1024,
                 audio_ffmpeg=False,
                 storage="recorded", compact_limits=None, _output_options=None, _native=None):
        native = importlib.import_module("manimlib") if _native is None else _native
        self._compact_limits = _validated_storage(storage, compact_limits)
        if storage == "compact" and max_capture_bytes != _CAP:
            raise ValueError("max_capture_bytes applies to recorded storage; use compact_limits")
        require_bundle_capability(native, camera=camera, audio=audio,
                                  audio_ffmpeg=audio_ffmpeg, storage=storage)
        self.storage = storage
        self._audio_options = _validated_audio_options(
            audio=audio, audio_sample_rate=audio_sample_rate, audio_channels=audio_channels,
            audio_threads=audio_threads, max_audio_bytes=max_audio_bytes,
            audio_ffmpeg=audio_ffmpeg)
        self._audio = audio
        self._audio_ffmpeg = audio_ffmpeg
        self._output_options = _validated_bundle_output_options(
            _output_options, audio=audio, audio_ffmpeg=audio_ffmpeg)
        self._camera_track = camera
        # Reuse the existing output validation, without entering a pixel sink.
        self._configuration = RenderSession(
            scene, destination, format="png_sequence", resolution=resolution,
            fps=fps, threads=1, _native=native,
        )
        self.destination = Path(os.path.abspath(os.fspath(destination)))
        self.scene, self._native = scene, native
        self.resolution, self.fps = self._configuration.resolution, self._configuration.fps
        limits = _validated_limits(max_frames=max_frames, max_capture_bytes=max_capture_bytes,
                                   max_output_bytes=max_output_bytes)
        self.max_frames = limits["max_frames"]
        self.max_capture_bytes = limits["max_capture_bytes"]
        self.max_output_bytes = limits["max_output_bytes"]
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
        if self._output_options:
            self._native._portal_validate_bundle_owner(self.scene)
            _apply_output_options(self.scene, self._output_options)
        # Freeze configuration before native generation acquisition. Pixel
        # shape affects camera aspect, so it must be set before validation.
        arguments = (self.scene, str(self.destination), *self.resolution, self.fps,
                     self._configuration.seed, self.max_frames, self.max_capture_bytes,
                     self.max_output_bytes)
        if self.storage == "compact":
            self._native._portal_begin_compact_bundle(
                self.scene, str(self.destination), *self.resolution, self.fps,
                self._configuration.seed, self.max_frames, self.max_output_bytes,
                self._camera_track, self._audio,
                *(self._compact_limits[key] for key in _COMPACT_DEFAULTS),
                *self._audio_options,
            )
        elif self._audio:
            self._native._portal_begin_soundtrack_bundle(
                *arguments, self._camera_track, *self._audio_options)
        else:
            begin = (self._native._portal_begin_camera_bundle if self._camera_track
                     else self._native._portal_begin_bundle)
            begin(*arguments)
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
            soundtrack = None
            if self._audio:
                receipt, facts = self._native._portal_finish_soundtrack_bundle(self.scene)
                self._artifact_published = True
                if facts is not None:
                    soundtrack = BundleSoundtrackInfo(*facts)
            else:
                receipt = self._native._portal_finish_bundle(self.scene)
                self._artifact_published = True
            path, frames, segments, size, digest = receipt
            audio_inputs, invocations, policy = (), (), None
            if self._audio_ffmpeg and soundtrack is not None:
                audio_inputs = tuple(copy.deepcopy(self.scene._render_audio_inputs))
                invocations = tuple(copy.deepcopy(self.scene._render_invocations))
                policy = copy.deepcopy(self.scene._render_ffmpeg_limits)
            self.result = BundleExportResult(Path(path), frames, segments, size, digest,
                                             self.fps, self.resolution, self._camera_track,
                                             soundtrack, self.storage, audio_inputs,
                                             invocations, policy)
            self._state = "finished"
        except BaseException as primary:
            self._abort_preserving(primary)
            raise
        finally:
            self._release()
            self._failure = None
        return False


def export_bundle(scene, destination, *, scene_kwargs=None, **options):
    """Run once; camera=True records its view, audio=True its sound cues.

    The default remains the planar browser-compatible format. Both modes use
    the same ownership, capture budgets and atomic create-only publication.
    Compressed cues require audio_ffmpeg=True and the governed host decoder.
    """
    native = importlib.import_module("manimlib")
    compact_limits = _validated_storage(options.get("storage", "recorded"), options.get("compact_limits"))
    if compact_limits is not None:
        # Preserve the checked values even if the caller supplied an iterator
        # or a mapping that authored construction could subsequently mutate.
        options["compact_limits"] = compact_limits
    if options.get("storage") == "compact" and options.get("max_capture_bytes", _CAP) != _CAP:
        raise ValueError("max_capture_bytes applies to recorded storage; use compact_limits")
    _validated_limits(**{key: options[key] for key in (
        "max_frames", "max_capture_bytes", "max_output_bytes") if key in options})
    require_bundle_capability(native, camera=options.get("camera", False),
                              audio=options.get("audio", False),
                              audio_ffmpeg=options.get("audio_ffmpeg", False),
                              storage=options.get("storage", "recorded"))
    _validated_audio_options(**{key: options[key] for key in (
        "audio", "audio_sample_rate", "audio_channels", "audio_threads", "max_audio_bytes",
        "audio_ffmpeg") if key in options})
    if "_output_options" in options:
        options["_output_options"] = _validated_bundle_output_options(
            options["_output_options"], audio=options.get("audio", False),
            audio_ffmpeg=options.get("audio_ffmpeg", False))
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
