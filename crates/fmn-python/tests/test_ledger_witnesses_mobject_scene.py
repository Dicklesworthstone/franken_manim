"""Ledger witnesses for mobject_scene (fm-6u1g): each test calls a manimlib symbol the Parity Ledger
marks same/improved and checks the Reference's behaviour (manimlib @ 6199a00d)."""
import unittest

import numpy as np

from test_parallel_batch_native import native_wheel_available


def _ids(mobjects):
    return [id(mob) for mob in mobjects]


def _record_call(self, x, key=None):
    """A decorated body: records its arguments and returns a sentinel."""
    self.recorded_call = (x, key)
    return ("result", x)


@unittest.skipUnless(native_wheel_available(), "requires the built FrankenManim native wheel")
class MobjectSceneWitnesses(unittest.TestCase):
    # Reference mobject.py:216: wrapper runs func, then self.note_changed_data(), which flags
    # self and (recurse_up) every parent; it returns func's result.
    def test_affects_data(self):
        from manimlib import Mobject, Square, VGroup

        outer, inner = VGroup(), VGroup()
        child, sibling = Square(), Square()
        inner.add(child, sibling)
        outer.add(inner)
        for mob in (outer, inner, child, sibling):
            mob._data_has_changed = False

        wrapped = Mobject.affects_data(_record_call)
        self.assertEqual(wrapped.__name__, "_record_call")
        self.assertIs(wrapped.__wrapped__, _record_call)
        self.assertEqual(wrapped(child, 3, key="k"), ("result", 3))
        self.assertEqual(child.recorded_call, (3, "k"))
        self.assertIs(child._data_has_changed, True)
        self.assertIs(inner._data_has_changed, True)
        self.assertIs(outer._data_has_changed, True)
        # Only the receiver and its ancestors are noted, never a sibling.
        self.assertIs(sibling._data_has_changed, False)

    # Reference scene.py:319: the wrapper runs func and returns the scene, whatever func
    # returned (its render regrouping step is Lumen's draw plan in the portal).
    def test_scene_affects_mobject_list(self):
        from manimlib import Scene

        scene = Scene()
        wrapped = Scene.affects_mobject_list(_record_call)
        self.assertEqual(wrapped.__name__, "_record_call")
        self.assertIs(wrapped(scene, 7, key="k"), scene)
        self.assertEqual(scene.recorded_call, (7, "k"))

    # Reference mobject.py:225: after func, note_changed_data() on every
    # family_members_with_points() member (each also flags its parents).
    def test_affects_family_data(self):
        from manimlib import Circle, Mobject, Square, VGroup, VMobject

        root, sub = VGroup(), VGroup()
        square, circle, empty = Square(), Circle(), VMobject()
        sub.add(circle)
        root.add(square, sub, empty)
        outside = VGroup(root)
        members = (outside, root, square, sub, circle, empty)
        for mob in members:
            mob._data_has_changed = False

        wrapped = Mobject.affects_family_data(_record_call)
        self.assertEqual(wrapped.__name__, "_record_call")
        self.assertEqual(wrapped(root, 7), ("result", 7))
        self.assertEqual(root.recorded_call, (7, None))
        flags = {name: mob._data_has_changed for name, mob in zip(
            ("outside", "root", "square", "sub", "circle", "empty"), members)}
        # Members with points are noted; their ancestors through recurse_up; a point-less
        # leaf is not a family member with points and has no noted descendant.
        self.assertEqual(flags, {"outside": True, "root": True, "square": True,
                                 "sub": True, "circle": True, "empty": False})

    # Reference mobject.py:1909: wrapper runs func, then calls self.refresh_shader_wrapper_id(),
    # and returns func's result. The hook is observed through an ordinary subclass override.
    def test_affects_shader_info_id(self):
        from manimlib import Mobject, Square

        class Hooked(Square):
            def refresh_shader_wrapper_id(self):
                vars(self).setdefault("hook_log", []).append(
                    getattr(self, "recorded_call", None))
                return super().refresh_shader_wrapper_id()

        hooked = Hooked()
        vars(hooked)["hook_log"] = []
        wrapped = Mobject.affects_shader_info_id(_record_call)
        self.assertEqual(wrapped.__name__, "_record_call")
        self.assertIs(wrapped.__wrapped__, _record_call)
        self.assertEqual(wrapped(hooked, 5, key="z"), ("result", 5))
        # Exactly one refresh, and it ran after func (it already sees func's effect).
        self.assertEqual(hooked.hook_log, [(5, "z")])
        self.assertEqual(wrapped(hooked, 6), ("result", 6))
        self.assertEqual(hooked.hook_log, [(5, "z"), (6, None)])

        plain = Square()
        self.assertEqual(wrapped(plain, 9), ("result", 9))
        self.assertEqual(plain.recorded_call, (9, None))

    # Reference mobject.py:636: during func, parents is [] and target/saved_state are None;
    # afterwards the very same objects are restored. func's result is returned.
    def test_stash_mobject_pointers(self):
        from manimlib import Circle, Mobject, Square, Triangle, VGroup

        mob = Square()
        group = VGroup(mob)
        target, saved = Circle(), Triangle()
        mob.target = target
        mob.saved_state = saved
        parents = mob.parents
        self.assertEqual(_ids(parents), [id(group)])
        seen = {}

        def capture(self, tag):
            seen["parents"] = list(self.parents)
            seen["parents_is_list"] = isinstance(self.parents, list)
            seen["target"] = self.target
            seen["saved_state"] = self.saved_state
            return ("captured", tag)

        wrapped = Mobject.stash_mobject_pointers(capture)
        self.assertEqual(wrapped.__name__, "capture")
        self.assertEqual(wrapped(mob, "t"), ("captured", "t"))
        self.assertEqual(seen, {"parents": [], "parents_is_list": True,
                                "target": None, "saved_state": None})
        self.assertIs(mob.parents, parents)
        self.assertEqual(_ids(mob.parents), [id(group)])
        self.assertIs(mob.target, target)
        self.assertIs(mob.saved_state, saved)
        self.assertEqual(_ids(group.submobjects), [id(mob)])

    # Reference mobject.py:901: drops the cached updater status of self and every ancestor, so
    # has_updaters() reflects updaters added to or removed from the family; returns self.
    def test_refresh_has_updater_status(self):
        from manimlib import Square, VGroup

        top, mid, leaf = VGroup(), VGroup(), Square()
        mid.add(leaf)
        top.add(mid)
        self.assertEqual([m.has_updaters() for m in (top, mid, leaf)], [False, False, False])

        def updater(mob):
            pass

        leaf.updaters.append(updater)
        self.assertIs(leaf.refresh_has_updater_status(), leaf)
        self.assertEqual([m.has_updaters() for m in (top, mid, leaf)], [True, True, True])

        leaf.updaters.remove(updater)
        self.assertIs(leaf.refresh_has_updater_status(), leaf)
        self.assertEqual([m.has_updaters() for m in (top, mid, leaf)], [False, False, False])

        mid.updaters.append(updater)
        self.assertIs(mid.refresh_has_updater_status(), mid)
        self.assertEqual([m.has_updaters() for m in (top, mid, leaf)], [True, True, False])

    # Reference mobject.py:1182: set_height(max_height, **kwargs) only when the height exceeds
    # max_height; returns self either way.
    def test_set_max_height(self):
        from manimlib import DOWN, Rectangle

        rect = Rectangle(width=2, height=4).shift([1.0, 0.5, 0.0])
        self.assertIs(rect.set_max_height(3), rect)
        self.assertAlmostEqual(rect.get_height(), 3.0, places=6)
        self.assertAlmostEqual(rect.get_width(), 1.5, places=6)
        np.testing.assert_allclose(rect.get_center(), [1.0, 0.5, 0.0], atol=1e-6)

        # Not binding (larger bound, or equal: the test is strictly greater).
        for bound in (5, 3):
            self.assertIs(rect.set_max_height(bound), rect)
            self.assertAlmostEqual(rect.get_height(), 3.0, places=6)
            self.assertAlmostEqual(rect.get_width(), 1.5, places=6)

        # kwargs reach set_height: stretch keeps the width, about_edge pins the bottom.
        tall = Rectangle(width=2, height=4)
        bottom = tall.get_bottom()
        tall.set_max_height(2, stretch=True, about_edge=DOWN)
        self.assertAlmostEqual(tall.get_height(), 2.0, places=6)
        self.assertAlmostEqual(tall.get_width(), 2.0, places=6)
        np.testing.assert_allclose(tall.get_bottom(), bottom, atol=1e-6)

    # Reference mobject.py:1192: set_width(min_width, **kwargs) only when the width is below
    # min_width; returns self either way.
    def test_set_min_width(self):
        from manimlib import LEFT, Rectangle

        rect = Rectangle(width=2, height=4).shift([0.0, 1.0, 0.0])
        self.assertIs(rect.set_min_width(3), rect)
        self.assertAlmostEqual(rect.get_width(), 3.0, places=6)
        self.assertAlmostEqual(rect.get_height(), 6.0, places=6)
        np.testing.assert_allclose(rect.get_center(), [0.0, 1.0, 0.0], atol=1e-6)

        for bound in (1, 3):
            self.assertIs(rect.set_min_width(bound), rect)
            self.assertAlmostEqual(rect.get_width(), 3.0, places=6)
            self.assertAlmostEqual(rect.get_height(), 6.0, places=6)

        narrow = Rectangle(width=2, height=4)
        left = narrow.get_left()
        narrow.set_min_width(5, stretch=True, about_edge=LEFT)
        self.assertAlmostEqual(narrow.get_width(), 5.0, places=6)
        self.assertAlmostEqual(narrow.get_height(), 4.0, places=6)
        np.testing.assert_allclose(narrow.get_left(), left, atol=1e-6)

    # Reference vectorized_mobject.py:363: one upper-case long hex per point of stroke_rgba.
    def test_get_stroke_colors(self):
        from manimlib import BLUE, LEFT, RED, RIGHT, Line, Square

        square = Square().set_stroke(RED)
        colors = square.get_stroke_colors()
        self.assertEqual(square.get_num_points(), 9)
        self.assertEqual(colors, ["#FC6255"] * 9)

        # A two-colour stroke is interpolated across the 3 points of a line; the middle point is
        # the exact average of #FC6255 and #58C4DD.
        line = Line(LEFT, RIGHT).set_stroke([RED, BLUE])
        self.assertEqual(line.get_num_points(), 3)
        self.assertEqual(line.get_stroke_colors(), ["#FC6255", "#AA9399", "#58C4DD"])

    # Reference value_tracker.py:22: stores value, then Mobject.__init__(**kwargs) seeds
    # uniforms["value"] as a float64 array of listify(value).
    def test_value_tracker_init(self):
        from manimlib import RED, ValueTracker

        tracker = ValueTracker(3.5)
        self.assertEqual(tracker.get_value(), 3.5)
        self.assertIsInstance(tracker.get_value(), np.float64)
        self.assertEqual(tracker.uniforms["value"].dtype, np.float64)
        np.testing.assert_array_equal(tracker.uniforms["value"], [3.5])
        self.assertEqual(tracker.get_num_points(), 0)
        tracker.increment_value(1.25)
        self.assertEqual(tracker.get_value(), 4.75)

        self.assertEqual(ValueTracker().get_value(), 0.0)

        vector = ValueTracker(np.array([1.0, 2.0, 3.0]))
        np.testing.assert_array_equal(vector.get_value(), [1.0, 2.0, 3.0])
        self.assertEqual(vector.uniforms["value"].dtype, np.float64)

        # Remaining kwargs reach Mobject.__init__.
        self.assertEqual(ValueTracker(2.0, color=RED).get_color(), RED)

    # Reference numbers.py:22: lru_cached; a backslash command or "i" builds Tex, any other
    # character builds Text, each with **text_config.
    def test_char_to_cahced_mob(self):
        from manimlib import Tex, Text
        from manimlib.mobject import numbers

        size = 41.75  # an argument set no other test uses, so the first call is a cache miss
        digit = numbers.char_to_cahced_mob("7", font_size=size)
        self.assertIsInstance(digit, Text)
        self.assertNotIsInstance(digit, Tex)
        self.assertEqual(digit.text, "7")
        self.assertEqual(digit.font_size, size)

        imaginary = numbers.char_to_cahced_mob("i", font_size=size)
        self.assertIsInstance(imaginary, Tex)
        self.assertEqual(imaginary.get_tex(), "i")
        command = numbers.char_to_cahced_mob("\\pi", font_size=size)
        self.assertIsInstance(command, Tex)
        self.assertEqual(command.get_tex(), "\\pi")

        # The cache returns the same template for the same arguments, a new one otherwise.
        self.assertIs(numbers.char_to_cahced_mob("7", font_size=size), digit)
        double = numbers.char_to_cahced_mob("7", font_size=2 * size)
        self.assertIsNot(double, digit)
        self.assertAlmostEqual(double.get_height() / digit.get_height(), 2.0, places=5)

    # Reference probability.py:302: each bar (zip with values, so a prefix updates a prefix)
    # takes height value / max_value * height, keeping its bottom.
    def test_change_bar_values(self):
        from manimlib import BarChart

        chart = BarChart([0.5, 1.0, 0.25], label_y_axis=False)
        before = [(bar.get_bottom(), bar.get_width()) for bar in chart.bars]
        np.testing.assert_allclose([bar.get_height() for bar in chart.bars], [2.0, 4.0, 1.0],
                                   atol=1e-6)

        self.assertIsNone(chart.change_bar_values([0.75, 0.5, 1.0]))
        for bar, (bottom, width), height in zip(chart.bars, before, (3.0, 2.0, 4.0)):
            self.assertAlmostEqual(bar.get_height(), height, places=5)
            self.assertAlmostEqual(bar.get_width(), width, places=5)
            np.testing.assert_allclose(bar.get_bottom(), bottom, atol=1e-5)

        chart.change_bar_values([0.125])
        np.testing.assert_allclose([bar.get_height() for bar in chart.bars], [0.5, 2.0, 4.0],
                                   atol=1e-5)

        scaled = BarChart([2, 4], max_value=4, height=3, width=4, label_y_axis=False)
        bottoms = [bar.get_bottom() for bar in scaled.bars]
        scaled.change_bar_values([1, 3])
        np.testing.assert_allclose([bar.get_height() for bar in scaled.bars], [0.75, 2.25],
                                   atol=1e-5)
        for bar, bottom in zip(scaled.bars, bottoms):
            np.testing.assert_allclose(bar.get_bottom(), bottom, atol=1e-5)

    # Reference scene.py:883: copies scene.time and num_plays; maps scene.mobjects (in order,
    # minus ignore) to copies, reusing the last undo state's copy when it still looks identical.
    def test_scene_state_init(self):
        from manimlib import Circle, Scene, Square
        from manimlib.scene.scene import SceneState

        scene = Scene()
        circle, square = Circle(), Square()
        scene.add(circle, square)
        scene.wait(0.5)

        state = SceneState(scene)
        self.assertEqual(state.time, scene.time)
        self.assertGreater(state.time, 0.0)
        self.assertEqual(state.num_plays, scene.num_plays)
        self.assertEqual(state.num_plays, 1)
        self.assertEqual(_ids(state.mobjects_to_copies), _ids(scene.mobjects))
        self.assertEqual(_ids(state.mobjects_to_copies)[1:], [id(circle), id(square)])
        for mob, saved in state.mobjects_to_copies.items():
            self.assertIsNot(saved, mob)
            self.assertIs(type(saved), type(mob))
            self.assertTrue(saved.looks_identical(mob))

        ignoring = SceneState(scene, ignore=[square])
        self.assertEqual(_ids(ignoring.mobjects_to_copies), _ids(scene.mobjects[:-1]))

        scene.undo_stack.append(state)
        square.shift([1.0, 0.0, 0.0])
        later = SceneState(scene)
        self.assertIs(later.mobjects_to_copies[circle], state.mobjects_to_copies[circle])
        self.assertIsNot(later.mobjects_to_copies[square], state.mobjects_to_copies[square])
        np.testing.assert_allclose(later.mobjects_to_copies[square].get_center(), [1.0, 0, 0],
                                   atol=1e-6)
        np.testing.assert_allclose(state.mobjects_to_copies[square].get_center(), [0, 0, 0],
                                   atol=1e-6)

    # Reference interactive_scene.py:249: Scene.remove_all_except (clear, then add the kept
    # mobjects in the given order), then regenerate the selection search set; returns None.
    def test_interactive_remove_all_except(self):
        from manimlib import Circle, InteractiveScene, Square, Triangle, VGroup

        scene = InteractiveScene()
        scene.setup()
        a, b, d = Circle(), Square(), Triangle()
        scene.add(a, b, d)
        self.assertEqual(_ids(scene.mobjects),
                         _ids([scene.frame, scene.selection_highlight, a, b, d]))

        self.assertIsNone(scene.remove_all_except(d, a))
        self.assertEqual(_ids(scene.mobjects), _ids([d, a]))
        self.assertEqual(_ids(scene.get_selection_search_set()), _ids([d, a]))

        # A kept unselectable stays in the scene but out of the search set.
        scene.remove_all_except(a, scene.selection_highlight)
        self.assertEqual(_ids(scene.mobjects), _ids([a, scene.selection_highlight]))
        self.assertEqual(_ids(scene.get_selection_search_set()), _ids([a]))

        # Below the top level the search set is the kept family members with points.
        scene.select_top_level_mobs = False
        group = VGroup(Circle(), Square())
        scene.remove_all_except(group)
        self.assertEqual(_ids(scene.mobjects), _ids([group]))
        self.assertEqual(_ids(scene.get_selection_search_set()), _ids(list(group)))


if __name__ == "__main__":
    unittest.main()
