# Agent control plane

This document defines the operational layer that lets an agent reconstruct current work without loading the full Beads ledger into context or creating a second task database.

## Authority hierarchy

1. **`.beads/issues.jsonl`** is the authoritative task graph after `br sync --flush-only`.
2. **`scripts/agent_brief.py`** is a bounded, read-only semantic projection.
3. **`scripts/generate_agent_brief.py`** renders that projection as deterministic Markdown.
4. A generated `docs/AGENT_BRIEF.md`, when deliberately committed, is only a cache of exact ledger bytes at one ledger timestamp. It never outranks Beads.

Source authority, semantic authority, and artifact evidence remain separate:

- source code says what is implemented;
- API schema/overlay says what compatibility status is claimed;
- Beads says what work is open or blocked;
- gates and retained artifacts say what has actually been verified.

## Safe session start

```bash
python3 scripts/agent_brief.py --format next --check
python3 scripts/generate_agent_brief.py --stdout
```

The first command emits either one claimable leaf ID or `none`. It exits nonzero when the activation cap or dependency-integrity contract is broken. The second renders the complete human brief without mutating the worktree.

Before claiming the recommendation, also verify current `main`, current file reservations, and any coordinating agent messages. A recommendation is a deterministic graph choice, not a lease.

## Claimability rules

An issue is recommendation-eligible only when all of the following hold:

- status is `open`;
- every `blocks` target exists and is closed or tombstoned;
- it has no assignee;
- it is not an epic container;
- the dependency graph has no live blocking cycle or missing blocker target;
- claiming it does not violate the four-workstream activation cap.

Among eligible leaves, work in an already-active workstream wins before a numerically higher-priority issue that would activate another stream. This deliberately trades a small amount of local priority optimality for lower coordination and context-switch cost.

## Integrity contract

The parser refuses before recommendation on:

- malformed UTF-8 JSONL, blank records, missing final LF, duplicate issue IDs;
- unknown statuses;
- invalid priorities or required text fields;
- dependency arrays over their finite budget;
- dependency rows owned by another issue;
- self-dependencies or duplicate `(target, type)` edges;
- total ledger, line, issue, or dependency counts over their limits.

The projection reports all missing targets. Missing non-blocking links such as an orphan historical parent link are visible diagnostics but do not disable work. A missing `blocks` target or a live blocking strongly connected component makes the graph non-claimable.

Cycle analysis is iterative rather than recursive, so a valid deep dependency chain cannot fail merely because it exceeds Python's call-stack limit.

## Deterministic rendering

The generated brief's `as_of` is the newest issue `updated_at`, not wall-clock time. Identical ledger bytes therefore produce identical Markdown.

```bash
# Nonmutating exact output
python3 scripts/generate_agent_brief.py --stdout

# Publish docs/AGENT_BRIEF.md atomically
python3 scripts/generate_agent_brief.py

# Verify a deliberately committed projection
python3 scripts/generate_agent_brief.py --check
```

Publication is fail-closed:

- graph integrity and activation-cap checks run before output;
- output and temporary symlinks are refused;
- a pre-existing temporary path is never truncated or followed;
- the new file is written exclusively, flushed, fsynced, and atomically replaced;
- malformed input leaves the prior artifact untouched.

## Mutation protocol

The projection never mutates task state. Status, dependencies, comments, and closure reasons must be changed through Beads:

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

`scripts/check.sh` compiles the control-plane scripts, runs their unit suites, and renders the real ledger through `--stdout`. The same local gate then proceeds into the Rust, Python, WASM, and structural checks.

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
