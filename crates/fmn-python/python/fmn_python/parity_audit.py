"""Runtime truth checks for the installed FrankenManim compatibility portal."""

from __future__ import annotations

import hashlib
import importlib
import inspect
import json
from dataclasses import dataclass
from types import ModuleType
from typing import Any, Callable

from .schema_provenance import (
    MAX_SCHEMA_BYTES,
    SCHEMA_PROVENANCE_VERSION,
)

SCHEMA = "fmn.portal.runtime-audit"
SCHEMA_VERSION = 1
MAX_OVERLAY_BYTES = 8 * 1024 * 1024
MAX_STATUS_ROWS = 100_000
MAX_OUTPUT_BYTES = 4 * 1024 * 1024
MAX_DETAIL_CHARS = 4_096
IMPLEMENTED_STATUSES = frozenset({"same", "improved"})
KNOWN_STATUSES = frozenset({"same", "improved", "tiered", "excluded", "unreviewed"})
_PLACEHOLDER_MARKER = "_fmn_schema_placeholder"
_PLACEHOLDER_KIND = "_fmn_schema_placeholder_kind"
_PLACEHOLDER_SYMBOL = "_fmn_schema_placeholder_symbol"
_PROVENANCE_VERSION = "_fmn_schema_provenance_version"
_PROVENANCE_COUNTS = "_fmn_schema_provenance_counts"
_PROVENANCE_COUNT_KEYS = frozenset(
    {"schema_rows", "modules", "classes", "constructors", "functions", "methods"}
)
_MISSING = object()


class ParityAuditError(ValueError):
    pass


class _QualifiedLookupError(Exception):
    def __init__(self, path: str, error: Exception, *, missing: bool) -> None:
        self.path = path
        self.error = error
        self.missing = missing
        super().__init__(f"{path}: {type(error).__name__}: {error}")


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


@dataclass(frozen=True)
class _ResolvedComponent:
    path: str
    value: Any


@dataclass(frozen=True)
class _PlaceholderIdentity:
    path: str
    kind: str
    symbol: str


def _bounded_detail(value: object) -> str:
    text = str(value)
    if len(text) <= MAX_DETAIL_CHARS:
        return text
    return text[: MAX_DETAIL_CHARS - 1] + "…"


def _text_bytes(text: str, *, label: str, limit: int) -> bytes:
    try:
        payload = text.encode("utf-8")
    except UnicodeEncodeError as exc:
        raise ParityAuditError(f"{label} is not valid UTF-8 text: {exc}") from exc
    if len(payload) > limit:
        raise ParityAuditError(f"{label} exceeds {limit}-byte limit")
    return payload


def _overlay_bytes(text: str) -> bytes:
    return _text_bytes(text, label="overlay", limit=MAX_OVERLAY_BYTES)


def _schema_bytes(text: str) -> bytes:
    return _text_bytes(text, label="API schema", limit=MAX_SCHEMA_BYTES)


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


def _resolve_qualified(module: ModuleType, qualified: str) -> tuple[_ResolvedComponent, ...]:
    value: Any = module
    resolved: list[_ResolvedComponent] = []
    components: list[str] = []
    for component in qualified.split("."):
        components.append(component)
        path = ".".join(components)
        try:
            candidate = inspect.getattr_static(value, component, _MISSING)
        except Exception as exc:
            raise _QualifiedLookupError(path, exc, missing=False) from exc
        if candidate is _MISSING:
            try:
                candidate = getattr(value, component)
            except AttributeError as exc:
                raise _QualifiedLookupError(path, exc, missing=True) from exc
            except Exception as exc:
                raise _QualifiedLookupError(path, exc, missing=False) from exc
        value = candidate
        resolved.append(_ResolvedComponent(path, value))
    return tuple(resolved)


def _placeholder_identity(component: _ResolvedComponent) -> _PlaceholderIdentity | None:
    try:
        namespace = vars(component.value)
    except (TypeError, AttributeError):
        return None
    if namespace.get(_PLACEHOLDER_MARKER) is not True:
        return None
    kind = namespace.get(_PLACEHOLDER_KIND)
    symbol = namespace.get(_PLACEHOLDER_SYMBOL)
    return _PlaceholderIdentity(
        path=component.path,
        kind=kind if isinstance(kind, str) and kind else "unspecified",
        symbol=symbol if isinstance(symbol, str) and symbol else "unspecified",
    )


def _placeholder_contradiction(
    row: StatusRow,
    identity: _PlaceholderIdentity,
    *,
    owner: bool,
) -> dict[str, str]:
    if owner:
        detail = (
            f"runtime owner {row.module_name}:{identity.path} carries "
            f"{_PLACEHOLDER_MARKER}=True (kind={identity.kind}, "
            f"declared={identity.symbol}); its resolved member cannot substantiate "
            f"the {row.status} claim"
        )
        code = "reviewed-symbol-has-placeholder-owner"
    else:
        detail = (
            f"runtime value {row.module_name}:{identity.path} carries "
            f"{_PLACEHOLDER_MARKER}=True (kind={identity.kind}, "
            f"declared={identity.symbol})"
        )
        code = "reviewed-symbol-is-placeholder"
    return {
        "symbol": row.symbol,
        "status": row.status,
        "code": code,
        "detail": detail,
    }


