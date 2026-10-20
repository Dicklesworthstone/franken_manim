"""Data-only transport for native clip collections and final-frame pairs.

Publication and checkpoint verification retain their existing owners. This
module reconstructs only immutable receipt types from bounded worker JSON;
paths and output kinds come from the admitted plan, never from a worker's
choice of a class, deserializer, filesystem destination or executable object.
"""
from __future__ import annotations

from pathlib import Path

from .batch_rendering import _restored_outcome
from .paired_output import PairedRenderResult, companion_png_path
from .subdivision import RenderSegment, SubdividedRenderResult

_PAIR_FIELDS = frozenset({"schema", "version", "destination", "still_destination",
                          "primary", "still", "completed", "unreceipted_artifacts"})
_CLIP_FIELDS = frozenset({"schema", "version", "destination", "completed", "frame_count",
                          "sample_frames", "bytes", "segments", "unreceipted_artifacts"})


def _header(data, destination, schema, fields):
    if (not isinstance(data, dict) or set(data) != fields or data.get("schema") != schema
            or type(data.get("version")) is not int or data["version"] != 1
            or type(data.get("completed")) is not bool
            or data.get("destination") != str(destination)):
        raise ValueError("worker output receipt does not match its schema and planned destination")


def _integer(value, name):
    if type(value) is not int or not 0 <= value < 1 << 63:
        raise ValueError("invalid worker receipt " + name)
    return value


def _plain(data, destination, format):
    if (not isinstance(data, dict) or data.get("destination") != str(destination)
            or data.get("format") != format or data.get("certified") is not False
            or any(key in data for key in ("schema", "manifest", "closure_digest", "artifact_digest"))):
        raise ValueError("worker publication does not match its planned output")
    result = _restored_outcome({"name": "publication", "result": data}).result
    if result.bytes <= 0 or not 0 <= result.seed < 1 << 64:
        raise ValueError("worker publication requires bytes and a valid seed")
    if format == "wav":
        if (result.frame_count is not None or result.sample_frames is None
                or type(data.get("sample_rate")) is not int or data["sample_rate"] != 48000
                or type(data.get("channels")) is not int or data["channels"] != 2):
            raise ValueError("invalid worker WAV publication")
    elif result.sample_frames is not None or result.frame_count is None or result.frame_count <= 0:
        raise ValueError("invalid worker frame publication")
    if format == "png" and result.frame_count != 1:
        raise ValueError("final PNG requires exactly one frame")
    return result


def _unreceipted(data, allowed):
    values = data["unreceipted_artifacts"]
    if (type(values) is not list or len(values) > 100_001
            or any(type(value) is not str or not allowed(value) for value in values)
            or len(set(values)) != len(values) or (data["completed"] and values)):
        raise ValueError("invalid worker unreceipted artifact inventory")
    return tuple(Path(value) for value in values)


def _segment_path(destination, index, format):
    stem = f"segment-{index:06d}"
    return destination / (stem if format == "png_sequence" else stem + "." + format)


def _clips(data, destination, format, maximum):
    _header(data, destination, "fmn.subdivided-render", _CLIP_FIELDS)
    values = data["segments"]
    if type(values) is not list or len(values) > maximum:
        raise ValueError("worker clip inventory exceeds its segment budget")
    segments, previous_index, previous_end = [], -1, 0
    for row in values:
        if not isinstance(row, dict) or set(row) != {"play_index", "kind", "start_frame", "end_frame", "render"}:
            raise ValueError("invalid worker clip record")
        index = _integer(row["play_index"], "play index")
        start, end = (_integer(row[name], name) for name in ("start_frame", "end_frame"))
        if index <= previous_index or start < previous_end or end < start or row["kind"] not in {"play", "wait"}:
            raise ValueError("worker clips do not preserve scene order and clock intervals")
        receipt = _plain(row["render"], _segment_path(destination, index, format), format)
        segments.append(RenderSegment(index, row["kind"], start, end, receipt))
        previous_index, previous_end = index, end
    published = {str(segment.render.destination) for segment in segments}

    def allowed(value):
        path = Path(value)
        stem = path.name if format == "png_sequence" else path.stem
        digits = stem.removeprefix("segment-")
        if not stem.startswith("segment-") or not 6 <= len(digits) <= 19 or not digits.isascii() or not digits.isdecimal():
            return False
        index = int(digits)
        return (index < 1 << 63 and value == str(_segment_path(destination, index, format))
                and value not in published)

    missing = _unreceipted(data, allowed)
    result = SubdividedRenderResult(destination, tuple(segments), data["completed"], missing)
    for name in ("frame_count", "sample_frames", "bytes"):
        actual = getattr(result, name)
        if type(data[name]) is not type(actual) or data[name] != actual:
            raise ValueError("worker clip aggregate disagrees with its publications: " + name)
    return result


def restore_output(data, *, destination, format, subdivide, save_last_frame, max_segments):
    """Restore one planned multi-artifact output, including partial publication."""
    destination = Path(destination)
    if not destination.is_absolute():
        raise ValueError("worker output destination must be absolute")
    if not save_last_frame:
        return _clips(data, destination, format, max_segments)
    _header(data, destination, "fmn.paired-render", _PAIR_FIELDS)
    still_destination = companion_png_path(destination, format, subdivide=subdivide)
    if data["still_destination"] != str(still_destination):
        raise ValueError("worker final PNG does not match its planned destination")
    primary = data["primary"]
    if primary is not None:
        primary = (_clips(primary, destination, format, max_segments) if subdivide else
                   _plain(primary, destination, format))
    still = None if data["still"] is None else _plain(data["still"], still_destination, "png")
    complete_primary = primary is not None and (not subdivide or primary.completed)
    if ((data["completed"] and (not complete_primary or still is None))
            or (still is not None and not complete_primary)):
        raise ValueError("worker output pair is missing its completed publications")
    allowed = set()
    if primary is None:
        allowed.add(str(destination))
    if still is None:
        allowed.add(str(still_destination))
    missing = _unreceipted(data, allowed.__contains__)
    # A readback can use a different engine/thread budget, but cannot represent
    # a different scene. Clip receipts intentionally carry no outer range.
    samples = (() if primary is None else
               tuple(segment.render for segment in primary.segments) if subdivide else (primary,))
    if still is not None:
        for sample in samples:
            for name in ("resolution", "fps", "seed") + (() if subdivide else ("animation_range",)):
                if getattr(sample, name) != getattr(still, name):
                    raise ValueError("worker pair publications disagree on " + name)
    return PairedRenderResult(destination, still_destination, primary, still, data["completed"], missing)
