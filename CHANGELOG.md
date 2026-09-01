# Changelog

This is the evidence-bounded project changelog for **franken_manim**, a sovereign deterministic rewrite of 3Blue1Brown's `manim` in pure Rust with a separately installed `manimlib`-compatible Python portal.

The repository remains **pre-1.0**. Tagged releases `v0.1.0` through `v0.4.0` are prereleases. Source, compatibility rulings, task state, and release evidence are intentionally kept distinct:

- source code says what behavior exists;
- `API_SCHEMA.tsv` plus `API_OVERLAY.tsv` say what compatibility status is claimed;
- `.beads/issues.jsonl` says what work is open or blocked;
- gates and retained artifacts say what was actually exercised.

The current unreleased source checkpoint covered here is **[`60ec415`](https://github.com/Dicklesworthstone/franken_manim/commit/60ec415bb86685bebe5bad6e3a156d2df8dfd186)** on 2026-09-01 UTC. Later documentation-only commits truth up that implementation state.

---

## Unreleased — executable boundaries and agent control plane

### Added

- A fail-closed claim planner, `scripts/agent_next.py`, layered over the bounded Beads graph projection.
  - A task with any live `parent-child` descendant is a container even when its issue type is not `epic`.
  - Assigned work, epic containers, and topology-derived containers are never autonomous recommendations.
  - Work in an already-active workstream wins before activating another stream.
  - Equal-priority leaves are ordered by immediate-unblock pressure, direct blocker pressure, scope, recency, and lexical ID.
  - Canonical compact JSON is published as `fmn.agent.next` version 1.
  - Exit 1 means unsafe graph or activation state, exit 2 malformed input, and exit 3 a valid graph with no claimable leaf.
- Six focused claim-planner regressions covering non-epic parents, closed-child release, assigned/epic exclusion, active-workstream preference, unblock pressure, canonical JSON, and fail-closed exits.
- A dedicated process-output publisher for the shipped `fmn` binary.
  - stdout and stderr are attempted independently.
  - `BrokenPipe` is treated as normal downstream closure and preserves the command's typed exit code.
  - a stdout failure cannot suppress a typed stderr diagnostic.
  - non-broken output failures remain internal failures with deterministic precedence.
  - five deterministic writer tests cover the full policy.
- The mandatory local/owned-host gate now compiles and tests the graph parser, claim planner, and deterministic brief generator, validates the live claim plan, and then proceeds into the Rust, Python, WASM, and crate-DAG gates.

### Changed

- The agent control plane is now an explicit tower of linked abstractions:
  1. Beads is task authority.
  2. `agent_brief.py` owns bounded parsing, graph integrity, and broad situational state.
  3. `agent_next.py` owns leaf-safe claim selection.
  4. `generate_agent_brief.py` owns deterministic human rendering.
- Hosted GitHub Actions is no longer described as part of the correctness authority chain. The repository gate is designed for local or owned build hosts.
- Implementation-status documentation now corrects an earlier overstatement: human `doctor --quiet` suppression still occurs in the binary entry point; it is not yet centralized for embedded `run_with_capabilities` callers.

### Fixed

- Closed stdout pipes no longer prevent stderr publication or replace a previously determined usage, capability, render, or other typed exit code.
- Non-epic parent tasks can no longer be returned as autonomous leaf recommendations while live children remain.
- Same-priority recommendations no longer ignore how much blocked work completion would immediately release.

### Representative commits

- [`b5518dd`](https://github.com/Dicklesworthstone/franken_manim/commit/b5518dd41fb21ffcd3d53e64c66779ace46e4d2d) — add deterministic two-stream output publication.
- [`d1cdc02`](https://github.com/Dicklesworthstone/franken_manim/commit/d1cdc0202b4810d86bb9800dc879bce1a9befae6) — wire independent stdout/stderr publication into the shipped binary.
- [`c3b6ede`](https://github.com/Dicklesworthstone/franken_manim/commit/c3b6ede2fc0c46c30ebd8c179c949616e5e3fe10) — add the leaf-aware Beads claim planner.
- [`99eb4ef`](https://github.com/Dicklesworthstone/franken_manim/commit/99eb4ef01cb3e23b9da12c76a12c35781d947db3) — lock leaf and blocker-pressure semantics with focused tests.
- [`60ec415`](https://github.com/Dicklesworthstone/franken_manim/commit/60ec415bb86685bebe5bad6e3a156d2df8dfd186) — make leaf-safe live triage part of the mandatory local gate.

---

## Unreleased — prior August 31 control-plane and front-door tranche

### Added and fixed

- The shipped `fmn` executable gained a real one-scene batch smoke that renders a native PNG and verifies `manifest.fmnp` plus `manifest.txt` publication.
- Robot batch output gained explicit terminal success records rather than requiring success inference from aggregate output.
- The Python/Rust geometry-constructor boundary became mechanical: Rust-only snake_case helpers cannot leak into the pinned Python wildcard namespace, while all corresponding Reference CamelCase classes remain required in schema, wrapper, and clean-wheel probes.
- The Beads broad projection gained bounded JSONL parsing, known-status enforcement, dependency ownership checks, duplicate/self-edge refusal, missing-target reporting, iterative blocking-cycle detection, stale/unowned/unscoped queues, and activation-cap enforcement.
- Deterministic brief generation uses the newest ledger timestamp instead of wall-clock time, publishes atomically, refuses symlinks and stale temporary paths, and leaves prior output untouched on malformed state.

### Representative commits

- [`82d915e`](https://github.com/Dicklesworthstone/franken_manim/commit/82d915ee34285ea7ab2198e76e831ee23bf88250) — smoke a real batch render and manifest publication.
- [`0fc2fc7`](https://github.com/Dicklesworthstone/franken_manim/commit/0fc2fc7fd4ec5f7a21820faf471228797b72670e) — enforce the Rust-helper / Reference-class Python boundary.
- [`40df985`](https://github.com/Dicklesworthstone/franken_manim/commit/40df985fc40d35ea7bdbb80a71608bad16d7d724) — reject vacuous package alias guards.
- [`5ce894f`](https://github.com/Dicklesworthstone/franken_manim/commit/5ce894f888a71d6fae7c82fd685c84d51e1902d3) — fail closed on malformed Beads dependency graphs.
- [`4629943`](https://github.com/Dicklesworthstone/franken_manim/commit/4629943ccdb0bfe14829dc1f94c8b88dcb0446b1) — make cycle analysis iterative for deep valid graphs.
- [`403c1f4`](https://github.com/Dicklesworthstone/franken_manim/commit/403c1f4c5a772ab228dc0f2c14739b7e8c954f7e) — prove corrupt graphs never publish briefs.

---

## Release timeline

| Version | Date | Evidence-bounded summary |
|---|---:|---|
| Project inception | 2026-07-20 | Rev-4 comprehensive plan, workspace governance, Beads graph, and architecture doctrine. |
| [`v0.1.0`](https://github.com/Dicklesworthstone/franken_manim/releases/tag/v0.1.0) prerelease | 2026-08-15 | Early native preview: standalone `fmn` CLI, scene/runtime foundations, native renderer and output stack; no Python wheel in this cut. |
| [`v0.2.0`](https://github.com/Dicklesworthstone/franken_manim/releases/tag/v0.2.0) prerelease | 2026-08-16 | Production Python-render preview: portal scenes produce real retained-renderer PNG sequences. |
| [`v0.3.0`](https://github.com/Dicklesworthstone/franken_manim/releases/tag/v0.3.0) prerelease | 2026-08-16, published 2026-08-17 | Source-unedited final-still preview for the locked seed corpus. |
| [`v0.4.0`](https://github.com/Dicklesworthstone/franken_manim/releases/tag/v0.4.0) prerelease | 2026-08-18 | Native Studio/Metal and threaded-browser preview; checksummed native installer and WASM package foundations. |
| Current unreleased line | 2026-08-19 onward | Broad Python portal convergence, Studio/runtime hardening, distribution gates, executable CLI smoke, namespace truthfulness, and fail-closed agent operations. |

---

## Major implementation phases

### 1. Substrate, geometry, scene data, animation, and native typesetting

The initial implementation established the shared semantic substrate:

- deterministic constants, color, rate functions, RNG, numeric canonicalization, and rational frame time;
- QuadPath geometry and true-arclength behavior;
- Marionette Stage/RecordBuffer object state, identity, views, snapshots, and copy semantics;
- Choreo animations, composition, timeline, and conservative purity vocabulary;
- native text and TeX-math through the Scribe stack, without a LaTeX fallback;
- governed platform capabilities and content-addressed cache foundations.

### 2. Certified retained rendering, codecs, output, 3D, and WASM

The rendering and media plane added:

- analytic curve fill and true curve-distance strokes;
- retained tile rendering, deterministic ordering, camera, clipping, depth, lighting, glow, and textures;
- owned DEFLATE, PNG, JPEG, GIF, y4m, and WAV paths;
- typed ffmpeg capability negotiation for video containers;
- FMTL timeline bundles with export-time reconstruction proof;
- wasm32 render/player foundations and browser-package gates.

### 3. Rust and Python front doors, Studio, and conformance

The product front doors and Gauntlet added:

- a first-class Rust composition API and native `fmn` executable;
- the separately installed CPython portal with subclassing, live state, copy/pickle behavior, authored compatibility layers, and real pixel output;
- crash-isolated Studio worker/supervisor, replay, interaction, inspection, and presentation foundations;
- one generated API schema/overlay and symbol-granular Parity Ledger;
- corpus, self-golden, differential, determinism, fuzzing, performance, and packaging gates.

### 4. Portal convergence and truthfulness ratchets

Post-`v0.4.0` work substantially broadened the `manimlib` surface across geometry, mobjects, scenes, animations, camera, interactive controls, file writer, drawings, utilities, and capability-excluded OpenGL/host surfaces.

A 100% reviewed ledger is not interpreted as universal implementation. `fm-5wq.4` remains open because its newest reality-check identifies remaining portal implementation and representative-wheel obligations that supersede older completion commentary.

---

## Current evidence boundaries

The following are **not** inferred from a local green source tree:

- cross-platform release artifacts;
- real aarch64 topology evidence;
- platform-native SIMD and certified bit-identity matrices;
- real-browser npm/WASM publication;
- clean-wheel behavior on every supported Python/platform pair;
- ffmpeg/video-container equivalence receipts;
- closure of `fm-5wq.4` or the W10 epic.

Hosted GitHub Actions availability is not a correctness dependency. The authoritative gate is `scripts/check.sh` run on an exact local or owned-host checkout, with the source commit and any opt-in release axes recorded alongside the result.

## Agent notes

- Start with `docs/IMPLEMENTATION_STATUS.md` for present-tense evidence.
- Use `scripts/agent_next.py`, not the legacy broad `agent_brief.py --format next`, for autonomous claim selection.
- Treat Beads as authoritative and mutate it only through `br` followed by `br sync --flush-only`.
- Never replace `.beads/issues.jsonl` from truncated connector output.
- Do not turn “reviewed,” “compiled,” “imported,” or “historically green” into a stronger implementation or release claim.
