"""Immutable certification input/receipt transport, not native certificates.

Run production invocation freezing, worker dispatch, source composition,
provenance validation, spawn and sidecar hashing. Explicit runtime/scene/render
fixtures stand in for the native wheel. NOT-FMNP/NOT-IMAGE files do not witness
native determinism, effect enforcement or actual manifest semantics.
"""
from contextlib import contextmanager, nullcontext
from copy import deepcopy
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import sys
import tempfile
import types
import unittest
from unittest.mock import Mock, patch

from test_parallel_bundle_protocol import host_fixture, Outcome, Result, load_module
from test_source_job_variants_protocol import RenderJobFixture

KEY = "__fmn_parallel__/invocation.json"


class RuntimeChanged(RuntimeError):
    pass


class Snapshot:
    identities = {"wheel": "explicit-runtime-fixture", "cpython": "fixture", "numpy": "fixture", "abi": "fixture"}

    def verify(self):
        pass


@dataclass(frozen=True)
class Receipt:
    data: dict

    def __getattr__(self, key):
        return self.data[key]

    def as_dict(self):
        result = deepcopy(self.data)
        result["destination"] = str(result["destination"])
        result["manifest"] = {"path": str(self.data["manifest"]), "closure_digest": self.closure_digest}
        result["artifact_digest"] = self.digest
        return result


def restore_receipt(row):
    values = dict(row["result"])
    values["destination"] = Path(values["destination"])
    return Outcome(row["name"], values["destination"], "succeeded", Receipt(values))


def source_snapshot(values):
    if not values or any(type(value) is not bytes for value in values.values()):
        raise ValueError("fixture source boundary expects immutable compilation bytes")
    return dict(values)


@contextmanager
def certified_host():
    with host_fixture() as h:
        h.native._portal_publish_manifest = lambda *a: None
        h.batch.RenderJob = RenderJobFixture
        h.batch._restored_outcome = restore_receipt
        sys.modules["fmn_python.rendering"]._source_snapshot = source_snapshot
        effects = types.ModuleType("fmn_python.effect_audit")
        effects.paused = nullcontext
        effects.recording = lambda enabled: nullcontext()
        runtime = types.ModuleType("fmn_python.runtime_identity")
        runtime.RuntimeIdentityError = RuntimeChanged
        runtime.capture_runtime = lambda native: Snapshot()
        names = ("fmn_python.parallel_provenance", "fmn_python.batch_provenance")
        absent = object()
        saved = {name: sys.modules.get(name, absent) for name in names}
        try:
            with patch.dict(sys.modules, {effects.__name__: effects, runtime.__name__: runtime}):
                h.policy = load_module(names[1], "batch_provenance.py")
                h.provenance = load_module(names[0], "parallel_provenance.py")
                yield h
        finally:
            for name, value in saved.items():
                if value is absent:
                    sys.modules.pop(name, None)
                else:
                    sys.modules[name] = value


def invocation_job(**changes):
    job = {"name": "First", "source_digest": "a" * 64,
           "options": {"format": "png", "resolution": [64, 48], "fps": 24,
                       "threads": 1, "animation_range": None, "_output_options": None}}
    job.update(changes)
    return job


