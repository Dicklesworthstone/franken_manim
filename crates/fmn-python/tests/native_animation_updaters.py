"""Installed-extension witnesses for scene-bound persistent native animations."""
import numpy as np
from manimlib import Scene, Square, Swap, LEFT, RIGHT, linear, turn_animation_into_updater, cycle_animation


def setup_scene():
    scene = Scene()
    left, right = Square().shift(2 * LEFT), Square().shift(2 * RIGHT)
    scene.add(left, right)
    return scene, left, right


def native_swap_midpoint_and_finish():
    scene, left, right = setup_scene()
    roots = list(scene.mobjects)
    animation = Swap(left, right, path_arc=0, rate_func=linear, run_time=1)
    assert turn_animation_into_updater(animation) is left
    left.update(.5)
    left.update(0)
    np.testing.assert_allclose(left.get_center(), (0, 0, 0), atol=1e-5)
    np.testing.assert_allclose(right.get_center(), (0, 0, 0), atol=1e-5)
    left.update(.5)
    left.update(0)
    np.testing.assert_allclose(left.get_center(), 2 * RIGHT, atol=1e-5)
    np.testing.assert_allclose(right.get_center(), 2 * LEFT, atol=1e-5)
    assert not left.updaters
    assert all(any(root is actual for actual in scene.mobjects) for root in roots)


def native_swap_cycles():
    _, left, right = setup_scene()
    cycle_animation(Swap(left, right, path_arc=0, rate_func=linear, run_time=1))
    left.update(2.25)
    left.update(0)
    np.testing.assert_allclose(left.get_center(), LEFT, atol=1e-5)
    np.testing.assert_allclose(right.get_center(), RIGHT, atol=1e-5)
    assert left.updaters
    left.clear_updaters()


def explicit_removal_keeps_current_pose():
    _, left, right = setup_scene()
    cycle_animation(Swap(left, right, path_arc=0, rate_func=linear))
    left.update(.25)
    left.update(0)
    expected = left.get_points().copy(), right.get_points().copy()
    left.clear_updaters()
    left.update(2)
    np.testing.assert_array_equal(left.get_points(), expected[0])
    np.testing.assert_array_equal(right.get_points(), expected[1])
    assert not left.updaters


def copied_updaters_do_not_control_the_source():
    _, left, right = setup_scene()
    animation = Swap(left, right, path_arc=0, rate_func=linear)
    cycle_animation(animation)
    duplicate = left.copy()
    duplicate.update(5)
    duplicate.clear_updaters()
    assert animation.total_time == 0
    assert left.updaters
    left.clear_updaters()


def wait_drives_native_background_effect():
    scene, left, right = setup_scene()
    turn_animation_into_updater(Swap(left, right, path_arc=0, rate_func=linear, run_time=.1))
    scene.wait(.3)
    np.testing.assert_allclose(left.get_center(), 2 * RIGHT, atol=1e-5)
    np.testing.assert_allclose(right.get_center(), 2 * LEFT, atol=1e-5)
    assert not left.updaters


def invalid_dt_recovers():
    _, left, right = setup_scene()
    turn_animation_into_updater(Swap(left, right, path_arc=0, rate_func=linear))
    try:
        left.update(float("nan"))
    except ValueError:
        pass
    else:
        raise AssertionError("nonfinite updater dt must fail")
    assert not left.updaters
    assert not left._is_updating_suspended()
    turn_animation_into_updater(Swap(left, right, path_arc=0, rate_func=linear))
    left.update(1)
    left.update(0)
    np.testing.assert_allclose(left.get_center(), 2 * RIGHT, atol=1e-5)


for case in (native_swap_midpoint_and_finish, native_swap_cycles,
             explicit_removal_keeps_current_pose, copied_updaters_do_not_control_the_source,
             wait_drives_native_background_effect, invalid_dt_recovers):
    case()
print("native persistent animation acceptance: 6 cases passed")
