#!/usr/bin/env python3
"""Negative controls for the original-workload correctness gate."""
import tempfile
import unittest
from pathlib import Path
import zipfile

import benchmark_issue8 as issue8


class OriginalWorkloadTests(unittest.TestCase):
    def test_observed_source_receipt_and_conservative_floor(self):
        observed = dict(frames=716, planar_frames=716, camera_frames=0,
                        raw_frame_reuses=598, conversion_reuses=598,
                        converted_frames=118, conversion_cache_bytes=345600)
        issue8.verify_source_work(observed)
        boundary = dict(observed, raw_frame_reuses=595, conversion_reuses=595, converted_frames=121)
        issue8.verify_source_work(boundary)
        issue8.verify_source_work(dict(observed, raw_frame_reuses=708,
                                       conversion_reuses=708, converted_frames=8))
        for change in (
            dict(raw_frame_reuses=594, conversion_reuses=594, converted_frames=122),
            dict(conversion_reuses=597), dict(converted_frames=117),
            dict(planar_frames=715, camera_frames=1), dict(conversion_cache_bytes=0),
            dict(raw_frame_reuses=715, conversion_reuses=715, converted_frames=1),
            dict(raw_frame_reuses=709, conversion_reuses=709, converted_frames=7),
        ):
            with self.subTest(change=change), self.assertRaises(ValueError):
                issue8.verify_source_work(dict(observed, **change))

    def test_repeated_video_and_audio_are_checked_independently(self):
        first = {"engine": "source-current", "decoded": {
            "video": {"sha256": "picture"}, "audio": {"sha256": "sound"}}}
        second = {"engine": "source-current", "decoded": {
            "video": {"sha256": "picture"}, "audio": {"sha256": "sound"}}}
        issue8.verify_repeatability([first, second])
        for stream in ("video", "audio"):
            second["decoded"][stream]["sha256"] = "changed"
            with self.subTest(stream=stream), self.assertRaises(ValueError):
                issue8.verify_repeatability([first, second])
            second["decoded"][stream]["sha256"] = first["decoded"][stream]["sha256"]
        with self.assertRaises(ValueError):
            issue8.verify_repeatability([first])

    def test_robot_receipt_requires_success_and_single_result(self):
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / "render.log"
            good = '{"schema":"fmn-python.cli","exit":{"code":0},"performance":{"frames":716}}'
            log.write_text("ordinary log {not JSON}\n" + good + "\n")
            self.assertEqual(issue8.receipt(log)["performance"]["frames"], 716)
            for text in (good + "\n" + good, good.replace('"code":0', '"code":5'), "no receipt"):
                log.write_text(text)
                with self.subTest(text=text), self.assertRaises(ValueError):
                    issue8.receipt(log)

    def test_unknown_archive_is_rejected_before_any_renderer_runs(self):
        with tempfile.TemporaryDirectory() as directory:
            archive = Path(directory) / "archive.tar.gz"
            archive.write_bytes(b"not the reviewed contributor archive")
            with self.assertRaisesRegex(ValueError, "archive digest"):
                issue8.verify_inputs(archive, Path(directory) / "absent")

    def test_wheel_must_cover_the_installed_native_extension(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            files = {"fmn_python/__init__.py": b"portal", "fmn_python/helper.py": b"helper",
                     "manimlib/native.so": b"native"}
            for name, contents in files.items():
                path = root / name
                path.parent.mkdir(exist_ok=True)
                path.write_bytes(contents)
            identity = {"modules": {"fmn_python": {"path": str(root / "fmn_python/__init__.py")},
                                    "manimlib.manimlib": {"path": str(root / "manimlib/native.so")}}}
            incomplete = root / "helpers-only.whl"
            with zipfile.ZipFile(incomplete, "w") as wheel:
                for name, contents in files.items():
                    if not name.endswith(".so"):
                        wheel.writestr(name, contents)
            with self.assertRaisesRegex(ValueError, "omits the installed native"):
                issue8.verify_source_wheel(incomplete, identity)
            complete = root / "complete.whl"
            with zipfile.ZipFile(complete, "w") as wheel:
                for name, contents in files.items():
                    wheel.writestr(name, contents)
            self.assertEqual(issue8.verify_source_wheel(complete, identity)["payload_files_checked"], 3)

    def test_cheaper_codecs_and_changed_audio_layout_are_rejected(self):
        video = {"codec_type": "video", "codec_name": "h264", "pix_fmt": "yuv420p"}
        audio = {"codec_type": "audio", "codec_name": "aac", "sample_rate": "48000",
                 "channels": 2, "channel_layout": "stereo"}
        issue8.verify_media_format({"streams": [video, audio]})
        for stream, change in ((0, {"codec_name": "mpeg4"}), (0, {"pix_fmt": "yuv444p"}),
                               (1, {"codec_name": "mp3"}), (1, {"sample_rate": "8000"}),
                               (1, {"channels": 1}), (1, {"channel_layout": "mono"})):
            streams = [dict(video), dict(audio)]
            streams[stream].update(change)
            with self.subTest(change=change), self.assertRaises(ValueError):
                issue8.verify_media_format({"streams": streams})


if __name__ == "__main__":
    unittest.main()
