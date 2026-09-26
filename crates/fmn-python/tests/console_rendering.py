"""Installed-wheel CLI acceptance using real Lumen/Reel output and Scene state.

Invoke the public console entrypoint in its host interpreter. Only argv and
text streams are scoped; no parser, render session, Scene or native sink is
substituted. Fixtures and output artifacts are retained for investigation.
"""
from __future__ import annotations

import contextlib
import hashlib
import io
import json
from pathlib import Path
import struct
import sys
import tempfile
import textwrap
from types import SimpleNamespace
import wave

import numpy as np
import manimlib as m
from fmn_python.__main__ import main


ROOT = Path(tempfile.mkdtemp(prefix="fmn-console-native-"))
SUPPORT = "fmn_console_support_" + ROOT.name.rsplit("-", 1)[-1]
# The console's scoped loader gives each invocation a fresh copy of this
# project-local module and evicts it on exit (scene_loading.SceneSource), so
# scene-side events go to a log beside it rather than to a module global.
EVENTS = ROOT / (SUPPORT + ".events")
(ROOT / (SUPPORT + ".py")).write_text(textwrap.dedent('''\
    import json
    from pathlib import Path

    import manimlib as m

    class _Events:
        def append(self, event):
            with Path(__file__).with_suffix(".events").open("a") as log:
                log.write(json.dumps(list(event)) + "\\n")

    events = _Events()

    class Motion(m.Scene):
        direction = 1

        def __init__(self):
            events.append(("construct", type(self).__name__))
            super().__init__()

        def construct(self):
            assert self.camera.fps == 8, self.camera.fps
            assert self.camera.get_pixel_shape() == (96, 54)
            square = m.Square(side_length=1, fill_color=m.WHITE,
                              fill_opacity=1, stroke_width=0)
            square.shift(-2 * self.direction * m.RIGHT + m.UP)
            self.add(square)
            self.wait(1 / 8)
            self.play(square.animate.shift(4 * self.direction * m.RIGHT),
                      run_time=1 / 4, rate_func=m.linear)
            print("construct chatter:", type(self).__name__)

        def tear_down(self):
            events.append(("teardown", type(self).__name__))
            print("teardown chatter:", type(self).__name__)
'''))


def source(name, body):
    path = ROOT / (name + ".py")
    path.write_text(
        f"import manimlib as m\nfrom {SUPPORT} import Motion, events\n"
        "events.append(('import', __file__))\nprint('module chatter')\n"
        + textwrap.dedent(body)
    )
    return path


SCENES = source("motion", '''\
    class Right(Motion):
        pass
    class Left(Motion):
        direction = -1
''')


def invoke(scene_source, destination, *selectors, format="y4m", threads=1, extra=()):
    argv = ["fmn-python", "--robot", str(scene_source), *selectors,
            "--format", format, "--resolution", "96x54", "--fps", "8",
            "--threads", str(threads), "--video_dir", str(destination), *extra]
    out, err = io.StringIO(), io.StringIO()
    original_argv, original_path = sys.argv, list(sys.path)
    try:
        sys.argv = argv
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = main()
    finally:
        sys.argv = original_argv
    assert sys.path == original_path, "source import scope leaked"
    lines = out.getvalue().splitlines()
    assert len(lines) == 1, out.getvalue()
    report = json.loads(lines[0])
    assert report["schema"] == "fmn-python.cli" and report["version"] == 1
    assert report["exit"]["code"] == code
    assert report["status"] == ("success" if code == 0 else "error")
    return code, report, err.getvalue()


class _EventLog(list):
    def clear(self):
        super().clear()
        EVENTS.write_text("")


def support():
    lines = EVENTS.read_text().splitlines() if EVENTS.exists() else []
    return SimpleNamespace(events=_EventLog(tuple(json.loads(line)) for line in lines))


def constructed():
    return [name for kind, name in support().events if kind == "construct"]


def receipt(path, result):
    data = Path(path).read_bytes()
    assert len(data) == result["bytes"]
    assert hashlib.sha256(data).hexdigest() == result["digest"]
    assert result["certified"] is False
    return data


