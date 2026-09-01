# FrankenManim implementation status

**Status date:** 2026-09-01 UTC / America/New_York  
**Evidence checkpoint:** substantive source-and-test commits through `301684d`; documentation reconciled through the current status tranche.  
**Authority rule:** this document summarizes evidence; `.beads/issues.jsonl` remains the task and dependency authority.

## How to read this document

FrankenManim is pre-1.0. A capability is listed as implemented only when a concrete source surface and a checkable test or artifact boundary exist. Aspirational plan text, a reviewed Parity Ledger row, an inventoried refusal, or an old Beads comment is not by itself implementation evidence. Hardware- and release-specific claims remain separate from local semantic correctness.

## Product surfaces

| Surface | Current implementation state | Evidence boundary |
|---|---|---|
| Native Rust library | The composition root, scene runtime, mobject/animation stack, native text and math, retained renderer, codecs, output pipeline, and built-in scene corpus are implemented as workspace crates. | Ordinary workspace checks and tests run through `scripts/check.sh`; platform certification remains a separate matrix. |
| `fmn` CLI | Typed render, doctor, batch, and Studio command surfaces exist. The shipped binary has executable smoke coverage for version/help, doctor robot and quiet behavior, typed exits, a real one-scene batch render, native artifact publication, and machine/human manifests. | `cargo test -p fmn-cli --features batch --test cli_smoke` exercises the complete shipping feature shape. |
| Studio | Supervisor/worker, replay, event, inspection, and presentation foundations are implemented. | Platform and real-browser presentation evidence is not inferred from local Rust tests. |
| `fmn-python` portal | A separately installed CPython wheel exposes the pinned `manimlib` namespace with native and authored compatibility behavior. Every explicit bootstrap refusal is mechanically inventoried and must be named. | The W10 semantic-surface parent remains open. A named refusal is evidence of an honest missing capability, not evidence that the capability is implemented. |
| WASM/browser | The wasm32 render/player foundations and package gate exist. | The npm/real-browser release gate is opt-in (`FMN_WASM_PACKAGE_GATE=1`) and is not claimed merely because ordinary local checks pass. |
| Distribution | Tagged `v0.1.0` through `v0.4.0` releases are prereleases. | Cross-platform artifacts, hardware-specific execution, and release matrices require their own receipts. |

## Recently completed implementation tranche

### Bounded claim-command lifetime

`scripts/agent_claim.py` now gives every production `br update` and `br sync --flush-only` invocation a finite independent wall-clock timeout.

The command contract is:

- default timeout: 60 seconds per child command;
- accepted values: finite and positive;
- maximum: 3,600 seconds;
- CLI control: `--command-timeout-seconds`;
- invalid values fail before repository, guard, lock, or process mutation work begins.

The executor starts each child in an isolated process scope:

- POSIX uses a new session and kills the complete process group with `SIGKILL` after timeout;
- Windows uses a new process group, attempts no-shell `taskkill.exe /T /F`, and retains a direct-child kill fallback;
- process termination, child wait, and output-reader joins all have finite cleanup bounds.

A direct child can exit while one of its descendants still owns inherited stdout or stderr. The executor now detects that condition, forces cleanup, and returns exit `5` rather than holding `fmn-agent-claim.lock` indefinitely.

The timeout is a command deadline, not an exact end-to-end response-time promise. Process-tree termination and bounded reader cleanup can extend a timeout failure slightly. Each `br` invocation gets its own timeout; there is not yet one transaction-wide deadline.

#### Output and timeout composition

stdout and stderr remain concurrently drained. Each reader retains at most `MAX_COMMAND_OUTPUT_BYTES + 1` bytes and discards later bytes while continuing to drain, so the memory bound is eager and a full pipe cannot deadlock the child.

The timeout bounds child lifetime, but there is not yet a separate total-produced-byte ceiling. A child can continue producing discarded bytes until it exits or reaches the wall-clock deadline.

#### Receipt contract

The claim receipt is now schema `fmn.agent.claim` version `2`. Its `executor_policy` records:

- the requested per-command timeout;
- the retained-byte ceiling for each output stream.

The CLI production path enforces this policy. The optional injected runner is an internal focused-test seam and is not a production subprocess contract.

As before, exit `5` means **no verified success receipt**, not necessarily **no tracker mutation**. `br update` may have changed its native store before a timeout, flush failure, or postcondition failure. Recovery starts with `br show`, `br sync --status`, and working-tree inspection; the old token must not be replayed.

#### Focused coverage

The real-process suite now covers:

- exact-limit stdout and stderr retention;
- independent stdout and stderr overflow;
- simultaneous large-stream draining without deadlock;
- bounded diagnostics from a real nonzero exit;
- a stalled direct child;
- POSIX descendant termination, verified by a delayed marker that must never be created;
- a parent that exits while a descendant retains inherited pipes;
- zero, negative, non-finite, and excessive timeout values.

A separate CLI test proves that `--command-timeout-seconds nan` exits `2`, emits no stdout receipt, reports the finite-value requirement, and never invokes `br`.

### Guarded Beads claim execution

The timeout layer extends the existing guarded transaction rather than replacing it. `scripts/agent_claim.py` still keeps these operations under one shared-common-directory advisory lock:

1. read `.beads/issues.jsonl` through the strict parser;
2. rebuild the complete v2 graph/plan/policy/schema guard;
3. compare the supplied token and selected issue;
4. require the row to remain unassigned and `open`;
5. run exact no-shell `br update` argv;
6. run `br sync --flush-only`;
7. re-read the exported JSONL;
8. require `in_progress` plus the requested assignee;
9. emit a canonical receipt only after every prior step succeeds.

`--dry-run` performs the guarded read and emits the exact intended command vectors without invoking `br`. `scripts/check.sh` uses this path against a freshly issued live-ledger token so verification exercises the composition without mutating tracker state.

The lock is stored in Git's shared `commondir`. A primary checkout and sibling linked worktrees therefore contend on one persistent inode. This is a local coordination mechanism, not a distributed lease: it does not serialize another clone, a direct manual `br`, Agent Mail, file reservations, or an agent that ignores the executor.

### Literal strict-JSON ledger boundary

The Beads parser rejects unquoted `NaN`, `Infinity`, and `-Infinity` at the initial decode boundary wherever they occur, including ignored extension fields. Duplicate keys, invalid UTF-8/framing, malformed arrays/comments/dependencies, unknown statuses, invalid timestamps, self/duplicate edges, non-regular files, and bounded-resource violations also fail closed.

The canonical claim-graph grammar is version `2`; the version participates in the v2 claim digest, so a token from the former permissive grammar cannot survive the change.

### Graph-and-policy-bound recommendation

`agent_next.py` decides the leaf. `agent_claim_guard.py` binds the complete graph, complete planner output, normalized policy, and parser/planner/guard schema contracts into a token:

```text
v2:<claim-sha256>:<issue-id-or-none>
```

The planner excludes assigned work, epic and topology-derived containers, blocking/containment cycles, and activation-cap violations. It prefers already-active workstreams, then numeric priority, immediate-unblock pressure, direct blocker pressure, scope, recency, and lexical ID.

The broad `agent_brief.py` projection is situational only. Its unsafe legacy `--format next` spelling has been retired.

### Python portal refusal truthfulness

`scripts/audit_portal_refusals.py` parses the portal source into a canonical bounded inventory. It uses descriptor-bound no-follow reads, iterative AST traversal, source/AST/depth/site/output ceilings, exact abstract-method scoping, and fail-closed checks for anonymous `NotImplementedError` and malformed `_refuse_unrouted` calls.

This is not a completion metric. Each refusal remains a product gap until the owning native behavior lands or an evidence-backed tier/exclusion ruling is recorded.

### Executable CLI boundaries

The shipped `fmn` process publishes stdout and stderr independently, treats closed pipes as normal lifecycle, and preserves typed command exits. Batch robot output includes explicit terminal per-job success records, and a shipped-binary smoke renders a tiny FMTL scene and verifies native PNG plus machine/human manifests.

The Python/Rust namespace boundary mechanically prevents Rust-only snake_case helpers from leaking into the pinned Reference wildcard surface while requiring the corresponding Reference CamelCase constructors.

## Agent control-plane architecture

1. `scripts/agent_brief.py`: strict bounded parser and broad situational projection.
2. `scripts/agent_next.py`: leaf-safe planner, schema `fmn.agent.next` version `2`.
3. `scripts/agent_claim_guard.py`: read-only graph/policy/schema token, schema `fmn.agent.claim-guard` version `2`.
4. `scripts/agent_claim.py`: shared-lock claim mutation, bounded command lifetime/output, explicit flush, postcondition, and schema-versioned receipt.
5. `scripts/generate_agent_brief.py`: deterministic human projection and atomic publication.
6. Beads: sole task/dependency authority.

`scripts/check.sh` compiles and runs the strict parser, strict JSON, planner, guard, executor, real-process output/lifetime, generator, publication-I/O, portal-refusal, and helper-alias suites before entering the Rust, Python, WASM, and structural gates.

## Current open obligations

### W10 semantic surface

`fm-5wq.4` remains in progress. Its newest reality-check supersedes older “100% reviewed” commentary. Therefore:

