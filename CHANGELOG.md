# Changelog

This is the evidence-bounded project changelog for **franken_manim**, a sovereign deterministic rewrite of 3Blue1Brown's `manim` in pure Rust with a separately installed `manimlib`-compatible Python portal.

The repository remains **pre-1.0**. Tagged releases `v0.1.0` through `v0.4.0` are prereleases. Source, compatibility rulings, task state, and release evidence remain distinct:

- source code says what behavior exists;
- `API_SCHEMA.tsv` plus `API_OVERLAY.tsv` say what compatibility status is claimed;
- `.beads/issues.jsonl` says what work is open or blocked;
- gates and retained artifacts say what was actually exercised.

The current unreleased substantive source-and-test checkpoint covered here is **[`0d56b27`](https://github.com/Dicklesworthstone/franken_manim/commit/0d56b27ec734fd6e60305faadbe28a69d7808a19)** on 2026-09-01 UTC. Later documentation commits truth up that implementation state.

---

## Unreleased — bounded-output, exact-delta guarded claims

### Added

- One shared 16 MiB total-produced-byte budget for each claim-executor child command, across stdout and stderr together.
  - Every produced chunk is counted before bytes beyond the retained diagnostic ceilings are discarded.
  - Exactly the ceiling is accepted.
  - The first byte beyond it triggers bounded process-tree cleanup and exit `5` rather than allowing a noisy process to run until its wall-clock timeout.
- Exact post-sync claim-delta verification over the canonical parsed Beads graph.
  - Issue membership must be unchanged.
  - Every non-target issue must remain unchanged.
  - The selected target may change only from unassigned `open` to the requested assignee and `in_progress`, with a non-regressing `updated_at`.
  - When requested, exactly one transition comment with matching text may be appended; otherwise comments must remain unchanged.
  - Target title, priority, type, dependencies, and identity must not drift.
- A normalized `claim_delta` record in successful mutating receipts.
- Focused regressions for exact and excessive combined output, a continuously spewing child, unrelated issue drift, issue-membership drift, target-field drift, exact transition-comment append, and injected-runner combined-output bounds.

### Changed

- Claim receipt schema `fmn.agent.claim` advanced to version `4`.
- `executor_policy` now records three independent controls:
  - wall-clock timeout per child command;
  - retained diagnostic bytes per stream;
  - total produced bytes across both streams per child command.
- The production runner polls both its deadline and the shared output-budget event while child output is drained concurrently.
- A mutating success receipt now proves the entire canonical parsed graph changed only through the intended claim transition, rather than checking only the selected row's final status and assignee.

### Fixed

- A child can no longer produce unlimited discarded output until timeout after overflowing its retained diagnostic buffer.
- Concurrent parsed-graph changes from another clone, a direct manual `br`, or another actor outside the local advisory lock can no longer receive a successful claim receipt merely because the selected issue itself looks claimed.
- A transition comment cannot silently disappear, multiply, or be replaced by different text while the executor reports success.
- A target claim cannot silently alter priority, type, title, or dependency topology.

### Representative commits

- [`06bab2a`](https://github.com/Dicklesworthstone/franken_manim/commit/06bab2acbae2e92d649dc6314432193171e28cb3) — cap total produced claim-subprocess output.
- [`15e7d25`](https://github.com/Dicklesworthstone/franken_manim/commit/15e7d25315d968e622fc65ca2cd58d219a88b486) — prove exact, excessive, combined, and continuously spewing output behavior.
- [`2350609`](https://github.com/Dicklesworthstone/franken_manim/commit/2350609ba27a9ccd4f34988e0101330ac32e62cf) — expose the total-output policy in versioned receipts.
- [`240bcc9`](https://github.com/Dicklesworthstone/franken_manim/commit/240bcc9bcea7c32100fa88b1f966aa40609f6c53) — require an exact claim-only post-sync graph delta.
- [`0d56b27`](https://github.com/Dicklesworthstone/franken_manim/commit/0d56b27ec734fd6e60305faadbe28a69d7808a19) — lock concurrent-drift and transition-comment semantics.

### Evidence boundary

The exact replacement Python source and test bytes passed bytecode compilation. The real-child-process suite passed 13/13 locally, including exact total-output acceptance, combined overflow, prompt termination of a continuously spewing child, retained-output overflow, timeout, POSIX descendant cancellation, and inherited-pipe cleanup. Targeted claim-delta probes passed for the permitted transition and all newly rejected drift classes.

This editing environment did not contain an exact complete repository checkout with the full Rust workspace and installed toolchain. The complete `scripts/test_agent_claim.py`, repository-wide `scripts/check.sh`, Cargo, Clippy, rustdoc, WASM, wheel, browser, and release axes are therefore not claimed as run at `0d56b27`.

No live Beads mutation was performed from this environment because tracker-native `br` execution was unavailable. `.beads/issues.jsonl` was deliberately left untouched rather than reconstructed or hand-edited through truncated connector output.

---

## Unreleased — bounded claim-command lifetime

### Added and changed

- Every production `br update` and `br sync --flush-only` invocation has a finite independent wall-clock timeout.
  - default: 60 seconds;
  - accepted range: finite, positive, at most 3,600 seconds;
  - CLI: `--command-timeout-seconds`.
- POSIX children start in a new session and timed-out process groups are killed; Windows uses a new process group, native `taskkill.exe /T /F` when available, and direct-child fallback.
- Process termination, child wait, and output-reader joins use finite cleanup bounds.
- A direct child that exits while a descendant retains inherited stdout or stderr can no longer strand the reader threads and repository claim lock.
- Receipt policy records the selected timeout and output ceilings.

### Representative commits

- [`47cdf1f`](https://github.com/Dicklesworthstone/franken_manim/commit/47cdf1fa5af31ee3c4427b0b255435a8422c0246) — bound command lifetime and process-tree cleanup.
- [`da07b21`](https://github.com/Dicklesworthstone/franken_manim/commit/da07b210ba096d98ded4548d0960d1ef7eb5b606) — add real-process timeout and descendant regressions.
- [`d3b79e6`](https://github.com/Dicklesworthstone/franken_manim/commit/d3b79e6a15ca8f10ab846e92e245cf35b511336f) — version bounded executor-policy receipts.
- [`301684d`](https://github.com/Dicklesworthstone/franken_manim/commit/301684d670eadced9c948a3859438f9aa8d755a1) — lock invalid-timeout CLI semantics.

---

## Unreleased — guarded claim execution and strict ledger grammar

### Added and changed

- `scripts/agent_claim.py` keeps token revalidation, `br update`, `br sync --flush-only`, and parsed-ledger postconditions inside one process and one advisory-lock scope.
- The claim lock lives in Git's shared `commondir`, so a primary checkout and linked worktrees contend on one persistent inode.
- `.git` and `commondir` markers use bounded no-follow regular-file reads where supported.
- stdout and stderr are concurrently drained under eager retained-memory ceilings.
- `--dry-run` exercises guard, lock, and exact argv composition without mutating Beads.
- The strict parser rejects duplicate keys and issue IDs, malformed framing and optional arrays, invalid statuses/timestamps, self/duplicate dependencies, and unquoted `NaN`, `Infinity`, or `-Infinity` at any depth.
- The canonical claim-graph grammar is version `2`.

### Representative commits

- [`23dec4b`](https://github.com/Dicklesworthstone/franken_manim/commit/23dec4baa9a677703f31106f9fc0542733a68bf7) — add the guarded Beads claim executor.
- [`2e7a80b`](https://github.com/Dicklesworthstone/franken_manim/commit/2e7a80ba2020276213a8aad524624e7fe468e207) — lock executor success and failure semantics.
- [`6169519`](https://github.com/Dicklesworthstone/franken_manim/commit/6169519af2bf586f19effeda10f4c2e71b39ebaa) — exercise guarded dry-run in the mandatory gate.
- [`4546c8f`](https://github.com/Dicklesworthstone/franken_manim/commit/4546c8f1631a6c67bc9d03a892972608bad1a0e1) — reject non-finite Beads JSON constants.
- [`bc09262`](https://github.com/Dicklesworthstone/franken_manim/commit/bc09262de628b3ea7fbebd1a9f8793fa7517f37c) — version the strict claim-graph grammar.
- [`6f1584d`](https://github.com/Dicklesworthstone/franken_manim/commit/6f1584dbc903d2920a9133e32d500daade279c36) — serialize claims across linked worktrees.
- [`95187b9`](https://github.com/Dicklesworthstone/franken_manim/commit/95187b950fdb078ffaea71d48008b5defeb678ca) — prove shared worktree locking.
- [`9ea1045`](https://github.com/Dicklesworthstone/franken_manim/commit/9ea1045aef8b20c037c84ed54a00822c1c528ee9) — retain child output under an eager bound while draining both pipes.
- [`f96c3eb`](https://github.com/Dicklesworthstone/franken_manim/commit/f96c3eb12bdbafa7822e989d1a39702df28215a2) — gate real-child-process output coverage.

---

## Unreleased — graph-and-policy-bound claim revalidation

- `scripts/agent_claim_guard.py` issues tokens of the form `v2:<claim-sha256>:<issue-id-or-none>`.
- The claim digest binds the canonical graph, complete `fmn.agent.next` plan, normalized policy, and parser/planner/guard schemas.
- Machine output separates graph-only `graph_sha256` from complete-claim `claim_sha256`.
- The literal issue ID `none` is reserved for the valid no-recommendation token subject.
- Adversarial tests cover graph, comments, policy, schemas, planner-output drift, canonical ordering, malformed tokens, output bounds, and integrity precedence.

Representative commits: [`6a6b3c8`](https://github.com/Dicklesworthstone/franken_manim/commit/6a6b3c8fe52b34095958d3fe0e68ef55c62b6536), [`93877d4`](https://github.com/Dicklesworthstone/franken_manim/commit/93877d486254a61a1eee85c2c2c7dda22bd45f8f), [`5629a41`](https://github.com/Dicklesworthstone/franken_manim/commit/5629a41349d1fb0f4574ab8b8473348becd6a312), and [`d7a1716`](https://github.com/Dicklesworthstone/franken_manim/commit/d7a17163399c7347c39869bf09aa8744fceda5e4).

---

## Unreleased — strict deterministic task control plane

- Strict JSONL ingestion rejects malformed framing, duplicate keys/IDs, invalid statuses and timestamps, malformed arrays/comments/dependencies, non-finite constants, self/duplicate edges, non-regular files, and bounded-resource violations.
- Blocking and containment cycles fail closed before plan, brief, token, or claim publication.
- Planner output is ledger-time-derived, canonical, bounded, and independent of wall-clock execution.
- Generated brief publication is descriptor-bound, exclusive-temporary, fsynced, atomic, and recoverable after replacement failure.
- Broad situational context and autonomous leaf selection are separate; the unsafe legacy `agent_brief.py --format next` output has been retired.

Representative commits include [`9839a38`](https://github.com/Dicklesworthstone/franken_manim/commit/9839a3862dfca41083ad8f0a5b680edfd3cd17d9), [`9bf1d20`](https://github.com/Dicklesworthstone/franken_manim/commit/9bf1d20f94f3bd8d3cf252badfe57bb215236f2f), [`dcad037`](https://github.com/Dicklesworthstone/franken_manim/commit/dcad037a2bf52e010efa97b0cd1bb838e3f63cf4), [`f6c23f6`](https://github.com/Dicklesworthstone/franken_manim/commit/f6c23f65fa3f16d524589f1f4dba4b7ec654dcd0), and [`204eabf`](https://github.com/Dicklesworthstone/franken_manim/commit/204eabfdada854ad55d14de9c466fda652f1bd2f).

---

## Unreleased — executable boundaries and portal truthfulness

- The shipped `fmn` executable publishes stdout and stderr independently and preserves typed command exits across closed pipes.
- Batch robot mode emits explicit per-job success records and has a real one-scene artifact/manifest smoke.
- Rust-only geometry helpers cannot leak into the pinned Python wildcard namespace; Reference CamelCase constructors remain required.
- Explicit Python portal refusals are parsed into a canonical bounded inventory and anonymous placeholders fail the mandatory gate.
- `fm-5wq.4` remains open: a fully reviewed parity ledger and an inventoried refusal surface are not universal callable implementation.

Representative commits include [`b5518dd`](https://github.com/Dicklesworthstone/franken_manim/commit/b5518dd41fb21ffcd3d53e64c66779ace46e4d2d), [`d1cdc02`](https://github.com/Dicklesworthstone/franken_manim/commit/d1cdc0202b4810d86bb9800dc879bce1a9befae6), [`82d915e`](https://github.com/Dicklesworthstone/franken_manim/commit/82d915ee34285ea7ab2198e76e831ee23bf88250), and [`0fc2fc7`](https://github.com/Dicklesworthstone/franken_manim/commit/0fc2fc7fd4ec5f7a21820faf471228797b72670e).

---

## Release timeline

| Version | Date | Evidence-bounded summary |
|---|---:|---|
| Project inception | 2026-07-20 | Rev-4 comprehensive plan, workspace governance, Beads graph, and architecture doctrine. |
| [`v0.1.0`](https://github.com/Dicklesworthstone/franken_manim/releases/tag/v0.1.0) prerelease | 2026-08-15 | Early native preview: standalone `fmn` CLI, scene/runtime foundations, native renderer and output stack; no Python wheel in this cut. |
| [`v0.2.0`](https://github.com/Dicklesworthstone/franken_manim/releases/tag/v0.2.0) prerelease | 2026-08-16 | Production Python-render preview: portal scenes produce real retained-renderer PNG sequences. |
| [`v0.3.0`](https://github.com/Dicklesworthstone/franken_manim/releases/tag/v0.3.0) prerelease | 2026-08-16, published 2026-08-17 | Source-unedited final-still preview for the locked seed corpus. |
| [`v0.4.0`](https://github.com/Dicklesworthstone/franken_manim/releases/tag/v0.4.0) prerelease | 2026-08-18 | Native Studio/Metal and threaded-browser preview; checksummed native installer and WASM package foundations. |
| Current unreleased line | 2026-08-19 onward | Portal convergence, Studio/runtime hardening, distribution gates, executable smoke, namespace truthfulness, and fail-closed agent operations. |

## Major implementation phases

1. **Substrate and semantics:** deterministic constants, color, rates, RNG, numeric canonicalization, rational frame time, QuadPath geometry, Marionette state, Choreo timelines, native text and math.
2. **Rendering and media:** analytic fills/strokes, retained tile rendering, camera/depth/lighting/textures, native codecs, ffmpeg capability boundary, FMTL, and WASM foundations.
3. **Front doors and Gauntlet:** Rust API, `fmn`, optional CPython portal, Studio supervisor/worker, API schema/Parity Ledger, self-goldens, differential, determinism, fuzzing, performance, and packaging gates.
4. **Portal convergence and truthfulness:** broad authored `manimlib` compatibility, precise capability refusals, executable wheel/bridge tests, and refusal/namespace ratchets.
5. **Agent operations:** strict graph parsing, leaf-aware planning, stale-plan tokens, shared-lock mutation, exact post-sync deltas, and bounded subprocess resources.

## Current evidence boundaries

The following are **not** inferred from a focused green source probe:

- the complete local repository gate;
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
- Use `scripts/agent_claim_guard.py --require`, complete external coordination checks, then use `scripts/agent_claim.py` with an explicit command timeout for the guarded mutation.
- Treat `agent_brief.py` as broad situational context only; it has no autonomous claim output.
- Treat Beads as authoritative. The executor performs the claim update and `br sync --flush-only`; the resulting `.beads/` export still needs an explicit commit.
- On executor exit `5`, inspect Beads and the working tree before doing anything else; never replay the old token blindly.
- Never replace `.beads/issues.jsonl` from truncated connector output.
- Do not turn “reviewed,” “compiled,” “inventoried,” or “historically green” into a stronger implementation or release claim.
