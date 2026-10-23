"""Both shipping CLI wrappers to paired recovery, with explicit native fixtures."""
import json
from pathlib import Path
import sys
import unittest
from unittest.mock import patch

import test_parallel_paired_clip_recovery as core
import test_paired_clip_recovery_protocol as owners
from fmn_python import paired_output as actual_pairs
from fmn_python.subdivision_rendering import validate_subdivided_mode
import test_media_job_file_protocol as media


class PairedClipCliTests(unittest.TestCase):
    def setUp(self):
        host = media.MediaJobFileTests("test_all_standard_media_formats_keep_job_names_kwargs_and_spawn_isolation")
        self.addCleanup(host.doCleanups)
        host.setUp()
        self.host, self.state = host, host.root / "clip-state"
        host.h.coordinator._render_source_job = core.paired_worker_fixture
        host.h.native._portal_prepare_recording_scene = lambda: None
        host.h.native._portal_begin_audio_clip = lambda: None
        host.h.native._CameraCapture = owners.CaptureFixture
        restore = patch.dict(sys.modules, {"fmn_python.paired_output": actual_pairs})
        restore.start()
        self.addCleanup(restore.stop)
        host.media.validate_paired_mode = actual_pairs.validate_paired_mode
        host.media.companion_png_path = actual_pairs.companion_png_path
        sys.modules["fmn_python.subdivision_rendering"].validate_subdivided_mode = validate_subdivided_mode
        host.media.validate_subdivided_mode = validate_subdivided_mode

    def invoke(self, *extra, **options):
        return self.host.invoke("--subdivide", "--save-last-frame", "--clip-checkpoint-dir", str(self.state),
            "--resume-key", "paired-inputs", "--fps", "8", *extra, format="y4m", **options)

    def signature(self, path):
        return path.stat().st_mtime_ns, path.read_bytes()

    def test_named_scene_cli_recovers_clips_and_adds_final_png(self):
        host = self.host
        host.behavior(Other="fail")
        self.assertEqual(self.invoke("--keep-going", job_file=False, names=("Lesson", "Other")), 5,
                         host.records[-1])
        first = host.records[-1]["batch"]["outcomes"]
        self.assertTrue(first[0]["result"]["completed"])
        self.assertFalse(first[1]["result"]["completed"])
        self.assertIsNone(first[1]["result"]["still"])
        image = host.output / "Lesson/clips.png"
        clip = host.output / "Other/clips/segment-000000.y4m"
        saved = self.signature(image), self.signature(clip)
        host.behavior()
        self.assertEqual(self.invoke("--resume", job_file=False, names=("Lesson", "Other")), 0,
                         host.records[-1])
        self.assertEqual((self.signature(image), self.signature(clip)), saved)
        self.assertTrue((host.output / "Other/clips.png").is_file())
        self.assertEqual(host.records[-1]["batch"]["reused"], [])
        host.loading.SceneSource.assert_not_called()

    def test_job_file_variants_keep_separate_pair_journals_and_constructor_identity(self):
        host = self.host
        host.behavior(seed11="fail")
        self.assertEqual(self.invoke("--keep-going"), 5, host.records[-1])
        host.behavior()
        self.assertEqual(self.invoke("--resume"), 0, host.records[-1])
        self.assertEqual([row["name"] for row in host.records[-1]["batch"]["outcomes"]], ["seed7", "seed11"])
        for name, value in (("seed7", 7), ("seed11", 11)):
            final = json.loads((self.state / f"jobs/{name}.still.json").read_text())
            self.assertEqual(final["plan"]["clip_plan"]["identity"]["scene_kwargs"], {"value": value})
            self.assertTrue(final["pair"]["completed"])
        host.jobs[0]["scene_kwargs"]["value"] = 99
        host.write_jobs()
        self.assertNotEqual(self.invoke("--resume"), 0)

    def test_write_all_quiet_replays_without_recapture_or_encoder_work(self):
        host = self.host
        self.assertEqual(self.invoke("--write_all", "--quiet", job_file=False), 0, host.records[-1])
        image = host.output / "Lesson/clips.png"
        saved = self.signature(image)
        self.assertEqual(self.invoke("--write_all", "--quiet", "--resume", job_file=False), 0,
                         host.records[-1])
        self.assertEqual(host.stderr.getvalue(), "")
        self.assertEqual(self.signature(image), saved)
        self.assertNotIn("capture", json.loads((host.root / "Lesson.events.json").read_text()))
        self.assertTrue(host.records[-1]["batch"]["ok"])

    def test_unjournaled_companion_is_refused_without_overwrite(self):
        host = self.host
        foreign = host.output / "seed7/clips.png"
        foreign.parent.mkdir(parents=True)
        foreign.write_bytes(b"foreign image")
        self.assertNotEqual(self.invoke(), 0)
        self.assertEqual(foreign.read_bytes(), b"foreign image")
        self.assertFalse((host.root / "seed7.calls").exists())

    def test_existing_still_and_certification_exclusions_remain_before_discovery(self):
        for extra, code in ((("-s",), 2), (("--reproducible",), 4)):
            self.assertEqual(self.invoke(*extra, job_file=False, names=("Lesson", "Other")), code)
            self.host.assert_not_started()

    def test_usage_help_describes_the_new_combination_and_fmtl_stays_refused(self):
        from fmn_python.checkpoint_cli import CHECKPOINT_HELP
        self.assertIn("Completed final", CHECKPOINT_HELP)
        host = self.host
        self.assertEqual(host.invoke("--save-last-frame", "--clip-checkpoint-dir", str(self.state),
            "--resume-key", "inputs", format="fmtl"), 2)
        host.assert_not_started()


if __name__ == "__main__":
    unittest.main(verbosity=2)
