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

python3 scripts/verify_portal_runtime_receipt.py "$report_file"
