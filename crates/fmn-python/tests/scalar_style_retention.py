"""Public style writes retain NumPy aliases and authored accessor behavior."""

import types
import unittest

import numpy as np
import manimlib as m


class ScalarStyleRetentionTests(unittest.TestCase):
    def test_live_column_aliases_observe_scalar_writes_without_point_changes(self):
        mob = m.Square().shift((0.5, -0.25, 0))
        data = mob.data
        points = data["point"].copy()
        widths = data["stroke_width"]
        borders = data["fill_border_width"]
        for value in (0, 2.5, np.float16(1.25), np.float32(3.5), np.float64(4.75)):
            self.assertIs(mob.set_stroke(width=value), mob)
            self.assertIs(mob.set_fill(border_width=value), mob)
            np.testing.assert_array_equal(widths, np.full_like(widths, value))
            np.testing.assert_array_equal(borders, np.full_like(borders, value))
            np.testing.assert_array_equal(data["point"], points)

    def test_bound_style_writes_preserve_public_proxy_and_live_aliases(self):
        scene = m.Scene()
        mob = m.Square()
        scene.add(mob)
        data = mob.data
        points = data["point"].copy()
        mob.set_stroke(m.RED, width=3.25, opacity=0.6)
        mob.set_fill(m.BLUE, opacity=0.4, border_width=1.5)
        np.testing.assert_array_equal(data["stroke_width"], np.float32(3.25))
        np.testing.assert_array_equal(data["fill_border_width"], np.float32(1.5))
        np.testing.assert_array_equal(data["point"], points)
        np.testing.assert_allclose(data["stroke_rgba"][:, 3], 0.6)
        np.testing.assert_allclose(data["fill_rgba"][:, 3], 0.4)
        self.assertIs(scene.mobjects[0], mob)

    def test_empty_record_defaults_apply_when_points_are_created(self):
        mob = m.VMobject().set_stroke(width=3.25).set_fill(border_width=1.5)
        self.assertEqual(mob.get_num_points(), 0)
        mob.set_points([[0, 0, 0], [0.5, 0, 0], [1, 0, 0]])
        np.testing.assert_array_equal(mob.data["stroke_width"], np.float32(3.25))
        np.testing.assert_array_equal(mob.data["fill_border_width"], np.float32(1.5))

    def test_width_profiles_keep_numpy_interpolation(self):
        mob = m.Square()
        mob.set_stroke(width=[1, 3, 2])
        observed = mob.data["stroke_width"][:, 0]
        expected = np.interp(np.linspace(0, 2, len(observed)), np.arange(3), [1, 3, 2])
        np.testing.assert_allclose(observed, expected, atol=1e-6)

    def test_subclass_style_accessor_is_called_for_scalar_and_rgba_writes(self):
        class Authored(m.Square):
            def _style_data(self):
                self.style_reads = self.__dict__.get("style_reads", 0) + 1
                return super()._style_data()

        mob = Authored()
        for operation in (
            lambda: mob.set_stroke(width=5),
            lambda: mob.set_fill(border_width=2),
            lambda: mob.set_fill(m.RED, opacity=0.3),
        ):
            before = mob.style_reads
            operation()
            self.assertGreater(mob.style_reads, before)

    def test_instance_style_accessor_is_called_for_scalar_and_rgba_writes(self):
        mob = m.Square()
        reads = []
        original = mob._style_data

        def authored():
            reads.append("style")
            return original()

        mob._style_data = authored
        for operation in (
            lambda: mob.set_stroke(width=5),
            lambda: mob.set_fill(border_width=2),
            lambda: mob.set_fill(m.RED, opacity=0.3),
        ):
            before = len(reads)
            operation()
            self.assertGreater(len(reads), before)

    def test_subclass_data_property_is_observed(self):
        class Authored(m.Square):
            @property
            def data(self):
                self.data_reads = self.__dict__.get("data_reads", 0) + 1
                return super().data

        mob = Authored()
        before = mob.data_reads
        mob.set_stroke(width=5).set_fill(m.RED, border_width=2)
        self.assertGreaterEqual(mob.data_reads - before, 3)

    def test_instance_record_export_override_is_observed(self):
        mob = m.Square()
        reads = []
        original = mob._data_array

        def authored(*args, **kwargs):
            reads.append("export")
            return original(*args, **kwargs)

        mob._data_array = authored
        mob.set_stroke(width=5).set_fill(m.RED, border_width=2)
        self.assertGreaterEqual(len(reads), 3)

    def test_instance_public_setter_override_is_not_bypassed(self):
        mob = m.Square()
        calls = []
        original = mob.set_stroke

        def authored(self, *args, **kwargs):
            calls.append(kwargs["width"])
            return original(*args, **kwargs)

        mob.set_stroke = types.MethodType(authored, mob)
        mob.set_stroke(width=5)
        self.assertEqual(calls, [5])
        np.testing.assert_array_equal(mob.data["stroke_width"], np.float32(5))

    def test_authored_structured_scalar_columns_keep_numpy_assignment(self):
        class WideColumns(m.Square):
            data_dtype = [
                (name, 2 if name in ("stroke_width", "fill_border_width") else width)
                for name, width in m.VMobject.data_dtype
            ]

        mob = WideColumns()
        previous_second_lane = mob.data["stroke_width"][:, 1].copy()
        mob.set_stroke(width=5).set_fill(border_width=2)
        np.testing.assert_array_equal(mob.data["stroke_width"][:, 0], np.float32(5))
        np.testing.assert_array_equal(mob.data["stroke_width"][:, 1], previous_second_lane)
        np.testing.assert_array_equal(mob.data["fill_border_width"], np.float32(2))

    def test_later_base_accessor_patch_is_observed(self):
        mob = m.Square()
        original = m.Mobject._style_data
        calls = []

        def authored(owner):
            calls.append(owner)
            return original(owner)

        try:
            m.Mobject._style_data = authored
            mob.set_stroke(width=5).set_fill(m.RED, border_width=2)
        finally:
            m.Mobject._style_data = original
        self.assertGreaterEqual(len(calls), 3)
        self.assertTrue(all(owner is mob for owner in calls))

    def test_authored_numeric_conversion_stays_on_numpy_path(self):
        calls = []

        class AuthoredFloat(float):
            def __float__(self):
                calls.append("float")
                return 7.0

        mob = m.Square()
        value = AuthoredFloat(2)
        expected = np.zeros_like(mob.data["stroke_width"])
        expected[:, 0] = value
        expected_calls = list(calls)
        calls.clear()
        mob.set_stroke(width=value)
        self.assertEqual(calls, expected_calls)
        np.testing.assert_array_equal(mob.data["stroke_width"], expected)


_suite = unittest.defaultTestLoader.loadTestsFromTestCase(ScalarStyleRetentionTests)
_result = unittest.TextTestRunner(verbosity=2).run(_suite)
if not _result.wasSuccessful():
    raise AssertionError("scalar style retention regressions failed")
scalar_style_cases = _result.testsRun
