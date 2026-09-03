# FrankenManim implementation status

**Status date:** 2026-09-02 America/New_York / 2026-09-03 UTC  
**Substantive source-and-test checkpoint:** `7aeb3f40a763998d07b43b74613f3c6becc49207`  
**Agent-governance checkpoint:** ADR-0023 and `docs/GOVERNANCE.md` through `19e1e8b0014f5e4dd68aebc0520cd7ba9fb98283`.  
**Authority rule:** this document summarizes evidence. `.beads/issues.jsonl` remains the task, status, and dependency authority; the Revision-4 comprehensive plan remains the design authority.

## How to read this document

FrankenManim is pre-1.0. A capability is implemented only when a concrete source surface and a checkable test or artifact boundary exist. Plan text, a reviewed Parity Ledger row, an inventoried refusal, an old Beads comment, a generated projection, or a queued hosted workflow is not implementation evidence.

Source correctness, compatibility adjudication, tracker state, release packaging, hardware execution, and gate verdicts are distinct evidence lanes. This document keeps those lanes separate.

## Executive state

The repository has a broad native Rust implementation, a separately installed Python compatibility portal, a deterministic agent control plane, and multiple platform/release gates. The highest-risk current gap is semantic convergence and evidence discipline at the boundaries rather than native crate structure.

The W10 portal now has a machine-enforced distinction between **reviewed parity claims** and **runtime truth**: a wheel can audit every `same`/`improved` overlay row against its actually imported namespace, reject schema placeholders or missing symbols, and report the SHA-256 of the exact embedded overlay bytes. The clean-wheel wrapper additionally rejects a wheel whose embedded overlay differs from the checkout. This is a truthfulness mechanism, not evidence that the current wheel has zero contradictions; that installed-wheel gate has not been executed in this editing environment.

The remaining program-level risks are therefore explicit:

- real portal placeholders/refusals still need to be converted into callable semantics or evidence-backed tiers/exclusions;
- a reviewed ledger row must not be confused with a passing runtime self-audit;
- real-hardware, pinned-host, clean-wheel, browser, and release claims require their own receipts;
- autonomous agents must distinguish executable work from human decisions and external-evidence tasks;
- no Beads state may be reconstructed or hand-edited when tracker-native `br` execution is unavailable.

## Product surfaces

| Surface | Current implementation state | Evidence boundary |
|---|---|---|
| Native Rust library | The composition root, scene runtime, mobject/animation stack, native text and math, retained renderer, codecs, output pipeline, and built-in scene corpus exist as workspace crates. Menagerie families include boolean, data, drawing, graph, field, image, model, and 3D surfaces. | Ordinary source checks run through `scripts/check.sh`. Platform certification and gate verdicts remain separate. |
| `fmn` CLI | Typed render, doctor, batch, and Studio surfaces exist. Batch robot output includes terminal per-job records; CLI smoke covers a tiny render and native artifact/manifest publication. | The shipping feature shape is exercised by feature-specific commands in `scripts/check.sh`; a queued hosted run is not the named local/owned-host gate. |
| Studio | Supervisor/worker, replay, event, inspection, and presentation foundations exist. | Platform-native and real-browser presentation evidence is not inferred from Rust unit tests. |
| `fmn-python` portal | A separately installed CPython wheel exposes the pinned `manimlib` namespace with native and authored compatibility behavior. Explicit bootstrap refusals are mechanically inventoried. The wheel now ships `fmn-python --audit-parity [--robot]`, backed by one shared runtime audit module. | `fm-5wq.4` remains in progress. A named refusal, imported symbol, reviewed ledger row, or existence of the auditor is not callable semantic completion. A current clean wheel must actually pass `scripts/check_portal_runtime.sh`. |
| WASM/browser | wasm32 render/player foundations and an npm/package gate exist. | The npm/real-browser release gate is opt-in and independent of ordinary source checks. |
| Distribution | Tagged `v0.1.0` through `v0.4.0` are prereleases. | Cross-platform artifacts, hardware-specific execution, clean wheels, and certified reproducibility require independent receipts. |

## W10 runtime parity truth model

The portal has two complementary truthfulness ratchets.

### Explicit refusal inventory

