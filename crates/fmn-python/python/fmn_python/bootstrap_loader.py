"""Load the embedded portal skin through CPython's source-backed code cache.

The wheel's source is generated from the very file Rust embeds, never a second
maintained implementation. Verify those bytes before consulting ``__pycache__``;
direct-extension and embedded users without the generated file compile the
embedded source. Execution still belongs to the native module's globals.
"""
from __future__ import annotations

import hashlib
import importlib.machinery
import importlib.util
import os
from pathlib import Path
import stat
import struct
import sys
from types import CodeType

_MODULE = "fmn_python._embedded_bootstrap"
_SOURCE = Path(__file__).with_name("_embedded_bootstrap.py")
_MAX_SOURCE_BYTES = 4 * 1024 * 1024
_MAX_BYTECODE_BYTES = 32 * 1024 * 1024
_MAX_IDENTITY_BYTES = 32 * 1024 * 1024
_FILENAME = "<string>"
_CODE_FIELDS = (
    "co_argcount", "co_posonlyargcount", "co_kwonlyargcount", "co_nlocals",
    "co_stacksize", "co_flags", "co_code", "co_consts", "co_names",
    "co_varnames", "co_freevars", "co_cellvars", "co_filename", "co_name",
    "co_qualname", "co_firstlineno", "co_linetable", "co_exceptiontable",
)


