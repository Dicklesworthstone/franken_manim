"""Real-extension acceptance for authored AnimationGroup lifecycles."""
import unittest
import numpy as np
from pathlib import Path
import tempfile

from manimlib import (
    BLUE, LEFT, RED, RIGHT, UP, AnimationGroup, Arrow, Circle, DrawBorderThenFill, FadeIn, Flash,
    Group, LaggedStart, Line, Point, Rotate, Scene, ShowCreation, Square, Succession, Transform,
    VGroup, VShowPassingFlash, WiggleOutThenIn,
)
from fmn_python import render_session


class RecordingGroup(AnimationGroup):
    def begin(self):
        self.begin_count = getattr(self, "begin_count", 0) + 1
        self.positions = []
        super().begin()
    def interpolate(self, alpha):
        super().interpolate(alpha)
        self.positions.append((float(alpha), self.animations[0].mobject.get_center().copy()))
    def finish(self):
        super().finish()
        self.finish_count = getattr(self, "finish_count", 0) + 1


class CompositionLifecycleAcceptance(unittest.TestCase):
    def test_member_preparation_keeps_iterables_builders_and_native_identity(self):
        for container in (list, tuple, iter):
            with self.subTest(container=container.__name__):
                first, second = Square(), Circle()
                transform = Transform(first, first.copy().shift(RIGHT))
                builder = second.animate.shift(UP)
                build = builder.build
                calls = []

                def counted_build():
                    calls.append(True)
                    return build()

                builder.build = counted_build
                group = AnimationGroup(container([transform, builder]), run_time=2 / 30)
                self.assertIs(group.animations[0], transform)
                self.assertEqual(calls, [True])
                Scene().play(group)
                self.assertEqual(calls, [True], "playing a prepared member rebuilt it")
                np.testing.assert_allclose(first.get_center(), RIGHT, atol=1e-6)
                np.testing.assert_allclose(second.get_center(), UP, atol=1e-6)

    def test_invalid_members_fail_before_authored_timing_or_group_construction(self):
        calls = []

        class ObservedGroup(AnimationGroup):
            def build_animations_with_timings(self, lag_ratio):
                calls.append("timing")
                super().build_animations_with_timings(lag_ratio)

        for container in (list, tuple, iter):
            with self.subTest(container=container.__name__):
                with self.assertRaisesRegex(
                    TypeError, r"^Object 42 cannot be converted to an animation$"
                ):
                    ObservedGroup(container([FadeIn(Square()), 42]),
                                  group_type=lambda *members: calls.append("group"))
        self.assertEqual(calls, [])

    def test_native_motion_runs_inside_authored_group(self):
        source = Square()
        target = source.copy().shift((2, 0, 0))
        group = RecordingGroup(Transform(source, target, rate_func=lambda a: a), run_time=2 / 30)
        scene = Scene()
        scene.add(source)
        scene.play(group)
        self.assertEqual(group.begin_count, 1)
        self.assertEqual(group.finish_count, 1)
        self.assertTrue(any(abs(alpha - .5) < 1e-7 and np.allclose(point, [1, 0, 0])
                            for alpha, point in group.positions))
        np.testing.assert_allclose(source.get_center(), [2, 0, 0], atol=1e-6)
        self.assertIsNone(group._composition_driver)
        self.assertIsNone(group._composition_scene)

    def test_nested_custom_group_keeps_hooks(self):
        source = Square()
        inner = RecordingGroup(Transform(source, source.copy().shift((2, 0, 0))))
        scene = Scene()
        scene.add(source)
        scene.play(AnimationGroup(inner), run_time=2 / 30)
        self.assertEqual(inner.begin_count, 1)
        self.assertEqual(inner.finish_count, 1)
        np.testing.assert_allclose(source.get_center(), [2, 0, 0], atol=1e-6)

    def test_mixed_succession_snapshots_previous_native_result(self):
        source = Square()
        start_centers = []
        class AuthoredTransform(Transform):
            def begin(self):
                start_centers.append(self.mobject.get_center().copy())
                super().begin()
        class AuthoredSuccession(Succession):
            def begin(self):
                self.called = True
                super().begin()
        sequence = AuthoredSuccession(
            Transform(source, source.copy().shift((1, 0, 0)), run_time=2 / 30),
            AuthoredTransform(source, source.copy().shift((2, 0, 0)), run_time=2 / 30),
        )
        scene = Scene()
        scene.add(source)
        scene.play(sequence)
        self.assertTrue(sequence.called)
        self.assertEqual(len(start_centers), 1)
        np.testing.assert_allclose(start_centers[0], [1, 0, 0], atol=1e-6)
        np.testing.assert_allclose(source.get_center(), [2, 0, 0], atol=1e-6)

    def test_direct_lifecycle_with_scene_bound_member(self):
        source = Square()
        scene = Scene()
        scene.add(source)
        group = AnimationGroup(Transform(source, source.copy().shift((2, 0, 0)), rate_func=lambda a: a))
        group.begin()
        group.interpolate(.5)
        np.testing.assert_allclose(source.get_center(), [1, 0, 0], atol=1e-6)
        group.finish()
        group.clean_up_from_scene(scene)
        np.testing.assert_allclose(source.get_center(), [2, 0, 0], atol=1e-6)

    def test_override_failure_releases_native_state(self):
        class BrokenGroup(AnimationGroup):
            def begin(self):
                super().begin()
                raise ValueError("authored group failure")
        source = Square()
        group = BrokenGroup(Transform(source, source.copy().shift((1, 0, 0))),
                            suspend_mobject_updating=True, run_time=2 / 30)
        scene = Scene()
        scene.add(source)
        with self.assertRaisesRegex(ValueError, "authored group failure"):
            scene.play(group)
        self.assertFalse(group.mobject._is_updating_suspended())
        self.assertFalse(source.is_changing())
        self.assertIsNone(group._composition_driver)
        self.assertIsNone(group._composition_scene)


    def test_explicit_root_is_the_played_root(self):
        source = Square()
        root = VGroup(source)
        group = AnimationGroup(Transform(source, source.copy().shift((2, 0, 0))),
                               group=root, run_time=2 / 30)
        self.assertIs(group.group, root)
        self.assertIs(group.mobject, root)
        scene = Scene()
        scene.play(group)
        self.assertTrue(any(mobject is root for mobject in scene.mobjects))
        np.testing.assert_allclose(source.get_center(), [2, 0, 0], atol=1e-6)

    def test_authored_group_factory_keeps_native_members(self):
        source = Square()
        roots = []
        def factory(*mobjects):
            root = VGroup(*mobjects)
            roots.append(root)
            return root
        group = AnimationGroup(source.animate.shift((2, 0, 0)), group_type=factory,
                               run_time=2 / 30)
        self.assertEqual(len(roots), 1)
        self.assertIs(group.mobject, roots[0])
        Scene().play(group)
        np.testing.assert_allclose(source.get_center(), [2, 0, 0], atol=1e-6)

    def test_authored_timing_table_delays_second_member(self):
        class DelayedGroup(AnimationGroup):
            def build_animations_with_timings(self, lag_ratio):
                self.anims_with_timings = [(member, index * 2., index * 2. + 1.)
                                           for index, member in enumerate(self.animations)]
            def interpolate(self, alpha):
                super().interpolate(alpha)
                self.samples.append((float(alpha), first.get_center().copy(), second.get_center().copy()))
        first, second = Square(), Square().shift((0, 3, 0))
        group = DelayedGroup(
            Transform(first, first.copy().shift((2, 0, 0)), rate_func=lambda a: a),
            Transform(second, second.copy().shift((2, 0, 0)), rate_func=lambda a: a),
            run_time=6 / 30,
        )
        group.samples = []
        Scene().play(group)
        self.assertTrue(any(abs(alpha - .5) < 1e-7
                            and np.allclose(a, [2, 0, 0]) and np.allclose(b, [0, 3, 0])
                            for alpha, a, b in group.samples))
        np.testing.assert_allclose(first.get_center(), [2, 0, 0], atol=1e-6)
        np.testing.assert_allclose(second.get_center(), [2, 3, 0], atol=1e-6)


    def test_following_flash_retains_its_shipped_leaf_lifecycle(self):
        source = Square().shift((2, 1, 0))
        flash = Flash(source, run_time=2 / 30)
        scene = Scene()
        scene.add(source)
        scene.play(flash)
        self.assertFalse(any(mobject is flash.lines for mobject in scene.mobjects))
        np.testing.assert_allclose(source.get_center(), [2, 1, 0], atol=1e-6)

    def test_played_groups_are_vgroups_over_vmobjects(self):
        # fm-3jw2, composition.py:44-51: the group a play adds is a VGroup
        # when every animated mobject is a VMobject, a Group otherwise. The
        # pinned Reference's roots for these plays are CameraFrame, VGroup,
        # VGroup, Group (the portal used Group for all three).
        scene = Scene()
        scene.play(LaggedStart(ShowCreation(Line(LEFT, RIGHT)), FadeIn(Square()), lag_ratio=0.5),
                   run_time=2 / 30)
        scene.play(AnimationGroup(FadeIn(Circle()), FadeIn(Square())), run_time=2 / 30)
        scene.play(AnimationGroup(FadeIn(Circle()), FadeIn(Point())), run_time=2 / 30)
        roots = [mobject for mobject in scene.mobjects if mobject is not scene.frame]
        self.assertEqual([type(root) for root in roots], [VGroup, VGroup, Group])
        self.assertEqual([len(root.submobjects) for root in roots], [2, 2, 2])

    def test_a_later_plain_animation_draws_over_an_earlier_composition(self):
        # fm-1auv: the Reference roots animated mobjects in animation order, so the arrow
        # played after the LaggedStart is drawn over its squares.
        width, height = 320, 180
        scene = Scene()
        squares = VGroup(*[Square(side_length=2, fill_color=BLUE, fill_opacity=1, stroke_width=0)
                           for _ in range(3)])
        arrow = Arrow(3 * LEFT, 3 * RIGHT, buff=0, stroke_width=40, stroke_color=RED)
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "order.y4m"
            with render_session(scene, path, resolution=(width, height), fps=8, threads=1):
                scene.play(LaggedStart(*map(DrawBorderThenFill, squares)), ShowCreation(arrow),
                           run_time=0.5)
            data = path.read_bytes()
        self.assertEqual([type(root).__name__ for root in scene.mobjects][-2:], ["VGroup", "Arrow"])
        frame = data[data.rindex(b"FRAME\n") + 6:][:width * height]
        luma = np.frombuffer(frame, dtype=np.uint8).reshape(height, width).astype(int)
        units = width / scene.frame.get_width()

        def pixel(point):
            return luma[int(round(height / 2 - point[1] * units)), int(round(width / 2 + point[0] * units))]

        overlap = pixel(0.4 * RIGHT)
        arrow_only, square_only = pixel(2 * RIGHT), pixel(0.4 * RIGHT + 0.6 * UP)
        self.assertLess(abs(overlap - arrow_only), abs(overlap - square_only),
                        (overlap, arrow_only, square_only))

    def test_an_unadded_animated_mobject_runs_its_updaters_during_the_play(self):
        # fm-1auv: native begin roots the mobject, so the play must release for its
        # updaters before it is rooted. In the Reference every kind runs them once per
        # frame; a remover leaves the scene before the finishing dt = 0 pass.
        def played(make):
            scene, mob, calls = Scene(), Square(), []
            mob.add_updater(lambda obj, dt: calls.append(dt) if obj is mob else None, call=False)
            scene.play(make(mob), run_time=0.25)
            return calls, any(mob in root.get_family() for root in scene.mobjects)

        rotate, in_scene = played(lambda mob: Rotate(mob, 1.0))
        self.assertTrue(in_scene)
        self.assertGreater(len(rotate), 2)
        self.assertEqual(rotate[-1], 0.0)
        self.assertEqual(played(WiggleOutThenIn), (rotate, True))
        self.assertEqual(played(VShowPassingFlash), (rotate[:-1], False))


suite = unittest.defaultTestLoader.loadTestsFromTestCase(CompositionLifecycleAcceptance)
result = unittest.TextTestRunner(verbosity=2).run(suite)
if result.testsRun != 14 or not result.wasSuccessful():
    raise AssertionError("Native composition lifecycle acceptance failed")
