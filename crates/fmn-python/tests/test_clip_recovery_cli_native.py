"""Installed front-door recovery and native audio/lifecycle edge cases."""
from __future__ import annotations

import json
import os
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
import wave

from test_clip_checkpoint_native import _native_available

PYTHON = Path(__file__).resolve().parents[1] / "python"


@unittest.skipUnless(_native_available(), "requires the built FrankenManim native extension")
class ClipRecoveryCliTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="fmn-clip-cli-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.source = self.root / "scene.py"
        self.journal, self.output = self.root / "progress.json", self.root / "clips"
        self.marker = self.root / "continue"
        self.source.write_text('''from manimlib import *
from pathlib import Path
print("import diagnostic")
class Motion(Scene):
    def construct(self):
        print("construct diagnostic")
        box = Square(fill_opacity=1)
        box.add_updater(lambda mob, dt: mob.shift(UP * dt))
        self.add(box)
        self.play(box.animate.shift(RIGHT), run_time=.25, rate_func=linear)
        if not Path(__file__).with_name("continue").exists():
            raise RuntimeError("temporary stop")
        self.wait(.25)
''')
        self.base = [str(self.source), "Motion", "--subdivide", "--format", "png_sequence",
                     "--resolution", "64x48", "--fps", "8", "--threads", "1",
                     "--output", str(self.output), "--clip-checkpoint", str(self.journal),
                     "--resume-key", "inputs-v1", "--robot"]

    def cli(self, *extra, args=None):
        env = dict(os.environ, PYTHONPATH=str(PYTHON))
        result = subprocess.run([sys.executable, "-m", "fmn_python", *(self.base if args is None else args), *extra],
                                cwd=self.root, env=env, capture_output=True, text=True, timeout=25)
        lines = result.stdout.splitlines()
        self.assertEqual(len(lines), 1, (result.returncode, result.stdout, result.stderr))
        report = json.loads(lines[0])
        self.assertEqual(result.returncode, report["exit"]["code"])
        return result.returncode, report, result.stderr

    def test_cli_failed_prefix_resumes_and_complete_replay_does_not_rewrite(self):
        code, failed, stderr = self.cli()
        self.assertEqual(code, 5)
        self.assertEqual(len(failed["subdivision"]["segments"]), 1)
        self.assertIn("import diagnostic", stderr)
        self.assertIn("construct diagnostic", stderr)
        frame = self.output / "segment-000000/frame_000000.png"
        original, info = frame.read_bytes(), frame.stat()
        self.marker.write_text("continue")
        code, result, _ = self.cli("--resume")
        self.assertEqual(code, 0, result)
        self.assertTrue(result["resume_requested"])
        self.assertTrue(result["subdivision"]["completed"])
        self.assertEqual([item["play_index"] for item in result["subdivision"]["segments"]], [0, 1])
        before = {str(p.relative_to(self.output)): (p.read_bytes(), p.stat().st_mtime_ns)
                  for p in self.output.rglob("*") if p.is_file()}
        code, _, _ = self.cli("--resume")
        self.assertEqual(code, 0)
        self.assertEqual(frame.read_bytes(), original)
        self.assertEqual((frame.stat().st_ino, frame.stat().st_mtime_ns), (info.st_ino, info.st_mtime_ns))
        self.assertEqual(before, {str(p.relative_to(self.output)): (p.read_bytes(), p.stat().st_mtime_ns)
                                 for p in self.output.rglob("*") if p.is_file()})

    def test_incompatible_modes_refuse_before_source_import(self):
        sentinel = self.root / "imported"
        self.source.write_text(f'from pathlib import Path\nPath({str(sentinel)!r}).write_text("executed")\n')
        for extra in (["--workers=2"], ["--write_all"], ["--save-last-frame"], ["--reproducible"],
                      ["--checkpoint", "other.json"], ["--skip_animations"]):
            with self.subTest(extra=extra):
                code, _, _ = self.cli(*extra)
                self.assertEqual(code, 2)
                self.assertFalse(sentinel.exists())
        self.assertFalse(self.output.exists())
        self.assertFalse(self.journal.exists())

    def test_key_values_that_look_like_flags_remain_values(self):
        self.marker.write_text("go")
        args = self.base.copy()
        args[args.index("inputs-v1")] = "--workers=7"
        args[args.index("--clip-checkpoint")] = "--clip-checkpoint=" + str(self.journal)
        args.remove(str(self.journal))
        code, result, _ = self.cli(args=args)
        self.assertEqual(code, 0, result)
        self.assertEqual(json.loads(self.journal.read_text())["key"], "--workers=7")

    def test_entry_source_edit_refuses_without_replacing_saved_output(self):
        self.assertEqual(self.cli()[0], 5)
        original = self.journal.read_bytes()
        frame = self.output / "segment-000000/frame_000000.png"
        before = frame.read_bytes()
        self.source.write_text(self.source.read_text() + "\n# new input version\n")
        self.marker.write_text("go")
        code, report, _ = self.cli("--resume")
        self.assertEqual(code, 5)
        self.assertIn("inputs or render options", report["message"])
        self.assertEqual(self.journal.read_bytes(), original)
        self.assertEqual(frame.read_bytes(), before)
        self.assertFalse((self.output / "segment-000001").exists())

    def test_interrupt_keeps_durable_prefix_and_exit_130(self):
        self.source.write_text(self.source.read_text().replace('RuntimeError("temporary stop")', 'KeyboardInterrupt("temporary stop")'))
        code, report, _ = self.cli()
        self.assertEqual(code, 130)
        self.assertFalse(report["subdivision"]["completed"])
        self.assertEqual(len(report["subdivision"]["segments"]), 1)
        self.marker.write_text("go")
        self.assertEqual(self.cli("--resume")[0], 0)

    def test_import_and_constructor_chdir_do_not_redirect_journal_or_output(self):
        other = self.root / "elsewhere"
        other.mkdir()
        self.source.write_text(f'''from manimlib import Scene, Square
import os
os.chdir({str(other)!r})
class Motion(Scene):
    def construct(self):
        self.add(Square())
        self.wait(.125)
''')
        args = self.base.copy()
        args[args.index(str(self.output))] = "clips"
        args[args.index(str(self.journal))] = "progress.json"
        self.assertEqual(self.cli(args=args)[0], 0)
        self.assertEqual(self.cli("--resume", args=args)[0], 0)
        self.assertTrue(self.journal.is_file())
        self.assertTrue(self.output.is_dir())
        self.assertEqual(list(other.iterdir()), [])

    def test_native_quality_controls_reach_actual_mp4_clips(self):
        # Capability absence is a visible skip, never a fake video encoder.
        import shutil
        if shutil.which("ffmpeg") is None:
            self.skipTest("requires the host ffmpeg video capability")
        self.marker.write_text("go")
        args = self.base.copy()
        args[args.index("png_sequence")] = "mp4"
        code, result, _ = self.cli("--vcodec", "libx264", "--crf", "23", "--preset", "ultrafast", args=args)
        self.assertEqual(code, 0, result)
        first = result["subdivision"]["segments"][0]["render"]
        self.assertTrue(Path(first["destination"]).is_file())
        self.assertIn("ftyp", Path(first["destination"]).read_bytes()[:64].decode("latin1"))
        self.assertIn("libx264", json.dumps(first["ffmpeg_invocations"]))
        self.assertIn("ultrafast", json.dumps(first["ffmpeg_invocations"]))
        self.assertEqual(self.cli("--resume", "--vcodec", "libx264", "--crf", "23",
                                  "--preset", "ultrafast", args=args)[0], 0)

    def test_native_plain_render_and_global_help_still_work(self):
        self.marker.write_text("go")
        args = [str(self.source), "Motion", "--format", "png", "--resolution", "64x48", "--threads", "1",
                "--output", str(self.root / "plain.png"), "--robot"]
        code, report, _ = self.cli(args=args)
        self.assertEqual(code, 0, report)
        self.assertEqual((self.root / "plain.png").read_bytes()[:8], b"\x89PNG\r\n\x1a\n")
        code, report, _ = self.cli(args=["--help", "--robot"])
        self.assertEqual(code, 0)
        self.assertIn("--clip-checkpoint", report["help"])


