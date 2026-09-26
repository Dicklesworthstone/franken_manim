"""Curved-arrow initialization on real Marionette records and native geometry."""
import copy
from pathlib import Path
import tempfile
import unittest

import numpy as np
import manimlib as m
from manimlib.mobject.geometry import (
    ArcBetweenPoints as QualifiedBetween,
    CurvedArrow as QualifiedCurved,
    CurvedDoubleArrow as QualifiedDouble,
)


class NativeCurvedLifecycle(unittest.TestCase):
    def assert_points(self, actual, expected):
        np.testing.assert_allclose(actual, expected, rtol=2e-5, atol=2e-5)

    def test_all_initialization_phases_precede_fitting_and_tips(self):
        for base, tips in ((m.ArcBetweenPoints, 0), (m.CurvedArrow, 1),
                           (m.CurvedDoubleArrow, 2)):
            with self.subTest(base=base.__name__):
                events = []
                class Authored(base):
                    def init_data(self):
                        events.append("data")
                        super().init_data()
                    def init_points(self):
                        events.append("points")
                        self.recipe = (self.radius, self.start_angle, self.arc_center.copy())
                        super().init_points()
                    def init_uniforms(self):
                        events.append("uniforms")
                        super().init_uniforms()
                        self.uniforms["custom_uniform"] = 17
                    def init_colors(self):
                        events.append("colors")
                        super().init_colors()
                    def put_start_and_end_on(self, *args, **kwargs):
                        events.append("fit")
                        return super().put_start_and_end_on(*args, **kwargs)
                    def add_tip(self, *args, **kwargs):
                        events.append("tip")
                        return super().add_tip(*args, **kwargs)
                obj = Authored(2*m.LEFT, 3*m.RIGHT, radius=2, start_angle=.3,
                               arc_center=m.UP, n_components=12)
                # Tip fitting may call put_start_and_end_on again. The initial
                # fit must nevertheless follow colors and precede the first tip.
                self.assertEqual(events[:5], ["data", "points", "uniforms", "colors", "fit"])
                self.assertEqual(events.count("points"), 1)
                self.assertEqual(events.count("tip"), tips)
                self.assertEqual(obj.recipe[:2], (2.0, .3))
                self.assert_points(obj.recipe[2], m.UP)
                self.assertEqual(obj.uniforms["custom_uniform"], 17)
                self.assert_points(obj.get_start(), 2*m.LEFT)
                self.assert_points(obj.get_end(), 3*m.RIGHT)

    def test_qualified_exports_keep_the_original_class_objects(self):
        self.assertIs(QualifiedBetween, m.ArcBetweenPoints)
        self.assertIs(QualifiedCurved, m.CurvedArrow)
        self.assertIs(QualifiedDouble, m.CurvedDoubleArrow)
        self.assertIs(m.CurvedDoubleArrow.__bases__[0], m.CurvedArrow)

    def test_authored_point_run_is_fitted_instead_of_replaced(self):
        points = np.array([[-1.,0.,0.], [-.5,2.,0.], [0.,1.,0.],
                           [.5,2.,0.], [1.,0.,0.]])
        class Authored(m.ArcBetweenPoints):
            def init_points(self):
                self.set_points(points)
        obj = Authored(m.DOWN, 3*m.UP, angle=.8)
        expected = m.VMobject().set_points(points)
        expected.put_start_and_end_on(m.DOWN, 3*m.UP)
        self.assertEqual(obj.get_num_points(), 5)
        self.assert_points(obj.get_points(), expected.get_points())

    def test_zero_angle_uses_the_reference_line_before_tip_creation(self):
        for cls, count in ((m.ArcBetweenPoints, 0), (m.CurvedArrow, 1),
                           (m.CurvedDoubleArrow, 2)):
            with self.subTest(cls=cls.__name__):
                obj = cls(2*m.LEFT, 2*m.RIGHT, angle=0)
                self.assert_points(obj.get_start(), 2*m.LEFT)
                self.assert_points(obj.get_end(), 2*m.RIGHT)
                self.assertEqual(len(obj.get_tips()), count)
                self.assert_points(obj.get_points()[:, 1], 0)

    def test_custom_dtype_and_hook_owned_children_survive(self):
        class Authored(m.CurvedDoubleArrow):
            data_dtype = m.VMobject.data_dtype + [("weight", 1)]
            def init_data(self):
                self.resize(1)
                self.set_field("weight", 0, [.75])
                self.marker = m.VectorizedPoint(m.ORIGIN)
                self.add(self.marker)
        obj = Authored(2*m.LEFT, 2*m.RIGHT, angle=.8)
        self.assertIn("weight", obj.data.dtype.names)
        self.assert_points(obj.data["weight"], .75)
        self.assertIs(obj[0], obj.marker)
        self.assertEqual(len(obj.get_tips()), 2)
        self.assertIn(obj.tip, obj.submobjects)
        self.assertIn(obj.start_tip, obj.submobjects)
        self.assertIsNot(obj.tip, obj.start_tip)

    def test_cooperative_mixin_and_later_base_patch_are_observed(self):
        seen = []
        class Mixin:
            def init_points(self):
                seen.append("mixin")
                super().init_points()
        class Authored(Mixin, m.CurvedArrow):
            pass
        original = m.Arc.__init__
        def patched(obj, *args, **kwargs):
            seen.append("arc")
            original(obj, *args, **kwargs)
        m.Arc.__init__ = patched
        try:
            obj = Authored(m.LEFT, m.RIGHT)
        finally:
            m.Arc.__init__ = original
        self.assertEqual(seen, ["arc", "mixin"])
        self.assert_points(obj.get_end(), m.RIGHT)

    def test_authored_tip_factory_is_used_for_both_ends(self):
        calls = []
        class Authored(m.CurvedDoubleArrow):
            def get_unpositioned_tip(self, **kwargs):
                calls.append("tip")
                tip = super().get_unpositioned_tip(**kwargs)
                tip.custom_tag = len(calls)
                return tip
        obj = Authored(2*m.LEFT, 2*m.RIGHT, color=m.BLUE)
        self.assertEqual(calls, ["tip", "tip"])
        self.assertEqual((obj.tip.custom_tag, obj.start_tip.custom_tag), (1, 2))
        self.assertEqual(obj.tip.get_fill_color(), m.BLUE)
        self.assertEqual(obj.start_tip.get_fill_color(), m.BLUE)

    def test_hook_failure_stops_before_fitting_or_publishing_tips(self):
        for phase in ("init_data", "init_points", "init_uniforms", "init_colors"):
            with self.subTest(phase=phase):
                seen, error = [], RuntimeError("authored " + phase)
                def fail(obj):
                    seen.append(phase)
                    raise error
                class Authored(m.CurvedDoubleArrow):
                    def put_start_and_end_on(self, *args, **kwargs): seen.append("fit")
                    def add_tip(self, **kwargs): seen.append("tip")
                setattr(Authored, phase, fail)
                with self.assertRaises(RuntimeError) as caught:
                    Authored(m.LEFT, m.RIGHT)
                self.assertIs(caught.exception, error)
                self.assertEqual(seen, [phase])

    def test_copies_preserve_tip_aliases_without_reinitializing(self):
        calls = []
        class Authored(m.CurvedDoubleArrow):
            def init_points(self):
                calls.append("points")
                super().init_points()
        obj = Authored(2*m.LEFT, 2*m.RIGHT)
        original = obj.get_points().copy()
        for clone in (obj.copy(), copy.deepcopy(obj)):
            self.assertIs(type(clone), Authored)
            self.assertIsNot(clone.tip, obj.tip)
            self.assertIsNot(clone.start_tip, obj.start_tip)
            self.assertIn(clone.tip, clone.submobjects)
            self.assertIn(clone.start_tip, clone.submobjects)
            clone.shift(m.UP)
        self.assertEqual(calls, ["points"])
        np.testing.assert_array_equal(obj.get_points(), original)

    def test_public_construction_matches_independent_native_arc_and_tip_composition(self):
        for angle in (-.8, 0, .8):
            obj = m.CurvedDoubleArrow(2*m.LEFT, 2*m.RIGHT, angle=angle, color=m.BLUE)
            # Do not use any of the three constructors under test as oracle.
            expected = m.Arc(angle=angle, color=m.BLUE)
            if angle == 0:
                expected.set_points_as_corners([m.LEFT, m.RIGHT])
            expected.put_start_and_end_on(2*m.LEFT, 2*m.RIGHT)
            expected.add_tip()
            expected.add_tip(at_start=True)
            for actual, reference in zip(obj.get_family(), expected.get_family()):
                np.testing.assert_array_equal(actual.data, reference.data)
            self.assertEqual(len(obj.get_family()), len(expected.get_family()))

    def test_authored_curved_geometry_animates_at_one_and_four_threads(self):
        directory = Path(tempfile.mkdtemp(prefix="fmn-curved-lifecycle-"))
        def render(name, threads, authored):
            scene = m.Scene()
            destination = directory / name
            with scene.render_session(destination, format="png_sequence", fps=4,
                                      resolution=(96,54), threads=threads):
                class Authored(m.CurvedArrow):
                    def init_points(self):
                        self.set_points([[-1,0,0], [-.5,2,0], [0,1,0], [.5,2,0], [1,0,0]])
                if authored:
                    obj = Authored(2*m.LEFT, 2*m.RIGHT, color=m.BLUE)
                else:
                    obj = m.Arc(color=m.BLUE)
                    obj.set_points([[-1,0,0], [-.5,2,0], [0,1,0], [.5,2,0], [1,0,0]])
                    obj.put_start_and_end_on(2*m.LEFT, 2*m.RIGHT)
                    obj.add_tip()
                scene.add(obj)
                scene.play(m.Transform(obj, obj.copy().shift(m.UP), rate_func=m.linear), run_time=1)
            return [p.read_bytes() for p in sorted(destination.glob("*.png"))]
        frames = render("one", 1, True)
        self.assertEqual(len(frames), 4)
        self.assertGreater(len(set(frames)), 1)
        self.assertEqual(frames, render("four", 4, True))
        self.assertEqual(frames, render("oracle", 1, False))
        print("retained curved lifecycle frames:", directory)


if __name__ == "__main__":
    unittest.main()
else:
    result = unittest.TextTestRunner(verbosity=2).run(
        unittest.defaultTestLoader.loadTestsFromTestCase(NativeCurvedLifecycle))
    if not result.wasSuccessful():
        raise AssertionError("native curved lifecycle acceptance failed")
