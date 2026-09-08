"""Focused acceptance tests for the shared Animation/Transform protocol."""

import math

import numpy as np

from manimlib import (
    Animation,
    AnimationGroup,
    Circle,
    CyclicReplace,
    Group,
    LaggedStart,
    LaggedStartMap,
    Mobject,
    ReplacementTransform,
    Scene,
    ShowCreation,
    Square,
    Succession,
    Swap,
    Transform,
    TransformFromCopy,
    VMobject,
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

native_animation = ShowCreation(VMobject().set_points([[0.0, 0.0, 0.0]]))
assert str(native_animation) == "ShowCreationVMobject"
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
arc_source.uniforms["clip_planes"] = [[0.0] * 4 for _ in range(4)]
arc_target.uniforms["clip_planes"] = [[2.0, 4.0, 6.0, 8.0] for _ in range(4)]
arc_source.uniforms["depth_test"] = True
arc_target.uniforms["depth_test"] = False
arc_source.uniforms["joint_type"] = 2.0
arc_target.uniforms["joint_type"] = 3.0
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
assert np.allclose(arc_source.uniforms["clip_planes"], [[1.0, 2.0, 3.0, 4.0]] * 4)
assert arc_source.uniforms["depth_test"] is True
assert arc_source.uniforms["joint_type"] == 2.0
arc_transform.finish()
assert arc_source.uniforms["depth_test"] is True
assert arc_source.uniforms["joint_type"] == 2.0

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

# The wheel installer must preserve the native composition specification.
# Choreo derives the mobject root at lowering. Public timing is available
# immediately from that same native interval builder, as in the Reference.
composition_children = [Animation(Mobject()), Animation(Mobject())]
for composition_class, duration in ((AnimationGroup, 1.0), (LaggedStart, 1.05), (Succession, 2.0)):
    composition = composition_class(*composition_children)
    assert composition.mobject is None
    assert composition.run_time == composition.max_end_time == duration
    assert composition.get_run_time() == duration
    assert composition.animations == composition_children
    assert str(composition) == composition_class.__name__
mapped_members = Mobject(Mobject(), Mobject())
mapped = LaggedStartMap(Animation, mapped_members)
assert [child.mobject for child in mapped.animations] == list(mapped_members)
for targetless_class in (CyclicReplace, Swap):
    targetless = targetless_class(Mobject(), Mobject())
    assert targetless._native_target() is None

# A Transform subclass's leaf hook must survive the installed dispatch.
class TransformHookProbe(Transform):
    def update_mobjects(self, dt):
        self.seen_dts.append(float(dt))
        super().update_mobjects(dt)

    def interpolate_submobject(self, current, starting, target, alpha):
        self.seen_alphas.append(float(alpha))


hook_probe = TransformHookProbe(Mobject(), Mobject(), rate_func=linear_rate)
hook_probe.seen_alphas = []
hook_probe.seen_dts = []
hook_probe.begin()
hook_probe.interpolate(0.5)
assert hook_probe.seen_alphas == [0.0, 0.5]
hook_probe.finish()

# The same override must run through Scene.play, where native-kind lowering
# previously bypassed Python subclass methods. This hook deliberately leaves
# points alone, so endpoint-only native interpolation is also a negative.
play_hook_source = Mobject().set_points([[0.0, 0.0, 0.0]])
play_hook_target = Mobject().set_points([[1.0, 0.0, 0.0]])
play_hook_probe = TransformHookProbe(
    play_hook_source, play_hook_target, rate_func=linear_rate,
)
play_hook_probe.seen_alphas = []
play_hook_probe.seen_dts = []
Scene().play(play_hook_probe, run_time=2.0 / 30.0)
assert play_hook_probe.seen_alphas, "Scene.play bypassed the Transform subclass hook"
assert any(0.0 < alpha < 1.0 for alpha in play_hook_probe.seen_alphas)
assert play_hook_probe.seen_alphas[0] == 0.0
assert play_hook_probe.seen_alphas[-1] == 1.0
# Plan §9.2: begin interpolates at zero separately; progression samples are
# k/fps for k=1..N, each with the same rational frame delta.
assert np.allclose(play_hook_probe.seen_dts, [1.0 / 30.0, 1.0 / 30.0]), play_hook_probe.seen_dts
assert np.array_equal(play_hook_source.get_points(), [[0.0, 0.0, 0.0]])
assert not play_hook_source.locked_data_keys


class TimelineMove(Animation):
    def __init__(self, mobject, destination, events, **kwargs):
        self.destination = float(destination)
        self.events = events
        self.helper_ticks = []
        super().__init__(mobject, rate_func=linear_rate, **kwargs)

    def begin(self):
        self.events.append(("begin", self.destination, float(self.mobject.get_x())))
        super().begin()

    def update_mobjects(self, dt):
        self.helper_ticks.append(float(dt))
        super().update_mobjects(dt)

    def interpolate_mobject(self, alpha):
        start = float(self.starting_mobject.get_x())
        self.mobject.set_x(start + (self.destination - start) * alpha)

    def finish(self):
        super().finish()
        self.events.append(("finish", self.destination, float(self.mobject.get_x())))


def timeline_point():
    return Mobject().set_points([[0.0, 0.0, 0.0]])


# Changing a group's wall-clock duration preserves its member timeline.
# Compare Python and native leaves at every observed frame, including any
# final refresh, with an independent first-frame value and intermediate state.
for play_override, timeline_rate in (
    (False, None), (True, linear_rate), (False, "linear"), (True, "linear"),
):
    timeline_source, timeline_native, timeline_observer = (
        timeline_point(), timeline_point(), timeline_point()
    )
    mixed_samples = []
    timeline_observer.add_updater(
        lambda mob, dt: mixed_samples.append(
            (timeline_source.get_x(), timeline_native.get_x())
        ) if dt > 0 else None,
        call=False,
    )
    timeline_scene = Scene().add(timeline_source, timeline_native, timeline_observer)
    timeline_group = AnimationGroup(
        TimelineMove(timeline_source, 1.0, [], run_time=0.1),
        Transform(
            timeline_native, timeline_native.copy().set_x(1.0),
            run_time=0.1, rate_func=linear_rate,
        ),
        run_time=-1 if play_override else 0.2,
        rate_func=None if play_override else timeline_rate,
    )
    if play_override:
        timeline_scene.play(timeline_group, run_time=0.2, rate_func=timeline_rate)
    else:
        timeline_scene.play(timeline_group)
    assert len(mixed_samples) >= 6, mixed_samples
    assert np.allclose(mixed_samples[0], [1.0 / 6.0] * 2, atol=1e-6), mixed_samples
    assert np.allclose(
        [pair[0] for pair in mixed_samples],
        [pair[1] for pair in mixed_samples], atol=1e-6,
    ), mixed_samples
    assert any(0.0 < pair[0] < 1.0 for pair in mixed_samples)
    assert np.allclose(mixed_samples[-1], [1.0, 1.0], atol=1e-6)

# A future Succession child must begin from the previous child's final state;
# its helpers must not tick while another child is active.
timeline_source, timeline_observer = timeline_point(), timeline_point()
successive_samples, timeline_events = [], []
timeline_observer.add_updater(
    lambda mob, dt: successive_samples.append(timeline_source.get_x()) if dt > 0 else None,
    call=False,
)
timeline_first = TimelineMove(timeline_source, 1.0, timeline_events, run_time=0.1)
timeline_second = TimelineMove(timeline_source, 2.0, timeline_events, run_time=0.1)
Scene().add(timeline_source, timeline_observer).play(
    Succession(timeline_first, timeline_second, rate_func=linear_rate)
)
assert np.allclose(successive_samples[:6], [k / 3.0 for k in range(1, 7)], atol=1e-6), successive_samples
assert timeline_events == [
    ("begin", 1.0, 0.0), ("finish", 1.0, 1.0),
    ("begin", 2.0, 1.0), ("finish", 2.0, 2.0),
], timeline_events
assert len(timeline_first.helper_ticks) == 3, timeline_first.helper_ticks
assert len(timeline_second.helper_ticks) >= 3, timeline_second.helper_ticks

# Native and Python leaves share the same succession lifecycle, even when
# both mutate one object. The second begin must see the first final state.
for python_first in (True, False):
    mixed_source, mixed_observer = timeline_point(), timeline_point()
    mixed_successive_samples, mixed_successive_events = [], []
    mixed_observer.add_updater(
        lambda mob, dt: mixed_successive_samples.append(mixed_source.get_x())
        if dt > 0 else None, call=False,
    )
    mixed_members = [
        TimelineMove(mixed_source, destination, mixed_successive_events, run_time=0.1)
        if (index == 0) == python_first else Transform(
            mixed_source, mixed_source.copy().set_x(destination),
            run_time=0.1, rate_func=linear_rate,
        )
        for index, destination in enumerate((1.0, 2.0))
    ]
    Scene().add(mixed_source, mixed_observer).play(Succession(*mixed_members))
    assert np.allclose(
        mixed_successive_samples[:6], [k / 3.0 for k in range(1, 7)], atol=1e-6,
    ), (python_first, mixed_successive_samples)
    expected_start = 0.0 if python_first else 1.0
    assert mixed_successive_events[0][2] == expected_start, mixed_successive_events

# Mixed simultaneous leaves also preserve argument order on shared records.
for python_last in (True, False):
    shared_source, shared_observer = timeline_point(), timeline_point()
    shared_samples = []
    shared_observer.add_updater(
        lambda mob, dt: shared_samples.append(shared_source.get_x()) if dt > 0 else None,
        call=False,
    )
    shared_python = TimelineMove(shared_source, 2.0, [], run_time=0.1)
    shared_native = Transform(
        shared_source, shared_source.copy().set_x(1.0), run_time=0.1, rate_func=linear_rate,
    )
    shared_members = (shared_native, shared_python) if python_last else (shared_python, shared_native)
    Scene().add(shared_source, shared_observer).play(AnimationGroup(*shared_members))
    end = 2.0 if python_last else 1.0
    assert np.allclose(shared_samples[:3], [end * k / 3 for k in (1, 2, 3)], atol=1e-6), shared_samples

# A host exception releases a begun native child's locks and suspension
# without snapping the partial frame to its endpoint.
class FailingTimelineMove(TimelineMove):
    def interpolate_mobject(self, alpha):
        if alpha > 0.0:
            raise RuntimeError("mixed-leaf failure witness")
        super().interpolate_mobject(alpha)


failed_source = timeline_point()
failed_scene = Scene().add(failed_source)
try:
    failed_scene.play(AnimationGroup(
        Transform(
            failed_source, failed_source.copy().set_x(1.0), run_time=0.1,
            rate_func=linear_rate, suspend_mobject_updating=True,
        ),
        FailingTimelineMove(failed_source, 2.0, [], run_time=0.1),
    ))
except RuntimeError as error:
    assert str(error) == "mixed-leaf failure witness", error
else:
    raise AssertionError("mixed composition swallowed the host exception")
assert 0.0 < failed_source.get_x() < 1.0, failed_source.get_x()
assert not failed_source.locked_data_keys
assert not failed_source._is_updating_suspended()

# TransformFromCopy's public constructor owns exactly one source copy. The
# native play must remove it after replacement, without an orphaned family.
copy_source = Group(timeline_point(), timeline_point().shift((0.0, 1.0, 0.0)))
copy_target = copy_source.copy().shift((2.0, 0.0, 0.0))
copy_animation = TransformFromCopy(copy_source, copy_target, run_time=0.1, rate_func=linear_rate)
constructor_copy = copy_animation.mobject
assert constructor_copy is not copy_source
copy_scene = Scene().add(copy_source)
copy_scene.play(copy_animation)
assert copy_scene.mobjects == [copy_source, copy_target], copy_scene.mobjects
assert constructor_copy not in copy_scene.get_mobject_family_members()
assert copy_scene._engine_facts()[:2] == (2, 6), copy_scene._engine_facts()
assert np.allclose(copy_source.get_center(), [0.0, 0.5, 0.0])
assert np.allclose(copy_target.get_center(), [2.0, 0.5, 0.0])

# Nested coarse sampling still runs every crossed child in order (BN-11).
coarse_source, coarse_events = timeline_point(), []
Scene().play(AnimationGroup(Succession(*[
    TimelineMove(coarse_source, destination, coarse_events, run_time=0.1)
    for destination in (1.0, 2.0, 3.0)
]), run_time=1.0 / 30.0))
assert coarse_events == [
    ("begin", 1.0, 0.0), ("finish", 1.0, 1.0),
    ("begin", 2.0, 1.0), ("finish", 2.0, 2.0),
    ("begin", 3.0, 2.0), ("finish", 3.0, 3.0),
], coarse_events


def render_animation_lifecycle(destination, seed):
    """Exercise shared hooks and native composition through real PNG output."""
    class AnimationLifecycleScene(Scene):
        random_seed = seed & 0xFFFF_FFFF

        def construct(self):
            square = Square(side_length=0.5).move_to((1.0, 0.0, 0.0))
            target = square.copy().move_to((-1.0, 0.0, 0.0))
            self.add(square)
            morph = Transform(square, target, path_arc=math.pi, rate_func=linear_rate)
            morph.begin()
            morph.interpolate(0.5)
            midpoint = square.get_center()
            assert np.allclose(midpoint, [0.0, 1.0, 0.0], atol=1e-5), midpoint
            self.wait(1.0 / 30.0)
            morph.finish()
            assert np.allclose(square.get_center(), [-1.0, 0.0, 0.0], atol=1e-5)
            assert not square.locked_data_keys
            self.wait(1.0 / 30.0)

            circle = Circle(radius=0.25).move_to((1.0, 0.0, 0.0))
            self.play(
                Succession(
                    Transform(square, square.copy().shift((0.0, -1.0, 0.0)), run_time=0.1),
                    ShowCreation(circle, run_time=0.1),
                    rate_func=linear_rate,
                ),
            )
            assert np.allclose(square.get_center(), [-1.0, -1.0, 0.0], atol=1e-5)
            roots = self.mobjects
            assert len(roots) == 1, roots
            root = roots[0]
            assert root.submobjects == [square, circle]
            assert self.mobjects[0] is root
            assert root in square.parents and root in circle.parents
            assert self.get_mobject_family_members() == [root, square, circle]
            # A projected container is a live Stage identity: changing its
            # Python family must affect subsequent native captures too.
            root.remove(circle)
            assert root.submobjects == [square] and root not in circle.parents
            assert self.get_mobject_family_members() == [root, square]
            root.add(circle)
            assert self.get_mobject_family_members() == [root, square, circle]
            # The published frames include a mixed, same-object succession;
            # the native second child must capture the Python endpoint.
            mixed_positions = []
            circle.add_updater(
                lambda mob, dt: mixed_positions.append(square.get_x()) if dt > 0 else None,
                call=False,
            )
            self.play(Succession(
                TimelineMove(square, 0.0, [], run_time=0.1),
                Transform(square, square.copy().set_x(1.0), run_time=0.1, rate_func=linear_rate),
            ))
            assert np.allclose(
                mixed_positions[:6], [-1.0 + k / 3.0 for k in range(1, 7)], atol=1e-5,
            ), mixed_positions
            circle.clear_updaters()
            self.wait(1.0 / 30.0)

    scene = AnimationLifecycleScene()
    scene._begin_png_sequence(destination, 96, 54, 30, 1, seed)
    try:
        scene.run()
        return scene._finish_render(
            scene.frame._core, scene.camera.light_source.get_center(),
        )
    except Exception:
        scene._abort_render()
        raise
