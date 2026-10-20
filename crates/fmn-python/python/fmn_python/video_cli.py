"""Portal console quality syntax; Reel remains the encoder-policy authority.

This module does not import manimlib, open source files, or start processes.
It consumes only the new quality flags; all existing option/value pairs are
forwarded intact to the native parser, including values that resemble flags.
"""
from __future__ import annotations

_VIDEO_FLAGS = {
    "--crf": "video_crf",
    "--preset": "video_preset",
    "--tune": "video_tune",
    "--video-bitrate": "video_bitrate",
    "--audio-bitrate": "audio_bitrate",
}
_QUALITY_WRITER_OPTIONS = tuple(_VIDEO_FLAGS.values())
_LIMIT_FLAGS = {
    "--ffmpeg-timeout": "ffmpeg_timeout",
    "--ffmpeg-max-log-bytes": "ffmpeg_max_log_bytes",
    "--ffmpeg-max-artifact-bytes": "ffmpeg_max_artifact_bytes",
    "--ffmpeg-max-input-bytes": "ffmpeg_max_input_bytes",
}
_VIDEO_FLAGS.update(_LIMIT_FLAGS)
VIDEO_VALUE_FLAGS = frozenset(_VIDEO_FLAGS)
VIDEO_WRITER_OPTIONS = tuple(_VIDEO_FLAGS.values())
VIDEO_HELP = """Compressed video quality (fmn-python):
  --crf N                 Software x264/x265 CRF, 0..51 (zero is retained).
  --preset NAME           Software x264/x265 compression-speed preset.
  --tune NAME             One native-validated encoder tune, e.g. animation.
  --video-bitrate BPS     Positive integer bits/second, instead of CRF.
  --audio-bitrate BPS     AAC soundtrack bits/second; video is stream-copied.
  --ffmpeg-timeout SECONDS       Positive wall-clock allowance per invocation.
  --ffmpeg-max-log-bytes BYTES    Captured stdout/stderr ceiling per stream.
  --ffmpeg-max-artifact-bytes N   Encoded/intermediate artifact ceiling.
  --ffmpeg-max-input-bytes N     Raw-video input ceiling; audio keeps its own cap.

Both --flag value and --flag=value are accepted; each setting is specified once.
Video quality applies to MP4/MOV, including named batches, --write_all and --subdivide.
Native outputs refuse quality controls; transparent MOV accepts audio bitrate and resource limits.
The shared native
negotiator rejects software-only controls on hardware encoders and unsupported
tunes. Omitting these options preserves encoder defaults; no quality or speed
improvement is inferred from a setting. Actual arguments appear in receipts.
The ffmpeg watchdog defaults to 24 hours, including scene construction and
frame rendering. Limits also apply to compressed sound decoding for WAV output.
Every bound is a positive integer; there is no unlimited sentinel. Video encode,
audio decode and final mux share the admitted policy, recorded in receipts.
"""


def _value(flag: str, text: str):
    if flag in _LIMIT_FLAGS:
        if not text or len(text) > 20 or any(char not in "0123456789" for char in text):
            raise ValueError(f"{flag} requires a positive unsigned decimal integer")
        value = int(text)
        if not 0 < value <= (1 << 64) - 1:
            raise ValueError(f"{flag} must lie between 1 and {(1 << 64) - 1}")
        return value
    if flag in {"--crf", "--video-bitrate", "--audio-bitrate"}:
        # Bound before int(): huge decimal strings are not a parser workload.
        if not text or len(text) > 10 or any(char not in "0123456789" for char in text):
            raise ValueError(f"{flag} requires an unsigned decimal integer")
        value = int(text)
        minimum, maximum = (0, 51) if flag == "--crf" else (1, (1 << 32) - 1)
        if not minimum <= value <= maximum:
            raise ValueError(f"{flag} must lie between {minimum} and {maximum}")
        return value
    # Do not duplicate the encoder-specific preset/tune catalog in Python.
    # This is lexical validation only; Rust owns exact names and compatibility.
    if not text or len(text) > 32 or any(char not in "abcdefghijklmnopqrstuvwxyz" for char in text):
        raise ValueError(f"{flag} requires one canonical lowercase name, not an argument string")
    return text


def take_video_options(arguments: list[str], value_flags=frozenset()):
    """Return untouched legacy tokens and explicit quality overrides."""
    forwarded, overrides, index = [], {}, 0
    while index < len(arguments):
        token = arguments[index]
        flag, separator, inline = token.partition("=")
        if flag in _VIDEO_FLAGS:
            name = _VIDEO_FLAGS[flag]
            if name in overrides:
                raise ValueError(f"{flag} must not be repeated")
            if separator:
                text = inline
            else:
                index += 1
                if index >= len(arguments):
                    raise ValueError(f"{flag} requires a value")
                text = arguments[index]
            overrides[name] = _value(flag, text)
        else:
            forwarded.append(token)
            if token in value_flags and index + 1 < len(arguments):
                index += 1
                forwarded.append(arguments[index])
        index += 1
    if "video_crf" in overrides and "video_bitrate" in overrides:
        raise ValueError("--crf and --video-bitrate are mutually exclusive")
    return forwarded, overrides


def validate_video_format(format: str, options, *, transparent: bool = False) -> None:
    """Reject explicitly ignored controls before console source execution."""
    if any(options.get(name) is not None for name in _LIMIT_FLAGS.values()):
        if format not in {"mp4", "mov", "wav"}:
            raise ValueError("ffmpeg resource controls require mp4, mov, or wav output")
    if not any(options.get(name) is not None for name in _QUALITY_WRITER_OPTIONS):
        return
    if format not in {"mp4", "mov"}:
        raise ValueError("video quality controls require mp4 or mov output")
    if transparent and any(options.get(name) is not None for name in _QUALITY_WRITER_OPTIONS
                           if name != "audio_bitrate"):
        raise ValueError("video quality controls require opaque MP4/MOV, not transparent MOV")


def apply_video_options(scene, options) -> None:
    """Apply explicit overrides without discarding zero or resetting defaults."""
    for name in VIDEO_WRITER_OPTIONS:
        value = options.get(name)
        if value is not None:
            setattr(scene.file_writer, name, value)


def validate_writer_video_format(writer, format: str) -> None:
    # Video parsing happens ONCE in native code, with ownership checks around
    # authored getters. Do not invoke those getters a second time here.
    if format not in {"mp4", "mov"}:
        names = _QUALITY_WRITER_OPTIONS if format == "wav" else VIDEO_WRITER_OPTIONS
        validate_video_format(format, {name: getattr(writer, name, None)
                                       for name in names})
