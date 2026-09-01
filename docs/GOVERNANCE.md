# GOVERNANCE.md — the program governance machinery (R9)

Risk R9 — program bandwidth across eleven workstreams — is rated **High**, and the plan names governance as its mitigation, with a hard kill criterion: **breaching governance halts new activation.** This document makes that mitigation checkable. Governance is a deliverable, not a vibe.

Authority: the plan (`COMPREHENSIVE_PLAN_FOR_THE_DESIGN_OF_FRANKEN_MANIM.md`, Revision 4) is the source of truth; this document operationalizes §20 (workstreams and gates), §23 (the decision log), §2.9 (the upstream ledger), and R9/R20/R21 (standing stop conditions). Where they seem to disagree, the plan wins and this file gets fixed.

---

## 1. Workstream activation and claim integrity

**The cap: at most 4 simultaneously active workstreams.** G0 spike work counts as one workstream named `G0`. Raising the cap requires an ADR.

**Definition — active.** A workstream is active iff at least one of its beads is `in_progress`. Historical touch does not count. A workstream with landed beads and no current claim is dormant; reactivation goes through the same check as fresh activation. A workstream deactivates when its last `in_progress` bead is closed or released to `open`.

**Definition — claimable leaf.** An issue is claimable only when it is `open`, unassigned, dependency-ready, not an epic, and has no live `parent-child` descendant. A task, bug, or feature with live children is a container regardless of type label. The live blocking graph must have no missing blocker or cycle; the live containment graph must have no cycle.

### Mandatory pre-claim sequence

```bash
# Validate broad state and inspect the exact plan.
python3 scripts/agent_brief.py --format json --check >/dev/null
python3 scripts/agent_next.py --format json --check

# Issue one exact graph/plan/policy/schema-bound token.
token="$(python3 scripts/agent_claim_guard.py --require)"
issue="${token##*:}"

# Inspect task and all non-ledger coordination state.
br show "$issue"
# Check current main, Agent Mail, file reservations, and active peers.

# Optional no-mutation receipt.
python3 scripts/agent_claim.py \
    --expect-token "$token" \
    --issue "$issue" \
    --assignee "$FMN_AGENT_ID" \
    --command-timeout-seconds 60 \
    --dry-run

# Canonical guarded claim transition.
python3 scripts/agent_claim.py \
    --expect-token "$token" \
    --issue "$issue" \
    --assignee "$FMN_AGENT_ID" \
    --command-timeout-seconds 60 \
    --transition-comment "Claimed after graph, reservation, and HEAD checks"
```

Exit identities are governance:

- exit `0`: valid plan/token, or a verified executor receipt;
- exit `1`: graph integrity or activation state is unsafe;
- exit `2`: ledger, arguments, bounds, token grammar, timeout, or output contract is malformed;
- exit `3`: the graph is valid but has no claimable leaf;
- exit `4`: the token or explicit issue is stale;
- exit `5`: no verified executor receipt because lock acquisition, child execution, timeout/cleanup, flush, output bounds, or postcondition verification failed.

Every nonzero planner, guard, or executor result emits no usable success payload. **Never move an issue to `in_progress` from a failed or empty plan.** A recommendation and token are not a lease: external coordination checks remain mandatory immediately before executor invocation.

Exit `5` does not prove that native tracker state is unchanged. `br update` may have mutated state before a timeout, flush failure, or postcondition failure. Inspect `br show ISSUE`, `br sync --status`, and the working tree before retrying; never replay the old token blindly.

The executor timeout is per `br update` and per `br sync --flush-only`. Production defaults to 60 seconds and accepts finite positive values up to 3,600 seconds. Timeout cleanup is process-tree-aware and bounded; it may extend the failure path slightly beyond the selected deadline.

The planner prefers an eligible leaf in an already-active workstream. If none exists, it may recommend activating a new stream only while the active count is below four. Breaching the cap halts *new activation only*; in-flight work runs to completion.

`br ready` remains broad dependency-ready context; it does not understand non-epic containment. `bv` is graph-analysis support; rank cannot override assignment, integrity, containment, activation, token, or executor refusal. `agent_next.py` is the selection surface, `agent_claim_guard.py` is the compare-before-set contract, and `agent_claim.py` is the canonical autonomous mutation surface.

The shared advisory lock serializes cooperating processes only within one clone's Git common directory. Another clone, direct manual `br`, Agent Mail, reservations, and agents that ignore the executor remain outside that lock.

---

## 2. Gate ownership

Every gate G0–G5 has a named owner responsible for convening review and recording its evidence packet. The owner of record remains **Jeffrey Emanuel (program owner)**. He may delegate a verdict to a reviewing agent under ADR-0018; the packet records the delegate, date, and evidence viewed. The session assembling a gate packet (the marshal) is named in the gate's epic bead. A chat summary is never a pass.

