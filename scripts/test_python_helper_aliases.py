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
        builders: set[str] | None = None,
        lambda_builders: bool = True,
        resolve_class: bool = True,
        invoke_builder: bool = True,
        recheck_alias: bool = True,
    ) -> Path:
        selected = policy.REFERENCE_CONSTRUCTOR_ALIASES if mapping is None else mapping
        expected_builders = {
            class_name for _module, class_name in selected.values()
        }
        selected_builders = expected_builders if builders is None else builders
        if lambda_builders:
            builder_items = ", ".join(
                f"{name!r}: (lambda: None)" for name in sorted(selected_builders)
            )
        else:
            builder_items = ", ".join(
                f"{name!r}: None" for name in sorted(selected_builders)
            )
        constructor_line = (
            "        constructor = getattr(manimlib, class_name)\n"
            if resolve_class
            else "        constructor = None\n"
        )
        builder_line = (
            "        instance = builders[class_name]()\n"
            if invoke_builder
            else "        instance = None\n"
        )
        alias_line = (
            "        hasattr(manimlib, alias)\n"
            if recheck_alias
            else "        pass\n"
        )
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
            + f"    builders = {{{builder_items}}}\n"
            + "    for alias, (module_name, class_name) in "
            + "REFERENCE_CONSTRUCTOR_ALIASES.items():\n"
            + constructor_line
            + builder_line
            + alias_line
            + "    return None\n\n"
            + "def verify_installed_distribution():\n"
            + "    manimlib = object()\n"
            + call_line,
        )

    def wrapper(
        self,
        mapping: dict[str, str] | None = None,
        *,
        helper_guard: bool = True,
        class_guard: bool = True,
        cleanup: bool = True,
    ) -> Path:
        expected = {
            alias: class_name
            for alias, (_module, class_name) in policy.REFERENCE_CONSTRUCTOR_ALIASES.items()
        }
        selected = expected if mapping is None else mapping
        helper_block = (
            "    if hasattr(_native, _rust_helper):\n"
            "        raise ImportError('helper leaked')\n"
            if helper_guard
            else ""
        )
        class_block = (
            "    if not hasattr(_native, _reference_class):\n"
            "        raise ImportError('class missing')\n"
            if class_guard
            else ""
        )
        cleanup_line = (
            "del _REFERENCE_CLASS_BY_RUST_HELPER, _rust_helper, _reference_class\n"
            if cleanup
            else ""
        )
        return self.write(
            "__init__.py",
            "_REFERENCE_CLASS_BY_RUST_HELPER = "
            + repr(selected)
            + "\n"
            + "for _rust_helper, _reference_class in "
            + "_REFERENCE_CLASS_BY_RUST_HELPER.items():\n"
            + helper_block
            + class_block
            + cleanup_line,
        )

    def verify(
        self,
        *,
        schema: Path | None = None,
        wheel: Path | None = None,
        wrapper: Path | None = None,
    ) -> None:
        policy.verify(
            self.schema() if schema is None else schema,
            self.wheel() if wheel is None else wheel,
            self.wrapper() if wrapper is None else wrapper,
        )

    def test_complete_policy_passes(self) -> None:
        self.verify()

    def test_snake_case_export_leak_is_rejected(self) -> None:
        with self.assertRaisesRegex(policy.AliasPolicyError, "leaked into Python exports"):
            self.verify(schema=self.schema(leaked="small_dot"))

    def test_missing_reference_class_is_rejected(self) -> None:
        with self.assertRaisesRegex(policy.AliasPolicyError, "VHighlight is missing"):
            self.verify(schema=self.schema(omit="VHighlight"))

    def test_wheel_mapping_drift_is_rejected(self) -> None:
        mapping = dict(policy.REFERENCE_CONSTRUCTOR_ALIASES)
        mapping.pop("v_highlight")
        with self.assertRaisesRegex(policy.AliasPolicyError, "wheel mapping drift"):
            self.verify(wheel=self.wheel(mapping))

    def test_wheel_acceptance_must_call_runtime_probe(self) -> None:
        with self.assertRaisesRegex(policy.AliasPolicyError, "does not call"):
            self.verify(wheel=self.wheel(call=False))

    def test_wheel_constructor_table_must_cover_every_reference_class(self) -> None:
        builders = {
            class_name
            for _module, class_name in policy.REFERENCE_CONSTRUCTOR_ALIASES.values()
        }
        builders.remove("VHighlight")
        with self.assertRaisesRegex(policy.AliasPolicyError, "constructor table drift"):
            self.verify(wheel=self.wheel(builders=builders))

    def test_wheel_constructor_table_must_not_add_unrelated_classes(self) -> None:
        builders = {
            class_name
            for _module, class_name in policy.REFERENCE_CONSTRUCTOR_ALIASES.values()
        }
        builders.add("UnrelatedMobject")
        with self.assertRaisesRegex(policy.AliasPolicyError, "constructor table drift"):
            self.verify(wheel=self.wheel(builders=builders))

    def test_wheel_constructor_table_requires_explicit_lambdas(self) -> None:
        with self.assertRaisesRegex(policy.AliasPolicyError, "constructor lambda"):
            self.verify(wheel=self.wheel(lambda_builders=False))

    def test_wheel_probe_must_resolve_each_root_constructor(self) -> None:
        with self.assertRaisesRegex(policy.AliasPolicyError, "does not resolve"):
            self.verify(wheel=self.wheel(resolve_class=False))

    def test_wheel_probe_must_invoke_each_constructor_builder(self) -> None:
        with self.assertRaisesRegex(policy.AliasPolicyError, "does not invoke"):
            self.verify(wheel=self.wheel(invoke_builder=False))

    def test_wheel_probe_must_recheck_alias_absence_after_construction(self) -> None:
        with self.assertRaisesRegex(policy.AliasPolicyError, "does not recheck"):
            self.verify(wheel=self.wheel(recheck_alias=False))

    def test_wrapper_mapping_drift_is_rejected(self) -> None:
        mapping = {
            alias: class_name
            for alias, (_module, class_name) in policy.REFERENCE_CONSTRUCTOR_ALIASES.items()
        }
        mapping["small_dot"] = "Dot"
        with self.assertRaisesRegex(policy.AliasPolicyError, "wrapper mapping drift"):
            self.verify(wrapper=self.wrapper(mapping))

    def test_wrapper_must_reject_helper_leaks(self) -> None:
        with self.assertRaisesRegex(policy.AliasPolicyError, "reject every Rust-only helper"):
            self.verify(wrapper=self.wrapper(helper_guard=False))

    def test_wrapper_must_reject_missing_reference_classes(self) -> None:
        with self.assertRaisesRegex(policy.AliasPolicyError, "reject every missing Reference"):
            self.verify(wrapper=self.wrapper(class_guard=False))

    def test_wrapper_must_clean_private_guard_state(self) -> None:
        with self.assertRaisesRegex(policy.AliasPolicyError, "does not delete"):
            self.verify(wrapper=self.wrapper(cleanup=False))

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
                    "--wrapper",
                    str(self.wrapper()),
                ]
            )
        self.assertEqual(status, 0)
        self.assertEqual(stderr.getvalue(), "")
        self.assertIn("16 Reference classes verified", stdout.getvalue())
        self.assertIn("constructed-wheel probe agree", stdout.getvalue())


if __name__ == "__main__":
    unittest.main()