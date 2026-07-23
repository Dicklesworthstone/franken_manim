# GOVERNANCE.md — the program governance machinery (R9)

Risk R9 — program bandwidth across eleven workstreams — is rated **High**,
and the plan names governance as its mitigation, with a hard kill
criterion: **breaching governance halts new activation.** This document is
that mitigation made concrete. Governance is a deliverable, not a vibe:
each section below is a checkable rule, and the session workflow in
`AGENTS.md` references it directly.

Authority: the plan (`COMPREHENSIVE_PLAN_FOR_THE_DESIGN_OF_FRANKEN_MANIM.md`,
Revision 4) is the source of truth; this document operationalizes §20
(workstreams and gates), §23 (the decision log), §2.9 (the upstream
ledger), and R9/R20/R21 (the standing stop conditions). Where they seem to
disagree, the plan wins and this file gets fixed.

---

## 1. The workstream activation limit

**The cap: at most 4 simultaneously-active workstreams.** (G0 spike work
counts as one workstream, named "G0". The cap is chosen for the current
agent-session bandwidth; raising it is an ADR.)

**Definition — active.** A workstream is *active* iff at least one of its
beads is `in_progress`. Historical touch does not count: a workstream with
landed (closed) beads and nothing claimed is *dormant*, and re-activating
it goes through the same check as activating a fresh one. A workstream
deactivates the moment its last `in_progress` bead is closed or released
back to `open`.

**The check (mandatory, before claiming any bead).** Before moving a bead
to `in_progress`:

```bash
br list --status=in_progress
```

Map each listed bead to its workstream by the `W#:` / `G0-#:` title prefix
(epics and unprefixed beads map to the workstream of the crates they
touch). Count distinct active workstreams. If the bead you want to claim
belongs to a workstream **not** in that set and the count is already at
the cap: **do not activate.** Pick ready work inside an already-active
workstream instead, or land in-flight work first. Breaching the cap halts
*new activation only* — in-flight work always runs to completion.

---

## 2. Gate ownership

Every gate G0–G5 has a named owner responsible for convening the
gate-review session and for the recorded evidence packet. The owner of
record for all gate *verdicts* is **Jeffrey Emanuel (program owner)**; the
agent session that assembles a gate's evidence packet ("the marshal") is
named in the gate's epic bead at review time, and the packet lands in the
repo before the gate is declared passed.

| Gate | Name | Evidence packet (recorded, in-repo) |
|---|---|---|
| G0 | The Laws of the Machine | one ratification note per spike under `docs/g0/` (the G0-1 note is the pattern); decision amendments filed as ADRs; `SUITE.lock` + `SUITE_ALLOWLIST.tsv` committed |
| G1 | Core 2D | primitive-corpus self-goldens bit-locked; the {1,4,16} thread-sweep proof; path-invariant + kernel fixture runs; Look Gallery verdict sheet vs Reference imagery |
| G2 | The Native Word | tier-1 construct set with published-rule verification runs; the span-map fixtures (`isolate`/`t2c`/`TransformMatchingTex`); the ratchet dashboard snapshot; Gallery verdicts; PG-1(G2) + PG-7 runs |
| G3 | Depth & Motion | 3D/lighting/camera fixture runs; Studio-baseline demonstration; the annex-or-fallback declaration (OQ-12) with its public note if the fallback is exercised; PG-3 (+ PG-A if the annex ships) |
| G4a | The Python Gallery | `VIDEO_CORPUS.lock` frozen; structural-assertion runs for every enumerated scene; Gallery review; TeX-pending scenes named with missing constructs; the published PG-8 class table |
| G4b | Certified Reproducibility | bit-identity manifests across the certified matrix; PG-5 runs; PG-1(G4); sidecar provenance samples |
| G5 | Distribution & Leapfrogs | per-tier release artifacts + selection UX; reproducible-release proof; the broadened-annex and ratchet-progress records |

A gate is *passed* when the owner records the verdict against the packet —
in the gate epic's close reason and, for anything that amends the plan, an
ADR. **A gate is never passed on a summary; the packet is the pass.**

---

## 3. Architecture Decision Records

Convention: `docs/adr/NNNN-kebab-title.md`, numbered monotonically, never
renumbered or reused; template at `docs/adr/TEMPLATE.md`; status vocabulary
`Proposed → Accepted → Superseded by ADR-NNNN`.

