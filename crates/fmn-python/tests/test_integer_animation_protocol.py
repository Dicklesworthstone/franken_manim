"""Production number-track adapter; explicit animation/native fixture boundaries."""
from fractions import Fraction
import math
import unittest

import numpy as np

from test_integer_readouts_protocol import host, glyphs


class IntegerAnimationTests(unittest.TestCase):
    def test_both_endpoints_are_exact_and_reach_the_live_readout(self):
        with host(animations=True) as h:
            for start, target in ((2**53 + 1, 2**53 + 2), (10**30, 10**30 + 1),
                                  (10**400, -(10**400) + 1), (-10**100, -10**100 - 1)):
                mob = h.native.Integer(start)
                animation = h.native.ChangeDecimalToValue(mob, target)
                for alpha, expected in ((0., start), (1., target), (0., start)):
                    result = animation.number_update_func(alpha)
                    self.assertIs(type(result), int)
                    self.assertEqual(result, expected)
                    mob.set_value(result)
                    self.assertEqual(mob.get_value(), expected)
                    self.assertEqual(glyphs(mob), format(expected, ",d").replace("-", "–"))

    def test_intermediate_wide_values_match_an_independent_fraction_oracle(self):
        with host(animations=True) as h:
            pairs = ((10**100 + 7, 10**100 + 101), (-10**100 - 7, -10**100 - 101),
                     (-10**400 + 9, 10**400 - 7), (2**53 + 1, -(2**53) - 7))
            phases = (-.25, -0., math.nextafter(0., 1.), .1, .25, .5, .9,
                      math.nextafter(1., 0.), 1., 1.25)
            for start, target in pairs:
                track = h.native.ChangeDecimalToValue(h.native.Integer(start), target)
                for alpha in phases:
                    expected = int(Fraction(start) + (target - start) * Fraction(alpha))
                    self.assertEqual(track.number_update_func(alpha), expected)

    def test_one_unit_tracks_never_invent_out_of_range_digits(self):
        with host(animations=True) as h:
            for start in (10**30, -10**30):
                for step in (-1, 1):
                    track = h.native.ChangeDecimalToValue(h.native.Integer(start), start + step)
                    values = [track.number_update_func(k / 100) for k in range(101)]
                    self.assertTrue(set(values) <= {start, start + step})
                    self.assertEqual(values, sorted(values, reverse=step < 0))
                    self.assertEqual((values[0], values[-1]), (start, start + step))

    def test_numpy_endpoints_cannot_overflow_fixed_width_arithmetic(self):
        with host(animations=True) as h:
            start, target = np.int64(-(2**63)), np.uint64(2**64 - 1)
            track = h.native.ChangeDecimalToValue(h.native.Integer(start), target)
            self.assertEqual(track.number_update_func(0.), int(start))
            self.assertEqual(track.number_update_func(1.), int(target))
            self.assertEqual(track.number_update_func(.5), int(Fraction(int(start) + int(target), 2)))

    def test_small_integer_interior_and_float_decimal_tracks_are_unchanged(self):
        with host(animations=True) as h:
            for mob, target in ((h.native.Integer(3), 8), (h.native.Integer(1.25), 9),
                                (h.native.DecimalNumber(10**16 + 1), 10**16 + 3),
                                (h.native.DecimalNumber(1 + 2j), -3j)):
                start = mob.number
                track = h.native.ChangeDecimalToValue(mob, target)
                for alpha in (.125, .25, .5, .75):
                    self.assertEqual(track.number_update_func(alpha), (1 - alpha) * start + alpha * target)
            self.assertEqual(h.native.ChangeDecimalToValue(h.native.Integer(3), 8).number_update_func(.5), 5.5)

    def test_original_constructor_options_and_captured_start_survive(self):
        with host(animations=True) as h:
            mob = h.native.Integer(10**30 + 1)
            rate = lambda alpha: 1 - alpha
            track = h.native.ChangeDecimalToValue(mob, 10**30 + 9, rate_func=rate,
                                                  run_time=2, time_span=(.2, 1.8))
            mob.set_value(7)
            self.assertEqual(track.number_update_func(0.), 10**30 + 1)
            self.assertEqual(track.number_update_func(1.), 10**30 + 9)
            self.assertEqual(track.options, dict(rate_func=rate, run_time=2, time_span=(.2, 1.8)))
            self.assertIs(track.mobject, mob)

    def test_nonfinite_phase_and_oversized_target_do_not_publish(self):
        with host(animations=True) as h:
            mob = h.native.Integer(10**30 + 1)
            track = h.native.ChangeDecimalToValue(mob, 10**30 + 9)
            before = mob.number, mob.num_string, tuple(map(id, mob.submobjects))
            for alpha in (float("inf"), float("nan")):
                with self.assertRaises((ValueError, OverflowError)): track.number_update_func(alpha)
            with self.assertRaisesRegex(ValueError, "characters"):
                h.native.ChangeDecimalToValue(mob, 1 << 1_000_000)
            self.assertEqual((mob.number, mob.num_string, tuple(map(id, mob.submobjects))), before)

    def test_authored_animation_and_value_overrides_are_preserved(self):
        with host(animations=True) as h:
            class Custom(h.native.ChangeDecimalToValue):
                def __init__(self, mob, target, **kwargs):
                    super().__init__(mob, target, **kwargs)
                    original = self.number_update_func
                    self.number_update_func = lambda alpha: original(alpha) + 7
            track = Custom(h.native.Integer(10**30), 10**30 + 1)
            self.assertEqual(track.number_update_func(1.), 10**30 + 8)

    def test_readout_publication_does_not_force_the_animation_to_its_endpoint(self):
        with host(animations=True) as h:
            mob = h.native.Integer(10**30)
            track = h.native.ChangeDecimalToValue(mob, 10**30 + 100)
            mob.set_value(track.number_update_func(.25))
            self.assertEqual(mob.get_value(), 10**30 + 25)
            self.assertNotEqual(mob.get_value(), 10**30 + 100)
            mob.set_value(track.number_update_func(0.))
            self.assertEqual(mob.get_value(), 10**30)


if __name__ == "__main__":
    unittest.main()
