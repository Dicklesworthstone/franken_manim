"""Native populated-scene recording: no replacement arena, clock or encoder."""
from pathlib import Path
import hashlib
import struct
import tempfile
import wave

import manimlib


def begin(scene, path, format="y4m", threads=1):
    fps, frame = manimlib._portal_scene_clock(scene)
    actual = manimlib._portal_begin_recording(scene, str(path), format, 96, 54, fps, threads)
    assert actual == frame
    assert manimlib._portal_scene_clock(scene) == (fps, frame)
    return frame


def finish(scene, path):
    receipt = scene._finish_render()
    assert Path(receipt[0]) == path
    if path.is_file():
        data = path.read_bytes()
        assert receipt[2] == len(data)
        assert receipt[3] == hashlib.sha256(data).hexdigest()
    return receipt


def y4m_centers(path):
    header, body = path.read_bytes().split(b"\n", 1)
    assert header == b"YUV4MPEG2 W96 H54 F30:1 Ip A1:1 C420mpeg2"
    size = 96 * 54 * 3 // 2
    assert len(body) % (size + 6) == 0
    centers = []
    for offset in range(0, len(body), size + 6):
        assert body[offset:offset + 6] == b"FRAME\n"
        luma = body[offset + 6:offset + 6 + 96 * 54]
        xs = [i % 96 for i, value in enumerate(luma) if value > 200]
        assert len(xs) > 10
        centers.append(sum(xs) / len(xs))
    return centers


def populated_scene_and_repeated_clips(root):
    scene = manimlib.Scene()
    square = manimlib.Square(side_length=1, fill_color=manimlib.WHITE, fill_opacity=1, stroke_width=0)
    square.shift(2 * manimlib.LEFT)
    scene.add(square)
    scene.wait(0.125)  # Binary-exact duration: ceil(30/8) = four samples.
    view = square.data
    first = root / "first.y4m"
    assert begin(scene, first) == 4, manimlib._portal_scene_clock(scene)
    # The list starts with the frame, as in the Reference (fm-6l2i).
    assert scene.mobjects[0] is scene.frame and scene.mobjects[1] is square

    view["point"][:, 0] += 0.25
    assert abs(square.get_center()[0] + 1.75) < 1e-5
    scene.play(square.animate.shift(3 * manimlib.RIGHT), run_time=0.125, rate_func=manimlib.linear)
    before = manimlib._portal_scene_clock(scene)
    receipt = finish(scene, first)
    assert receipt[1] == 4, receipt
    assert manimlib._portal_scene_clock(scene) == before == (30, 8)
    xs = y4m_centers(first)
    assert len(xs) == 4 and all(b > a + 3 for a, b in zip(xs, xs[1:])), xs
    scene.wait(0.125)
    second = root / "second.y4m"
    assert begin(scene, second, threads=4) == 12
    scene.play(square.animate.shift(3 * manimlib.LEFT), run_time=0.125, rate_func=manimlib.linear)
    assert finish(scene, second)[1] == 4
    xs = y4m_centers(second)
    assert all(b < a - 3 for a, b in zip(xs, xs[1:])), xs
    assert scene.mobjects[0] is scene.frame and scene.mobjects[1] is square

    assert manimlib._portal_scene_clock(scene) == (30, 16)


def thread_replay_and_native_formats(root):
    outputs = []
    for workers in (1, 4):
        scene = manimlib.Scene()
        scene.add(manimlib.Square(fill_opacity=1))
        scene.wait(0.125)
        path = root / f"replay-{workers}.y4m"
        begin(scene, path, threads=workers)
        scene.wait(0.125)
        finish(scene, path)
        outputs.append(path.read_bytes())
    assert outputs[0] == outputs[1]
    for format in ("gif", "png_sequence"):
        scene = manimlib.Scene()
        scene.add(manimlib.Square(fill_opacity=1))
        scene.wait(0.125)
        path = root / format
        begin(scene, path, format)
        scene.wait(0.125)
        assert finish(scene, path)[1] == 4
        if format == "gif":
            assert path.read_bytes().startswith(b"GIF89a")
        else:
            frames = sorted(path.glob("*.png"))
            assert len(frames) == 4
            assert all(frame.read_bytes().startswith(b"\x89PNG\r\n\x1a\n") for frame in frames)


