"""Assemble encoded clips through the same bounded Reel path as native Rust.

The native boundary owns input reads, snapshots, hashing, process supervision
and create-only publication. Python normalizes arguments and exposes its receipt.
"""
from __future__ import annotations

from collections.abc import Iterable
import copy
from dataclasses import dataclass
import importlib
import operator
import os
from pathlib import Path
from typing import Any

_MAX_CLIPS = 4093
_U64_MAX = (1 << 64) - 1


def _positive(value: Any, name: str, maximum: int = _U64_MAX) -> int:
    if isinstance(value, bool):
        raise TypeError(f"{name} must be a positive integer, not bool")
    try:
        value = operator.index(value)
    except TypeError:
        raise TypeError(f"{name} must be a positive integer") from None
    if not 0 < value <= maximum:
        raise ValueError(f"{name} must lie between 1 and {maximum}")
    return value


def _path(value: os.PathLike[str] | str, name: str, cwd: str) -> str:
    text = os.fspath(value)
    if not isinstance(text, str):
        raise TypeError(f"{name} must be a text path")
    if not text or "\0" in text:
        raise ValueError(f"{name} must be nonempty and contain no NUL")
    drive, _ = os.path.splitdrive(text)
    if drive and not os.path.isabs(text):
        raise ValueError(f"{name} must not be a Windows drive-relative path")
    # The captured cwd anchors relative paths without changing filesystem
    # traversal. Collapsing `..` before following a symlink can select another
    # clip, executable, work directory, or output destination.
    return os.path.join(cwd, text)


@dataclass(frozen=True)
class VideoClipResult:
    """One authored input position and the identity of its frozen bytes."""

    source: Path
    bytes: int
    digest: str

    def as_dict(self) -> dict[str, Any]:
        return {"source": str(self.source), "bytes": self.bytes, "digest": self.digest}


@dataclass(frozen=True)
class VideoConcatResult:
    """A newly published movie and native evidence of its ordered inputs.

    Stream copying does not measure frame count or duration. Encoded movies
    are outside the certified artifact set; ``certified`` is always false.
    """

    destination: Path
    inputs: tuple[VideoClipResult, ...]
    total_input_bytes: int
    bytes: int
    digest: str
    ffmpeg_invocations: tuple[dict[str, Any], ...]
    ffmpeg_limits: dict[str, Any]
    source_limits: dict[str, int]

    @property
    def certified(self) -> bool:
        return False

    def as_dict(self) -> dict[str, Any]:
        return {
            "schema": "fmn.video_concat.v1",
            "destination": str(self.destination),
            "inputs": [entry.as_dict() for entry in self.inputs],
            "total_input_bytes": self.total_input_bytes,
            "bytes": self.bytes,
            "digest": self.digest,
            "certified": False,
            "ffmpeg_invocations": copy.deepcopy(list(self.ffmpeg_invocations)),
            "ffmpeg_limits": copy.deepcopy(self.ffmpeg_limits),
            "source_limits": dict(self.source_limits),
        }


def concatenate_video(
    inputs: Iterable[os.PathLike[str] | str],
    destination: os.PathLike[str] | str,
    *,
    ffmpeg_bin: os.PathLike[str] | str = "ffmpeg",
    workdir: os.PathLike[str] | str | None = None,
    max_clips: int = 1024,
    max_clip_bytes: int = 64 * 1024 * 1024,
    max_input_bytes: int = 256 * 1024 * 1024,
    timeout: int = 600,
    max_log_bytes: int = 1024 * 1024,
    max_output_bytes: int = 8 * 1024 * 1024 * 1024,
) -> VideoConcatResult:
    """Join compatible MP4/MOV clips into a new movie without re-encoding.

    ``inputs`` is consumed once, in order, and is bounded even for generators.
    Repeated clips remain repeated and consume the aggregate input budget
    again. Relative paths resolve against the working directory at entry,
    including when a PathLike or generator changes the working directory.
    Parent components retain their filesystem traversal semantics; ambiguous
    Windows drive-relative paths such as ``C:clip.mp4`` are refused.

    Each clip must be a self-contained MP4/MOV with compatible codecs, stream
    layouts and time bases. All streams are copied; there is no resize, trim,
    resample or codec conversion. Native snapshots bind receipts to the exact
    input bytes. Defaults retain at most 256 MiB of clip bytes in memory.
    ``workdir`` optionally selects an existing directory for the private
    process jobs and staged clip bytes; the default is the system temp directory.

    The destination must end in .mp4 or .mov and must not exist. Native
    publication also refuses a competing writer. ``timeout`` is the positive
    integer allowance in seconds for the concat invocation; tool probes have
    their own short bounds. The GIL is released during native input reads,
    process waits and publication. Active certified captures refuse this
    standard-only operation before reading clips or discovering ffmpeg.
    """
    # Capture before operator.index, __fspath__ or an iterator can run authored
    # code. No Python callback later can redirect an already supplied path.
    cwd = os.getcwd()
    policy = {
        "max_clips": _positive(max_clips, "max_clips", _MAX_CLIPS),
        "max_clip_bytes": _positive(max_clip_bytes, "max_clip_bytes"),
        "max_input_bytes": _positive(max_input_bytes, "max_input_bytes"),
        "timeout": _positive(timeout, "timeout"),
        "max_log_bytes": _positive(max_log_bytes, "max_log_bytes"),
        "max_output_bytes": _positive(max_output_bytes, "max_output_bytes"),
    }
    output = _path(destination, "destination", cwd)
    if Path(output).suffix.lower() not in {".mp4", ".mov"}:
        raise ValueError("destination must have an MP4 or MOV extension")
    executable = os.fspath(ffmpeg_bin)
    if not isinstance(executable, str):
        raise TypeError("ffmpeg_bin must be a text path or executable name")
    if not executable or "\0" in executable:
        raise ValueError("ffmpeg_bin must be nonempty and contain no NUL")
    if os.path.dirname(executable):
        executable = _path(executable, "ffmpeg_bin", cwd)
    private_root = None if workdir is None else _path(workdir, "workdir", cwd)
    if isinstance(inputs, (str, bytes, os.PathLike)):
        raise TypeError("inputs must be an iterable of clip paths, not a single path")
    paths = []
    for value in inputs:
        if len(paths) >= policy["max_clips"]:
            raise ValueError("input count exceeds max_clips")
        paths.append(_path(value, "input", cwd))
    if not paths:
        raise ValueError("at least one input clip is required")
    native = importlib.import_module("manimlib")
    assemble = getattr(native, "_portal_concatenate_video", None)
    if not callable(assemble):
        error_type = getattr(native, "_CapabilityError", RuntimeError)
        raise error_type("video composition requires a matching native FrankenManim wheel")
    receipt = assemble(tuple(paths), output, executable, policy, private_root)
    if receipt.get("schema") != "fmn.video_concat.v1" or receipt.get("certified") is not False:
        raise RuntimeError("the native wheel returned an incompatible video composition receipt")
    return VideoConcatResult(
        destination=Path(receipt["destination"]),
        inputs=tuple(VideoClipResult(Path(item["source"]), item["bytes"], item["digest"])
                     for item in receipt["inputs"]),
        total_input_bytes=receipt["total_input_bytes"],
        bytes=receipt["bytes"],
        digest=receipt["digest"],
        ffmpeg_invocations=tuple(copy.deepcopy(receipt["ffmpeg_invocations"])),
        ffmpeg_limits=copy.deepcopy(receipt["ffmpeg_limits"]),
        source_limits=dict(receipt["source_limits"]),
    )
