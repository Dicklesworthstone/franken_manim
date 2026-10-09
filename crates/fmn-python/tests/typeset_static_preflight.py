"""The portal's automatic static Tex preflight and config-selected cache.

Invoked by the Rust portal test against the real extension; also runnable
directly against an installed wheel. No layout or render substitute is used.
"""
import gc
import importlib.util
import os
from pathlib import Path
import sys
import tempfile
import threading

FORMULAS = [
    r"e^{i\pi} + 1 = 0",
    r"a^2 + b^2 = c^2",
    r"\frac{a}{b} + \frac{c}{d} = \frac{ad + bc}{bd}",
    r"x_{1,2} = \frac{-b \pm \sqrt{b^2 - 4ac}}{2a}",
    r"\sum_{n=1}^{\infty} \frac{1}{n^2} = \frac{\pi^2}{6}",
    r"\int_0^1 x^2 \, dx = \frac{1}{3}",
    r"\prod_{k=1}^{n} k = n!",
    r"\lim_{h \to 0} \frac{f(x+h) - f(x)}{h}",
    r"\binom{n}{k} = \frac{n!}{k!\,(n-k)!}",
    r"\begin{pmatrix} a & b \\ c & d \end{pmatrix}",
    r"f(x) = \begin{cases} x & x > 0 \\ -x & x \le 0 \end{cases}",
    r"\sqrt[3]{x + 1}",
    r"\nabla \cdot \mathbf{E} = \frac{\rho}{\varepsilon_0}",
    r"\mathbb{E}[X] = \sum_x x \, p(x)",
    r"\sigma^2 = \mathbb{E}\left[(X - \mu)^2\right]",
    r"P(A \mid B) = \frac{P(B \mid A)\, P(A)}{P(B)}",
    r"\left| \sum_i a_i b_i \right| \le \sqrt{\sum_i a_i^2} \sqrt{\sum_i b_i^2}",
    r"\hat{x} + \overline{AB}",
    r"\bar{X}_n = \frac{1}{n} \sum_{i=1}^{n} X_i",
    r"P\left(|\bar{X}_n - \mu| \ge t\right) \le 2 e^{-2 n t^2}",
]


def _scene_source():
    calls = ",\n            ".join(f"Tex({formula!r}, font_size=20)" for formula in FORMULAS)
    return f'''from manimlib import *


class TwentyStatic(Scene):
    def construct(self):
        sheet = VGroup(
            {calls},
            TexText("Hoeffding's inequality", font_size=20),
        ).arrange(DOWN, buff=0.05)
        dynamic = "x^{{" + str(7) + "}}"
        self.add(sheet, Tex(dynamic))
        self.play(sheet.animate.shift(0.1 * UP), run_time=0.2)
'''


def _load(directory):
    path = Path(directory) / "fmn_static_preflight_scene.py"
    path.write_text(_scene_source(), encoding="utf-8")
    spec = importlib.util.spec_from_file_location("fmn_static_preflight_scene", path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def _render(m, scene_type, output):
    from fmn_python import render_scene

    scene = scene_type()
    result = render_scene(scene, output, format="png", resolution=(160, 90), fps=10, threads=2)
    assert result.frame_count == 1, result
    return scene, Path(output).read_bytes()


def run_static_preflight(m):
    from fmn_python.typesetting import configure_static_tex_preflight, configure_tex_cache

    info = m._fmn_tex_cache_info
    with tempfile.TemporaryDirectory(prefix="fmn-static-preflight-") as directory:
        root = Path(directory)
        module = _load(root)
        try:
            # Cold: the twenty literal formulas and the TexText are found in
            # the scene's own source and typeset on the pool before setup().
            configure_tex_cache(root / "cache")
            scene, cold_png = _render(m, module.TwentyStatic, root / "cold.png")
            report = scene._fmn_static_tex_preflight
            assert report["enabled"] and report["discovered"] == 21, report
            assert report["requests"] == 21 and report["succeeded"] == 21, report
            assert report["failed"] == 0 and report["wall_ns"] > 0, report
            # Math and text are separate request groups.
            assert report["batches"] == 2, report
            before, after = report["before"]["default"], report["after"]["default"]
            preflighted = after["layout_computations"] - before["layout_computations"]
            # 21 strings plus the shared scale-calibration probe per batch;
            # the probe is laid out once.
            assert preflighted == 22, (before, after)
            workers = after["preflight_workers"]
            if (os.cpu_count() or 1) >= 2:
                assert workers >= 2 and after["preflight_active_workers"] >= 2, after
            # Construction and play laid out only the one dynamic string.
            done = info()["templates"]["default"]
            assert done["layout_computations"] - after["layout_computations"] == 1, (after, done)

            # Warm, fresh binding: every static string is a verified disk hit
            # and the rendered PNG is byte-identical to the cold one.
            configure_tex_cache(root / "cache")
            scene, warm_png = _render(m, module.TwentyStatic, root / "warm.png")
            report = scene._fmn_static_tex_preflight
            before, after = report["before"]["default"], report["after"]["default"]
            assert after["layout_computations"] == before["layout_computations"], report
            assert after["disk_hits"] - before["disk_hits"] == 22, report
            assert warm_png == cold_png, "a warm typeset cache changed the PNG"

            # Planted negative: with the preflight off, the same scene's
            # construction lays out every formula itself.
            configure_static_tex_preflight(False)
            configure_tex_cache(root / "other-cache")
            start = info()["templates"]["default"]["layout_computations"]
            scene, off_png = _render(m, module.TwentyStatic, root / "off.png")
            report = scene._fmn_static_tex_preflight
            assert not report["enabled"] and report["discovered"] == 0, report
            done = info()["templates"]["default"]["layout_computations"]
            assert done - start >= 22, (start, done)
            assert off_png == cold_png, "the preflight changed the PNG"
        finally:
            configure_static_tex_preflight(True)
            configure_tex_cache(enabled=False)
            sys.modules.pop("fmn_static_preflight_scene", None)


def run_config_selected_cache(m):
    """``directories.cache`` from the config selects a fresh thread's store."""
    with tempfile.TemporaryDirectory(prefix="fmn-config-cache-") as directory:
        cache = Path(directory) / "configured"
        directories = m.manim_config["directories"]
        previous = directories.get("cache")
        directories["cache"] = str(cache)
        outcome = {}

        def build():
            # A new host thread has no binding yet: its first typesetting
            # binds the configured store. Only native typesetting runs here;
            # portal mobjects are unsendable and must stay on their thread.
            try:
                m._fmn_ensure_tex_cache()
                receipt = m._preflight_tex([r"\frac{1}{2}"])
                outcome["succeeded"] = receipt["succeeded"]
                outcome["persistent"] = receipt["after"]["persistent"]
            except BaseException as error:  # surfaced below
                outcome["error"] = repr(error)
            finally:
                m._fmn_configure_tex_cache(enabled=False)

        # Collect this thread's garbage first and keep the collector off while
        # the other thread runs, so no unsendable family is freed over there.
        gc.collect()
        gc.disable()
        try:
            worker = threading.Thread(target=build)
            worker.start()
            worker.join()
        finally:
            gc.enable()
            directories["cache"] = previous
        assert "error" not in outcome, outcome
        assert outcome["persistent"] is True and outcome["succeeded"] == 1, outcome
        assert (cache / "STORE_OWNER").is_file()
        assert any((cache / "ns" / "typeset").rglob("objects/*/*"))


if __name__ in ("__main__", "<run_path>"):
    import manimlib as m
    run_static_preflight(m)
    run_config_selected_cache(m)
    print("static Tex preflight and config-selected cache acceptance passed")
