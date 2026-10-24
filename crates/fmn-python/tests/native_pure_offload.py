"""Real portal pixels, exact clocks and authored fallback through native teams.

The oracle forces the existing serial path with a public Scene updater that
only records observations. The production case must report actual admitted
pure frames; a pipeline unit test alone cannot prove portal reachability.
"""
from pathlib import Path
import math
import tempfile

import numpy as np
import manimlib as m


class SerialScene(m.Scene):
    def update_mobjects(self, dt):
        super().update_mobjects(dt)
        self.observations.append((self.get_time(), float(dt)))


def frame_bytes(directory):
    return [path.read_bytes() for path in sorted(directory.glob("*.png"))]


def render_play(root, label, scene_type, fps, threads, depth, moved):
    scene = scene_type()
    scene.observations = []
    directory = root / label
    durations = (.137, .209)
    with scene.render_session(directory, format="png_sequence", resolution=(96, 54),
                              fps=fps, threads=threads) as session:
        square = m.Square(side_length=1.25, fill_color=m.BLUE,
                          fill_opacity=1, stroke_width=0)
        scene.add(square)
        if moved:
            scene.frame.move_to((.2, 0, 0))
        first = square.copy().shift((.75, 0, .5 if depth else 0))
        scene.play(m.Transform(square, first), run_time=durations[0],
                   rate_func=m.there_and_back)
        # A second segment proves sequence continuity across independently
        # joined pipelines. Its endpoint then shares the ordinary wait cache.
        scene.play(m.Transform(square, square.copy().shift((.5, 0, 0))),
                   run_time=durations[1], rate_func=m.linear)
        scene.wait(2 / fps)
    expected = sum(math.ceil(duration * fps) for duration in durations) + 2
    assert session.result.frame_count == expected
    assert scene.get_time() == expected / fps
    np.testing.assert_allclose(square.get_center(), (.5, 0, 0), atol=1e-7)
    frames = frame_bytes(directory)
    assert len(frames) == expected
    assert len(set(frames[:-2])) > 1, "play was replaced with a static frame"
    assert frames[-1] == frames[-2]
    stats = scene._render_performance
    assert stats["frames"] == expected
    assert stats["converted_frames"] + stats["conversion_reuses"] == expected
    return scene, session.result, frames, expected - 2


def independent_plays_match_serial(root):
    total_pure = total_frames = 0
    cases = ((15, 1, False, False), (24, 4, True, False), (60, 16, True, True),
             (24, 64, False, False))
    for index, (fps, threads, depth, moved) in enumerate(cases):
        serial, expected, expected_bytes, pure_count = render_play(
            root, f"serial-{index}", SerialScene, fps, threads, depth, moved)
        native, actual, actual_bytes, _ = render_play(
            root, f"native-{index}", m.Scene, fps, threads, depth, moved)
        assert actual_bytes == expected_bytes, (fps, threads, depth, moved)
        assert actual.engine == expected.engine, "offload changed the declared renderer"
        assert native._render_routes == serial._render_routes
        stats = native._render_performance
        assert stats["pure_play_segments"] == 2, stats
        assert stats["pure_play_frames"] == pure_count > 0, stats
        assert serial._render_performance["pure_play_frames"] == 0
        used, planned, _, in_flight = native._render_plan
        assert 1 <= used <= planned <= in_flight
        if planned > 1:
            assert used > 1, "a multi-team plan still used only the owner renderer"
        observations = [(time, dt) for time, dt in serial.observations if dt > 0]
        assert len(observations) == actual.frame_count
        np.testing.assert_allclose([time for time, _ in observations],
                                   np.arange(1, actual.frame_count + 1) / fps, atol=1e-14)
        np.testing.assert_allclose([dt for _, dt in observations],
                                   [1 / fps] * actual.frame_count, atol=1e-14)
        if depth and not moved:
            assert stats["planar_frames"] > 0 and stats["camera_frames"] > 0
        if moved:
            assert stats["camera_frames"] == actual.frame_count
        total_pure += stats["pure_play_frames"]
        total_frames += actual.frame_count
    return total_pure, total_frames


def retained_tail_survives_waits_and_rejects_edits(root):
    for samples in (1, 2):
        results = []
        for scene_type, label in ((SerialScene, "serial"), (m.Scene, "native")):
            scene = scene_type()
            scene.observations = []
            destination = root / f"tail-{samples}-{label}"
            with scene.render_session(destination, format="png_sequence",
                                      resolution=(96, 54), fps=8, threads=4):
                square = m.Square(fill_color=m.BLUE, fill_opacity=1)
                scene.add(square)
                scene.play(m.Transform(square, square.copy().shift(m.RIGHT)),
                           run_time=samples / 8, rate_func=m.linear)
                scene.wait(2 / 8)
                for edit in (lambda: square.set_color(m.RED),
                             lambda: square.shift(m.UP),
                             lambda: scene.frame.shift(.25 * m.UP)):
                    edit()
                    scene.wait(1 / 8)
            stats = scene._render_performance
            assert stats["conversion_reuses"] == 2, stats
            assert stats["converted_frames"] == samples + 3, stats
            assert stats["raw_frame_reuses"] == 2, stats
            pixels = frame_bytes(destination)
            assert len(pixels) == samples + 5
            assert pixels[samples - 1] == pixels[samples] == pixels[samples + 1]
            assert all(pixels[index] != pixels[index - 1]
                       for index in range(samples + 2, samples + 5))
            results.append(pixels)
        assert results[0] == results[1], "tail handoff changed pixels or ignored edits"


