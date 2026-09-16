"""Installed-wheel input acceptance using real native controls and output.

Protocol fixtures do not establish these assertions. This script requires a
built extension and uses the actual Scene, CameraFrame, Atlas, Lumen and Reel.
"""
from contextlib import contextmanager
import importlib
from pathlib import Path
import tempfile

import numpy as np
import manimlib as m
from fmn_python import render_scene
from manimlib.mobject.interactive import Button as QualifiedButton


@contextmanager
def isolated_dispatcher():
    module = importlib.import_module("manimlib.event_handler")
    previous = module.EVENT_DISPATCHER
    module.EVENT_DISPATCHER = m.EventDispatcher()
    try:
        yield module.EVENT_DISPATCHER
    finally:
        module.EVENT_DISPATCHER = previous


def world_point(scene, target):
    point = target.get_center()
    return scene.frame.from_fixed_frame_point(point) if target.is_fixed_in_frame() else point


def click(scene, target):
    return scene.on_mouse_press(world_point(scene, target), 1, 0)


def test_public_identities_and_control_bindings():
    assert m.Button is QualifiedButton
    shape = m.Square()
    button = m.Button(shape, lambda mob: None)
    motion = m.MotionMobject(m.Dot())
    checkbox = m.Checkbox()
    toggle = m.EnableDisableButton()
    slider = m.LinearNumberSlider()
    text = m.Textbox()
    panel = m.ControlPanel(checkbox)
    expected = (
        (button.mobject, m.EventType.MousePressEvent, button),
        (motion.mobject, m.EventType.MouseDragEvent, motion),
        (checkbox, m.EventType.MousePressEvent, checkbox),
        (toggle, m.EventType.MousePressEvent, toggle),
        (slider.slider, m.EventType.MouseDragEvent, slider),
        (text.box, m.EventType.MousePressEvent, text),
        (text, m.EventType.KeyPressEvent, text),
        (panel.panel, m.EventType.MouseScrollEvent, panel),
        (panel.panel_opener, m.EventType.MouseDragEvent, panel),
    )
    for target, event_type, owner in expected:
        assert any(listener.event_type == event_type and listener.callback.__self__ is owner
                   for listener in target.get_event_listners())
    assert panel.panel_opener_rect is panel.panel_opener[0]
    assert panel.panel_info_text is panel.panel_opener[1]
    assert panel.is_fixed_in_frame()


def test_button_press_changes_original_geometry_without_hover():
    shape = m.Square().shift(3 * m.RIGHT)
    seen = []
    def action(mob):
        seen.append(mob)
        mob.shift(m.UP)
    button = m.Button(shape, action)
    scene = m.Scene()
    scene.add(button)
    assert click(scene, shape) is False
    assert seen == [shape]
    np.testing.assert_allclose(shape.get_center(), [3, 1, 0], atol=1e-6)


def test_drag_capture_does_not_pan_camera():
    shape = m.Square().shift(2 * m.LEFT)
    motion = m.MotionMobject(shape)
    scene = m.Scene()
    scene.add(motion)
    camera = scene.frame.get_center().copy()
    click(scene, shape)
    scene.on_mouse_drag(2 * m.RIGHT, 4 * m.RIGHT, 1, 0)
    np.testing.assert_allclose(shape.get_center(), 2 * m.RIGHT, atol=1e-6)
    np.testing.assert_array_equal(scene.frame.get_center(), camera)


def test_fixed_slider_follows_native_camera_projection():
    slider = m.LinearNumberSlider(value=0., min_value=-2., max_value=2., step=0.5)
    slider.shift(m.LEFT)
    scene = m.Scene()
    scene.add(slider)
    scene.frame.shift(3 * m.RIGHT).scale(0.75)
    camera = scene.frame.get_center().copy()
    click(scene, slider.slider)
    end = scene.frame.from_fixed_frame_point(slider.slider_axis.get_end() + m.RIGHT)
    scene.on_mouse_drag(end, m.RIGHT, 1, 0)
    assert float(slider.get_value()) == 2.
    np.testing.assert_array_equal(scene.frame.get_center(), camera)


def test_checkbox_keeps_mark_at_resized_moved_box():
    checkbox = m.Checkbox(True).scale(2).shift(2 * m.RIGHT + m.UP)
    scene = m.Scene()
    scene.add(checkbox)
    click(scene, checkbox)
    assert not bool(checkbox.get_value())
    np.testing.assert_allclose(checkbox.box_content.get_center(), checkbox.box.get_center(), atol=1e-6)
    assert checkbox.box_content.get_width() <= checkbox.box.get_width() + 1e-6
    assert checkbox.box_content.get_height() <= checkbox.box.get_height() + 1e-6


def test_toggle_changes_color_not_native_box_points():
    toggle = m.EnableDisableButton(True).scale(1.5).shift(2 * m.RIGHT)
    before = toggle.box.get_points().copy()
    scene = m.Scene()
    scene.add(toggle)
    click(scene, toggle)
    assert not bool(toggle.get_value())
    np.testing.assert_array_equal(toggle.box.get_points(), before)
    np.testing.assert_allclose(m.color_to_rgb(toggle.box.get_fill_color()),
                               m.color_to_rgb(toggle.disable_color), atol=1e-6)