def y4m(path, expected_frames):
    header, data = Path(path).read_bytes().split(b"\n", 1)
    assert header == b"YUV4MPEG2 W96 H54 F8:1 Ip A1:1 C420mpeg2", header
    size = 96 * 54 * 3 // 2
    assert len(data) == expected_frames * (size + 6), len(data)
    frames = []
    for offset in range(0, len(data), size + 6):
        assert data[offset:offset + 6] == b"FRAME\n"
        frames.append(np.frombuffer(data[offset + 6:offset + 6 + 96 * 54],
                                    dtype=np.uint8).reshape(54, 96))
    return frames


def motion(frames, direction):
    centers = []
    for frame in frames:
        ys, xs = np.nonzero(frame > 200)
        assert len(xs) > 10, len(xs)
        assert ys.mean() < 25, "world UP must appear above image center"
        centers.append(float(xs.mean()))
    assert all(direction * (b - a) > 5 for a, b in zip(centers, centers[1:])), centers


def ordinary_scene_capture():
    path = ROOT / "single.y4m"
    code, report, stderr = invoke(SCENES, path, "Right")
    assert code == 0, report
    assert report["kind"] == "render" and report["rendered"] is True
    assert report["fps"] == 8 and report["resolution"] == [96, 54]
    assert report["frame_count"] == 3
    receipt(path, report)
    motion(y4m(path, 3), 1)
    assert "module chatter" in stderr and "construct chatter: Right" in stderr
    assert "teardown chatter: Right" in stderr
    assert constructed() == ["Right"]


def selected_order_and_output():
    support().events.clear()
    output = ROOT / "selected"
    code, report, _ = invoke(SCENES, output, "Left", "Right")
    assert code == 0, report
    rows = report["batch"]["outcomes"]
    assert [row["name"] for row in rows] == ["Left", "Right"]
    assert constructed() == ["Left", "Right"]
    assert sum(kind == "import" for kind, _ in support().events) == 1
    assert report["batch"]["counts"] == dict(succeeded=2, failed=0, cancelled=0, not_run=0)
    for row, direction in zip(rows, (-1, 1)):
        assert row["status"] == "succeeded"
        assert row["result"]["frame_count"] == 3
        receipt(row["destination"], row["result"])
        motion(y4m(row["destination"], 3), direction)


def write_all_alias_and_thread_repeatability():
    support().events.clear()
    output = ROOT / "all-four-threads"
    code, report, _ = invoke(SCENES, output, "-a", threads=4)
    assert code == 0, report
    assert constructed() == ["Left", "Right"]
    for row in report["batch"]["outcomes"]:
        receipt(row["destination"], row["result"])
        assert Path(row["destination"]).read_bytes() == (ROOT / "selected" / (row["name"] + ".y4m")).read_bytes()


def missing_selection_starts_no_scene():
    support().events.clear()
    output = ROOT / "missing"
    code, report, _ = invoke(SCENES, output, "Right", "NotDeclared")
    assert code == 5 and report["kind"] == "scene-load-failed", report
    assert constructed() == []
    assert not output.exists()


def early_scene_termination_publishes():
    path = source("early", '''\
        class Early(Motion):
            def construct(self):
                self.add(m.Square(fill_opacity=1))
                self.wait(1 / 8)
                raise m.EndScene("normal early completion")
    ''')
    output = ROOT / "early.y4m"
    code, report, _ = invoke(path, output)
    assert code == 0 and report["frame_count"] == 1, report
    receipt(output, report)
    y4m(output, 1)
    assert ("teardown", "Early") in support().events


def interrupt_after_capture_does_not_publish():
    path = source("interrupted", '''\
        class Interrupted(Motion):
            def construct(self):
                super().construct()
                raise KeyboardInterrupt()
    ''')
    output = ROOT / "interrupted.y4m"
    code, report, _ = invoke(path, output)
    assert code == 130 and report["kind"] == "render-interrupted", report
    assert report["artifact_published"] is False
    assert not output.exists()
    assert ("teardown", "Interrupted") in support().events
    # A later invocation must not inherit the cancelled generation.
    code, result, _ = invoke(SCENES, ROOT / "after-interrupt.y4m", "Right")
    assert code == 0 and result["frame_count"] == 3, result


def system_exit_zero_is_not_success():
    path = source("system_exit", '''\
        class Exiting(Motion):
            def construct(self):
                super().construct()
                raise SystemExit(0)
    ''')
    output = ROOT / "system-exit.y4m"
    code, report, _ = invoke(path, output)
    assert code == 5 and report["kind"] == "render-interrupted", report
    assert not output.exists()


