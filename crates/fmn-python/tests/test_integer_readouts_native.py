"""Actual Integer/Scene/PNG acceptance; FMN_TEST_NATIVE=1 requires the wheel."""
import copy
from pathlib import Path
import pickle
import tempfile
import unittest

import numpy as np
from test_parallel_batch_native import native_wheel_available


@unittest.skipUnless(native_wheel_available(), "requires the built FrankenManim native wheel")
class NativeIntegerReadouts(unittest.TestCase):
    def test_exact_value_string_and_native_character_geometry(self):
        import manimlib as m
        for value in (2**53 + 1, -(2**53 + 1), 10**30 + 123456789, 10**400):
            number = m.Integer(value)
            expected = format(value, ",d").replace("-", "–")
            self.assertEqual(number.get_value(), value)
            self.assertEqual(number.get_tex(), expected)
            self.assertEqual(len(number), len(expected))
            for char, child in zip(expected, number):
                # Points are stored as float32. Re-centering a glyph that sat at |x| = R can
                # move each coordinate by one f32 ULP of R (3.7e-6 for 10**400's farthest
                # glyph, at |x| = 38.9). So allow two ULPs of the glyph's own magnitude,
                # and never less than the 1e-6 kept near the origin.
                reach = float(np.max(np.abs(child.get_all_points())))
                tolerance = max(1e-6, 2 * float(np.spacing(np.float32(reach))))
                np.testing.assert_allclose(child.copy().center().get_all_points(),
                                           m.Text(char).get_all_points(), atol=tolerance)
            self.assertTrue(all(np.isfinite(part.get_points()).all()
                                for part in number.family_members_with_points()))

    def test_bound_update_increment_anchor_style_copy_and_pickle(self):
        import manimlib as m
        number = m.Integer(10**30, color=m.BLUE, edge_to_fix=m.RIGHT)
        number.scale(.5).shift(m.UP)
        scene = m.Scene()
        parent = m.VGroup(number)
        scene.add(parent)
        edge = number.get_right().copy()
        for value in (10**30 + 1, 2**53 + 1, -10**100, 7):
            self.assertIs(number.set_value(value), number)
            self.assertEqual(number.get_value(), value)
            number.increment_value()
            self.assertEqual(number.get_value(), value + 1)
            np.testing.assert_allclose(number.get_right(), edge, atol=2e-6)
            self.assertIs(parent[0], number)
            self.assertTrue(all(part.get_fill_color() == m.BLUE
                                for part in number.family_members_with_points()))
        number.set_value(10**100 + 17)
        for other in (number.copy(), copy.deepcopy(number), pickle.loads(pickle.dumps(number))):
            self.assertIs(type(other), m.Integer)
            self.assertEqual(other.get_value(), 10**100 + 17)
            other.increment_value()
            self.assertEqual(other.get_value(), 10**100 + 18)
            self.assertEqual(number.get_value(), 10**100 + 17)

    def test_real_scene_clock_delivers_exact_animation_endpoint(self):
        import manimlib as m
        for start, target in ((10**30, 10**30 + 1), (10**100, -10**100 + 1)):
            number = m.Integer(start)
            parent = m.VGroup(number)
            scene = m.Scene(camera_config=dict(resolution=(192, 108), fps=8))
            scene.add(parent)
            scene.play(m.ChangeDecimalToValue(number, target), run_time=.5)
            self.assertEqual(number.get_value(), target)
            self.assertEqual(number.get_tex(), format(target, ",d").replace("-", "–"))
            self.assertEqual(list(scene.mobjects), [scene.frame, parent])
            self.assertIs(parent[0], number)

    def test_rendered_integer_frames_are_nonstatic_and_thread_stable(self):
        import manimlib as m
        class Counter(m.Scene):
            def construct(self):
                self.number = m.Integer(2**53 + 1).set_width(12)
                self.add(self.number)
                self.play(m.ChangeDecimalToValue(self.number, 2**53 + 101), run_time=.5)
        with tempfile.TemporaryDirectory(prefix="fmn-exact-integer-") as directory:
            sequences = []
            for threads in (1, 4):
                scene = Counter()
                receipt = scene.render(Path(directory) / str(threads),
                                       resolution=(320, 180), fps=8, threads=threads)
                self.assertEqual(scene.number.get_value(), 2**53 + 101)
                self.assertEqual(receipt.frame_count, 4)
                frames = [path.read_bytes() for path in sorted(receipt.destination.glob("*.png"))]
                self.assertEqual(len(frames), 4)
                self.assertGreater(len(set(frames)), 1)
                sequences.append(frames)
            self.assertEqual(sequences[0], sequences[1])

    def test_failed_glyph_or_budget_never_publishes_partial_integer(self):
        import manimlib as m
        class Fragile(m.Integer):
            fail = False
            def char_to_mob(self, char):
                if self.fail and char == "9":
                    raise ValueError("missing integer glyph")
                return super().char_to_mob(char)
        number = Fragile(10**30 + 1)
        scene = m.Scene()
        scene.add(number)
        def snapshot():
            return (number.number, number.num_string, tuple(map(id, number.get_family())),
                    tuple(part.data.copy().tobytes() for part in number.get_family()))
        before = snapshot()
        number.fail = True
        with self.assertRaisesRegex(ValueError, "missing integer glyph"):
            number.set_value(10**30 + 9)
        self.assertEqual(snapshot(), before)
        with self.assertRaisesRegex(ValueError, "characters"):
            number.set_value(1 << 1_000_000)
        self.assertEqual(snapshot(), before)
        # DecimalNumber keeps an integral value too, as the Reference does (fm-yuin).
        ordinary = m.DecimalNumber(10**16 + 1, num_decimal_places=0)
        self.assertEqual(ordinary.get_tex(), "10,000,000,000,000,001")
        self.assertEqual(ordinary.get_value(), 10**16 + 1)
        self.assertEqual(m.DecimalNumber(10**18 + 99).get_tex(), "1,000,000,000,000,000,128.00")


if __name__ == "__main__":
    unittest.main()
