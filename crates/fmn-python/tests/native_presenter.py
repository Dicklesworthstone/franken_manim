"""Host presenter events through the real native clock, updates and output."""
import io
from pathlib import Path
import tempfile
import unittest

import manimlib as m


class PresenterAcceptance(unittest.TestCase):
    def scene(self, **kwargs):
        options = dict(presenter_mode=True, camera_config={
            "fps": 7, "pixel_width": 64, "pixel_height": 36,
        })
        options.update(kwargs)
        return m.Scene(**options)

    def test_missing_controller_refuses_without_advancing(self):
        scene = self.scene()
        with self.assertRaises(Exception) as caught:
            scene.hold_loop()
        self.assertEqual(type(caught.exception).__name__, "CapabilityError")
        self.assertIn("set_presenter_controller", str(caught.exception))
        self.assertEqual(scene.get_time(), 0)
        self.assertEqual(scene.num_plays, 0)
        self.assertNotIn("_fmn_presenter_hold_active", vars(scene))

    def test_initial_and_wait_release_with_keys_on_exact_native_ticks(self):
        scene = self.scene()
        dot = m.Dot()
        seen = []

        def update(mob, dt):
            seen.append((float(dt), scene.get_time()))
            mob.shift(float(dt) * m.RIGHT)

        dot.add_updater(update, call=False)
        scene.add(dot)
        polls = {"initial": 0, "wait": 0}

        def pump(current, kind):
            self.assertIs(current, scene)
            polls[kind] += 1
            if polls[kind] > {"initial": 1, "wait": 2}[kind]:
                current.on_key_press(ord(" "), 0)
            return None

        self.assertIs(scene.set_presenter_controller(pump, max_frames=3), scene)
        scene.wait(99.0)
        self.assertEqual(polls, {"initial": 2, "wait": 3})
        self.assertEqual(scene.get_time(), 3 / 7)
        self.assertEqual(scene.num_plays, 1)
        self.assertTrue(scene.hold_on_wait)
        positive = [(dt, time) for dt, time in seen if dt > 0]
        self.assertEqual(len(positive), 3)
        self.assertEqual([time for _, time in positive], [1 / 7, 2 / 7, 3 / 7])
        self.assertTrue(all(dt == 1 / 7 for dt, _ in positive))
        self.assertAlmostEqual(dot.get_x(), 3 / 7, places=6)

    def test_finite_and_skipped_waits_need_no_controller(self):
        for mode in ("frames", "ignore", "skip"):
            with self.subTest(mode=mode):
                scene = self.scene(skip_animations=mode == "skip")
                if mode == "frames":
                    scene.wait_frames(2)
                    expected = 2 / 7
                elif mode == "ignore":
                    scene.wait(1.0, ignore_presenter_mode=True)
                    expected = 1.0
                else:
                    scene.wait(1.0)
                    expected = 1.0
                self.assertEqual(scene.get_time(), expected)
                self.assertEqual(scene.num_plays, 1)
                self.assertNotIn("_fmn_presenter_hold_active", vars(scene))
                self.assertNotIn("_fmn_bypass_initial_hold", vars(scene))
        ordinary = self.scene(presenter_mode=False)
        ordinary.wait(1.0)
        self.assertEqual(ordinary.get_time(), 1.0)

    def test_budget_stops_before_an_extra_tick_and_releases_ownership(self):
        scene = self.scene()
        scene.set_presenter_controller(lambda *_: True, max_frames=2)
        with self.assertRaisesRegex(RuntimeError, "frame budget"):
            scene.hold_loop()
        self.assertEqual(scene.get_time(), 2 / 7)
        self.assertEqual(scene.num_plays, 0)
        self.assertNotIn("_fmn_presenter_hold_active", vars(scene))
        scene.set_presenter_controller(None)
        scene.wait_frames(1)
        self.assertEqual(scene.get_time(), 3 / 7)

        exact = self.scene()
        polls = []

        def release_at_limit(*_):
            polls.append(None)
            return len(polls) <= 2

        exact.set_presenter_controller(release_at_limit, max_frames=2)
        exact.hold_loop()
        self.assertEqual(exact.get_time(), 2 / 7)
        with self.assertRaises((ValueError, TypeError)):
            exact.set_presenter_controller(lambda *_: False, max_frames=0)

    def test_poll_exception_and_close_preserve_the_initiating_failure(self):
        scene = self.scene()
        failure = LookupError("authored presenter failure")
        polls = []

        def fail(*_):
            polls.append(None)
            if len(polls) == 2:
                raise failure
            return True

        scene.set_presenter_controller(fail, max_frames=4)
        with self.assertRaises(LookupError) as caught:
            scene.hold_loop()
        self.assertIs(caught.exception, failure)
        self.assertEqual(scene.get_time(), 1 / 7)
        self.assertNotIn("_fmn_presenter_hold_active", vars(scene))
        closing = self.scene()

        def close(current, _kind):
            current.quit_interaction = True

        closing.set_presenter_controller(close, max_frames=2)
        with self.assertRaises(Exception) as caught:
            closing.hold_loop()
        self.assertEqual(type(caught.exception).__name__, "EndScene")
        self.assertEqual(closing.get_time(), 0)
        self.assertNotIn("_fmn_presenter_hold_active", vars(closing))

        waiting = self.scene()
        wait_failure = ArithmeticError("wait poll failed")
        wait_polls = []

        def fail_wait(_scene, kind):
            if kind == "initial":
                return False
            wait_polls.append(None)
            if len(wait_polls) == 2:
                raise wait_failure
            return True

        waiting.set_presenter_controller(fail_wait, max_frames=3)
        with self.assertRaises(ArithmeticError) as caught:
            waiting.wait(99.0)
        self.assertIs(caught.exception, wait_failure)
        self.assertEqual(waiting.get_time(), 1 / 7)
        self.assertNotIn("_fmn_presenter_hold_active", vars(waiting))
        self.assertNotIn("_fmn_scene_execution", vars(waiting))
        self.assertNotIn("_fmn_presenter_initial_allowed", vars(waiting))
        waiting.set_presenter_controller(None)
        waiting.wait_frames(1)
        self.assertEqual(waiting.get_time(), 2 / 7)

    def test_poll_can_edit_and_inspect_but_cannot_reenter_segments(self):
        scene = self.scene()
        dot = m.Dot()
        scene.add(dot)

        def pump(current, _kind):
            with self.assertRaisesRegex(RuntimeError, "another play/wait"):
                current.wait_frames(1)
            self.assertEqual(current.get_time(), 0)
            dot.shift(m.RIGHT)
            current.on_key_press(ord(" "), 0)

        scene.set_presenter_controller(pump, max_frames=2)
        scene.hold_loop()
        self.assertAlmostEqual(dot.get_x(), 1.0)
        self.assertEqual(scene.get_time(), 0)
        scene.set_presenter_controller(lambda *_: object(), max_frames=2)
        with self.assertRaisesRegex(TypeError, "True, False, or None"):
            scene.hold_loop()
        self.assertEqual(scene.get_time(), 0)
        self.assertNotIn("_fmn_presenter_hold_active", vars(scene))

        # The inverse direction is also unsafe: an updater must not open
        # a nested hold while its existing wait owns the rational clock.
        active = self.scene(presenter_mode=False)
        mob = m.Dot()
        nested_attempts = []

        def nested(_mob, dt):
            nested_attempts.append(float(dt))
            with self.assertRaisesRegex(RuntimeError, "active play/wait"):
                active.hold_loop()

        mob.add_updater(nested, call=False)
        active.add(mob)
        active.wait_frames(1)
        self.assertTrue(nested_attempts)
        self.assertEqual(active.get_time(), 1 / 7)
        self.assertNotIn("_fmn_presenter_initial_allowed", vars(active))

    def test_authored_scene_updates_and_readback_observe_each_tick_once(self):
        class Observed(m.Scene):
            def __init__(self, **kwargs):
                self.host_updates = []
                super().__init__(**kwargs)

            def update_mobjects(self, dt):
                self.host_updates.append(float(dt))
                return super().update_mobjects(dt)

        scene = Observed(presenter_mode=True, camera_config={
            "fps": 7, "pixel_width": 64, "pixel_height": 36,
        })
        dot = m.Dot(radius=0.5)
        updates = []
        dot.add_updater(lambda _mob, dt: updates.append(float(dt)), call=False)
        scene.add(dot)
        polls = []

        def pump(*_):
            polls.append(None)
            return len(polls) <= 2

        scene.set_presenter_controller(pump, max_frames=2)
        scene.hold_loop()
        self.assertEqual([dt for dt in updates if dt > 0], [1 / 7, 1 / 7])
        self.assertEqual([dt for dt in scene.host_updates if dt > 0], [1 / 7, 1 / 7])
        pixels = scene.camera.get_pixel_array()
        self.assertEqual(pixels.shape, (36, 64, 4))
        self.assertTrue(pixels[:, :, :3].any())

    def test_existing_native_generation_records_all_presenter_frames(self):
        from fmn_python.rendering import render_scene

        class Recorded(m.Scene):
            def construct(self):
                self.add(m.Square().set_fill(m.BLUE, opacity=1.0))
                polls = {"initial": 0, "wait": 0}

                def pump(_scene, kind):
                    polls[kind] += 1
                    return polls[kind] <= {"initial": 1, "wait": 2}[kind]

                self.set_presenter_controller(pump, max_frames=3)
                self.wait(99.0)

        with tempfile.TemporaryDirectory(prefix="fmn-presenter-") as directory:
            destination = Path(directory) / "frames"
            result = render_scene(
                Recorded, destination,
                scene_kwargs={"presenter_mode": True},
                format="png_sequence",
                resolution=(64, 36), fps=7,
            )
            self.assertEqual(result.frame_count, 3)
            images = sorted(destination.glob("*.png"))
            self.assertEqual(len(images), 3)
            for image in images:
                self.assertTrue(image.read_bytes().startswith(b"\x89PNG\r\n\x1a\n"))

            class FinalOnly(m.Scene):
                def construct(self):
                    self.add(m.Square().set_fill(m.BLUE, opacity=1.0))
                    self.wait(1.0)

            final_path = Path(directory) / "final.png"
            final_result = render_scene(
                FinalOnly, final_path,
                scene_kwargs={"presenter_mode": True},
                format="png", resolution=(64, 36), fps=7,
            )
            self.assertEqual(final_result.frame_count, 1)
            self.assertTrue(final_path.read_bytes().startswith(b"\x89PNG\r\n\x1a\n"))


def run_native_presenter():
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(PresenterAcceptance)
    output = io.StringIO()
    result = unittest.TextTestRunner(stream=output, verbosity=2).run(suite)
    if not result.wasSuccessful():
        raise AssertionError(output.getvalue())
    return result.testsRun
