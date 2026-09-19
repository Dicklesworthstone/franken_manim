"""Real native playback failure recovery for mixed camera/drawable scenes."""
import math

import numpy as np
import manimlib as m


_MISSING = object()


class Probe(m.Animation):
    _fmn_allow_camera_callback = True

    def __init__(self, mob, phase=None, broken_abort=False, **kwargs):
        self.phase, self.broken_abort = phase, broken_abort
        self.error = LookupError("camera callback failed")
        self.events = []
        self.at_failure = None
        super().__init__(mob, run_time=2 / 30, rate_func=m.linear,
                         suspend_mobject_updating=True, **kwargs)

    def fail(self, phase):
        if self.phase == phase:
            if self.at_failure is not None:
                self.at_failure()
            raise self.error

    def begin(self):
        self.events.append("begin")
        super().begin()
        self.mobject.locked_data_keys = {"animation-lock"}
        self.mobject.const_data_keys.add("point")
        self.mobject.locked_uniform_keys = {"animation-uniform"}
        self.fail("begin")

    def update_mobjects(self, dt):
        self.fail("helpers")
        super().update_mobjects(dt)

    def interpolate_mobject(self, alpha):
        if alpha > 0:
            self.mobject.shift((0, 0.1, 0))
            self.fail("interpolate")

    def finish(self):
        self.events.append("finish")
        self.fail("finish")
        super().finish()

    def clean_up_from_scene(self, scene):
        self.events.append("cleanup")
        self.fail("cleanup")
        super().clean_up_from_scene(scene)

    def abort(self):
        self.events.append("abort")
        if self.broken_abort:
            raise RuntimeError("authored abort failed too")
        # Deliberately not a repair routine. Shared execution ownership must
        # recover objects even for callbacks with a no-op or broken abort.


def snapshot(mob):
    names = ("locked_data_keys", "const_data_keys", "locked_uniform_keys")
    return (mob._is_updating_suspended(), vars(mob).get("_is_animating", _MISSING),
            [(name, getattr(mob, name), set(getattr(mob, name))) for name in names])


def assert_restored(mob, before):
    assert mob._is_updating_suspended() == before[0]
    assert vars(mob).get("_is_animating", _MISSING) == before[1]
    for name, identity, values in before[2]:
        assert getattr(mob, name) is identity, name
        assert getattr(mob, name) == values, name


def all_callback_phases_restore_real_roots():
    cases = 0
    for nested in (False, True):
        for phase in ("begin", "helpers", "interpolate", "finish", "cleanup"):
            for presuspended in (False, True):
                for failed_camera in (False, True):
                    scene = m.Scene()
                    square = m.Square()
                    scene.add(square)
                    objects = (square, scene.frame)
                    ticks = []
                    for mob in objects:
                        mob.locked_data_keys.add("retained")
                        mob.locked_uniform_keys.add("retained-uniform")
                        mob._is_animating = presuspended
                        if presuspended:
                            mob.suspend_updating()
                        mob.add_updater(lambda obj, dt: ticks.append(dt), call=False)
                    before = [snapshot(mob) for mob in objects]
                    drawable = Probe(square, None if failed_camera else phase, broken_abort=True)
                    camera = Probe(scene.frame, phase if failed_camera else None, broken_abort=True)
                    failed = camera if failed_camera else drawable
                    at_failure = []
                    failed.at_failure = lambda: at_failure.append(len(ticks))
                    animations = ((m.AnimationGroup(m.AnimationGroup(drawable, camera)),)
                                  if nested else (drawable, camera))
                    try:
                        scene.play(*animations)
                    except BaseException as error:
                        assert error is failed.error
                    else:
                        raise AssertionError("authored callback failure was swallowed")
                    for mob, saved in zip(objects, before):
                        assert_restored(mob, saved)
                    assert len(ticks) == at_failure[-1], "unwinding invoked user updaters"
                    assert drawable.events.count("abort") <= 1, drawable.events
                    assert camera.events.count("abort") <= 1, camera.events
                    assert not scene.frame._is_bound()
                    assert scene.frame is scene.camera.frame
                    assert scene.num_plays == 0
                    assert "_fmn_scene_execution" not in vars(scene)
                    assert not scene._fmn_camera_play_active
                    # Executed geometry is not rewound. Recover ownership and
                    # run the next segment on the very same native scene.
                    for mob in objects:
                        mob.clear_updaters()
                        mob.resume_updating(call_updater=False)
                    old_time = scene.time()
                    scene.wait(1 / 30)
                    assert scene.time() > old_time and scene.num_plays == 1
                    cases += 1
    assert cases == 40
    return cases


def later_begin_failure_never_begins_future_animations():
    scene = m.Scene()
    first, failure, future = Probe(m.Square()), Probe(scene.frame, "begin"), Probe(m.Circle())
    scene.add(first.mobject, future.mobject)
    saved = [snapshot(a.mobject) for a in (first, failure, future)]
    try:
        scene.play(first, failure, future)
    except LookupError as error:
        assert error is failure.error
    else:
        raise AssertionError("begin failure was swallowed")
    assert future.events == [], future.events
    assert first.events == ["begin", "abort"], first.events
    assert failure.events == ["begin", "abort"], failure.events
    for animation, before in zip((first, failure, future), saved):
        assert_restored(animation.mobject, before)


def stock_camera_abort_does_not_invoke_an_extra_updater():
    scene = m.Scene()
    frame, ticks = scene.frame, []
    frame.add_updater(lambda mob, dt: ticks.append(dt) if mob is frame else None, call=False)
    error = KeyboardInterrupt("camera path interrupted")

    def path(start, target, alpha):
        if alpha > 0:
            raise error
        return start

    try:
        scene.play(m.AnimationGroup(m.Transform(frame, frame.copy(), path_func=path,
                                                suspend_mobject_updating=True),
                                    suspend_mobject_updating=True), run_time=2 / 30)
    except KeyboardInterrupt as caught:
        assert caught is error
    else:
        raise AssertionError("interrupt was swallowed")
    assert ticks == [], ticks
    assert not frame._is_updating_suspended() and not getattr(frame, "_is_animating", False)
    scene.wait(1 / 30)
    assert any(dt > 0 for dt in ticks)


def renderer_state_progress_is_not_rolled_back_on_error():
    scene = m.Scene()
    frame = scene.frame
    animation = Probe(frame, "interpolate")
    start = frame.get_center().copy()
    try:
        scene.play(animation)
    except LookupError as error:
        assert error is animation.error
    else:
        raise AssertionError("interpolation failure was swallowed")
    np.testing.assert_allclose(frame.get_center(), start + (0, .1, 0))
    # The native clock has not advanced the failed frame to scene updaters.
    assert math.isclose(scene.time(), 0.)
    assert not frame._is_updating_suspended() and not getattr(frame, "_is_animating", False)


_count = all_callback_phases_restore_real_roots()
for _case in (later_begin_failure_never_begins_future_animations,
              stock_camera_abort_does_not_invoke_an_extra_updater,
              renderer_state_progress_is_not_rolled_back_on_error):
    _case()
print(f"camera execution acceptance: {_count} phase/ownership combinations and 3 recovery cases passed")
