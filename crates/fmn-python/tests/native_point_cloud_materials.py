"""Packed point-cloud records and native scalar glow against actual Lumen output."""
from pathlib import Path
import copy
import gc
import pickle
import tempfile
import unittest
import weakref

import numpy as np
import manimlib as m
from fmn_python import render_session


def render(path, cloud, threads=1):
    scene = m.Scene()
    with render_session(scene, path, resolution=(64, 40), fps=8, threads=threads) as session:
        scene.add(cloud)
        scene.wait(.375)
    assert session.result.frame_count == 3
    return path.read_bytes()


def cloud(glow=0., points=None):
    return m.DotCloud([[0, 0, 0]] if points is None else points,
                      color=m.BLUE, radius=.8, opacity=.7, glow_factor=glow)


class PointCloudMaterialTests(unittest.TestCase):
    def test_reference_dtype_is_packed_and_accepts_independent_set_data(self):
        # These are the pinned Reference's fields, offsets and itemsize, not
        # a dtype derived from the implementation under test.
        dtype = np.dtype([('point', np.float32, (3,)),
                          ('radius', np.float32, (1,)),
                          ('rgba', np.float32, (4,))])
        for cls in (m.DotCloud, m.TrueDot, m.GlowDot, m.GlowDots):
            with self.subTest(cls=cls.__name__):
                dots = cls()
                self.assertEqual(dots.data.dtype, dtype)
                self.assertEqual(dots.data.dtype.itemsize, 32)
                self.assertEqual([dots.data.dtype.fields[key][1] for key in dtype.names],
                                 [0, 12, 16])
                self.assertEqual(dots._engine_state()['fields'],
                                 [('point', 3), ('radius', 1), ('rgba', 4)])
                records = np.zeros(2, dtype=dtype)
                records['point'] = [[-1., 0., 0.], [1., 0., 0.]]
                records['radius'] = .5
                records['rgba'] = [.25, .5, .75, 1.]
                glow = dots.get_glow_factor()
                self.assertIs(dots.set_data(records), dots)
                self.assertEqual(dots.data.strides, (32,))
                self.assertEqual(dots.data.tobytes(), records.tobytes())
                self.assertEqual(dots.get_glow_factor(), glow)
                # The packed export is still a real writable record view.
                live = dots.data
                live['radius'][0, 0] = .25
                self.assertEqual(dots.get_radii()[0, 0], .25)

    def test_direct_uniform_assignment_updates_native_material_only(self):
        dots = cloud(points=[[-1, 0, 0], [1, 0, 0]])
        before = dots.data.copy()
        dots.uniforms['glow_factor'] = 2.
        self.assertEqual(dots.get_glow_factor(), 2.)
        self.assertEqual(dots._get_uniform('glow_factor'), 2.)
        self.assertNotIn('glow_factor', dots.uniforms._extras)
        self.assertEqual(dots.data.tobytes(), before.tobytes())

    def test_mapping_updates_and_public_uniform_setters_share_the_native_path(self):
        dots = cloud()
        for apply in (lambda: dots.uniforms.update(glow_factor=3.),
                      lambda: dots.set_uniform(glow_factor=3.),
                      lambda: dots.set_uniforms({'glow_factor': 3.})):
            dots.set_glow_factor(0.)
            apply()
            self.assertEqual(dots._get_uniform('glow_factor'), 3.)

    def test_public_method_returns_self_and_updates_legacy_attribute(self):
        dots = cloud()
        self.assertIs(dots.set_glow_factor(4.), dots)
        self.assertEqual(dots.glow_factor, 4.)
        self.assertEqual(dots.get_glow_factor(), 4.)
        self.assertEqual(dots._get_uniform('glow_factor'), 4.)

    def test_empty_default_survives_append_copy_and_deepcopy(self):
        for edit in (lambda d: d.set_glow_factor(3.),
                     lambda d: d.uniforms.update(glow_factor=3.)):
            dots = cloud(points=[])
            edit(dots)
            for value in (dots, dots.copy(), copy.deepcopy(dots)):
                with self.subTest(copy=value is not dots):
                    self.assertEqual(value.get_num_points(), 0)
                    value.add_points([[0, 0, 0]])
                    self.assertEqual(value.get_glow_factor(), 3.)
                    self.assertEqual(value._get_uniform('glow_factor'), 3.)
                    self.assertEqual(value.data.dtype.itemsize, 32)

    def test_cleared_cloud_retains_the_edited_material(self):
        dots = cloud(glow=1.)
        dots.clear_points()
        dots.uniforms['glow_factor'] = 4.
        dots.set_points([[-1, 0, 0], [1, 0, 0]])
        self.assertEqual(dots._get_uniform('glow_factor'), 4.)
        self.assertEqual(dots.data.dtype.names, ('point', 'radius', 'rgba'))

    def test_copies_do_not_alias_native_uniforms(self):
        for points in ([], [[0, 0, 0]]):
            dots = cloud(points=points)
            dots.uniforms['glow_factor'] = 2.
            duplicate = dots.copy()
            duplicate.uniforms['glow_factor'] = 5.
            self.assertEqual(dots.get_glow_factor(), 2.)
            self.assertEqual(dots._get_uniform('glow_factor'), 2.)
            self.assertEqual(duplicate._get_uniform('glow_factor'), 5.)

    def test_invalid_glow_refuses_before_mapping_records_or_attributes_change(self):
        for points in ([], [[0, 0, 0]]):
            dots = cloud(points=points)
            for value in (-1., float('nan'), float('inf'), -float('inf')):
                for method in (False, True):
                    with self.subTest(points=points, value=value, method=method):
                        before = dots._style_data().tobytes(), dict(dots.uniforms), dots.glow_factor
                        with self.assertRaises(ValueError):
                            if method:
                                dots.set_glow_factor(value)
                            else:
                                dots.uniforms['glow_factor'] = value
                        self.assertEqual((dots._style_data().tobytes(), dict(dots.uniforms), dots.glow_factor), before)

    def test_scalar_uniform_keeps_f64_precision_and_converts_before_arena_borrow(self):
        dots = cloud()
        value = 1.0 + 2.0 ** -40
        dots.set_glow_factor(value)
        self.assertEqual(dots.get_glow_factor(), value)
        self.assertNotEqual(dots.get_glow_factor(), float(np.float32(value)))
        self.assertEqual(cloud(glow=1e40).get_glow_factor(), 1e40)
        scene = m.Scene()
        scene.add(dots)
        class Reentrant:
            def __float__(self):
                dots.uniforms['anti_alias_width'] = 3.
                dots.shift(m.RIGHT)
                return 2.
        dots.uniforms['glow_factor'] = Reentrant()
        self.assertEqual(dots.get_glow_factor(), 2.)
        self.assertEqual(dots.uniforms['anti_alias_width'], 3.)
        np.testing.assert_array_equal(dots.get_points(), [[1., 0., 0.]])

    def test_other_mobject_uniforms_and_cloud_extras_are_not_reinterpreted(self):
        shape = m.Circle()
        shape.uniforms['glow_factor'] = 'authored payload'
        self.assertEqual(shape.uniforms['glow_factor'], 'authored payload')
        dots = cloud()
        payload = object()
        dots.uniforms['custom'] = payload
        dots.uniforms['anti_alias_width'] = 3.
        self.assertIs(dots.uniforms['custom'], payload)
        self.assertEqual(dots.uniforms['anti_alias_width'], 3.)

    def test_uniform_edits_do_not_need_a_writable_record_view(self):
        dots = cloud()
        frozen = dots.data.copy()
        frozen.flags.writeable = False
        dots._style_data = lambda: frozen
        before = dots.data.tobytes()
        dots.set_glow_factor(3.)
        self.assertEqual(dots._get_uniform('glow_factor'), 3.)
        self.assertEqual(dots.data.tobytes(), before)

    def test_authored_style_accessor_does_not_intercept_native_uniform(self):
        dots = cloud()
        def forbidden():
            raise AssertionError('a scalar uniform must not ask for record data')
        dots._style_data = forbidden
        dots.uniforms['glow_factor'] = 3.
        self.assertEqual(dots._get_uniform('glow_factor'), 3.)

    def test_uniform_mapping_keeps_weak_owner_lifetime(self):
        dots = cloud()
        table = dots.uniforms
        ref = weakref.ref(dots)
        dots.uniforms['glow_factor'] = 3.
        del dots
        gc.collect()
        self.assertIsNone(ref())
        with self.assertRaises(ReferenceError):
            table['glow_factor'] = 1.

    def test_authored_field_named_glow_remains_independent_custom_data(self):
        class CustomCloud(m.DotCloud):
            data_dtype = [*m.DotCloud.data_dtype, ('glow_factor', np.float32, (2,))]
        points = [[-1, 0, 0], [1, 0, 0]]
        dots = CustomCloud(points, color=m.BLUE, radius=.8, opacity=.7, glow_factor=4.)
        dots.data['glow_factor'][:] = [[1., 2.], [3., 4.]]
        duplicate = dots.copy()
        np.testing.assert_array_equal(duplicate.data['glow_factor'], [[1., 2.], [3., 4.]])
        duplicate.uniforms['glow_factor'] = 2.
        self.assertEqual(duplicate.get_glow_factor(), 2.)
        self.assertEqual(dots.get_glow_factor(), 4.)
        np.testing.assert_array_equal(duplicate.data['glow_factor'], [[1., 2.], [3., 4.]])
        np.testing.assert_array_equal(dots.data['glow_factor'], [[1., 2.], [3., 4.]])
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.assertEqual(render(root / 'custom.y4m', duplicate),
                             render(root / 'stock.y4m', cloud(glow=2., points=points)))

    def test_interpolation_keeps_glow_out_of_point_records(self):
        start = cloud(glow=1., points=[[-1, 0, 0], [1, 0, 0]])
        end = cloud(glow=3., points=[[-1, 1, 0], [1, 1, 0]])
        result = start.copy()
        result.interpolate(start, end, .5)
        np.testing.assert_array_equal(result.get_points(), [[-1., .5, 0.], [1., .5, 0.]])
        self.assertEqual(result.uniforms['glow_factor'], 2.)
        self.assertEqual(result._get_uniform('glow_factor'), 2.)
        self.assertEqual(result.data.dtype.itemsize, 32)
        np.testing.assert_array_equal(start.get_points(), [[-1., 0., 0.], [1., 0., 0.]])
        self.assertEqual(start.uniforms['glow_factor'], 1.)

    def test_interpolation_locks_and_authored_paths_remain_live(self):
        start, end = cloud(glow=1.), cloud(glow=3.)
        result, other = cloud(glow=0.), cloud(glow=0.)
        result.locked_uniform_keys = {'glow_factor'}
        def path(a, b, alpha):
            self.assertIn('glow_factor', start.uniforms)
            self.assertIn('glow_factor', result.uniforms)
            other.uniforms['glow_factor'] = 4.
            return (1 - alpha) * a + alpha * b
        result.interpolate(start, end, .5, path)
        self.assertEqual(result.get_glow_factor(), 0.)
        self.assertEqual(other._get_uniform('glow_factor'), 4.)

    def test_stock_transform_keeps_native_dispatch(self):
        dots = cloud()
        animation = m.Transform(dots, cloud(glow=3.))
        native = getattr(m, '_native', m)
        self.assertFalse(native._requires_python_animation(animation))

    def test_native_and_callback_transform_interpolate_the_same_scalar_glow(self):
        def sequence(path, rate_func):
            start, end = cloud(glow=1.), cloud(glow=4.)
            scene = m.Scene()
            with render_session(scene, path, resolution=(64, 40), fps=8) as session:
                scene.add(start)
                scene.play(m.Transform(start, end), run_time=.375, rate_func=rate_func)
            self.assertEqual(session.result.frame_count, 3)
            self.assertEqual(start._get_uniform('glow_factor'), 4.)
            self.assertEqual(start.data.dtype.itemsize, 32)
            return path.read_bytes()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            native = sequence(root / 'native.y4m', m.linear)
            callback = sequence(root / 'callback.y4m', lambda t: t)
            self.assertEqual(native, callback)

    def test_callback_transform_preserves_authored_fields_with_glow_spelling(self):
        class CustomCloud(m.DotCloud):
            data_dtype = [*m.DotCloud.data_dtype, ('glow_factor', np.float32, (1,))]
        def custom(glow, points):
            return CustomCloud(points, color=m.BLUE, radius=.8, opacity=.7, glow_factor=glow)
        start = custom(1., [[-1, 0, 0], [1, 0, 0]])
        end = custom(3., [[-1, 1, 0], [1, 1, 0]])
        start.data['glow_factor'][:] = [[1.], [2.]]
        end.data['glow_factor'][:] = [[3.], [6.]]
        scene = m.Scene()
        with tempfile.TemporaryDirectory() as tmp:
            with render_session(scene, Path(tmp) / 'animated.y4m', resolution=(64, 40), fps=8) as session:
                scene.add(start)
                scene.play(m.Transform(start, end), run_time=.375, rate_func=lambda t: t)
            self.assertEqual(session.result.frame_count, 3)
        np.testing.assert_array_equal(start.data['glow_factor'], [[3.], [6.]])
        self.assertEqual(start.uniforms['glow_factor'], 3.)

    def test_authored_path_exception_keeps_original_error_and_live_maps(self):
        start, end, result = cloud(glow=1.), cloud(glow=3.), cloud(glow=0.)
        error = RuntimeError('authored path refused')
        def path(a, b, alpha):
            self.assertIn('glow_factor', start.uniforms)
            result.uniforms['glow_factor'] = 4.
            raise error
        with self.assertRaises(RuntimeError) as caught:
            result.interpolate(start, end, .5, path)
        self.assertIs(caught.exception, error)
        self.assertEqual(result.get_glow_factor(), 4.)
        self.assertEqual(result._get_uniform('glow_factor'), 4.)
        self.assertEqual(start.get_glow_factor(), 1.)

    def test_direct_uniform_renders_as_independent_constructor_at_all_worker_counts(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            expected = render(root / 'expected.y4m', cloud(glow=2.))
            negative = render(root / 'negative.y4m', cloud(glow=0.))
            self.assertNotEqual(expected, negative)
            for threads in (1, 4, 16):
                dots = cloud()
                dots.uniforms['glow_factor'] = 2.
                actual = render(root / f'actual-{threads}.y4m', dots, threads)
                self.assertEqual(actual, expected)

    def test_live_edits_invalidate_retained_frames_without_rebuilding_geometry(self):
        def sequence(path, replace, threads=1):
            scene, dots = m.Scene(), cloud()
            with render_session(scene, path, resolution=(64, 40), fps=8, threads=threads) as session:
                scene.add(dots)
                for glow in (0., 2., 4.):
                    if replace:
                        scene.remove(dots)
                        dots = cloud(glow=glow)
                        scene.add(dots)
                    else:
                        before = dots.data.tobytes()
                        dots.uniforms['glow_factor'] = glow
                        self.assertEqual(dots.data.tobytes(), before)
                    scene.wait(.125)
            self.assertEqual(session.result.frame_count, 3)
            return path.read_bytes()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            # Fresh independently constructed clouds defeat all retained
            # material/frame caches and therefore provide the live-edit oracle.
            expected = sequence(root / 'expected.y4m', True)
            negative = render(root / 'negative.y4m', cloud(glow=0.))
            self.assertNotEqual(expected, negative)
            for threads in (1, 4, 16):
                actual = sequence(root / f'actual-{threads}.y4m', False, threads)
                self.assertEqual(actual, expected)

    def test_native_uniform_survives_become_restore_and_pickle(self):
        source = cloud(glow=1.0 + 2.0 ** -40)
        class Receiver(m.Mobject):
            data_dtype = m.DotCloud.data_dtype
        receiver = Receiver()
        receiver.uniforms['glow_factor'] = 'authored payload'
        receiver.become(source)
        self.assertEqual(receiver.uniforms['glow_factor'], source.get_glow_factor())
        self.assertEqual(list(receiver.uniforms).count('glow_factor'), 1)
        self.assertEqual(len(receiver.uniforms), len(receiver.uniforms.keys()))
        self.assertEqual(receiver.uniforms._extras['glow_factor'], 'authored payload')
        receiver.uniforms['glow_factor'] = 3.
        self.assertEqual(receiver._get_uniform('glow_factor'), 3.)
        self.assertEqual(source.get_glow_factor(), 1.0 + 2.0 ** -40)
        receiver.become(Receiver())
        self.assertEqual(receiver.uniforms['glow_factor'], 'authored payload')
        source.save_state()
        source.set_glow_factor(4.)
        source.restore()
        self.assertEqual(source.get_glow_factor(), 1.0 + 2.0 ** -40)
        restored = pickle.loads(pickle.dumps(source))
        self.assertEqual(restored.data.dtype.itemsize, 32)
        self.assertEqual(restored.get_glow_factor(), source.get_glow_factor())
        restored.set_glow_factor(5.)
        self.assertEqual(source.get_glow_factor(), 1.0 + 2.0 ** -40)

    def test_scene_checkpoint_restores_scalar_material_and_pixels(self):
        scene, dots = m.Scene(), cloud(glow=2.)
        scene.add(dots)
        saved = scene.get_state()
        dots.uniforms['glow_factor'] = 4.
        scene.restore_state(saved)
        self.assertEqual(dots._get_uniform('glow_factor'), 2.)
        self.assertEqual(dots.data.dtype.itemsize, 32)
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.assertEqual(render(root / 'restored.y4m', dots.copy()),
                             render(root / 'fresh.y4m', cloud(glow=2.)))

    def test_zero_radius_draws_nothing_and_can_grow_again(self):
        def sequence(path, remove_at_zero, threads):
            scene, dots = m.Scene(), cloud(glow=2.)
            with render_session(scene, path, resolution=(64, 40), fps=8, threads=threads) as session:
                scene.add(dots)
                scene.wait(.125)
                if remove_at_zero:
                    scene.remove(dots)
                else:
                    dots.set_radius(0.)
                scene.wait(.125)
                if remove_at_zero:
                    scene.add(dots)
                else:
                    dots.set_radius(.8)
                scene.wait(.125)
            self.assertEqual(session.result.frame_count, 3)
            return path.read_bytes()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            expected = sequence(root / 'expected.y4m', True, 1)
            for threads in (1, 4, 16):
                actual = sequence(root / f'zero-{threads}.y4m', False, threads)
                self.assertEqual(actual, expected)

    def test_zero_radius_records_do_not_hide_visible_members(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            expected = render(root / 'expected.y4m', cloud(points=[[1., 0., 0.]]))
            for threads in (1, 4, 16):
                dots = cloud(points=[[-1., 0., 0.], [1., 0., 0.]])
                dots.set_radii([0., .8])
                self.assertEqual(render(root / f'mixed-{threads}.y4m', dots, threads), expected)


def run_native_point_cloud_materials():
    result = unittest.TextTestRunner(verbosity=2).run(
        unittest.defaultTestLoader.loadTestsFromTestCase(PointCloudMaterialTests))
    if not result.wasSuccessful():
        raise AssertionError('native point-cloud material acceptance failed')
    return result.testsRun


if __name__ == '__main__':
    run_native_point_cloud_materials()