def execute_bound_job(job):
    with certified_host() as h:
        root = Path(job["source"]).parent
        behavior_path = root / "behavior.json"
        behavior = json.loads(behavior_path.read_text()) if behavior_path.exists() else {}
        behavior = behavior.get(job["name"], "")
        if behavior == "changed-before-import":
            job["options"]["fps"] += 1
        loader = types.ModuleType("fmn_python.scene_loading")

        @contextmanager
        def load(path, base, **options):
            path = Path(path)
            data = path.read_bytes()
            with (root / (job["name"] + ".imports")).open("a") as stream:
                stream.write(str(os.getpid()) + "\n")
            namespace = {"__name__": "certified_invocation_fixture", "__file__": str(path)}
            exec(compile(data, str(path), "exec"), namespace)
            scenes = {name: cls for name, cls in namespace.items()
                      if isinstance(cls, type) and issubclass(cls, base)
                      and cls.__module__ == namespace["__name__"]}
            loaded = types.SimpleNamespace(scenes=scenes, _source_bytes={path: data},
                                           sources={"scenes.py": data})
            if behavior == "source-collision":
                loaded.sources[KEY] = b"authored collision"
            yield loaded

        def render(entries, directory, **options):
            if isinstance(entries, dict):
                entries = [RenderJobFixture(name, cls) for name, cls in entries.items()]
            entry, = entries
            scene = entry.scene(**(entry.scene_kwargs or {}))
            scene.run()
            if behavior == "mutate-options":
                options["_output_options"]["transparent"] = False
            sources = options["sources"]()
            (root / (entry.name + ".invocation.json")).write_bytes(sources[KEY])
            target = Path(directory) / (entry.name + "." + options["format"])
            if options["format"] == "png_sequence":
                target = Path(directory) / entry.name / "frames"
                target.mkdir(parents=True, exist_ok=False)
                artifact = target / "000000.png"
            else:
                target.parent.mkdir(parents=True, exist_ok=True)
                artifact = target
            raw = b"NOT-IMAGE:" + json.dumps(scene.value, sort_keys=True).encode()
            with artifact.open("xb") as stream:
                stream.write(raw)
            manifest = target.with_name(target.name + ".manifest") / "manifest.fmnp"
            manifest.parent.mkdir()
            manifest_bytes = b"NOT-FMNP:" + json.dumps({k: v.hex() for k, v in sources.items()}, sort_keys=True).encode()
            manifest.write_bytes(manifest_bytes)
            data = dict(destination=target, format=options["format"], resolution=options["resolution"],
                        fps=options["fps"], threads=options["threads"], engine="fixture-only",
                        bytes=len(raw), digest=hashlib.sha256(raw).hexdigest(),
                        frame_count=None if options["format"] == "wav" else 1,
                        sample_frames=8 if options["format"] == "wav" else None,
                        seed=0, certified=True, manifest=manifest,
                        closure_digest=hashlib.sha256(manifest_bytes).hexdigest(), ffmpeg_invocations=[])
            if options["format"] == "wav":
                data.update(sample_rate=48000, channels=2)
            return Result((Outcome(entry.name, target, "succeeded", Receipt(data)),), reproducible=True)

        loader.SceneSource = load
        h.batch.render_scenes = render
        with patch.dict(sys.modules, {loader.__name__: loader}):
            row = h.coordinator._render_source_job(job)
        if behavior == "bad-proof":
            row["provenance"]["invocation_sha256"] = "f" * 64
        if behavior == "changed-sidecar":
            Path(row["result"]["manifest"]["path"]).write_bytes(b"changed after proof")
        return row


SOURCE = '''from manimlib import Scene
class First(Scene):
    def __init__(self, value=0, nested=None):
        self.value = value
        if nested is not None:
            nested["items"].append("constructor-mutated")
    def run(self): pass
class Second(First): pass
'''


