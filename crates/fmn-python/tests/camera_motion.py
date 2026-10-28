"""Spatial CameraFrame animations on the real native clock and renderer.

Run against an installed portal with ``python -I camera_motion.py``. These
checks use native camera/geometry values and scene playback, never mocks.
"""
import math
from pathlib import Path
import tempfile

import numpy as np
import manimlib as m


def close(actual, expected):
    np.testing.assert_allclose(actual, expected, atol=2e-9, rtol=2e-9)


def camera_samples(scene):
    samples = []

    def observe(frame, dt):
        if frame is scene.frame and dt > 0:
            samples.append((frame.get_center().copy(), frame.get_orientation().copy()))

    scene.frame.add_updater(observe, call=False)
    return samples


def rotation_is_absolute_at_every_native_sample():
    for cls in (m.Rotate, m.Rotating):
        for frames in (2, 4, 7):
            for angle in (-1.3, math.tau * 2.5):
                scene = m.Scene()
                frame = scene.frame
                frame.shift((2, -1, 0.5)).scale(0.75).rotate(0.6, axis=m.RIGHT)
                frame.set_field_of_view(0.9)
                start, core = frame.copy(), frame._core
                samples = camera_samples(scene)
                scene.play(cls(frame, angle=angle, axis=(0, 2, 1),
                               run_time=frames / 30, rate_func=m.linear))
                for index, (center, quaternion) in enumerate(samples, 1):
                    # BN-02 preserves the nominal overshoot sample when the
                    # supplied binary-float duration lies above a frame tick.
                    alpha = index / frames
                    expected = start.copy().rotate(angle * alpha, axis=(0, 2, 1))
                    close(quaternion, expected.get_orientation())
                    close(center, start.get_center())
                close(frame.get_orientation(), start.copy().rotate(angle, axis=(0, 2, 1)).get_orientation())
                close(frame.get_shape(), start.get_shape())
                assert frame.get_field_of_view() == 0.9
                assert frame._core is core and scene.mobjects == [frame] and frame._is_bound()


def rotation_obeys_live_rates_windows_and_final_alpha():
    class Rate:
        __hash__ = None

        def __call__(self, alpha):
            return alpha * alpha

    scene = m.Scene()
    start = scene.frame.copy()
    samples = camera_samples(scene)
    scene.play(m.Rotate(scene.frame, angle=1.2, run_time=4 / 30,
                        rate_func=Rate(), final_alpha_value=0.5))
    for index, (_, quaternion) in enumerate(samples, 1):
        close(quaternion, start.copy().rotate(1.2 * (index / 4) ** 2).get_orientation())
    close(scene.frame.get_orientation(), start.copy().rotate(0.3).get_orientation())
    scene = m.Scene()
    start = scene.frame.copy()
    samples = camera_samples(scene)
    scene.play(m.Rotate(scene.frame, angle=1.2, run_time=4 / 30,
                        time_span=(2 / 30, 4 / 30), rate_func="linear"))
    for (_, quaternion), alpha in zip(samples, (0, 0, .5, 1)):
        close(quaternion, start.copy().rotate(1.2 * alpha).get_orientation())


def succession_captures_predecessor_camera_orientation():
    scene = m.Scene()
    frame = scene.frame
    first = m.Rotate(frame, angle=0.7, axis=m.RIGHT, run_time=2 / 30, rate_func=m.linear)
    second = m.Rotate(frame, angle=-1.3, axis=m.UP, run_time=2 / 30, rate_func=m.linear)
    initial = frame.copy()
    scene.play(m.AnimationGroup(m.Succession(first, second)))
    expected = initial.copy().rotate(0.7, axis=m.RIGHT)
    close(second.starting_mobject.get_orientation(), expected.get_orientation())
    close(frame.get_orientation(), expected.rotate(-1.3, axis=m.UP).get_orientation())
    assert frame._is_bound() and scene.mobjects == [frame]


