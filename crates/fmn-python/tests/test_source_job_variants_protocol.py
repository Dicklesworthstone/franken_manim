"""Named constructor variants through real spawn, receipts and checkpoint I/O.

Production coordination and _render_source_job execute. An explicit source
compiler/Scene/render fixture replaces native capture and writes NOT-FMTL bytes.
This is not native rendering or installed-wheel acceptance.
"""
from contextlib import contextmanager, nullcontext
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

from test_parallel_bundle_protocol import host_fixture, Outcome, Result, BatchError
from fmn_python.source_jobs import SourceRenderJob, prepare_source_jobs


@dataclass(frozen=True)
class RenderJobFixture:
    name: str
    scene: type
    scene_kwargs: dict | None = None


def execute_variant(job):
    with host_fixture() as h:
        h.batch.RenderJob = RenderJobFixture

        @contextmanager
        def load(source, base, **options):
            path = Path(source)
            data = path.read_bytes()
            namespace = {"__name__": "variant_fixture", "__file__": str(path)}
            exec(compile(data, str(path), "exec"), namespace)
            scenes = {name: value for name, value in namespace.items()
                      if isinstance(value, type) and issubclass(value, base)
                      and value.__module__ == namespace["__name__"]}
            yield types.SimpleNamespace(scenes=scenes, _source_bytes={path: data})

        def render(entries, directory, **options):
            if isinstance(entries, dict):
                entries = [RenderJobFixture(name, scene) for name, scene in entries.items()]
            assert len(entries) == 1
            entry = entries[0]
            root = Path(job["source"]).parent
            with (root / (entry.name + ".attempts")).open("a") as output:
                output.write(str(os.getpid()) + "\n")
            instance = entry.scene(**(entry.scene_kwargs or {}))
            instance.run()
            target = Path(directory) / (entry.name + ".fmtl")
            target.parent.mkdir(parents=True, exist_ok=True)
            data = (b"NOT-FMTL:" + json.dumps(instance.values, sort_keys=True).encode()).ljust(4096, b" ")
            with target.open("xb") as output:
                output.write(data)
            receipt = h.bundle.BundleExportResult(target, 12, 2, len(data),
                hashlib.sha256(data).hexdigest(), options["fps"] or 30,
                tuple(options["resolution"] or [64, 36]), options["bundle_camera"])
            return Result((Outcome(entry.name, target, "succeeded", result=receipt),))

        h.batch.render_scenes = render
        loader = types.ModuleType("fmn_python.scene_loading")
        loader.SceneSource = load
        effects = types.ModuleType("fmn_python.effect_audit")
        effects.recording = lambda enabled: nullcontext()
        with patch.dict(sys.modules, {loader.__name__: loader, effects.__name__: effects}):
            return h.coordinator._render_source_job(job)


SOURCE = '''from manimlib import Scene
from pathlib import Path
import os
root = Path(__file__).parent
with (root / "imports").open("a") as stream:
    stream.write(str(os.getpid()) + "\\n")
generation = 0
class Lesson(Scene):
    def __init__(self, value=0, nested=None, transient=False, fail=False, cancel=False):
        global generation
        generation += 1
        assert generation == 1, "shared interpreter state"
        self.values = dict(value=value, nested=nested)
        self.transient, self.fail, self.cancel = transient, fail, cancel
    def run(self):
        if self.cancel:
            raise KeyboardInterrupt("authored cancellation")
        if self.fail or (self.transient and not (root / "retry").exists()):
            raise RuntimeError("authored transient failure")
        if self.values["nested"] is not None:
            self.values["nested"]["items"].append("worker")
class Other(Lesson):
    pass
'''


class SourceVariantTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name).resolve()
        self.source = self.root / "scenes.py"
        self.source.write_text(SOURCE)
        self.output = self.root / "out"
        self.checkpoint = self.root / "progress.json"
        owner = host_fixture()
        self.h = owner.__enter__()
        self.addCleanup(owner.__exit__, None, None, None)
        self.h.coordinator._render_source_job = execute_variant

    def render(self, entries, **extra):
        options = dict(workers=2, format="fmtl", resolution=(64, 36), fps=30)
        options.update(extra)
        return self.h.coordinator.render_source_scenes(self.source, self.output, entries, **options)

    def recovery(self, entries, **extra):
        return self.render(entries, checkpoint=self.checkpoint, resume_key="stable-assets", **extra)

    def counts(self, *names):
        return [len((self.root / (name + ".attempts")).read_text().splitlines())
                if (self.root / (name + ".attempts")).exists() else 0 for name in names]

    def values(self, name):
        return json.loads((self.output / (name + ".fmtl")).read_bytes().removeprefix(b"NOT-FMTL:"))

    def test_repeated_source_class_has_unique_outputs_kwargs_and_interpreters(self):
        report = self.render([SourceRenderJob("seed7", "Lesson", {"value": 7}),
                              SourceRenderJob("seed11", "Lesson", {"value": 11}), "Other"])
        self.assertTrue(report.ok)
        self.assertEqual([row.name for row in report.outcomes], ["seed7", "seed11", "Other"])
        self.assertEqual([self.values(name)["value"] for name in ("seed7", "seed11", "Other")], [7, 11, 0])
        pids = list(map(int, (self.root / "imports").read_text().splitlines()))
        self.assertEqual(len(set(pids)), 3)
        self.assertNotIn(os.getpid(), pids)
        self.assertEqual(self.counts("seed7", "seed11", "Other"), [1, 1, 1])

    def test_constructor_and_parent_mutations_cannot_change_later_admitted_jobs(self):
        nested = {"items": [1]}
        kwargs = {"value": 1, "nested": nested}
        entries = [SourceRenderJob("a", "Lesson", kwargs), SourceRenderJob("b", "Lesson", kwargs)]
        def observer(row):
            if row.name == "a":
                kwargs["value"] = 99
                nested["items"].append("parent")
        self.render(entries, workers=1, on_result=observer)
        self.assertEqual(self.values("a"), {"value": 1, "nested": {"items": [1, "worker"]}})
        self.assertEqual(self.values("b"), self.values("a"))
        self.assertEqual(nested, {"items": [1, "parent"]})

    def test_resume_reuses_only_completed_variants_with_different_worker_count(self):
        entries = [SourceRenderJob("a", "Lesson", {"value": 1}),
                   SourceRenderJob("b", "Lesson", {"value": 2, "transient": True})]
        first = self.recovery(entries, continue_on_error=True)
        self.assertEqual([x.status for x in first.outcomes], ["succeeded", "failed"])
        before = (self.output / "a.fmtl").stat().st_mtime_ns
        plan = json.loads(self.checkpoint.read_text())["plan"]
        self.assertEqual(plan["constructor_inputs"], "source-variants-v1")
        self.assertEqual(plan["jobs"][1]["scene"], {"qualname": "Lesson"})
        self.assertEqual(plan["jobs"][1]["scene_kwargs"], {"value": 2, "transient": True})
        (self.root / "retry").write_text("repaired")
        events = []
        second = self.recovery(entries, workers=1, resume=True, on_result=lambda row: events.append(row.name))
        self.assertTrue(second.ok)
        self.assertEqual(second.reused, ("a",))
        self.assertEqual(events, ["a", "b"])
        self.assertEqual(self.counts("a", "b"), [1, 2])
        self.assertEqual((self.output / "a.fmtl").stat().st_mtime_ns, before)
        with patch.object(self.h.coordinator, "run_process_jobs", side_effect=AssertionError("must not spawn")):
            self.assertEqual(self.recovery(entries, workers=3, resume=True).reused, ("a", "b"))

    def test_changed_class_and_type_distinct_kwargs_refuse_reuse(self):
        self.recovery([SourceRenderJob("same", "Lesson", {"value": True})])
        before = self.checkpoint.read_bytes()
        for job in (SourceRenderJob("same", "Other", {"value": True}),
                    SourceRenderJob("same", "Lesson", {"value": 1}),
                    SourceRenderJob("same", "Lesson", {"value": 1.0})):
            with self.subTest(job=job), self.assertRaisesRegex(ValueError, "plan|options"):
                self.recovery([job], resume=True)
        self.assertEqual(self.checkpoint.read_bytes(), before)
        self.assertEqual(self.counts("same"), [1])

    def test_legacy_string_checkpoint_is_byte_compatible_and_not_a_variant(self):
        self.recovery(["Lesson"])
        plan = json.loads(self.checkpoint.read_text())["plan"]
        self.assertEqual(plan["constructor_inputs"], "source-file-v1")
        self.assertEqual(plan["jobs"][0]["scene_kwargs"], {})
        self.assertNotIn("variants", plan["options"])
        with self.assertRaisesRegex(ValueError, "plan|options"):
            self.recovery([SourceRenderJob("Lesson", "Lesson")], resume=True)
        self.assertEqual(self.counts("Lesson"), [1])

    def test_invalid_json_inputs_are_refused_before_files_or_worker_acquisition(self):
        circular = {}; circular["self"] = circular
        class Custom(dict):
            pass
        for value in ({"a": (1, 2)}, {1: 2}, {"a": float("nan")}, {"a": float("inf")},
                      {"a": object()}, circular, Custom(x=1), {"a": {1, 2}}, {"a": 1 << 4097}):
            with self.subTest(value=type(value).__name__), self.assertRaises((TypeError, ValueError)):
                self.recovery([SourceRenderJob("a", "Lesson", value)])
        self.assertFalse(self.checkpoint.exists())
        self.assertFalse(self.output.exists())
        self.assertFalse((self.root / "imports").exists())

    def test_invalid_selector_and_colliding_output_names_refuse_before_execution(self):
        for entries in ([SourceRenderJob("x", "Lesson.run")],
                        [SourceRenderJob("x", "__import__('os')")],
                        [SourceRenderJob("bad/name", "Lesson")],
                        [SourceRenderJob("same", "Lesson"), SourceRenderJob("Same", "Other")],
                        [SourceRenderJob("Other", "Lesson"), "Other"], [object()]):
            with self.subTest(entries=entries), self.assertRaises((TypeError, ValueError)):
                self.render(entries)
        self.assertFalse((self.root / "imports").exists())

    def test_constructor_budgets_bound_alias_expansion_depth_and_total_bytes(self):
        deep = []
        for _ in range(65):
            deep = [deep]
        cases = [{"a": "x" * (1024 * 1024)}, {"a": [0] * 65536}, {"a": deep}]
        shared = [0] * 512
        cases.append({"a": [shared] * 512})
        for value in cases:
            with self.subTest(kind=type(value["a"]).__name__), self.assertRaisesRegex(ValueError, "budget"):
                self.render([SourceRenderJob("job", "Lesson", value)])
        with self.assertRaisesRegex(ValueError, "8 MiB"):
            self.render([SourceRenderJob(f"a{i}", "Lesson", {"value": "x" * 950000}) for i in range(9)])
        self.assertFalse((self.root / "imports").exists())

    def test_iterable_is_admitted_once_and_job_budget_is_bounded(self):
        consumed = []
        def entries():
            for i in range(100):
                consumed.append(i)
                yield SourceRenderJob(f"job{i}", "Lesson")
        with self.assertRaisesRegex(ValueError, "1..2"):
            self.render(entries(), max_jobs=2)
        self.assertEqual(consumed, [0, 1, 2])
        with self.assertRaises(TypeError):
            self.render(SourceRenderJob("a", "Lesson"))

    def test_explicit_variants_do_not_inherit_certification(self):
        with self.assertRaisesRegex(RuntimeError, "CAPABILITY:.*constructor"):
            self.render([SourceRenderJob("a", "Lesson")], format="png", reproducible=True)
        self.assertFalse((self.root / "imports").exists())

    def test_fail_fast_preserves_earlier_variant_and_leaves_later_unstarted(self):
        entries = [SourceRenderJob("a", "Lesson"), SourceRenderJob("bad", "Lesson", {"fail": True}),
                   SourceRenderJob("later", "Lesson", {"value": 8})]
        with self.assertRaises(BatchError) as caught:
            self.render(entries, workers=1)
        self.assertEqual([row.status for row in caught.exception.result.outcomes],
                         ["succeeded", "failed", "not_run"])
        self.assertEqual(self.counts("a", "bad", "later"), [1, 1, 0])
        self.assertTrue((self.output / "a.fmtl").exists())

    def test_authored_cancellation_preserves_prior_publication_and_stops_admission(self):
        with self.assertRaises(KeyboardInterrupt) as caught:
            self.render([SourceRenderJob("a", "Lesson"),
                         SourceRenderJob("stop", "Lesson", {"cancel": True}),
                         SourceRenderJob("later", "Lesson")], workers=1, continue_on_error=True)
        self.assertEqual([row.status for row in caught.exception.render_batch_result.outcomes],
                         ["succeeded", "cancelled", "not_run"])
        self.assertEqual(self.counts("later"), [0])

    def test_observer_failure_is_after_durable_publication(self):
        entries = [SourceRenderJob("a", "Lesson")]
        def observer(row):
            saved = json.loads(self.checkpoint.read_text())
            self.assertEqual(saved["outcomes"][0]["status"], "succeeded")
            raise RuntimeError("observer failed")
        with self.assertRaisesRegex(RuntimeError, "observer failed") as caught:
            self.recovery(entries, on_result=observer)
        self.assertTrue(caught.exception.render_batch_result.ok)
        self.assertEqual(self.recovery(entries, resume=True).reused, ("a",))
        self.assertEqual(self.counts("a"), [1])

    def test_missing_class_is_reported_under_output_name_and_keep_going_continues(self):
        report = self.render([SourceRenderJob("bad", "Absent"), SourceRenderJob("good", "Lesson")],
                             workers=1, continue_on_error=True)
        self.assertEqual([row.name for row in report.outcomes], ["bad", "good"])
        self.assertIn("Absent", report.outcomes[0].message)
        self.assertEqual([row.status for row in report.outcomes], ["failed", "succeeded"])

    def test_changed_artifact_and_source_refuse_resume_without_another_constructor(self):
        entries = [SourceRenderJob("a", "Lesson")]
        self.recovery(entries)
        path = self.output / "a.fmtl"
        old = path.read_bytes()
        path.write_bytes(old + b"tamper")
        with self.assertRaisesRegex(ValueError, "missing or modified"):
            self.recovery(entries, resume=True)
        path.write_bytes(old)
        self.source.write_text(SOURCE + "\n# different input\n")
        with self.assertRaisesRegex(ValueError, "plan|options"):
            self.recovery(entries, resume=True)
        self.assertEqual(self.counts("a"), [1])


if __name__ == "__main__":
    unittest.main()
