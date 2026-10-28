"""Actual native camera-pose initialization and public placement-hook dispatch."""
from pathlib import Path
import tempfile
import unittest

import numpy as np
import manimlib as m


class CameraFrameLifecycleTests(unittest.TestCase):
    def test_five_control_points_are_real_live_record_views(self):
        frame = m.CameraFrame(frame_shape=(8., 4.), center_point=(2., 1., .5))
        expected = np.array([[2., 1., .5], [-2., 1., .5], [6., 1., .5],
                             [2., -1., .5], [2., 3., .5]], dtype=np.float32)
        points = frame.get_points()
        self.assertTrue(frame.has_points())
        self.assertEqual(frame.get_num_points(), 5)
        self.assertEqual(points.dtype, np.dtype('float32'))
        self.assertTrue(np.shares_memory(points, frame.data['point']))
        np.testing.assert_array_equal(points, expected)
        pointer = points.__array_interface__['data'][0]
        frame.shift(m.RIGHT).scale(.5, about_point=m.ORIGIN)
        np.testing.assert_array_equal(points, (expected + m.RIGHT) * .5)
        self.assertEqual(pointer, frame.get_points().__array_interface__['data'][0])
        before = points.copy()
        frame.reorient(30., 40., 20.).rotate(.2, m.RIGHT)
        np.testing.assert_array_equal(points, before)
        frame.set_shape(width=6., height=3.)
        np.testing.assert_array_equal(points, [[1.5, .5, .25], [-1.5, .5, .25],
                                              [4.5, .5, .25], [1.5, -1., .25], [1.5, 2., .25]])

    def test_point_view_mutations_update_pose_projection_and_focal_distance(self):
        frame = m.CameraFrame(frame_shape=(8., 4.))
        points = frame.get_points()
        points[:] += (2., -1., .5)
        points[1, 0] -= 2.
        points[2, 0] += 2.
        points[3, 1] -= 1.
        points[4, 1] += 1.
        control = m.CameraFrame(frame_shape=(12., 6.), center_point=(2., -1., .5))
        np.testing.assert_array_equal(frame.get_center(), control.get_center())
        self.assertEqual(frame.get_shape(), control.get_shape())
        self.assertEqual(frame.get_focal_distance(), control.get_focal_distance())
        np.testing.assert_array_equal(frame.get_view_matrix(), control.get_view_matrix())
        np.testing.assert_array_equal(frame.to_fixed_frame_point((1., 2., 3.)),
                                      control.to_fixed_frame_point((1., 2., 3.)))

    def test_observing_points_does_not_change_the_native_pose(self):
        frame = m.CameraFrame(frame_shape=(7.3, 2.1)).shift((.1, .2, .3)).scale(.7)
        before = frame.get_center(), frame.get_shape(), frame.get_view_matrix()
        frame.get_points()
        np.testing.assert_array_equal(frame.get_center(), before[0])
        self.assertEqual(frame.get_shape(), before[1])
        np.testing.assert_array_equal(frame.get_view_matrix(), before[2])

    def test_raw_control_edits_preserve_unaffected_exact_pose_components(self):
        frame = m.CameraFrame(frame_shape=(7.3, 2.1), center_point=(.1, .2, .3))
        center, shape = frame.get_center().copy(), frame.get_shape()
        matrix = frame.get_view_matrix().copy()
        points = frame.get_points()
        points[1, 2] += 1.25
        points[2, 1] += .75
        np.testing.assert_array_equal(frame.get_center(), center)
        self.assertEqual(frame.get_shape(), shape)
        np.testing.assert_array_equal(frame.get_view_matrix(), matrix)

        points[2, 0] += .5
        width = float(points[2, 0]) - float(points[1, 0])
        self.assertEqual(frame.get_shape(), (width, shape[1]))
        np.testing.assert_array_equal(frame.get_center(), center)

        points[0, 1] += .25
        center[1] = float(points[0, 1])
        np.testing.assert_array_equal(frame.get_center(), center)
        self.assertEqual(frame.get_shape(), (width, shape[1]))
        right = float(points[2, 0])
        points[2, 0] = points[1, 0]
        with self.assertRaises(ValueError):
            frame.get_shape()
        points[2, 0] = right
        self.assertEqual(frame.get_shape(), (width, shape[1]))

    def test_group_bounds_and_affine_edits_include_the_camera(self):
        scene = m.Scene()
        scene.add(m.Square())
        group = m.Group(*scene.mobjects)
        np.testing.assert_allclose((group.get_width(), group.get_height()), scene.frame.get_shape())
        points = scene.frame.get_points()
        original = points.copy()
        group.save_state()
        group.shift((2., -1., .5)).scale(.5, about_point=m.ORIGIN)
        np.testing.assert_allclose(points, (original + [2., -1., .5]) * .5)
        np.testing.assert_array_equal(scene.frame.get_center(), (1., -.5, .25))
        np.testing.assert_allclose(scene.frame.get_shape(), (m.FRAME_WIDTH / 2, 4.))
        matrix = np.array([[2., .25, 0.], [0., .5, 0.], [0., 0., 1.]])
        expected = points.copy() @ matrix.T
        group.apply_matrix(matrix, about_point=m.ORIGIN)
        np.testing.assert_allclose(points, expected)
        np.testing.assert_allclose(scene.frame.get_center(), expected[0])
        np.testing.assert_allclose(scene.frame.get_shape(),
                                   (expected[2, 0] - expected[1, 0], expected[4, 1] - expected[3, 1]))
        group.restore()
        np.testing.assert_array_equal(scene.frame.get_points(), original)

    def test_direct_frame_affine_methods_move_decorated_family_once(self):
        frame = m.CameraFrame(frame_shape=(8., 4.))
        dot = m.Dot((1., 0., 0.))
        frame.add(m.Group(dot), dot)
        frame.shift(m.RIGHT).scale(2., about_point=m.ORIGIN).stretch(.5, 1, about_point=m.ORIGIN)
        np.testing.assert_allclose(dot.get_center(), (4., 0., 0.), atol=1e-6)
        np.testing.assert_array_equal(frame.get_center(), (2., 0., 0.))
        self.assertEqual(frame.get_shape(), (16., 4.))

    def test_point_functions_and_native_core_setters_share_the_same_points(self):
        frame = m.CameraFrame(frame_shape=(8., 4.))
        points = frame.get_points()
        frame.apply_points_function(lambda values: values + [1., 2., 3.])
        np.testing.assert_array_equal(frame.get_center(), (1., 2., 3.))
        frame._core.set_shape((4., 2.))
        frame._core.set_center((2., 1., 0.))
        np.testing.assert_array_equal(points, [[2., 1., 0.], [0., 1., 0.], [4., 1., 0.],
                                              [2., 0., 0.], [2., 2., 0.]])

    def test_group_copy_and_restore_keep_independent_camera_geometry_and_uniforms(self):
        frame = m.CameraFrame(frame_shape=(8., 4.)).reorient(20., 30., 10.)
        group = m.Group(frame, m.Square())
        before = frame.get_points().copy()
        orientation = frame.get_orientation().copy()
        for deep in (False, True):
            copied = group.copy(deep=deep)
            copied_frame = copied[0]
            self.assertIsNot(copied_frame._core, frame._core)
            self.assertFalse(np.shares_memory(copied_frame.get_points(), frame.get_points()))
            copied.shift(2*m.RIGHT)
            copied_frame.get_points()[2, 0] += 2
            self.assertEqual(copied_frame.get_width(), 10.)
            np.testing.assert_array_equal(frame.get_points(), before)
        group.save_state()
        group.shift(m.RIGHT).scale(.5, about_point=m.ORIGIN)
        frame.reorient(60., 50., 40.).set_field_of_view(.7)
        group.restore()
        np.testing.assert_array_equal(frame.get_points(), before)
        np.testing.assert_array_equal(frame.get_orientation(), orientation)
        self.assertEqual(frame.get_shape(), (8., 4.))
        self.assertAlmostEqual(frame.get_field_of_view(), np.pi/4)

    def test_interpolation_preserves_actual_path_points_and_retained_views(self):
        start = m.CameraFrame(frame_shape=(8., 4.))
        start.get_points()[1, 2] = 2.
        end = start.copy().shift((2., 1., 0.)).scale(.5)
        end.get_points()[2, 1] += .75
        live = start.copy()
        view = live.get_points()
        calls = []
        def path(a, b, alpha):
            calls.append((a.copy(), b.copy()))
            result = (1-alpha)*a + alpha*b
            result[3, 2] += alpha
            return result
        live.interpolate(start, end, .5, path)
        np.testing.assert_array_equal(calls[0][0], start.get_points())
        np.testing.assert_array_equal(calls[0][1], end.get_points())
        expected = (start.get_points() + end.get_points()) / 2
        expected[3, 2] += .5
        np.testing.assert_array_equal(view, expected)
        before = view.copy()
        with self.assertRaises(ValueError):
            live.interpolate(start, end, .5, lambda *args: np.zeros((5, 3)))
        np.testing.assert_array_equal(view, before)

    def test_scene_state_restores_real_controls_without_rewriting_checkpoint_records(self):
        scene = m.Scene()
        frame = scene.frame
        frame.set_shape(7.3, 2.1).shift((.1, .2, .3))
        frame.get_points()[1, 2] = 1.25
        state = scene.get_state()
        points = frame.get_points().copy()
        shape, center = frame.get_shape(), frame.get_center().copy()
        frame.shift(2*m.RIGHT).scale(.5).reorient(20., 35., 10.)
        state.restore_scene(scene)
        np.testing.assert_array_equal(frame.get_points(), points)
        np.testing.assert_array_equal(frame.get_center(), center)
        self.assertEqual(frame.get_shape(), shape)
        self.assertEqual(bytes(scene._checkpoint_bytes()), state._checkpoint)

    def test_scene_state_ignored_camera_keeps_current_live_geometry(self):
        scene = m.Scene()
        square = m.Square()
        scene.add(square)
        frame = scene.frame
        state = m.SceneState(scene, ignore=[frame])
        frame.shift(2*m.RIGHT).scale(.5)
        frame.get_points()[1, 2] = 1.25
        points = frame.get_points().copy()
        center, shape = frame.get_center().copy(), frame.get_shape()
        square.shift(m.UP)
        state.restore_scene(scene)
        np.testing.assert_array_equal(frame.get_points(), points)
        np.testing.assert_array_equal(frame.get_center(), center)
        self.assertEqual(frame.get_shape(), shape)
        np.testing.assert_array_equal(square.get_center(), m.ORIGIN)
        self.assertEqual(scene.mobjects, [square])

    def test_scene_state_restores_detached_replacement_camera_controls(self):
        scene = m.Scene()
        frame = m.CameraFrame(frame_shape=(7.3, 2.1))
        scene.frame = frame
        frame.get_points()[2, 2] = 1.25
        original = frame.get_points().copy()
        state = scene.get_state()
        frame.get_points()[1, 2] = 2.5
        self.assertFalse(state.mobjects_match(scene.get_state()))
        self.assertGreater(state.n_changes(state), 0)
        frame.shift(m.RIGHT).scale(.5)
        state.restore_scene(scene)
        self.assertIs(scene.frame, frame)
        np.testing.assert_array_equal(frame.get_points(), original)
        self.assertEqual(frame.get_shape(), (7.3, 2.1))

    def test_raw_point_writes_reach_capture_without_a_prior_pose_getter(self):
        options = dict(resolution=(96, 54), samples=1)
        actual = m.Camera(frame_config=dict(frame_shape=(8., 4.)), **options)
        control = m.Camera(frame_config=dict(frame_shape=(8., 4.), center_point=(2., 0., 0.)), **options)
        shape = m.Triangle(fill_color=m.RED, fill_opacity=1).shift(2*m.RIGHT)
        baseline = actual.capture_snapshot(shape).png()
        actual.frame.get_points()[:] += 2*m.RIGHT
        captured = actual.capture_snapshot(shape).png()
        self.assertEqual(captured, control.capture_snapshot(shape).png())
        self.assertNotEqual(captured, baseline)

    def test_camera_animation_updates_retained_point_views_and_exact_endpoint(self):
        scene = m.Scene(camera_config=dict(fps=4))
        frame = scene.frame
        points = frame.get_points()
        before = points.copy()
        samples = []
        frame.add_updater(lambda mob, dt: samples.append(points.copy()))
        scene.play(frame.animate.shift(2*m.RIGHT).scale(.5, about_point=m.ORIGIN),
                   run_time=.75, rate_func=m.linear)
        np.testing.assert_allclose(points, (before + 2*m.RIGHT)*.5)
        np.testing.assert_allclose(frame.get_center(), points[0])
        self.assertGreater(len({sample.tobytes() for sample in samples}), 1)

    def test_pose_exists_during_hooks_and_finalizers_run_in_reference_order(self):
        class Authored(m.CameraFrame):
            def init_data(self):
                self.events = ['data']
                self.seen_core = self._core
                np.testing.assert_array_equal(self.get_center(), m.ORIGIN)
                super().init_data()
            def init_points(self):
                self.events.append('points')
                self.shift(3*m.RIGHT)
                self.reorient(10., 20., 30.)
                super().init_points()
            def init_uniforms(self):
                self.events.append('uniforms')
                super().init_uniforms()
            def set_width(self, value, **kwargs):
                self.events.append(('width', value, kwargs))
                return super().set_width(value, **kwargs)
            def set_height(self, value, **kwargs):
                self.events.append(('height', value, kwargs))
                return super().set_height(value, **kwargs)
            def move_to(self, point, **kwargs):
                self.events.append(('move', tuple(point)))
                return super().move_to(point, **kwargs)
        frame = Authored(frame_shape=(8., 5.), center_point=(2., 1., 0.), fovy=.8)
        self.assertEqual(frame.events, ['data', 'points', 'uniforms',
            ('width', 8., {'stretch': True}), ('height', 5., {'stretch': True}),
            ('move', (2., 1., 0.))])
        self.assertIs(frame.seen_core, frame._core)
        np.testing.assert_array_equal(frame.get_center(), (2., 1., 0.))
        np.testing.assert_array_equal(frame.get_orientation(), (0., 0., 0., 1.))
        self.assertEqual(frame.get_shape(), (8., 5.))
        self.assertAlmostEqual(frame.get_field_of_view(), .8)

    def test_public_sizing_and_placement_overrides_control_the_native_pose(self):
        class Authored(m.CameraFrame):
            def set_width(self, value, **kwargs):
                return super().set_width(value/2, **kwargs)
            def set_height(self, value, **kwargs):
                return super().set_height(value/2, **kwargs)
            def move_to(self, point, **kwargs):
                return super().move_to(np.array(point)+m.RIGHT, **kwargs)
        frame = Authored(frame_shape=(8., 4.), center_point=(2., 0., 0.))
        self.assertEqual(frame.get_shape(), (4., 2.))
        np.testing.assert_array_equal(frame.get_center(), (3., 0., 0.))
        self.assertEqual(tuple(frame._core.shape()), (4., 2.))

    def test_stock_constructor_retains_native_pose_values(self):
        for shape, center, fovy, axes in [
            ((14.222222222222221, 8.), (0., 0., 0.), np.pi/4, 'zxz'),
            ((7.3, 2.1), (3.2, -1.7, .8), .5, 'xyz'),
            ((1e-100, 2e-100), (0., 0., 0.), 1.1, 'zyx'),
        ]:
            with self.subTest(shape=shape):
                expected = m._CameraFrameCore(shape, center, fovy, axes)
                actual = m.CameraFrame(frame_shape=shape, center_point=center, fovy=fovy, euler_axes=axes)
                self.assertEqual(actual._core.shape(), expected.shape())
                self.assertEqual(actual._core.center(), expected.center())
                self.assertEqual(actual._core.field_of_view(), expected.field_of_view())
                self.assertEqual(actual._core.euler_axes(), expected.euler_axes())
                np.testing.assert_array_equal(actual.get_view_matrix(), expected.view_matrix())

    def test_records_decorations_and_updaters_survive_finalization(self):
        updater = lambda mob, dt: None
        marker_radius = .08
        target = np.array((4., 0., 0.))
        class Authored(m.CameraFrame):
            data_dtype = m.CameraFrame.data_dtype + [('mass', 1)]
            def init_points(self):
                self.set_points([[1., 2., 0.]])
                self.data['mass'][:] = 7
                self.marker = m.Dot(radius=marker_radius)
                self.add(self.marker)
                self.add_updater(updater)
                self.shift(m.RIGHT)
        frame = Authored(center_point=target)
        self.assertIn(frame.marker, frame.submobjects)
        self.assertIn(updater, frame.updaters)
        np.testing.assert_array_equal(frame.data['mass'], [[7.]] * 5)
        self.assertEqual(frame.get_num_points(), 5)
        # Finalization resets the camera controls to [-1, 1], while the
        # shifted Dot reaches 1 + radius. Public sizing and move_to act on
        # that family box; the indexed camera center remains left of it.
        x_scale = m.FRAME_WIDTH / (2. + marker_radius)
        expected_center = target - np.array((marker_radius * x_scale / 2., 0., 0.))
        np.testing.assert_allclose(frame.get_bounding_box()[1], target, rtol=0, atol=1e-6)
        np.testing.assert_allclose(frame.get_center(), expected_center, rtol=0, atol=1e-6)
        np.testing.assert_allclose(frame.get_center(), frame.get_points()[0], rtol=0, atol=1e-6)
        np.testing.assert_allclose(frame.get_shape(), (2*x_scale, m.FRAME_HEIGHT),
                                   rtol=0, atol=1e-6)

    def test_copy_taken_during_initialization_has_independent_native_pose(self):
        class Authored(m.CameraFrame):
            def init_points(self):
                self.early_copy = self.copy()
                self.early_copy.shift(m.LEFT)
        frame = Authored(frame_shape=(8., 4.), center_point=(2., 0., 0.))
        self.assertIsNot(frame._core, frame.early_copy._core)
        np.testing.assert_array_equal(frame.early_copy.get_center(), (-1., 0., 0.))
        self.assertEqual(frame.early_copy.get_shape(), (2., 2.))
        np.testing.assert_array_equal(frame.get_center(), (2., 0., 0.))

    def test_copy_and_deepcopy_do_not_repeat_finalization_hooks(self):
        calls = []
        class Authored(m.CameraFrame):
            def set_width(self, value, **kwargs):
                calls.append(value)
                return super().set_width(value/2, **kwargs)
        original = Authored(frame_shape=(8., 4.))
        for deep in (False, True):
            copied = original.copy(deep=deep)
            self.assertEqual(copied.get_width(), 4.)
            copied.shift(m.RIGHT)
            np.testing.assert_array_equal(original.get_center(), m.ORIGIN)
            self.assertIsNot(copied._core, original._core)
        self.assertEqual(calls, [8.])

    def test_hook_failure_is_not_retried_and_no_positioning_follows(self):
        calls, failure = [], RuntimeError('authored camera points')
        class Authored(m.CameraFrame):
            def init_points(self):
                calls.append('points')
                self.shift(m.RIGHT)
                raise failure
            def set_width(self, value, **kwargs):
                calls.append('width')
                return super().set_width(value, **kwargs)
        with self.assertRaises(RuntimeError) as caught:
            Authored()
        self.assertIs(caught.exception, failure)
        self.assertEqual(calls, ['points'])
        self.assertEqual(m.CameraFrame().get_height(), 8.)

    def test_invalid_constructor_inputs_refuse_before_hooks(self):
        calls = []
        class Authored(m.CameraFrame):
            def init_points(self):
                calls.append('points')
        for options in [dict(frame_shape=(0., 4.)), dict(center_point=(float('nan'), 0., 0.)),
                        dict(fovy=0), dict(euler_axes='bad')]:
            with self.subTest(options=options), self.assertRaises(ValueError):
                Authored(**options)
        self.assertEqual(calls, [])

    def test_finalizer_failure_does_not_continue_or_hide_the_error(self):
        calls, failure = [], LookupError('authored camera width')
        class Authored(m.CameraFrame):
            def set_width(self, value, **kwargs):
                calls.append('width')
                raise failure
            def set_height(self, value, **kwargs):
                calls.append('height')
                return super().set_height(value, **kwargs)
        with self.assertRaises(LookupError) as caught:
            Authored()
        self.assertIs(caught.exception, failure)
        self.assertEqual(calls, ['width'])

    def test_authored_finalizer_orientation_and_default_reset(self):
        class Authored(m.CameraFrame):
            def move_to(self, point, **kwargs):
                super().move_to(point, **kwargs)
                self.reorient(20., 35., 10.)
                return self
        frame = Authored()
        expected = m.CameraFrame().reorient(20., 35., 10.)
        np.testing.assert_array_equal(frame.get_orientation(), expected.get_orientation())
        frame.make_orientation_default()
        frame.reorient(0, 0, 0).to_default_state()
        np.testing.assert_array_equal(frame.get_orientation(), expected.get_orientation())

    def test_authored_frame_capture_matches_independent_camera(self):
        class Authored(m.CameraFrame):
            def set_width(self, value, **kwargs):
                return super().set_width(value/2, **kwargs)
            def move_to(self, point, **kwargs):
                return super().move_to(np.array(point)+2*m.RIGHT, **kwargs)
        class AuthoredCamera(m.Camera):
            def init_frame(self, **config):
                self.frame = Authored(**config)
        options = dict(resolution=(96, 54), samples=1)
        camera = AuthoredCamera(frame_config=dict(frame_shape=(8., 4.)), **options)
        control = m.Camera(frame_config=dict(frame_shape=(4., 4.), center_point=(2., 0., 0.)), **options)
        shape = m.Triangle(fill_color=m.RED, fill_opacity=1).move_to((2., 0., 0.))
        self.assertEqual(camera.capture_snapshot(shape).png(), control.capture_snapshot(shape).png())
        self.assertNotEqual(camera.get_png(), m.Camera(**options).capture_snapshot(shape).png())

    def test_scene_camera_tracks_authored_pose_with_identical_one_four_thread_frames(self):
        class Authored(m.CameraFrame):
            def set_width(self, value, **kwargs):
                return super().set_width(value/2, **kwargs)
            def move_to(self, point, **kwargs):
                return super().move_to(np.array(point)+2*m.RIGHT, **kwargs)
        def render(path, threads, authored):
            scene = m.Scene(camera_config=dict(resolution=(96, 54), fps=4, samples=1))
            scene.frame = (Authored(frame_shape=(8., 4.)) if authored else
                           m.CameraFrame(frame_shape=(4., 4.), center_point=(2., 0., 0.)))
            with scene.render_session(path, format='png_sequence', resolution=(96, 54), fps=4, threads=threads):
                shape = m.Triangle(fill_color=m.RED, fill_opacity=1).move_to((2., 0., 0.))
                scene.add(shape)
                scene.play(scene.frame.animate.shift(.5*m.RIGHT), run_time=.75, rate_func=m.linear)
            return [p.read_bytes() for p in sorted(path.glob('*.png'))]
        with tempfile.TemporaryDirectory(prefix='fmn-frame-init-') as temp:
            root = Path(temp)
            actual = render(root/'one', 1, True)
            self.assertEqual(len(set(actual)), 3)
            self.assertEqual(actual, render(root/'four', 4, True))
            self.assertEqual(actual, render(root/'control', 1, False))


if __name__ == '__main__':
    unittest.main()
