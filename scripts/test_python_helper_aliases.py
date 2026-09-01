from __future__ import annotations

import contextlib
import io
import tempfile
import unittest
from pathlib import Path

import check_python_helper_aliases as policy


class AliasPolicyTests(unittest.TestCase):
    def write(self, name: str, text: str) -> Path:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        path = Path(directory.name) / name
        path.write_text(text, encoding="utf-8")
        return path

    def schema(self, *, leaked: str | None = None, omit: str | None = None) -> Path:
        rows = []
        for alias, (module, class_name) in policy.REFERENCE_CONSTRUCTOR_ALIASES.items():
            if class_name != omit:
                rows.append(f"{module}\t{class_name}\tclass\tdefined\t1\t-")
            if alias == leaked:
                rows.append(f"{module}\t{alias}\tfunction\tdefined\t1\t-")
        return self.write(
            "API_SCHEMA.tsv",
            "[meta]\nschema_version\t1\n[symbols]\n"
            "# module\tname\tkind\torigin\texported\tdetail\n"
            + "\n".join(rows)
            + "\n",
        )

    def wheel(
        self,
        mapping: dict[str, tuple[str, str]] | None = None,
        *,
        call: bool = True,
    ) -> Path:
        selected = policy.REFERENCE_CONSTRUCTOR_ALIASES if mapping is None else mapping
        call_line = (
            "    verify_reference_constructor_aliases(manimlib)\n"
            if call
            else "    pass\n"
        )
        return self.write(
            "wheel_smoke.py",
            "REFERENCE_CONSTRUCTOR_ALIASES = "
            + repr(selected)
            + "\n\n"
            + "def verify_reference_constructor_aliases(manimlib):\n"
            + "    return None\n\n"
            + "def verify_installed_distribution():\n"
            + call_line,
        )

    def test_complete_policy_passes(self) -> None:
        policy.verify(self.schema(), self.wheel())

    def test_snake_case_export_leak_is_rejected(self) -> None:
        with self.assertRaisesRegex(policy.AliasPolicyError, "leaked into Python exports"):
            policy.verify(self.schema(leaked="small_dot"), self.wheel())

    def test_missing_reference_class_is_rejected(self) -> None:
        with self.assertRaisesRegex(policy.AliasPolicyError, "VHighlight is missing"):
            policy.verify(self.schema(omit="VHighlight"), self.wheel())

    def test_wheel_mapping_drift_is_rejected(self) -> None:
        mapping = dict(policy.REFERENCE_CONSTRUCTOR_ALIASES)
        mapping.pop("v_highlight")
        with self.assertRaisesRegex(policy.AliasPolicyError, "mapping drift"):
            policy.verify(self.schema(), self.wheel(mapping))

    def test_wheel_acceptance_must_call_runtime_probe(self) -> None:
        with self.assertRaisesRegex(policy.AliasPolicyError, "does not call"):
            policy.verify(self.schema(), self.wheel(call=False))

    def test_main_is_machine_stable(self) -> None:
        stdout = io.StringIO()
        stderr = io.StringIO()
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            status = policy.main(
                [
                    "--schema",
                    str(self.schema()),
                    "--wheel-smoke",
                    str(self.wheel()),
                ]
            )
        self.assertEqual(status, 0)
        self.assertEqual(stderr.getvalue(), "")
        self.assertIn("16 Reference classes verified", stdout.getvalue())


if __name__ == "__main__":
    unittest.main()