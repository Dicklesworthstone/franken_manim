"""Bounded source discovery in a disposable host-CPython worker.

The coordinator receives names and an entry-source digest, never imported
classes, code objects or native proxies. SceneSource remains the one loader.
This protects the coordinator from crashes/hangs and interpreter mutations;
Python source still has the host's filesystem and process authority.
"""
from __future__ import annotations

from contextlib import contextmanager, redirect_stdout
from dataclasses import dataclass
import hashlib
import importlib
import os
from pathlib import Path
import sys

from ._process_batch import positive_integer, run_process_jobs, validate_job_timeout
from .batch_checkpoint import _open_regular
from .cancellation import _check_cancelled, _validate_cancellation

_MAX_SOURCE_BYTES = 16 * 1024 * 1024
_SCHEMA = "fmn-python.scene-discovery"


@dataclass(frozen=True)
class DiscoveredScenes:
    source: Path
    source_digest: str
    scenes: tuple[str, ...]


class SceneDiscoveryError(RuntimeError):
    """An import failed in its worker, before any render job was admitted."""
    def __init__(self, row):
        error = row.get("error") or {}
        self.worker_error_type = error.get("type", "RuntimeError")
        self.worker_message = error.get("message", "discovery worker failed")
        super().__init__(f"scene discovery failed: {self.worker_error_type}: {self.worker_message}")


def _entry_digest(source):
    # A .py-named FIFO must not hang the coordinator before its timeout starts.
    # Reuse the regular-file opener (no links/devices, nonblocking open, checked
    # file identity); paths are already canonicalized at the host boundary.
    with _open_regular(source) as stream:
        if os.fstat(stream.fileno()).st_size > _MAX_SOURCE_BYTES:
            raise ValueError("scene entry source exceeds its 16 MiB budget")
        data = stream.read(_MAX_SOURCE_BYTES + 1)
    if len(data) > _MAX_SOURCE_BYTES:
        raise ValueError("scene entry source exceeds its 16 MiB budget")
    return hashlib.sha256(data).hexdigest()


def _discover_source_job(job):
    # Include native initialization in the worker's timeout and log routing.
    with redirect_stdout(sys.stderr):
        os.chdir(job["working_directory"])
        native = importlib.import_module("manimlib")
        from .scene_loading import SceneSource

        source = Path(job["source"])
        with SceneSource(source, native.Scene, entry_digest=job["source_digest"],
                         configuration=job.get("configuration")) as loaded:
            if len(loaded.scenes) > job["max_scenes"]:
                raise ValueError("scene discovery exceeds its scene-count budget")
            names = tuple(loaded.scenes)
            if any(type(name) is not str or len(name.encode("utf-8")) > 4096 for name in names):
                raise ValueError("discovered scene names must be bounded strings")
            # Selection validates output identifiers later. An unselected alias
            # need not be a legal filename; preserve the loader's actual names.
            return {"status": "succeeded", "error": None, "result": {
                "schema": _SCHEMA, "version": 1, "source": str(source),
                "source_digest": job["source_digest"], "scenes": sorted(names),
            }}


def discover_source_scenes(source, *, working_directory=None, job_timeout=None,
                           max_scenes=65536, configuration=None, cancellation=None):
    """Import in a fresh worker and return an immutable, data-only inventory.

    job_timeout includes interpreter/native startup and source import. A child
    interrupt propagates as KeyboardInterrupt; other failures are reported as
    SceneDiscoveryError without reconstructing worker-supplied exceptions.
    cancellation accepts the same parent-owned CancellationToken as rendering;
    interrupted discovery is reaped before BatchCancelled reaches the caller.
    A pre-cancelled request does not read source or start a worker. The same
    one-way token may cover both discovery and subsequent render admission.
    Call from a spawn-safe main guard, just like render_source_scenes.
    """
    _validate_cancellation(cancellation)
    _check_cancelled(cancellation)
    limit = positive_integer(max_scenes, "max_scenes", 65536)
    timeout = validate_job_timeout(job_timeout)
    text = os.fspath(source)
    if not isinstance(text, str) or not text or "\0" in text:
        raise ValueError("scene source must be a nonempty text path without NUL")
    path = Path(text).resolve()
    if path.suffix.lower() not in {".py", ".pyw"}:
        raise ValueError("the Python portal accepts only .py or .pyw scene sources")
    cwd = Path.cwd() if working_directory is None else Path(working_directory).resolve()
    if not cwd.is_dir():
        raise NotADirectoryError(str(cwd))
    digest = _entry_digest(path)
    job = {"source": str(path), "source_digest": digest,
           "working_directory": str(cwd), "max_scenes": limit}
    if configuration is not None:
        import json
        job["configuration"] = json.loads(json.dumps(configuration, allow_nan=False))
    rows = run_process_jobs([job], _discover_source_job, workers=1, job_timeout=timeout,
                            **({"cancellation": cancellation} if cancellation is not None else {}))
    row = rows[0]
    if row["status"] == "cancelled":
        raise KeyboardInterrupt("scene discovery worker interrupted")
    if row["status"] != "succeeded":
        raise SceneDiscoveryError(row)
    result = row.get("result")
    if (not isinstance(result, dict) or result.get("schema") != _SCHEMA
            or type(result.get("version")) is not int or result["version"] != 1
            or result.get("source") != str(path) or result.get("source_digest") != digest):
        raise ValueError("discovery receipt does not identify its planned source")
    names = result.get("scenes")
    if (not isinstance(names, list) or len(names) > limit
            or any(type(name) is not str or len(name.encode("utf-8")) > 4096 for name in names)
            or len(set(names)) != len(names) or names != sorted(names)):
        raise ValueError("invalid discovery scene inventory")
    return DiscoveredScenes(path, digest, tuple(names))


@contextmanager
def discovery_source(source, **options):
    """Console loading context; all workers are reaped before yielding data."""
    yield discover_source_scenes(source, **options)
