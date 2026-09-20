"""Installed-native matching authoring, intermediate frames and failure paths.

Registered in check_portal_runtime.sh; no source injection or native doubles.
"""
from pathlib import Path
import tempfile

import numpy as np
import manimlib as ml


def test_string_hook_renders_translated_text_at_actual_sample_points():
    def render(destination, threads):
        calls, samples = [], []
        class RecordedTransform(ml.Transform):
            def interpolate_mobject(self, alpha):
                result = super().interpolate_mobject(alpha)
                # Empty group parents precede their glyph children in the
                # family walk. Observe the complete frame, not the parent's
                # submobject hook before the descendants have interpolated.
                samples.append((float(alpha), self.mobject.get_center().copy()))
                return result
        class AuthoredStrings(ml.TransformMatchingStrings):
            def matching_blocks(self, source, target, matched_keys, key_map):
                calls.append((source, target))
                return super().matching_blocks(source, target, matched_keys, key_map)
        scene = ml.Scene()
        with scene.render_session(destination, format="png_sequence", resolution=(160, 90),
                                  fps=4, threads=threads) as session:
            source = ml.Text("move").move_to(ml.LEFT)
            target = ml.Text("move").move_to(ml.RIGHT)
            source_spans, target_spans = list(source._string_sub_spans), list(target._string_sub_spans)
            scene.add(source)
            animation = AuthoredStrings(source, target, match_animation=RecordedTransform,
                                        key_map={"move": "move"}, run_time=1, rate_func=ml.linear)
            scene.play(animation)
            assert len(calls) == 1, "constructor must call the authored block hook exactly once"
            assert animation.matched_pairs == [], "inferred blocks are not explicit native pair claims"
            assert source._string_sub_spans == source_spans and target._string_sub_spans == target_spans
            assert target in scene.mobjects and source not in scene.mobjects
        assert session.result.frame_count == 4
        midpoints = [point for alpha, point in samples if np.isclose(alpha, .5)]
        assert midpoints, "native playback never invoked the authored midpoint"
        np.testing.assert_allclose(midpoints[0], [0, 0, 0], atol=1e-6)
        frames = [path.read_bytes() for path in sorted(Path(destination).glob("*.png"))]
        assert len(frames) == 4 and len(set(frames)) > 1, "requires actual changing native images"
        return frames
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        assert render(root / "one", 1) == render(root / "four", 4), "thread count changed matching output"


def test_custom_block_plan_controls_real_children_and_native_metadata():
    calls = []
    class FirstOnly(ml.TransformMatchingStrings):
        def matching_blocks(self, source, target, matched_keys, key_map):
            calls.append("blocks")
            return [(source._string_submobject(0), target._string_submobject(0))]
    source, target = ml.Text("ab"), ml.Text("ba").shift(ml.RIGHT)
    animation = FirstOnly(source, target, run_time=2 / 30)
    assert [child._native_kind for child in animation.animations] == [
        "transform", "fade_out_to_point", "fade_in_from_point",
    ]
    params = animation._native_params()
    assert "source_keys" in params and "target_keys" in params
    scene = ml.Scene()
    scene.add(source)
    scene.play(animation)
    assert calls == ["blocks"]
    assert target in scene.mobjects and source not in scene.mobjects


def test_invalid_native_span_cannot_be_bypassed_by_authored_matcher():
    calls = []
    class Authored(ml.TransformMatchingStrings):
        def matching_blocks(self, *args):
            calls.append("blocks")
            return []
    source, target = ml.Text("é"), ml.Text("é")
    previous = source._string_sub_spans
    try:
        source._string_sub_spans = [(0, 1)]
        try:
            Authored(source, target)
        except Exception as error:
            assert "UTF-8" in str(error), error
        else:
            raise AssertionError("invalid UTF-8 source span accepted")
        assert calls == []
    finally:
        source._string_sub_spans = previous


def test_matching_callback_failure_keeps_source_and_allows_next_play():
    failure = RuntimeError("authored matching interpolation failed")
    class Broken(ml.Transform):
        def interpolate_submobject(self, current, start, target, alpha):
            if alpha > 0:
                raise failure
            return super().interpolate_submobject(current, start, target, alpha)
    source, target = ml.Square(), ml.Square().shift(ml.RIGHT)
    scene = ml.Scene()
    scene.add(source)
    animation = ml.TransformMatchingParts(source, target, match_animation=Broken,
                                         suspend_mobject_updating=True, run_time=2 / 30)
    try:
        scene.play(animation)
    except RuntimeError as error:
        assert error is failure
    else:
        raise AssertionError("authored matching failure disappeared")
    assert target not in scene.mobjects
    assert not source._is_updating_suspended()
    assert animation._composition_driver is None
    scene.play(ml.Transform(source, source.copy().shift(ml.UP), run_time=2 / 30))
    np.testing.assert_allclose(source.get_center(), [0, 1, 0], atol=1e-6)


assert getattr(ml, "__franken_manim__", False), "requires the real native FrankenManim portal"
_cases = sorted((name, case) for name, case in globals().items()
                if name.startswith("test_") and callable(case))
assert len(_cases) == 4, "matching native authoring acceptance inventory changed"
for _name, _case in _cases:
    _case()
    print("matching authoring acceptance passed:", _name)
print("matching authoring native cases:", len(_cases))
