"""Audio batch dispatch and real checkpoint I/O with an exporter-boundary spy.

The files below are protocol fixtures, not native FMTL/PCM acceptance evidence.
"""
from copy import deepcopy
import hashlib
import json
from pathlib import Path
import sys
import tempfile
import types
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "python"))
from fmn_python import batch_rendering as batch
from fmn_python.bundle_checkpoint import restore_bundle_receipt
from fmn_python.bundle_export import BundleExportResult, BundleSoundtrackInfo


class CapabilityError(RuntimeError):
    pass


class Scene:
    constructions = []

    def __init__(self, **kwargs):
        self.constructions.append((type(self).__name__, kwargs))


class First(Scene):
    pass


class Second(Scene):
    pass


def native_boundary():
    return types.SimpleNamespace(
        Scene=Scene, _CapabilityError=CapabilityError,
        _portal_begin_bundle=lambda *args: None,
        _portal_begin_camera_bundle=lambda *args: None,
        _portal_begin_soundtrack_bundle=lambda *args: None,
        _portal_bundle_segment=lambda *args: None,
        _portal_finish_bundle=lambda *args: None,
        _portal_finish_soundtrack_bundle=lambda *args: None,
    )


class AudioBatchProtocol(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.directory = self.root / "bundles"
        self.checkpoint = self.root / "progress.json"
        self.native = native_boundary()
        self.calls, self.failure, self.mutate, self.silent = [], None, None, False
        Scene.constructions = []
        self.imports = patch.dict(sys.modules, {"manimlib": self.native})
        self.imports.start()
        self.addCleanup(self.imports.stop)
        exporter = patch.object(batch, "export_bundle", self.export)
        exporter.start()
        self.addCleanup(exporter.stop)

    def export(self, scene, destination, **options):
        self.calls.append((scene.__name__, deepcopy(options)))
        scene(**(options.get("scene_kwargs") or {}))
        if self.mutate is not None:
            self.mutate()
        if scene is Second and self.failure is not None:
            raise self.failure
        path = Path(destination)
        path.parent.mkdir(parents=True, exist_ok=True)
        data = ("protocol fixture for " + scene.__name__).encode().ljust(256, b".")
        with path.open("xb") as stream:
            stream.write(data)
        soundtrack = None
        if options.get("audio") and not self.silent:
            soundtrack = BundleSoundtrackInfo(options["audio_sample_rate"], options["audio_channels"], 8)
        return BundleExportResult(path, 12, 2, len(data), hashlib.sha256(data).hexdigest(),
                                  options.get("fps") or 24, options.get("resolution") or (64, 36),
                                  options.get("camera", False), soundtrack)

    def render(self, **options):
        return batch.render_scenes({"First": First, "Second": Second}, self.directory,
                                   format="fmtl", **options)

    def test_audio_options_are_frozen_and_forwarded_to_each_export(self):
        settings = {"audio_sample_rate": 32_000, "audio_channels": 1,
                    "audio_threads": 4, "max_audio_bytes": 8192}
        expected = dict(settings)
        self.mutate = lambda: settings.update(audio_sample_rate=1, audio_channels=2)
        result = self.render(bundle_audio=True, bundle_audio_options=settings, bundle_camera=True)
        self.assertTrue(result.ok)
        self.assertEqual([name for name, _ in self.calls], ["First", "Second"])
        for _, options in self.calls:
            self.assertIs(options["audio"], True)
            self.assertIs(options["camera"], True)
            self.assertEqual({key: options[key] for key in expected}, expected)
        for row in result.as_dict()["outcomes"]:
            self.assertEqual(row["result"]["soundtrack"],
                             {"sample_rate": 32_000, "channels": 1, "sample_frames": 8})
            self.assertEqual(row["result"]["fmtl_minor"], 2)
            self.assertIs(row["result"]["certified_source"], False)

    def test_default_picture_path_keeps_legacy_kwargs_and_checkpoint_plan(self):
        result = self.render(checkpoint=self.checkpoint, resume_key="v1")
        self.assertTrue(result.ok)
        self.assertTrue(all("audio" not in options for _, options in self.calls))
        plan = json.loads(self.checkpoint.read_text())["plan"]["options"]
        self.assertNotIn("bundle_audio", plan)
        self.assertNotIn("bundle_audio_options", plan)
        self.assertTrue(all("soundtrack" not in row.result.as_dict() for row in result.outcomes))

    def test_invalid_audio_options_refuse_before_constructing_or_exporting(self):
        invalid = [{"bundle_audio": 1}, {"bundle_audio": "yes"},
                   {"bundle_audio_options": {}}, {"bundle_audio": True, "bundle_audio_options": []}]
        for key, values in {
            "audio_sample_rate": (0, -1, True, 384_001, 48_000.0, "48000"),
            "audio_channels": (0, 3, True), "audio_threads": (0, 65, False),
            "max_audio_bytes": (0, 7, 268_435_457, True), "unknown": (1,),
        }.items():
            invalid.extend({"bundle_audio": True, "bundle_audio_options": {key: value}} for value in values)
        for options in invalid:
            with self.subTest(options=options), self.assertRaises((TypeError, ValueError)):
                self.render(**options)
        self.assertEqual(self.calls, [])
        self.assertEqual(Scene.constructions, [])
        self.assertFalse(self.directory.exists())

    def test_audio_settings_are_not_accepted_for_other_output_formats(self):
        for options in ({"bundle_audio": True}, {"bundle_audio_options": {}}):
            with self.subTest(options=options), self.assertRaisesRegex(ValueError, "fmtl"):
                batch.render_scenes({"First": First}, self.directory, format="wav", **options)
        self.assertEqual(self.calls, [])

    def test_audio_capabilities_are_checked_before_any_scene_runs(self):
        for name in ("_portal_begin_soundtrack_bundle", "_portal_finish_soundtrack_bundle"):
            for camera in (False, True):
                with self.subTest(name=name, camera=camera):
                    saved = getattr(self.native, name)
                    setattr(self.native, name, None)
                    try:
                        with self.assertRaises(CapabilityError):
                            self.render(bundle_audio=True, bundle_camera=camera)
                    finally:
                        setattr(self.native, name, saved)
        self.assertEqual(Scene.constructions, [])
        self.assertFalse(self.directory.exists())

    def test_audio_checkpoint_resumes_only_failed_jobs_and_retains_metadata(self):
        self.failure = ValueError("narration decode failed")
        with self.assertRaises(batch.BatchRenderError) as caught:
            self.render(bundle_audio=True, bundle_camera=True,
                        checkpoint=self.checkpoint, resume_key="source-v1")
        first = caught.exception.result.outcomes[0].result
        self.assertEqual(caught.exception.result.counts["failed"], 1)
        saved_bytes = first.destination.read_bytes()
        self.failure = None
        self.calls.clear()
        Scene.constructions.clear()
        result = self.render(bundle_audio=True, bundle_camera=True,
                             checkpoint=self.checkpoint, resume=True, resume_key="source-v1")
        self.assertEqual([name for name, _ in self.calls], ["Second"])
        self.assertEqual(Scene.constructions, [("Second", {})])
        self.assertEqual(result.outcomes[0].result.as_dict(), first.as_dict())
        self.assertEqual(first.destination.read_bytes(), saved_bytes)
        self.assertTrue(result.ok)

    def test_checkpoint_identity_binds_audio_mode_and_all_audio_options(self):
        self.render(bundle_audio=True, checkpoint=self.checkpoint, resume_key="v1")
        original = self.checkpoint.read_bytes()
        variants = [{"bundle_audio": False}]
        variants.extend({"bundle_audio": True, "bundle_audio_options": {name: value}}
                        for name, value in (("audio_sample_rate", 44_100), ("audio_channels", 1),
                                            ("audio_threads", 2), ("max_audio_bytes", 8192)))
        self.calls.clear()
        for options in variants:
            with self.subTest(options=options), self.assertRaisesRegex(ValueError, "plan|options"):
                self.render(checkpoint=self.checkpoint, resume=True, resume_key="v1", **options)
            self.assertEqual(self.checkpoint.read_bytes(), original)
        self.assertEqual(self.calls, [])

    def test_explicit_defaults_can_resume_implicit_audio_defaults(self):
        self.render(bundle_audio=True, checkpoint=self.checkpoint, resume_key="v1")
        self.calls.clear()
        result = self.render(bundle_audio=True, bundle_audio_options={
            "audio_sample_rate": 48_000, "audio_channels": 2,
            "audio_threads": 1, "max_audio_bytes": 64 * 1024 * 1024},
            checkpoint=self.checkpoint, resume=True, resume_key="v1")
        self.assertTrue(result.ok)
        self.assertEqual(self.calls, [])

    def test_no_cue_audio_export_reuses_picture_only_receipts(self):
        self.silent = True
        first = self.render(bundle_audio=True, checkpoint=self.checkpoint, resume_key="v1")
        self.calls.clear()
        resumed = self.render(bundle_audio=True, checkpoint=self.checkpoint, resume=True, resume_key="v1")
        self.assertEqual(first.as_dict(), resumed.as_dict())
        self.assertIsNone(resumed.outcomes[0].result.soundtrack)
        self.assertEqual(self.calls, [])

    def test_changed_artifact_is_refused_without_reconstruction_or_overwrite(self):
        first = self.render(bundle_audio=True, checkpoint=self.checkpoint, resume_key="v1")
        path = first.outcomes[0].destination
        changed = b"!" + path.read_bytes()[1:]
        path.write_bytes(changed)
        self.calls.clear()
        with self.assertRaisesRegex(ValueError, "modified"):
            self.render(bundle_audio=True, checkpoint=self.checkpoint, resume=True, resume_key="v1")
        self.assertEqual(self.calls, [])
        self.assertEqual(path.read_bytes(), changed)

    def test_keep_going_and_interrupts_preserve_audio_receipts(self):
        self.failure = ValueError("decode")
        result = self.render(bundle_audio=True, continue_on_error=True)
        self.assertEqual([row.status for row in result.outcomes], ["succeeded", "failed"])
        self.assertIsNotNone(result.outcomes[0].result.soundtrack)
        self.directory = self.root / "interrupted"
        self.failure = KeyboardInterrupt()
        with self.assertRaises(KeyboardInterrupt) as caught:
            self.render(bundle_audio=True, continue_on_error=True)
        partial = caught.exception.render_batch_result
        self.assertEqual([row.status for row in partial.outcomes], ["succeeded", "cancelled"])
        self.assertIsNotNone(partial.outcomes[0].result.soundtrack)

    def test_destination_collision_refuses_before_first_export(self):
        self.directory.mkdir()
        occupied = self.directory / "Second.fmtl"
        occupied.write_bytes(b"another writer")
        with self.assertRaises(FileExistsError):
            self.render(bundle_audio=True)
        self.assertEqual(self.calls, [])
        self.assertEqual(occupied.read_bytes(), b"another writer")


class AudioReceiptProtocol(unittest.TestCase):
    def receipt(self, *, camera=False, audio=True):
        return BundleExportResult(Path.cwd() / "scene.fmtl", 12, 2, 2048, "a" * 64,
                                  24, (64, 36), camera,
                                  BundleSoundtrackInfo(32_000, 1, 16) if audio else None).as_dict()

    def test_all_picture_camera_audio_receipts_round_trip_without_certification(self):
        for camera in (False, True):
            for audio in (False, True):
                with self.subTest(camera=camera, audio=audio):
                    data = self.receipt(camera=camera, audio=audio)
                    self.assertEqual(restore_bundle_receipt(data).as_dict(), data)
                    self.assertIs(restore_bundle_receipt(data).as_dict()["certified_source"], False)

    def test_malformed_soundtrack_metadata_and_envelopes_are_refused(self):
        invalid = []
        for name, values in {"sample_rate": (0, True, 384_001, 32_000.0),
                             "channels": (0, 3, True), "sample_frames": (-1, True, 1 << 63)}.items():
            for value in values:
                data = self.receipt()
                data["soundtrack"][name] = value
                invalid.append(data)
        for mutate in (
            lambda data: data.update(fmtl_minor=1), lambda data: data.update(fmtl_minor=True),
            lambda data: data.update(camera_track=False), lambda data: data.update(soundtrack=None),
            lambda data: data["soundtrack"].update(extra=1),
            lambda data: data["soundtrack"].pop("channels"),
            lambda data: data.update(bytes=16), lambda data: data.pop("soundtrack"),
        ):
            data = self.receipt()
            mutate(data)
            invalid.append(data)
        for data in invalid:
            with self.subTest(data=data), self.assertRaises(ValueError):
                restore_bundle_receipt(data)

    def test_audio_plan_must_admit_receipt_layout_and_pcm_size(self):
        options = {"bundle_camera": False, "fps": 24, "resolution": (64, 36),
                   "bundle_limits": {"max_frames": 100, "max_output_bytes": 4096},
                   "bundle_audio": True, "bundle_audio_options": {
                       "audio_sample_rate": 32_000, "audio_channels": 1,
                       "audio_threads": 1, "max_audio_bytes": 1024}}
        data = self.receipt()
        self.assertEqual(restore_bundle_receipt(data, options=options).soundtrack.sample_frames, 16)
        for change in ({"bundle_audio": False}, {"bundle_audio_options": {
            **options["bundle_audio_options"], "audio_sample_rate": 48_000}},
            {"bundle_audio_options": {**options["bundle_audio_options"], "audio_channels": 2}},
            {"bundle_audio_options": {**options["bundle_audio_options"], "max_audio_bytes": 32}}):
            with self.subTest(change=change), self.assertRaises(ValueError):
                restore_bundle_receipt(data, options={**options, **change})


if __name__ == "__main__":
    unittest.main()
