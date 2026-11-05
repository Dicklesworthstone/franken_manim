"""Exact-frame authoring through the installed portal and real output sinks.

The issue #10 witness intentionally keeps the legacy second-based result:
864 requested ticks become 872 when represented as binary seconds at 24 fps.
The frame-native surface must deliver 864 without a nextafter workaround.
No clock, animation driver, renderer, receipt, or encoder is mocked here.
"""
from __future__ import annotations

import json
import math
import os
from fractions import Fraction
from pathlib import Path
import shutil
import subprocess
import tempfile
import wave

import numpy as np
import manimlib as m


REPORTER_FRAMES = (43, 17, 61, 29, 101, 7, 55, 13, 89, 31,
                   47, 19, 73, 11, 37, 23, 67, 3, 97, 41)
assert sum(REPORTER_FRAMES) == 864


class SerialScene(m.Scene):
    """A public observer forces the ordinary owner-thread sampling path."""

    def update_mobjects(self, dt):
        super().update_mobjects(dt)
        if dt > 0:
            self.observations.append((self.get_frame_count(), float(dt)))


def scene_at(fps=24, *, skip=False, scene_type=m.Scene):
    scene = scene_type(camera_config={"fps": fps, "resolution": (48, 32)},
                       skip_animations=skip)
    scene.observations = []
    assert scene.camera.fps == fps
    assert m._portal_scene_clock(scene) == (fps, 0)
    return scene


def seconds_frames(seconds, fps):
    """Independent exact-binary64 ceiling, without a rounded float product."""
    value = Fraction.from_float(seconds) * fps
    return (value.numerator + value.denominator - 1) // value.denominator


def reporter(scene, fps, mode="frames", *, before_segment=None):
    square = m.Square(side_length=.5, fill_opacity=1, stroke_width=0)
    scene.add(square)
    for index, count in enumerate(REPORTER_FRAMES):
        if before_segment is not None:
            before_segment(index)
        seconds = count / fps
        if mode == "nextafter":
            seconds = math.nextafter(seconds, 0.)
        if index % 2:
            if mode == "frames":
                scene.wait_frames(count)
            else:
                scene.wait(seconds)
        else:
            motion = square.animate.shift((.08 if index % 4 else -.08) * m.RIGHT)
            if mode == "frames":
                scene.play(motion, run_time_frames=count, rate_func=m.linear)
            else:
                scene.play(motion, run_time=seconds, rate_func=m.linear)
    return square


def raises(types, operation):
    try:
        operation()
    except types as error:
        return error
    raise AssertionError("invalid duration was accepted")


def reporter_and_rate_matrix():
    results = {}
    for fps in (1, 7, 24, 25, 30, 60, 120):
        scene = scene_at(fps, skip=True)
        reporter(scene, fps)
        assert scene.get_frame_count() == 864
        assert scene.get_time() == 864 / fps
        results[fps] = scene.get_frame_count()
    for mode, expected in (("seconds", 872), ("nextafter", 864)):
        scene = scene_at(skip=True)
        reporter(scene, 24, mode)
        assert scene.get_frame_count() == expected, (mode, scene.get_frame_count())
    return results


