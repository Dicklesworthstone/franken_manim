"""Synchronize callback interpolation with Marionette's typed tracker state.

The native Transform path already interpolates trackers. The Python Transform
path interpolates records and uniform mirrors instead; bridge that operation
to the existing scalar/complex setters without changing scene timing.
"""
from __future__ import annotations

from functools import wraps
from typing import Any


def install_tracker_interpolation(native: Any) -> None:
    """Keep tracker-driven geometry live on the existing callback path."""
    g = vars(native)
    if g.get("_FMN_TRACKER_INTERPOLATION_INSTALLED", False):
        return
    Mobject, ValueTracker, np = g["Mobject"], g["ValueTracker"], g["_np"]
    original = Mobject.interpolate

    def read(tracker, kind):
        if not isinstance(tracker, ValueTracker) or tracker._tracker_kind != kind:
            raise TypeError("Tracker interpolation requires matching tracker encodings")
        if kind == 2:
            real, imaginary = tracker._tracker_complex_value()
            return complex(real, imaginary)
        return tracker._tracker_value()

    def interpolate_value(start, end, alpha, kind):
        if kind == 1 and not (np.isfinite(start) and np.isfinite(end) and start > 0 and end > 0):
            raise ValueError(
                "Exponential callback interpolation requires positive finite decoded endpoints; "
                "use native Transform for nonrepresentable encoded tracker states"
            )
        # Endpoint selection avoids 0 * infinity and preserves scalar/complex
        # values without passing them through NumPy's float32 record plane.
        if alpha == 0.0:
            return start
        if alpha == 1.0:
            return end
        if kind != 1:
            return (1.0 - alpha) * start + alpha * end
        # Exponential trackers interpolate logarithmically. Work in the log
        # domain rather than multiplying powers (which can under/overflow
        # even when the result is representable). Existing decoded accessors
        # are the portal boundary; this is standard-mode f64, not a claim of
        # bit identity with Choreo's encoded-lane/dmath implementation.
        if start == end:
            return start
        with np.errstate(divide="ignore", invalid="ignore", over="ignore", under="ignore"):
            value = float(np.exp((1.0 - alpha) * np.log(start) + alpha * np.log(end)))
        if not np.isfinite(value) or value <= 0:
            raise ValueError(
                "Exponential callback interpolation produced a nonrepresentable decoded value; "
                "use native Transform for nonrepresentable encoded tracker states"
            )
        return value

    @wraps(original)
    def interpolate(self, mobject1, mobject2, alpha, path_func=None):
        if not isinstance(self, ValueTracker) or "value" in getattr(self, "locked_uniform_keys", ()):
            return original(self, mobject1, mobject2, alpha, path_func)
        kind = self._tracker_kind
        if kind not in (0, 1, 2):
            raise ValueError("Unknown native tracker encoding")
        alpha = float(alpha)
        # Read both endpoints before writing anything: self may also be one
        # endpoint, and the mirrors can be stale after a native animation.
        value = interpolate_value(read(mobject1, kind), read(mobject2, kind), alpha, kind)
        result = original(self, mobject1, mobject2, alpha, path_func)
        # Interpolation must not call ControlMobject.set_value: that method
        # rebuilds controls and has user-facing discrete validation. Choreo
        # likewise interpolates the stored state, not the authored setter.
        if kind == 2:
            self._set_tracker_complex_value(value.real, value.imag)
        else:
            self._set_tracker_value(float(value))
        self._refresh_value_uniform()
        return result

    Mobject.interpolate = interpolate
    g["_FMN_TRACKER_INTERPOLATION_INSTALLED"] = True
