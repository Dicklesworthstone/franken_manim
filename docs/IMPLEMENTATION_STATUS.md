# FrankenManim implementation status

**Status date:** 2026-09-02 UTC / America/New_York  
**Substantive source-and-test checkpoint:** `c2f045340943e3a2c8e241535453653a1d835d87`  
**Documentation checkpoint:** this file and the claim-guard/executor contracts were trued up after that source checkpoint.  
**Authority rule:** this document summarizes evidence; `.beads/issues.jsonl` remains the task and dependency authority.

## How to read this document

FrankenManim is pre-1.0. A capability is listed as implemented only when a concrete source surface and a checkable test or artifact boundary exist. Plan text, a reviewed Parity Ledger row, an inventoried refusal, or an old Beads comment is not implementation evidence. Hardware, platform, wheel, browser, and release claims remain separate from source-level correctness.

## Product surfaces

| Surface | Current implementation state | Evidence boundary |
|---|---|---|
| Native Rust library | The composition root, scene runtime, mobject/animation stack, native text and math, retained renderer, codecs, output pipeline, and built-in scene corpus exist as workspace crates. The Menagerie includes boolean, data, drawing, graph, field, image, model, and 3D families, although individual compatibility and release obligations remain open. | Ordinary workspace checks run through `scripts/check.sh`; platform certification remains a separate matrix. |
| `fmn` CLI | Typed render, doctor, batch, and Studio surfaces exist. Human `doctor --quiet` semantics are owned by the shared dispatcher. Batch robot output includes terminal per-job records, and executable smoke covers a real tiny render plus native artifact and manifest publication. | The complete shipping feature shape is exercised by `cargo test -p fmn-cli --features batch --test cli_smoke` when the exact local gate runs. |
| Studio | Supervisor/worker, replay, event, inspection, and presentation foundations exist. | Platform and real-browser presentation evidence is not inferred from local Rust tests. |
| `fmn-python` portal | A separately installed CPython wheel exposes the pinned `manimlib` namespace with native and authored compatibility behavior. Explicit bootstrap refusals are mechanically inventoried and must be named. | `fm-5wq.4` remains open. A named refusal or reviewed ledger row is not implemented behavior. |
| WASM/browser | wasm32 render/player foundations and a package gate exist. | The npm/real-browser release gate is opt-in and independent of ordinary local source checks. |
| Distribution | Tagged `v0.1.0` through `v0.4.0` are prereleases. | Cross-platform artifacts, hardware-specific execution, clean wheels, and certification require their own receipts. |

## Recently completed tranche: full task-semantic claim binding

The autonomous claim guard no longer treats the narrow `agent_brief.Issue` model as the complete task contract.

### Claim-graph version 3

`scripts/agent_task_semantics.py` now reads the exact exported Beads JSONL authority and binds every task-semantic field not represented by the broad planner model.

The claim graph includes:

- the established core planning fields: ID, title, status, priority, issue type, assignee, normalized update time, dependencies, and complete comment objects;
- descriptions, design, acceptance criteria, and notes;
- owners, estimates, creation/source metadata, due/defer values, and labels;
- every unknown future top-level extension field;
- every non-core dependency-record field, including metadata, thread identity, and creation metadata.

The canonicalization deliberately ignores only representation order with no task meaning:

- issue-row order;
- dependency-array order;
- JSON object-key order at every depth;
- label order.

Comment order and ordinary array order remain significant. Duplicate labels remain represented.

### Stable ledger projection

The semantic loader proves that the broad planning projection and the full semantic projection describe one stable ledger state:

1. open the JSONL authority as a no-follow regular file;
2. read under the existing 32 MiB and per-line limits;
3. strictly decode each row and derive both the full semantics and an `agent_brief.Issue` projection from the same bytes;
4. run the established bounded `agent_brief` loader;
5. repeat the full semantic/core read;
6. require matching before/after digests and projections;
7. require the established loader's issue map to equal the core map derived from the stable bytes.