def _read_bounded(path, limit):
    # Bytecode is an optional optimization, never an unbounded read or a FIFO
    # that can stall startup. File descriptors also keep a concurrent atomic
    # cache replacement from mixing the old and new file's contents.
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NONBLOCK", 0))
    with os.fdopen(descriptor, "rb") as stream:
        info = os.fstat(stream.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_size > limit:
            raise OSError("bootstrap input is not a bounded regular file")
        payload = stream.read(limit + 1)
        if len(payload) > limit:
            raise OSError("bootstrap input exceeds its byte budget")
        return payload


class _VerifiedSourceLoader(importlib.machinery.SourceFileLoader):
    """Normal CPython caching, bound to the already verified source bytes."""

    def __init__(self, path, source, *, ignore_bytecode=False):
        super().__init__(_MODULE, str(path))
        self.source = source
        self.source_hash = importlib.util.source_hash(source)
        self.ignore_bytecode = ignore_bytecode

    def get_data(self, path):
        if path == self.path:
            # Never reopen the source after its comparison with the extension.
            return self.source
        if self.ignore_bytecode:
            raise OSError("discarding an invalid bootstrap bytecode cache")
        data = _read_bounded(path, _MAX_BYTECODE_BYTES)
        # PEP 552's checked-hash header. Timestamp caches cannot prove source
        # equality after a same-size, same-mtime wheel update. Checking here
        # also respects the contract under --check-hash-based-pycs=never.
        if (len(data) < 16 or data[:4] != importlib.util.MAGIC_NUMBER
                or data[4:8] != b"\x03\x00\x00\x00"
                or data[8:16] != self.source_hash):
            raise OSError("bootstrap bytecode has a stale source or interpreter identity")
        return data

    def set_data(self, path, data, **kwargs):
        # Leave serialization and atomic publication to importlib. Only the
        # public PEP 552 header changes; its bytecode payload is untouched.
        checked = (importlib.util.MAGIC_NUMBER + b"\x03\x00\x00\x00"
                   + self.source_hash + data[16:])
        try:
            super().set_data(path, checked, **kwargs)
        except OSError:
            # A read-only installation still imports from the verified source.
            pass


def _canonical_filename(code):
    """Keep installed/embedded tracebacks and code identity path-independent."""
    memo = {}
    remaining = _MAX_IDENTITY_BYTES

    def visit(current, depth=0):
        nonlocal remaining
        if depth > 128:
            raise ImportError("bootstrap executable identity exceeds its resource budget")
        if id(current) in memo:
            return memo[id(current)]
        # Code objects are immutable and kept alive by the original root.
        # Preserve a cache's shared code graph instead of expanding every
        # path through it before the digest can enforce its own budget.
        remaining -= 256 + len(current.co_code) + 8 * len(current.co_consts)
        if remaining < 0:
            raise ImportError("bootstrap executable identity exceeds its resource budget")
        normalized = current.replace(
            co_filename=_FILENAME,
            co_consts=tuple(visit(value, depth + 1) if isinstance(value, CodeType) else value
                           for value in current.co_consts),
        )
        memo[id(current)] = normalized
        return normalized

    return visit(code)


def _code_digest(code):
    """Hash executable contents independently of marshal's object encoding.

    Marshal v2 cannot represent CPython 3.14's constant slices, while newer
    versions encode sharing/interning that can differ after a cache load.
    This versioned, typed encoding covers the code constructor's fields and
    immutable compiler constants, including nested code and slices. It ignores
    object sharing and sorts frozensets; float bits (including signed zero)
    remain distinct. Interpreter/optimization identity is recorded separately.
    """
    remaining = _MAX_IDENTITY_BYTES

    def encode(value, depth=0):
        nonlocal remaining
        if depth > 128 or remaining < 9:
            raise ImportError("bootstrap executable identity exceeds its resource budget")
        kind = type(value)
        if value is None:
            tag, payload = b"n", b""
        elif value is Ellipsis:
            tag, payload = b"e", b""
        elif kind is bool:
            tag, payload = b"b", bytes((value,))
        elif kind is int:
            size = (value.bit_length() + 7) // 8
            if size + 10 > remaining:
                raise ImportError("bootstrap executable identity exceeds its resource budget")
            tag = b"i"
            payload = (b"-" if value < 0 else b"+") + abs(value).to_bytes(size, "big")
        elif kind is float:
            tag, payload = b"f", struct.pack(">d", value)
        elif kind is complex:
            tag, payload = b"j", struct.pack(">dd", value.real, value.imag)
        elif kind is str:
            tag, payload = b"u", value.encode("utf-8", "surrogatepass")
        elif kind is bytes:
            tag, payload = b"y", value
        elif kind in (tuple, frozenset):
            parts = [encode(item, depth + 1) for item in value]
            if kind is frozenset:
                parts.sort()
            tag, payload = (b"t" if kind is tuple else b"r"), b"".join(parts)
        elif kind is slice:
            tag, payload = b"s", encode((value.start, value.stop, value.step), depth + 1)
        elif kind is CodeType:
            tag, payload = b"c", encode(tuple(getattr(value, field) for field in _CODE_FIELDS), depth + 1)
        else:
            # A cache containing a foreign constant is not identified by repr
            # (which may carry an address) or silently reduced to its source.
            raise TypeError("unsupported bootstrap code constant type: " + kind.__name__)
        size = len(payload) + 9
        if size > remaining:
            raise ImportError("bootstrap executable identity exceeds its resource budget")
        # Charge every materialized subtree, bounding even a compact marshal
        # payload that expands through heavily shared constants.
        remaining -= size
        return tag + len(payload).to_bytes(8, "big") + payload

    return hashlib.sha256(b"fmn-python.bootstrap-code.v1\0" + encode(code)).hexdigest()


def load_bootstrap(source):
    """Return the exact embedded skin's code and its executed-runtime identity.

    Missing generated source is the supported direct-extension path. A present
    source from a different build is an installation error, not a cache miss.
    Damaged, stale, oversized and unwritable bytecode caches remain harmless
    misses; the embedded source is always available to compile.
    """
    if not isinstance(source, bytes) or len(source) > _MAX_SOURCE_BYTES:
        raise ImportError("embedded bootstrap must be bounded source bytes")
    source_path = None
    try:
        installed = _read_bounded(_SOURCE, _MAX_SOURCE_BYTES)
    except FileNotFoundError:
        code = compile(source, _FILENAME, "exec", dont_inherit=True)
    except OSError as error:
        raise ImportError("cannot verify the installed bootstrap source") from error
    else:
        if installed != source:
            raise ImportError("installed bootstrap source differs from the native extension; rebuild the wheel")
        source_path = str(_SOURCE)
        loader = _VerifiedSourceLoader(_SOURCE, source)
        try:
            code = loader.get_code(_MODULE)
        except (ImportError, EOFError, ValueError, TypeError):
            # A syntactically invalid marshal payload is outside get_code's
            # header-validation fallback. Retry through the same compiler and
            # publisher with bytecode reads disabled, without deleting files.
            code = _VerifiedSourceLoader(_SOURCE, source, ignore_bytecode=True).get_code(_MODULE)
    try:
        code = _canonical_filename(code)
        code_digest = _code_digest(code)
    except (ImportError, TypeError, ValueError, RecursionError):
        if source_path is None:
            raise
        # A valid marshal header/body can still contain foreign constants or
        # an expanding code graph. Such a cache is a miss, just like malformed
        # bytecode; retry once from the source already verified above. A refusal
        # on that trusted source propagates instead of retrying indefinitely.
        code = _canonical_filename(
            _VerifiedSourceLoader(_SOURCE, source, ignore_bytecode=True).get_code(_MODULE)
        )
        code_digest = _code_digest(code)
    identity = {
        "source_sha256": hashlib.sha256(source).hexdigest(),
        # This digest binds actual executable code, not the cache filename or
        # cache warmness, into the certified runtime's ABI inventory.
        "code_sha256": code_digest,
        "cache_tag": sys.implementation.cache_tag,
        "magic": importlib.util.MAGIC_NUMBER.hex(),
        "optimization": sys.flags.optimize,
        "source_path": source_path,
    }
    return code, identity
