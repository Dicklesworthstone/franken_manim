#!/usr/bin/env python3
"""Verify the Python/Rust geometry-constructor alias boundary.

The native Rust API intentionally exposes ergonomic snake_case constructor
helpers. The pinned manimlib Reference exports CamelCase classes instead. This
gate keeps those two front doors distinct: every Reference class must remain in
the extracted schema, no Rust-only helper may leak into Python wildcard
exports, the package import guard must enforce the same mapping, and the
installed-wheel smoke must exercise the complete boundary.
"""

from __future__ import annotations

import argparse
import ast
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any

MAX_SCHEMA_BYTES = 16 * 1024 * 1024
MAX_SOURCE_BYTES = 2 * 1024 * 1024

REFERENCE_CONSTRUCTOR_ALIASES: dict[str, tuple[str, str]] = {
    "group": ("manimlib.mobject.mobject", "Group"),
    "v_group": ("manimlib.mobject.types.vectorized_mobject", "VGroup"),
    "vectorized_point": (
        "manimlib.mobject.types.vectorized_mobject",
        "VectorizedPoint",
    ),
    "small_dot": ("manimlib.mobject.geometry", "SmallDot"),
    "sector": ("manimlib.mobject.geometry", "Sector"),
    "vector": ("manimlib.mobject.geometry", "Vector"),
    "polyline": ("manimlib.mobject.geometry", "Polyline"),
    "triangle": ("manimlib.mobject.geometry", "Triangle"),
    "rounded_rectangle": ("manimlib.mobject.geometry", "RoundedRectangle"),
    "svg_mobject": ("manimlib.mobject.svg.svg_mobject", "SVGMobject"),
    "tangent_line": ("manimlib.mobject.geometry", "TangentLine"),
    "curved_arrow": ("manimlib.mobject.geometry", "CurvedArrow"),
    "curved_double_arrow": ("manimlib.mobject.geometry", "CurvedDoubleArrow"),
    "curves_as_submobjects": (
        "manimlib.mobject.types.vectorized_mobject",
        "CurvesAsSubmobjects",
    ),
    "dashed_vmobject": (
        "manimlib.mobject.types.vectorized_mobject",
        "DashedVMobject",
    ),
    "v_highlight": ("manimlib.mobject.types.vectorized_mobject", "VHighlight"),
}

WRAPPER_MAPPING_NAME = "_REFERENCE_CLASS_BY_RUST_HELPER"
WHEEL_MAPPING_NAME = "REFERENCE_CONSTRUCTOR_ALIASES"
WRAPPER_HELPER_NAME = "_rust_helper"
WRAPPER_CLASS_NAME = "_reference_class"
WRAPPER_NATIVE_NAME = "_native"


class AliasPolicyError(ValueError):
    pass


@dataclass(frozen=True)
class Symbol:
    module: str
    name: str
    kind: str
    exported: bool


def read_bounded(path: Path, limit: int) -> str:
    try:
        payload = path.read_bytes()
    except OSError as exc:
        raise AliasPolicyError(f"cannot read {path}: {exc}") from exc
    if len(payload) > limit:
        raise AliasPolicyError(f"{path} exceeds the {limit}-byte limit")
    try:
        return payload.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise AliasPolicyError(f"{path} is not UTF-8: {exc}") from exc


def parse_python(path: Path) -> ast.Module:
    text = read_bounded(path, MAX_SOURCE_BYTES)
    try:
        return ast.parse(text, filename=str(path))
    except SyntaxError as exc:
        raise AliasPolicyError(f"{path}: invalid Python: {exc}") from exc


def parse_schema(path: Path) -> list[Symbol]:
    text = read_bounded(path, MAX_SCHEMA_BYTES)
    if text and not text.endswith("\n"):
        raise AliasPolicyError(f"{path} is missing its final LF")
    in_symbols = False
    symbols: list[Symbol] = []
    seen: set[tuple[str, str]] = set()
    for line_number, raw_line in enumerate(text.splitlines(), 1):
        line = raw_line.strip()
        if line == "[symbols]":
            in_symbols = True
            continue
        if in_symbols and line.startswith("["):
            break
        if not in_symbols or not line or line.startswith("#"):
            continue
        fields = raw_line.split("\t")
        if len(fields) != 6:
            raise AliasPolicyError(
                f"{path}:{line_number}: expected six symbol fields, got {len(fields)}"
            )
        module, name, kind, _origin, exported, _detail = fields
        if exported not in {"0", "1"}:
            raise AliasPolicyError(
                f"{path}:{line_number}: exported must be 0 or 1, got {exported!r}"
            )
        key = (module, name)
        if key in seen:
            raise AliasPolicyError(f"{path}:{line_number}: duplicate symbol {module}.{name}")
        seen.add(key)
        symbols.append(Symbol(module, name, kind, exported == "1"))
    if not in_symbols:
        raise AliasPolicyError(f"{path} has no [symbols] section")
    return symbols


