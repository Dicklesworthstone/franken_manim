# Changelog

This is the evidence-bounded project changelog for **franken_manim**, a sovereign deterministic rewrite of 3Blue1Brown's `manim` in pure Rust with a separately installed `manimlib`-compatible Python portal.

The repository remains **pre-1.0**. Tagged releases `v0.1.0` through `v0.4.0` are prereleases. Source, compatibility rulings, task state, and release evidence remain distinct:

- source code says what behavior exists;
- `API_SCHEMA.tsv` plus `API_OVERLAY.tsv` say what compatibility status is claimed;
- `.beads/issues.jsonl` says what work is open or blocked;
- gates and retained artifacts say what was actually exercised.

The current unreleased substantive source-and-test checkpoint covered here is **[`301684d`](https://github.com/Dicklesworthstone/franken_manim/commit/301684d670eadced9c948a3859438f9aa8d755a1)** on 2026-09-01 UTC. Later documentation commits truth up that implementation state.

---

## Unreleased — bounded claim-command lifetime

### Added

- A finite wall-clock contract for each production `br update` and `br sync --flush-only` invocation in `scripts/agent_claim.py`.
  - default timeout: 60 seconds;
  - accepted caller range: finite, positive, and no more than 3,600 seconds;
  - CLI option: `--command-timeout-seconds`.
- Process-tree cleanup for timed-out claim commands:
  - POSIX children start in a new session and the complete process group is killed;
  - Windows children start in a new process group, use no-shell `taskkill.exe /T /F` when available, and retain direct-child kill fallback;
  - all waits and output-reader joins use finite cleanup bounds.
- Receipt schema `fmn.agent.claim` version `2`, including an `executor_policy` record for the requested command timeout and retained per-stream output ceiling.
- Focused real-process regressions for:
  - a stalled direct child;
  - descendant termination on POSIX;
  - a direct child that exits while a descendant retains its output pipes;
  - zero, negative, non-finite, and excessive timeout values;
  - the pre-existing exact-limit, overflow, dual-stream, and nonzero-exit cases.
- A CLI regression proving an invalid timeout exits `2`, emits no stdout receipt, and never invokes `br`.

### Changed

- `run_command` now starts a bounded process group/session and uses `Popen.wait(timeout=...)` rather than waiting indefinitely.
- Reader threads are daemonized and joined under a finite bound. If inherited output descriptors remain open after the direct child exits, forced cleanup occurs and the command fails instead of holding the repository claim lock.
- The timeout is per child command rather than one transaction-wide deadline. Bounded termination and reader cleanup may extend the return path slightly beyond the selected deadline.
- stdout and stderr remain concurrently drained under the eager 1 MiB retained-byte ceiling. The timeout bounds lifetime; a separate total-produced-byte ceiling is still future work.

### Fixed

- A stalled `br` process can no longer hold `fmn-agent-claim.lock` indefinitely.
- A timed-out descendant tree can no longer continue under the executor's POSIX session after the parent is killed.
- A background descendant that inherits stdout or stderr can no longer strand the reader threads and lock after its direct parent exits.
- Invalid `NaN`, infinite, nonpositive, or excessive timeout values can no longer reach process execution.
- Timeout and cleanup failures emit exit `5` and no success receipt. As with any post-`br update` failure, this does not prove no tracker mutation occurred; recovery begins by inspecting Beads, not replaying the stale token.

### Representative commits

- [`47cdf1f`](https://github.com/Dicklesworthstone/franken_manim/commit/47cdf1fa5af31ee3c4427b0b255435a8422c0246) — bound command lifetime, process-group cleanup, and receipt policy.
- [`da07b21`](https://github.com/Dicklesworthstone/franken_manim/commit/da07b210ba096d98ded4548d0960d1ef7eb5b606) — add real-process timeout and descendant regressions.
- [`d3b79e6`](https://github.com/Dicklesworthstone/franken_manim/commit/d3b79e6a15ca8f10ab846e92e245cf35b511336f) — lock receipt schema version 2 and timeout policy evidence.
- [`301684d`](https://github.com/Dicklesworthstone/franken_manim/commit/301684d670eadced9c948a3859438f9aa8d755a1) — lock timeout CLI failure semantics.

### Evidence boundary

The exact replacement executor and real-process regression bytes were Python-bytecode compiled before publication. The nine-case real-process suite passed locally in about five seconds, including the POSIX descendant marker and inherited-pipe probes. A separate dry-run receipt probe passed with schema version `2` and an explicit timeout value. Blob identities were checked against the committed files.

The editing environment did not contain an exact complete checkout with the full parser/planner modules, Rust workspace, and installed repository toolchain. Therefore the complete `scripts/test_agent_claim.py`, repository-wide `scripts/check.sh`, Cargo, Clippy, rustdoc, WASM, wheel, browser, and release axes are not claimed as run at `301684d`.

No live Beads mutation was performed from this environment because tracker-native `br` execution was unavailable. `.beads/issues.jsonl` was deliberately left untouched rather than reconstructed or hand-edited through truncated connector output.

---

## Unreleased — guarded claim execution and strict ledger grammar

### Added

- `scripts/agent_claim.py`, the mutation companion to the version-2 claim guard.
  - It acquires a persistent advisory lock in Git's shared common directory.
  - It re-reads the ledger and rebuilds the complete graph/policy/schema guard while holding that lock.
  - It invokes `br update` and `br sync --flush-only` through exact argv without a shell.
  - It re-reads the exported JSONL and requires the selected row to be `in_progress` with the requested assignee before emitting a success receipt.
  - `--dry-run` performs the guarded read and publishes the exact intended argv without invoking `br`.
- Canonical claim receipts record the issue, assignee, guard token, pre-claim graph and claim digests, planner policy, schema contracts, recommendation evidence, exact command vectors, executor policy, and post-claim graph/status evidence.
- `docs/AGENT_CLAIM_EXECUTOR.md` and `docs/AGENT_CONTROL_PLANE.md` specify the transaction, receipt, exit codes, partial-failure semantics, concurrency boundary, strict parser, and process policy.
- `scripts/test_agent_claim.py`, `scripts/test_agent_claim_subprocess.py`, and `scripts/test_agent_brief_strict_json.py` provide focused executor, process, and strict-JSON coverage.

### Changed

- The claim lock is stored in Git's shared `commondir`, so a primary checkout and its linked worktrees contend on one persistent inode.
- `.git` and `commondir` markers are read through bounded regular-file descriptors with no-follow opening where supported.
- Claim stdout and stderr are drained concurrently; each stream retains at most `MAX_COMMAND_OUTPUT_BYTES + 1` bytes and discards subsequent bytes while continuing to drain.
- The canonical claim-graph grammar is version `2`.
- `scripts/check.sh` runs the executor and strict-JSON suites, revalidates a live-ledger token, and exercises `agent_claim.py --dry-run` without mutating Beads.

### Fixed

- The manual interval between token revalidation and `br update` is no longer the canonical workflow.
- A successful `br` exit is not reported as a verified claim unless JSONL export and parsed-ledger postconditions also succeed.
- Linked worktrees can no longer acquire independent claim locks for the same clone.
- Child output is bounded while being read rather than only rejected after unbounded capture.
- Unquoted `NaN`, `Infinity`, and `-Infinity` are rejected at the initial Beads JSON boundary, including ignored nested extension fields.

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

## Current evidence boundaries

The following are **not** inferred from focused source tests:

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
- Never replace `.beads/issues.jsonl` from truncated connector output.
- Do not turn “reviewed,” “compiled,” “inventoried,” or “historically green” into a stronger implementation or release claim.
