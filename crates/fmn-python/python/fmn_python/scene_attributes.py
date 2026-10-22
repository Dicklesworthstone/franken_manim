"""Structural Python state accompanying the native SceneState checkpoint.

Copy owned containers and arrays, not executable host objects or native proxy
identities. One memo spans the entire captured family: cross-object references,
shared containers and cycles survive without cloning a second native arena.
"""
from __future__ import annotations

import struct

# These are projections/owners, not user data. SceneState restores them through
# the native checkpoint and its existing topology/updater/camera protocols.
_PROJECTIONS = frozenset({
    "data", "submobjects", "uniforms", "updaters", "parents", "_scene", "_core",
    "locked_data_keys", "const_data_keys", "locked_uniform_keys", "event_listners",
    "_fmn_persistent_controller",
})
_MAX_NODES = 262_144
_MAX_BYTES = 64 * 1024 * 1024
_MAX_DEPTH = 128


def owned(name):
    return (isinstance(name, str) and name not in _PROJECTIONS
            and not (name.startswith("_fmn_") and name.endswith("_busy")))


class _Copier:
    def __init__(self, np):
        self.np, self.memo, self.retained = np, {}, []
        self.nodes, self.bytes = 0, 0

    def copy(self, value, depth=0):
        key = id(value)
        if key in self.memo:
            return self.memo[key]
        self.nodes += 1
        if self.nodes > _MAX_NODES or depth > _MAX_DEPTH:
            raise ValueError("SceneState Python data exceeds its structural budget")
        kind, np = type(value), self.np
        if isinstance(value, (np.ndarray, np.void)):
            self.bytes += value.nbytes
            if self.bytes > _MAX_BYTES:
                raise ValueError("SceneState Python arrays exceed the 64 MiB budget")
            # Strip ndarray subclasses; never run an authored copy/deepcopy.
            if isinstance(value, np.void):
                result = np.empty((), dtype=value.dtype)
                result[()] = value
            else:
                result = np.array(value, copy=True, subok=False, order="K")
            # A structured scalar is a mutable view, even without object
            # fields. Register its detached view BEFORE visiting object slots:
            # a record may name itself, another record, or an owning array.
            target = result
            if isinstance(value, np.void):
                target = result.view()
                result.setflags(write=bool(value.flags.writeable))
                result = result[()]
            self.memo[key] = result
            self.retained.append(value)
            if value.dtype.hasobject:
                self._objects(np.asarray(value), target, depth + 1)
            if isinstance(result, np.ndarray):
                result.setflags(write=bool(value.flags.writeable))
            return result
        if kind is dict:
            result = {}
            self.memo[key] = result
            for name, item in value.items():
                result[self.copy(name, depth + 1)] = self.copy(item, depth + 1)
        elif kind is list:
            result = []
            self.memo[key] = result
            result.extend(self.copy(item, depth + 1) for item in value)
        elif kind is tuple:
            items = [self.copy(item, depth + 1) for item in value]
            # A tuple -> list -> tuple cycle may have installed the tuple
            # while cloning one of its mutable descendants.
            result = self.memo.get(key, value if all(a is b for a, b in zip(value, items))
                                   else tuple(items))
        elif kind is set:
            result = value.copy()
        elif kind is bytearray:
            self.bytes += len(value)
            if self.bytes > _MAX_BYTES:
                raise ValueError("SceneState Python buffers exceed the 64 MiB budget")
            result = value.copy()
        else:
            # Mobjects, functions, bound methods, modules, and opaque external
            # objects retain identity. Checkpoints do not rewind their state.
            return value
        self.memo[key] = result
        self.retained.append(value)
        return result

    def _objects(self, source, target, depth):
        if depth > _MAX_DEPTH:
            raise ValueError("SceneState Python data exceeds its structural budget")
        if source.dtype.names:
            for name in source.dtype.names:
                if source.dtype[name].hasobject:
                    self._objects(source[name], target[name], depth + 1)
        elif source.dtype.kind == "O":
            for index in self.np.ndindex(source.shape):
                target[index] = self.copy(source[index], depth + 1)


def freeze(value, np, memo=None):
    """Detach owned Python data without invoking authored copy/equality hooks."""
    copier = _Copier(np)
    if memo is not None:
        copier.memo = memo
    return copier.copy(value)


def _attribute_values(members):
    return {id(member): {key: value for key, value in vars(member).items() if owned(key)}
            for member in members}


def _validate_attributes(snapshot, members):
    if (type(snapshot) is not dict or set(snapshot) != {id(member) for member in members}
            or any(type(values) is not dict or any(not owned(key) for key in values)
                   for values in snapshot.values())):
        raise ValueError("SceneState Python projection does not match its captured family")


def _validate_uniforms(uniforms, members):
    if (type(uniforms) is not dict or set(uniforms) != {id(member) for member in members}
            or any(value is not None and type(value) is not dict for value in uniforms.values())):
        raise ValueError("SceneState uniform projection does not match its captured family")


