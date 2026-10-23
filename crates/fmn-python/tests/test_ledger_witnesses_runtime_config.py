"""Ledger witnesses for runtime_config (fm-6u1g): each test calls a manimlib symbol the Parity Ledger
marks same/improved and checks the Reference's behaviour (manimlib @ 6199a00d)."""
import contextlib
import io
import os
from pathlib import Path
import struct
import sys
import tempfile
import textwrap
import unittest
import zlib

import numpy as np

from test_parallel_batch_native import native_wheel_available


# Two scenes in one file, so selection is observable. Frame height 8 at 64x36
# gives 4.5 px per unit: a side-2 square centred at ORIGIN covers pixel
# columns 27.5..36.5 and rows 13.5..22.5, fully covering columns 28..35 and
# rows 14..21. Colours are the Reference constants RED #FC6255, BLUE #58C4DD
# over default_config.yml's camera background #333333.
_SCENES = textwrap.dedent("""\
    from manimlib import *

    class Tiny(Scene):
        def construct(self):
            self.add(Square(side_length=2, fill_color=RED, fill_opacity=1, stroke_width=0))

    class Other(Scene):
        def construct(self):
            self.add(Circle(radius=1, fill_color=BLUE, fill_opacity=1, stroke_width=0))
""")
_RED = (0xFC, 0x62, 0x55)
_BLUE = (0x58, 0xC4, 0xDD)
_BACKGROUND = (0x33, 0x33, 0x33)


def _read_png(path):
    """Decode an 8-bit, non-interlaced RGB/RGBA PNG to an (h, w, c) uint8 array."""
    data = Path(path).read_bytes()
    assert data[:8] == b"\x89PNG\r\n\x1a\n", "not a PNG"
    pos, idat, header = 8, b"", None
    while pos < len(data):
        length, kind = struct.unpack(">I4s", data[pos:pos + 8])
        body = data[pos + 8:pos + 8 + length]
        if kind == b"IHDR":
            header = struct.unpack(">IIBBBBB", body)
        elif kind == b"IDAT":
            idat += body
        pos += 12 + length
    width, height, depth, color, _, _, interlace = header
    assert depth == 8 and interlace == 0 and color in (2, 6), header
    channels = 3 if color == 2 else 4
    stride = width * channels
    raw = zlib.decompress(idat)
    rows, prev = [], [0] * stride
    for y in range(height):
        start = y * (stride + 1)
        kind, line = raw[start], raw[start + 1:start + 1 + stride]
        cur = [0] * stride
        for x in range(stride):
            a = cur[x - channels] if x >= channels else 0
            b = prev[x]
            c = prev[x - channels] if x >= channels else 0
            if kind == 0:
                p = 0
            elif kind == 1:
                p = a
            elif kind == 2:
                p = b
            elif kind == 3:
                p = (a + b) // 2
            else:
                pa, pb, pc = abs(b - c), abs(a - c), abs(a + b - 2 * c)
                p = a if pa <= pb and pa <= pc else (b if pb <= pc else c)
            cur[x] = (line[x] + p) & 0xFF
        rows.append(cur)
        prev = cur
    return np.array(rows, dtype=np.uint8).reshape(height, width, channels)


