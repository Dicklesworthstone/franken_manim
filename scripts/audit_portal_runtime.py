#!/usr/bin/env python3
"""Audit reviewed portal parity claims against an actually imported wheel.

This deliberately runs at the Python API boundary rather than inferring runtime
truth from the bootstrap source. Schema fallbacks already mark themselves with
``_fmn_schema_placeholder``; this tool turns that marker into a bounded,
machine-readable contradiction check for every ``same`` or ``improved`` row in
API_OVERLAY.tsv.
"""

from __future__ import annotations

import argparse
import importlib
import json
import sys
from dataclasses import dataclass
from pathlib import Path
from types import ModuleType
from typing import Any, Callable

SCHEMA = "fmn.portal.runtime-audit"
SCHEMA_VERSION = 1
MAX_OVERLAY_BYTES = 8 * 1024 * 1024
MAX_STATUS_ROWS = 100_000
MAX_OUTPUT_BYTES = 4 * 1024 * 1024
IMPLEMENTED_STATUSES = frozenset({"same", "improved"})
KNOWN_STATUSES = frozenset({"same", "improved", "tiered", "excluded", "unreviewed"})


class AuditError(ValueError):
    pass


@dataclass(frozen=True)
class StatusRow:
    symbol: str
    status: str
    evidence: str
    tests: str
    notes: str

    @property
    def module_name(self) -> str:
        return self.symbol.split(":", 1)[0]

    @property
    def qualified(self) -> str:
        return self.symbol.split(":", 1)[1]


def _read_bounded(path: Path) -> str:
    try:
        with path.open("rb") as handle:
            payload = handle.read(MAX_OVERLAY_BYTES + 1)
    except OSError as exc:
        raise AuditError(f"cannot read overlay {path}: {exc}") from exc
    if len(payload) > MAX_OVERLAY_BYTES:
        raise AuditError(f"overlay exceeds {MAX_OVERLAY_BYTES}-byte limit")
    try:
        return payload.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise AuditError(f"overlay is not valid UTF-8: {exc}") from exc


def parse_status_rows(text: str) -> tuple[StatusRow, ...]:
    section: str | None = None
    rows: list[StatusRow] = []
    seen: set[str] = set()
    for line_number, raw_line in enumerate(text.splitlines(), 1):
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("[") and line.endswith("]"):
            section = line[1:-1]
            continue
        if section != "status":
            continue
        columns = raw_line.split("\t")
        if len(columns) != 5:
            raise AuditError(
                f"[status] line {line_number} must contain exactly 5 tab-separated columns"
            )
        symbol, status, evidence, tests, notes = (column.strip() for column in columns)
        if ":" not in symbol or not all(symbol.split(":", 1)):
            raise AuditError(f"invalid status symbol on line {line_number}: {symbol!r}")
        if status not in KNOWN_STATUSES:
            raise AuditError(f"unknown status {status!r} for {symbol} on line {line_number}")
        if symbol in seen:
            raise AuditError(f"duplicate [status] row for {symbol}")
        seen.add(symbol)
        rows.append(StatusRow(symbol, status, evidence, tests, notes))
        if len(rows) > MAX_STATUS_ROWS:
            raise AuditError(f"status row count exceeds {MAX_STATUS_ROWS}")
    if not rows:
        raise AuditError("overlay contains no [status] rows")
    return tuple(rows)


def _resolve_qualified(module: ModuleType, qualified: str) -> Any:
    value: Any = module
    for component in qualified.split("."):
        value = getattr(value, component)
    return value


def audit_rows(
    rows: tuple[StatusRow, ...],
    *,
    importer: Callable[[str], ModuleType] = importlib.import_module,
) -> dict[str, Any]:
    reviewed = [row for row in rows if row.status in IMPLEMENTED_STATUSES]
    modules: dict[str, ModuleType] = {}
    contradictions: list[dict[str, str]] = []
    placeholder_count = 0
    missing_count = 0
    for row in reviewed:
        try:
            module = modules.setdefault(row.module_name, importer(row.module_name))
        except Exception as exc:
            missing_count += 1
            contradictions.append(
                {
                    "symbol": row.symbol,
                    "status": row.status,
                    "code": "module-import-failed",
                    "detail": f"{type(exc).__name__}: {exc}",
                }
            )
            continue
        try:
            value = _resolve_qualified(module, row.qualified)
        except AttributeError as exc:
            missing_count += 1
            contradictions.append(
                {
                    "symbol": row.symbol,
                    "status": row.status,
                    "code": "missing-reviewed-symbol",
                    "detail": str(exc),
                }
            )
            continue
        if bool(getattr(value, "_fmn_schema_placeholder", False)):
            placeholder_count += 1
            contradictions.append(
                {
                    "symbol": row.symbol,
                    "status": row.status,
                    "code": "reviewed-symbol-is-placeholder",
                    "detail": "runtime value carries _fmn_schema_placeholder=True",
                }
            )
    contradictions.sort(key=lambda row: (row["symbol"], row["code"], row["detail"]))
    status_counts = {
        status: sum(row.status == status for row in rows)
        for status in sorted(KNOWN_STATUSES)
    }
    return {
        "schema": SCHEMA,
        "version": SCHEMA_VERSION,
        "ok": not contradictions,
        "counts": {
            "status_rows": len(rows),
            "reviewed_implemented": len(reviewed),
            "runtime_placeholders": placeholder_count,
            "missing_reviewed": missing_count,
            "contradictions": len(contradictions),
        },
        "status_counts": status_counts,
        "contradictions": contradictions,
    }


def render_json(report: dict[str, Any]) -> str:
    text = json.dumps(report, sort_keys=True, separators=(",", ":")) + "\n"
    if len(text.encode("utf-8")) > MAX_OUTPUT_BYTES:
        raise AuditError(f"audit output exceeds {MAX_OUTPUT_BYTES}-byte limit")
    return text


def make_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--overlay", type=Path, default=Path("API_OVERLAY.tsv"))
    parser.add_argument("--check", action="store_true", help="exit 1 on runtime contradictions")
    return parser


def main(argv: list[str] | None = None) -> int:
    args = make_parser().parse_args(argv)
    try:
        rows = parse_status_rows(_read_bounded(args.overlay))
        report = audit_rows(rows)
        output = render_json(report)
    except AuditError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2
    sys.stdout.write(output)
    if args.check and not report["ok"]:
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
