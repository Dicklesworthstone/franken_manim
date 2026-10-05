"""Host cache configuration for the production native typesetting engines.

The host owns configuration; fmn-cache owns paths, checksums and publication,
fmn-tex owns layouts and source spans. No cache payload is parsed in Python.
"""
from __future__ import annotations

from copy import deepcopy
from functools import wraps
from importlib import import_module
import json
import os
from pathlib import Path
import sys
from threading import local
from typing import Any


_STATE = "_fmn_typesetting_runtime"


def _directory(value: os.PathLike[str] | str | None, enabled: bool) -> str | None:
    if not isinstance(enabled, bool):
        raise TypeError("typeset cache enabled must be bool")
    if value is None:
        return "" if enabled else None
    text = os.fspath(value)
    if not isinstance(text, str):
        raise TypeError("typeset cache directory must be a text path")
    if "\0" in text or len(text.encode("utf-8")) > 4096:
        raise ValueError("typeset cache directory must contain at most 4096 UTF-8 bytes and no NUL")
    if ".." in Path(text).parts:
        raise ValueError("typeset cache directory may not contain parent-directory components")
    # Freeze a relative path before a scene can chdir. Do not resolve symlinks:
    # fmn-cache must see and reject link-like ownership boundaries itself.
    if text:
        text = os.path.abspath(os.path.expanduser(text))
        if len(text.encode("utf-8")) > 4096:
            raise ValueError("resolved typeset cache directory exceeds 4096 UTF-8 bytes")
    return text if enabled else None


def _diagnose(report: dict[str, Any]) -> None:
    errors = sorted({row["error"] for row in report["templates"].values() if row.get("error")})
    if not errors:
        return
    # One bounded, structured diagnostic per attempted binding. A full disk or
    # foreign directory never redirects the cache and never suppresses ink.
    event = {"event": "typeset-cache-unavailable", "fallback": "memory-and-layout",
             "errors": [error[:2048] for error in errors[:3]]}
    try:
        sys.stderr.write(json.dumps(event, ensure_ascii=True, sort_keys=True) + "\n")
    except Exception:
        # Diagnostic sinks are not part of the render contract. Cancellation
        # still propagates; ordinary logging failures cannot suppress ink.
        pass


def install_typesetting(native: Any) -> None:
    """Install once per native module; each host thread has its own engines.

    Scene construction configures the persistent cache before construct().
    Direct Tex construction outside a Scene uses the same binding through the
    native option-validation seam. Importing the package alone does no I/O.
    """
    g = vars(native)
    if _STATE in g:
        return
    backend, inspect_backend = g["_configure_tex_cache"], g["_tex_cache_info"]
    state = local()

    def configure(directory=None, *, enabled=True):
        selected = _directory(directory, enabled)
        # Reconfiguration is explicit even when the path is unchanged: after
        # --clear-cache, reopen the new owned-root generation and cold-start
        # the memory front. Ordinary constructors use ensure() and do NOT
        # repeatedly reopen the store or discard its working set.
        report = backend(selected)
        state.report = deepcopy(report)
        _diagnose(report)
        return deepcopy(report)

    def ensure():
        if not hasattr(state, "report"):
            configure()

    def info():
        ensure()
        report = deepcopy(state.report)
        current = inspect_backend()
        for name, values in current.items():
            report["templates"][name].update(values)
        return report

    Scene = g["Scene"]
    previous_init = Scene.__init__
    previous_validate = g["_validate_tex_options"]

    @wraps(previous_init)
    def scene_init(self, *args, **kwargs):
        previous_init(self, *args, **kwargs)
        ensure()

    @wraps(previous_validate)
    def validate(*args, **kwargs):
        # Preserve template/preamble refusals at their original boundary.
        result = previous_validate(*args, **kwargs)
        ensure()
        return result

    Scene.__init__ = scene_init
    g["_validate_tex_options"] = validate
    g["_fmn_configure_tex_cache"] = configure
    g["_fmn_tex_cache_info"] = info
    g["_fmn_ensure_tex_cache"] = ensure
    g[_STATE] = state


