"""Partial-scene batch admission and delegation with real spawned workers.

The durable byte worker below is deliberately NOT a native renderer. It tests
parent plan ownership, dispatch, interruption and retries using real processes,
locks, files and receipt transport. Native prefix replay has separate installed-
wheel acceptance; successful protocol tests do not establish rendered pixels.
"""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import sys
import tempfile
import time
from types import SimpleNamespace
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "python"))
from fmn_python import parallel_batch
from fmn_python._process_batch import failure
from fmn_python.batch_checkpoint import _CheckpointFile, _json, _read_document
from fmn_python.batch_rendering import SceneRenderOutcome
from fmn_python.parallel_clip_recovery import ParallelClipPlan, render_clip_job
from fmn_python.rendering import RenderResult
from fmn_python.source_jobs import SourceRenderJob
from fmn_python.subdivision import RenderSegment, SubdividedRenderResult


class ByteJournal(_CheckpointFile):
    schema = "fmn-python.clip-checkpoint"
    version = 1


def byte_clip_job(job):
    """Importable fixture for native capture, not for parent recovery logic."""
    name = job["name"]
    root = Path(job["source"]).parent
    destination = Path(job["directory"]) / name / "clips"
    recovery, options = job["clip_recovery"], job["options"]
    mode = json.loads((root / "behavior.json").read_text()).get(name, "ok")
    with (root / (name + ".calls")).open("a") as stream:
        stream.write(str(os.getpid()) + "\n")
    (root / (name + ".job.json")).write_text(json.dumps(job))
    segments = []
    owner = ByteJournal(recovery["checkpoint"], resume=recovery["resume"], key=recovery["resume_key"])
    with owner:
        saved = [] if owner.document is None else owner.document["segments"]
        if owner.document is None:
            owner._publish(_json({"schema": owner.schema, "version": owner.version, "key": owner.key,
                                  "plan": {"destination": str(destination)}, "segments": []}))
        destination.mkdir(parents=True, exist_ok=True)
        expected = {f"segment-{index:06d}.y4m" for index in range(len(saved))}
        if {path.name for path in destination.iterdir()} != expected:
            raise ValueError("unrecorded clip: fixture refuses adoption")
        for index in range(3):
            path = destination / f"segment-{index:06d}.y4m"
            payload = b"NOT-MEDIA clip " + str(index).encode()
            digest = hashlib.sha256(payload).hexdigest()
            if index < len(saved):
                if path.read_bytes() != payload or saved[index] != digest:
                    raise ValueError("modified clip: fixture refuses reuse")
            else:
                with path.open("xb") as stream:
                    stream.write(payload)
                saved.append(digest)
                owner._publish(_json({"schema": owner.schema, "version": owner.version, "key": owner.key,
                                      "plan": {"destination": str(destination)}, "segments": saved}))
            result = RenderResult(path, "y4m", (64, 36), 8, 1, "byte-fixture", len(payload), digest,
                                  1, None, 7)
            segments.append(RenderSegment(index, "wait", index, index + 1, result))
            if index == 0 and mode != "ok":
                if mode == "crash":
                    os._exit(23)
                if mode == "sleep":
                    try:
                        time.sleep(30)
                    except KeyboardInterrupt:
                        mode = "cancel"
                partial = SubdividedRenderResult(destination, tuple(segments), False)
                return SceneRenderOutcome(name, destination, "cancelled" if mode == "cancel" else "failed",
                                          partial, "KeyboardInterrupt" if mode == "cancel" else "RuntimeError",
                                          "fixture failure after first clip").as_dict()
    return SceneRenderOutcome(name, destination, "succeeded",
                              SubdividedRenderResult(destination, tuple(segments), True)).as_dict()


class ParallelClipPlanTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="fmn-clip-prefix-"))
        self.state = self.root / "recovery"
        self.source = self.root / "source.py"
        self.source.write_text("# unchanged fixture source\n")
        self.destination = self.root / "out" / "A" / "clips"
        self.job = dict(source=str(self.source), source_digest="a" * 64, name="A",
                        directory=str(self.root / "out"), working_directory=str(self.root), options={"fps": 8})

    def owner(self, *, resume=False):
        return ParallelClipPlan(self.state, resume=resume, key="inputs-v1")

    def admit(self):
        with self.owner() as owner:
            owner.validate_destinations([self.destination])
            owner.prepare([self.job], [self.destination])
            owner.admit()

    def child(self, **changes):
        path = self.state / "jobs" / "A.json"
        path.parent.mkdir(parents=True, exist_ok=True)
        doc = {"schema": "fmn-python.clip-checkpoint", "version": 1, "key": "inputs-v1",
               "plan": {"destination": str(self.destination)}}
        doc.update(changes)
        path.write_text(json.dumps(doc))
        return path

    def test_new_and_unstarted_jobs_remain_fresh_after_resume(self):
        self.admit()
        with self.owner(resume=True) as owner:
            owner.prepare([self.job], [self.destination])
            self.assertFalse(owner.recovery["A"]["resume"])
            self.assertEqual(owner.existing_destinations, set())

    def test_saved_child_is_admitted_without_adopting_unrecorded_outputs(self):
        self.admit()
        path = self.child()
        self.destination.mkdir(parents=True)
        with self.owner(resume=True) as owner:
            owner.prepare([self.job], [self.destination])
            self.assertEqual(owner.recovery["A"], {"checkpoint": str(path), "resume": True,
                                                   "resume_key": "inputs-v1"})
            self.assertEqual(owner.existing_destinations, {self.destination})

    def test_source_variants_and_type_sensitive_inputs_are_bound(self):
        self.job.update(scene_name="Lesson", scene_kwargs={"value": True})
        self.admit()
        changes = ({"source_digest": "b" * 64}, {"scene_name": "Other"},
                   {"scene_kwargs": {"value": 1}}, {"working_directory": str(self.root.parent)},
                   {"options": {"fps": 9}}, {"name": "B"})
        for change in changes:
            with self.subTest(change=change), self.owner(resume=True) as owner:
                with self.assertRaisesRegex(ValueError, "plan"):
                    owner.prepare([dict(self.job, **change)], [self.destination])

    def test_unjournaled_output_is_refused_without_modification(self):
        self.admit()
        self.destination.mkdir(parents=True)
        path = self.destination / "foreign"
        path.write_bytes(b"keep")
        with self.owner(resume=True) as owner, self.assertRaises(FileExistsError):
            owner.prepare([self.job], [self.destination])
        self.assertEqual(path.read_bytes(), b"keep")

    def test_unknown_or_redirected_child_document_is_refused(self):
        self.admit()
        for change in ({"version": True}, {"key": "different"}, {"schema": "other"},
                       {"plan": {"destination": str(self.root / "foreign")}}, {"plan": None}):
            with self.subTest(change=change):
                self.child(**change)
                with self.owner(resume=True) as owner, self.assertRaises(ValueError):
                    owner.prepare([self.job], [self.destination])

    def test_state_cannot_overlap_output_collections(self):
        for destination in (self.state, self.state / "nested", self.state.parent):
            with self.subTest(destination=destination), self.assertRaises(ValueError):
                self.owner().validate_destinations([destination])

    @unittest.skipUnless(hasattr(os, "symlink"), "requires symlink support")
    def test_state_and_child_links_are_refused(self):
        target = self.root / "target"
        target.mkdir()
        link = self.root / "linked"
        link.symlink_to(target, target_is_directory=True)
        with self.assertRaises(ValueError):
            ParallelClipPlan(link, resume=False, key="v1")
        self.admit()
        (self.state / "jobs").symlink_to(target, target_is_directory=True)
        with self.assertRaises(ValueError):
            self.owner(resume=True)

    def test_live_parent_lock_excludes_a_second_invocation_and_releases_after_error(self):
        self.admit()
        with self.owner(resume=True):
            with self.assertRaises(OSError):
                with self.owner(resume=True):
                    self.fail("concurrent plan ownership was admitted")
        with self.owner(resume=True) as owner:
            owner.prepare([self.job], [self.destination])

    def test_interrupted_atomic_write_temporaries_do_not_block_verified_retry(self):
        self.admit()
        (self.state / "jobs").mkdir()
        paths = (self.state / "plan.json.abcdefgh.tmp", self.state / "jobs/A.json.12345678.tmp")
        for path in paths:
            path.write_bytes(b"incomplete journal, not authority")
        with self.owner(resume=True) as owner:
            owner.prepare([self.job], [self.destination])
            self.assertFalse(owner.recovery["A"]["resume"])
        for path in paths:
            self.assertEqual(path.read_bytes(), b"incomplete journal, not authority")

    def test_unexpected_state_is_not_overwritten(self):
        self.admit()
        extra = self.state / "foreign"
        extra.write_bytes(b"keep")
        with self.owner(resume=True) as owner, self.assertRaisesRegex(ValueError, "unrecorded"):
            owner.prepare([self.job], [self.destination])
        self.assertEqual(extra.read_bytes(), b"keep")


class ParallelClipProcessTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="fmn-clip-process-"))
        self.source = self.root / "scenes.py"
        self.source.write_text("# durable byte fixture, not native rendering\n")
        self.output, self.state = self.root / "out", self.root / "state"
        self.behavior()
        self.native = SimpleNamespace(_portal_prepare_recording_scene=lambda: None,
                                      _portal_begin_audio_clip=lambda: None)

    def behavior(self, **values):
        (self.root / "behavior.json").write_text(json.dumps(values))

    def render(self, names=("A", "B"), **options):
        values = dict(format="y4m", subdivide=True, resolution=(64, 36), fps=8,
                      workers=2, clip_checkpoint_dir=self.state, resume_key="v1", continue_on_error=True)
        values.update(options)
        with patch.dict(sys.modules, {"manimlib": self.native}), \
                patch.object(parallel_batch, "_render_source_job", byte_clip_job):
            return parallel_batch.render_source_scenes(self.source, self.output, names, **values)

    def test_failed_scene_recovers_its_prefix_without_rewriting_saved_clips(self):
        self.behavior(B="fail")
        first = self.render()
        self.assertEqual([row.status for row in first.outcomes], ["succeeded", "failed"])
        saved = self.output / "B/clips/segment-000000.y4m"
        signature = (saved.stat().st_mtime_ns, saved.read_bytes())
        self.behavior()
        second = self.render(resume=True, workers=1, job_timeout=8)
        self.assertTrue(second.ok)
        self.assertEqual((saved.stat().st_mtime_ns, saved.read_bytes()), signature)
        self.assertEqual(second.reused, ())
        self.assertEqual(len(second.outcomes[1].result.segments), 3)
        for name in ("A", "B"):
            self.assertEqual(len((self.root / (name + ".calls")).read_text().splitlines()), 2)
            job = json.loads((self.root / (name + ".job.json")).read_text())
            self.assertTrue(job["clip_recovery"]["resume"])
            self.assertNotIn("checkpoint", job["options"])

    def test_crashed_worker_prefix_is_recovered_without_a_terminal_receipt(self):
        self.behavior(A="crash")
        first = self.render(("A",))
        self.assertEqual(first.outcomes[0].status, "failed")
        self.assertIsNone(first.outcomes[0].result)
        self.assertTrue((self.state / "jobs/A.json").is_file())
        self.behavior()
        self.assertTrue(self.render(("A",), resume=True).ok)

    def test_fail_fast_resume_starts_unadmitted_scene_without_inventing_a_journal(self):
        self.behavior(A="fail")
        with self.assertRaises(parallel_batch.BatchRenderError) as caught:
            self.render(workers=1, continue_on_error=False)
        self.assertEqual(caught.exception.result.outcomes[1].status, "not_run")
        self.assertFalse((self.state / "jobs/B.json").exists())
        self.behavior()
        self.assertTrue(self.render(resume=True).ok)
        self.assertEqual(len((self.root / "B.calls").read_text().splitlines()), 1)

    @unittest.skipUnless(os.name == "posix", "cooperative timeout uses SIGINT")
    def test_timeout_preserves_prefix_and_later_resume(self):
        self.behavior(A="sleep")
        first = self.render(("A",), workers=1, job_timeout=1.5)
        self.assertEqual(first.outcomes[0].error_type, "TimeoutError")
        self.assertEqual(len(first.outcomes[0].result.segments), 1)
        self.behavior()
        self.assertTrue(self.render(("A",), resume=True).ok)

    def test_mutated_entry_source_refuses_before_new_workers(self):
        self.render()
        self.source.write_text("# changed input\n")
        with patch.object(parallel_batch, "run_process_jobs") as workers, self.assertRaisesRegex(ValueError, "plan"):
            self.render(resume=True)
        workers.assert_not_called()

    def test_named_variants_keep_distinct_child_journals(self):
        names = [SourceRenderJob("seed7", "Lesson", {"seed": 7}),
                 SourceRenderJob("seed9", "Lesson", {"seed": 9})]
        self.behavior(seed9="fail")
        self.render(names)
        self.assertTrue((self.state / "jobs/seed7.json").is_file())
        self.assertTrue((self.state / "jobs/seed9.json").is_file())
        self.behavior()
        self.assertTrue(self.render(names, resume=True).ok)
        with self.assertRaisesRegex(ValueError, "plan"):
            self.render([SourceRenderJob("seed7", "Lesson", {"seed": 8}), names[1]], resume=True)

    def test_incompatible_modes_refuse_before_worker_admission(self):
        for changes in ({"subdivide": False}, {"checkpoint": self.root / "batch.json"},
                        {"save_last_frame": True}, {"format": "fmtl"}, {"reproducible": True}):
            with self.subTest(changes=changes), patch.object(parallel_batch, "run_process_jobs") as workers:
                with self.assertRaises((ValueError, RuntimeError)):
                    self.render(**changes)
                workers.assert_not_called()
        self.assertFalse(self.state.exists())


class ClipWorkerDelegationTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="fmn-clip-worker-"))
        self.destination = self.root / "out/A/clips"
        self.job = dict(name="A", scene_name="Lesson", scene_kwargs={"seed": 7}, source="/source.py",
                        source_digest="a" * 64, working_directory="/work", directory=str(self.root / "out"),
                        options=dict(format="y4m", resolution=[64, 36], fps=8, threads=1,
                                     animation_range=[1, 3], max_segments=8, _output_options={"video_crf": 20},
                                     _configuration={"frozen": True}),
                        clip_recovery=dict(checkpoint=str(self.root / "state/jobs/A.json"), resume=True, resume_key="v1"))
        self.scene = type("Lesson", (), {})

    def test_worker_delegates_camera_selection_writer_variants_and_identity(self):
        result = SubdividedRenderResult(self.destination, (), True)
        with patch("fmn_python.subdivision_rendering.render_subdivided_scene", create=True, return_value=result) as render:
            row = render_clip_job(self.job, self.scene)
        args, options = render.call_args
        self.assertEqual(args, (self.scene, self.destination))
        self.assertEqual(options["scene_kwargs"], {"seed": 7})
        self.assertEqual(options["animation_range"], [1, 3])
        self.assertEqual(options["_output_options"], {"video_crf": 20})
        self.assertEqual(options["checkpoint"], self.job["clip_recovery"]["checkpoint"])
        self.assertEqual(options["_source_identity"]["configuration"], {"frozen": True})
        self.assertEqual(options["_source_identity"]["source_digest"], "a" * 64)
        self.assertEqual(row["status"], "succeeded")

    def test_shipping_source_worker_selects_original_class_and_uses_prefix_owner(self):
        import contextlib
        import types
        source = self.root / "source.py"
        source.write_bytes(b"# compiled fixture source\n")
        self.job.update(source=str(source), source_digest=hashlib.sha256(source.read_bytes()).hexdigest(),
                        working_directory=str(self.root))
        loaded = SimpleNamespace(scenes={"Lesson": self.scene}, _source_bytes={source: source.read_bytes()})
        loader = types.ModuleType("fmn_python.scene_loading")
        loader.SceneSource = lambda *args, **kwargs: contextlib.nullcontext(loaded)
        audit = types.ModuleType("fmn_python.effect_audit")
        audit.recording = lambda *args: contextlib.nullcontext()
        native = types.ModuleType("manimlib")
        native.Scene = object
        result = SubdividedRenderResult(self.destination, (), True)
        original = Path.cwd()
        try:
            with patch.dict(sys.modules, {"manimlib": native, "fmn_python.scene_loading": loader,
                                         "fmn_python.effect_audit": audit}), \
                    patch("fmn_python.batch_rendering.render_scenes", create=True,
                          side_effect=AssertionError("must not enter fresh batch no-clobber")), \
                    patch("fmn_python.subdivision_rendering.render_subdivided_scene", create=True,
                          return_value=result) as render:
                row = parallel_batch._render_source_job(self.job)
            self.assertEqual(row["status"], "succeeded")
            self.assertIs(render.call_args.args[0], self.scene)
            self.assertEqual(render.call_args.kwargs["scene_kwargs"], {"seed": 7})
            self.assertTrue(render.call_args.kwargs["resume"])
        finally:
            os.chdir(original)

    def test_worker_keeps_typed_partial_result_and_interrupt_identity(self):
        for error in (RuntimeError("failed"), KeyboardInterrupt("cancelled"), SystemExit(2)):
            with self.subTest(error=type(error).__name__):
                result = SubdividedRenderResult(self.destination, (), False)
                error.render_subdivision_result = result
                error.add_note("native publication was retained")
                with patch("fmn_python.subdivision_rendering.render_subdivided_scene", create=True, side_effect=error):
                    row = render_clip_job(self.job, self.scene)
                self.assertEqual(row["status"], "failed" if isinstance(error, Exception) else "cancelled")
                self.assertEqual(row["result"], result.as_dict())
                self.assertEqual(row["error"]["type"], type(error).__name__)
                self.assertIn("native publication was retained", row["error"]["notes"])

    def test_worker_does_not_adopt_foreign_or_success_shaped_partial_results(self):
        for result in (SubdividedRenderResult(self.root / "foreign", (), False),
                       SubdividedRenderResult(self.destination, (), True), {"completed": False}):
            with self.subTest(result=result):
                error = ValueError("failure")
                error.render_subdivision_result = result
                with patch("fmn_python.subdivision_rendering.render_subdivided_scene", create=True, side_effect=error):
                    row = render_clip_job(self.job, self.scene)
                self.assertIsNone(row["result"])


if __name__ == "__main__":
    unittest.main(verbosity=2)
