"""Python protocol tests; fixture records are not native-render acceptance."""

import copy
import importlib.util
from pathlib import Path
from types import ModuleType
import unittest

import numpy as np


SOURCE = Path(__file__).resolve().parents[1] / "crates/fmn-python/python/manimlib/_animation_semantics.py"
spec = importlib.util.spec_from_file_location("animation_protocol_under_test", SOURCE)
protocol = importlib.util.module_from_spec(spec)
spec.loader.exec_module(protocol)


def fixture():
    native = ModuleType("fixture_native")

    class Mobject:
        pointlike_data_keys = ("point",)

        def __init__(self, point=(0, 0, 0)):
            self.data = np.zeros(1, dtype=[("point", float, (3,)), ("rgba", float, (4,))])
            self.data["point"] = point
            self.uniforms = {"opacity": 1.0}
            self.locked_data_keys = set()
            self.const_data_keys = set()
            self.locked_uniform_keys = set()
            self.suspended = False
            self.lock_calls = 0
            self.changed = 0

        def copy(self):
            return copy.deepcopy(self)

        def get_family(self):
            return [self]

        def is_aligned_with(self, other):
            return self.data.shape == other.data.shape

        def align_data_and_family(self, other):
            assert self.is_aligned_with(other)

        def note_changed_data(self):
            self.changed += 1

        def set_animating_status(self, status):
            self.animating = status

        def _is_updating_suspended(self):
            return self.suspended

        def suspend_updating(self):
            self.suspended = True

        def resume_updating(self):
            self.suspended = False

        def has_updaters(self):
            return False

        def lock_matching_data(self, start, target):
            self.lock_calls += 1
            self.locked_data_keys.update(
                key for key in self.data.dtype.names
                if np.array_equal(start.data[key], target.data[key])
            )

        def unlock_data(self):
            self.locked_data_keys.clear()

        def update(self, dt):
            pass

    class Animation:
        pass

    class NativeAnimation(Animation):
        pass

    def refuse(name, options):
        for key, blocked in options:
            if blocked:
                raise NotImplementedError(f"{name}: {key}")

    class Transform(NativeAnimation):
        _native_kind = "transform"
        _target_attr = "target_mobject"

        def __init__(self, mobject, target_mobject=None, path_arc=0.0,
                     path_arc_axis=(0, 0, 1), path_func=None, **kwargs):
            # The bootstrap constructor's pre-callback contract.
            if path_func is not None:
                routed_arc = getattr(path_func, "_fmn_path_arc", None)
                if routed_arc is None:
                    refuse("Transform()", [("path_func", True)])
                path_arc = float(routed_arc)
                path_arc_axis = getattr(path_func, "_fmn_path_axis", path_arc_axis)
            super().__init__(mobject, **kwargs)
            self.target_mobject = target_mobject
            self.path_arc = float(path_arc)
            self.path_arc_axis = tuple(path_arc_axis)
            self.path_func = path_func

    class ReplacementTransform(Transform):
        pass

    class TransformFromCopy(Transform):
        pass

    class DrawBorderThenFill(NativeAnimation):
        pass

    class FadeTransform(Transform):
        pass

    class FadeTransformPieces(FadeTransform):
        pass

    class AnimationGroup(NativeAnimation):
        pass

    def straight(start, end, alpha):
        return (1 - alpha) * start + alpha * end

    straight._fmn_path_arc = 0.0

    def requires(animation):
        if not getattr(animation, "_native_kind", None):
            return True
        if not isinstance(animation, Transform) or animation._target_attr is None:
            return False
        for name in ("begin", "finish", "update_mobjects", "interpolate",
                     "interpolate_mobject", "interpolate_submobject", "clean_up_from_scene"):
            method = getattr(animation, name)
            if getattr(method, "__func__", method) is not getattr(Transform, name):
                return True
        return False

    native.__dict__.update(
        Mobject=Mobject, Animation=Animation, _NativeAnimation=NativeAnimation,
        Transform=Transform, ReplacementTransform=ReplacementTransform,
        TransformFromCopy=TransformFromCopy, DrawBorderThenFill=DrawBorderThenFill,
        FadeTransform=FadeTransform, FadeTransformPieces=FadeTransformPieces,
        AnimationGroup=AnimationGroup, _np=np, _copy=copy, _interpolate=straight,
        _smooth_rate=lambda alpha: alpha, _refuse_unrouted=refuse, _OUT=(0, 0, 1),
        straight_path=straight, _requires_python_animation=requires,
    )
    protocol.install(native)
    return native


def excursion(start, end, alpha):
    return (1 - alpha) * start + alpha * end + np.array([0, 12 * alpha * (1 - alpha), 0])


