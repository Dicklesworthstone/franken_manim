"""Shipping CLI to process/checkpoint recovery, with explicit native fixtures.

Reuse the shared common-option/discovery fixture, then run the production
coordinator, real spawned interpreters, typed receipts and artifact verifier.
The byte worker does not encode media or import authored scene classes.
"""
from pathlib import Path
import sys
import unittest
from unittest.mock import patch

# Load production receipt owners before the shared CLI fixture substitutes
# unrelated native interfaces. This keeps collection verification real.
import test_subdivision_batch_checkpoint as core
from fmn_python.clip_checkpoint import _clip_inventory
from fmn_python.paired_output import companion_png_path
from fmn_python.subdivision_rendering import validate_subdivided_mode
import test_media_job_file_protocol as media


class SubdivisionBatchCliTests(unittest.TestCase):
    def setUp(self):
        host = media.MediaJobFileTests(
            "test_all_standard_media_formats_keep_job_names_kwargs_and_spawn_isolation")
        self.addCleanup(host.doCleanups)
        host.setUp()
        self.host = host
        host.h.coordinator._render_source_job = core.publish_clip_job
        host.h.native._portal_prepare_recording_scene = lambda: None
        host.h.native._portal_begin_audio_clip = lambda: None
        pairs = sys.modules["fmn_python.paired_output"]
        pairs.companion_png_path = companion_png_path
        clips = sys.modules["fmn_python.subdivision_rendering"]
        clips.validate_subdivided_mode = validate_subdivided_mode
        host.media.validate_subdivided_mode = validate_subdivided_mode
        host.media.companion_png_path = companion_png_path

    def invoke(self, *extra, **options):
        # Every run selects -n 2,5, so its collections are named
        # SCENE_2_5/clips, as the Reference's writer names them (fm-7w26).
        host = self.host
        return host.invoke("--subdivide", "--checkpoint", str(host.checkpoint),
                           "--resume-key", "stable-inputs", "--fps", "8", "-n", "2,5",
                           *extra, format="y4m", **options)

    def attempts(self, name):
        path = self.host.root / (name + ".attempts")
        return [] if not path.exists() else path.read_text().splitlines()

    def test_named_batch_retry_preserves_successes_and_invocation_options(self):
        host = self.host
        host.behavior(Other="fail")
        self.assertEqual(self.invoke("--keep-going", job_file=False, names=("Lesson", "Other")), 5)
        saved = (host.output / "Lesson_2_5/clips/segment-000004.y4m").read_bytes()
        host.behavior()
        self.assertEqual(self.invoke("--resume", job_file=False, names=("Lesson", "Other")), 0)
        report = host.records[-1]["batch"]
        self.assertTrue(report["ok"])
        self.assertEqual(report["reused"], ["Lesson"])
        self.assertEqual([len(self.attempts(name)) for name in ("Lesson", "Other")], [1, 2])
        self.assertEqual((host.output / "Lesson_2_5/clips/segment-000004.y4m").read_bytes(), saved)
        host.loading.SceneSource.assert_not_called()
        arguments = host.process_calls[-1][1]
        self.assertEqual(arguments["animation_range"], (2, 5))
        self.assertEqual(arguments["threads"], 1)
        self.assertTrue(arguments["subdivide"])

    def test_job_file_pairs_resume_without_render_workers(self):
        host = self.host
        host.behavior(seed11="empty")
        self.assertEqual(self.invoke("--save-last-frame"), 0)
        saved = host.records[-1]["batch"]["outcomes"]
        with patch.object(host.h.coordinator, "run_process_jobs",
                          side_effect=AssertionError("completed scenes must not render")):
            self.assertEqual(self.invoke("--save-last-frame", "--resume"), 0)
        self.assertEqual(host.records[-1]["batch"]["reused"], ["seed7", "seed11"])
        self.assertEqual(host.records[-1]["batch"]["outcomes"], saved)
        self.assertTrue((host.output / "seed11_2_5/clips.png").is_file())
        self.assertEqual([len(self.attempts(name)) for name in ("seed7", "seed11")], [1, 1])

    def test_write_all_retry_uses_same_collection_owner(self):
        host = self.host
        host.behavior(Other="fail")
        self.assertEqual(self.invoke("--write_all", "--keep-going", job_file=False), 5)
        host.behavior()
        self.assertEqual(self.invoke("--write_all", "--resume", job_file=False), 0)
        self.assertEqual(host.records[-1]["batch"]["reused"], ["Lesson"])
        self.assertEqual([row["name"] for row in host.records[-1]["batch"]["outcomes"]], ["Lesson", "Other"])

    def test_tampered_saved_clip_refuses_before_retry_admission(self):
        host = self.host
        host.behavior(Other="fail")
        self.assertEqual(self.invoke("--keep-going", job_file=False, names=("Lesson", "Other")), 5)
        (host.output / "Lesson_2_5/clips/segment-000002.y4m").write_bytes(b"modified")
        host.behavior()
        self.assertNotEqual(self.invoke("--resume", job_file=False, names=("Lesson", "Other")), 0)
        self.assertEqual(len(self.attempts("Other")), 1)
        self.assertIn("checkpoint", host.records[-1]["message"])

    def test_partial_collections_still_refuse_no_clobber_on_resume(self):
        host = self.host
        host.behavior(Other="partial")
        self.assertEqual(self.invoke("--keep-going", job_file=False, names=("Lesson", "Other")), 5)
        path = host.output / "Other_2_5/clips/segment-000002.y4m"
        saved = path.read_bytes()
        host.behavior()
        self.assertNotEqual(self.invoke("--resume", job_file=False, names=("Lesson", "Other")), 0)
        self.assertEqual(path.read_bytes(), saved)
        self.assertEqual(len(self.attempts("Other")), 1)

    def test_sequential_recovery_is_refused_before_loading_source(self):
        host = self.host
        self.assertEqual(self.invoke(workers=False, job_file=False, names=("Lesson", "Other")), 2)
        self.assertIn("requires --workers", host.records[-1]["message"])
        host.assert_not_started()

    def test_single_scene_batch_recovery_remains_an_admission_error(self):
        host = self.host
        self.assertEqual(self.invoke(workers=False, job_file=False, names=("Lesson",)), 2)
        host.assert_not_started()

    def test_certified_and_still_only_combinations_refuse_before_discovery(self):
        host = self.host
        self.assertEqual(self.invoke("--reproducible", job_file=False, names=("Lesson", "Other")), 4)
        host.assert_not_started()
        self.assertEqual(self.invoke("-s", job_file=False, names=("Lesson", "Other")), 2)
        host.assert_not_started()


if __name__ == "__main__":
    unittest.main(verbosity=2)
