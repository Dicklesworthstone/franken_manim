"""Crash-recoverable receipts for sequential Python batches, not a render cache.

Only completed native publications may be reused. The caller's resume_key
identifies authored scene/asset inputs; hashes below verify output integrity,
not certified determinism or the current meaning of Python source code.
"""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import stat
import tempfile
from typing import Any

_SCHEMA = "fmn-python.batch-checkpoint"
_VERSION = 2
_MAX_BYTES = 32 * 1024 * 1024
_MAX_FILES = 200000


def _json(value: Any) -> bytes:
    return json.dumps(value, ensure_ascii=True, allow_nan=False,
                      sort_keys=True, separators=(",", ":")).encode("utf-8")


def _scene_identity(scene):
    cls = scene if isinstance(scene, type) else type(scene)
    identity = {"module": cls.__module__, "qualname": cls.__qualname__}
    if any(not isinstance(value, str) or not value for value in identity.values()):
        raise ValueError("checkpoint scene identity requires a module and qualified class name")
    # Separate fields avoid collisions between module a.b / class C and
    # module a / nested class b.C. Source/asset versions remain resume_key's job.
    return identity


def _signature(info):
    return (info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns, info.st_ctime_ns)


def _open_regular(path: Path):
    """Refuse special files and links before opening, without a FIFO race hang."""
    before = path.lstat()
    if not stat.S_ISREG(before.st_mode):
        raise ValueError(f"checkpoint input must be a regular file: {path}")
    flags = (os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
             | getattr(os, "O_NONBLOCK", 0) | getattr(os, "O_BINARY", 0))
    descriptor = os.open(path, flags)
    try:
        opened = os.fstat(descriptor)
        if not stat.S_ISREG(opened.st_mode) or _signature(opened) != _signature(before):
            raise ValueError(f"checkpoint input changed while opening: {path}")
        return os.fdopen(descriptor, "rb")
    except BaseException:
        os.close(descriptor)
        raise


def _regular_file(path: Path) -> dict[str, Any]:
    """Hash without admitting links, devices, or files changed during the read."""
    with _open_regular(path) as stream:
        before = os.fstat(stream.fileno())
        digest, size = hashlib.sha256(), 0
        while chunk := stream.read(1024 * 1024):
            digest.update(chunk)
            size += len(chunk)
        after = os.fstat(stream.fileno())
    current = path.lstat()
    if _signature(before) != _signature(after) or _signature(after) != _signature(current) or size != after.st_size:
        raise ValueError(f"checkpoint artifact changed while hashing: {path}")
    return {"bytes": size, "sha256": digest.hexdigest()}


def _read_document(path: Path):
    with _open_regular(path) as stream:
        before = os.fstat(stream.fileno())
        if before.st_size > _MAX_BYTES:
            raise ValueError("checkpoint exceeds its byte budget")
        raw = stream.read(_MAX_BYTES + 1)
        after = os.fstat(stream.fileno())
    if len(raw) > _MAX_BYTES:
        raise ValueError("checkpoint exceeds its byte budget")
    if _signature(before) != _signature(after) or _signature(after) != _signature(path.lstat()) or len(raw) != after.st_size:
        raise ValueError("checkpoint changed while reading")
    return json.loads(raw)


def _scan_error(error):
    # os.walk otherwise silently skips unreadable directories. An incomplete
    # inventory cannot be evidence that a completed render remains intact.
    raise error


def _inventory(destination: Path) -> list[dict[str, Any]]:
    if destination.is_symlink():
        raise ValueError(f"checkpoint artifact cannot be a symlink: {destination}")
    if not destination.is_dir():
        return [{"path": "", **_regular_file(destination)}]
    files = []
    for directory, dirs, names in os.walk(destination, followlinks=False, onerror=_scan_error):
        for name in dirs:
            if (Path(directory) / name).is_symlink():
                raise ValueError("checkpoint sequences cannot contain symlinks")
        for name in names:
            path = Path(directory) / name
            files.append({"path": path.relative_to(destination).as_posix(), **_regular_file(path)})
            if len(files) > _MAX_FILES:
                raise ValueError("checkpoint artifact inventory exceeds its file budget")
    if not files:
        raise ValueError(f"checkpoint artifact directory is empty: {destination}")
    return sorted(files, key=lambda item: item["path"])


