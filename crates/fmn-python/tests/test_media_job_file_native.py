"""Installed media CLI acceptance; run with FMN_TEST_NATIVE=1 to require the wheel."""
import hashlib
import json
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import unittest
import wave

from test_parallel_batch_native import native_wheel_available

SOURCE = '''from pathlib import Path
from manimlib import Scene, Square, RIGHT
class Lesson(Scene):
    def __init__(self, side=1.0, gain=0.0, **kwargs):
        super().__init__(**kwargs)
        self.side, self.gain = side, gain
    def construct(self):
        square = Square(side_length=self.side, fill_opacity=1)
        self.add(square)
        self.add_sound(str(Path(__file__).parent / "tone.wav"), gain=self.gain)
        self.wait(0.25)
        self.play(square.animate.shift(RIGHT), run_time=0.25)
        self.wait(0.25)
'''


@unittest.skipUnless(native_wheel_available(), "requires the built FrankenManim native wheel")
class NativeMediaJobFileTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.source = self.root / "scenes.py"
        self.source.write_text(SOURCE)
        self.job_file = self.root / "jobs.json"
        self.jobs = [{"name": "small", "scene": "Lesson", "scene_kwargs": {"side": 0.5, "gain": -6}},
                     {"name": "large", "scene": "Lesson", "scene_kwargs": {"side": 2.0, "gain": 0}}]
        self.job_file.write_text(json.dumps(dict(schema="fmn-python.source-jobs", version=1, jobs=self.jobs)))
        with wave.open(str(self.root / "tone.wav"), "wb") as stream:
            stream.setnchannels(1)
            stream.setsampwidth(2)
            stream.setframerate(8000)
            stream.writeframes(struct.pack("<h", 4096) * 8000)

    def command(self, directory, format, *extra):
        result = subprocess.run([sys.executable, "-m", "fmn_python", "--robot", str(self.source),
            "--workers", "2", "--jobs-file", str(self.job_file), "--format", format,
            "--video_dir", str(directory), "--resolution", "64x36", "--fps", "8", "--threads", "1",
            *extra], cwd=self.root, capture_output=True, text=True, timeout=180, check=False)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        report = json.loads(result.stdout)["batch"]
        self.assertTrue(report["ok"])
        self.assertEqual(report["execution"], "spawn")
        self.assertEqual([row["name"] for row in report["outcomes"]], ["small", "large"])
        return report

    def sequential(self, directory, format, **options):
        import manimlib
        from fmn_python import RenderJob, render_scenes
        from fmn_python.scene_loading import SceneSource
        with SceneSource(self.source, manimlib.Scene) as loaded:
            result = render_scenes([RenderJob(row["name"], loaded.scenes[row["scene"]], row["scene_kwargs"])
                                    for row in self.jobs], directory, format=format,
                                   resolution=(64, 36), fps=8, threads=1, **options)
        self.assertTrue(result.ok)
        return result

    def test_native_png_y4m_and_wav_match_sequential_parameters_and_resume(self):
        for format, prefix in (("png", b"\x89PNG\r\n\x1a\n"), ("y4m", b"YUV4MPEG2"), ("wav", b"RIFF")):
            with self.subTest(format=format):
                directory = self.root / format
                checkpoint = self.root / (format + "-progress.json")
                args = ("--checkpoint", str(checkpoint), "--resume-key", "media-variants-v1")
                report = self.command(directory, format, *args)
                expected = self.sequential(self.root / (format + "-serial"), format)
                data, stamps = [], []
                for row, reference in zip(report["outcomes"], expected.outcomes):
                    path = Path(row["destination"])
                    raw = path.read_bytes()
                    self.assertTrue(raw.startswith(prefix))
                    self.assertEqual(raw, reference.destination.read_bytes())
                    self.assertEqual(hashlib.sha256(raw).hexdigest(), row["result"]["digest"])
                    self.assertEqual(len(raw), row["result"]["bytes"])
                    data.append(raw)
                    stamps.append(path.stat().st_mtime_ns)
                self.assertNotEqual(data[0], data[1])
                recovered = self.command(directory, format, *args, "--resume")
                self.assertEqual(recovered["reused"], ["small", "large"])
                self.assertEqual([Path(row["destination"]).stat().st_mtime_ns for row in recovered["outcomes"]], stamps)

    def test_native_segment_excerpts_and_companion_png_match_one_pass_outputs(self):
        # Compare actual artifacts rather than relying on the collection JSON's shape.
        actual_root, serial_root = self.root / "clips", self.root / "serial-clips"
        self.command(actual_root, "png_sequence", "--subdivide", "--save-last-frame", "-n", "1,3")
        expected = self.sequential(serial_root, "png_sequence", subdivide=True, save_last_frame=True,
                                   animation_range=(1, 3))
        for outcome in expected.outcomes:
            self.assertEqual([clip.play_index for clip in outcome.result.primary.segments], [1, 2])
            self.assertTrue(outcome.result.completed)
        expected_files = sorted(path.relative_to(serial_root) for path in serial_root.rglob("*.png"))
        actual_files = sorted(path.relative_to(actual_root) for path in actual_root.rglob("*.png"))
        self.assertGreaterEqual(len(actual_files), 6)
        self.assertEqual(actual_files, expected_files)
        for relative in expected_files:
            self.assertEqual((actual_root / relative).read_bytes(), (serial_root / relative).read_bytes())


if __name__ == "__main__":
    unittest.main()
