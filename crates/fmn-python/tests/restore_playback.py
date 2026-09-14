"""Installed-wheel restoration over real native objects, cameras and playback."""
import numpy as np
import manimlib as m
from manimlib.animation.transform import Restore as QualifiedRestore


def close(actual, expected):
    assert np.allclose(actual, expected, atol=2e-5), (actual, expected)


def saved_target_identity():
    scene, square = m.Scene(), m.Square()
    scene.add(square)
    square.save_state()
    saved = square.saved_state
    square.shift(2 * m.RIGHT)
    animation = m.Restore(square, run_time=.125, rate_func=m.linear)
    square.save_state()
    square.shift(m.RIGHT)
    assert animation.target_mobject is saved
    scene.play(animation)
    close(square.get_center(), m.ORIGIN)
    assert square in scene.mobjects
    assert QualifiedRestore is m.Restore and isinstance(animation, m.Transform)


def authored_path():
    scene, square, samples, calls = m.Scene(), m.Square(), [], []
    scene.add(square)
    square.save_state()
    square.shift(2 * m.RIGHT)
    square.add_updater(lambda obj: samples.append(tuple(obj.get_center())), call=False)
    def path(start, end, alpha):
        calls.append(alpha)
        return (1 - alpha) * start + alpha * end + 4 * alpha * (1 - alpha) * m.UP
    scene.play(m.Restore(square, run_time=.25, rate_func=m.linear, path_func=path))
    close(square.get_center(), m.ORIGIN)
    assert calls and max(point[1] for point in samples) > .5


def authored_transform_hooks():
    scene, square, calls = m.Scene(), m.Square(), []
    scene.add(square)
    square.save_state()
    square.shift(2 * m.RIGHT)
    class Restore(m.Restore):
        def create_target(self):
            calls.append("target")
            return super().create_target()
        def interpolate_submobject(self, *args):
            calls.append("interpolate")
            return super().interpolate_submobject(*args)
    scene.play(Restore(square, run_time=.125, rate_func=m.linear))
    close(square.get_center(), m.ORIGIN)
    assert "target" in calls and calls.count("interpolate") > 1


def nested_succession():
    scene, square, positions = m.Scene(), m.Square(), []
    scene.add(square)
    square.save_state()
    square.add_updater(lambda obj: positions.append(obj.get_x()), call=False)
    scene.play(m.Succession(
        square.animate(run_time=.125, rate_func=m.linear).shift(2 * m.RIGHT),
        m.Restore(square, run_time=.125, rate_func=m.linear),
    ))
    close(square.get_center(), m.ORIGIN)
    assert max(positions) > 1


def mixed_camera_restore():
    scene, square = m.Scene(), m.Square()
    scene.add(square)
    frame, core = scene.frame, scene.frame._core
    initial = (frame.get_center().copy(), frame.get_shape(), frame.get_field_of_view(),
               tuple(core.orientation()))
    frame.save_state()
    frame.shift(2 * m.RIGHT).scale(.5).set_theta(.5).set_field_of_view(.7)
    scene.play(m.AnimationGroup(
        m.Restore(frame, run_time=.125, rate_func=m.linear),
        square.animate(run_time=.125, rate_func=m.linear).shift(m.UP),
    ))
    assert scene.frame is frame and frame._core is core
    for actual, expected in zip((frame.get_center(), frame.get_shape(), frame.get_field_of_view(),
                                 tuple(core.orientation())), initial):
        close(actual, expected)
    close(square.get_center(), m.UP)
    assert frame not in scene.mobjects


def partial_restoration():
    scene, square = m.Scene(), m.Square()
    scene.add(square)
    square.save_state()
    square.shift(4 * m.RIGHT)
    scene.play(m.Restore(square, run_time=.125, rate_func=m.linear, final_alpha_value=.25))
    close(square.get_center(), 3 * m.RIGHT)


def remover_restoration():
    scene, square = m.Scene(), m.Square()
    scene.add(square)
    square.save_state()
    square.shift(m.RIGHT)
    scene.play(m.Restore(square, run_time=.125, rate_func=m.linear, remover=True))
    close(square.get_center(), m.ORIGIN)
    assert square not in scene.mobjects


def direct_transform_lifecycle():
    scene, square = m.Scene(), m.Square()
    scene.add(square)
    square.save_state()
    square.shift(4 * m.RIGHT)
    animation = m.Restore(square, rate_func=m.linear)
    animation.begin()
    animation.interpolate(.5)
    close(square.get_center(), 2 * m.RIGHT)
    animation.finish()
    animation.clean_up_from_scene(scene)
    close(square.get_center(), m.ORIGIN)


for case in (saved_target_identity, authored_path, authored_transform_hooks, nested_succession,
             mixed_camera_restore, partial_restoration, remover_restoration, direct_transform_lifecycle):
    case()
    print("restore playback acceptance:", case.__name__)
