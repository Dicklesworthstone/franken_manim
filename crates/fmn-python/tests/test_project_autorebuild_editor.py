"""Real IPython/source/editor transactions with explicit native lifecycle doubles."""
from __future__ import annotations

import unittest

import test_project_editor as fixtures
from fmn_python.project_autorebuild import _STATE
from fmn_python.source_autoreload import _STATE as _DEFINITIONS


@unittest.skipUnless(fixtures.HAVE_IPYTHON, 'IPython is not installed')
class AutomaticRebuildEditorTests(unittest.TestCase):
    setUp = fixtures.ProjectEditorTests.setUp
    write = fixtures.ProjectEditorTests.write
    source = fixtures.ProjectEditorTests.source
    project = fixtures.ProjectEditorTests.project
    edit = fixtures.ProjectEditorTests.edit
    cell = fixtures.ProjectEditorTests.cell

    def test_enable_preserves_unchanged_console_checkpoints_and_interactive_edits(self):
        with self.project(self.source()) as project:
            def interact(shell):
                console = project._editor._fmn_console
                manager = project._editor.checkpoint_manager
                manager.handle_checkpoint_key(project.scene, '#before')
                self.cell(shell, 'auto_rebuild()\nadd(4)')
                self.cell(shell, 'observed=tuple(scene.mobjects)')
                self.assertEqual(shell.user_ns['observed'], (1, 4))
                self.assertEqual(project.generation, 1)
                self.assertIs(project._editor._fmn_console, console)
                self.assertFalse(console._closed)
                self.assertIn('#before', manager.checkpoint_states)
            self.edit(project, interact)

    def test_source_edit_rebuilds_before_cell_and_keeps_old_aliases(self):
        with self.project(self.source()) as project:
            old = project.scene
            def interact(shell):
                self.cell(shell, 'auto_rebuild()\nold_scene=scene\ncustom=42')
                self.source(7)
                self.cell(shell, 'add(8)\nobserved=VALUE')
                self.assertEqual(project.scene.mobjects, [7, 8])
                self.assertEqual(shell.user_ns['observed'], 7)
                self.assertIs(shell.user_ns['old_scene'], old)
                self.assertEqual(shell.user_ns['custom'], 42)
                self.assertEqual(project.generation, 2)
                self.cell(shell, 'pass')
                self.assertEqual(project.generation, 2)
            self.edit(project, interact)

    def test_failed_candidate_is_retained_reported_once_and_recovers(self):
        with self.project(self.source()) as project:
            old = project.scene
            def interact(shell):
                self.cell(shell, 'auto_rebuild()')
                self.source(2, "        raise ValueError('broken generation')\n")
                self.cell(shell, 'still_working=scene')
                self.assertIs(project.scene, old)
                self.assertIs(shell.user_ns['still_working'], old)
                self.assertEqual(project.generation, 1)
                events = list(self.native.events)
                self.cell(shell, 'pass')
                self.assertEqual(self.native.events, events)
                self.source(6)
                self.cell(shell, 'value=VALUE')
                self.assertEqual(project.scene.mobjects, [6])
                self.assertEqual(shell.user_ns['value'], 6)
                self.assertEqual(project.generation, 2)
            self.edit(project, interact)
            self.assertIn('broken generation', self.captured.getvalue())

    def test_native_capture_failure_retains_previous_scene_module_and_preview(self):
        with self.project(self.source()) as project:
            old, module, image = project.scene, project.module, project.preview
            def interact(shell):
                self.cell(shell, 'auto_rebuild()')
                self.source(4)
                self.native.preview_failure = OSError('capture failed')
                self.cell(shell, 'pass')
                self.native.preview_failure = None
                self.assertIs(project.scene, old)
                self.assertIs(project.module, module)
                self.assertIs(project.preview, image)
                self.source(5)
                self.cell(shell, 'pass')
                self.assertEqual(project.scene.mobjects, [5])
            self.edit(project, interact)

    def test_added_helper_and_explicit_asset_changes_force_reconstruction(self):
        asset = self.write('data.txt', 'one')
        with self.project(self.source()) as project:
            def interact(shell):
                shell.user_ns['asset_path'] = str(asset)
                self.cell(shell, 'auto_rebuild(paths=[asset_path])')
                # The source is unchanged, but this edit occurs before the
                # first watch callback; activation bytes must be remembered.
                asset.write_text('two')
                self.cell(shell, 'pass')
                self.assertEqual(project.generation, 2)
                self.write('new_helper.py', 'VALUE = 9\n')
                self.cell(shell, 'pass')
                self.assertEqual(project.generation, 3)
            self.edit(project, interact)

    def test_modes_are_exclusive_and_switching_back_only_updates_definitions(self):
        with self.project(self.source()) as project:
            def interact(shell):
                self.cell(shell, 'auto_reload()')
                self.cell(shell, 'auto_rebuild()')
                self.assertIsNone(vars(project._editor)[_DEFINITIONS].callback)
                self.source(7)
                self.cell(shell, 'pass')
                self.assertEqual(project.scene.mobjects, [7])
                self.assertEqual(project.generation, 2)
                self.cell(shell, 'auto_reload()')
                self.assertNotIn(_STATE, vars(project._editor))
                self.source(9)
                self.cell(shell, 'value=VALUE')
                self.assertEqual(shell.user_ns['value'], 9)
                self.assertEqual(project.scene.mobjects, [7])
                self.assertEqual(project.generation, 2)
            self.edit(project, interact)

    def test_stopped_and_closed_callbacks_are_inert(self):
        retained = []
        with self.project(self.source()) as project:
            def interact(shell):
                self.cell(shell, 'auto_rebuild()')
                state = vars(project._editor)[_STATE]
                callback = state.callback
                self.cell(shell, 'stop_auto_rebuild()')
                self.source(2)
                callback()
                self.assertTrue(state.closed)
                self.assertEqual(project.generation, 1)
                self.cell(shell, 'auto_rebuild()')
                retained.append(vars(project._editor)[_STATE])
            self.edit(project, interact)
            self.assertTrue(retained[0].closed)
            retained[0].callback()
            self.assertIsNone(retained[0].project)
            self.assertEqual(project.generation, 1)

    def test_conditional_manual_reload_is_a_true_noop(self):
        with self.project(self.source()) as project:
            def interact(shell):
                console = project._editor._fmn_console
                self.cell(shell, 'reload(if_changed=True)\nadd(4)')
                self.assertEqual(project.generation, 1)
                self.assertIs(project._editor._fmn_console, console)
                self.assertFalse(console._closed)
                self.assertEqual(project.scene.mobjects, [1, 4])
                result = shell.run_cell('reload(if_changed=1)')
                self.assertIsInstance(result.error_in_exec, TypeError)
            self.edit(project, interact)

    def test_active_output_does_not_consume_pending_rebuild(self):
        with self.project(self.source()) as project:
            def interact(shell):
                self.cell(shell, 'auto_rebuild()')
                state = vars(project._editor)[_STATE]
                self.source(8)
                vars(project.scene)['_fmn_owned_render_session'] = object()
                try:
                    with self.assertRaises(RuntimeError):
                        state.poll()
                finally:
                    vars(project.scene).pop('_fmn_owned_render_session')
                self.assertTrue(state.poll())
                self.assertEqual(project.scene.mobjects, [8])
            self.edit(project, interact)


if __name__ == '__main__':
    unittest.main()
