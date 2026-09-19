"""Real installed-native project rebuilding in the actual IPython mainloop.

Only terminal interaction is scripted. Scene, checkpoints, geometry, source
loading, shell activation and image capture are production implementations.
"""
from __future__ import annotations

from contextlib import redirect_stdout, redirect_stderr
import io
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from IPython.terminal.embed import InteractiveShellEmbed
import manimlib as m
import numpy as np

from fmn_python import SceneProject


class NativeProjectEditorAcceptance(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="fmn-native-project-edit-")
        self.addCleanup(self.temp.cleanup)
        self.path = Path(self.temp.name) / "scene.py"
        self.output = io.StringIO()

    def source(self, offset=1, fail=False):
        self.path.write_text(
            "from manimlib import *\nfrom fmn_python import embed_scene\n"
            f"OFFSET={offset}\nclass Demo(Scene):\n"
            "    def setup(self):\n        self.camera.reset_pixel_shape(160, 90)\n"
            "    def construct(self):\n"
            "        square=Square(side_length=1, fill_opacity=1, stroke_width=0, color=BLUE)\n"
            "        self.add(square)\n"
            "        self.play(square.animate.shift(OFFSET*RIGHT), run_time=.125)\n"
            + ("        raise ValueError('candidate failed')\n" if fail else "")
            + "        embed_scene(self, locals())\n"
            "        raise AssertionError('code after the edit checkpoint must not execute')\n"
        )

    def edit(self, project, script):
        def interact(shell, *args, **kwargs):
            self.addCleanup(shell.history_manager.end_session)
            script(shell)
        with patch.object(InteractiveShellEmbed, "interact", interact), \
                redirect_stdout(self.output), redirect_stderr(self.output):
            return project.edit()

    def cell(self, shell, source):
        result = shell.run_cell(source)
        self.assertTrue(result.success, self.output.getvalue())
        return result

    def test_same_shell_rebuilds_native_scene_and_rebinds_constructor_locals(self):
        self.source(-1)
        with SceneProject(self.path, "Demo") as project:
            old, before = project.scene, project.preview.pixels()
            points = project.namespace["square"].get_points().copy()
            def script(shell):
                square = shell.user_ns["square"]
                self.assertIs(shell.user_ns["self"], old)
                old_namespace, old_module = shell.user_ns, shell.user_module
                self.source(1)
                self.cell(shell, "reload()\nimage=preview()")
                self.assertIsNot(project.scene, old)
                self.assertIs(shell.user_ns, old_namespace)
                self.assertIs(shell.user_module, old_module)
                self.assertIs(shell.user_ns["self"], project.scene)
                self.assertIsNot(shell.user_ns["square"], square)
                np.testing.assert_allclose(shell.user_ns["square"].get_center(), m.RIGHT, atol=2e-5)
                np.testing.assert_array_equal(square.get_points(), points)
                self.assertNotEqual(shell.user_ns["image"].pixels(), before)
                self.assertEqual(project.scene.time(), old.time())
                self.assertEqual(project.generation, 2)
            self.edit(project, script)
            self.assertNotIn("_fmn_scene_console", vars(old))
            self.assertNotIn("_fmn_scene_console", vars(project.scene))

    def test_failed_edit_preserves_native_checkpoint_then_corrected_edit_resets_history(self):
        self.source(-1)
        with SceneProject(self.path, "Demo") as project:
            old, before = project.scene, project.preview.pixels()
            def script(shell):
                editor = project._editor
                manager = editor.checkpoint_manager
                manager.handle_checkpoint_key(old, "#before")
                point = shell.user_ns["square"].get_center().copy()
                self.source(2, fail=True)
                result = shell.run_cell("reload()")
                self.assertIsInstance(result.error_in_exec, ValueError)
                self.assertIs(project.scene, old)
                self.assertEqual(project.preview.pixels(), before)
                self.assertIn("#before", manager.checkpoint_states)
                np.testing.assert_allclose(shell.user_ns["square"].get_center(), point, atol=2e-5)
                self.source(2)
                self.cell(shell, "reload()")
                self.assertIsNot(editor.checkpoint_manager, manager)
                self.assertEqual(editor.checkpoint_manager.checkpoint_states, {})
                self.assertEqual(manager.checkpoint_states, {})
                np.testing.assert_allclose(shell.user_ns["square"].get_center(), 2*m.RIGHT, atol=2e-5)
            self.edit(project, script)

    def test_definition_autoreload_does_not_reconstruct_until_full_reload(self):
        self.source(-1)
        with SceneProject(self.path, "Demo") as project:
            old = project.scene
            def script(shell):
                self.cell(shell, "auto_reload()")
                self.source(1)
                self.cell(shell, "observed=OFFSET")
                self.assertEqual(shell.user_ns["observed"], 1)
                self.assertIs(project.scene, old)
                self.cell(shell, "reload()")
                self.assertIsNot(project.scene, old)
                np.testing.assert_allclose(shell.user_ns["square"].get_center(), m.RIGHT, atol=2e-5)
                self.source(2)
                self.cell(shell, "observed=OFFSET")
                self.assertEqual(shell.user_ns["observed"], 2)
                np.testing.assert_allclose(shell.user_ns["square"].get_center(), m.RIGHT, atol=2e-5)
                self.cell(shell, "reload()")
                np.testing.assert_allclose(shell.user_ns["square"].get_center(), 2*m.RIGHT, atol=2e-5)
            self.edit(project, script)


suite = unittest.defaultTestLoader.loadTestsFromTestCase(NativeProjectEditorAcceptance)
if suite.countTestCases() != 3:
    raise AssertionError("native project editor selected the wrong test count")
result = unittest.TextTestRunner(verbosity=2).run(suite)
if not result.wasSuccessful():
    raise AssertionError("native project editor acceptance failed")
