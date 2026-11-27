"""Real-extension cache acceptance; also invoked by the Rust portal test.

Run directly against an installed wheel for the additional production PNG
comparison. No layout/renderer substitute is used by this suite.
"""
from contextlib import redirect_stderr, redirect_stdout
import io
import json
import subprocess
import sys
from pathlib import Path
import tempfile

import numpy as np


def run_typeset_cache(m):
    configure = m._fmn_configure_tex_cache
    info = m._fmn_tex_cache_info
    source = r"\sum_{k=1}^{3} k = 6"
    with tempfile.TemporaryDirectory(prefix="fmn-typeset-cache-") as directory:
        root = Path(directory)
        cache = root / "owned-cache"
        report = configure(cache)
        assert all(row["persistent"] for row in report["templates"].values()), report
        first = m.Tex(source)
        expected = first.get_all_points().copy()
        assert expected.size > 0
        cold = info()["templates"]["default"]
        assert cold["layout_computations"] > 0
        assert (cache / "STORE_OWNER").is_file()
        assert any((cache / "ns" / "typeset").rglob("objects/*/*"))

        # Explicit reattachment clears the resident front without replacing
        # the engine. A second construction must then prove actual disk reuse.
        configure(cache)
        warm = m.Tex(source)
        after = info()["templates"]["default"]
        np.testing.assert_array_equal(warm.get_all_points(), expected)
        assert after["layout_computations"] == cold["layout_computations"], (cold, after)
        assert after["disk_hits"] > cold["disk_hits"], (cold, after)
        m.Tex(source)
        resident = info()["templates"]["default"]
        assert resident["memory_hits"] > after["memory_hits"]

        configure(enabled=False)
        uncached = m.Tex(source)
        np.testing.assert_array_equal(uncached.get_all_points(), expected)
        assert all(not row["persistent"] for row in info()["templates"].values())
        assert (cache / "STORE_OWNER").is_file(), "disable must not delete stored data"

        sentinel = root / "foreign-data.txt"
        sentinel.write_bytes(b"not cache data")
        with redirect_stderr(io.StringIO()) as diagnostic:
            refused = configure(root)
        assert all(not row["persistent"] for row in refused["templates"].values()), refused
        assert any(row["error"] for row in refused["templates"].values())
        assert "typeset-cache-unavailable" in diagnostic.getvalue()
        np.testing.assert_array_equal(m.Tex(source).get_all_points(), expected)
        assert sentinel.read_bytes() == b"not cache data"
        configure(enabled=False)


def run_rendered_cache_acceptance(m):
    from fmn_python import render_scene
    from fmn_python.typesetting import configure_tex_cache

    class Formula(m.Scene):
        def construct(self):
            self.add(m.Tex(r"\int_0^1 x^2\,dx = \frac{1}{3}"))

    with tempfile.TemporaryDirectory(prefix="fmn-cache-render-") as directory:
        root = Path(directory)
        outputs = []
        for name, enabled in (("cold", True), ("warm", True), ("disabled", False)):
            configure_tex_cache(root / "cache", enabled=enabled)
            output = root / (name + ".png")
            result = render_scene(Formula, output, format="png", resolution=(160, 90),
                                  fps=30, threads=1)
            assert result.frame_count == 1
            outputs.append(output.read_bytes())
        assert outputs[0].startswith(b"\x89PNG\r\n\x1a\n")
        assert outputs[0] == outputs[1] == outputs[2], "cache warmth changed production PNG bytes"
        configure_tex_cache(enabled=False)


