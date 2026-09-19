"""Installed-extension acceptance: real glyphs, native playback and Reel bytes.

This suite intentionally imports manimlib without a fixture fallback. Run via
scripts/check_portal_runtime.sh against a wheel built from the tested source.
"""
from pathlib import Path
import tempfile

import numpy as np
import manimlib as m
from fmn_python import render_scene


def nested_text():
    text = m.Text("alpha beta", font_size=36)
    old_paths = [list(path) for path in text._string_sub_paths]
    assert old_paths and len(old_paths) == len(text._string_sub_spans)
    inner = m.VGroup(*list(text.submobjects))
    text.set_submobjects([m.VGroup(inner)])
    text._string_sub_paths = [[0, 0, *path] for path in old_paths]
    return text


def snapshot(text):
    return [(node, tuple(node.submobjects)) for node in text.get_family()]


def assert_restored(rows):
    for node, children in rows:
        assert len(node.submobjects) == len(children)
        assert all(a is b for a, b in zip(node.submobjects, children))
        assert not node._is_updating_suspended()
        assert not getattr(node, "_is_animating", False)


for cls in (m.AddTextWordByWord, m.AddTextLetterByLetter):
    text = nested_text()
    rows = snapshot(text)
    animation = cls(text, suspend_mobject_updating=True, run_time=.5)
    animation.begin()
    animation.interpolate(.5)
    animation.abort()
    assert_restored(rows)
    animation.begin()
    animation.finish()
    assert_restored(rows)

# A real native Scene.play, including an AnimationGroup callback driver.
scene = m.Scene(camera_config=dict(resolution=(192, 108), fps=8))
text = nested_text()
rows = snapshot(text)
scene.play(m.AnimationGroup(m.AddTextWordByWord(text, run_time=.5)))
assert_restored(rows)


class RevealScene(m.Scene):
    default_camera_config = dict(resolution=(192, 108), fps=8)

    def construct(self):
        self.text = nested_text()
        self.rows = snapshot(self.text)
        self.play(m.AddTextWordByWord(self.text, run_time=.5))
        assert_restored(self.rows)


def luma_frames(path):
    header, payload = Path(path).read_bytes().split(b"\n", 1)
    assert header == b"YUV4MPEG2 W192 H108 F8:1 Ip A1:1 C420mpeg2", header
    size = 192 * 108 * 3 // 2
    assert len(payload) % (size + 6) == 0
    frames = []
    for offset in range(0, len(payload), size + 6):
        assert payload[offset:offset + 6] == b"FRAME\n"
        frames.append(np.frombuffer(payload[offset + 6:offset + 6 + 192 * 108], dtype=np.uint8))
    return frames


root = Path(tempfile.mkdtemp(prefix="fmn-text-reveal-"))
outputs = []
for threads in (1, 4):
    result = render_scene(RevealScene, root / f"reveal-{threads}.y4m", threads=threads)
    assert result.frame_count == 4 and result.certified is False
    frames = luma_frames(result.destination)
    assert len(frames) == 4
    assert np.count_nonzero(frames[-1] > 180) > np.count_nonzero(frames[0] > 180)
    outputs.append(result.destination.read_bytes())
assert outputs[0] == outputs[1]


class RevealFailure(RuntimeError):
    pass


class BrokenReveal(RevealScene):
    def construct(self):
        self.text = nested_text()
        self.rows = snapshot(self.text)
        def rate(alpha):
            if alpha > .5:
                raise RevealFailure("failure after real native frame capture")
            return alpha
        self.play(m.AddTextLetterByLetter(self.text, rate_func=rate, run_time=.5,
                                         suspend_mobject_updating=True))


failed = BrokenReveal()
destination = root / "not-published.y4m"
try:
    render_scene(failed, destination, threads=1)
except RevealFailure:
    pass
else:
    raise AssertionError("authored reveal failure was not propagated")
assert not destination.exists()
assert_restored(failed.rows)
print("native text reveal acceptance: nested glyph identity, playback, thread replay, cancellation")
