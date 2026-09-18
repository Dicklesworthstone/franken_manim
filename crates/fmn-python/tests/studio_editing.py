"""Fresh-wheel InteractiveScene editing over real native records and workers.

Use the installed interpreter with -I. No source-tree native imports or window
emulation: the same public class receives the same admitted Studio events.
"""
from __future__ import annotations

import json
from pathlib import Path
import runpy
import tempfile
import unittest

import numpy as np
import manimlib as m
from fmn_python.interaction import dispatch_live_input
from fmn_python.studio import Studio

_helpers = runpy.run_path(str(Path(__file__).with_name("studio_preview.py")))
Preview, rgba_png = _helpers["Preview"], _helpers["rgba_png"]


class NativeEditingTests(unittest.TestCase):
    def setUp(self):
        self.s = m.InteractiveScene()
        self.s.setup()
        self.box = m.Square(side_length=2, fill_opacity=1)
        self.s.add(self.box)
        self.s.add_to_selection(self.box)

    def tearDown(self):
        self.s.clear_selection()
        self.s.undo_stack.clear()
        self.s.redo_stack.clear()
        self.s.clear()

    def motion(self, point, modifiers=0):
        return dispatch_live_input(self.s, "on_mouse_motion", (np.array(point, dtype=float), np.zeros(3)), modifiers)

    def key(self, char, modifiers=0, release=False):
        method = "on_key_release" if release else "on_key_press"
        symbol = ord(char) if isinstance(char, str) else char
        return dispatch_live_input(self.s, method, (symbol, modifiers), modifiers)

    def test_axis_grabs_repeat_release_and_native_undo_redo(self):
        self.motion([0, 0, 0])
        self.key("h")
        self.assertEqual(len(self.s.undo_stack), 0, "cancelled gestures must not allocate history")
        self.motion([2, 3, 4])
        np.testing.assert_allclose(self.box.get_center(), [2, 0, 0])
        self.key("h")  # Browser auto-repeat must not rebase the mouse offset.
        self.motion([3, 5, 9])
        np.testing.assert_allclose(self.box.get_center(), [3, 0, 0])
        self.assertEqual(len(self.s.undo_stack), 1)
        self.key("h", release=True)
        self.motion([4, 5, 9])
        np.testing.assert_allclose(self.box.get_center(), [3, 0, 0])
        self.key("z", 2)
        np.testing.assert_allclose(self.box.get_center(), [0, 0, 0])
        self.assertEqual(len(self.s.undo_stack), 0)
        self.assertEqual(len(self.s.redo_stack), 1)
        self.key("z", 3)
        np.testing.assert_allclose(self.box.get_center(), [3, 0, 0])
        self.assertEqual(len(self.s.redo_stack), 0)

    def test_y_and_z_grabs_only_change_the_selected_world_axis(self):
        for char, axis in (("v", 1), ("z", 2)):
            self.motion([0, 0, 0])
            old = self.box.get_center().copy()
            self.key(char)
            self.motion([2, 3, 4])
            target = old.copy()
            target[axis] += [2, 3, 4][axis]
            np.testing.assert_allclose(self.box.get_center(), target)
            self.key(char, release=True)

    def test_uniform_resize_is_cumulative_and_recovers_from_the_pivot(self):
        self.motion([1, 1, 0])
        self.key("t")  # Previously only shift-t initialized the reference.
        for point, size in (([2, 2, 0], 4), ([3, 3, 0], 6), ([1, 1, 0], 2)):
            self.motion(point)
            np.testing.assert_allclose([self.box.get_width(), self.box.get_height()], [size, size])
        self.motion([0, 0, 0])
        self.assertGreater(self.box.get_width(), 0)
        self.motion([1, 1, 0])
        np.testing.assert_allclose(self.box.get_width(), 2, atol=1e-5)
        self.assertEqual(len(self.s.undo_stack), 1)
        self.key("t", release=True)
        self.motion([2, 2, 0])
        np.testing.assert_allclose(self.box.get_width(), 2, atol=1e-5)

    def test_ctrl_stretch_reflection_and_return_to_uniform_are_finite(self):
        self.motion([1, 1, 0])
        self.key("t")
        self.motion([2, 0.5, 0], 2)
        np.testing.assert_allclose([self.box.get_width(), self.box.get_height()], [4, 1])
        original = self.box.get_points().copy()
        self.motion([-2, 0.5, 0], 2)
        np.testing.assert_allclose(self.box.get_points()[:, 0], -original[:, 0])
        self.motion([1, 1, 0], 0)
        np.testing.assert_allclose([self.box.get_width(), self.box.get_height()], [2, 2])
        self.motion([0, 1, 0], 2)
        self.assertTrue(np.isfinite(self.box.get_points()).all())
        self.motion([1, 1, 0], 2)
        np.testing.assert_allclose([self.box.get_width(), self.box.get_height()], [2, 2], atol=1e-5)

    def test_corner_resize_preserves_its_native_pivot(self):
        self.motion([1, 1, 0])
        pivot = self.box.get_corner(m.DL).copy()
        self.key("t", 1)
        self.motion([3, 3, 0], 1)
        np.testing.assert_allclose(self.box.get_corner(m.DL), pivot)
        np.testing.assert_allclose([self.box.get_width(), self.box.get_height()], [4, 4])
        self.key("t", release=True)

    def test_empty_degenerate_and_changed_selection_do_not_corrupt_geometry(self):
        self.s.clear_selection()
        self.key("g")
        self.key("t")
        self.motion([0, 0, 0], 2)
        self.assertEqual(len(self.s.undo_stack), 0)
        self.s.add_to_selection(self.box)
        self.key("t")  # mouse exactly at pivot: undefined radial reference
        self.motion([1, 1, 0])
        np.testing.assert_allclose(self.box.get_width(), 2)
        self.key("t", release=True)
        self.key("g")
        self.s.clear_selection()
        self.motion([2, 2, 0])
        np.testing.assert_allclose(self.box.get_center(), [0, 0, 0])
        self.assertFalse(self.s.is_grabbing)

    def test_consumed_motion_never_applies_the_selection_gesture(self):
        self.motion([0, 0, 0])
        self.key("h")
        seen = []
        self.box.add_mouse_motion_listner(lambda mob, event: seen.append(event["point"].copy()) or False)
        self.motion([0.5, 0, 0])
        self.assertEqual(len(seen), 1)
        np.testing.assert_allclose(self.box.get_center(), [0, 0, 0])
        self.assertEqual(len(self.s.undo_stack), 0)
        self.box.clear_event_listners()


