"""Shipping parser/wrappers to two-level recovery with explicit native fixtures."""
import json
from pathlib import Path
import unittest
from unittest.mock import patch

import test_parallel_paired_clip_cli as paired
import test_two_level_batch_recovery as core
from fmn_python.checkpoint_cli import CHECKPOINT_HELP, take_checkpoint_options


class TwoLevelCliOptionsTests(unittest.TestCase):
    def test_both_path_orders_share_one_key_and_preserve_native_values(self):
        for paths in (("--checkpoint", "done.json", "--clip-checkpoint-dir", "prefix"),
                      ("--clip-checkpoint-dir", "prefix", "--checkpoint", "done.json")):
            with self.subTest(paths=paths):
                args = ["--file_name", "--checkpoint", *paths, "--resume-key", "v1", "--resume"]
                remaining, options = take_checkpoint_options(args, {"--file_name"})
                self.assertEqual(remaining, ["--file_name", "--checkpoint"])
                self.assertEqual(options, dict(checkpoint=Path("done.json").absolute(),
                    clip_checkpoint_dir=Path("prefix").absolute(), resume_key="v1", resume=True))
        self.assertIn("two-level recovery", CHECKPOINT_HELP)
        self.assertIn("module-level side effects", CHECKPOINT_HELP)

    def test_both_modes_still_reject_duplicate_missing_and_invalid_values(self):
        base = ["--checkpoint", "done.json", "--clip-checkpoint-dir", "prefix"]
        for args in (base, base + ["--resume-key", ""],
                     base + ["--resume-key", "x" * 4097],
                     base + ["--resume-key", "v1", "--checkpoint", "other"],
                     base + ["--resume-key", "v1", "--resume", "--resume"],
                     ["--checkpoint", "", "--clip-checkpoint-dir", "prefix", "--resume-key", "v1"],
                     ["--checkpoint", "done", "--clip-checkpoint-dir", "bad\0path", "--resume-key", "v1"]):
            with self.subTest(args=args), self.assertRaises(ValueError):
                take_checkpoint_options(args, frozenset())


