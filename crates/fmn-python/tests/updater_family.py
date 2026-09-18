"""Durable updater suspension across detached nurseries and scene adoption."""
import manimlib as m


def flags(root):
    return [member._is_updating_suspended() for member in root.get_family()]


def detached_recursive_flags_survive_scene_adoption():
    a, b = m.Square(), m.Circle()
    inner, root = m.Group(a, b), m.Group()
    root.add(inner)
    assert root.suspend_updating() is root
    assert flags(root) == [True] * 4
    assert not any(member._is_bound() for member in root.get_family())
    scene = m.Scene().add(root)
    assert flags(root) == [True] * 4
    assert root.resume_updating(call_updater=False) is root
    assert flags(root) == [False] * 4
    assert scene.mobjects == [root]


def self_only_scope_keeps_descendants_and_callbacks_untouched():
    for bound in (False, True):
        a, b = m.Square(), m.Circle()
        root, ticks = m.Group(a, b), []
        for name, member in (("root", root), ("a", a), ("b", b)):
            member.add_updater(lambda obj, dt, name=name: ticks.append((name, dt)), call=False)
        if bound:
            scene = m.Scene().add(root)
        root.suspend_updating(recurse=False)
        assert flags(root) == [True, False, False]
        root.update(.1)
        assert ticks == []
        root.resume_updating(recurse=False)
        assert ticks == [("root", 0.)], ticks
        assert flags(root) == [False] * 3


def child_resume_clears_ancestors_without_resuming_siblings():
    for bound in (False, True):
        a, b = m.Square(), m.Circle()
        inner, root = m.Group(a, b), m.Group()
        root.add(inner)
        ticks = []
        for name, member in (("root", root), ("inner", inner), ("a", a), ("b", b)):
            member.add_updater(lambda obj, dt, name=name: ticks.append((name, dt)), call=False)
        if bound:
            scene = m.Scene().add(root)
        root.suspend_updating()
        a.resume_updating(recurse=False, call_updater=False)
        assert flags(root) == [False, False, False, True]
        assert ticks == []
        root.update(.25)
        assert ticks == [("a", .25), ("inner", .25), ("root", .25)], ticks


def detached_parent_of_bound_child_is_not_an_invisible_suspension_barrier():
    a, b = m.Square(), m.Circle()
    wrapper = m.Group(a, b)
    scene = m.Scene().add(a)
    assert a._is_bound() and not wrapper._is_bound() and not b._is_bound()
    wrapper.suspend_updating()
    assert flags(wrapper) == [True] * 3
    a.resume_updating(recurse=False, call_updater=False)
    assert flags(wrapper) == [False, False, True]
    assert scene.mobjects == [a]
    assert not wrapper._is_bound() and not b._is_bound()


def shared_descendants_keep_identity_and_resume_runs_one_family_pass():
    for bound in (False, True):
        child = m.Square()
        left, right = m.Group(child), m.Group(child)
        root = m.Group(left, right)
        if bound:
            scene = m.Scene().add(root)
        root.suspend_updating()
        # Public get_family is path-wise: the shared leaf appears twice.
        assert flags(root) == [True] * 5
        root.resume_updating(call_updater=False)
        assert flags(root) == [False] * 5
        assert left[0] is right[0] is child
    # A tree witnesses the existing single child-first update(0) contract.
    a, b, ticks = m.Square(), m.Circle(), []
    root = m.Group(a, b)
    for name, member in (("root", root), ("a", a), ("b", b)):
        member.add_updater(lambda obj, dt, name=name: ticks.append((name, dt)), call=False)
    root.suspend_updating().resume_updating()
    assert ticks == [("a", 0.), ("b", 0.), ("root", 0.)], ticks


def direct_animation_restores_only_the_suspension_it_acquired():
    a, b = m.Line(m.ORIGIN, m.RIGHT), m.Line(m.UP, m.UP + m.RIGHT)
    root, ticks = m.VGroup(a, b), []
    b.suspend_updating()
    a.add_updater(lambda obj, dt: ticks.append(dt), call=False)
    animation = m.Homotopy(lambda x, y, z, t: (x + t, y, z), root,
                          suspend_mobject_updating=True, rate_func=m.linear)
    animation.begin()
    assert flags(root) == [True] * 3
    animation.finish()
    assert flags(root) == [False, False, True]
    assert ticks == [0.], ticks


_CASES = (detached_recursive_flags_survive_scene_adoption,
          self_only_scope_keeps_descendants_and_callbacks_untouched,
          child_resume_clears_ancestors_without_resuming_siblings,
          detached_parent_of_bound_child_is_not_an_invisible_suspension_barrier,
          shared_descendants_keep_identity_and_resume_runs_one_family_pass,
          direct_animation_restores_only_the_suspension_it_acquired)
for _case in _CASES:
    _case()
print(f"updater family acceptance: {len(_CASES)} native cases passed")
