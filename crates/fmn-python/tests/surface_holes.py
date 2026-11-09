"""Explicit numerical holes through the real Python/Atlas/Lumen lifecycle."""
import copy
from pathlib import Path
import struct
import tempfile
from types import SimpleNamespace
import unittest
import zlib

import numpy as np
import manimlib as m
from fmn_python import render_session


DOMAIN = dict(u_range=(-2., 2.), v_range=(-2., 2.), resolution=(5, 5),
              shading=(0., 0., 0.))


def punctured(u, v):
    return u, v, float('inf') if u == 0 and v == 0 else 0.


def surface(function=punctured, **options):
    return m.ParametricSurface(function, **dict(DOMAIN, allow_undefined=True, **options))


def white_texture(path):
    def chunk(kind, body):
        return (struct.pack('>I', len(body)) + kind + body
                + struct.pack('>I', zlib.crc32(kind + body)))
    path.write_bytes(b'\x89PNG\r\n\x1a\n'
                     + chunk(b'IHDR', struct.pack('>IIBBBBB', 1, 1, 8, 6, 0, 0, 0))
                     + chunk(b'IDAT', zlib.compress(b'\0\xff\xff\xff\xff'))
                     + chunk(b'IEND', b''))
    return path


def render(path, mob, threads=1):
    scene = m.Scene()
    with render_session(scene, path, resolution=(48, 32), fps=8, threads=threads):
        if mob is not None:
            scene.add(mob)
        scene.wait(.125)
    return path.read_bytes()


