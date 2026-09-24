# WASM-target audit of the governed closure (fm-7wm.4, R15)

**Status:** re-run again 2026-09-24 against `Cargo.lock` `6ce5886a` at `86f2896d`.
**The release package gate still FAILS at its size budget**, so this audit does
not claim a passing package.

- The lock change behind this re-run is d121ddd5. It adds five workspace edges to
  the `fmn` umbrella crate (`fmn-codec`, `fmn-frame`, `fmn-output`, `fmn-render`,
  `fmn-runtime`), and `fmn` is not in any wasm graph. **VERIFIED (mechanical):**
  `git diff ad0b7d25 HEAD -- Cargo.lock` is those five lines, and `SUITE.lock` and
  `wasm-smoke/Cargo.lock` are unchanged. The path-normalized output of `cargo tree
  -p fmn-wasm --target wasm32-unknown-unknown --edges normal --locked` is identical
  at `ad0b7d25` and `86f2896d` (130 lines).
- `cargo build --locked -p fmn-wasm --target wasm32-unknown-unknown` exited 0.
  `wasm-smoke/run.sh` exited 0 with the unchanged Node digest `1f248a71347b82aa`.
- `scripts/check_wasm_package.sh` was not re-run. It has failed at the size
  budget since `b492564b` (fm-8j70) and stops before the Chromium stage, and the
  wasm graph is unchanged. The qualified wasm-pack 0.15.0 bundler artifact at
  `86f2896d` is 555,763 bytes against the 514,521-byte budget. The attribution
  is on fm-8j70.

Earlier re-run, 2026-09-24, against `Cargo.lock` `b97f1d76` from a clean worktree
at `c5f50e1e`, with the qualified tools (wasm-pack 0.15.0, wasm-bindgen 0.2.127,
wasm-opt 117, webpack 5.109.2 / webpack-cli 7.2.2):

- The lock change behind that re-run is a single edge: `fmn-python` now depends on
  `fsci-opt` (6bc0ddb2), which the lock already held. `fmn-python` is not in any
  wasm graph. **VERIFIED (mechanical):** `git diff 2649c18b HEAD -- Cargo.lock` is
  that one line, and `SUITE.lock` and `wasm-smoke/Cargo.lock` are unchanged.
- `cargo tree -p fmn-wasm --target wasm32-unknown-unknown --edges normal --locked`
  exited 0, and `cargo build --locked -p fmn-wasm --target wasm32-unknown-unknown`
  exited 0.
