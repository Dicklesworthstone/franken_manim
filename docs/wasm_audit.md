# WASM-target audit of the governed closure (fm-g2c, R15)

**Status:** current as of SUITE.lock's pins (2026-07-22). Re-run at every
suite pin bump and whenever a `pending` allowlist row is consumed.
Method labels: **VERIFIED** = compiled or mechanically checked here;
**ASSESSED** = judged from the crate's own dependency posture, to be
verified when the wasm CI lane (fm-sol) stands up `wasm32-unknown-unknown`
in the matrix (the pinned local toolchain does not install that target;
adding it is part of fm-sol's CI work, deliberately not a side effect of
this audit).

## Workspace crates (the current entire closure)

| member | tier | verdict | notes |
|---|---|---|---|
| fmn-core, fmn-dmath, fmn-hash, fmn-geom, fmn-mobject, fmn-anim, fmn-frame, fmn-codec, fmn-cache, fmn-render, fmn-text, fmn-tex, fmn-library, fmn-scene, fmn-output, fmn-runtime, fmn-conformance, fmn-config | wasm tier 1 eligible | **VERIFIED (mechanical)** | std-only, `#![forbid(unsafe_code)]`, zero external deps (governed-closure check proves the closure is exactly the workspace); no `std::fs`/`std::process`/network use outside tests — checked by grep over `src/` |
| fmn-platform | wasm tier 1 with shims | **ASSESSED** | this crate *is* the shim point: capability traits (fs/clock/process/AssetFetcher) get wasm implementations here (R15); nothing else in the workspace touches the OS directly |
| fmn-studio, fmn-cli | not wasm targets | by design | supervisor/subprocess and CLI surfaces; the browser Studio is a separate W11 artifact |
| fmn-python | not a wasm target | by design | PyO3 bridge; CPython is out of scope for wasm tiers |
| fmn-spike-object-model | n/a | spike | publish = false, not shipped |

## Pre-authorized, not yet consumed

| package | wasm posture |
|---|---|
| pyo3 | never on the wasm path (fmn-python excluded above) |
| clap | `cli` feature only; wasm builds are `--no-default-features` |
| wasm-bindgen | wasm-only by definition; enters the closure with the `wasm` feature axis (fm-l97) |

## FrankenSuite repos (consumed later; assessed at their pinned commits)

| repo | tier assignment | verdict | basis |
|---|---|---|---|
| franken_numpy | tier 1 candidate | **ASSESSED** | fnp is the array/RNG substrate the wasm frame renderer needs; audit its crates' std-surface at consumption (fm-ai1/fm-n1b) |
| frankenscipy | tier 1 candidate (subset) | **ASSESSED** | only the solve/quadrature crates fmn-geom consumes need wasm; audit at fm-go7 |
| franken_markdown | tier 1 candidate | **ASSESSED** | engine documents zero third-party deps and an explicit `--no-default-features` wasm build; fmd-font/fmd-math inherit that posture when fm-ydw lands |
| franken_networkx / frankenpandas | tier 2, never blocking | **ASSESSED** | enhanced-tier mobjects; wasm support is nice-to-have by doctrine |
| frankentorch | excluded from wasm | by design | the Accelerator Annex is native GPU only (§10.7) |
| asupersync | excluded from wasm tier 1 | by design | batch farms are host-side; never in the frame loop |

## Standing rules

1. A crate enters wasm tier 1 only if it is in the governed closure AND
   this audit marks it eligible; feature-gate or shim at fmn-platform
   otherwise (R15).
2. Every `pending` → consumed transition in SUITE_ALLOWLIST.tsv re-runs
   this audit and upgrades ASSESSED rows to VERIFIED in the wasm CI lane.
3. Threads in wasm stay behind atomics + cross-origin isolation (§10.7);
   nothing in tier 1 may assume them.
