"""Scoped native font faces supplied by the scene author."""
from contextlib import contextmanager
import os
from pathlib import Path
import stat

from . import effect_audit

_MAX_FONT_BYTES = 64 * 1024 * 1024


def _font_bytes(path):
    # The open itself must not block on a FIFO, even when a path is replaced.
    flags = (os.O_RDONLY | getattr(os, "O_NONBLOCK", 0)
             | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_BINARY", 0))
    with open(path, "rb", opener=lambda name, _: os.open(name, flags)) as stream:
        info = os.fstat(stream.fileno())
        if not stat.S_ISREG(info.st_mode):
            raise ValueError(f"font input must be a regular file: {path}")
        if info.st_size > _MAX_FONT_BYTES:
            raise ValueError("font input exceeds 64 MiB")
        data = stream.read(_MAX_FONT_BYTES + 1)
    if len(data) > _MAX_FONT_BYTES:
        raise ValueError("font input exceeds 64 MiB")
    return data


@contextmanager
def registered_font(font_file, register):
    path = Path(font_file).resolve()
    data = _font_bytes(path)
    registration = register(data)
    try:
        size = len(data)
        del data
        with effect_audit.scoped_file_input(str(path), registration.sha256, size):
            yield font_file
    finally:
        registration.close()
