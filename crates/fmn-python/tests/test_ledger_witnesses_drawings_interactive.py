"""Ledger witnesses for drawings_interactive (fm-6u1g): each test calls a manimlib symbol the Parity Ledger
marks same/improved and checks the Reference's behaviour (manimlib @ 6199a00d)."""
import importlib
import unittest

import numpy as np

from test_parallel_batch_native import native_wheel_available

ATOL = 1e-6

# Reference default_config.yml colors / sizes used by the controls below.
GREEN_HEX, RED_HEX, BLUE_HEX = "#83C167", "#FC6255", "#58C4DD"
GREY_A_HEX, WHITE_HEX, BLACK_HEX = "#DDDDDD", "#FFFFFF", "#000000"


def close(test, actual, expected):
    np.testing.assert_allclose(np.asarray(actual, dtype=float), np.asarray(expected, dtype=float),
                               atol=ATOL, rtol=0)


def listeners(mobject):
    return [(listener.event_type, getattr(listener.callback, "__name__", None))
            for listener in mobject.get_event_listners()]


@unittest.skipUnless(native_wheel_available(), "requires the built FrankenManim native wheel")
class DrawingsInteractiveWitnesses(unittest.TestCase):
    # ----------------------------------------------------------------- drawings.Bubble
    # Default SpeechBubble (Reference): filler Rectangle(2, 1) at the origin, body =
    # SurroundingRectangle(buff=MED_SMALL_BUFF=0.25) -> [-1.25, 1.25] x [-0.75, 0.75], plus a stem
    # whose tip is lp + 0.5 * 1.5 * DOWN = (-1.25, -1.5). Group bbox [-1.25, 1.25] x [-1.5, 0.75],
    # center (0, -0.375), height 2.25, tip = corner(DOWN + LEFT) = (-1.25, -1.5).

    def test_bubble_get_bubble_center(self):
        from manimlib import UP, Square
        from manimlib.mobject.svg.drawings import SpeechBubble, ThoughtBubble
        bubble = SpeechBubble()
        # center + 0.125 * height * UP = (0, -0.375 + 0.28125, 0)
        close(self, bubble.get_bubble_center(), [0.0, -0.09375, 0.0])
        bubble.shift(3 * UP)
        close(self, bubble.get_bubble_center(), [0.0, 2.90625, 0.0])
        # Square(4): rect [-2.25, 2.25]^2 (height 4.5), stem 2.25 below -> y in [-4.5, 2.25]:
        # center y -1.125, height 6.75.
        wide = SpeechBubble(Square(4))
        close(self, wide.get_bubble_center(), [0.0, -1.125 + 0.125 * 6.75, 0.0])
        thought = ThoughtBubble()
        close(self, thought.get_bubble_center(),
              thought.get_center() + 0.125 * thought.get_height() * UP)

    def test_bubble_move_tip_to(self):
        from manimlib.mobject.svg.drawings import SpeechBubble
        bubble = SpeechBubble()
        close(self, bubble.get_tip(), [-1.25, -1.5, 0.0])
        self.assertIs(bubble.move_tip_to([2.0, 1.0, 0.0]), bubble)
        close(self, bubble.get_tip(), [2.0, 1.0, 0.0])
        # The whole bubble shifts by point - old tip = (3.25, 2.5).
        close(self, bubble.content.get_center(), [3.25, 2.5, 0.0])
        close(self, bubble.body.get_bounding_box()[[0, 2]], [[2.0, 1.0, 0.0], [4.5, 3.25, 0.0]])

    def test_bubble_flip(self):
        from manimlib import LEFT, ORIGIN, RIGHT, UP, Polygon
        from manimlib.mobject.svg.drawings import SpeechBubble
        # Asymmetric content [0, 2] x [0, 0.5]: body [-0.25, 2.25] x [-0.25, 0.75], stem tip at
        # (-0.25, -0.75); group center (1, 0).
        content = Polygon(ORIGIN, 2 * RIGHT, 0.5 * UP)
        bubble = SpeechBubble(content)
        close(self, bubble.get_tip(), [-0.25, -0.75, 0.0])
        body_before = bubble.body.get_points().copy()
        content_before = content.get_points().copy()
        self.assertIs(bubble.flip(), bubble)
        # Rotation by PI about UP through the group center: x -> 2 - x, z -> -z.
        mirrored = body_before.copy()
        mirrored[:, 0] = 2.0 - mirrored[:, 0]
        mirrored[:, 2] = -mirrored[:, 2]
        close(self, bubble.body.get_points(), mirrored)
        np.testing.assert_array_equal(bubble.direction, RIGHT)
        close(self, bubble.get_tip(), [2.25, -0.75, 0.0])  # corner(DOWN + RIGHT): the stem tip
        # only_body=True flips the content back in place.
        close(self, content.get_points(), content_before)

        other = SpeechBubble(Polygon(ORIGIN, 2 * RIGHT, 0.5 * UP))
        other_before = other.content.get_points().copy()
        other.flip(only_body=False)
        flipped = other_before.copy()
        flipped[:, 0] = 2.0 - flipped[:, 0]
        flipped[:, 2] = -flipped[:, 2]
        close(self, other.content.get_points(), flipped)

        # A horizontal axis mirrors y about the center (-0.375) and keeps the direction.
        plain = SpeechBubble()
        plain_before = plain.body.get_points().copy()
        plain.flip(axis=RIGHT)
        np.testing.assert_array_equal(plain.direction, LEFT)
        vertical = plain_before.copy()
        vertical[:, 1] = -0.75 - vertical[:, 1]
        vertical[:, 2] = -vertical[:, 2]
        close(self, plain.body.get_points(), vertical)

    def test_bubble_pin_to(self):
        from manimlib import DOWN, LEFT, RIGHT, Square
        from manimlib.mobject.svg.drawings import SpeechBubble
        target = Square(1).move_to(3 * RIGHT + DOWN)
        bubble = SpeechBubble()
        self.assertIs(bubble.pin_to(target), bubble)
        # direction LEFT: tip goes to target's bounding-box point UP - LEFT = UR corner.
        close(self, bubble.get_tip(), [3.5, -0.5, 0.0])
        close(self, bubble.content.get_center(), [4.75, 1.0, 0.0])
        np.testing.assert_array_equal(bubble.direction, LEFT)
        # auto_flip: the target is right of the origin, opposite to LEFT, so the bubble flips and
        # pins its (now right-hand) tip to the UL corner.
        flipping = SpeechBubble()
        flipping.pin_to(target, auto_flip=True)
        np.testing.assert_array_equal(flipping.direction, RIGHT)
        close(self, flipping.get_tip(), [2.5, -0.5, 0.0])
        # Same-side target: no flip even with auto_flip.
        same_side = SpeechBubble()
        same_side.pin_to(Square(1).move_to(3 * LEFT + DOWN), auto_flip=True)
        np.testing.assert_array_equal(same_side.direction, LEFT)
        close(self, same_side.get_tip(), [-2.5, -0.5, 0.0])

    def test_bubble_position_mobject_inside(self):
        from manimlib import Rectangle, Square
        from manimlib.mobject.svg.drawings import SpeechBubble
        bubble = SpeechBubble()  # body width 2.5, height 2.25
        big = Square(4)
        self.assertIs(bubble.position_mobject_inside(big, buff=0.5), big)
        # max width 2.5 - 1 = 1.5, then max height 2.25 / 1.5 - 1 = 0.5 (proportional scaling).
        self.assertAlmostEqual(big.get_width(), 0.5, places=6)
        self.assertAlmostEqual(big.get_height(), 0.5, places=6)
        close(self, big.get_center(), [0.0, -0.09375, 0.0])
        self.assertFalse(any(big is mob for mob in bubble.submobjects))
        wide = Rectangle(6, 0.2)
        bubble.position_mobject_inside(wide)  # default buff MED_LARGE_BUFF = 0.5
        self.assertAlmostEqual(wide.get_width(), 1.5, places=6)
        self.assertAlmostEqual(wide.get_height(), 0.05, places=6)
        close(self, wide.get_center(), [0.0, -0.09375, 0.0])
        small = Rectangle(1, 0.4).shift([5.0, 2.0, 0.0])
        bubble.position_mobject_inside(small, buff=0.25)  # fits: size kept, only moved
        self.assertAlmostEqual(small.get_width(), 1.0, places=6)
        self.assertAlmostEqual(small.get_height(), 0.4, places=6)
        close(self, small.get_center(), [0.0, -0.09375, 0.0])

    def test_bubble_add_content(self):
        from manimlib import RIGHT, Rectangle, Square
        from manimlib.mobject.svg.drawings import SpeechBubble
        bubble = SpeechBubble()
        new = Rectangle(1, 0.4).shift(5 * RIGHT)
        self.assertIs(bubble.add_content(new), new)
        self.assertIs(bubble.content, new)
        close(self, new.get_center(), [0.0, -0.09375, 0.0])
        self.assertAlmostEqual(new.get_width(), 1.0, places=6)
        # Reference add_content positions and records the content; it does not add it.
        self.assertFalse(any(new is mob for mob in bubble.submobjects))
        big = Square(4)
        bubble.add_content(big)  # default buff 0.5 -> limits (1.5, 0.5)
        self.assertIs(bubble.content, big)
        self.assertAlmostEqual(big.get_height(), 0.5, places=6)

    def test_bubble_write(self):
        from manimlib import Text
        from manimlib.mobject.svg.drawings import SpeechBubble
        bubble = SpeechBubble()
        self.assertIs(bubble.write("Hi"), bubble)
        self.assertIsInstance(bubble.content, Text)
        self.assertEqual(bubble.content.text, "Hi")
        natural = Text("Hi")
        self.assertAlmostEqual(bubble.content.get_width(), natural.get_width(), places=6)
        close(self, bubble.content.get_center(), [0.0, -0.09375, 0.0])
        sentence = "A considerably longer sentence"
        bubble.write(sentence)
        self.assertEqual(bubble.content.text, sentence)
        expected = Text(sentence)
        scale = min(1.0, 1.5 / expected.get_width(), 0.5 / expected.get_height())
        self.assertLess(scale, 1.0)
        self.assertAlmostEqual(bubble.content.get_width(), scale * expected.get_width(), places=5)
        close(self, bubble.content.get_center(), [0.0, -0.09375, 0.0])

    def test_bubble_resize_to_content(self):
        from manimlib import Rectangle
        from manimlib.mobject.svg.drawings import SpeechBubble
        bubble = SpeechBubble()
        close(self, bubble.body.get_bounding_box()[[0, 2]], [[-1.25, -1.5, 0.0], [1.25, 0.75, 0.0]])
        content = Rectangle(1, 0.4)
        bubble.add_content(content)  # now centered at (0, -0.09375)
        bubble.resize_to_content(buff=0.25)
        # New body: SurroundingRectangle(buff=0.25) = 1.5 x 0.9 around the content, plus a stem
        # 0.5 * 0.9 below its lower-left corner: x [-0.75, 0.75], y [-0.99375, 0.35625].
        close(self, bubble.body.get_bounding_box()[[0, 2]],
              [[-0.75, -0.99375, 0.0], [0.75, 0.35625, 0.0]])
        # Only the points change; the body's style and the content's position remain.
        self.assertEqual(bubble.body.get_fill_color(), BLACK_HEX)
        self.assertAlmostEqual(bubble.body.get_fill_opacity(), 0.8, places=6)
        self.assertEqual(bubble.body.get_stroke_color(), WHITE_HEX)
        close(self, content.get_center(), [0.0, -0.09375, 0.0])

    def test_bubble_clear(self):
        from manimlib.mobject.svg.drawings import SpeechBubble
        bubble = SpeechBubble()
        content, body = bubble.content, bubble.body
        self.assertTrue(any(content is mob for mob in bubble.submobjects))
        self.assertIs(bubble.clear(), bubble)
        self.assertEqual(len(bubble.submobjects), 1)
        self.assertIs(bubble.submobjects[0], body)
        self.assertIs(bubble.content, content)  # the attribute is kept, only the child is removed

    # ------------------------------------------------------------------ interactive
    def test_control_mobject_init(self):
        from manimlib import Circle, Square
        from manimlib.mobject.interactive import ControlMobject
        square, circle = Square(), Circle()
        control = ControlMobject(2.5, square, circle)
        self.assertEqual(control.get_value(), 2.5)
        self.assertIsInstance(control.get_value(), np.float64)
        self.assertEqual(len(control.submobjects), 2)
        self.assertIs(control.submobjects[0], square)
        self.assertIs(control.submobjects[1], circle)
        # The Reference adds a no-op updater (keeps it dynamic) and fixes the family in frame.
        self.assertEqual(len(control.get_updaters()), 1)
        self.assertTrue(control.is_fixed_in_frame())
        self.assertTrue(square.is_fixed_in_frame())
        self.assertTrue(circle.is_fixed_in_frame())

    def test_checkbox_init(self):
        from manimlib import EventType, Line, Rectangle, VGroup
        from manimlib.mobject.interactive import Checkbox
        checkbox = Checkbox()
        self.assertIs(type(checkbox.get_value()), np.bool_)
        self.assertTrue(checkbox.get_value())
        self.assertEqual(np.dtype(checkbox.value_type), np.dtype(bool))
        self.assertIsInstance(checkbox.box, Rectangle)
        self.assertAlmostEqual(checkbox.box.get_width(), 0.5, places=6)
        self.assertAlmostEqual(checkbox.box.get_height(), 0.5, places=6)
        self.assertEqual(checkbox.box.get_fill_opacity(), 0.0)
        self.assertAlmostEqual(checkbox.box_content_buff, 0.1, places=9)
        self.assertEqual([type(mob) for mob in checkbox.submobjects], [Rectangle, VGroup])
        self.assertIs(checkbox.submobjects[0], checkbox.box)
        self.assertIs(checkbox.submobjects[1], checkbox.box_content)
        # Checkmark: Line(UP/2 + 2LEFT, DL), Line(DL, UR) fit to the 0.5 box, halved, centered.
        check = checkbox.box_content
        self.assertEqual([type(mob) for mob in check], [Line, Line])
        close(self, [check[0].get_start(), check[0].get_end(), check[1].get_start(), check[1].get_end()],
              [[-0.125, 0.0625, 0.0], [-0.125 + 0.25 / 3, -0.125, 0.0],
               [-0.125 + 0.25 / 3, -0.125, 0.0], [0.125, 0.125, 0.0]])
        for line in check:
            self.assertEqual(line.get_stroke_color(), GREEN_HEX)
            self.assertEqual(line.get_stroke_width(), 6.0)
        self.assertTrue(checkbox.is_fixed_in_frame())
        self.assertIn((EventType.MousePressEvent, "on_mouse_press"), listeners(checkbox))

        unchecked = Checkbox(value=False)
        self.assertFalse(unchecked.get_value())
        cross = unchecked.box_content
        close(self, [cross[0].get_start(), cross[0].get_end(), cross[1].get_start(), cross[1].get_end()],
              [[-0.125, 0.125, 0.0], [0.125, -0.125, 0.0], [0.125, 0.125, 0.0], [-0.125, -0.125, 0.0]])
        for line in cross:
            self.assertEqual(line.get_stroke_color(), RED_HEX)

    def test_enable_disable_button_init(self):
        from manimlib import EventType, Rectangle
        from manimlib.mobject.interactive import EnableDisableButton
        button = EnableDisableButton()
        self.assertIs(type(button.get_value()), np.bool_)
        self.assertTrue(button.get_value())
        self.assertEqual(np.dtype(button.value_type), np.dtype(bool))
        self.assertEqual(button.rect_kwargs, {"width": 0.5, "height": 0.5, "fill_opacity": 1.0})
        self.assertEqual(button.enable_color, GREEN_HEX)
        self.assertEqual(button.disable_color, RED_HEX)
        self.assertEqual(len(button.submobjects), 1)
        self.assertIs(button.submobjects[0], button.box)
        self.assertIsInstance(button.box, Rectangle)
        self.assertAlmostEqual(button.box.get_width(), 0.5, places=6)
        self.assertAlmostEqual(button.box.get_height(), 0.5, places=6)
        self.assertEqual(button.box.get_fill_opacity(), 1.0)
        self.assertTrue(button.is_fixed_in_frame())
        self.assertEqual(len(button.get_updaters()), 1)
        self.assertIn((EventType.MousePressEvent, "on_mouse_press"), listeners(button))
        off = EnableDisableButton(value=False)
        self.assertFalse(off.get_value())

    def test_linear_number_slider_init(self):
        from manimlib import Circle, EventType, Line, RoundedRectangle
        from manimlib.mobject.interactive import LinearNumberSlider
        slider = LinearNumberSlider(value=3)
        self.assertEqual(slider.get_value(), 3.0)
        self.assertIs(type(slider.get_value()), np.float64)
        self.assertIs(slider.value_type, np.float64)
        self.assertEqual((slider.min_value, slider.max_value, slider.step), (-10.0, 10.0, 1.0))
        self.assertEqual([type(mob) for mob in slider.submobjects], [RoundedRectangle, Circle, Line])
        self.assertIs(slider.submobjects[0], slider.bar)
        self.assertIs(slider.submobjects[1], slider.slider)
        self.assertIs(slider.submobjects[2], slider.slider_axis)
        self.assertAlmostEqual(slider.bar.get_width(), 2.0, places=6)
        self.assertAlmostEqual(slider.bar.get_height(), 0.075, places=6)
        self.assertAlmostEqual(slider.slider.get_width(), 0.2, places=6)
        self.assertEqual(slider.slider.get_fill_color(), GREY_A_HEX)
        self.assertEqual(slider.slider.get_stroke_color(), GREY_A_HEX)
        self.assertEqual(slider.slider.get_fill_opacity(), 1.0)
        close(self, [slider.slider_axis.get_start(), slider.slider_axis.get_end()],
              [[-1.0, 0.0, 0.0], [1.0, 0.0, 0.0]])
        self.assertEqual(slider.slider_axis.get_stroke_opacity(), 0.0)
        # The Reference moves the handle to the axis center and does not run set_value_anim
        # during construction.
        close(self, slider.slider.get_center(), [0.0, 0.0, 0.0])
        self.assertTrue(slider.is_fixed_in_frame())
        self.assertIn((EventType.MouseDragEvent, "slider_on_mouse_drag"), listeners(slider.slider))

    def test_textbox_init(self):
        from manimlib import EventType, Rectangle, Text
        from manimlib.mobject.interactive import Textbox
        box = Textbox("hello")
        self.assertEqual(box.get_value(), "hello")
        self.assertIsInstance(box.get_value(), str)
        self.assertEqual(np.dtype(box.value_type), np.dtype(object))
        self.assertFalse(box.isActive)
        self.assertFalse(box.isInitiallyActive)
        self.assertEqual(box.text_buff, 0.25)
        self.assertEqual((box.active_color, box.deactive_color), (BLUE_HEX, RED_HEX))
        self.assertEqual([type(mob) for mob in box.submobjects], [Rectangle, Text])
        self.assertIs(box.submobjects[0], box.box)
        self.assertIs(box.submobjects[1], box.text)
        self.assertAlmostEqual(box.box.get_width(), 2.0, places=6)
        self.assertAlmostEqual(box.box.get_height(), 1.0, places=6)
        self.assertEqual(box.box.get_fill_color(), WHITE_HEX)
        self.assertEqual(box.box.get_fill_opacity(), 1.0)
        self.assertEqual(box.box.get_stroke_color(), RED_HEX)  # active_anim(False) at construction
        self.assertEqual(box.text.text, "hello")
        self.assertEqual(box.text.get_color(), BLUE_HEX)
        # Narrow text keeps its natural size (set_width(1.5) then capped back to its height).
        self.assertAlmostEqual(box.text.get_width(), Text("hello", color=BLUE_HEX).get_width(), places=5)
        close(self, box.text.get_center(), box.box.get_center())
        self.assertTrue(box.is_fixed_in_frame())
        self.assertIn((EventType.KeyPressEvent, "on_key_press"), listeners(box))
        self.assertIn((EventType.MousePressEvent, "box_on_mouse_press"), listeners(box.box))
        wide = Textbox("abcdefghijklmnopqrstuvwxyz")
        self.assertAlmostEqual(wide.text.get_width(), 2.0 - 2 * 0.25, places=5)
        active = Textbox(isInitiallyActive=True)
        self.assertTrue(active.isActive)
        self.assertEqual(active.box.get_stroke_color(), BLUE_HEX)
        self.assertEqual(active.get_value(), "")

    def test_enable_disable_button_set_value_anim(self):
        from manimlib import BLUE, YELLOW
        from manimlib.mobject.interactive import EnableDisableButton
        button = EnableDisableButton()
        button.set_value_anim(False)
        self.assertEqual(button.box.get_fill_color(), RED_HEX)
        self.assertEqual(button.box.get_fill_opacity(), 1.0)  # color only, opacity kept
        self.assertTrue(button.get_value())  # the anim does not change the value
        button.set_value_anim(True)
        self.assertEqual(button.box.get_fill_color(), GREEN_HEX)
        button.set_value(False)  # ControlMobject.set_value runs the anim
        self.assertEqual(button.box.get_fill_color(), RED_HEX)
        self.assertFalse(button.get_value())
        custom = EnableDisableButton(enable_color=BLUE, disable_color=YELLOW)
        custom.set_value_anim(True)
        self.assertEqual(custom.box.get_fill_color(), BLUE_HEX)
        custom.set_value_anim(False)
        self.assertEqual(custom.box.get_fill_color(), "#FFFF00")

    def test_textbox_active_anim(self):
        from manimlib.mobject.interactive import Textbox
        box = Textbox("x")
        self.assertEqual(box.box.get_stroke_color(), RED_HEX)
        box.active_anim(True)
        self.assertEqual(box.box.get_stroke_color(), BLUE_HEX)
        self.assertFalse(box.isActive)  # only the style changes
        box.active_anim(False)
        self.assertEqual(box.box.get_stroke_color(), RED_HEX)
        self.assertEqual(box.box.get_fill_color(), WHITE_HEX)

    def test_textbox_on_key_press(self):
        from manimlib.mobject.interactive import Textbox
        box = Textbox()

        def press(symbol, modifiers=0):
            return box.on_key_press(box, {"symbol": symbol, "modifiers": modifiers})

        # Inactive: nothing happens and the handler returns None.
        self.assertIsNone(press(ord("a")))
        self.assertEqual(box.get_value(), "")
        box.isActive = True
        # pyglet: MOD_SHIFT = 1, MOD_CAPSLOCK = 8, SPACE = 0x20, TAB = 0xff09, BACKSPACE = 0xff08.
        steps = [
            (ord("a"), 0, "a"),
            (ord("b"), 1, "aB"),
            (ord("c"), 8, "aBC"),
            (ord("7"), 0, "aBC7"),
            (0x20, 0, "aBC7 "),
            (0xFF09, 0, "aBC7 \t"),
            (0xFF08, 0, "aBC7 "),
            (0xFF08, 0, "aBC7"),
        ]
        for symbol, modifiers, expected in steps:
            self.assertIs(press(symbol, modifiers), False)
            self.assertEqual(box.get_value(), expected)
            self.assertEqual(box.text.text, expected)
        for _ in range(4):
            press(0xFF08)
        self.assertEqual(box.get_value(), "")
        self.assertIs(press(0xFF08), False)  # backspace on empty stays empty
        self.assertEqual(box.get_value(), "")

    # ------------------------------------------------------------- event dispatcher
    def test_event_dispatcher_getters(self):
        import manimlib as m
        from manimlib import EventType
        handler = importlib.import_module("manimlib.event_handler")
        dispatcher_cls = importlib.import_module("manimlib.event_handler.event_dispatcher").EventDispatcher
        self.assertIsInstance(handler.EVENT_DISPATCHER, dispatcher_cls)
        previous = handler.EVENT_DISPATCHER
        handler.EVENT_DISPATCHER = dispatcher = dispatcher_cls()
        try:
            np.testing.assert_array_equal(dispatcher.get_mouse_point(), np.zeros(3))
            np.testing.assert_array_equal(dispatcher.get_mouse_drag_point(), np.zeros(3))
            self.assertFalse(dispatcher.is_key_pressed(65))
            hover = np.array([1.0, 2.0, 0.0])
            dispatcher(EventType.MouseMotionEvent, point=hover, d_point=np.zeros(3))
            self.assertIs(dispatcher.get_mouse_point(), hover)
            np.testing.assert_array_equal(dispatcher.get_mouse_point(), [1.0, 2.0, 0.0])
            np.testing.assert_array_equal(dispatcher.get_mouse_drag_point(), np.zeros(3))
            drag = np.array([-3.0, 0.5, 0.0])
            dispatcher(EventType.MouseDragEvent, point=drag, d_point=np.zeros(3), buttons=1, modifiers=0)
            self.assertIs(dispatcher.get_mouse_drag_point(), drag)
            np.testing.assert_array_equal(dispatcher.get_mouse_drag_point(), [-3.0, 0.5, 0.0])
            self.assertIs(dispatcher.get_mouse_point(), hover)
            dispatcher(EventType.KeyPressEvent, symbol=65, modifiers=0)
            self.assertIs(dispatcher.is_key_pressed(65), True)
            self.assertIs(dispatcher.is_key_pressed(66), False)
            dispatcher(EventType.KeyReleaseEvent, symbol=65, modifiers=0)
            self.assertIs(dispatcher.is_key_pressed(65), False)

            # Through Scene input: listeners read the getters while the event is dispatched.
            scene = m.Scene()
            target = m.Square()
            scene.add(target)
            seen = {}
            target.add_mouse_motion_listner(
                lambda mob, data: seen.setdefault("motion", dispatcher.get_mouse_point().copy()))
            target.add_mouse_drag_listner(
                lambda mob, data: seen.setdefault("drag", dispatcher.get_mouse_drag_point().copy()))
            target.add_key_press_listner(
                lambda mob, data: seen.setdefault("key", dispatcher.is_key_pressed(data["symbol"])))
            scene.on_mouse_motion(np.array([0.5, 0.25, 0.0]), np.zeros(3))
            scene.on_mouse_press(np.array([0.5, 0.25, 0.0]), 1, 0)
            scene.on_mouse_drag(np.array([0.75, -0.5, 0.0]), np.array([0.25, -0.75, 0.0]), 1, 0)
            scene.on_mouse_release(np.array([0.75, -0.5, 0.0]), 1, 0)
            scene.on_key_press(65, 0)
            scene.on_key_release(65, 0)
            close(self, seen["motion"], [0.5, 0.25, 0.0])
            close(self, seen["drag"], [0.75, -0.5, 0.0])
            self.assertIs(seen["key"], True)
        finally:
            handler.EVENT_DISPATCHER = previous

    # ------------------------------------------------------------------ event type
    def test_event_type(self):
        from manimlib.event_handler.event_type import EventType
        import manimlib
        reference = {
            "MouseMotionEvent": "mouse_motion_event",
            "MousePressEvent": "mouse_press_event",
            "MouseReleaseEvent": "mouse_release_event",
            "MouseDragEvent": "mouse_drag_event",
            "MouseScrollEvent": "mouse_scroll_event",
            "KeyPressEvent": "key_press_event",
            "KeyReleaseEvent": "key_release_event",
        }
        members = vars(manimlib.EventType)["_member_map_"]
        self.assertEqual({name: member.value for name, member in members.items()}, reference)
        for name, value in reference.items():
            member = EventType(value)
            self.assertIs(member, members[name])
            self.assertEqual(member.name, name)
            self.assertEqual(member.value, value)
            self.assertIs(EventType(member), member)
        with self.assertRaises(ValueError):
            EventType("mouse_hover_event")


if __name__ == "__main__":
    unittest.main()