class BatchCheckpoint:
    """An exclusive, process-scoped journal owner. Lock files intentionally persist.

    Kernel locks release on process exit, including crashes, so resume does not
    need stale-lock deletion. Artifact publication remains exclusively native.
    """

    def __init__(self, path: os.PathLike[str] | str, *, resume: bool, key: str):
        text = os.fspath(path)
        if not isinstance(text, str) or not text or "\0" in text:
            raise ValueError("checkpoint must be a nonempty text path without NUL")
        if not isinstance(key, str) or not key or len(key.encode("utf-8")) > 4096:
            raise ValueError("checkpoint requires a nonempty resume_key of at most 4096 UTF-8 bytes")
        # Freeze cwd without resolving the leaf: a symlink must be refused,
        # not turned into permission to overwrite its target.
        original = Path(text).absolute()
        self.path = original.parent.resolve() / original.name
        self.lock_path = self.path.with_name(self.path.name + ".lock")
        self.resume, self.key = resume, key
        self.document = None
        self.lock = None
        self.plan = None
        self.artifacts: dict[str, list[dict[str, Any]]] = {}
        self.completed: dict[str, dict[str, Any]] = {}

    def __enter__(self):
        self.path.parent.mkdir(parents=True, exist_ok=True)
        flags = os.O_RDWR | os.O_CREAT | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_BINARY", 0)
        if self.lock_path.is_symlink():
            raise ValueError("checkpoint lock cannot be a symlink")
        descriptor = os.open(self.lock_path, flags, 0o600)
        try:
            if not stat.S_ISREG(os.fstat(descriptor).st_mode):
                raise ValueError("checkpoint lock must be a regular file")
            self.lock = os.fdopen(descriptor, "r+b")
        except BaseException:
            os.close(descriptor)
            raise
        try:
            if os.name == "nt":
                import msvcrt
                if os.fstat(self.lock.fileno()).st_size == 0:
                    self.lock.write(b"\0")
                    self.lock.flush()
                self.lock.seek(0)
                msvcrt.locking(self.lock.fileno(), msvcrt.LK_NBLCK, 1)
            else:
                import fcntl
                fcntl.flock(self.lock.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
            if self.path.is_symlink():
                raise ValueError("checkpoint cannot be a symlink")
            if self.resume:
                document = _read_document(self.path)
                if (not isinstance(document, dict) or document.get("schema") != _SCHEMA
                        or type(document.get("version")) is not int or document["version"] != _VERSION):
                    raise ValueError("unsupported batch checkpoint schema/version")
                if document.get("key") != self.key:
                    raise ValueError("checkpoint resume_key does not match")
                self.document = document
            elif os.path.lexists(self.path):
                raise FileExistsError(f"checkpoint already exists; use resume=True: {self.path}")
            return self
        except BaseException:
            self.__exit__(None, None, None)
            raise

    def __exit__(self, *_):
        if self.lock is not None:
            # Closing releases both flock and Windows byte-range locks.
            self.lock.close()
            self.lock = None

    @property
    def existing_destinations(self) -> set[Path]:
        if self.document is None:
            return set()
        try:
            return {Path(row["destination"]) for row in self.document["outcomes"]
                    if row["status"] == "succeeded"}
        except (KeyError, TypeError) as error:
            raise ValueError("invalid checkpoint outcomes") from error

    def validate_destinations(self, destinations):
        for destination in destinations:
            for own_path in (self.path, self.lock_path):
                if (own_path == destination or destination in own_path.parents
                        or own_path in destination.parents):
                    raise ValueError("checkpoint and its lock must be outside published artifacts")

    def prepare(self, jobs, destinations, options):
        # resume_key is an explicit caller assertion about source/assets. Keep
        # constructor parameters JSON-only when checkpointing so changes are
        # rejected rather than silently reusing another invocation's outputs.
        plan = {"jobs": [{"name": job.name, "destination": str(path),
                          "scene": _scene_identity(job.scene),
                          "scene_kwargs": dict(job.scene_kwargs or {})}
                         for job, path in zip(jobs, destinations)], "options": options}
        self.plan = json.loads(_json(plan))
        if self.document is None:
            return
        # Python container equality conflates True, 1 and 1.0 (and +/-0.0).
        # Their constructor behavior can differ, even with an unchanged key.
        # Canonical JSON retains those distinctions but ignores mapping order.
        if _json(self.document.get("plan")) != _json(self.plan):
            raise ValueError("checkpoint scene plan or render options do not match")
        rows = self.document.get("outcomes")
        if not isinstance(rows, list) or len(rows) != len(jobs):
            raise ValueError("checkpoint outcomes do not match the scene plan")
        artifacts = self.document.get("artifacts")
        if not isinstance(artifacts, dict):
            raise ValueError("invalid checkpoint artifact inventory")
        for job, destination, row in zip(jobs, destinations, rows):
            if not isinstance(row, dict) or row.get("name") != job.name or row.get("destination") != str(destination):
                raise ValueError("checkpoint outcome identity does not match the scene plan")
            status = row.get("status")
            if status not in {"succeeded", "failed", "cancelled", "not_run"}:
                raise ValueError("invalid checkpoint outcome status")
            if status == "succeeded":
                receipt = row.get("result")
                if not isinstance(receipt, dict) or receipt.get("destination") != str(destination) or receipt.get("format") != options["format"] or receipt.get("certified") is not False:
                    raise ValueError("invalid checkpoint publication receipt")
                actual = _inventory(destination)
                if artifacts.get(job.name) != actual:
                    raise ValueError(f"checkpoint artifact is missing or modified: {destination}")
                if options["format"] != "png_sequence" and (len(actual) != 1 or actual[0]["bytes"] != receipt.get("bytes") or actual[0]["sha256"] != receipt.get("digest")):
                    raise ValueError("checkpoint artifact does not match its native receipt")
                self.artifacts[job.name] = actual
                self.completed[job.name] = row

    def record(self, outcomes):
        if self.lock is None or self.plan is None:
            raise RuntimeError("checkpoint must be opened and prepared before recording")
        rows = [outcome.as_dict() for outcome in outcomes]
        for outcome in outcomes:
            if outcome.status == "succeeded" and outcome.name not in self.artifacts:
                files = _inventory(outcome.destination)
                receipt = outcome.result
                if receipt.format != "png_sequence" and (len(files) != 1 or files[0]["bytes"] != receipt.bytes or files[0]["sha256"] != receipt.digest):
                    raise ValueError("published artifact does not match its native receipt")
                self.artifacts[outcome.name] = files
        payload = _json({"schema": _SCHEMA, "version": _VERSION, "key": self.key,
                         "plan": self.plan, "outcomes": rows, "artifacts": self.artifacts})
        if len(payload) > _MAX_BYTES:
            raise ValueError("checkpoint exceeds its byte budget")
        descriptor, temporary = tempfile.mkstemp(prefix=self.path.name + ".", suffix=".tmp", dir=self.path.parent)
        try:
            with os.fdopen(descriptor, "wb") as stream:
                stream.write(payload)
                stream.flush()
                os.fsync(stream.fileno())
            os.replace(temporary, self.path)
            if os.name != "nt":
                descriptor = os.open(self.path.parent, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
                try:
                    os.fsync(descriptor)
                finally:
                    os.close(descriptor)
        finally:
            if os.path.exists(temporary):
                os.unlink(temporary)