def authoring_and_validation():
    class Count:
        def __index__(self):
            return 7

    scene = scene_at(skip=True)
    square = m.Square()
    scene.add(square)
    scene.wait_frames(Count())
    scene.play(square.animate(run_time_frames=np.int64(13)).shift(m.RIGHT))
    scene.play(m.Transform(square, square.copy().shift(m.UP), run_time_frames=17))
    animation = m.Transform(square, square.copy().shift(m.LEFT))
    assert animation.set_run_time_frames(19) is animation
    scene.play(animation)
    assert scene.get_frame_count() == 56

    # Each explicit override replaces the prior unit, including zero.
    scene.play(animation, run_time=0.)
    assert animation.run_time_frames is None
    assert scene.get_frame_count() == 56
    scene.play(animation, run_time_frames=0)
    assert animation.run_time_frames == 0
    scene.wait_frames(0)
    assert scene.get_frame_count() == 56

    for value, error in ((True, TypeError), (False, TypeError), (None, TypeError),
                         (1., TypeError), (np.float64(7), TypeError),
                         (1.5, TypeError), (-1, ValueError), (1 << 63, OverflowError),
                         (1 << 80, OverflowError)):
        for operation in (
            lambda value=value: scene.wait_frames(value),
            lambda value=value: animation.set_run_time_frames(value),
        ):
            before = scene.get_frame_count()
            center = square.get_center().copy()
            raises(error, operation)
            assert scene.get_frame_count() == before
            np.testing.assert_array_equal(square.get_center(), center)
        # None retains its existing meaning of an omitted animation override.
        if value is not None:
            for operation in (
                lambda value=value: scene.play(square.animate.shift(m.RIGHT), run_time_frames=value),
                lambda value=value: m.Transform(square, square.copy(), run_time_frames=value),
                lambda value=value: square.animate(run_time_frames=value).shift(m.RIGHT).build(),
            ):
                before = scene.get_frame_count()
                center = square.get_center().copy()
                raises(error, operation)
                assert scene.get_frame_count() == before
                np.testing.assert_array_equal(square.get_center(), center)
    for operation in (
        lambda: scene.play(run_time=1., run_time_frames=24),
        lambda: scene.play(animation, run_time=1., run_time_frames=24),
        lambda: m.Transform(square, square.copy(), run_time=1., run_time_frames=24),
        lambda: m.Animation(square, run_time=1., run_time_frames=24),
        lambda: m.Delay(run_time=1., run_time_frames=24),
        lambda: m.AnimationGroup(animation, run_time=1., run_time_frames=24),
    ):
        raises(ValueError, operation)
    assert scene.get_frame_count() == 56

    # A played animation carries opaque prepared timing. Copying it must
    # retain the declared integer without trying to pickle the native handle.
    copied_scene = scene_at()
    source = m.Square()
    copied_scene.add(source)
    played = m.Transform(source, source.copy().shift(m.RIGHT), run_time_frames=7)
    copied_scene.play(played)
    copied = played.copy()
    assert copied.run_time_frames == 7
    copied_scene.play(copied)
    assert copied_scene.get_frame_count() == 14


def planning_and_reporting_boundaries():
    # Planning is bounded work. Skipped playback intentionally still visits
    # every nominal update boundary, so never execute these huge durations.
    for fps in (1, 24, 25, 60, 120):
        for count in (0, 1, (1 << 53) + 1, (1 << 63) - 1):
            timing = m._frame_timing((None, count, None, 0., None), fps)
            assert timing.frame_native
            assert timing.frame_count == count
    too_long = (None, None, None, 1., [
        (None, (1 << 63) - 1, None, 0., None),
        (None, 1, None, 0., None),
    ])
    raises((OverflowError, RuntimeError), lambda: m._frame_timing(too_long, 24).frame_count)

    # int(scene.get_time() * fps) loses one frame for this small real scene.
    assert int((29 / 25) * 25) == 28
    class TwentyNine(m.Scene):
        def construct(self):
            self.wait_frames(29)
    from manimlib.extract_scene import compute_total_frames
    assert compute_total_frames(TwentyNine, {"camera_config": {"fps": 25}}) == 29


def mixed_zero_long_and_boundaries():
    scene = scene_at(skip=True)
    square = m.Square()
    scene.add(square)
    scene.wait_frames(7)
    scene.wait(math.nextafter(1 / 24, math.inf))
    scene.play(square.animate.shift(m.RIGHT), run_time_frames=13)
    scene.play(square.animate.shift(m.UP), run_time=.1)
    assert scene.get_frame_count() == 25  # 7 + ceil(above-one) + 13 + ceil(2.4)
    before = square.get_center().copy()
    scene.play(square.animate.shift(m.LEFT), run_time_frames=0)
    np.testing.assert_allclose(square.get_center(), before + m.LEFT, atol=1e-6)
    assert scene.get_frame_count() == 25
    for _ in range(4096):
        scene.wait_frames(1)
    assert scene.get_frame_count() == 4121

    stopped = scene_at()
    observed = []
    def stop():
        observed.append(stopped.get_frame_count())
        return observed[-1] == 3
    stopped.wait_frames(20, stop_condition=stop)
    assert stopped.get_frame_count() == 3
    assert observed == [1, 2, 3]
    stopped.wait_frames(0, stop_condition=stop)
    assert observed == [1, 2, 3]

    for fps in (7, 24, 25, 30, 60, 120):
        scene = scene_at(fps, skip=True)
        total = 0
        for count in (1, 7, 29, 43):
            seconds = count / fps
            for value in (math.nextafter(seconds, 0.), seconds,
                          math.nextafter(seconds, math.inf)):
                scene.wait(value)
                total += seconds_frames(value, fps)
                assert scene.get_frame_count() == total, (fps, count, value)
            scene.wait_frames(count)
            total += count
            assert scene.get_frame_count() == total
        scene.wait(-0.)
        assert scene.get_frame_count() == total


