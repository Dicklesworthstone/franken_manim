"""Installed-portal acceptance for exact nested composition positions.

The short scenes use real playback. The large-index checks query the same
native timing and callback driver directly, without iterating 2**53 frames.
"""
from fractions import Fraction
import math
from types import SimpleNamespace

import manimlib as m


class Probe(m.Animation):
    def __init__(self, scene, **duration):
        self.scene, self.begins, self.alphas = scene, [], []
        super().__init__(m.Mobject(), rate_func=m.linear, **duration)

    def begin(self):
        self.begins.append(self.scene.get_frame_count())
        super().begin()

    def interpolate_mobject(self, alpha):
        self.alphas.append(float(alpha))


def real_mixed_playback():
    for fps in (7, 24, 25, 30, 60):
        for nested in (False, True):
            scene = m.Scene(camera_config={"fps": fps, "resolution": (48, 32)})
            first = Probe(scene, run_time=43 / fps)
            second = Probe(scene, run_time_frames=1)
            animation = m.Succession(first, second)
            if nested:
                animation = m.AnimationGroup(m.AnimationGroup(animation))
            scene.play(animation)
            boundary = Fraction.from_float(43 / fps) * fps
            assert scene.get_frame_count() == math.ceil(boundary + 1)
            assert first.begins == [0]
            # Callbacks run before that sample advances the native clock.
            assert second.begins == [math.ceil(boundary) - 1], (fps, nested, second.begins)
            assert first.alphas[-1] == second.alphas[-1] == 1.


class Child:
    def __init__(self, name, duration, events):
        self.name, self.duration, self.events = name, duration, events

    def get_run_time(self):
        return self.duration / 24

    def begin(self):
        self.events.append((self.name, "begin"))

    def finish(self):
        self.events.append((self.name, "finish"))

    def interpolate(self, alpha):
        self.events.append((self.name, "alpha", alpha))

    def _fmn_interpolate_at_frame(self, position, fps, alpha=None):
        self.events.append((self.name, "position", position))


def native_ratio_and_large_positions():
    boundary = 2**53 + 1
    descriptor = (None, None, None, 1., [
        (None, boundary, None, 0., None),
        (None, 1, None, 0., None),
    ])
    timing = m._frame_timing(descriptor, 24)
    assert Fraction(*timing.duration_ratio) == boundary + 1
    assert timing.frame_count == boundary + 1
    before = timing.exact_position_at_frame(boundary - 1, 24)
    at = timing.exact_position_at_frame(boundary, 24)
    assert timing.member_at_position(before) == 0
    assert timing.member_at_position(at) == 1
    assert timing.child_sample_at_position(1, at) == (0., None)

    events = []
    children = [Child("first", boundary, events), Child("second", 1, events)]
    group = SimpleNamespace(
        rate_func=m.linear, _native_kind="succession", time_span=None,
        run_time=timing.seconds, final_alpha_value=1., lag_ratio=1.,
        _fmn_frame_timing=timing,
    )
    driver = m._CompositionCallbackDriver(group, children)
    driver.begin()
    driver._fmn_interpolate_at_frame(boundary - 1, 24)
    assert ("second", "begin") not in events
    driver._fmn_interpolate_at_frame(boundary, 24)
    assert events[-2:] == [("second", "begin"), ("second", "alpha", 0.)]

    mixed = m._frame_timing((None, None, None, 1., [
        (43 / 24, None, None, 0., None),
        (None, 1, None, 0., None),
    ]), 24)
    exact_end = Fraction.from_float(43 / 24) * 24
    assert Fraction(*mixed.duration_ratio) == exact_end + 1
    assert mixed.frame_count == 45
    token = mixed.position_from_ratio(exact_end.numerator, exact_end.denominator, 24)
    assert mixed.member_at_position(token) == 1
    assert mixed.member_at_position(mixed.exact_position_at_frame(43, 24)) == 0
    assert mixed.member_at_position(mixed.exact_position_at_frame(44, 24)) == 1
    try:
        mixed.alpha_at_frame(token, 30)
    except ValueError:
        pass
    else:
        raise AssertionError("exact position accepted on a different frame grid")
    for denominator in (0, -1):
        try:
            mixed.position_from_ratio(1, denominator, 24)
        except ValueError:
            pass
        else:
            raise AssertionError("invalid ratio denominator accepted")


real_mixed_playback()
native_ratio_and_large_positions()
print("Exact mixed, nested, rational and large-frame composition positions passed")
