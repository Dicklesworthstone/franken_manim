"""Named assets resolve under the configured asset root, as the Reference finds them.

The Reference finds an asset named by a scene with find_file over the configured
``directories`` (custom_config.yml ``directories.base`` plus its subdirectories):
ImageMobject and TexturedSurface through ``get_full_raster_image_path``, SVGMobject
through ``get_full_vector_image_path``, Scene.add_sound through
``get_full_sound_file_path``. The fixture assets are authored here and are public.
"""
from __future__ import annotations

import importlib
import struct
import tempfile
import unittest
import wave
import zlib
from pathlib import Path

import numpy as np
import manimlib as m


PIXELS = np.array([
    [[255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255]],
    [[255, 255, 0, 255], [0, 255, 255, 255], [255, 0, 255, 255]],
], dtype=np.uint8)
SVG = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><rect width="6" height="3"/></svg>'


def png(pixels):
    height, width, _ = pixels.shape
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))
    return (b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(b"".join(b"\0" + row.tobytes() for row in pixels)))
            + chunk(b"IEND", b""))


class AssetRootTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="fmn asset root ")
        self.root = Path(self.directory.name)
        for name in ("raster_images", "vector_images", "sounds"):
            (self.root / name).mkdir()
        (self.root / "raster_images" / "swatch.png").write_bytes(png(PIXELS))
        (self.root / "vector_images" / "glyph.svg").write_text(SVG)
        with wave.open(str(self.root / "sounds" / "tick.wav"), "wb") as stream:
            stream.setnchannels(1)
            stream.setsampwidth(2)
            stream.setframerate(48000)
            stream.writeframes(struct.pack("<h", 8000) * 2400)
        # The Reference's own mechanism (config.py:235): every subdirectory
        # hangs from directories.base, which custom_config.yml sets.
        config = importlib.import_module("manimlib.config")
        directories = config.manim_config.directories
        self.saved = dict(directories)
        directories["base"] = str(self.root)
        config.update_directory_config(config.manim_config)

    def tearDown(self):
        directories = importlib.import_module("manimlib.config").manim_config.directories
        directories.clear()
        directories.update(self.saved)
        self.directory.cleanup()

    def test_named_assets_resolve_under_the_configured_root(self):
        for name in ("swatch", "swatch.png"):
            image = m.ImageMobject(name, height=2)
            self.assertEqual(Path(image.image_path), self.root / "raster_images" / "swatch.png")
            self.assertEqual((image.pixel_width, image.pixel_height), (3, 2))
        glyph = m.SVGMobject("glyph")
        self.assertAlmostEqual(glyph.get_width() / glyph.get_height(), 2.0, places=6)
        scene = m.Scene()
        scene.add_sound("tick", gain=-3.0)
        self.assertEqual([fact[0] for fact in scene._sound_request_facts()],
                         [str(self.root / "sounds" / "tick.wav")])
        # A skipped cue is not looked up, as in the Reference.
        scene.skip_animations = True
        scene.add_sound("https://example.invalid/skipped.wav")
        self.assertEqual(len(scene._sound_request_facts()), 1)

    def test_resolved_assets_reach_the_rendered_frame_and_the_mix(self):
        scene = m.Scene()
        frame = self.root / "frame.png"
        with scene.render_session(frame, format="png", resolution=(96, 64), fps=8, threads=1):
            scene.add(m.ImageMobject("swatch", height=8))
        pixels = m.ImageMobject(str(frame)).get_pixel_array()
        # The 96x64 frame is 12 units by 8, so the 3x2 swatch fills it: each
        # source pixel is a 32x32 cell centred at x = 16, 48, 80 and y = 16, 48.
        # The sampler filters in linear light, so a cell centre is nearest its
        # own colour without being exactly it.
        palette = PIXELS[:, :, :3].reshape(-1, 3).astype(int)
        for row, y in enumerate((16, 48)):
            for column, x in enumerate((16, 48, 80)):
                distances = ((palette - pixels[y, x, :3].astype(int)) ** 2).sum(axis=1)
                self.assertEqual(int(np.argmin(distances)), 3 * row + column, (row, column, pixels[y, x]))
        sound = self.root / "mix.wav"
        scene = m.Scene()
        with scene.render_session(sound, format="wav", resolution=(96, 64), fps=8, threads=1):
            scene.add_sound("tick")
            scene.wait(.25)
        with wave.open(str(sound), "rb") as stream:
            frames = stream.readframes(stream.getnframes())
        self.assertTrue(any(frames), "the configured tick.wav was not mixed")

    def test_missing_and_network_assets_refuse_by_name(self):
        with self.assertRaises(OSError) as caught:
            m.ImageMobject("absent")
        self.assertEqual(str(caught.exception), "absent not Found")
        with self.assertRaises(OSError) as caught:
            m.SVGMobject("absent")
        self.assertEqual(str(caught.exception), "absent not Found")
        for make in (lambda: m.ImageMobject("https://example.invalid/a.png"),
                     lambda: m.Scene().add_sound("https://example.invalid/a.wav")):
            with self.assertRaisesRegex(m._CapabilityError, "AssetFetcher"):
                make()
        # A sound the lookup cannot find is recorded as authored, and the mix
        # refuses it by name (BN-14).
        scene = m.Scene()
        scene.add_sound("absent")
        self.assertEqual([fact[0] for fact in scene._sound_request_facts()], ["absent"])
        destination = self.root / "absent.wav"
        scene = m.Scene()
        with self.assertRaisesRegex(OSError, "absent"):
            with scene.render_session(destination, format="wav", resolution=(96, 64), fps=8,
                                      threads=1):
                scene.add_sound("absent")
                scene.wait(.25)
        self.assertFalse(destination.exists())


suite = unittest.defaultTestLoader.loadTestsFromTestCase(AssetRootTests)
result = unittest.TextTestRunner(verbosity=2).run(suite)
if not result.wasSuccessful():
    raise AssertionError("asset-root acceptance failed")
