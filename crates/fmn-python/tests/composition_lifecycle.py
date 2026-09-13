"""Real-extension acceptance for authored AnimationGroup lifecycles."""
import unittest
import numpy as np
from manimlib import AnimationGroup, Scene, Square, Succession, Transform


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


suite = unittest.defaultTestLoader.loadTestsFromTestCase(CompositionLifecycleAcceptance)
result = unittest.TextTestRunner(verbosity=2).run(suite)
if result.testsRun != 5 or not result.wasSuccessful():
    raise AssertionError("Native composition lifecycle acceptance failed")
