"""Real native output collections through fresh parallel scene workers."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import shutil
import wave
import sys
import tempfile
import unittest
from unittest.mock import patch

from test_parallel_batch_native import native_wheel_available

PYTHON = Path(__file__).resolve().parents[1] / "python"
# Append, as test_parallel_batch_native does: the source tree's manimlib has no compiled
# extension, so it must never shadow the installed wheel being tested.
if str(PYTHON) not in sys.path:
    sys.path.append(str(PYTHON))

SOURCE = '''from pathlib import Path
from manimlib import Scene, Square, RIGHT
class First(Scene):
    def construct(self):
        root = Path(__file__).parent
        with (root / (type(self).__name__ + ".calls")).open("a") as stream:
            stream.write("construct\\n")
        if type(self).__name__ == "Second" and (root / "fail").exists():
            raise RuntimeError("transient failure")
        square = Square()
        self.add(square)
        self.wait(0.1)
        self.play(square.animate.shift(RIGHT), run_time=0.2)
        self.wait(0.1)
class Second(First): pass
class Broken(Scene):
    def construct(self):
        self.add(Square())
        self.wait(0.1)
        raise RuntimeError("failed after a clip")
class Interrupted(Scene):
    def construct(self):
        self.add(Square())
        self.wait(0.1)
        raise KeyboardInterrupt()
'''


@unittest.skipUnless(native_wheel_available(), "requires the built FrankenManim native wheel")
class NativeParallelOutputsTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.source = self.root / "scenes.py"
        self.source.write_text(SOURCE)
        from fmn_python.parallel_batch import render_source_scenes
        self.render = render_source_scenes

    def run_batch(self, names=("First", "Second"), **options):
        return self.render(self.source, self.root / "output", names, workers=2,
                           resolution=(64, 48), fps=10, threads=1, **options)

    def verify_artifact(self, result):
        path = result.destination
        if result.format == "png_sequence":
            frames = sorted(path.glob("*.png"))
            self.assertEqual(len(frames), result.frame_count)
            self.assertEqual(sum(item.stat().st_size for item in frames), result.bytes)
            for frame in frames:
                self.assertEqual(frame.read_bytes()[:8], b"\x89PNG\r\n\x1a\n")
        else:
            data = path.read_bytes()
            self.assertEqual(len(data), result.bytes)
            self.assertEqual(hashlib.sha256(data).hexdigest(), result.digest)
            self.assertTrue(data.startswith(b"GIF") if result.format == "gif" else
                            data.startswith(b"\x89PNG\r\n\x1a\n"))

    def test_complete_pairs_execute_once_and_keep_native_receipts(self):
        report = self.run_batch(format="gif", save_last_frame=True)
        self.assertTrue(report.ok)
        for outcome in report.outcomes:
            pair = outcome.result
            self.assertTrue(pair.completed)
            self.assertEqual(pair.still_destination, self.root / "output" / (outcome.name + ".png"))
            self.verify_artifact(pair.primary)
            self.verify_artifact(pair.still)
            self.assertEqual((self.root / (outcome.name + ".calls")).read_text().splitlines(), ["construct"])

    def test_pair_resume_verifies_both_outputs_before_skipping_constructors(self):
        (self.root / "fail").touch()
        options = dict(format="png_sequence", save_last_frame=True, continue_on_error=True,
                       checkpoint=self.root / "progress.json", resume_key="inputs-v1")
        first = self.run_batch(**options)
        self.assertEqual([item.status for item in first.outcomes], ["succeeded", "failed"])
        pair = first.outcomes[0].result
        self.verify_artifact(pair.primary)
        self.verify_artifact(pair.still)
        old = pair.still.destination.stat()
        # Rename a transient failure trigger; entry-source bytes are unchanged.
        (self.root / "fail").rename(self.root / "repaired")
        resumed = self.run_batch(**options, resume=True)
        self.assertTrue(resumed.ok)
        self.assertEqual(resumed.reused, ("First",))
        self.assertEqual(pair.still.destination.stat().st_mtime_ns, old.st_mtime_ns)
        self.assertEqual((self.root / "First.calls").read_text().splitlines(), ["construct"])
        self.assertEqual(len((self.root / "Second.calls").read_text().splitlines()), 2)
        pair.still.destination.write_bytes(b"tampered")
        with self.assertRaises(ValueError), patch("fmn_python.parallel_batch.run_process_jobs") as workers:
            self.run_batch(**options, resume=True)
        workers.assert_not_called()

    def test_subdivision_and_final_frame_match_sequential_native_outputs(self):
        options = dict(format="png_sequence", subdivide=True, save_last_frame=True,
                       animation_range=(1, 3))
        report = self.run_batch(**options)
        from fmn_python.batch_rendering import render_scenes
        from fmn_python.scene_loading import SceneSource
        with SceneSource(self.source, __import__("manimlib").Scene) as loaded:
            serial = render_scenes({name: loaded.scenes[name] for name in ("First", "Second")},
                                  self.root / "serial", resolution=(64, 48), fps=10, threads=1, **options)
        self.assertTrue(report.ok)
        for actual, expected in zip(report.outcomes, serial.outcomes):
            pair = actual.result
            self.assertTrue(pair.completed)
            self.assertTrue(pair.primary.completed)
            self.assertEqual([clip.play_index for clip in pair.primary.segments], [1, 2])
            self.assertEqual([clip.kind for clip in pair.primary.segments], ["play", "wait"])
            self.verify_artifact(pair.still)
            self.assertEqual(pair.still.destination.read_bytes(), expected.result.still.destination.read_bytes())
            for clip, reference in zip(pair.primary.segments, expected.result.primary.segments):
                self.verify_artifact(clip.render)
                self.assertEqual([p.read_bytes() for p in sorted(clip.render.destination.glob("*.png"))],
                                 [p.read_bytes() for p in sorted(reference.render.destination.glob("*.png"))])

    def test_later_failure_keeps_receipted_clips_in_the_failed_outcome(self):
        report = self.run_batch(("Broken", "First"), format="gif", subdivide=True, continue_on_error=True)
        self.assertEqual([item.status for item in report.outcomes], ["failed", "succeeded"])
        partial = report.outcomes[0].result
        self.assertFalse(partial.completed)
        self.assertEqual(len(partial.segments), 1)
        self.verify_artifact(partial.segments[0].render)
        self.assertIn("failed after a clip", report.outcomes[0].message)

    def test_interruption_preserves_partial_paired_clips(self):
        with self.assertRaises(KeyboardInterrupt) as caught:
            self.run_batch(("Interrupted",), format="gif", subdivide=True, save_last_frame=True)
        outcome = caught.exception.render_batch_result.outcomes[0]
        self.assertEqual(outcome.status, "cancelled")
        self.assertFalse(outcome.result.completed)
        self.assertIsNone(outcome.result.still)
        self.assertEqual(len(outcome.result.primary.segments), 1)
        self.verify_artifact(outcome.result.primary.segments[0].render)
        self.assertFalse(outcome.result.still_destination.exists())

    @unittest.skipUnless(os.name == "posix", "requires cooperative SIGINT cancellation")
    def test_timeout_preserves_cooperatively_returned_clip_receipts(self):
        self.source.write_text(SOURCE + '\nimport time\nclass Slow(Scene):\n'
            '    def construct(self):\n'
            '        self.add(Square())\n'
            '        self.wait(0.1)\n'
            '        Path(__file__).with_suffix(".started").touch()\n'
            '        time.sleep(60)\n')
        report = self.run_batch(("Slow",), format="gif", subdivide=True,
                                job_timeout=4, continue_on_error=True)
        outcome = report.outcomes[0]
        self.assertTrue(self.source.with_suffix(".started").exists())
        self.assertEqual(outcome.status, "failed")
        self.assertEqual(outcome.error_type, "TimeoutError")
        self.assertFalse(outcome.result.completed)
        self.assertEqual(len(outcome.result.segments), 1)
        self.verify_artifact(outcome.result.segments[0].render)

    def test_late_still_collision_keeps_published_primary_and_foreign_file(self):
        self.source.write_text(SOURCE + '''
from fmn_python.rendering import RenderSession
finish = RenderSession.finish
def collide(self):
    receipt = finish(self)
    self.destination.with_suffix(".png").write_bytes(b"foreign")
    return receipt
RenderSession.finish = collide
''')
        report = self.run_batch(("First",), format="gif", save_last_frame=True, continue_on_error=True)
        self.assertEqual(report.outcomes[0].status, "failed")
        pair = report.outcomes[0].result
        self.assertFalse(pair.completed)
        self.assertIsNone(pair.still)
        self.verify_artifact(pair.primary)
        self.assertEqual(pair.still_destination.read_bytes(), b"foreign")

    def test_companion_and_journal_conflicts_refuse_before_any_worker(self):
        from fmn_python.parallel_batch import render_source_scenes
        still = self.root / "output" / "Second.png"
        still.parent.mkdir()
        still.write_bytes(b"owned elsewhere")
        with patch("fmn_python.parallel_batch.run_process_jobs") as workers:
            with self.assertRaises(FileExistsError):
                self.run_batch(format="gif", save_last_frame=True)
            with self.assertRaises(ValueError):
                render_source_scenes(self.source, self.root / "fresh", ["First"], format="gif",
                    save_last_frame=True, checkpoint=self.root / "fresh" / "First.png", resume_key="x")
        workers.assert_not_called()
        self.assertEqual(still.read_bytes(), b"owned elsewhere")
        self.assertFalse((self.root / "First.calls").exists())

    def test_segment_budget_and_empty_collection_are_reported_without_fake_clips(self):
        report = self.run_batch(("First",), format="gif", subdivide=True,
                                max_segments=1, continue_on_error=True)
        self.assertEqual(report.outcomes[0].status, "failed")
        self.assertEqual(len(report.outcomes[0].result.segments), 1)
        self.verify_artifact(report.outcomes[0].result.segments[0].render)
        self.source.write_text("from manimlib import Scene\nclass Empty(Scene): pass\n")
        report = self.render(self.source, self.root / "empty", ["Empty"], workers=1,
                             subdivide=True, format="gif", resolution=(64, 48), fps=10)
        self.assertTrue(report.ok)
        self.assertTrue(report.outcomes[0].result.completed)
        self.assertEqual(report.outcomes[0].result.segments, ())

    def test_wav_clip_windows_and_final_image_preserve_audio_receipts(self):
        self.source.write_text('from pathlib import Path\nfrom manimlib import Scene, Square\nclass Sound(Scene):\n    def construct(self):\n        self.add(Square())\n        self.add_sound(str(Path(__file__).with_suffix(".wav")))\n        self.wait(0.125)\n        self.wait(0.125)\n')
        with wave.open(str(self.source.with_suffix(".wav")), "wb") as stream:
            stream.setparams((1, 2, 48000, 0, "NONE", "not compressed"))
            stream.writeframes(b"\x10\x00" * 48000)
        # Binary-exact 1/8-second calls at 8 FPS each span exactly one sample
        # on the native rational clock: 6,000 audio sample frames per clip.
        report = self.render(self.source, self.root / "output", ["Sound"], workers=1,
                             resolution=(64, 48), fps=8, threads=1, format="wav",
                             subdivide=True, save_last_frame=True)
        self.assertTrue(report.ok)
        pair = report.outcomes[0].result
        self.assertEqual(pair.primary.sample_frames, 12000)
        self.assertEqual(pair.primary.frame_count, 0)
        self.verify_artifact(pair.still)
        for segment in pair.primary.segments:
            receipt = segment.render
            data = receipt.destination.read_bytes()
            self.assertEqual(hashlib.sha256(data).hexdigest(), receipt.digest)
            self.assertEqual(receipt.sample_frames, 6000)
            self.assertEqual(len(receipt.audio_inputs), 1)
            self.assertIsNone(receipt.frame_count)
            with wave.open(str(receipt.destination)) as stream:
                self.assertEqual((stream.getnchannels(), stream.getframerate()), (2, 48000))
                self.assertEqual(stream.getnframes(), 6000)
                self.assertNotEqual(set(stream.readframes(6000)), {0})

    @unittest.skipUnless(shutil.which("ffmpeg"), "requires the optional ffmpeg encoder")
    def test_actual_mp4_primary_and_final_png_keep_encoder_provenance(self):
        report = self.run_batch(("First",), format="mp4", save_last_frame=True,
            _output_options={"vcodec": "libx264", "video_crf": 24, "video_preset": "ultrafast"})
        self.assertTrue(report.ok)
        pair = report.outcomes[0].result
        data = pair.primary.destination.read_bytes()
        self.assertEqual(data[4:8], b"ftyp")
        self.assertEqual(hashlib.sha256(data).hexdigest(), pair.primary.digest)
        self.assertTrue(pair.primary.ffmpeg_invocations)
        self.assertIn("libx264", json.dumps(pair.primary.ffmpeg_invocations))
        self.assertIn("ultrafast", json.dumps(pair.primary.ffmpeg_invocations))
        self.verify_artifact(pair.still)

    def test_real_cli_enables_parallel_clips_and_final_still(self):
        process = subprocess.run([sys.executable, "-m", "fmn_python", str(self.source),
            "Second", "First", "--workers", "2", "--subdivide", "--save-last-frame",
            "--format", "gif", "--resolution", "64x48", "--fps", "10", "--robot",
            "--video_dir", str(self.root / "cli")], cwd=self.root,
            capture_output=True, text=True, timeout=30)
        self.assertEqual(process.returncode, 0, process.stderr)
        data = json.loads(process.stdout)["batch"]
        self.assertEqual([item["name"] for item in data["outcomes"]], ["Second", "First"])
        self.assertEqual(data["counts"]["succeeded"], 2)
        for item in data["outcomes"]:
            self.assertTrue(item["result"]["completed"])
            self.assertEqual(len(item["result"]["primary"]["segments"]), 3)
            self.assertEqual(Path(item["result"]["still_destination"]).read_bytes()[:8], b"\x89PNG\r\n\x1a\n")


if __name__ == "__main__":
    unittest.main()
