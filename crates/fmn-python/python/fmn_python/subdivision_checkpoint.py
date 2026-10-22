"""Whole-scene recovery for verified clip collections in process batches.

Reuse the existing clip reader and per-clip inventory, never infer receipts
from directory names. This is completed-scene reuse, not prefix replay: an
unfinished scene's files remain create-only and are not silently adopted.
"""
from __future__ import annotations

import os
from pathlib import Path

from .batch_checkpoint import _MAX_FILES, _inventory, _json, _signature
from .paired_output import companion_png_path
from .parallel_output import restore_output


def collection_destinations(destination, options):
    destination = Path(destination)
    return (destination, companion_png_path(destination, options["format"], subdivide=True)) if (
        options.get("save_last_frame") is True
    ) else (destination,)


def collection_receipt(data, destination, options):
    """Restore and bind a complete collection to its admitted batch options."""
    if options.get("subdivide") is not True:
        raise ValueError("clip collection recovery requires a subdivided plan")
    maximum = options.get("max_segments", 10_000)
    if type(maximum) is not int or not 1 <= maximum <= 100_000:
        raise ValueError("invalid checkpoint collection segment budget")
    if options["format"] not in {"png_sequence", "gif", "y4m", "wav", "mp4", "mov"}:
        raise ValueError("invalid checkpoint collection format")
    paired = options.get("save_last_frame") is True
    receipt = restore_output(data, destination=destination, format=options["format"],
                             subdivide=True, save_last_frame=paired, max_segments=maximum)
    if not receipt.completed:
        raise ValueError("checkpoint collection must be complete before reuse")
    primary = receipt.primary if paired else receipt
    selection = options.get("animation_range")
    samples = [segment.render for segment in primary.segments]
    if paired:
        samples.append(receipt.still)
    for sample in samples:
        for name in ("resolution", "fps"):
            expected = options.get(name)
            if expected is not None and _json(getattr(sample, name)) != _json(expected):
                raise ValueError("checkpoint collection does not match admitted " + name)
        if samples:
            for name in ("resolution", "fps", "seed"):
                if _json(getattr(sample, name)) != _json(getattr(samples[0], name)):
                    raise ValueError("checkpoint collection publications disagree on " + name)
    for segment in primary.segments:
        if segment.render.animation_range is not None:
            raise ValueError("checkpoint clips must not carry an outer animation range")
        if selection is not None and (segment.play_index < selection[0] or (
                selection[1] is not None and segment.play_index >= selection[1])):
            raise ValueError("checkpoint clip is outside the admitted animation range")
    return receipt


def _directory_signature(destination, expected):
    if not os.path.lexists(destination):
        if expected:
            raise ValueError("checkpoint clip collection is missing")
        return None
    if destination.is_symlink() or not destination.is_dir():
        raise ValueError("checkpoint clip collection must be a directory, not a link")
    before = destination.stat()
    found = set()
    with os.scandir(destination) as entries:
        for entry in entries:
            if entry.name not in expected or entry.is_symlink():
                raise ValueError("checkpoint collection contains an unrecorded artifact: " + entry.name)
            found.add(entry.name)
    if found != expected:
        raise ValueError("checkpoint clip collection is missing an artifact")
    after = destination.stat()
    if _signature(before) != _signature(after):
        raise ValueError("checkpoint clip directory changed while reading")
    return _signature(after)


def collection_inventory(data, destination, options):
    """Validate every declared clip and optional still before journal reuse.

    A complete, empty selection is valid. Its directory's presence is recorded
    explicitly; no empty-directory exception is added to ordinary artifacts.
    Symlinks, undeclared files/directories and a missing final still all refuse.
    """
    from .clip_checkpoint import _clip_inventory

    destination = Path(destination)
    receipt = collection_receipt(data, destination, options)
    paired = options.get("save_last_frame") is True
    primary = receipt.primary if paired else receipt
    expected = {segment.render.destination.name for segment in primary.segments}
    before = _directory_signature(destination, expected)
    inventories, total = [], 0
    for segment in primary.segments:
        files = _clip_inventory(segment)
        total += len(files)
        if total > _MAX_FILES:
            raise ValueError("checkpoint collection exceeds its combined file budget")
        inventories.append({"play_index": segment.play_index, "files": files})
    still = None
    if paired:
        still = _inventory(receipt.still.destination)
        sample = receipt.still
        if (len(still) != 1 or still[0]["path"] != "" or still[0]["bytes"] != sample.bytes
                or still[0]["sha256"] != sample.digest):
            raise ValueError("checkpoint final PNG does not match its publication receipt")
        if total + len(still) > _MAX_FILES:
            raise ValueError("checkpoint collection exceeds its combined file budget")
    if before != _directory_signature(destination, expected):
        raise ValueError("checkpoint clip directory changed while hashing")
    return {"schema": "fmn-python.subdivided-artifacts", "version": 1,
            "directory_present": before is not None, "segments": inventories, "still": still}
