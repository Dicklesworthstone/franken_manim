"""Paired prefix recovery through real process transport and shipping dispatch.

Native scene playback and immutable camera capture are explicit byte fixtures.
The paired owner, both journals, invocation planner, worker dispatch, result
restoration, locks and spawn supervisor execute production code. NOT-MEDIA
files are never claimed as PNG/codec evidence.
"""
from __future__ import annotations

from contextlib import contextmanager
import json
import os
from pathlib import Path
import sys
import tempfile
import time
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import test_parallel_clip_recovery as old
from test_paired_clip_recovery_protocol import ClipPrimaryFixture, CaptureFixture
from fmn_python import paired_output, parallel_batch, rendering
from fmn_python._process_batch import positive_integer
from fmn_python.paired_clip_recovery import FinalImageCheckpoint, final_checkpoint_path
from fmn_python.parallel_clip_recovery import ParallelClipPlan, render_clip_job
from fmn_python.paired_output import PairedRenderResult
from fmn_python.subdivision import SubdividedRenderResult


def paired_worker_fixture(job):
    root = Path(job["source"]).parent
    name, options, recovery = job["name"], job["options"], job["clip_recovery"]
    mode = json.loads((root / "behavior.json").read_text()).get(name, "ok")
    destination = Path(job["directory"]) / name / "clips"
    with (root / (name + ".calls")).open("a") as stream:
        stream.write(str(os.getpid()) + "\n")
    (root / (name + ".job.json")).write_text(json.dumps(job))
    primary = ClipPrimaryFixture(root, resume=recovery["resume"], destination=destination,
                                 checkpoint=recovery["checkpoint"], key=recovery["resume_key"])
    primary.plan["identity"] = {key: job.get(key) for key in (
        "name", "source_digest", "scene_name", "scene_kwargs", "working_directory")}
    class SceneFixture:
        def __init__(self, **kwargs):
            self.file_writer = primary.scene.file_writer
            self.camera = primary.scene.camera
            self.mobjects = []
            self.kwargs = kwargs
        def run(self):
            for index in range(3):
                primary.play()
                if index == 0:
                    if mode == "crash":
                        os._exit(31)
                    if mode == "fail":
                        raise RuntimeError("fixture interruption after durable first clip")
                    if mode == "sleep":
                        time.sleep(30)
    native = SimpleNamespace(Scene=SceneFixture, EndScene=type("EndScene", (Exception,), {}),
        _CameraCapture=CaptureFixture, _CapabilityError=RuntimeError)
    def session(scene, path, **kwargs):
        if Path(path) != destination or Path(kwargs["checkpoint"]) != primary._journal.path:
            raise AssertionError("worker changed planned paths")
        if kwargs["resume"] is not recovery["resume"] or kwargs["resume_key"] != recovery["resume_key"]:
            raise AssertionError("worker changed resume identity")
        if kwargs["_input_identity"]["source"]["source_digest"] != job["source_digest"]:
            raise AssertionError("worker lost source identity")
        if kwargs["_input_identity"]["scene_kwargs"] != (job.get("scene_kwargs") or {}):
            raise AssertionError("worker lost constructor inputs")
        primary.scene = scene
        return primary
    def lifecycle(scene, owner, native):
        with owner:
            try:
                scene.run()
            except native.EndScene:
                pass
        if owner.result is None:
            raise AssertionError("paired owner failed to publish its receipt")
        return owner.result
    def publish(owner, payload):
        if mode in {"image-journal-failure", "crash-after-image"} and json.loads(payload)["pair"] is not None:
            if mode == "crash-after-image":
                os._exit(32)
            raise OSError("injected final journal publication failure")
        return original_publish(owner, payload)
    original_publish = FinalImageCheckpoint._publish
    with patch.dict(sys.modules, {"manimlib": native}), \
            patch("fmn_python.subdivision_rendering.subdivided_render_session", session, create=True), \
            patch.object(rendering, "_positive_integer", positive_integer, create=True), \
            patch.object(rendering, "_run_owned_scene_render", lifecycle, create=True), \
            patch.object(FinalImageCheckpoint, "_publish", publish):
        row = render_clip_job(job, SceneFixture)
    (root / (name + ".events.json")).write_text(json.dumps(primary.events))
    return row


class PairedProcessTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="fmn-pair-process-"))
        self.source = self.root / "source.py"
        self.source.write_text("# fixture, native media is not executed\n")
        self.output, self.state = self.root / "out", self.root / "state"
        self.behavior()
        self.native = SimpleNamespace(_portal_prepare_recording_scene=lambda: None,
            _portal_begin_audio_clip=lambda: None, _CameraCapture=CaptureFixture)

    def behavior(self, **values):
        (self.root / "behavior.json").write_text(json.dumps(values))

    def render(self, names=("A", "B"), **options):
        values = dict(format="y4m", subdivide=True, save_last_frame=True, resolution=(64, 36), fps=8,
            workers=2, clip_checkpoint_dir=self.state, resume_key="v1", continue_on_error=True)
        values.update(options)
        with patch.dict(sys.modules, {"manimlib": self.native}), \
                patch.object(parallel_batch, "_render_source_job", paired_worker_fixture):
            return parallel_batch.render_source_scenes(self.source, self.output, names, **values)

    def signature(self, path):
        return path.stat().st_mtime_ns, path.read_bytes()

    def test_partial_scene_and_completed_pair_both_resume_without_republication(self):
        self.behavior(B="fail")
        first = self.render()
        self.assertEqual([row.status for row in first.outcomes], ["succeeded", "failed"])
        self.assertTrue(first.outcomes[0].result.completed)
        partial = first.outcomes[1].result
        self.assertFalse(partial.completed)
        self.assertEqual(len(partial.primary.segments), 1)
        self.assertIsNone(partial.still)
        a = first.outcomes[0].result.still_destination
        b = partial.primary.segments[0].render.destination
        old_a, old_b = self.signature(a), self.signature(b)
        self.behavior()
        second = self.render(resume=True, workers=1, job_timeout=8)
        self.assertTrue(second.ok, second.as_dict())
        self.assertEqual(second.reused, ())
        self.assertEqual((self.signature(a), self.signature(b)), (old_a, old_b))
        self.assertEqual(len(second.outcomes[1].result.primary.segments), 3)
        self.assertTrue(second.outcomes[1].result.still_destination.exists())
        self.assertNotIn("capture", json.loads((self.root / "A.events.json").read_text()))
        self.assertIn("capture", json.loads((self.root / "B.events.json").read_text()))
        for name in ("A", "B"):
            self.assertEqual(len((self.root / (name + ".calls")).read_text().splitlines()), 2)
            job = json.loads((self.root / (name + ".job.json")).read_text())
            self.assertTrue(job["clip_recovery"]["resume_final"])
            self.assertEqual(job["clip_recovery"]["final_checkpoint"], str(self.state / f"jobs/{name}.still.json"))

    def test_hard_exit_after_a_durable_clip_recovers_without_terminal_receipt(self):
        self.behavior(A="crash")
        first = self.render(("A",))
        self.assertEqual(first.outcomes[0].status, "failed")
        self.assertIsNone(first.outcomes[0].result)
        clip = self.output / "A/clips/segment-000000.y4m"
        saved = self.signature(clip)
        self.behavior()
        second = self.render(("A",), resume=True)
        self.assertTrue(second.ok, second.as_dict())
        self.assertEqual(self.signature(clip), saved)
        self.assertTrue(second.outcomes[0].result.completed)

    def test_fail_fast_retry_starts_an_unadmitted_pair_with_fresh_journals(self):
        self.behavior(A="fail")
        with self.assertRaises(parallel_batch.BatchRenderError) as caught:
            self.render(workers=1, continue_on_error=False)
        self.assertEqual(caught.exception.result.outcomes[1].status, "not_run")
        self.assertFalse((self.state / "jobs/B.still.json").exists())
        self.behavior()
        self.assertTrue(self.render(resume=True).ok)
        self.assertEqual(len((self.root / "B.calls").read_text().splitlines()), 1)

    def test_final_image_journal_failure_keeps_receipts_and_refuses_orphan_retry(self):
        self.behavior(A="image-journal-failure")
        first = self.render(("A",))
        row = first.outcomes[0]
        self.assertEqual(row.status, "failed")
        self.assertEqual(row.error_type, "OSError")
        self.assertFalse(row.result.completed)
        self.assertTrue(row.result.primary.completed)
        self.assertIsNotNone(row.result.still)
        image = row.result.still_destination
        saved = self.signature(image)
        self.behavior()
        with patch.object(parallel_batch, "run_process_jobs") as workers, self.assertRaises(FileExistsError):
            self.render(("A",), resume=True)
        workers.assert_not_called()
        self.assertEqual(self.signature(image), saved)

    def test_crash_after_png_publication_preserves_unrecorded_image(self):
        self.behavior(A="crash-after-image")
        self.assertEqual(self.render(("A",)).outcomes[0].status, "failed")
        image = self.output / "A/clips.png"
        saved = self.signature(image)
        self.behavior()
        with self.assertRaises(FileExistsError):
            self.render(("A",), resume=True)
        self.assertEqual(self.signature(image), saved)

    def test_tampered_completed_image_fails_without_new_render_generation(self):
        self.render(("A",))
        image = self.output / "A/clips.png"
        image.write_bytes(b"tampered")
        saved_clip = self.signature(self.output / "A/clips/segment-000000.y4m")
        second = self.render(("A",), resume=True)
        self.assertEqual(second.outcomes[0].status, "failed")
        self.assertEqual(image.read_bytes(), b"tampered")
        self.assertEqual(self.signature(self.output / "A/clips/segment-000000.y4m"), saved_clip)
        events = json.loads((self.root / "A.events.json").read_text())
        self.assertNotIn("capture", events)

    @unittest.skipUnless(os.name == "posix", "cooperative cancellation uses POSIX signals")
    def test_timeout_preserves_paired_partial_progress_for_retry(self):
        self.behavior(A="sleep")
        first = self.render(("A",), workers=1, job_timeout=2)
        row = first.outcomes[0]
        self.assertEqual(row.error_type, "TimeoutError")
        self.assertFalse(row.result.completed)
        self.assertEqual(len(row.result.primary.segments), 1)
        self.assertIsNone(row.result.still)
        self.behavior()
        self.assertTrue(self.render(("A",), resume=True).ok)

    def test_recovery_mode_is_bound_and_state_cannot_overlap_companion(self):
        self.render(("A",))
        with self.assertRaisesRegex(ValueError, "plan"):
            self.render(("A",), resume=True, save_last_frame=False)
        other = self.root / "fresh"
        with patch.dict(sys.modules, {"manimlib": self.native}), \
                patch.object(parallel_batch, "run_process_jobs") as workers, self.assertRaises(ValueError):
            parallel_batch.render_source_scenes(self.source, other, ["A"], subdivide=True,
                save_last_frame=True, format="y4m", clip_checkpoint_dir=other / "A/clips.png",
                resume_key="v1")
        workers.assert_not_called()


