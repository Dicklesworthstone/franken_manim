# Changelog

This is the evidence-bounded project changelog for **franken_manim**, a sovereign deterministic rewrite of 3Blue1Brown's `manim` in pure Rust with a separately installed `manimlib`-compatible Python portal.

The repository remains **pre-1.0**. Tagged releases `v0.1.0` through `v0.4.0` are prereleases. Source, compatibility rulings, task state, and release evidence remain distinct:

- source code says what behavior exists;
- `API_SCHEMA.tsv` plus `API_OVERLAY.tsv` say what compatibility status is claimed;
- `.beads/issues.jsonl` says what work is open or blocked;
- gates and retained artifacts say what was actually exercised.

The current unreleased substantive source-and-test checkpoint covered here is **[`c699278`](https://github.com/Dicklesworthstone/franken_manim/commit/c699278da29c8979772529d28cbc13f6491c2f37)** on 2026-09-01 UTC. Later documentation commits truth up that implementation state.

---

## Unreleased — atomic guarded Beads claims

### Added

- Storage-level atomic claim execution through Beads' dedicated primitive:

  ```text
  br update ISSUE --claim --actor ASSIGNEE --json [--transition-comment TEXT]
  ```

  This replaces the former generic `--status in_progress --assignee ...` mutation in the autonomous claim path.
- Strict validation of the successful Beads JSON response before the explicit export step.
  - UTF-8 and JSON syntax are required.
  - Duplicate object keys and non-finite numeric constants are rejected.
  - Success-path stderr is refused.
  - Exactly one updated issue must be reported.
  - ID, title, priority, status, assignee, and timestamp must match the guarded transition.
  - Both the ordinary issue-array response and the `{updated, warnings}` capacity-warning envelope are accepted.
- `atomic_claim` evidence in successful mutating receipts, including response shape, issue, assignee, status, timestamp, and warning count.
- Caller-configurable combined child-output production budget:
  - CLI: `--command-output-budget-bytes`;
  - default: 16 MiB per child command;
  - maximum: 1 GiB;
  - exact-bound output succeeds and the first byte beyond the bound triggers process-tree cleanup.

### Changed

- Claim receipt schema `fmn.agent.claim` advanced to version `5`.
- `executor_policy` now names `beads.update.claim/v1` and records the timeout, retained per-stream bytes, and configured combined production ceiling.
- The successful proof chain is now explicit and two-stage:
  1. strict semantic validation of Beads' atomic JSON response;
  2. exact comparison of the complete exported parsed graph before and after `br sync --flush-only`.
- The atomic response timestamp must equal the exported issue timestamp.
- The mandatory gate's live-ledger dry-run now renders the exact atomic claim argv and explicit timeout/output-budget policy.

### Fixed

- A direct manual status/assignee update can no longer win the local interval between guard validation and the selected issue's storage-level unassigned check.
- A zero exit from `br update` is no longer accepted as claim evidence when its machine response is absent, malformed, ambiguous, or semantically inconsistent.
- Capacity-warning envelopes no longer require an unsafe fallback to human output parsing.
- The previously documented configurable output ceiling is now implemented by the CLI and injected-runner contract rather than existing only as a fixed internal constant.

### Representative commits

- [`531ddbc`](https://github.com/Dicklesworthstone/franken_manim/commit/531ddbcc180a34a0ec97580572d8411504259205) — use Beads' atomic claim primitive, strict JSON response proof, version-5 receipts, and configurable output budget.
- [`c800299`](https://github.com/Dicklesworthstone/franken_manim/commit/c800299c2a4125c974b32762b620fda42ed61c32) — lock atomic argv, ordinary/envelope response semantics, malformed-success refusal, timestamp agreement, and exact graph deltas.
- [`cc71d1c`](https://github.com/Dicklesworthstone/franken_manim/commit/cc71d1c2f7798d49eaa56fdb472b9b0507028870) — exercise configurable output ceilings with real child processes and CLI refusal cases.
- [`c699278`](https://github.com/Dicklesworthstone/franken_manim/commit/c699278da29c8979772529d28cbc13f6491c2f37) — make atomic claim composition part of the mandatory dry-run gate.

### Evidence boundary

The exact replacement source and test bytes passed Python bytecode compilation. Twenty-eight focused tests passed locally against those exact files: fourteen executor/receipt tests and fourteen real-child-process resource tests. Because the editing environment did not contain the repository checkout, the executor suite used minimal interface-compatible parser/planner/guard modules for its local composition run. Blob SHA-1 identities for the three committed Python files matched the GitHub content SHAs.

The complete repository-native Python suite, `scripts/check.sh`, Cargo, Clippy, rustdoc, WASM, wheel, browser, and release axes are not claimed as run at `c699278`.

No Beads record was created, claimed, or closed from this environment because tracker-native `br` execution was unavailable. The authoritative `.beads/issues.jsonl` was deliberately left untouched rather than reconstructed from connector output.

---

## Unreleased — bounded-output, exact-delta guarded claims

- Each child command has a shared total-produced-byte budget across stdout and stderr. Every produced chunk is counted before retained surplus is discarded; overflow triggers bounded process-tree cleanup.
- After explicit JSONL export, issue membership must be identical, every non-target issue must be unchanged, and the target may change only through the requested status, assignee, non-regressing timestamp, and optional exact appended transition comment.
- Successful mutating receipts carry normalized `claim_delta` evidence.

Representative commits: [`06bab2a`](https://github.com/Dicklesworthstone/franken_manim/commit/06bab2acbae2e92d649dc6314432193171e28cb3), [`15e7d25`](https://github.com/Dicklesworthstone/franken_manim/commit/15e7d25315d968e622fc65ca2cd58d219a88b486), [`2350609`](https://github.com/Dicklesworthstone/franken_manim/commit/2350609ba27a9ccd4f34988e0101330ac32e62cf), [`240bcc9`](https://github.com/Dicklesworthstone/franken_manim/commit/240bcc9bcea7c32100fa88b1f966aa40609f6c53), and [`0d56b27`](https://github.com/Dicklesworthstone/franken_manim/commit/0d56b27ec734fd6e60305faadbe28a69d7808a19).

---

## Unreleased — bounded claim-command lifetime

- Every production `br update` and `br sync --flush-only` invocation has a finite independent timeout: 60 seconds by default and at most 3,600 seconds.
- POSIX cleanup targets the complete child process group. Windows uses a new process group, best-effort `taskkill.exe /T /F`, and direct-child fallback.
- Process waits and output-reader joins are bounded; inherited descendant pipes cannot strand the repository claim lock indefinitely.

Representative commits: [`47cdf1f`](https://github.com/Dicklesworthstone/franken_manim/commit/47cdf1fa5af31ee3c4427b0b255435a8422c0246), [`da07b21`](https://github.com/Dicklesworthstone/franken_manim/commit/da07b210ba096d98ded4548d0960d1ef7eb5b606), [`d3b79e6`](https://github.com/Dicklesworthstone/franken_manim/commit/d3b79e6a15ca8f10ab846e92e245cf35b511336f), and [`301684d`](https://github.com/Dicklesworthstone/franken_manim/commit/301684d670eadced9c948a3859438f9aa8d755a1).

---

## Unreleased — guarded claim execution and strict ledger grammar

- `scripts/agent_claim.py` keeps token revalidation, mutation, explicit export, and postconditions inside one process and one shared-common-directory advisory-lock scope.
- A primary checkout and linked worktrees contend on one persistent lock inode.
- `.git` and `commondir` markers use bounded no-follow regular-file reads where supported.
- The strict ledger parser rejects malformed framing, duplicate keys and IDs, invalid statuses and timestamps, malformed optional arrays/comments/dependencies, self/duplicate edges, unquoted non-finite constants, and bounded-resource violations.
- The canonical claim-graph grammar is version `2`.

Representative commits include [`23dec4b`](https://github.com/Dicklesworthstone/franken_manim/commit/23dec4baa9a677703f31106f9fc0542733a68bf7), [`2e7a80b`](https://github.com/Dicklesworthstone/franken_manim/commit/2e7a80ba2020276213a8aad524624e7fe468e207), [`6169519`](https://github.com/Dicklesworthstone/franken_manim/commit/6169519af2bf586f19effeda10f4c2e71b39ebaa), [`4546c8f`](https://github.com/Dicklesworthstone/franken_manim/commit/4546c8f1631a6c67bc9d03a892972608bad1a0e1), [`bc09262`](https://github.com/Dicklesworthstone/franken_manim/commit/bc09262de628b3ea7fbebd1a9f8793fa7517f37c), [`6f1584d`](https://github.com/Dicklesworthstone/franken_manim/commit/6f1584dbc903d2920a9133e32d500daade279c36), [`95187b9`](https://github.com/Dicklesworthstone/franken_manim/commit/95187b950fdb078ffaea71d48008b5defeb678ca), and [`f96c3eb`](https://github.com/Dicklesworthstone/franken_manim/commit/f96c3eb12bdbafa7822e989d1a39702df28215a2).

---

## Unreleased — graph-and-policy-bound claim revalidation

- `scripts/agent_claim_guard.py` issues `v2:<claim-sha256>:<issue-id-or-none>` tokens.
- The digest binds the canonical graph, complete `fmn.agent.next` plan, normalized policy, and parser/planner/guard schemas.
- `graph_sha256` remains graph-only; `claim_sha256` is the complete token identity.
- The literal issue ID `none` is reserved for the no-recommendation token subject.

Representative commits: [`6a6b3c8`](https://github.com/Dicklesworthstone/franken_manim/commit/6a6b3c8fe52b34095958d3fe0e68ef55c62b6536), [`93877d4`](https://github.com/Dicklesworthstone/franken_manim/commit/93877d486254a61a1eee85c2c2c7dda22bd45f8f), [`5629a41`](https://github.com/Dicklesworthstone/franken_manim/commit/5629a41349d1fb0f4574ab8b8473348becd6a312), and [`d7a1716`](https://github.com/Dicklesworthstone/franken_manim/commit/d7a17163399c7347c39869bf09aa8744fceda5e4).

---

## Unreleased — strict deterministic task control plane

- Broad situational state and autonomous leaf selection are separate.
- Non-epic issues with live children are containers, not leaves.
- Missing blockers and blocking/containment cycles fail before plan, brief, token, or claim publication.
- Planner output is ledger-time-derived, canonical, bounded, and wall-clock independent.
- Deterministic brief publication uses bounded descriptor reads, exclusive temporary creation, fsync, and atomic replacement.
- The unsafe legacy `agent_brief.py --format next` output is retired.

Representative commits include [`9839a38`](https://github.com/Dicklesworthstone/franken_manim/commit/9839a3862dfca41083ad8f0a5b680edfd3cd17d9), [`9bf1d20`](https://github.com/Dicklesworthstone/franken_manim/commit/9bf1d20f94f3bd8d3cf252badfe57bb215236f2f), [`dcad037`](https://github.com/Dicklesworthstone/franken_manim/commit/dcad037a2bf52e010efa97b0cd1bb838e3f63cf4), [`f6c23f6`](https://github.com/Dicklesworthstone/franken_manim/commit/f6c23f65fa3f16d524589f1f4dba4b7ec654dcd0), and [`204eabf`](https://github.com/Dicklesworthstone/franken_manim/commit/204eabfdada854ad55d14de9c466fda652f1bd2f).

---

## Unreleased — executable boundaries and portal truthfulness

- The shipped `fmn` process publishes stdout and stderr independently and preserves typed exits across closed pipes.
- Successful human `doctor --quiet` suppression is owned by the reusable dispatcher, while robot records and typed failures remain visible.
- Batch robot mode emits terminal per-job success records and has a real one-scene artifact/manifest smoke.
- Rust-only snake_case helpers cannot leak into the pinned Reference Python wildcard namespace.
- Explicit portal refusals are parsed into a canonical bounded inventory and anonymous placeholders fail the mandatory gate.
- `fm-5wq.4` remains open: reviewed parity and inventoried refusals are not universal callable implementation.

Representative commits include [`b5518dd`](https://github.com/Dicklesworthstone/franken_manim/commit/b5518dd41fb21ffcd3d53e64c66779ace46e4d2d), [`d1cdc02`](https://github.com/Dicklesworthstone/franken_manim/commit/d1cdc0202b4810d86bb9800dc879bce1a9befae6), [`82d915e`](https://github.com/Dicklesworthstone/franken_manim/commit/82d915ee34285ea7ab2198e76e831ee23bf88250), and [`0fc2fc7`](https://github.com/Dicklesworthstone/franken_manim/commit/0fc2fc7fd4ec5f7a21820faf471228797b72670e).

---

## Release timeline

| Version | Date | Evidence-bounded summary |
|---|---:|---|
| Project inception | 2026-07-20 | Revision-4 plan, workspace governance, Beads graph, and architecture doctrine. |
| [`v0.1.0`](https://github.com/Dicklesworthstone/franken_manim/releases/tag/v0.1.0) prerelease | 2026-08-15 | Early native preview: standalone CLI, scene/runtime foundations, renderer, and output stack. |
| [`v0.2.0`](https://github.com/Dicklesworthstone/franken_manim/releases/tag/v0.2.0) prerelease | 2026-08-16 | Python-render preview producing real retained-renderer PNG sequences. |
| [`v0.3.0`](https://github.com/Dicklesworthstone/franken_manim/releases/tag/v0.3.0) prerelease | 2026-08-16, published 2026-08-17 | Source-unedited final-still preview for the locked seed corpus. |
| [`v0.4.0`](https://github.com/Dicklesworthstone/franken_manim/releases/tag/v0.4.0) prerelease | 2026-08-18 | Native Studio/Metal and threaded-browser preview; installer and WASM package foundations. |
| Current unreleased line | 2026-08-19 onward | Portal convergence, Studio/runtime hardening, distribution gates, executable truthfulness, and fail-closed agent operations. |

## Major implementation phases

1. **Substrate and semantics:** deterministic constants, color, rates, RNG, canonicalization, rational frame time, QuadPath geometry, Marionette state, Choreo timelines, native text, and math.
2. **Rendering and media:** analytic fills/strokes, retained rendering, camera/depth/lighting/textures, native codecs, ffmpeg boundary, FMTL, and WASM foundations.
3. **Front doors and Gauntlet:** Rust API, `fmn`, optional Python portal, Studio supervisor/worker, API schema and parity ledger, self-goldens, differential, determinism, fuzzing, performance, and packaging gates.
4. **Portal convergence and truthfulness:** authored `manimlib` compatibility, precise capability refusals, wheel/bridge tests, and namespace/refusal ratchets.
5. **Agent operations:** strict graph parsing, leaf planning, stale-plan tokens, atomic Beads compare-and-set, shared locking, structured response proof, exact exported deltas, and bounded subprocess resources.

## Current evidence boundaries

The following are **not** inferred from a focused source probe:

- the complete local repository gate;
- cross-platform release artifacts;
- real aarch64 topology evidence;
- platform-native SIMD and certified bit-identity matrices;
- real-browser npm/WASM publication;
- clean-wheel behavior on every supported Python/platform pair;
- ffmpeg/video-container equivalence receipts;
- closure of `fm-5wq.4` or the W10 epic.

Hosted GitHub Actions availability is not a correctness dependency. The authoritative gate is `scripts/check.sh` on an exact local or owned-host checkout, with the source commit and opt-in release axes recorded alongside the result.

## Agent notes

- Start with `docs/IMPLEMENTATION_STATUS.md` for present-tense evidence.
- Issue a fresh guard token, complete external coordination, and use `scripts/agent_claim.py` for the atomic claim.
- Treat `agent_brief.py` as broad situational context only.
- Treat Beads as authoritative; commit the executor's resulting `.beads/` export explicitly.
- On exit `5`, inspect Beads and the working tree before any retry; never replay the old token.
- Never replace `.beads/issues.jsonl` from truncated connector output.
- Never turn “reviewed,” “compiled,” “inventoried,” or “historically green” into a stronger implementation or release claim.