def authored_light_callbacks_match_serial(root):
    results = []
    for scene_type, label in ((m.Scene, "light-callback"), (SerialScene, "light-serial")):
        scene = scene_type()
        scene.observations = []
        calls = []
        directory = root / label
        with scene.render_session(directory, format="png_sequence", resolution=(96, 54),
                                  fps=24, threads=4) as session:
            square = m.Square(fill_opacity=1)
            scene.add(square)
            light = scene.camera.light_source
            center = light.get_center

            def authored_center():
                calls.append(scene.get_time())
                return center()

            light.get_center = authored_center
            scene.play(m.Transform(square, square.copy().shift(m.RIGHT)),
                       run_time=.137, rate_func=m.linear)
        assert scene._render_performance["pure_play_frames"] == 0
        assert len(calls) == session.result.frame_count + 2, calls
        results.append((frame_bytes(directory), calls))
    assert results[0] == results[1], "offload admission skipped an authored light observer"


def native_light_motion_is_sampled_per_frame(root):
    results = []
    for scene_type, label in ((m.Scene, "native-light"), (SerialScene, "native-light-serial")):
        scene = scene_type()
        scene.observations = []
        directory = root / label
        with scene.render_session(directory, format="png_sequence", resolution=(96, 54),
                                  fps=24, threads=4) as session:
            square = m.Square(side_length=2, fill_color=m.WHITE, fill_opacity=1,
                              stroke_width=0).set_shading(0, 0, 1)
            light = scene.camera.light_source
            light.move_to(10 * m.OUT)
            scene.add(square, light)
            scene.play(m.Transform(light, light.copy().move_to(-10 * m.OUT)),
                       run_time=.209, rate_func=m.linear)
        stats = scene._render_performance
        assert session.result.frame_count == stats["camera_frames"] == 6
        assert stats["pure_play_frames"] == (6 if scene_type is m.Scene else 0), stats
        frames = frame_bytes(directory)
        assert len(set(frames)) > 1, "the native light was frozen at its begin position"
        results.append(frames)
    assert results[0] == results[1], "native light sampling changed shaded pixels"


def implicit_sources_views_and_empty_callbacks_stay_serial(root):
    class ObservedSquare(m.Square):
        def __init__(self, *args, **kwargs):
            self.update_calls = []
            super().__init__(*args, **kwargs)

        def update(self, dt=0, recurse=True):
            self.update_calls.append(float(dt))
            return super().update(dt, recurse)

    fallback_cases = 0
    for reason in ("implicit-source", "light-view", "empty-updater-iterator"):
        scene = m.Scene()
        calls = []
        directory = root / reason
        with scene.render_session(directory, format="png_sequence", resolution=(96, 54),
                                  fps=24, threads=4) as session:
            square = ObservedSquare() if reason == "implicit-source" else m.Square()
            square.update_calls = calls
            if reason != "implicit-source":
                scene.add(square)
            if reason == "light-view":
                light_points = scene.camera.light_source.get_points()
            if reason == "empty-updater-iterator":
                class EmptyUpdaters(list):
                    def __iter__(self):
                        calls.append("iterate")
                        return super().__iter__()
                square.updaters = EmptyUpdaters()
            scene.play(m.Transform(square, square.copy().shift(m.RIGHT)),
                       run_time=.137, rate_func=m.linear)
            if reason == "light-view":
                assert light_points.shape[0] > 0
        assert session.result.frame_count == 4
        assert scene._render_performance["pure_play_frames"] == 0, reason
        if reason != "light-view":
            assert calls, f"{reason} lost its authored update observer"
        fallback_cases += 1
    return fallback_cases


def zero_duration_and_failure_do_not_publish_extra_frames(root):
    scene = m.Scene()
    destination = root / "zero-duration"
    with scene.render_session(destination, format="png_sequence", resolution=(96, 54),
                              fps=24, threads=4) as session:
        square = m.Square()
        scene.add(square)
        scene.play(m.Transform(square, square.copy().shift(m.RIGHT)),
                   run_time=.137, rate_func=m.linear)
        before = scene.get_time()
        scene.play(m.Transform(square, square.copy().shift(m.UP)),
                   run_time=0, rate_func=m.linear)
        assert scene.get_time() == before
        np.testing.assert_allclose(square.get_center(), (1, 1, 0), atol=1e-7)
    assert session.result.frame_count == 4
    assert scene._render_performance["pure_play_frames"] == 4
    assert len(frame_bytes(destination)) == 4

    failed = root / "must-not-publish"
    sentinel = RuntimeError("authored failure after a joined pure play")
    scene = m.Scene()
    try:
        with scene.render_session(failed, format="png_sequence", resolution=(96, 54),
                                  fps=24, threads=4):
            square = m.Square()
            scene.add(square)
            scene.play(m.Transform(square, square.copy().shift(m.RIGHT)),
                       run_time=.137, rate_func=m.linear)
            raise sentinel
    except RuntimeError as error:
        assert error is sentinel, "offload cancellation replaced the authored root cause"
    else:
        raise AssertionError("authored render failure was swallowed")
    assert not failed.exists(), "cancelled output was published"


def run_native_pure_offload_acceptance(directory):
    root = Path(directory)
    root.mkdir(parents=True, exist_ok=True)
    pure, frames = independent_plays_match_serial(root)
    native_light_motion_is_sampled_per_frame(root)
    authored_light_callbacks_match_serial(root)
    fallbacks = implicit_sources_views_and_empty_callbacks_stay_serial(root)
    zero_duration_and_failure_do_not_publish_extra_frames(root)
    retained_tail_survives_waits_and_rejects_edits(root)
    return pure + 6, frames + 6, fallbacks + 2


if __name__ == "__main__":
    with tempfile.TemporaryDirectory(prefix="fmn-native-pure-offload-") as directory:
        print(run_native_pure_offload_acceptance(directory))
