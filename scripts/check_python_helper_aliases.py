#!/usr/bin/env python3
"""Verify the Python/Rust geometry-constructor alias boundary.

The native Rust API intentionally exposes ergonomic snake_case constructor
helpers. The pinned manimlib Reference exports CamelCase classes instead. This
gate keeps those two front doors distinct: every Reference class must remain in
the extracted schema, no Rust-only helper may leak into Python wildcard
exports, and the installed-wheel smoke must exercise the same complete mapping.
"""

from __future__ import annotations

import argparse
import ast
import sys
from dataclasses import dataclass
from pathlib import Path

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


def literal_mapping(tree: ast.Module, path: Path) -> dict[str, tuple[str, str]]:
    for node in tree.body:
        if not isinstance(node, (ast.Assign, ast.AnnAssign)):
            continue
        targets = node.targets if isinstance(node, ast.Assign) else [node.target]
        if not any(
            isinstance(target, ast.Name)
            and target.id == "REFERENCE_CONSTRUCTOR_ALIASES"
            for target in targets
        ):
            continue
        value = node.value
        if value is None:
            break
        try:
            raw = ast.literal_eval(value)
        except (ValueError, TypeError, SyntaxError) as exc:
            raise AliasPolicyError(
                f"{path}: REFERENCE_CONSTRUCTOR_ALIASES is not a literal mapping"
            ) from exc
        if not isinstance(raw, dict):
            raise AliasPolicyError(
                f"{path}: REFERENCE_CONSTRUCTOR_ALIASES must be a dict"
            )
        normalized: dict[str, tuple[str, str]] = {}
        for key, item in raw.items():
            if (
                not isinstance(key, str)
                or not isinstance(item, tuple)
                or len(item) != 2
                or not all(isinstance(part, str) for part in item)
            ):
                raise AliasPolicyError(
                    f"{path}: malformed alias mapping entry {key!r}: {item!r}"
                )
            normalized[key] = item
        return normalized
    raise AliasPolicyError(f"{path}: missing REFERENCE_CONSTRUCTOR_ALIASES")


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


def parse_wheel_smoke(path: Path) -> tuple[dict[str, tuple[str, str]], ast.Module]:
    text = read_bounded(path, MAX_SOURCE_BYTES)
    try:
        tree = ast.parse(text, filename=str(path))
    except SyntaxError as exc:
        raise AliasPolicyError(f"{path}: invalid Python: {exc}") from exc
    return literal_mapping(tree, path), tree


def verify(schema_path: Path, wheel_smoke_path: Path) -> None:
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

    wheel_mapping, tree = parse_wheel_smoke(wheel_smoke_path)
    if wheel_mapping != REFERENCE_CONSTRUCTOR_ALIASES:
        missing = sorted(REFERENCE_CONSTRUCTOR_ALIASES.keys() - wheel_mapping.keys())
        extra = sorted(wheel_mapping.keys() - REFERENCE_CONSTRUCTOR_ALIASES.keys())
        changed = sorted(
            key
            for key in REFERENCE_CONSTRUCTOR_ALIASES.keys() & wheel_mapping.keys()
            if REFERENCE_CONSTRUCTOR_ALIASES[key] != wheel_mapping[key]
        )
        raise AliasPolicyError(
            "wheel alias mapping drift: "
            f"missing={missing}, extra={extra}, changed={changed}"
        )
    required_function = "verify_reference_constructor_aliases"
    if required_function not in function_names(tree):
        raise AliasPolicyError(f"{wheel_smoke_path}: missing {required_function}")
    calls = function_calls(tree, "verify_installed_distribution")
    if required_function not in calls:
        raise AliasPolicyError(
            f"{wheel_smoke_path}: verify_installed_distribution does not call "
            f"{required_function}"
        )


def make_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--schema", type=Path, default=Path("API_SCHEMA.tsv"))
    parser.add_argument(
        "--wheel-smoke",
        type=Path,
        default=Path("crates/fmn-python/tests/wheel_smoke.py"),
    )
    return parser


def main(argv: list[str] | None = None) -> int:
    args = make_parser().parse_args(argv)
    try:
        verify(args.schema, args.wheel_smoke)
    except AliasPolicyError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1
    print(
        "Python geometry alias policy: "
        f"{len(REFERENCE_CONSTRUCTOR_ALIASES)} Reference classes verified; "
        "no Rust-only helpers exported"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())