class Probe(m.Animation):
    def __init__(self, scene, frames):
        self.scene = scene
        self.begins = []
        self.alphas = []
        super().__init__(m.Mobject(), run_time_frames=frames, rate_func=m.linear)

    def begin(self):
        self.begins.append(self.scene.get_frame_count())
        super().begin()

    def interpolate_mobject(self, alpha):
        self.alphas.append(float(alpha))


def composition_and_callback_boundaries():
    for fps, nested, scaled, zeros in (
        (24, False, False, False), (25, True, False, False),
        (30, False, True, False), (60, True, True, False),
        (24, True, False, True),
    ):
        scene = scene_at(fps)
        first, second = Probe(scene, 29), Probe(scene, 38)
        members = (first, second)
        if zeros:
            members = (m.Delay(run_time_frames=0), first, m.Delay(run_time_frames=0),
                       second, m.Delay(run_time_frames=0))
        animation = m.Succession(*members)
        if nested:
            animation = m.AnimationGroup(m.AnimationGroup(animation))
        scene.play(animation, **({"run_time_frames": 201} if scaled else {}))
        assert scene.get_frame_count() == (201 if scaled else 67)
        assert first.begins == [0]
        # Callback interpolation precedes the frame's clock advance. The
        # transition at sample 29 therefore observes 28 completed ticks.
        assert second.begins == [86 if scaled else 28], (fps, nested, scaled, second.begins)
        assert second.alphas[0] == 0.
        assert second.alphas[-1] == 1.
        assert all(math.isfinite(alpha) and 0 <= alpha <= 1 for alpha in second.alphas)
        assert second.alphas == sorted(second.alphas)

    for fps in (7, 24, 25, 30, 60):
        scene = scene_at(fps, skip=True)
        a, b = m.Square(), m.Circle()
        scene.add(a, b)
        # Native constructor arguments, a fractional-frame lag, and one
        # ceiling of the final endpoint, not a ceiling for every child.
        scene.play(m.LaggedStart(
            m.Transform(a, a.copy().shift(m.RIGHT), run_time_frames=3),
            m.Transform(b, b.copy().shift(m.UP), run_time_frames=3), lag_ratio=.5))
        assert scene.get_frame_count() == 5
        scene.play(m.Succession(m.Delay(run_time_frames=5), m.Delay(run_time=.125)))
        assert scene.get_frame_count() == 10 + math.ceil(.125 * fps)
        scene.play(m.AnimationGroup(m.Delay(run_time_frames=7), m.Delay(run_time_frames=13)),
                   run_time_frames=11)
        assert scene.get_frame_count() == 21 + math.ceil(.125 * fps)
        # The outer succession must add the two unrounded 1.5-frame
        # intervals, not two independently rounded 2-frame segments.
        def half_overlap():
            return m.AnimationGroup(m.Delay(run_time_frames=1), m.Delay(run_time_frames=1),
                                    lag_ratio=.5)
        scene.play(m.Succession(half_overlap(), half_overlap()))
        assert scene.get_frame_count() == 24 + math.ceil(.125 * fps)

    camera = scene_at()
    camera.play(camera.frame.animate(run_time_frames=29).shift(.25 * m.RIGHT))
    assert camera.get_frame_count() == 29
    camera_probe = Probe(camera, 11)
    camera.play(m.Succession(
        m.Transform(camera.frame, camera.frame.copy().shift(.25 * m.UP), run_time_frames=7),
        camera_probe))
    assert camera.get_frame_count() == 47
    assert camera_probe.begins == [35]
    np.testing.assert_allclose(camera.frame.get_center(), .25 * (m.RIGHT + m.UP), atol=1e-6)

    # The mixed camera slot contains a native leaf and a nested Python
    # lifecycle. Their internal transition still uses the same frame grid.
    mixed = scene_at(25)
    square = m.Square()
    mixed.add(square)
    nested_probe = Probe(mixed, 16)
    mixed.play(m.AnimationGroup(
        mixed.frame.animate(run_time_frames=29).shift(.25 * m.RIGHT),
        m.Succession(m.Transform(square, square.copy().shift(m.UP), run_time_frames=13),
                     nested_probe),
    ))
    assert mixed.get_frame_count() == 29
    assert nested_probe.begins == [12]
    np.testing.assert_allclose(square.get_center(), m.UP, atol=1e-6)


