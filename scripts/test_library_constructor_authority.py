from __future__ import annotations

import contextlib
import io
import json
import shutil
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import check_library_constructor_authority as audit


class LibraryConstructorAuthorityTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        relative_paths = {
            audit.BOOTSTRAP_PATH,
            audit.BRIDGE_PATH,
            audit.WHEEL_SMOKE_PATH,
            *(
                Path(record.rust_source)
                for record in audit.LIBRARY_CONSTRUCTOR_AUTHORITIES
            ),
        }
        for relative in relative_paths:
            source = audit.ROOT / relative
            destination = self.root / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, destination)

    def mutate(self, relative: Path | str, old: str, new: str) -> None:
        path = self.root / relative
        text = path.read_text(encoding="utf-8")
        self.assertIn(old, text, f"fixture token absent: {old!r}")
        path.write_text(text.replace(old, new, 1), encoding="utf-8")

    def assert_audit_error(
        self,
        code: str,
        *,
        helper: str | None = None,
    ) -> audit.AuthorityAuditError:
        with self.assertRaises(audit.AuthorityAuditError) as raised:
            audit.audit(self.root)
        self.assertEqual(raised.exception.code, code)
        if helper is not None:
            self.assertEqual(raised.exception.helper, helper)
        return raised.exception

    def test_complete_authority_contract_passes(self) -> None:
        report = audit.audit(self.root)
        self.assertTrue(report["ok"])
        self.assertEqual(report["counts"]["authorities"], 16)
        self.assertEqual(report["counts"]["rust_sources"], 5)
        self.assertEqual(report["counts"]["native_bridges"], 13)
        self.assertEqual(report["counts"]["python_owned"], 3)
        self.assertEqual(
            [record["helper"] for record in report["records"]],
            sorted(audit.REFERENCE_SYMBOL_BY_RUST_HELPER),
        )
        self.assertEqual(
            report["binding_kinds"],
            {
                "direct_native_builder": 4,
                "hardened_native_builder": 1,
                "inherited_native_builder": 4,
                "native_algorithm_python_shell": 1,
                "native_equivalent_builder": 3,
                "python_identity_container": 2,
                "python_reference_composition": 1,
            },
        )

    def test_missing_rust_helper_authority_fails_scoped(self) -> None:
        self.mutate(
            "crates/fmn-library/src/arc.rs",
            "Dot::small().point(point)",
            "Dot::new().point(point)",
        )
        self.assert_audit_error(
            "rust-authority-missing",
            helper="small_dot",
        )

    def test_missing_bridge_authority_fails_scoped(self) -> None:
        self.mutate(
            audit.BRIDGE_PATH,
            "fmn_library::vmobject::vectorized_point(location)",
            "fmn_library::VMobject::from_points(vec![location])",
        )
        self.assert_audit_error(
            "bridge-authority-missing",
            helper="vectorized_point",
        )

    def test_python_constructor_route_and_base_fail_scoped(self) -> None:
        self.mutate(
            audit.BOOTSTRAP_PATH,
            "super().__init__(n=3, **kwargs)",
            "super().__init__(n=4, **kwargs)",
        )
        self.assert_audit_error(
            "python-authority-missing",
            helper="triangle",
        )

        self.mutate(
            audit.BOOTSTRAP_PATH,
            "class Triangle(RegularPolygon):",
            "class Triangle(VMobject):",
        )
        self.assert_audit_error(
            "python-base-mismatch",
            helper="triangle",
        )

    def test_inherited_parent_must_still_call_native_builder(self) -> None:
        self.mutate(
            audit.BOOTSTRAP_PATH,
            "self._build_dot(",
            "self._build_circle(",
        )
        self.assert_audit_error(
            "python-native-builder-missing",
            helper="small_dot",
        )

    def test_wheel_alias_map_drift_fails_closed(self) -> None:
        self.mutate(
            audit.WHEEL_SMOKE_PATH,
            '"vector": ("manimlib.mobject.geometry", "Vector"),',
            '"vector": ("manimlib.mobject.geometry", "WrongVector"),',
        )
        error = self.assert_audit_error("wheel-mapping-drift")
        self.assertIn("changed=['vector']", error.detail)

    def test_duplicate_bridge_function_is_rejected(self) -> None:
        path = self.root / audit.BRIDGE_PATH
        text = path.read_text(encoding="utf-8")
        marker = "    fn _build_vectorized_point<'py>("
        self.assertIn(marker, text)
        path.write_text(text + "\n" + marker + "\n    }\n", encoding="utf-8")
        self.assert_audit_error(
            "rust-function-count",
            helper="vectorized_point",
        )

    def test_bounded_source_read_is_typed(self) -> None:
        with mock.patch.object(audit, "MAX_SOURCE_BYTES", 8):
            self.assert_audit_error("source-too-large")

    def test_cli_human_and_robot_outputs_are_deterministic(self) -> None:
        for robot in (False, True):
            with self.subTest(robot=robot):
                stdout = io.StringIO()
                stderr = io.StringIO()
                argv = ["--root", str(self.root)]
                if robot:
                    argv.append("--robot")
                with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(
                    stderr
                ):
                    code = audit.main(argv)
                self.assertEqual(code, 0)
                self.assertEqual(stderr.getvalue(), "")
                if robot:
                    payload = json.loads(stdout.getvalue())
                    self.assertEqual(payload["schema"], audit.SCHEMA)
                    self.assertEqual(payload["version"], audit.SCHEMA_VERSION)
                    self.assertEqual(payload["counts"]["authorities"], 16)
                else:
                    self.assertIn(
                        "library constructor authority audit: PASS",
                        stdout.getvalue(),
                    )
                    self.assertIn("16 helpers", stdout.getvalue())

    def test_cli_robot_failure_has_typed_error(self) -> None:
        self.mutate(
            audit.WHEEL_SMOKE_PATH,
            '"vector": ("manimlib.mobject.geometry", "Vector"),',
            '"vector": ("manimlib.mobject.geometry", "WrongVector"),',
        )
        stdout = io.StringIO()
        stderr = io.StringIO()
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            code = audit.main(["--root", str(self.root), "--robot"])
        self.assertEqual(code, 2)
        self.assertEqual(stderr.getvalue(), "")
        payload = json.loads(stdout.getvalue())
        self.assertFalse(payload["ok"])
        self.assertEqual(payload["error"]["code"], "wheel-mapping-drift")


if __name__ == "__main__":
    unittest.main()
