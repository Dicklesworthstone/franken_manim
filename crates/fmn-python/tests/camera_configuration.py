"""Native scene camera configuration, live frame identity and actual pixels."""
from pathlib import Path
from copy import deepcopy
import subprocess
import sys
import tempfile
import unittest

import numpy as np
import manimlib as m


class CameraConfigurationTests(unittest.TestCase):
    def test_bare_runs_use_constructor_and_class_camera_fps(self):
        class Timed(m.Scene):
            default_camera_config = {"fps": 60}

            def construct(self):
                self.ticks = []
                self.rates = []
                dot = m.Dot()
                dot.add_updater(lambda mob, dt: self.ticks.append(dt))
                self.add(dot)
                self.play(m.Square().animate.shift(m.RIGHT), run_time=.25,
                          rate_func=lambda alpha: self.rates.append(alpha) or alpha)
                self.wait(.25)

        for options, fps in (({}, 60), ({"camera_config": {"fps": 24}}, 24)):
            with self.subTest(fps=fps):
                scene = Timed(**options)
                frame = scene.frame
                scene.run()
                positive = [dt for dt in scene.ticks if dt > 0]
                self.assertTrue(positive)
                np.testing.assert_allclose(positive, 1 / fps, rtol=0, atol=1e-15)
                self.assertEqual(scene.camera.fps, fps)
                self.assertIs(scene.camera.frame, frame)
                # Custom rates are evaluated on the same nominal frame grid.
                self.assertTrue(any(abs(alpha - 4 / fps) < 1e-12
                                    for alpha in scene.rates))
                self.assertFalse(any(abs(dt - 1 / 30) < 1e-12 for dt in positive))

    def test_constructor_clock_is_ready_for_imperative_playback(self):
        scene = m.Scene(camera_config={"fps": 60})
        ticks = []
        dot = m.Dot().add_updater(lambda mob, dt: ticks.append(dt))
        scene.add(dot)
        scene.wait(.1)
        positive = [dt for dt in ticks if dt > 0]
        self.assertTrue(positive)
        np.testing.assert_allclose(positive, 1 / 60, rtol=0, atol=1e-15)

    def test_bare_clock_configuration_does_not_apply_a_recording_pixel_budget(self):
        scene = m.Scene(camera_config={"resolution": (7680, 4320), "fps": 60})
        frame = scene.frame
        self.assertNotIn("_camera", vars(scene))
        scene.run()
        self.assertIs(scene.frame, frame)
        self.assertNotIn("_camera", vars(scene))
        self.assertEqual(m._portal_scene_clock(scene), (60, 0))
        # Selecting only a clock allocates no pixels. The existing recording
        # boundary must still refuse this resolution at its unchanged limit.
        with self.assertRaisesRegex(ValueError, "at most 16777216 pixels"):
            m._portal_prepare_recording_scene(scene, 7680, 4320, 60, 0)

    def test_project_scene_and_writer_defaults_reach_the_installed_initializer(self):
        from fmn_python.scene_loading import SceneSource
        configuration = m._resolved_portal_config(None,
            "scene:\n  default_wait_time: 0.25\n  show_animation_progress: true\n"
            "  leave_progress_bars: true\n  preview_while_skipping: false\n"
            "file_writer:\n  video_crf: 0\n  video_preset: slow\n"
            "  video_tune: animation\n  audio_bitrate: 192000\n")
        live, camera = m.manim_config, m.manim_config["camera"]
        original = deepcopy(live)
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory, "configured_scene.py")
            path.write_text(
                "import manimlib as m\n"
                "class Configured(m.Scene): pass\n"
                "class Authored(m.Scene):\n"
                "    default_wait_time = 1.0\n"
                "    default_file_writer_config = {'video_crf': 18}\n")
            with SceneSource(path, m.Scene, configuration=configuration) as source:
                scene = source.scenes["Configured"]()
                self.assertEqual(scene.default_wait_time, .25)
                self.assertEqual((scene.show_animation_progress, scene.leave_progress_bars,
                                  scene.preview_while_skipping), (True, True, False))
                self.assertEqual(scene.file_writer.video_crf, 0)
                self.assertEqual(scene.file_writer.video_preset, "slow")
                self.assertEqual(scene.file_writer.video_tune, "animation")
                self.assertEqual(scene.file_writer.audio_bitrate, 192000)
                self.assertIs(scene.camera.frame, scene.frame)
                authored = source.scenes["Authored"]()
                self.assertEqual(authored.default_wait_time, 1.)
                self.assertEqual(authored.file_writer.video_crf, 18)
                overridden = source.scenes["Authored"](
                    default_wait_time=0., file_writer_config={"video_crf": None})
                self.assertEqual(overridden.default_wait_time, 0.)
                self.assertIsNone(overridden.file_writer.video_crf)
                self.assertEqual(overridden.file_writer.video_tune, "animation")
        self.assertIs(m.manim_config, live)
        self.assertIs(m.manim_config["camera"], camera)
        self.assertEqual(m.manim_config, original)

    def test_documented_typography_defaults_change_native_ink_and_survive_scope(self):
        from fmn_python.scene_loading import SceneSource
        from manimlib.mobject.svg.old_tex_mobject import SingleStringTex
        live = m.manim_config
        text_config, tex_config = live["text"], live["tex"]
        original = deepcopy(live)
        preamble = r"\newcommand{\minus}{+}"
        layers = (
            "style:\n  font: IBM Plex Sans\n  tex_pack: basic\n"
            "output:\n  vcodec: auto\n  pix_fmt: auto\n",
            "text:\n  font: IBM Plex Sans\ntex:\n  template: basic\n"
            "file_writer:\n  video_codec: auto\n  pixel_format: auto\n",
        )
        retained, images = [], []
        camera = m.Camera(resolution=(128, 72), samples=1)
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory, "configured_typography.py")
            path.write_text(
                "import manimlib as m\n"
                "from manimlib.mobject.svg.old_tex_mobject import SingleStringTex\n"
                "TEXT = m.Text('Config', font_size=72, should_center=False)\n"
                "TEX = m.Tex(r'\\minus', additional_preamble=r'\\newcommand{\\minus}{+}', should_center=False)\n"
                "LEGACY = SingleStringTex(r'\\minus', additional_preamble=r'\\newcommand{\\minus}{+}')\n"
                "class Configured(m.Scene): pass\n")
            for layer in layers:
                with SceneSource(path, m.Scene,
                                 configuration=m._resolved_portal_config(None, layer)) as source:
                    text, tex, legacy = source.module.TEXT, source.module.TEX, source.module.LEGACY
                    explicit = m.Text("Config", font="IBM Plex Sans", font_size=72,
                                      should_center=False)
                    baseline = m.Text("Config", font="Computer Modern", font_size=72,
                                      should_center=False)
                    np.testing.assert_array_equal(text.get_all_points(), explicit.get_all_points())
                    png = camera.capture_snapshot(text.copy().center()).png()
                    self.assertEqual(png, camera.capture_snapshot(explicit.copy().center()).png())
                    self.assertNotEqual(png, camera.capture_snapshot(baseline.copy().center()).png())
                    images.append(png)
                    np.testing.assert_allclose(
                        tex.get_all_points(), m.Tex("+", template="basic", should_center=False).get_all_points(),
                        rtol=0, atol=1e-6)
                    np.testing.assert_allclose(
                        legacy.get_all_points(), SingleStringTex("+", template="basic").get_all_points(),
                        rtol=0, atol=1e-6)
                    self.assertEqual((text.font, tex.template, legacy.template), ("", "", ""))
                    # The default pack defines this macro, unlike basic. An
                    # explicit constructor selection must override the file.
                    with self.assertRaises(ValueError):
                        m.Tex(r"\minus", template="default", additional_preamble=preamble)
                    scene = source.scenes["Configured"]()
                    self.assertEqual((scene.file_writer.video_codec, scene.file_writer.pixel_format),
                                     ("auto", "auto"))
                    authored = source.scenes["Configured"](
                        file_writer_config={"video_codec": "ffv1", "pixel_format": "bgra"})
                    self.assertEqual((authored.file_writer.video_codec, authored.file_writer.pixel_format),
                                     ("ffv1", "bgra"))
                    retained.extend((obj, obj.get_all_points().copy()) for obj in (text, tex, legacy))
                self.assertIs(m.manim_config, live)
                self.assertIs(live["text"], text_config)
                self.assertIs(live["tex"], tex_config)
                self.assertEqual(live, original)
        self.assertEqual(images[0], images[1])
        for obj, points in retained:
            for target in (obj, obj.copy()):
                target.init_points()
                np.testing.assert_array_equal(target.get_all_points(), points)

    def test_failed_configured_typography_import_restores_scope(self):
        from fmn_python.scene_loading import SceneSource
        live, original = m.manim_config, deepcopy(m.manim_config)
        text_config, tex_config = live["text"], live["tex"]
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory, "bad_configured_typography.py")
            for layer, statement in (
                ("style:\n  font: definitely-unbundled-font\n", "m.Text('unavailable')"),
                ("style:\n  tex_pack: definitely-unavailable-pack\n", "m.Tex('x')"),
            ):
                path.write_text("import manimlib as m\n" + statement + "\n")
                configuration = m._resolved_portal_config(None, layer)
                with self.assertRaises(ValueError):
                    with SceneSource(path, m.Scene, configuration=configuration):
                        self.fail("invalid typography was accepted")
                self.assertIs(m.manim_config, live)
                self.assertIs(live["text"], text_config)
                self.assertIs(live["tex"], tex_config)
                self.assertEqual(live, original)

    def test_explicit_typography_file_routes_output_with_cli_precedence(self):
        from fmn_python.scene_loading import resolve_render_configuration
        from fmn_python.console_rendering import _output_paths, _single_destination
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            path = root / "render.yml"
            path.write_text(
                "style:\n  font: IBM Plex Sans\n  tex_pack: basic\n"
                "camera:\n  resolution: (80, 40)\n  fps: 8\n"
                "output:\n  vcodec: auto\n  pix_fmt: auto\n"
                f"directories:\n  base: {root}\n  output: configured-frames\n"
                "  mirror_module_path: false\n")
            options = {"config_file": str(path), "format": "png",
                       "_resolution_explicit": True}
            configuration, width, height, fps = resolve_render_configuration(m, options, 96, 54, 12)
            self.assertEqual((width, height, fps), (96, 54, 12))
            self.assertEqual(configuration["values"]["camera"]["fps"], 12)
            self.assertEqual(configuration["values"]["text"]["font"], "IBM Plex Sans")
            self.assertEqual(configuration["values"]["tex"]["template"], "basic")
            selected, exact = _output_paths(options, root / "lesson.py")
            self.assertEqual(selected, root / "configured-frames")
            self.assertEqual(_single_destination(selected, exact, "Lesson", options),
                             root / "configured-frames" / "Lesson.png")
            selected, exact = _output_paths(dict(options, video_dir=str(root / "cli-dir")),
                                            root / "lesson.py")
            self.assertEqual(selected, root / "cli-dir")
            selected, exact = _output_paths(dict(options, output=str(root / "exact.png")),
                                            root / "lesson.py")
            self.assertEqual(_single_destination(selected, exact, "Lesson", options), root / "exact.png")
            with self.assertRaisesRegex(RuntimeError, "certified configuration-file/color provenance"):
                resolve_render_configuration(m, dict(options, reproducible=True), 96, 54, 12)

    def test_manim_config_carries_the_reference_sections(self):
        # fm-e661: manim_config is default_config.yml at the pin, with the
        # camera layer under the cwd custom_config.yml, read once at import
        # (as in the Reference), so the configured case runs in a child.
        config = m.manim_config
        self.assertEqual(dict(config.camera), {"resolution": (1920, 1080), "fps": 30,
                                               "background_color": "#333333",
                                               "background_opacity": 1.0})
        self.assertEqual(dict(config.vmobject), {"default_stroke_width": 4.0,
                                                 "default_stroke_color": "#DDDDDD",
                                                 "default_fill_color": "#888888"})
        self.assertEqual(config.tex.font_size_for_unit_height, 144)
        self.assertEqual(config.text.font_size_for_unit_height, 144)
        self.assertEqual(config.sizes.frame_height, 8.0)
        self.assertEqual(config.colors.blue_c, "#58C4DD")
        self.assertEqual(dict(config.camera), m.Scene().camera_config)
        probe = ("import manimlib as m; c = m.manim_config.camera; "
                 "print(c.background_color, c.fps, tuple(c.resolution))")
        with tempfile.TemporaryDirectory() as directory:
            Path(directory, "custom_config.yml").write_text(
                'camera:\n  background_color: "#000000"\n  fps: 24\n')
            child = subprocess.run([sys.executable, "-c", probe], cwd=directory,
                                   capture_output=True, text=True, timeout=120)
        self.assertEqual(child.returncode, 0, child.stderr)
        self.assertEqual(child.stdout.split(), ["#000000", "24", "(1920,", "1080)"])

    # Measured on the pinned Reference under custom_config.yml camera
    # resolution (1080, 1920) (fm-8azr): constants.py derives every frame
    # constant from the configured resolution. LEFT_SIDE is the portal's C-16
    # unit direction (BN-07); FullScreenRectangle keeps ScreenRectangle's
    # literal 16:9 aspect (frame.py:14), as in the Reference.
    _PORTRAIT_FACTS = {
        "FRAME_WIDTH": 4.5, "constants.FRAME_WIDTH": 4.5, "ASPECT_RATIO": 0.5625,
        "FRAME_X_RADIUS": 2.25, "FRAME_SHAPE": (4.5, 8.0), "DEFAULT_RESOLUTION": (1080, 1920),
        "LEFT_SIDE": (-1.0, 0.0, 0.0), "detached_to_edge_left": -2.25,
        "title_underline_default": 2.5, "full_screen_rectangle_width": 14.2222,
        "screen_rectangle_width": 7.1111, "implicit_x_range": (-2.25, 2.25),
        "bound_to_edge_left": -2.25, "scene_frame_shape": (4.5, 8.0),
        "frame_default_state_shape": (4.5, 8.0), "off_screen_at_x3": True,
    }
    _FRAME_PROBE = (
        "import inspect, manimlib as m\n"
        "from manimlib.constants import FRAME_WIDTH as constants_frame_width\n"
        "r = lambda v: round(float(v), 4)\n"
        "facts = {'FRAME_WIDTH': r(m.FRAME_WIDTH), 'constants.FRAME_WIDTH': r(constants_frame_width),\n"
        "  'ASPECT_RATIO': r(m.ASPECT_RATIO), 'FRAME_X_RADIUS': r(m.FRAME_X_RADIUS),\n"
        "  'FRAME_SHAPE': tuple(r(v) for v in m.FRAME_SHAPE),\n"
        "  'DEFAULT_RESOLUTION': tuple(int(v) for v in m.DEFAULT_RESOLUTION),\n"
        "  'LEFT_SIDE': tuple(r(v) for v in m.LEFT_SIDE),\n"
        "  'detached_to_edge_left': r(m.Square(side_length=1).to_edge(m.LEFT, buff=0).get_left()[0]),\n"
        "  'title_underline_default': r(inspect.signature(m.Title).parameters['underline_width'].default),\n"
        "  'full_screen_rectangle_width': r(m.FullScreenRectangle().get_width()),\n"
        "  'screen_rectangle_width': r(m.ScreenRectangle().get_width()),\n"
        "  'implicit_x_range': tuple(r(v) for v in\n"
        "      inspect.signature(m.ImplicitFunction).parameters['x_range'].default)}\n"
        "scene = m.Scene(); square = m.Square(side_length=1); scene.add(square)\n"
        "square.to_edge(m.LEFT, buff=0)\n"
        "facts['bound_to_edge_left'] = r(square.get_left()[0])\n"
        "facts['scene_frame_shape'] = tuple(r(v) for v in scene.frame.get_shape())\n"
        "scene.frame.set_width(3).to_default_state()\n"
        "facts['frame_default_state_shape'] = tuple(r(v) for v in scene.frame.get_shape())\n"
        "facts['off_screen_at_x3'] = bool(m.Square(side_length=1).move_to(3 * m.RIGHT).is_off_screen())\n"
        "print(repr(facts))\n"
    )

    def test_frame_constants_follow_a_portrait_custom_config(self):
        # Read at import from the cwd custom_config.yml, so the probe runs in
        # a child with that working directory (fm-8azr).
        with tempfile.TemporaryDirectory() as directory:
            Path(directory, "custom_config.yml").write_text(
                "camera:\n  resolution: (1080, 1920)\n"
                f"directories:\n  cache: {Path(directory, 'cache')}\n")
            child = subprocess.run([sys.executable, "-c", self._FRAME_PROBE], cwd=directory,
                                   capture_output=True, text=True, timeout=300)
        self.assertEqual(child.returncode, 0, child.stderr)
        import ast
        facts = ast.literal_eval(child.stdout.strip().splitlines()[-1])
        self.assertEqual(facts, self._PORTRAIT_FACTS)

    def test_a_render_resolution_rebinds_the_frame_for_its_source_and_restores_it(self):
        # A CLI -r reaches the constants through SceneSource, before the scene
        # source runs and only while it is open (fm-8azr).
        import inspect
        from fmn_python.scene_loading import SceneSource
        constants = sys.modules["manimlib.constants"]
        self.assertEqual((m.FRAME_WIDTH, m.DEFAULT_RESOLUTION), (16 * 8 / 9, (1920, 1080)))
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory, "portrait_scene.py")
            path.write_text("from manimlib import *\nWIDTH_AT_IMPORT = FRAME_WIDTH\n"
                            "class Portrait(Scene): pass\n")
            with SceneSource(path, m.Scene, frame_resolution=(1080, 1920)) as source:
                self.assertEqual(source.module.WIDTH_AT_IMPORT, 4.5)
                self.assertEqual((m.FRAME_WIDTH, constants.FRAME_WIDTH, m.FRAME_X_RADIUS),
                                 (4.5, 4.5, 2.25))
                self.assertEqual((m.ASPECT_RATIO, m.DEFAULT_RESOLUTION), (0.5625, (1080, 1920)))
                self.assertEqual(inspect.signature(m.Title).parameters["underline_width"].default, 2.5)
                scene = source.scenes["Portrait"]()
                self.assertEqual(scene.frame.get_shape(), (4.5, 8.0))
                square = m.Square(side_length=1)
                scene.add(square)
                square.to_edge(m.LEFT, buff=0)
                self.assertAlmostEqual(square.get_left()[0], -2.25)
        self.assertEqual((m.FRAME_WIDTH, constants.FRAME_WIDTH), (16 * 8 / 9, 16 * 8 / 9))
        self.assertEqual((m.ASPECT_RATIO, m.DEFAULT_RESOLUTION), (16 / 9, (1920, 1080)))
        self.assertEqual(inspect.signature(m.Title).parameters["underline_width"].default,
                         16 * 8 / 9 - 2)
        self.assertEqual(m.Scene().frame.get_shape(), (16 * 8 / 9, 8.0))

    def test_scene_frame_uses_configured_pose_and_field_of_view(self):
        scene = m.Scene(camera_config={"frame_config": {
            "frame_shape": (10., 6.), "center_point": (2., -1., .5), "fovy": .7,
        }, "resolution": (120, 60)})
        frame = scene.frame
        np.testing.assert_array_equal(frame.get_center(), (2., -1., .5))
        self.assertEqual(frame.get_shape(), (10., 6.))
        self.assertAlmostEqual(frame.get_field_of_view(), .7)
        self.assertIs(scene.camera.frame, frame)
        # The camera keeps the frame height and takes the 2:1 output aspect (BN-25).
        self.assertEqual(frame.get_shape(), (12., 6.))
        np.testing.assert_array_equal(frame.get_center(), (2., -1., .5))

    def test_nested_constructor_config_merges_with_scene_defaults(self):
        class Configured(m.Scene):
            default_camera_config = {
                "resolution": (120, 80),
                "frame_config": {"frame_shape": (9., 5.), "fovy": .6, "euler_axes": "xyz"},
            }
        override = {"frame_config": {"center_point": (3., 2., 0.)}, "fps": 24}
        scene = Configured(camera_config=override)
        self.assertEqual(scene.camera_config["frame_config"]["frame_shape"], (9., 5.))
        self.assertEqual(scene.camera.get_frame_shape(), (7.5, 5.))
        self.assertAlmostEqual(scene.frame.get_field_of_view(), .6)
        self.assertEqual(scene.frame._core.euler_axes(), "xyz")
        np.testing.assert_array_equal(scene.frame.get_center(), (3., 2., 0.))
        self.assertEqual(scene.camera.fps, 24)
        self.assertEqual(override, {"frame_config": {"center_point": (3., 2., 0.)}, "fps": 24})
        self.assertNotIn("center_point", Configured.default_camera_config["frame_config"])

    def test_frame_recipe_dictionary_is_not_shared(self):
        options = {"frame_config": {"frame_shape": (9., 5.)}}
        scene = m.Scene(camera_config=options)
        options["frame_config"]["frame_shape"] = (100., 100.)
        self.assertEqual(scene.camera_config["frame_config"]["frame_shape"], (9., 5.))
        self.assertEqual(scene.frame.get_width(), 9.)
        scene.camera_config["frame_config"]["fovy"] = .8
        self.assertNotIn("fovy", options["frame_config"])

    def test_scene_default_orientation_applies_to_configured_frame(self):
        class Configured(m.Scene):
            default_frame_orientation = (25., 40., 10.)
            default_camera_config = {"frame_config": {"frame_shape": (7., 4.), "center_point": (1., 2., 0.)}}
        scene = Configured()
        expected = m.CameraFrame(frame_shape=(7., 4.), center_point=(1., 2., 0.)).reorient(25., 40., 10.)
        np.testing.assert_allclose(scene.frame.get_orientation(), expected.get_orientation())
        scene.frame.reorient(0, 0, 0).to_default_state()
        np.testing.assert_allclose(scene.frame.get_orientation(), expected.get_orientation())

    def test_lazy_capture_retains_preexisting_frame_edits_and_updaters(self):
        scene = m.Scene(camera_config={"resolution": (120, 80), "frame_config": {"frame_shape": (9., 6.)}})
        frame = scene.frame
        frame.shift(m.RIGHT).scale(.5).reorient(10., 25., 0.)
        updater = lambda mob, dt: None
        frame.add_updater(updater)
        before = frame.copy()
        camera = scene.camera
        self.assertIs(camera.frame, frame)
        self.assertIs(scene.frame, frame)
        np.testing.assert_allclose(frame.get_center(), before.get_center())
        np.testing.assert_allclose(frame.get_orientation(), before.get_orientation())
        self.assertEqual(frame.get_shape(), (4.5, 3.))
        self.assertIn(updater, frame.updaters)
        # The Reference's scene list starts with the frame (fm-6l2i).
        self.assertEqual(scene.mobjects, [frame])

    def test_aspect_ratio_uses_actual_live_height_not_stale_recipe(self):
        scene = m.Scene(camera_config={"resolution": (120, 60), "frame_config": {"frame_shape": (8., 8.)}})
        scene.frame.set_height(5., stretch=True)
        self.assertEqual(scene.camera.get_frame_shape(), (10., 5.))
        scene.frame.set_height(7., stretch=True)
        self.assertIs(scene.camera, scene.camera)
        self.assertEqual(scene.frame.get_height(), 7., "repeated access must not reapply the initial correction")

    def test_explicit_frame_before_camera_is_not_discarded(self):
        scene = m.Scene(camera_config={"resolution": (120, 60)})
        frame = m.CameraFrame(frame_shape=(6., 4.), center_point=(3., 0., 0.), fovy=.8)
        scene.frame = frame
        self.assertIs(scene.camera.frame, frame)
        self.assertEqual(frame.get_shape(), (8., 4.))
        self.assertAlmostEqual(frame.get_field_of_view(), .8)

    def test_invalid_frame_config_is_rejected(self):
        for config in (None, 1, [], "bad"):
            with self.subTest(config=config), self.assertRaisesRegex(TypeError, "frame_config must be a dict"):
                m.Scene(camera_config={"frame_config": config})
        with self.assertRaises(ValueError):
            m.Scene(camera_config={"frame_config": {"frame_shape": (0., 4.)}})

    def test_unsupported_capture_option_still_fails_lazily(self):
        scene = m.Scene(camera_config={"background_image": "missing.png", "frame_config": {"center_point": (1, 0, 0)}})
        np.testing.assert_array_equal(scene.frame.get_center(), (1., 0., 0.))
        with self.assertRaises(NotImplementedError):
            _ = scene.camera
        self.assertNotIn("_camera", vars(scene))
        scene.camera_config.pop("background_image")
        self.assertIs(scene.camera.frame, scene.frame)

    def test_independent_scenes_do_not_share_camera_or_frame(self):
        config = {"resolution": (120, 60), "frame_config": {"center_point": (2., 0., 0.)}}
        first, second = m.Scene(camera_config=config), m.Scene(camera_config=config)
        first.frame.shift(m.RIGHT)
        self.assertIsNot(first.camera, second.camera)
        self.assertIsNot(first.frame._core, second.frame._core)
        np.testing.assert_array_equal(second.frame.get_center(), (2., 0., 0.))

    def test_direct_camera_and_scene_camera_capture_equal_pixels(self):
        config = {"resolution": (96, 54), "samples": 1, "frame_config": {
            "frame_shape": (5., 3.), "center_point": (2., 1., 0.), "fovy": .8,
        }}
        # A Scene's camera also inherits manim_config.camera (#333333); a
        # directly constructed Camera keeps the class default, as in the
        # Reference, so state the scene background for a pose-only comparison.
        direct = m.Camera(**config, background_color="#333333")
        scene = m.Scene(camera_config=config)
        obj = m.VGroup(m.Square(side_length=1.3, fill_color=m.RED, fill_opacity=1).move_to((2., 1., 0.)),
                       m.Triangle(fill_color=m.BLUE, fill_opacity=1).scale(.3).move_to((3., 1.5, 0.)))
        expected = direct.capture_snapshot(obj).png()
        actual = scene.camera.capture_snapshot(obj).png()
        self.assertEqual(actual, expected)
        wrong = m.Camera(resolution=(96, 54), samples=1,
                         background_color="#333333").capture_snapshot(obj).png()
        self.assertNotEqual(actual, wrong, "a discarded configured pose must visibly fail")

    def test_configured_frame_animation_matches_manual_pose_at_one_four_threads(self):
        def render(path, threads, configured, wrong=False):
            options = dict(resolution=(96, 54), fps=4, samples=1)
            if configured:
                options['frame_config'] = dict(frame_shape=(5., 3.), center_point=(2., 1., 0.), fovy=.8)
            scene = m.Scene(camera_config=options)
            # Explicit independent pose construction, not the configuration path.
            if not configured and not wrong:
                camera = scene.camera
                scene.frame.set_height(3., stretch=True).set_width((96/54) * 3., stretch=True)
                scene.frame.move_to((2., 1., 0.)).set_field_of_view(.8)
            with scene.render_session(path, format='png_sequence', resolution=(96, 54), fps=4, threads=threads):
                square = m.Square(side_length=1.3, fill_color=m.RED, fill_opacity=1).move_to((2., 1., 0.))
                scene.add(square)
                scene.play(scene.frame.animate.shift(.5*m.RIGHT), square.animate.shift(.5*m.UP),
                           run_time=.75, rate_func=m.linear)
            return [p.read_bytes() for p in sorted(path.glob('*.png'))]
        with tempfile.TemporaryDirectory(prefix='fmn-camera-config-') as temp:
            root = Path(temp)
            actual = render(root/'one', 1, True)
            self.assertEqual(len(actual), 3)
            self.assertEqual(len(set(actual)), 3)
            self.assertEqual(actual, render(root/'four', 4, True))
            self.assertEqual(actual, render(root/'manual', 1, False))
            self.assertNotEqual(actual, render(root/'wrong', 1, False, True))


    def test_lazy_camera_does_not_evaluate_frame_recipe_twice(self):
        class Once:
            def __init__(self, value):
                self.value, self.calls = value, 0
            def __float__(self):
                self.calls += 1
                if self.calls != 1:
                    raise RuntimeError("frame recipe evaluated twice")
                return self.value
        fovy, height = Once(.8), Once(3.)
        scene = m.Scene(camera_config={"resolution": (96, 54), "samples": 1,
            "frame_config": {"frame_shape": (5., height), "fovy": fovy}})
        frame = scene.frame
        self.assertIs(scene.camera.frame, frame)
        self.assertEqual((fovy.calls, height.calls), (1, 1))
        self.assertEqual(frame.get_height(), 3.)
        self.assertAlmostEqual(frame.get_field_of_view(), .8)

    def test_caller_mutating_consumed_recipe_cannot_poison_capture(self):
        shape = [5., 3.]
        center = np.array([2., 1., 0.])
        scene = m.Scene(camera_config={"resolution": (96, 54), "samples": 1,
            "frame_config": {"frame_shape": shape, "center_point": center, "fovy": .8}})
        shape[:] = [0., float('nan')]
        center[:] = float('nan')
        obj = m.Square(side_length=1., fill_color=m.RED, fill_opacity=1).move_to((2., 1., 0.))
        expected = m.Camera(resolution=(96, 54), samples=1, background_color="#333333",
            frame_config={"frame_shape": (5., 3.), "center_point": (2., 1., 0.), "fovy": .8})
        self.assertEqual(scene.camera.capture_snapshot(obj).png(), expected.capture_snapshot(obj).png())
        np.testing.assert_array_equal(scene.frame.get_center(), (2., 1., 0.))

    def test_retrying_failed_capture_does_not_replay_recipe_callbacks(self):
        calls = []
        class Fovy:
            def __float__(self):
                calls.append("fovy")
                return .8
        scene = m.Scene(camera_config={"resolution": (0, 54),
            "frame_config": {"fovy": Fovy(), "center_point": (2., 1., 0.)}})
        with self.assertRaises(ValueError):
            _ = scene.camera
        self.assertNotIn("_camera", vars(scene))
        self.assertEqual(calls, ["fovy"])
        scene.frame.shift(m.RIGHT)
        scene.camera_config["resolution"] = (96, 54)
        self.assertIs(scene.camera.frame, scene.frame)
        self.assertEqual(calls, ["fovy"])
        np.testing.assert_array_equal(scene.frame.get_center(), (3., 1., 0.))

    def test_consumed_frame_recipe_is_not_used_after_explicit_replacement(self):
        scene = m.Scene(camera_config={"resolution": (96, 54),
            "frame_config": {"frame_shape": (8., 4.)}})
        scene.camera_config["frame_config"] = {"fovy": object()}
        replacement = m.CameraFrame(frame_shape=(6., 4.), center_point=(3., 0., 0.), fovy=.7)
        scene.frame = replacement
        self.assertIs(scene.camera.frame, replacement)
        self.assertEqual(replacement.get_shape(), ((96/54) * 4., 4.))
        self.assertAlmostEqual(replacement.get_field_of_view(), .7)


if __name__ == '__main__':
    unittest.main()