def legacy_rate_override_hook():
    class LegacyProbe(Probe):
        def __init__(self, scene):
            self.rate_updates = []
            super().__init__(scene, 5)

        # Existing authored overrides are allowed to retain the old explicit
        # signature. The new integer override belongs to native preparation.
        def update_rate_info(self, run_time=None, rate_func=None, lag_ratio=None):
            self.rate_updates.append((run_time, rate_func, lag_ratio))
            return super().update_rate_info(run_time=run_time, rate_func=rate_func,
                                            lag_ratio=lag_ratio)

    scene = scene_at()
    animation = LegacyProbe(scene)
    before = len(animation.rate_updates)
    scene.play(animation, run_time_frames=7, rate_func=m.linear)
    assert len(animation.rate_updates) == before + 1
    assert animation.rate_updates[-1] == (None, m.linear, None)
    assert animation.run_time_frames == 7
    assert animation.begins == [0]
    assert scene.get_frame_count() == 7


def specialized_constructor_durations():
    factories = (
        lambda mob, timing: m.Delay(**timing),
        lambda mob, timing: m.DrawBorderThenFill(mob, **timing),
        lambda mob, timing: m.Rotating(mob, **timing),
        lambda mob, timing: m.Rotate(mob, **timing),
        lambda mob, timing: m.Homotopy(lambda x, y, z, t: (x, y + t, z), mob, **timing),
        lambda mob, timing: m.ApplyPointwiseFunction(lambda p: p + m.RIGHT, mob, **timing),
        lambda mob, timing: m.ApplyWave(mob, **timing),
        lambda mob, timing: m.PhaseFlow(lambda p: m.RIGHT, mob, **timing),
        lambda mob, timing: m.FocusOn(mob, **timing),
        lambda mob, timing: m.WiggleOutThenIn(mob, **timing),
        lambda mob, timing: m.Flash(mob, num_lines=3, **timing),
        lambda mob, timing: m.Broadcast(mob, n_circles=2, **timing),
        lambda mob, timing: m.ClockPassesTime(m.Clock(), **timing),
        lambda mob, timing: m.LaggedStartMap(m.FadeIn, m.VGroup(mob, m.Square()), **timing),
        lambda mob, timing: m.TransformMatchingParts(mob, mob.copy().shift(m.RIGHT), **timing),
        lambda mob, timing: m.TransformMatchingStrings(m.Text("a"), m.Text("ab"), **timing),
    )
    for factory in factories:
        scene = scene_at(skip=True)
        mob = m.Square()
        scene.add(mob)
        animation = factory(mob, {"run_time_frames": 7})
        scene.play(animation)
        assert scene.get_frame_count() == 7, type(animation).__name__
        raises(ValueError, lambda: factory(mob, {"run_time": 1., "run_time_frames": 7}))


