"""Shared interpreter/library roots and project import ownership.

Installed code must not become reloadable source just because an environment
lives inside a scene project. This is an import-ownership policy, not a sandbox
or a claim that all library reads belong to a certified input closure.
"""
from __future__ import annotations

import os
from pathlib import Path
import site
import sys
import sysconfig

_INSTALL_COMPONENTS = frozenset({"site-packages", "dist-packages"})


def library_roots() -> tuple[Path, ...]:
    """Real installation paths used by both effect auditing and source loading.

    Do not include whole interpreter prefixes here: exempting an entire /usr
    or project tree from effect auditing would hide ordinary authored inputs.
    """
    paths = sysconfig.get_paths()
    values = [paths.get(key) for key in ("stdlib", "platstdlib", "purelib", "platlib")]
    if hasattr(site, "getsitepackages"):
        values.extend(site.getsitepackages())
    if hasattr(site, "getusersitepackages"):
        user = site.getusersitepackages()
        values.extend((user,) if isinstance(user, str) else user)
    # The portal can also be used from a source checkout during development.
    values.append(Path(__file__).resolve().parent.parent)
    return tuple(sorted({Path(value).resolve() for value in values if value}))


class RuntimePaths:
    """Freeze runtime exclusions for one project/import generation.

    Use both lexical and real paths for nested prefixes and search entries:
    symlinked environments must protect their targets as well as their aliases.
    Arbitrary PYTHONPATH directories remain project source, not installations.
    """

    def __init__(self, project_root: Path):
        project_root = project_root.resolve()
        roots = set(library_roots())
        for value in (sys.prefix, sys.base_prefix, sys.exec_prefix, sys.base_exec_prefix):
            if not value:
                continue
            lexical = Path(os.path.abspath(value))
            real = lexical.resolve()
            if lexical.is_relative_to(project_root) or real.is_relative_to(project_root):
                roots.update((lexical, real))
        # Include symlink targets of installation entries even when resolving
        # an imported module has erased its site-packages path component.
        for entry in tuple(sys.path):
            if not isinstance(entry, str) or not entry:
                continue
            path = Path(os.path.abspath(entry))
            for part in (path, *path.parents):
                if part.name in _INSTALL_COMPONENTS:
                    roots.update((part, part.resolve()))
                    break
        self.roots = tuple(sorted(roots))
        # Freeze component prefixes once, rather than rebuilding every parent
        # Path for every imported module. normcase preserves Windows Path's
        # case-insensitive comparison; components preserve directory boundaries.
        self._root_parts = tuple(
            tuple(os.path.normcase(part) for part in root.parts) for root in self.roots
        )

    def contains(self, path: Path) -> bool:
        path = Path(path)
        if _INSTALL_COMPONENTS.intersection(path.parts):
            return True
        lexical = Path(os.path.abspath(path))
        real = lexical.resolve()
        lexical_parts = tuple(os.path.normcase(part) for part in lexical.parts)
        real_parts = tuple(os.path.normcase(part) for part in real.parts)
        return bool(_INSTALL_COMPONENTS.intersection(real.parts)) or any(
            lexical_parts[:len(root)] == root or real_parts[:len(root)] == root
            for root in self._root_parts
        )