`scripts/audit_portal_refusals.py` statically inventories authored `NotImplementedError`/capability refusals in the bootstrap. It proves that known refusal sites are named and mechanically visible. It does **not** prove that a reviewed symbol is callable.

### Runtime SAME/IMPROVED audit

`crates/fmn-python/python/fmn_python/parity_audit.py` is the single semantic implementation used by both checkout tooling and the installed product. It parses only the authored `[status]` section of `API_OVERLAY.tsv` and treats `same` and `improved` as implementation claims.

For every such row, the audit imports the recorded module and resolves the qualified symbol. It fails the claim when:

- the module cannot be imported;
- the reviewed symbol is missing;
- the runtime value carries `_fmn_schema_placeholder=True`.

`tiered`, `excluded`, and `unreviewed` rows are intentionally not treated as implementation claims. The report is schema `fmn.portal.runtime-audit` version 1 and contains deterministic counts, sorted contradictions, and `overlay_sha256`, the SHA-256 of the exact UTF-8 overlay bytes audited.

The installed wheel exposes the same proof directly:

```bash
fmn-python --audit-parity
fmn-python --audit-parity --robot
python3 -m fmn_python --audit-parity --robot
```

Audit-mode exits are typed:

- `0`: all reviewed SAME/IMPROVED runtime claims resolved to non-placeholder values;
- `1`: at least one runtime contradiction;
- `2`: malformed embedded audit contract or invalid audit arguments;
- existing namespace-collision behavior remains capability exit `4` before native use.

`scripts/check_portal_runtime.sh` is the clean-wheel boundary. It first imports the installed `manimlib`, invokes the wheel's own robot self-audit, preserves that exit, then compares the report's `overlay_sha256` with the checkout `API_OVERLAY.tsv`. A semantically green but stale wheel therefore fails rather than proving an obsolete ledger.

The mandatory source gate does **not** pretend to execute that installed-wheel boundary. Instead `scripts/check.sh` byte-compiles and runs hermetic regressions for the shared parser/resolver and wheel-facing CLI. Actual installed-wheel behavior stays a separate release/evidence lane.

## Agent control plane

The control plane follows a one-authority/many-projections design. The exported Beads JSONL is authority; every other document is derived and disposable.

| Layer | Contract | Role |
|---|---:|---|
| Broad snapshot | `agent_brief` snapshot version 5 | Bounded parsing, blocking integrity, broad queues, stale/unowned diagnostics. Never a claim authority. |
| Leaf planner | `fmn.agent.next` version 4 | Exact scope, leafhood, containment, activation, claim-kind policy, ranking, and recommendation. |
| Claim policy | `fmn.agent.claim-policy` version 1 | Exact `agent:claim:*` label vocabulary and autonomous/manual/external classification. |
| Claim guard | `fmn.agent.claim-guard` version 2 | Stable JSON/token envelope for graph/plan/policy/schema-bound revalidation. |
| Claim input | `fmn.agent.claim-input` version 3 | Complete graph, plan, policy, and schema input to the token digest. |
| Claim graph | `fmn.agent.claim-graph` version 3 | Canonical core graph plus complete exported task semantics. |
| Task semantics | `fmn.agent.task-semantics` version 1 | Descriptions, design, acceptance, notes, labels, metadata, and every non-core extension field. |
| Claim executor | `fmn.agent.claim` version 6 | Atomic Beads claim, bounded child execution, structured response proof, explicit export, semantic invariant, and core delta receipt. |
| Human brief | deterministic Markdown | Planner-normalized context for humans and agents; never a second tracker. |

The public token remains:

```text
v2:<claim-sha256>:<issue-id-or-none>
```

The token binds the complete task graph, planner output, nested claim-policy contract, policy bounds, and schema versions. Any authoritative change makes the old token stale.

### Exact governed scope

One shared classifier accepts only an anchored `G0` or `W1` through `W11` prefix followed by a word boundary or `:`. `W0`, `W12+`, lowercase forms, zero-padded forms, and embedded prefixes are `UNSCOPED`.

Open unscoped work remains visible but never enters autonomous ranking. Any active unscoped issue invalidates the plan. `G0` consumes one of the four active-workstream slots. ADR-0022 records this single-classifier decision and `scripts/test_agent_scope.py` locks broad/planner parity.