def phase_flow_preserves_its_virtual_time():
    for fps in (24, 30):
        scene = scene_at(fps)
        square = m.Square()
        scene.add(square)
        flow = m.PhaseFlow(lambda point: m.RIGHT, square, run_time_frames=12)
        scene.play(flow, run_time_frames=6, rate_func=m.linear)
        assert scene.get_frame_count() == 6
        # As with a seconds constructor, a later play override changes the
        # animation speed; it does not change the field's virtual travel time.
        np.testing.assert_allclose(square.get_center(), (12 / fps) * m.RIGHT, atol=1e-6)

        explicit = m.PhaseFlow(lambda point: m.UP, square, run_time_frames=12, virtual_time=2.)
        scene.play(explicit, run_time_frames=6, rate_func=m.linear)
        assert scene.get_frame_count() == 12
        np.testing.assert_allclose(square.get_center(), (12 / fps) * m.RIGHT + 2 * m.UP, atol=1e-6)


def speed_profile_frame_durations():
    from manimlib.animation.speed import ChangeSpeed

    for fps in (24, 25, 49, 60):
        for speed, frames, expected in ((2., 12, 6), (.5, 7, 14), (2., 3, 2)):
            scene = scene_at(fps)
            probe = Probe(scene, frames)
            animation = ChangeSpeed(probe, {0: speed, 1: speed})
            scene.play(animation)
            assert scene.get_frame_count() == expected, (fps, speed, frames)
            assert probe.alphas[-1] == 1.
            assert animation._speed_child_duration == frames / fps

        # Two 1.5-frame children form one 3-frame succession: no child ceil.
        scene = scene_at(fps)
        probes = (Probe(scene, 3), Probe(scene, 3))
        scene.play(m.Succession(*(ChangeSpeed(probe, {0: 2.}, affects_speed_updaters=False)
                                 for probe in probes)))
        assert scene.get_frame_count() == 3
        assert all(probe.alphas[-1] == 1. for probe in probes)

        nested = scene_at(fps)
        probe = Probe(nested, 29)
        inner = ChangeSpeed(probe, {0: 2.}, affects_speed_updaters=False)
        nested.play(ChangeSpeed(inner, {0: .5}, affects_speed_updaters=False))
        assert nested.get_frame_count() == 29

        # Constant profiles are normalized identity maps. They must carry
        # the integer parent position through to the succession boundary,
        # not reconstruct 29 as the smaller float (29 / 67) * 67.
        for frames, speed in (((29, 38), 1.), ((58, 76), 2.)):
            boundary = scene_at(fps)
            first, second = (Probe(boundary, count) for count in frames)
            boundary.play(ChangeSpeed(m.Succession(first, second), {0: speed}))
            assert boundary.get_frame_count() == 67
            assert second.begins == [28], (fps, frames, speed, second.begins)

        # A seconds member retains its deliberate exact-binary64 duration
        # while the enclosing mixed composition is scaled on the frame grid.
        mixed = scene_at(fps)
        child = m.Succession(Probe(mixed, 1), m.Delay(run_time=.125))
        mixed.play(ChangeSpeed(child, {0: 2.}))
        extent = (Fraction(1) + Fraction.from_float(.125) * fps) / 2
        assert mixed.get_frame_count() == (extent.numerator + extent.denominator - 1) // extent.denominator

        # A reciprocal such as 1/5 cannot be represented by the dyadic
        # extent model. Refuse inference before begin; either explicit unit
        # remains available and must override the non-dyadic profile.
        refused = scene_at(fps)
        probe = Probe(refused, 5)
        error = raises(ValueError, lambda: refused.play(ChangeSpeed(probe, {0: 5.})))
        assert "run_time_frames or run_time" in str(error)
        assert refused.get_frame_count() == 0 and probe.begins == []

        for options, expected in (({"run_time_frames": 1}, 1),
                                  ({"run_time": 3.}, 3 * fps),
                                  ({"run_time_frames": 0}, 0),
                                  ({"run_time": 0.}, 0)):
            scene = scene_at(fps)
            probe = Probe(scene, 5)
            scene.play(ChangeSpeed(probe, {0: 5.}, **options))
            assert scene.get_frame_count() == expected, (fps, options)
            assert probe.alphas[-1] == 1.

        reused = scene_at(fps)
        animation = ChangeSpeed(Probe(reused, 12), {0: 5.})
        animation.set_run_time_frames(7)
        reused.play(animation)
        assert reused.get_frame_count() == 7
        animation.set_run_time(.5)
        reused.play(animation)
        assert reused.get_frame_count() == 7 + seconds_frames(.5, fps)
        reused.play(animation, run_time_frames=3)
        assert reused.get_frame_count() == 10 + seconds_frames(.5, fps)

        overridden = scene_at(fps)
        overridden.play(ChangeSpeed(Probe(overridden, 5), {0: 5.}), run_time_frames=1)
        assert overridden.get_frame_count() == 1

        switched = scene_at(fps)
        child = Probe(switched, 5)
        animation = ChangeSpeed(child, {0: 2.}, run_time=1.)
        switched.play(animation)
        child.set_run_time(.25)
        switched.play(animation)
        assert switched.get_frame_count() == 2 * fps
        assert animation._speed_child_duration == .25

    raises(ValueError, lambda: ChangeSpeed(m.Delay(), {}, run_time=0., run_time_frames=0))


