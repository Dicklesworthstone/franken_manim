"""Distribution helpers for the optional FrankenManim CPython portal."""

import csv as _csv
import re as _re
from importlib.metadata import distributions as _distributions
from pathlib import PurePosixPath as _PurePosixPath


class _ManimlibNamespaceCollision(ImportError):
    """Two installed distributions claim the exclusive ``manimlib`` tree."""

    def __init__(self, providers):
        self.providers = tuple(providers)
        joined = ", ".join(self.providers)
        super().__init__(
            "the exclusive manimlib package is claimed by franken-manim and "
            f"{joined}; install each provider in a separate virtual environment"
        )


def _canonical_distribution_name(name):
    """Apply the comparison part of Python's distribution-name normalization."""

    return _re.sub(r"[-_.]+", "-", str(name)).lower()


def _claims_manimlib(package_path):
    parts = _PurePosixPath(str(package_path).replace("\\", "/")).parts
    return bool(parts) and parts[0] == "manimlib"


def _foreign_manimlib_providers():
    """Return installed non-FrankenManim distributions owning ``manimlib``."""

    providers = set()
    for distribution in _distributions():
        try:
            files = distribution.files or ()
            if not any(_claims_manimlib(path) for path in files):
                continue
            name = distribution.metadata.get("Name") or "<unknown distribution>"
        except (OSError, UnicodeError, ValueError, _csv.Error):
            # Broken metadata in an unrelated distribution must not disable the
            # portal. A malformed claimant whose files cannot be enumerated is
            # outside the package manager's own collision model as well.
            continue
        if _canonical_distribution_name(name) != "franken-manim":
            providers.add(str(name))
    return tuple(sorted(providers, key=lambda name: (name.casefold(), name)))


def _ensure_exclusive_manimlib_namespace():
    """Refuse a detectable package-file collision before loading native code."""

    providers = _foreign_manimlib_providers()
    if providers:
        raise _ManimlibNamespaceCollision(providers)


def __getattr__(name):
    if name == "__version__":
        _ensure_exclusive_manimlib_namespace()
        from manimlib import __version__

        return __version__
    raise AttributeError(f"module {__name__!r} has no attribute {name!r}")
