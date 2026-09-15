"""Installed-wheel acceptance for authored Rotating/Rotate playback."""
import numpy as np
from manimlib import Dot, Group, RIGHT, UP, ORIGIN, Rotate, Rotating, Scene, linear


def stock_rotation():
    scene = Scene()
    dot = Dot(RIGHT)
    scene.add(dot)
    scene.play(Rotate(dot, angle=np.pi / 2, about_point=ORIGIN, rate_func=linear, run_time=1 / 30))
    np.testing.assert_allclose(dot.get_center()[:2], UP[:2], atol=1e-5)


def authored_rate_and_final_alpha():
    scene = Scene()
    dot = Dot(RIGHT)
    seen = []
    rate = lambda t: seen.append(float(t)) or t * t
    animation = Rotate(
        dot, angle=np.pi, about_point=ORIGIN, rate_func=rate,
        final_alpha_value=.5, run_time=2 / 30,
    )
    scene.add(dot)
    scene.play(animation)
    # final alpha .5 passes through the authored t^2 curve -> pi/4.
    root = np.sqrt(.5)
    np.testing.assert_allclose(dot.get_center()[:2], [root, root], atol=2e-4)
    assert seen and any(0 < value < 1 for value in seen)


def subclass_hook_dispatch():
    events = []
    class CustomRotate(Rotate):
        def interpolate_mobject(self, alpha):
            events.append(float(alpha))
            return super().interpolate_mobject(alpha)
    scene = Scene()
    dot = Dot(RIGHT)
    scene.add(dot)
    scene.play(CustomRotate(dot, angle=np.pi / 2, about_point=ORIGIN, rate_func=linear, run_time=1 / 30))
    assert events
    np.testing.assert_allclose(dot.get_center()[:2], UP[:2], atol=1e-5)


def mobject_hook_dispatch():
    calls = []
    class CustomDot(Dot):
        def rotate(self, angle, *args, **kwargs):
            calls.append(float(angle))
            return super().rotate(angle, *args, **kwargs)
    scene = Scene()
    dot = CustomDot(RIGHT)
    scene.add(dot)
    scene.play(Rotate(dot, angle=np.pi / 2, about_point=ORIGIN, rate_func=linear, run_time=1 / 30))
    assert calls
    np.testing.assert_allclose(dot.get_center()[:2], UP[:2], atol=1e-5)


def nested_rotation_callback():
    events = []
    class CustomRotating(Rotating):
        def interpolate_mobject(self, alpha):
            events.append(float(alpha))
            return super().interpolate_mobject(alpha)
    scene = Scene()
    a, b = Dot(RIGHT), Dot(2 * RIGHT)
    scene.add(a, b)
    scene.play(
        Group(
            a,
            b,
        ).animate.shift(0 * RIGHT),
        CustomRotating(b, angle=np.pi / 2, about_point=ORIGIN, rate_func=linear, run_time=1 / 30),
        run_time=1 / 30,
        rate_func=linear,
    )
    assert events


for case in (
    stock_rotation,
    authored_rate_and_final_alpha,
    subclass_hook_dispatch,
    mobject_hook_dispatch,
    nested_rotation_callback,
):
    case()
print("rotation playback acceptance: 5 cases passed")