def run_cache_gc_cli(m):
    from fmn_python.__main__ import _emit_cache_gc

    def collect(*args):
        stream = io.StringIO()
        with redirect_stdout(stream):
            code = _emit_cache_gc(m, list(args) + ["--robot"])
        return code, json.loads(stream.getvalue())

    with tempfile.TemporaryDirectory(prefix="fmn-typeset-gc-") as directory:
        cache = Path(directory) / "cache"
        try:
            m._fmn_configure_tex_cache(cache, max_bytes=0)
            points = m.Tex("x^2 + 1").get_all_points().copy()
            objects = list((cache / "ns/typeset").rglob("objects/*/*"))
            assert objects and points.size
            other = cache / "ns" / "journal" / "v1" / "record"
            other.parent.mkdir(parents=True)
            other.write_bytes(b"unrelated namespace")

            code, _ = collect("--cache-gc", "scene.py")
            assert code == 2 and all(path.is_file() for path in objects)
            code, report = collect("--cache-gc")
            assert code == 0 and report["kind"] == "cache-gc", report
            assert report["outcome"] == "completed" and report["max_bytes"] == 0, report
            assert report["evicted_entries"] == len(objects), report
            assert report["evicted_bytes"] > 0 and report["retained_bytes"] == 0, report
            assert not any(path.exists() for path in objects)
            assert other.read_bytes() == b"unrelated namespace"

            code, report = collect("--cache-gc")
            assert code == 0 and report["evicted_entries"] == report["evicted_bytes"] == 0, report
            m._fmn_configure_tex_cache(cache, max_bytes=0)
            np.testing.assert_array_equal(m.Tex("x^2 + 1").get_all_points(), points)
            with redirect_stderr(io.StringIO()):
                m._fmn_configure_tex_cache(directory)
            code, report = collect("--cache-gc")
            assert code == 4 and report["outcome"] == "unavailable", report
            assert other.read_bytes() == b"unrelated namespace"
        finally:
            m._fmn_configure_tex_cache(enabled=False)


def run_cache_retention_boundaries(m):
    from fmn_python import render_scene
    from fmn_python.typesetting import configure_tex_cache, typesetting_receipt

    class Formula(m.Scene):
        def construct(self):
            self.add(*(m.Tex(r"x^2 + y^2 = z^2", template=pack)
                       for pack in ("default", "basic", "empty")))

    with tempfile.TemporaryDirectory(prefix="fmn-cache-retention-") as directory:
        root, images = Path(directory), []
        cache = root / "owned-cache"
        try:
            for name, enabled, ceiling in (("retained", True, 1024 * 1024),
                                           ("evicted", True, 0), ("disabled", False, None)):
                configure_tex_cache(cache, enabled=enabled, max_bytes=ceiling)
                scene = Formula()
                output = root / (name + ".png")
                render_scene(scene, output, format="png", resolution=(120, 80), fps=12, threads=1)
                images.append(output.read_bytes())
                receipt = typesetting_receipt(scene)
                assert receipt['persistent'] is enabled, receipt
                assert receipt['persistent_ceiling_bytes'] == (ceiling if enabled else None), receipt
                assert receipt['maintenance']['runs'] == int(enabled), receipt
                if name == "retained":
                    assert any((cache / 'ns/typeset').rglob('objects/*/*'))
                if name == "evicted":
                    assert receipt['maintenance']['evicted_entries'] > 0, receipt
                    assert receipt['maintenance']['evicted_bytes'] > 0, receipt
                    assert receipt['maintenance']['retained_bytes'] == 0, receipt
                    assert receipt['maintenance']['errors'] == 0, receipt
                    assert not any((cache / 'ns/typeset').rglob('objects/*/*'))
            assert images[0].startswith(b'\x89PNG\r\n\x1a\n')
            assert images[0] == images[1] == images[2], 'disk retention changed rendered ink'
        finally:
            configure_tex_cache(enabled=False)


def run_installed_cache_gc():
    """Exercise console dispatch and native configuration discovery in fresh interpreters."""
    with tempfile.TemporaryDirectory(prefix="fmn-typeset-gc-cli-") as directory:
        root = Path(directory)
        config = root / "custom_config.yml"
        config.write_text("directories:\n  cache: local-cache\ncache:\n  max_bytes: 0\n",
                          encoding="utf-8")
        subprocess.run([sys.executable, "-c", "from manimlib import Tex; Tex('x+1')"],
                       cwd=root, check=True, timeout=60)
        objects = list((root / "local-cache/ns/typeset").rglob("objects/*/*"))
        assert objects
        result = subprocess.run([sys.executable, "-m", "fmn_python", "--robot", "--cache-gc"],
                                cwd=root, capture_output=True, text=True, check=True, timeout=60)
        receipt = json.loads(result.stdout)
        assert receipt["kind"] == "cache-gc" and receipt["evicted_entries"] == len(objects), receipt
        assert receipt["retained_bytes"] == 0 and not any(path.exists() for path in objects)


if __name__ in ("__main__", "<run_path>"):
    import manimlib as m
    run_typeset_cache(m)
    run_rendered_cache_acceptance(m)
    run_cache_gc_cli(m)
    run_installed_cache_gc()
    run_cache_retention_boundaries(m)
    print("native typeset cache and cold/warm/disabled PNG acceptance passed")
