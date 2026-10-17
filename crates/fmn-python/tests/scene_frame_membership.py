"""fm-6l2i: the camera frame is a member of Scene.mobjects, as in the Reference.

The pinned Reference (3b1b/manim@6199a00d) starts every scene as
`self.mobjects = [self.camera.frame]` (scene.py:117). Every expected value
below was measured there by running the same construction. The frame's
z_index is -1, so Scene.add's sort keeps it ahead of default content.
clear() and remove() drop it, and a dropped frame's updaters stop.
Animating the frame re-adds it (Scene.begin_animations).
"""
import pathlib
import tempfile
import unittest

import manimlib as m
from fmn_python import render_scene

FRAME = "FRAME"

REFERENCE = {
    "init": {"names": [FRAME], "first_is_frame": True, "frame_z": -1},
    "add_two": [FRAME, "sq", "c"],
    "add_negative_z": ["neg", FRAME, "sq"],
    "bring_to_back": ["c", FRAME, "sq"],
    "bring_to_back_then_add": [FRAME, "c", "sq", "d"],
    "readd_frame": [FRAME, "sq"],
    "remove_frame": ["sq"],
    "clear_then_add": [[], ["c"]],
    "family_members": [FRAME, "g", "sq", "c"],
    "remove_all": [],
    "group_of_all": {"names": ["g"], "children": [FRAME, "sq"]},
    "index": 1,
    "undo": [FRAME, "sq"],
    "checkpoint_restore": [FRAME, "sq"],
    "frame_updater_after_clear": [2, 2],
    "top_level": [FRAME, "sq"],
    "fadeout_group_of_all": [],
    "animate_frame_after_clear": [FRAME],
    "construct_view": {"first": True, "len": 3},
}


def names(scene, sequence):
    return [FRAME if mobject is scene.frame else getattr(mobject, "_probe_name", type(mobject).__name__)
            for mobject in sequence]


def tag(mobject, name):
    mobject._probe_name = name
    return mobject


def cases():
    out = {}

    def case(fn):
        out[fn.__name__] = fn
        return fn

    @case
    def init(s):
        return {"names": names(s, s.mobjects), "first_is_frame": s.mobjects[0] is s.frame,
                "frame_z": s.frame.z_index}

    @case
    def add_two(s):
        s.add(tag(m.Square(), "sq"), tag(m.Circle(), "c"))
        return names(s, s.mobjects)

    @case
    def add_negative_z(s):
        s.add(tag(m.Square(), "sq"))
        s.add(tag(m.Circle().set_z_index(-5), "neg"))
        return names(s, s.mobjects)

    @case
    def bring_to_back(s):
        sq, c = tag(m.Square(), "sq"), tag(m.Circle(), "c")
        s.add(sq, c)
        s.bring_to_back(c)
        return names(s, s.mobjects)

    @case
    def bring_to_back_then_add(s):
        sq, c = tag(m.Square(), "sq"), tag(m.Circle(), "c")
        s.add(sq, c)
        s.bring_to_back(c)
        s.add(tag(m.Dot(), "d"))
        return names(s, s.mobjects)

    @case
    def readd_frame(s):
        s.add(tag(m.Square(), "sq"))
        s.add(s.frame)
        return names(s, s.mobjects)

    @case
    def remove_frame(s):
        s.add(tag(m.Square(), "sq"))
        s.remove(s.frame)
        return names(s, s.mobjects)

    @case
    def clear_then_add(s):
        s.add(tag(m.Square(), "sq"))
        s.clear()
        cleared = names(s, s.mobjects)
        s.add(tag(m.Circle(), "c"))
        return [cleared, names(s, s.mobjects)]

    @case
    def family_members(s):
        s.add(tag(m.Group(tag(m.Square(), "sq"), tag(m.Circle(), "c")), "g"))
        return names(s, s.get_mobject_family_members())

    @case
    def remove_all(s):
        s.add(tag(m.Square(), "sq"))
        s.remove(*s.mobjects)
        return names(s, s.mobjects)

    @case
    def group_of_all(s):
        s.add(tag(m.Square(), "sq"))
        group = tag(m.Group(*s.mobjects), "g")
        s.add(group)
        return {"names": names(s, s.mobjects), "children": names(s, group.submobjects)}

    @case
    def index(s):
        sq = tag(m.Square(), "sq")
        s.add(sq)
        return s.mobjects.index(sq)

    @case
    def undo(s):
        s.add(tag(m.Square(), "sq"))
        s.save_state()
        s.add(tag(m.Circle(), "c"))
        s.undo()
        return names(s, s.mobjects)

    @case
    def checkpoint_restore(s):
        # checkpoint_paste replays through get_state/restore_state: a state
        # taken while the frame was listed restores it, even after clear().
        s.add(tag(m.Square(), "sq"))
        state = s.get_state()
        s.clear()
        s.add(tag(m.Circle(), "c"))
        s.restore_state(state)
        return names(s, s.mobjects)

    @case
    def frame_updater_after_clear(s):
        calls = []
        s.frame.add_updater(lambda mob, dt: calls.append(dt))
        s.update_mobjects(0.1)
        listed = len(calls)
        s.clear()
        s.update_mobjects(0.1)
        return [listed, len(calls)]

    @case
    def top_level(s):
        s.add(tag(m.Square(), "sq"))
        return names(s, s.get_top_level_mobjects())

    @case
    def fadeout_group_of_all(s):
        s.add(tag(m.Square(), "sq"))
        s.play(m.FadeOut(m.Group(*s.mobjects)), run_time=0.2)
        return names(s, s.mobjects)

    @case
    def animate_frame_after_clear(s):
        s.add(tag(m.Square(), "sq"))
        s.clear()
        s.play(s.frame.animate.shift(m.RIGHT), run_time=0.2)
        return names(s, s.mobjects)

    @case
    def construct_view(s):
        del s
        seen = {}

        class Probe(m.Scene):
            def construct(self):
                seen["first"] = bool(self.mobjects) and self.mobjects[0] is self.frame
                self.add(m.Square(), m.Circle())
                seen["len"] = len(self.mobjects)

        Probe().run()
        return seen

    return out


