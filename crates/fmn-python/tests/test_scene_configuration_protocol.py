"""Execute the installed Scene initializer and source scope without a wheel.

Camera/storage doubles stand only at the native boundary. Configuration reads,
recursive merges, source imports and initializer dispatch use production code.
The native camera_configuration suite separately checks native objects/pixels.
"""
from __future__ import annotations

import ast
from copy import deepcopy
import itertools
from pathlib import Path
import sys
import tempfile
from types import ModuleType, SimpleNamespace
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "python"))
from fmn_python.camera_lifecycle import install_scene_camera_configuration
from fmn_python.scene_loading import SceneSource

BOOTSTRAP = ROOT / "python" / "manimlib_bootstrap.py"
TREE = ast.parse(BOOTSTRAP.read_text())


def native_boundary():
    native = ModuleType("manimlib")
    native.__path__ = []

    class Scene:
        random_seed = None
        default_camera_config = {}
        default_file_writer_config = {}
        default_frame_orientation = (0, 0, 0)
        default_wait_time = 1.0
        pan_sensitivity = scroll_sensitivity = 1.0
        drag_to_pan = False
        max_num_saved_states = 50

        @property
        def camera(self):
            """Capture is deliberately lazy."""
            raise AssertionError("the adapter must own this property")

    class Frame:
        def __init__(self, **config):
            self.config = config

        def reorient(self, *orientation):
            self.orientation = orientation

        def make_orientation_default(self):
            self.default_orientation = self.orientation

    def add(scene, frame):
        scene.mobjects = [frame]

    native.Scene, native.CameraFrame = Scene, Frame
    native.Point = object
    native._SceneCore = SimpleNamespace(add=add)
    native._FMN_ROOT = native
    native.manim_config = {
        "camera": {"resolution": (120, 80), "fps": 30,
                   "background_color": "#333333", "background_opacity": 1.0},
        "scene": {"default_wait_time": 1., "preview_while_skipping": True},
        "file_writer": {"video_crf": None},
    }
    native._resolved_camera = lambda _: dict(native.manim_config["camera"])
    # The resolution the engine bound its frame constants to (fm-8azr).
    native._DEFAULT_RESOLUTION = native.manim_config["camera"]["resolution"]
    native._cwd_custom_config = lambda: None
    native._itertools = itertools
    native._pathlib = __import__("pathlib")
    native._sys = sys
    selected = [node for node in TREE.body if isinstance(node, ast.FunctionDef)
                and node.name in {"_portal_configuration_section", "_reference_camera_layer"}]
    utilities = next(node for node in TREE.body if isinstance(node, ast.FunctionDef)
                     and node.name == "_install_reference_utils")
    selected += [node for node in utilities.body if isinstance(node, ast.FunctionDef)
                 and node.name == "merge_dicts_recursively"]
    selected += [node for node in TREE.body if isinstance(node, ast.ClassDef)
                 and node.name == "SceneFileWriter"]
    exec(compile(ast.Module(body=selected, type_ignores=[]), str(BOOTSTRAP), "exec"), vars(native))
    install_scene_camera_configuration(native)
    return native


class SceneConfigurationProtocolTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.native = native_boundary()
        self.config_module = ModuleType("manimlib.config")
        self.config_module.manim_config = self.native.manim_config
        self.native.config = self.config_module
        modules = patch.dict(sys.modules, {"manimlib": self.native, "manimlib.config": self.config_module})
        modules.start()
        self.addCleanup(modules.stop)
        self.path = self.root / "lesson.py"
        self.path.write_text("import manimlib as m\nclass Lesson(m.Scene): pass\n")
        self.configuration = {
            "values": {
                "camera": dict(self.native.manim_config["camera"], fps=24),
                "scene": {"default_wait_time": .25, "preview_while_skipping": False,
                          "show_animation_progress": True, "leave_progress_bars": True},
                "file_writer": {"ffmpeg_bin": "lesson-ffmpeg", "video_crf": 0,
                                "video_preset": "slow", "video_tune": "animation",
                                "audio_bitrate": 192000},
            },
            "overrides": {},
            "explicit_overrides": {},
        }

    def source(self):
        return SceneSource(self.path, self.native.Scene, configuration=self.configuration)

    def test_actual_installer_consumes_scene_and_encoder_layers(self):
        with self.source() as source:
            scene = source.scenes["Lesson"]()
            self.assertEqual(scene.default_wait_time, .25)
            self.assertEqual((scene.preview_while_skipping, scene.show_animation_progress,
                              scene.leave_progress_bars), (False, True, True))
            self.assertEqual(scene.file_writer.ffmpeg_bin, "lesson-ffmpeg")
            self.assertEqual(scene.file_writer.video_crf, 0)
            self.assertEqual(scene.file_writer.video_preset, "slow")
            self.assertEqual(scene.file_writer.video_tune, "animation")
            self.assertEqual(scene.file_writer.audio_bitrate, 192000)
            self.assertIs(scene.mobjects[0], scene.frame)
            self.assertNotIn("_camera", vars(scene))

    def test_scope_installed_on_a_distinct_package_reaches_the_native_reader(self):
        # In the installed wheel the manimlib package is not the native module
        # that defines the reader; SceneSource marks the package (fm-upbk).
        package = ModuleType("manimlib")
        package.__path__ = []
        package.Scene, package.config = self.native.Scene, self.config_module
        package.manim_config = self.native.manim_config
        with patch.dict(sys.modules, {"manimlib": package}):
            with self.source() as source:
                self.assertNotIn("_portal_cli_configuration", vars(self.native))
                scene = source.scenes["Lesson"]()
                self.assertEqual(scene.default_wait_time, .25)
                self.assertEqual(scene.file_writer.video_preset, "slow")
            self.assertNotIn("_portal_cli_configuration", vars(package))
        self.assertEqual(self.native.Scene().default_wait_time, 1.)

    def test_constructor_options_win_including_false_and_zero(self):
        with self.source() as source:
            scene = source.scenes["Lesson"](
                default_wait_time=0, show_animation_progress=False, leave_progress_bars=False,
                preview_while_skipping=True, file_writer_config={"video_crf": None})
            self.assertEqual(scene.default_wait_time, 0)
            self.assertEqual((scene.show_animation_progress, scene.leave_progress_bars,
                              scene.preview_while_skipping), (False, False, True))
            self.assertIsNone(scene.file_writer.video_crf)
            self.assertEqual(scene.file_writer.video_preset, "slow")

    def test_recursive_writer_precedence_does_not_mutate_inputs(self):
        # An authored writer may accept nested extension config; its ordinary
        # fields still pass through the real SceneFileWriter initializer.
        original_writer = self.native.SceneFileWriter
        class AuthoredWriter(original_writer):
            def __init__(self, scene, metadata, **config):
                super().__init__(scene, **config)
                self.metadata = metadata
        self.native.SceneFileWriter = AuthoredWriter
        self.configuration["values"]["file_writer"]["metadata"] = {"owner": "lesson"}
        self.path.write_text(
            "import manimlib as m\n"
            "class Lesson(m.Scene):\n"
            "    default_file_writer_config = {'video_crf': 18, 'metadata': {'class': 7}}\n")
        overrides = {"video_crf": 16, "metadata": {"constructor": 9}}
        with self.source() as source:
            cls = source.scenes["Lesson"]
            snapshot = deepcopy(cls.default_file_writer_config)
            scene = cls(file_writer_config=overrides)
            self.assertEqual(scene.file_writer.video_crf, 16)
            self.assertEqual(scene.file_writer.metadata,
                             {"owner": "lesson", "class": 7, "constructor": 9})
            self.assertEqual(cls.default_file_writer_config, snapshot)
            self.assertEqual(overrides, {"video_crf": 16, "metadata": {"constructor": 9}})

    def test_authored_class_wait_default_wins_even_if_equal_to_base(self):
        self.path.write_text(
            "import manimlib as m\nclass Parent(m.Scene):\n    default_wait_time = 1.0\n"
            "class Lesson(Parent): pass\n")
        with self.source() as source:
            self.assertEqual(source.scenes["Lesson"]().default_wait_time, 1.)
            self.assertEqual(source.scenes["Lesson"](default_wait_time=.75).default_wait_time, .75)

    def test_import_time_edits_reach_later_constructors(self):
        self.path.write_text(
            "import manimlib as m\n"
            "m.manim_config['scene']['default_wait_time'] = .5\n"
            "m.manim_config['file_writer']['video_crf'] = 12\n"
            "class Lesson(m.Scene): pass\n")
        with self.source() as source:
            scene = source.scenes["Lesson"]()
            self.assertEqual((scene.default_wait_time, scene.file_writer.video_crf), (.5, 12))
            self.native.manim_config["file_writer"]["video_crf"] = 8
            self.assertEqual(source.scenes["Lesson"]().file_writer.video_crf, 8)
            self.assertEqual(scene.file_writer.video_crf, 12)

    def test_completed_scope_restores_nested_mapping_identities_and_values(self):
        live = self.native.manim_config
        scene, writer = live["scene"], live["file_writer"]
        original = deepcopy(live)
        with self.source() as source:
            made = source.scenes["Lesson"]()
            self.assertEqual(made.file_writer.video_crf, 0)
            self.native.manim_config["file_writer"]["video_crf"] = 12
            self.native.manim_config["scene"]["default_wait_time"] = 99
        self.assertIs(self.native.manim_config, live)
        self.assertIs(live["scene"], scene)
        self.assertIs(live["file_writer"], writer)
        self.assertIs(self.config_module.manim_config, live)
        self.assertEqual(live, original)
        self.assertEqual(self.native.Scene().default_wait_time, 1.)
        self.assertNotIn("video_crf", vars(self.native.Scene().file_writer))

    def test_constructor_failure_and_interrupt_restore_configuration(self):
        live, original = self.native.manim_config, deepcopy(self.native.manim_config)
        writer = live["file_writer"]
        for error in ("ValueError('construction')", "KeyboardInterrupt()"):
            with self.subTest(error=error):
                self.path.write_text(
                    "import manimlib as m\nclass Lesson(m.Scene):\n"
                    "    def __init__(self):\n        super().__init__()\n"
                    "        assert self.file_writer.video_crf == 0\n"
                    "        m.manim_config['file_writer']['video_crf'] = 12\n"
                    "        raise " + error + "\n")
                with self.assertRaises((ValueError, KeyboardInterrupt)):
                    with self.source() as source:
                        source.scenes["Lesson"]()
                self.assertEqual(live, original)
                self.assertIs(live["file_writer"], writer)
                self.assertNotIn("_portal_cli_configuration", vars(self.native))

    def test_bad_writer_override_is_named_before_writer_creation(self):
        with self.source() as source:
            with self.assertRaisesRegex(TypeError, "file_writer_config must be a dict"):
                source.scenes["Lesson"](file_writer_config=[])

    def test_writer_retains_explicit_quality_values_for_native_validation(self):
        class NoConversion:
            def __int__(self):
                raise AssertionError("integer conversion belongs to native admission")
            def __str__(self):
                raise AssertionError("string conversion belongs to native admission")
        value = NoConversion()
        scene = self.native.Scene(file_writer_config={"video_crf": value, "video_preset": value,
                                                     "video_tune": None, "audio_bitrate": value})
        self.assertIs(scene.file_writer.video_crf, value)
        self.assertIs(scene.file_writer.video_preset, value)
        self.assertIs(scene.file_writer.audio_bitrate, value)
        self.assertIsNone(scene.file_writer.video_tune)
        with self.assertRaisesRegex(TypeError, "unexpected keyword arguments: misspelled_quality"):
            self.native.Scene(file_writer_config={"misspelled_quality": 16})

    def test_unscoped_scene_keeps_existing_constructor_defaults(self):
        # A config mapping alone is not an authorization to change the audited
        # unscoped/certified behavior. Only SceneSource's configured scope opts in.
        self.native.manim_config["scene"]["default_wait_time"] = 9
        self.native.manim_config["file_writer"]["video_crf"] = 12
        scene = self.native.Scene()
        self.assertEqual(scene.default_wait_time, 1.)
        self.assertEqual(scene.file_writer_config, {})


if __name__ == "__main__":
    unittest.main()