def capture(members, np):
    """Capture every member dictionary with one cross-family alias memo."""
    return freeze(_attribute_values(members), np)


def capture_projection(members, np):
    """Freeze attributes AND extension uniforms as one bounded alias graph.

    Native uniforms remain native checkpoint data. Only the Python mapping of
    extension values is included, with the same structural policy as attributes.
    Shared arrays/containers keep their identity across both projections.
    """
    members = tuple(members)
    uniforms = {id(member): getattr(getattr(member, "uniforms", None), "_extras", None)
                for member in members}
    _validate_uniforms(uniforms, members)
    return freeze((_attribute_values(members), uniforms), np)


def prepare_restore(snapshot, np, members=None):
    """Validate and allocate the projection before any native mutation."""
    if members is not None:
        _validate_attributes(snapshot, members)
    return freeze(snapshot, np)


def prepare_projection(attributes, uniforms, np, members):
    """Allocate both restored projections before entering native restoration."""
    members = tuple(members)
    _validate_attributes(attributes, members)
    _validate_uniforms(uniforms, members)
    return freeze((attributes, uniforms), np)


def restore(members, prepared):
    for member in members:
        namespace = vars(member)
        saved = prepared[id(member)]
        for key in tuple(namespace):
            if owned(key) and key not in saved:
                del namespace[key]
        # Bypass authored setters: this is state restoration, not a new edit.
        namespace.update(saved)


def same(np, left, right):
    """Compare data and alias topology without calling opaque __eq__ hooks."""
    forward, backward, retained = {}, {}, []

    def equal(a, b):
        # Snapshot arrays intentionally lose authored subclasses. Compare
        # their owned data without making an unchanged live subclass dirty.
        if type(a) is not type(b) and not (isinstance(a, np.ndarray) and isinstance(b, np.ndarray)):
            return False
        kind = type(a)
        structural = (kind in (dict, list, tuple, set, bytearray)
                      or isinstance(a, (np.ndarray, np.void)))
        if structural:
            x, y = id(a), id(b)
            if x in forward:
                return forward[x] == y
            if y in backward:
                return False
            forward[x], backward[y] = y, x
            retained.append((a, b))
        if a is b:
            return True
        if isinstance(a, np.ndarray):
            if (a.shape != b.shape or a.dtype != b.dtype
                    or a.flags.writeable != b.flags.writeable):
                return False
            if a.dtype.names:
                return all(equal(a[name], b[name]) for name in a.dtype.names)
            if a.dtype.hasobject:
                return all(equal(x, y) for x, y in zip(a.flat, b.flat))
            # Signed zero and NaN payloads may affect subsequent computation.
            return a.tobytes() == b.tobytes()
        if isinstance(a, np.generic):
            if (a.dtype != b.dtype or (isinstance(a, np.void)
                    and a.flags.writeable != b.flags.writeable)):
                return False
            if a.dtype.names:
                return all(equal(a[name], b[name]) for name in a.dtype.names)
            return a.tobytes() == b.tobytes()
        if kind is dict:
            return (len(a) == len(b)
                    and all(equal(ak, bk) and equal(av, bv)
                            for (ak, av), (bk, bv) in zip(a.items(), b.items())))
        if kind in (tuple, list):
            return len(a) == len(b) and all(equal(x, y) for x, y in zip(a, b))
        if kind is float:
            return struct.pack("!d", a) == struct.pack("!d", b)
        if kind is complex:
            return struct.pack("!dd", a.real, a.imag) == struct.pack("!dd", b.real, b.imag)
        if kind in (str, bytes, bytearray, bool, int, type(None)):
            return a == b
        if kind in (set, frozenset):
            # Only immutable/hashable values can be members. Canonical keys
            # avoid both arbitrary __eq__ and quadratic candidate searches.
            # Count keys: distinct NaNs can coexist even with identical bits.
            def counts(values):
                result = {}
                for value in values:
                    key = _set_key(np, value)
                    result[key] = result.get(key, 0) + 1
                return result
            return counts(a) == counts(b)
        return False

    return equal(left, right)


def _set_key(np, value):
    kind = type(value)
    if kind in (str, bytes, bool, int, type(None)):
        return (kind.__name__, value)
    if kind is float:
        return ("float", struct.pack("!d", value))
    if kind is complex:
        return ("complex", struct.pack("!dd", value.real, value.imag))
    if kind is tuple:
        return ("tuple", tuple(_set_key(np, item) for item in value))
    if kind is frozenset:
        counts = {}
        for item in value:
            key = _set_key(np, item)
            counts[key] = counts.get(key, 0) + 1
        return ("frozenset", frozenset(counts.items()))
    if isinstance(value, np.generic) and not value.dtype.hasobject:
        return ("numpy", str(value.dtype), value.tobytes())
    return ("identity", id(value))
