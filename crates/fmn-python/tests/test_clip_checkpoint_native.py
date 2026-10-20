"""Real native clips: replay the stateful prefix, never rerender its pixels."""
from __future__ import annotations

import importlib.util
from importlib.machinery import PathFinder, ExtensionFileLoader
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


def _native_available():
    package = importlib.util.find_spec("manimlib")
    if package is None or package.submodule_search_locations is None:
        return False
    extension = PathFinder.find_spec("manimlib.manimlib", package.submodule_search_locations)
    return extension is not None and isinstance(extension.loader, ExtensionFileLoader)


@unittest.skipUnless(_native_available(), "requires the built FrankenManim native extension")
class ClipCheckpointNativeTests(unittest.TestCase):
    def setUp(self):
        import manimlib as m
        from fmn_python import render_subdivided_scene, subdivided_render_session
        self.m, self.render, self.session = m, render_subdivided_scene, subdivided_render_session
        self.temp = tempfile.TemporaryDirectory(prefix="fmn-clip-recovery-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.dest, self.journal = self.root / "clips", self.root / "clips.json"
        self.options = dict(format="png_sequence", resolution=(64, 48), fps=8, threads=1,
                            checkpoint=self.journal, resume_key="unchanged-scene-and-assets")
        self.state = {"fail": True, "duration": .25, "calls": [], "extra": False}
        state, m = self.state, self.m

        class Motion(m.Scene):
            def construct(self):
                state["calls"].append("construct")
                self.square = m.Square(fill_opacity=1)
                self.square.add_updater(lambda mob, dt: mob.shift(m.UP * dt))
                self.add(self.square)
                self.play(self.square.animate.shift(m.RIGHT), run_time=state["duration"],
                          rate_func=m.linear)
                if state["fail"]:
                    raise RuntimeError("temporary failure after clip zero")
                self.wait(.25)
                if state["extra"]:
                    self.wait(.125)
                state["end"] = (m._portal_scene_clock(self), self.square.get_center().tolist())
        self.Motion = Motion

    def first_failure(self):
        with self.assertRaisesRegex(RuntimeError, "temporary failure") as caught:
            self.render(self.Motion, self.dest, **self.options)
        self.assertEqual(len(caught.exception.render_subdivision_result.segments), 1)
        self.state["fail"] = False
        return json.loads(self.journal.read_text())

    def inventory(self, root):
        return {str(path.relative_to(root)): path.read_bytes()
                for path in root.rglob("*") if path.is_file()}

    def test_resume_preserves_old_bytes_and_only_renders_the_missing_suffix(self):
        self.first_failure()
        old = self.dest / "segment-000000" / "frame_000000.png"
        before = old.stat()
        with patch.object(self.m, "_portal_begin_recording", wraps=self.m._portal_begin_recording) as begin:
            result = self.render(self.Motion, self.dest, resume=True, **self.options)
        self.assertTrue(result.completed)
        self.assertEqual([segment.play_index for segment in result.segments], [0, 1])
        self.assertEqual(begin.call_count, 1)
        self.assertEqual((before.st_ino, before.st_mtime_ns), (old.stat().st_ino, old.stat().st_mtime_ns))
        resumed_state = self.state["end"]
        clean = self.root / "clean"
        self.render(self.Motion, clean, **{k: v for k, v in self.options.items()
                                          if k not in {"checkpoint", "resume_key"}})
        self.assertEqual(resumed_state, self.state["end"])
        self.assertEqual(self.inventory(self.dest), self.inventory(clean))
        self.assertEqual(self.state["calls"], ["construct"] * 3)
        self.assertTrue(json.loads(self.journal.read_text())["completed"])

    def test_complete_replay_runs_callbacks_but_no_output_generation(self):
        self.state["fail"] = False
        self.render(self.Motion, self.dest, **self.options)
        calls = len(self.state["calls"])
        # Use the same wrapper identity, including JSON constructor inputs.
        with patch.object(self.m, "_portal_begin_recording", wraps=self.m._portal_begin_recording) as begin:
            self.render(self.Motion, self.dest, resume=True, **self.options)
        self.assertEqual(begin.call_count, 0)
        self.assertEqual(len(self.state["calls"]), calls + 1)

    def test_clock_divergence_refuses_before_new_output_and_keeps_journal(self):
        self.first_failure()
        before = self.journal.read_bytes()
        self.state["duration"] = .5
        with patch.object(self.m, "_portal_begin_recording", wraps=self.m._portal_begin_recording) as begin:
            with self.assertRaisesRegex(ValueError, "end frame"):
                self.render(self.Motion, self.dest, resume=True, **self.options)
        self.assertEqual(begin.call_count, 0)
        self.assertEqual(self.journal.read_bytes(), before)
        self.assertEqual(len(list(self.dest.iterdir())), 1)

    def test_complete_checkpoint_rejects_an_extra_selected_segment(self):
        self.state["fail"] = False
        self.render(self.Motion, self.dest, **self.options)
        self.state["extra"] = True
        with patch.object(self.m, "_portal_begin_recording", wraps=self.m._portal_begin_recording) as begin:
            with self.assertRaisesRegex(ValueError, "selection"):
                self.render(self.Motion, self.dest, resume=True, **self.options)
        self.assertEqual(begin.call_count, 0)
        self.assertEqual(len(list(self.dest.iterdir())), 2)

    def test_missing_modified_or_extra_outputs_refuse_before_construct(self):
        self.first_failure()
        frame = self.dest / "segment-000000" / "frame_000000.png"
        original = frame.read_bytes()
        cases = [
            (lambda: frame.write_bytes(original + b"damage"), lambda: frame.write_bytes(original)),
            (lambda: (self.dest / "foreign").write_text("not ours"), lambda: (self.dest / "foreign").unlink()),
            (lambda: (frame.parent / "empty-directory").mkdir(), lambda: (frame.parent / "empty-directory").rmdir()),
            (lambda: frame.rename(frame.with_suffix(".hidden")), lambda: frame.with_suffix(".hidden").rename(frame)),
        ]
        for alter, restore in cases:
            with self.subTest(alter=alter):
                before = len(self.state["calls"])
                alter()
                try:
                    with self.assertRaises(ValueError):
                        self.render(self.Motion, self.dest, resume=True, **self.options)
                    self.assertEqual(len(self.state["calls"]), before)
                finally:
                    restore()

    def test_symlink_root_and_published_clip_are_not_followed(self):
        self.first_failure()
        clip = self.dest / "segment-000000"
        backup = self.root / "untouched-clip"
        clip.rename(backup)
        clip.symlink_to(backup, target_is_directory=True)
        with self.assertRaises(ValueError):
            self.render(self.Motion, self.dest, resume=True, **self.options)
        self.assertTrue(backup.is_dir())
        self.assertTrue(clip.is_symlink())

    def test_changed_plan_key_and_source_identity_refuse_before_execution(self):
        self.first_failure()
        before = len(self.state["calls"])
        for change in ({"fps": 16}, {"resolution": (96, 54)}, {"resume_key": "new"},
                       {"_source_identity": {"sha256": "a" * 64}}, {"max_segments": 20}):
            with self.subTest(change=change), self.assertRaises(ValueError):
                self.render(self.Motion, self.dest, resume=True, **(self.options | change))
        self.assertEqual(len(self.state["calls"]), before)

    def test_malicious_receipt_path_is_rejected_without_touching_foreign_file(self):
        document = self.first_failure()
        foreign = self.root / "foreign.png"
        foreign.write_bytes(b"private-file")
        document["segments"][0]["render"]["destination"] = str(foreign)
        self.journal.write_text(json.dumps(document))
        with self.assertRaisesRegex(ValueError, "destination"):
            self.render(self.Motion, self.dest, resume=True, **self.options)
        self.assertEqual(foreign.read_bytes(), b"private-file")

    def test_journal_lock_is_exclusive_and_outside_clips(self):
        from fmn_python.clip_checkpoint import ClipCheckpoint
        with ClipCheckpoint(self.journal, resume=False, key=self.options["resume_key"]):
            with self.assertRaises(OSError):
                self.render(self.Motion, self.dest, **self.options)
        self.assertFalse(self.dest.exists())
        with self.assertRaisesRegex(ValueError, "outside"):
            self.render(self.Motion, self.dest, **(self.options | {"checkpoint": self.dest / "journal"}))
        self.assertFalse(self.dest.exists())

    def test_journal_write_failure_keeps_publication_but_will_not_adopt_it_on_resume(self):
        from fmn_python.clip_checkpoint import ClipCheckpoint
        publish = ClipCheckpoint._publish
        def fail_nonempty(owner, payload):
            if json.loads(payload)["segments"]:
                raise OSError("journal disk full")
            return publish(owner, payload)
        with patch.object(ClipCheckpoint, "_publish", fail_nonempty):
            with self.assertRaisesRegex(OSError, "disk full") as caught:
                self.render(self.Motion, self.dest, **self.options)
        self.assertEqual(len(caught.exception.render_subdivision_result.segments), 1)
        self.assertTrue((self.dest / "segment-000000" / "FMN_COMPLETE").is_file())
        self.assertEqual(json.loads(self.journal.read_text())["segments"], [])
        with self.assertRaisesRegex(ValueError, "unrecorded"):
            self.render(self.Motion, self.dest, resume=True, **self.options)

    def test_empty_prefix_resumes_and_completed_context_reports_reused_indices(self):
        m = self.m
        with self.assertRaises(KeyboardInterrupt):
            with self.session(m.Scene(), self.dest, **self.options):
                raise KeyboardInterrupt("before any play")
        scene = m.Scene()
        with self.session(scene, self.dest, resume=True, **self.options) as session:
            scene.add(m.Square())
            scene.wait(.25)
        with self.session(m.Scene(), self.dest, resume=True, **self.options) as session:
            session.scene.add(m.Square())
            session.scene.wait(.25)
        self.assertEqual(session.reused, (0,))
        self.assertTrue(session.result.completed)

    def test_ranges_empty_plays_and_exclusive_ends_keep_original_clip_indices(self):
        m, state = self.m, self.state
        class Ranged(m.Scene):
            def construct(self):
                self.add(m.Square())
                self.play()
                self.wait(.125)  # preroll 0
                self.wait(.125)  # retained 1
                if state["fail"]:
                    raise RuntimeError("transient")
                self.play()      # empty call does not consume index 2
                self.wait(.25)   # new 2
                self.wait(.125)  # exclusive boundary 3, not an error
        options = self.options | {"animation_range": (1, 3), "format": "gif"}
        with self.assertRaisesRegex(RuntimeError, "transient"):
            self.render(Ranged, self.dest, **options)
        state["fail"] = False
        result = self.render(Ranged, self.dest, resume=True, **options)
        self.assertEqual([segment.play_index for segment in result.segments], [1, 2])
        self.assertEqual([(segment.start_frame, segment.end_frame) for segment in result.segments], [(1, 2), (2, 4)])

    def test_early_scene_end_cannot_mark_an_unreplayed_prefix_complete(self):
        m, state = self.m, self.state
        class Early(m.Scene):
            def construct(self):
                self.add(m.Square())
                if not state["fail"]:
                    return
                self.wait(.125)
                raise RuntimeError("transient")
        with self.assertRaisesRegex(RuntimeError, "transient"):
            self.render(Early, self.dest, **self.options)
        state["fail"] = False
        with self.assertRaisesRegex(ValueError, "before replaying"):
            self.render(Early, self.dest, resume=True, **self.options)
        self.assertFalse(json.loads(self.journal.read_text())["completed"])

    def test_configured_writer_hooks_do_not_try_to_encode_replayed_clips(self):
        m = self.m
        class Configured(m.Scene):
            def __init__(self):
                super().__init__(file_writer_config={"subdivide_output": True, "write_to_movie": True})
            def construct(self):
                self.add(m.Square())
                self.wait(.125)
        options = self.options | {"format": "gif"}
        self.render(Configured, self.dest, **options)
        with patch.object(m, "_portal_begin_recording", wraps=m._portal_begin_recording) as begin:
            result = self.render(Configured, self.dest, resume=True, **options)
        self.assertTrue(result.completed)
        self.assertEqual(begin.call_count, 0)

    def test_constructor_cannot_mutate_the_checkpoint_inputs_or_caller_data(self):
        m = self.m
        values = [3]
        class Constructor(m.Scene):
            def __init__(self, values):
                super().__init__()
                values.append(4)
            def construct(self):
                self.add(m.Square())
                self.wait(.125)
        self.render(Constructor, self.dest, scene_kwargs={"values": values}, **self.options)
        self.render(Constructor, self.dest, scene_kwargs={"values": values}, resume=True, **self.options)
        self.assertEqual(values, [3])
        self.assertEqual(json.loads(self.journal.read_text())["plan"]["identity"]["inputs"]["scene_kwargs"],
                         {"values": [3]})

    def test_invalid_recovery_arguments_refuse_before_constructor(self):
        calls, m = [], self.m
        class Uncalled(m.Scene):
            def __init__(self):
                calls.append("constructor")
                super().__init__()
        for options in ({"resume": True}, {"resume": 1}, {"resume_key": "x"},
                        {"checkpoint": self.journal}, {"checkpoint": self.journal, "resume_key": ""}):
            with self.subTest(options=options), self.assertRaises((TypeError, ValueError)):
                self.render(Uncalled, self.dest, **options)
        self.assertEqual(calls, [])


if __name__ == "__main__":
    unittest.main(verbosity=2)
