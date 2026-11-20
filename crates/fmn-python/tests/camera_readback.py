"""Actual extension/Lumen acceptance, not a mocked renderer."""
import numpy as np
import manimlib as m


def run_camera_readback_acceptance():
    camera = m.Camera(resolution=(96, 64), background_opacity=0)
    camera.clear()
    empty = camera.get_pixel_array()
    assert empty.shape == (64, 96, 4) and empty.dtype == np.uint8
    assert not empty.any()
    top = m.Square(side_length=1.0, fill_color=m.RED, fill_opacity=1, stroke_width=0).shift(m.UP * 2)
    before = np.array(top.get_points(), copy=True)
    assert camera.capture(top) is None
    image = camera.get_pixel_array()
    ys, xs = np.where(image[:, :, 3] > 128)
    assert len(ys) > 5 and ys.mean() < 32, "camera projection must produce top-row-first images"
    assert not top._is_bound(), "readback must not adopt detached objects"
    np.testing.assert_array_equal(before, top.get_points())
    image[:] = 0
    assert camera.get_pixel_array().any(), "returned pixel arrays must own their data"
    frozen = camera.get_pixel_array()
    top.shift(m.DOWN * 4)
    np.testing.assert_array_equal(frozen, camera.get_pixel_array())
    camera.capture(top)
    assert np.where(camera.get_pixel_array()[:, :, 3] > 128)[0].mean() > 32

    # A point-free group and shared children must retain their Python topology.
    group = m.Group(top, m.Circle(radius=.5, fill_opacity=1, stroke_width=0))
    camera.capture(group)
    grouped = camera.get_pixel_array()
    camera.capture(*group.submobjects)
    np.testing.assert_array_equal(grouped, camera.get_pixel_array())
    left = m.Group(top)
    right = m.Group(top)
    camera.capture(left, right)
    assert left.submobjects[0] is right.submobjects[0] is top
    assert not left._is_bound() and not right._is_bound()

    # Preserve explicit root order, even if z_index differs from that order.
    red = m.Square(side_length=2, fill_color=m.RED, fill_opacity=1, stroke_width=0).set_z_index(10)
    blue = m.Square(side_length=2, fill_color=m.BLUE, fill_opacity=1, stroke_width=0).set_z_index(-10)
    camera.capture(red, blue)
    pixel = camera.get_pixel_array()[32, 48]
    assert int(pixel[2]) > int(pixel[0])
    camera.capture(blue, red)
    pixel = camera.get_pixel_array()[32, 48]
    assert int(pixel[0]) > int(pixel[2])

    scene = m.Scene()
    scene.add(red)
    ticks = []
    red.add_updater(lambda obj, dt: ticks.append(dt), call=False)
    time, roots = scene.get_time(), tuple(scene.mobjects)
    camera.capture(red, blue)
    assert scene.get_time() == time and tuple(scene.mobjects) == roots and ticks == []
    assert red._scene is scene and not blue._is_bound()
    previous = camera.get_pixel_array()
    # Failed capture must leave the last successful frame available.
    try:
        camera.capture(object())
    except TypeError:
        pass
    else:
        raise AssertionError("non-Mobject capture must refuse")
    np.testing.assert_array_equal(previous, camera.get_pixel_array())
    for bad in (0, 97):
        camera.capture_threads = bad
        try:
            camera.capture(red)
        except ValueError:
            pass
        else:
            raise AssertionError("invalid thread count must refuse")
        np.testing.assert_array_equal(previous, camera.get_pixel_array())
    camera.capture_threads = 1
    camera.capture(red)
    serial = camera.get_pixel_array()
    for threads in (4, 16):
        camera.capture_threads = threads
        camera.capture(red)
        np.testing.assert_array_equal(serial, camera.get_pixel_array())

    # Geometry writes through an exported NumPy view must affect the next read.
    points = red.get_points()
    points[:, 0] += 2
    camera.capture(red)
    assert not np.array_equal(serial, camera.get_pixel_array())
    assert ticks == [] and scene.get_time() == time
    camera.frame.shift(m.RIGHT * 2)
    camera.capture(red)
    np.testing.assert_array_equal(serial, camera.get_pixel_array())

    # Direct calls cannot bypass graph validation, including disconnected cycles.
    for adjacency, roots in (([[0]], [0]), ([[1]], [0]), ([[]], [1])):
        try:
            m._CameraCapture(camera._core, camera.frame._core, (0., 0., 0., 0.),
                             (-10., 10., 10.), [red], adjacency, roots, 1)
        except ValueError:
            pass
        else:
            raise AssertionError("invalid native graph must refuse")
    camera.clear()
    assert not camera.get_pixel_array().any()
    camera.reset_pixel_shape(48, 32)
    assert camera.get_pixel_array().shape == (32, 48, 4)
    assert ticks == []
    red.clear_updaters()
    print("camera readback: native pixels, family identity, view writes, camera pose, order, failures, threads 1/4/16 passed")


