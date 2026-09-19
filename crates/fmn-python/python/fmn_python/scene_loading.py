"""Scoped, source-fresh loading of Python scene projects.

A source is a real module for its entire render, not a transient ``runpy``
namespace. Package-relative imports, dataclasses, lazy imports and imports of
one's own scene module consequently see the same objects. Only project-local
modules are refreshed; interpreter/portal modules are never reloaded. This is
an authoring boundary, not a sandbox or a certified input-closure claim.
Namespace packages are scoped too; namespaces spanning the project and foreign
roots are refused rather than silently reusing another checkout's children.
"""
from __future__ import annotations

import hashlib
import importlib
import importlib.abc
import importlib.machinery
import importlib.util
import sys
import threading
from pathlib import Path
from types import ModuleType
from typing import Any

# sys.path, sys.meta_path and sys.modules are interpreter-wide. Reject another
# owner instead of blocking: authored code may join a thread trying to render.
_SOURCE_OWNER = threading.Lock()
_MISSING = object()


def _origin(module: Any) -> Path | None:
    if not isinstance(module, ModuleType):
        return None
    filename = vars(module).get("__file__")
    if not isinstance(filename, str):
        return None
    try:
        return Path(filename).resolve()
    except (OSError, ValueError, RuntimeError):
        return None


def _protected(name: str) -> bool:
    return name in ("__main__", "manimlib", "fmn_python") or name.startswith(("manimlib.", "fmn_python."))


def _namespace_paths(module: Any) -> tuple[Path, ...]:
    if not isinstance(module, ModuleType):
        return ()
    spec = vars(module).get("__spec__")
    if spec is None or spec.origin is not None or spec.submodule_search_locations is None:
        return ()
    return tuple(Path(path).resolve() for path in vars(module).get("__path__", ()))


def _local_namespace(name: str, paths: tuple[Path, ...], root: Path) -> bool:
    local = [path.is_relative_to(root) for path in paths]
    if any(local) and not all(local):
        raise ImportError(f"scene namespace {name!r} has locations outside the scene project")
    return bool(local) and all(local)


class _FreshNamespace(importlib.machinery.NamespaceLoader):
    def __init__(self, name: str, path: Any, owner: SceneSource) -> None:
        # Keep CPython's namespace search and resource-reader behavior. A bare
        # custom Loader would break importlib.resources for authored assets.
        super().__init__(name, path, importlib.machinery.PathFinder.find_spec)
        self.owner = owner

    def exec_module(self, module: ModuleType) -> None:
        module.__file__ = None
        self.owner._loaded[module.__name__] = module
        super().exec_module(module)


class _FreshSource(importlib.machinery.SourceFileLoader):
    def __init__(self, name: str, path: str, owner: SceneSource) -> None:
        super().__init__(name, path)
        self.owner = owner

    def get_code(self, fullname: str):
        # Timestamp/size-based pyc validation can miss an equal-length edit in
        # the same timestamp tick. Compile exactly the bytes observed here.
        source = self.get_data(self.path)
        self.owner.source_digests[Path(self.path).resolve()] = hashlib.sha256(source).hexdigest()
        return self.source_to_code(source, self.path)

    def exec_module(self, module: ModuleType) -> None:
        self.owner._loaded[module.__name__] = module
        super().exec_module(module)


class _ProjectFinder(importlib.abc.MetaPathFinder):
    def __init__(self, owner: SceneSource) -> None:
        self.owner = owner

    def find_spec(self, fullname, path=None, target=None):
        if _protected(fullname):
            return None
        spec = importlib.machinery.PathFinder.find_spec(fullname, path)
        if spec is not None and spec.loader is None and spec.submodule_search_locations is not None:
            locations = tuple(Path(item).resolve() for item in spec.submodule_search_locations)
            if not _local_namespace(fullname, locations, self.owner.root):
                return None
            spec.loader = _FreshNamespace(fullname, spec.submodule_search_locations, self.owner)
            return spec
        if (spec is None or not isinstance(spec.loader, importlib.machinery.SourceFileLoader)
                or not isinstance(spec.origin, str)):
            return None
        origin = Path(spec.origin).resolve()
        if not origin.is_relative_to(self.owner.root):
            return None
        spec.loader = _FreshSource(fullname, str(origin), self.owner)
        return spec


