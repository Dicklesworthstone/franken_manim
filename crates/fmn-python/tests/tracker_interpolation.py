"""Installed-extension tracker animation acceptance; no engine substitutes."""
import numpy as np
import manimlib as m


def path(start, end, alpha):
    return (1 - alpha) * start + alpha * end


def test_scalar_callback_drives_dependent_geometry():
    scene, tracker, dot = m.Scene(), m.ValueTracker(0), m.Dot()
    observed = []
    def update(obj):
        value = float(tracker.get_value())
        observed.append(value)
        obj.move_to(value * m.RIGHT)
    dot.add_updater(update)
    scene.add(tracker, dot)
    scene.play(m.Transform(tracker, m.ValueTracker(3), path_func=path),
               run_time=.2, rate_func=m.linear)
    assert any(0 < value < 3 for value in observed), observed
    assert np.isclose(tracker.get_value(), 3)
    assert np.isclose(dot.get_center()[0], 3)


def test_complex_callback_updates_real_and_imaginary_native_lanes():
    scene, tracker = m.Scene(), m.ComplexValueTracker(1 + 2j)
    scene.add(tracker)
    scene.play(m.Transform(tracker, m.ComplexValueTracker(5 - 6j), path_func=path),
               run_time=.2, rate_func=m.linear)
    assert np.isclose(tracker.get_value(), 5 - 6j)
    assert np.isclose(complex(*tracker._tracker_complex_value()), 5 - 6j)


def test_exponential_callback_uses_geometric_interpolation():
    tracker = m.ExponentialValueTracker(1)
    animation = m.Transform(tracker, m.ExponentialValueTracker(81), path_func=path,
                            rate_func=m.linear)
    animation.begin()
    animation.interpolate(.5)
    assert np.isclose(tracker.get_value(), 9, rtol=2e-13), tracker.get_value()
    animation.finish()
    assert np.isclose(tracker.get_value(), 81, rtol=2e-13)


def test_native_then_callback_uses_actual_native_endpoint():
    scene, tracker = m.Scene(), m.ValueTracker(0)
    scene.add(tracker)
    scene.play(tracker.animate.set_value(4), run_time=.1, rate_func=m.linear)
    values = []
    class Observe(m.Transform):
        def interpolate_submobject(self, current, start, target, alpha):
            result = super().interpolate_submobject(current, start, target, alpha)
            values.append(float(current.get_value()))
            return result
    scene.play(Observe(tracker, m.ValueTracker(8)), run_time=.1, rate_func=m.linear)
    assert np.isclose(values[0], 4), values
    assert any(4 < value < 8 for value in values), values
    assert np.isclose(tracker.get_value(), 8)


def test_builder_endpoint_fraction_updates_native_tracker():
    scene, tracker = m.Scene(), m.ValueTracker(2)
    scene.add(tracker)
    scene.play(tracker.animate(final_alpha_value=.25).set_value(10),
               run_time=.1, rate_func=m.linear)
    assert np.isclose(tracker.get_value(), 4)


def test_group_callback_animates_nested_tracker_families():
    first, second = m.ValueTracker(0), m.ComplexValueTracker(0)
    source = m.Group(first, m.Group(second))
    target = m.Group(m.ValueTracker(6), m.Group(m.ComplexValueTracker(2 + 4j)))
    scene = m.Scene()
    scene.add(source)
    scene.play(m.Transform(source, target, path_func=path), run_time=.1, rate_func=m.linear)
    assert np.isclose(first.get_value(), 6)
    assert np.isclose(second.get_value(), 2 + 4j)


def test_value_uniform_lock_blocks_callback_tracker_write():
    tracker, start, end = m.ValueTracker(7), m.ValueTracker(0), m.ValueTracker(10)
    tracker.locked_uniform_keys.add("value")
    tracker.interpolate(start, end, .5)
    assert tracker.get_value() == 7
    tracker.locked_uniform_keys.clear()
    tracker.interpolate(start, end, .5)
    assert tracker.get_value() == 5


def test_callback_tracker_retains_float64_precision():
    tracker = m.ValueTracker(0)
    start, end = 1 + 2**-40, 1 + 2**-38
    tracker.interpolate(m.ValueTracker(start), m.ValueTracker(end), .5)
    expected = (start + end) / 2
    assert float(tracker.get_value()) == expected
    assert float(tracker.get_value()) != float(np.float32(expected))


for name, function in tuple(globals().items()):
    if name.startswith("test_") and callable(function):
        function()
print("tracker interpolation: 8 installed-extension cases passed")
