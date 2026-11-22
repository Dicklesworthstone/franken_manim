"""HDR color conversion through real portal owners, not metadata-only output.

Refusals always run. Real encode/decode checks require a host ffmpeg offering
10-bit libx264; the returned count includes only successfully decoded HDR frames.
"""
import contextlib
import copy
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

import numpy as np
import manimlib as m
from fmn_python import record_scene, render_scene
from fmn_python.batch_rendering import _restored_outcome
from fmn_python.console_rendering import try_render_cli


HDR_PROFILE = dict(primaries="bt2020", transfer="smpte2084", matrix="bt2020nc",
                   range="limited", reference_white_nits=203.0, peak_nits=1000.0)


def run_hdr_video_acceptance():
    with tempfile.TemporaryDirectory(prefix="fmn-hdr-portal-") as directory:
        root = Path(directory)

        class Unexecuted(m.Scene):
            def construct(self):
                raise AssertionError("invalid HDR must fail before construct")

        refusals = [
            ("mp4", dict(gamma=2), "identity"),
            ("mp4", dict(saturation=.5), "identity"),
            ("mov", dict(background_opacity=.5), "opaque"),
            ("mp4", dict(pixel_format="rgba"), "P010"),
            ("mp4", dict(pixel_format="nv12"), "P010"),
            ("mp4", dict(hdr_pq=1), "bool"),
            ("mp4", dict(hdr_pq="true"), "bool"),
            ("mp4", dict(hdr_pq=None), "bool"),
        ]
        refusals.extend((format, {}, "MP4/MOV")
                        for format in ("png", "png_sequence", "gif", "y4m", "wav", "svg"))
        for index, (format, overrides, message) in enumerate(refusals):
            scene = Unexecuted()
            scene.file_writer.hdr_pq = True
            scene.file_writer.ffmpeg_bin = str(root / "must-not-start-an-encoder")
            for name, value in overrides.items():
                if name == "background_opacity":
                    scene.camera.background_rgba[3] = value
                else:
                    setattr(scene.file_writer, name, value)
            destination = root / f"refusal-{index}.{format}"
            destination.write_bytes(b"existing output")
            try:
                render_scene(scene, destination, format=format,
                             resolution=(96, 54), fps=8, threads=1)
            except (TypeError, ValueError, RuntimeError) as error:
                assert message in str(error), (format, overrides, error)
            else:
                raise AssertionError(("invalid HDR was accepted", format, overrides))
            assert destination.read_bytes() == b"existing output"

        required = (os.environ.get("FMN_REQUIRE_FFMPEG") == "1"
                    or os.environ.get("FMN_REQUIRE_FULL_INPUTS") == "1")
        ffmpeg = shutil.which("ffmpeg")
        if ffmpeg is None:
            assert not required, "HDR portal acceptance requires real ffmpeg"
            return 0
        capability = subprocess.run([ffmpeg, "-hide_banner", "-h", "encoder=libx264"],
                                    capture_output=True, text=True, check=True, timeout=30)
        if not any(wire in capability.stdout for wire in
                   ("p010le", "yuv420p10le", "yuv422p10le", "yuv444p10le")):
            assert not required, "HDR portal acceptance requires 10-bit libx264"
            return 0

        def decode(path, width=96, height=54):
            raw = subprocess.run([ffmpeg, "-v", "error", "-i", str(path),
                                  "-f", "rawvideo", "-pix_fmt", "yuv420p10le", "-"],
                                 capture_output=True, check=True, timeout=30).stdout
            return np.frombuffer(raw, dtype="<u2").reshape(-1, width * height * 3 // 2)

        def check_receipt(report, width=96, height=54):
            assert report["certified"] is False and report["hdr_pq"] == HDR_PROFILE, report
            encodes = [job for job in report["ffmpeg_invocations"] if job["encoder"] is not None]
            assert len(encodes) == 1, report
            job = encodes[0]
            assert job["encoder"] == "libx264", job
            argv = job["argv"]
            input_at = argv.index("-i")
            for flag, value in (("-color_primaries", "bt2020"), ("-color_trc", "smpte2084"),
                                ("-colorspace", "bt2020nc")):
                positions = [index for index, token in enumerate(argv) if token == flag]
                assert len(positions) == 2 and positions[0] < input_at < positions[1], argv
                assert all(argv[index + 1] == value for index in positions), argv
            assert argv[argv.index("-pix_fmt") + 1] == "p010le" and "-vf" not in argv, argv
            artifact = Path(report["destination"])
            # ISO BMFF colr: BT.2020 primaries, PQ transfer and BT.2020 matrix.
            data = artifact.read_bytes()
            assert any(b"colr" + kind + b"\x00\x09\x00\x10\x00\x09" in data
                       for kind in (b"nclx", b"nclc")), artifact
            frames = decode(artifact, width, height)
            assert len(frames) == report["frame_count"], report
            return frames

        class SerialScene(m.Scene):
            def update_mobjects(self, dt):
                # A public callback makes the native serial fallback observable.
                super().update_mobjects(dt)
                self.callback_count = getattr(self, "callback_count", 0) + 1

        def movie(label, scene_type=m.Scene, threads=1, camera=False, hdr=True):
            scene = scene_type()
            scene.set_background_color(m.BLACK)
            writer = scene.file_writer
            writer.hdr_pq = hdr
            writer.pixel_format = "auto" if hdr else "p010le"
            writer.video_codec = "libx264"
            writer.video_crf = 0
            writer.video_preset = "ultrafast"
            writer.ffmpeg_bin = ffmpeg
            if camera:
                scene.frame.reorient(20, 15)
            with scene.render_session(root / (label + ".mp4"), format="mp4",
                                      resolution=(96, 54), fps=8, threads=threads) as session:
                square = m.Square(side_length=2, fill_color=m.WHITE, fill_opacity=1,
                                  stroke_width=0).shift(m.LEFT)
                scene.add(square)
                scene.play(m.Transform(square, square.copy().shift(2 * m.RIGHT)),
                           run_time_frames=4, rate_func=m.linear)
                # A live setting mutation cannot relabel the admitted generation.
                writer.hdr_pq = False
                scene.wait_frames(2)
            report = session.result.as_dict()
            assert report["frame_count"] == 6 and scene.get_time() == .75, report
            stats = scene._render_performance
            assert stats["pure_play_frames"] == (4 if scene_type is m.Scene else 0), stats
            assert stats["conversion_reuses"] == 2, stats
            assert stats["camera_frames"] == (6 if camera else 0), stats
            frames = check_receipt(report) if hdr else decode(report["destination"])
            assert len({frame.tobytes() for frame in frames[:4]}) == 4
            assert np.array_equal(frames[-3], frames[-2]) and np.array_equal(frames[-2], frames[-1])
            maximum = frames[:, :96 * 54].max()
            if hdr:
                # ST2084(203 nits) is approximately code 573 in limited 10-bit Y.
                assert 571 <= maximum <= 575, maximum
            else:
                assert maximum == 940 and "hdr_pq" not in report, (maximum, report)
            return scene, report, frames

        scene, report, baseline = movie("pure-1")
        _, _, pure_team = movie("pure-4", threads=4)
        _, _, serial = movie("serial-4", scene_type=SerialScene, threads=4)
        _, _, camera_pure = movie("camera-pure", threads=4, camera=True)
        _, _, camera_serial = movie("camera-serial", scene_type=SerialScene, threads=4, camera=True)
        movie("ordinary-p010", hdr=False)
        np.testing.assert_array_equal(baseline, pure_team)
        np.testing.assert_array_equal(baseline, serial)
        np.testing.assert_array_equal(camera_pure, camera_serial)
        verified_frames = sum(len(frames) for frames in
                              (baseline, pure_team, serial, camera_pure, camera_serial))

        row = dict(name="HdrScene", destination=report["destination"], result=report)
        assert _restored_outcome(row).result.hdr_pq == HDR_PROFILE
        for key, value in (("transfer", "srgb"), ("peak_nits", -1),
                           ("reference_white_nits", float("nan"))):
            corrupt = copy.deepcopy(row)
            corrupt["result"]["hdr_pq"][key] = value
            try:
                _restored_outcome(corrupt)
            except ValueError as error:
                assert "hdr_pq" in str(error), error
            else:
                raise AssertionError("corrupt HDR checkpoint receipt was restored")

        scene.file_writer.hdr_pq = True
        with record_scene(scene, root / "live.mov", format="mov",
                          resolution=(96, 54), fps=8, threads=1) as recording:
            scene.wait_frames(2)
        live = check_receipt(recording.result.as_dict())
        assert len(live) == 2
        np.testing.assert_array_equal(live[0], baseline[-1])
        verified_frames += len(live)

        source = root / "scene.py"
        source.write_text(
            "from manimlib import *\n"
            "class First(Scene):\n"
            "    def construct(self):\n"
            "        self.add(Square(side_length=2, fill_color=WHITE, fill_opacity=1, stroke_width=0))\n"
            "        self.wait_frames(1)\n"
            "        self.wait_frames(1)\n"
            "class Second(First):\n    pass\n", encoding="utf-8")

        def hdr_receipts(value):
            if isinstance(value, dict):
                if "hdr_pq" in value and "destination" in value:
                    yield value
                else:
                    for child in value.values():
                        yield from hdr_receipts(child)
            elif isinstance(value, list):
                for child in value:
                    yield from hdr_receipts(child)

        cases = [
            (("First", "--output", str(root / "console.mp4")), 1),
            (("--write_all", "--video_dir", str(root / "batch")), 2),
            (("First", "--subdivide", "--output", str(root / "clips")), 2),
            (("First", "--save-last-frame", "--output", str(root / "paired.mp4")), 1),
        ]
        for selection, count in cases:
            stdout, stderr = io.StringIO(), io.StringIO()
            with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                status = try_render_cli(m, ["--robot", str(source), *selection,
                    "--hdr-pq", "--format", "mp4", "--resolution", "32x18", "--fps", "8",
                    "--threads", "1", "--ffmpeg_bin", ffmpeg, "--vcodec", "libx264",
                    "--crf", "0", "--preset", "ultrafast"])
            result = json.loads(stdout.getvalue())
            assert status == 0, (result, stderr.getvalue())
            receipts = list(hdr_receipts(result))
            assert len(receipts) == count, result
            for receipt in receipts:
                verified_frames += len(check_receipt(receipt, 32, 18))
        assert verified_frames == 42, verified_frames
        return verified_frames
