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

for suite in decimal_authoring complex_matrix complex_readouts_render checkpoint_identity fill_profile_pixels fill_profile_animation functional_color pointwise_color pointwise_paint_integration textured_surfaces portal_initialization point_editing text_authoring code_authoring persistent_recovery fading_family bridge animation_semantics matching_transform_semantics matching_authoring composition_lifecycle camera_animation_semantics camera_motion camera_execution updater_family drawing_runtime creation_semantics subset_reveal text_reveal native_outputs programmatic_rendering runtime_provenance batch_rendering reproducible_batch builder_playback restore_playback cyclic_replace deferred_transform_playback fading_semantics movement_semantics rotation_semantics indication_semantics composite_effects tracker_interpolation live_tex live_tex_render live_rates animation_updaters native_animation_updaters deferred_animation_updaters console_rendering scene_state temporal_visualization detached_tracing_tail streamline_authoring streamline_rebuild control_interaction color_sliders scene_execution matrix_authoring source_autoreload scene_project scene_project_editor live_graphing live_vector_fields; do
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

# Runtime inventories exercise real files; protocol sinks are explicitly doubled.
python3 crates/fmn-python/tests/test_batch_checkpoint_identity.py
python3 crates/fmn-python/tests/test_batch_checkpoint_inputs.py
python3 crates/fmn-python/tests/test_runtime_identity.py
python3 crates/fmn-python/tests/test_runtime_provenance_protocol.py
python3 crates/fmn-python/tests/test_reproducible_batch_protocol.py
python3 crates/fmn-python/tests/test_reproducible_batch_cli_protocol.py

# These unittest suites include native geometry/clock and failure-recovery cases.
FMN_TEST_NATIVE=1 python3 crates/fmn-python/tests/test_speed.py
FMN_TEST_NATIVE=1 python3 crates/fmn-python/tests/test_speed_updaters.py

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
