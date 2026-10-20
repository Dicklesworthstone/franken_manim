"""Real portal reuse versus independent one-frame output generations.

The cold oracle constructs a fresh Scene/renderer/converter for each state;
the measured path keeps one generation across waits and mutations. No output
or scheduler is mocked, and assertions depend on work counters and bytes,
never machine speed.
"""
from pathlib import Path
import tempfile

import numpy as np
import manimlib as m


def widgets():
    label = m.Text("Static wait", font="IBM Plex Sans", font_size=18).shift(2 * m.UP)
    square = m.Square(side_length=1.25, fill_color=m.BLUE,
                      fill_opacity=1, stroke_width=0)
    return label, square


def apply_state(scene, square, points, initial, state):
    x, color, visible, camera_x, background = state
    # Keep the writable NumPy view alive across captures. Scene snapshots and
    # the retained plan must observe foreign writes before proving reuse.
    points[:] = initial
    points[:, 0] += x
    square.set_fill(color, opacity=1)
    scene.frame.move_to((camera_x, 0, 0))
    scene.set_background_color(background)
    if visible:
        scene.add(square)
    else:
        scene.remove(square)


def state_changes_match_cold_frames(root):
    states = [
        (0, m.BLUE, True, 0, m.BLACK),
        (0, m.BLUE, True, 0, m.BLACK),
        (0, m.BLUE, True, 0, "#182030"),
        (0, m.BLUE, True, 0, "#182030"),
        (0, m.BLUE, True, .5, "#182030"),
        (0, m.BLUE, True, .5, "#182030"),
        (0, m.BLUE, True, 0, "#182030"),
        (0, m.BLUE, True, 0, "#182030"),
        (0, m.BLUE, False, 0, "#182030"),
        (0, m.BLUE, False, 0, "#182030"),
        (.5, m.RED, True, 0, "#182030"),
        (.5, m.RED, True, 0, "#182030"),
    ]
    expected = []
    for index, state in enumerate(states):
        scene = m.Scene()
        directory = root / f"cold-{index}"
        with scene.render_session(directory, format="png_sequence", resolution=(96, 54),
                                  fps=8, threads=1) as session:
            label, square = widgets()
            scene.add(label, square)
            points = square.get_points()
            apply_state(scene, square, points, points.copy(), state)
            scene.wait(1 / 8)
        assert session.result.frame_count == 1
        assert scene._render_performance["converted_frames"] == 1
        assert scene._render_performance["conversion_reuses"] == 0
        expected.append(next(directory.glob("*.png")).read_bytes())

    for threads in (1, 4, 16):
        scene = m.Scene()
        directory = root / f"retained-{threads}"
        with scene.render_session(directory, format="png_sequence", resolution=(96, 54),
                                  fps=8, threads=threads) as session:
            label, square = widgets()
            scene.add(label, square)
            points = square.get_points()
            initial = points.copy()
            for state in states:
                apply_state(scene, square, points, initial, state)
                scene.wait(1 / 8)
        actual = [path.read_bytes() for path in sorted(directory.glob("*.png"))]
        assert actual == expected, f"retained conversion changed captured pixels at {threads} threads"
        assert session.result.frame_count == len(states)
        assert scene.get_time() == len(states) / 8
        stats = scene._render_performance
        assert stats["schema_version"] == 1
        assert stats["frames"] == len(states)
        assert stats["planar_frames"] > 0 and stats["camera_frames"] > 0
        assert stats["raw_frame_reuses"] == stats["conversion_reuses"]
        assert stats["conversion_reuses"] >= 5, stats
        assert stats["converted_frames"] + stats["conversion_reuses"] == len(states)
        assert stats["conversion_cache_bytes"] == 96 * 54 * 4
        assert stats["tile_cache_hits"] > 0


def callbacks_clock_and_animation_finish_are_preserved(root):
    class Observed(m.Scene):
        def update_mobjects(self, dt):
            super().update_mobjects(dt)
            self.observations.append((self.get_time(), float(dt)))

    for threads in (1, 4, 16):
        scene = Observed()
        scene.observations = []
        directory = root / f"callbacks-{threads}"
        with scene.render_session(directory, format="png_sequence", resolution=(96, 54),
                                  fps=8, threads=threads) as session:
            label, square = widgets()
            scene.add(label, square)
            scene.play(square.animate.shift(m.RIGHT), run_time=.5, rate_func=m.linear)
            scene.wait(3 / 8)
        np.testing.assert_allclose(square.get_center(), (1, 0, 0), atol=1e-12)
        assert scene.get_time() == 7 / 8
        assert session.result.frame_count == 7
        positive = [(time, dt) for time, dt in scene.observations if dt > 0]
        assert len(positive) == 7, "reuse skipped a public scene update"
        np.testing.assert_allclose([dt for _, dt in positive], [1 / 8] * 7, atol=0)
        np.testing.assert_allclose([time for time, _ in positive], np.arange(1, 8) / 8, atol=0)
        frames = [path.read_bytes() for path in sorted(directory.glob("*.png"))]
        assert len(set(frames[:4])) > 1, "animation samples were replaced by a static frame"
        assert len(set(frames[-4:])) == 1, "finish/wait pixels diverged from the final sample"
        assert scene._render_performance["conversion_reuses"] >= 3


def run_retained_conversion_acceptance():
    with tempfile.TemporaryDirectory(prefix="fmn-retained-conversion-") as directory:
        root = Path(directory)
        state_changes_match_cold_frames(root)
        callbacks_clock_and_animation_finish_are_preserved(root)


run_retained_conversion_acceptance()
