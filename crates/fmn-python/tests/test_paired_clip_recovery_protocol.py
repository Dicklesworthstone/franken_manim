"""Real paired-session and journal owners with explicit native-byte fixtures.

The clip scene and immutable camera capture below do not render pixels. They
exercise production ownership, receipt validation, hashing, locks and atomic
journal writes, including failures between independent publications. Native
wheel acceptance remains a separate suite; NOT-MEDIA is not a PNG/clip proof.
"""
from __future__ import annotations

from dataclasses import replace
import hashlib
import json
import os
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "python"))
from fmn_python import rendering
from fmn_python._process_batch import positive_integer
from fmn_python.batch_checkpoint import _CheckpointFile, _json
from fmn_python.paired_clip_recovery import (
    FinalImageCheckpoint, PairedClipRenderSession, final_checkpoint_path, prepare_paired_recovery,
)
from fmn_python.paired_output import PairedRenderSession
from fmn_python.parallel_output import restore_output
from fmn_python.rendering import RenderResult
from fmn_python.subdivision import RenderSegment, SubdividedRenderResult
from fmn_python.subdivision_checkpoint import collection_inventory


class ClipJournalFixture(_CheckpointFile):
    schema = "fmn-python.clip-checkpoint"
    version = 1


class PendingImageFixture:
    def __init__(self, camera, destination):
        self.camera, self.destination = camera, Path(destination)
        self.aborted = False

    def abort(self):
        self.aborted = True
        self.camera.events.append("abort-png")

    def commit(self):
        self.camera.events.append("commit-png")
        if self.camera.on_commit is not None:
            self.camera.on_commit(self)
        if self.aborted:
            raise RuntimeError("fixture PNG was cancelled")
        data = b"NOT-MEDIA final image fixture"
        with self.destination.open("xb") as stream:
            stream.write(data)
        return str(self.destination), len(data), hashlib.sha256(data).hexdigest()


class CaptureFixture:
    def __init__(self, camera):
        self.camera, self.size = camera, (64, 36)

    def prepare_png(self, destination, *, threads):
        self.camera.events.append("prepare-png")
        pending = PendingImageFixture(self.camera, destination)
        self.camera.pending = pending
        return pending


class CameraFixture:
    capture_threads = 1

    def __init__(self, events):
        self.events, self.on_capture, self.on_commit = events, None, None
        self.pending = None

    def capture_snapshot(self, *mobjects):
        self.events.append("capture")
        if self.on_capture is not None:
            self.on_capture()
        return CaptureFixture(self)


class ClipPrimaryFixture:
    """Fixture only for native playback; the real paired owner is not replaced."""
    def __init__(self, root, *, resume=False, frames=3, options=None, destination=None, checkpoint=None, key="v1"):

        self.root, self.destination = Path(root), Path(destination) if destination is not None else Path(root) / "out/A/clips"
        self.events, self.encoded, self.replayed = [], [], 0
        self.frames, self.fail_finish, self.result = frames, None, None
        self._configuration = SimpleNamespace(format="y4m", reproducible=False, resolution=(64, 36),
            fps=8, threads=1, seed=7, animation_range=None)
        self.scene = SimpleNamespace(file_writer=SimpleNamespace(png_mode="RGBA"), mobjects=[])
        self.scene.camera = CameraFixture(self.events)
        self._native = SimpleNamespace(_CameraCapture=CaptureFixture, _CapabilityError=RuntimeError)
        self._journal = ClipJournalFixture(checkpoint or self.root / "state/jobs/A.json", resume=resume, key=key)
        self.plan = {"destination": str(self.destination), "identity": {"fixture": "stable-source"},
                     "options": dict(format="y4m", resolution=[64, 36], fps=8, seed=7,
                         threads=1, animation_range=None, max_segments=10)}
        self.plan["options"].update(options or {})
        self.saved = []

    def _check_owner(self):
        pass

    def __enter__(self):
        journal = self._journal
        journal.__enter__()
        try:
            document = journal.document
            journal.plan = self.plan
            if document is not None:
                if _json(document["plan"]) != _json(self.plan):
                    raise ValueError("fixture source plan changed")
                receipt = restore_output(document["collection"], destination=self.destination, format="y4m",
                                         subdivide=True, save_last_frame=False, max_segments=10)
                self.saved = list(receipt.segments)
                journal.complete = receipt.completed
                collection_inventory(replace(receipt, completed=True).as_dict(), self.destination,
                                     dict(self.plan["options"], subdivide=True))
            else:
                self.destination.mkdir(parents=True, exist_ok=False)
                journal.complete = False
            journal.segments = list(self.saved)
            self._save(journal.complete)
            self.events.append("enter-clips")
            return self
        except BaseException:
            journal.__exit__(None, None, None)
            raise

    def _save(self, completed):
        self._journal._publish(_json({"schema": self._journal.schema, "version": 1, "key": self._journal.key,
            "plan": self.plan, "collection": SubdividedRenderResult(
                self.destination, tuple(self._journal.segments), completed).as_dict()}))
        self._journal.complete = completed

    def play(self):
        index = self.replayed
        if index == len(self._journal.segments):
            data = b"NOT-MEDIA clip " + str(index).encode()
            path = self.destination / f"segment-{index:06d}.y4m"
            with path.open("xb") as stream:
                stream.write(data)
            receipt = RenderResult(path, "y4m", (64, 36), 8, 1, "byte-fixture", len(data),
                                   hashlib.sha256(data).hexdigest(), 1, None, 7)
            self._journal.segments.append(RenderSegment(index, "wait", index, index + 1, receipt))
            self._save(False)
            self.encoded.append(index)
        self.replayed += 1

    def run(self):
        for _ in range(self.frames):
            self.play()

    @property
    def partial_result(self):
        return SubdividedRenderResult(self.destination, tuple(self._journal.segments), False)

    @property
    def artifact_published(self):
        return bool(self._journal.segments)

    def finish(self):
        self.events.append("finish-clips")
        if self.fail_finish is not None:
            raise self.fail_finish
        if self.replayed != len(self._journal.segments):
            raise ValueError("fixture did not replay the complete prefix")
        self._save(True)
        self.result = SubdividedRenderResult(self.destination, tuple(self._journal.segments), True)
        self._journal.__exit__(None, None, None)
        return self.result

    def abort(self):
        self.events.append("abort-clips")
        self._journal.__exit__(None, None, None)


