from __future__ import annotations

import contextlib
import hashlib
import importlib
import io
import json
import sys
import types
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
PORTAL_PYTHON = ROOT / "crates" / "fmn-python" / "python"
if str(PORTAL_PYTHON) not in sys.path:
    sys.path.insert(0, str(PORTAL_PYTHON))

cli = importlib.import_module("fmn_python.__main__")


def overlay(symbol: str, status: str = "same") -> str:
    return f"[status]\n{symbol}\t{status}\tevidence\ttests\tnote\n"


class PortalParityCliTests(unittest.TestCase):
    def module(self, name: str) -> types.ModuleType:
        module = types.ModuleType(name)
        sys.modules[name] = module
        self.addCleanup(sys.modules.pop, name, None)
        return module

    def native(self, text: str) -> types.ModuleType:
        module = types.ModuleType("fake_native")
        module._API_OVERLAY_TSV = text
        return module

    def invoke(self, native: types.ModuleType, *args: str):
        stdout = io.StringIO()
        stderr = io.StringIO()
        with mock.patch.object(sys, "argv", ["fmn-python", *args]), contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            code = cli._emit_parity_audit(native)
        return code, stdout.getvalue(), stderr.getvalue()

    def test_human_success_is_compact(self) -> None:
        module = self.module("fake_cli.pass")
        module.value = 1
        code, stdout, stderr = self.invoke(
            self.native(overlay("fake_cli.pass:value")),
            "--audit-parity",
        )
        self.assertEqual(code, 0)
        self.assertIn("portal parity audit: PASS", stdout)
        self.assertIn("1 SAME/IMPROVED rows", stdout)
        self.assertEqual(stderr, "")

    def test_robot_contradiction_is_structured_and_binds_overlay(self) -> None:
        module = self.module("fake_cli.placeholder")

        def placeholder():
            raise NotImplementedError

        placeholder._fmn_schema_placeholder = True
        module.value = placeholder
        text = overlay("fake_cli.placeholder:value", "improved")
        code, stdout, stderr = self.invoke(
            self.native(text),
            "--audit-parity",
            "--robot",
        )
        payload = json.loads(stdout)
        self.assertEqual(code, 1)
        self.assertFalse(payload["ok"])
        self.assertEqual(payload["counts"]["runtime_placeholders"], 1)
        self.assertEqual(payload["contradictions"][0]["code"], "reviewed-symbol-is-placeholder")
        self.assertEqual(
            payload["overlay_sha256"],
            hashlib.sha256(text.encode("utf-8")).hexdigest(),
        )
        self.assertEqual(stderr, "")

    def test_human_contradiction_writes_detail_to_stderr(self) -> None:
        self.module("fake_cli.missing")
        code, stdout, stderr = self.invoke(
            self.native(overlay("fake_cli.missing:nope")),
            "--audit-parity",
        )
        self.assertEqual(code, 1)
        self.assertIn("portal parity audit: FAIL", stdout)
        self.assertIn("missing-reviewed-symbol", stderr)

    def test_extra_audit_arguments_refuse_before_running(self) -> None:
        code, stdout, stderr = self.invoke(
            self.native(overlay("fake_cli.any:value")),
            "--audit-parity",
            "scene.py",
        )
        self.assertEqual(code, 2)
        self.assertEqual(stdout, "")
        self.assertIn("accepts only", stderr)

    def test_invalid_embedded_overlay_has_typed_robot_error(self) -> None:
        code, stdout, stderr = self.invoke(
            self.native("[status]\n"),
            "--audit-parity",
            "--robot",
        )
        payload = json.loads(stdout)
        self.assertEqual(code, 2)
        self.assertFalse(payload["ok"])
        self.assertIn("no [status] rows", payload["error"])
        self.assertEqual(stderr, "")


if __name__ == "__main__":
    unittest.main()
