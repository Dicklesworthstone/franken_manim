"""Installed-wheel builder playback over real native geometry and clocks."""
import numpy as np
import manimlib as m


def close(actual, expected):
    assert np.allclose(actual, expected, atol=2e-5), (actual, expected)


def authored_path():
    scene, square, samples, calls = m.Scene(), m.Square(), [], []
    scene.add(square)
    square.add_updater(lambda obj: samples.append(tuple(obj.get_center())), call=False)
    def path(start, end, alpha):
        calls.append(alpha)
        return (1 - alpha) * start + alpha * end + 4 * alpha * (1 - alpha) * m.UP
    scene.play(square.animate(run_time=.25, rate_func=m.linear, path_func=path).shift(2 * m.RIGHT))
    close(square.get_center(), 2 * m.RIGHT)
    assert calls and max(point[1] for point in samples) > .5


def final_alpha():
    scene, square = m.Scene(), m.Square()
    scene.add(square)
    scene.play(square.animate(run_time=.125, rate_func=m.linear, final_alpha_value=.25).shift(4 * m.RIGHT))
    close(square.get_center(), m.RIGHT)
    assert square in scene.mobjects


def removal():
    scene, square = m.Scene(), m.Square()
    scene.add(square)
    scene.play(square.animate(run_time=.125, rate_func=m.linear, remover=True).shift(m.RIGHT))
    assert square not in scene.mobjects
    close(square.get_center(), m.RIGHT)


def timed_window_and_suspension():
    scene, square, samples = m.Scene(), m.Square(), []
    scene.add(square)
    square.add_updater(lambda obj: samples.append((scene.get_time(), obj.get_x())), call=False)
    scene.play(square.animate(run_time=.25, rate_func=m.linear, time_span=(.125, .25)).shift(4 * m.RIGHT))
    assert len(samples) >= 4
    assert all(abs(x) < 2e-5 for time, x in samples if 0 < time < .125), samples
    close(square.get_x(), 4)
    calls = []
    square.clear_updaters()
    square.add_updater(lambda obj, dt: calls.append(dt), call=False)
    scene.play(square.animate(run_time=.125, rate_func=m.linear, suspend_mobject_updating=True).shift(m.RIGHT))
    assert calls and all(dt == 0 for dt in calls), calls
    assert not square._is_updating_suspended()


def nested_builder():
    scene, left, right = m.Scene(), m.Square(), m.Square().shift(3 * m.RIGHT)
    scene.add(left, right)
    scene.play(m.AnimationGroup(
        left.animate(run_time=.125, rate_func=m.linear, final_alpha_value=.5).shift(2 * m.UP),
        right.animate(run_time=.125, rate_func=m.linear).shift(m.RIGHT),
    ))
    close(left.get_center(), m.UP)
    close(right.get_center(), 4 * m.RIGHT)


def build_override():
    scene, square, seen = m.Scene(), m.Square(), []
    scene.add(square)
    target = square.copy().shift(m.RIGHT)
    product = m.Transform(square, target, run_time=.125, rate_func=m.linear)
    class Builder(m._AnimationBuilder):
        def build(self):
            seen.append(self)
            return product
    builder = Builder(square)
    scene.play(builder)
    assert seen == [builder]
    close(square.get_center(), m.RIGHT)


def explicit_transform_endpoint():
    scene, square = m.Scene(), m.Square()
    scene.add(square)
    scene.play(m.Transform(square, square.copy().shift(2 * m.RIGHT),
                           run_time=.125, rate_func=m.linear, final_alpha_value=.5))
    close(square.get_center(), m.RIGHT)


def mixed_camera_builder():
    scene, square = m.Scene(), m.Square()
    scene.add(square)
    frame = scene.frame
    core = frame._core
    scene.play(frame.animate(run_time=.125, rate_func=m.linear).shift(m.RIGHT),
               square.animate(run_time=.125, rate_func=m.linear, final_alpha_value=.5).shift(2 * m.UP))
    close(frame.get_center(), m.RIGHT)
    close(square.get_center(), m.UP)
    assert scene.frame is frame and frame._core is core


def authored_transform_lifecycle():
    # The endpoints deliberately differ, but authored interpolate is a no-op.
    # A native endpoint lerp would visibly move the square and skip the hooks.
    scene, square, events = m.Scene(), m.Square(), []
    scene.add(square)
    before = square.get_points().copy()

    class StationaryTransform(m.Transform):
        def begin(self):
            events.append("begin")
            super().begin()

        def interpolate(self, alpha):
            events.append(("interpolate", float(alpha)))

        def update_mobjects(self, dt):
            events.append(("update", float(dt)))
            super().update_mobjects(dt)

        def finish(self):
            events.append("finish")
            super().finish()

        def clean_up_from_scene(self, owner):
            events.append("cleanup")
            super().clean_up_from_scene(owner)

    animation = StationaryTransform(square, square.copy().shift(2 * m.RIGHT),
                                    run_time=.25, rate_func=m.linear)
    scene.play(animation)
    assert np.array_equal(square.get_points(), before)
    assert events[0] == "begin" and events[-1] == "cleanup", events
    assert "finish" in events
    assert any(isinstance(event, tuple) and event[0] == "update" and event[1] > 0
               for event in events), events
    assert any(isinstance(event, tuple) and event[0] == "interpolate" and 0 < event[1] < 1
               for event in events), events
    assert not square._is_updating_suspended()


def authored_mobject_inside_stock_transform():
    for grouped in (False, True):
        samples = []

        class ArchedSquare(m.Square):
            def interpolate(self, start, end, alpha, path_func=None):
                result = super().interpolate(start, end, alpha, path_func)
                self.shift(4 * alpha * (1 - alpha) * m.UP)
                samples.append((float(alpha), tuple(self.get_center())))
                return result

        scene, square, circle = m.Scene(), ArchedSquare(), m.Circle().shift(3 * m.RIGHT)
        root = m.VGroup(square, circle)
        scene.add(root)
        animation = m.Transform(root, root.copy().shift(2 * m.RIGHT),
                                run_time=.25, rate_func=m.linear)
        entry = m.AnimationGroup(animation) if grouped else animation
        scene.play(entry)
        close(square.get_center(), 2 * m.RIGHT)
        close(circle.get_center(), 5 * m.RIGHT)
        assert samples and max(center[1] for _, center in samples) > .5, samples
        assert any(0 < alpha < 1 for alpha, _ in samples), samples
        assert not square._is_updating_suspended()


def stock_transform_keeps_native_admission():
    native = getattr(m, "_native", m)
    root = m.VGroup(m.Square(), m.Circle().shift(3 * m.RIGHT))
    animation = m.Transform(root, root.copy().shift(m.RIGHT))
    assert not native._requires_python_animation(animation)


for case in (authored_path, final_alpha, removal, timed_window_and_suspension,
             nested_builder, build_override, explicit_transform_endpoint, mixed_camera_builder,
             authored_transform_lifecycle, authored_mobject_inside_stock_transform,
             stock_transform_keeps_native_admission):
    case()
    print("builder playback acceptance:", case.__name__)
