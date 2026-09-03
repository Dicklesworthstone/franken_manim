from __future__ import annotations

import contextlib
import hashlib
import io
import json
import sys
import tempfile
import types
import unittest
from pathlib import Path

import audit_portal_runtime as audit


def status_text(*rows: str) -> str:
    return "\n".join(("[status]", "# symbol\tstatus\tevidence\ttests\tnotes", *rows, ""))


def row(symbol: str, status: str = "same") -> str:
    return f"{symbol}\t{status}\tevidence\ttests\tnote"


class RuntimeAuditTests(unittest.TestCase):
    def module(self, name: str) -> types.ModuleType:
        module = types.ModuleType(name)
        self.addCleanup(sys.modules.pop, name, None)
        sys.modules[name] = module
        return module

    def test_reviewed_real_function_and_method_pass(self) -> None:
        module = self.module("fake_portal.real")

        def function():
            return 1

        class Widget:
            def method(self):
                return 2

        module.function = function
        module.Widget = Widget
        rows = audit.parse_status_rows(
            status_text(
                row("fake_portal.real:function", "same"),
                row("fake_portal.real:Widget.method", "improved"),
            )
        )
        report = audit.audit_rows(rows)
        self.assertTrue(report["ok"])
        self.assertEqual(report["counts"]["reviewed_implemented"], 2)
        self.assertEqual(report["counts"]["contradictions"], 0)

    def test_schema_placeholders_fail_reviewed_claims(self) -> None:
        module = self.module("fake_portal.placeholder")

        def unavailable():
            raise NotImplementedError

        unavailable._fmn_schema_placeholder = True
        module.function = unavailable
        rows = audit.parse_status_rows(status_text(row("fake_portal.placeholder:function")))
        report = audit.audit_rows(rows)
        self.assertFalse(report["ok"])
        self.assertEqual(report["counts"]["runtime_placeholders"], 1)
        self.assertEqual(
            report["contradictions"][0]["code"],
            "reviewed-symbol-is-placeholder",
        )

    def test_tiered_and_excluded_placeholders_do_not_claim_implementation(self) -> None:
        module = self.module("fake_portal.boundary")

        def unavailable():
            raise NotImplementedError

        unavailable._fmn_schema_placeholder = True
        module.tiered = unavailable
        module.excluded = unavailable
        rows = audit.parse_status_rows(
            status_text(
                row("fake_portal.boundary:tiered", "tiered"),
                row("fake_portal.boundary:excluded", "excluded"),
            )
        )
        report = audit.audit_rows(rows)
        self.assertTrue(report["ok"])
        self.assertEqual(report["counts"]["reviewed_implemented"], 0)
        self.assertEqual(report["counts"]["runtime_placeholders"], 0)

    def test_missing_reviewed_symbol_and_module_fail_closed(self) -> None:
        self.module("fake_portal.missing_symbol")
        rows = audit.parse_status_rows(
            status_text(
                row("fake_portal.missing_symbol:nope"),
                row("fake_portal.no_such_module:nope"),
            )
        )
        report = audit.audit_rows(rows)
        self.assertFalse(report["ok"])
        self.assertEqual(report["counts"]["missing_reviewed"], 2)
        self.assertEqual(
            {item["code"] for item in report["contradictions"]},
            {"missing-reviewed-symbol", "module-import-failed"},
        )

    def test_contradictions_are_sorted_by_symbol(self) -> None:
        module = self.module("fake_portal.sort")
        rows = audit.parse_status_rows(
            status_text(
                row("fake_portal.sort:zeta"),
                row("fake_portal.sort:alpha"),
            )
        )
        report = audit.audit_rows(rows)
        self.assertEqual(
            [item["symbol"] for item in report["contradictions"]],
            ["fake_portal.sort:alpha", "fake_portal.sort:zeta"],
        )

    def test_parser_rejects_duplicate_unknown_and_malformed_rows(self) -> None:
        cases = (
            status_text(row("fake:x"), row("fake:x")),
            status_text(row("fake:x", "pretend")),
            "[status]\nfake:x\tsame\ttoo\tfew\n",
            "[status]\nmissing-colon\tsame\te\tt\tn\n",
            "[status]\n",
        )
        for text in cases:
            with self.subTest(text=text):
                with self.assertRaises(audit.AuditError):
                    audit.parse_status_rows(text)

    def test_json_envelope_binds_exact_overlay_bytes(self) -> None:
        module = self.module("fake_portal.json")
        module.x = 1
        text = status_text(row("fake_portal.json:x"))
        report = audit.audit_overlay(text)
        payload = json.loads(audit.render_json(report))
        self.assertEqual(payload["schema"], "fmn.portal.runtime-audit")
        self.assertEqual(payload["version"], 1)
        self.assertTrue(payload["ok"])
        self.assertEqual(
            payload["overlay_sha256"],
            hashlib.sha256(text.encode("utf-8")).hexdigest(),
        )

    def test_main_check_returns_one_without_hiding_report(self) -> None:
        module = self.module("fake_portal.cli")

        def placeholder():
            raise NotImplementedError

        placeholder._fmn_schema_placeholder = True
        module.placeholder = placeholder
        text = status_text(row("fake_portal.cli:placeholder"))
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "overlay.tsv"
            path.write_text(text, encoding="utf-8")
            stdout = io.StringIO()
            stderr = io.StringIO()
            with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                code = audit.main(["--overlay", str(path), "--check"])
        payload = json.loads(stdout.getvalue())
        self.assertEqual(code, 1)
        self.assertEqual(stderr.getvalue(), "")
        self.assertFalse(payload["ok"])
        self.assertEqual(
            payload["overlay_sha256"],
            hashlib.sha256(text.encode("utf-8")).hexdigest(),
        )


if __name__ == "__main__":
    unittest.main()