def batch_fail_fast_and_keep_going():
    path = source("failure_batch", '''\
        class First(Motion): pass
        class Failed(Motion):
            def construct(self):
                super().construct()
                raise ValueError("authored failure after capture")
        class Last(Motion): pass
    ''')
    for keep in (False, True):
        support().events.clear()
        output = ROOT / ("keep-going" if keep else "fail-fast")
        code, report, _ = invoke(path, output, "First", "Failed", "Last",
                                 extra=("--keep-going",) if keep else ())
        assert code == 5, report
        assert report["batch"]["counts"] == dict(succeeded=2 if keep else 1,
                                                  failed=1, cancelled=0, not_run=0 if keep else 1)
        assert constructed() == (["First", "Failed", "Last"] if keep else ["First", "Failed"])
        first = report["batch"]["outcomes"][0]
        receipt(first["destination"], first["result"])
        assert not (output / "Failed.y4m").exists()
        assert (output / "Last.y4m").exists() == keep


def batch_interrupt_retains_completed_artifacts():
    path = source("cancel_batch", '''\
        class First(Motion): pass
        class Cancelled(Motion):
            def construct(self):
                super().construct()
                raise KeyboardInterrupt()
        class Last(Motion): pass
    ''')
    support().events.clear()
    output = ROOT / "cancel-batch"
    code, report, _ = invoke(path, output, "First", "Cancelled", "Last", extra=("--keep-going",))
    assert code == 130 and report["kind"] == "render-batch-interrupted", report
    assert report["batch"]["counts"] == dict(succeeded=1, failed=0, cancelled=1, not_run=1)
    assert constructed() == ["First", "Cancelled"]
    first = report["batch"]["outcomes"][0]
    receipt(first["destination"], first["result"])
    y4m(first["destination"], 3)
    assert not (output / "Cancelled.y4m").exists()
    assert not (output / "Last.y4m").exists()


def later_destination_preflight_preserves_files():
    support().events.clear()
    output = ROOT / "preflight"
    output.mkdir()
    (output / "Right.y4m").write_bytes(b"do not replace")
    code, report, _ = invoke(SCENES, output, "Left", "Right")
    assert code == 6, report
    assert constructed() == []
    assert not (output / "Left.y4m").exists()
    assert (output / "Right.y4m").read_bytes() == b"do not replace"


def wav_output_keeps_native_sample_units():
    cue = ROOT / "cue.wav"
    with wave.open(str(cue), "wb") as stream:
        stream.setnchannels(1)
        stream.setsampwidth(2)
        stream.setframerate(48000)
        stream.writeframes(struct.pack("<h", 2500) * 6000)
    path = source("soundtrack", f'''\
        class Soundtrack(Motion):
            def construct(self):
                assert self.camera.fps == 8
                self.add_sound({str(cue)!r})
                self.wait(1 / 4)
    ''')
    output = ROOT / "soundtrack.wav"
    code, report, _ = invoke(path, output, format="wav")
    assert code == 0, report
    assert report["sample_frames"] == report["frame_count"] == 12000
    assert report["sample_rate"] == 48000 and report["channels"] == 2
    receipt(output, report)
    with wave.open(str(output), "rb") as stream:
        assert (stream.getframerate(), stream.getnchannels(), stream.getsampwidth(), stream.getnframes()) == (48000, 2, 2, 12000)
        samples = np.frombuffer(stream.readframes(12000), dtype="<i2")
    assert np.max(np.abs(samples)) > 0, "native soundtrack must not be silent"


CASES = (
    ordinary_scene_capture, selected_order_and_output,
    write_all_alias_and_thread_repeatability, missing_selection_starts_no_scene,
    early_scene_termination_publishes, interrupt_after_capture_does_not_publish,
    system_exit_zero_is_not_success, batch_fail_fast_and_keep_going,
    batch_interrupt_retains_completed_artifacts, later_destination_preflight_preserves_files,
    wav_output_keeps_native_sample_units,
)
assert len(CASES) == 11
for case in CASES:
    case()
    print("native console acceptance:", case.__name__)
print(f"native console acceptance: {len(CASES)} cases passed; retained artifacts: {ROOT}")