def assigned_literal(tree: ast.Module, path: Path, name: str) -> Any:
    matches: list[ast.expr] = []
    for node in tree.body:
        if not isinstance(node, (ast.Assign, ast.AnnAssign)):
            continue
        targets = node.targets if isinstance(node, ast.Assign) else [node.target]
        if any(isinstance(target, ast.Name) and target.id == name for target in targets):
            if node.value is not None:
                matches.append(node.value)
    if len(matches) != 1:
        raise AliasPolicyError(f"{path}: expected one literal {name}, found {len(matches)}")
    try:
        return ast.literal_eval(matches[0])
    except (ValueError, TypeError, SyntaxError) as exc:
        raise AliasPolicyError(f"{path}: {name} is not a literal value") from exc


def tuple_mapping(tree: ast.Module, path: Path, name: str) -> dict[str, tuple[str, str]]:
    raw = assigned_literal(tree, path, name)
    if not isinstance(raw, dict):
        raise AliasPolicyError(f"{path}: {name} must be a dict")
    normalized: dict[str, tuple[str, str]] = {}
    for key, item in raw.items():
        if (
            not isinstance(key, str)
            or not isinstance(item, tuple)
            or len(item) != 2
            or not all(isinstance(part, str) for part in item)
        ):
            raise AliasPolicyError(f"{path}: malformed {name} entry {key!r}: {item!r}")
        normalized[key] = item
    return normalized


def string_mapping(tree: ast.Module, path: Path, name: str) -> dict[str, str]:
    raw = assigned_literal(tree, path, name)
    if not isinstance(raw, dict):
        raise AliasPolicyError(f"{path}: {name} must be a dict")
    normalized: dict[str, str] = {}
    for key, item in raw.items():
        if not isinstance(key, str) or not isinstance(item, str):
            raise AliasPolicyError(f"{path}: malformed {name} entry {key!r}: {item!r}")
        normalized[key] = item
    return normalized


def mapping_drift(
    label: str,
    expected: dict[str, Any],
    observed: dict[str, Any],
) -> AliasPolicyError:
    missing = sorted(expected.keys() - observed.keys())
    extra = sorted(observed.keys() - expected.keys())
    changed = sorted(
        key for key in expected.keys() & observed.keys() if expected[key] != observed[key]
    )
    return AliasPolicyError(
        f"{label} mapping drift: missing={missing}, extra={extra}, changed={changed}"
    )


def function_names(tree: ast.Module) -> set[str]:
    return {
        node.name
        for node in tree.body
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
    }


def function_calls(tree: ast.Module, function_name: str) -> set[str]:
    functions = [
        node
        for node in tree.body
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
        and node.name == function_name
    ]
    if len(functions) != 1:
        raise AliasPolicyError(
            f"expected exactly one {function_name} function, found {len(functions)}"
        )
    calls: set[str] = set()
    for node in ast.walk(functions[0]):
        if isinstance(node, ast.Call) and isinstance(node.func, ast.Name):
            calls.add(node.func.id)
    return calls


def is_mapping_items_call(node: ast.expr) -> bool:
    return (
        isinstance(node, ast.Call)
        and not node.args
        and not node.keywords
        and isinstance(node.func, ast.Attribute)
        and node.func.attr == "items"
        and isinstance(node.func.value, ast.Name)
        and node.func.value.id == WRAPPER_MAPPING_NAME
    )


def is_hasattr_test(node: ast.expr, attribute_name: str, *, negated: bool) -> bool:
    candidate = node
    if negated:
        if not isinstance(node, ast.UnaryOp) or not isinstance(node.op, ast.Not):
            return False
        candidate = node.operand
    if not isinstance(candidate, ast.Call):
        return False
    return (
        isinstance(candidate.func, ast.Name)
        and candidate.func.id == "hasattr"
        and len(candidate.args) == 2
        and not candidate.keywords
        and isinstance(candidate.args[0], ast.Name)
        and candidate.args[0].id == WRAPPER_NATIVE_NAME
        and isinstance(candidate.args[1], ast.Name)
        and candidate.args[1].id == attribute_name
    )


def raises_import_error(node: ast.If) -> bool:
    return any(
        isinstance(child, ast.Raise)
        and isinstance(child.exc, ast.Call)
        and isinstance(child.exc.func, ast.Name)
        and child.exc.func.id == "ImportError"
        for child in node.body
    )