| Gate | Name | Evidence packet |
|---|---|---|
| G0 | The Laws of the Machine | one ratification note per spike under `docs/g0/`; decision amendments as ADRs; `SUITE.lock` + `SUITE_ALLOWLIST.tsv` committed |
| G1 | Core 2D | primitive-corpus self-goldens; {1,4,16} thread proof; path/kernel fixtures; Look Gallery verdicts |
| G2 | The Native Word | tier-1 published-rule verification; span-map fixtures; ratchet dashboard; Gallery verdicts; PG-1(G2) + PG-7 |
| G3 | Depth & Motion | 3D/lighting/camera fixtures; Studio baseline; OQ-12 annex/fallback declaration; PG-3 (+ PG-A if shipped) |
| G4a | The Python Gallery | frozen `VIDEO_CORPUS.lock`; structural runs for enumerated scenes; Gallery review; TeX gaps; PG-8 class table |
| G4b | Certified Reproducibility | certified-matrix bit manifests; PG-5; PG-1(G4); provenance samples |
| G5 | Distribution & Leapfrogs | release artifacts by tier; selection UX; reproducible-release proof; annex and ratchet records |

A gate passes only when the owner records a verdict against the committed packet — in the gate epic's close reason and, when the plan changes, an ADR. **The packet is the pass.**

---

## 3. Architecture Decision Records

Convention: `docs/adr/NNNN-kebab-title.md`, numbered monotonically and never reused; template at `docs/adr/TEMPLATE.md`; status vocabulary `Proposed → Accepted → Superseded by ADR-NNNN`.

An ADR is required for:

- every amendment to D-01…D-24;
- every resolution of OQ-1…OQ-12;
- every policy ruling made under a standing rule.

**True-up rule:** when an ADR amends the plan, the plan is edited to match in the same commit, or the ADR records why true-up is deferred and under which bead.

Worked examples: ADR-0001 (Rev-4 contract pivot), ADR-0002 (D-11 ratification), ADR-0003 (dev/fuzz allowlist tiers).

---

## 4. Review rules before handoff

A workstream may not hand off with failing gates or unwritten fixtures. Every item below is verified before a session ends; if a handoff violates one, the next session's first duty is restoring the invariant.

1. `scripts/check.sh` green on one named, unchanged committed source HEAD. A partial run, uncommitted tree, different commit, or hosted-only result is not this evidence.
2. `ubs` run over changed files; criticals fixed or explicitly adjudicated as false positives in the handoff.
3. New or changed behavior carries tests/fixtures in the same commit. Self-golden drift is adjudicated, never reflexively re-blessed.
4. New semantic divergences from the Reference have Behavior Notes under `docs/behavior_notes/`.
5. Beads trued up: finished work closed with reasons; unfinished claims released to `open` with status comments; follow-ups filed.
6. `br sync --flush-only`, then `.beads/` staged and committed.
7. Re-run `python3 scripts/agent_next.py --format json --check` against the exported post-mutation ledger.
8. If a next leaf exists, issue a **fresh** `agent_claim_guard.py --require` token for the handoff. A pre-mutation token is stale by construction.
9. Agent-mail reservations released; a handoff message posted in the bead thread (`thread_id` = bead ID).
10. The handoff records exact commits, commands actually run, source HEAD, remaining evidence gaps, and the current next-leaf/no-work/refusal state.

---

## 5. Stop conditions and leapfrog postponement

| Tripwire | Halt |
|---|---|
| Activation cap reached | no new workstream activation |
| Missing blocker, blocking cycle, or containment cycle | no new claim until repaired |
| Planner/guard/executor nonzero exit | no autonomous claim from that plan/token |
| Claim exit `5` | inspect tracker/native state before any retry |
| Core performance gate PG-1…PG-3 regresses | all annex work pauses (R21) |
| Purity misclassification appears | effect class demotes to stateful engine-wide until root-caused (R20) |
| Self-golden drifts without adjudicated cause | introducing change reverts |
| Governed closure contains an unlisted package | no lands until closure is green |

Enhanced-tier and leapfrog work — W7 lineage, annex broadening beyond G3 Studio-preview duty, exploratory tier, and WASM beyond tier 1 — is postponable by policy.

1. **Postponement never weakens core work.** Core interfaces retain the seams leapfrogs require.
2. **Postponement is never scope reduction.** The item moves publicly to a later gate and its acceptance criteria travel unchanged.

---

## 6. Upstream ledger ritual (§2.9)

Primitives belonging in a foundation crate land there, never here, and are tracked in [`UPSTREAM_LEDGER.md`](../UPSTREAM_LEDGER.md).

1. **Propose:** add/update the ledger row with primitive, target repo, owner, status, and coordination dependency.
2. **Land upstream:** implement under the foundation repository's governance.
3. **Pin:** bump the foundation commit in `SUITE.lock` and affected `SUITE_ALLOWLIST.tsv` rows in a single-purpose commit.
4. **Diff:** run the full Gauntlet; diff self-goldens and Look Gallery evidence.
5. **Adjudicate and record:** record the adjudication in the pin-bump commit and advance the ledger row. Land only green.
