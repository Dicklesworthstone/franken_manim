"""Certified worker receipts over the existing guarded native session.

The native publisher owns FMNP and the input closure. This module binds each
worker to the parent's measured runtime, restores only RenderResult data, and
checks that the exact published sidecar crossed the return boundary intact.
It neither parses a second manifest format nor certifies a batch aggregate.
"""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path

from .batch_checkpoint import _open_regular, _signature
from .batch_provenance import _available, _sidecar, has_provenance, validate_batch_mode
from .batch_rendering import SceneRenderOutcome, _restored_outcome
from .effect_audit import paused
from .runtime_identity import RuntimeIdentityError, capture_runtime

_SCHEMA = "fmn-python.parallel-provenance"
_MAX_MANIFEST_BYTES = 64 * 1024 * 1024


def _digest(value):
    return (type(value) is str and len(value) == 64
            and all(char in "0123456789abcdef" for char in value))


def _manifest_fingerprint(path):
    """Bounded sidecar transport check; FMNP semantics remain native-owned."""
    if path.parent.is_symlink():
        raise ValueError("worker manifest generation cannot be a symlink")
    with paused(), _open_regular(path) as stream:
        before = os.fstat(stream.fileno())
        if not 0 < before.st_size <= _MAX_MANIFEST_BYTES:
            raise ValueError("worker manifest exceeds its 64 MiB byte budget or is empty")
        digest, size = hashlib.sha256(), 0
        while chunk := stream.read(min(1024 * 1024, _MAX_MANIFEST_BYTES - size + 1)):
            size += len(chunk)
            if size > _MAX_MANIFEST_BYTES:
                raise ValueError("worker manifest grew past its byte budget")
            digest.update(chunk)
        after = os.fstat(stream.fileno())
        current = path.lstat()
    if (_signature(before) != _signature(after) or _signature(after) != _signature(current)
            or size != after.st_size):
        raise RuntimeError("worker manifest changed while hashing")
    return {"bytes": size, "sha256": digest.hexdigest()}


def freeze_invocation(job):
    """Snapshot the admitted invocation BEFORE imports or constructors run.

    Keep legacy class-name inputs byte-identical. Version 2 also identifies
    the selected class and typed constructor values of an explicit variant.
    Native source closure publication still owns hashing these input bytes.
    """
    options = job["options"]
    invocation = {"schema": "fmn-python.source-invocation", "version": 1,
                  "scene": job["name"], "source_sha256": job["source_digest"],
                  "options": {key: options.get(key) for key in
                              ("format", "resolution", "fps", "animation_range", "_output_options")}}
    if "scene_name" in job:
        from .source_jobs import _kwargs

        invocation.update(version=2, scene=job["scene_name"], output_name=job["name"],
                          scene_kwargs=_kwargs(job["scene_kwargs"], [0, 0]))
    raw = json.dumps(invocation, sort_keys=True, separators=(",", ":"),
                     ensure_ascii=True, allow_nan=False).encode("ascii")
    if "invocation_sha256" in job:
        expected = job["invocation_sha256"]
        if not _digest(expected) or hashlib.sha256(raw).hexdigest() != expected:
            raise ValueError("certified worker invocation differs from the admitted job")
    return raw


def executed_sources(loaded, job, *, invocation=None):
    """Append frozen invocation inputs without losing late executed imports.

    Worker code passes immutable pre-execution bytes. Never reserialize live
    constructor dictionaries here: authored code may have mutated their nested
    lists while running. Scheduling knobs do not enter the semantic inputs.
    """
    from .rendering import _source_snapshot

    sources = _source_snapshot(loaded.sources)
    name = "__fmn_parallel__/invocation.json"
    if name in sources:
        raise ValueError("scene sources collide with the parallel invocation identity")
    sources[name] = freeze_invocation(job) if invocation is None else invocation
    return _source_snapshot(sources)


def worker_runtime(native, expected):
    """Refuse a different installed runtime BEFORE importing scene sources."""
    with paused():
        snapshot = capture_runtime(native)
    if snapshot.identities != expected:
        raise RuntimeIdentityError("parallel worker runtime disagrees with the planned runtime")
    return snapshot


def worker_proof(snapshot, source_digest, receipt, *, invocation=None):
    """Attest transport only after the guarded session returned its receipt."""
    if (not has_provenance(receipt)
            or receipt.manifest != _sidecar(receipt.destination) / "manifest.fmnp"):
        raise RuntimeError("certified worker did not return complete native provenance")
    with paused():
        snapshot.verify()
    proof = {"schema": _SCHEMA, "version": 1, "runtime": snapshot.identities,
             "source_digest": source_digest,
             "manifest": _manifest_fingerprint(receipt.manifest)}
    if invocation is not None:
        if type(invocation) is not bytes:
            raise TypeError("worker invocation proof requires immutable admitted bytes")
        proof.update(version=2, invocation_sha256=hashlib.sha256(invocation).hexdigest())
    return proof


