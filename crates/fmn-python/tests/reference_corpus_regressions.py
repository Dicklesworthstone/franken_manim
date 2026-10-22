"""Minimized constructs from corpus scenes the portal once refused or rendered
differently, and Reference helpers it once computed differently, each checked
against what the pinned Reference showed.

The e2e scenario parity.reference_corpus_regressions.v1 runs this module in the
embedded interpreter (fm-5wq.27..30, fm-sq8.10 and fm-5wq.11 each add their
cases here). Every case records the bead, the construct, the observed and
expected values, the expectation's provenance and a verdict; no timestamps or
addresses. The module uses only Reference-public API, so it also runs under the
pinned Reference itself, where every case must pass.
"""
import math

import manimlib as m
import numpy as np

REFERENCE = "3b1b/manim@6199a00d"


def _decimal_text(value, **kwargs):
    return m.DecimalNumber(value, **kwargs).get_tex()


def _set_value_text(value):
    number = m.DecimalNumber(1.5)
    number.set_value(value)
    return number.get_tex()


def _span(indices):
    if len(indices) > 1 and indices == list(range(indices[0], indices[-1] + 1)):
        return f"{indices[0]}-{indices[-1]}"
    return ",".join(map(str, indices)) or "-"


def _matching(source, target):
    """Which source glyphs transform into which target glyphs, and which fade.

    Read through the Reference's public surface only: the group's animations,
    each one's mobject and (for a transform) target_mobject, located among the
    two strings' family_members_with_points by identity.
    """
    animation = m.TransformMatchingStrings(source, target)
    sources = source.family_members_with_points()
    targets = target.family_members_with_points()

    def located(pieces, mobject):
        if mobject is None:
            return []
        family = mobject.family_members_with_points()
        return [i for i, piece in enumerate(pieces) if any(piece is f for f in family)]

    order = {"transform": 0, "fade_out": 1, "fade_in": 2}
    parts = []
    for child in animation.animations:
        moved = located(sources, child.mobject)
        landed = located(targets, getattr(child, "target_mobject", None))
        if moved and landed:
            parts.append(("transform", moved[0], f"transform s{_span(moved)}>t{_span(landed)}"))
        elif moved:
            parts.append(("fade_out", moved[0], f"fade_out s{_span(moved)}"))
        else:
            arrived = located(targets, child.mobject)
            parts.append(("fade_in", arrived[0] if arrived else -1, f"fade_in t{_span(arrived)}"))
    parts.sort(key=lambda part: (order[part[0]], part[1]))
    return f"{len(sources)}>{len(targets)}: " + "; ".join(part[2] for part in parts)


_COST = ((4.0, 1.0, 3.0, 7.0), (2.0, 0.0, 5.0, 1.0), (3.0, 2.0, 2.0, 6.0))
_XA = ((0.0, 0.0), (1.0, 2.0))
_XB = ((3.0, 4.0), (-1.0, 1.0), (2.0, 0.0))


def _assignment(maximize):
    cost = np.array(_COST)
    rows, cols = m.linear_sum_assignment(cost, maximize=maximize)
    return (f"rows {[int(r) for r in rows]} cols {[int(c) for c in cols]} "
            f"total {float(cost[rows, cols].sum()):g}")


def _distances(metric):
    with np.errstate(all="ignore"):
        values = m.cdist(np.array(_XA), np.array(_XB), metric=metric)
    return " ".join(f"{float(value):.6g}" for value in np.asarray(values).ravel())


def _updated_through(play, read):
    """Play one animation in a fresh Scene, in playback, and read one coordinate after it."""
    observed = []

    class Probe(m.Scene):
        def construct(self):
            observed.append(play(self))

    Probe().run()
    return f"{read(observed[0]):.4f}"


def _fade_in_with_updater(scene):
    square = m.Square()
    square.add_updater(lambda mob, dt: mob.shift(dt * m.RIGHT))
    scene.play(m.FadeIn(square))
    after = float(square.get_center()[0])
    scene.wait(1)
    return after, float(square.get_center()[0])


def _animate_with_updater(scene):
    square = m.Square()
    scene.add(square)
    square.add_updater(lambda mob, dt: mob.shift(dt * m.UP))
    scene.play(square.animate.shift(m.RIGHT))
    return float(square.get_center()[1])


def _alpha_func_suspended(scene):
    square = m.Square()
    scene.add(square)
    square.add_updater(lambda mob, dt: mob.shift(dt * m.RIGHT))
    scene.play(m.UpdateFromAlphaFunc(square, lambda mob, alpha: None, suspend_mobject_updating=True))
    return float(square.get_center()[0])


