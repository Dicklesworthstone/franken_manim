"""Bounded JSON source-job documents; no source evaluation or new executor."""
from __future__ import annotations

import json
import os
from pathlib import Path

from ._process_batch import positive_integer
from .batch_checkpoint import _open_regular, _signature
from .source_jobs import SourceRenderJob, prepare_source_jobs, MAX_VARIANT_PLAN_BYTES

JOBS_FILE_FLAG = "--jobs-file"
JOBS_FILE_HELP = """Named FMTL parameter batches:
  fmn-python SOURCE.py --format fmtl --workers N --jobs-file variants.json

The document has schema fmn-python.source-jobs, version 1, and an ordered jobs
array of {name, scene, scene_kwargs} objects. scene_kwargs is optional. Each
name owns a distinct output; scene selects a local class. Plain bounded JSON
constructor values are snapshotted before isolated source discovery. No class
is imported in the coordinator. --jobs-file cannot combine with explicit scene
names, --write_all, --output or --file_name. It requires --workers, even with
one job. Existing audio/camera, keep-going and checkpoint options compose.
Resume binds the normalized jobs, not the document's spelling or file path.
"""


def take_jobs_file_options(arguments, value_flags):
    """Collect without raising before the owning CLI has selected robot mode."""
    remaining, found, index = [], [], 0
    while index < len(arguments):
        token = arguments[index]
        if token == JOBS_FILE_FLAG:
            value = arguments[index + 1] if index + 1 < len(arguments) else None
            if value is not None and value.startswith("--"):
                value = None
            found.append(value)
            index += 1 if value is None else 2
        elif token in value_flags:
            remaining.extend(arguments[index:index + 2])
            index += 2
        else:
            remaining.append(token)
            index += 1
    return remaining, found


def _unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("source job document contains a duplicate JSON key")
        result[key] = value
    return result


def _invalid_constant(value):
    raise ValueError("source job document contains nonfinite JSON: " + value)


def load_source_jobs(path, *, max_jobs=1024):
    """Read one stable regular UTF-8 file and return admitted, owned job data."""
    maximum = positive_integer(max_jobs, "max_jobs", 65_536)
    text = os.fspath(path)
    if not isinstance(text, str) or not text or "\0" in text:
        raise ValueError("jobs file must be a nonempty text path without NUL")
    path = Path(text).absolute()
    with _open_regular(path) as stream:
        before = os.fstat(stream.fileno())
        if before.st_size > MAX_VARIANT_PLAN_BYTES:
            raise ValueError("source job document exceeds its 8 MiB byte budget")
        raw = stream.read(MAX_VARIANT_PLAN_BYTES + 1)
        after = os.fstat(stream.fileno())
    if len(raw) > MAX_VARIANT_PLAN_BYTES:
        raise ValueError("source job document exceeds its 8 MiB byte budget")
    if (_signature(before) != _signature(after) or _signature(after) != _signature(path.lstat())
            or len(raw) != after.st_size):
        raise ValueError("source job document changed while reading")
    try:
        data = json.loads(raw.decode("utf-8"), object_pairs_hook=_unique_object,
                          parse_constant=_invalid_constant)
    except RecursionError as error:
        raise ValueError("source job document exceeds its nesting budget") from error
    if (not isinstance(data, dict) or set(data) != {"schema", "version", "jobs"}
            or data["schema"] != "fmn-python.source-jobs" or type(data["version"]) is not int
            or data["version"] != 1):
        raise ValueError("unsupported source job document schema/version/fields")
    rows = data["jobs"]
    if not isinstance(rows, list) or not 1 <= len(rows) <= maximum:
        raise ValueError(f"source job document requires 1..{maximum} jobs")
    entries = []
    for row in rows:
        if (not isinstance(row, dict) or not {"name", "scene"} <= set(row)
                or set(row) - {"name", "scene", "scene_kwargs"}):
            raise ValueError("source job document requires name, scene and optional scene_kwargs")
        entries.append(SourceRenderJob(row["name"], row["scene"], row.get("scene_kwargs")))
    names, variants = prepare_source_jobs(entries, maximum)
    return tuple(SourceRenderJob(name, variant["scene"], variant["scene_kwargs"])
                 for name, variant in zip(names, variants))
