"""Focused acceptance tests for the shared Animation/Transform protocol."""

import math

import numpy as np

from manimlib import (
    Animation,
    Mobject,
    ReplacementTransform,
    ShowCreation,
    Transform,
    TransformFromCopy,
)


class AnimationProbe(Animation):
    def __init__(self, mobject, **kwargs):
        self.calls = []
        super().__init__(mobject, **kwargs)

    def interpolate_submobject(self, current, starting, alpha):
        self.calls.append((current, starting, float(alpha)))


assert str(Mobject()) == "Mobject"
try:
    Animation(None)
except TypeError as error:
    assert str(error) == "Animation only works for Mobjects."
else:
    raise AssertionError("Animation accepted a non-Mobject target")
assert str(Animation(Mobject())) == "AnimationMobject"

root = Mobject(Mobject(), Mobject())
square_rate = lambda value: value * value
animation = AnimationProbe(
    root,
    run_time=2.0,
    lag_ratio=0.5,
    rate_func=square_rate,
    name="probe",
)
assert str(animation) == "probe"
assert animation.get_run_time() == 2.0
assert animation.get_rate_func() is square_rate
animation.begin()
assert root.is_changing()
assert animation.starting_mobject is not root
assert len(animation.families) == 3
assert all(len(row) == 2 for row in animation.families)
assert [row[0] for row in animation.families] == root.get_family()
assert [row[1] for row in animation.families] == animation.starting_mobject.get_family()

animation.calls.clear()
assert animation.interpolate(0.5) is None
assert [call[2] for call in animation.calls] == [1.0, 0.25, 0.0]
animation.calls.clear()
assert animation.update(0.25) is None
assert [call[2] for call in animation.calls] == [0.25, 0.0, 0.0]

helper_ticks = []
live_ticks = []
helper_updater = lambda mob, dt=0: helper_ticks.append(float(dt))
live_updater = lambda mob, dt=0: live_ticks.append(float(dt))
animation.starting_mobject.add_updater(helper_updater, call=False)
root.add_updater(live_updater, call=False)
assert animation.get_all_mobjects_to_update() == [animation.starting_mobject]
animation.update_mobjects(0.125)
assert helper_ticks == [0.125]
assert live_ticks == []
root.remove_updater(live_updater)

original_get_all = animation.get_all_mobjects
animation.get_all_mobjects = lambda: (
    animation.mobject,
    animation.starting_mobject,
    animation.starting_mobject,
)
assert animation.get_all_mobjects_to_update() == [animation.starting_mobject]
animation.get_all_mobjects = original_get_all

clone = animation.copy()
assert clone is not animation
assert clone.mobject is not animation.mobject
linear_rate = lambda value: value
assert animation.update_rate_info(4.0, linear_rate, 0.75) is animation
assert (animation.run_time, animation.lag_ratio) == (4.0, 0.75)
assert animation.rate_func is linear_rate
animation.update_rate_info(0, None, 0)
assert (animation.run_time, animation.lag_ratio) == (4.0, 0.75)
assert animation.set_run_time(3.0) is animation
assert animation.set_rate_func(square_rate) is animation
assert animation.set_name("renamed") is animation
assert str(animation) == "renamed"
animation.finish()
assert not root.is_changing()

suspended_root = Mobject()
suspended = AnimationProbe(
    suspended_root,
    suspend_mobject_updating=True,
)
suspended.begin()
assert suspended_root._is_updating_suspended()
suspended.finish()
assert not suspended_root._is_updating_suspended()
assert not suspended_root.is_changing()

spanned = AnimationProbe(
    Mobject(),
    run_time=1.0,
    time_span=(0.25, 0.75),
)
assert spanned.time_spanned_alpha(0.0) == 0.0
assert spanned.time_spanned_alpha(0.5) == 0.5
assert spanned.time_spanned_alpha(1.0) == 1.0

native_animation = ShowCreation(Mobject().set_points([[0.0, 0.0, 0.0]]))
assert str(native_animation) == "ShowCreationMobject"
assert native_animation.is_remover() is False

