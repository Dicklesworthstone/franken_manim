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
