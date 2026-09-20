"""Native point editing across empty paths, field views and animation families."""
import gc
import unittest

import numpy as np
import manimlib as m


class PointEditingTests(unittest.TestCase):
    def test_empty_point_tables_clear_geometry_without_changing_identity(self):
        for empty in ([], (), np.empty((0,)), np.empty((0, 3))):
            with self.subTest(empty=repr(empty)):
                mob = m.Square(fill_color=m.RED, fill_opacity=.5)
                scene = m.Scene()
                scene.add(mob)
                old = mob.get_points()
                old_values = old.copy()
                self.assertIs(mob.set_points(empty), mob)
                self.assertEqual(mob.get_points().shape, (0, 3))
                self.assertEqual(mob.get_subpaths(), [])
                self.assertIn(mob, scene.mobjects)
                np.testing.assert_array_equal(old, old_values)
                mob.set_points(old_values)
                np.testing.assert_array_equal(mob.get_points(), old_values)
                self.assertAlmostEqual(float(mob.get_fill_opacity()), .5)
                self.assertEqual(mob.get_fill_color(), m.RED)

    def test_malformed_points_do_not_resize_or_change_native_records(self):
        bad = ([1., 2., 3.], [[1., 2.]] * 3, np.empty((0, 2)),
               np.zeros((1, 1, 3)), [[np.nan, 0, 0]], [[np.inf, 0, 0]],
               [[1e300, 0, 0]], [[1j, 0, 0]], [["not a coordinate", 0, 0]])
        for points in bad:
            for operation in ("set_points", "append_points"):
                # VMobject parity checks are deliberately still authoritative.
                mob = m.Mobject().set_points([[0, 0, 0], [1, 0, 0]])
                before, view = mob.data.copy(), mob.get_points()
                with self.subTest(points=repr(points), operation=operation):
                    with self.assertRaises((ValueError, TypeError)):
                        getattr(mob, operation)(points)
                    np.testing.assert_array_equal(mob.data, before)
                    view[0] = [3, 2, 1]
                    np.testing.assert_array_equal(mob.get_points()[0], [3, 2, 1])

    def test_empty_append_is_a_noop_and_retains_live_views(self):
        for mob in (m.Mobject(), m.Square()):
            before = mob.data.copy()
            view = mob.get_points()
            self.assertIs(mob.append_points([]), mob)
            np.testing.assert_array_equal(mob.data, before)
            if len(view):
                view[0] += 1
                np.testing.assert_array_equal(mob.get_points()[0], view[0])

    def test_first_append_uses_retained_style_defaults(self):
        mob = m.Square(fill_color=m.RED, fill_opacity=.25, stroke_width=7)
        saved = mob.data[0].copy()
        mob.clear_points()
        m.Mobject.append_points(mob, [[0, 0, 0], [1, 0, 0], [2, 0, 0]])
        for field in ("fill_rgba", "stroke_rgba", "stroke_width"):
            np.testing.assert_array_equal(mob.data[field], np.repeat(saved[field][None], 3, axis=0))

    def test_append_inherits_last_record_and_preserves_detached_old_view(self):
        mob = m.Square()
        mob.data["stroke_width"][-1] = 9
        old = mob.get_points()
        expected = old.copy()
        tail = old[:2][::-1]
        self.assertIs(mob.append_points(tail), mob)
        np.testing.assert_array_equal(mob.get_points(), np.vstack([expected, expected[:2][::-1]]))
        np.testing.assert_array_equal(old, expected)
        np.testing.assert_array_equal(mob.data["stroke_width"][-2:], [[9], [9]])
        old[:] = -42
        self.assertFalse(np.any(mob.get_points() == -42))

    def test_reversed_and_strided_aliases_survive_set_and_resize(self):
        for step in (-1, 2):
            mob = m.Square()
            expected = mob.get_points()[::step].copy()
            mob.set_points(mob.get_points()[::step])
            np.testing.assert_array_equal(mob.get_points(), expected)

    def test_authored_resize_hook_cannot_change_the_frozen_input(self):
        class Authored(m.Mobject):
            def resize_points(self, length, **kwargs):
                if hasattr(self, "source"):
                    self.source[:] = -99
                return super().resize_points(length, **kwargs)
        mob = Authored()
        values = np.array([[1., 2., 3.], [4., 5., 6.]])
        expected = values.copy()
        mob.source = values
        mob.set_points(values)
        np.testing.assert_array_equal(mob.get_points(), expected)
        np.testing.assert_array_equal(values, np.full((2, 3), -99))

    def test_vmobject_keeps_count_validation_and_parent_bounds(self):
        mob = m.Square()
        parent = m.VGroup(mob)
        old = mob.get_points().copy()
        with self.assertRaises(AssertionError):
            mob.set_points(np.zeros((2, 3)))
        np.testing.assert_array_equal(mob.get_points(), old)
        mob.set_points([[2, 0, 0], [3, 0, 0], [4, 0, 0]])
        np.testing.assert_allclose(parent.get_center(), [3, 0, 0])
        mob.set_points([])
        self.assertEqual(parent.family_members_with_points(), [])

    def test_grouped_authored_transform_aligns_empty_container_paths(self):
        class Authored(m.Transform):
            def interpolate_submobject(self, current, start, target, alpha):
                return super().interpolate_submobject(current, start, target, alpha)
        scene = m.Scene()
        source = m.VGroup(m.Square(), m.VGroup(m.Circle().shift(m.RIGHT)))
        target = source.copy().shift(2 * m.RIGHT)
        before = source.get_center().copy()
        scene.add(source)
        scene.play(Authored(source, target, rate_func=m.linear), run_time=.125)
        np.testing.assert_allclose(source.get_center(), before + 2 * m.RIGHT, atol=1e-6)
        self.assertEqual(source.get_points().shape, (0, 3))
        self.assertFalse(source._is_updating_suspended())


_suite = unittest.defaultTestLoader.loadTestsFromTestCase(PointEditingTests)
assert _suite.countTestCases() == 9, "native point-editing inventory changed"
_result = unittest.TextTestRunner(verbosity=2).run(_suite)
gc.collect()
if not _result.wasSuccessful():
    raise AssertionError("native point-editing acceptance failed")
