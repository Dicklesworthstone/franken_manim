"""Installed-wheel scene snapshots through native records, cameras and output.

No storage doubles are used here. Run via scripts/check_portal_runtime.sh in
an environment containing a wheel built from the same checkout.
"""
from __future__ import annotations

import copy
from pathlib import Path
import tempfile

import numpy as np
import manimlib as m
from fmn_python import render_scene
from manimlib.scene.scene import SceneState as QualifiedState


def close(actual, expected):
    np.testing.assert_allclose(actual, expected, atol=2e-5, rtol=1e-6)


def captured_families_styles_and_arrays():
    assert QualifiedState is m.SceneState
    scene = m.Scene()
    child = m.Square(fill_opacity=1, fill_color=m.WHITE)
    group = m.Group(child)
    group.uniforms["scene_gain"] = np.array([1., 2.])
    scene.add(group)
    state = scene.get_state()
    scene.undo_stack.append(state)
    mirror = state.mobjects_to_copies[group]
    saved_points = mirror[0].get_points().copy()
    assert len(group.data) == 0
    child.shift(m.RIGHT).set_fill(m.RED, opacity=.5)
    group.uniforms["scene_gain"][:] = 9
    changed = scene.get_state()
    assert not state.mobjects_match(changed)
    assert state.n_changes(changed) == 1
    assert changed.mobjects_to_copies[group] is not mirror
    np.testing.assert_array_equal(mirror[0].get_points(), saved_points)
    np.testing.assert_array_equal(mirror.uniforms["scene_gain"], [1., 2.])
    # Comparing two captures must not depend on later changes to their sources.
    before = state.n_changes(changed)
    child.shift(100 * m.UP)
    assert state.n_changes(changed) == before


def shared_child_identity():
    scene, shared = m.Scene(), m.Square()
    left, right = m.Group(shared), m.Group(shared)
    root = m.Group(left, right)
    scene.add(root)
    state = scene.get_state()
    replacement = m.Circle()
    left.set_submobjects([replacement])
    right.set_submobjects([])
    shared.shift(3 * m.RIGHT)
    state.restore_scene(scene)
    assert root[0] is left and root[1] is right
    assert left[0] is shared and right[0] is shared
    assert left in shared.parents and right in shared.parents
    assert left not in replacement.parents
    close(shared.get_center(), m.ORIGIN)


def updater_restoration():
    scene, square, events = m.Scene(), m.Square(), []
    def original(obj, dt):
        events.append((obj, dt))
    square.add_updater(original, call=False)
    square.uniforms["state_array"] = np.array([2., 4.])
    scene.add(square)
    state = scene.get_state()
    square.updaters[:] = [lambda obj: events.append("wrong updater")]
    square.uniforms["state_array"][:] = 99
    state.restore_scene(scene)
    assert len(square.updaters) == 1 and square.updaters[0] is original
    scene.update(0.)
    assert events and all(isinstance(event, tuple) and event[0] is square for event in events)
    np.testing.assert_array_equal(square.uniforms["state_array"], [2., 4.])
    square.uniforms["state_array"][:] = 7
    state.restore_scene(scene)
    np.testing.assert_array_equal(square.uniforms["state_array"], [2., 4.])


def ordered_roots():
    scene = m.Scene()
    left, right = m.Square(), m.Circle()
    scene.add(left, right)
    state = scene.get_state()
    scene.clear()
    scene.add(right, left)
    assert not state.mobjects_match(scene.get_state())
    state.restore_scene(scene)
    assert len(scene.mobjects) == 2
    assert scene.mobjects[0] is left and scene.mobjects[1] is right


def camera_pose_and_callbacks():
    scene = m.Scene()
    frame, core, events = scene.frame, scene.frame._core, []
    core.set_orientation((0., 0., 1., 0.))
    core.make_orientation_default()
    core.set_orientation((0., 1., 0., 0.))
    core.set_euler_axes("zxy")
    core.set_center((2., -1., 3.))
    core.set_shape((12., 7.))
    core.set_field_of_view(.6)
    def rotate(obj, dt):
        events.append(dt)
    frame.add_updater(rotate, call=False)
    expected = (tuple(core.center()), core.shape(), tuple(core.orientation()), core.field_of_view())
    state = scene.get_state()
    frame.shift(3 * m.RIGHT).scale(.5).set_theta(.7)
    core.set_euler_axes("zxz")
    core.set_field_of_view(.9)
    core.make_orientation_default()
    frame.updaters.clear()
    assert not state.mobjects_match(scene.get_state())
    state.restore_scene(scene)
    assert scene.frame is frame and frame._core is core
    for actual, saved in zip((core.center(), core.shape(), core.orientation(), core.field_of_view()), expected):
        close(actual, saved)
    assert core.euler_axes() == "zxy"
    restored_default = copy.copy(core)
    restored_default.to_default_state()
    close(restored_default.orientation(), (0., 0., 1., 0.))
    assert len(frame.updaters) == 1 and frame.updaters[0] is rotate
    assert all(obj is not frame for obj in scene.mobjects)


def native_clock_and_bytes():
    scene, square = m.Scene(), m.Square()
    scene.add(square)
    scene.wait(.125)
    scene.num_plays = 7
    state = scene.get_state()
    assert state._checkpoint
    before = scene.get_time()
    scene.wait(.125)
    scene.num_plays = 99
    square.shift(3 * m.RIGHT)
    state.restore_scene(scene)
    assert scene.get_time() == before and scene.num_plays == 7
    close(square.get_center(), m.ORIGIN)
    assert bytes(scene._checkpoint_bytes()) == state._checkpoint


