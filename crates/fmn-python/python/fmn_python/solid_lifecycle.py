"""Solid constructor hooks over Surface's native sampling/publication lifecycle.

The stock UV function selects Atlas's specialized solid builder, not a Python
sampler. An authored UV function instead uses Surface's bounded callback path.
In either case only pointlike columns are published into the actual subclass's
records, so custom schemas, children, and cooperative hooks keep their owners.
"""
from __future__ import annotations

import math

from .surface_admission import _finite


def install_solid_lifecycle(native):
    g = vars(native)
    if g.get("_FMN_SOLID_LIFECYCLE_INSTALLED", False):
        return
    if not g.get("_FMN_SURFACE_LIFECYCLE_INSTALLED", False):
        raise ImportError("solid construction requires the shared Surface lifecycle")
    Surface, Torus = g["Surface"], g["Torus"]
    # Capture identities now. Later class/instance monkey patches are authored
    # functions, even if they retain a built-in method's name or signature.
    torus_uv = Torus.uv_func
    constructing = set()

    def dimensions(self):
        return tuple(_finite(getattr(self, key), "surface " + key, record=True)
                     for key in ("r1", "r2"))

    def torus(self, u_range=(0, math.tau), v_range=(0, math.tau),
              r1=3.0, r2=1.0, **kwargs):
        if self._is_bound():
            raise RuntimeError("a surface constructor requires a detached target; use init_points or become")
        if id(self) in constructing:
            raise RuntimeError("solid initialization is already in progress")
        radii = tuple(_finite(value, "surface " + key, record=True)
                      for key, value in (("r1", r1), ("r2", r2)))
        constructing.add(id(self))
        try:
            self.r1, self.r2 = radii
            super(Torus, self).__init__(u_range=u_range, v_range=v_range, **kwargs)
            self._solid_params = ("torus", self.r1, self.r2)
            self._solid_native_height = self.get_height()
        finally:
            constructing.discard(id(self))

    def build_candidate(self, options, sample):
        if not isinstance(self, Torus):
            return sample()
        radii = dimensions(self)
        if getattr(self.uv_func, "__func__", None) is torus_uv:
            candidate = Surface.__new__(Surface)
            g["_install_live_state"](candidate)
            specs = candidate._build_torus(
                g["_native_surface_shell_factory"], *radii,
                options["u_range"], options["v_range"], options["resolution"],
                options["preferred_creation_axis"], options["epsilon"],
                options["normal_nudge"], 0)
            if specs:
                raise RuntimeError("a solid sampler returned unexpected children")
        else:
            candidate = sample()
        if dimensions(self) != radii:
            raise RuntimeError("solid shape parameters changed during sampling; geometry was not published")
        return candidate

    torus.__name__, torus.__qualname__, torus.__module__ = (
        "__init__", Torus.__qualname__ + ".__init__", Torus.__module__)
    Torus.__init__ = torus
    g["_fmn_build_solid_candidate"] = build_candidate
    g["_FMN_SOLID_LIFECYCLE_INSTALLED"] = True
