"""Point-cloud authoring over the engine's scalar glow uniform.

The ordinary live-uniform map owns material writes and revision invalidation.
No record lane or second interpolation path carries the material's value.
"""
from __future__ import annotations

import math


def _glow(value):
    value = float(value)
    if not math.isfinite(value) or value < 0:
        raise ValueError("glow_factor must be finite and non-negative")
    return value


def install_point_cloud_materials(native):
    g = vars(native)
    if g.get("_FMN_POINT_CLOUD_MATERIALS_INSTALLED", False):
        return
    DotCloud = g["DotCloud"]

    def set_glow_factor(self, glow_factor):
        value = _glow(glow_factor)
        self.uniforms["glow_factor"] = value
        self.glow_factor = value
        return self

    set_glow_factor.__name__ = "set_glow_factor"
    set_glow_factor.__qualname__ = DotCloud.__qualname__ + ".set_glow_factor"
    set_glow_factor.__module__ = DotCloud.__module__
    DotCloud.set_glow_factor = set_glow_factor
    g["_FMN_POINT_CLOUD_MATERIALS_INSTALLED"] = True
