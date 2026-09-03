#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

python3 - <<'PY'
import importlib
module = importlib.import_module("manimlib")
path = getattr(module, "__file__", None)
if not path:
    raise SystemExit("installed-wheel parity gate requires an importable manimlib with __file__")
print(f"auditing imported manimlib: {path}")
PY

report_file="$(mktemp)"
trap 'rm -f "$report_file"' EXIT
set +e
python3 -m fmn_python --audit-parity --robot >"$report_file"
audit_status=$?
set -e
cat "$report_file"
if [[ "$audit_status" -ne 0 ]]; then
    exit "$audit_status"
fi

python3 - "$report_file" API_SCHEMA.tsv API_OVERLAY.tsv <<'PY'
import hashlib
import json
import pathlib
import re
import sys

report_path = pathlib.Path(sys.argv[1])
schema_path = pathlib.Path(sys.argv[2])
overlay_path = pathlib.Path(sys.argv[3])
report = json.loads(report_path.read_text(encoding="utf-8"))
if report.get("schema") != "fmn.portal.runtime-audit" or report.get("version") != 1:
    raise SystemExit("installed-wheel parity report has an unknown schema contract")
if report.get("ok") is not True:
    raise SystemExit("installed-wheel parity report is not a successful audit")


def require_digest(field):
    value = report.get(field)
    if not isinstance(value, str) or re.fullmatch(r"[0-9a-f]{64}", value) is None:
        raise SystemExit(f"installed-wheel parity report omitted a valid {field}")
    return value


embedded_schema = require_digest("api_schema_sha256")
embedded_overlay = require_digest("overlay_sha256")
checkout_schema = hashlib.sha256(schema_path.read_bytes()).hexdigest()
checkout_overlay = hashlib.sha256(overlay_path.read_bytes()).hexdigest()
if embedded_schema != checkout_schema:
    raise SystemExit(
        f"installed wheel embeds stale API_SCHEMA.tsv: wheel={embedded_schema} checkout={checkout_schema}"
    )
if embedded_overlay != checkout_overlay:
    raise SystemExit(
        f"installed wheel embeds stale API_OVERLAY.tsv: wheel={embedded_overlay} checkout={checkout_overlay}"
    )
print(f"schema identity: {checkout_schema}")
print(f"overlay identity: {checkout_overlay}")
PY
