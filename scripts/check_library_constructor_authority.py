#!/usr/bin/env python3
"""Audit every fmn-library helper-to-manimlib constructor authority path."""

from __future__ import annotations

import argparse
import ast
import json
import re
import sys
from collections import Counter
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
PORTAL_PYTHON = ROOT / "crates" / "fmn-python" / "python"
if str(PORTAL_PYTHON) not in sys.path:
    sys.path.insert(0, str(PORTAL_PYTHON))

from fmn_python.library_constructor_authority import (  # noqa: E402
    LIBRARY_CONSTRUCTOR_AUTHORITIES,
    REFERENCE_SYMBOL_BY_RUST_HELPER,
)

SCHEMA = "fmn.library-constructor-authority-audit"
SCHEMA_VERSION = 1
MAX_SOURCE_BYTES = 8 * 1024 * 1024
MAX_OUTPUT_BYTES = 256 * 1024
BOOTSTRAP_PATH = Path("crates/fmn-python/python/manimlib_bootstrap.py")
BRIDGE_PATH = Path("crates/fmn-python/src/lib.rs")
WHEEL_SMOKE_PATH = Path("crates/fmn-python/tests/wheel_smoke.py")
WHEEL_MAPPING_NAME = "REFERENCE_CONSTRUCTOR_ALIASES"


class AuthorityAuditError(ValueError):
    def __init__(
        self,
        code: str,
        detail: str,
        *,
        helper: str | None = None,
        path: Path | None = None,
    ) -> None:
        self.code = code
        self.detail = detail
        self.helper = helper
        self.path = path
        super().__init__(detail)


def _error(
    code: str,
    detail: str,
    *,
    helper: str | None = None,
    path: Path | None = None,
) -> AuthorityAuditError:
    return AuthorityAuditError(code, detail, helper=helper, path=path)


def _read_bounded(path: Path) -> str:
    try:
        with path.open("rb") as handle:
            payload = handle.read(MAX_SOURCE_BYTES + 1)
    except OSError as exc:
        raise _error(
            "source-read-failed",
            f"cannot read {path}: {exc}",
            path=path,
        ) from exc
    if len(payload) > MAX_SOURCE_BYTES:
        raise _error(
            "source-too-large",
            f"{path} exceeds the {MAX_SOURCE_BYTES}-byte limit",
            path=path,
        )
    try:
        return payload.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise _error(
            "source-not-utf8",
            f"{path} is not valid UTF-8: {exc}",
            path=path,
        ) from exc


def _parse_python(text: str, path: Path) -> ast.Module:
    try:
        return ast.parse(text, filename=str(path))
    except SyntaxError as exc:
        raise _error(
            "python-parse-failed",
            f"{path} is not valid Python: {exc}",
            path=path,
        ) from exc


def _top_level_class(
    tree: ast.Module,
    *,
    name: str,
    path: Path,
    helper: str,
) -> ast.ClassDef:
    matches = [
        node
        for node in tree.body
        if isinstance(node, ast.ClassDef) and node.name == name
    ]
    if len(matches) != 1:
        raise _error(
            "python-class-count",
            f"{path}: expected one top-level class {name}, found {len(matches)}",
            helper=helper,
            path=path,
        )
    return matches[0]


def _node_source(text: str, node: ast.AST) -> str:
    if not hasattr(node, "lineno") or not hasattr(node, "end_lineno"):
        return ""
    lines = text.splitlines(keepends=True)
    return "".join(lines[node.lineno - 1 : node.end_lineno])


def _base_spelling(node: ast.ClassDef) -> str:
    return ", ".join(ast.unparse(base) for base in node.bases)


def _rust_function_block(
    text: str,
    *,
    name: str,
    path: Path,
    helper: str,
    top_level_public: bool,
) -> str:
    if top_level_public:
        pattern = re.compile(
            rf"(?m)^pub\s+fn\s+{re.escape(name)}(?:<[^>\n]+>)?\s*\("
        )
    else:
        pattern = re.compile(
            rf"(?m)^(?P<indent>[ \t]+)fn\s+"
            rf"{re.escape(name)}(?:<[^>\n]+>)?\s*\("
        )
    matches = list(pattern.finditer(text))
    if len(matches) != 1:
        kind = "top-level public" if top_level_public else "bridge"
        raise _error(
            "rust-function-count",
            f"{path}: expected one {kind} function {name}, found {len(matches)}",
            helper=helper,
            path=path,
        )
    match = matches[0]
    indent = "" if top_level_public else match.group("indent")
    opening = text.find("{", match.end())
    if opening < 0:
        raise _error(
            "rust-function-unclosed",
            f"{path}: function {name} has no body opening brace",
            helper=helper,
            path=path,
        )
    closing = re.search(
        rf"(?m)^{re.escape(indent)}}}\s*$",
        text[opening + 1 :],
    )
    if closing is None:
        raise _error(
            "rust-function-unclosed",
            f"{path}: function {name} has no scoped closing brace",
            helper=helper,
            path=path,
        )
    end = opening + 1 + closing.end()
    return text[match.start() : end]


