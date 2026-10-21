"""JSON parameter batches through the complete shipping FMTL CLI wrapper.

Processes, worker dispatch, receipts and checkpoint I/O are real. Source
inventory, common native option parsing and capture are explicit fixtures.
"""
from contextlib import contextmanager
import json
import os
from pathlib import Path
import unittest
from unittest.mock import patch

import test_parallel_bundle_cli_protocol as prior_cli
from test_source_job_variants_protocol import SOURCE, execute_variant
from fmn_python import source_job_file as job_file


class SourceJobFileTests(unittest.TestCase):
    parse = prior_cli.ParallelBundleCliTests.parse
    emit = prior_cli.ParallelBundleCliTests.emit
    discover = prior_cli.ParallelBundleCliTests.discover
    behavior = prior_cli.ParallelBundleCliTests.behavior
    invoke = prior_cli.ParallelBundleCliTests.invoke

    def setUp(self):
        prior_cli.ParallelBundleCliTests.setUp(self)
        self.source.write_text(SOURCE)
        self.names = ("Lesson", "Other")
        self.h.coordinator._render_source_job = execute_variant
        self.job_path = self.root / "jobs.json"
        self.rows = [{"name": "seed7", "scene": "Lesson", "scene_kwargs": {"value": 7}},
                     {"name": "seed11", "scene": "Lesson", "scene_kwargs": {"value": 11}}]
        self.write_jobs(self.rows)

    def write_jobs(self, rows):
        self.document = {"schema": "fmn-python.source-jobs", "version": 1, "jobs": rows}
        self.job_path.write_text(json.dumps(self.document))

    def run_jobs(self, *extra, **options):
        return self.invoke("--jobs-file", str(self.job_path), *extra, names=(), **options)

    def values(self, name):
        return json.loads((self.output / (name + ".fmtl")).read_bytes().removeprefix(b"NOT-FMTL:"))

    def test_cli_executes_named_variants_of_same_class_in_manifest_order(self):
        self.assertEqual(self.run_jobs("--bundle-camera", "--bundle-audio", "--quiet"), 0)
        self.loading.SceneSource.assert_not_called()
        self.assertEqual([row["name"] for row in self.records[-1]["batch"]["outcomes"]], ["seed7", "seed11"])
        self.assertEqual([self.values(name)["value"] for name in ("seed7", "seed11")], [7, 11])
        self.assertEqual(self.stderr.getvalue(), "")
        self.assertTrue(self.records[-1]["robot"])
        self.assertTrue(all("--jobs-file" not in token for token in self.parses[0]))
        self.assertTrue(all(job.scene == "Lesson" for job in self.process_calls[0][1]["scene_names"]))

    def test_manifest_resume_tolerates_formatting_but_rejects_constructor_change(self):
        options = ("--checkpoint", str(self.checkpoint), "--resume-key", "assets-v1")
        self.assertEqual(self.run_jobs(*options), 0)
        before = (self.root / "imports").read_text()
        self.job_path.write_text(json.dumps(self.document, indent=4, sort_keys=True))
        self.assertEqual(self.run_jobs(*options, "--resume"), 0)
        self.assertEqual(self.records[-1]["batch"]["reused"], ["seed7", "seed11"])
        self.assertEqual((self.root / "imports").read_text(), before)
        saved = self.checkpoint.read_bytes()
        self.rows[1]["scene_kwargs"]["value"] = 12
        self.write_jobs(self.rows)
        self.assertEqual(self.run_jobs(*options, "--resume"), 2)
        self.assertEqual(self.checkpoint.read_bytes(), saved)
        self.assertEqual((self.root / "imports").read_text(), before)

    def test_single_manifest_job_still_uses_an_isolated_batch(self):
        self.write_jobs(self.rows[:1])
        self.assertEqual(self.invoke("--jobs-file=" + str(self.job_path), names=()), 0)
        self.assertEqual(self.records[-1]["kind"], "render-batch")
        self.assertEqual(self.records[-1]["batch"]["execution"], "spawn")
        self.assertEqual(self.values("seed7")["value"], 7)

    def test_selection_conflicts_and_missing_workers_fail_before_discovery(self):
        for extra in (("--write_all",), ("--output", str(self.root / "exact.fmtl")),
                      ("--file_name", "custom"), ("--jobs-file", str(self.job_path))):
            with self.subTest(extra=extra):
                self.assertEqual(self.run_jobs(*extra), 2)
        self.assertEqual(self.run_jobs(workers=False), 2)
        self.assertEqual(self.invoke("--jobs-file", str(self.job_path), names=("Lesson",)), 2)
        self.assertEqual(self.discoveries, [])
        self.assertEqual(self.process_calls, [])
        self.assertFalse(self.output.exists())

    def test_missing_file_value_preserves_robot_error_and_help_does_not_read(self):
        self.assertEqual(self.invoke("--jobs-file", "--robot", names=(), robot=False), 2)
        self.assertTrue(self.records[-1]["robot"])
        self.assertEqual(self.invoke("--jobs-file", str(self.root / "absent"), "--help", names=()), 0)
        self.assertIn("fmn-python.source-jobs", self.records[-1]["help"])
        self.assertEqual(self.discoveries, [])

    def test_option_looking_value_in_another_flag_does_not_select_jobs_file(self):
        self.names = ("Alpha", "Beta")
        # The ordinary path uses a byte writer here, as in its original suite.
        from test_parallel_bundle_protocol import publish_bundle_job
        self.h.coordinator._render_source_job = publish_bundle_job
        self.assertEqual(self.invoke("--config_file", "--jobs-file"), 0)
        self.assertEqual(self.process_calls[0][1]["scene_names"], ["Alpha", "Beta"])

    def test_all_manifest_classes_are_checked_against_discovery_before_any_render(self):
        self.rows[1]["scene"] = "Absent"
        self.write_jobs(self.rows)
        self.assertEqual(self.run_jobs(), 2)
        self.assertEqual(len(self.discoveries), 1)
        self.assertEqual(self.process_calls, [])
        self.assertFalse((self.root / "imports").exists())

    def test_job_file_change_after_admission_cannot_modify_frozen_kwargs(self):
        original = self.discover
        @contextmanager
        def discover(*args, **kwargs):
            self.rows[0]["scene_kwargs"]["value"] = 99
            self.write_jobs(self.rows)
            with original(*args, **kwargs) as loaded:
                yield loaded
        with patch("fmn_python.parallel_discovery.discovery_source", discover):
            self.assertEqual(self.run_jobs(), 0)
        self.assertEqual(self.values("seed7")["value"], 7)

    def test_keep_going_retains_variant_failures_and_successes(self):
        self.rows[0]["scene_kwargs"]["fail"] = True
        self.write_jobs(self.rows)
        self.assertEqual(self.run_jobs("--keep-going"), 5)
        self.assertEqual([row["status"] for row in self.records[-1]["batch"]["outcomes"]],
                         ["failed", "succeeded"])
        self.assertEqual(self.values("seed11")["value"], 11)

    def test_document_shape_versions_and_unknown_fields_are_strict(self):
        invalid = [[], {}, {**self.document, "version": True}, {**self.document, "version": 2},
                   {**self.document, "schema": "other"}, {**self.document, "unused": 1},
                   {**self.document, "jobs": []}, {**self.document, "jobs": {}},
                   {**self.document, "jobs": [{"name": "a", "scene": "Lesson", "kwargs": {}}]},
                   {**self.document, "jobs": [{"name": "a"}]}]
        for document in invalid:
            with self.subTest(document=document):
                self.job_path.write_text(json.dumps(document))
                self.assertEqual(self.run_jobs(), 2)
        self.assertEqual(self.discoveries, [])
        self.assertFalse(self.output.exists())

    def test_duplicate_keys_nonfinite_numbers_and_invalid_utf8_are_refused(self):
        for raw in (b'{"schema":"fmn-python.source-jobs","version":1,"version":1,"jobs":[]}',
                    b'{"schema":"fmn-python.source-jobs","version":1,"jobs":[{"name":"a","scene":"Lesson","scene_kwargs":{"x":1,"x":2}}]}',
                    b'{"schema":"fmn-python.source-jobs","version":1,"jobs":[{"name":"a","scene":"Lesson","scene_kwargs":{"x":NaN}}]}',
                    b'{"schema":"fmn-python.source-jobs","version":1,"jobs":[{"name":"a","scene":"Lesson","scene_kwargs":{"x":1e999}}]}',
                    b'\xff\xfe\0'):
            with self.subTest(raw=raw[:30]):
                self.job_path.write_bytes(raw)
                self.assertEqual(self.run_jobs(), 2)
        self.assertEqual(self.discoveries, [])

    def test_symlinks_and_special_files_are_refused_without_blocking(self):
        self.job_path.unlink()
        real = self.root / "real.json"
        real.write_text(json.dumps(self.document))
        self.job_path.symlink_to(real)
        self.assertEqual(self.run_jobs(), 2)
        self.job_path.unlink()
        if hasattr(os, "mkfifo"):
            os.mkfifo(self.job_path)
            self.assertEqual(self.run_jobs(), 2)
        self.assertEqual(self.discoveries, [])

    def test_document_byte_depth_and_job_count_budgets_precede_discovery(self):
        with self.job_path.open("wb") as stream:
            stream.truncate(job_file.MAX_VARIANT_PLAN_BYTES + 1)
        self.assertEqual(self.run_jobs(), 2)
        self.job_path.write_text("[" * 2000 + "]" * 2000)
        self.assertEqual(self.run_jobs(), 2)
        self.write_jobs([{"name": f"a{i}", "scene": "Lesson"} for i in range(1025)])
        self.assertEqual(self.run_jobs(), 2)
        self.assertEqual(self.discoveries, [])

    def test_file_change_during_read_is_refused(self):
        original = job_file._open_regular
        @contextmanager
        def changing(path):
            with original(path) as stream:
                class Stream:
                    fileno = stream.fileno
                    def read(self, limit):
                        data = stream.read(limit)
                        path.write_bytes(data + b" ")
                        return data
                yield Stream()
        with patch.object(job_file, "_open_regular", changing):
            self.assertEqual(self.run_jobs(), 2)
        self.assertIn("changed while reading", self.records[-1]["message"])
        self.assertEqual(self.discoveries, [])

    def test_optional_kwargs_are_owned_and_types_remain_distinct(self):
        self.write_jobs([{"name": "a", "scene": "Lesson"},
                         {"name": "b", "scene": "Lesson", "scene_kwargs": {"values": [True, 1, 1.0, -0.0]}}])
        jobs = job_file.load_source_jobs(self.job_path)
        self.assertEqual(jobs[0].scene_kwargs, {})
        values = jobs[1].scene_kwargs["values"]
        self.assertEqual([type(x) for x in values], [bool, int, float, float])
        self.assertEqual(json.dumps(values), "[true, 1, 1.0, -0.0]")
        with self.assertRaises((TypeError, ValueError)):
            job_file.load_source_jobs(self.job_path, max_jobs=True)

    def test_missing_file_has_error_receipt_and_never_imports_source(self):
        self.job_path.unlink()
        self.assertEqual(self.run_jobs(), 2)
        self.assertTrue(self.records[-1]["robot"])
        self.assertEqual(self.records[-1]["phase"], "options")
        self.assertEqual(self.discoveries, [])


if __name__ == "__main__":
    unittest.main()
