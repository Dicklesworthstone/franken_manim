"""Speed ramps over Choreo's existing child-animation driver.

A profile maps presentation alpha to child-segment alpha. It does not emit
frames, accumulate a second clock, replace a child's interpolation, or sample
an authored callable in advance. Child easing, lag, helper updates, native
geometry, and cleanup remain owned by the shared AnimationGroup driver.

ChangeSpeed is an extension surface, not a class in the pinned 3b1b Reference.
Its speed knots describe child *timeline* progress. An explicit ``rate_func``
overrides child easing for this execution; it is not applied a second time to
the wrapper. The input animation and speed mapping can be reused afterwards.
"""
from __future__ import annotations

from bisect import bisect_right
from collections.abc import Mapping
from fractions import Fraction
import inspect
import math
from numbers import Real
from typing import Any


_MAX_KNOTS = 4096
_MAX_FRAME_SCALE_BITS = 16384
_MISSING = object()


def _number(value, name):
    if isinstance(value, bool) or not isinstance(value, Real):
        raise TypeError(name + " must be a real number")
    value = float(value)
    if not math.isfinite(value):
        raise ValueError(name + " must be finite")
    return value


class _SpeedProfile:
    """Piecewise constant-acceleration clock map with exact knot endpoints.

    A child interval of length d with endpoint speeds u and v takes 2d/(u+v)
    presentation time. Computing its normalized quadratic avoids squaring
    large speeds, numerical integration, and dependence on a frame rate.
    """
    def __init__(self, speedinfo=None):
        if speedinfo is None:
            speedinfo = {}
        if not isinstance(speedinfo, Mapping):
            raise TypeError("ChangeSpeed speedinfo must be a mapping")
        if len(speedinfo) > _MAX_KNOTS:
            raise ValueError("ChangeSpeed speedinfo exceeds the 4096-knot budget")
        nodes = {}
        for position, speed in speedinfo.items():
            position = _number(position, "speedinfo position")
            speed = _number(speed, "speedinfo speed")
            if not 0.0 <= position <= 1.0:
                raise ValueError("speedinfo positions must be in [0, 1]")
            if speed < 0.0:
                raise ValueError("speedinfo speeds must be nonnegative")
            if position in nodes:
                raise ValueError("speedinfo positions must remain distinct as floats")
            nodes[position] = speed
        nodes.setdefault(0.0, 1.0)
        nodes.setdefault(1.0, nodes[max(nodes)])
        self.nodes = tuple(sorted(nodes.items()))
        self.segments = []
        self.ends = []
        elapsed = 0.0
        for (left, initial), (right, final) in zip(self.nodes, self.nodes[1:]):
            scale = max(initial, final)
            if scale == 0.0:
                raise ValueError("a positive-width speed interval cannot have two zero speeds")
            # Scale before addition: finite endpoint speeds may sum to infinity.
            a, b = initial / scale, final / scale
            duration = ((right - left) / scale) * (2.0 / (a + b))
            end = elapsed + duration
            if not math.isfinite(end) or duration <= 0.0 or end <= elapsed:
                raise ValueError("speedinfo produces an unrepresentable finite duration")
            self.segments.append((left, right, elapsed, duration, a / (a + b)))
            self.ends.append(end)
            elapsed = end
        self.total_time = elapsed
        self._frame_scale = None
        self._constant_speed = (self.nodes[0][1] > 0.0
                                and all(speed == self.nodes[0][1] for _, speed in self.nodes))

    def frame_scale(self):
        """Return an exact dyadic scale or require an explicit duration.

        The ordinary seconds path retains its established floating-point
        arithmetic. Frame composition cannot round a reciprocal first:
        five frames at speed five would otherwise become ceil(5 * 0.2)=2.
        Sum the authored binary64 knots as rationals before checking whether
        Choreo's dyadic frame arithmetic can represent the resulting scale.
        """
        if self._frame_scale is not None:
            return self._frame_scale
        total = Fraction(0)
        for (left, initial), (right, final) in zip(self.nodes, self.nodes[1:]):
            width = Fraction.from_float(right) - Fraction.from_float(left)
            speeds = Fraction.from_float(initial) + Fraction.from_float(final)
            total += 2 * width / speeds
            if max(total.numerator.bit_length(), total.denominator.bit_length()) > _MAX_FRAME_SCALE_BITS:
                raise ValueError("ChangeSpeed exact frame scale exceeds its precision budget; "
                                 "specify run_time_frames or run_time")
        try:
            factor = float(total)
        except OverflowError as error:
            raise ValueError("ChangeSpeed exact frame scale is not finite; "
                             "specify run_time_frames or run_time") from error
        if not math.isfinite(factor) or Fraction.from_float(factor) != total:
            raise ValueError("ChangeSpeed profile has no exactly representable frame scale; "
                             "specify run_time_frames or run_time")
        self._frame_scale = factor
        return factor

    def map(self, alpha):
        alpha = _number(alpha, "ChangeSpeed alpha")
        if alpha <= 0.0:
            return 0.0
        if alpha >= 1.0:
            return 1.0
        elapsed = alpha * self.total_time
        index = min(bisect_right(self.ends, elapsed), len(self.segments) - 1)
        left, right, start, duration, initial_share = self.segments[index]
        t = min(1.0, max(0.0, (elapsed - start) / duration))
        progress = t * (2.0 * initial_share * (1.0 - t) + t)
        return min(right, max(left, left + (right - left) * progress))