### Exact autonomous claim kind

ADR-0023 defines one closed Beads label namespace:

```text
agent:claim:auto
agent:claim:manual
agent:claim:external
```

Unlabelled work defaults to `auto`. Manual and external leaves remain visible in `non_autonomous_ready` but cannot be recommended. Unknown, duplicate, conflicting, or malformed reserved labels on live issues invalidate planning before any usable payload.

`scripts/agent_claim_policy.py` interprets canonical labels already loaded by `agent_task_semantics`; it does not reread or edit JSONL. `scripts/test_agent_claim_policy.py` covers the default, all three modes, no-recommendation behavior, malformed labels, closed-history tolerance, and the exact machine contract. The suite is wired into `scripts/check.sh`.

### Deterministic recommendation order

Among autonomous governed dependency-ready unassigned leaves:

1. prefer work in an already-active governed workstream;
2. lower numeric priority;
3. greater immediate-unblock pressure;
4. greater direct-blocker pressure;
5. most recently updated;
6. lexical issue ID.

Non-epic issues with live `parent-child` descendants are containers, not leaves. A recommendation is not a lease; current `main`, Agent Mail, file reservations, active peers, and the exact Bead still require inspection immediately before the guarded executor runs.

## Full task-semantic claim binding

The claim graph no longer treats the narrow broad-planner model as the complete task contract. `scripts/agent_task_semantics.py` binds descriptions/design/acceptance/notes, owners and estimates, source/due/defer metadata, labels, future extension fields, extended dependency records, and complete comments.

Issue-row order, dependency-array order, object-key order, and label order are normalized as representation only. Duplicate labels remain represented. Comment order and ordinary array order remain significant.

The loader brackets the established bounded parser with stable before/after reads and requires the broad/core projection, semantic projection, and source digests to agree. Unknown nested metadata is bounded by depth and node ceilings. The executor preserves all task-semantic fields on the selected issue and every unrelated issue across an atomic claim export.

## Atomic guarded claim execution

The only autonomous mutation path is:

```text
br update ISSUE --claim --actor ASSIGNEE --json [--transition-comment TEXT]
br sync --flush-only
```

The executor keeps token replay, Beads' storage-level compare-and-set, response validation, export, semantic preservation, and core postcondition verification under one shared-common-directory advisory lock.

Each child process has a bounded wall-clock deadline, produced-output ceiling, retained-diagnostic ceiling, concurrent stream draining, and process-tree cleanup. Exit `5` means **no verified success receipt**, not proof that native tracker state is unchanged. Recovery begins with `br show`, `br sync --status`, and `git status`; the old token must not be replayed blindly.

The lock coordinates cooperating worktrees inside one clone. It cannot serialize another clone, direct manual Beads activity, Agent Mail, file reservations, or unrelated movement of `main`.

## Python portal convergence truth

`fm-5wq.4` remains in progress. Its latest reality-check in the exported ledger supersedes older “100% reviewed” commentary: the native engine was described as structurally complete, while the live Python portal still had 63 `NotImplementedError` sites, 930 placeholder methods, and 2,413 rows requiring semantic review at the time of that comment.

Those numbers are a dated diagnostic, not an implementation percentage. The governing rules are:

- reviewed ledger coverage is not universal callable implementation;
- refusal inventory coverage is not implementation coverage;
- SAME/IMPROVED requires real callable semantics and focused evidence;
- a runtime self-audit tool existing is not the same thing as that audit passing;
- representative clean-wheel semantics remain required across API families;
- each refusal falls only through implementation or an evidence-backed tier/exclusion;
- no W10, G4, or 1.0 closure is claimed here.

## Current open obligations

### 1. W10 semantic surface

Run the new runtime audit against freshly built clean wheels and treat every reported contradiction as a direct truth defect: either implement the reviewed symbol, or correct the overlay to a justified tier/exclusion. Continue converting high-value shared portal placeholders/refusals into real behavior with focused tests. Prefer load-bearing base classes and shared mechanisms over isolated leaf wrappers so each tranche collapses many downstream gaps.

