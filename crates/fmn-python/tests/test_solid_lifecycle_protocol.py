"""Exercise real lifecycle adapters with explicit in-memory native seam doubles.

These tests prove dispatch, ownership checks, and error handling, not native
geometry or rendering. native_solid_lifecycle.py supplies that acceptance suite.
"""
from pathlib import Path
import sys
from types import SimpleNamespace
import unittest

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'python'))
from fmn_python.surface_lifecycle import install_surface_lifecycle
from fmn_python.solid_lifecycle import install_solid_lifecycle


def portal():
    calls = []
    dtype = [('point', np.float32, (3,)), ('d_normal_point', np.float32, (3,)),
             ('rgba', np.float32, (4,))]

    def install_live(self):
        self.submobjects = []
        self.locked_data_keys = set()
        self.color, self.opacity = 'white', 1.

    class Mobject:
        data_dtype = dtype
        def __init__(self, **kwargs):
            install_live(self)
            vars(self).update(kwargs)
            self.data = np.zeros(0, dtype=self.data_dtype)
            self.init_data()
            self.init_points()
            self.init_uniforms()
        def _is_bound(self):
            return vars(self).get('bound', False)
        def init_data(self):
            pass
        def init_uniforms(self):
            pass
        def init_colors(self):
            self.set_color(self.color)
        def set_color(self, value):
            self.color = value
        def get_family(self):
            return [self, *self.submobjects]
        def get_points(self):
            return self.data['point']
        def get_height(self):
            return float(np.ptp(self.get_points()[:, 1])) if len(self.data) else 0.
        def set_z_index(self, value):
            self.z_index = value
        def set_shading(self, *values):
            self.shading = values
        def apply_depth_test(self):
            self.depth_test = True
        def deactivate_depth_test(self):
            self.depth_test = False
        def set_points(self, points):
            records = np.zeros(len(points), dtype=self.data_dtype)
            records['point'] = points
            records['d_normal_point'] = np.asarray(points) + (0, 0, .001)
            self.data = records
        def shift(self, delta):
            for name in ('point', 'd_normal_point'):
                self.data[name] += delta
        def _build_parametric_surface(self, factory, function, ur, vr, shape, epsilon, nudge):
            calls.append('generic')
            self.set_points([function(u, v) for u in np.linspace(*ur, shape[0])
                             for v in np.linspace(*vr, shape[1])])
            return []
        def _build_torus(self, factory, r1, r2, ur, vr, shape, axis, epsilon, nudge, z):
            calls.append(('torus', r1, r2))
            # Deliberately recognizable fixtures, NOT a replacement UV kernel.
            self.set_points([(r1, r2, k) for k in range(shape[0]*shape[1])])
            return []

    class Surface(Mobject):
        def uv_func(self, u, v):
            return u, v, 0.
        def init_points(self):
            calls.append('regenerate')
        def compute_triangle_indices(self):
            self.triangles_computed = True

    class ParametricSurface(Surface):
        def uv_func(self, u, v):
            return self.passed_uv_func(u, v)

    class Torus(Surface):
        def uv_func(self, u, v):
            raise AssertionError('stock Torus must select its specialized native builder')

    def publish(self, shape, candidate=None):
        count = shape[0]*shape[1]
        if candidate is not None:
            records = np.zeros(count, dtype=self.data_dtype)
            keep = min(len(self.data), count)
            records[:keep] = self.data[:keep]
            for key in ('point', 'd_normal_point'):
                records[key] = candidate.data[key]
            self.data = records
        if len(self.data) != count:
            raise ValueError('record count must match resolution')
        self.native_shape = shape

    def refuse(where, flags):
        if flags:
            raise TypeError(where + ': unexpected keyword')

    native = SimpleNamespace(Mobject=Mobject, Surface=Surface,
        ParametricSurface=ParametricSurface, Torus=Torus, GREY='grey',
        _refuse_unrouted=refuse, _install_live_state=install_live,
        _initialize_surface_grid=publish, _native_surface_shell_factory=None)
    install_surface_lifecycle(native)
    install_solid_lifecycle(native)
    return native, calls


