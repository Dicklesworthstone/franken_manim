"""Installed-wheel live recording API, checkpoint retries and real encoded video."""
from pathlib import Path
import copy
import hashlib
import os
import shutil
import subprocess
import tempfile

import numpy as np
import manimlib as m
from fmn_python import RecordingSession, SceneConsole, record_scene, render_scene


def make_scene():
    scene = m.Scene()
    square = m.Square(side_length=1, fill_color=m.WHITE, fill_opacity=1, stroke_width=0)
    square.shift(2 * m.LEFT)
    scene.add(square)
    scene.wait(0.125)
    return scene, square


def check(receipt):
    assert receipt is not None and not receipt.certified
    data = receipt.destination.read_bytes()
    assert receipt.bytes == len(data) and receipt.digest == hashlib.sha256(data).hexdigest()


def repeated_contexts(root):
    scene, square = make_scene()
    camera = scene.camera
    shape = camera.get_pixel_shape()
    for index in range(2):
        with record_scene(scene, root / f"context-{index}.y4m", resolution=(96, 54), threads=1) as clip:
            assert isinstance(clip, RecordingSession)
            scene.play(square.animate.shift(m.RIGHT), run_time=0.125, rate_func=m.linear)
        check(clip.result)
        assert clip.result.frame_count == 4
        assert (clip.start_frame, clip.end_frame) == (4 + index * 4, 8 + index * 4)
        # The list starts with the frame, as in the Reference (fm-6l2i).
        assert scene.mobjects[0] is scene.frame and scene.mobjects[1] is square
        assert scene.camera is camera

        assert camera.get_pixel_shape() == shape
    with record_scene(scene, root / "static.gif", resolution=(96, 54), threads=1) as clip:
        pass
    check(clip.result)
    assert clip.result.frame_count == 1 and clip.start_frame == clip.end_frame == 12


def recorded_checkpoint_cells(root):
    scene, square = make_scene()
    # Frame publication is independent of the optional preview/readback channel.
    with SceneConsole(scene, {"square": square, "RIGHT": m.RIGHT}, capture=False) as editor:
        for index in range(2):
            editor.run_cell("# clip\nscene.play(square.animate.shift(RIGHT), run_time=0.125)",
                            record_to=root / f"cell-{index}.y4m",
                            recording_options={"resolution": (96, 54), "threads": index + 1})
            check(editor.last_recording)
            assert editor.last_recording.frame_count == 4
            assert m._portal_scene_clock(scene) == (30, 8)
            assert abs(square.get_center()[0] + 1) < 1e-5
        assert (root / "cell-0.y4m").read_bytes() == (root / "cell-1.y4m").read_bytes()
        previous = editor.last_recording
        error = KeyboardInterrupt("recorded cell interrupted")
        editor.namespace["interrupted"] = error
        try:
            editor.run_cell("# clip\nscene.wait(0.125)\nraise interrupted",
                            record_to=root / "cancelled.y4m",
                            recording_options={"resolution": (96, 54), "threads": 1})
        except KeyboardInterrupt as caught:
            assert caught is error
        else:
            raise AssertionError("recorded-cell interruption was swallowed")
        assert editor.last_recording is previous
        assert not (root / "cancelled.y4m").exists()
        editor.run_cell("# clip\nscene.wait(0.125)", record_to=root / "retry.y4m",
                        recording_options={"resolution": (96, 54), "threads": 1})
        check(editor.last_recording)
        assert editor.last_recording.frame_count == 4


def errors_preserve_existing_scene(root):
    scene, square = make_scene()
    try:
        with record_scene(scene, root / "failure.y4m", resolution=(96, 54), threads=1):
            scene.wait(0.125)
            raise ValueError("authored")
    except ValueError as error:
        assert str(error) == "authored"
    assert not (root / "failure.y4m").exists()
    assert scene.mobjects[1] is square and m._portal_scene_clock(scene) == (30, 8)

    with record_scene(scene, root / "recovered.y4m", resolution=(96, 54), threads=1) as clip:
        scene.wait(0.125)
    check(clip.result)
    assert clip.result.frame_count == 4
    for kwargs in ({"fps": 8}, {"format": "png"}, {"format": "svg"}):
        try:
            record_scene(scene, root / "refused", **kwargs)
        except ValueError:
            pass
        else:
            raise AssertionError("invalid live recording admitted")
    assert m._portal_scene_clock(scene) == (30, 12)


