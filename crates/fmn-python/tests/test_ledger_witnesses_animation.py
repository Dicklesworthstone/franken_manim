"""Ledger witnesses for animation (fm-6u1g): each test calls a manimlib symbol the Parity Ledger
marks same/improved and checks the Reference's behaviour (manimlib @ 6199a00d)."""
import unittest

import numpy as np

from test_parallel_batch_native import native_wheel_available


def ids(mobjects):
    return [id(m) for m in mobjects]


@unittest.skipUnless(native_wheel_available(), "requires the built FrankenManim native wheel")
class AnimationWitnesses(unittest.TestCase):
    def assertNear(self, actual, expected, atol=1e-6):
        np.testing.assert_allclose(np.asarray(actual, dtype=float), expected, atol=atol, rtol=0)

    # manimlib.animation.animation ------------------------------------------------------------

    def test_constructor_defaults_are_available_before_begin(self):
        # fm-v3oj: shipped rate/path callables are public constructor state.
        from manimlib import ApplyMethod, FadeIn, ShowCreation, Square, Transform, UP
        from manimlib import manimlib as native
        from manimlib.utils.paths import straight_path
        from manimlib.utils.rate_functions import smooth

        source, target = Square(), Square().shift(UP)
        animations = (FadeIn(source), ShowCreation(source), Transform(source, target),
                      ApplyMethod(source.shift, UP))
        for animation in animations:
            with self.subTest(animation=type(animation).__name__):
                self.assertIs(animation.rate_func, smooth)
                # ApplyMethod's target must be deferred until begin so a
                # preceding Succession child can change its source state.
                self.assertEqual(native._requires_python_animation(animation),
                                 isinstance(animation, ApplyMethod))
                self.assertFalse(hasattr(animation, "starting_mobject"))
                if isinstance(animation, Transform):
                    self.assertIs(animation.path_func, straight_path)
                    self.assertNear(animation.path_func(np.zeros((1, 3)), np.ones((1, 3)), .25),
                                    [[.25, .25, .25]])

    def test_rate_functions_keep_the_reference_names_and_pickle(self):
        import pickle
        import subprocess
        import sys

        from manimlib import FadeIn, Square, Write
        from manimlib.utils import rate_functions

        # fm-v3oj: manimlib/utils/rate_functions.py defines each at module scope, so
        # `smooth.__name__ == "smooth"` and pickle stores it by reference.
        names = ("linear", "smooth", "rush_into", "rush_from", "slow_into", "double_smooth",
                 "there_and_back", "there_and_back_with_pause", "running_start", "overshoot",
                 "not_quite_there", "wiggle", "squish_rate_func", "lingering",
                 "exponential_decay")
        for name in names:
            with self.subTest(name=name):
                function = getattr(rate_functions, name)
                self.assertEqual((function.__name__, function.__qualname__, function.__module__),
                                 (name, name, "manimlib.utils.rate_functions"))
                self.assertIs(pickle.loads(pickle.dumps(function)), function)
        self.assertIs(FadeIn(Square()).rate_func, rate_functions.smooth)
        self.assertIs(Write(Square()).rate_func, rate_functions.linear)
        # A fresh interpreter resolves the pickled reference too.
        loaded = subprocess.run(
            [sys.executable, "-c", "import pickle, sys; f = pickle.loads(sys.stdin.buffer.read());"
             " print(f.__module__, f.__name__, f(0.25))"],
            input=pickle.dumps(rate_functions.rush_into), capture_output=True, check=True)
        self.assertEqual(loaded.stdout.decode().split()[:2],
                         ["manimlib.utils.rate_functions", "rush_into"])
        self.assertAlmostEqual(float(loaded.stdout.split()[2]), rate_functions.rush_into(0.25))

    def test_transform_get_all_mobjects_returns_list(self):
        from manimlib import Square, Transform

        animation = Transform(Square(), Square())
        animation.begin()
        try:
            members = animation.get_all_mobjects()
            self.assertIs(type(members), list)
            self.assertEqual(ids(members), ids([animation.mobject, animation.starting_mobject,
                                               animation.target_mobject, animation.target_copy]))
        finally:
            animation.finish()

    def test_animation_copy(self):
        # animation.py:122: copy() is deepcopy(self). The copy is a new animation of the same
        # class whose mobjects are independent copies, with deepcopy's memo keeping internal
        # aliasing (an aligned Transform's target_copy *is* its target_mobject).
        from manimlib import BLUE, RED, RIGHT, UP, Square
        from manimlib.animation.transform import Transform
        from manimlib.utils.rate_functions import linear

        source = Square(side_length=2, color=BLUE)
        target = Square(side_length=2, color=BLUE).shift(4 * RIGHT)
        anim = Transform(source, target, rate_func=linear, run_time=2.0)
        anim.begin()
        self.assertIs(anim.target_copy, anim.target_mobject)
        source_color = source.get_color()

        dup = anim.copy()
        self.assertIs(type(dup), Transform)
        self.assertIsNot(dup, anim)
        self.assertIsNot(dup.mobject, source)
        self.assertIsNot(dup.starting_mobject, anim.starting_mobject)
        self.assertIsNot(dup.target_mobject, target)
        self.assertIs(dup.target_copy, dup.target_mobject)
        self.assertEqual(dup.run_time, 2.0)
        self.assertIs(dup.rate_func, linear)
        self.assertNear(dup.mobject.get_points(), source.get_points())
        self.assertNear(dup.target_mobject.get_points(), target.get_points())

        # Driving or mutating the copy leaves the original untouched, and vice versa.
        dup.interpolate(0.5)
        self.assertNear(dup.mobject.get_center(), [2, 0, 0])
        self.assertNear(source.get_center(), [0, 0, 0])
        anim.interpolate(0.25)
        self.assertNear(source.get_center(), [1, 0, 0])
        self.assertNear(dup.mobject.get_center(), [2, 0, 0])
        dup.set_run_time(7.0)
        dup.mobject.set_color(RED)
        dup.target_mobject.shift(UP)
        self.assertEqual(anim.run_time, 2.0)
        self.assertEqual(source.get_color(), source_color)
        self.assertNear(target.get_center(), [4, 0, 0])
        dup.finish()
        anim.finish()

    def test_animation_update(self):
        # animation.py:140: the deprecated update(alpha) is interpolate(alpha). A linear
        # Transform of a square 4 units right sits at alpha * 4, resampled from the start.
        from manimlib import RIGHT, Square
        from manimlib.animation.transform import Transform
        from manimlib.utils.rate_functions import linear

        source = Square(side_length=2)
        target = Square(side_length=2).shift(4 * RIGHT)
        anim = Transform(source, target, rate_func=linear)
        anim.begin()
        self.assertNear(source.get_center(), [0, 0, 0])
        self.assertIsNone(anim.update(0.5))
        self.assertNear(source.get_center(), [2, 0, 0])
        self.assertNear(source.get_width(), 2.0)
        anim.update(0.25)
        self.assertNear(source.get_center(), [1, 0, 0])
        twin_source = Square(side_length=2)
        twin = Transform(twin_source, Square(side_length=2).shift(4 * RIGHT), rate_func=linear)
        twin.begin()
        twin.interpolate(0.25)
        self.assertNear(source.get_points(), twin_source.get_points())
        anim.finish()
        twin.finish()

    # manimlib.animation.composition ----------------------------------------------------------

    def test_composition_constructor_sets_root_and_parent_relationships(self):
        from manimlib import AnimationGroup, Circle, FadeIn, LaggedStart, Square, Succession, VGroup
        from manimlib.utils.rate_functions import linear

        for cls in (AnimationGroup, LaggedStart, Succession):
            with self.subTest(animation=cls.__name__):
                a, b = Square(), Circle()
                animation = cls(FadeIn(a), FadeIn(b))
                root = animation.mobject
                self.assertIs(type(root), VGroup)
                self.assertIs(animation.group, root)
                self.assertEqual(ids(root.submobjects), ids([a, b]))
                self.assertIn(id(root), ids(a.parents))
                self.assertIn(id(root), ids(b.parents))
                self.assertIs(animation.rate_func, linear)

    def test_surrounding_rectangle_effects_have_roots_before_begin(self):
        from manimlib import (ShowCreationThenDestructionAround, ShowCreationThenFadeAround,
                              ShowPassingFlashAround, Square, VGroup)

        for cls in (ShowPassingFlashAround, ShowCreationThenDestructionAround,
                    ShowCreationThenFadeAround):
            with self.subTest(animation=cls.__name__):
                animation = cls(Square())
                self.assertIsInstance(animation.mobject, VGroup)
                self.assertIs(animation.group, animation.mobject)
                self.assertIn(id(animation.rectangle), ids(animation.mobject.get_family()))
                self.assertEqual(animation.mobject.get_points().shape, (0, 3))

    def test_surrounding_rectangle_follows_its_moving_mobject(self):
        import tempfile

        from manimlib import Mobject, RIGHT, Scene, ShowCreationThenFadeAround, Square, linear

        # indication.py AnimationOnSurroundingRectangle: rect.add_updater(lambda r:
        # r.move_to(mobject)), and the rectangle is the group root from construction, so its
        # updater runs on the animation's helper copies too (fm-v3oj). Measured on the pinned
        # Reference (6199a00d) at 10 fps, sampled by a scene updater: the rectangle trails the
        # shifting square by one frame. A native route that dropped the updater left it at 0.
        rows = []

        class Follow(Scene):
            def construct(self):
                square = Square()
                self.add(square)
                effect = ShowCreationThenFadeAround(square, rate_func=linear)
                recorder = Mobject()
                recorder.add_updater(lambda mob, dt: rows.append(
                    (effect.mobject[0].get_center()[0], square.get_center()[0])))
                self.add(recorder)
                self.play(effect, square.animate.shift(2 * RIGHT), run_time=0.5)

        with tempfile.TemporaryDirectory() as tmp:
            Follow().render(tmp + "/follow", format="png_sequence", resolution=(160, 90),
                            fps=10, threads=1)
        self.assertEqual([round(float(r), 3) for r, _ in rows],
                         [0.0, 0.0, 0.116, 0.635, 1.365, 1.884, 1.884])
        self.assertEqual([round(float(s), 3) for _, s in rows],
                         [0.0, 0.116, 0.635, 1.365, 1.884, 2.0, 2.0])

    def test_animation_group_get_all_mobjects(self):
        # composition.py:61: get_all_mobjects returns self.group, a single Mobject: VGroup of
        # the members' mobjects when all are VMobjects, else Group; an authored group wins.
        from manimlib import Circle, FadeIn, Group, Square, VGroup
        from manimlib.animation.composition import AnimationGroup

        a, b = Square(), Circle()
        anim = AnimationGroup(FadeIn(a), FadeIn(b))
        root = anim.get_all_mobjects()
        self.assertIsInstance(root, VGroup)
        self.assertIs(root, anim.group)
        self.assertIs(root, anim.mobject)
        self.assertEqual(ids(root.submobjects), ids([a, b]))
        self.assertIs(anim.get_all_mobjects(), root)

        authored = Group(a, b)
        self.assertIs(AnimationGroup(FadeIn(a), FadeIn(b), group=authored).get_all_mobjects(),
                      authored)

        inner = Group(Square())
        mixed = AnimationGroup(FadeIn(a), FadeIn(inner)).get_all_mobjects()
        self.assertIs(type(mixed), Group)
        self.assertEqual(ids(mixed.submobjects), ids([a, inner]))

        # composition.py:51 builds the group from remove_list_redundancies (iterables.py:17),
        # which keeps each mobject's LAST occurrence: [a, b, a] groups as [b, a] (fm-fxov).
        from manimlib import Indicate

        repeated = AnimationGroup(FadeIn(a), FadeIn(b), Indicate(a)).get_all_mobjects()
        self.assertEqual(ids(repeated.submobjects), ids([b, a]))

    # manimlib.animation.creation -------------------------------------------------------------

    def test_show_increasing_subsets_init(self):
        # creation.py:177: the constructor freezes group.submobjects, keeps int_func (default
        # np.round), and interpolate_mobject shows all_submobs[:int(int_func(rate(a) * n))].
        from manimlib import RIGHT, Square, VGroup
        from manimlib.animation.creation import ShowIncreasingSubsets
        from manimlib.utils.rate_functions import linear

        group = VGroup(*[Square(side_length=0.5).shift(i * RIGHT) for i in range(5)])
        subs = list(group.submobjects)
        anim = ShowIncreasingSubsets(group, rate_func=linear)
        self.assertIs(anim.mobject, group)
        self.assertIs(anim.int_func, np.round)
        self.assertEqual(ids(anim.all_submobs), ids(subs))
        self.assertFalse(anim.suspend_mobject_updating)
        self.assertEqual(anim.run_time, 1.0)
        group.add(Square())  # added after construction: never part of the reveal
        anim.begin()
        # round(1.5) = 2 and round(2.5) = 2 (NumPy rounds half to even), round(3.1) = 3.
        for alpha, count in ((0.0, 0), (0.3, 2), (0.5, 2), (0.62, 3), (1.0, 5)):
            anim.interpolate(alpha)
            self.assertEqual(ids(group.submobjects), ids(subs[:count]), alpha)
        anim.finish()
        self.assertEqual(ids(group.submobjects), ids(subs))

        group = VGroup(*[Square(side_length=0.5).shift(i * RIGHT) for i in range(5)])
        subs = list(group.submobjects)
        floor = ShowIncreasingSubsets(group, int_func=np.floor, rate_func=linear,
                                      suspend_mobject_updating=True)
        self.assertIs(floor.int_func, np.floor)
        self.assertTrue(floor.suspend_mobject_updating)
        floor.begin()
        floor.interpolate(0.58)  # floor(2.9) = 2
        self.assertEqual(ids(group.submobjects), ids(subs[:2]))
        floor.finish()

    def test_show_submobjects_one_by_one_init(self):
        # creation.py:203: int_func defaults to np.ceil; one submobject at a time,
        # all_submobs[clip(index, 0, n - 1) - 1], none at index 0 (so the last one never shows).
        from manimlib import RIGHT, Square, VGroup
        from manimlib.animation.creation import ShowSubmobjectsOneByOne
        from manimlib.utils.rate_functions import linear

        group = VGroup(*[Square(side_length=0.5).shift(i * RIGHT) for i in range(5)])
        subs = list(group.submobjects)
        anim = ShowSubmobjectsOneByOne(group, rate_func=linear)
        self.assertIs(anim.mobject, group)
        self.assertIs(anim.int_func, np.ceil)
        self.assertEqual(ids(anim.all_submobs), ids(subs))
        anim.begin()
        for alpha, shown in ((0.0, None), (0.1, 0), (0.5, 2), (0.75, 3), (0.9, 3), (1.0, 3)):
            anim.interpolate(alpha)
            expected = [] if shown is None else [subs[shown]]
            self.assertEqual(ids(group.submobjects), ids(expected), alpha)
        anim.finish()

        group = VGroup(*[Square(side_length=0.5).shift(i * RIGHT) for i in range(5)])
        subs = list(group.submobjects)
        floor = ShowSubmobjectsOneByOne(group, int_func=np.floor, rate_func=linear)
        self.assertIs(floor.int_func, np.floor)
        floor.begin()
        floor.interpolate(0.5)  # floor(2.5) = 2 -> all_submobs[1]
        self.assertEqual(ids(group.submobjects), ids([subs[1]]))
        floor.finish()

    # manimlib.animation.fading ---------------------------------------------------------------

    def test_fade_transform_clean_up_from_scene(self):
        # fading.py:132: remove the animated group, restore the source to its saved
        # (construction-time) state, and add the target unless the animation is a remover.
        # Driven in Scene.play's order: begin, add mobject, interpolate, finish, clean up.
        from manimlib import BLUE, RIGHT, Circle, Scene, Square
        from manimlib.animation.fading import FadeTransform
        from manimlib.utils.rate_functions import linear

        for remover in (False, True):
            scene = Scene()
            source = Square(fill_opacity=1).set_color(BLUE)
            target = Circle(fill_opacity=1).shift(3 * RIGHT)
            scene.add(source)
            before = source.get_points().copy()
            color = source.get_color()
            anim = FadeTransform(source, target, rate_func=linear, remover=remover)
            anim.begin()
            scene.add(anim.mobject)
            anim.interpolate(0.5)
            anim.finish()
            # At final alpha the source is a transparent ghost on the target's place.
            self.assertEqual(ids(scene.mobjects), ids([scene.frame, anim.mobject]))
            self.assertNear(source.get_center(), [3, 0, 0])
            self.assertNear(source.get_fill_opacity(), 0.0)

            anim.clean_up_from_scene(scene)
            expected = [scene.frame] if remover else [scene.frame, target]
            self.assertEqual(ids(scene.mobjects), ids(expected), remover)
            self.assertNear(source.get_points(), before)
            self.assertNear(source.get_fill_opacity(), 1.0)
            self.assertEqual(source.get_color(), color)
            self.assertNear(target.get_center(), [3, 0, 0])

    def test_fade_transform_get_all_mobjects(self):
        # fading.py:122: [mobject, starting_mobject, ending_mobject]. After begin the start has
        # the target ghosted onto the source and the end has the source ghosted onto the target.
        from manimlib import RIGHT, Rectangle, Square
        from manimlib.animation.fading import FadeTransform
        from manimlib.utils.rate_functions import linear

        source = Square(side_length=2, fill_opacity=1, stroke_width=0)
        target = Rectangle(width=4, height=2, fill_opacity=1, stroke_width=0).shift(4 * RIGHT)
        anim = FadeTransform(source, target, rate_func=linear)
        anim.begin()
        mobs = anim.get_all_mobjects()
        self.assertEqual(len(mobs), 3)
        self.assertIs(mobs[0], anim.mobject)
        self.assertIs(mobs[1], anim.starting_mobject)
        self.assertIs(mobs[2], anim.ending_mobject)
        self.assertIs(mobs[0][0], source)
        self.assertEqual(len(set(ids(mobs))), 3)
        start, end = mobs[1], mobs[2]
        self.assertNear(start[0].get_center(), [0, 0, 0])
        self.assertNear(start[0].get_fill_opacity(), 1.0)
        self.assertNear(start[1].get_center(), [0, 0, 0])
        self.assertNear([start[1].get_width(), start[1].get_height()], [2, 2])
        self.assertNear(start[1].get_fill_opacity(), 0.0)
        self.assertNear(end[0].get_center(), [4, 0, 0])
        self.assertNear([end[0].get_width(), end[0].get_height()], [4, 2])
        self.assertNear(end[0].get_fill_opacity(), 0.0)
        self.assertNear(end[1].get_center(), [4, 0, 0])
        self.assertNear(end[1].get_fill_opacity(), 1.0)
        # The family zip of these three drives interpolation: halfway is halfway.
        anim.interpolate(0.5)
        self.assertNear(source.get_center(), [2, 0, 0])
        self.assertNear(source.get_width(), 3.0)
        anim.abort()

    def test_fade_transform_pieces_ghost_to(self):
        # fading.py:145: zip the two families and, per pair, replace(target, stretch,
        # dim_to_match), take the target's uniforms, then set opacity 0.
        from manimlib import DOWN, LEFT, RIGHT, UP, Circle, Rectangle, Square, VGroup
        from manimlib.animation.fading import FadeTransformPieces

        def pieces():
            source = VGroup(Square(side_length=1, fill_opacity=1).shift(2 * LEFT),
                            Circle(radius=0.5, fill_opacity=1).shift(2 * RIGHT))
            target = VGroup(Rectangle(width=2, height=1, fill_opacity=1).shift(2 * UP),
                            Square(side_length=3, fill_opacity=1).shift(2 * DOWN + RIGHT))
            return source, target

        anim = FadeTransformPieces(*pieces(), stretch=True)
        source, target = pieces()
        target[1].fix_in_frame()
        self.assertFalse(source[1].is_fixed_in_frame())
        self.assertIsNone(anim.ghost_to(source, target))
        for piece, goal in zip(source, target):
            self.assertNear(piece.get_center(), goal.get_center())
            self.assertNear([piece.get_width(), piece.get_height()],
                            [goal.get_width(), goal.get_height()])
            self.assertNear([piece.get_fill_opacity(), piece.get_stroke_opacity()], [0, 0])
        self.assertTrue(source[1].is_fixed_in_frame())
        self.assertFalse(source[0].is_fixed_in_frame())

        # No stretch, match heights: each piece keeps its aspect ratio (both are 1 x 1).
        anim = FadeTransformPieces(*pieces(), stretch=False, dim_to_match=1)
        source, target = pieces()
        anim.ghost_to(source, target)
        self.assertNear(source[0].get_center(), [0, 2, 0])
        self.assertNear([source[0].get_width(), source[0].get_height()], [1, 1])
        self.assertNear(source[1].get_center(), [1, -2, 0])
        self.assertNear([source[1].get_width(), source[1].get_height()], [3, 3])
        self.assertNear([source[0].get_fill_opacity(), source[1].get_fill_opacity()], [0, 0])

    # manimlib.animation.movement -------------------------------------------------------------

    def test_phase_flow_init(self):
        # movement.py:75: stores the field, virtual_time = virtual_time or run_time,
        # rate_func linear, run_time 3; interpolation is the stateful forward-Euler step
        # p += virtual_time * (alpha - last_alpha) * function(p).
        from manimlib import RIGHT, Dot
        from manimlib.animation.movement import PhaseFlow
        from manimlib.utils.rate_functions import linear

        def rotation(p):
            return np.array([-p[1], p[0], 0.0])

        dot = Dot(RIGHT)
        flow = PhaseFlow(rotation, dot, run_time=2.0)
        self.assertIs(flow.function, rotation)
        self.assertIs(flow.mobject, dot)
        self.assertEqual(flow.virtual_time, 2.0)
        self.assertEqual(flow.run_time, 2.0)
        self.assertIs(flow.rate_func, linear)
        self.assertFalse(flow.suspend_mobject_updating)
        flow.begin()
        self.assertNear(dot.get_center(), [1, 0, 0])
        flow.interpolate(0.25)  # dt = 0.5
        self.assertNear(dot.get_center(), [1, 0.5, 0])
        flow.interpolate(0.5)   # dt = 0.5 from (1, 0.5)
        self.assertNear(dot.get_center(), [0.75, 1.0, 0])
        flow.finish()

        def constant(p):
            return np.array([1.0, 0.0, 0.0])

        dot = Dot()
        flow = PhaseFlow(constant, dot, virtual_time=5, suspend_mobject_updating=True)
        self.assertEqual(flow.virtual_time, 5.0)
        self.assertEqual(flow.run_time, 3.0)
        self.assertTrue(flow.suspend_mobject_updating)
        flow.begin()
        flow.interpolate(0.2)
        self.assertNear(dot.get_center(), [1, 0, 0])
        flow.finish()
        self.assertNear(dot.get_center(), [5, 0, 0])

    def test_smoothed_vectorized_homotopy(self):
        # movement.py:51: a Homotopy whose apply_function passes make_smooth=True, so every
        # sample is VMobject.make_smooth(approx=True) of the mapped start. For the unit square
        # (anchors UR, UL, DL, DR, UR) approx_smooth_quadratic_bezier_handles gives handles
        # (0, 1.5), (-1.5, 0), (0, -1.5), (1.5, 0); the map (x + t, y, z) at t = 0.5 shifts all.
        from manimlib import Square
        from manimlib.animation.movement import Homotopy, SmoothedVectorizedHomotopy
        from manimlib.utils.rate_functions import linear

        def shift_right(x, y, z, t):
            return (x + t, y, z)

        square = Square()
        anim = SmoothedVectorizedHomotopy(shift_right, square, rate_func=linear)
        self.assertIs(type(anim), SmoothedVectorizedHomotopy)
        self.assertIs(anim.homotopy, shift_right)
        self.assertEqual(anim.run_time, 3.0)
        self.assertEqual(anim.apply_function_config, {"make_smooth": True})
        anim.begin()
        anim.interpolate(0.5)
        corners = np.array([[1, 1], [-1, 1], [-1, -1], [1, -1], [1, 1]], dtype=float)
        handles = np.array([[0, 1.5], [-1.5, 0], [0, -1.5], [1.5, 0]])
        expected = np.zeros((9, 3))
        expected[0::2, :2] = corners
        expected[1::2, :2] = handles
        expected[:, 0] += 0.5
        self.assertNear(square.get_points(), expected)
        anim.finish()
        expected[:, 0] += 0.5
        self.assertNear(square.get_points(), expected)

        # A plain Homotopy of the same map keeps the jagged square: the smoothing is this class's.
        plain = Square()
        homotopy = Homotopy(shift_right, plain, rate_func=linear)
        homotopy.begin()
        homotopy.interpolate(0.5)
        self.assertNear(plain.get_points()[1], [0.5, 1, 0])
        homotopy.finish()

    # manimlib.animation.transform ------------------------------------------------------------

    def test_transform_update_config(self):
        # BN-12 §8: the Reference's body (transform.py:95) makes the kwargs attributes and
        # rebuilds path_func from path_arc about the given axis or OUT; at the pin its call
        # to the absent Animation.update_config raises, so the portal does what it intends.
        from manimlib import IN, OUT, PI, RIGHT, Square, Transform
        from manimlib.utils.paths import clockwise_path, path_along_arc

        start, end = np.array([[1.0, 0.0, 0.0]]), np.array([[-1.0, 0.0, 0.0]])
        transform = Transform(Square(), Square().shift(RIGHT), path_func=clockwise_path())
        self.assertNear(transform.path_arc, -PI)
        self.assertIsNone(transform.update_config(path_arc=PI / 2, run_time=2.5))
        self.assertEqual(transform.run_time, 2.5)
        self.assertNear(transform.path_arc, PI / 2)
        self.assertNear(transform.path_arc_axis, OUT)
        self.assertNear(transform.path_func(start, end, 0.5),
                        path_along_arc(PI / 2, OUT)(start, end, 0.5))
        # An axis given with the arc is kept; an axis alone changes only the axis.
        transform.update_config(path_arc=0.5, path_arc_axis=IN)
        self.assertNear(transform.path_arc_axis, IN)
        self.assertNear(transform.path_func(start, end, 0.5),
                        path_along_arc(0.5, IN)(start, end, 0.5))
        transform.update_config(path_arc_axis=OUT)
        self.assertNear(transform.path_arc, 0.5)
        self.assertNear(transform.path_arc_axis, OUT)

    def test_transform_get_all_mobjects(self):
        # transform.py:103: [mobject, starting_mobject, target_mobject, target_copy]. An
        # unaligned target is aligned through a copy, so the target itself stays untouched;
        # update_mobjects updates every one but the animated mobject.
        from manimlib import RIGHT, Circle, Square
        from manimlib.animation.transform import Transform

        source = Square()
        target = Circle().shift(RIGHT)
        target_points = target.get_points().copy()
        calls = []
        target.add_updater(lambda m, dt: calls.append((id(m), dt)))
        anim = Transform(source, target)
        anim.begin()
        mobs = anim.get_all_mobjects()
        self.assertEqual(len(mobs), 4)
        self.assertIs(mobs[0], source)
        self.assertIs(mobs[1], anim.starting_mobject)
        self.assertIs(mobs[2], target)
        self.assertIs(mobs[3], anim.target_copy)
        self.assertIsNot(mobs[1], source)
        self.assertIsNot(mobs[3], target)
        self.assertNear(target.get_points(), target_points)
        counts = {m.get_num_points() for m in (mobs[0], mobs[1], mobs[3])}
        self.assertEqual(counts, {target.get_num_points()})
        self.assertNear(mobs[1].get_center(), [0, 0, 0])
        self.assertNear(mobs[3].get_center(), [1, 0, 0])
        calls.clear()
        anim.update_mobjects(0.1)
        self.assertEqual(calls, [(id(target), 0.1), (id(anim.target_copy), 0.1)])
        anim.finish()

        # An already aligned target needs no copy: target_copy is target_mobject.
        aligned_target = Square().shift(RIGHT)
        aligned = Transform(Square(), aligned_target)
        aligned.begin()
        mobs = aligned.get_all_mobjects()
        self.assertIs(mobs[2], aligned_target)
        self.assertIs(mobs[3], aligned_target)
        aligned.finish()


if __name__ == "__main__":
    unittest.main()
