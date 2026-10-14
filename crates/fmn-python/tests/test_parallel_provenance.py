"""Worker transport/input-binding tests, not native certification witnesses.

These use real receipt types, file hashing and JSON transport with explicit
runtime doubles. Native rendering is tested in test_parallel_certified_native.
"""
from __future__ import annotations

import copy
from dataclasses import replace
import hashlib
import json
import os
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "python"))
from fmn_python import parallel_provenance as provenance
from fmn_python.batch_provenance import validate_batch_mode
from fmn_python.rendering import RenderResult
from fmn_python.runtime_identity import RuntimeIdentityError


class Snapshot:
    def __init__(self):
        self.identities = {key: key + " fixture" for key in ("cpython", "abi", "wheel", "numpy")}
        self.verified = 0
        self.changed = False

    def verify(self):
        self.verified += 1
        if self.changed:
            raise RuntimeIdentityError("runtime fixture changed")


class ProvenanceTransportTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.destination = self.root / "First.png"
        self.path = self.root / "First.png.manifest" / "manifest.fmnp"
        self.path.parent.mkdir()
        self.path.write_bytes(b"FMNP explicit byte fixture, not a native certificate")
        self.snapshot = Snapshot()
        self.source_digest = hashlib.sha256(b"source").hexdigest()
        self.receipt = RenderResult(self.destination, "png", (64, 48), 4, 1,
            "explicit-transport-fixture", 120, "d" * 64, 1, None, 3,
            certified=True, manifest=self.path, closure_digest="c" * 64)
        self.proof = provenance.worker_proof(self.snapshot, self.source_digest, self.receipt)
        self.row = {"name": "First", "destination": str(self.destination), "status": "succeeded",
                    "result": self.receipt.as_dict(), "error": None, "provenance": self.proof}
        self.owner = provenance.ParallelProvenance.__new__(provenance.ParallelProvenance)
        self.owner.snapshot = self.snapshot
        self.owner.source_digest = self.source_digest
        self.owner.format = "png"

    def restore(self, row=None):
        return self.owner.restore("First", self.destination, self.row if row is None else row)

    def test_public_receipt_round_trip_preserves_typed_manifest_and_digests(self):
        restored = self.restore(json.loads(json.dumps(self.row)))
        self.assertEqual(restored.result, self.receipt)
        self.assertEqual(restored.result.as_dict(), self.row["result"])
        self.assertIsInstance(restored.result.manifest, Path)
        self.assertGreaterEqual(self.snapshot.verified, 2)

    def test_wrong_scene_output_format_or_claim_is_rejected(self):
        for key, value in (("certified", False), ("certified", 1), ("format", "gif"),
                           ("destination", str(self.root / "foreign.png")),
                           ("closure_digest", "not a digest"), ("artifact_digest", "e" * 64),
                           ("frame_count", True), ("frame_count", 2), ("sample_frames", 10),
                           ("bytes", 0), ("seed", 1 << 64),
                           ("ffmpeg_invocations", [{"process": "unrecorded"}])):
            row = copy.deepcopy(self.row)
            row["result"][key] = value
            with self.subTest(key=key), self.assertRaises(ValueError):
                self.restore(row)
        for key, value in (("name", "Second"), ("status", "failed"),
                           ("error", {"type": "ValueError"}), ("destination", "relative.png")):
            row = copy.deepcopy(self.row)
            row[key] = value
            with self.subTest(key=key), self.assertRaises(ValueError):
                self.restore(row)

    def test_missing_or_conflicting_manifest_fields_are_rejected(self):
        for value in (None, str(self.path), {"path": str(self.path)},
                      {"path": str(self.path), "closure_digest": "e" * 64},
                      {"path": str(self.root / "other.fmnp"), "closure_digest": "c" * 64}):
            row = copy.deepcopy(self.row)
            row["result"]["manifest"] = value
            with self.subTest(value=value), self.assertRaises(ValueError):
                self.restore(row)
        for key in ("closure_digest", "artifact_digest", "manifest"):
            row = copy.deepcopy(self.row)
            row["result"].pop(key)
            with self.subTest(key=key), self.assertRaises(ValueError):
                self.restore(row)

    def test_parent_rejects_other_runtime_source_and_missing_transport_proof(self):
        for key, value in (("runtime", {}), ("source_digest", "f" * 64),
                           ("version", True), ("schema", "another schema")):
            row = copy.deepcopy(self.row)
            row["provenance"][key] = value
            with self.subTest(key=key), self.assertRaises(ValueError):
                self.restore(row)
        row = copy.deepcopy(self.row)
        row.pop("provenance")
        with self.assertRaises(ValueError):
            self.restore(row)

    def test_changed_missing_or_linked_sidecar_cannot_be_accepted(self):
        self.path.write_bytes(b"changed transport bytes")
        with self.assertRaisesRegex(ValueError, "manifest changed"):
            self.restore()
        moved = self.path.with_name("preserved.fmnp")
        self.path.rename(moved)
        with self.assertRaises(FileNotFoundError):
            self.restore()
        self.path.symlink_to(moved)
        with self.assertRaisesRegex(ValueError, "regular file"):
            self.restore()

    def test_manifest_reads_are_bounded_and_empty_or_fifo_inputs_refuse(self):
        with patch.object(provenance, "_MAX_MANIFEST_BYTES", 4):
            with self.assertRaises(ValueError):
                provenance._manifest_fingerprint(self.path)
        self.path.write_bytes(b"")
        with self.assertRaises(ValueError):
            provenance._manifest_fingerprint(self.path)
        if hasattr(os, "mkfifo"):
            fifo = self.root / "fifo.fmnp"
            os.mkfifo(fifo)
            with self.assertRaisesRegex(ValueError, "regular file"):
                provenance._manifest_fingerprint(fifo)

    def test_runtime_edit_after_worker_proof_prevents_acceptance(self):
        self.snapshot.changed = True
        with self.assertRaises(RuntimeIdentityError):
            self.restore()

    def test_runtime_is_compared_as_measured_bytes_not_version_label_fallback(self):
        with patch.object(provenance, "capture_runtime", return_value=self.snapshot):
            self.assertIs(provenance.worker_runtime(object(), self.snapshot.identities), self.snapshot)
            changed = dict(self.snapshot.identities, wheel="franken-manim 0.5.0")
            with self.assertRaises(RuntimeIdentityError):
                provenance.worker_runtime(object(), changed)

    def test_worker_proof_requires_a_completed_native_receipt(self):
        for receipt in (replace(self.receipt, certified=False),
                        replace(self.receipt, manifest=None),
                        replace(self.receipt, closure_digest=None)):
            with self.assertRaises(RuntimeError):
                provenance.worker_proof(self.snapshot, self.source_digest, receipt)

    def test_unsupported_modes_refuse_before_publication(self):
        native = SimpleNamespace(_portal_publish_manifest=lambda: None)
        for format in ("gif", "y4m", "mp4", "mov", "svg"):
            with self.subTest(format=format), self.assertRaisesRegex(RuntimeError, "require png"):
                validate_batch_mode(native, format, True)
        with self.assertRaisesRegex(RuntimeError, "checkpoint"):
            validate_batch_mode(native, "png", True, "progress.json")

    def test_selected_invocation_is_an_additional_input_and_lazy_sources_are_retained(self):
        loaded = SimpleNamespace(sources={"scenes.py": b"two scenes", "helper.py": b"helper"})
        job = {"name": "First", "source_digest": self.source_digest,
               "options": {"format": "png", "resolution": [64, 48], "fps": 4,
                           "animation_range": None, "_output_options": {"transparent": True},
                           "threads": 1}}
        source = provenance.executed_sources(loaded, job)
        self.assertEqual(source["scenes.py"], b"two scenes")
        self.assertEqual(source["helper.py"], b"helper")
        key = "__fmn_parallel__/invocation.json"
        data = json.loads(source[key])
        self.assertEqual(data["scene"], "First")
        self.assertTrue(data["options"]["_output_options"]["transparent"])
        job["options"]["threads"] = 16
        self.assertEqual(provenance.executed_sources(loaded, job), source)
        job["name"] = "Second"
        self.assertNotEqual(provenance.executed_sources(loaded, job)[key], source[key])
        self.assertNotIn(key, loaded.sources, "do not mutate the loader's source inventory")
        job["name"] = "First"
        job["options"]["animation_range"] = [1, 2]
        self.assertNotEqual(provenance.executed_sources(loaded, job)[key], source[key])
        loaded.sources[key] = b"authored input"
        with self.assertRaisesRegex(ValueError, "collide"):
            provenance.executed_sources(loaded, job)


if __name__ == "__main__":
    unittest.main()