def native_video(root):
    executable = shutil.which("ffmpeg")
    if not executable:
        assert os.environ.get("FMN_REQUIRE_FFMPEG") != "1", "ffmpeg required by this acceptance run"
        print("live recording video acceptance skipped: ffmpeg unavailable")
        return
    for format, transparent in (("mp4", False), ("mov", True)):
        scene, square = make_scene()
        if transparent:
            scene.camera.background_rgba[3] = 0
        with record_scene(scene, root / ("clip." + format), resolution=(96, 54), threads=1) as clip:
            scene.play(square.animate.shift(3 * m.RIGHT), run_time=0.125, rate_func=m.linear)
        check(clip.result)
        assert clip.result.frame_count == 4
        assert clip.result.ffmpeg_invocations
        invocation = clip.result.ffmpeg_invocations[0]
        assert invocation["encoder"] == ("qtrle" if transparent else "libx264")
        assert "-vf" not in invocation["argv"]
        decoded = subprocess.run([executable, "-v", "error", "-nostdin", "-i", str(clip.destination),
                                  "-frames:v", "5", "-pix_fmt", "rgba", "-f", "rawvideo", "pipe:1"],
                                 capture_output=True, check=True, timeout=30)
        frames = np.frombuffer(decoded.stdout, dtype=np.uint8).reshape(-1, 54, 96, 4)
        assert len(frames) == 4
        assert np.all(frames[:, 0, 0, 3] == (0 if transparent else 255))
        centers = []
        for frame in frames:
            ys, xs = np.nonzero(frame[:, :, 0] > 220)
            assert len(xs) > 10
            centers.append(xs.mean())
        assert all(b > a + 3 for a, b in zip(centers, centers[1:])), centers
    print("live recording MP4 and transparent MOV independently decoded")


def configured_raster_preflight(root):
    scene, square = make_scene()
    square.rotate(0.27)
    camera, core, clock = scene.camera, scene.camera._core, m._portal_scene_clock(scene)
    before = square.get_points().copy()
    saved = copy.deepcopy(m.manim_config)
    try:
        for section, key, value, error_type in (
            ("render", "aa", "unknown-aa", ValueError),
            ("cache", "max_bytes", object(), TypeError),
            ("directories", "cache", object(), TypeError),
        ):
            m.manim_config.clear()
            m.manim_config.update(copy.deepcopy(saved))
            m.manim_config.setdefault(section, {})[key] = value
            destination = root / ("invalid-" + section)
            try:
                with record_scene(scene, destination, format="png_sequence", resolution=(32, 18), threads=1):
                    raise AssertionError("invalid typed raster config acquired a recording")
            except error_type as error:
                assert key in str(error), str(error)
            else:
                raise AssertionError("invalid typed raster config was accepted")
            assert not destination.exists()
            assert m._portal_scene_clock(scene) == clock
            assert scene.camera is camera and scene.camera._core is core
            assert scene.mobjects[1] is square
            np.testing.assert_array_equal(square.get_points(), before)

        m.manim_config.clear()
        m.manim_config.update(copy.deepcopy(saved))
        m.manim_config.setdefault("render", {})["aa"] = "ssaa2x"
        with record_scene(scene, root / "configured-aa", format="png_sequence", resolution=(32, 18), threads=1) as clip:
            pass
        assert clip.result.frame_count == 1 and m._portal_scene_clock(scene) == clock
        assert scene.mobjects[1] is square and scene.camera._core is core

        class SameGeometry(m.Scene):
            def construct(self):
                self.add(square.copy())

        full = render_scene(SameGeometry, root / "configured-aa.png", format="png", resolution=(32, 18), threads=1)
        frames = list(clip.result.destination.glob("*.png"))
        assert len(frames) == 1
        assert frames[0].read_bytes() == full.destination.read_bytes()
        m.manim_config["render"]["aa"] = "adaptive"
        control = render_scene(SameGeometry, root / "adaptive-aa.png", format="png", resolution=(32, 18), threads=1)
        assert control.destination.read_bytes() != full.destination.read_bytes(), "fixture must distinguish AA policies"
        # Soundtrack output has no raster/cache consumer and must not inspect
        # settings that are irrelevant to its output session.
        m.manim_config.setdefault("directories", {})["cache"] = object()
        m.manim_config.setdefault("cache", {})["max_bytes"] = object()
        with record_scene(scene, root / "non-raster.wav", format="wav", resolution=(32, 18), threads=1) as sound:
            scene.wait(0.125)
        check(sound.result)
    finally:
        m.manim_config.clear()
        m.manim_config.update(saved)


with tempfile.TemporaryDirectory(prefix="fmn-recording-sessions-") as temporary:
    root = Path(temporary)
    repeated_contexts(root)
    recorded_checkpoint_cells(root)
    errors_preserve_existing_scene(root)
    native_video(root)
    configured_raster_preflight(root)
print("native recording sessions acceptance passed")
