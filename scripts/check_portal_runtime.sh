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

python3 scripts/audit_portal_runtime.py --overlay API_OVERLAY.tsv --check
