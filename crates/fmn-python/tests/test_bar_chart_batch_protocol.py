"""Production bar-chart batch admission with explicit native storage doubles.

These host tests verify conversion, hook dispatch and publication admission.
The real native records, f32 geometry, views and pixels are exercised by
live_bar_chart.py through portal_bar_chart::tests::native_bar_chart_batch.
"""
from pathlib import Path
import sys
from types import MethodType, ModuleType
import unittest
from unittest.mock import patch

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'python'))
from fmn_python.bar_chart import install_bar_chart

UV = np.array(((1, 1), (.5, 1), (0, 1), (0, .5), (0, 0),
               (.5, 0), (1, 0), (1, .5), (1, 1)))


def fixture():
    native = ModuleType('bar_chart_storage_double')
    native._np, native._MED_LARGE_BUFF = np, .5
    native.resize_preserving_order = lambda data, length: data[:length]
    native.calls, native.refuse = [], False

    class Mobject:
        pointlike_data_keys = ['point']

        def __init__(self, points=()):
            self.records = np.asarray(points, dtype=np.float32).reshape(-1, 3).copy()
            self.parents = []
            self._data_defaults = np.ones((1, 3), dtype=np.float32)
            self.reads, self.writes, self.refreshes = 0, 0, 0
            self.needs_new_unit_normal = False

        def _is_bound(self):
            return False

        def get_points(self):
            self.reads += 1
            return self.records

        def set_points(self, points):
            self.resize_points(len(points))
            self.records[:] = points
            self.writes += 1
            self.needs_new_unit_normal = True
            return self

        def resize_points(self, length, **kwargs):
            if len(self.records) != length:
                self.records = np.zeros((length, 3), dtype=np.float32)
            self.refresh_bounding_box()
            return self

        def get_family(self, recurse=True):
            return [self]

        def refresh_bounding_box(self, recurse_down=False, recurse_up=True):
            self.refreshes += 1
            if recurse_up:
                for parent in self.parents:
                    parent.refresh_bounding_box()
            return self

        def center(self):
            return self

    class Rectangle(Mobject):
        def __init__(self, width=1., height=1.):
            points = np.column_stack(((UV[:, 0] - .5) * width,
                                      (UV[:, 1] - .5) * height,
                                      np.zeros(9)))
            super().__init__(points)

    class Line(Mobject):
        def get_start(self):
            return self.records[0].astype(float)

        def get_end(self):
            return self.records[-1].astype(float)

    class Chart(Mobject):
        def __init__(self, values, max_value=1., height=4., width=6., **kwargs):
            super().__init__()
            self.max_value = max_value
            self.y_axis = Line(((0., -.5, 0.), (0., height, 0.)))
            self.bars = [Rectangle(width / (2 * len(values)), height * v / max_value)
                         for v in values]
            for bar in self.bars:
                bar.parents = [self]

    native.Mobject, native.Rectangle, native.Line, native.BarChart = Mobject, Rectangle, Line, Chart

    def batch(axis, bars, values, parents, maximum, fraction):
        native.calls.append((axis, bars, values, parents, maximum, fraction))
        if native.refuse:
            return False
        height = (axis.records[-1].astype(float) - axis.records[0]) * fraction
        targets = []
        for bar, value in zip(bars, values):
            points = bar.records.astype(float)
            target = (points[4] + UV[:, :1] * (points[6] - points[4])
                      + UV[:, 1:] * (height * (value / maximum)))
            if not np.isfinite(target).all() or np.any(np.abs(target) > np.finfo(np.float32).max):
                return False
            targets.append(target)
        for bar, target in zip(bars, targets):
            bar.records[:] = target
        for parent in parents:
            parent.refreshes += 1
        return True

    native._change_bar_values = batch
    install_bar_chart(native)
    return native


