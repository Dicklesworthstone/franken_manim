#!/usr/bin/env python3
"""Inventory and validate explicit refusal sites in the Python portal.

The Python skin is allowed to refuse capabilities that the native engine does
not yet expose, but those refusals must stay precise and mechanically visible.
This audit parses the bootstrap as untrusted source, inventories direct
``NotImplementedError`` raises and ``_refuse_unrouted`` calls, and rejects
anonymous/bare failures outside abstract methods.
"""

from __future__ import annotations

import argparse
import ast
import hashlib
import json
import os
import stat
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any

DEFAULT_SOURCE = Path("crates/fmn-python/python/manimlib_bootstrap.py")
MAX_SOURCE_BYTES = 8 * 1024 * 1024
MAX_SITES = 4_096
MAX_OUTPUT_BYTES = 4 * 1024 * 1024
SCHEMA = "fmn.portal.refusals"
SCHEMA_VERSION = 1


class AuditError(ValueError):
    pass


@dataclass(frozen=True)
class Context:
    qualname: str
    abstract: bool


def final_name(node: ast.AST | None) -> str | None:
    if isinstance(node, ast.Name):
        return node.id
    if isinstance(node, ast.Attribute):
        return node.attr
    return None


def compact_expression(node: ast.AST | None, limit: int = 180) -> str | None:
    if node is None:
        return None
    try:
        text = ast.unparse(node)
    except (TypeError, ValueError):
        text = type(node).__name__
    text = " ".join(text.split())
    return text if len(text) <= limit else text[: limit - 1] + "…"


def static_blank_string(node: ast.AST | None) -> bool:
    return isinstance(node, ast.Constant) and isinstance(node.value, str) and not node.value.strip()


def static_empty_collection(node: ast.AST | None) -> bool:
    if isinstance(node, (ast.List, ast.Tuple, ast.Set)):
        return not node.elts
    if isinstance(node, ast.Dict):
        return not node.keys
    return False


def is_abstract(decorators: list[ast.expr]) -> bool:
    return any(final_name(decorator) == "abstractmethod" for decorator in decorators)


def read_source(path: Path) -> tuple[bytes, str]:
    flags = os.O_RDONLY
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    try:
        descriptor = os.open(path, flags)
    except OSError as exc:
        raise AuditError(f"cannot open portal source {path}: {exc}") from exc
    try:
        metadata = os.fstat(descriptor)
        if not stat.S_ISREG(metadata.st_mode):
            raise AuditError(f"portal source is not a regular file: {path}")
        if metadata.st_size > MAX_SOURCE_BYTES:
            raise AuditError(
                f"portal source exceeds the {MAX_SOURCE_BYTES}-byte input limit: {path}"
            )
        with os.fdopen(descriptor, "rb") as handle:
            descriptor = -1
            data = handle.read(MAX_SOURCE_BYTES + 1)
    except Exception:
        if descriptor >= 0:
            os.close(descriptor)
        raise
    if len(data) > MAX_SOURCE_BYTES:
        raise AuditError(
            f"portal source exceeds the {MAX_SOURCE_BYTES}-byte input limit: {path}"
        )
    try:
        return data, data.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise AuditError(f"portal source is not valid UTF-8: {path}: {exc}") from exc


class RefusalVisitor(ast.NodeVisitor):
    def __init__(self) -> None:
        self.scope: list[str] = []
        self.abstract_stack: list[bool] = []
        self.sites: list[dict[str, Any]] = []

    def context(self) -> Context:
        return Context(
            qualname=".".join(self.scope) if self.scope else "<module>",
            abstract=self.abstract_stack[-1] if self.abstract_stack else False,
        )

    def add_site(
        self,
        node: ast.AST,
        *,
        kind: str,
        subject: ast.AST | None,
        detail: ast.AST | None,
        violations: list[str],
        abstract: bool | None = None,
        bare: bool = False,
    ) -> None:
        if len(self.sites) >= MAX_SITES:
            raise AuditError(f"portal refusal inventory exceeds the {MAX_SITES}-site limit")
        context = self.context()
        self.sites.append(
            {
                "line": getattr(node, "lineno", 0),
                "column": getattr(node, "col_offset", 0),
                "qualname": context.qualname,
                "kind": kind,
                "subject": compact_expression(subject),
                "detail": compact_expression(detail),
                "abstract": context.abstract if abstract is None else abstract,
                "bare": bare,
                "violations": violations,
            }
        )

    def visit_ClassDef(self, node: ast.ClassDef) -> None:
        # A class body is a new executable scope. In particular, a class nested
        # inside an abstract method does not inherit that method's permission to
        # use a bare NotImplementedError.
        self.scope.append(node.name)
        self.abstract_stack.append(False)
        self.generic_visit(node)
        self.abstract_stack.pop()
        self.scope.pop()

    def _visit_function(self, node: ast.FunctionDef | ast.AsyncFunctionDef) -> None:
        self.scope.append(node.name)
        self.abstract_stack.append(is_abstract(node.decorator_list))
        self.generic_visit(node)
        self.abstract_stack.pop()
        self.scope.pop()

    def visit_FunctionDef(self, node: ast.FunctionDef) -> None:
        self._visit_function(node)

    def visit_AsyncFunctionDef(self, node: ast.AsyncFunctionDef) -> None:
        self._visit_function(node)

    def visit_Raise(self, node: ast.Raise) -> None:
        exception = node.exc
        call = exception if isinstance(exception, ast.Call) else None
        target = call.func if call is not None else exception
        if final_name(target) == "NotImplementedError":
            context = self.context()
            violations: list[str] = []
            message = call.args[0] if call is not None and call.args else None
            if call is None:
                if not context.abstract:
                    violations.append("bare NotImplementedError outside an abstract method")
            elif message is None:
                violations.append("NotImplementedError call has no message")
            elif static_blank_string(message):
                violations.append("NotImplementedError message is blank")
            self.add_site(
                node,
                kind="not_implemented",
                subject=target,
                detail=message,
                violations=violations,
                bare=call is None,
            )
        self.generic_visit(node)

    def visit_Call(self, node: ast.Call) -> None:
        if final_name(node.func) == "_refuse_unrouted":
            violations: list[str] = []
            subject = node.args[0] if node.args else None
            entries = node.args[1] if len(node.args) > 1 else None
            if subject is None:
                violations.append("_refuse_unrouted call has no subject")
            elif static_blank_string(subject):
                violations.append("_refuse_unrouted subject is blank")
            if entries is None:
                violations.append("_refuse_unrouted call has no entries argument")
            elif static_empty_collection(entries):
                violations.append("_refuse_unrouted entries are statically empty")
            self.add_site(
                node,
                kind="refuse_unrouted",
                subject=subject,
                detail=entries,
                violations=violations,
            )
        self.generic_visit(node)


