"""Library-class subclasses get their engine hooks called back (plan §15.2, fm-5wq.13).

The Reference's Mobject.__init__ calls init_points once on every construction, through normal
MRO, whatever library class is being built. A 3b1b scene subclasses Circle, Axes or Text as
readily as Mobject, so the portal must honor an init_points override on each exported base, and
the engine must render the overridden geometry. A subclass overriding interpolate is likewise
called back by Transform on every frame, and its result is what the frame shows.

Each case has a negative control: the unmodified base, constructed the same way, which the
override must visibly change.
"""
from pathlib import Path
import tempfile
import unittest

import numpy as np
import manimlib as m
from fmn_python import render_session

TRIANGLE = [m.LEFT, m.UP, m.RIGHT, m.LEFT]
# Constructor arguments for each required base (fm-5wq.13 item 3).
BASES = {
    "Circle": (), "Square": (), "Line": (), "Arrow": (m.LEFT, m.RIGHT), "VGroup": (),
    "Axes": (), "NumberPlane": (), "Text": ("hi",), "Tex": ("x",), "DecimalNumber": (1.5,),
}


def first_frame(mobject):
    """One rendered frame (RGB rows) of a scene holding only this mobject."""
    with tempfile.TemporaryDirectory() as tmp:
        scene = m.Scene()
        path = Path(tmp) / "frame.y4m"
        with render_session(scene, path, resolution=(64, 36), fps=8, threads=1):
            scene.add(mobject)
            scene.wait(0.125)
        data = path.read_bytes()
    frame = data[data.index(b"FRAME\n") + 6:]
    return np.frombuffer(frame[:64 * 36], dtype=np.uint8).reshape(36, 64)


class InitPointsOverrides(unittest.TestCase):
    def test_every_required_base_calls_the_override_once_and_renders_its_geometry(self):
        for name, args in BASES.items():
            base = getattr(m, name)
            calls = []

            def init_points(self, _calls=calls):
                _calls.append(type(self).__name__)
                # Replace the class's own geometry; the triangle is distinctive for every base.
                self.set_points_as_corners(TRIANGLE)

            Authored = type("Authored" + name, (base,), {"init_points": init_points})
            with self.subTest(base=name):
                authored, control = Authored(*args), base(*args)
                self.assertEqual(calls, ["Authored" + name], "one callback per construction")
                own = np.asarray(authored.get_points())
                self.assertGreater(len(own), 0)
                corners = authored.get_anchors()
                # The constructor may still scale or move the family after init_points;
                # the shape survives as three distinct anchors closing a loop.
                self.assertEqual(len({tuple(np.round(p, 9)) for p in corners}), 3, corners)
                baseline = np.asarray(control.get_points())
                self.assertFalse(baseline.shape == own.shape and np.allclose(baseline, own),
                                 "the control's geometry must differ, or the check proves nothing")
                self.assertFalse(np.array_equal(first_frame(authored), first_frame(control)),
                                 "the overridden geometry must reach the rendered frame")

    def test_surface_override_reaches_the_mesh(self):
        calls = []

        class Doubled(m.Surface):
            def init_points(self):
                calls.append(1)
                super().init_points()
                self.set_points(2 * self.get_points())

        doubled, control = Doubled(), m.Surface()
        self.assertEqual(calls, [1])
        np.testing.assert_allclose(doubled.get_points(), 2 * control.get_points())
        self.assertFalse(np.array_equal(first_frame(doubled), first_frame(control)))


class InterpolateOverride(unittest.TestCase):
    def test_transform_calls_the_override_each_frame_and_shows_its_result(self):
        def run(cls):
            centers = []

            class Scene(m.Scene):
                def construct(self):
                    mob = cls()
                    probe = m.Mobject()
                    probe.add_updater(lambda obj, dt: centers.append(mob.get_center()[1]) if dt else None)
                    self.add(mob, probe)
                    self.play(m.Transform(mob, m.Square().shift(m.RIGHT)), run_time=0.5)
            with tempfile.TemporaryDirectory() as tmp:
                scene = Scene()
                with render_session(scene, Path(tmp) / "t.y4m", resolution=(32, 18), fps=8, threads=1):
                    scene.run()
            return centers

        seen = []

        class Lifted(m.Square):
            def interpolate(self, mobject1, mobject2, alpha, *args, **kwargs):
                seen.append(alpha)
                result = super().interpolate(mobject1, mobject2, alpha, *args, **kwargs)
                # A visible authored effect: lift by sin(pi * alpha), zero at both ends.
                self.shift(np.sin(np.pi * alpha) * m.UP)
                return result

        lifted, plain = run(Lifted), run(m.Square)
        self.assertGreaterEqual(len(seen), 4, "one callback per frame")
        self.assertTrue(all(0 <= a <= 1 for a in seen))
        self.assertGreater(max(lifted), 0.5, "the authored lift is what the frames hold")
        np.testing.assert_allclose(plain, 0, atol=1e-9)


if __name__ == "__main__":
    unittest.main()
