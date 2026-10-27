"""Governed image decoding through real ImageMobjects, frames and checkpoints."""
from __future__ import annotations

import copy
import hashlib
import os
from pathlib import Path
import pickle
import shutil
import struct
import tempfile
import unittest
import zlib

import numpy as np
import manimlib as m


PIXELS = np.array([
    [[255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255]],
    [[255, 255, 0, 255], [0, 255, 255, 255], [255, 0, 255, 255]],
], dtype=np.uint8)


def png(pixels):
    height, width, _ = pixels.shape
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))
    return (b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(b"".join(b"\0" + row.tobytes() for row in pixels)))
            + chunk(b"IEND", b""))


def bmp(pixels):
    """Small bottom-up BGR fixture with padding, exercising orientation too."""
    height, width, _ = pixels.shape
    padding = b"\0" * ((-width * 3) % 4)
    data = b"".join(row[:, [2, 1, 0]].tobytes() + padding for row in pixels[::-1])
    return (b"BM" + struct.pack("<IHHI", 54 + len(data), 0, 0, 54)
            + struct.pack("<IiiHHIIiiII", 40, width, height, 1, 24, 0, len(data), 0, 0, 0, 0)
            + data)


def output(image, path, *, threads=1, moving=False):
    scene = m.Scene()
    with scene.render_session(path, format="png_sequence" if moving else "png",
                              resolution=(96, 64), fps=8, threads=threads) as session:
        scene.add(image)
        if moving:
            scene.play(image.animate.shift(m.RIGHT), run_time=.5, rate_func=m.linear)
            scene.wait(.25)
    return session.result


class ImageInputTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="fmn image inputs ")
        self.root = Path(self.directory.name)

    def tearDown(self):
        self.directory.cleanup()

    def external(self):
        ffmpeg = shutil.which("ffmpeg")
        if not ffmpeg:
            if os.environ.get("FMN_REQUIRE_FFMPEG"):
                self.fail("required ffmpeg is unavailable")
            self.skipTest("external image decoding requires ffmpeg")
        executable = self.root / "decoder with spaces"
        shutil.copyfile(ffmpeg, executable)
        executable.chmod(0o700)
        return executable

    def assert_certified_refusal(self, image, *, restore=False, sequence=False):
        # Exercise the real certified native generation. Embedded acceptance
        # has no installed wheel identity and therefore cannot sign a manifest.
        scene = m.Scene()
        path = self.root / ("refused-sequence" if sequence else "refused.png")
        scene._begin_native_output(str(path), "png_sequence" if sequence else "png",
                                   96, 64, 8, 4, 0, True)
        try:
            scene.add(image)
            if restore:
                checkpoint = scene.get_state()
                image.set_pixel_array(np.zeros_like(PIXELS))
                checkpoint.restore_scene(scene)
                np.testing.assert_array_equal(image.get_pixel_array(), PIXELS)
            with self.assertRaisesRegex(RuntimeError, "certified rendering refuses ffmpeg-decoded"):
                if sequence:
                    scene.play(image.animate.shift(m.RIGHT), run_time=.5, rate_func=m.linear)
                else:
                    scene._finish_render()
        finally:
            scene._abort_render()
        self.assertFalse(path.exists(), "a refused generation published an artifact")

    def test_native_png_ignores_missing_tool_and_reports_exact_bytes(self):
        encoded = png(PIXELS)
        asset = self.root / "native.asset"
        asset.write_bytes(encoded)
        image = m.ImageMobject(asset, height=3, ffmpeg_bin=self.root / "missing-ffmpeg")
        np.testing.assert_array_equal(image.get_pixel_array(), PIXELS)
        receipt = output(image, self.root / "native.png")
        expected = output(m.ImageMobject(PIXELS, height=3), self.root / "pixels.png")
        self.assertEqual(receipt.digest, expected.digest)
        self.assertEqual(receipt.ffmpeg_invocations, ())
        self.assertEqual(len(receipt.image_inputs), 1)
        fact = receipt.image_inputs[0]
        self.assertEqual((fact["container"], fact["decoder"]), ("png", "native"))
        self.assertEqual(fact["path"], str(asset))
        self.assertEqual(fact["source_sha256"], hashlib.sha256(encoded).hexdigest())
        self.assertEqual(fact["rgba_sha256"], hashlib.sha256(PIXELS.tobytes()).hexdigest())
        self.assertEqual((fact["width"], fact["height"]), (3, 2))
        self.assertEqual(len(fact["pixel_sha256"]), 64)
        self.assertEqual(fact["pixel_sha256"], expected.image_inputs[0]["pixel_sha256"])
        detached = receipt.as_dict()
        detached["image_inputs"][0]["source_sha256"] = "edited copy"
        self.assertNotEqual(fact["source_sha256"], "edited copy")

    def test_unknown_and_corrupt_native_inputs_never_fall_through(self):
        for encoded in (b"<svg></svg>", b"\x89PNG\r\n\x1a\ninvalid", b"\xff\xd8\xffinvalid"):
            with self.subTest(encoded=encoded), self.assertRaises(ValueError):
                m.ImageMobject.from_bytes(encoded, ffmpeg_bin=self.root / "missing-ffmpeg")
        with self.assertRaisesRegex(m._CapabilityError, "BMP|bmp"):
            m.ImageMobject.from_bytes(bmp(PIXELS))
        image = m.ImageMobject(PIXELS)
        before = image.get_pixel_array()
        with self.assertRaises(m._CapabilityError):
            image.set_image(bmp(PIXELS[::-1]), ffmpeg_bin=self.root / "missing-ffmpeg")
        np.testing.assert_array_equal(image.get_pixel_array(), before)

    def test_external_pixels_and_frames_match_native_at_one_and_four_threads(self):
        executable = self.external()
        encoded = bmp(PIXELS)
        asset = self.root / "not-an-extension.asset"
        asset.write_bytes(encoded)
        tool_hash = hashlib.sha256(executable.read_bytes()).hexdigest()
        results = []
        for threads in (1, 4):
            image = m.ImageMobject(asset, height=3, ffmpeg_bin=executable)
            np.testing.assert_array_equal(image.get_pixel_array(), PIXELS)
            receipt = output(image, self.root / f"external-{threads}", threads=threads, moving=True)
            expected = output(m.ImageMobject(PIXELS, height=3), self.root / f"native-{threads}",
                              threads=threads, moving=True)
            self.assertEqual(receipt.frame_count, 6)
            rendered = [path.read_bytes() for path in sorted(receipt.destination.glob("*.png"))]
            baseline = [path.read_bytes() for path in sorted(expected.destination.glob("*.png"))]
            self.assertEqual(rendered, baseline)
            self.assertEqual(len(receipt.image_inputs), 1)
            fact = receipt.image_inputs[0]
            self.assertEqual((fact["container"], fact["decoder"]), ("bmp", "ffmpeg"))
            self.assertEqual(fact["source_sha256"], hashlib.sha256(encoded).hexdigest())
            self.assertEqual(fact["decoder_sha256"], tool_hash)
            self.assertEqual(len(receipt.ffmpeg_invocations), 1)
            invocation = receipt.ffmpeg_invocations[0]
            self.assertEqual(invocation["tool_path"], str(executable))
            self.assertEqual(invocation["tool_sha256"], tool_hash)
            self.assertIn("bmp_pipe", invocation["argv"])
            self.assertIn("-frames:v", invocation["argv"])
            results.append(rendered)
        self.assertEqual(results[0], results[1])

    def test_copies_checkpoint_restore_and_dark_textures_keep_external_origin(self):
        executable = self.external()
        source = m.ImageMobject.from_bytes(bmp(PIXELS), ffmpeg_bin=executable)
        for clone in (source.copy(), copy.deepcopy(source)):
            self.assert_certified_refusal(clone)
        self.assert_certified_refusal(source.copy(), restore=True)
        self.assert_certified_refusal(source.copy(), sequence=True)
        dark = m.TexturedSurface(m.Surface(resolution=(3, 3)), PIXELS,
                                 bmp(PIXELS[::-1]), ffmpeg_bin=executable)
        np.testing.assert_array_equal(dark.get_pixel_array(dark=True), PIXELS[::-1])
        self.assert_certified_refusal(dark.copy())
        # An unrelated still-native material does not inherit any global taint.
        native = m.Scene()
        native._begin_native_output(str(self.root / "certified-native.png"), "png", 96, 64, 8, 1, 0, True)
        try:
            native.add(m.ImageMobject.from_bytes(png(PIXELS), ffmpeg_bin=self.root / "absent"))
            native._finish_render()
        finally:
            native._abort_render()
        self.assertEqual(native._render_image_inputs[0]["decoder"], "native")

    def test_certified_decode_refuses_before_resolving_configured_tool(self):
        scene = m.Scene()
        destination = self.root / "never-published.png"
        scene._begin_native_output(str(destination), "png", 96, 64, 8, 1, 0, True)
        try:
            with self.assertRaisesRegex(m._CapabilityError, "BMP|bmp") as caught:
                m.ImageMobject.from_bytes(bmp(PIXELS), ffmpeg_bin=self.root / "missing-ffmpeg")
            self.assertNotIn("missing-ffmpeg", str(caught.exception))
        finally:
            scene._abort_render()
        self.assertFalse(destination.exists())

    def test_equal_pixels_do_not_erase_replacement_or_constant_transition_origin(self):
        executable = self.external()
        encoded = bmp(PIXELS)
        replaced = m.ImageMobject(PIXELS)
        replaced.set_image(encoded, ffmpeg_bin=executable)
        self.assert_certified_refusal(replaced)

        native = m.ImageMobject(PIXELS)
        external = m.ImageMobject.from_bytes(encoded, ffmpeg_bin=executable)
        plan = m._RasterTransition(native, m._read_raster_image(external))
        constant = m.ImageMobject(plan.sample(.5))
        np.testing.assert_array_equal(constant.get_pixel_array(), PIXELS)
        self.assert_certified_refusal(constant)
        # The union belongs to the intermediate material, not the original
        # native allocation. Sampling must never globally taint that source.
        scene = m.Scene()
        scene._begin_native_output(str(self.root / "still-native.png"), "png", 96, 64, 8, 1, 0, True)
        try:
            scene.add(native)
            scene._finish_render()
        finally:
            scene._abort_render()
        self.assertEqual(scene._render_image_inputs[0]["decoder"], "embedded-rgba8")

    def test_equal_pixel_history_and_transform_keep_current_source(self):
        executable = self.external()
        encoded = bmp(PIXELS)
        scene = m.Scene()
        image, ignored = m.ImageMobject(PIXELS), m.Square()
        scene.add(image, ignored)
        previous = m.SceneState(scene, ignore=[ignored])
        scene.undo_stack.append(previous)
        image.set_image(encoded, ffmpeg_bin=executable)
        current = m.SceneState(scene, ignore=[ignored])
        self.assertIsNone(current._checkpoint)
        self.assertTrue(image.looks_identical(previous.mobjects_to_copies[image]))
        self.assertIsNot(current.mobjects_to_copies[image], previous.mobjects_to_copies[image])
        image.set_pixel_array(np.zeros_like(PIXELS))
        current.restore_scene(scene)
        np.testing.assert_array_equal(image.get_pixel_array(), PIXELS)
        self.assert_certified_refusal(m.ImageMobject(m._read_raster_image(image)))

        scene = m.Scene()
        start = m.ImageMobject(PIXELS)
        end = m.ImageMobject.from_bytes(encoded, ffmpeg_bin=executable)
        scene.add(start)
        scene.play(m.Transform(start, end), run_time=.25, rate_func=m.linear)
        self.assert_certified_refusal(m.ImageMobject(m._read_raster_image(start)))

    def test_intermediate_material_retains_both_endpoints_and_animate_capability(self):
        executable = self.external()
        encoded = bmp(PIXELS)
        source = m.ImageMobject(np.full_like(PIXELS, 128))
        end = m.ImageMobject.from_bytes(encoded, ffmpeg_bin=executable)
        transition = m._RasterTransition(source, m._read_raster_image(end))
        intermediate = m.ImageMobject(transition.sample(.5))
        self.assert_certified_refusal(intermediate.copy())
        receipt = output(intermediate, self.root / "intermediate.png")
        self.assertEqual({item["decoder"] for item in receipt.image_inputs}, {"embedded-rgba8", "ffmpeg"})
        self.assertEqual(len(receipt.ffmpeg_invocations), 1)
        scene = m.Scene()
        animated = m.ImageMobject(np.full_like(PIXELS, 128))
        with scene.render_session(self.root / "transition", format="png_sequence",
                                  resolution=(96, 64), fps=8, threads=4) as session:
            scene.add(animated)
            scene.play(animated.animate.set_image(encoded, ffmpeg_bin=executable),
                       run_time=.5, rate_func=m.linear)
        np.testing.assert_array_equal(animated.get_pixel_array(), PIXELS)
        self.assertEqual(len(session.result.ffmpeg_invocations), 1)
        self.assertTrue(any(item["decoder"] == "ffmpeg" for item in session.result.image_inputs))

    def test_durable_pixels_are_labeled_as_explicit_embedded_input(self):
        executable = self.external()
        original = m.ImageMobject.from_bytes(bmp(PIXELS), ffmpeg_bin=executable)
        baked = pickle.loads(pickle.dumps(original))
        np.testing.assert_array_equal(baked.get_pixel_array(), PIXELS)
        receipt = output(baked, self.root / "embedded.png")
        self.assertEqual(receipt.ffmpeg_invocations, ())
        self.assertEqual(len(receipt.image_inputs), 1)
        self.assertEqual(receipt.image_inputs[0]["decoder"], "embedded-rgba8")
        self.assertEqual(receipt.image_inputs[0]["container"], "rgba8")
        self.assertIsNone(receipt.image_inputs[0]["path"])


if globals().get("_FMN_IMAGE_NATIVE_GAUNTLET", False):
    # The per-commit Gauntlet is independent of an optional host executable.
    # The complete unit acceptance still exercises real ffmpeg when available,
    # and requires it when FMN_REQUIRE_FFMPEG is set.
    suite = unittest.TestSuite(ImageInputTests(name) for name in (
        "test_native_png_ignores_missing_tool_and_reports_exact_bytes",
        "test_unknown_and_corrupt_native_inputs_never_fall_through",
        "test_certified_decode_refuses_before_resolving_configured_tool",
    ))
else:
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(ImageInputTests)
result = unittest.TextTestRunner(verbosity=2).run(suite)
if not result.wasSuccessful():
    raise AssertionError("governed native image-input acceptance failed")
