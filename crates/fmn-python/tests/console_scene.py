"""README-style installed-artifact fixture for the ``fmn-python`` console."""

from manimlib import DOWN, UP, YELLOW, FadeIn, Scene, Tex, Text, Write


class Hello(Scene):
    def construct(self):
        title = Text("FrankenManim", font_size=72)
        formula = Tex(r"e^{i\pi} + 1 = 0")
        formula.next_to(title, DOWN)
        self.play(Write(title), FadeIn(formula, shift=UP))
        self.play(formula.animate.set_color_by_tex("i", YELLOW))
        self.wait()
