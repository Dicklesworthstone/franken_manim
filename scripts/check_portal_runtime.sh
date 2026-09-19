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

for suite in textured_surfaces portal_initialization persistent_recovery fading_family bridge animation_semantics matching_transform_semantics composition_lifecycle camera_animation_semantics camera_motion camera_execution updater_family drawing_runtime creation_semantics subset_reveal text_reveal native_outputs programmatic_rendering batch_rendering builder_playback restore_playback cyclic_replace deferred_transform_playback fading_semantics movement_semantics rotation_semantics indication_semantics composite_effects tracker_interpolation live_tex live_tex_render live_rates animation_updaters native_animation_updaters deferred_animation_updaters console_rendering scene_state temporal_visualization control_interaction color_sliders scene_execution matrix_authoring source_autoreload scene_project scene_project_editor live_graphing live_vector_fields; do
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