def _literal_assignment(
    tree: ast.Module,
    *,
    name: str,
    path: Path,
) -> Any:
    values: list[ast.expr] = []
    for node in tree.body:
        if isinstance(node, ast.Assign):
            targets = node.targets
            value = node.value
        elif isinstance(node, ast.AnnAssign):
            targets = [node.target]
            value = node.value
        else:
            continue
        if value is None:
            continue
        if any(
            isinstance(target, ast.Name) and target.id == name
            for target in targets
        ):
            values.append(value)
    if len(values) != 1:
        raise _error(
            "wheel-mapping-count",
            f"{path}: expected one {name} assignment, found {len(values)}",
            path=path,
        )
    try:
        return ast.literal_eval(values[0])
    except (TypeError, ValueError, SyntaxError) as exc:
        raise _error(
            "wheel-mapping-not-literal",
            f"{path}: {name} must be a literal mapping",
            path=path,
        ) from exc


def _wheel_mapping(text: str, path: Path) -> dict[str, tuple[str, str]]:
    raw = _literal_assignment(
        _parse_python(text, path),
        name=WHEEL_MAPPING_NAME,
        path=path,
    )
    if not isinstance(raw, dict):
        raise _error(
            "wheel-mapping-type",
            f"{path}: {WHEEL_MAPPING_NAME} must be a dict",
            path=path,
        )
    result: dict[str, tuple[str, str]] = {}
    for helper, symbol in raw.items():
        if (
            not isinstance(helper, str)
            or not isinstance(symbol, tuple)
            or len(symbol) != 2
            or not all(isinstance(part, str) and part for part in symbol)
        ):
            raise _error(
                "wheel-mapping-entry",
                f"{path}: malformed constructor alias entry {helper!r}: {symbol!r}",
                path=path,
            )
        result[helper] = symbol
    return result