### 2. Tracker-native classification and reconciliation

Known human-decision and external-evidence leaves should receive ADR-0023 labels through `br`. Existing descriptions, statuses, dependencies, and comments should be reconciled from a real checkout. Never reconstruct `.beads/issues.jsonl` from connector output.

### 3. External evidence lanes

The following remain independent and must not crowd autonomous implementation ranking once correctly labeled:

- real aarch64 topology fixtures;
- pinned-host performance-gate receipts;
- platform-native execution matrices;
- SIMD-tier and certified cross-platform bit evidence;
- npm/WASM real-browser packaging;
- clean-wheel gates on every supported platform;
- ffmpeg/video-container equivalence receipts;
- release signing, publication, and credential-bound effects.

### 4. Gate verdicts and release claims

A green source test is not a gate verdict. The named owner or delegated reviewer must record the committed evidence packet. Tagged prereleases do not imply the 1.0 contract is earned.

## Verification entry points

```bash
# Mandatory local or owned-host repository gate
scripts/check.sh

# Runtime parity truth from a current installed wheel
fmn-python --audit-parity --robot
scripts/check_portal_runtime.sh

# Checkout-side runtime auditor (useful when the portal is importable here)
python3 scripts/audit_portal_runtime.py --check

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

## Evidence for this checkpoint

For substantive checkpoint `7aeb3f40`:

- the repository contains one shared wheel/checkout runtime parity implementation rather than duplicated parsers;
- `same`/`improved` runtime claims fail closed on module-import failure, missing symbols, and `_fmn_schema_placeholder=True` values;
- the installed product exposes deterministic human and robot self-audit surfaces with typed exits;
- every valid audit report binds the exact embedded overlay by SHA-256;
- `scripts/check_portal_runtime.sh` rejects a wheel whose embedded overlay hash differs from the checkout;
- hermetic regressions cover real reviewed callables, placeholders, missing modules/symbols, tiered/excluded exemptions, malformed/duplicate status rows, deterministic ordering, CLI argument/error behavior, and overlay identity;
- those regression files are wired into the mandatory source gate.

This connector editing environment did **not** execute the Python regressions, build or install the current wheel, run `scripts/check_portal_runtime.sh`, run the complete Cargo/Rust repository gate, execute UBS, call tracker-native `br`, use Agent Mail, exercise release credentials, or produce hardware/browser/platform receipts. Hosted GitHub Actions runs triggered by the incremental commits were pending or cancelled by newer `main` pushes and are not used as acceptance evidence.

Therefore this checkpoint claims the **implementation of the runtime truth mechanism and its committed tests**, not a passing parity verdict for the current wheel. No `.beads/` file was reconstructed or replaced.

## Protocol for the next agent

1. Read `AGENTS.md`, the comprehensive plan, this status file, ADR-0022, ADR-0023, and the exact Bead before editing.
2. Run the broad check and guarded planner. Treat every nonzero result as a refusal, not a hint.
3. For W10 work, build/install a fresh wheel and run `scripts/check_portal_runtime.sh`; use contradictions as a prioritized semantic defect queue.
4. Inspect `main`, reservations, peers, and the recommended Bead; then use the guarded atomic executor.
5. Prefer shared mechanisms and base abstractions that retire many portal or runtime gaps together.
6. Keep implementation, tests, tracker transition, and evidence in the same small tranche.
7. Commit directly to `main` only as an atomic fast-forward; never batch unrelated work into a giant final commit.
8. Record what actually ran, what remains external, and why any claimed capability is earned.

## Truthfulness rules for future updates

- Record exact commands and the source commit they exercised.
- Do not convert “compiled,” “reviewed,” “inventoried,” “imported,” “auditable,” “queued,” or “pending” into “implemented,” “compatible,” or “green.”
- A runtime-audit implementation does not prove an audit pass; preserve the actual report and overlay hash when claiming one.
- Do not infer hardware or artifact evidence from another platform.
- Do not close a parent merely because one child or one census reaches 100%.
- Distinguish broad readiness, autonomous claimability, human decisions, and external evidence.
- Preserve the one-authority model: projections may be regenerated; Beads state may only be changed through tracker-native operations.
