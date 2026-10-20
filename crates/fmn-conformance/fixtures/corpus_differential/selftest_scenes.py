"""Three scenes for the corpus differential's own end-to-end check (fm-5wq.33).

Written for this purpose, not taken from the CC BY-NC-SA corpus. Each one has a known
answer, which scripts/test_corpus_differential.py asserts after running all three
through the pinned Reference and the installed portal:

  EqualShapes        constructors whose formulas coincide: structurally equal.
  NotedDotColor      Dot(color=...) is fill-coloured by the portal and masked by the
                     Reference's constructor default (Appendix C-19): admitted only by
                     the Behavior Note row bn07-c19-dot-color-keyword.
  PlantedDifference  draws a Square under the portal and a Circle under the Reference,
                     so the harness must report a structure difference at its class.
"""
from manimlib import *  # noqa: F403  # ubs:ignore — the manim scene idiom these scenes reproduce.

try:
    import fmn_python  # noqa: F401 - installed only beside the portal
    PORTAL = True
except ImportError:
    PORTAL = False


class EqualShapes(Scene):
    def construct(self):
        self.add(Square(side_length=2).shift(LEFT * 2), Circle(radius=1).shift(RIGHT * 2))


class NotedDotColor(Scene):
    def construct(self):
        self.add(Dot(color=RED))


class PlantedDifference(Scene):
    def construct(self):
        self.add(Square() if PORTAL else Circle())
