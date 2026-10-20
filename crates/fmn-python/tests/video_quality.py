"""Actual portal quality admission and x264 bitstream evidence.

Run by portal_video::quality's production extension test. The bitstream probe
uses ffmpeg only to demux/copy H.264, never to re-encode or infer our settings.
"""
import pathlib
import re
import shutil
import subprocess
import tempfile
import os
import time

import manimlib as m
from fmn_python import render_scene


class QualityPicture(m.Scene):
    def construct(self):
        self.add(m.Square(side_length=2, fill_color=m.BLUE, fill_opacity=1,
                          stroke_width=0))
        self.wait(0.25)


def quality_scene(**settings):
    scene = QualityPicture()
    for name, value in settings.items():
        setattr(scene.file_writer, name, value)
    return scene


def x264_user_data(annex_b):
    """Read user_data_unregistered SEI, excluding unrelated NAL payloads."""
    for nal in re.split(b"\x00\x00(?:\x00)?\x01", annex_b):
        if not nal or nal[0] & 31 != 6:
            continue
        rbsp = nal[1:].replace(b"\x00\x00\x03", b"\x00\x00")
        at = 0
        while at < len(rbsp):
            fields = []
            for _ in range(2):
                value = 0
                while at < len(rbsp):
                    byte = rbsp[at]
                    at += 1
                    value += byte
                    if byte != 255:
                        break
                else:
                    return None
                fields.append(value)
            kind, size = fields
            end = at + size
            if end > len(rbsp):
                return None
            payload = rbsp[at:end]
            if kind == 5 and len(payload) >= 16 and payload[16:].startswith(b"x264 - core "):
                return payload[16:].rstrip(b"\0").decode("utf-8")
            at = end
    return None


def check_probe():
    text = b"x264 - core TEST - options: crf=16.0 subme=8 psy_rd=0.40:0.00"
    payload = b"0123456789abcdef" + text
    nal = b"\x06\x05" + bytes([len(payload)]) + payload + b"\x80"
    assert x264_user_data(b"\x00\x00\x00\x01" + nal) == text.decode()
    assert x264_user_data(b"\x00\x00\x01\x65" + nal[1:]) is None
    assert x264_user_data(b"\x00\x00\x01" + nal[:-3]) is None
    assert x264_user_data(b"\x00\x00\x01\x06\xff") is None


check_probe()
ffmpeg = shutil.which("ffmpeg")
if os.environ.get("FMN_REQUIRE_FFMPEG") == "1" or os.environ.get("FMN_REQUIRE_FULL_INPUTS") == "1":
    assert ffmpeg, "real ffmpeg is required for portal quality acceptance"