def path_uses_true_length_and_preserves_optical_pose():
    scene = m.Scene()
    path = m.VMobject().set_points_as_corners([(0, 0, 0), (1, 0, 0), (1, 3, 0)])
    scene.frame.rotate(0.7, axis=m.RIGHT).scale(0.5).set_field_of_view(0.8)
    orientation, shape = scene.frame.get_orientation(), scene.frame.get_shape()
    samples = camera_samples(scene)
    scene.play(m.MoveAlongPath(scene.frame, path, run_time=4 / 30,
                               rate_func=m.linear, suspend_mobject_updating=False))
    close([sample[0] for sample in samples], [(1, 0, 0), (1, 1, 0), (1, 2, 0), (1, 3, 0)])
    for _, quaternion in samples:
        close(quaternion, orientation)
    close(scene.frame.get_shape(), shape)
    assert scene.frame.get_field_of_view() == 0.8
    assert not path._is_bound() and scene.mobjects == [scene.frame]


def simultaneous_path_and_rotation_keep_native_play_order():
    scene = m.Scene()
    start = scene.frame.copy()
    path = m.Line(m.ORIGIN, 4 * m.RIGHT)
    square = m.Square()
    scene.add(square)
    samples = camera_samples(scene)
    scene.play(m.Rotate(scene.frame, angle=1.2, rate_func=m.linear),
               m.MoveAlongPath(scene.frame, path, rate_func=m.linear,
                               suspend_mobject_updating=False),
               square.animate(rate_func=m.linear).shift(2 * m.UP), run_time=4 / 30)
    for index, (center, quaternion) in enumerate(samples, 1):
        close(center, (index, 0, 0))
        close(quaternion, start.copy().rotate(1.2 * index / 4).get_orientation())
    close(square.get_center(), 2 * m.UP)
    assert scene.mobjects == [scene.frame, square] and scene.frame._is_bound()


def tracking_uses_construction_offset_and_current_sibling_state():
    scene = m.Scene()
    dot = m.Dot().shift(m.RIGHT)
    scene.add(dot)
    scene.frame.shift(3 * m.RIGHT)
    follower = m.MaintainPositionRelativeTo(scene.frame, dot,
                                           suspend_mobject_updating=False)
    dot.shift(m.UP)
    scene.frame.shift(7 * m.LEFT)
    samples = camera_samples(scene)
    scene.play(dot.animate(rate_func=m.linear).shift(4 * m.RIGHT), follower, run_time=4 / 30)
    close([sample[0] for sample in samples], [(4, 1, 0), (5, 1, 0), (6, 1, 0), (7, 1, 0)])
    assert scene.mobjects == [scene.frame, dot] and scene.frame._is_bound()


def tracking_samples_authored_centers_exactly_once_at_construction():
    scene = m.Scene()
    dot = m.Dot()
    calls = []
    frame_center, dot_center = scene.frame.get_center, dot.get_center

    def camera_center():
        calls.append("camera")
        return frame_center()

    def target_center():
        calls.append("target")
        return dot_center()

    scene.frame.get_center, dot.get_center = camera_center, target_center
    animation = m.MaintainPositionRelativeTo(scene.frame, dot)
    assert calls == ["camera", "target"], calls
    close(animation.diff, m.ORIGIN)


def foreign_helpers_and_destructive_camera_operations_are_refused():
    scene, other = m.Scene(), m.Scene()
    path, dot = m.Line(m.ORIGIN, m.RIGHT), m.Dot()
    other.add(path, dot)
    for animation in (m.MoveAlongPath(scene.frame, path),
                      m.MaintainPositionRelativeTo(scene.frame, dot),
                      m.Rotate(scene.frame.copy())):
        before = scene.frame.copy()
        try:
            scene.play(m.AnimationGroup(animation), run_time=2 / 30)
        except (ValueError, NotImplementedError, m._ForeignStageError):
            pass
        else:
            raise AssertionError("invalid camera motion was accepted")
        close(scene.frame.get_center(), before.get_center())
        close(scene.frame.get_orientation(), before.get_orientation())
        assert scene.time == 0 and scene.mobjects == [scene.frame]
        assert not scene.frame._is_updating_suspended() and scene.frame._is_bound()
    assert other.mobjects == [other.frame, path, dot]


def authored_rotation_dispatch_preserves_live_receiver():
    scene = m.Scene()
    frame = scene.frame
    calls, rotate = [], frame.rotate

    def authored_rotate(angle, axis=m.OUT, **kwargs):
        calls.append((angle, axis))
        return rotate(angle, axis, **kwargs)

    frame.rotate = authored_rotate
    scene.play(m.Rotate(frame, angle=0.8, run_time=2 / 30, rate_func=m.linear))
    close([entry[0] for entry in calls], [0., 0.4, 0.8, 0.8])
    close(frame.get_orientation(), m.CameraFrame().rotate(0.8).get_orientation())