class BarChartBatchProtocolTests(unittest.TestCase):
    def setUp(self):
        self.native = fixture()
        self.chart = self.native.BarChart((.5, .25, .75))

    def test_stock_prefix_freezes_numbers_and_batches_once_without_point_getters(self):
        untouched = self.chart.bars[-1].records.copy()
        seen = []

        def values():
            for value in (0., -.5):
                seen.append(value)
                yield value

        self.assertIsNone(self.chart.change_bar_values(values()))
        self.assertEqual(seen, [0., -.5])
        self.assertEqual(len(self.native.calls), 1)
        axis, bars, numbers, parents, maximum, fraction = self.native.calls[0]
        self.assertIs(axis, self.chart.y_axis)
        self.assertEqual(bars, tuple(self.chart.bars[:2]))
        self.assertEqual(numbers, (0., -.5))
        self.assertEqual(parents, (self.chart,))
        self.assertEqual((maximum, fraction), (1., 4 / 4.5))
        self.assertEqual([bar.reads for bar in self.chart.bars], [0, 0, 0])
        self.assertEqual([bar.writes for bar in self.chart.bars], [0, 0, 0])
        self.assertTrue(all(bar.needs_new_unit_normal for bar in bars))
        np.testing.assert_array_equal(self.chart.bars[-1].records, untouched)

    def test_untouched_prefix_suffix_is_not_admitted_or_read(self):
        untouched = self.chart.bars[-1]
        untouched.pointlike_data_keys = ['point', 'extra']
        untouched.records = np.zeros((4, 3))
        untouched.get_points = lambda: self.fail('untouched suffix getter ran')
        self.chart.change_bar_values((.75,))
        self.assertEqual(len(self.native.calls), 1)
        self.assertEqual(len(self.native.calls[0][1]), 1)

    def test_native_refusal_uses_existing_geometry_and_publication_protocol(self):
        self.native.refuse = True
        self.chart.change_bar_values((-.5,))
        bar = self.chart.bars[0]
        self.assertEqual((bar.reads, bar.writes), (1, 1))
        self.assertAlmostEqual(float(bar.records[0, 1] - bar.records[4, 1]), -2.)

    def test_authored_point_and_resize_hooks_select_fallback(self):
        for name in ('get_points', 'set_points', 'resize_points', 'refresh_bounding_box'):
            with self.subTest(hook=name):
                native = fixture()
                obj = native.BarChart((.5,))
                bar = obj.bars[0]
                original = getattr(bar, name)
                calls = []

                def hook(self, *args, **kwargs):
                    calls.append(name)
                    return original(*args, **kwargs)

                setattr(bar, name, MethodType(hook, bar))
                obj.change_bar_values((.75,))
                self.assertEqual(native.calls, [])
                self.assertTrue(calls)

    def test_authored_ancestor_hook_remains_observable(self):
        calls = []
        original = self.chart.refresh_bounding_box

        def refresh(self, *args, **kwargs):
            calls.append('parent')
            return original(*args, **kwargs)

        self.chart.refresh_bounding_box = MethodType(refresh, self.chart)
        self.chart.change_bar_values((.75,))
        self.assertEqual(self.native.calls, [])
        self.assertEqual(calls, ['parent'])

    def test_broad_parent_expansion_falls_back_before_native_or_partial_publication(self):
        owners = [self.native.Mobject() for _ in range(9)]
        self.chart.bars[0].parents = owners
        before = [bar.records.copy() for bar in self.chart.bars]
        with patch('fmn_python.bar_chart._MAX_PARENTS', 4):
            # Finite conversion succeeds; the later target exceeds f32. The
            # bounded admission must first decline the broad parent expansion,
            # and the incumbent all-target validation then refuses atomically.
            with self.assertRaisesRegex(ValueError, 'f32-representable'):
                self.chart.change_bar_values((.75, 1e100))
        self.assertEqual(self.native.calls, [])
        self.assertTrue(all(owner.refreshes == 0 for owner in owners))
        self.assertTrue(all(bar.writes == 0 for bar in self.chart.bars))
        for bar, old in zip(self.chart.bars, before):
            np.testing.assert_array_equal(bar.records, old)

    def test_axis_descriptor_preserves_two_reads_and_getter_order(self):
        native, calls = self.native, []

        class Authored(native.BarChart):
            @property
            def y_axis(self):
                calls.append('axis')
                return self._axis

            @y_axis.setter
            def y_axis(self, value):
                self._axis = value

        obj = Authored((.5,))
        calls.clear()
        obj.change_bar_values((.75,))
        self.assertEqual(native.calls, [])
        self.assertEqual(calls, ['axis', 'axis'])

    def test_pointlike_iterables_do_not_execute_during_admission(self):
        calls = []
        axis = self.chart.y_axis
        original = axis.get_end

        def end(self):
            calls.append('axis')
            return original()

        axis.get_end = MethodType(end, axis)

        class Keys:
            def __iter__(self):
                calls.append('keys')
                return iter(('point',))

        self.chart.bars[0].pointlike_data_keys = Keys()
        self.chart.change_bar_values((.75,))
        self.assertEqual(self.native.calls, [])
        self.assertEqual(calls, ['axis', 'keys'])

    def test_empty_prefix_retains_axis_validation_without_native_call(self):
        calls = []
        axis = self.chart.y_axis
        original = axis.get_end
        axis.get_end = lambda: (calls.append('end'), original())[1]
        self.assertIsNone(self.chart.change_bar_values(()))
        self.assertEqual(self.native.calls, [])
        self.assertEqual(calls, ['end'])

    def test_conversion_failure_or_reentry_cannot_reach_native_publication(self):
        original = [bar.records.copy() for bar in self.chart.bars]
        error = LookupError('authored conversion')

        class Bad:
            def __float__(self):
                raise error

        with self.assertRaises(LookupError) as caught:
            self.chart.change_bar_values((.75, Bad()))
        self.assertIs(caught.exception, error)
        obj = self.chart

        class Reentrant:
            def __float__(self):
                obj.change_bar_values((0.,))
                return .5

        with self.assertRaisesRegex(RuntimeError, 'reenter'):
            obj.change_bar_values((Reentrant(),))
        self.assertEqual(self.native.calls, [])
        for bar, before in zip(obj.bars, original):
            np.testing.assert_array_equal(bar.records, before)
        obj.change_bar_values((.75,))
        self.assertEqual(len(self.native.calls), 1)

    def test_class_monkeypatch_after_success_invalidates_batch_admission(self):
        self.chart.change_bar_values((.75,))
        original = self.native.Rectangle.get_points
        calls = []

        def authored(self):
            calls.append('read')
            return original(self)

        self.native.Rectangle.get_points = authored
        self.chart.change_bar_values((.5,))
        self.assertEqual(len(self.native.calls), 1)
        self.assertEqual(calls, ['read'])


if __name__ == '__main__':
    unittest.main()
