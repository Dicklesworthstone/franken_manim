"""Shipping CLI to partial-scene recovery, with explicit native byte fixtures."""
from pathlib import Path
import sys
import unittest
from unittest.mock import patch

# Load production collection readers before the shared native-facing fixtures.
import test_parallel_clip_recovery as core
import test_subdivision_batch_checkpoint
from fmn_python.checkpoint_cli import CHECKPOINT_VALUES, take_checkpoint_options
from fmn_python.subdivision_rendering import validate_subdivided_mode
import test_media_job_file_protocol as media


class ClipRecoveryOptionsTests(unittest.TestCase):
    def test_explicit_modes_share_resume_syntax_without_changing_old_mode(self):
        for flag, key in (("--checkpoint", "checkpoint"), ("--clip-checkpoint-dir", "clip_checkpoint_dir")):
            with self.subTest(flag=flag):
                remaining, options = take_checkpoint_options(["--format", "y4m", flag, "state", "--resume-key", "v1", "--resume"], {"--format"})
                self.assertEqual(remaining, ["--format", "y4m"])
                self.assertEqual(options, {key: Path("state").absolute(), "resume_key": "v1", "resume": True})

    def test_missing_duplicate_and_conflicting_recovery_options_refuse(self):
        for args in (["--clip-checkpoint-dir"], ["--clip-checkpoint-dir", "state"],
                     ["--resume-key", "v1"], ["--resume"],
                     ["--clip-checkpoint-dir", "state", "--checkpoint", "batch.json", "--resume-key", "v1"],
                     ["--clip-checkpoint-dir", "state", "--clip-checkpoint-dir", "more", "--resume-key", "v1"],
                     ["--clip-checkpoint-dir", "", "--resume-key", "v1"],
                     ["--clip-checkpoint-dir", "state", "--resume-key", ""]):
            with self.subTest(args=args), self.assertRaises(ValueError):
                take_checkpoint_options(args, frozenset())

    def test_values_that_resemble_the_new_switch_are_not_consumed(self):
        args = ["--file_name", "--clip-checkpoint-dir", "--format", "y4m"]
        self.assertEqual(take_checkpoint_options(args, {"--file_name", "--format"}), (args, {}))
        self.assertIn("--clip-checkpoint-dir", CHECKPOINT_VALUES)


class ParallelClipCliTests(unittest.TestCase):
    def setUp(self):
        host = media.MediaJobFileTests("test_all_standard_media_formats_keep_job_names_kwargs_and_spawn_isolation")
        self.addCleanup(host.doCleanups)
        host.setUp()
        self.host = host
        self.state = host.root / "clip-state"
        host.h.coordinator._render_source_job = core.byte_clip_job
        host.h.native._portal_prepare_recording_scene = lambda: None
        host.h.native._portal_begin_audio_clip = lambda: None
        clips = sys.modules["fmn_python.subdivision_rendering"]
        clips.validate_subdivided_mode = validate_subdivided_mode
        host.media.validate_subdivided_mode = validate_subdivided_mode

    def invoke(self, *extra, subdivide=True, **options):
        return self.host.invoke(*(["--subdivide"] if subdivide else []),
            "--clip-checkpoint-dir", str(self.state), "--resume-key", "stable-inputs",
            "--fps", "8", *extra, format="y4m", **options)

    def test_named_scenes_resume_partial_jobs_and_preserve_source_isolation(self):
        host = self.host
        host.behavior(Other="fail")
        self.assertEqual(self.invoke("--keep-going", job_file=False, names=("Lesson", "Other")), 5)
        path = host.output / "Other/clips/segment-000000.y4m"
        saved = path.stat().st_mtime_ns, path.read_bytes()
        host.behavior()
        self.assertEqual(self.invoke("--resume", job_file=False, names=("Lesson", "Other")), 0)
        report = host.records[-1]["batch"]
        self.assertTrue(report["ok"])
        self.assertEqual(report["reused"], [])
        self.assertEqual(len(report["outcomes"][1]["result"]["segments"]), 3)
        self.assertEqual((path.stat().st_mtime_ns, path.read_bytes()), saved)
        host.loading.SceneSource.assert_not_called()

    def test_job_file_variants_resume_distinct_partial_prefixes(self):
        host = self.host
        host.behavior(seed11="fail")
        self.assertEqual(self.invoke("--keep-going"), 5)
        host.behavior()
        self.assertEqual(self.invoke("--resume"), 0)
        self.assertEqual([row["name"] for row in host.records[-1]["batch"]["outcomes"]], ["seed7", "seed11"])
        self.assertTrue((self.state / "jobs/seed7.json").is_file())
        self.assertTrue((self.state / "jobs/seed11.json").is_file())
        self.assertEqual(host.process_calls[-1][1]["scene_names"][1].scene_kwargs, {"value": 11})

    def test_write_all_and_quiet_keep_one_machine_readable_result(self):
        self.assertEqual(self.invoke("--write_all", "--quiet", job_file=False), 0)
        self.assertEqual(self.host.stderr.getvalue(), "")
        self.assertEqual(self.invoke("--write_all", "--quiet", "--resume", job_file=False), 0)
        self.assertEqual([row["status"] for row in self.host.records[-1]["batch"]["outcomes"]],
                         ["succeeded", "succeeded"])

    def test_conflicting_modes_refuse_before_source_discovery(self):
        for extra, options in (((), {"subdivide": False}),
                               (("--checkpoint", "other.json"), {}), (("-s",), {}),
                               ((), {"workers": False, "job_file": False, "names": ("Lesson", "Other")})):
            with self.subTest(extra=extra, options=options):
                self.assertEqual(self.invoke(*extra, **options), 2)
                self.host.assert_not_started()
        self.assertFalse(self.state.exists())

    def test_certification_is_refused_before_source_discovery(self):
        self.assertEqual(self.invoke("--reproducible", job_file=False, names=("Lesson", "Other")), 4)
        self.host.assert_not_started()

    def test_fmtl_dispatch_refuses_prefix_option_before_source_discovery(self):
        host = self.host
        result = host.invoke("--clip-checkpoint-dir", str(self.state), "--resume-key", "v1", format="fmtl")
        self.assertEqual(result, 2)
        self.assertIn("FMTL", host.records[-1]["message"])
        host.assert_not_started()

    def test_existing_foreign_clip_is_never_silently_adopted(self):
        host = self.host
        destination = host.output / "seed7/clips"
        destination.mkdir(parents=True)
        foreign = destination / "segment-000000.y4m"
        foreign.write_bytes(b"foreign output")
        self.assertNotEqual(self.invoke(), 0)
        self.assertEqual(foreign.read_bytes(), b"foreign output")
        self.assertFalse((host.root / "seed7.calls").exists())


if __name__ == "__main__":
    unittest.main(verbosity=2)
