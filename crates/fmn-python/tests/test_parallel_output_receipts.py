"""Malformed transport checks; these do not stand in for native pixel tests."""
from copy import deepcopy
from pathlib import Path
import sys
import tempfile
import types
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "python"))
from fmn_python.parallel_output import restore_output
from fmn_python.parallel_batch import _outcome, render_source_scenes
from fmn_python.rendering import RenderResult
from fmn_python.subdivision import RenderSegment, SubdividedRenderResult
from fmn_python.paired_output import PairedRenderResult


class ParallelOutputReceiptTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.path = self.root / "clips"
        render = RenderResult(self.path / "segment-000002.gif", "gif", (64, 48), 10, 1,
                              "test-receipt", 100, "a" * 64, 2, None, 0)
        clip = RenderSegment(2, "play", 3, 5, render)
        self.collection = SubdividedRenderResult(self.path, (clip,), True)
        still = RenderResult(self.root / "clips.png", "png", (64, 48), 10, 1,
                             "test-readback", 100, "b" * 64, 1, None, 0)
        self.pair = PairedRenderResult(self.path, still.destination, self.collection, still, True)

    def decode(self, data, paired=False, **options):
        return restore_output(data, destination=self.path, format="gif", subdivide=True,
                              save_last_frame=paired, max_segments=options.get("maximum", 10))

    def test_round_trip_complete_and_partial_collections_and_pairs(self):
        self.assertEqual(self.decode(self.collection.as_dict()), self.collection)
        self.assertEqual(self.decode(self.pair.as_dict(), True), self.pair)
        partial = self.collection.as_dict() | {"completed": False}
        data = self.pair.as_dict() | {"completed": False, "still": None, "primary": partial}
        result = self.decode(data, True)
        self.assertFalse(result.completed)
        self.assertFalse(result.primary.completed)
        self.assertEqual(result.primary.segments, self.collection.segments)

    def test_ffmpeg_policy_survives_worker_receipt_transport(self):
        destination = self.path / "segment-000002.mp4"
        policy = dict(timeout_seconds=37, timeout_nanoseconds=0, max_log_bytes=131072,
                      max_artifact_bytes=1048576, max_input_bytes=2097152, keep_workdir=False)
        render = RenderResult(destination, "mp4", (64, 48), 10, 1, "test-receipt",
                              100, "a" * 64, 2, None, 0, ffmpeg_limits=policy)
        collection = SubdividedRenderResult(self.path, (RenderSegment(2, "play", 3, 5, render),), True)
        restored = restore_output(collection.as_dict(), destination=self.path, format="mp4",
                                  subdivide=True, save_last_frame=False, max_segments=10)
        self.assertEqual(restored.segments[0].render.ffmpeg_limits, policy)
        self.assertEqual(restored, collection)

    def test_wrong_paths_and_certification_are_never_admitted(self):
        for key, value in (("destination", str(self.root / "foreign.gif")), ("format", "mp4"),
                           ("certified", True), ("schema", "fmn.paired-render"), ("manifest", {})):
            data = self.collection.as_dict()
            data["segments"][0]["render"][key] = value
            with self.subTest(key=key), self.assertRaises(ValueError):
                self.decode(data)
        data = self.pair.as_dict()
        data["still_destination"] = str(self.root / "foreign.png")
        with self.assertRaises(ValueError):
            self.decode(data, True)

    def test_bad_clip_order_intervals_and_aggregates_refuse(self):
        for key, value in (("start_frame", True), ("end_frame", 2), ("kind", "constructor"),
                           ("play_index", 3)):
            data = self.collection.as_dict()
            data["segments"][0][key] = value
            with self.subTest(key=key), self.assertRaises(ValueError):
                self.decode(data)
        data = self.collection.as_dict()
        data["segments"].append(deepcopy(data["segments"][0]))
        with self.assertRaises(ValueError):
            self.decode(data)
        for key, value in (("frame_count", 3), ("bytes", 101), ("sample_frames", 1), ("version", True)):
            with self.subTest(key=key), self.assertRaises(ValueError):
                self.decode(self.collection.as_dict() | {key: value})
        with self.assertRaises(ValueError):
            self.decode(self.collection.as_dict(), maximum=0)

    def test_pair_cannot_claim_completion_or_disagree_with_its_primary(self):
        for key, value in (("primary", None), ("still", None),
                           ("unreceipted_artifacts", [str(self.root / "clips.png")])):
            with self.subTest(key=key), self.assertRaises(ValueError):
                self.decode(self.pair.as_dict() | {key: value}, True)
        data = self.pair.as_dict()
        data["still"]["seed"] = 1
        with self.assertRaises(ValueError):
            self.decode(data, True)
        data = self.pair.as_dict()
        data["primary"]["completed"] = False
        with self.assertRaises(ValueError):
            self.decode(data, True)

    def test_unreceipted_paths_are_bounded_to_native_clip_names(self):
        base = self.collection.as_dict() | {"completed": False}
        allowed = str(self.path / "segment-000003.gif")
        self.assertEqual(self.decode(base | {"unreceipted_artifacts": [allowed]}).unreceipted_artifacts,
                         (Path(allowed),))
        for value in (str(self.path / "../elsewhere.gif"), str(self.path / "segment-3.gif"),
                      str(self.path / "segment-000003.png"), str(self.path / "segment-000002.gif")):
            with self.subTest(value=value), self.assertRaises(ValueError):
                self.decode(base | {"unreceipted_artifacts": [value]})

    def test_outcome_cannot_hide_partial_or_fabricate_complete_output(self):
        row = {"name": "First", "destination": str(self.path), "status": "succeeded",
               "result": self.collection.as_dict() | {"completed": False}}
        with self.assertRaises(ValueError):
            _outcome("First", self.path, row, format="gif", subdivide=True)
        with self.assertRaises(ValueError):
            _outcome("First", self.path, row | {"result": None}, format="gif", subdivide=True)
        result = _outcome("First", self.path, row | {"status": "failed"}, format="gif", subdivide=True)
        self.assertFalse(result.result.completed)
        self.assertEqual(len(result.result.segments), 1)

    def test_bad_mode_options_refuse_before_worker_start_or_source_execution(self):
        source = self.root / "scene.py"
        source.write_text("raise AssertionError('must not run')\n")
        # A capability table is enough for admission: no scene or renderer
        # should execute, including in a source-only test installation.
        native = types.SimpleNamespace(
            _CameraCapture=types.SimpleNamespace(prepare_png=lambda: None),
            _portal_prepare_recording_scene=lambda: None,
            _portal_begin_audio_clip=lambda: None,
        )
        with (patch("fmn_python.parallel_batch.run_process_jobs") as workers,
              patch.dict(sys.modules, {"manimlib": native})):
            for options in ({"subdivide": 1}, {"save_last_frame": 1}, {"max_segments": 3},
                            {"subdivide": True, "format": "png"},
                            {"subdivide": True, "format": "gif", "max_segments": 0},
                            {"subdivide": True, "format": "gif", "resolution": (100000, 100000)},
                            {"subdivide": True, "format": "gif", "checkpoint": self.root / "p.json",
                             "resume_key": "x"}, {"save_last_frame": True, "format": "svg"}):
                with self.subTest(options=options), self.assertRaises((TypeError, ValueError)):
                    render_source_scenes(source, self.root / "out", ["First"], **options)
            workers.assert_not_called()
        self.assertFalse((self.root / "out").exists())


if __name__ == "__main__":
    unittest.main()