def y4m_count(path):
    """Independent container reader: count complete encoded planar frames."""
    with path.open("rb") as stream:
        header = stream.readline().split()
        assert header[0] == b"YUV4MPEG2"
        width = int(next(item[1:] for item in header if item.startswith(b"W")))
        height = int(next(item[1:] for item in header if item.startswith(b"H")))
        rate = next(item[1:] for item in header if item.startswith(b"F"))
        numerator, denominator = map(int, rate.split(b":"))
        assert width > 0 and height > 0 and width % 2 == height % 2 == 0
        assert any(item.startswith(b"C420") for item in header)
        frame_size = width * height * 3 // 2
        count = 0
        while marker := stream.readline():
            assert marker == b"FRAME\n"
            assert len(stream.read(frame_size)) == frame_size
            count += 1
    return count, Fraction(numerator, denominator), (width, height)


def assert_y4m(path, scene, result, expected, fps):
    actual, encoded_rate, dimensions = y4m_count(path)
    assert actual == scene.get_frame_count() == result.frame_count == expected
    assert encoded_rate == fps == result.fps
    assert dimensions == result.resolution == (48, 32)
    assert scene.get_time() == expected / fps
    assert result.as_dict()["frame_count"] == expected
    assert scene._render_performance["frames"] == expected
    return actual


def native_rendered_counts(root):
    results = {}
    for fps, mode, expected in ((24, "frames", 864), (25, "frames", 864),
                                 (30, "frames", 864), (60, "frames", 864),
                                 (24, "seconds", 872), (24, "nextafter", 864)):
        scene = scene_at(fps)
        destination = root / f"reporter-{fps}-{mode}.y4m"
        with scene.render_session(destination, format="y4m", resolution=(48, 32),
                                  fps=fps, threads=4) as session:
            reporter(scene, fps, mode)
        actual = assert_y4m(destination, scene, session.result, expected, fps)
        results[f"{fps}-{mode}"] = actual
        if mode == "frames":
            assert scene._render_performance["pure_play_frames"] == sum(REPORTER_FRAMES[::2])

    serial = scene_at(24, scene_type=SerialScene)
    serial_destination = root / "reporter-24-serial.y4m"
    with serial.render_session(serial_destination, format="y4m", resolution=(48, 32),
                               fps=24, threads=1) as session:
        reporter(serial, 24)
    assert_y4m(serial_destination, serial, session.result, 864, 24)
    assert serial._render_performance["pure_play_frames"] == 0
    assert [frame for frame, _dt in serial.observations] == list(range(1, 865))
    assert all(dt == 1 / 24 for _frame, dt in serial.observations)
    assert serial_destination.read_bytes() == (root / "reporter-24-frames.y4m").read_bytes()
    results["24-serial"] = session.result.frame_count

    for operation in ("wait", "play"):
        zero = scene_at()
        square = m.Square(fill_opacity=1)
        empty = root / f"empty-{operation}.y4m"
        def empty_movie():
            with zero.render_session(empty, format="y4m", resolution=(48, 32), fps=24, threads=1):
                zero.add(square)
                if operation == "wait":
                    zero.wait_frames(0)
                else:
                    zero.play(square.animate.shift(m.RIGHT), run_time_frames=0)
        error = raises(RuntimeError, empty_movie)
        assert "empty" in str(error).lower()
        assert zero.get_frame_count() == 0 and not empty.exists()
    still = scene_at()
    with still.render_session(root / "still.png", format="png", resolution=(48, 32),
                              fps=24, threads=1) as session:
        still.add(m.Square(fill_opacity=1))
        still.wait_frames(0)
    assert session.result.frame_count == 1 and still.get_frame_count() == 0
    assert (root / "still.png").read_bytes().startswith(b"\x89PNG\r\n\x1a\n")
    return results