class SolidLifecycleProtocol(unittest.TestCase):
    def test_hooks_are_cooperative_once_and_native_sampling_is_selected(self):
        m, calls = portal()
        events = []
        class Hooks:
            def init_data(self):
                events.append(('data', self.r1, self.r2, self.resolution))
                super().init_data()
            def init_points(self):
                events.append('points')
                super().init_points()
                self.shift((1, 0, 0))
            def init_uniforms(self):
                events.append('uniforms')
                super().init_uniforms()
                self.set_shading(.2, .3, .4)
            def init_colors(self):
                events.append('colors')
                super().init_colors()
                self.set_color('blue')
        class Authored(Hooks, m.Torus):
            pass
        surface = Authored(r1=2, r2=.5, resolution=(2, 3), color='red', z_index=7)
        self.assertEqual(events, [('data', 2., .5, (2, 3)), 'points', 'uniforms', 'colors'])
        self.assertEqual(calls, [('torus', 2., .5)])
        self.assertEqual(surface.color, 'blue')
        self.assertEqual(surface.shading, (.2, .3, .4))
        self.assertEqual(surface.z_index, 7)
        self.assertEqual(surface.native_shape, (2, 3))
        np.testing.assert_array_equal(surface.get_points()[:, 0], 3.)

    def test_authored_uv_is_evaluated_not_replaced_by_native_shape(self):
        m, calls = portal()
        visits = []
        class Authored(m.Torus):
            def uv_func(self, u, v):
                visits.append((u, v))
                return u, v, self.r1 - self.r2
        surface = Authored(r1=2, r2=.5, resolution=(3, 4))
        self.assertEqual(len(visits), 12)
        self.assertEqual(calls, ['generic'])
        np.testing.assert_array_equal(surface.get_points()[:, 2], 1.5)

    def test_replacement_point_hook_never_samples_uv(self):
        m, calls = portal()
        class Authored(m.Torus):
            def init_points(self):
                self.set_points([(1, 2, 3)] * 4)
        surface = Authored(resolution=(2, 2))
        self.assertEqual(calls, [])
        self.assertEqual(surface.native_shape, (2, 2))
        np.testing.assert_array_equal(surface.get_points(), [(1, 2, 3)] * 4)

    def test_initialization_preserves_authored_schema_and_child_identity(self):
        m, _ = portal()
        child = object()
        class Authored(m.Torus):
            data_dtype = [*m.Surface.data_dtype, ('tag', np.float32, (1,))]
            def init_data(self):
                super().init_data()
                self.set_points([(0, 0, 0)] * 4)
                self.data['tag'] = 23
                self.submobjects.append(child)
        surface = Authored(resolution=(2, 2))
        self.assertIs(surface.submobjects[0], child)
        np.testing.assert_array_equal(surface.data['tag'], 23)

    def test_shape_recipe_mutation_is_not_published(self):
        m, _ = portal()
        roots = []
        class Authored(m.Torus):
            def init_data(self):
                roots.append(self)
            def uv_func(self, u, v):
                self.r1 += 1
                return u, v, 0
        with self.assertRaisesRegex(RuntimeError, 'shape parameters changed'):
            Authored(resolution=(2, 2))
        self.assertEqual(len(roots[0].data), 0)

    def test_callback_exception_identity_and_next_constructor_remains_usable(self):
        m, _ = portal()
        error = RuntimeError('authored failure')
        class Authored(m.Torus):
            def uv_func(self, u, v):
                raise error
        with self.assertRaises(RuntimeError) as caught:
            Authored(resolution=(2, 2))
        self.assertIs(caught.exception, error)
        self.assertEqual(m.Torus(resolution=(2, 2)).native_shape, (2, 2))

    def test_bound_and_recursive_construction_refuse_before_recipe_changes(self):
        m, _ = portal()
        surface = m.Torus(r1=4, resolution=(2, 2))
        surface.bound = True
        with self.assertRaisesRegex(RuntimeError, 'detached'):
            m.Torus.__init__(surface, r1=99)
        self.assertEqual(surface.r1, 4)
        class Recursive(m.Torus):
            def init_data(self):
                with self_test.assertRaisesRegex(RuntimeError, 'already in progress'):
                    m.Torus.__init__(self, r1=99)
                self_test.assertEqual(self.r1, 4)
        self_test = self
        Recursive(r1=4, resolution=(2, 2))

    def test_admission_rejects_bad_dimensions_and_resolution_before_hooks(self):
        m, _ = portal()
        events = []
        class Authored(m.Torus):
            def init_data(self):
                events.append('data')
        for kwargs in ({'r1': float('nan')}, {'r2': float('inf')}, {'r1': 1e40},
                       {'resolution': (2.5, 2)}, {'resolution': (1000, 1000)},
                       {'unsupported': 1}):
            with self.assertRaises((TypeError, ValueError)):
                Authored(**kwargs)
        self.assertEqual(events, [])

    def test_class_monkey_patch_is_not_mistaken_for_the_stock_uv(self):
        m, calls = portal()
        m.Torus.uv_func = lambda self, u, v: (u, v, 9)
        surface = m.Torus(resolution=(2, 2))
        self.assertEqual(calls, ['generic'])
        np.testing.assert_array_equal(surface.get_points()[:, 2], 9)

    def test_installer_is_idempotent_and_plain_surface_keeps_its_sampler(self):
        m, calls = portal()
        constructor = m.Torus.__init__
        install_solid_lifecycle(m)
        self.assertIs(m.Torus.__init__, constructor)
        surface = m.Surface(resolution=(2, 2))
        self.assertEqual(calls, ['generic'])
        surface.init_points()
        self.assertEqual(calls[-1], 'regenerate')


if __name__ == '__main__':
    unittest.main()
