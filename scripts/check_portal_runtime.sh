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

python3 - "$report_file" API_OVERLAY.tsv <<'PY'
import hashlib
import json
import pathlib
import sys

report_path = pathlib.Path(sys.argv[1])
overlay_path = pathlib.Path(sys.argv[2])
report = json.loads(report_path.read_text(encoding="utf-8"))
embedded = report.get("overlay_sha256")
if not isinstance(embedded, str) or len(embedded) != 64:
    raise SystemExit("installed-wheel parity report omitted a valid overlay_sha256")
checkout = hashlib.sha256(overlay_path.read_bytes()).hexdigest()
if embedded != checkout:
    raise SystemExit(
        f"installed wheel embeds stale API_OVERLAY.tsv: wheel={embedded} checkout={checkout}"
    )
print(f"overlay identity: {checkout}")
PY
