"""Native vector-field construction, authored hooks, records and real frames."""
from pathlib import Path
import copy
import tempfile
import unittest
from unittest.mock import patch

import manimlib as m
import numpy as np


def axes():
    return m.Axes(x_range=(-2., 2., 1.), y_range=(-2., 2., 1.), width=4., height=4.)


def horizontal(rows):
    return np.tile([1., 0.], (len(rows), 1))


def make(cls=m.VectorField, function=horizontal, chart=None, **options):
    defaults = dict(sample_coords=[[-1., 0.], [1., 0.]], max_vect_len=1., color=m.WHITE)
    defaults.update(options)
    return cls(function, axes() if chart is None else chart, **defaults)


class VectorFieldLifecycleTests(unittest.TestCase):
    def test_world_grid_and_arrow_endpoints_use_scene_coordinates(self):
        field = m.VectorField(lambda rows: np.tile([0., 0., 3.], (len(rows), 1)),
                              center=(1., 2., 3.), width=2., height=2., depth=0.,
                              x_density=1., y_density=1., max_vect_len=np.inf)
        bases = np.array([[x, y, 3.] for x in (0., 1., 2.) for y in (1., 2., 3.)])
        np.testing.assert_array_equal(field.get_points()[0::8], bases)
        np.testing.assert_array_equal(field.get_points()[6::8], bases + [0., 0., 3.])
        self.assertEqual(field.stroke_width, 2.)
        np.testing.assert_allclose(field.data["stroke_rgba"][0, :3], m.color_to_rgb(m.BLUE))

    def test_world_field_uses_authored_samples_and_observes_later_replacement(self):
        calls = []
        class SampledWave(m.VectorField):
            def get_sample_points(self, *args):
                calls.append("grid")
                return [[1., -1., 0.], [2., 1., 0.]]
        field = SampledWave(lambda points: np.tile([0., 1., 0.], (len(points), 1)),
                            stroke_color=m.RED, max_vect_len=np.inf)
        self.assertEqual(calls, ["grid"])
        field.sample_points = np.array([[0., 0., -1.], [0., 0., 1.], [1., 2., 3.]])
        field.update_vectors()
        np.testing.assert_array_equal(field.get_points()[0::8], field.sample_points)
        np.testing.assert_array_equal(field.get_points()[6::8], field.sample_points + m.UP)
        self.assertEqual(calls, ["grid"])
        clone = field.copy()
        clone.set_sample_points([[4., 5., 6.], [7., 8., 9.]]).update_vectors()
        self.assertEqual((field.get_num_points(), clone.get_num_points()), (23, 15))

    def test_world_time_callbacks_follow_each_copy_and_raw_magnitudes_reach_paint(self):
        samples = np.array([[1., 0., 0.], [2., 0., 0.]])
        field = m.TimeVaryingVectorField(lambda points, t: points * t, sample_points=samples,
                                         max_vect_len=np.inf, min_drawn_norm=.3,
                                         norm_to_rgb_func=lambda norms: np.stack(
                                             [norms, np.zeros_like(norms), np.zeros_like(norms)], axis=1))
        clone = field.copy()
        clone.update(.25)
        self.assertEqual((field.time, clone.time), (0., .25))
        np.testing.assert_array_equal(field.get_points()[6::8], samples)
        np.testing.assert_array_equal(clone.get_points()[6::8], [[1., 0., 0.], [2.5, 0., 0.]])
        np.testing.assert_array_equal(clone.data["stroke_rgba"][0::8, 0], [.25, .5])

    def test_world_field_refuses_invalid_inputs_and_keeps_live_geometry(self):
        for options in (dict(width=-1), dict(x_density=0), dict(sample_points=[[0., 1.], [2., 3.]]),
                        dict(sample_coords=[[0., 0.], [1., 0.]]), dict(norm_to_rgb_func=5)):
            with self.subTest(options=options), self.assertRaises((ValueError, TypeError)):
                m.VectorField(lambda points: points, **options)
        field = m.VectorField(lambda points: points, sample_points=[[0, 0, 0], [1, 2, 3]])
        before = field.data.copy()
        field.func = lambda points: points[:, :2]
        with self.assertRaisesRegex(ValueError, "three components"):
            field.update_vectors()
        np.testing.assert_array_equal(field.data, before)

    def test_world_wave_frames_match_chart_based_field(self):
        def render(path, world, threads):
            options = dict(color=m.WHITE, max_vect_len=np.inf, stroke_width=8.)
            def wave(rows, time):
                result = np.zeros_like(rows)
                result[:, 0] = .5
                result[:, 1] = time * (rows[:, 0] + 2.)
                return result
            if world:
                field = m.TimeVaryingVectorField(wave, sample_points=[[-1, 0, 0], [1, 0, 0]],
                                                 **options)
            else:
                field = m.TimeVaryingVectorField(wave, axes(), sample_coords=[[-1, 0], [1, 0]],
                                                 **options)
            scene = m.Scene()
            with scene.render_session(path, format="png_sequence", resolution=(160, 90), fps=4,
                                      threads=threads) as output:
                scene.add(field)
                scene.wait(.75)
            self.assertEqual(output.result.frame_count, 3)
            return [p.read_bytes() for p in sorted(path.glob("*.png"))]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            expected = render(root/"chart", False, 1)
            self.assertEqual(len(set(expected)), 3)
            for threads in (1, 4):
                self.assertEqual(render(root/f"world-{threads}", True, threads), expected)

    def test_preparation_and_all_base_hooks_run_before_public_update(self):
        events = []
        class Authored(m.VectorField):
            data_dtype = m.VectorField.data_dtype + [('mass', 1)]
            def update_sample_points(self):
                events.append('samples')
                return super().update_sample_points()
            def init_base_stroke_width_array(self, count):
                events.append(('widths', count))
                return super().init_base_stroke_width_array(count)
            def init_data(self):
                events.append('data')
                self.asserted_recipe = (self.stroke_width, self.magnitude_range)
                super().init_data()
            def init_points(self):
                events.append('points')
                super().init_points()
                self.data['mass'][:] = 7
                self.mass_view = self.data['mass']
                self.marker = m.Dot().shift(2 * m.UP)
                self.add(self.marker)
            def init_uniforms(self):
                events.append('uniforms')
                super().init_uniforms()
            def init_colors(self):
                events.append('colors')
                super().init_colors()
            def update_vectors(self):
                events.append('vectors')
                return super().update_vectors()
        field = make(Authored, magnitude_range=(0, 2), stroke_width=5)
        self.assertEqual(events, ['samples', ('widths', 2), 'data', 'points',
                                 'uniforms', 'colors', 'vectors', 'samples', ('widths', 2)])
        self.assertEqual(field.asserted_recipe, (5., (0., 2.)))
        self.assertIs(field[0], field.marker)
        np.testing.assert_array_equal(field.data['mass'], 7.)
        field.mass_view[:] = 9
        np.testing.assert_array_equal(field.data['mass'], 9.)
        self.assertEqual(field.get_joint_type(), 0)

    def test_final_update_override_controls_actual_displayed_geometry(self):
        class Lifted(m.VectorField):
            def update_vectors(self):
                super().update_vectors()
                self.shift(m.UP)
                return self
        actual, expected = make(Lifted), make().shift(m.UP)
        np.testing.assert_array_equal(actual.get_points(), expected.get_points())
        actual.func = lambda rows: np.tile([0., 1.], (len(rows), 1))
        actual.update_vectors()
        expected = make(function=actual.func).shift(m.UP)
        np.testing.assert_array_equal(actual.get_points(), expected.get_points())

    def test_complete_update_override_is_not_replaced_by_native_builder(self):
        calls = []
        class Authored(m.VectorField):
            def update_vectors(self):
                calls.append(self)
                self.set_points([[0., 0., 0.], [.5, .5, 0.], [1., 0., 0.]])
                return self
        field = make(Authored, magnitude_range=(0, 1), function=lambda rows: self.fail('bypassed override'))
        self.assertEqual(calls, [field])
        self.assertEqual(field.get_num_points(), 3)
        np.testing.assert_array_equal(field.get_points()[1], [.5, .5, 0.])

    def test_preparation_and_final_sampling_observe_hook_changes(self):
        phases = []
        class Authored(m.VectorField):
            def init_colors(self):
                super().init_colors()
                self.func = lambda rows: phases.append('final') or np.tile([0., 1.], (len(rows), 1))
        field = make(Authored, function=lambda rows: phases.append('range') or horizontal(rows))
        self.assertEqual(phases, ['range', 'final'])
        self.assertEqual(field.magnitude_range, (0., 1.))
        np.testing.assert_allclose(field.get_points()[6::8, 0], [-1., 1.])
        self.assertTrue(np.all(field.get_points()[6::8, 1] > 0))

    def test_explicit_range_needs_only_the_final_callback(self):
        calls = []
        class Authored(m.VectorField):
            pass
        make(Authored, magnitude_range=(0, 1), function=lambda rows: calls.append(rows.copy()) or horizontal(rows))
        self.assertEqual(len(calls), 1)
        calls.clear()
        make(function=lambda rows: calls.append(rows.copy()) or horizontal(rows))
        self.assertEqual(len(calls), 1, 'unchanged concrete construction keeps its established callback count')

    def test_authored_sample_point_mapping_is_used_by_native_builder(self):
        class Authored(m.VectorField):
            def update_sample_points(self):
                super().update_sample_points()
                self.sample_points += m.UP
        field = make(Authored)
        np.testing.assert_array_equal(field.get_points(), make().shift(m.UP).get_points())

    def test_late_concrete_class_hook_patch_cannot_take_the_native_shortcut(self):
        calls = []
        original = m.VectorField.init_points
        def points(field):
            calls.append(field)
            return original(field)
        with patch.object(m.VectorField, 'init_points', points):
            field = make()
        self.assertEqual(calls, [field])

    def test_constructor_exception_stops_later_hooks_and_guard_recovers(self):
        events, failure = [], RuntimeError('authored points')
        class Authored(m.VectorField):
            def init_points(self):
                events.append('points')
                if len(events) == 1:
                    raise failure
                super().init_points()
            def init_colors(self):
                events.append('colors')
                super().init_colors()
            def update_vectors(self):
                events.append('vectors')
                return super().update_vectors()
        obj = Authored.__new__(Authored)
        chart = axes()
        with self.assertRaises(RuntimeError) as caught:
            obj.__init__(horizontal, chart, sample_coords=[[-1, 0], [1, 0]], max_vect_len=1)
        self.assertIs(caught.exception, failure)
        self.assertEqual(events, ['points'])
        # Native allocation is one-shot: do not retry a partially initialized
        # record object. The guard must be released, not leak that receiver.
        with self.assertRaisesRegex(RuntimeError, 'initialization may run only once'):
            obj.__init__(horizontal, chart, sample_coords=[[-1, 0], [1, 0]], max_vect_len=1)
        make(Authored, chart=chart)
        self.assertEqual(events, ['points', 'points', 'colors', 'vectors'])

    def test_reentry_and_scene_bound_reconstruction_refuse(self):
        failures = []
        class Authored(m.VectorField):
            def init_points(self):
                super().init_points()
                try:
                    self.__init__(horizontal, self.coordinate_system)
                except RuntimeError as error:
                    failures.append(str(error))
        field = make(Authored)
        self.assertEqual(len(failures), 1)
        self.assertIn('reenter', failures[0])
        scene = m.Scene()
        scene.add(field)
        before = field.data.copy()
        with self.assertRaisesRegex(RuntimeError, 'detached'):
            field.__init__(horizontal, field.coordinate_system)
        np.testing.assert_array_equal(field.data, before)

    def test_invalid_input_fails_before_geometry_hooks(self):
        calls = []
        class Authored(m.VectorField):
            def init_points(self):
                calls.append(self)
                super().init_points()
        for options in (dict(sample_coords=[[0, 0]], max_vect_len=None), dict(sample_coords=[]),
                        dict(sample_coords=[[0, 0], [float('nan'), 1]]),
                        dict(stroke_width=float('inf')), dict(magnitude_range=(2, 1)),
                        dict(norm_to_opacity_func=3), dict(max_vect_len=0)):
            with self.subTest(options=options), self.assertRaises((TypeError, ValueError)):
                make(Authored, **options)
        self.assertEqual(calls, [])

    def test_single_arrow_construction_and_live_time_updates(self):
        sample = [[-1., 0.]]
        wave = lambda rows, time: np.tile([1., time], (len(rows), 1))
        single = make(m.TimeVaryingVectorField, wave, sample_coords=sample)
        pair = make(m.TimeVaryingVectorField, wave)
        for field in (single, pair):
            field.update(.5)
        for column in ("point", "stroke_width", "stroke_rgba"):
            np.testing.assert_array_equal(single.data[column], pair.data[column][:7])
        pair.set_sample_coords(sample).update_vectors()
        for column in ("point", "stroke_width", "stroke_rgba"):
            np.testing.assert_array_equal(pair.data[column], single.data[column])
        world = m.VectorField(lambda rows: rows, sample_points=[[1., 2., 3.]],
                              max_vect_len=np.inf)
        np.testing.assert_array_equal(world.get_points()[[0, 6]], [[1., 2., 3.], [2., 4., 6.]])

    def test_authored_fields_copy_without_rerunning_construction(self):
        calls = []
        class Authored(m.VectorField):
            data_dtype = m.VectorField.data_dtype + [('mass', 1)]
            def init_points(self):
                calls.append(self)
                super().init_points()
                self.data['mass'][:] = 7
        field = make(Authored)
        for clone in (field.copy(), copy.copy(field), copy.deepcopy(field)):
            self.assertIsInstance(clone, Authored)
            np.testing.assert_array_equal(clone.data, field.data)
            clone.set_sample_coords([[-1, 1], [0, 1], [1, 1]]).update_vectors()
            self.assertEqual(clone.get_num_points(), 23)
            np.testing.assert_array_equal(clone.data['mass'], 7.)
            self.assertEqual(field.get_num_points(), 15)
        self.assertEqual(calls, [field])

    def test_time_varying_subclasses_execute_hooks_and_native_updates(self):
        calls = []
        class Authored(m.TimeVaryingVectorField):
            def init_points(self):
                calls.append(self.time)
                super().init_points()
        field = make(Authored, function=lambda rows, time: np.tile([1., time], (len(rows), 1)))
        self.assertEqual(calls, [0.])
        before = field.get_points().copy()
        field.update(.25)
        self.assertEqual(field.time, .25)
        self.assertFalse(np.array_equal(field.get_points(), before))

    def test_callback_colors_and_constructor_style_reach_native_records(self):
        class Authored(m.VectorField):
            pass
        options = dict(color=None, magnitude_range=(0, 2), stroke_width=7, flat_stroke=True,
                       color_map=lambda a: np.tile([.2, .4, .8, 1.], (len(a), 1)),
                       norm_to_opacity_func=lambda n: .4)
        actual, expected = make(Authored, **options), make(**options)
        for name in ('point', 'stroke_rgba', 'stroke_width'):
            np.testing.assert_array_equal(actual.data[name], expected.data[name])
        self.assertTrue(actual.flat_stroke)

    def test_actual_frames_match_independent_native_fields_and_threads(self):
        class Lifted(m.VectorField):
            def update_vectors(self):
                super().update_vectors()
                self.shift(m.UP)
                return self
        def render(path, threads, authored):
            chart, phase = axes(), [0.]
            def function(rows):
                return np.tile([1., phase[0]], (len(rows), 1))
            field = make(Lifted if authored else m.VectorField, function, chart, stroke_width=8)
            if not authored:
                field.shift(m.UP)
            def update(obj, dt):
                if not dt:
                    return
                phase[0] += dt
                if authored:
                    obj.update_vectors()
                else:
                    obj.become(make(function=function, chart=chart, stroke_width=8).shift(m.UP))
            field.add_updater(update)
            scene = m.Scene()
            with scene.render_session(path, format='png_sequence', resolution=(160, 90), fps=4,
                                      threads=threads) as output:
                scene.add(field)
                scene.wait(.75)
            self.assertEqual(output.result.frame_count, 3)
            frames = [p.read_bytes() for p in sorted(path.glob('*.png'))]
            self.assertEqual(len(set(frames)), 3)
            return frames
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            expected = render(root/'literal', 1, False)
            for threads in (1, 4):
                self.assertEqual(render(root/f'authored-{threads}', threads, True), expected)


if __name__ == '__main__':
    unittest.main()
