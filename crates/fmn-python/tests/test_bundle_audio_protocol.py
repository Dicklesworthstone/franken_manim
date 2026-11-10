"""Audio export host ownership; native-boundary spies are not PCM evidence."""
from pathlib import Path
import os
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "python"))
from fmn_python import BundleExportSession, export_bundle
from test_bundle_export_protocol import Native, CapabilityError
from test_render_session_protocol import Scene

class AudioNative(Native):
    _PORTAL_BUNDLE_AUDIO_FFMPEG = True

    def _portal_validate_bundle_owner(self, scene):
        if scene.active:
            raise RuntimeError("external owner")

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
            self.assertEqual(args[8:], (camera, 48000, 2, 4, 8192, False))
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
        for transcode in (False, True):
            with BundleExportSession(Scene(), "empty.fmtl", audio=True,
                                     audio_ffmpeg=transcode) as session:
                pass
            self.assertIsNone(session.result.soundtrack)
            self.assertNotIn("fmtl_minor", session.result.as_dict())
            self.assertNotIn("soundtrack", session.result.as_dict())
            self.assertNotIn("audio_inputs", session.result.as_dict())
            self.assertNotIn("ffmpeg_invocations", session.result.as_dict())
            self.assertNotIn("ffmpeg_limits", session.result.as_dict())

    def test_compressed_opt_in_routes_and_freezes_native_receipt_values(self):
        for camera in (False, True):
            scene = Scene()
            scene._render_audio_inputs = [{"path": "/source.flac", "decoder": "ffmpeg"}]
            scene._render_invocations = [{"argv": ["-f", "flac"], "encoder": "pcm_f32le"}]
            scene._render_ffmpeg_limits = {"timeout_seconds": 37, "max_input_bytes": 8192}
            with BundleExportSession(scene, "compressed.fmtl", audio=True, camera=camera,
                                     audio_ffmpeg=True) as session:
                pass
            args = next(event[1] for event in reversed(self.native.events) if event[0] == "audio-begin")
            self.assertEqual(args[8:], (camera, 48000, 2, 1, 64 * 1024 * 1024, True))
            result = session.result
            scene._render_audio_inputs[0]["decoder"] = "changed"
            scene._render_invocations[0]["argv"].append("changed")
            scene._render_ffmpeg_limits["timeout_seconds"] = 99
            self.assertEqual(result.audio_inputs[0]["decoder"], "ffmpeg")
            self.assertEqual(result.ffmpeg_invocations[0]["argv"], ["-f", "flac"])
            self.assertEqual(result.ffmpeg_limits["timeout_seconds"], 37)
            document = result.as_dict()
            document["audio_inputs"][0]["decoder"] = "changed again"
            document["ffmpeg_invocations"][0]["argv"].append("changed again")
            document["ffmpeg_limits"]["timeout_seconds"] = 101
            self.assertEqual(result.as_dict()["audio_inputs"][0]["decoder"], "ffmpeg")
            self.assertEqual(result.as_dict()["ffmpeg_invocations"][0]["argv"], ["-f", "flac"])
            self.assertEqual(result.as_dict()["ffmpeg_limits"]["timeout_seconds"], 37)

    def test_default_audio_does_not_read_or_add_transcoder_receipts(self):
        class NativeOnlyScene(Scene):
            def __getattribute__(self, name):
                if name == "_render_audio_inputs":
                    raise AssertionError("default export read optional receipt properties")
                return super().__getattribute__(name)
        with BundleExportSession(NativeOnlyScene(), "native.fmtl", audio=True) as session:
            pass
        self.assertEqual(session.result.audio_inputs, ())
        self.assertEqual(session.result.ffmpeg_invocations, ())
        self.assertIsNone(session.result.ffmpeg_limits)
        self.assertNotIn("audio_inputs", session.result.as_dict())

    def test_options_and_missing_symbols_refuse_before_factory(self):
        constructed = []
        class UserScene(Scene):
            def __init__(self):
                constructed.append(True)
                super().__init__()
        for key, values in {"audio": [None, 0, 1, "true"],
                            "audio_ffmpeg": [None, 0, 1, "true"],
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
        with self.assertRaisesRegex(ValueError, "requires audio=True"):
            export_bundle(UserScene, "no.fmtl", audio_ffmpeg=True)
        with patch.dict(sys.modules, {"manimlib": Native()}):
            with self.assertRaisesRegex(CapabilityError, "matching native wheel"):
                export_bundle(UserScene, "no.fmtl", audio=True)
        with patch.object(self.native, "_PORTAL_BUNDLE_AUDIO_FFMPEG", False):
            with self.assertRaisesRegex(CapabilityError, "audio_ffmpeg support"):
                export_bundle(UserScene, "no.fmtl", audio=True, audio_ffmpeg=True)
        self.assertEqual(constructed, [])
        self.assertEqual(self.native.events, [])

    def test_decoder_overrides_validate_before_class_construction(self):
        constructed = []
        class UserScene(Scene):
            def __init__(self):
                constructed.append(True)
                super().__init__()
        for override in ([], {"other": 1}, {"ffmpeg_bin": ""},
                         {"ffmpeg_bin": "\0bad"}, {"ffmpeg_bin": 3}):
            with self.subTest(override=override), self.assertRaises((TypeError, ValueError)):
                export_bundle(UserScene, "unused.fmtl", audio=True, audio_ffmpeg=True,
                              _output_options=override)
        with self.assertRaisesRegex(ValueError, "require audio=True and audio_ffmpeg=True"):
            export_bundle(UserScene, "unused.fmtl", audio=True,
                          _output_options={"ffmpeg_bin": "ffmpeg"})
        self.assertEqual(constructed, [])
        self.assertEqual(self.native.events, [])

    def test_decoder_override_is_frozen_and_beats_authored_constructor_defaults(self):
        previous = Path.cwd()
        created, observed = [], []
        supplied = {"ffmpeg_bin": "./tools/decoder"}
        begin = self.native._portal_begin_soundtrack_bundle
        def observe(scene, *args):
            observed.append(scene.file_writer.ffmpeg_bin)
            begin(scene, *args)
        self.native._portal_begin_soundtrack_bundle = observe
        self.native._portal_finish_soundtrack_bundle = lambda scene: (
            self.native._portal_finish_bundle(scene), None)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            nested = root / "authored-cwd"
            nested.mkdir()
            class UserScene(Scene):
                def __init__(self):
                    super().__init__()
                    self.file_writer = SimpleNamespace(ffmpeg_bin="authored", unrelated=73)
                    supplied["ffmpeg_bin"] = "mutated"
                    created.append(self)
                    os.chdir(nested)
            try:
                os.chdir(root)
                export_bundle(UserScene, root / "out.fmtl", audio=True, audio_ffmpeg=True,
                              _output_options=supplied)
            finally:
                os.chdir(previous)
        self.assertEqual(observed, [str(root / "tools" / "decoder")])
        self.assertEqual(len(created), 1)
        self.assertEqual(created[0].file_writer.unrelated, 73)

    def test_decoder_override_does_not_modify_an_existing_native_owner(self):
        scene = Scene()
        scene.file_writer = SimpleNamespace(ffmpeg_bin="existing-owner")
        scene.active = True
        with self.assertRaisesRegex(RuntimeError, "external owner"):
            BundleExportSession(scene, "unused.fmtl", audio=True, audio_ffmpeg=True,
                                _output_options={"ffmpeg_bin": "new-request"}).__enter__()
        self.assertEqual(scene.file_writer.ffmpeg_bin, "existing-owner")
        self.assertTrue(scene.active)
        self.assertNotIn(("abort",), scene.events)

    def test_writer_setter_cannot_hide_its_new_output_owner(self):
        scene = Scene()
        class Writer:
            @property
            def ffmpeg_bin(self):
                return "authored"
            @ffmpeg_bin.setter
            def ffmpeg_bin(self, value):
                scene.active = True
        scene.file_writer = Writer()
        with self.assertRaisesRegex(RuntimeError, "external owner"):
            BundleExportSession(scene, "unused.fmtl", audio=True, audio_ffmpeg=True,
                                _output_options={"ffmpeg_bin": "new-request"}).__enter__()
        self.assertTrue(scene.active)
        self.assertNotIn(("abort",), scene.events)

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