class SceneFrameMembershipTests(unittest.TestCase):
    maxDiff = None

    def test_scene_list_matches_the_reference(self):
        constructions = cases()
        self.assertEqual(sorted(constructions), sorted(REFERENCE))
        for name, build in constructions.items():
            with self.subTest(case=name):
                self.assertEqual(build(m.Scene()), REFERENCE[name])

    def test_listed_frame_draws_nothing(self):
        # The Reference never draws its frame, so listing it changes no pixel:
        # a rendered scene with the frame listed equals the same scene with it
        # removed, and both differ from a scene holding only the frame.
        def render(remove_frame, with_content):
            class Probe(m.Scene):
                def construct(self):
                    if with_content:
                        self.add(m.Square(side_length=2, fill_color=m.RED, fill_opacity=1))
                    if remove_frame:
                        self.remove(self.frame)
                    seen.append([mobject is self.frame for mobject in self.mobjects])

            seen = []
            with tempfile.TemporaryDirectory(prefix="fmn-frame-membership-") as directory:
                output = pathlib.Path(directory) / "frame.png"
                receipt = render_scene(Probe(), output, format="png", resolution=(64, 36),
                                       fps=8, threads=1)
                self.assertEqual(receipt.frame_count, 1)
                return seen, output.read_bytes()

        listed_seen, listed = render(remove_frame=False, with_content=True)
        removed_seen, removed = render(remove_frame=True, with_content=True)
        frame_seen, frame_only = render(remove_frame=False, with_content=False)
        self.assertEqual((listed_seen, removed_seen, frame_seen),
                         ([[True, False]], [[False]], [[True]]))
        self.assertEqual(listed, removed)
        self.assertNotEqual(listed, frame_only)


_suite = unittest.defaultTestLoader.loadTestsFromTestCase(SceneFrameMembershipTests)
assert _suite.countTestCases() == 2, "scene frame membership inventory changed"
_result = unittest.TextTestRunner(verbosity=2).run(_suite)
if not _result.wasSuccessful():
    raise AssertionError("scene frame membership acceptance failed")
