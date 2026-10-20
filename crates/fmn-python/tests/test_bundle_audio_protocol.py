"""Audio export host ownership; native-boundary spies are not PCM evidence."""
from pathlib import Path
import sys
import unittest
from unittest.mock import patch
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "python"))
from fmn_python import BundleExportSession, export_bundle
from test_bundle_export_protocol import Native, CapabilityError
from test_render_session_protocol import Scene

class AudioNative(Native):
    def _portal_begin_soundtrack_bundle(self, scene, *arguments):
        self._portal_begin_bundle(scene, *arguments[:8])
        self.events[-1] = ("audio-begin", arguments)
    def _portal_finish_soundtrack_bundle(self, scene):
        return self._portal_finish_bundle(scene), (48000, 2, 72000)

class AudioProtocol(unittest.TestCase):
    def setUp(self):
        self.native = AudioNative()
        self.imports = patch.dict(sys.modules, {"manimlib": self.native})
        self.imports.start()
        self.addCleanup(self.imports.stop)

    def test_audio_camera_selection_and_receipt(self):
        for camera in (False, True):
            with BundleExportSession(Scene(), "sound.fmtl", camera=camera, audio=True,
                                     audio_threads=4, max_audio_bytes=8192) as session:
                with session._segment("wait") as segment:
                    segment.select(True)
            args = self.native.events[-4][1]
            self.assertEqual(args[8:], (camera, 48000, 2, 4, 8192))
            result = session.result
            self.assertTrue(session.artifact_published)
            self.assertEqual(result.camera_track, camera)
            self.assertEqual(result.as_dict()["fmtl_minor"], 2)
            self.assertEqual(result.soundtrack.sample_frames, 72000)
            self.assertEqual(result.as_dict()["soundtrack"], {
                "sample_rate": 48000, "channels": 2, "sample_frames": 72000})
            self.assertFalse(result.as_dict()["certified_source"])

    def test_absent_audio_keeps_legacy_receipt(self):
        self.native._portal_finish_soundtrack_bundle = lambda s: (self.native._portal_finish_bundle(s), None)
        with BundleExportSession(Scene(), "empty.fmtl", audio=True) as session:
            pass
        self.assertIsNone(session.result.soundtrack)
        self.assertNotIn("fmtl_minor", session.result.as_dict())
        self.assertNotIn("soundtrack", session.result.as_dict())

    def test_options_and_missing_symbols_refuse_before_factory(self):
        constructed = []
        class UserScene(Scene):
            def __init__(self):
                constructed.append(True)
                super().__init__()
        for key, values in {"audio": [None, 0, 1, "true"],
                            "audio_sample_rate": [0, True, 384001],
                            "audio_channels": [0, 3, True],
                            "audio_threads": [0, 65, True],
                            "max_audio_bytes": [0, 7, True, 256 * 1024 * 1024 + 1]}.items():
            for value in values:
                options = {"audio": True, key: value}
                with self.assertRaises((TypeError, ValueError)):
                    export_bundle(UserScene, "no.fmtl", **options)
        with self.assertRaisesRegex(ValueError, "require audio=True"):
            export_bundle(UserScene, "no.fmtl", audio_channels=1)
        with patch.dict(sys.modules, {"manimlib": Native()}):
            with self.assertRaisesRegex(CapabilityError, "matching native wheel"):
                export_bundle(UserScene, "no.fmtl", audio=True)
        self.assertEqual(constructed, [])
        self.assertEqual(self.native.events, [])

    def test_audio_cannot_cancel_an_existing_owner(self):
        scene = Scene()
        scene.active = True
        with self.assertRaisesRegex(RuntimeError, "external owner"):
            BundleExportSession(scene, "sound.fmtl", audio=True).__enter__()
        self.assertTrue(scene.active)
        self.assertNotIn(("abort",), scene.events)

    def test_audio_finish_failure_never_reports_publication(self):
        scene = Scene()
        session = BundleExportSession(scene, "bad.fmtl", audio=True)
        error = OSError("source disappeared")
        with self.assertRaises(OSError) as caught:
            with session:
                self.native.error = error
        self.assertIs(caught.exception, error)
        self.assertFalse(session.artifact_published)
        self.assertIsNone(session.result)
        self.assertFalse(scene.active)
        self.assertNotIn("_fmn_owned_render_session", vars(scene))

    def test_caught_capture_failure_cannot_publish_audio(self):
        scene = Scene()
        session = BundleExportSession(scene, "bad.fmtl", audio=True)
        with self.assertRaisesRegex(RuntimeError, "missing frame"):
            with session:
                try:
                    with session._segment("wait") as segment:
                        segment.select(True)
                        raise RuntimeError("missing frame")
                except RuntimeError:
                    pass
        self.assertNotIn(("finish",), self.native.events)
        self.assertFalse(session.artifact_published)

if __name__ == "__main__":
    unittest.main()