class SurfaceHoleTests(unittest.TestCase):
    def test_strict_default_and_all_finite_bits_are_preserved(self):
        for value in (float('nan'), float('inf'), float('-inf')):
            with self.subTest(value=value), self.assertRaises(ValueError):
                m.ParametricSurface(lambda u, v: (u, v, value), **DOMAIN)
        finite = lambda u, v: (u, v, u*v)
        strict = m.ParametricSurface(finite, **DOMAIN)
        opted = surface(finite)
        self.assertEqual(strict.data.dtype, opted.data.dtype)
        self.assertEqual(strict.data.tobytes(), opted.data.tobytes())
        self.assertNotIn('surface_validity', opted.data.dtype.names)
        self.assertTrue(opted.get_sample_validity().all())
        for value in ('holes', 1, None):
            with self.subTest(policy=value), self.assertRaises(TypeError):
                m.ParametricSurface(finite, allow_undefined=value, **DOMAIN)

    def test_mask_filters_triangles_queries_normals_and_bounds(self):
        obj = surface()
        expected = np.ones(25, dtype=bool)
        expected[12] = False
        np.testing.assert_array_equal(obj.get_sample_validity(), expected)
        self.assertTrue(all(np.isfinite(obj.data[name]).all() for name in obj.data.dtype.names))
        indices = obj.get_triangle_indices().reshape(-1, 3)
        self.assertEqual(len(indices), 26)  # six of 32 triangles meet the middle knot
        self.assertNotIn(12, indices)
        np.testing.assert_array_equal(obj.get_unit_normals()[12], [0, 0, 0])
        with self.assertRaisesRegex(ValueError, 'undefined surface sample'):
            obj.uv_to_point(0, 0)
        with self.assertRaises(ValueError):
            obj.uv_to_point(.5, .5)
        # The absent (0, 0) neighbor contributes zero at this exact knot.
        np.testing.assert_allclose(obj.uv_to_point(-1, 0), [-1, 0, 0])
        # A translated surface's inert storage must not expand its bounds.
        obj.shift((10, 20, 30))
        np.testing.assert_allclose(obj.get_center(), [10, 20, 30], atol=1e-6)
        self.assertAlmostEqual(obj.get_width(), 4.)
        self.assertAlmostEqual(obj.get_height(), 4.)
        mask = obj.get_sample_validity()
        mask[:] = False
        np.testing.assert_array_equal(obj.get_sample_validity(), expected)

    def test_derivative_holes_and_first_callback_failure_keep_their_meaning(self):
        calls = []
        def derivative(u, v):
            calls.append((u, v))
            return (u, v, float('nan') if u == .001 and v == 0 else 0.)
        obj = surface(derivative)
        self.assertFalse(obj.get_sample_validity()[12])
        self.assertEqual(len(calls), 75)  # point, du, dv for every UV knot
        error, visits = LookupError('authored UV failure'), []
        def broken(u, v):
            visits.append((u, v))
            if len(visits) == 4:
                raise error
            return u, v, float('inf')
        with self.assertRaises(LookupError) as caught:
            surface(broken)
        self.assertIs(caught.exception, error)
        self.assertEqual(len(visits), 4)
        for sample in ((1e100, 0, float('inf')), (0, 0), (0, 0, 1j)):
            with self.subTest(sample=sample), self.assertRaises((ValueError, TypeError)):
                surface(lambda u, v: sample)
        # Source-dtype bounds precede narrowing: this first finite value can
        # round back to f32::MAX as f64, and the second can become infinity.
        above_f32 = np.nextafter(np.longdouble(np.finfo(np.float32).max), np.longdouble('inf'))
        with self.assertRaises(ValueError):
            surface(lambda u, v: np.array((above_f32, 0, np.nan), dtype=np.longdouble))
        if np.finfo(np.longdouble).max > np.finfo(np.float64).max:
            huge = np.finfo(np.longdouble).max
            for z in (0., float('inf')):
                with self.subTest(extended_finite=True, z=z), self.assertRaises(ValueError):
                    surface(lambda u, v: np.array((huge, 0, z), dtype=np.longdouble))

    def test_regeneration_preserves_identity_and_existing_live_generation(self):
        obj = surface()
        scene = m.Scene()
        scene.add(obj)
        child = m.Dot()
        obj.add(child)
        records = obj.data
        obj.passed_uv_func = lambda u, v: (u, v, 1.)
        obj.init_points()
        self.assertIs(obj._scene, scene)
        self.assertIs(obj.submobjects[0], child)
        self.assertTrue(obj.get_sample_validity().all())
        self.assertEqual(len(obj.get_triangle_indices()), 96)
        np.testing.assert_array_equal(records['point'][:, 2], 1.)
        np.testing.assert_array_equal(records['surface_validity'], 1.)
        obj.passed_uv_func = punctured
        obj.init_points()
        self.assertFalse(records['surface_validity'][12, 0])
        self.assertEqual(scene.get_time(), 0.)

    def test_schema_promotion_detaches_old_views_without_losing_custom_data(self):
        class Tagged(m.ParametricSurface):
            data_dtype = [*m.Surface.data_dtype, ('tag', np.float32, (1,))]
        obj = Tagged(lambda u, v: (u, v, 0.), allow_undefined=True, **DOMAIN)
        obj.data['tag'][:] = 7
        old = obj.data
        before = old.copy()
        obj.passed_uv_func = punctured
        obj.init_points()
        self.assertIn('surface_validity', obj.data.dtype.names)
        self.assertNotIn('surface_validity', old.dtype.names)
        np.testing.assert_array_equal(old, before)
        np.testing.assert_array_equal(obj.data['tag'], 7.)
        old['point'][:] = 100
        self.assertLess(obj.get_width(), 5.)

    def test_regridding_copy_and_become_keep_policy_and_mask(self):
        obj = surface()
        for duplicate in (obj.copy(), copy.deepcopy(obj)):
            self.assertTrue(duplicate.allow_undefined)
            np.testing.assert_array_equal(duplicate.data, obj.data)
            duplicate.init_points()
            np.testing.assert_array_equal(duplicate.get_sample_validity(), obj.get_sample_validity())
        obj.set_resolution((7, 7))
        self.assertEqual(obj.resolution, (7, 7))
        self.assertEqual(np.count_nonzero(~obj.get_sample_validity()), 1)
        receiver = m.Surface(resolution=(3, 3))
        receiver.become(obj)
        self.assertTrue(receiver.allow_undefined)
        self.assertEqual(receiver.resolution, obj.resolution)
        np.testing.assert_array_equal(receiver.get_sample_validity(), obj.get_sample_validity())
        complete = m.Surface(resolution=(7, 7))
        receiver.become(complete)
        self.assertFalse(receiver.allow_undefined)
        self.assertTrue(receiver.get_sample_validity().all())

    def test_strict_to_masked_become_detaches_old_live_views(self):
        receiver = m.ParametricSurface(lambda u, v: (u, v, 2.), **DOMAIN)
        source = surface()
        old = receiver.data
        before = old.copy()
        source_before = source.data.copy()
        self.assertIs(receiver.become(source), receiver)
        self.assertTrue(receiver.allow_undefined)
        np.testing.assert_array_equal(receiver.get_sample_validity(), source.get_sample_validity())
        np.testing.assert_array_equal(receiver.get_points(), source.get_points())
        np.testing.assert_array_equal(source.data, source_before)
        np.testing.assert_array_equal(old, before)
        self.assertNotIn('surface_validity', old.dtype.names)
        old['point'][:] = 100
        np.testing.assert_array_equal(receiver.get_points(), source.get_points())

    def test_masked_to_strict_become_keeps_live_mask_valid_and_source_strict(self):
        receiver = surface()
        source = m.ParametricSurface(lambda u, v: (u, v, 2.), **DOMAIN)
        records = receiver.data
        self.assertIs(receiver.become(source), receiver)
        self.assertFalse(receiver.allow_undefined)
        self.assertTrue(receiver.get_sample_validity().all())
        np.testing.assert_array_equal(records['surface_validity'], 1.)
        np.testing.assert_array_equal(records['point'], source.data['point'])
        np.testing.assert_array_equal(receiver.get_points(), source.get_points())
        self.assertNotIn('surface_validity', source.data.dtype.names)

    def test_become_promotes_nested_surface_descendants(self):
        left = m.Surface(resolution=(3, 3))
        right = m.Surface(resolution=(4, 4))
        receiver = m.Group(left, m.Group(right))
        first = surface()
        second = surface(lambda u, v: (u, v, float('nan') if u == 1 and v == 0 else 1.))
        source = m.Group(first, m.Group(second))
        family = tuple(receiver.get_family())
        receiver.become(source)
        self.assertEqual(tuple(receiver.get_family()), family)
        for actual, expected in ((left, first), (right, second)):
            self.assertTrue(actual.allow_undefined)
            self.assertEqual(actual.resolution, expected.resolution)
            np.testing.assert_array_equal(actual.get_sample_validity(), expected.get_sample_validity())
            np.testing.assert_array_equal(actual.get_points(), expected.get_points())

    def test_saved_state_round_trip_across_mask_schema_promotion(self):
        finite = lambda u, v: (u, v, 2.)
        for initially_masked in (False, True):
            with self.subTest(initially_masked=initially_masked):
                obj = surface() if initially_masked else m.ParametricSurface(finite, **DOMAIN)
                before = obj.data.copy()
                present = obj.get_sample_validity()
                policy = obj.allow_undefined
                obj.save_state()
                saved = obj.saved_state
                obj.become(m.ParametricSurface(finite, **DOMAIN) if initially_masked else surface())
                self.assertIs(obj.restore(), obj)
                self.assertIs(obj.saved_state, saved)
                self.assertEqual(obj.allow_undefined, policy)
                np.testing.assert_array_equal(obj.get_sample_validity(), present)
                for name in before.dtype.names:
                    np.testing.assert_array_equal(obj.data[name], before[name])

    def test_detached_family_become_preflights_masks_and_unrelated_schemas(self):
        invalid = surface()
        invalid.data['surface_validity'][0, 0] = float('nan')
        first = m.Dot()
        before = first.data.copy()
        receiver = m.Group(first, invalid)
        source = m.Group(m.Dot().shift(m.RIGHT), surface())
        with self.assertRaises(ValueError):
            receiver.become(source)
        np.testing.assert_array_equal(first.data, before)

        # The optional presence lane must not relax unrelated record schemas.
        first = m.Surface(resolution=(3, 3))
        receiver = m.Group(first, m.Dot())
        source = m.Group(surface(), m.Mobject())
        before = first.data.copy()
        with self.assertRaisesRegex(RuntimeError, 'different schemas'):
            receiver.become(source)
        np.testing.assert_array_equal(first.data, before)
        self.assertNotIn('surface_validity', first.data.dtype.names)

    def test_failed_regeneration_is_atomic_and_policy_changes_are_detected(self):
        obj = surface()
        before = obj.data.copy()
        def mutate_policy(u, v):
            obj.allow_undefined = False
            return u, v, 2.
        obj.passed_uv_func = mutate_policy
        with self.assertRaisesRegex(RuntimeError, 'changed during regeneration'):
            obj.init_points()
        np.testing.assert_array_equal(obj.data, before)
        obj.passed_uv_func = punctured
        with self.assertRaises(ValueError):
            obj.init_points()
        np.testing.assert_array_equal(obj.data, before)
        obj.allow_undefined = True
        obj.init_points()
        np.testing.assert_array_equal(obj.data, before)

    def test_chart_helpers_preserve_holes_and_follow_regenerated_axes(self):
        axes = m.ThreeDAxes(x_range=(-2, 2), y_range=(-2, 2), z_range=(-2, 2))
        for helper, function in ((axes.get_parametric_surface, punctured),
                                 (axes.get_graph, lambda u, v: punctured(u, v)[2])):
            with self.subTest(helper=helper.__name__):
                with self.assertRaises(ValueError):
                    helper(function, **DOMAIN)
                obj = helper(function, allow_undefined=True, **DOMAIN)
                self.assertFalse(obj.get_sample_validity()[12])
                axes.shift(m.RIGHT)
                obj.init_points()
                np.testing.assert_allclose(obj.uv_to_point(1, 0), axes.c2p(1, 0, 0), atol=1e-6)
                obj.allow_undefined = False
                with self.assertRaises(ValueError):
                    obj.init_points()
        # Finite input overflowing a known affine chart is not a mathematical
        # pole, even with holes enabled.
        axes.stretch(2, 0)
        with np.errstate(over='ignore', invalid='ignore'), self.assertRaises(ValueError):
            axes.get_parametric_surface(lambda u, v: (1e308, 0., 0.),
                                        allow_undefined=True, **DOMAIN)
        if np.finfo(np.longdouble).max > np.finfo(np.float64).max:
            huge = np.finfo(np.longdouble).max
            for helper, function in (
                (axes.get_parametric_surface,
                 lambda u, v: np.array((huge, 0., np.nan), dtype=np.longdouble)),
                (axes.get_graph, lambda u, v: huge),
            ):
                with self.subTest(extended_finite=helper.__name__), self.assertRaises(ValueError):
                    helper(function, allow_undefined=True, **DOMAIN)

    def test_wireframe_and_texture_preserve_missing_support(self):
        obj = surface()
        mesh = m.SurfaceMesh(obj, resolution=(3, 3), normal_nudge=0)
        self.assertEqual(len(mesh), 6)
        # The central u and v lines each split at the absent sample.
        self.assertEqual(len(mesh[1].get_subpaths()), 2)
        self.assertEqual(len(mesh[4].get_subpaths()), 2)
        wires = tuple(mesh.submobjects)
        for function, subpaths in ((lambda u, v: (u, v, 0.), 1), (punctured, 2)):
            obj.passed_uv_func = function
            obj.init_points()
            mesh.init_points()
            self.assertEqual(tuple(mesh.submobjects), wires)
            self.assertEqual(len(mesh[1].get_subpaths()), subpaths)
            self.assertEqual(len(mesh[4].get_subpaths()), subpaths)
        with tempfile.TemporaryDirectory() as root:
            texture = m.TexturedSurface(obj, white_texture(Path(root) / 'white.png'))
            self.assertTrue(texture.allow_undefined)
            np.testing.assert_array_equal(texture.get_sample_validity(), obj.get_sample_validity())
            np.testing.assert_array_equal(texture.get_triangle_indices(), obj.get_triangle_indices())
            uv = texture.data['im_coords'].copy()
            obj.passed_uv_func = lambda u, v: (u, v, 0.)
            obj.init_points()
            texture.init_points()
            self.assertTrue(texture.get_sample_validity().all())
            np.testing.assert_array_equal(texture.data['im_coords'], uv)

    def test_partial_and_transform_lifecycles_restore_masks(self):
        original = surface()
        partial = original.copy()
        partial.pointwise_become_partial(original, 0., .4)
        self.assertTrue(np.isfinite(partial.get_points()).all())
        partial.pointwise_become_partial(original, 0., 1.)
        np.testing.assert_array_equal(partial.get_sample_validity(), original.get_sample_validity())
        complete = surface(lambda u, v: (u, v, 0.))
        animation = m.Transform(partial, complete, rate_func=m.linear)
        animation.begin()
        animation.interpolate(.5)
        self.assertFalse(partial.get_sample_validity()[12])
        animation.finish()
        self.assertTrue(partial.get_sample_validity().all())
        np.testing.assert_array_equal(partial.get_points(), complete.get_points())

    def test_invalid_mask_is_refused_in_queries_and_native_rendering(self):
        obj = surface()
        obj.data['surface_validity'][0, 0] = float('nan')
        with self.assertRaises(ValueError):
            obj.get_sample_validity()
        with self.assertRaises(ValueError):
            obj.get_triangle_indices()
        with tempfile.TemporaryDirectory() as root, self.assertRaises((RuntimeError, ValueError)):
            render(Path(root) / 'invalid.y4m', obj)

    def test_rendered_hole_matches_independent_triangle_and_worker_counts(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            image = white_texture(root / 'white.png')
            # The UV grid's first triangle touches its missing lower-left
            # vertex. Only [upper-left, lower-right, upper-right] survives.
            triangle = SimpleNamespace(
                vertices=np.array([[-2., 2., 0.], [2., -2., 0.], [2., 2., 0.]]),
                faces=np.array([[0, 1, 2]], dtype=np.uint32),
                visual=SimpleNamespace(uv=np.array([[0., 0.], [1., 1.], [1., 0.]])),
            )
            expected = render(root / 'triangle.y4m',
                              m.TexturedGeometry(triangle, image, shading=(0, 0, 0)))
            empty = render(root / 'empty.y4m', None)
            self.assertNotEqual(expected, empty)
            for threads in (1, 4, 16):
                grid = surface(lambda u, v: (u, v, float('inf') if u == -2 and v == -2 else 0.),
                               resolution=(2, 2))
                textured = m.TexturedSurface(grid, image, shading=(0, 0, 0))
                actual = render(root / f'hole-{threads}.y4m', textured, threads)
                self.assertEqual(actual, expected)
            absent = surface(lambda u, v: (u, v, float('nan')))
            self.assertEqual(render(root / 'absent.y4m', absent), empty)


def run_surface_holes():
    result = unittest.TextTestRunner(verbosity=2).run(
        unittest.defaultTestLoader.loadTestsFromTestCase(SurfaceHoleTests))
    if not result.wasSuccessful():
        raise AssertionError('Python surface-hole acceptance failed')
    return result.testsRun


if __name__ in ('__main__', '<run_path>'):
    run_surface_holes()