class WorkerEditingTests(unittest.TestCase):
    def test_unedited_interactive_scene_uses_real_modifiers_native_history_and_pixels(self):
        with tempfile.TemporaryDirectory(prefix="fmn edit scene ") as directory:
            root = Path(directory)
            path, facts = root / "edit.py", root / "facts.json"
            path.write_text("from manimlib import *\nimport json\n" + f'''
class Edit(InteractiveScene):
    def construct(self):
        self.box = Square(side_length=2, fill_opacity=1, fill_color=RED, stroke_width=0)
        self.add(self.box)
        self.add_to_selection(self.box)
        self.wait(0.25)
    def on_key_release(self, symbol, modifiers):
        super().on_key_release(symbol, modifiers)
        with open({str(facts)!r}, 'w') as output:
            json.dump(dict(center=self.box.get_center().tolist(), width=self.box.get_width(),
                           height=self.box.get_height(), undo=len(self.undo_stack),
                           redo=len(self.redo_stack)), output)
''')
            with Studio(path, "Edit", interactive=True, resolution=(96, 54), fps=8,
                        threads=1, max_frames=2, max_bytes=4 * 1024 * 1024) as host:
                api = Preview(host)
                history = api.frame()
                api.json("/api/scrub", {"frame": 1})
                initial = api.frame()
                def event(kind, **fields):
                    with api.request("/api/inspect") as response:
                        generation = int(response.headers["X-FMN-Worker-Generation"])
                        view = json.load(response)["view"]
                    result = api.json("/api/event", dict(type=kind, worker_generation=generation,
                        frame=view["frame_index"], revision=view["input_revision"], **fields))
                    api.expected_digest = result["sha256"]
                    return result
                event("mouse_motion", x=0, y=0, dx=0, dy=0, modifiers=0)
                event("key_press", key="h", modifiers=0)
                event("mouse_motion", x=1, y=-2, dx=1, dy=-2, modifiers=0)
                event("key_release", key="h", modifiers=0)
                state = json.loads(facts.read_text())
                np.testing.assert_allclose(state["center"], [1, 0, 0])
                moved = api.frame()
                self.assertNotEqual(initial, moved)
                # Native geometry and separately decoded PNGs must agree.
                width, height, pixels = rgba_png(moved)
                image = np.frombuffer(pixels, np.uint8).reshape(height, width, 4)
                red = (image[:, :, 0] > 150) & (image[:, :, 1] < 100) & (image[:, :, 2] < 100)
                self.assertGreater(np.nonzero(red)[1].mean(), width / 2 + 4)
                event("mouse_motion", x=2, y=-1, dx=1, dy=1, modifiers=0)
                event("key_press", key="t", modifiers=0)
                event("mouse_motion", x=3, y=-0.5, dx=1, dy=0.5, modifiers=2)
                event("key_release", key="t", modifiers=0)
                state = json.loads(facts.read_text())
                np.testing.assert_allclose([state["width"], state["height"]], [4, 1])
                event("key_press", key="z", modifiers=2)
                event("key_release", key="z", modifiers=2)
                state = json.loads(facts.read_text())
                np.testing.assert_allclose([state["width"], state["height"]], [2, 2])
                event("key_press", key="z", modifiers=3)
                event("key_release", key="z", modifiers=3)
                state = json.loads(facts.read_text())
                np.testing.assert_allclose([state["width"], state["height"]], [4, 1])
                api.json("/api/scrub", {"frame": 0})
                self.assertEqual(history, api.frame(), "editing rewrote historical captures")
                self.assertTrue(host.alive)


if __name__ == "__main__":
    unittest.main()
