# FrankenManim implementation status

**Status date:** 2026-09-02 UTC / America/New_York  
**Substantive checkpoint:** `fffc469e99ac6e9b79e66ddd890fc98238e3f3c9`  
**Authority rule:** this document summarizes evidence; `.beads/issues.jsonl` remains the task and dependency authority.

## How to read this document

FrankenManim is pre-1.0. A capability is listed as implemented only when a concrete source surface and a checkable test or artifact boundary exist. Plan text, a reviewed Parity Ledger row, an inventoried refusal, or an old Beads comment is not implementation evidence. Hardware, platform, wheel, browser, and release claims remain separate from source-level correctness.

## Product surfaces

| Surface | Current implementation state | Evidence boundary |
|---|---|---|
| Native Rust library | The composition root, scene runtime, mobject/animation stack, native text and math, retained renderer, codecs, output pipeline, and built-in scene corpus exist as workspace crates. | Ordinary workspace checks run through `scripts/check.sh`; platform certification remains a separate matrix. |
| `fmn` CLI | Typed render, doctor, batch, and Studio surfaces exist. Human `doctor --quiet` semantics are owned by the shared dispatcher. Batch robot output includes terminal per-job records, and executable smoke covers a real tiny render plus native artifact and manifest publication. | The complete shipping feature shape is exercised by `cargo test -p fmn-cli --features batch --test cli_smoke` when the exact local gate runs. |
| Studio | Supervisor/worker, replay, event, inspection, and presentation foundations exist. | Platform and real-browser presentation evidence is not inferred from local Rust tests. |
| `fmn-python` portal | A separately installed CPython wheel exposes the pinned `manimlib` namespace with native and authored compatibility behavior. Explicit bootstrap refusals are mechanically inventoried and must be named. | `fm-5wq.4` remains open. A named refusal or reviewed ledger row is not implemented behavior. |
| WASM/browser | wasm32 render/player foundations and a package gate exist. | The npm/real-browser release gate is opt-in and independent of ordinary local source checks. |
| Distribution | Tagged `v0.1.0` through `v0.4.0` are prereleases. | Cross-platform artifacts, hardware-specific execution, clean wheels, and certification require their own receipts. |

## Recently completed tranche: governed autonomous scope

### Planner schema version 4

`scripts/agent_next.py` now owns one exact autonomous-workstream grammar:

```text
G0
W1 through W11
```

A valid prefix begins at the first title character and is followed by a word boundary or `:`. This accepts titles such as `G0: spike`, `W7: drawings`, and `W5/fm-4wt: SIMD`. It rejects `W0`, `W12`, `W999`, lower-case `w10`, and embedded `prefix W10` as `UNSCOPED`.

This is a semantic contract, not display decoration:

- open unscoped leaves remain visible but never enter autonomous ranking;
- when only unscoped leaves remain, `--require` exits `3` with no stdout;
- a governed leaf wins even when an unscoped leaf has a lower numeric priority;
- any active unscoped issue makes planner integrity invalid and causes exit `1` before token or mutation;
- `G0` counts toward the four-workstream activation cap;
- `G0` plus four W-streams is a five-stream breach.

Recommendation order remains:

1. governed leaves in an already-active governed workstream;
2. lower numeric priority;
3. greater immediate-unblock pressure;
4. greater direct-blocker pressure;
5. most recently updated;
6. lexical issue ID.

### Token and executor propagation

`agent_claim_guard.py` remains schema `fmn.agent.claim-guard` version `2`, with token spelling:

```text
v2:<claim-sha256>:<issue-id-or-none>
```

Its schema contract dynamically includes `fmn.agent.next` version `4`. Old tokens issued under planner versions 2 or 3 therefore fail revalidation without changing token syntax.

Direct guard regressions cover:

- an unscoped-only graph producing the stable `none` token and exit `3` under `--require`;
- a valid `G0` active lane selecting a `G0` leaf ahead of new-stream work;
- invalid `W12` work remaining unscoped;
- an active unscoped issue suppressing token publication with exit `1`.

`agent_claim.py` consumes that guard, so unscoped work cannot reach its atomic Beads mutation path as a valid recommendation.

### Deterministic human brief