def authored_rates_observe_restored_pose_and_may_edit_it():
    scene = m.Scene()
    frame = scene.frame.shift((1, 2, 0)).rotate(0.3)
    start, seen = frame.copy(), []

    def rate(alpha):
        seen.append((frame.get_center().copy(), frame.get_orientation().copy()))
        # The rate's authored edits survive until the rotation hook, exactly
        # as they do after a drawable's match_points restoration.
        frame.shift(alpha * m.UP)
        return alpha

    scene.play(m.Rotate(frame, angle=0.8, run_time=4 / 30, rate_func=rate))
    assert len(seen) == 6
    for center, orientation in seen:
        close(center, start.get_center())
        close(orientation, start.get_orientation())
    close(frame.get_center(), start.get_center() + m.UP)
    close(frame.get_orientation(), start.copy().rotate(0.8).get_orientation())


def authored_rotation_owns_axis_interpretation():
    scene = m.Scene()
    frame, calls = scene.frame, []
    native_rotate = frame.rotate

    def authored_rotate(angle, axis=m.OUT, **kwargs):
        calls.append(tuple(axis))
        return native_rotate(angle, axis=m.OUT if not any(axis) else axis, **kwargs)

    frame.rotate = authored_rotate
    scene.play(m.Rotate(frame, angle=0.8, axis=m.ORIGIN,
                        run_time=2 / 30, rate_func=m.linear))
    assert calls == [(0., 0., 0.)] * 4
    close(frame.get_orientation(), m.CameraFrame().rotate(0.8).get_orientation())
    # Without an override, native validation still refuses a zero axis.
    ordinary = m.Scene()
    try:
        ordinary.play(m.Rotate(ordinary.frame, axis=m.ORIGIN), run_time=1 / 30)
    except ValueError:
        pass
    else:
        raise AssertionError("stock native rotation accepted a zero axis")


def final_camera_motion_changes_real_native_pixels():
    class Image(m.Scene):
        reference = False

        def construct(self):
            self.add(m.Rectangle(width=1, height=2, fill_color=m.RED,
                                 fill_opacity=1, stroke_width=0).shift(m.RIGHT))
            if self.reference:
                self.frame.rotate(0.8, axis=m.OUT).move_to((0.3, 0.5, 0))
            else:
                self.play(m.Rotate(self.frame, angle=0.8, rate_func=m.linear),
                          m.MoveAlongPath(self.frame, m.Line(m.ORIGIN, (0.3, 0.5, 0)),
                                          rate_func=m.linear), run_time=2 / 30)

    images = []
    with tempfile.TemporaryDirectory(prefix="fmn-camera-motion-") as directory:
        for name, reference, threads in (("motion-1", False, 1), ("motion-4", False, 4),
                                         ("reference", True, 1)):
            scene = Image()
            scene.reference = reference
            output = Path(directory) / (name + ".png")
            scene._begin_png(str(output), 96, 64, 30, threads, 0)
            try:
                scene.run()
                scene._finish_render(scene.frame._core, scene.camera.light_source.get_center())
            except BaseException:
                scene._abort_render()
                raise
            images.append(output.read_bytes())
    assert images[0] == images[1] == images[2], "camera motion disagrees with a directly authored native pose"
    assert images[0][:8] == b"\x89PNG\r\n\x1a\n"


def removers_drop_the_frame_from_the_scene_list_and_keep_the_camera():
    # Since the frame joined Scene.mobjects (fm-6l2i), a remover animation of
    # it drops it from the scene list, as in the Reference, where
    # FadeOut(Group(*self.mobjects)) fades it with everything else.
    # Scene.frame stays the camera (fm-5wwt). The lists and frame values
    # were measured in the pinned Reference.
    scene = m.Scene()
    scene.add(m.Square())
    width = scene.frame.get_width()
    scene.play(m.LaggedStartMap(m.FadeOut, m.Group(*scene.mobjects)), run_time=2 / 30)
    assert scene.mobjects == [], scene.mobjects
    close(scene.frame.get_center(), m.ORIGIN)
    assert math.isclose(scene.frame.get_width(), width)
    scene.add(m.Circle())
    assert [type(mob).__name__ for mob in scene.mobjects] == ["Circle"]
    scene = m.Scene()
    scene.play(m.FadeOut(scene.frame), run_time=2 / 30)
    assert scene.mobjects == [], scene.mobjects
    plain, removed = m.Scene(), m.Scene()
    plain.play(m.Rotate(plain.frame, m.PI / 2), run_time=2 / 30)
    removed.play(m.Rotate(removed.frame, m.PI / 2, remover=True), run_time=2 / 30)
    assert plain.mobjects == [plain.frame] and removed.mobjects == []
    close(removed.frame.get_orientation(), plain.frame.get_orientation())