This catches mutation during loading, including a change that would affect only descriptions, labels, estimates, or unknown metadata while leaving broad planner output unchanged.

Unknown nested metadata is bounded before planning by a per-record depth limit of 64 and node limit of 100,000. Duplicate keys, non-finite JSON constants, invalid UTF-8, unpaired surrogates, blank rows, missing final LF, excessive files/lines/issues, and projection disagreement fail closed.

### Post-export semantic invariant

The executor already recalculates `after_graph_sha256` after `br sync --flush-only`. That path now checks the remembered full task semantics before emitting the digest.

For the exact guarded ledger path:

- every exported task-semantic value on the selected issue must remain unchanged;
- every exported task-semantic value on every unrelated issue must remain unchanged;
- the existing core delta still permits only `open` → `in_progress`, assignment to the requested actor, a non-regressing timestamp, and optionally one exact transition comment;
- represented issue membership must remain unchanged.

The remembered semantic baseline is context-local and scoped to the absolute ledger path. A guard created for another fixture, worktree, or repository cannot contaminate verification.

### Regression surface

`scripts/test_agent_task_semantics.py` contains focused cases for:

- token invalidation across every major task-semantic field and unknown nested extensions;
- dependency metadata changes;
- harmless row/dependency/label/object-key order normalization;
- the permitted claim-core transition;
- selected and unrelated semantic drift;
- nested-metadata depth refusal;
- mutation between projections;
- a broad core projection that does not match the core projection derived from the same stable bytes.

The suite is compiled and executed by `scripts/check.sh` before the executor tests.

### Representative commits

- `5811d5c` added the full task-semantic reader and canonical projection.
- `43feb35` bound the projection into claim-graph/input generation.
- `2df9d98` scoped remembered postconditions to one exact ledger path.
- `ce33db7` preserved the public guard/token envelope while versioning graph semantics.
- `fb964b1` added the focused semantic test suite.
- `3a8ff75` ratcheted the graph/input grammar to version 3.
- `d35685f` added the new suite to the mandatory repository gate.
- `6ec8f5a` proved the broad and semantic projections came from one stable source state.
- `c2f0453` added direct core-projection disagreement coverage.

## Agent control-plane versions

| Layer | Current contract | Role |
|---|---:|---|
| Broad snapshot | `agent_brief` snapshot version 5 | Bounded parsing, blocking integrity, broad queues, stale/unowned diagnostics. Not a claim authority. |
| Leaf planner | `fmn.agent.next` version 4 | Governed workstream classification, leafhood, containment integrity, activation, and recommendation. |
| Claim guard | `fmn.agent.claim-guard` version 2 | Stable JSON/token envelope for graph/plan/policy/schema-bound revalidation. |
| Claim input | `fmn.agent.claim-input` version 3 | Complete graph, plan, policy, and schema input to the token digest. |
| Claim graph | `fmn.agent.claim-graph` version 3 | Canonical core graph plus complete exported task semantics. |
| Task semantics | `fmn.agent.task-semantics` version 1 | Top-level and dependency fields outside `agent_brief.Issue`. |
| Claim executor | `fmn.agent.claim` version 6 | Atomic Beads claim, structured response proof, explicit export, semantic invariant, and core delta receipt. |
| Human brief | deterministic Markdown | Planner-normalized human context and atomic publication. |

The public token remains:

```text
v2:<claim-sha256>:<issue-id-or-none>
```

Old tokens do not validate against graph/input version 3 because those versions and the full graph content participate in the digest.

## Governed autonomous scope

`scripts/agent_next.py` owns one exact autonomous-workstream grammar:

```text
G0
W1 through W11
```

A valid prefix begins at the first title character and is followed by a word boundary or `:`. Open unscoped leaves remain visible but never enter autonomous ranking. Any active unscoped issue invalidates the plan, and `G0` counts toward the four-workstream activation cap.

Recommendation order remains:

