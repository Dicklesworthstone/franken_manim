"""Source-level dispatch regressions; no native extension or renderer doubles.

These tests exercise the actual installer against minimal protocol objects.
The installed-extension geometry/clock witnesses live in live_fade_rates.py.
Run: python -m unittest discover -s crates/fmn-python/tests -p test_live_rate_routing.py -v
"""
from __future__ import annotations

import importlib.util
from pathlib import Path
from types import SimpleNamespace
import unittest


_SOURCE = Path(__file__).resolve().parents[1] / "python" / "fmn_python" / "live_rates.py"
_spec = importlib.util.spec_from_file_location("fmn_test_live_rates", _SOURCE)
_adapter = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_adapter)


def protocol():
    def linear(t):
        return t

    class Animation:
        _native_kind = None

        def __init__(self, rate_func=None):
            self.rate_func = linear if rate_func is None else rate_func
            self.run_time = None
            self.time_span = None
            self.lag_ratio = 0.0

    class Transform(Animation):
        _native_kind = "transform"
        _target_attr = "target_mobject"

        def _ensure_runtime_defaults(self):
            pass

    class Fade(Transform):
        _native_kind = "fade"
        _target_attr = None

    class FadeIn(Fade):
        _native_kind = "fade_in"

    class FadeOut(Fade):
        _native_kind = "fade_out"

    class FadeInFromLarge(FadeIn):
        pass

    class FadeInFromPoint(FadeIn):
        pass

    class FadeOutToPoint(FadeOut):
        pass

    class NativeOnly(Animation):
        _native_kind = "native_only"

    class AnimationGroup(Animation):
        _native_kind = "animation_group"

        def __init__(self, *animations, rate_func=None):
            super().__init__(rate_func)
            self.animations = list(animations)
            self.max_end_time = 1.0
            self.aborts = 0

        def abort(self):
            self.aborts += 1

    class Builder:
        def __init__(self, result):
            self.result = result
            self.builds = 0

        def build(self):
            self.builds += 1
            return self.result

    class Scene:
        def play(self, *animations, **kwargs):
            self.received = animations, kwargs
            self.callbacks = tuple(native._requires_python_animation(a) for a in animations)
            if getattr(self, "failure", None) is not None:
                raise self.failure
            return "played"

    native = SimpleNamespace(
        Animation=Animation, Transform=Transform, Fade=Fade, FadeIn=FadeIn, FadeOut=FadeOut,
        FadeInFromLarge=FadeInFromLarge, FadeInFromPoint=FadeInFromPoint,
        FadeOutToPoint=FadeOutToPoint, NativeOnly=NativeOnly, AnimationGroup=AnimationGroup,
        _AnimationBuilder=Builder, Scene=Scene, _RATE_FUNC_NAMES={linear: "linear"}, linear=linear,
        _requires_python_animation=lambda animation: getattr(animation, "authored", False),
        prepare_animation=lambda builder: builder.build(),
        _fmn_ensure_composition_root=lambda group: None,
    )
    _adapter.install_live_rates(native)
    return native


class Curve:
    __hash__ = None

    def __bool__(self):
        raise AssertionError("rate truth value was inspected")

    def __eq__(self, other):
        raise AssertionError("rate equality was inspected")

    def __call__(self, t):
        raise AssertionError("dispatch must not speculatively evaluate easing")


class FadeRateRouting(unittest.TestCase):
    def setUp(self):
        self.native = protocol()

    def families(self):
        return tuple(getattr(self.native, name) for name in (
            "FadeIn", "FadeOut", "FadeInFromLarge", "FadeInFromPoint", "FadeOutToPoint"))

    def test_leaf_custom_rates_select_callbacks_without_sampling(self):
        for cls in self.families():
            with self.subTest(kind=cls.__name__):
                self.assertTrue(self.native._requires_python_animation(cls(Curve())))

    def test_catalog_functions_and_spellings_remain_native(self):
        for cls in self.families():
            for rate in (self.native.linear, "linear", None):
                with self.subTest(kind=cls.__name__, rate=rate):
                    self.assertFalse(self.native._requires_python_animation(cls(rate)))

    def test_mixed_fade_transform_uses_live_play_override(self):
        for cls in self.families():
            with self.subTest(kind=cls.__name__):
                scene, rate = self.native.Scene(), Curve()
                fade, transform = cls(), self.native.Transform()
                self.assertEqual(scene.play(fade, transform, rate_func=rate), "played")
                self.assertEqual(scene.callbacks, (True, True))
                self.assertIsNone(scene.received[1]["rate_func"])
                self.assertIs(fade.rate_func, rate)
                self.assertIs(transform.rate_func, rate)

    def test_unsupported_native_sibling_preserves_global_lowering(self):
        fade, sibling = self.native.FadeIn(), self.native.NativeOnly()
        scene, rate = self.native.Scene(), Curve()
        scene.play(fade, sibling, rate_func=rate)
        self.assertIs(scene.received[1]["rate_func"], rate)
        self.assertIs(fade.rate_func, self.native.linear)
        self.assertEqual(scene.callbacks, (False, False))

    def test_bare_fade_is_not_promoted_without_a_target(self):
        self.assertFalse(self.native._requires_python_animation(self.native.Fade(Curve())))

    def test_builder_returned_fade_is_built_once(self):
        fade, scene, rate = self.native.FadeOut(), self.native.Scene(), Curve()
        builder = self.native._AnimationBuilder(fade)
        scene.play(builder, rate_func=rate)
        self.assertEqual(builder.builds, 1)
        self.assertEqual(scene.callbacks, (True,))
        self.assertIs(scene.received[0][0], fade)

    def test_group_override_does_not_replace_child_easing(self):
        child_rate, group_rate = Curve(), Curve()
        child = self.native.FadeIn(child_rate)
        group = self.native.AnimationGroup(child)
        scene = self.native.Scene()
        scene.play(group, rate_func=group_rate)
        self.assertIs(group.rate_func, group_rate)
        self.assertIs(child.rate_func, child_rate)
        self.assertNotIn("_composition_scene", vars(group))

    def test_callback_defaults_resolve_catalog_strings_for_fades(self):
        for cls in self.families():
            fade = cls("linear")
            fade._ensure_runtime_defaults()
            self.assertIs(fade.rate_func, self.native.linear)

    def test_previous_authored_classifier_is_preserved(self):
        animation = self.native.NativeOnly()
        animation.authored = True
        self.assertTrue(self.native._requires_python_animation(animation))

    def test_installation_is_idempotent(self):
        play = self.native.Scene.play
        _adapter.install_live_rates(self.native)
        self.assertIs(self.native.Scene.play, play)


if __name__ == "__main__":
    unittest.main()