@unittest.skipUnless(_native_available(), "requires the built FrankenManim native extension")
class ClipRecoveryNativeEdges(unittest.TestCase):
    def test_constructor_paths_are_frozen_at_the_call_site(self):
        from fmn_python import render_subdivided_scene
        from manimlib import Scene, Square
        initial = Path.cwd()
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            other = root / "other"
            other.mkdir()
            class MovesCwd(Scene):
                def __init__(self):
                    os.chdir(other)
                    super().__init__()
                def construct(self):
                    self.add(Square())
                    self.wait(.125)
            try:
                for resumed in (False, True):
                    os.chdir(root)
                    result = render_subdivided_scene(MovesCwd, "clips", checkpoint="progress.json",
                        resume_key="v1", resume=resumed, format="gif", resolution=(64, 48), fps=8, threads=1)
                    self.assertEqual(result.destination, root / "clips")
                    self.assertTrue((root / "progress.json").is_file())
                    self.assertEqual(list(other.iterdir()), [])
            finally:
                os.chdir(initial)

    def test_replayed_audio_cue_retains_phase_across_clip_boundary(self):
        from fmn_python import render_subdivided_scene
        import manimlib as m
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            sound = root / "sound.wav"
            with wave.open(str(sound), "wb") as output:
                output.setparams((1, 2, 24000, 0, 'NONE', 'not compressed'))
                output.writeframes(b''.join(struct.pack('<h', ((i * 53) % 9000) - 4500) for i in range(24000)))
            state = {"fail": True}
            class Audio(m.Scene):
                def construct(self):
                    self.add_sound(str(sound), time_offset=-.125)
                    self.wait(.25)
                    if state["fail"]:
                        raise RuntimeError("after first audio clip")
                    self.wait(.25)
            options = dict(format="wav", fps=8, threads=1, resolution=(32, 24))
            recovery = dict(checkpoint=root / "audio.json", resume_key="audio-v1")
            with self.assertRaises(RuntimeError):
                render_subdivided_scene(Audio, root / "clips", **options, **recovery)
            state["fail"] = False
            with patch.object(m, "_portal_begin_audio_clip", wraps=m._portal_begin_audio_clip) as begin:
                result = render_subdivided_scene(Audio, root / "clips", resume=True, **options, **recovery)
            self.assertEqual(begin.call_count, 1)
            self.assertEqual([s.render.sample_frames for s in result.segments], [12000, 12000])
            clean = render_subdivided_scene(Audio, root / "clean", **options)
            for a, b in zip(result.segments, clean.segments):
                self.assertEqual(a.render.destination.read_bytes(), b.render.destination.read_bytes())
                with wave.open(str(a.render.destination), 'rb') as stream:
                    self.assertEqual((stream.getnchannels(), stream.getframerate()), (2, 48000))

    def test_mid_clip_cancellation_retries_only_unpublished_suffix(self):
        from fmn_python import render_subdivided_scene
        import manimlib as m
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            state = {"fail": True}
            class Interrupted(m.Scene):
                def construct(self):
                    box = m.Square()
                    self.add(box)
                    self.wait(.25)
                    def update(mob, dt):
                        if state["fail"] and dt > 0:
                            raise KeyboardInterrupt("inside the next clip")
                    box.add_updater(update)
                    self.wait(.25)
            options = dict(format="gif", fps=8, threads=1, resolution=(64, 48),
                           checkpoint=root / "progress.json", resume_key="v1")
            with self.assertRaises(KeyboardInterrupt) as caught:
                render_subdivided_scene(Interrupted, root / "clips", **options)
            self.assertEqual(len(caught.exception.render_subdivision_result.segments), 1)
            self.assertEqual({p.name for p in (root / "clips").iterdir()}, {"segment-000000.gif"})
            state["fail"] = False
            result = render_subdivided_scene(Interrupted, root / "clips", resume=True, **options)
            self.assertTrue(result.completed)
            self.assertEqual(len(result.segments), 2)


if __name__ == "__main__":
    unittest.main(verbosity=2)