def verify_wrapper_guard(tree: ast.Module, path: Path) -> None:
    loops = [
        node
        for node in tree.body
        if isinstance(node, ast.For)
        and isinstance(node.target, (ast.Tuple, ast.List))
        and [item.id for item in node.target.elts if isinstance(item, ast.Name)]
        == [WRAPPER_HELPER_NAME, WRAPPER_CLASS_NAME]
        and is_mapping_items_call(node.iter)
    ]
    if len(loops) != 1:
        raise AliasPolicyError(
            f"{path}: expected one import-guard loop over {WRAPPER_MAPPING_NAME}, "
            f"found {len(loops)}"
        )
    direct_ifs = [node for node in loops[0].body if isinstance(node, ast.If)]
    helper_guards = [
        node
        for node in direct_ifs
        if is_hasattr_test(node.test, WRAPPER_HELPER_NAME, negated=False)
        and raises_import_error(node)
    ]
    class_guards = [
        node
        for node in direct_ifs
        if is_hasattr_test(node.test, WRAPPER_CLASS_NAME, negated=True)
        and raises_import_error(node)
    ]
    if len(helper_guards) != 1:
        raise AliasPolicyError(
            f"{path}: import guard must reject every Rust-only helper with ImportError"
        )
    if len(class_guards) != 1:
        raise AliasPolicyError(
            f"{path}: import guard must reject every missing Reference class with ImportError"
        )

    required_cleanup = {
        WRAPPER_MAPPING_NAME,
        WRAPPER_HELPER_NAME,
        WRAPPER_CLASS_NAME,
    }
    cleanup_sets = [
        {target.id for target in node.targets if isinstance(target, ast.Name)}
        for node in tree.body
        if isinstance(node, ast.Delete)
    ]
    if not any(required_cleanup <= names for names in cleanup_sets):
        raise AliasPolicyError(
            f"{path}: import guard does not delete its private mapping and loop variables"
        )


def parse_wheel_smoke(path: Path) -> tuple[dict[str, tuple[str, str]], ast.Module]:
    tree = parse_python(path)
    return tuple_mapping(tree, path, WHEEL_MAPPING_NAME), tree


def parse_wrapper(path: Path) -> tuple[dict[str, str], ast.Module]:
    tree = parse_python(path)
    return string_mapping(tree, path, WRAPPER_MAPPING_NAME), tree


def verify(schema_path: Path, wheel_smoke_path: Path, wrapper_path: Path) -> None:
    symbols = parse_schema(schema_path)
    exported_by_name: dict[str, list[Symbol]] = {}
    by_identity = {(symbol.module, symbol.name): symbol for symbol in symbols}
    for symbol in symbols:
        if symbol.exported:
            exported_by_name.setdefault(symbol.name, []).append(symbol)

    for alias, (module, class_name) in REFERENCE_CONSTRUCTOR_ALIASES.items():
        leaked = exported_by_name.get(alias, [])
        if leaked:
            locations = ", ".join(f"{row.module}.{row.name}" for row in leaked)
            raise AliasPolicyError(
                f"Rust-only helper {alias!r} leaked into Python exports at {locations}"
            )
        row = by_identity.get((module, class_name))
        if row is None:
            raise AliasPolicyError(
                f"Reference class {module}.{class_name} is missing from the schema"
            )
        if row.kind != "class" or not row.exported:
            raise AliasPolicyError(
                f"Reference class {module}.{class_name} is not an exported class"
            )

    wheel_mapping, wheel_tree = parse_wheel_smoke(wheel_smoke_path)
    if wheel_mapping != REFERENCE_CONSTRUCTOR_ALIASES:
        raise mapping_drift("wheel", REFERENCE_CONSTRUCTOR_ALIASES, wheel_mapping)
    required_function = "verify_reference_constructor_aliases"
    if required_function not in function_names(wheel_tree):
        raise AliasPolicyError(f"{wheel_smoke_path}: missing {required_function}")
    calls = function_calls(wheel_tree, "verify_installed_distribution")
    if required_function not in calls:
        raise AliasPolicyError(
            f"{wheel_smoke_path}: verify_installed_distribution does not call "
            f"{required_function}"
        )

    wrapper_mapping, wrapper_tree = parse_wrapper(wrapper_path)
    expected_wrapper = {
        alias: class_name
        for alias, (_module, class_name) in REFERENCE_CONSTRUCTOR_ALIASES.items()
    }
    if wrapper_mapping != expected_wrapper:
        raise mapping_drift("wrapper", expected_wrapper, wrapper_mapping)
    verify_wrapper_guard(wrapper_tree, wrapper_path)


def make_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--schema", type=Path, default=Path("API_SCHEMA.tsv"))
    parser.add_argument(
        "--wheel-smoke",
        type=Path,
        default=Path("crates/fmn-python/tests/wheel_smoke.py"),
    )
    parser.add_argument(
        "--wrapper",
        type=Path,
        default=Path("crates/fmn-python/python/manimlib/__init__.py"),
    )
    return parser


def main(argv: list[str] | None = None) -> int:
    args = make_parser().parse_args(argv)
    try:
        verify(args.schema, args.wheel_smoke, args.wrapper)
    except AliasPolicyError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1
    print(
        "Python geometry alias policy: "
        f"{len(REFERENCE_CONSTRUCTOR_ALIASES)} Reference classes verified; "
        "schema, import guard, and wheel probe agree; no Rust-only helpers exported"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())