- `wasm-smoke/run.sh` exited 0 with the unchanged Node digest `1f248a71347b82aa`,
  after 3837dac0 restored its build (it had missed fm-sq8.9's `ScreenMap::y_up`).
- `scripts/check_wasm_package.sh` exited 1 at the size budget: the bundler wasm is
  555,759 bytes against the 514,521-byte `crates/fmn-wasm/SIZE_BUDGET.tsv` budget.
  The breach is older than this lock change. The same build at `b492564b` (before
  fm-sq8.9) is already 554,530 bytes; fm-sq8.9 adds 1,229. The growth landed after
  the 2026-09-17 pass below and needs bisecting (bead fm-8j70). The
  Chromium stage was not reached. The browser digests and the
  `certified-cpu:scalar:6` identity recorded below predate fm-sq8.9, which flips
  front-door frames to +Y-up and bumps the renderer to `scalar:7`; they are
  historical until the package gate passes again.

Previous full pass: re-derived 2026-09-17 on the local qualified-tool host at commit
`efdf8e4041bbbe9f2a63b4b91f30b59286861b89`. The locked dependency tree, wasm32
target build, locked Node smoke, and the complete serial / shared-memory package
and Chromium gate all exited 0 on a **clean tree** (`source_dirty=false`, receipt
schema `fmn-wasm-package-receipt/2`). Node smoke digest `1f248a71347b82aa`;
browser digests `f5b7f4b9` / `c864b03d` / `8d9ea9c9`.
The always-on `wasm_audit_is_bound_to_current_locks` Gauntlet test fails when
either authority changes. That forces this audit to be re-run and its outcome
recorded, instead of leaving a plausible but stale "current pins" claim behind.

- `SUITE.lock` SHA-256: `d38bb18884f060d3867dfbb26b1c985e90ddf8ce716589dd13d2e68aa404b3d3`
- `Cargo.lock` SHA-256: `6ce5886ad19ccd5c11fe29bfcc6818b9d83317740cc87e8f5b66378ddba28a11`
- Auxiliary `wasm-smoke/Cargo.lock` SHA-256: `01f3e42a699383d33b42379bab14661069b40496b869cfbbd7d35b7e58fde53b`

Method labels are deliberately narrow:

- **VERIFIED (build):** compiled for `wasm32-unknown-unknown` by a named gate.
- **VERIFIED (execution):** instantiated and exercised in a named JavaScript or
  browser runtime.
- **VERIFIED (mechanical):** derived from the committed manifests, locks, and
  governed-closure checker without claiming runtime behavior.
- **ASSESSED:** a future path whose design posture is known but whose artifact
  is not part of the current compiled graph.

## Shipped serial and shared-memory graphs

The tree derivation command `cargo tree -p fmn-wasm --target
wasm32-unknown-unknown --edges normal --locked` exited 0. Cargo also emitted a
diagnostic about the intentionally malformed upstream asupersync fixture;
that diagnostic did not prevent tree generation or the subsequent builds.
Both packaged execution variants compiled the workspace crates below with
the exact `nightly-2026-08-31` toolchain.

| workspace packages | verdict | executable evidence |
|---|---|---|
| `fmn-wasm`, `fmn-anim`, `fmn-cache`, `fmn-codec`, `fmn-config`, `fmn-core`, `fmn-dmath`, `fmn-frame`, `fmn-geom`, `fmn-hash`, `fmn-mobject`, `fmn-platform`, `fmn-render`, `fmn-scene` | **VERIFIED (build)** | `scripts/check_wasm_package.sh` runs wasm-pack 0.15.0 over the actual `fmn-wasm` cdylib; `scripts/check.sh` also compiles the render-axis crates directly for `wasm32-unknown-unknown` |

`fmn-core` consumes `fnp-random-core` 0.2.0 at
`a15d5c32e9330b555b0a653058bcfcff22fcb4ec` for the governed RNG.
fmn-geom enters with its doctrine-D4 solver gateway: the pinned
frankenscipy crates `fsci-linalg` 0.1.0, `fsci-fft` 0.1.0, and `fsci-runtime`
0.1.0 at `5b1441b13a0997901ad2f9835c30072f87ca93b2` (the last bringing its
audit-ledger blake3 hashing chain), plus their
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
and verifies that process access fails closed. The 2026-09-08 run under Node
22.22.1 emitted the locked smoke digest `1f248a71347b82aa`, reported
`process=capability-absent`, and used the one-CPU browser topology shim.

## Product surfaces

| surface | verdict | current boundary |
|---|---|---|
| Tier 1: `FmnScene` fixed-scene renderer | **VERIFIED (execution)** | The packaged ESM/TypeScript surface enumerates `circle_shift`, `parametric_wave`, and `orbit_duet`, renders RGBA8 into caller-reused buffers, and precisely refuses a wrong destination length. wasm-bindgen copies the mutable JS view into Wasm memory; this is buffer reuse, not a zero-copy claim. |
| Tier 2: `FmnPlayer` FMTL/1 player | **VERIFIED (execution)** | The packaged player parses the governed timeline bundle, refuses engine-major mismatches, exposes labels/seek state, and renders RGBA8 through the same semantic Lumen path. |
| npm bundler artifact | **VERIFIED (execution)** | `scripts/check_wasm_package.sh` builds the self-contained package, checks its exact file/license inventory and recorded size budgets, runs `npm pack` plus `npm publish --dry-run`, installs the tarball into a fresh consumer, bundles it with webpack 5.109.2, and exercises both tiers through Canvas write/readback in headless Chrome. The current diagnostic receipt and its limits are recorded below. |
| threads artifact | **VERIFIED (execution)** | The separate `fmn-wasm/threads` export is built with atomics and imported shared memory. Its coordinator compiles the module once, verifies the instantiated buffer is a `SharedArrayBuffer`, passes the same module and memory to two module workers, and renders independent whole frames concurrently. The Chromium gate byte-compares serial and threaded results, checks repeatability, and proves a document without COOP/COEP receives `FMN_WASM_CROSS_ORIGIN_ISOLATION_REQUIRED` before worker startup. This is frame-batch parallelism, not intra-frame parallelism. |

The browser/package evidence proves compilation, packaging, deterministic
same-build execution, and the tested failure paths. It does **not** prove
browser performance, cross-browser behavior, the certified platform matrix,
cross-platform bit identity, registry publication, or service-worker/CDN
deployment. WASM remains standard-mode only.

### Current package receipt

The release-grade run exited 0 in 111.9 seconds on a clean clone of commit
`efdf8e4041bbbe9f2a63b4b91f30b59286861b89` (`source_dirty=false`); retained
evidence is `.rch-results/wasm-package-20260917-fm7wm6/release-receipt.json`
(evidence root `/tmp/fmn-fm7wm6-release-20260917`). An independent dirty-tree
diagnostic run at the same commit produced byte-identical browser digests,
retained at `.rch-results/wasm-package-20260917-fm7wm6/receipt.json`. The
gate script now accepts both npm receipt shapes (npm 10 bare object and the
npm 9 `{package-name: receipt}` mapping); this host's npm is 11.19.1.
No registry publication occurred.

| Identity or result | Executed value |
|---|---|
| Tools | wasm-pack 0.15.0; wasm-bindgen 0.2.127; Binaryen 117; Node 22.22.1; npm 11.19.1; webpack 5.109.2 / CLI 7.2.2; Chrome 153.0.8010.47 |
| npm tarball | 409,605 bytes; SHA-256 `2a7a7f4de062b4f112619baebec2aa79db953c78e4c303dc179013ea3c2d2a18` |
| Serial Wasm | 487,503 raw bytes; 194,668 gzip bytes |
| Shared-memory Wasm | 487,117 raw bytes; 195,145 gzip bytes |
| `FmnScene` | 45 rendered frames; digest `f5b7f4b9` |
| `FmnPlayer` | 45 rendered frames; digest `c864b03d`; engine identity `certified-cpu:scalar:6` |
| Workers | 2 workers over shared memory; 4 frames; digest `8d9ea9c9`; serial/threaded byte equality passed |
| Isolation refusal | `FMN_WASM_CROSS_ORIGIN_ISOLATION_REQUIRED` before worker startup in the nonisolated document |

Chrome ran with its sandbox enabled. The asupersync malformed-fixture cargo
diagnostic is the intentionally non-fatal upstream diagnostic tracked by
bead fm-asupersync-fixture-diagnostic-5bxf; it did not affect tree
generation or any build. The player engine identifier names the semantic
renderer serialized in the timeline; it does not change the browser's
standard-only certification scope.

Historical evidence: the v0.4.0 release-commit receipt passed at
`d1e4274aa32ba5c59934029db397bbbeb9bfa18c` with `source_dirty=false`; its
395,038-byte tarball had SHA-256
`f531940b5849ed2f3e091424931581bf4450e2811ec66e850aebfe3a76dc371d`.
The 2026-09-08 a40c4a88 receipt (npm 10.9.4, Chrome 152, tarball
`41a94cd2…cf85`) qualified the August locks and is superseded by the
receipt above.

## Intentionally absent from the current wasm graph

| packages or capability | posture |
|---|---|
| `fmn-studio`, `fmn-cli` | host front doors: supervisor/subprocess, terminal, filesystem, and CLI behavior; the browser package is a separate W11 artifact |
| `fmn-python`, PyO3, NumPy | supported host-CPython portal only; never part of a standalone browser artifact |
| `asupersync` | host-side multi-scene batch farms and scheduler laboratory; never in the frame loop |
| `frankentorch`, `ft-kernel-metal`, CUDA/Metal annexes | native standard-mode accelerator annexes; no GPU annex enters this wasm package |
| `fmn-output`, ffmpeg | host publication and optional external-tool boundary; browser frames are returned to the consumer instead |
| `fmn-text`, `fmn-tex`, `fmn-library`, `fmd-font`, `fmd-math`, the complete bundled font set | not reached by the current three-scene Tier-1 package or FMTL demo graph; future browser text/font consumption must be built, size-measured, and re-audited before it is called verified |
| Remaining `franken_numpy` / `frankenscipy` crates; `franken_networkx`, `frankenpandas` | only `fnp-random-core` and the three named `fsci-*` crates are consumed; other subsets require their own target build and governed-closure update |

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