`scripts/generate_agent_brief.py` now consumes the exact planner scope and activation result before publication.

It:

- normalizes broad queue labels to `G0`, `W1`–`W11`, or `UNSCOPED`;
- uses the planner's activation count rather than the broad projection's historical grouping;
- suppresses an unscoped broad priority instead of presenting it as actionable;
- keeps unscoped open leaves visible in the leaf-plan census;
- refuses active unscoped issues before stdout or file replacement;
- refuses `G0` plus four W-streams as a `5/4` cap breach;
- preserves broad non-leaf context while making the planner section the only claim contract.

Focused generator regressions cover G0 rendering, an unscoped broad priority, unscoped active refusal across every publication mode, strict activation-cap refusal, and deterministic output.

## Agent control-plane versions

| Layer | Current contract | Role |
|---|---:|---|
| Broad snapshot | `agent_brief` snapshot version 5 | Bounded parsing, blocking integrity, broad queues, stale/unowned diagnostics. Not a claim authority. |
| Leaf planner | `fmn.agent.next` version 4 | Governed workstream classification, leafhood, containment integrity, activation, and recommendation. |
| Claim guard | `fmn.agent.claim-guard` version 2 | Graph/plan/policy/schema-bound token. |
| Claim graph | `fmn.agent.claim-graph` version 2 | Canonical represented-field graph used in token and delta evidence. |
| Claim executor | `fmn.agent.claim` version 6 | Atomic Beads claim, structured response proof, explicit export, and represented-field delta receipt. |
| Human brief | deterministic Markdown | Planner-normalized human context and atomic publication. |

The direct `agent_brief.py` workstream labels remain broad diagnostic grouping. `agent_next.py` is the only authority for governed scope and activation, and generated Markdown is normalized to that result before publication.

## Atomic guarded claim execution

The autonomous mutation path is:

```text
br update ISSUE --claim --actor ASSIGNEE --json [--transition-comment TEXT]
br sync --flush-only
```

The executor keeps token revalidation, Beads' storage-level unassigned compare-and-set, response validation, export, and postcondition verification under one shared-common-directory advisory lock.

### Atomic response proof

The version-6 executor rejects successful child output unless it is:

- UTF-8 strict JSON;
- free of duplicate keys and non-finite constants;
- no deeper than 64 decoded levels;
- no larger than 100,000 decoded nodes;
- one ordinary updated-row array or one valid `{updated, warnings}` envelope;
- exactly one guarded issue with matching ID, title, priority, status, assignee, and timestamp;
- accompanied by no success-path stderr.

The response timestamp must equal the exported issue timestamp.

### Resource policy

Each `br update --claim` and `br sync --flush-only` child has:

- default 60-second wall-clock deadline, configurable to 3,600 seconds;
- default 16 MiB combined produced-output ceiling, configurable to 1 GiB;
- 1 MiB retained diagnostic ceiling per stream;
- concurrent stdout/stderr draining;
- bounded process-tree termination and reader cleanup.

A stalled, continuously producing, or inherited-pipe child cannot hold the local claim lock indefinitely.

### Failure semantics

Exit `5` means **no verified success receipt**, not necessarily **no native mutation**. Recovery starts with:

```bash
br show "$issue"
br sync --status
git status
```

The old token must never be replayed blindly.

## Canonical planning-graph boundary

The token and post-export claim delta currently bind the fields represented by `agent_brief.Issue`:

- ID;
- title;
- status;
- priority;
- issue type;
- assignee;
- normalized update timestamp;
- dependencies;
- comments.

The executor verifies unchanged represented issue membership, unchanged represented non-target records, and only the expected represented target transition.

The following persisted Beads fields are **not yet bound**: description, design, acceptance criteria, notes, owner, estimate, due/defer values, labels, and unknown extension metadata. A successful version-6 receipt is not raw-record or whole-database identity. `br show` immediately before claiming remains mandatory, and expanding this field set is a high-value future hardening task.

## Python portal truthfulness

`scripts/audit_portal_refusals.py` parses the portal source into a bounded canonical refusal inventory. It uses descriptor-bound no-follow reads, iterative AST traversal, source/AST/depth/site/output ceilings, exact abstract-method scoping, and fail-closed checks for anonymous `NotImplementedError` and malformed `_refuse_unrouted` calls.