1. governed leaves in an already-active governed workstream;
2. lower numeric priority;
3. greater immediate-unblock pressure;
4. greater direct-blocker pressure;
5. most recently updated;
6. lexical issue ID.

`scripts/generate_agent_brief.py` normalizes its human presentation to this planner result. The direct `agent_brief.py` workstream grouping remains broad, diagnostic, and non-authoritative.

## Atomic guarded claim execution

The autonomous mutation path is:

```text
br update ISSUE --claim --actor ASSIGNEE --json [--transition-comment TEXT]
br sync --flush-only
```

The executor keeps token revalidation, Beads' storage-level unassigned compare-and-set, response validation, export, semantic preservation, and core postcondition verification under one shared-common-directory advisory lock.

### Atomic response proof

The version-6 executor rejects successful child output unless it is:

- strict UTF-8 JSON;
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

## SVG untrusted-input hardening

The preserved 17-byte `svg_document` fuzz input exposed a valid-UTF-8 panic: the tokenizer sliced a fixed nine-byte DOCTYPE probe through the first byte of a two-byte character.

The public SVG path now performs a bounded UTF-8-safe admission pass before the private parser can inspect fixed ASCII prefixes. Permanent tests cover the exact reproducer, case-insensitive DOCTYPE rejection, and markup-like multibyte text inside quoted attributes. The finding README records the root cause and source-level resolution.

A follow-up removed an unnecessary clone of every retained SVG shape during `emit_svg_document`; admitted documents now emit directly from their shape slice.

Evidence boundary:

- the source-level panic path and regression are fixed;
- a sanitizer replay of the original fuzz target has not been executed in this editing environment;
- the admission facade currently performs a second bounded scan before the private parser. Collapsing that facade into a byte-safe tokenizer probe is a reasonable later simplification, not a current public safety gap.

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

### Unify broad display scope

The autonomous planner and generated human brief use the exact governed vocabulary. The direct broad snapshot retains its historical display classifier and is explicitly non-authoritative. Moving the shared classifier into one common module would reduce conceptual duplication, provided broad compatibility and snapshot versioning are handled deliberately.

### Tracker-native reconciliation

The full claim graph now binds every field that Beads exports in JSONL, but `.beads/issues.jsonl` must still be mutated through `br`, never reconstructed from connector output. Tracker descriptions and status should be reconciled from a real checkout with tracker-native commands when available.

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

# Focused full-semantic claim tests
python3 scripts/test_agent_task_semantics.py
python3 scripts/test_agent_claim_guard.py
python3 scripts/test_agent_claim.py

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

## Evidence for this checkpoint

Before the final stable-core projection test was added, the authored semantic module and guard passed Python bytecode compilation and a focused seven-case stub-backed test run. The committed suite now contains eight focused tests and is wired into `scripts/check.sh`.

This editing environment cannot resolve `github.com` from a container checkout, so it did not contain an exact complete repository tree, Cargo/Rust toolchain state, `br`, or UBS execution context. GitHub Actions runs triggered by the incremental commits remained queued or pending without jobs and were not used as acceptance evidence.

Therefore the complete repository gate, exact current Python suite, live-ledger planner result, Beads mutation, Rust axes, sanitizer replay, wheel/browser gates, and platform matrices are **not** represented as executed against `c2f0453`.

No `.beads/` file was manually reconstructed or replaced. Tracker state was left unchanged because tracker-native `br` execution was unavailable.

## Truthfulness rules for future updates

- Record exact commands and the source commit they exercised.
- Do not convert “compiled,” “reviewed,” “inventoried,” or “imported” into “fully implemented.”
- Do not infer hardware or artifact evidence from another platform.
- Do not close a parent merely because one child or one census reaches 100%.
- Distinguish broad readiness from governed leaf claimability.
- Scope autonomous work to `G0` or `W1` through `W11`, issue a fresh token, complete external coordination, and use the atomic executor.
- Treat claim-graph v3 as complete exported task semantics, not raw Beads database identity.
- Keep target-state design in the comprehensive plan and current-state evidence here.
