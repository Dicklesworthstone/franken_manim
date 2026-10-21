"""Data-only scene variants for the existing process batch owner."""
from __future__ import annotations

from dataclasses import dataclass
from itertools import islice
import json
import math

from .batch_checkpoint import _constructor_values

MAX_VARIANT_BYTES = 1024 * 1024
MAX_VARIANT_PLAN_BYTES = 8 * 1024 * 1024
MAX_VARIANT_NODES = 65_536
MAX_VARIANT_DEPTH = 64


@dataclass(frozen=True)
class SourceRenderJob:
    """A unique output name, local source class name, and plain JSON kwargs.

    No class, callable, native proxy or pickle crosses the process boundary.
    Inputs are deep-copied at batch admission, before any source import,
    observer or worker. Mutating a supplied dictionary afterward cannot change
    the admitted job. The same class may appear in any number of named jobs.
    """
    name: str
    scene: str
    scene_kwargs: dict | None = None


def _kwargs(value, budget):
    if value is None:
        value = {}
    if type(value) is not dict:
        raise TypeError("source job scene_kwargs must be a plain JSON object or None")

    def inspect(item, depth):
        budget[0] += 1
        if budget[0] > MAX_VARIANT_NODES or depth > MAX_VARIANT_DEPTH:
            raise ValueError("source jobs exceed their constructor node/depth budget")
        kind = type(item)
        if kind is str:
            if len(item) > MAX_VARIANT_BYTES:
                raise ValueError("source job constructor string exceeds its byte budget")
        elif kind is int:
            if item.bit_length() > 4096:
                raise ValueError("source job constructor integer exceeds its bit budget")
        elif kind is float:
            if not math.isfinite(item):
                raise ValueError("source job constructor numbers must be finite")
        elif kind is list or kind is dict:
            # Check width before visiting/copying, and charge every alias
            # occurrence. Cycles also fail the finite depth bound.
            if len(item) > MAX_VARIANT_NODES - budget[0]:
                raise ValueError("source jobs exceed their constructor node budget")
            if kind is dict:
                for key, child in item.items():
                    if type(key) is not str:
                        raise TypeError("source job constructor object keys must be strings")
                    inspect(key, depth + 1)
                    inspect(child, depth + 1)
            else:
                for child in item:
                    inspect(child, depth + 1)
        elif item is not None and kind is not bool:
            raise TypeError("source job constructors require plain JSON values")

    inspect(value, 0)
    frozen = _constructor_values(value)
    encoded = json.dumps(frozen, allow_nan=False, ensure_ascii=True,
                         sort_keys=True, separators=(",", ":")).encode("utf-8")
    if len(encoded) > MAX_VARIANT_BYTES:
        raise ValueError("source job constructor exceeds its 1 MiB byte budget")
    budget[1] += len(encoded)
    if budget[1] > MAX_VARIANT_PLAN_BYTES:
        raise ValueError("source jobs exceed their 8 MiB constructor byte budget")
    return frozen


def prepare_source_jobs(entries, maximum):
    """Normalize once; None preserves the exact legacy string-only plan."""
    from .batch_rendering import _name_key

    if isinstance(entries, (str, bytes, SourceRenderJob)):
        raise TypeError("scene_names must be an iterable of names or SourceRenderJob values")
    selected = list(islice(iter(entries), maximum + 1))
    if not selected or len(selected) > maximum:
        raise ValueError(f"parallel batch requires 1..{maximum} scene jobs")
    names, variants, keys, budget = [], [], set(), [0, 0]
    explicit = False
    for entry in selected:
        if isinstance(entry, SourceRenderJob):
            explicit = True
            name, scene, kwargs = entry.name, entry.scene, entry.scene_kwargs
        elif isinstance(entry, str):
            name, scene, kwargs = entry, entry, None
        else:
            raise TypeError("each source job must be a class name or SourceRenderJob")
        key = _name_key(name)
        if key in keys:
            raise ValueError("parallel batch scene names collide")
        keys.add(key)
        if (type(scene) is not str or not scene.isidentifier()
                or len(scene.encode("utf-8")) > 4096):
            raise ValueError("source job scene must be a local class identifier of at most 4096 UTF-8 bytes")
        names.append(name)
        variants.append({"scene": scene, "scene_kwargs": _kwargs(kwargs, budget)})
    return names, variants if explicit else None