def configure_tex_cache(
    directory: os.PathLike[str] | str | None = None, *, enabled: bool = True,
) -> dict[str, Any]:
    """Select the current thread's persistent typeset cache.

    None/empty directory selects the native owned per-user cache. An explicit
    directory must be an absent or already-owned dedicated cache leaf, not an
    arbitrary existing folder. enabled=False detaches without deleting files.
    Storage failures return a diagnostic receipt with persistent=False; normal
    native typesetting remains available. Repeating this call reopens the root
    generation, useful after clearing the cache. No public class is replaced.
    """
    return import_module("manimlib")._fmn_configure_tex_cache(directory, enabled=enabled)


def tex_cache_info() -> dict[str, Any]:
    """Current binding and actual native memory/disk-hit/layout counters.

    Counters are diagnostics, not a certified manifest or a performance claim.
    Configuration and statistics belong to the calling thread, just like the
    unsendable portal objects and their native engine slots.
    """
    return import_module("manimlib")._fmn_tex_cache_info()


def _preflight_arguments(sources, *, template="", preamble="", text_mode=False,
                         alignment=None, max_workers=32):
    """Own a bounded batch before cache configuration or native work begins."""
    if isinstance(sources, (str, bytes)):
        raise TypeError("Tex preflight sources must be an iterable of strings, not one string")
    if not isinstance(template, str) or not isinstance(preamble, str):
        raise TypeError("Tex preflight template and preamble must be strings")
    if not isinstance(text_mode, bool):
        raise TypeError("Tex preflight text_mode must be bool")
    if isinstance(max_workers, bool) or not isinstance(max_workers, int):
        raise TypeError("Tex preflight max_workers must be an integer")
    if not 1 <= max_workers <= 64:
        raise ValueError("Tex preflight max_workers must be in 1..=64")
    if alignment is not None and (not isinstance(alignment, str)
            or alignment not in (("left", "center", "right") if text_mode else ("left",))):
        raise ValueError("Tex preflight alignment must be left for mathematics, or left/center/right for text")

    def size(value, limit, name):
        # Check code points before encoding so a rejected giant input cannot
        # allocate another giant temporary UTF-8 buffer merely to be refused.
        if str.__len__(value) > limit:
            raise ValueError(f"Tex preflight {name} exceeds {limit} UTF-8 bytes")
        count = len(str.encode(value, "utf-8"))
        if count > limit:
            raise ValueError(f"Tex preflight {name} exceeds {limit} UTF-8 bytes")
        return count

    size(template, 1024, "template")
    preamble_bytes = size(preamble, 262_144, "preamble")
    batch, total = [], 0
    for index, source in enumerate(sources):
        if index >= 4096:
            raise ValueError("Tex preflight exceeds 4096 sources")
        if not isinstance(source, str):
            raise TypeError(f"Tex preflight source {index} must be str")
        total += size(source, 262_144, f"source {index}") + preamble_bytes
        if total > 4 * 1024 * 1024:
            raise ValueError("Tex preflight exceeds 4 MiB of source and preamble bytes")
        batch.append(source)
    return batch, dict(template=template, preamble=preamble, text_mode=text_mode,
                       alignment=alignment, max_workers=max_workers)


def preflight_tex(sources, *, template="", preamble="", text_mode=False,
                  alignment=None, max_workers=32) -> dict[str, Any]:
    r"""Typeset a batch before constructing Tex/TexText objects or playing frames.

    This warms the SAME native engine and content-addressed cache that ordinary
    constructors use. Mathematics defaults to display style, TexText to centered
    text with math islands. Use the same template, preamble and alignment as the
    subsequent constructors. Scale, color and isolate selections are not layout
    inputs. No Python layout worker or temporary engine is created.

    For example: preflight_tex([r"x^2", r"\frac{1}{2}"], max_workers=4).
    Invalid batch types and size/worker/alignment limits fail before cache
    configuration. Native template/preamble validation is retained. Formula errors
    are returned per source in input order; later valid sources still warm. The
    receipt includes actual before/after native cache counters and a worker
    ceiling, NOT a guarantee about the number of threads the host can start.
    This explicit batch does not discover dynamic strings in arbitrary Python.
    """
    batch, options = _preflight_arguments(
        sources, template=template, preamble=preamble, text_mode=text_mode,
        alignment=alignment, max_workers=max_workers,
    )
    module = import_module("manimlib")
    module._fmn_ensure_tex_cache()
    return module._preflight_tex(batch, **options)