def repeated_history():
    scene, square = m.Scene(), m.Square()
    scene.add(square)
    frame, core = scene.frame, scene.frame._core
    for x in (0., 1., 2.):
        square.set_x(x)
        frame.set_x(x / 2)
        scene.save_state()
    square.set_x(3.)
    frame.set_x(1.5)
    for x in (2., 1., 0.):
        scene.undo()
        close(square.get_x(), x)
        close(frame.get_x(), x / 2)
    for x in (1., 2., 3.):
        scene.redo()
        close(square.get_x(), x)
        close(frame.get_x(), x / 2)
    assert scene.frame is frame and frame._core is core


def history_branch_and_limit():
    scene, square = m.Scene(), m.Square()
    scene.add(square)
    undo, redo = scene.undo_stack, scene.redo_stack
    scene.save_state()
    scene.save_state()
    assert len(undo) == 1
    square.shift(m.RIGHT)
    scene.undo()
    assert len(redo) == 1
    square.shift(2 * m.UP)
    scene.save_state()
    assert scene.redo_stack is redo and not redo
    scene.max_num_saved_states = 2
    for _ in range(4):
        square.shift(m.RIGHT)
        scene.save_state()
    assert scene.undo_stack is undo and len(undo) == 2
    scene.max_num_saved_states = 0
    scene.save_state()
    assert not undo and not redo


def rejected_checkpoint_preserves_history():
    scene, square = m.Scene(), m.Square()
    scene.add(square)
    scene.save_state()
    state = scene.undo_stack[-1]
    state._checkpoint = b"deliberately-invalid-checkpoint"
    square.shift(m.RIGHT)
    undo, redo = tuple(scene.undo_stack), tuple(scene.redo_stack)
    try:
        scene.undo()
    except Exception:
        pass
    else:
        raise AssertionError("native checkpoint decoder must reject malformed bytes")
    assert len(scene.undo_stack) == len(undo)
    assert all(a is b for a, b in zip(scene.undo_stack, undo))
    assert tuple(scene.redo_stack) == redo
    close(square.get_center(), m.RIGHT)


def foreign_owner_refusal():
    source, target = m.Scene(), m.Scene()
    a, b = m.Square(), m.Square().shift(3 * m.RIGHT)
    source.add(a)
    target.add(b)
    state = source.get_state()
    try:
        state.restore_scene(target)
    except m._ForeignStageError as error:
        assert "another Scene" in str(error)
    else:
        raise AssertionError("a checkpoint cannot be restored into another arena")
    close(a.get_center(), m.ORIGIN)
    close(b.get_center(), 3 * m.RIGHT)
    assert source.mobjects[0] is a and target.mobjects[0] is b


def ignored_camera():
    scene, square = m.Scene(), m.Square()
    scene.add(square)
    state = m.SceneState(scene, ignore=[scene.frame])
    assert state._checkpoint is not None
    scene.frame.shift(2 * m.RIGHT)
    scene.wait(.125)
    square.shift(m.UP)
    state.restore_scene(scene)
    close(scene.frame.get_center(), 2 * m.RIGHT)
    close(square.get_center(), m.ORIGIN)
    assert scene.get_time() == state.time


def rendered_restore():
    class Original(m.Scene):
        default_camera_config = dict(resolution=(96, 54), fps=8)
        def construct(self):
            self.subject = m.Square(side_length=1, fill_color=m.WHITE,
                                    fill_opacity=1, stroke_width=0)
            self.subject.shift(2 * m.LEFT + m.UP)
            self.add(self.subject)

    class Restored(Original):
        def construct(self):
            super().construct()
            self.save_state()
            self.subject.shift(4 * m.RIGHT).set_fill(m.RED)
            self.frame.shift(m.RIGHT).scale(.5)
            self.undo()

    directory = Path(tempfile.mkdtemp(prefix="fmn-scene-state-"))
    original = render_scene(Original, directory / "original.y4m", threads=1)
    restored = render_scene(Restored, directory / "restored.y4m", threads=4)
    assert original.frame_count == restored.frame_count == 1
    expected = original.destination.read_bytes()
    actual = restored.destination.read_bytes()
    assert actual == expected, "restored geometry/style/camera must render the original view"
    header, payload = actual.split(b"\n", 1)
    assert header == b"YUV4MPEG2 W96 H54 F8:1 Ip A1:1 C420mpeg2"
    assert payload[:6] == b"FRAME\n" and len(payload) == 6 + 96 * 54 * 3 // 2
    luma = np.frombuffer(payload[6:6 + 96 * 54], dtype=np.uint8).reshape(54, 96)
    ys, xs = np.nonzero(luma > 200)
    assert len(xs) > 10 and xs.mean() < 48 and ys.mean() < 27
    assert original.certified is False and restored.certified is False
    print("scene snapshot rendering artifacts:", directory)


CASES = (
    captured_families_styles_and_arrays, shared_child_identity, updater_restoration,
    ordered_roots, camera_pose_and_callbacks, native_clock_and_bytes,
    repeated_history, history_branch_and_limit, rejected_checkpoint_preserves_history,
    foreign_owner_refusal, ignored_camera, rendered_restore,
)
assert len(CASES) == 12
for case in CASES:
    case()
    print("scene snapshot acceptance:", case.__name__)
print("scene snapshot acceptance: 12 cases passed")
