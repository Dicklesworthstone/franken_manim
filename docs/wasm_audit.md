# WASM-target audit of the governed closure (fm-7wm.4, R15)

**Status:** re-derived 2026-08-26 (wasm32 render-axis check + node smoke green on this host; smoke digest unchanged at `1f248a71347b82aa`) after fm-n64 tranche 2 admitted fp-frame (+fp-columnar/fp-index/fp-runtime/fp-types and their registry transitives) alongside the earlier fnx-classes admission (+ fnx-runtime, rustc-hash) into the workspace closure for the enhanced-tier NetworkGraph — consumed by fmn-library only, which is not in fmn-wasm's dependency graph, so the wasm tree is unchanged; re-proven by the same two gates under the new lock.
The always-on `wasm_audit_is_bound_to_current_locks` Gauntlet test fails when
either authority changes, forcing this audit to be re-run instead of leaving a
plausible but stale “current pins” claim behind.

- `SUITE.lock` SHA-256: `ed186d765192e0afebf046beeaea310b2bbd68511c2fd7ee9a9637b4dfe0612f`
- `Cargo.lock` SHA-256: `bc392046ce8a0639f6417a103db1b7b6c496066b3fbc5688ad2999f5111c0f4c`

Method labels are deliberately narrow:

- **VERIFIED (build):** compiled for `wasm32-unknown-unknown` by a named gate.
- **VERIFIED (execution):** instantiated and exercised in a named JavaScript or
  browser runtime.
- **VERIFIED (mechanical):** derived from the committed manifests, locks, and
  governed-closure checker without claiming runtime behavior.
- **ASSESSED:** a future path whose design posture is known but whose artifact
  is not part of the current compiled graph.

## Shipped serial and shared-memory graphs

The workspace crate set below is unchanged; the tree derivation command
`cargo tree -p fmn-wasm --target wasm32-unknown-unknown --edges normal
--locked` currently aborts on a malformed upstream fixture
(`asupersync@c48399f` `tests/fixtures/migration_readiness_planner/malformed/Cargo.toml`),
so this re-derivation used actual wasm32 compilation of every listed crate plus
a lock-graph walk — the **VERIFIED (mechanical)** evidence class.

| workspace packages | verdict | executable evidence |
|---|---|---|
| `fmn-wasm`, `fmn-anim`, `fmn-cache`, `fmn-codec`, `fmn-config`, `fmn-core`, `fmn-dmath`, `fmn-frame`, `fmn-geom`, `fmn-hash`, `fmn-mobject`, `fmn-platform`, `fmn-render`, `fmn-scene` | **VERIFIED (build)** | `scripts/check_wasm_package.sh` runs wasm-pack 0.15.0 over the actual `fmn-wasm` cdylib; `scripts/check.sh` also compiles the render-axis crates directly for `wasm32-unknown-unknown` |

fmn-geom now enters with its doctrine-D4 solver gateway: the pinned
frankenscipy crates `fsci-linalg` 0.1.0, `fsci-fft` 0.1.0, and `fsci-runtime`
0.1.0 (the last bringing its audit-ledger blake3 hashing chain), plus their
transitive numeric and serialization stacks (`nalgebra` 0.34.2 with the
`simba`/`matrixmultiply`/`wide`/`safe_arch`/`approx`/num-* chain, `serde`
1.0.229 with derive, `serde_json` 1.0.151 with `preserve_order`, and the
wasm-bindgen family already listed above). The complete admission — exact
versions, checksums, features, licenses, proc-macro/build-script posture, and
unsafe reviews for every package this gateway adds — is enforced row-by-row by
`SUITE_ALLOWLIST.tsv`; `workspace_closure_is_exactly_the_governed_universe`
checks the primary and auxiliary locks on every `cargo test` run.

The pinned nightly has `wasm32-unknown-unknown` installed on the release host.
This is exercised rather than inferred: `scripts/check.sh` target-compiles the
render axis and `wasm-smoke/run.sh` builds a fresh release probe, instantiates it
under Node, checks deterministic render bytes, reads the browser clock shim,
and verifies that process access fails closed. The 2026-08-18 v0.4.0 re-derivation emitted
the locked smoke digest `1f248a71347b82aa`.

## Product surfaces