def _provenance_contradiction(row: StatusRow, module: ModuleType) -> dict[str, str] | None:
    if row.module_name != "manimlib" and not row.module_name.startswith("manimlib."):
        return None
    observed = vars(module).get(_PROVENANCE_VERSION, _MISSING)
    if observed == SCHEMA_PROVENANCE_VERSION:
        return None
    if observed is _MISSING:
        code = "runtime-schema-provenance-missing"
        detail = (
            f"imported module {row.module_name} has no {_PROVENANCE_VERSION}; "
            f"expected version {SCHEMA_PROVENANCE_VERSION} before reviewing symbols"
        )
    else:
        code = "runtime-schema-provenance-version-mismatch"
        detail = (
            f"imported module {row.module_name} reports {_PROVENANCE_VERSION}="
            f"{_bounded_detail(repr(observed))}; expected {SCHEMA_PROVENANCE_VERSION}"
        )
    return {
        "symbol": row.symbol,
        "status": row.status,
        "code": code,
        "detail": detail,
    }


def _embedded_provenance(native_module: ModuleType) -> dict[str, Any]:
    version = vars(native_module).get(_PROVENANCE_VERSION, _MISSING)
    if version is _MISSING:
        raise ParityAuditError(
            f"native portal has no {_PROVENANCE_VERSION}; package assembly is incomplete"
        )
    if version != SCHEMA_PROVENANCE_VERSION:
        raise ParityAuditError(
            f"native portal reports {_PROVENANCE_VERSION}={version!r}; "
            f"expected {SCHEMA_PROVENANCE_VERSION}"
        )
    raw_counts = vars(native_module).get(_PROVENANCE_COUNTS, _MISSING)
    if not isinstance(raw_counts, tuple):
        raise ParityAuditError(
            f"native portal {_PROVENANCE_COUNTS} must be a tuple of key/value pairs"
        )
    counts: dict[str, int] = {}
    for index, item in enumerate(raw_counts):
        if not isinstance(item, tuple) or len(item) != 2:
            raise ParityAuditError(
                f"native portal {_PROVENANCE_COUNTS}[{index}] is not a key/value pair"
            )
        key, value = item
        if not isinstance(key, str) or key not in _PROVENANCE_COUNT_KEYS:
            raise ParityAuditError(
                f"native portal {_PROVENANCE_COUNTS}[{index}] has unknown key {key!r}"
            )
        if key in counts:
            raise ParityAuditError(
                f"native portal {_PROVENANCE_COUNTS} repeats key {key!r}"
            )
        if isinstance(value, bool) or not isinstance(value, int) or value < 0:
            raise ParityAuditError(
                f"native portal {_PROVENANCE_COUNTS}[{key!r}] is not a nonnegative integer"
            )
        counts[key] = value
    missing = sorted(_PROVENANCE_COUNT_KEYS - counts.keys())
    if missing:
        raise ParityAuditError(
            f"native portal {_PROVENANCE_COUNTS} omits {', '.join(missing)}"
        )
    if counts["schema_rows"] == 0 or counts["modules"] == 0:
        raise ParityAuditError(
            f"native portal {_PROVENANCE_COUNTS} reports an empty assembly"
        )
    return {
        "version": version,
        "counts": {key: counts[key] for key in sorted(counts)},
    }


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
                import_error = f"{type(exc).__name__}: {_bounded_detail(exc)}"
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
        provenance_contradiction = _provenance_contradiction(row, module)
        if provenance_contradiction is not None:
            missing_count += 1
            contradictions.append(provenance_contradiction)
            continue
        try:
            resolved = _resolve_qualified(module, row.qualified)
        except _QualifiedLookupError as exc:
            missing_count += 1
            contradictions.append(
                {
                    "symbol": row.symbol,
                    "status": row.status,
                    "code": (
                        "missing-reviewed-symbol"
                        if exc.missing
                        else "reviewed-symbol-resolution-failed"
                    ),
                    "detail": (
                        f"{row.module_name}:{exc.path}: "
                        f"{type(exc.error).__name__}: {_bounded_detail(exc.error)}"
                    ),
                }
            )
            continue
        final_identity = _placeholder_identity(resolved[-1])
        if final_identity is not None:
            placeholder_count += 1
            contradictions.append(
                _placeholder_contradiction(row, final_identity, owner=False)
            )
            continue
        owner_identity = next(
            (
                identity
                for component in reversed(resolved[:-1])
                if (identity := _placeholder_identity(component)) is not None
            ),
            None,
        )
        if owner_identity is not None:
            placeholder_count += 1
            contradictions.append(
                _placeholder_contradiction(row, owner_identity, owner=True)
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
    try:
        schema = getattr(native_module, "_API_SCHEMA_TSV")
    except AttributeError as exc:
        raise ParityAuditError("native portal does not expose its embedded API schema") from exc
    if not isinstance(overlay, str):
        raise ParityAuditError("embedded API overlay is not text")
    if not isinstance(schema, str):
        raise ParityAuditError("embedded API schema is not text")
    schema_payload = _schema_bytes(schema)
    provenance = _embedded_provenance(native_module)
    report = audit_overlay(overlay)
    report["api_schema_sha256"] = hashlib.sha256(schema_payload).hexdigest()
    report["schema_provenance"] = provenance
    return report


def render_json(report: dict[str, Any]) -> str:
    text = json.dumps(report, sort_keys=True, separators=(",", ":")) + "\n"
    if len(text.encode("utf-8")) > MAX_OUTPUT_BYTES:
        raise ParityAuditError(f"audit output exceeds {MAX_OUTPUT_BYTES}-byte limit")
    return text