def _restore_receipt(name, destination, format, row):
    """Decode the public RenderResult.as_dict form, never executable objects."""
    if (row.get("name") != name or row.get("destination") != str(destination)
            or row.get("status") != "succeeded" or row.get("error") is not None):
        raise ValueError("certified worker outcome does not identify its planned scene")
    data = row.get("result")
    if (type(data) is not dict or data.get("destination") != str(destination)
            or data.get("format") != format or data.get("certified") is not True
            or not _digest(data.get("closure_digest"))
            or not _digest(data.get("artifact_digest"))
            or data["artifact_digest"] != data.get("digest")):
        raise ValueError("certified worker omitted or changed its native publication receipt")
    manifest = data.get("manifest")
    path = _sidecar(destination) / "manifest.fmnp"
    if (type(manifest) is not dict or set(manifest) != {"path", "closure_digest"}
            or manifest["path"] != str(path)
            or manifest["closure_digest"] != data["closure_digest"]):
        raise ValueError("certified worker manifest does not match its planned output")
    # The public JSON form nests manifest metadata and repeats artifact_digest;
    # RenderResult's constructor takes a Path and the already-checked digest.
    values = dict(data)
    values.pop("artifact_digest")
    values["manifest"] = path
    receipt = _restored_outcome({"name": name, "result": values}).result
    if receipt.bytes <= 0 or not 0 <= receipt.seed < 1 << 64 or receipt.ffmpeg_invocations:
        raise ValueError("invalid certified worker publication size, seed or external-tool receipt")
    if format == "wav":
        if (receipt.frame_count is not None or receipt.sample_frames is None
                or type(data.get("sample_rate")) is not int or data["sample_rate"] != 48000
                or type(data.get("channels")) is not int or data["channels"] != 2):
            raise ValueError("invalid certified worker WAV publication")
    elif (receipt.sample_frames is not None or receipt.frame_count is None
          or receipt.frame_count <= 0 or (format == "png" and receipt.frame_count != 1)):
        raise ValueError("invalid certified worker frame publication")
    return receipt


class ParallelProvenance:
    """One parent runtime generation, with independently certified scene outputs."""
    def __init__(self, native, format, destinations, source_digest, *, invocation_digests=None):
        if invocation_digests is not None:
            if (type(invocation_digests) is not dict or not invocation_digests
                    or any(type(name) is not str or not name or not _digest(digest)
                           for name, digest in invocation_digests.items())):
                raise ValueError("certified batch requires valid admitted invocation digests")
            invocation_digests = dict(invocation_digests)
        self._invocation_digests = invocation_digests
        validate_batch_mode(native, format, True)
        for destination in destinations:
            _available(destination)
        with paused():
            self.snapshot = capture_runtime(native)
        self.format = format
        self.source_digest = source_digest

    @property
    def identities(self):
        return self.snapshot.identities

    def verify(self):
        with paused():
            self.snapshot.verify()

    def restore(self, name, destination, row):
        proof = row.get("provenance")
        admitted = getattr(self, "_invocation_digests", None)
        fields = {"schema", "version", "runtime", "source_digest", "manifest"}
        if admitted is not None:
            fields.add("invocation_sha256")
            if (name not in admitted or type(proof) is not dict
                    or proof.get("invocation_sha256") != admitted[name]):
                raise ValueError("certified worker invocation proof disagrees with its admitted job")
        if (type(proof) is not dict
                or set(proof) != fields
                or proof["schema"] != _SCHEMA or type(proof["version"]) is not int
                or proof["version"] != (2 if admitted is not None else 1)
                or proof["runtime"] != self.identities
                or proof["source_digest"] != self.source_digest):
            raise ValueError("certified worker provenance disagrees with the planned inputs")
        receipt = _restore_receipt(name, destination, self.format, row)
        fingerprint = proof["manifest"]
        if (type(fingerprint) is not dict or set(fingerprint) != {"bytes", "sha256"}
                or type(fingerprint["bytes"]) is not int or not _digest(fingerprint["sha256"])
                or fingerprint != _manifest_fingerprint(receipt.manifest)):
            raise ValueError("certified worker manifest changed before receipt acceptance")
        self.verify()
        return SceneRenderOutcome(name, destination, "succeeded", result=receipt)