class FrozenInvocationTests(unittest.TestCase):
    def setUp(self):
        owner = certified_host()
        self.h = owner.__enter__()
        self.addCleanup(owner.__exit__, None, None, None)
        self.p = self.h.provenance

    def test_legacy_invocation_encoding_stays_byte_identical(self):
        expected = (b'{"options":{"_output_options":null,"animation_range":null,"format":"png",'
                    b'"fps":24,"resolution":[64,48]},"scene":"First","schema":"fmn-python.source-invocation",'
                    b'"source_sha256":"' + b'a' * 64 + b'","version":1}')
        self.assertEqual(self.p.freeze_invocation(invocation_job()), expected)

    def test_scheduling_and_destinations_are_not_semantic_inputs(self):
        job = invocation_job()
        before = self.p.freeze_invocation(job)
        job.update(directory="another-output", workers=16, job_timeout=1)
        job["options"]["threads"] = 16
        self.assertEqual(self.p.freeze_invocation(job), before)

    def test_options_and_selected_class_are_semantic_inputs(self):
        original = invocation_job()
        baseline = self.p.freeze_invocation(original)
        for field, value in [("format", "wav"), ("fps", 30), ("resolution", [320, 180]),
                             ("animation_range", [1, 2]), ("_output_options", {"transparent": True})]:
            job = deepcopy(original)
            job["options"][field] = value
            self.assertNotEqual(self.p.freeze_invocation(job), baseline)
        self.assertNotEqual(self.p.freeze_invocation(invocation_job(name="Second")), baseline)

    def test_variant_class_name_and_typed_kwargs_are_bound(self):
        raws = []
        for value in [None, False, True, 0, 1, 0.0, -0.0, 1.0, "1", [1], {"x": 1}]:
            job = invocation_job(scene_name="Lesson", scene_kwargs={"value": value})
            raw = self.p.freeze_invocation(job)
            data = json.loads(raw)
            self.assertEqual(data["scene"], "Lesson")
            self.assertEqual(data["output_name"], "First")
            self.assertEqual(data["version"], 2)
            raws.append(raw)
        self.assertEqual(len(set(raws)), len(raws))
        job["scene_name"] = "AnotherLesson"
        self.assertNotEqual(self.p.freeze_invocation(job), raws[-1])

    def test_mutation_cannot_rewrite_frozen_inputs_but_late_imports_are_included(self):
        job = invocation_job(scene_name="Lesson", scene_kwargs={"nested": {"items": [1]}})
        job["options"]["_output_options"] = {"transparent": True}
        raw = self.p.freeze_invocation(job)
        loaded = types.SimpleNamespace(sources={"scene.py": b"scene"})
        job["scene_kwargs"]["nested"]["items"].append(99)
        job["options"]["_output_options"]["transparent"] = False
        loaded.sources["lazy.py"] = b"late actual import"
        sources = self.p.executed_sources(loaded, job, invocation=raw)
        self.assertEqual(sources[KEY], raw)
        self.assertEqual(json.loads(sources[KEY])["scene_kwargs"], {"nested": {"items": [1]}})
        self.assertEqual(sources["lazy.py"], b"late actual import")
        self.assertNotIn(KEY, loaded.sources)

    def test_source_collision_is_not_overwritten(self):
        loaded = types.SimpleNamespace(sources={KEY: b"authored"})
        with self.assertRaisesRegex(ValueError, "collide"):
            self.p.executed_sources(loaded, invocation_job())
        self.assertEqual(loaded.sources, {KEY: b"authored"})

    def test_hash_mismatch_and_invalid_hash_are_rejected(self):
        job = invocation_job()
        job["invocation_sha256"] = hashlib.sha256(self.p.freeze_invocation(job)).hexdigest()
        self.p.freeze_invocation(job)
        for value in [None, 1, True, "invalid", "0" * 64]:
            with self.subTest(value=value), self.assertRaises(ValueError):
                self.p.freeze_invocation(dict(job, invocation_sha256=value))
        job["options"]["fps"] += 1
        with self.assertRaises(ValueError):
            self.p.freeze_invocation(job)

    def test_invalid_constructor_values_cannot_enter_a_certificate(self):
        for value in [(1, 2), {1: 2}, {"v": float("nan")}, {"v": object()}, {"v": {1, 2}}]:
            with self.subTest(value=repr(value)), self.assertRaises((TypeError, ValueError)):
                self.p.freeze_invocation(invocation_job(scene_name="Lesson", scene_kwargs=value))

    def test_parent_copies_admitted_digests_and_checks_proof_before_disk_io(self):
        with tempfile.TemporaryDirectory() as temporary:
            destination = Path(temporary) / "First.png"
            admitted = {"First": "a" * 64}
            owner = self.p.ParallelProvenance(self.h.native, "png", [destination], "b" * 64,
                                             invocation_digests=admitted)
            admitted["First"] = "c" * 64
            row = {"provenance": {"invocation_sha256": "c" * 64}}
            with patch.object(self.p, "_manifest_fingerprint") as disk:
                with self.assertRaisesRegex(ValueError, "invocation proof"):
                    owner.restore("First", destination, row)
                disk.assert_not_called()
            self.assertEqual(owner._invocation_digests["First"], "a" * 64)


class BoundWorkerTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.source = self.root / "scenes.py"
        self.source.write_text(SOURCE)
        self.output = self.root / "out"
        owner = certified_host()
        self.h = owner.__enter__()
        self.addCleanup(owner.__exit__, None, None, None)
        self.h.coordinator._render_source_job = execute_bound_job

    def render(self, entries=("First", "Second"), **extra):
        options = dict(format="png", reproducible=True, resolution=(64, 48), fps=24, workers=2)
        options.update(extra)
        return self.h.coordinator.render_source_scenes(self.source, self.output, entries, **options)

    def behavior(self, **values):
        (self.root / "behavior.json").write_text(json.dumps(values))

    def test_real_process_legacy_jobs_get_bound_proofs_and_keep_input_order(self):
        report = self.render()
        self.assertTrue(report.ok)
        self.assertEqual([row.name for row in report.outcomes], ["First", "Second"])
        pids = [int((self.root / (name + ".imports")).read_text()) for name in ("First", "Second")]
        self.assertEqual(len(set([os.getpid(), *pids])), 3)
        for row in report.outcomes:
            self.assertTrue(row.result.manifest.is_file())
            identity = json.loads((self.root / (row.name + ".invocation.json")).read_bytes())
            self.assertEqual(identity["scene"], row.name)
            self.assertEqual(identity["version"], 1)

    def test_worker_rejects_changed_invocation_before_source_import(self):
        self.behavior(First="changed-before-import")
        report = self.render(continue_on_error=True)
        self.assertEqual([row.status for row in report.outcomes], ["failed", "succeeded"])
        self.assertIn("invocation differs", report.outcomes[0].message)
        self.assertFalse((self.root / "First.imports").exists())
        self.assertFalse((self.output / "First.png").exists())

    def test_failed_transport_does_not_discard_completed_receipt(self):
        self.behavior(Second="bad-proof")
        with self.assertRaisesRegex(ValueError, "invocation proof") as caught:
            self.render(workers=1)
        report = caught.exception.render_batch_result
        self.assertEqual(report.outcomes[0].status, "succeeded")
        self.assertEqual(report.outcomes[1].status, "failed")
        self.assertTrue((self.output / "First.png").exists())
        self.assertTrue((self.output / "Second.png").exists())

    def test_changed_sidecar_is_not_accepted(self):
        self.behavior(First="changed-sidecar")
        with self.assertRaisesRegex(ValueError, "manifest changed"):
            self.render(entries=("First",))
        self.assertTrue((self.output / "First.png").exists())

    def test_runtime_mismatch_stops_before_source_import(self):
        job = invocation_job(source=str(self.source), directory=str(self.output),
                             working_directory=str(self.root), runtime_identities={})
        job["options"]["reproducible"] = True
        job["invocation_sha256"] = hashlib.sha256(self.h.provenance.freeze_invocation(job)).hexdigest()
        cwd = Path.cwd()
        try:
            with self.assertRaises(RuntimeChanged):
                execute_bound_job(job)
        finally:
            os.chdir(cwd)
        self.assertFalse((self.root / "First.imports").exists())

    def test_source_collision_stops_before_artifact_publication(self):
        self.behavior(First="source-collision")
        report = self.render(entries=("First",), continue_on_error=True)
        self.assertEqual(report.outcomes[0].status, "failed")
        self.assertIn("collide", report.outcomes[0].message)
        self.assertFalse((self.output / "First.png").exists())


if __name__ == "__main__":
    unittest.main()
