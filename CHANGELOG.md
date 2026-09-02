# Changelog

This is the evidence-bounded project changelog for **franken_manim**, a sovereign deterministic rewrite of 3Blue1Brown's `manim` in pure Rust with a separately installed `manimlib`-compatible Python portal.

The repository remains **pre-1.0**. Tagged releases `v0.1.0` through `v0.4.0` are prereleases. Source behavior, compatibility rulings, task state, and release evidence remain distinct:

- source code says what behavior exists;
- `API_SCHEMA.tsv` plus `API_OVERLAY.tsv` say what compatibility status is claimed;
- `.beads/issues.jsonl` says what work is open, active, blocked, or closed;
- gates and retained artifacts say what was actually exercised.

The current unreleased substantive source-and-test checkpoint covered here is **[`fffc469`](https://github.com/Dicklesworthstone/franken_manim/commit/fffc469e99ac6e9b79e66ddd890fc98238e3f3c9)** on 2026-09-02 UTC. Later documentation commits truth up that implementation state.

---

## Unreleased — scoped autonomous planning

### Added

- An exact autonomous-workstream grammar in `scripts/agent_next.py`:
  - `G0`;
  - `W1` through `W11`;
  - beginning at the first title character and followed by a word boundary or `:`.
- Explicit `unscoped_leaves` and `unscoped_active` planner evidence.
- Human-brief normalization that uses the planner's governed activation state and workstream labels rather than the broad projection's historical grouping.
- Cross-layer regressions proving the same scope policy reaches the planner, claim token, generated human brief, and activation cap.

### Changed

- `fmn.agent.next` advanced to schema version `4`.
- Open unscoped leaves remain visible for repair but never enter autonomous ranking.
- A valid governed leaf wins even when an unscoped leaf has a numerically higher priority.
- Any active unscoped issue invalidates the planner before token, executor, or brief publication.
- `G0` now counts as one real active workstream. `G0` plus four W-streams is a five-stream cap breach.
- The generated brief checks the planner's activation result, suppresses an unscoped broad priority, and labels `G0` and invalid W-prefixes consistently with the claim contract.
- Claim tokens automatically bind planner version 4 through the existing schema contract; older planner tokens cannot revalidate.

### Fixed

- An issue without a governed workstream prefix can no longer be selected or atomically claimed by autonomous tooling.
- Titles such as `W12: ...`, `W999: ...`, lower-case `w10: ...`, or embedded `prefix W10: ...` can no longer masquerade as governed workstreams.
- `G0` is no longer omitted from the activation count.
- The deterministic human brief can no longer accept a `G0` plus four-W-stream cap breach because its broad snapshot happened to classify `G0` as unscoped.
- A broad unscoped priority can no longer appear beside a different leaf-safe recommendation as though both were actionable.

### Representative commits

- [`88354b6`](https://github.com/Dicklesworthstone/franken_manim/commit/88354b68bc926580a1472196b0ad6476bc00f969) — refuse unscoped autonomous claims.
- [`57d8c3c`](https://github.com/Dicklesworthstone/franken_manim/commit/57d8c3c1c9ce1a460ce738e76af7a3172a627ea9) — lock scoped-only leaf planning.
- [`29c61d1`](https://github.com/Dicklesworthstone/franken_manim/commit/29c61d158d0812a5a1f45aa9b2f71e60f376fd1f) — enforce `G0` and `W1`–`W11` as the governed vocabulary.
- [`4ba640a`](https://github.com/Dicklesworthstone/franken_manim/commit/4ba640a1f49e123a0743e5f04ba3385b56888b43) — lock the exact workstream grammar.
- [`73bf062`](https://github.com/Dicklesworthstone/franken_manim/commit/73bf06220dbdd920f95c3bc17b97006749902952) — align deterministic human rendering with the planner scope.
- [`b66f06e`](https://github.com/Dicklesworthstone/franken_manim/commit/b66f06e5f6e1f0584e2cd9830d119e8c47d71329) — preserve blocker truth while normalizing rows.
- [`1abc050`](https://github.com/Dicklesworthstone/franken_manim/commit/1abc050bb60623859bd2209d55744501fbb871f0) — cover G0, unscoped priorities, unscoped active claims, and strict cap refusal.
- [`fffc469`](https://github.com/Dicklesworthstone/franken_manim/commit/fffc469e99ac6e9b79e66ddd890fc98238e3f3c9) — propagate scoped semantics through claim-guard tokens.

### Evidence boundary

A focused 12-case planner harness passed against the exact planner blob using a faithful `agent_brief` test seam, and the planner source passed Python bytecode compilation. The committed test suites are part of the existing `scripts/check.sh` gate, but this editing environment did not contain the full exact checkout, Cargo toolchain, `br`, or UBS. Therefore the complete repository gate, Rust axes, live-ledger projection, and tracker mutation are not claimed as executed for this tranche.

---

## Unreleased — atomic guarded Beads claims

### Added and changed

- `scripts/agent_claim.py` uses Beads' storage-level atomic claim primitive:

  ```text
  br update ISSUE --claim --actor ASSIGNEE --json [--transition-comment TEXT]
  ```

- `fmn.agent.claim` is schema version `6`.
- Atomic response JSON is bounded by byte, structural-depth, and node-count limits and rejects duplicate keys, malformed UTF-8, non-finite numbers, malformed envelopes, multiple updated rows, identity drift, and success-path stderr.
- The response timestamp must agree with the explicitly exported JSONL row.
- Successful mutation receipts include normalized atomic-response evidence and a represented-field post-export claim delta.
- The clone-local lock lives in Git's shared `commondir`, so the primary checkout and linked worktrees contend on one persistent inode.

### Resource policy

Each `br update --claim` and `br sync --flush-only` child has:

- a default 60-second wall-clock deadline, configurable up to 3,600 seconds;
- a default 16 MiB combined stdout/stderr production budget, configurable up to 1 GiB;
- a 1 MiB retained diagnostic ceiling per stream;
- bounded process-tree termination and reader cleanup.

A stalled, continuously producing, or inherited-pipe child cannot hold the claim lock indefinitely. Exit `5` means no verified receipt, not proof that native tracker state is unchanged.

### Canonical-graph boundary

The token and post-export delta currently bind the field set represented by `agent_brief.Issue`: ID, title, status, priority, issue type, assignee, normalized update timestamp, dependencies, and comments.

Description, design, acceptance criteria, notes, owner, estimates, dates, labels, and unknown extension fields are not yet included. A version-6 receipt is not raw JSONL or whole-database identity; `br show` and external coordination remain mandatory before invocation.

---

## Unreleased — graph- and policy-bound recommendations

- `agent_claim_guard.py` emits tokens of the form `v2:<claim-sha256>:<issue-id-or-none>`.
- The claim digest binds the canonical planning graph, complete planner output, policy values, and parser/planner/guard schema contract.
- `graph_sha256` remains graph-only evidence; `claim_sha256` is the complete token digest.
- Issue-row and dependency-array ordering are canonicalized, while semantic graph, policy, schema, or planner-output changes invalidate the token.
- The literal issue ID `none` is reserved for the no-recommendation sentinel.
- The unsafe broad `agent_brief.py --format next` spelling has been retired.

---

## Unreleased — deterministic agent control plane

- The Beads reader uses bounded regular-file input and strict JSONL framing.
- Duplicate JSON keys and IDs, invalid UTF-8, blank records, missing final LF, non-finite constants, malformed optional arrays, invalid statuses/timestamps, ownership errors, self-edges, duplicate edges, and finite resource-budget violations fail closed.
- Blocking and containment cycles suppress claims.
- Planner output and generated Markdown derive time from the newest ledger record rather than wall clock.
- Generated brief publication uses descriptor-bound reads, exclusive temporary creation, flush, fsync, and atomic replacement.
- Hosted GitHub Actions availability is not part of the correctness authority chain.

---

## Unreleased — CLI and portal truthfulness

- The shipped `fmn` process publishes stdout and stderr independently and preserves typed exits when downstream pipes close.
- Human `doctor --quiet` behavior is owned by the reusable dispatcher; robot records and typed failures remain visible.
- Batch robot mode emits explicit terminal per-job success records.
- A shipped-binary smoke renders a tiny FMTL scene and verifies native PNG and machine/human manifest publication.
- Rust-only snake_case geometry helpers cannot leak into the pinned Reference wildcard namespace.
- Explicit Python portal refusals are parsed into a canonical bounded inventory; anonymous placeholders fail the mandatory gate.
- `fm-5wq.4` remains open: reviewed parity rows and inventoried refusals are not universal callable implementation.

---

## Release timeline

| Version | Date | Evidence-bounded summary |
|---|---:|---|
| Project inception | 2026-07-20 | Revision-4 comprehensive plan, workspace governance, Beads graph, and architecture doctrine. |
| `v0.1.0` prerelease | 2026-08-15 | Early native preview: standalone CLI, scene/runtime foundations, renderer, and output stack. |
| `v0.2.0` prerelease | 2026-08-16 | Production Python-render preview: portal scenes produce retained-renderer PNG sequences. |
| `v0.3.0` prerelease | 2026-08-16; published 2026-08-17 | Source-unedited final-still preview for the locked seed corpus. |
| `v0.4.0` prerelease | 2026-08-18 | Native Studio/Metal and threaded-browser preview; installer and WASM package foundations. |
| Current unreleased line | 2026-08-19 onward | Portal convergence, Studio/runtime hardening, distribution gates, executable smoke, and fail-closed agent operations. |

## Major implementation phases

1. **Substrate and semantics:** constants, color, rates, RNG, numeric canonicalization, rational frame time, QuadPath geometry, Marionette state, Choreo timelines, native text, and math.
2. **Rendering and media:** analytic fills/strokes, retained tiles, camera/depth/lighting/textures, native codecs, ffmpeg boundary, FMTL, and WASM foundations.
3. **Front doors and Gauntlet:** Rust API, `fmn`, optional CPython portal, Studio supervisor/worker, API schema, Parity Ledger, self-goldens, differential tests, determinism, fuzzing, performance, and packaging gates.
4. **Portal convergence:** authored `manimlib` compatibility, precise capability refusals, executable wheel/bridge tests, and namespace/refusal ratchets.
5. **Agent operations:** strict graph parsing, governed leaf planning, stale-plan tokens, atomic claims, shared-local locking, structured response proof, represented-field deltas, and bounded subprocess resources.

## Current evidence boundaries

The following are not inferred from a focused source probe or from documentation:

- the complete local repository gate on the latest commit;
- cross-platform release artifacts;
- real aarch64 topology evidence;
- platform-native SIMD and certified bit-identity matrices;
- real-browser npm/WASM publication;
- clean-wheel behavior on every supported Python/platform pair;
- ffmpeg/video-container equivalence receipts;
- closure of `fm-5wq.4` or the W10 epic.

Hosted GitHub Actions availability is not a correctness dependency. The authoritative gate is `scripts/check.sh` run on an exact local or owned-host checkout, with the source commit and opt-in release axes recorded alongside the result.

## Agent notes

- Start with `docs/IMPLEMENTATION_STATUS.md` for present-tense evidence.
- Use `agent_next.py` for governed selection, `agent_claim_guard.py` for the bound token, and `agent_claim.py` for atomic mutation.
- Treat `agent_brief.py` as broad situational context only.
- Treat Beads as authoritative; commit the `.beads/` export explicitly after every mutation.
- On executor exit `5`, inspect Beads and the working tree before doing anything else; never replay the old token blindly.
- Never replace `.beads/issues.jsonl` from truncated connector output.
- Do not turn “reviewed,” “compiled,” “inventoried,” or “historically green” into a stronger implementation or release claim.