def audio_is_relative_and_excludes_earlier_cues(root):
    source = root / "cue.wav"
    with wave.open(str(source), "wb") as stream:
        stream.setnchannels(1)
        stream.setsampwidth(2)
        stream.setframerate(48000)
        stream.writeframes(struct.pack("<hh", 4000, -4000) * 800)
    scene = manimlib.Scene()
    scene.add_sound(str(root / "earlier-missing.wav"))
    scene.wait(1)
    path = root / "insert.wav"
    assert begin(scene, path, "wav") == 30
    scene.wait(0.5)
    scene.add_sound(str(source))
    scene.wait(0.5)
    receipt = finish(scene, path)
    assert receipt[1] == 48000, receipt
    with wave.open(str(path), "rb") as stream:
        assert stream.getframerate() == 48000 and stream.getnchannels() == 2
        samples = struct.unpack("<" + "h" * (stream.getnframes() * 2), stream.readframes(stream.getnframes()))
    assert all(value == 0 for value in samples[:24000 * 2])
    assert any(value != 0 for value in samples[24000 * 2:25600 * 2])
    inputs = scene._render_audio_inputs
    assert len(inputs) == 1 and inputs[0]["decoder"] == "native-wav"
    assert not scene._render_invocations
    assert manimlib._portal_scene_clock(scene) == (30, 60)


def cancellation_and_ownership(root):
    scene = manimlib.Scene()
    square = manimlib.Square(fill_opacity=1)
    scene.add(square)
    scene.wait(0.125)
    path = root / "cancelled.y4m"
    begin(scene, path)
    scene.wait(0.125)
    try:
        begin(scene, root / "nested.y4m")
    except RuntimeError:
        pass
    else:
        raise AssertionError("nested generation was admitted")
    scene._abort_render()
    assert not path.exists()
    assert scene.mobjects[1] is square and manimlib._portal_scene_clock(scene) == (30, 8)

    after = root / "after-abort.y4m"
    begin(scene, after)
    scene.wait(0.125)
    assert finish(scene, after)[1] == 4
    previous = after.read_bytes()
    try:
        begin(scene, after)
        scene.wait(0.125)
        finish(scene, after)
    except Exception:
        scene._abort_render()
    else:
        raise AssertionError("existing recording was overwritten")
    assert after.read_bytes() == previous
    for format, fps, threads in (("png", 30, 1), ("svg", 30, 1), ("y4m", 8, 1), ("y4m", 30, 0)):
        before = manimlib._portal_scene_clock(scene)
        destination = root / f"refused-{format}-{fps}-{threads}"
        try:
            manimlib._portal_begin_recording(scene, str(destination), format, 96, 54, fps, threads)
        except (ValueError, RuntimeError):
            pass
        else:
            raise AssertionError("invalid recording was admitted")
        assert manimlib._portal_scene_clock(scene) == before and not destination.exists()


def insert_scene(root):
    scene = manimlib.Scene(
        camera_config={"resolution": (96, 54), "fps": 30},
        file_writer_config={"output_directory": str(root), "file_name": "lesson.part",
                            "movie_file_extension": ".y4m", "quiet": True},
    )
    square = manimlib.Square(side_length=1, fill_color=manimlib.WHITE,
                            fill_opacity=1, stroke_width=0).shift(2 * manimlib.LEFT)
    scene.add(square)
    scene.wait(0.125)
    return scene, square, scene.file_writer


def file_writer_numbered_inserts(root):
    scene, square, writer = insert_scene(root)
    camera, view = scene.camera, square.data
    original = Path(writer.get_movie_file_path())
    original.write_bytes(b"original movie must survive every insert")
    cached_movie_path = writer.movie_file_path
    occupied = Path(writer.get_insert_file_path(0))
    assert occupied == root / "inserts" / "lesson.part_0.y4m"
    assert not occupied.parent.exists(), "path queries created output directories"
    occupied.parent.mkdir()
    occupied.write_bytes(b"existing insert")
    assert writer.begin_insert() is None
    first = Path(writer.inserted_file_path)
    assert first.name == "lesson.part_1.y4m" and writer.write_to_movie
    assert manimlib._portal_scene_clock(scene) == (30, 4)
    view["point"][:, 0] += 0.25
    scene.play(square.animate.shift(3 * manimlib.RIGHT), run_time=0.125,
               rate_func=manimlib.linear)
    assert writer.end_insert() is None
    xs = y4m_centers(first)
    assert len(xs) == 4 and all(b > a + 3 for a, b in zip(xs, xs[1:])), xs
    assert not writer.write_to_movie and "_fmn_owned_render_session" not in vars(scene)
    assert scene.camera is camera and scene.mobjects[1] is square
    assert writer.movie_file_path == cached_movie_path
    assert manimlib._portal_scene_clock(scene) == (30, 8)
    assert original.read_bytes() == b"original movie must survive every insert"
    assert occupied.read_bytes() == b"existing insert"
    # An empty insert still publishes the current frame once, with no clock
    # tick, arena replacement, or accidental overwrite of the previous clip.
    writer.begin_insert()
    second = Path(writer.inserted_file_path)
    assert second.name == "lesson.part_2.y4m"
    writer.end_insert()
    assert len(y4m_centers(second)) == 1
    assert manimlib._portal_scene_clock(scene) == (30, 8)
    assert first.read_bytes() != second.read_bytes()


