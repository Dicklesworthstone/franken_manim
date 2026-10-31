"""Native witnesses for configured persistent raster tile profiles."""
import copy
import json
from pathlib import Path

import manimlib as native
from fmn_python import render_scene

root = Path(_AUTOTUNE_ROOT)
config = native.manim_config
saved = copy.deepcopy(config)


class VisibleCircle(native.Scene):
    expected = (8, 64, "adaptive")

    def construct(self):
        _assert_active_plan(self, *self.expected)
        self.add(native.Circle(radius=0.7, fill_opacity=1.0, stroke_width=0))


def file_render(name, expected=(8, 64, "adaptive")):
    scene = VisibleCircle()
    scene.expected = expected
    result = render_scene(
        scene, root / name, format="png", resolution=(32, 18), fps=8, threads=1,
    )
    assert result.frame_count == 1 and result.threads == 1
    assert result.autotune["applies_to"] == "retained-planar-2d"
    assert result.autotune["planar_frames"] == 1
    assert result.autotune["camera_frames"] == 0
    assert result.autotune["camera_fine_tile"] == _BASELINE_FINE
    assert result.autotune["camera_macro_tile"] == _BASELINE_MACRO
    assert tuple(scene._render_plan)[2] == 1
    assert result.as_dict()["autotune"] == result.autotune
    return result


def studio_capture(unusable_cache=False):
    scene = native.Scene()
    if unusable_cache:
        # Build the scene first: only the native capture boundary is under test.
        config["directories"]["cache"] = object()
        config["cache"]["max_bytes"] = object()
    scene._begin_studio_capture(
        "autotune-camera", "11" * 32, "22" * 32, 32, 18, 8, 1, 0, 4, 1024 * 1024,
    )
    scene.camera._core.set_pixel_shape(32, 18)
    scene.camera.fps = 8
    scene.add(native.Circle(radius=0.7, fill_opacity=1.0, stroke_width=0))
    recording = scene._finish_studio_capture()
    assert recording.frame_count == 1
    report = recording.autotune
    assert report["source"] == "standard-baseline"
    assert report["engine"] == "certified-cpu"
    assert report["fine_tile"] == _BASELINE_FINE
    assert report["macro_tile"] == _BASELINE_MACRO
    assert report["diagnostic"] is None
    assert "does not apply" in report["bypass_reason"]
    assert json.loads(recording.inspect(0)) is not None


try:
    config.setdefault("directories", {})["cache"] = str(root / "warm-cache")
    config.setdefault("cache", {})["max_bytes"] = 16 * 1024 * 1024
    config.setdefault("render", {})["aa"] = "adaptive"
    warm = file_render("warm.png")
    assert warm.autotune["source"] == "standard-autotune-cache"
    assert warm.autotune["diagnostic"] is None
    assert warm.autotune["bypass_reason"] is None

    class MixedRoutes(native.Scene):
        def construct(self):
            _assert_active_plan(self, 8, 64, "adaptive")
            circle = native.Circle(radius=0.7, fill_opacity=1.0, stroke_width=0)
            self.add(circle)
            self.wait(0.25)
            circle.shift(native.OUT)
            self.wait(0.25)

    mixed = render_scene(
        MixedRoutes, root / "mixed", format="png_sequence",
        resolution=(32, 18), fps=8, threads=1,
    )
    assert mixed.autotune["source"] == "standard-autotune-cache"
    assert mixed.autotune["applies_to"] == "retained-planar-2d"
    assert mixed.autotune["planar_frames"] > 0
    assert mixed.autotune["camera_frames"] > 0
    assert mixed.autotune["camera_fine_tile"] == _BASELINE_FINE
    assert mixed.autotune["camera_macro_tile"] == _BASELINE_MACRO
    assert mixed.autotune["planar_frames"] + mixed.autotune["camera_frames"] == mixed.frame_count

    # The exact same profile root has a distinct measured record for this AA.
    # The Rust hook checks the real retained renderer's AA and tile geometry.
    config["render"]["aa"] = "ssaa2x"
    aa = file_render("aa.png", (32, 256, "ssaa2x"))
    assert aa.autotune["source"] == "standard-autotune-cache"

    config["render"]["aa"] = "adaptive"
    config["directories"]["cache"] = str(root / "foreign-cache")
    foreign = file_render("foreign.png", (_BASELINE_FINE, _BASELINE_MACRO, "adaptive"))
    assert foreign.autotune["source"] == "standard-baseline"
    assert foreign.autotune["diagnostic"] is None

    config["directories"]["cache"] = str(root / "corrupt-cache")
    corrupt = file_render("corrupt.png", (_BASELINE_FINE, _BASELINE_MACRO, "adaptive"))
    assert corrupt.autotune["source"] == "standard-baseline"
    assert "autotune" in corrupt.autotune["diagnostic"]
    assert "using standard-baseline" in corrupt.autotune["diagnostic"]

    config["directories"]["cache"] = str(root / "missing-cache")
    miss = file_render("miss.png", (_BASELINE_FINE, _BASELINE_MACRO, "adaptive"))
    assert miss.autotune["source"] == "standard-baseline"
    assert miss.autotune["diagnostic"] is None
    # Other render consumers may initialize the shared root (Scribe currently
    # does). A profile miss must leave its own namespace absent; the Rust
    # witness separately checks that the cold profile read creates no root.
    assert not (root / "missing-cache" / _AUTOTUNE_NAMESPACE).exists()

    # A populated FastCpu record must not enter the distinct camera engine.
    config["directories"]["cache"] = str(root / "warm-cache")
    studio_capture()
    studio_capture(unusable_cache=True)
finally:
    config.clear()
    config.update(saved)
