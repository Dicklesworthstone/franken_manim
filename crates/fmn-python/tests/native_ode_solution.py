"""Public n-dimensional ODE authoring through Atlas's real adaptive solver."""
import unittest

import numpy as np

from manimlib.mobject.vector_field import ode_solution_points


class NativeOdeSolutionTests(unittest.TestCase):
    def test_decay_accuracy_and_endpoint_exclusion(self):
        calls = []

        def decay(state):
            self.assertIsInstance(state, np.ndarray)
            self.assertEqual(state.shape, (1,))
            calls.append(state.copy())
            return -state

        points = ode_solution_points(decay, [1.0], 1.0)
        times = np.arange(0, 1, .01)
        self.assertEqual(points.shape, (100, 1))
        self.assertEqual(points.dtype, np.float64)
        np.testing.assert_allclose(points[:, 0], np.exp(-times), rtol=1e-3)
        self.assertLess(len(calls), len(points))
        self.assertEqual(points[0, 0], 1.0)

    def test_oscillator_dense_samples(self):
        points = ode_solution_points(lambda state: [state[1], -state[0]], [1., 0.], 1., .01)
        times = np.arange(0, 1, .01)
        expected = np.column_stack((np.cos(times), -np.sin(times)))
        np.testing.assert_allclose(points, expected, rtol=1e-3, atol=1e-4)

    def test_six_dimensional_state_is_not_truncated_or_padded(self):
        rates = np.array([.1, .2, .4, .8, 1., 1.5])
        initial = np.arange(1., 7.)
        points = ode_solution_points(lambda state: -rates * state, initial, 1., .01)
        expected = initial * np.exp(-np.arange(0, 1, .01)[:, None] * rates)
        self.assertEqual(points.shape, (100, 6))
        np.testing.assert_allclose(points, expected, rtol=2e-3, atol=1e-6)
        np.testing.assert_array_equal(initial, np.arange(1., 7.))

    def test_nonintegral_and_coarse_output_intervals(self):
        for duration, step in ((1., .3), (.3, .1), (.1, 1.), (1., .125)):
            with self.subTest(duration=duration, step=step):
                points = ode_solution_points(lambda state: [1.], [2.], duration, step)
                times = np.arange(0, duration, step)
                self.assertEqual(points.shape, (len(times), 1))
                np.testing.assert_allclose(points[:, 0], 2. + times, atol=1e-12, rtol=0.)

    def test_first_callback_exception_is_preserved_without_further_calls(self):
        for stop in (1, 2, 3, 7, 12):
            for kind in (RuntimeError, KeyboardInterrupt, SystemExit):
                with self.subTest(stop=stop, kind=kind):
                    calls = []
                    original = kind("ODE callback stopped")

                    def derivative(state):
                        calls.append(state.copy())
                        if len(calls) == stop:
                            raise original
                        return -state

                    with self.assertRaises(kind) as caught:
                        ode_solution_points(derivative, [1.], 1., .01)
                    self.assertIs(caught.exception, original)
                    self.assertEqual(len(calls), stop)

    def test_invalid_controls_and_state_refuse_before_callbacks(self):
        cases = (
            ([1.], 0., .01), ([1.], -1., .01), ([1.], float('inf'), .01),
            ([1.], 1., 0.), ([1.], 1., -.01), ([1.], 1., float('nan')),
            ([1.], 1., 1e-12), ([], 1., .01), ([[1.]], 1., .01),
            (1., 1., .01), ([float('nan')], 1., .01), ([float('inf')], 1., .01),
        )
        for initial, duration, step in cases:
            with self.subTest(initial=initial, duration=duration, step=step):
                calls = []
                with self.assertRaises(ValueError):
                    ode_solution_points(lambda state: calls.append(state) or state,
                                        initial, duration, step)
                self.assertEqual(calls, [])
        with self.assertRaisesRegex(TypeError, "callable"):
            ode_solution_points(None, [1.], 1.)

    def test_malformed_derivatives_stop_at_the_first_invalid_value(self):
        for invalid in ([], [1., 2.], [[1.]], 1., [float('nan')], [float('inf')]):
            for stop in (1, 3, 7):
                with self.subTest(invalid=invalid, stop=stop):
                    calls = []

                    def derivative(state):
                        calls.append(state.copy())
                        return invalid if len(calls) == stop else -state

                    with self.assertRaises(ValueError):
                        ode_solution_points(derivative, [1.], 1.)
                    self.assertEqual(len(calls), stop)

    def test_callback_can_solve_another_ode(self):
        nested = []

        def derivative(state):
            if not nested:
                nested.append(ode_solution_points(lambda inner: [2.], [3.], .2, .05))
            return -state

        points = ode_solution_points(derivative, [1.], 1.)
        np.testing.assert_allclose(nested[0][:, 0], [3., 3.1, 3.2, 3.3], atol=1e-12)
        np.testing.assert_allclose(points[:, 0], np.exp(-np.arange(0, 1, .01)), rtol=1e-3)


def run_native_ode_solution():
    result = unittest.TextTestRunner(verbosity=2).run(
        unittest.defaultTestLoader.loadTestsFromTestCase(NativeOdeSolutionTests))
    if not result.wasSuccessful():
        raise AssertionError("native ODE authoring acceptance failed")
    return result.testsRun


if __name__ in ('__main__', '<run_path>'):
    run_native_ode_solution()
