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
import marshal
import os
from pathlib import Path
import stat
import sys
from types import CodeType

_MODULE = "fmn_python._embedded_bootstrap"
_SOURCE = Path(__file__).with_name("_embedded_bootstrap.py")
_MAX_SOURCE_BYTES = 4 * 1024 * 1024
_MAX_BYTECODE_BYTES = 32 * 1024 * 1024
_FILENAME = "<string>"
_CODE_FIELDS = (
    "co_argcount", "co_posonlyargcount", "co_kwonlyargcount", "co_nlocals",
    "co_stacksize", "co_flags", "co_code", "co_consts", "co_names",
    "co_varnames", "co_filename", "co_name", "co_qualname", "co_firstlineno",
    "co_linetable", "co_exceptiontable", "co_freevars", "co_cellvars",
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
    return code.replace(
        co_filename=_FILENAME,
        co_consts=tuple(_canonical_filename(value) if isinstance(value, CodeType) else value
                       for value in code.co_consts),
    )


def _code_identity_value(value):
    """Encode executable structure without marshal's object-reference state.

    A raw code object cannot always use marshal v2: CPython 3.14 adds slice
    constants, which require v5. Newer marshal formats also encode interning
    and reference sharing, so merely selecting the host's latest format would
    make fresh compilation and a cache load hash differently. Normalize every
    supported constant into tagged v2-compatible values instead. Tags retain
    type distinctions, and sorting frozensets removes hash-seed dependence.
    """
    kind = type(value)
    if kind is CodeType:
        return ("code", tuple(_code_identity_value(getattr(value, field))
                              for field in _CODE_FIELDS))
    if kind is tuple:
        return ("tuple", tuple(_code_identity_value(item) for item in value))
    if kind is frozenset:
        return ("frozenset", tuple(sorted(marshal.dumps(_code_identity_value(item), 2)
                                         for item in value)))
    if kind is slice:
        return ("slice", _code_identity_value(value.start),
                _code_identity_value(value.stop), _code_identity_value(value.step))
    if value is None:
        return ("none",)
    if value is Ellipsis:
        return ("ellipsis",)
    if kind in (bool, int, float, complex, str, bytes):
        # Marshal v2 preserves the exact floating-point bits, including signed
        # zero, without the reference/interning flags of newer formats.
        return (kind.__name__, value)
    raise TypeError("unsupported bootstrap code constant type: " + kind.__name__)


def _code_digest(code):
    payload = marshal.dumps(_code_identity_value(code), 2)
    return hashlib.sha256(b"fmn-bootstrap-code-v1\x00" + payload).hexdigest()


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
        code_sha256 = _code_digest(code)
    except (TypeError, ValueError, RecursionError):
        if source_path is None:
            raise
        # A valid pyc header and marshal body can still contain constants
        # that the compiler never emits, or an excessively nested code tree.
        # The optional cache must not prevent importing verified source.
        # Retry outside this handler so genuine compilation or identity
        # failures propagate, and let importlib repair the checked cache.
        code = _VerifiedSourceLoader(_SOURCE, source, ignore_bytecode=True).get_code(_MODULE)
        code = _canonical_filename(code)
        code_sha256 = _code_digest(code)
    identity = {
        "source_sha256": hashlib.sha256(source).hexdigest(),
        # Bind actual executable code, not the cache filename or cache
        # warmness, into the certified runtime's ABI inventory.
        "code_sha256": code_sha256,
        "cache_tag": sys.implementation.cache_tag,
        "magic": importlib.util.MAGIC_NUMBER.hex(),
        "optimization": sys.flags.optimize,
        "source_path": source_path,
    }
    return code, identity