with tempfile.TemporaryDirectory(prefix="fmn-portal-quality-") as directory:
    root = pathlib.Path(directory)
    missing = root / "no-encoder"
    failures = [
        ({"video_crf": 52}, "video_crf"),
        ({"video_crf": True}, "not bool"),
        ({"video_crf": 1.5}, "integer"),
        ({"video_preset": "slow -vf eq"}, "video_preset"),
        ({"video_tune": "animation,grain"}, "video_tune"),
        ({"video_bitrate": 0}, "video_bitrate"),
        ({"video_bitrate": False}, "not bool"),
        ({"audio_bitrate": 0}, "audio_bitrate"),
        ({"audio_bitrate": False}, "not bool"),
        ({"audio_bitrate": 1.5}, "integer"),
        ({"video_codec": "libx265", "video_tune": "film"}, "libx264"),
        ({"video_codec": "h264_nvenc", "video_crf": 18}, "crf"),
        ({"video_codec": "hevc_videotoolbox", "video_preset": "slow"}, "preset"),
        ({"video_crf": 16, "video_bitrate": 8000000}, "mutually exclusive"),
        ({"ffmpeg_timeout": 0}, "ffmpeg_timeout"),
        ({"ffmpeg_timeout": False}, "not bool"),
        ({"ffmpeg_timeout": 1.5}, "integer"),
        ({"ffmpeg_timeout": (1 << 64) - 1}, "monotonic clock"),
        ({"ffmpeg_max_log_bytes": 0}, "ffmpeg_max_log_bytes"),
        ({"ffmpeg_max_artifact_bytes": 0}, "ffmpeg_max_artifact_bytes"),
        ({"ffmpeg_max_input_bytes": 0}, "ffmpeg_max_input_bytes"),
        ({"ffmpeg_max_input_bytes": 1}, "one output frame"),
    ]
    for index, (settings, fragment) in enumerate(failures):
        target = root / f"refused-{index}.mp4"
        target.write_bytes(b"preserve existing movie")
        scene = quality_scene(ffmpeg_bin=str(missing), **settings)
        try:
            render_scene(scene, target, resolution=(96, 54), fps=8, threads=1)
        except (TypeError, ValueError, OverflowError) as error:
            assert fragment in str(error), (settings, str(error))
            assert target.read_bytes() == b"preserve existing movie"
            assert not hasattr(scene, "_fmn_owned_render_session")
        else:
            raise AssertionError(f"incompatible quality was accepted: {settings}")

    # A Python getter failure is not turned into an absent optional setting.
    scene = quality_scene(ffmpeg_bin=str(missing))
    original = scene.file_writer
    marker = RuntimeError("authored quality getter failed")

    class BrokenWriter:
        def __getattr__(self, name):
            return getattr(original, name)

        @property
        def video_crf(self):
            raise marker

    scene.file_writer = BrokenWriter()
    try:
        render_scene(scene, root / "getter.mp4", resolution=(96, 54), fps=8, threads=1)
    except RuntimeError as error:
        assert error is marker, error
    else:
        raise AssertionError("authored descriptor exception was suppressed")
    assert not (root / "getter.mp4").exists()

    scene = quality_scene(ffmpeg_bin=str(missing))
    original = scene.file_writer
    marker = RuntimeError("authored ffmpeg limit getter failed")

    class BrokenLimitsWriter:
        def __getattr__(self, name):
            return getattr(original, name)

        @property
        def ffmpeg_timeout(self):
            raise marker

    scene.file_writer = BrokenLimitsWriter()
    try:
        render_scene(scene, root / "limit-getter.mp4", resolution=(96, 54), fps=8, threads=1)
    except RuntimeError as error:
        assert error is marker, error
    else:
        raise AssertionError("authored limit descriptor exception was suppressed")
    assert not (root / "limit-getter.mp4").exists()

    scene = quality_scene(ffmpeg_bin=str(missing))
    original = scene.file_writer

    class MutatingLimitsWriter:
        def __getattr__(self, name):
            return getattr(original, name)

        @property
        def ffmpeg_timeout(self):
            scene.add(m.Square())
            return 60

    scene.file_writer = MutatingLimitsWriter()
    try:
        render_scene(scene, root / "limit-owner.mp4", resolution=(96, 54), fps=8, threads=1)
    except RuntimeError as error:
        assert "before Scene construction mutates engine state" in str(error), error
    else:
        raise AssertionError("resource getter bypassed generation ownership")
    assert not (root / "limit-owner.mp4").exists()

    if ffmpeg:
        class DelayedPicture(QualityPicture):
            def construct(self):
                super().construct()
                time.sleep(2.1)

        for timeout, succeeds in ((1, False), (60, True)):
            scene = DelayedPicture()
            scene.file_writer.ffmpeg_bin = ffmpeg
            scene.file_writer.ffmpeg_timeout = timeout
            target = root / f"watchdog-{timeout}.mp4"
            try:
                report = render_scene(scene, target, resolution=(96, 54), fps=8, threads=1)
            except RuntimeError as error:
                assert not succeeds and "timeout" in str(error), error
                assert not target.exists(), "timed-out encode published an artifact"
                assert scene._render_ffmpeg_limits["timeout_seconds"] == timeout
            else:
                assert succeeds and report.frame_count == 2
                assert report.ffmpeg_limits["timeout_seconds"] == timeout
                assert report.as_dict()["ffmpeg_limits"] == report.ffmpeg_limits
            print(f"portal-ffmpeg-limits timeout={timeout} published={target.exists()}")

        scene = quality_scene(ffmpeg_bin=ffmpeg, ffmpeg_timeout=37, ffmpeg_max_log_bytes=131072,
                              ffmpeg_max_artifact_bytes=2 << 20, ffmpeg_max_input_bytes=1 << 20)
        report = render_scene(scene, root / "limited.mp4", resolution=(96, 54), fps=8, threads=1)
        assert report.ffmpeg_limits == dict(timeout_seconds=37, timeout_nanoseconds=0,
                                            max_log_bytes=131072, max_artifact_bytes=1 << 20,
                                            max_input_bytes=1 << 20, keep_workdir=False)
        copied = report.as_dict()
        copied["ffmpeg_limits"]["timeout_seconds"] = 99
        assert report.ffmpeg_limits["timeout_seconds"] == 37

        for settings, fragment in (({"ffmpeg_max_input_bytes": 7776}, "cumulative input"),
                                   ({"ffmpeg_max_artifact_bytes": 1}, "artifact")):
            scene = quality_scene(ffmpeg_bin=ffmpeg, **settings)
            target = root / (next(iter(settings)) + ".mp4")
            try:
                render_scene(scene, target, resolution=(96, 54), fps=8, threads=1)
            except RuntimeError as error:
                assert fragment in str(error).lower(), error
            else:
                raise AssertionError("ffmpeg byte limit was ignored")
            assert not target.exists()

        for extension in ("mp4", "mov"):
            for crf in (16, 28):
                scene = quality_scene(ffmpeg_bin=ffmpeg, video_crf=crf,
                                      video_preset="slow", video_tune="animation")
                target = root / f"quality-{crf}.{extension}"
                report = render_scene(scene, target, resolution=(96, 54), fps=8, threads=1)
                assert report.frame_count == 2 and not report.certified
                invocation, = report.ffmpeg_invocations
                assert invocation["encoder"] == "libx264"
                argv = invocation["argv"]
                input_at = argv.index("-i")
                for flag, value in (("-crf", str(crf)), ("-preset", "slow"), ("-tune", "animation")):
                    assert argv.count(flag) == 1 and argv.index(flag) > input_at + 1
                    assert argv[argv.index(flag) + 1] == value, argv
                assert "-b:v" not in argv
                demuxed = subprocess.run(
                    [ffmpeg, "-v", "error", "-nostdin", "-i", str(target), "-map", "0:v:0",
                     "-c:v", "copy", "-bsf:v", "h264_mp4toannexb", "-f", "h264", "pipe:1"],
                    capture_output=True, check=True, timeout=30,
                ).stdout
                sei = x264_user_data(demuxed)
                assert sei is not None, "missing actual x264 SEI"
                words = sei.split()
                assert f"crf={crf}.0" in words and "subme=8" in words, sei
                assert any(word.startswith("psy_rd=0.40:") for word in words), sei
                print(f"portal-quality {extension} crf={crf}: {sei}")

        # The real native WAV mix is muxed at the selected AAC target; video
        # remains stream-copy and carries the same H.264 bytes as the default-AAC run.
        import math
        import struct
        import wave
        tone = root / "audio-quality.wav"
        with wave.open(str(tone), "wb") as stream:
            stream.setparams((1, 2, 48000, 0, "NONE", "not compressed"))
            stream.writeframes(b"".join(struct.pack("<h", int(12000 * math.sin(i * 2 * math.pi * 440 / 48000)))
                                      for i in range(12000)))
        class AudioPicture(QualityPicture):
            def construct(self):
                self.add_sound(str(tone))
                super().construct()
        compressed_tone = root / "limit-tone.flac"
        subprocess.run([ffmpeg, "-v", "error", "-nostdin", "-i", str(tone),
                        "-c:a", "flac", str(compressed_tone)],
                       capture_output=True, check=True, timeout=30)

        class CompressedPicture(QualityPicture):
            def construct(self):
                self.add_sound(str(compressed_tone))
                super().construct()

        assert compressed_tone.stat().st_size < 32768
        scene = CompressedPicture()
        scene.file_writer.ffmpeg_bin = ffmpeg
        scene.file_writer.ffmpeg_max_input_bytes = 32768
        report = render_scene(scene, root / "expanded-audio.wav", resolution=(96, 54), fps=8, threads=1)
        assert report.bytes > 32768, "fixture must expand beyond its compressed input allowance"
        assert report.ffmpeg_limits["max_input_bytes"] == 32768
        assert report.ffmpeg_limits["max_artifact_bytes"] == 8 << 30
        assert len(report.ffmpeg_invocations) == 1

        scene = CompressedPicture()
        scene.file_writer.ffmpeg_bin = ffmpeg
        report = render_scene(scene, root / "default-audio.wav", resolution=(96, 54), fps=8, threads=1)
        assert report.ffmpeg_limits["max_input_bytes"] == 64 << 20
        assert report.ffmpeg_limits["max_artifact_bytes"] == 8 << 30

        for extension in ("mp4", "wav"):
            scene = CompressedPicture()
            scene.file_writer.ffmpeg_bin = ffmpeg
            scene.file_writer.ffmpeg_timeout = 37
            scene.file_writer.ffmpeg_max_artifact_bytes = 8192
            target = root / ("limited-audio." + extension)
            try:
                render_scene(scene, target, resolution=(96, 54), fps=8, threads=1)
            except RuntimeError as error:
                assert "sound cue" in str(error) and "artifact" in str(error).lower(), error
            else:
                raise AssertionError("compressed audio ignored the render's ffmpeg artifact cap")
            assert not target.exists()

        reference_video = None
        for index, bitrate in enumerate((None, 96000, 192000)):
            scene = AudioPicture()
            scene.file_writer.ffmpeg_bin = ffmpeg
            scene.file_writer.audio_bitrate = bitrate
            scene.file_writer.ffmpeg_timeout = 37
            scene.file_writer.ffmpeg_max_artifact_bytes = 1 << 20
            scene.file_writer.ffmpeg_max_input_bytes = 2 << 20
            target = root / f"audio-quality-{index}.mp4"
            report = render_scene(scene, target, resolution=(96, 54), fps=8, threads=1)
            encode, mux = report.ffmpeg_invocations
            assert report.ffmpeg_limits["timeout_seconds"] == 37
            assert report.ffmpeg_limits["max_artifact_bytes"] == 1 << 20
            assert "-b:a" not in encode["argv"]
            args = mux["argv"]
            assert args[args.index("-c:v") + 1] == "copy"
            assert args[args.index("-c:a") + 1] == "aac"
            actual = int(args[args.index("-b:a") + 1]) if "-b:a" in args else None
            assert actual == bitrate, (actual, bitrate)
            packets = subprocess.run([ffmpeg, "-v", "error", "-nostdin", "-i", str(target),
                                      "-map", "0:v:0", "-c:v", "copy", "-bsf:v", "h264_mp4toannexb",
                                      "-f", "h264", "pipe:1"], capture_output=True, check=True, timeout=30).stdout
            assert packets
            if reference_video is None:
                reference_video = packets
            else:
                assert packets == reference_video, "AAC policy changed the encoded video stream"
            pcm = subprocess.run([ffmpeg, "-v", "error", "-nostdin", "-i", str(target), "-map", "0:a:0",
                                  "-f", "s16le", "pipe:1"], capture_output=True, check=True, timeout=30).stdout
            assert len(pcm) >= 12000 * 2 and any(pcm), "published AAC soundtrack missing or silent"

        # CRF zero must not disappear through a truthiness check.
        report = render_scene(quality_scene(ffmpeg_bin=ffmpeg, video_crf=0),
                              root / "lossless.mp4", resolution=(96, 54), fps=8, threads=1)
        argv = report.ffmpeg_invocations[0]["argv"]
        assert argv[argv.index("-crf") + 1] == "0"

        # None and absent attributes preserve the old encoder-default argv.
        report = render_scene(quality_scene(ffmpeg_bin=ffmpeg, video_crf=None,
                                             video_preset=None, video_tune=None, video_bitrate=None),
                              root / "defaults.mp4", resolution=(96, 54), fps=8, threads=1)
        argv = report.ffmpeg_invocations[0]["argv"]
        assert all(flag not in argv for flag in ("-crf", "-preset", "-tune", "-b:v")), argv
        assert report.ffmpeg_limits == dict(timeout_seconds=86400, timeout_nanoseconds=0,
                                            max_log_bytes=1 << 20, max_artifact_bytes=7_776_000_000,
                                            max_input_bytes=7_776_000_000, keep_workdir=False)

print("portal-quality: admission verified; actual x264 bitstream checks=" + str(bool(ffmpeg)))