def build_inventory(path: Path) -> dict[str, Any]:
    source_bytes, source = read_source(path)
    try:
        tree = ast.parse(source, filename=str(path))
    except SyntaxError as exc:
        location = f"{exc.lineno}:{exc.offset}" if exc.lineno is not None else "unknown"
        raise AuditError(f"portal source is not valid Python at {location}: {exc.msg}") from exc
    visitor = RefusalVisitor()
    visitor.visit(tree)
    sites = sorted(
        visitor.sites,
        key=lambda site: (site["line"], site["column"], site["kind"], site["qualname"]),
    )
    violations = [
        {
            "line": site["line"],
            "column": site["column"],
            "qualname": site["qualname"],
            "kind": site["kind"],
            "messages": site["violations"],
        }
        for site in sites
        if site["violations"]
    ]
    direct = [site for site in sites if site["kind"] == "not_implemented"]
    helpers = [site for site in sites if site["kind"] == "refuse_unrouted"]
    return {
        "schema": SCHEMA,
        "version": SCHEMA_VERSION,
        "source": path.as_posix(),
        "source_sha256": hashlib.sha256(source_bytes).hexdigest(),
        "source_bytes": len(source_bytes),
        "counts": {
            "sites": len(sites),
            "not_implemented": len(direct),
            "abstract_bare": sum(site["abstract"] and site["bare"] for site in direct),
            "refuse_unrouted": len(helpers),
            "violations": len(violations),
        },
        "violations": violations,
        "sites": sites,
    }


def bounded(text: str) -> str:
    size = len(text.encode("utf-8"))
    if size > MAX_OUTPUT_BYTES:
        raise AuditError(
            f"portal refusal report exceeds the {MAX_OUTPUT_BYTES}-byte output limit ({size} bytes)"
        )
    return text


def render_json(inventory: dict[str, Any]) -> str:
    return bounded(json.dumps(inventory, sort_keys=True, separators=(",", ":")) + "\n")


def render_markdown(inventory: dict[str, Any]) -> str:
    counts = inventory["counts"]
    lines = [
        "# Python portal refusal inventory",
        "",
        f"- Source: `{inventory['source']}`",
        f"- SHA-256: `{inventory['source_sha256']}`",
        (
            f"- Sites: **{counts['sites']}** total; **{counts['not_implemented']}** direct "
            f"`NotImplementedError`; **{counts['refuse_unrouted']}** `_refuse_unrouted`; "
            f"**{counts['abstract_bare']}** abstract bare; **{counts['violations']}** violations."
        ),
        "",
    ]
    for site in inventory["sites"]:
        subject = f" — `{site['subject']}`" if site["subject"] else ""
        lines.append(
            f"- `{site['line']}:{site['column']}` **{site['kind']}** "
            f"in `{site['qualname']}`{subject}"
        )
        for message in site["violations"]:
            lines.append(f"  - **VIOLATION:** {message}")
    return bounded("\n".join(lines).rstrip() + "\n")


def make_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, default=DEFAULT_SOURCE)
    parser.add_argument("--format", choices=("json", "markdown"), default="json")
    parser.add_argument(
        "--check",
        action="store_true",
        help="exit nonzero and emit no report when anonymous refusal sites exist",
    )
    return parser


def main(argv: list[str] | None = None) -> int:
    args = make_parser().parse_args(argv)
    try:
        inventory = build_inventory(args.source)
    except AuditError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2
    if args.check and inventory["violations"]:
        print(
            f"error: Python portal has {len(inventory['violations'])} anonymous refusal site(s)",
            file=sys.stderr,
        )
        for violation in inventory["violations"]:
            print(
                f"error: {violation['line']}:{violation['column']} "
                f"{violation['qualname']}: {'; '.join(violation['messages'])}",
                file=sys.stderr,
            )
        return 1
    try:
        output = render_json(inventory) if args.format == "json" else render_markdown(inventory)
    except AuditError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2
    sys.stdout.write(output)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
