"""Public recursive updates against native records, lifecycle and rendered frames."""
from pathlib import Path
import copy
import gc
import tempfile
import unittest
import weakref

import numpy as np
import manimlib as m


class MovingSquare(m.Square):
    def update(self, dt=0):
        self.shift(float(dt) * m.RIGHT)
        return super().update(dt)


class CopyMotion(m.Animation):
    def interpolate_submobject(self, current, starting, alpha):
        current.set_points(starting.get_points())


class LiteralCopyMotion(CopyMotion):
    def update_mobjects(self, dt):
        self.starting_mobject.shift(float(dt) * m.RIGHT)


def render_copy_motion(path, authored, threads):
    scene = m.Scene()
    leaf = MovingSquare() if authored else m.Square()
    root = m.Group(leaf)
    animation = (CopyMotion if authored else LiteralCopyMotion)(
        root, run_time=.125, rate_func=m.linear, suspend_mobject_updating=True)
    with scene.render_session(path, resolution=(64, 40), fps=24, threads=threads):
        scene.play(animation)
    return path.read_bytes(), leaf.get_points()


class PublicUpdateTests(unittest.TestCase):
    def test_nested_dt_only_override_changes_native_geometry(self):
        for bound in (False, True):
            with self.subTest(bound=bound):
                child = MovingSquare()
                root = m.Group(m.Group(child))
                before = child.get_points().copy()
                if bound:
                    scene = m.Scene().add(root)
                self.assertIs(root.update(.25), root)
                np.testing.assert_array_equal(child.get_points(), before + .25 * m.RIGHT)
                self.assertIs(root[0][0], child)

    def test_virtual_entry_super_and_exit_order(self):
        events = []
        class Child(m.Square):
            def update(self, dt):
                events.append(('enter', dt))
                super().update(dt)
                events.append(('exit', dt))
                return self
        child = Child()
        child.add_updater(lambda obj, dt: events.append(('leaf', dt)), call=False)
        root = m.Group(child)
        root.add_updater(lambda obj, dt: events.append(('root', dt)), call=False)
        root.update(.5)
        self.assertEqual(events, [('enter', .5), ('leaf', .5), ('exit', .5), ('root', .5)])

    def test_native_phase_runs_once_child_first_after_all_host_callbacks(self):
        events = []
        class Probe:
            def _update_native_mobject(self, dt, recurse):
                events.append(('native', self.label, dt, recurse))
                return super()._update_native_mobject(dt, recurse)
        class Child(Probe, m.Square):
            def update(self, dt):
                return super().update(2 * dt)
        class Root(Probe, m.Group):
            pass
        child = Child(); child.label = 'child'
        root = Root(child); root.label = 'root'
        for obj in (root, child):
            obj.add_updater(lambda obj, dt: events.append(('host', obj.label, dt)), call=False)
        m.Scene().add(root)
        root.update(.25)
        self.assertEqual(events, [('host', 'child', .5), ('host', 'root', .25),
                                  ('native', 'child', .5, False), ('native', 'root', .25, False)])

    def test_ordinary_family_keeps_one_native_crossing(self):
        events = []
        class Root(m.Group):
            def _update_native_mobject(self, dt, recurse):
                events.append((dt, recurse))
                return super()._update_native_mobject(dt, recurse)
        root = Root(m.Group(m.Square(), m.Circle()))
        root.update(.25)
        self.assertEqual(events, [(.25, True)])

    def test_override_selects_nonrecursive_super_for_both_phases(self):
        events = []
        class Child(m.Group):
            def update(self, dt):
                return super().update(dt, recurse=False)
        grandchild = MovingSquare()
        grandchild.add_updater(lambda obj, dt: events.append('grandchild'), call=False)
        child = Child(grandchild)
        child.add_updater(lambda obj, dt: events.append('child'), call=False)
        root = m.Group(child)
        before = grandchild.get_points().copy()
        root.update(.25)
        self.assertEqual(events, ['child'])
        np.testing.assert_array_equal(grandchild.get_points(), before)

    def test_override_omitting_super_owns_its_subtree(self):
        events = []
        class Child(m.Group):
            def update(self, dt):
                events.append(('override', dt))
                return self
            def _update_native_mobject(self, dt, recurse):
                raise AssertionError('an omitted base call must not receive native slots')
        child = Child(MovingSquare())
        child.add_updater(lambda obj, dt: events.append('base'), call=False)
        m.Group(child).update(.1)
        self.assertEqual(events, [('override', .1)])
        np.testing.assert_array_equal(child[0].get_center(), m.ORIGIN)

    def test_shared_children_keep_pathwise_public_visits(self):
        child = MovingSquare()
        root = m.Group(m.Group(child), m.Group(child))
        root.update(.125)
        np.testing.assert_array_equal(child.get_center(), .25 * m.RIGHT)
        self.assertIs(root[0][0], root[1][0])

    def test_suspension_and_self_only_scope_preserve_pruning(self):
        child = MovingSquare()
        root = m.Group(child)
        root.update(.2, recurse=False)
        root.suspend_updating(recurse=False).update(.2)
        np.testing.assert_array_equal(child.get_center(), m.ORIGIN)
        root.resume_updating(recurse=False, call_updater=False).update(.25)
        np.testing.assert_array_equal(child.get_center(), .25 * m.RIGHT)

    def test_updater_snapshot_is_taken_at_each_nodes_turn(self):
        events = []
        def later(obj, dt): events.append('later')
        class Child(m.Square):
            def update(self, dt):
                root.add_updater(later, call=False)
                return super().update(dt)
        child = Child()
        root = m.Group(child)
        def initial(obj, dt):
            events.append('initial')
            root.remove_updater(later)
        root.add_updater(initial, call=False)
        root.update(.1)
        self.assertEqual(events, ['initial', 'later'])

    def test_children_snapshot_survives_family_edits(self):
        events = []
        class Child(m.Square):
            def update(self, dt):
                events.append(self.label)
                if self.label == 'a':
                    root.remove(b); root.add(c)
                return super().update(dt)
        a, b, c = Child(), Child(), Child()
        a.label, b.label, c.label = 'a', 'b', 'c'
        root = m.Group(a, b)
        root.update(.1)
        self.assertEqual(events, ['a', 'b'])
        events.clear(); root.update(.1)
        self.assertEqual(events, ['a', 'c'])

    def test_late_instance_and_class_updates_are_live(self):
        events = []
        child = m.Square(); root = m.Group(child)
        child.update = lambda dt: events.append(('instance', dt))
        root.update(.2)
        class Child(m.Square): pass
        child = Child(); root = m.Group(child)
        Child.update = lambda self, dt: events.append(('class', dt))
        root.update(.3)
        self.assertEqual(events, [('instance', .2), ('class', .3)])

    def test_authored_failure_preserves_exception_and_releases_pending_work(self):
        events = []; failure = RuntimeError('child update failed')
        class Child(m.Square):
            def update(self, dt):
                super().update(dt)
                if dt > 0: raise failure
                return self
            def _update_native_mobject(self, dt, recurse):
                events.append(dt)
                return super()._update_native_mobject(dt, recurse)
        child = Child(); root = m.Group(child)
        with self.assertRaises(RuntimeError) as caught: root.update(.1)
        self.assertIs(caught.exception, failure)
        self.assertEqual(events, [])
        child.update(0)
        self.assertEqual(events, [0])
        root.update(0)
        self.assertEqual(events, [0, 0])

    def test_explicit_nested_update_is_an_independent_invocation(self):
        events = []
        class Child(m.Square):
            def _update_native_mobject(self, dt, recurse):
                events.append(('native', dt))
                return super()._update_native_mobject(dt, recurse)
        child = Child()
        def tick(obj, dt):
            events.append(('host', dt))
            if dt: obj.update(0)
        child.add_updater(tick, call=False)
        m.Group(child).update(.5)
        self.assertEqual(events, [('host', .5), ('host', 0.), ('native', 0.), ('native', .5)])

    def test_add_updater_and_resume_run_one_public_family_pass(self):
        events = []
        class Child(m.Square):
            def update(self, dt):
                events.append(dt)
                return super().update(dt)
        root = m.Group(Child())
        root.add_updater(lambda obj: None)
        self.assertEqual(events, [0.])
        root.suspend_updating().resume_updating()
        self.assertEqual(events, [0., 0.])

    def test_private_host_only_entry_does_not_run_native_slots(self):
        events = []
        class Child(m.Square):
            def update(self, dt):
                events.append(dt); return super().update(dt)
            def _update_native_mobject(self, dt, recurse):
                raise AssertionError('private host-only traversal ran native slots')
        m.Group(Child())._update_python_family(.25, True)
        self.assertEqual(events, [.25])

    def test_nested_private_host_pass_does_not_join_native_queue(self):
        events = []
        class Child(m.Square):
            def _update_native_mobject(self, dt, recurse):
                events.append(dt)
                return super()._update_native_mobject(dt, recurse)
        child = Child()
        other = m.Group(child)
        root = m.Group()
        root.add_updater(lambda obj, dt: other._update_python_family(dt, True), call=False)
        root.update(.25)
        self.assertEqual(events, [])
        other.update(.5)
        self.assertEqual(events, [.5])

    def test_copies_and_collection_do_not_retain_invocation_state(self):
        child = MovingSquare(); root = m.Group(child)
        root.add_updater(lambda obj, dt: copy.deepcopy(obj), call=False)
        root.update(.125)
        duplicate = root.copy()
        duplicate.update(.125)
        np.testing.assert_array_equal(child.get_center(), .125 * m.RIGHT)
        np.testing.assert_array_equal(duplicate[0].get_center(), .25 * m.RIGHT)
        references = [weakref.ref(obj) for obj in (root, child, duplicate)]
        del root, child, duplicate
        gc.collect()
        self.assertTrue(all(ref() is None for ref in references))

    def test_animation_helper_updates_reach_descendant_override(self):
        root = m.Group(MovingSquare())
        animation = CopyMotion(root, rate_func=m.linear)
        animation.begin()
        animation.update_mobjects(.25)
        animation.interpolate(.5)
        np.testing.assert_array_equal(root[0].get_center(), .25 * m.RIGHT)
        animation.finish()

    def test_scene_public_update_mobjects_dispatches_children(self):
        child = MovingSquare(); scene = m.Scene().add(m.Group(child))
        scene.update_mobjects(.25)
        np.testing.assert_array_equal(child.get_center(), .25 * m.RIGHT)

    def test_rendered_snapshot_updates_match_literal_control(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)
            actual, points = render_copy_motion(path / 'one.y4m', True, 1)
            for threads in (4, 16):
                self.assertEqual(actual, render_copy_motion(path / f'{threads}.y4m', True, threads)[0])
            expected, expected_points = render_copy_motion(path / 'expected.y4m', False, 1)
            self.assertEqual(actual, expected)
            np.testing.assert_array_equal(points, expected_points)
            frames = actual.split(b'FRAME\n')[1:]
            self.assertEqual(len(frames), 3)
            self.assertNotEqual(frames[0], frames[1])
            self.assertNotEqual(frames[1], frames[2])


if __name__ == '__main__':
    unittest.main()
