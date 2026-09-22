"""In-memory images and live pixels over immutable native raster resources.

Array conversion is an explicit input boundary, not an image codec or renderer.
PNG/JPEG decoding, texture storage, revisions, copies and snapshots stay native.
Input arrays and returned arrays never become writable aliases of live pixels.
"""
from __future__ import annotations

from contextlib import contextmanager
import math
import os

_MAX_PIXELS = 16_777_216
_MAX_ENCODED_BYTES = 64 * 1024 * 1024
_BUSY = "_fmn_image_authoring_busy"


def _pixels(np, pixels):
    array = np.asarray(pixels)
    if array.dtype != np.uint8:
        raise TypeError("pixels must be uint8 samples; normalize other data explicitly")
    if array.ndim == 2:
        array = array[..., None]
    if array.ndim != 3 or array.shape[2] not in (1, 2, 3, 4):
        raise ValueError("pixels must be (H, W), (H, W, 1/2/3/4) grayscale/gray-alpha/RGB/RGBA")
    height, width, channels = array.shape
    if width < 1 or height < 1 or width * height > _MAX_PIXELS:
        raise ValueError("pixel dimensions are empty or exceed the 16M-pixel budget")
    # Copy before calling native code: strided/reversed/transposed arrays and
    # shared caller storage all have the same top-row-first immutable meaning.
    if channels == 4:
        rgba = array.tobytes(order="C")
    else:
        output = np.empty((height, width, 4), dtype=np.uint8)
        output[..., :3] = array[..., :1] if channels < 3 else array
        output[..., 3] = array[..., 1] if channels == 2 else 255
        rgba = output.tobytes(order="C")
    return width, height, rgba


def _encoded(value):
    size = value.nbytes if isinstance(value, memoryview) else len(value)
    if size > _MAX_ENCODED_BYTES:
        raise ValueError("raster input exceeds the 64 MiB encoded budget")
    return bytes(value)


def _source(g, value):
    """Prepare one raster and informational path without touching a target."""
    if isinstance(value, g["_RasterImage"]):
        return value, None
    if isinstance(value, (str, os.PathLike)):
        path = g["_resolve_raster_image_path"](value)
        with path.open("rb") as stream:
            encoded = stream.read(_MAX_ENCODED_BYTES + 1)
        return g["_RasterImage"].decode(_encoded(encoded)), str(path)
    if isinstance(value, (bytes, bytearray, memoryview)):
        return g["_RasterImage"].decode(_encoded(value)), None
    return g["_RasterImage"](*_pixels(g["_np"], value)), None


def _method(cls, name, function):
    function.__name__ = name
    function.__qualname__ = cls.__qualname__ + "." + name
    function.__module__ = cls.__module__
    setattr(cls, name, function)


@contextmanager
def _editing(target):
    attrs = vars(target)
    if attrs.get(_BUSY, False):
        raise RuntimeError("image authoring cannot reenter the same mobject")
    attrs[_BUSY] = True
    try:
        yield
    finally:
        attrs.pop(_BUSY, None)


def install_image_authoring(native):
    """Keep the original ImageMobject class and qualified import identities."""
    g = vars(native)
    if g.get("_FMN_IMAGE_AUTHORING_INSTALLED", False):
        return
    Image, np = g["ImageMobject"], g["_np"]

    def initialize(self, filename, height=4.0, **kwargs):
        if self._is_bound():
            raise RuntimeError("an image constructor requires a detached target; use set_image()")
        with _editing(self):
            opacity = float(kwargs.pop("opacity", 1.0))
            z_index = int(kwargs.pop("z_index", 0))
            fixed = bool(kwargs.pop("is_fixed_in_frame", False))
            depth = bool(kwargs.pop("depth_test", False))
            color = kwargs.pop("color", None)
            if kwargs:
                raise TypeError("ImageMobject() got unexpected keyword arguments: " + ", ".join(sorted(kwargs)))
            height = float(height)
            if (not math.isfinite(height) or height <= 0 or height > np.finfo(np.float32).max
                    or not math.isfinite(opacity) or abs(opacity) > np.finfo(np.float32).max):
                raise ValueError("image height/opacity must be finite and f32-representable; height must be positive")
            if not -(1 << 31) <= z_index < (1 << 31):
                raise ValueError("image z_index must fit a signed 32-bit integer")
            raster, path = _source(g, filename)
            width, pixel_height = raster.size
            if height * width / pixel_height > np.finfo(np.float32).max:
                raise ValueError("image aspect ratio produces an unrepresentable scene width")
            # Every conversion and decode has succeeded before installation.
            g["_install_live_state"](self)
            specs = g["_build_raster_image"](self, raster, g["_native_shell_factory"], height, opacity, z_index)
            g["_hang_native_children"](self, specs)
            vars(self).update(height=height, opacity=opacity, image_path=path,
                              pixel_width=width, pixel_height=pixel_height)
            if fixed:
                self.fix_in_frame()
            if depth:
                self.apply_depth_test()
            if color is not None:
                self.set_color(color)

    def from_pixel_array(cls, pixels, height=4.0, **kwargs):
        """Construct from uint8 grayscale, gray-alpha, RGB or RGBA samples."""
        raster = g["_RasterImage"](*_pixels(np, pixels))
        return cls(raster, height=height, **kwargs)

    def from_bytes(cls, encoded, height=4.0, **kwargs):
        """Construct from bounded native PNG/JPEG bytes, without filesystem I/O."""
        if not isinstance(encoded, (bytes, bytearray, memoryview)):
            raise TypeError("encoded image must be PNG/JPEG bytes")
        return cls(g["_RasterImage"].decode(_encoded(encoded)), height=height, **kwargs)

    def set_image(self, image):
        """Replace raster content, retaining geometry, UVs, opacity and identity.

        Pixel dimensions may change without resizing or reorienting the scene
        quad. Existing copies, camera captures and checkpoints retain old pixels.
        A failed preparation does not replace the current native resource.
        """
        with _editing(self):
            raster, path = _source(g, image)
            g["_replace_raster_image"](self, raster)
            width, height = raster.size
            vars(self).update(image_path=path, pixel_width=width, pixel_height=height)
        return self

    def set_pixel_array(self, pixels):
        """Publish an owned uint8 image; the supplied array is never retained."""
        with _editing(self):
            raster = g["_RasterImage"](*_pixels(np, pixels))
            g["_replace_raster_image"](self, raster)
            width, height = raster.size
            vars(self).update(image_path=None, pixel_width=width, pixel_height=height)
        return self

    def get_pixel_array(self):
        """Return a writable, detached RGBA8 copy of the native image pixels."""
        raster = g["_read_raster_image"](self)
        width, height = raster.size
        return np.frombuffer(raster.pixels(), dtype=np.uint8).reshape(height, width, 4).copy()

    for name, function in (("__init__", initialize), ("set_image", set_image),
                           ("set_pixel_array", set_pixel_array), ("get_pixel_array", get_pixel_array)):
        _method(Image, name, function)
    for name, function in (("from_bytes", from_bytes), ("from_pixel_array", from_pixel_array)):
        function.__name__ = name
        function.__qualname__ = Image.__qualname__ + "." + name
        function.__module__ = Image.__module__
        setattr(Image, name, classmethod(function))
    g["_FMN_IMAGE_AUTHORING_INSTALLED"] = True
