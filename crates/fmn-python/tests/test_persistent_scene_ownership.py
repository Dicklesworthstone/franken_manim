"""Persistent ownership admission using production lifecycle and storage doubles.

The shared Animation functions and _Execution are real; this suite checks the
Python/Scene boundary, not native geometry or rendering.
"""
import unittest

from test_animation_updater_protocol import adapter, environment


class PersistentSceneOwnership(unittest.TestCase):
    def setUp(self):
        self.native = environment()
        class ForeignStageError(ValueError):
            pass
        self.native._ForeignStageError = ForeignStageError
        adapter.install_animation_updaters(self.native)
        self.owner, self.foreign = object(), object()
        self.left = self.native.Mobject(-2.)
        self.right = self.native.Mobject(2.)
        self.root = self.native.Mobject(0., self.left, self.right)
        self.animation = self.native.PerMember(self.root)

    def attach(self, cycle=False):
        return self.native.turn_animation_into_updater(self.animation, cycle=cycle)

    def assert_rejected_before_begin(self):
        with self.assertRaisesRegex(self.native._ForeignStageError, "multiple Scenes"):
            self.attach()
        self.assertEqual(self.animation.events, [])
        self.assertFalse(self.root.animating)
        self.assertFalse(hasattr(self.animation, "starting_mobject"))
        self.assertEqual(self.root.updaters, [])
        self.assertEqual((self.left.value, self.right.value), (-2., 2.))

    def test_callback_group_operands_cannot_span_scenes(self):
        self.left._scene, self.right._scene = self.owner, self.foreign
        self.assert_rejected_before_begin()

    def test_root_owner_and_descendant_owner_must_agree(self):
        self.root._scene, self.right._scene = self.owner, self.foreign
        self.assert_rejected_before_begin()

    def test_target_family_is_checked_without_creating_a_target(self):
        self.root._scene = self.owner
        self.animation._target_attr = "target_mobject"
        target_child = self.native.Mobject()
        target_child._scene = self.foreign
        self.animation.target_mobject = self.native.Mobject(0., target_child)
        self.animation.create_target = lambda: self.fail("admission must not create targets")
        self.assert_rejected_before_begin()

    def test_native_extra_operands_are_checked_for_callback_leaves(self):
        self.root._scene = self.owner
        extra = self.native.Mobject()
        extra._scene = self.foreign
        self.animation._native_extra_mobjects = (extra,)
        self.assert_rejected_before_begin()

    def test_detached_callbacks_still_run_without_allocating_a_scene(self):
        self.assertIs(self.attach(), self.root)
        self.root.update(.5).update(0.)
        self.assertEqual((self.left.value, self.right.value), (-1.5, 2.5))
        self.assertIsNone(self.root._scene)
        self.assertEqual(self.animation.total_time, .5)

    def test_single_owner_with_detached_root_preserves_root_identity(self):
        self.left._scene = self.right._scene = self.owner
        self.assertIs(self.attach(), self.root)
        self.assertIsNone(self.root._scene, "callback admission must not adopt or add draw roots")
        self.root.update(1.).update(0.)
        self.assertEqual((self.left.value, self.right.value), (-1., 3.))
        self.assertEqual(self.root.updaters, [])

    def test_same_scene_shared_descendants_are_allowed(self):
        self.root.submobjects.append(self.left)
        self.left._scene = self.right._scene = self.owner
        self.attach()
        self.root.update(.5).update(0.)
        self.assertEqual(self.left.value, -1.5)

    def test_late_split_adoption_aborts_before_advancing_callbacks(self):
        self.attach()
        self.root.update(.25)
        self.left._scene, self.right._scene = self.owner, self.foreign
        events, elapsed = list(self.animation.events), self.animation.total_time
        with self.assertRaisesRegex(self.native._ForeignStageError, "multiple Scenes"):
            self.root.update(.5)
        self.assertEqual(self.animation.events, events)
        self.assertEqual(self.animation.total_time, elapsed)
        self.assertEqual(self.root.updaters, [])
        self.assertFalse(self.root.animating)

    def test_late_single_scene_adoption_remains_live(self):
        self.attach()
        self.left._scene = self.right._scene = self.owner
        self.root.update(.5).update(0.)
        self.assertEqual(self.left.value, -1.5)
        self.assertEqual(self.animation.total_time, .5)

    def test_bound_execution_cannot_silently_switch_to_a_new_scene(self):
        self.root._scene = self.owner
        self.attach()
        self.root._scene = self.foreign
        with self.assertRaisesRegex(self.native._ForeignStageError, "multiple Scenes"):
            self.root.update(.5)
        self.assertEqual(self.animation.total_time, 0.)
        self.assertFalse(self.root.animating)

    def test_foreign_adoption_at_completion_cannot_run_finish(self):
        self.attach()
        self.root.update(1.)
        self.left._scene, self.right._scene = self.owner, self.foreign
        with self.assertRaises(self.native._ForeignStageError):
            self.root.update(0.)
        self.assertNotIn(("finish",), self.animation.events)
        self.assertEqual(self.root.updaters, [])

    def test_cycles_enforce_ownership_without_advancing_their_clock(self):
        self.attach(cycle=True)
        self.root.update(2.25)
        self.left._scene, self.right._scene = self.owner, self.foreign
        with self.assertRaises(self.native._ForeignStageError):
            self.root.update(.5)
        self.assertEqual(self.animation.total_time, 2.25)
        self.assertNotIn(("finish",), self.animation.events)

    def test_admission_failure_preserves_an_unrelated_updater_and_locks(self):
        other = lambda mob, dt: None
        self.root.add_updater(other, call=False)
        locks = self.root.locked_data_keys = {"existing"}
        self.left._scene, self.right._scene = self.owner, self.foreign
        with self.assertRaises(self.native._ForeignStageError):
            self.attach()
        self.assertEqual(self.root.updaters, [other])
        self.assertIs(self.root.locked_data_keys, locks)
        self.assertEqual(locks, {"existing"})

    def test_late_failure_restores_prior_locks_without_rewinding_geometry(self):
        locks = self.root.locked_data_keys = {"existing"}
        self.attach()
        self.root.update(.25).update(0.)
        self.root.locked_data_keys = {"temporary"}
        self.left._scene, self.right._scene = self.owner, self.foreign
        with self.assertRaises(self.native._ForeignStageError):
            self.root.update(.5)
        self.assertEqual(self.left.value, -1.75)
        self.assertIs(self.root.locked_data_keys, locks)
        self.assertEqual(locks, {"existing"})
        self.assertFalse(self.root.animating)

    def test_failed_abort_does_not_mask_the_ownership_failure(self):
        self.attach()
        def abort():
            raise RuntimeError("secondary cleanup failure")
        self.animation.abort = abort
        self.left._scene, self.right._scene = self.owner, self.foreign
        with self.assertRaises(self.native._ForeignStageError) as caught:
            self.root.update(0.)
        self.assertTrue(caught.exception.__notes__)
        self.assertEqual(self.root.updaters, [])
        self.assertFalse(self.root.animating)

    def test_begin_that_changes_ownership_is_unwound_before_registration(self):
        begin = self.animation.begin
        def conflicting_begin():
            begin()
            self.left._scene, self.right._scene = self.owner, self.foreign
        self.animation.begin = conflicting_begin
        with self.assertRaises(self.native._ForeignStageError):
            self.attach()
        self.assertFalse(self.root.animating)
        self.assertEqual(self.root.updaters, [])


if __name__ == "__main__":
    unittest.main()