def file_writer_insert_owner_refusals(root):
    from fmn_python.recording import RecordingSession
    scene, square, writer = insert_scene(root)
    writer.begin_insert()
    owner = vars(scene)["_fmn_owned_render_session"]
    try:
        writer.begin_insert()
    except RuntimeError as error:
        assert "active insert" in str(error)
    else:
        raise AssertionError("nested insert stole the native generation")
    assert vars(scene)["_fmn_owned_render_session"] is owner and writer.write_to_movie
    scene.wait(0.125)
    writer.end_insert()
    external = root / "external.y4m"
    begin(scene, external)
    try:
        writer.begin_insert()
    except RuntimeError:
        pass
    else:
        raise AssertionError("insert stole an externally owned native generation")
    assert not writer.write_to_movie
    scene.wait(0.125)
    assert finish(scene, external)[1] == 4
    # The public owner is also protected, before native output admission.
    with RecordingSession(scene, root / "owned.y4m", threads=1, _native=manimlib) as recording:
        try:
            writer.begin_insert()
        except RuntimeError as error:
            assert "already has" in str(error)
        else:
            raise AssertionError("insert stole an owned recording")
        assert vars(scene)["_fmn_owned_render_session"] is recording
        scene.wait(0.125)
    assert recording.result.frame_count == 4 and scene.mobjects[1] is square
    # The exclusive animation-range boundary is successful termination, not
    # a failed animation. The caller can still publish the recorded prefix.
    scene.end_at_animation_number = scene.num_plays + 1
    writer.begin_insert()
    selected = Path(writer.inserted_file_path)
    scene.wait(0.125)
    try:
        scene.wait(0.125)
    except manimlib.EndScene:
        pass
    else:
        raise AssertionError("insert ignored the exclusive animation range")
    assert writer.write_to_movie
    writer.end_insert()
    assert len(y4m_centers(selected)) == 4
    writer.movie_file_extension = ".png"
    try:
        writer.begin_insert()
    except ValueError:
        pass
    else:
        raise AssertionError("insert admitted a still-image recording")
    assert not writer.write_to_movie and "_fmn_owned_render_session" not in vars(scene)


def file_writer_failed_insert_releases_owner(root):
    scene, square, writer = insert_scene(root)
    error = LookupError("insert animation failed")
    class FailedAnimation(manimlib.Animation):
        def interpolate_mobject(self, alpha):
            if alpha > 0:
                self.mobject.shift(0.25 * manimlib.RIGHT)
                raise error
    writer.begin_insert()
    path = Path(writer.inserted_file_path)
    try:
        scene.play(FailedAnimation(square), run_time=0.125)
    except LookupError as caught:
        assert caught is error
    else:
        raise AssertionError("insert swallowed an authored animation failure")
    assert not path.exists() and not writer.write_to_movie
    assert "_fmn_owned_render_session" not in vars(scene)
    assert "_fmn_file_writer_insert" not in vars(scene)
    assert "_fmn_file_writer_insert" not in vars(writer)
    assert scene.mobjects[1] is square and square.get_center()[0] > -2
    assert not square._is_updating_suspended()
    try:
        writer.end_insert()
    except RuntimeError as caught:
        assert "no active insert" in str(caught)
    else:
        raise AssertionError("failed insert could still publish partial frames")
    writer.begin_insert()
    assert Path(writer.inserted_file_path) == path
    scene.wait(0.125)
    writer.end_insert()
    assert len(y4m_centers(path)) == 4


def file_writer_insert_publish_race(root):
    scene, _, writer = insert_scene(root)
    writer.begin_insert()
    path = Path(writer.inserted_file_path)
    scene.wait(0.125)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(b"competing publication")
    try:
        writer.end_insert()
    except Exception:
        pass
    else:
        raise AssertionError("insert overwrote a competing published file")
    assert path.read_bytes() == b"competing publication"
    assert not writer.write_to_movie and "_fmn_owned_render_session" not in vars(scene)
    writer.begin_insert()
    next_path = Path(writer.inserted_file_path)
    assert next_path.name == "lesson.part_1.y4m"
    writer.end_insert()
    assert len(y4m_centers(next_path)) == 1


INSERT_CASES = (file_writer_numbered_inserts, file_writer_insert_owner_refusals,
                file_writer_failed_insert_releases_owner, file_writer_insert_publish_race)


def main():
    cases = INSERT_CASES if globals().get("_fmn_insert_only", False) else (
        populated_scene_and_repeated_clips, thread_replay_and_native_formats,
        audio_is_relative_and_excludes_earlier_cues, cancellation_and_ownership, *INSERT_CASES,
    )
    with tempfile.TemporaryDirectory(prefix="fmn-live-recording-") as temp:
        root = Path(temp)
        for case in cases:
            case_root = root / case.__name__
            case_root.mkdir()
            case(case_root)
    print(f"native live recording acceptance passed: {len(cases)} cases")
    return len(INSERT_CASES)


_fmn_insert_checks = main()