class TransformPathProtocolTests(unittest.TestCase):
    def setUp(self):
        self.n = fixture()
        self.source = self.n.Mobject()
        self.target = self.n.Mobject((4, 0, 0))

    def test_authored_path_constructor_and_dispatch(self):
        animation = self.n.Transform(self.source, self.target, path_func=excursion)
        self.assertIs(animation.path_func, excursion)
        self.assertTrue(self.n._requires_python_animation(animation))

    def test_analytic_midpoint_and_nonpoint_fields(self):
        self.target.data["rgba"] = 1
        self.target.uniforms["opacity"] = 0.25
        animation = self.n.Transform(self.source, self.target, path_func=excursion)
        animation.begin()
        animation.interpolate(0.5)
        np.testing.assert_allclose(self.source.data["point"], [[2, 3, 0]])
        np.testing.assert_allclose(self.source.data["rgba"], [[0.5] * 4])
        self.assertEqual(self.source.uniforms["opacity"], 0.625)
        animation.finish()
        np.testing.assert_allclose(self.source.data["point"], [[4, 0, 0]])
        self.assertFalse(self.source.animating)

    def test_closed_excursion_does_not_lock_equal_endpoints(self):
        animation = self.n.Transform(self.source, self.source.copy(), path_func=excursion)
        animation.begin()
        animation.interpolate(0.5)
        np.testing.assert_allclose(self.source.data["point"], [[0, 3, 0]])
        self.assertEqual(self.source.lock_calls, 0)
        animation.finish()
        np.testing.assert_allclose(self.source.data["point"], [[0, 0, 0]])

    def test_explicit_user_lock_is_not_removed_at_begin(self):
        self.source.locked_data_keys.add("point")
        animation = self.n.Transform(self.source, self.target, path_func=excursion)
        animation.begin()
        animation.interpolate(0.5)
        np.testing.assert_allclose(self.source.data["point"], [[0, 0, 0]])
        self.assertIn("point", self.source.locked_data_keys)

    def test_default_and_native_factory_keep_native_route(self):
        for path in (None, self.n.straight_path):
            animation = self.n.Transform(self.source, self.target, path_func=path)
            self.assertFalse(self.n._requires_python_animation(animation))
            animation.begin()
            self.assertEqual(self.source.lock_calls, 1)
            animation.finish()
            self.source.lock_calls = 0

    def test_native_arc_metadata_is_preserved(self):
        def arc(start, end, alpha):
            raise AssertionError("native arc factory must not be sampled by construction")
        arc._fmn_path_arc = 1.5
        arc._fmn_path_axis = (1, 0, 0)
        animation = self.n.Transform(self.source, self.target, path_func=arc)
        self.assertEqual(animation.path_arc, 1.5)
        self.assertEqual(animation.path_arc_axis, (1, 0, 0))
        self.assertFalse(self.n._requires_python_animation(animation))

    def test_live_path_assignment_changes_dispatch(self):
        animation = self.n.Transform(self.source, self.target)
        animation.path_func = excursion
        self.assertTrue(self.n._requires_python_animation(animation))
        animation.path_func = self.n.straight_path
        self.assertFalse(self.n._requires_python_animation(animation))

    def test_noncallable_path_is_rejected(self):
        for value in (1, "path", object()):
            with self.assertRaisesRegex(TypeError, "path_func must be callable"):
                self.n.Transform(self.source, self.target, path_func=value)

    def test_targetless_native_transforms_remain_explicitly_unsupported(self):
        class Swap(self.n.Transform):
            _target_attr = None
        with self.assertRaisesRegex(NotImplementedError, "path_func"):
            Swap(self.source, path_func=excursion)

    def test_replacement_and_copy_keep_source_and_target_identity(self):
        replacement = self.n.ReplacementTransform(self.source, self.target, path_func=excursion)
        self.assertTrue(self.n._requires_python_animation(replacement))
        copied = self.n.TransformFromCopy(self.source, self.target, path_func=excursion)
        self.assertIsNot(copied.mobject, self.source)
        self.assertIs(copied.target_mobject, self.target)
        copied.begin()
        copied.interpolate(0.5)
        np.testing.assert_allclose(copied.mobject.data["point"], [[2, 3, 0]])
        np.testing.assert_allclose(self.source.data["point"], [[0, 0, 0]])

    def test_rate_and_time_span_are_applied_before_path(self):
        animation = self.n.Transform(self.source, self.target, path_func=excursion,
                                     run_time=2, time_span=(0.5, 1.5),
                                     rate_func=lambda alpha: alpha * alpha)
        animation.begin()
        animation.interpolate(0.5)
        np.testing.assert_allclose(self.source.data["point"], [[1, 2.25, 0]])

    def test_path_exception_propagates_unchanged(self):
        error = RuntimeError("authored path failed")
        def broken(start, end, alpha):
            if alpha > 0:
                raise error
            return start
        animation = self.n.Transform(self.source, self.target, path_func=broken)
        animation.begin()
        with self.assertRaises(RuntimeError) as caught:
            animation.interpolate(0.5)
        self.assertIs(caught.exception, error)

    def test_installer_is_idempotent_and_module_local(self):
        constructor = self.n.Transform.__init__
        dispatcher = self.n._requires_python_animation
        protocol.install(self.n)
        self.assertIs(self.n.Transform.__init__, constructor)
        self.assertIs(self.n._requires_python_animation, dispatcher)
        second = fixture()
        self.assertIsNot(second.Transform.__init__, constructor)
        self.assertTrue(self.n._requires_python_animation(
            self.n.Transform(self.source, self.target, path_func=excursion)))


if __name__ == "__main__":
    unittest.main()
