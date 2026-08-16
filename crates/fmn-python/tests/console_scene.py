"""Permanent source fixture for the installed ``fmn-python`` console."""

from manimlib import Mobject, Scene


class ConsoleScene(Scene):
    def construct(self):
        mob = Mobject()
        mob.resize(1)
        mob.set_field("point", 0, [1.0, 2.0, 0.0])
        self.add(mob)
        self.wait(1 / 30)
