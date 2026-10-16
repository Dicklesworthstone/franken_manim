# G3 — Depth & Motion evidence packet

- **Gate bead:** `fm-o4q` (**still OPEN** — this packet marshals evidence, it does not pass the gate)
- **Packet status:** **Marshaled, gate not passed** (2026-10-14)
- **Program owner of record:** Jeffrey Emanuel
- **Host:** Linux x86-64

This is the recorded packet required by `docs/GOVERNANCE.md` §2, in the G1 (`docs/gates/G1-core-2d-evidence.md`) and G2 (`docs/gates/G2-native-word-evidence.md`) shape. It is **not** a pass record: prerequisite gate G2 (`fm-i1q`) and pinned-host performance observation PG-3 (`fm-inr.1`) remain open. Each row names the committed test, scenario, or file that reproduces it.

## Gate disposition

- [ ] **PASS** — not claimable: prerequisite gate G2 (`fm-i1q`) and pinned-host observation for PG-3 (`fm-inr.1`) remain open.
- [x] **OPEN** — `fm-o4q` stays open until G2 (`fm-i1q`), Look Gallery owner verdict review (`fm-5wq.50`), and PG-3 host qualification (`fm-inr.1`) complete.

## Acceptance matrix

The seven criteria are quoted from `fm-o4q` (plan §20.3).

| G3 criterion | Evidence | Result |
|---|---|---|
| (1) 3D surfaces with the kept lighting model; CameraFrame on the fixed Rotation conventions; depth, transparency, clip planes (W53D) | `crates/fmn-render/src/three_d.rs`, `crates/fmn-render/src/camera.rs`, standalone `fmn` CLI camera route (`fm-cli-camera-route-i1zc`, `surface_cube.v1` and `mixed_camera.v1`), certified self-golden lock `render_matrix.cli_camera_builtins.v1` and thread-invariance scenario `determinism.cli_builtins_threads_1_4.v1` (`crates/fmn-conformance/tests/e2e_scenarios.rs`), Look Gallery `lighting_3d` panel (`crates/fmn-conformance/fixtures/look_gallery.tsv:9`, awaiting owner verdict review under `fm-5wq.50`) | **Partial (pending Look Gallery owner verdict `fm-5wq.50`)** |
| (2) Dot clouds, images, textures (W53D/W73D + codecs) | `dot_cloud_depth.v1` and `image_quad.v1` rendered through the standalone `fmn` CLI camera route (`render_matrix.cli_camera_builtins.v1`, `crates/fmn-cli/tests/camera_pipeline.rs`), plus portal surface/texture lifecycle suites (`crates/fmn-python/src/portal_texture.rs`, `lifecycle.python_surface_hooks.v1`) | **Green** |
| (3) Vector fields + StreamLines on the tuned integrator (W73D) | `fm-2u6` closed: `crates/fmn-library/src/fields.rs` adaptive RK45 with dense output over `fsci-integrate`, portal `StreamLines` and `AnimatedStreamLines` (`crates/fmn-python/src/portal_streamlines.rs`) | **Green** |
| (4) Events + InteractiveScene (W9EVENTS) | `fm-eiw` and `fm-ffj.72` closed; general live input across all built-in and bundle scenes (`fm-studio-general-input-juh7`, closed) verified by `crates/fmn-cli/src/studio_live.rs` and e2e scenario `lifecycle.studio_general_live_input.v1` | **Green** |
| (5) The Studio baseline: supervisor/worker, PNG stream, scrubbing, TUI (W9SUPER/W9STUDIO) | `fm-39s`, `fm-yh0`, and `fm-ffj.67` closed; verified by `crates/fmn-studio/tests/supervisor.rs`, `crates/fmn-cli/tests/runtime_boundary.rs`, and e2e scenarios `lifecycle.studio_worker_protocol_roundtrip.v1`, `lifecycle.studio_general_live_input.v1`, `failure_path.studio_host_security_refusals.v1` | **Green** |
| (6) The annex serving Studio preview on supported Apple-silicon hardware (W5METAL) — or the declared OQ-12 fallback | `fm-xsz`, `fm-ffj.68`, and `fm-sq8.6` closed (`docs/gates/G3-studio-security-and-annex-ruling.md` / ADR-0024) | **Green** |
| (7) PG-3 (throughput) blocking; PG-A active for annex changes | Policy rows are `blocking` in `docs/performance/PERF_GATES.tsv`; `FrozenRenderState` in `FramePacket` (`fm-sq8.5`, closed) and `ExecutionPlan` (`fm-3df`, closed) are live; awaiting pinned-host observation (`fm-inr.1`) | **NOT GREEN (pending `fm-inr.1`)** |

## Dependency closure

Of `fm-o4q`'s 16 blockers, 15 are closed and 1 (`fm-i1q`, Gate G2) remains open:
- **Closed:** `fm-sq8.9`, `fm-cli-camera-route-i1zc`, `fm-sq8.6`, `fm-sq8.5`, `fm-ffj.72`, `fm-ffj.68`, `fm-ffj.67`, `fm-39s`, `fm-3df`, `fm-0gy`, `fm-studio-general-input-juh7`, `fm-yh0`, `fm-2u6`, `fm-xsz`, `fm-eiw`.
- **Open:** `fm-i1q` (Gate G2).
