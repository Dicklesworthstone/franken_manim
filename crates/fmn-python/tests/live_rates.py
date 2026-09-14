"""Installed-extension easing acceptance over the real Scene clock and renderer."""
import math
from pathlib import Path
import tempfile

import numpy as np
import manimlib as m
from fmn_python import render_session


def test_sixty_hz_curve_drives_actual_tracker_and_output():
    scene, tracker, dot = m.Scene(), m.ValueTracker(0), m.Dot()
    alphas, values = [], []
    def curve(t):
        alphas.append(float(t))
        return t + .02 * math.sin(30 * math.pi * t)
    def update(obj):
        value = float(tracker.get_value())
        values.append(value)
        obj.move_to(value * m.RIGHT)
    dot.add_updater(update)
    directory = Path(tempfile.mkdtemp(prefix="fmn-live-rates-"))
    with render_session(scene, directory / "motion.y4m", resolution=(32, 18), fps=60, threads=1) as output:
        scene.add(tracker, dot)
        scene.play(tracker.animate(run_time=1, rate_func=curve).set_value(1))
    assert any(np.isclose(t, 1/60, atol=1e-13) for t in alphas), alphas
    expected = 1/60 + .02
    assert any(np.isclose(value, expected, atol=1e-12) for value in values), values
    assert np.isclose(tracker.get_value(), 1)
    assert output.result.frame_count == 60
    assert (directory / "motion.y4m").read_bytes().startswith(b"YUV4MPEG2")
    print(f"retained live-rate output: {directory}")


def test_global_rate_is_not_probed_before_begin():
    began, phases = [False], []
    class Transform(m.Transform):
        def begin(self):
            began[0] = True
            super().begin()
    def curve(t):
        phases.append(began[0])
        return t*t
    square, scene = m.Square(), m.Scene()
    scene.add(square)
    scene.play(Transform(square, square.copy().shift(m.RIGHT)), run_time=.2, rate_func=curve)
    assert phases and all(phases), phases
    assert np.isclose(square.get_center()[0], 1)


def test_nested_custom_group_uses_existing_composition_driver():
    left, right, scene = m.Square(), m.Square().shift(3*m.RIGHT), m.Scene()
    seen = []
    def curve(t):
        seen.append(t)
        return t*t
    inner = m.AnimationGroup(m.Transform(left, left.copy().shift(m.UP)), rate_func=curve)
    outer = m.LaggedStart(inner, m.Transform(right, right.copy().shift(m.DOWN)))
    scene.add(left, right)
    scene.play(outer, run_time=.2)
    assert seen, "nested group curve was not executed"
    assert np.isclose(left.get_center()[1], 1)
    assert np.isclose(right.get_center()[1], -1)
    assert getattr(inner, "_composition_scene", None) is None


def test_catalog_spelling_on_authored_transform_path():
    square, scene = m.Square(), m.Scene()
    scene.add(square)
    scene.play(m.Transform(square, square.copy().shift(m.RIGHT), rate_func="linear",
                           path_func=lambda a,b,t:(1-t)*a+t*b), run_time=.1)
    assert np.isclose(square.get_center()[0], 1)


def test_unhashable_rate_object_uses_live_callback():
    class Curve:
        __hash__ = None
        def __eq__(self, other):
            raise AssertionError("rate equality must not be inspected")
        def __call__(self, t):
            return t*t
    square, scene = m.Square(), m.Scene()
    scene.add(square)
    scene.play(m.Transform(square, square.copy().shift(m.RIGHT), rate_func=Curve()), run_time=.1)
    assert np.isclose(square.get_center()[0], 1)


def test_authored_rate_failure_releases_native_animation_state():
    square, scene = m.Square(), m.Scene()
    scene.add(square)
    failure = ValueError("authored curve failed")
    def curve(t):
        if t > 0:
            raise failure
        return t
    try:
        scene.play(m.Transform(square, square.copy().shift(m.RIGHT), rate_func=curve,
                               suspend_mobject_updating=True), run_time=.1)
    except ValueError as caught:
        assert caught is failure
    else:
        raise AssertionError("the authored rate failure was lost")
    assert not square._is_updating_suspended()
    target = square.copy().shift(m.UP)
    scene.play(m.Transform(square, target), run_time=.1)
    assert np.allclose(square.get_center(), target.get_center())


for name, function in tuple(globals().items()):
    if name.startswith("test_") and callable(function):
        function()
print("live easing: 6 installed-extension cases passed")