class _ClockCurve:
    """Evaluate an outer easing once at an actual shared-driver sample."""
    def __init__(self, profile, easing):
        self.profile, self.easing = profile, easing
        self.last_value = None

    def __call__(self, alpha):
        value = self.profile.map(self.easing(alpha))
        self.last_value = value
        return value


def _rate(g, value):
    if value is None:
        return g["_linear_rate"]
    if isinstance(value, str):
        for function, name in g["_RATE_FUNC_NAMES"].items():
            if name == value:
                return function
        raise ValueError("unknown ChangeSpeed rate function: " + value)
    if not callable(value):
        raise TypeError("ChangeSpeed rate_func must be callable or a catalog name")
    return value


def install_speed(native: Any) -> None:
    """Retain the published class; execute it as a one-child composition."""
    g = vars(native)
    if g.get("_FMN_SPEED_INSTALLED", False):
        return
    import sys
    speed_mod = sys.modules.get("manimlib.animation.speed")
    ChangeSpeed = g.get("ChangeSpeed") or (getattr(speed_mod, "ChangeSpeed", None) if speed_mod else None)
    if ChangeSpeed is None:
        raise KeyError("ChangeSpeed")
    Group = g["AnimationGroup"]
    linear_rate = g["_linear_rate"]
    clock_call = _ClockCurve.__call__
    profile_map = _SpeedProfile.map

    def frame_identity_rate(rate):
        # A constant speed changes duration but its normalized clock map is
        # identity. Preserve exact frame positions only for the shipped map
        # and captured catalog easing, never an authored call/map override.
        # Easing can change during playback, so inspect these identities at
        # the actual sample; the constructor cached the O(knots) test.
        if (type(rate) is not _ClockCurve or vars(_ClockCurve).get("__call__") is not clock_call
                or type(rate.profile) is not _SpeedProfile or rate.easing is not linear_rate
                or not rate.profile._constant_speed):
            return False
        return "map" not in vars(rate.profile) and vars(_SpeedProfile).get("map") is profile_map

    g["_fmn_frame_identity_rate"] = frame_identity_rate

    def restore(self):
        previous = self.__dict__.pop("_speed_previous_rate", _MISSING)
        if previous is not _MISSING:
            kind, value = previous
            if kind == "descriptor":
                self.anim.rate_func = value
            elif kind == "attribute":
                self.anim.__dict__["rate_func"] = value
            else:
                self.anim.__dict__.pop("rate_func", None)

    def cancel_preserving(self, error):
        try:
            self.abort()
        except BaseException as cleanup:
            try:
                BaseException.add_note(error, "ChangeSpeed cleanup also failed: " + type(cleanup).__name__)
            except BaseException:
                pass

    def speed_init(self, anim=None, speedinfo=None, rate_func=None, **kwargs):
        if anim is None:
            # Keep the historical named refusal for an absent child, without
            # claiming that a valid child's native execution seam is absent.
            raise NotImplementedError("ChangeSpeed requires a child Animation for its clock-remap seam")
        profile = _SpeedProfile(speedinfo)
        self.speedinfo = dict(profile.nodes)
        self._speed_curve = _ClockCurve(profile, g["_linear_rate"])
        self._speed_child_rate = None if rate_func is None else _rate(g, rate_func)
        requested = kwargs.pop("run_time", None)
        requested_frames = kwargs.get("run_time_frames")
        if requested is not None:
            if requested_frames is not None:
                raise ValueError("run_time and run_time_frames are mutually exclusive")
            requested = _number(requested, "ChangeSpeed run_time")
            if requested < 0.0:
                raise ValueError("ChangeSpeed run_time must be nonnegative")
        # The shared constructor prepares .animate builders exactly once and
        # computes native child intervals, including nested groups/time spans.
        super(ChangeSpeed, self).__init__(
            anim, run_time=-1 if requested is None else requested, rate_func=None, **kwargs)
        self.anim = self.animations[0]
        duration = float(g["_composition_member_run_time"](self.anim))
        if not math.isfinite(duration) or duration < 0.0:
            raise ValueError("ChangeSpeed child run_time must be finite and nonnegative")
        self._speed_child_duration = duration
        self._speed_child_frame_bound = False
        if requested_frames is None:
            self.run_time = duration * profile.total_time if requested is None else requested
            if not math.isfinite(self.run_time) or (duration > 0.0 and requested is None and self.run_time == 0.0):
                raise ValueError("ChangeSpeed produces an unrepresentable finite run_time")
        self._speed_phase = "new"

    def get_scaled_total_time(self):
        return self._speed_curve.profile.total_time

    def frame_duration_scale(self, frame_children):
        profile = self._speed_curve.profile
        return profile.frame_scale() if frame_children else profile.total_time

    def begin(self):
        if getattr(self, "_composition_driver", None) is not None:
            self.abort()
        self._speed_phase = "active"
        self._speed_curve.last_value = None
        try:
            # Frame children only acquire their seconds projection on the
            # owning Scene's grid. Speed-aware updaters must use that bound
            # duration rather than the constructor's temporary default.
            timing = getattr(self.anim, "_fmn_frame_timing", None)
            if timing is not None:
                self._speed_child_duration = timing.seconds
                self._speed_child_frame_bound = True
            elif self._speed_child_frame_bound:
                duration = float(g["_composition_member_run_time"](self.anim))
                if not math.isfinite(duration) or duration < 0.0:
                    raise ValueError("ChangeSpeed child run_time must be finite and nonnegative")
                self._speed_child_duration = duration
                self._speed_child_frame_bound = False
            if self._speed_child_rate is not None:
                attrs = self.anim.__dict__
                descriptor = inspect.getattr_static(type(self.anim), "rate_func", None)
                if isinstance(self.anim, ChangeSpeed):
                    # The nested wrapper's public rate is a stable composite;
                    # preserve its inner easing, not that self-referential object.
                    previous = ("descriptor", self.anim._speed_curve.easing)
                elif hasattr(descriptor, "__set__"):
                    previous = ("descriptor", self.anim.rate_func)
                else:
                    previous = ("attribute" if "rate_func" in attrs else "absent",
                                attrs.get("rate_func"))
                self._speed_previous_rate = previous
                self.anim.rate_func = self._speed_child_rate
            return super(ChangeSpeed, self).begin()
        except BaseException as error:
            cancel_preserving(self, error)
            raise

    def update_mobjects(self, dt):
        try:
            return super(ChangeSpeed, self).update_mobjects(dt)
        except BaseException as error:
            cancel_preserving(self, error)
            raise

    def interpolate(self, alpha):
        try:
            return super(ChangeSpeed, self).interpolate(alpha)
        except BaseException as error:
            cancel_preserving(self, error)
            raise

    def finish(self):
        if self._speed_phase in ("finished", "cleaned"):
            return
        if self._speed_phase != "active":
            raise RuntimeError("ChangeSpeed must begin before finish")
        try:
            super(ChangeSpeed, self).finish()
            self._speed_phase = "finished"
        except BaseException as error:
            cancel_preserving(self, error)
            raise
        finally:
            restore(self)

    def abort(self):
        try:
            return super(ChangeSpeed, self).abort()
        finally:
            restore(self)
            self._speed_phase = "aborted"

    def clean_up_from_scene(self, scene):
        if self._speed_phase == "cleaned":
            return
        if self._speed_phase != "finished":
            raise RuntimeError("ChangeSpeed must finish before scene cleanup")
        self._speed_phase = "cleaned"
        try:
            return super(ChangeSpeed, self).clean_up_from_scene(scene)
        finally:
            self._composition_driver = None
            restore(self)

    def get_rate(self):
        return self._speed_curve

    def set_rate(self, value):
        # Scene.play can override a group's easing by attribute assignment.
        # Keep the speed map instead of silently replacing it with that curve.
        # Assigning the composite back to itself is a no-op: update_rate_info
        # does `self.rate_func = rate_func or self.rate_func`, and storing the
        # curve as its own easing recursed forever.
        if value is self._speed_curve:
            return
        self._speed_curve.easing = _rate(g, value)

    # Existing qualified imports retain their identity. Making the wrapper a
    # real composition also exposes its child to ownership checks, camera
    # lowering, failure snapshots, and nested-group traversal already shipped.
    ChangeSpeed.__bases__ = (Group,)
    ChangeSpeed._native_kind = None
    ChangeSpeed.__doc__ = "Retime a child animation on the shared native composition clock."
    methods = {
        "__init__": speed_init, "get_scaled_total_time": get_scaled_total_time,
        "_fmn_duration_scale": frame_duration_scale,
        "begin": begin, "update_mobjects": update_mobjects, "interpolate": interpolate,
        "finish": finish, "abort": abort, "clean_up_from_scene": clean_up_from_scene,
    }
    for name, function in methods.items():
        function.__name__ = name
        function.__qualname__ = ChangeSpeed.__qualname__ + "." + name
        function.__module__ = ChangeSpeed.__module__
        setattr(ChangeSpeed, name, function)
    ChangeSpeed.rate_func = property(get_rate, set_rate)
    g["_FMN_SPEED_INSTALLED"] = True