class SceneSource:
    """Own one source module and its local imports through scene execution.

    ``scenes`` contains only Scene subclasses declared by the selected file.
    Imports stay live until exit, including imports inside ``construct`` and
    ``tear_down``. A later invocation observes current source bytes rather than
    stale modules or pyc files. Previously loaded project modules are restored
    afterward; foreign modules and authored sys.path changes are not discarded.
    Returned classes are intended for use inside the context.
    """

    def __init__(self, source: str | Path, scene_type: type) -> None:
        self.path = Path(source).resolve()
        if self.path.suffix.lower() not in (".py", ".pyw"):
            raise ValueError("the Python portal accepts only .py or .pyw scene sources")
        if not self.path.is_file():
            raise FileNotFoundError(f"scene source does not exist: {source}")
        if not isinstance(scene_type, type):
            raise TypeError("scene_type must be a Scene class")
        self.scene_type = scene_type
        parts = []
        root = self.path.parent
        while (root / "__init__.py").is_file():
            if not root.name.isidentifier():
                raise ImportError(f"scene package directory is not a Python identifier: {root.name!r}")
            parts.insert(0, root.name)
            if root.parent == root:
                raise ImportError("the filesystem root cannot be a scene package")
            root = root.parent
        self.root = root
        self.package = ".".join(parts)
        if self.package and self.path.name == "__init__.py":
            self.name = self.package
        elif self.package and self.path.stem.isidentifier() and self.path.suffix == ".py":
            self.name = self.package + "." + self.path.stem
        else:
            digest = hashlib.sha256(str(self.path).encode("utf-8", "surrogatepass")).hexdigest()[:24]
            self.name = (self.package + "." if self.package else "") + "__fmn_scene_" + digest
        self.module: ModuleType | None = None
        self.scenes: dict[str, type] = {}
        # Source-only observations, including lazy imports. This deliberately
        # does not purport to capture C2-C10 or arbitrary Python host effects.
        self.source_digests: dict[Path, str] = {}
        self._saved: dict[str, Any] = {}
        self._loaded: dict[str, ModuleType] = {}
        self._paths: list[str] = []
        self._finder = _ProjectFinder(self)
        self._entered = False
        self._active = False

    def _prepare(self) -> None:
        # A pre-existing foreign package must not redirect relative imports to
        # another checkout. Refuse before executing either project's code.
        if self.package:
            top = self.package.split(".", 1)[0]
            prior = sys.modules.get(top, _MISSING)
            if prior is not _MISSING:
                origin = _origin(prior)
                if origin is None or origin != self.root / top / "__init__.py":
                    raise ImportError(f"scene package {top!r} is already loaded from another location")
        # Cached absolute sibling imports bypass finders. Never silently reuse
        # a different project's helper just because it has the same filename.
        for name, module in tuple(sys.modules.items()):
            if "." in name or not name.isidentifier():
                continue
            for directory in (self.root, self.path.parent):
                namespace = _namespace_paths(module)
                if namespace and (directory / name).is_dir() and not _local_namespace(name, namespace, self.root):
                    raise ImportError(f"scene namespace {name!r} has locations outside the scene project")
                candidates = (directory / (name + ".py"), directory / name / "__init__.py")
                for candidate in candidates:
                    if (candidate != self.path and candidate.is_file()
                            and _origin(module) != candidate.resolve()):
                        raise ImportError(f"scene project module {name!r} is already loaded from another location")
        for name, module in tuple(sys.modules.items()):
            # Never evict the active engine even if a scene was placed in its
            # installation tree. That would fork native class identity.
            if _protected(name):
                continue
            origin = _origin(module)
            local_namespace = _local_namespace(name, _namespace_paths(module), self.root)
            if (origin is not None and origin.is_relative_to(self.root)) or local_namespace:
                # A namespace has no __file__, but retains child attributes.
                # Refresh its container as well as its source children, or
                # `from namespace import helper` can bypass sys.modules and
                # keep using a previous generation's helper indefinitely.
                self._saved[name] = module
        for name in self._saved:
            sys.modules.pop(name, None)
        # Keep the historical absolute sibling-import behavior as well as the
        # package root needed for normal qualified/relative imports.
        for path in dict.fromkeys((str(self.root), str(self.path.parent))):
            sys.path.insert(0, path)
            self._paths.append(path)
        index = next((i for i, finder in enumerate(sys.meta_path)
                      if finder is importlib.machinery.PathFinder), len(sys.meta_path))
        sys.meta_path.insert(index, self._finder)
        importlib.invalidate_caches()

    def _load(self) -> ModuleType:
        if self.package:
            parent = self.name.rpartition(".")[0]
            if parent:
                importlib.import_module(parent)
        existing = sys.modules.get(self.name)
        if existing is not None:
            if _origin(existing) != self.path:
                raise ImportError(f"scene module {self.name!r} resolves to another source")
            # A package initializer may already have imported this very file.
            return existing
        loader = _FreshSource(self.name, str(self.path), self)
        locations = [str(self.path.parent)] if self.path.name == "__init__.py" else None
        spec = importlib.util.spec_from_file_location(
            self.name, self.path, loader=loader, submodule_search_locations=locations,
        )
        if spec is None:
            raise ImportError(f"cannot create a scene module for {self.path}")
        module = importlib.util.module_from_spec(spec)
        sys.modules[self.name] = module
        self._loaded[self.name] = module
        # A standalone module can import itself by its normal filename. Do not
        # overwrite a stdlib/third-party module with a coincidentally equal name.
        if not self.package and self.path.stem.isidentifier():
            alias = self.path.stem
            if alias not in sys.modules:
                sys.modules[alias] = module
                self._loaded[alias] = module
        loader.exec_module(module)
        if self.package:
            parent, _, leaf = self.name.rpartition(".")
            if parent:
                setattr(sys.modules[parent], leaf, module)
        return module

    def __enter__(self) -> SceneSource:
        if self._entered:
            raise RuntimeError("a SceneSource can be entered only once")
        if not _SOURCE_OWNER.acquire(blocking=False):
            raise RuntimeError("another scene source owns this interpreter's import context")
        self._entered = self._active = True
        try:
            self._prepare()
            self.module = self._load()
            self.scenes = {
                name: value for name, value in vars(self.module).items()
                if isinstance(value, type) and value is not self.scene_type
                and issubclass(value, self.scene_type) and value.__module__ == self.name
            }
            return self
        except BaseException:
            self.__exit__(*sys.exc_info())
            raise

    def __exit__(self, exc_type, exc_value, traceback) -> bool:
        if not self._active:
            return False
        try:
            for name, module in self._loaded.items():
                if sys.modules.get(name) is module:
                    sys.modules.pop(name, None)
            for name, module in self._saved.items():
                # Preserve an explicit authored replacement instead of silently
                # destroying it. Normal imports above have just been removed.
                if name not in sys.modules:
                    sys.modules[name] = module
            for index in range(len(sys.meta_path) - 1, -1, -1):
                if sys.meta_path[index] is self._finder:
                    del sys.meta_path[index]
            for entry in self._paths:
                for index, value in enumerate(sys.path):
                    if value is entry:
                        del sys.path[index]
                        break
        finally:
            self._active = False
            _SOURCE_OWNER.release()
        return False

    @property
    def sources(self) -> dict[str, bytes]:
        """Expose raw byte contents of the primary source and imported modules."""
        result: dict[str, bytes] = {}
        primary_key = (
            self.path.relative_to(self.root).as_posix()
            if self.path.is_relative_to(self.root)
            else self.path.name
        )
        try:
            result[primary_key] = self.path.read_bytes()
        except OSError:
            pass
        for p in sorted(self.source_digests.keys()):
            if p == self.path:
                continue
            vpath = (
                p.relative_to(self.root).as_posix()
                if p.is_relative_to(self.root)
                else p.name
            )
            try:
                result[vpath] = p.read_bytes()
            except OSError:
                pass
        return result
