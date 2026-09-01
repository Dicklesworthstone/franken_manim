# Agent control plane

This document defines the operational layer that lets an agent reconstruct current work without loading the full Beads ledger into context or creating a second task database.

## Authority hierarchy

1. **`.beads/issues.jsonl`** is the authoritative task graph after `br sync --flush-only`.
2. **`scripts/agent_brief.py`** is the bounded, read-only graph parser and situational projection.
3. **`scripts/agent_next.py`** is the fail-closed claim planner over that projection. It alone decides autonomous leaf eligibility.
4. **`scripts/generate_agent_brief.py`** renders the broad situational projection and the exact leaf plan as deterministic Markdown.
5. A generated `docs/AGENT_BRIEF.md`, when deliberately committed, is only a cache of exact ledger bytes at one ledger timestamp. It never outranks Beads.

Source authority, semantic authority, and artifact evidence remain separate:

- source code says what is implemented;
- API schema/overlay says what compatibility status is claimed;
- Beads says what work is open or blocked;
- gates and retained artifacts say what has actually been verified.

## Safe session start

```bash
# Validate and inspect the complete graph.
python3 scripts/agent_brief.py --format json --check >/dev/null

# Obtain the canonical machine-readable claim plan.
python3 scripts/agent_next.py --format json --check

# Require one claimable leaf; exits 3 when the graph is valid but no leaf exists.
python3 scripts/agent_next.py --require

# Render the broad human brief plus the same leaf-safe decision.
python3 scripts/generate_agent_brief.py --stdout
```

`agent_next.py` emits either one claimable leaf ID or `none` in its default mode. Exit `1` means the task graph or activation state is unsafe, exit `2` means malformed input, an invalid bound, or an output-budget refusal, and exit `3` means the graph is valid but currently has no claimable recommendation. Every nonzero planner exit emits no stdout payload, so automation cannot accidentally consume a failed plan.

`agent_brief.py --format next` is a legacy broad ready-queue projection. It does not inspect live child containment and is not an autonomous claim surface. New automation must use `agent_next.py`; retaining the legacy spelling is compatibility only, not an endorsement.

Before claiming the recommendation, also verify current `main`, current file reservations, and any coordinating agent messages. A recommendation is a deterministic graph choice, not a lease.

## Claimability rules

An issue is recommendation-eligible only when all of the following hold:

- status is `open`;
- every `blocks` target exists and is closed or tombstoned;
- it has no assignee;
- it is not an epic container;
- it has no live issue whose `parent-child` edge points to it;
- neither the blocking graph nor the parent-child graph has a live cycle;
- claiming it does not violate the four-workstream activation cap.

A task can therefore be a container even when its `issue_type` is `task`, `feature`, or `bug`. Live child topology, not only the type label, determines whether it is a leaf.

Recommendation order is deterministic:

1. eligible leaves in an already-active workstream before leaves that activate another stream;
2. lower numeric priority;
3. more live issues for which completing this leaf removes the sole unresolved blocker;
4. more directly blocked live dependents;
5. scoped work before `UNSCOPED` work;
6. most recently updated;
7. lexical issue ID.

This deliberately values coordination economy and graph-release pressure without allowing those heuristics to override explicit priority.

## Ledger parsing contract

The parser is an untrusted-input boundary. It opens the ledger through a descriptor-bound regular-file check and uses `O_NOFOLLOW` when the host exposes it. It refuses:

- malformed UTF-8 JSONL, blank records, missing final LF, and duplicate issue IDs;
- duplicate JSON object keys at any depth;
- unknown statuses;
- invalid priorities, required text fields, or a present-but-invalid `updated_at`;
- falsey non-array spellings for `dependencies` or `comments`;
- malformed comment rows or non-string comment text;
- dependency or comment arrays over their finite per-issue budgets;
- dependency rows owned by another issue;
- self-dependencies or duplicate `(target, type)` edges;
- non-regular ledger files and total ledger, line, issue, dependency, or comment counts over their limits.

Explicit `null` remains the canonical empty spelling for optional dependency and comment arrays. Invalid present data is never silently converted to absence.

## Graph-integrity contract

The broad projection reports all missing targets. Missing non-blocking links such as an orphan historical parent link are visible diagnostics but do not disable unrelated work. A missing `blocks` target or live blocking strongly connected component makes the graph non-claimable.

The leaf planner additionally rejects live `parent-child` cycles. Both SCC analyses are iterative rather than recursive, so a valid deep blocking chain or hierarchy cannot fail merely because it exceeds Python's call-stack limit.

The deterministic brief generator consumes the exact planner integrity record. Blocking cycles, containment cycles, missing blockers, or an activation-cap breach are all decided before file publication or stdout rendering.

## Machine contract

`agent_next.py --format json` emits one canonical compact JSON record with schema `fmn.agent.next`, version `2`. It includes:

- normalized blocking- and containment-integrity records;
- the selected leaf and activation effect;
- live-child containment;
- direct and immediate-unblock pressure;
- bounded claimable, container, and assigned-ready queues.

Field order is canonical through sorted-key JSON, output always ends in one LF, and the default `as_of` is the newest ledger `updated_at`. Identical ledger bytes therefore produce identical JSON without requiring a wall-clock override. An explicit `--as-of` remains available for fixtures. The rendered payload has a 4 MiB ceiling; an oversized plan returns exit `2` before emitting any stdout bytes.

## Deterministic human rendering

The generated brief's `as_of` is also the newest issue `updated_at`, not wall-clock time. Its leaf-safe section is generated from the same planner record as the JSON interface.

```bash
# Nonmutating exact output
python3 scripts/generate_agent_brief.py --stdout

# Publish docs/AGENT_BRIEF.md atomically
python3 scripts/generate_agent_brief.py

# Verify a deliberately committed projection
python3 scripts/generate_agent_brief.py --check
```

Publication is fail-closed:

- task-graph integrity and activation-cap checks run before output;
- existing output is read through a descriptor-bound regular-file check;
- output and temporary symlinks are refused;
- a pre-existing temporary path is never truncated or followed;
- the new file is created exclusively, flushed, fsynced, and atomically replaced;
- a failed replace removes only the temporary inode created by that invocation;
- if that temporary path was substituted, it is preserved and the identity change is reported;
- malformed input or failed publication leaves the prior artifact untouched.

## Mutation protocol

The projections never mutate task state. Status, dependencies, comments, and closure reasons must be changed through Beads:

```bash
br show <id>
br update <id> --status=in_progress
br close <id> --reason "..."
br sync --flush-only
git add .beads/
git commit -m "chore(beads): ..."
```

Do not reconstruct or replace the large ledger from a truncated API response. If the complete current bytes are not available through `br` or an exact local checkout, leave the tracker unchanged and state that limitation explicitly.

## Verification policy

`scripts/check.sh` compiles the parser, planner, generator, and all focused regression files; runs the parser, planner, deterministic-output, generator, and publication-I/O suites; validates the live claim plan; and renders the real ledger through `--stdout`. The same local gate then proceeds into the Rust, Python, WASM, and structural checks.

Hosted GitHub Actions is not part of this authority chain. The gate is intentionally runnable on local or owned build hosts, and unavailable hosted capacity is neither a waiver nor a product failure.

## Handoff

At session end, record:

- exact commits and paths;
- focused and repository-wide commands actually run;
- whether the checked source commit remained unchanged;
- remaining graph or evidence failures;
- the next concrete claimable leaf, if any;
- any tracker mutation that still needs `br sync --flush-only`.

Never describe a partial gate, a different commit, an unrun platform, or a historical comment as current evidence.
