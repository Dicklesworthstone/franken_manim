"""Real-extension camera-pose and choreography acceptance; no fixture storage."""
import math
import numpy as np
from manimlib import CameraFrame, Transform, Scene, Square, linear


def state(frame):
    return (frame.get_center().copy(), frame.get_shape(), frame.get_field_of_view(),
            frame.get_orientation().copy())


def assert_state(frame, expected):
    for actual, wanted in zip(state(frame), expected):
        np.testing.assert_allclose(actual, wanted, atol=1e-10, rtol=1e-10)


def pose_interpolates_every_native_component():
    live = CameraFrame()
    start = live.copy()
    target = live.copy().shift((4., 2., -2.)).scale(.5).set_field_of_view(math.pi / 2.)
    target.set_orientation((0., 0., 1., 0.))
    core = live._core
    assert live.interpolate(start, target, .5) is live
    assert live._core is core
    np.testing.assert_allclose(live.get_center(), [2., 1., -1.])
    np.testing.assert_allclose(live.get_shape(), .75 * np.array(start.get_shape()))
    np.testing.assert_allclose(live.get_orientation(), [0., 0., math.sqrt(.5), math.sqrt(.5)])
    assert math.isclose(live.get_field_of_view(), 3. * math.pi / 8.)
    live.interpolate(start, target, 1.)
    assert_state(live, state(target))


def direct_transform_uses_pose_not_empty_records():
    live = CameraFrame()
    target = live.copy().shift((4., 0., 0.)).scale(.5)
    animation = Transform(live, target, rate_func=linear)
    animation.begin()
    animation.interpolate(.5)
    np.testing.assert_allclose(live.get_center(), [2., 0., 0.])
    np.testing.assert_allclose(live.get_shape(), np.array(target.get_shape()) * 1.5)
    animation.finish()
    assert_state(live, state(target))
    assert not live.is_changing()
    assert live.locked_data_keys == set()


def native_camera_locks_and_failure_are_atomic():
    live = CameraFrame()
    start, target = live.copy(), live.copy().shift((4., 0., 0.))
    before = state(live)
    for callback in (lambda a, b, t: np.zeros((5, 3)),
                     lambda a, b, t: np.full((5, 3), math.nan)):
        try:
            live.interpolate(start, target, .5, callback)
        except ValueError:
            pass
        else:
            raise AssertionError("invalid camera path was accepted")
        assert_state(live, before)
    live.locked_data_keys.add("point")
    live.interpolate(start, target, 1.)
    assert_state(live, before)
    live.locked_data_keys.clear()


def closed_camera_excursion_uses_reference_control_points():
    live = CameraFrame()
    start = live.copy()
    seen = []
    def path(a, b, t):
        seen.append((a.shape, b.shape, t))
        return (1. - t) * a + t * b + np.array([0., 4. * t * (1. - t), 0.])
    live.interpolate(start, start, .5, path)
    np.testing.assert_allclose(live.get_center(), [0., 1., 0.])
    live.interpolate(start, start, 1., path)
    assert_state(live, state(start))
    assert seen == [((5, 3), (5, 3), .5), ((5, 3), (5, 3), 1.)]


_CASES = [pose_interpolates_every_native_component, direct_transform_uses_pose_not_empty_records,
          native_camera_locks_and_failure_are_atomic, closed_camera_excursion_uses_reference_control_points]
for _case in _CASES:
    _case()
print(f"camera animation acceptance: {len(_CASES)} cases passed")