@unittest.skipUnless(native_wheel_available(), "requires the built FrankenManim native wheel")
class RuntimeConfigWitnesses(unittest.TestCase):
    # -- helpers -----------------------------------------------------------

    def _tempdir(self, prefix):
        temporary = tempfile.TemporaryDirectory(prefix=prefix)
        self.addCleanup(temporary.cleanup)
        return temporary.name

    def _scene_workdir(self, stem):
        """A temp cwd holding the scene file and a custom_config.yml that keeps the
        Tex cache inside it, so no render touches the real user cache."""
        work = Path(self._tempdir("fm6u1g-runtime-config-"))
        source = work / (stem + ".py")
        source.write_text(_SCENES)
        (work / "custom_config.yml").write_text(
            "directories:\n  cache: %s\n" % (work / "cache")
        )
        previous = os.getcwd()
        os.chdir(work)
        self.addCleanup(os.chdir, previous)
        return work, source

    def _with_argv(self, argv):
        previous = sys.argv
        sys.argv = list(argv)
        self.addCleanup(setattr, sys, "argv", previous)

    def _assert_scene_png(self, path, fill, square):
        self.assertTrue(Path(path).is_file(), f"{path} was not written")
        image = _read_png(path)
        # -r 64x36 is the camera resolution (Reference update_camera_config).
        self.assertEqual(image.shape[:2], (36, 64))
        rgb = image[..., :3].astype(int)
        np.testing.assert_allclose(rgb[18, 32], fill, atol=3)
        for corner in (rgb[0, 0], rgb[0, 63], rgb[35, 0], rgb[35, 63]):
            np.testing.assert_allclose(corner, _BACKGROUND, atol=3)
        if square:
            filled = np.all(np.abs(rgb - np.array(fill)) <= 3, axis=-1)
            ys, xs = np.nonzero(filled)
            self.assertEqual((ys.min(), ys.max(), xs.min(), xs.max()), (14, 21, 28, 35))
            self.assertEqual(int(filled.sum()), 64)

    def _set_directory(self, key, value):
        from manimlib.config import manim_config

        directories = manim_config.directories
        previous = directories[key]
        directories[key] = value
        self.addCleanup(directories.__setitem__, key, previous)

    # -- manimlib.__main__ -------------------------------------------------

    def test_main(self):
        # Reference __main__.main: parse the CLI, then run the named scenes.
        # `manimgl FILE Tiny -w -s --video_dir OUT -r 64x36` saves Tiny's last
        # frame as OUT/Tiny.png at 64x36 and does not run the unnamed Other.
        from manimlib.__main__ import main

        work, source = self._scene_workdir("rc_main_scenes")
        out = work / "out"
        self._with_argv(["manimgl", str(source), "Tiny", "-w", "-s",
                         "--video_dir", str(out), "-r", "64x36"])
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            code = main()
        self.assertIn(code, (None, 0))
        self._assert_scene_png(out / "Tiny.png", _RED, square=True)
        self.assertFalse((out / "Other.png").exists(), "an unselected scene was run")
        self.assertEqual(sorted(p.name for p in out.iterdir()), ["Tiny.png"])

    def test_run_scenes(self):
        # Reference __main__.run_scenes: run every scene the run config selects;
        # with -a (write_all) that is every scene in the file.
        from manimlib.__main__ import run_scenes

        work, source = self._scene_workdir("rc_run_scenes")
        out = work / "all"
        self._with_argv(["manimgl", str(source), "-a", "-w", "-s",
                         "--video_dir", str(out), "-r", "64x36"])
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            code = run_scenes()
        self.assertIn(code, (None, 0))
        self.assertEqual(sorted(p.name for p in out.iterdir()), ["Other.png", "Tiny.png"])
        self._assert_scene_png(out / "Tiny.png", _RED, square=True)
        self._assert_scene_png(out / "Other.png", _BLUE, square=False)

    # -- manimlib.config ---------------------------------------------------

    def test_update_directory_config(self):
        # Reference config.py:235: each directories.subdirs entry becomes
        # directories[key] = os.path.join(base, subdir), in place.
        from manimlib.config import manim_config, update_directory_config

        config_dict = type(manim_config)
        base = self._tempdir("fm6u1g-dirs-")
        config = config_dict(directories=config_dict(
            base=base,
            subdirs={"output": "vids", "raster_images": "rasters", "sounds": "snd"},
            output="stale-output",
        ))
        result = update_directory_config(config)
        self.assertIsNone(result)
        directories = config.directories
        self.assertEqual(directories["output"], os.path.join(base, "vids"))
        self.assertEqual(directories["raster_images"], os.path.join(base, "rasters"))
        self.assertEqual(directories["sounds"], os.path.join(base, "snd"))
        self.assertEqual(directories["base"], base)
        self.assertEqual(
            set(directories),
            {"base", "subdirs", "output", "raster_images", "sounds"},
        )

        # An empty base leaves the bare subdirectory names (os.path.join("", s)).
        bare = config_dict(directories=config_dict(base="", subdirs={"data": "data_dir"}))
        update_directory_config(bare)
        self.assertEqual(bare.directories["data"], "data_dir")

    # -- manimlib.utils.directories ---------------------------------------

    def _check_configured_getter(self, getter_name, key):
        from manimlib.config import manim_config
        from manimlib.utils import directories as dirs

        getter = getattr(dirs, getter_name)
        configured = manim_config.directories
        # The default config resolves the key through update_directory_config.
        self.assertEqual(
            getter(),
            os.path.join(configured["base"], configured["subdirs"][key]),
        )
        self.assertEqual(getter(), configured[key])
        # The getter reads manim_config.directories live, not a snapshot.
        replacement = os.path.join(self._tempdir("fm6u1g-getter-"), key + "-live")
        self._set_directory(key, replacement)
        self.assertEqual(getattr(dirs, getter_name)(), replacement)

    def test_load_yaml(self):
        # Reference config.py: yaml.safe_load(file) or {}, {} for a missing file, an error for
        # malformed YAML. A tuple-like scalar stays a string, as PyYAML leaves it.
        from manimlib.config import load_yaml

        base = Path(self._tempdir("fm6u1g-yaml-"))
        config = base / "custom_config.yml"
        config.write_text("camera:\n  fps: 15\n  resolution: (64, 36)\nscene:\n  leave_progress_bars: True\n")
        self.assertEqual(load_yaml(str(config)), {
            "camera": {"fps": 15, "resolution": "(64, 36)"},
            "scene": {"leave_progress_bars": True},
        })
        self.assertEqual(load_yaml(str(base / "absent.yml")), {})
        empty = base / "empty.yml"
        empty.write_text("")
        self.assertEqual(load_yaml(str(empty)), {})
        malformed = base / "malformed.yml"
        malformed.write_text("camera:\n  fps: 15\n bad indent: [\n")
        with self.assertRaises(Exception):
            load_yaml(str(malformed))

    def test_get_manim_dir(self):
        # Reference config.py: abspath(join(dirname(getabsfile(manimlib)), "..")), the
        # directory that holds the manimlib package, so join(get_manim_dir(), "manimlib", ...)
        # names files inside the package.
        import manimlib
        from manimlib.config import get_manim_dir

        manim_dir = get_manim_dir()
        package = os.path.dirname(os.path.abspath(manimlib.__file__))
        self.assertEqual(manim_dir, os.path.dirname(package))
        self.assertTrue(os.path.isfile(os.path.join(manim_dir, "manimlib", "__init__.py")))

    def test_get_raster_image_dir(self):
        self._check_configured_getter("get_raster_image_dir", "raster_images")

    def test_get_vector_image_dir(self):
        self._check_configured_getter("get_vector_image_dir", "vector_images")

    def test_get_three_d_model_dir(self):
        self._check_configured_getter("get_three_d_model_dir", "three_d_models")

    def test_get_downloads_dir(self):
        # Reference: directories["downloads"] or appdirs.user_cache_dir("manim_downloads").
        self._check_configured_getter("get_downloads_dir", "downloads")
        from manimlib.utils.directories import get_downloads_dir

        self._set_directory("downloads", "")
        if sys.platform == "darwin":
            expected = os.path.join(os.path.expanduser("~/Library/Caches"), "manim_downloads")
        elif os.name == "nt":
            return  # appdirs' Windows branch needs the registry; outside the certified matrix
        else:
            cache_home = self._tempdir("fm6u1g-xdg-")
            previous = os.environ.get("XDG_CACHE_HOME")
            os.environ["XDG_CACHE_HOME"] = cache_home
            if previous is None:
                self.addCleanup(os.environ.pop, "XDG_CACHE_HOME", None)
            else:
                self.addCleanup(os.environ.__setitem__, "XDG_CACHE_HOME", previous)
            expected = os.path.join(cache_home, "manim_downloads")
        self.assertEqual(get_downloads_dir(), expected)


if __name__ == "__main__":
    unittest.main()