def mixed_and_long_rendered_counts(root):
    results = {}
    for fps in (24, 25, 30, 60):
        outputs = []
        expected = 7 + seconds_frames(math.nextafter(1 / fps, math.inf), fps)
        expected += 13 + seconds_frames(.1, fps)
        for scene_type, route, threads in ((m.Scene, "native", 4), (SerialScene, "serial", 1)):
            scene = scene_at(fps, scene_type=scene_type)
            destination = root / f"mixed-{fps}-{route}.y4m"
            with scene.render_session(destination, format="y4m", resolution=(48, 32),
                                      fps=fps, threads=threads) as session:
                square = m.Square(fill_opacity=1)
                scene.add(square)
                scene.wait_frames(0)
                scene.play(square.animate.shift(.25 * m.RIGHT), run_time_frames=0)
                assert scene.get_frame_count() == 0
                np.testing.assert_allclose(square.get_center(), .25 * m.RIGHT, atol=1e-6)
                scene.wait_frames(7)
                scene.wait(math.nextafter(1 / fps, math.inf))
                scene.play(square.animate.shift(.25 * m.UP), run_time_frames=13, rate_func=m.linear)
                scene.play(square.animate.shift(.25 * m.LEFT), run_time=.1, rate_func=m.linear)
                scene.wait_frames(0)
            assert_y4m(destination, scene, session.result, expected, fps)
            outputs.append(destination.read_bytes())
        assert outputs[0] == outputs[1], fps
        results[str(fps)] = expected

    long = scene_at(25)
    destination = root / "long-25.y4m"
    with long.render_session(destination, format="y4m", resolution=(48, 32),
                             fps=25, threads=4) as session:
        long.add(m.Square(fill_opacity=1))
        for _ in range(1024):
            long.wait_frames(0)
            long.wait_frames(1)
    assert_y4m(destination, long, session.result, 1024, 25)
    results["2048-segments"] = session.result.frame_count
    return results


def write_cue(path):
    with wave.open(str(path), "wb") as sound:
        sound.setparams((1, 2, 48_000, 0, "NONE", "not compressed"))
        sound.writeframes(np.full(64, 2000, dtype="<i2").tobytes())


def segment_starts(fps, mode="frames"):
    starts, frame = [], 0
    for count in REPORTER_FRAMES:
        starts.append(frame)
        frame += count if mode == "frames" else seconds_frames(count / fps, fps)
    return starts


def sample_boundary(frame, fps):
    # Round the rational frame time to the nearest 48 kHz sample, ties up.
    return (2 * frame * 48_000 + fps) // (2 * fps)


def native_audio_sample_positions(root):
    cue = root / "pulse.wav"
    write_cue(cue)
    results = {}
    for fps in (24, 29, 60):
        scene = scene_at(fps)
        destination = root / f"sample-positions-{fps}.wav"
        with scene.render_session(destination, format="wav", resolution=(48, 32),
                                  fps=fps, threads=1) as session:
            reporter(scene, fps, before_segment=lambda _index: scene.add_sound(str(cue)))
        assert scene.get_frame_count() == 864
        starts = segment_starts(fps)
        assert [fact[1:3] for fact in scene._sound_request_facts()] == [
            (frame, fps) for frame in starts
        ]
        expected_samples = sample_boundary(864, fps)
        with wave.open(str(destination), "rb") as sound:
            assert (sound.getframerate(), sound.getnchannels(), sound.getsampwidth()) == (48_000, 2, 2)
            assert sound.getnframes() == session.result.sample_frames == expected_samples
            samples = np.frombuffer(sound.readframes(expected_samples), dtype="<i2").reshape(-1, 2)
        np.testing.assert_array_equal(samples[:, 0], samples[:, 1])
        occupied = np.flatnonzero(samples[:, 0])
        expected = np.concatenate([
            np.arange(sample_boundary(frame, fps), sample_boundary(frame, fps) + 64)
            for frame in starts
        ])
        np.testing.assert_array_equal(occupied, expected)
        assert np.all(samples[occupied, 0] == 2000)
        results[str(fps)] = {"frames": scene.get_frame_count(), "sample_frames": len(samples),
                            "cue_sample_starts": [sample_boundary(frame, fps) for frame in starts]}
    return results


