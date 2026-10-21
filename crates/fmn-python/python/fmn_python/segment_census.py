"""Segment census for the native-offload payoff (fm-offload-payoff-4rve).

When FMN_SEGMENT_CENSUS names a file, every engine segment the portal runs (one
play() or wait() reaching Scene._play_animations or Scene._wait) appends one
NDJSON record to it, schema `fmn.segment-census` version 1. The census only
observes: it changes no argument or result, and it reads only what the portal
itself consults to route a segment.

A segment's class says what native segment offload (fm-5wq.51) could do with it:

- `native`: every animation lowers to a native mechanism, and nothing authored
  runs per frame (no Python animation callback, no updater, no authored scene
  or mobject hook, no stop condition). Offload can run it in Choreo + Lumen.
- `rate-table`: as native, except for a custom Python rate function. Either the
  portal already samples it on the frame grid before the segment, or it is the
  only reason a shipped, natively lowerable animation takes the Python
  lifecycle (live_rates routes authored easing there). Offload could tabulate
  it the same way.
- `python`: anything else; `reasons` names each cause. An updater defined in
  the library itself (for example InteractiveScene's selection highlight) is
  `library-updaters:N`, apart from authored `updaters:N`: it runs Python per
  frame today, but offload could implement it natively.

Each record also carries the segment's wall time, its frames (calls of the
per-frame public-update dispatch) and the bridge's own crossing counters over
the segment (manimlib._crossing_stats_snapshot): crossings by class and the
time inside Python callbacks against the native advance.
"""
from __future__ import annotations

import json
import os
from time import perf_counter
from typing import Any, Iterable

SCHEMA = "fmn.segment-census"
VERSION = 1


def classify(python_animations: Iterable[str], updaters: int, authored_hook: bool,
             stop_condition: bool, rate_tables: bool,
             library_updaters: int = 0) -> tuple[str, list[str]]:
    """A segment's class and the reasons that demote it, in a stable order."""
    reasons = [f"python-animation:{name}" for name in sorted(set(python_animations))]
    if updaters:
        reasons.append(f"updaters:{updaters}")
    if library_updaters:
        reasons.append(f"library-updaters:{library_updaters}")
    if authored_hook:
        reasons.append("authored-hook")
    if stop_condition:
        reasons.append("stop-condition")
    if reasons:
        return "python", reasons
    return ("rate-table" if rate_tables else "native"), []


def _has_rate_table(specs: Iterable[Any]) -> bool:
    """Whether a spec (or a composition member) carries a pre-sampled rate table."""
    for spec in specs:
        if not isinstance(spec, tuple) or len(spec) < 7:
            continue
        if isinstance(spec[4], list):
            return True
        members = spec[6].get("members") if isinstance(spec[6], dict) else None
        if members and _has_rate_table(members):
            return True
    return False


def _library(function: Any) -> bool:
    """Whether an updater is defined in the library rather than authored."""
    target = getattr(function, "__func__", function)
    return (getattr(target, "__module__", None) or "").split(".")[0] in ("manimlib", "fmn_python")


def _updater_counts(scene: Any) -> tuple[int, int]:
    """Authored and library updaters on the scene and its members' families, read from the
    same `updaters` lists the bridge's has_python_updaters reads."""
    counts = [0, 0]

    def tally(functions):
        for function in functions or ():
            counts[1 if _library(function) else 0] += 1

    tally(getattr(scene, "updaters", None))
    seen = set()
    for root in list(scene.mobjects):
        for member in root.get_family():
            if id(member) not in seen:
                seen.add(id(member))
                tally(getattr(member, "updaters", None))
    return counts[0], counts[1]


def install_segment_census(native: Any) -> None:
    """Record every engine segment when FMN_SEGMENT_CENSUS names a file; otherwise do nothing."""
    path = os.environ.get("FMN_SEGMENT_CENSUS")
    g = vars(native)
    if not path or g.get("_FMN_SEGMENT_CENSUS_INSTALLED", False):
        return
    Scene = g["Scene"]
    snapshot = g["_crossing_stats_snapshot"]
    play_animations, wait, dispatch = (
        Scene._play_animations, Scene._wait, Scene._fmn_dispatch_public_scene_update)
    active: dict[int, dict] = {}
    indices: dict[int, int] = {}

    def measured(self, kind, python_animations, rate_tables, stop_condition, run_time, call):
        updaters, library_updaters = _updater_counts(self)
        authored = bool(self._fmn_requires_public_scene_update())
        segment = {"frames": 0}
        active[id(self)] = segment
        before, start = snapshot(), perf_counter()
        try:
            return call()
        finally:
            wall = perf_counter() - start
            after = snapshot()
            active.pop(id(self), None)
            index = indices.get(id(self), 0)
            indices[id(self)] = index + 1
            klass, reasons = classify(python_animations, updaters, authored, stop_condition,
                                      rate_tables, library_updaters)
            record = {
                "schema": SCHEMA, "version": VERSION, "scene": type(self).__name__,
                "index": index, "kind": kind, "class": klass, "reasons": reasons,
                "run_time": run_time, "frames": segment["frames"], "wall_s": round(wall, 6),
                "crossings": {key: after[key] - before[key] for key in sorted(after)
                              if key not in ("python_callback_ns", "native_ns")},
                "python_callback_s": round((after["python_callback_ns"]
                                            - before["python_callback_ns"]) / 1e9, 6),
                "native_s": round((after["native_ns"] - before["native_ns"]) / 1e9, 6),
            }
            with open(path, "a", encoding="utf-8") as out:
                out.write(json.dumps(record, sort_keys=True) + "\n")

    rate_names = g["_RATE_FUNC_NAMES"]

    def rate_only(callback):
        # Exactly live_rates' custom-rate test (identity, never equality), on a
        # shipped class with a native lowering: its rate is its only Python part.
        rate = getattr(callback, "rate_func", None)
        return (type(callback).__module__.split(".")[0] in ("manimlib", "fmn_python")
                and bool(getattr(callback, "_native_kind", None))
                and callable(rate) and all(rate is not known for known in rate_names))

    def census_play_animations(self, specs, callbacks, *args):
        live = [callback for callback in callbacks if callback is not None]
        python = [type(callback).__name__ for callback in live if not rate_only(callback)]
        rate_tables = (len(python) < len(live) or _has_rate_table(specs)
                       or (len(args) > 2 and isinstance(args[2], list)))
        run_time = args[1] if len(args) > 1 else None
        return measured(self, "play", python, rate_tables, False, run_time,
                        lambda: play_animations(self, specs, callbacks, *args))

    def census_wait(self, duration, stop_condition, *args):
        return measured(self, "wait", [], False, stop_condition is not None, duration,
                        lambda: wait(self, duration, stop_condition, *args))

    def census_dispatch(self, dt):
        segment = active.get(id(self))
        if segment is not None:
            segment["frames"] += 1
        return dispatch(self, dt)

    for name, function in (("_play_animations", census_play_animations), ("_wait", census_wait),
                           ("_fmn_dispatch_public_scene_update", census_dispatch)):
        function.__name__ = name
        function.__qualname__ = Scene.__qualname__ + "." + name
        setattr(Scene, name, function)
    g["_FMN_SEGMENT_CENSUS_INSTALLED"] = True
