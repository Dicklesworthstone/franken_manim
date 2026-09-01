# Changelog

This is a synthesized, agent-facing changelog for the full history of **franken_manim**.

Scope window: project inception on **2026-07-20** through the verified unreleased implementation checkpoint **[`403c1f4`](https://github.com/Dicklesworthstone/franken_manim/commit/403c1f4c5a772ab228dc0f2c14739b7e8c954f7e)** on **2026-09-01**.

**franken_manim** is a sovereign, deterministic rewrite of 3Blue1Brown's `manim` in pure Rust: analytic Bézier rasterization, native TeX-math (no LaTeX), a `manimlib`-compatible Python portal, and a crash-isolated Studio. Workspace version at this checkpoint is **`0.4.0`**.

The historical reconstruction below was originally complete through 2026-08-19. The new unreleased section is deliberately narrower: it records only later capabilities directly re-audited in the current implementation tranche, not an unsupported claim that every intervening commit has been reconstructed here.

This document was rebuilt from:

- git history on `main`
- annotated git tags `v0.1.0` … `v0.4.0`
- GitHub Releases (all four tags are **published pre-releases**, not drafts and not "latest stable")
- Beads tracker in [`.beads/issues.jsonl`](https://github.com/Dicklesworthstone/franken_manim/blob/main/.beads/issues.jsonl)
- GitHub Release notes on `Dicklesworthstone/franken_manim` (used as orientation; git history is authoritative)

It is organized by landed capabilities, not raw diff order. Representative commits use live GitHub URLs. Beads IDs (`fm-…`) are records in `.beads/issues.jsonl`, not GitHub Issues.

---

## Unreleased — executable front doors and fail-closed agent control plane (2026-08-31 → 2026-09-01)

### Delivered capability

- The shipped `fmn` executable now has a real one-scene batch smoke that renders a native artifact and verifies publication of both machine and human manifests. Robot-mode batch output includes an explicit terminal `batch_job` success record, so agents no longer infer job completion by reconciling unrelated records.
- `doctor --quiet` is enforced in the central dispatcher rather than by reparsing argv in `main.rs`; embedded callers, the OS front door, human output, robot output, and typed failures now share one policy.
- The Rust-native snake_case geometry helpers are mechanically prevented from leaking into the Python `manimlib` wildcard namespace. The policy proves the corresponding Reference CamelCase classes remain present in the schema, package wrapper, and clean-wheel smoke.
- The Beads operational projection advanced to schema v4. It distinguishes claimable leaves from assigned work and epic containers; enforces known statuses, ownership, unique dependency edges, and finite graph budgets; reports missing targets; detects live blocking cycles with a deterministic iterative SCC pass; and suppresses recommendations whenever dependency integrity is invalid.
- `scripts/generate_agent_brief.py` produces deterministic Markdown using the newest Beads record timestamp, supports exact `--check` and nonmutating `--stdout` modes, publishes atomically without following pre-existing temporary paths, and refuses before publication on activation-cap or dependency-integrity failure.
- `scripts/check.sh` runs the complete Python control-plane parser/generator tests and renders the real live ledger without mutating repository documentation. These are local repository gates; no GitHub Actions execution is required or claimed.

### Evidence boundary

- Beads remains the sole task authority. The brief is a projection and never edits task state.
- The current W10 semantic-surface parent remains open because the live tracker contains a newer reality-check that supersedes older completion commentary. This changelog does not convert ledger adjudication into a claim of universal portal implementation.
- Hardware-specific and release-matrix evidence remains separate from local correctness gates. No hosted GitHub Actions result is claimed in this tranche.

### Representative commits

- [`82d915e`](https://github.com/Dicklesworthstone/franken_manim/commit/82d915ee34285ea7ab2198e76e831ee23bf88250) Smoke a real batch render and manifest publication through the shipped binary.
- [`0fc2fc7`](https://github.com/Dicklesworthstone/franken_manim/commit/0fc2fc7fd4ec5f7a21820faf471228797b72670e) Enforce the Rust-helper / Reference-class Python boundary.
- [`40df985`](https://github.com/Dicklesworthstone/franken_manim/commit/40df985fc40d35ea7bdbb80a71608bad16d7d724) Reject vacuous package alias guards.
- [`38c3eb3`](https://github.com/Dicklesworthstone/franken_manim/commit/38c3eb3af1dbde03c2c5ce390997152e745d7c2d) Gate the live Beads projection locally.
- [`5ce894f`](https://github.com/Dicklesworthstone/franken_manim/commit/5ce894f888a71d6fae7c82fd685c84d51e1902d3) Fail closed on malformed Beads dependency graphs.
- [`4629943`](https://github.com/Dicklesworthstone/franken_manim/commit/4629943ccdb0bfe14829dc1f94c8b88dcb0446b1) Make cycle analysis iterative for deep valid graphs.
- [`55e1186`](https://github.com/Dicklesworthstone/franken_manim/commit/55e11868d5e1355844ef17948e45995e34e25bd9) Refuse publishing briefs from corrupt task graphs.
- [`403c1f4`](https://github.com/Dicklesworthstone/franken_manim/commit/403c1f4c5a772ab228dc0f2c14739b7e8c954f7e) Prove every publication mode preserves the prior artifact on integrity failure.

---

## Version Timeline

`Kind` distinguishes a published GitHub Release from a plain git tag. Every tag below also has a GitHub Release, and every Release is marked **prerelease**.

| Version | Kind | Date | Summary |
|---------|------|------|---------|
| inception [`92eff5ed`](https://github.com/Dicklesworthstone/franken_manim/commit/92eff5ed0dfbb69ea5c8cc582014b47f611074a6) | inception | 2026-07-20 | Comprehensive plan (Rev 4), README, AGENTS, license; 106-issue beads graph. |
| [`v0.1.0`](https://github.com/Dicklesworthstone/franken_manim/releases/tag/v0.1.0) | Release (prerelease) | 2026-08-15 | Early native preview. Peel [`ca0e830`](https://github.com/Dicklesworthstone/franken_manim/commit/ca0e83032eedadfe56a6f541a11903fc2c9d6665). Native `fmn` CLI + Studio/runtime; no `fmn-python` wheel. |
| [`v0.2.0`](https://github.com/Dicklesworthstone/franken_manim/releases/tag/v0.2.0) | Release (prerelease) | 2026-08-16 | Production Python-render preview. Peel [`88b431e`](https://github.com/Dicklesworthstone/franken_manim/commit/88b431e17d8e1cf3c3b09d91b82b6ae06f2f3ba9). Portal PNG sequences through retained Lumen + Reel. |
| [`v0.3.0`](https://github.com/Dicklesworthstone/franken_manim/releases/tag/v0.3.0) | Release (prerelease) | 2026-08-16 (published 2026-08-17) | Source-unedited final-still preview. Peel [`82c3daa`](https://github.com/Dicklesworthstone/franken_manim/commit/82c3daa84129e31d742e691e3835366b7356d1c4). Eight locked G4a corpus scenes render PNG sequences / atomic final PNGs. |
| [`v0.4.0`](https://github.com/Dicklesworthstone/franken_manim/releases/tag/v0.4.0) | Release (prerelease) | 2026-08-18 | Native Studio/Metal and threaded browser preview. Peel [`d1e4274`](https://github.com/Dicklesworthstone/franken_manim/commit/d1e4274aa32ba5c59934029db397bbbeb9bfa18c). Checksummed native installer; threaded WASM frame pool. |
| implementation checkpoint [`403c1f4`](https://github.com/Dicklesworthstone/franken_manim/commit/403c1f4c5a772ab228dc0f2c14739b7e8c954f7e) | unreleased | 2026-09-01 | Executable CLI smoke, Python helper-boundary ratchet, and fail-closed local Beads control plane. |

---

## 1) Plan, substrate, and certified CPU engine (2026-07-20 → 2026-07-26)

The first week turns the Rev 4 plan into a cargo workspace and the data planes every later renderer depends on: color/RNG/clock, QuadPath geometry, Stage/RecordBuffer mobjects, animation mechanisms, native TeX, owned codecs, and an analytic CPU fill/stroke engine.

### Delivered capability

- Workspace skeleton (`fmn-core`, `fmn-dmath`, `fmn-hash`, `fmn-platform`) with `SUITE.lock` governed-closure audit (G0-7).
- QuadPath shared-anchor path model; true arc length under the original names; rational frame clock with exact sample points; single PCG64DXSM RNG.
- Stage arena + RecordBuffer data plane; copy semantics (`CopyMap` / `become`); Transform and the five animation mechanism families; composition operators + Timeline.
- `fmd-font` / `fmd-math` consumed: Scribe I/II, span map instead of manim's two-render hack; tier-1 TeX coverage 99.379%.
- Owned DEFLATE/PNG/JPEG, then GIF/y4m/WAV; negotiated ffmpeg boundary; content-addressed cache.
- Analytic fill on the actual curves (not chords), true curve-distance strokes, certified CPU engine with a measured corpus.

### Closed workstreams

- [`fm-bsz`](https://github.com/Dicklesworthstone/franken_manim/blob/main/.beads/issues.jsonl) cargo workspace skeleton.
- [`fm-wuq`](https://github.com/Dicklesworthstone/franken_manim/blob/main/.beads/issues.jsonl) RationalFrameClock.
- [`fm-cye`](https://github.com/Dicklesworthstone/franken_manim/blob/main/.beads/issues.jsonl) Transform + mechanism catalog.
- [`fm-17m`](https://github.com/Dicklesworthstone/franken_manim/blob/main/.beads/issues.jsonl) / [`fm-65l`](https://github.com/Dicklesworthstone/franken_manim/blob/main/.beads/issues.jsonl) codecs I/II.
- [`fm-5oi`](https://github.com/Dicklesworthstone/franken_manim/blob/main/.beads/issues.jsonl) analytic fill; [`fm-oac`](https://github.com/Dicklesworthstone/franken_manim/blob/main/.beads/issues.jsonl) strokes; [`fm-ig3`](https://github.com/Dicklesworthstone/franken_manim/blob/main/.beads/issues.jsonl) certified CPU engine.

### Representative commits

- [`92eff5ed`](https://github.com/Dicklesworthstone/franken_manim/commit/92eff5ed0dfbb69ea5c8cc582014b47f611074a6) Initial commit: the comprehensive plan (Rev 4), README, AGENTS.md, and license.
- [`8d3193e9`](https://github.com/Dicklesworthstone/franken_manim/commit/8d3193e923e9f8944c4e8fb43d3eafd58fd1e0f8) Stand up the cargo workspace skeleton.
- [`876cbf5d`](https://github.com/Dicklesworthstone/franken_manim/commit/876cbf5d36e31d24f1f0ce724253b4b44adae2b6) The RationalFrameClock — exact sample points, zero drift.
- [`03b379d2`](https://github.com/Dicklesworthstone/franken_manim/commit/03b379d2) True arc length under the original names.
- [`deebd8bc`](https://github.com/Dicklesworthstone/franken_manim/commit/deebd8bc) `fmd-math` core consumed — the TeX-math engine enters the closure.
- [`92a57a7a`](https://github.com/Dicklesworthstone/franken_manim/commit/92a57a7a) The span map consumed — source-identity matching replaces the two-render hack.
- [`1facee3`](https://github.com/Dicklesworthstone/franken_manim/commit/1facee3) Owned PNG codec — full decode matrix + deterministic encode.
- [`b08f8a80`](https://github.com/Dicklesworthstone/franken_manim/commit/b08f8a80135ea8c9b9bf0d7df470f16ca3d0f1b8) Analytic fill core — exact on the curves, not on chords.
- [`ce0e5e37`](https://github.com/Dicklesworthstone/franken_manim/commit/ce0e5e372b8a2269edbc014306cb22a61e668feb) §10.1's certified CPU engine — the frame, the bands, the bits.

---

## 2) Scene runtime, production Python bridge, Studio/Metal, WASM (2026-07-27 → 2026-08-02)

With a CPU engine in place, the product grows the two front doors (Rust CLI / Python `manimlib`) and the Studio, plus a first wasm32 frame renderer.

### Delivered capability

- Rational-clock Scene runtime; crash-isolated Studio worker replay; secure Studio surfaces; native Metal Studio presentation and glow/lit Metal annex.
- Production `manimlib` bridge (`fm-aqv`); G0-5 already proved real subclassing on a prototype.
- Adaptive fused coverage AA; SIMD-tier checkpoints; FrameArena pools; FMTL/1 timeline-bundle writer.
- Geometry library: vector fields, solids, image/obj/pointcloud families; certified planar path booleans.
- `wasm32` tier-1 frame renderer + local wasm player smoke harness.
- A large fail-closed bounding campaign (Studio IPC, tile cache, Typeset codec, mobject arena) so untrusted input refuses rather than panics.

### Closed workstreams

- [`fm-aqv`](https://github.com/Dicklesworthstone/franken_manim/blob/main/.beads/issues.jsonl) production manimlib bridge.
- [`fm-yh0`](https://github.com/Dicklesworthstone/franken_manim/blob/main/.beads/issues.jsonl) Studio architecture / secure surfaces.
- [`fm-xsz`](https://github.com/Dicklesworthstone/franken_manim/blob/main/.beads/issues.jsonl) Metal compute / presentation.
- [`fm-l97`](https://github.com/Dicklesworthstone/franken_manim/blob/main/.beads/issues.jsonl) wasm32 frame renderer.
- [`fm-oee`](https://github.com/Dicklesworthstone/franken_manim/blob/main/.beads/issues.jsonl) FMTL/1 timeline bundle.

### Representative commits

- [`dac6fc9a`](https://github.com/Dicklesworthstone/franken_manim/commit/dac6fc9a6ca97542961d5b1235108230c15a4ef0) Land production manimlib bridge.
- [`a60a3ec3`](https://github.com/Dicklesworthstone/franken_manim/commit/a60a3ec3) Crash-isolated Studio worker replay.
- [`6aa4e58a`](https://github.com/Dicklesworthstone/franken_manim/commit/6aa4e58a) Render glow and lit surfaces through Metal.
- [`10808302`](https://github.com/Dicklesworthstone/franken_manim/commit/10808302) Wire native Metal Studio presentation.
- [`62a0061e`](https://github.com/Dicklesworthstone/franken_manim/commit/62a0061ee9f69ab9471588bfabb772d2056523bd) Tier-1 wasm32 frame renderer surface + workspace member.
- [`3abd5137`](https://github.com/Dicklesworthstone/franken_manim/commit/3abd5137) FMTL/1 timeline-bundle writer with export-time bit-identity proof.
- [`a8f53b37`](https://github.com/Dicklesworthstone/franken_manim/commit/a8f53b37) Vector fields, solids, image/obj/pointcloud mobject families.

---

## 3) `v0.1.0` — early native preview (through 2026-08-15)

First tagged/GitHub pre-release. Native `fmn` distribution on Linux/macOS/Windows via DSR (not GitHub Actions). The optional Python wheel is explicitly **not** in this cut.

### Delivered capability

- Native `fmn` CLI and Studio/runtime surfaces; certified-CPU rendering foundations; native text/math/output stacks.
- Fail-closed PG-8 definition/measurement front door.
- CLI PNG/GIF output for `@builtin` / FMTL; Studio terminal preview; Python portal separated from the standalone CLI (`fm-7wm.3`).
- Corpus-import harness on the seed allowlist; Reference Mobject container protocol.

GitHub Release: [FrankenManim v0.1.0 — early native preview](https://github.com/Dicklesworthstone/franken_manim/releases/tag/v0.1.0) (prerelease, published 2026-08-15). Peel commit [`ca0e830`](https://github.com/Dicklesworthstone/franken_manim/commit/ca0e83032eedadfe56a6f541a11903fc2c9d6665).

### Closed workstreams

- [`fm-7wm.3`](https://github.com/Dicklesworthstone/franken_manim/blob/main/.beads/issues.jsonl) CPython runtime boundary for one-binary CLI.
- [`fm-d3gt`](https://github.com/Dicklesworthstone/franken_manim/blob/main/.beads/issues.jsonl) Python corpus harness (in flight through later tags).

### Representative commits

- [`479f5af`](https://github.com/Dicklesworthstone/franken_manim/commit/479f5af) Separate Python portal from standalone CLI.
- [`5cd7704`](https://github.com/Dicklesworthstone/franken_manim/commit/5cd7704) Native single PNG and GIF output for `@builtin` / FMTL.
- [`52f8576`](https://github.com/Dicklesworthstone/franken_manim/commit/52f8576) Wire Studio terminal preview.
- [`149c2dd`](https://github.com/Dicklesworthstone/franken_manim/commit/149c2dd) Corpus-import harness green on the whole seed allowlist.
- [`ca0e830`](https://github.com/Dicklesworthstone/franken_manim/commit/ca0e83032eedadfe56a6f541a11903fc2c9d6665) `v0.1.0` tag peel (PG-8 producer evidence).

---

## 4) `v0.2.0` — production Python-render preview (2026-08-16)

The portal stops being a lifecycle stub: standard-mode PNG sequences pass captured scene frames through the shared retained Lumen CPU renderer and Reel's ordered, atomic PNG publisher.

### Delivered capability

- Successful `fmn-python` render means real pixels, not construct-only.
- Workspace preview bumped to 0.2.0; DSR preview cut filed as `fm-o1tp`.

GitHub Release: [FrankenManim v0.2.0 — production Python-render preview](https://github.com/Dicklesworthstone/franken_manim/releases/tag/v0.2.0) (prerelease, published 2026-08-16). Peel commit [`88b431e`](https://github.com/Dicklesworthstone/franken_manim/commit/88b431e17d8e1cf3c3b09d91b82b6ae06f2f3ba9).

### Closed workstreams

- [`fm-o1tp`](https://github.com/Dicklesworthstone/franken_manim/blob/main/.beads/issues.jsonl) v0.2.0 DSR preview cut.

### Representative commits

- [`2da7323`](https://github.com/Dicklesworthstone/franken_manim/commit/2da73238109f4691f8ff574928d0ba76bff08951) Bump the workspace preview to 0.2.0.
- [`e808471`](https://github.com/Dicklesworthstone/franken_manim/commit/e808471) Record v0.2.0 artifact evidence.
- [`88b431e`](https://github.com/Dicklesworthstone/franken_manim/commit/88b431e17d8e1cf3c3b09d91b82b6ae06f2f3ba9) Record v0.2.0 release gate evidence (tag peel).

---

## 5) `v0.3.0` — source-unedited final-still preview (2026-08-16 / published 2026-08-17)

The optional `fmn-python` portal executes all eight locked G4a corpus scenes **source-unedited** and renders a standard-mode PNG sequence or atomic final-state PNG through retained Lumen/Reel.

### Delivered capability

- Source-unedited G4a corpus → decodable, non-uniform final PNGs; same-process byte reproduction.
- Workspace bumped to 0.3.0 (`fm-7wm.5`).

GitHub Release: [FrankenManim v0.3.0 — source-unedited final-still preview](https://github.com/Dicklesworthstone/franken_manim/releases/tag/v0.3.0) (prerelease, published 2026-08-17). Peel commit [`82c3daa`](https://github.com/Dicklesworthstone/franken_manim/commit/82c3daa84129e31d742e691e3835366b7356d1c4).

### Closed workstreams

- [`fm-7wm.5`](https://github.com/Dicklesworthstone/franken_manim/blob/main/.beads/issues.jsonl) v0.3.0 DSR preview cut.

### Representative commits

- [`82c3daa`](https://github.com/Dicklesworthstone/franken_manim/commit/82c3daa84129e31d742e691e3835366b7356d1c4) Bump workspace to 0.3.0 and file the DSR preview cut.
- [`1cfe5be`](https://github.com/Dicklesworthstone/franken_manim/commit/1cfe5be) Record v0.3.0 artifact receipts.

---

## 6) `v0.4.0` — native Studio/Metal and threaded browser preview (2026-08-17 → 2026-08-18)

The standalone `fmn` distribution grows a checksummed native installer and compile provenance in robot version output. Studio Metal exports go through the CLI. WASM ships a verifiable npm browser artifact with an explicit threaded frame pool.

### Delivered capability

- Offline Metal exports through the CLI (`fm-sq8.3`).
- Verifiable npm WASM package; explicit threaded WASM frame pool (`fm-zsu`).
- Mean-value colour fields over multiple contours (`fm-ap9`).
- Workspace bumped to 0.4.0 (`fm-fqt2`).

GitHub Release: [FrankenManim v0.4.0 — native Studio/Metal and threaded browser preview](https://github.com/Dicklesworthstone/franken_manim/releases/tag/v0.4.0) (prerelease, published 2026-08-18). Peel commit [`d1e4274`](https://github.com/Dicklesworthstone/franken_manim/commit/d1e4274aa32ba5c59934029db397bbbeb9bfa18c).

### Closed workstreams

- [`fm-fqt2`](https://github.com/Dicklesworthstone/franken_manim/blob/main/.beads/issues.jsonl) v0.4.0 release bead.
- [`fm-zsu`](https://github.com/Dicklesworthstone/franken_manim/blob/main/.beads/issues.jsonl) threaded WASM package proof.
- [`fm-sq8.3`](https://github.com/Dicklesworthstone/franken_manim/blob/main/.beads/issues.jsonl) offline Metal CLI exports.

### Representative commits

- [`6771769`](https://github.com/Dicklesworthstone/franken_manim/commit/67717693b82964f1bb2d59b07e9c64071f48c5c8) Wire offline Metal exports through the CLI.
- [`cbd174e`](https://github.com/Dicklesworthstone/franken_manim/commit/cbd174e) Ship verifiable npm browser artifact.
- [`cbd01b9`](https://github.com/Dicklesworthstone/franken_manim/commit/cbd01b9b0e0a89f9c46def7909b180e2b1e2a743) Ship explicit threaded WASM frame pool.
- [`1f73a7a`](https://github.com/Dicklesworthstone/franken_manim/commit/1f73a7a) Mean-value colour fields over multiple contours.
- [`d1e4274`](https://github.com/Dicklesworthstone/franken_manim/commit/d1e4274aa32ba5c59934029db397bbbeb9bfa18c) Bump workspace to 0.4.0.

---

## 7) Post-`v0.4.0` portal parity and Aug 19 janitor (2026-08-18 → 2026-08-19)

After the tag, the Python portal keeps closing G4a composition gaps, and the repo-janitor relocates planning docs.

### Delivered capability

- Native ImageMobject with local raster path resolution; pickle preserves native renderer state; native Arc lineage wired through the portal.
- CameraFrame updaters run before drawable roots; BeamSplitter brace composition; 3D elbow visibility; finish-time zero-dt updater epilogue.
- Hermetic ffmpeg TMPDIR isolation; untracked WASM inputs dirty package identity.
- Janitor: `COMPREHENSIVE_PLAN_FOR_THE_DESIGN_OF_FRANKEN_MANIM.md` moved to `docs/planning/` (README/AGENTS updated in the same commit); `UPGRADE_LOG.md` moved to `docs/planning/` (README never listed it).

### Closed workstreams

- [`fm-5wq.4.21` … `fm-5wq.4.27`](https://github.com/Dicklesworthstone/franken_manim/blob/main/.beads/issues.jsonl) portal family: finish updater, BeamSplitter, elbows, ImageMobject, pickle, Arc lineage.
- [`fm-d72l`](https://github.com/Dicklesworthstone/franken_manim/blob/main/.beads/issues.jsonl) camera-frame updater parity.

### Representative commits

- [`b805605`](https://github.com/Dicklesworthstone/franken_manim/commit/b805605) Expose native ImageMobject with local raster path resolution.
- [`9849908`](https://github.com/Dicklesworthstone/franken_manim/commit/9849908) Preserve native renderer state through pickle.
- [`8332c0f`](https://github.com/Dicklesworthstone/franken_manim/commit/8332c0f) Wire native arc lineage.
- [`db21e30`](https://github.com/Dicklesworthstone/franken_manim/commit/db21e308865313ac058eb97960d8f40c428aaf64) Untrack skill-loop scratch; move root planning docs into `docs/planning/`.
- [`0d74a2b`](https://github.com/Dicklesworthstone/franken_manim/commit/0d74a2ba602232915a6d3f05f4d84dde736a22cd) Relocate remaining root reports and planning docs (`UPGRADE_LOG.md`).

---

## Notes for Agents

- Start with the version timeline if you need chronology. All four GitHub Releases are **prereleases**; do not treat `v0.4.0` as a 1.0 claim. The README is present-tense target state (G0→G5).
- The Python portal (`fmn-python`) is a separately installed host-CPython wheel; the native `fmn` / Studio binary is CPython-free. That split landed in `fm-7wm.3` before `v0.1.0`.
- Certified/`--reproducible` bit-identity, video containers, and the full SIMD-tier artifact matrix are still outside the tagged previews (called out in the v0.1.0 Release notes).
- Master plan lives at [`docs/planning/COMPREHENSIVE_PLAN_FOR_THE_DESIGN_OF_FRANKEN_MANIM.md`](https://github.com/Dicklesworthstone/franken_manim/blob/main/docs/planning/COMPREHENSIVE_PLAN_FOR_THE_DESIGN_OF_FRANKEN_MANIM.md) after the 2026-08-19 janitor. README links were updated in the same commit; do not look for the plan at repo root.
- Tracker of record is [`.beads/issues.jsonl`](https://github.com/Dicklesworthstone/franken_manim/blob/main/.beads/issues.jsonl). Generate a nonmutating operational view with `python3 scripts/generate_agent_brief.py --stdout`; use `python3 scripts/agent_brief.py --format next --check` before claiming work.
- The mandatory verification surface is `scripts/check.sh` on controlled local or owned build hosts. Hosted GitHub Actions availability is not a correctness premise.
- There is no `origin/master` on this remote.