- reviewed ledger coverage is not universal callable implementation;
- refusal inventory coverage is not implementation coverage;
- SAME/IMPROVED behavior requires real callable semantics and focused evidence;
- representative clean-wheel semantics remain required across API families;
- each refusal falls only through implementation or an evidence-backed tier/exclusion;
- no W10 or G4 closure is claimed here.

### CLI library/binary quiet-policy convergence

The shipped binary suppresses successful human doctor output under `--quiet` while retaining typed failures. That policy is still not centralized for every embedded `run_with_capabilities` caller.

### Distributed claim coordination

The executor serializes cooperating processes only inside one clone's shared Git common directory. Another clone, a manual `br`, or an agent that ignores the executor can still race it. Agent Mail, reservations, current-HEAD review, and explicit peer coordination remain mandatory.

### Claim-output production ceiling

Retained memory and wall-clock lifetime are bounded, but total bytes produced and discarded before timeout are not independently capped. Adding an early total-production kill threshold is a valid future hardening item, provided partial tracker-state recovery remains explicit.

### Platform and release evidence

The following remain independent evidence lanes:

- real aarch64 topology fixtures;
- platform-native execution matrices;
- SIMD-tier and certified cross-platform bit evidence;
- npm/WASM real-browser packaging;
- clean-wheel gates on every supported platform;
- ffmpeg/video-container equivalence receipts.

### Tracker synchronization

The executor performs only the guarded claim transition through `br`, followed by `br sync --flush-only` and parsed-ledger verification. Other Beads changes still require tracker-native commands, and every resulting `.beads/` export requires an explicit commit.

No live Beads mutation was performed from the current editing environment because it did not expose tracker-native `br`. The authoritative ledger was deliberately left untouched.

## Verification entry points

```bash
# Mandatory local or owned-host repository gate
scripts/check.sh

# Whole graph, leaf plan, and human context
python3 scripts/agent_brief.py --format json --check
python3 scripts/agent_next.py --format json --check
python3 scripts/generate_agent_brief.py --stdout

# Guarded claim
token="$(python3 scripts/agent_claim_guard.py --require)"
issue="${token##*:}"
br show "$issue"
# Check Agent Mail, reservations, peers, and current main.
python3 scripts/agent_claim.py \
    --expect-token "$token" \
    --issue "$issue" \
    --assignee "$FMN_AGENT_ID" \
    --command-timeout-seconds 60 \
    --dry-run
python3 scripts/agent_claim.py \
    --expect-token "$token" \
    --issue "$issue" \
    --assignee "$FMN_AGENT_ID" \
    --command-timeout-seconds 60 \
    --transition-comment "Claimed after graph, reservation, and HEAD checks"

# Focused control-plane regressions
python3 scripts/test_agent_brief.py
python3 scripts/test_agent_brief_strict_json.py
python3 scripts/test_agent_next.py
python3 scripts/test_agent_next_output.py
python3 scripts/test_agent_claim_guard.py
python3 scripts/test_agent_claim.py
python3 scripts/test_agent_claim_subprocess.py
python3 scripts/test_generate_agent_brief.py
python3 scripts/test_generate_agent_brief_io.py
python3 scripts/test_audit_portal_refusals.py
```

## Evidence from the current editing environment

The timeout implementation and real-process test bytes were Python-bytecode compiled before publication. The nine-case real-process suite passed locally in about five seconds. The suite included direct timeout, POSIX descendant, and inherited-pipe probes. A separate mocked dry-run receipt probe passed with receipt version `2` and timeout `17.5`. Local Git-blob calculations matched the content SHAs returned for the committed executor and tests.

The CLI invalid-timeout regression and documentation were committed incrementally after that focused run. This environment did not contain an exact complete checkout with the full parser/planner modules, portal source, Rust workspace, and installed toolchain. Therefore the complete Python suite, live-ledger dry-run gate, portal audit, Cargo/Clippy/rustdoc/WASM/wheel/browser axes, and repository-wide `scripts/check.sh` are not represented as run against `301684d`.

GitHub Actions is not a correctness dependency. Hosted runs were not used as acceptance evidence.

## Truthfulness rules for future updates

- Record exact commands and the source commit they exercised.
- Do not convert “compiled,” “reviewed,” “inventoried,” or “imported” into “fully implemented.”
- Do not infer hardware or artifact evidence from a different platform.
- Do not close a parent merely because one child or one census reaches 100%.
- Distinguish broad readiness from true leaf claimability.
- Issue a v2 token, complete external coordination, and use `agent_claim.py` with an explicit bounded timeout for autonomous claim mutation.
- Keep target-state design in the comprehensive plan and current-state evidence here.