class TwoLevelBatchCliTests(unittest.TestCase):
    def setUp(self):
        owner = paired.PairedClipCliTests("test_named_scene_cli_recovers_clips_and_adds_final_png")
        self.addCleanup(owner.doCleanups)
        owner.setUp()
        self.host, self.state = owner.host, owner.state
        self.checkpoint = self.host.root / "completed.json"
        self.host.h.coordinator._render_source_job = core.two_level_worker_fixture

    def invoke(self, *extra, paired_output=False, recovery=True, prefix=True, format="y4m", **options):
        args = ["--subdivide", "--resume-key", "two-level-inputs", "--fps", "8"]
        if recovery:
            args += ["--checkpoint", str(self.checkpoint)]
        if prefix:
            args += ["--clip-checkpoint-dir", str(self.state)]
        if paired_output:
            args += ["--save-last-frame"]
        return self.host.invoke(*args, *extra, format=format, **options)

    def calls(self, name):
        path = self.host.root / (name + ".calls")
        return len(path.read_text().splitlines()) if path.exists() else 0

    def signature(self, path):
        return path.read_bytes(), path.stat().st_mtime_ns

    def test_named_batch_skips_finished_scene_and_recovers_failed_prefix(self):
        h = self.host
        h.behavior(Other="fail")
        names = dict(job_file=False, names=("Lesson", "Other"))
        self.assertEqual(self.invoke("--keep-going", **names), 5, h.records[-1])
        clip = h.output / "Other/clips/segment-000000.y4m"
        saved = self.signature(clip)
        h.behavior()
        self.assertEqual(self.invoke("--resume", **names), 0, h.records[-1])
        report = h.records[-1]["batch"]
        self.assertEqual(report["reused"], ["Lesson"])
        self.assertEqual([row["name"] for row in report["outcomes"]], ["Lesson", "Other"])
        self.assertEqual((self.calls("Lesson"), self.calls("Other")), (1, 2))
        self.assertEqual(self.signature(clip), saved)
        h.loading.SceneSource.assert_not_called()

    def test_job_file_pairs_recover_without_reexecuting_successful_variant(self):
        h = self.host
        h.behavior(seed11="fail")
        self.assertEqual(self.invoke("--keep-going", paired_output=True), 5, h.records[-1])
        paths = [h.output / "seed7/clips.png", h.output / "seed11/clips/segment-000000.y4m"]
        saved = list(map(self.signature, paths))
        h.behavior()
        self.assertEqual(self.invoke("--resume", paired_output=True), 0, h.records[-1])
        self.assertEqual(h.records[-1]["batch"]["reused"], ["seed7"])
        self.assertEqual((self.calls("seed7"), self.calls("seed11")), (1, 2))
        self.assertEqual(list(map(self.signature, paths)), saved)
        final = json.loads((self.state / "jobs/seed11.still.json").read_text())
        self.assertEqual(final["plan"]["clip_plan"]["identity"]["scene_kwargs"], {"value": 11})
        self.assertTrue(final["pair"]["completed"])
        h.jobs[0]["scene_kwargs"]["value"] = 99
        h.write_jobs()
        self.assertNotEqual(self.invoke("--resume", paired_output=True), 0)
        self.assertEqual((self.calls("seed7"), self.calls("seed11")), (1, 2))

    def test_write_all_completed_retry_runs_discovery_but_no_scene_workers(self):
        h = self.host
        self.assertEqual(self.invoke("--write_all", "--quiet", job_file=False), 0, h.records[-1])
        discoveries = len(h.discoveries)
        with patch.object(h.h.coordinator, "run_process_jobs") as workers:
            self.assertEqual(self.invoke("--write_all", "--quiet", "--resume", job_file=False), 0,
                             h.records[-1])
        workers.assert_not_called()
        self.assertEqual(len(h.discoveries), discoveries + 1)
        self.assertEqual(h.records[-1]["batch"]["reused"], ["Lesson", "Other"])
        self.assertEqual((self.calls("Lesson"), self.calls("Other")), (1, 1))
        self.assertEqual(h.stderr.getvalue(), "")
        h.loading.SceneSource.assert_not_called()

    def test_tampered_completed_image_prevents_launching_unfinished_scene(self):
        h = self.host
        h.behavior(seed11="fail")
        self.assertEqual(self.invoke("--keep-going", paired_output=True), 5, h.records[-1])
        image = h.output / "seed7/clips.png"
        image.write_bytes(b"do not replace this modified image")
        h.behavior()
        with patch.object(h.h.coordinator, "run_process_jobs") as workers:
            self.assertNotEqual(self.invoke("--resume", paired_output=True), 0)
        workers.assert_not_called()
        self.assertEqual((self.calls("seed7"), self.calls("seed11")), (1, 1))
        self.assertEqual(image.read_bytes(), b"do not replace this modified image")

    def test_removing_either_mode_refuses_instead_of_changing_replay_semantics(self):
        h = self.host
        self.assertEqual(self.invoke(), 0, h.records[-1])
        for options in ({"recovery": False}, {"prefix": False}):
            with self.subTest(options=options), patch.object(h.h.coordinator, "run_process_jobs") as workers:
                self.assertNotEqual(self.invoke("--resume", **options), 0)
            workers.assert_not_called()
        self.assertEqual((self.calls("seed7"), self.calls("seed11")), (1, 1))

    def test_overlapping_journal_namespace_fails_before_creating_state_or_workers(self):
        self.checkpoint = self.state / "plan.json"
        h = self.host
        with patch.object(h.h.coordinator, "run_process_jobs") as workers:
            self.assertNotEqual(self.invoke(), 0)
        workers.assert_not_called()
        self.assertFalse(self.state.exists())
        self.assertFalse(h.output.exists())

    def test_certification_stills_fmtl_and_sequential_modes_still_refuse_before_discovery(self):
        for extra, options, code in (((), {"format": "fmtl"}, 2),
                (("--reproducible",), {"job_file": False, "names": ("Lesson", "Other")}, 4),
                (("-s",), {}, 2), ((), {"format": "png"}, 2),
                ((), {"workers": False, "job_file": False, "names": ("Lesson", "Other")}, 2)):
            with self.subTest(extra=extra, options=options):
                self.assertEqual(self.invoke(*extra, **options), code, self.host.records[-1])
                self.host.assert_not_started()

    def test_terminal_receipt_loss_replays_pending_pair_but_preserves_finished_scene(self):
        h = self.host
        h.behavior(seed11="terminal-crash")
        self.assertEqual(self.invoke("--keep-going", paired_output=True), 5, h.records[-1])
        image = h.output / "seed11/clips.png"
        saved = self.signature(image)
        h.behavior()
        self.assertEqual(self.invoke("--resume", paired_output=True), 0, h.records[-1])
        self.assertEqual(h.records[-1]["batch"]["reused"], ["seed7"])
        self.assertEqual((self.calls("seed7"), self.calls("seed11")), (1, 2))
        self.assertEqual(self.signature(image), saved)


if __name__ == "__main__":
    unittest.main(verbosity=2)
