"""Runtime truth checks for the installed FrankenManim compatibility portal."""

from __future__ import annotations

import hashlib
import importlib
import json
from dataclasses import dataclass
from types import ModuleType
from typing import Any, Callable

SCHEMA = "fmn.portal.runtime-audit"
SCHEMA_VERSION = 1
MAX_OVERLAY_BYTES = 8 * 1024 * 1024
MAX_STATUS_ROWS = 100_000
MAX_OUTPUT_BYTES = 4 * 1024 * 1024
IMPLEMENTED_STATUSES = frozenset({"same", "improved"})
KNOWN_STATUSES = frozenset({"same", "improved", "tiered", "excluded", "unreviewed"})


class ParityAuditError(ValueError):
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


def _overlay_bytes(text: str) -> bytes:
    try:
        payload = text.encode("utf-8")
    except UnicodeEncodeError as exc:
        raise ParityAuditError(f"overlay is not valid UTF-8 text: {exc}") from exc
    if len(payload) > MAX_OVERLAY_BYTES:
        raise ParityAuditError(f"overlay exceeds {MAX_OVERLAY_BYTES}-byte limit")
    return payload


def parse_status_rows(text: str) -> tuple[StatusRow, ...]:
    _overlay_bytes(text)
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
            raise ParityAuditError(
                f"[status] line {line_number} must contain exactly 5 tab-separated columns"
            )
        symbol, status, evidence, tests, notes = (column.strip() for column in columns)
        if ":" not in symbol or not all(symbol.split(":", 1)):
            raise ParityAuditError(f"invalid status symbol on line {line_number}: {symbol!r}")
        if status not in KNOWN_STATUSES:
            raise ParityAuditError(f"unknown status {status!r} for {symbol} on line {line_number}")
        if symbol in seen:
            raise ParityAuditError(f"duplicate [status] row for {symbol}")
        seen.add(symbol)
        rows.append(StatusRow(symbol, status, evidence, tests, notes))
        if len(rows) > MAX_STATUS_ROWS:
            raise ParityAuditError(f"status row count exceeds {MAX_STATUS_ROWS}")
    if not rows:
        raise ParityAuditError("overlay contains no [status] rows")
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
    import_errors: dict[str, str] = {}
    contradictions: list[dict[str, str]] = []
    placeholder_count = 0
    missing_count = 0
    for row in reviewed:
        module = modules.get(row.module_name)
        import_error = import_errors.get(row.module_name)
        if module is None and import_error is None:
            try:
                module = importer(row.module_name)
            except Exception as exc:
                import_error = f"{type(exc).__name__}: {exc}"
                import_errors[row.module_name] = import_error
            else:
                modules[row.module_name] = module
        if module is None:
            missing_count += 1
            contradictions.append(
                {
                    "symbol": row.symbol,
                    "status": row.status,
                    "code": "module-import-failed",
                    "detail": import_error or "module importer returned no module",
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


def audit_overlay(
    overlay: str,
    *,
    importer: Callable[[str], ModuleType] = importlib.import_module,
) -> dict[str, Any]:
    payload = _overlay_bytes(overlay)
    report = audit_rows(parse_status_rows(overlay), importer=importer)
    report["overlay_sha256"] = hashlib.sha256(payload).hexdigest()
    return report


def audit_embedded_overlay(native_module: ModuleType) -> dict[str, Any]:
    try:
        overlay = getattr(native_module, "_API_OVERLAY_TSV")
    except AttributeError as exc:
        raise ParityAuditError("native portal does not expose its embedded API overlay") from exc
    if not isinstance(overlay, str):
        raise ParityAuditError("embedded API overlay is not text")
    return audit_overlay(overlay)


def render_json(report: dict[str, Any]) -> str:
    text = json.dumps(report, sort_keys=True, separators=(",", ":")) + "\n"
    if len(text.encode("utf-8")) > MAX_OUTPUT_BYTES:
        raise ParityAuditError(f"audit output exceeds {MAX_OUTPUT_BYTES}-byte limit")
    return text