# (bead, construct, observe, expected, provenance). Expected values were
# measured on the pinned Reference; _2023/clt/wordy_scenes.py AverageDiceValues
# shows a DecimalNumber of an empty average, which is nan.
CASES = (
    ("fm-5wq.30", "DecimalNumber(float('nan'))",
     lambda: _decimal_text(math.nan), "nan",
     REFERENCE + ":manimlib/mobject/numbers.py:get_num_string (measured)"),
    ("fm-5wq.30", "DecimalNumber(float('nan'), include_sign=True)",
     lambda: _decimal_text(math.nan, include_sign=True), "+nan",
     REFERENCE + ":manimlib/mobject/numbers.py:get_num_string (measured)"),
    ("fm-5wq.30", "DecimalNumber(1.5).set_value(float('nan'))",
     lambda: _set_value_text(math.nan), "nan",
     REFERENCE + ":manimlib/mobject/numbers.py:set_value (measured)"),
    ("fm-5wq.30", "DecimalNumber(float('inf'))",
     lambda: _decimal_text(math.inf), "inf",
     REFERENCE + ":manimlib/mobject/numbers.py:get_num_string (measured)"),
    # fm-5wq.11: the bootstrap once shipped matching_blocks as `return []` and
    # linear_sum_assignment as the identity. The first construct is
    # 3b1b/videos@e317d6c _2026/hairy_ball/supplements.py WriteAntipode.
    ("fm-5wq.11", 'TransformMatchingStrings(Text("“Antipodes”"), Text("Antipode map"))',
     lambda: _matching(m.Text("“Antipodes”"), m.Text("Antipode map")),
     "11>11: transform s1-8>t0-7; fade_out s0; fade_out s9; fade_out s10; "
     "fade_in t8; fade_in t9; fade_in t10",
     REFERENCE + ":manimlib/animation/transform_matching_parts.py:matching_blocks (measured)"),
    ("fm-5wq.11", 'TransformMatchingStrings(Text("abcde"), Text("cdeab"))',
     lambda: _matching(m.Text("abcde"), m.Text("cdeab")),
     "5>5: transform s0-1>t3-4; transform s2-4>t0-2",
     REFERENCE + ":manimlib/animation/transform_matching_parts.py:matching_blocks (measured)"),
    # `from manimlib import *` exports string_mobject's scipy imports.
    ("fm-5wq.11", f"linear_sum_assignment({_COST})",
     lambda: _assignment(False), "rows [0, 1, 2] cols [1, 3, 2] total 4",
     REFERENCE + ":manimlib/mobject/svg/string_mobject.py:linear_sum_assignment (scipy 1.18.1, measured)"),
    ("fm-5wq.11", f"linear_sum_assignment({_COST}, maximize=True)",
     lambda: _assignment(True), "rows [0, 1, 2] cols [3, 2, 0] total 15",
     REFERENCE + ":manimlib/mobject/svg/string_mobject.py:linear_sum_assignment (scipy 1.18.1, measured)"),
    # fm-k00v: an updater keeps acting through a Transform-family animation, because
    # Animation.update_mobjects updates the starting copy and the target that carry it.
    ("fm-k00v", "Square with a RIGHT-shift updater, play(FadeIn(square)): x after the play",
     lambda: _updated_through(_fade_in_with_updater, lambda value: value[0]), "1.0000",
     REFERENCE + ":manimlib/animation/animation.py:update_mobjects (measured, playback)"),
    ("fm-k00v", "the same, then wait(1): x",
     lambda: _updated_through(_fade_in_with_updater, lambda value: value[1]), "2.0000",
     REFERENCE + ":manimlib/animation/animation.py:update_mobjects (measured, playback)"),
    ("fm-k00v", "Square with an UP-shift updater, play(square.animate.shift(RIGHT)): y",
     lambda: _updated_through(_animate_with_updater, float), "1.0000",
     REFERENCE + ":manimlib/animation/animation.py:update_mobjects (measured, playback)"),
    ("fm-k00v", "UpdateFromAlphaFunc(..., suspend_mobject_updating=True) on an updating Square: x",
     lambda: _updated_through(_alpha_func_suspended, float), "0.0000",
     REFERENCE + ":manimlib/animation/animation.py:begin (measured, playback)"),
    *(
        ("fm-5wq.11", f"cdist({_XA}, {_XB}, metric={metric!r})",
         (lambda metric=metric: _distances(metric)), expected,
         REFERENCE + ":manimlib/mobject/svg/string_mobject.py:cdist (scipy 1.18.1, measured)")
        for metric, expected in (
            ("euclidean", "5 1.41421 2 2.82843 2.23607 2.23607"),
            ("sqeuclidean", "25 2 4 8 5 5"),
            ("cityblock", "7 2 2 4 3 3"),
            ("cosine", "nan nan nan 0.0161301 0.683772 0.552786"),
            ("chebyshev", "4 1 2 2 2 2"),
        )
    ),
)


def run_reference_corpus_regressions():
    """One record per case; the caller fails the run on any non-pass."""
    records = []
    for bead, construct, observe, expected, provenance in CASES:
        try:
            observed = observe()
        except Exception as error:  # noqa: BLE001 - a raise is the observation
            observed = f"raises {type(error).__name__}"
        records.append({
            "bead": bead, "construct": construct, "observed": observed,
            "expected": expected, "provenance": provenance,
            "verdict": "pass" if observed == expected else "fail",
        })
    return records