def test_textbox_consumes_selection_shortcuts_without_origin_jump():
    text = m.Textbox("x", isInitiallyActive=True).shift(2 * m.RIGHT)
    scene = m.InteractiveScene()
    scene.add(text)
    before = text.box.get_points().copy()
    assert scene.on_key_press(ord("a"), 0) is False
    assert text.get_value() == "xa"
    for symbol, modifiers in ((0xFF51, 0), (ord("c"), 2), (ord("v"), 64)):
        assert scene.on_key_press(symbol, modifiers) is False
    assert text.get_value() == "xa"
    scene.on_key_press(0xFF08, 0)
    assert text.get_value() == "x"
    np.testing.assert_array_equal(text.box.get_points(), before)
    np.testing.assert_allclose(text.text.get_center(), text.box.get_center(), atol=1e-6)


def test_scene_membership_prevents_cross_scene_control_delivery():
    first, second = m.Checkbox(), m.Checkbox()
    one, two = m.Scene(), m.Scene()
    one.add(first)
    two.add(second)
    click(one, first)
    assert not bool(first.get_value()) and bool(second.get_value())
    one.remove(first)
    one.on_mouse_press(m.ORIGIN, 1, 0)
    assert bool(second.get_value())
    click(two, second)
    assert not bool(second.get_value())


def test_standalone_dispatch_contract_remains_unchanged():
    dispatcher = m.EventDispatcher()
    shape, seen = m.Square(), []
    listener = m.EventListener(shape, m.EventType.MouseDragEvent,
                               lambda mob, data: seen.append(mob))
    dispatcher.add_listner(listener)
    hover, destination = m.ORIGIN.copy(), 4 * m.RIGHT
    dispatcher(m.EventType.MouseMotionEvent, point=hover)
    dispatcher(m.EventType.MousePressEvent, point=destination)
    dispatcher(m.EventType.MouseDragEvent, point=destination)
    assert seen == [shape]
    assert dispatcher.get_mouse_point() is hover
    assert dispatcher.get_mouse_drag_point() is destination


def test_callback_failure_clears_capture_without_masking_error():
    shape = m.Square()
    failure = LookupError("native interaction witness")
    def fail(mob, data):
        raise failure
    shape.add_mouse_drag_listner(fail)
    scene = m.Scene()
    scene.add(shape)
    click(scene, shape)
    try:
        scene.on_mouse_drag(m.RIGHT, m.RIGHT, 1, 0)
    except LookupError as error:
        assert error is failure
    else:
        raise AssertionError("authored input exception disappeared")
    shape.remove_mouse_drag_listner(fail)
    seen = []
    shape.add_mouse_drag_listner(lambda mob, data: seen.append(mob))
    scene.on_mouse_drag(m.ORIGIN, m.LEFT, 1, 0)
    assert seen == []


def read_y4m(path):
    payload = Path(path).read_bytes()
    header, body = payload.split(b"\n", 1)
    assert header.startswith(b"YUV4MPEG2 ")
    fields = {token[:1]: token[1:] for token in header.split()[1:]}
    width, height = int(fields[b"W"]), int(fields[b"H"])
    frame_bytes = width * height * 3 // 2
    frames = []
    while body:
        marker, body = body.split(b"\n", 1)
        assert marker == b"FRAME"
        assert len(body) >= frame_bytes
        frames.append(np.frombuffer(body[:width * height], dtype=np.uint8).reshape(height, width))
        body = body[frame_bytes:]
    return payload, frames


def test_rendered_button_edit_changes_pixels_and_is_thread_repeatable():
    class ButtonEdit(m.Scene):
        def construct(self):
            square = m.Square(fill_opacity=1, fill_color=m.WHITE, stroke_width=0).shift(2 * m.LEFT)
            self.add(m.Button(square, lambda mob: mob.shift(4 * m.RIGHT)))
            self.wait(0.25)
            assert click(self, square) is False
            self.wait(0.25)
    with tempfile.TemporaryDirectory(prefix="fmn-input-") as directory:
        outputs = []
        for threads in (1, 4):
            destination = Path(directory) / f"input-{threads}.y4m"
            receipt = render_scene(ButtonEdit, destination, resolution=(64, 48), fps=4, threads=threads)
            assert receipt.frame_count == 2
            payload, frames = read_y4m(destination)
            assert len(frames) == 2 and not np.array_equal(frames[0], frames[1])
            centers = []
            for frame in frames:
                low, high = int(frame.min()), int(frame.max())
                assert high - low > 50
                ys, xs = np.nonzero(frame > low + (high - low) / 2)
                assert len(xs) > 8
                centers.append(float(xs.mean()))
            assert centers[1] > centers[0] + 10
            outputs.append(payload)
        assert outputs[0] == outputs[1]


for case in (
    test_public_identities_and_control_bindings,
    test_button_press_changes_original_geometry_without_hover,
    test_drag_capture_does_not_pan_camera,
    test_fixed_slider_follows_native_camera_projection,
    test_checkbox_keeps_mark_at_resized_moved_box,
    test_toggle_changes_color_not_native_box_points,
    test_textbox_consumes_selection_shortcuts_without_origin_jump,
    test_scene_membership_prevents_cross_scene_control_delivery,
    test_standalone_dispatch_contract_remains_unchanged,
    test_callback_failure_clears_capture_without_masking_error,
    test_rendered_button_edit_changes_pixels_and_is_thread_repeatable,
):
    with isolated_dispatcher():
        case()
    print("native control interaction passed:", case.__name__)
