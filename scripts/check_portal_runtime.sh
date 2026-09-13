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

for suite in bridge animation_semantics matching_transform_semantics native_outputs; do
    python3 - "$suite" <<'PY'
import pathlib
import runpy
import sys
import tomllib

root = pathlib.Path.cwd()
version = tomllib.loads((root / "Cargo.toml").read_text())["workspace"]["package"]["version"]
suite = root / "crates" / "fmn-python" / "tests" / (sys.argv[1] + ".py")
runpy.run_path(str(suite), init_globals={"_expected_package_version": version})
print(f"installed-wheel acceptance passed: {suite.name}")
PY
done

report_file="$(mktemp)"
echo "retaining installed-wheel parity receipt: $report_file" >&2
set +e
python3 -m fmn_python --audit-parity --robot >"$report_file"
audit_status=$?
set -e
cat "$report_file"
if [[ "$audit_status" -ne 0 ]]; then
    exit "$audit_status"
fi

python3 scripts/verify_portal_runtime_receipt.py "$report_file"