| surface | verdict | current boundary |
|---|---|---|
| Tier 1: `FmnScene` fixed-scene renderer | **VERIFIED (execution)** | The packaged ESM/TypeScript surface enumerates `circle_shift`, `parametric_wave`, and `orbit_duet`, renders RGBA8 into caller-reused buffers, and precisely refuses a wrong destination length. wasm-bindgen copies the mutable JS view into Wasm memory; this is buffer reuse, not a zero-copy claim. |
| Tier 2: `FmnPlayer` FMTL/1 player | **VERIFIED (execution)** | The packaged player parses the governed timeline bundle, refuses engine-major mismatches, exposes labels/seek state, and renders RGBA8 through the same semantic Lumen path. |
| npm bundler artifact | **VERIFIED (execution)** | `scripts/check_wasm_package.sh` builds the self-contained package, checks its exact file/license inventory and recorded size budgets, runs `npm pack` plus `npm publish --dry-run`, installs the tarball into a fresh consumer, bundles it with webpack 5.109.2, and exercises both tiers through Canvas write/readback in headless Chrome. The clean v0.4.0 release-commit receipt passed at `d1e4274aa32ba5c59934029db397bbbeb9bfa18c` with `source_dirty=false`; its 395,038-byte npm tarball has SHA-256 `f531940b5849ed2f3e091424931581bf4450e2811ec66e850aebfe3a76dc371d`. This is a publishable artifact and dry-run proof, not a claim that the package exists in the npm registry. |
| threads artifact | **VERIFIED (execution)** | The separate `fmn-wasm/threads` export is built with atomics and imported shared memory. Its coordinator compiles the module once, verifies the instantiated buffer is a `SharedArrayBuffer`, passes the same module and memory to two module workers, and renders independent whole frames concurrently. The Chromium gate byte-compares serial and threaded results, checks repeatability, and proves a document without COOP/COEP receives `FMN_WASM_CROSS_ORIGIN_ISOLATION_REQUIRED` before worker startup. This is frame-batch parallelism, not intra-frame parallelism. |

The browser/package evidence proves compilation, packaging, deterministic
same-build execution, and the tested failure paths. It does **not** prove
browser performance, cross-browser behavior, the certified platform matrix,
cross-platform bit identity, registry publication, or service-worker/CDN
deployment. WASM remains standard-mode only.

## Intentionally absent from the current wasm graph

| packages or capability | posture |
|---|---|
| `fmn-studio`, `fmn-cli` | host front doors: supervisor/subprocess, terminal, filesystem, and CLI behavior; the browser package is a separate W11 artifact |
| `fmn-python`, PyO3, NumPy | supported host-CPython portal only; never part of a standalone browser artifact |
| `asupersync` | host-side multi-scene batch farms and scheduler laboratory; never in the frame loop |
| `frankentorch`, `ft-kernel-metal`, CUDA/Metal annexes | native standard-mode accelerator annexes; no GPU annex enters this wasm package |
| `fmn-output`, ffmpeg | host publication and optional external-tool boundary; browser frames are returned to the consumer instead |
| `fmn-text`, `fmn-tex`, `fmn-library`, `fmd-font`, `fmd-math`, the complete bundled font set | not reached by the current three-scene Tier-1 package or FMTL demo graph; future browser text/font consumption must be built, size-measured, and re-audited before it is called verified |
| `franken_numpy`, `frankenscipy`, `franken_networkx`, `frankenpandas` | not consumed by the current wasm graph; any future subset requires its own target build and governed-closure update |

## Standing rules

1. A crate enters the shipped wasm graph only after an actual
   `wasm32-unknown-unknown` build and the governed closure both pass.
2. Any `SUITE.lock` or `Cargo.lock` change invalidates this audit mechanically.
   Update the recorded digests only after re-running the tree, target build,
   Node smoke, and every affected browser/package check.
3. An absent host-only package is not evidence that it is wasm-compatible.
   Future text, fonts, or suite libraries stay **ASSESSED** until their real
   artifact is compiled and exercised.
4. Threads remain a separate explicit subpath behind atomics, shared memory,
   and cross-origin isolation. The default import stays serial and no fallback
   is implicit. Certified mode and native accelerator annexes remain explicit
   refusals in the browser package.