**An ADR is required for:**
- every amendment to the plan's decision log D-01…D-24;
- every resolution of an open question OQ-1…OQ-12 (each has a named owner
  gate/workstream in §23 — code never silently resolves one);
- every policy ruling made under a standing rule (e.g. allowlist-tier
  rulings under D1, certification-matrix changes under §16.7).

**The true-up rule:** when an ADR amends the plan, the plan document is
edited to match **in the same commit** — or the ADR's Consequences section
records precisely why the true-up is deferred and under which bead.

Worked examples: ADR-0001 (the retroactive record of the Rev-4 contract
pivot), ADR-0002 (the D-11 amendment via G0-1 ratification), ADR-0003 (a
live policy ruling: the dev/fuzz allowlist tiers).

---

## 4. Review rules — required coverage before handoff

A workstream may not hand off with failing gates or unwritten fixtures.
"Landing the plane" (`AGENTS.md`), made checkable — every item below is
verified before a session ends, and a handoff violating any of them is not
a handoff (the next session's first duty is restoring the invariant):

1. `scripts/check.sh` green — `cargo fmt --check`, `cargo check
   --all-targets`, `cargo clippy --all-targets -- -D warnings`, and the
   hard gate: `cargo test` exits 0.
2. `ubs` run over changed files; criticals fixed, or adjudicated
   false-positive *in the handoff note* — never silently ignored.
3. New or changed behavior carries its fixtures/tests in the same commit.
   Self-goldens drifted by the change are adjudicated, not re-blessed.
4. Semantic divergences from the Reference introduced this session have
   Behavior Notes (`docs/behavior_notes/`).
5. Beads trued up: finished work closed with reasons; claimed-but-unfinished
   beads released back to `open` with a status comment; follow-ups filed.
6. `br sync --flush-only`, then `.beads/` staged and committed.
7. Agent-mail file reservations released; a handoff message posted in the
   bead's thread (`thread_id` = the bead ID).

---

## 5. Stop conditions & the leapfrog-postponement policy

**Stop conditions.** Each is a tripwire that halts a named class of work
until its condition clears:

| Tripwire | Halt |
|---|---|
| Activation cap reached (§1) | no new workstream activation |
| Core performance gate (PG-1…PG-3) regresses | all annex work pauses (R21) |
| A purity misclassification is observed in frame-parallel output | that effect class demotes to stateful engine-wide until root-caused (R20) |
| A self-golden drifts without an adjudicated cause | the introducing change reverts; the drift is a finding, never noise |
| A governed-closure violation (unlisted package) appears | no lands until the closure check is green |

**The leapfrog-postponement policy.** Enhanced-tier and leapfrog work —
the W7 fnx/fp/data lineage, annex broadening beyond the G3 Studio-preview
duty, the exploratory tier, WASM beyond tier-1 — is postponable **by
policy**, at any time, for bandwidth. Two clauses make the policy safe,
and the policy text is itself the artifact:

1. **Postponement never weakens core work.** No core interface is shaped
   around a postponed leapfrog's absence; the seams the leapfrog needs
   (the backend trait, the enhanced-mobject registration points, the WASM
   feature axes) are built with core regardless.
2. **Postponement is never scope reduction.** Gates are integration
   checkpoints (there is no MVP); a postponed item moves to a later gate
   *publicly* — a dated note in the gate's epic bead — and its acceptance
   criteria travel unmodified.

---

## 6. The upstream ledger ritual (§2.9)

Primitives that belong in a foundation crate land there, never here —
tracked in [`UPSTREAM_LEDGER.md`](../UPSTREAM_LEDGER.md) at the repo root.
The ritual, per entry (R8, R17 — upgrades deliberate, Gauntlet-diffed):

1. **Propose:** add or update the ledger row (primitive, target repo,
   owner, status, coordination step it is waiting on).
2. **Land upstream:** the work happens in the foundation repo, governed
   like any foundation change.
3. **Pin:** bump the foundation commit in `SUITE.lock` (and the
   `SUITE_ALLOWLIST.tsv` rows it affects) in a commit that does nothing
   else.
4. **Diff:** run the full Gauntlet; diff self-goldens and the Look
   Gallery. A drifted golden is a finding to adjudicate, not noise to
   re-bless.
5. **Adjudicate and record:** the adjudication goes in the pin-bump commit
   message; the ledger row's status advances. Land only green.