This is not a completion metric. Each refusal remains a product gap until the owning behavior lands or an evidence-backed tier/exclusion ruling is recorded.

The Python/Rust namespace policy also prevents Rust-only snake_case helpers from leaking into the pinned Reference wildcard surface while requiring corresponding Reference CamelCase constructors.

## Current open obligations

### W10 semantic surface

`fm-5wq.4` remains in progress. Its newest reality-check supersedes older “100% reviewed” commentary.

- reviewed ledger coverage is not universal callable implementation;
- refusal inventory coverage is not implementation coverage;
- SAME/IMPROVED behavior requires real callable semantics and focused evidence;
- representative clean-wheel semantics remain required across API families;
- each refusal falls only through implementation or an evidence-backed tier/exclusion;
- no W10 or G4 closure is claimed here.

### Expand the canonical claim graph

Task-semantic fields outside `agent_brief.Issue` can currently change without changing `graph_sha256`. The next hardening step is to bind the complete task-semantic record while preserving deterministic canonicalization and harmless row-order independence.

### Unify broad display scope

The autonomous planner and generated human brief use the exact governed vocabulary. The direct broad snapshot retains its historical display classifier and is explicitly non-authoritative. Moving the shared classifier into one common module would reduce conceptual duplication, provided broad compatibility and snapshot versioning are handled deliberately.

### Distributed coordination

The executor lock covers cooperating processes only inside one clone's Git common directory. Another clone, direct manual Beads activity, Agent Mail, reservations, and unrelated `main` movement remain outside the lock.

### Platform and release evidence

The following remain independent evidence lanes:

- real aarch64 topology fixtures;
- platform-native execution matrices;
- SIMD-tier and certified cross-platform bit evidence;
- npm/WASM real-browser packaging;
- clean-wheel gates on every supported platform;
- ffmpeg/video-container equivalence receipts.

## Verification entry points

```bash
# Mandatory local or owned-host repository gate
scripts/check.sh

# Whole graph, governed plan, and human context
python3 scripts/agent_brief.py --format json --check
python3 scripts/agent_next.py --format json --check
python3 scripts/generate_agent_brief.py --stdout

# Guarded atomic claim
token="$(python3 scripts/agent_claim_guard.py --require)"
issue="${token##*:}"
br show "$issue"
# Check current main, Agent Mail, reservations, peers, and task scope.
python3 scripts/agent_claim.py \
    --expect-token "$token" \
    --issue "$issue" \
    --assignee "$FMN_AGENT_ID" \
    --command-timeout-seconds 60 \
    --command-output-budget-bytes 16777216 \
    --dry-run
```

## Evidence for this tranche

The exact `agent_next.py` blob passed Python bytecode compilation and a focused 12-case planner harness using a faithful `agent_brief` test seam. That harness covered non-epic containers, active-stream preference, blocker pressure, G0, all W1–W11 prefixes, invalid W-prefixes, unscoped-only state, active unscoped refusal, containment cycles, a 1,500-node hierarchy, activation limits, canonical JSON, and distinct no-work exits.

The committed planner, guard, and generator suites are already invoked by `scripts/check.sh`. This editing environment did not contain an exact complete repository checkout, Cargo/Rust toolchain, `br`, or UBS. Therefore the full repository gate, live-ledger planner result, Beads mutation, Rust axes, wheel/browser gates, and platform matrices are **not** represented as executed against `fffc469`.

No `.beads/` file was manually reconstructed or replaced. Tracker state was left unchanged because tracker-native `br` execution was unavailable.

GitHub Actions is not a correctness dependency and was not used as acceptance evidence.

## Truthfulness rules for future updates

- Record exact commands and the source commit they exercised.
- Do not convert “compiled,” “reviewed,” “inventoried,” or “imported” into “fully implemented.”
- Do not infer hardware or artifact evidence from another platform.
- Do not close a parent merely because one child or one census reaches 100%.
- Distinguish broad readiness from governed leaf claimability.
- Scope autonomous work to `G0` or `W1`–`W11`, issue a fresh token, complete external coordination, and use the atomic executor.
- Keep target-state design in the comprehensive plan and current-state evidence here.