def become_and_restore_take_the_source_frame_pose():
    # The Reference's become copies a frame's points and uniforms, so restore()
    # and become(target) move the camera at once; the pose lives in the core
    # here (fm-rgr0). Values measured in the pinned Reference.
    scene = m.Scene()
    frame = scene.frame
    width = frame.get_width()
    frame.save_state()
    frame.shift(m.RIGHT).set_width(20).reorient(30, 40)
    frame.restore()
    close(frame.get_center(), m.ORIGIN)
    assert math.isclose(frame.get_width(), width)
    close(frame.get_euler_angles(), [0, 0, 0])
    target = frame.copy()
    target.reorient(10, 20).shift(m.UP)
    frame.become(target)
    close(frame.get_center(), m.UP)
    close(frame.get_euler_angles(), [10 * m.DEG, 20 * m.DEG, 0])
    # A group over the scene list includes the frame (fm-6l2i), so restoring
    # it restores the camera too.
    scene = m.Scene()
    square = m.Square()
    scene.add(square)
    everything = m.Group(*scene.mobjects)
    everything.save_state()
    scene.frame.shift(m.RIGHT).set_width(20)
    square.shift(m.UP)
    everything.restore()
    close(scene.frame.get_center(), m.ORIGIN)
    assert math.isclose(scene.frame.get_width(), width)
    close(square.get_center(), m.ORIGIN)


def frame_rotation_ends_at_its_angle_whatever_the_frame_count():
    # C-34: the Reference's Rotate resets only point data between samples,
    # so a frame's orientation (a uniform) accumulates them. It ends at
    # pi, -3pi/4, pi/2 or pi/4 for these run times. Here every run time ends
    # at the animation's angle (fm-k8e3).
    for run_time in (1 / 30, 1 / 15, 0.5, 1.0):
        scene = m.Scene()
        scene.play(m.Rotate(scene.frame, m.PI / 2), run_time=run_time)
        close(scene.frame.get_euler_angles(), [m.PI / 2, 0, 0])


def affine_precision_survives_placement_copy_and_raw_control_edits():
    # Two equivalent f64 affine expressions straddle this f32 tie. Native
    # Placement multiplies the point before adding its retained offset;
    # mistaking its rounding for a callback edit loses authored precision.
    coordinate = float(np.float32(1.2345678))
    low = float(np.nextafter(np.float32(1.), np.float32(2.)))
    high = float(np.nextafter(np.float32(low), np.float32(2.)))
    factor = .9
    pivot = ((low + high)/2 - factor*coordinate) / (1-factor)
    expected = pivot + factor*(coordinate-pivot)
    native = factor*coordinate + (pivot-factor*pivot)
    assert np.float32(expected) != np.float32(native)
    boundary = m.CameraFrame(frame_shape=(8., 4.), center_point=(coordinate, 0., 0.))
    boundary.scale(factor, about_point=(pivot, 0., 0.))
    np.testing.assert_allclose(boundary.get_center(), (expected, 0., 0.), rtol=0, atol=1e-15)
    assert boundary.get_points()[0, 0] == np.float32(native)

    frame = m.CameraFrame(frame_shape=(7.3, 2.1), center_point=(.1, .2, .3))
    view = frame.get_points()
    pointer = view.__array_interface__['data'][0]
    target = np.array((1.1, -.7, .2))
    for _ in range(12):
        frame.move_to(target).set_shape(7.3, 2.1)
        frame.scale(.5).scale(2.)
        np.testing.assert_allclose(frame.get_center(), target, rtol=0, atol=1e-13)
        np.testing.assert_allclose(frame.get_shape(), (7.3, 2.1), rtol=0, atol=1e-13)
    assert pointer == frame.get_points().__array_interface__['data'][0]
    destination = m.CameraFrame().shift((4., 2., -2.)).scale(.5)
    frame.interpolate(frame.copy(), destination, 1.)
    for _ in range(12):
        frame.move_to(target).set_shape(7.3, 2.1)
        np.testing.assert_allclose(frame.get_center(), target, rtol=0, atol=1e-13)
        np.testing.assert_allclose(frame.get_shape(), (7.3, 2.1), rtol=0, atol=1e-13)
    for deep in (False, True):
        copied = frame.copy(deep=deep)
        copied.move_to((-.9, .4, .8))
        np.testing.assert_allclose(copied.get_center(), (-.9, .4, .8), rtol=0, atol=1e-13)
        np.testing.assert_allclose(frame.get_center(), target, rtol=0, atol=1e-13)

    # A cached placement must not hide an asymmetric raw control edit or an
    # auxiliary coordinate that extends the family box beyond the pose span.
    view[1, 0] -= 1.
    view[2, 1] += 4.
    view[1, 2] = 2.75
    before = frame.get_bounding_box()
    assert before[0, 0] == float(view[1, 0])
    assert before[2, 1] == float(view[2, 1])
    assert before[2, 2] == 2.75
    center = frame.get_center().copy()
    frame.move_to((3., 2., 1.))
    np.testing.assert_allclose(frame.get_bounding_box()[1], (3., 2., 1.), rtol=0, atol=1e-13)
    np.testing.assert_allclose(frame.get_center(), center + (3., 2., 1.) - before[1],
                               rtol=0, atol=1e-13)

    decorated = m.CameraFrame(frame_shape=(8., 4.))
    decorated.add(m.Dot((5., 0., 0.), radius=.5))
    decorated.move_to((3., 0., 0.))
    np.testing.assert_allclose(decorated.get_center(), (2.25, 0., 0.), rtol=0, atol=1e-13)
    decorated.set_width(19., stretch=True)
    np.testing.assert_allclose(decorated.get_center(), (1.5, 0., 0.), rtol=0, atol=1e-13)
    np.testing.assert_allclose(decorated.get_shape(), (16., 4.), rtol=0, atol=1e-13)