class PairedPlanTests(unittest.TestCase):
    def setUp(self):
        self.host = PairedProcessTests()
        self.host.setUp()

    def test_foreign_final_journal_identity_and_partial_receipts_refuse_before_spawn(self):
        host = self.host
        host.render(("A",))
        path = host.state / "jobs/A.still.json"
        original = json.loads(path.read_text())
        mutations = [dict(original, schema="foreign"), dict(original, version=True),
                     dict(original, key="other"), dict(original, extra="unexpected"),
                     dict(original, plan=dict(original["plan"], still_destination="/foreign.png")),
                     dict(original, pair=dict(original["pair"], completed=False)),
                     dict(original, pair=dict(original["pair"], still_destination="/foreign.png"))]
        for document in mutations:
            with self.subTest(document=document):
                path.write_text(json.dumps(document))
                with patch.object(parallel_batch, "run_process_jobs") as workers, self.assertRaises(ValueError):
                    host.render(("A",), resume=True)
                workers.assert_not_called()
        path.write_text(json.dumps(original))
        self.assertTrue(host.render(("A",), resume=True).ok)

    def test_final_journal_requires_clip_authority_and_never_adopts_existing_png(self):
        host = self.host
        host.render(("A",))
        clip = host.state / "jobs/A.json"
        clip.rename(host.root / "preserved-clip.json")
        with self.assertRaises((FileExistsError, ValueError)):
            host.render(("A",), resume=True)
        self.assertTrue((host.output / "A/clips.png").is_file())

    def test_inert_final_journal_temporary_is_preserved_on_resume(self):
        host = self.host
        host.render(("A",))
        temporary = host.state / "jobs/A.still.json.abcd1234.tmp"
        temporary.write_bytes(b"incomplete, not authority")
        self.assertTrue(host.render(("A",), resume=True).ok)
        self.assertEqual(temporary.read_bytes(), b"incomplete, not authority")


class PairedDispatchTests(unittest.TestCase):
    def setUp(self):
        original = old.ClipWorkerDelegationTests()
        original.setUp()
        self.original = original
        self.job = original.job
        self.job["options"]["save_last_frame"] = True
        self.job["clip_recovery"].update(final_checkpoint=str(final_checkpoint_path(
            self.job["clip_recovery"]["checkpoint"])), resume_final=True)
        self.still = original.destination.with_name("clips.png")

    def test_worker_routes_pair_and_identity_through_existing_authoring_helper(self):
        result = PairedRenderResult(self.original.destination, self.still,
            SubdividedRenderResult(self.original.destination, (), True), None, True)
        with patch.object(paired_output, "render_scene_with_still", return_value=result) as render:
            row = render_clip_job(self.job, self.original.scene)
        args, options = render.call_args
        self.assertEqual(args, (self.original.scene, self.original.destination, self.still))
        self.assertTrue(options["subdivide"])
        self.assertIs(options["_clip_recovery"], self.job["clip_recovery"])
        self.assertEqual(options["scene_kwargs"], {"seed": 7})
        self.assertEqual(options["_source_identity"]["configuration"], {"frozen": True})
        self.assertEqual(options["animation_range"], [1, 3])
        self.assertEqual(row["status"], "succeeded")

    def test_authored_error_and_interrupt_keep_only_this_jobs_typed_pair(self):
        for error in (ValueError("bad"), KeyboardInterrupt("cancelled"), SystemExit(2)):
            expected = PairedRenderResult(self.original.destination, self.still,
                SubdividedRenderResult(self.original.destination, (), False), None, False)
            error.render_pair_result = expected
            with patch.object(paired_output, "render_scene_with_still", side_effect=error):
                row = render_clip_job(self.job, self.original.scene)
            self.assertEqual(row["result"], expected.as_dict())
            self.assertEqual(row["error"]["type"], type(error).__name__)
            error.render_pair_result = PairedRenderResult(self.original.destination,
                self.still.with_name("foreign.png"), expected.primary, None, False)
            with patch.object(paired_output, "render_scene_with_still", side_effect=error):
                row = render_clip_job(self.job, self.original.scene)
            self.assertIsNone(row["result"])


if __name__ == "__main__":
    unittest.main(verbosity=2)
