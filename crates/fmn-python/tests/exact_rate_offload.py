"""Actual portal PNGs and state under exact declared-pure rate preparation."""
from pathlib import Path
import math
import tempfile

import numpy as np
import manimlib as m
from fmn_python import PureRateFunction


def cubic(alpha):
    return alpha ** 3


def there_and_back_with_pause(alpha):
    if alpha < 1 / 3:
        return 3 * alpha
    if alpha < 2 / 3:
        return 1.0
    return 3 * (1 - alpha)


def discontinuous(alpha):
    if alpha < .37:
        return 0.0
    if alpha < .81:
        return .6
    return 1.0


def render_case(directory, fps, threads, declared, curve, reproducible):
    scene = m.Scene()
    calls = []

    def outer(alpha):
        calls.append(float(alpha))
        return curve(alpha)

    rate = PureRateFunction(curve) if declared else outer
    step = PureRateFunction(discontinuous) if declared else discontinuous
    sources = {"exact_rate_offload.py": Path(__file__).read_bytes()} if reproducible else None
    with scene.render_session(directory, format="png_sequence", resolution=(96, 54),
                              fps=fps, threads=threads, reproducible=reproducible,
                              sources=sources) as session:
        shapes = [m.Square(side_length=.24, fill_opacity=1,
                           fill_color=m.BLUE, stroke_width=0).shift(
                               ((index % 5 - 2) * .7, (index // 5 - 1.5) * .7, 0))
                  for index in range(20)]
        for shape in shapes:
            shape.add(m.Square(side_length=.08, fill_opacity=1,
                               fill_color=m.YELLOW, stroke_width=0).move_to(
                                   shape.get_center() + np.array((.04, -.04, 0))))
        scene.add(*shapes)
        children = [m.Transform(shape, shape.copy().shift((.3, .2, 0)),
                                run_time=.237, lag_ratio=.17,
                                time_span=(.021, .219), rate_func=step)
                    for shape in shapes]
        group = m.LaggedStart(
            m.LaggedStart(*children[:10], lag_ratio=.1, rate_func=m.linear),
            m.LaggedStart(*children[10:], lag_ratio=.1, rate_func=m.linear),
            lag_ratio=.13, run_time=.413, time_span=(.031, .397), rate_func=rate,
        )
        scene.play(group)
        # Reusing a declaration at a new duration/family layout must collect a
        # fresh grid, and a play-level override must use the same exact path.
        scene.play(m.Transform(shapes[0], shapes[0].copy().shift((.1, 0, 0))),
                   run_time=.137, rate_func=rate)
        scene.wait(1 / fps)
    frames = [path.read_bytes() for path in sorted(directory.glob("*.png"))]
    expected_frames = math.ceil(.413 * fps) + math.ceil(.137 * fps) + 1
    assert session.result.frame_count == expected_frames
    assert session.result.certified is reproducible
    if reproducible:
        assert session.result.manifest is not None
        assert session.result.manifest.is_file()
    assert len(frames) == expected_frames
    assert scene.get_time() == expected_frames / fps
    stats = scene._render_performance
    if declared:
        assert stats["exact_rate_segments"] == 2, stats
        assert stats["exact_rate_samples"] > 2, stats
        assert stats["pure_play_segments"] == 2, stats
        assert stats["pure_play_frames"] == expected_frames - 1, stats
    else:
        assert stats["exact_rate_segments"] == 0, stats
        assert stats["pure_play_segments"] == 0, stats
    points = [member.get_points().copy()
              for shape in shapes for member in shape.get_family()]
    return frames, points, scene.get_time(), scene.num_plays, calls, stats


def errors_precede_live_scene_mutation():
    class AuthoredError(Exception):
        pass

    scene = m.Scene()
    shape = m.Square()
    scene.add(shape)
    before = shape.get_points().copy()
    calls = []
    failure = AuthoredError("keep this exception")

    def broken(alpha):
        calls.append(alpha)
        if alpha > .2:
            raise failure
        return alpha

    try:
        scene.play(m.Transform(shape, shape.copy().shift(m.RIGHT)),
                   rate_func=PureRateFunction(broken))
    except AuthoredError as error:
        assert error is failure
        assert str(error) == "keep this exception"
    else:
        raise AssertionError("the authored rate failure disappeared")
    np.testing.assert_array_equal(shape.get_points(), before)
    assert scene.get_time() == 0.0
    assert not shape._is_updating_suspended()
    assert len([alpha for alpha in calls if alpha > .2]) == 1

    for invalid in (float("nan"), float("inf")):
        try:
            scene.play(m.Transform(shape, shape.copy().shift(m.RIGHT)),
                       rate_func=PureRateFunction(lambda _: invalid))
        except (ValueError, RuntimeError) as error:
            assert "non-finite" in str(error), str(error)
        else:
            raise AssertionError("a nonfinite rate was admitted")
        np.testing.assert_array_equal(shape.get_points(), before)
        assert scene.get_time() == 0.0


def preparation_preserves_first_failure():
    class AuthoredError(Exception):
        pass

    scene = m.Scene()
    first, second = m.Square(), m.Square().shift(2 * m.RIGHT)
    scene.add(first, second)
    before = [shape.get_points().copy() for shape in (first, second)]
    body_failure = AuthoredError("the initiating body exception")
    finish_failure = AuthoredError("later cleanup must not run this callback")
    calls = []

    def finish_only(alpha):
        calls.append(("first", alpha))
        if alpha == 1.0:
            raise finish_failure
        return alpha

    def body(alpha):
        calls.append(("second", alpha))
        if alpha >= .1:
            raise body_failure
        return alpha

    try:
        scene.play(
            m.Transform(first, first.copy().shift(m.UP),
                        rate_func=PureRateFunction(finish_only)),
            m.Transform(second, second.copy().shift(m.UP),
                        rate_func=PureRateFunction(body)),
            run_time=.2,
        )
    except AuthoredError as error:
        assert error is body_failure, (error, calls)
    else:
        raise AssertionError("the initiating rate exception disappeared")
    assert ("first", 1.0) not in calls, calls
    assert calls[-1][0] == "second" and calls[-1][1] >= .1, calls
    for shape, expected in zip((first, second), before, strict=True):
        np.testing.assert_array_equal(shape.get_points(), expected)
        assert not shape._is_updating_suspended()
    assert scene.get_time() == 0.0


def undeclared_stateful_rates_keep_live_semantics():
    scene = m.Scene()
    shape = m.Square()
    scene.add(shape)
    seen = []

    def live(alpha):
        seen.append((alpha, scene.get_time()))
        return alpha

    scene.play(m.Transform(shape, shape.copy().shift(m.RIGHT)),
               run_time=.2, rate_func=live)
    assert len(seen) > 2
    assert any(time > 0 for _, time in seen), seen

    updates = []
    shape.add_updater(lambda mob, dt: updates.append(dt), call=False)
    before = shape.get_points().copy()
    try:
        scene.play(m.Transform(shape, shape.copy().shift(m.RIGHT)),
                   run_time=.1, rate_func=PureRateFunction(cubic))
    except ValueError as error:
        assert "callback-free native play" in str(error), str(error)
    else:
        raise AssertionError("effectful play was rehearsed as pure")
    np.testing.assert_array_equal(shape.get_points(), before)
    assert not updates, updates


def _play_state(scene, *mobjects):
    families = [tuple(mobject.get_family()) for mobject in mobjects]
    return {
        "roots": tuple(id(root) for root in scene.mobjects),
        "families": tuple(tuple(id(member) for member in family)
                          for family in families),
        "points": [member.get_points().copy()
                   for family in families for member in family],
        "lookup": {key: id(value)
                   for key, value in scene.id_to_mobject_map.items()},
        "time": scene.get_time(),
        "plays": scene.num_plays,
    }


def _assert_play_state(scene, expected, *mobjects):
    actual = _play_state(scene, *mobjects)
    for key in ("roots", "families", "lookup", "time", "plays"):
        assert actual[key] == expected[key], (key, actual[key], expected[key])
    for actual_points, expected_points in zip(
            actual["points"], expected["points"], strict=True):
        np.testing.assert_array_equal(actual_points, expected_points)
    for mobject in mobjects:
        assert not mobject._is_updating_suspended()


def callback_lowering_preserves_declared_rate_refusal():
    # A declaration must survive the callback selection used for authored
    # lifecycle methods, updaters, and mixed/nested animation compositions.
    for mode in ("updater", "hook", "mixed", "nested"):
        scene = m.Scene()
        first, second = m.Square(), m.Square().shift(2 * m.RIGHT)
        scene.add(first, second)
        events = []

        def should_not_run(alpha):
            events.append(("rate", alpha))
            return alpha

        declaration = PureRateFunction(should_not_run)

        class HookedTransform(m.Transform):
            def begin(self):
                events.append(("begin",))
                return super().begin()

        if mode == "updater":
            first.add_updater(
                lambda mob, dt: events.append(("updater", dt)), call=False)
            animations = [m.Transform(first, first.copy().shift(m.UP),
                                      rate_func=declaration)]
        elif mode == "hook":
            animations = [HookedTransform(first, first.copy().shift(m.UP),
                                          rate_func=declaration)]
        elif mode == "mixed":
            animations = [
                m.Transform(first, first.copy().shift(m.UP),
                            rate_func=declaration),
                HookedTransform(second, second.copy().shift(m.UP)),
            ]
        else:
            animations = [m.AnimationGroup(
                m.Transform(first, first.copy().shift(m.UP),
                            rate_func=declaration),
                m.UpdateFromFunc(
                    second, lambda mob: events.append(("update_from_func",))),
            )]
        before = _play_state(scene, first, second)
        try:
            scene.play(*animations, run_time=.2)
        except ValueError as error:
            assert "PureRateFunction" in str(error), (mode, str(error))
        else:
            raise AssertionError(f"{mode} lost its declared-rate admission")
        assert not events, (mode, events)
        _assert_play_state(scene, before, first, second)


def camera_refusal_preserves_removed_frame():
    for mode in ("explicit", "builder", "drawable"):
        scene = m.Scene()
        shape = m.Square()
        scene.add(shape)
        frame = scene.frame
        scene.remove(frame)
        calls = []

        def should_not_run(alpha):
            calls.append(alpha)
            return alpha

        declaration = PureRateFunction(should_not_run)
        if mode == "builder":
            animations = [frame.animate(
                run_time=.2, rate_func=declaration).shift(m.RIGHT)]
        else:
            animations = [m.Transform(
                frame, frame.copy().shift(m.RIGHT),
                run_time=.2,
                rate_func=m.linear if mode == "drawable" else declaration)]
            if mode == "drawable":
                animations.append(m.Transform(
                    shape, shape.copy().shift(m.UP), rate_func=declaration))
        before = _play_state(scene, shape, frame)
        pose = (frame.get_center().copy(), frame.get_orientation().copy(),
                frame.get_width(), frame.get_height(), frame.get_field_of_view())
        core_identity = id(frame._core)
        assert all(root is not frame for root in scene.mobjects)
        try:
            scene.play(*animations, run_time=.2)
        except ValueError as error:
            assert "PureRateFunction" in str(error), (mode, str(error))
        else:
            raise AssertionError(f"{mode} camera declaration bypassed admission")
        assert not calls, (mode, calls)
        _assert_play_state(scene, before, shape, frame)
        assert scene.frame is frame and id(frame._core) == core_identity
        np.testing.assert_array_equal(frame.get_center(), pose[0])
        np.testing.assert_array_equal(frame.get_orientation(), pose[1])
        assert (frame.get_width(), frame.get_height(),
                frame.get_field_of_view()) == pose[2:]

        # An ordinary root override makes the declaration inactive. Normal
        # camera playback must still restore a removed frame at begin.
        if mode != "drawable":
            scene.play(*animations, run_time=.1, rate_func=m.linear)
            assert not calls, (mode, calls)
            assert any(root is frame for root in scene.mobjects)
            np.testing.assert_allclose(frame.get_center(), pose[0] + m.RIGHT,
                                       rtol=0, atol=1e-6)


def multi_object_refusal_preserves_membership():
    scene = m.Scene()
    first, second = m.Square(), m.Square().shift(2 * m.RIGHT)
    scene.add(first)
    calls = []

    def should_not_run(alpha):
        calls.append(alpha)
        return alpha

    # Installed Swap uses a deferred Python Transform over a Group. Verify
    # that its declaration is refused before creating targets or rooting it.
    animation = m.Swap(first, second,
                       rate_func=PureRateFunction(should_not_run))
    before = _play_state(scene, first, second)
    try:
        scene.play(animation, run_time=.2)
    except ValueError as error:
        assert "PureRateFunction" in str(error), str(error)
    else:
        raise AssertionError("Swap discarded its declared-rate admission")
    assert not calls, calls
    _assert_play_state(scene, before, first, second)

    # Also exercise the native multi-operand lowering seam. It must adopt an
    # extra operand to resolve handles, but leave rooting to successful begin.
    class NativeSwapProbe(m._NativeAnimation):
        _native_kind = "swap"

        def __init__(self, first, second, **kwargs):
            super().__init__(first, **kwargs)
            self.mobjects = [first, second]
            self._native_extra_mobjects = (second,)

        def _native_params(self):
            return {"mobs": self.mobjects, "path_arc": 0.0}

    class AuthoredError(Exception):
        pass

    scene = m.Scene()
    first, second = m.Square(), m.Square().shift(2 * m.RIGHT)
    scene.add(first)
    failure = AuthoredError("native multi-object preparation failed")
    calls = []

    def broken(alpha):
        calls.append(alpha)
        raise failure

    animation = NativeSwapProbe(
        first, second, rate_func=PureRateFunction(broken))
    before = _play_state(scene, first, second)
    try:
        scene.play(animation, run_time=.2)
    except AuthoredError as error:
        assert error is failure
    else:
        raise AssertionError("native multi-object rate failure disappeared")
    assert len(calls) == 1, calls
    _assert_play_state(scene, before, first, second)
    assert all(root is not second for root in scene.mobjects)

    first_center, second_center = first.get_center(), second.get_center()
    scene.play(NativeSwapProbe(first, second, rate_func=m.linear), run_time=.1)
    np.testing.assert_allclose(first.get_center(), second_center,
                               rtol=0, atol=1e-6)
    np.testing.assert_allclose(second.get_center(), first_center,
                               rtol=0, atol=1e-6)
    visible_family = [member for root in scene.mobjects
                      for member in root.get_family()]
    assert any(member is first for member in visible_family)
    assert any(member is second for member in visible_family)


def embedded_certification_refuses_before_mutation(root):
    # The native Cargo harness is an in-memory module. It must never invent
    # an installed extension identity to certify the standard-mode cases below.
    scene = m.Scene()
    before = _play_state(scene)
    destination = root / "refused-certified"
    try:
        with scene.render_session(destination, format="png_sequence",
                                  resolution=(96, 54), reproducible=True,
                                  sources={"empty.py": b""}):
            raise AssertionError("unidentified embedded runtime was certified")
    except m._CapabilityError as error:
        assert "portal runtime requires a file-backed native extension" in str(error), error
    else:
        raise AssertionError("embedded certification did not fail closed")
    _assert_play_state(scene, before)
    assert not destination.exists()
    assert not destination.with_name(destination.name + ".manifest").exists()


def detached_composition_observers_stay_serial(root):
    for mode in ("public-update", "empty-updater-iterator"):
        scene = m.Scene()
        calls = []
        destination = root / ("detached-" + mode)
        with scene.render_session(destination, format="png_sequence",
                                  resolution=(96, 54), fps=24, threads=4) as session:
            square = m.Square()
            scene.add(square)
            group = m.LaggedStart(
                m.Transform(square, square.copy().shift(m.RIGHT)),
                run_time=.137, rate_func=m.linear)
            container = group.mobject
            assert all(container is not root for root in scene.mobjects)
            if mode == "public-update":
                original = container.update

                def update(dt=0, recurse=True):
                    calls.append(float(dt))
                    return original(dt, recurse)

                container.update = update
            else:
                class EmptyUpdaters(list):
                    def __iter__(self):
                        calls.append("iterate")
                        return super().__iter__()

                container.updaters = EmptyUpdaters()
            calls.clear()
            scene.play(group)
        assert session.result.frame_count == 4
        assert scene._render_performance["pure_play_frames"] == 0, mode
        assert len(calls) >= 4, (mode, calls)
        if mode == "public-update":
            assert sum(dt > 0 for dt in calls) == 4, calls
        np.testing.assert_allclose(square.get_center(), m.RIGHT, rtol=0, atol=1e-7)


def _run_exact_rate_cases(root, reproducible):
    if not reproducible:
        embedded_certification_refuses_before_mutation(root)
    compared_frames = distinct_samples = 0
    for curve in (cubic, there_and_back_with_pause):
        for fps, threads in ((15, 1), (24, 4), (30, 1), (60, 16)):
            name = curve.__name__
            serial = render_case(root / f"serial-{name}-{fps}", fps, threads,
                                 False, curve, reproducible)
            native = render_case(root / f"native-{name}-{fps}", fps, threads,
                                 True, curve, reproducible)
            assert native[0] == serial[0], (name, fps, "PNG pixels differ")
            for actual, expected in zip(native[1], serial[1], strict=True):
                np.testing.assert_array_equal(actual, expected)
            assert native[2:4] == serial[2:4], (name, fps, native[2:4], serial[2:4])
            compared_frames += len(native[0])
            distinct_samples += native[5]["exact_rate_samples"]
    errors_precede_live_scene_mutation()
    preparation_preserves_first_failure()
    callback_lowering_preserves_declared_rate_refusal()
    camera_refusal_preserves_removed_frame()
    multi_object_refusal_preserves_membership()
    undeclared_stateful_rates_keep_live_semantics()
    detached_composition_observers_stay_serial(root)
    return compared_frames, distinct_samples, 16 * distinct_samples


def run_exact_rate_acceptance(root=None, reproducible=True):
    # The standalone installed-wheel gate always takes the certified default.
    # Only the explicit embedded Rust caller selects standard rendering.
    if root is None:
        with tempfile.TemporaryDirectory(prefix="fmn-exact-rate-") as temporary:
            return _run_exact_rate_cases(Path(temporary), reproducible)
    root = Path(root)
    root.mkdir(parents=True, exist_ok=True)
    return _run_exact_rate_cases(root, reproducible)


if __name__ == "__main__":
    frames, samples, table_bytes = run_exact_rate_acceptance()
    print(f"certified exact-rate acceptance: {frames} frames, {samples} samples, {table_bytes} table bytes")