def encoded_video_and_audio(root):
    ffmpeg, ffprobe = shutil.which("ffmpeg"), shutil.which("ffprobe")
    if not ffmpeg or not ffprobe:
        assert not os.environ.get("FMN_REQUIRE_FFMPEG"), "FFmpeg/ffprobe required but absent"
        return {"skipped": "FFmpeg/ffprobe unavailable"}
    cue = root / "encoded-pulse.wav"
    write_cue(cue)
    results = {}
    for fps, mode, expected in ((24, "frames", 864), (24, "seconds", 872), (30, "frames", 864)):
        scene = scene_at(fps)
        scene.file_writer.ffmpeg_bin = ffmpeg
        destination = root / f"reporter-{fps}-{mode}.mp4"
        with scene.render_session(destination, format="mp4", resolution=(48, 32),
                                  fps=fps, threads=1) as session:
            reporter(scene, fps, mode, before_segment=lambda _index: scene.add_sound(str(cue)))
        assert [fact[1:3] for fact in scene._sound_request_facts()] == [
            (frame, fps) for frame in segment_starts(fps, mode)
        ]
        probe = subprocess.run((ffprobe, "-v", "error", "-count_frames", "-show_streams",
                                "-of", "json", str(destination)), check=True,
                               capture_output=True, text=True, timeout=60)
        streams = json.loads(probe.stdout)["streams"]
        video = next(stream for stream in streams if stream["codec_type"] == "video")
        audio = next(stream for stream in streams if stream["codec_type"] == "audio")
        assert int(video["nb_read_frames"]) == session.result.frame_count == scene.get_frame_count() == expected
        assert Fraction(video["avg_frame_rate"]) == fps == session.result.fps
        video_duration = int(video["duration_ts"]) * Fraction(video["time_base"])
        audio_duration = int(audio["duration_ts"]) * Fraction(audio["time_base"])
        assert video_duration == Fraction(expected, fps)
        assert int(audio["sample_rate"]) == 48_000
        # Native PCM positions above are exact. The encoded AAC stream may
        # retain a codec padding block, bounded here by one video frame.
        assert abs(audio_duration - video_duration) < Fraction(1, fps)
        assert abs(Fraction(audio["start_time"])) < Fraction(1, 48_000)
        results[f"{fps}-{mode}"] = {
            "frames": int(video["nb_read_frames"]), "video_seconds": float(video_duration),
            "audio_seconds": float(audio_duration),
        }
    return results


def verify_exact_frame_durations():
    rates = reporter_and_rate_matrix()
    authoring_and_validation()
    planning_and_reporting_boundaries()
    mixed_zero_long_and_boundaries()
    composition_and_callback_boundaries()
    legacy_rate_override_hook()
    specialized_constructor_durations()
    phase_flow_preserves_its_virtual_time()
    speed_profile_frame_durations()
    with tempfile.TemporaryDirectory(prefix="fmn-exact-frames-") as directory:
        root = Path(directory)
        rendered = native_rendered_counts(root)
        mixed = mixed_and_long_rendered_counts(root)
        audio = native_audio_sample_positions(root)
        encoded = encoded_video_and_audio(root)
    return {"rate_matrix": rates, "y4m": rendered, "mixed_y4m": mixed,
            "audio_samples": audio, "mp4": encoded}


exact_frame_results = verify_exact_frame_durations()
print("Exact-frame regression results:", json.dumps(exact_frame_results, sort_keys=True))