def audit(root: Path = ROOT) -> dict[str, Any]:
    root = root.resolve()
    bootstrap_path = root / BOOTSTRAP_PATH
    bridge_path = root / BRIDGE_PATH
    wheel_path = root / WHEEL_SMOKE_PATH
    bootstrap_text = _read_bounded(bootstrap_path)
    bridge_text = _read_bounded(bridge_path)
    wheel_text = _read_bounded(wheel_path)
    bootstrap_tree = _parse_python(bootstrap_text, bootstrap_path)

    rust_sources = {
        record.rust_source: _read_bounded(root / record.rust_source)
        for record in LIBRARY_CONSTRUCTOR_AUTHORITIES
    }

    records = []
    for record in LIBRARY_CONSTRUCTOR_AUTHORITIES:
        helper = record.rust_helper
        rust_path = root / record.rust_source
        rust_block = _rust_function_block(
            rust_sources[record.rust_source],
            name=record.rust_function,
            path=rust_path,
            helper=helper,
            top_level_public=True,
        )
        if record.rust_authority_token not in rust_block:
            raise _error(
                "rust-authority-missing",
                f"{rust_path}: {record.rust_function} lacks "
                f"{record.rust_authority_token!r}",
                helper=helper,
                path=rust_path,
            )

        class_node = _top_level_class(
            bootstrap_tree,
            name=record.reference_class,
            path=bootstrap_path,
            helper=helper,
        )
        observed_base = _base_spelling(class_node)
        if observed_base != record.python_base:
            raise _error(
                "python-base-mismatch",
                f"{bootstrap_path}: {record.reference_class} bases are "
                f"{observed_base!r}, expected {record.python_base!r}",
                helper=helper,
                path=bootstrap_path,
            )
        class_source = _node_source(bootstrap_text, class_node)
        if record.python_authority_token not in class_source:
            raise _error(
                "python-authority-missing",
                f"{bootstrap_path}: {record.reference_class} lacks "
                f"{record.python_authority_token!r}",
                helper=helper,
                path=bootstrap_path,
            )

        if (
            record.native_builder is not None
            and record.python_authority_class != record.reference_class
        ):
            authority_node = _top_level_class(
                bootstrap_tree,
                name=record.python_authority_class,
                path=bootstrap_path,
                helper=helper,
            )
            authority_source = _node_source(bootstrap_text, authority_node)
            authority_call = f"self.{record.native_builder}("
            if authority_call not in authority_source:
                raise _error(
                    "python-native-builder-missing",
                    f"{bootstrap_path}: {record.python_authority_class} lacks "
                    f"{authority_call!r}",
                    helper=helper,
                    path=bootstrap_path,
                )

        if record.bridge_function is not None:
            bridge_block = _rust_function_block(
                bridge_text,
                name=record.bridge_function,
                path=bridge_path,
                helper=helper,
                top_level_public=False,
            )
            if record.bridge_authority_token not in bridge_block:
                raise _error(
                    "bridge-authority-missing",
                    f"{bridge_path}: {record.bridge_function} lacks "
                    f"{record.bridge_authority_token!r}",
                    helper=helper,
                    path=bridge_path,
                )

        records.append(
            {
                "helper": helper,
                "reference": (
                    f"{record.reference_module}:{record.reference_class}"
                ),
                "binding_kind": record.binding_kind,
            }
        )

    expected_wheel = dict(REFERENCE_SYMBOL_BY_RUST_HELPER)
    observed_wheel = _wheel_mapping(wheel_text, wheel_path)
    if observed_wheel != expected_wheel:
        missing = sorted(expected_wheel.keys() - observed_wheel.keys())
        extra = sorted(observed_wheel.keys() - expected_wheel.keys())
        changed = sorted(
            helper
            for helper in expected_wheel.keys() & observed_wheel.keys()
            if expected_wheel[helper] != observed_wheel[helper]
        )
        raise _error(
            "wheel-mapping-drift",
            f"{wheel_path}: missing={missing}, extra={extra}, changed={changed}",
            path=wheel_path,
        )

    kind_counts = Counter(
        record.binding_kind for record in LIBRARY_CONSTRUCTOR_AUTHORITIES
    )
    return {
        "schema": SCHEMA,
        "version": SCHEMA_VERSION,
        "ok": True,
        "counts": {
            "authorities": len(records),
            "rust_sources": len(rust_sources),
            "native_bridges": sum(
                record.bridge_function is not None
                for record in LIBRARY_CONSTRUCTOR_AUTHORITIES
            ),
            "python_owned": sum(
                record.bridge_function is None
                for record in LIBRARY_CONSTRUCTOR_AUTHORITIES
            ),
        },
        "binding_kinds": {
            name: kind_counts[name] for name in sorted(kind_counts)
        },
        "records": records,
    }


def render_json(report: dict[str, Any]) -> str:
    text = json.dumps(report, sort_keys=True, separators=(",", ":")) + "\n"
    if len(text.encode("utf-8")) > MAX_OUTPUT_BYTES:
        raise _error(
            "output-too-large",
            f"audit output exceeds the {MAX_OUTPUT_BYTES}-byte limit",
        )
    return text


def make_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--robot", action="store_true")
    return parser


def main(argv: list[str] | None = None) -> int:
    args = make_parser().parse_args(argv)
    try:
        report = audit(args.root)
    except AuthorityAuditError as exc:
        if args.robot:
            payload = {
                "schema": SCHEMA,
                "version": SCHEMA_VERSION,
                "ok": False,
                "error": {
                    "code": exc.code,
                    "detail": exc.detail[:4096],
                },
            }
            if exc.helper is not None:
                payload["error"]["helper"] = exc.helper
            if exc.path is not None:
                payload["error"]["path"] = str(exc.path)
            sys.stdout.write(render_json(payload))
        else:
            prefix = f"{exc.helper}: " if exc.helper else ""
            print(
                f"library constructor authority audit: FAIL: "
                f"{exc.code}: {prefix}{exc.detail}",
                file=sys.stderr,
            )
        return 2
    if args.robot:
        sys.stdout.write(render_json(report))
    else:
        counts = report["counts"]
        print(
            "library constructor authority audit: PASS; "
            f"{counts['authorities']} helpers, "
            f"{counts['native_bridges']} native bridge routes, "
            f"{counts['python_owned']} Python-owned identity routes"
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