run_camera_readback_acceptance()


def run_scene_readback_acceptance():
    """The image is the sampled state, including native/offloaded playback."""
    import gc
    import tempfile
    import weakref
    from pathlib import Path

    def scene():
        result = m.Scene()
        result.camera.reset_pixel_shape(96, 54)
        result.camera.background_rgba = [0., 0., 0., 0.]
        return result

    def square():
        return m.Square(side_length=1, fill_color=m.RED, fill_opacity=1, stroke_width=0)

    def expected(camera, *objects):
        oracle = m.Camera(resolution=camera.get_pixel_shape(), background_opacity=0)
        oracle.frame.move_to(camera.frame.get_center())
        oracle.frame._core.set_shape(camera.frame.get_shape())
        oracle.capture(*objects)
        return oracle.get_pixel_array()

    # Finish resets the source and removes it. The emitted endpoint still
    # shows the target, and must survive later live geometry/camera edits.
    sampled = scene()
    source = square().shift(2 * m.LEFT)
    target = source.copy().shift(4 * m.RIGHT)
    sampled.add(source)
    endpoint = expected(sampled.camera, target)
    sampled.play(m.Transform(source, target, final_alpha_value=0, remover=True),
                 run_time_frames=2, rate_func=m.linear)
    assert source not in sampled.mobjects
    source.shift(m.UP)
    sampled.frame.shift(m.LEFT)
    np.testing.assert_array_equal(sampled.camera.get_pixel_array(), endpoint)
    image = sampled.get_image()
    assert image is sampled.camera.get_image() and image.size == (96, 54)
    assert image.png() == sampled.camera.get_png()
    before = image.png()
    sampled.wait_frames(1)
    assert sampled.camera.get_png() != before
    assert image.png() == before

    # Each updater sees the previous capture. Reading never executes another
    # updater, and the last zero-dt pass cannot replace the last sampled image.
    stateful = scene()
    moving = square().shift(2 * m.LEFT)
    stateful.add(moving)
    stateful.camera.capture(moving)
    previous = [stateful.camera.get_pixel_array()]
    observations = []

    def move_after_read(mob, dt):
        if dt:
            observations.append(stateful.camera.get_pixel_array())
            np.testing.assert_array_equal(observations[-1], previous[-1])
            mob.shift(.5 * m.RIGHT)
            previous.append(expected(stateful.camera, mob))

    moving.add_updater(move_after_read, call=False)
    stateful.wait_frames(3)
    assert len(observations) == 3
    np.testing.assert_array_equal(stateful.camera.get_pixel_array(), previous[-1])
    moving.clear_updaters()

    # Host stop conditions run after capture and may edit the live scene.
    stopping = scene()
    stopped = square()
    stopping.add(stopped)
    stop_pixels = expected(stopping.camera, stopped)
    checked = []

    def stop():
        checked.append(stopping.get_time())
        if len(checked) == 2:
            stopped.shift(3 * m.UP)
            return True
        return False

    stopping.wait_frames(6, stop_condition=stop)
    assert len(checked) == 2
    np.testing.assert_array_equal(stopping.camera.get_pixel_array(), stop_pixels)

    # Empty and skipped segments do not manufacture captures. Explicit camera
    # clear/capture takes ownership only after it succeeds.
    preserved = stopping.camera.get_png()
    stopping.wait_frames(0)
    stopping.skip_animations = True
    stopping.play(stopped.animate.shift(m.LEFT), run_time_frames=2)
    stopping.skip_animations = False
    assert stopping.camera.get_png() == preserved
    stopping.wait_frames(1)
    try:
        stopping.camera.capture(object())
    except TypeError:
        pass
    else:
        raise AssertionError("invalid explicit capture must refuse")
    np.testing.assert_array_equal(stopping.camera.get_pixel_array(), expected(stopping.camera, stopped))
    stopping.wait_frames(1)
    stopping.camera.clear()
    assert not stopping.camera.get_pixel_array().any()
    # Repeated same-size resize notifications preserve the newer unread
    # sample, rather than exposing an older cached image of the same size.
    previous_pixels = stopping.camera.get_pixel_array()
    for reset in (lambda: stopping.camera.reset_pixel_shape(),
                  lambda: stopping.camera.reset_pixel_shape(96, 54),
                  lambda: stopping.on_resize(96, 54)):
        stopped.shift(.5 * m.RIGHT)
        next_pixels = expected(stopping.camera, stopped)
        assert not np.array_equal(previous_pixels, next_pixels)
        stopping.wait_frames(1)
        reset()
        previous_pixels = stopping.camera.get_pixel_array()
        np.testing.assert_array_equal(previous_pixels, next_pixels)
    stopping.wait_frames(1)
    stopping.camera.reset_pixel_shape(48, 27)
    assert stopping.camera.get_pixel_array().shape == (27, 48, 4)
    assert not stopping.camera.get_pixel_array().any()

    # A later failing callback preserves the previous completed sample.
    failing = scene()
    failed_shape = square()
    failing.add(failed_shape)
    calls = []

    def fail_on_second(mob, dt):
        if dt:
            calls.append(dt)
            if len(calls) == 2:
                mob.shift(3 * m.UP)
                raise ValueError("intentional readback callback failure")
            mob.shift(m.RIGHT)

    failed_shape.add_updater(fail_on_second, call=False)
    first_sample = expected(failing.camera, square().shift(m.RIGHT))
    try:
        failing.wait_frames(3)
    except ValueError as error:
        assert "intentional readback" in str(error)
    else:
        raise AssertionError("callback failure must propagate")
    np.testing.assert_array_equal(failing.camera.get_pixel_array(), first_sample)
    failed_shape.clear_updaters()

    # A shared camera belongs to its latest capture, independently of the
    # Scene used to request its pixels.
    first, second = scene(), scene()
    second.__dict__["_camera"] = first.camera
    first.add(square().shift(m.LEFT))
    second.add(square().shift(m.RIGHT))
    first.wait_frames(1)
    left_image = first.get_image()
    second.wait_frames(1)
    right_image = first.get_image()
    assert right_image.png() != left_image.png()
    assert second.get_image() is right_image

    # A pending readback cannot retain an updater -> Scene reference cycle.
    def orphaned_camera():
        owner = scene()
        member = square()
        member.add_updater(lambda mob, dt, owner=owner: None, call=False)
        owner.add(member)
        owner.wait_frames(1)
        member.clear_updaters()
        return owner.camera, weakref.ref(owner)

    camera, owner_ref = orphaned_camera()
    gc.collect()
    assert owner_ref() is None
    assert camera.get_pixel_array().any()

    # Production pure plays retain the final owner sample without disabling
    # native offload. Compare pixels to a separate camera before cleanup and
    # keep PNG sequences as evidence for the registered end-to-end scenario.
    root = Path(tempfile.mkdtemp(prefix="fmn-scene-readback-"))
    for threads in (1, 4, 16):
        pure = scene()
        with pure.render_session(root / str(threads), format="png_sequence",
                                 resolution=(96, 54), fps=8, threads=threads):
            shape = square().shift(m.LEFT)
            target = shape.copy().shift(2 * m.RIGHT)
            pure.add(shape)
            final_pixels = expected(pure.camera, target)
            pure.play(m.Transform(shape, target, remover=True), run_time_frames=3,
                      rate_func=m.linear)
            assert shape not in pure.mobjects
            np.testing.assert_array_equal(pure.camera.get_pixel_array(), final_pixels)
        assert pure._render_performance["pure_play_segments"] == 1
        assert pure._render_performance["pure_play_frames"] == 3
    # Authored camera/light accessors remain frame observers even without a
    # file render. Selecting the native path must not call them early, and
    # lazy pixel readback must not call them again after the sampled boundary.
    for mode in ("play", "wait"):
        observed = scene()
        shaded = m.Square(side_length=2, fill_color=m.WHITE, fill_opacity=1,
                          stroke_width=0).set_shading(0, 0, 1)
        observed.add(shaded)
        light_times = []

        def authored_light():
            time = observed.get_time()
            light_times.append(time)
            return (0., 0., 10. if time < .05 else -10.)

        observed.camera.light_source.get_center = authored_light
        if mode == "play":
            observed.play(m.Transform(shaded, shaded.copy().shift(m.RIGHT)),
                          run_time_frames=3, rate_func=m.linear)
        else:
            observed.wait_frames(3)
        np.testing.assert_allclose(light_times[:3], [1 / 30, 2 / 30, 3 / 30])
        assert len(light_times) == (4 if mode == "play" else 3), light_times
        oracle = m.Camera(resolution=(96, 54), background_opacity=0)
        oracle.light_source.move_to(-10 * m.OUT)
        oracle.capture(shaded)
        dark = oracle.get_pixel_array()
        oracle.light_source.move_to(10 * m.OUT)
        oracle.capture(shaded)
        assert not np.array_equal(dark, oracle.get_pixel_array())
        calls_before_read = len(light_times)
        np.testing.assert_array_equal(observed.camera.get_pixel_array(), dark)
        assert len(light_times) == calls_before_read

    class TimedCamera(m.Camera):
        @property
        def background_rgba(self):
            owner = self.__dict__.get("_readback_owner")
            if owner is None:
                return self.__dict__["_readback_background"]
            time = owner.get_time()
            self.__dict__["_readback_times"].append(time)
            return [.1 + time, 0., 0., 1.]

        @background_rgba.setter
        def background_rgba(self, value):
            self.__dict__["_readback_background"] = value

    class TimedScene(m.Scene):
        @property
        def camera(self):
            times = self.__dict__.get("_readback_camera_times")
            if times is not None:
                times.append(self.get_time())
            return super().camera

    temporal = TimedScene()
    timed = TimedCamera(resolution=(96, 54))
    timed.__dict__.update(_readback_owner=temporal, _readback_times=[])
    temporal.__dict__.update(_camera=timed, _readback_camera_times=[])
    temporal.wait_frames(3)
    np.testing.assert_allclose(timed.__dict__["_readback_times"], [1 / 30, 2 / 30, 3 / 30])
    np.testing.assert_allclose(temporal.__dict__["_readback_camera_times"], [1 / 30, 2 / 30, 3 / 30])
    oracle = m.Camera(resolution=(96, 54))
    oracle.background_rgba = [.2, 0., 0., 1.]
    oracle.clear()
    np.testing.assert_array_equal(timed.get_pixel_array(), oracle.get_pixel_array())
    assert len(timed.__dict__["_readback_times"]) == 3
    assert len(temporal.__dict__["_readback_camera_times"]) == 3

    # The embedded/gauntlet suite supplies real native updater slots. Keep the
    # installed-wheel script usable without exporting a test-only Python API.
    install_native = globals().get("_install_native_readback_updater")
    native_cases = 0
    if install_native is not None:
        for mode in ("play", "wait"):
            for installed_during_update in (False, True):
                observed = scene()
                driver = m.Point()
                shaded = m.Square(side_length=2, fill_color=m.WHITE,
                                  fill_opacity=1, stroke_width=0).set_shading(0, 0, 1)
                observed.add(driver, shaded)
                install_native(driver)
                native_positions, python_ticks = [], []

                def observe_native():
                    position = float(driver.get_center()[0])
                    native_positions.append(position)
                    return (0., 0., 10. if position < 2 else -10.)

                def note_python(mob, dt):
                    if dt:
                        python_ticks.append(dt)
                        if installed_during_update and len(python_ticks) == 1:
                            observed.camera.light_source.get_center = observe_native

                driver.add_updater(note_python, call=False)
                if not installed_during_update:
                    observed.camera.light_source.get_center = observe_native
                if mode == "play":
                    observed.play(m.Transform(shaded, shaded.copy().shift(m.RIGHT)),
                                  run_time_frames=3, rate_func=m.linear)
                else:
                    observed.wait_frames(3)
                assert len(python_ticks) == 3, python_ticks
                np.testing.assert_allclose(native_positions[:3], [1., 2., 3.])
                assert len(native_positions) == (4 if mode == "play" else 3)
                assert driver.get_center()[0] == 3, "native slots must run once per frame"
                oracle = m.Camera(resolution=(96, 54), background_opacity=0)
                oracle.light_source.move_to(-10 * m.OUT)
                oracle.capture(driver, shaded)
                np.testing.assert_array_equal(observed.camera.get_pixel_array(),
                                              oracle.get_pixel_array())
                assert len(native_positions) == (4 if mode == "play" else 3)
                driver.clear_updaters()
                native_cases += 1

            failed = scene()
            driver, shaded = m.Point(), square()
            failed.add(driver, shaded)
            install_native(driver, fail_at=2)
            observer_positions, previous_frames = [], []

            def observe_before_failure():
                observer_positions.append(float(driver.get_center()[0]))
                return (0., 0., 10.)

            def remember_previous_frame(mob, dt):
                if dt:
                    previous_frames.append(failed.camera.get_pixel_array())

            failed.camera.light_source.get_center = observe_before_failure
            driver.add_updater(remember_previous_frame, call=False)
            try:
                if mode == "play":
                    failed.play(m.Transform(shaded, shaded.copy().shift(m.RIGHT)),
                                run_time_frames=3, rate_func=m.linear)
                else:
                    failed.wait_frames(3)
            except RuntimeError as error:
                assert "intentional native updater failure" in str(error)
            else:
                raise AssertionError("native updater failure must precede capture accessors")
            assert observer_positions == [1.], observer_positions
            assert len(previous_frames) == 2
            np.testing.assert_array_equal(failed.camera.get_pixel_array(), previous_frames[-1])
            driver.clear_updaters()
            native_cases += 1
    return 14 + native_cases


scene_readback_cases = run_scene_readback_acceptance()