def fixture_session(root, *, resume=False, frames=3):
    primary = ClipPrimaryFixture(root, resume=resume, frames=frames)
    path = final_checkpoint_path(primary._journal.path)
    pair = PairedClipRenderSession(primary, primary.destination.with_name("clips.png"),
                                  checkpoint=path, resume=resume and path.exists())
    return primary, pair


class PairedClipRecoveryTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="fmn-paired-prefix-"))
        self.patch = patch.object(rendering, "_positive_integer", positive_integer, create=True)
        self.patch.start()
        self.addCleanup(self.patch.stop)

    def complete(self, *, frames=3):
        primary, pair = fixture_session(self.root, frames=frames)
        with pair:
            primary.run()
        self.assert_released(primary, pair)
        return primary, pair

    def assert_released(self, primary, pair):
        self.assertIsNone(primary._journal.lock)
        self.assertIsNone(pair._image_journal.lock)
        self.assertNotIn("_fmn_paired_render_session", vars(primary.scene))

    def test_complete_pair_resume_preserves_every_byte_and_avoids_capture(self):
        original, pair = self.complete()
        paths = [clip.render.destination for clip in pair.result.primary.segments] + [pair.still_destination]
        before = [(path.read_bytes(), path.stat().st_mtime_ns) for path in paths]
        primary, resumed = fixture_session(self.root, resume=True)
        with resumed:
            primary.run()
        self.assertEqual(resumed.result.as_dict(), pair.result.as_dict())
        self.assertEqual(primary.encoded, [])
        self.assertNotIn("capture", primary.events)
        self.assertEqual(before, [(path.read_bytes(), path.stat().st_mtime_ns) for path in paths])
        self.assertEqual(original.events[:5], ["enter-clips", "capture", "prepare-png", "finish-clips", "commit-png"])
        self.assert_released(primary, resumed)

    def test_partial_scene_reuses_prefix_then_publishes_final_image(self):
        primary, pair = fixture_session(self.root)
        with self.assertRaisesRegex(RuntimeError, "authored") as caught:
            with pair:
                primary.play()
                raise RuntimeError("authored failure")
        self.assertFalse(caught.exception.render_pair_result.completed)
        self.assertEqual(len(caught.exception.render_pair_result.primary.segments), 1)
        self.assertFalse(pair.still_destination.exists())
        self.assert_released(primary, pair)
        primary, resumed = fixture_session(self.root, resume=True)
        with resumed:
            primary.run()
        self.assertEqual(primary.encoded, [1, 2])
        self.assertTrue(resumed.result.completed)
        self.assert_released(primary, resumed)

    def test_failed_capture_preserves_clips_and_retries_image(self):
        primary, pair = fixture_session(self.root)
        def refuse():
            raise RuntimeError("capture refused")
        primary.scene.camera.on_capture = refuse
        with self.assertRaisesRegex(RuntimeError, "capture refused"):
            with pair:
                primary.run()
        self.assertNotIn("finish-clips", primary.events)
        self.assert_released(primary, pair)
        primary, resumed = fixture_session(self.root, resume=True)
        with resumed:
            primary.run()
        self.assertEqual(primary.encoded, [])
        self.assertTrue(resumed.result.completed)
        self.assertIn("capture", primary.events)

    def test_primary_finish_failure_aborts_prepared_png(self):
        primary, pair = fixture_session(self.root)
        primary.fail_finish = RuntimeError("primary finalization refused")
        with self.assertRaisesRegex(RuntimeError, "primary finalization refused"):
            with pair:
                primary.run()
        self.assertTrue(primary.scene.camera.pending.aborted)
        self.assertFalse(pair.still_destination.exists())
        self.assert_released(primary, pair)

    def test_cancel_during_capture_never_finishes_or_publishes(self):
        primary, pair = fixture_session(self.root)
        primary.scene.camera.on_capture = pair.abort
        with self.assertRaisesRegex(RuntimeError, "cancelled during final capture"):
            with pair:
                primary.run()
        self.assertNotIn("finish-clips", primary.events)
        self.assertFalse(pair.still_destination.exists())
        self.assert_released(primary, pair)

    def test_final_journal_failure_retains_image_receipt_but_refuses_orphan_on_resume(self):
        primary, pair = fixture_session(self.root)
        error = OSError("final fsync refused")
        actual = pair._image_journal._publish
        def fail_final(payload):
            if json.loads(payload)["pair"] is not None:
                raise error
            return actual(payload)
        with patch.object(pair._image_journal, "_publish", side_effect=fail_final):
            with self.assertRaises(OSError) as caught:
                with pair:
                    primary.run()
        self.assertIs(caught.exception, error)
        result = error.render_pair_result
        self.assertFalse(result.completed)
        self.assertTrue(result.primary.completed)
        self.assertIsNotNone(result.still)
        self.assert_released(primary, pair)
        primary, resumed = fixture_session(self.root, resume=True)
        with self.assertRaisesRegex(FileExistsError, "unrecorded final PNG"):
            with resumed:
                primary.run()
        self.assert_released(primary, resumed)

    def test_late_foreign_png_collision_preserves_foreign_file_and_completed_clips(self):
        primary, pair = fixture_session(self.root)
        primary.scene.camera.on_commit = lambda pending: pending.destination.write_bytes(b"foreign")
        with self.assertRaises(FileExistsError) as caught:
            with pair:
                primary.run()
        self.assertEqual(pair.still_destination.read_bytes(), b"foreign")
        self.assertTrue(caught.exception.render_pair_result.primary.completed)
        self.assertIsNone(caught.exception.render_pair_result.still)
        self.assert_released(primary, pair)

    def test_empty_scene_pair_is_reused_without_inventing_clips(self):
        _, first = self.complete(frames=0)
        primary, resumed = fixture_session(self.root, resume=True, frames=0)
        with resumed:
            primary.run()
        self.assertEqual(resumed.result.as_dict(), first.result.as_dict())
        self.assertEqual(resumed.result.primary.segments, ())
        self.assertNotIn("capture", primary.events)

    def test_tampered_missing_and_symlinked_final_images_refuse_before_replay(self):
        for kind in ("tamper", "missing", "symlink"):
            with self.subTest(kind=kind):
                self.root = Path(tempfile.mkdtemp(prefix="fmn-png-invalid-"))
                _, pair = self.complete()
                path = pair.still_destination
                if kind == "tamper":
                    path.write_bytes(b"changed")
                else:
                    saved = path.with_name("preserved.png")
                    path.rename(saved)
                    if kind == "symlink":
                        path.symlink_to(saved)
                primary, resumed = fixture_session(self.root, resume=True)
                with self.assertRaises((ValueError, FileNotFoundError)):
                    resumed.__enter__()
                self.assertNotIn("capture", primary.events)
                self.assertEqual(primary.replayed, 0)
                self.assert_released(primary, resumed)

    def test_image_is_reverified_after_authored_replay_before_success(self):
        _, first = self.complete()
        primary, resumed = fixture_session(self.root, resume=True)
        with self.assertRaises(ValueError):
            with resumed:
                primary.run()
                first.still_destination.write_bytes(b"mutated during authored replay")
        self.assertIsNone(resumed.partial_result.still)
        self.assertFalse(resumed.partial_result.completed)
        self.assertNotIn("capture", primary.events)
        self.assert_released(primary, resumed)

    def test_replay_interruption_does_not_expose_saved_still_as_current_success(self):
        self.complete()
        primary, resumed = fixture_session(self.root, resume=True)
        with self.assertRaises(KeyboardInterrupt) as caught:
            with resumed:
                primary.play()
                raise KeyboardInterrupt("cancelled replay")
        result = caught.exception.render_pair_result
        self.assertFalse(result.completed)
        self.assertFalse(result.primary.completed)
        self.assertIsNone(result.still)
        self.assert_released(primary, resumed)

    def test_changed_clip_receipt_refuses_reusing_saved_png(self):
        self.complete()
        primary, resumed = fixture_session(self.root, resume=True)
        with self.assertRaisesRegex(ValueError, "completed clip collection"):
            with resumed:
                primary.run()
                segment = primary._journal.segments[-1]
                primary._journal.segments[-1] = replace(segment, end_frame=99)
        self.assertIsNone(resumed.partial_result.still)
        self.assert_released(primary, resumed)

    def test_conflicting_final_journal_plan_and_corrupt_receipt_are_refused(self):
        for kind in ("plan", "digest", "partial", "inventory", "extra"):
            with self.subTest(kind=kind):
                self.root = Path(tempfile.mkdtemp(prefix="fmn-journal-invalid-"))
                _, first = self.complete()
                path = first._image_journal.path
                doc = json.loads(path.read_bytes())
                if kind == "plan": doc["plan"]["clip_plan"]["identity"] = {"fixture": "other"}
                elif kind == "digest": doc["pair"]["still"]["digest"] = "0" * 64
                elif kind == "partial": doc["pair"]["completed"] = False
                elif kind == "inventory": doc["artifacts"] = None
                else: doc["extra"] = True
                path.write_bytes(_json(doc))
                primary, resumed = fixture_session(self.root, resume=True)
                with self.assertRaises(ValueError):
                    resumed.__enter__()
                self.assert_released(primary, resumed)

    def test_final_journal_lock_contention_releases_primary_lock(self):
        _, first = self.complete()
        path = first._image_journal.path
        with FinalImageCheckpoint(path, resume=True, key="v1"):
            primary, resumed = fixture_session(self.root, resume=True)
            with self.assertRaises(OSError):
                resumed.__enter__()
            self.assert_released(primary, resumed)
        with ClipJournalFixture(primary._journal.path, resume=True, key="v1"):
            pass

    def test_missing_final_journal_after_clips_finish_is_recoverable_before_png_publication(self):
        primary, pair = fixture_session(self.root)
        def refuse(pending):
            raise OSError("PNG publisher unavailable")
        primary.scene.camera.on_commit = refuse
        with self.assertRaises(OSError):
            with pair:
                primary.run()
        pair._image_journal.path.rename(self.root / "preserved-empty-final-journal")
        primary, resumed = fixture_session(self.root, resume=True)
        with resumed:
            primary.run()
        self.assertEqual(primary.encoded, [])
        self.assertTrue(resumed.result.completed)

    def test_recovery_paths_freeze_and_foreign_journals_are_refused(self):
        recovery = {"checkpoint": self.root / "state/A.json", "resume": False, "resume_key": "v1",
                    "final_checkpoint": self.root / "state/A.still.json", "resume_final": False}
        result = prepare_paired_recovery(recovery, self.root / "out/clips", self.root / "out/clips.png")
        self.assertTrue(result["checkpoint"].is_absolute())
        for change in ({"resume": 1}, {"final_checkpoint": self.root / "different.json"},
                       {"checkpoint": self.root / "out/clips/progress.json",
                        "final_checkpoint": self.root / "out/clips/progress.still.json"}):
            with self.subTest(change=change), self.assertRaises((TypeError, ValueError)):
                prepare_paired_recovery(dict(recovery, **change), self.root / "out/clips", self.root / "out/clips.png")

    def test_plain_pair_still_uses_fresh_no_clobber_paths(self):
        self.complete()
        primary = ClipPrimaryFixture(self.root, resume=True)
        pair = PairedRenderSession(primary, primary.destination.with_name("clips.png"))
        with self.assertRaises(FileExistsError):
            pair.__enter__()
        self.assertEqual(primary.events, [])
        self.assertIsNone(primary._journal.lock)


if __name__ == "__main__":
    unittest.main(verbosity=2)
