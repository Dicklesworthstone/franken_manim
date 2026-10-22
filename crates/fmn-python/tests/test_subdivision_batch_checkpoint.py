"""Completed clip-collection recovery with real files, journals and workers.

Byte fixtures are explicitly NOT encoded media. The production receipt readers,
artifact hashing, locking and process coordinator are exercised; rasterization
and encoding remain native acceptance responsibilities.
"""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from fmn_python.batch_checkpoint import BatchCheckpoint
from fmn_python.batch_rendering import SceneRenderOutcome
from fmn_python.paired_output import PairedRenderResult, companion_png_path
from fmn_python.rendering import RenderResult
from fmn_python.subdivision import RenderSegment, SubdividedRenderResult
from fmn_python import parallel_batch
from fmn_python.subdivision_checkpoint import collection_inventory, collection_receipt


def publication(path, format, *, fps=8, seed=7):
    path.parent.mkdir(parents=True, exist_ok=True)
    payload = b"NOT-MEDIA checkpoint fixture " + path.name.encode()
    if format == "png_sequence":
        path.mkdir()
        for index in range(2):
            (path / f"frame_{index:06d}.png").write_bytes(payload)
        (path / "FMN_COMPLETE").write_bytes(b"fixture marker")
        size = 2 * len(payload)
    else:
        with path.open("xb") as stream:
            stream.write(payload)
        size = len(payload)
    return RenderResult(path, format, (64, 36), fps, 1, "fixture-no-renderer", size,
                        hashlib.sha256(payload).hexdigest(), None if format == "wav" else 1 if format == "png" else 2,
                        12000 if format == "wav" else None, seed)


def collection(root, format="y4m", *, paired=False, empty=False, complete=True):
    root.mkdir(parents=True)
    segments = []
    for index in (() if empty else (2, 4)):
        path = root / (f"segment-{index:06d}" + ("" if format == "png_sequence" else "." + format))
        receipt = publication(path, format)
        segments.append(RenderSegment(index, "play" if index == 2 else "wait", index * 2, index * 2 + 2, receipt))
    result = SubdividedRenderResult(root, tuple(segments), complete)
    if paired:
        still_path = companion_png_path(root, format, subdivide=True)
        result = PairedRenderResult(root, still_path, result,
                                    publication(still_path, "png") if complete else None, complete)
    return result


def publish_clip_job(job):
    """Spawn target replacing only authored/native rendering, not supervision."""
    root, name, options = Path(job["source"]).parent, job["name"], job["options"]
    with (root / (name + ".attempts")).open("a") as stream:
        stream.write(str(os.getpid()) + "\n")
    mode = json.loads((root / "behavior.json").read_text()).get(name, "ok")
    if mode == "fail":
        raise RuntimeError("fixture scene failed before output")
    destination = Path(job["directory"]) / name / "clips"
    result = collection(destination, options["format"], paired=options.get("save_last_frame", False),
                        empty=mode == "empty", complete=mode != "partial")
    if mode == "partial":
        return SceneRenderOutcome(name, destination, "failed", result,
                                  "RuntimeError", "fixture failure after clips").as_dict()
    return SceneRenderOutcome(name, destination, "succeeded", result).as_dict()


class CollectionJournalTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="fmn-collection-journal-"))
        self.destination = self.root / "A" / "clips"
        self.path = self.root / "progress.json"
        self.options = dict(format="y4m", subdivide=True, max_segments=8,
                            resolution=[64, 36], fps=8, animation_range=[2, 5])
        self.result = None

    def save(self, *, paired=False, empty=False, complete=True):
        self.options.update({"save_last_frame": True} if paired else {})
        self.result = collection(self.destination, self.options["format"], paired=paired,
                                 empty=empty, complete=complete)
        with BatchCheckpoint(self.path, resume=False, key="inputs-v1") as owner:
            self.prepare(owner)
            owner.record([SceneRenderOutcome("A", self.destination,
                                             "succeeded" if complete else "failed", self.result)])
        return self.result

    def prepare(self, owner):
        owner.prepare_sources(["A"], [self.destination], self.options, source=self.root / "scene.py",
                              source_digest="a" * 64, working_directory=self.root)

    def resume(self):
        with BatchCheckpoint(self.path, resume=True, key="inputs-v1") as owner:
            self.prepare(owner)
            return owner.completed, owner.artifacts, owner.existing_destinations

    def test_round_trip_all_formats_and_paired_outputs(self):
        for format in ("gif", "y4m", "mp4", "mov", "wav", "png_sequence"):
            for paired in (False, True):
                with self.subTest(format=format, paired=paired):
                    self.setUp()
                    self.options["format"] = format
                    result = self.save(paired=paired)
                    completed, artifacts, destinations = self.resume()
                    self.assertEqual(completed["A"]["result"], result.as_dict())
                    restored = collection_receipt(completed["A"]["result"], self.destination, self.options)
                    self.assertEqual(restored.as_dict(), result.as_dict())
                    self.assertEqual(len(artifacts["A"]["segments"]), 2)
                    expected = {self.destination}
                    if paired:
                        expected.add(self.destination.with_name("clips.png"))
                    self.assertEqual(destinations, expected)

    def test_empty_completed_selections_are_not_incomplete_outputs(self):
        for paired in (False, True):
            with self.subTest(paired=paired):
                self.setUp()
                self.save(paired=paired, empty=True)
                completed, artifacts, _ = self.resume()
                self.assertIn("A", completed)
                self.assertEqual(artifacts["A"]["segments"], [])
                self.assertTrue(artifacts["A"]["directory_present"])

    def test_empty_absent_directory_is_distinguished_from_removed_directory(self):
        empty = SubdividedRenderResult(self.destination, (), True)
        before = collection_inventory(empty.as_dict(), self.destination, self.options)
        self.assertFalse(before["directory_present"])
        self.destination.mkdir(parents=True)
        after = collection_inventory(empty.as_dict(), self.destination, self.options)
        self.assertTrue(after["directory_present"])
        self.assertNotEqual(before, after)

    def test_each_clip_and_final_png_is_verified_before_reuse(self):
        for index in (0, 1, 2):
            with self.subTest(index=index):
                self.setUp()
                result = self.save(paired=True)
                paths = [segment.render.destination for segment in result.primary.segments] + [result.still_destination]
                paths[index].write_bytes(b"altered content")
                with self.assertRaises(ValueError):
                    self.resume()

    def test_missing_clip_and_final_png_refuse(self):
        for still in (False, True):
            with self.subTest(still=still):
                self.setUp()
                result = self.save(paired=True)
                path = result.still_destination if still else result.primary.segments[0].render.destination
                path.rename(self.root / "preserved-missing-artifact")
                with self.assertRaises((ValueError, FileNotFoundError)):
                    self.resume()

    def test_extra_file_and_empty_directory_refuse(self):
        for directory in (False, True):
            with self.subTest(directory=directory):
                self.setUp()
                self.save()
                path = self.destination / "unrecorded"
                path.mkdir() if directory else path.write_bytes(b"other")
                with self.assertRaisesRegex(ValueError, "unrecorded"):
                    self.resume()

    @unittest.skipUnless(hasattr(os, "symlink"), "symlink API required")
    def test_symlink_collection_clip_and_still_refuse(self):
        for which in ("collection", "clip", "still"):
            with self.subTest(which=which):
                self.setUp()
                result = self.save(paired=True)
                path = (self.destination if which == "collection" else result.still_destination
                        if which == "still" else result.primary.segments[0].render.destination)
                moved = self.root / "preserved-original"
                path.rename(moved)
                path.symlink_to(moved, target_is_directory=which == "collection")
                with self.assertRaises(ValueError):
                    self.resume()

    def test_png_inventory_requires_every_frame_and_native_marker(self):
        for name in ("frame_000001.png", "FMN_COMPLETE"):
            with self.subTest(name=name):
                self.setUp()
                self.options["format"] = "png_sequence"
                result = self.save()
                path = result.segments[0].render.destination / name
                path.rename(self.root / "preserved-frame")
                with self.assertRaises(ValueError):
                    self.resume()

    def test_png_contents_and_extra_directory_are_not_hidden_by_aggregate_digest(self):
        for extra in (False, True):
            with self.subTest(extra=extra):
                self.setUp()
                self.options["format"] = "png_sequence"
                result = self.save()
                path = result.segments[0].render.destination
                if extra:
                    (path / "not-a-frame").mkdir()
                else:
                    frame = path / "frame_000000.png"
                    frame.write_bytes(b"x" * frame.stat().st_size)
                with self.assertRaises(ValueError):
                    self.resume()

    def test_plan_changes_refuse_before_reuse(self):
        self.save()
        for key, value in (("fps", 9), ("resolution", [80, 45]), ("max_segments", 9),
                           ("animation_range", [2, 6]), ("save_last_frame", True), ("format", "wav")):
            with self.subTest(key=key), patch.dict(self.options, {key: value}):
                with self.assertRaisesRegex(ValueError, "plan or render options"):
                    self.resume()

    def test_completed_false_and_unreceipted_artifacts_never_become_reusable(self):
        result = collection(self.destination)
        for data in (dict(result.as_dict(), completed=False),
                     dict(result.as_dict(), unreceipted_artifacts=[str(self.destination / "segment-000006.y4m")])):
            with self.assertRaises(ValueError):
                collection_inventory(data, self.destination, self.options)
        self.destination.rename(self.root / "preserved-collection")
        self.save(complete=False)
        completed, artifacts, destinations = self.resume()
        self.assertEqual((completed, artifacts, destinations), ({}, {}, set()))

    def test_receipt_mutations_cannot_redirect_or_relabel_clips(self):
        result = collection(self.destination)
        changes = (("destination", str(self.root / "outside.y4m")), ("format", "mp4"),
                   ("certified", True), ("fps", 9), ("resolution", [80, 45]), ("seed", 8),
                   ("animation_range", [2, 4]), ("bytes", 999), ("digest", "0" * 64))
        for name, value in changes:
            with self.subTest(name=name):
                data = result.as_dict()
                data["segments"][1]["render"][name] = value
                if name == "bytes":
                    data["bytes"] += 999 - result.segments[1].render.bytes
                with self.assertRaises(ValueError):
                    collection_inventory(data, self.destination, self.options)

    def test_invalid_order_selection_and_totals_refuse(self):
        result = collection(self.destination)
        for mutate in (lambda d: d["segments"].reverse(),
                       lambda d: d["segments"][1].update(start_frame=0),
                       lambda d: d.update(frame_count=999),
                       lambda d: d.update(bytes=999),
                       lambda d: d["segments"][0].update(play_index=True)):
            data = result.as_dict()
            mutate(data)
            with self.assertRaises(ValueError):
                collection_inventory(data, self.destination, self.options)
        with patch.dict(self.options, animation_range=[3, 4]):
            with self.assertRaisesRegex(ValueError, "outside"):
                collection_inventory(result.as_dict(), self.destination, self.options)

    def test_selection_end_is_exclusive_even_when_clip_path_and_digest_are_valid(self):
        result = collection(self.destination)
        with patch.dict(self.options, animation_range=[2, 4]):
            with self.assertRaisesRegex(ValueError, "outside"):
                collection_inventory(result.as_dict(), self.destination, self.options)
        with patch.dict(self.options, animation_range=[2, None]):
            self.assertEqual(len(collection_inventory(result.as_dict(), self.destination,
                                                     self.options)["segments"]), 2)

    def test_segment_and_combined_file_budgets(self):
        result = collection(self.destination, paired=True)
        self.options["save_last_frame"] = True
        with patch.dict(self.options, max_segments=1):
            with self.assertRaisesRegex(ValueError, "segment budget"):
                collection_inventory(result.as_dict(), self.destination, self.options)
        with patch("fmn_python.subdivision_checkpoint._MAX_FILES", 2):
            with self.assertRaisesRegex(ValueError, "combined file budget"):
                collection_inventory(result.as_dict(), self.destination, self.options)

    def test_bad_publication_does_not_replace_journal(self):
        self.result = collection(self.destination)
        with BatchCheckpoint(self.path, resume=False, key="inputs-v1") as owner:
            self.prepare(owner)
            owner.record([SceneRenderOutcome("A", self.destination, "not_run")])
            before = self.path.read_bytes()
            self.result.segments[0].render.destination.write_bytes(b"changed")
            with self.assertRaises(ValueError):
                owner.record([SceneRenderOutcome("A", self.destination, "succeeded", self.result)])
            self.assertEqual(self.path.read_bytes(), before)

    def test_empty_directory_disappearance_is_detected(self):
        self.save(empty=True)
        self.destination.rename(self.root / "preserved-empty")
        with self.assertRaisesRegex(ValueError, "missing or modified"):
            self.resume()

    def test_single_artifact_journals_keep_their_existing_inventory(self):
        path = self.root / "plain.y4m"
        receipt = publication(path, "y4m")
        options = dict(format="y4m")
        with BatchCheckpoint(self.path, resume=False, key="inputs-v1") as owner:
            owner.prepare_sources(["A"], [path], options, source=self.root / "scene.py",
                                  source_digest="a" * 64, working_directory=self.root)
            owner.record([SceneRenderOutcome("A", path, "succeeded", receipt)])
        saved = json.loads(self.path.read_text())
        self.assertIsInstance(saved["artifacts"]["A"], list)
        self.assertEqual(saved["version"], 2)


class ParallelCollectionRecoveryTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="fmn-parallel-collections-"))
        self.source = self.root / "scenes.py"
        self.source.write_text("# unchanged entry source for the explicit byte worker\n")
        self.output, self.journal = self.root / "output", self.root / "progress.json"
        self.behavior({})
        self.native = SimpleNamespace(_portal_prepare_recording_scene=lambda: None,
                                      _portal_begin_audio_clip=lambda: None,
                                      _CameraCapture=SimpleNamespace(prepare_png=lambda: None))

    def behavior(self, values):
        (self.root / "behavior.json").write_text(json.dumps(values))

    def run_batch(self, **options):
        settings = dict(workers=1, format="y4m", resolution=(64, 36), fps=8, subdivide=True,
                        max_segments=8, animation_range=(2, 5), checkpoint=self.journal,
                        resume_key="inputs-v1", continue_on_error=True)
        settings.update(options)
        with patch.dict(sys.modules, manimlib=self.native), patch.object(
                parallel_batch, "_render_source_job", publish_clip_job):
            return parallel_batch.render_source_scenes(self.source, self.output, ["A", "B", "C"], **settings)

    def attempts(self, name):
        path = self.root / (name + ".attempts")
        return [] if not path.exists() else path.read_text().splitlines()

    def test_retry_only_failed_scenes_and_preserve_order(self):
        self.behavior({"B": "fail"})
        first = self.run_batch(workers=2)
        self.assertEqual([row.status for row in first.outcomes], ["succeeded", "failed", "succeeded"])
        self.behavior({})
        second = self.run_batch(resume=True, workers=1, job_timeout=30)
        self.assertTrue(second.ok)
        self.assertEqual(second.reused, ("A", "C"))
        self.assertEqual([len(self.attempts(name)) for name in ("A", "B", "C")], [1, 2, 1])
        self.assertEqual([row.name for row in second.outcomes], ["A", "B", "C"])
        self.assertTrue(all(int(pid) != os.getpid() for name in ("A", "B", "C") for pid in self.attempts(name)))

    def test_completed_pairs_and_empty_collections_resume_without_workers(self):
        self.behavior({"B": "empty"})
        first = self.run_batch(save_last_frame=True)
        self.assertTrue(first.ok)
        with patch.object(parallel_batch, "run_process_jobs", side_effect=AssertionError("no worker expected")):
            second = self.run_batch(resume=True, save_last_frame=True)
        self.assertEqual(second.reused, ("A", "B", "C"))
        self.assertEqual([row.result.as_dict() for row in second.outcomes], [row.result.as_dict() for row in first.outcomes])

    def test_observer_failure_leaves_completed_scene_recoverable(self):
        def observer(row):
            document = json.loads(self.journal.read_text())
            self.assertEqual(document["outcomes"][0]["status"], "succeeded")
            raise RuntimeError("observer stop")
        with self.assertRaisesRegex(RuntimeError, "observer stop"):
            self.run_batch(on_result=observer)
        report = self.run_batch(resume=True)
        self.assertTrue(report.ok)
        self.assertEqual(report.reused, ("A",))
        self.assertEqual([len(self.attempts(n)) for n in ("A", "B", "C")], [1, 1, 1])

    def test_all_saved_artifacts_are_verified_before_any_retry(self):
        self.behavior({"B": "fail"})
        self.run_batch()
        path = self.output / "C" / "clips" / "segment-000004.y4m"
        path.write_bytes(b"tampered")
        self.behavior({})
        with self.assertRaises(ValueError):
            self.run_batch(resume=True)
        self.assertEqual(len(self.attempts("B")), 1)

    def test_partial_outputs_are_retained_but_never_adopted(self):
        self.behavior({"B": "partial"})
        report = self.run_batch()
        self.assertFalse(report.outcomes[1].result.completed)
        path = self.output / "B" / "clips" / "segment-000002.y4m"
        before = path.read_bytes()
        self.behavior({})
        with self.assertRaises(FileExistsError):
            self.run_batch(resume=True)
        self.assertEqual(path.read_bytes(), before)
        self.assertEqual(len(self.attempts("B")), 1)

    def test_changed_source_is_refused_before_admission(self):
        self.behavior({"B": "fail"})
        self.run_batch()
        self.source.write_text("# changed source\n")
        with self.assertRaisesRegex(ValueError, "plan or render options"):
            self.run_batch(resume=True)
        self.assertEqual(len(self.attempts("B")), 1)


if __name__ == "__main__":
    unittest.main(verbosity=2)
