"""Actual non-16:9 file, readback and pure-worker camera admission."""
from pathlib import Path

import manimlib as m


class _SerialAspectScene(m.Scene):
    def update_mobjects(self, dt):
        # An authored observer forces the ordinary owner capture path. It
        # leaves geometry and time unchanged, providing the serial oracle.
        return super().update_mobjects(dt)


def run_planar_aspect_acceptance(directory):
    readings = []
    for resolution in ((854, 480), (480, 854)):
        root = Path(directory) / f"{resolution[0]}x{resolution[1]}"
        scenes = []
        for label, scene_type, threads in (
            ("serial", _SerialAspectScene, 1),
            ("parallel", m.Scene, 4),
        ):
            scene = scene_type(camera_config={"resolution": resolution})
            with scene.render_session(root / label, format="png_sequence",
                                      resolution=resolution, fps=8,
                                      threads=threads) as session:
                square = m.Square(side_length=1.25, fill_color=m.BLUE,
                                  fill_opacity=.8, stroke_color=m.YELLOW,
                                  stroke_width=2).shift((-.5, .2, 0))
                circle = m.Circle(radius=.35, fill_color=m.RED,
                                  fill_opacity=1, stroke_width=0).shift((.9, -1, 0))
                scene.add(square, circle)
                scene.play(m.Transform(square, square.copy().shift((.5, .4, 0))),
                           run_time=.5, rate_func=m.linear)
                # Publish the exact final pose as well as animated samples.
                scene.wait(.25)
            assert session.result.frame_count == 6
            assert tuple(scene._render_routes) == (6, 0, None), scene._render_routes
            scenes.append((scene, int(session.result.frame_count)))

        serial, frame_count = scenes[0]
        parallel, parallel_frames = scenes[1]
        assert frame_count == parallel_frames
        assert serial._render_performance["pure_play_frames"] == 0
        assert parallel._render_performance["pure_play_frames"] == 4
        used, planned, _, in_flight = parallel._render_plan
        assert 1 <= used <= planned <= in_flight
        if planned > 1:
            assert used > 1, "a multi-team plan still used only the owner renderer"
        parallel.camera.capture_threads = 4
        parallel.camera.capture(*parallel.mobjects)
        readback = parallel.camera.get_pixel_array()
        assert readback.shape == (resolution[1], resolution[0], 4)

        mutation_routes, mutation_readbacks = [], []
        # Both dimensions are even, so these bounded stills preserve the
        # exact output aspect used above rather than testing another baseline.
        small = (resolution[0] // 2, resolution[1] // 2)
        for name in ("move", "zoom", "rotate"):
            scene = m.Scene(camera_config={"resolution": small})
            with scene.render_session(root / name, format="png_sequence",
                                      resolution=small, fps=8, threads=1) as session:
                scene.add(m.Square(side_length=1.25, fill_color=m.BLUE,
                                   fill_opacity=1, stroke_width=0))
                scene.camera.capture(*scene.mobjects)
                baseline = scene.camera.get_pixel_array().tobytes()
                if name == "move":
                    scene.frame.shift((.25, 0, 0))
                elif name == "zoom":
                    scene.frame.scale(.8)
                else:
                    scene.frame.rotate(.2)
                scene.wait(1 / 8)
            assert session.result.frame_count == 1
            planar, camera, reason = scene._render_routes
            assert (planar, camera, reason) == (
                0, 1, "camera frame differs from the default frame"), scene._render_routes
            scene.camera.capture(*scene.mobjects)
            current = scene.camera.get_pixel_array().tobytes()
            assert current != baseline, f"{name} camera readback ignored its live frame"
            mutation_routes.append((name, int(planar), int(camera), reason))
            mutation_readbacks.append(current)

        readings.append({
            "resolution": resolution,
            "frame_count": frame_count,
            "serial_routes": tuple(serial._render_routes),
            "parallel_routes": tuple(parallel._render_routes),
            "parallel_teams_used": int(used),
            "readback": readback.tobytes(),
            "camera_mutation_routes": mutation_routes,
            "camera_mutation_readbacks": mutation_readbacks,
        })
    return readings
