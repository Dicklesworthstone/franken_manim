"""Actual file registration, geometry, lifetime and certified input witnesses."""
import hashlib
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest import TestCase

import numpy as np
import manimlib as m
from manimlib.mobject.svg.text_mobject import register_font
from fmn_python import effect_audit

check = TestCase()
# Rename only the fixture's name strings. No workstation fonts are needed,
# and comparing geometry with Plex establishes which outlines were selected.
name = "FMN Test Face"
data = original_font
for encoding in ("ascii", "utf-16-be"):
    data = data.replace("IBM Plex Sans".encode(encoding), name.encode(encoding))
digest = hashlib.sha256(data).hexdigest()
reference = m.Text("Jazz", font="IBM Plex Sans").get_all_points()
assert not np.array_equal(reference, m.Text("Jazz").get_all_points())

with TemporaryDirectory() as folder:
    path = Path(folder) / "unrelated-filename.ttf"
    path.write_bytes(data)
    invalid = path.with_name("corrupt.ttf")
    invalid.write_bytes(b"broken")
    conflict = path.with_name("conflicting.ttf")
    conflict.write_bytes(data + b"another source")
    shadow = path.with_name("bundled.ttf")
    shadow.write_bytes(original_font)

    with check.assertRaises(FileNotFoundError):
        with register_font(path.with_name("missing.ttf")):
            check.fail("missing file accepted")
    for bad in (invalid, shadow):
        with check.assertRaises(ValueError):
            with register_font(bad):
                check.fail("unusable face accepted")
    with check.assertRaisesRegex(ValueError, name):
        m.Text("A", font=name)

    with check.assertRaisesRegex(RuntimeError, "end scope"):
        with register_font(path):
            recorder = effect_audit.begin()
            try:
                check.assertIn(("read/" + digest + "/" + path.name, digest),
                               recorder.inputs(RuntimeError))
            finally:
                recorder.stop()
            with register_font(path):
                saved = m.Text("Jazz", font=name)
                np.testing.assert_array_equal(saved.get_all_points(), reference)
            with check.assertRaises(ValueError):
                with register_font(conflict):
                    check.fail("different bytes shadowed an active family")
            markup = m.MarkupText('<span font_family="FMN Test Face">Jazz</span>')
            np.testing.assert_array_equal(markup.get_all_points(), reference)
            raise RuntimeError("end scope")
    with check.assertRaisesRegex(ValueError, name):
        m.Text("A", font=name)
    np.testing.assert_array_equal(saved.get_all_points(), reference)
