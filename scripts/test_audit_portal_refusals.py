from __future__ import annotations

import contextlib
import io
import json
import os
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import audit_portal_refusals as audit


VALID_SOURCE = '''
import abc as _abc


def _refuse_unrouted(subject, entries):
    return subject, entries


class Base:
    @_abc.abstractmethod
    def get_bounds(self):
        raise NotImplementedError


class Concrete:
    def configure(self, enabled=False):
        _refuse_unrouted(type(self).__name__ + "()", [("enabled", enabled)])
        if enabled:
            raise NotImplementedError(
                "Concrete.configure(enabled=True) requires the native configuration seam"
            )
        return self
'''.lstrip()


class PortalRefusalAuditTests(unittest.TestCase):
    def fixture(self, source: str = VALID_SOURCE) -> Path:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        path = Path(directory.name) / "bootstrap.py"
        path.write_text(source, encoding="utf-8")
        return path

    def test_valid_named_and_abstract_refusals_are_inventoried(self) -> None:
        path = self.fixture()
        inventory = audit.build_inventory(path)
        self.assertEqual(inventory["schema"], "fmn.portal.refusals")
        self.assertEqual(inventory["version"], 1)
        self.assertGreater(inventory["ast_nodes"], 0)
        self.assertEqual(inventory["counts"]["sites"], 3)
        self.assertEqual(inventory["counts"]["not_implemented"], 2)
        self.assertEqual(inventory["counts"]["abstract_bare"], 1)
        self.assertEqual(inventory["counts"]["refuse_unrouted"], 1)
        self.assertEqual(inventory["counts"]["violations"], 0)
        self.assertEqual(
            [site["qualname"] for site in inventory["sites"]],
            ["Base.get_bounds", "Concrete.configure", "Concrete.configure"],
        )

    def test_bare_blank_and_missing_message_refusals_fail_closed(self) -> None:
        source = '''
def bare():
    raise NotImplementedError


def empty_call():
    raise NotImplementedError()


def blank_call():
    raise NotImplementedError("   ")
'''.lstrip()
        path = self.fixture(source)
        inventory = audit.build_inventory(path)
        messages = [
            message
            for violation in inventory["violations"]
            for message in violation["messages"]
        ]
        self.assertIn("bare NotImplementedError outside an abstract method", messages)
        self.assertIn("NotImplementedError call has no message", messages)
        self.assertIn("NotImplementedError message is blank", messages)

        stdout = io.StringIO()
        stderr = io.StringIO()
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            status = audit.main(["--source", str(path), "--check"])
        self.assertEqual(status, 1)
        self.assertEqual(stdout.getvalue(), "")
        self.assertIn("3 anonymous refusal site(s)", stderr.getvalue())

    def test_refuse_unrouted_requires_a_named_subject_and_nonempty_entries(self) -> None:
        source = '''
def _refuse_unrouted(subject, entries=None):
    return subject, entries


def missing_subject():
    _refuse_unrouted()


def missing_entries():
    _refuse_unrouted("Thing()")


def blank_subject():
    _refuse_unrouted("", [("x", True)])


def empty_entries():
    _refuse_unrouted("Thing()", [])
'''.lstrip()
        inventory = audit.build_inventory(self.fixture(source))
        messages = [
            message
            for violation in inventory["violations"]
            for message in violation["messages"]
        ]
        self.assertIn("_refuse_unrouted call has no subject", messages)
        self.assertIn("_refuse_unrouted call has no entries argument", messages)
        self.assertIn("_refuse_unrouted subject is blank", messages)
        self.assertIn("_refuse_unrouted entries are statically empty", messages)

    def test_refuse_unrouted_keyword_arguments_are_supported(self) -> None:
        source = '''
def _refuse_unrouted(class_name, entries):
    return class_name, entries


def configure(enabled=False):
    _refuse_unrouted(
        class_name="Thing()",
        entries=[("enabled", enabled)],
    )
'''.lstrip()
        inventory = audit.build_inventory(self.fixture(source))
        self.assertEqual(inventory["counts"]["refuse_unrouted"], 1)
        self.assertEqual(inventory["counts"]["violations"], 0)
        site = inventory["sites"][0]
        self.assertEqual(site["subject"], "'Thing()'")
        self.assertIn("enabled", site["detail"])

    def test_abstractness_does_not_leak_into_nested_class_bodies(self) -> None:
        source = '''
import abc


class Outer:
    @abc.abstractmethod
    def method(self):
        class Inner:
            raise NotImplementedError
        raise NotImplementedError
'''.lstrip()
        inventory = audit.build_inventory(self.fixture(source))
        sites = [site for site in inventory["sites"] if site["kind"] == "not_implemented"]
        self.assertEqual(len(sites), 2)
        inner = next(site for site in sites if site["qualname"] == "Outer.method.Inner")
        method = next(site for site in sites if site["qualname"] == "Outer.method")
        self.assertFalse(inner["abstract"])
        self.assertTrue(inner["violations"])
        self.assertTrue(method["abstract"])
        self.assertFalse(method["violations"])

    def test_json_output_is_canonical_and_deterministic(self) -> None:
        path = self.fixture()
        outputs: list[str] = []
        for _ in range(2):
            stdout = io.StringIO()
            with contextlib.redirect_stdout(stdout):
                status = audit.main(
                    ["--source", str(path), "--format", "json", "--check"]
                )
            self.assertEqual(status, 0)
            outputs.append(stdout.getvalue())
        self.assertEqual(outputs[0], outputs[1])
        payload = json.loads(outputs[0])
        self.assertEqual(
            outputs[0],
            json.dumps(payload, sort_keys=True, separators=(",", ":")) + "\n",
        )

    def test_output_ast_and_site_budgets_refuse_before_payload(self) -> None:
        path = self.fixture()
        stdout = io.StringIO()
        stderr = io.StringIO()
        with mock.patch.object(audit, "MAX_OUTPUT_BYTES", 32):
            with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                status = audit.main(["--source", str(path), "--format", "json"])
        self.assertEqual(status, 2)
        self.assertEqual(stdout.getvalue(), "")
        self.assertIn("32-byte output limit", stderr.getvalue())

        with mock.patch.object(audit, "MAX_AST_NODES", 1):
            with self.assertRaisesRegex(audit.AuditError, "1-node AST limit"):
                audit.build_inventory(path)

        with mock.patch.object(audit, "MAX_SITES", 1):
            with self.assertRaisesRegex(audit.AuditError, "1-site limit"):
                audit.build_inventory(path)

    def test_syntax_and_utf8_errors_are_typed_input_failures(self) -> None:
        syntax = self.fixture("def broken(:\n")
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr):
            status = audit.main(["--source", str(syntax), "--check"])
        self.assertEqual(status, 2)
        self.assertIn("not valid Python", stderr.getvalue())

        binary = self.fixture()
        binary.write_bytes(b"\xff\n")
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr):
            status = audit.main(["--source", str(binary), "--check"])
        self.assertEqual(status, 2)
        self.assertIn("not valid UTF-8", stderr.getvalue())

    @unittest.skipUnless(hasattr(os, "O_NOFOLLOW"), "host lacks no-follow open")
    def test_source_symlink_is_refused(self) -> None:
        target = self.fixture()
        link = target.with_name("linked.py")
        try:
            link.symlink_to(target)
        except OSError as exc:
            self.skipTest(f"host cannot create symlinks: {exc}")
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr):
            status = audit.main(["--source", str(link), "--check"])
        self.assertEqual(status, 2)
        self.assertIn("cannot open portal source", stderr.getvalue())


if __name__ == "__main__":
    unittest.main()