def scale_side_effects_keep_their_authored_camera_pose():
    class Authored(m.CameraFrame):
        def _handle_scale_side_effects(self, factor):
            self.get_points()[2, 0] += 1.
            self.shift((0., .1, 0.))
    frame = Authored(frame_shape=(8., 4.))
    frame.scale(.5, about_point=m.ORIGIN)
    np.testing.assert_array_equal(frame.get_center(), (0., .1, 0.))
    np.testing.assert_array_equal(frame.get_shape(), (5., 2.))
    np.testing.assert_array_equal(frame.get_points()[2], (3., np.float32(.1), 0.))
    frame.move_to((2., .3, .5))
    np.testing.assert_allclose(frame.get_bounding_box()[1], (2., .3, .5), rtol=0, atol=1e-13)

    class Raw(m.CameraFrame):
        def _handle_scale_side_effects(self, factor):
            self.get_points()[0, 1] += .25
            self.get_points()[1, 2] = 3.
    frame = Raw(frame_shape=(8., 4.)).scale(.5, about_point=m.ORIGIN)
    np.testing.assert_array_equal(frame.get_center(), (0., .25, 0.))
    np.testing.assert_array_equal(frame.get_shape(), (4., 2.))
    assert frame.get_bounding_box()[2, 2] == 3.


_CASES = (rotation_is_absolute_at_every_native_sample,
          rotation_obeys_live_rates_windows_and_final_alpha,
          succession_captures_predecessor_camera_orientation,
          path_uses_true_length_and_preserves_optical_pose,
          simultaneous_path_and_rotation_keep_native_play_order,
          tracking_uses_construction_offset_and_current_sibling_state,
          tracking_samples_authored_centers_exactly_once_at_construction,
          foreign_helpers_and_destructive_camera_operations_are_refused,
          removers_drop_the_frame_from_the_scene_list_and_keep_the_camera,
          become_and_restore_take_the_source_frame_pose,
          frame_rotation_ends_at_its_angle_whatever_the_frame_count,
          authored_rotation_dispatch_preserves_live_receiver,
          authored_rates_observe_restored_pose_and_may_edit_it,
          authored_rotation_owns_axis_interpretation,
          affine_precision_survives_placement_copy_and_raw_control_edits,
          scale_side_effects_keep_their_authored_camera_pose,
          final_camera_motion_changes_real_native_pixels)
for _case in _CASES:
    _case()
print(f"camera motion acceptance: {len(_CASES)} cases passed")