source = Mobject().set_points([[0.0, 0.0, 0.0]])
target = Mobject().set_points([
    [0.0, 0.0, 0.0],
    [1.0, 0.0, 0.0],
    [2.0, 0.0, 0.0],
])
transform = Transform(
    source,
    target,
    run_time=1.0,
    rate_func=linear_rate,
)
transform.begin()
assert source.is_changing()
assert transform.target_copy is not target
assert source.get_num_points() == 3
assert transform.starting_mobject.get_num_points() == 3
assert len(transform.families) == 1
assert transform.families[0] == (
    source,
    transform.starting_mobject,
    transform.target_copy,
)
assert "rgba" in source.locked_data_keys
transform.interpolate(0.5)
assert np.allclose(source.get_points()[-1], [1.0, 0.0, 0.0])

transform_ticks = []
source_ticks = []
source_updater = lambda mob, dt=0: source_ticks.append(float(dt))
source.add_updater(source_updater, call=False)
for label, helper in (
    ("start", transform.starting_mobject),
    ("target", target),
    ("target_copy", transform.target_copy),
):
    helper.add_updater(
        lambda mob, dt=0, helper_label=label: transform_ticks.append(
            (helper_label, float(dt))
        ),
        call=False,
    )
assert transform.get_all_mobjects_to_update() == [
    transform.starting_mobject,
    target,
    transform.target_copy,
]
transform.update_mobjects(0.2)
assert transform_ticks == [
    ("start", 0.2),
    ("target", 0.2),
    ("target_copy", 0.2),
]
assert source_ticks == []
source.remove_updater(source_updater)
transform.finish()
assert not source.is_changing()
assert source.locked_data_keys == set()

arc_source = Mobject().set_points([[1.0, 0.0, 0.0]])
arc_target = Mobject().set_points([[-1.0, 0.0, 0.0]])
arc_source.set_color("#000000")
arc_target.set_color("#FFFFFF")
arc_source.set_shading(0.0, 0.0, 0.0)
arc_target.set_shading(1.0, 0.5, 0.25)
arc_transform = Transform(
    arc_source,
    arc_target,
    path_arc=math.pi,
    run_time=1.0,
    rate_func=linear_rate,
)
arc_transform.begin()
assert arc_transform.target_copy is arc_target
assert arc_transform.get_all_mobjects_to_update() == [
    arc_transform.starting_mobject,
    arc_target,
]
arc_transform.interpolate(0.5)
midpoint = arc_source.get_points()[0]
assert abs(float(midpoint[0])) < 1e-5
assert abs(abs(float(midpoint[1])) - 1.0) < 1e-5
assert np.allclose(
    arc_source.data["rgba"][0, :3],
    [0.5, 0.5, 0.5],
    atol=1e-6,
)
assert np.allclose(
    arc_source.get_shading(),
    [0.5, 0.25, 0.125],
    atol=1e-6,
)
arc_transform.finish()

copy_source = Mobject().set_points([[0.0, 0.0, 0.0]])
copy_target = Mobject().set_points([[1.0, 0.0, 0.0]])
copy_transform = TransformFromCopy(copy_source, copy_target)
assert copy_transform.mobject is not copy_source
assert np.array_equal(copy_transform.mobject.get_points(), copy_source.get_points())
assert ReplacementTransform.replace_mobject_with_target_in_scene
assert TransformFromCopy.replace_mobject_with_target_in_scene


class SceneProbe:
    def __init__(self):
        self.added = []
        self.removed = []

    def add(self, *mobjects):
        self.added.extend(mobjects)

    def remove(self, *mobjects):
        self.removed.extend(mobjects)


replacement_source = Mobject()
replacement_target = Mobject()
replacement = ReplacementTransform(replacement_source, replacement_target)
replacement.target_mobject = replacement_target
scene_probe = SceneProbe()
replacement.clean_up_from_scene(scene_probe)
assert scene_probe.removed == [replacement_source]
assert scene_probe.added == [replacement_target]
