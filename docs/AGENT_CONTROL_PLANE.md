# Agent control plane

This document defines the operational layer that lets an agent reconstruct current work, select one safe leaf, revalidate that decision, and perform the claim transition without loading the full Beads ledger into context or creating a second task database.

## Authority hierarchy

1. **`.beads/issues.jsonl`** is the authoritative exported task graph after `br sync --flush-only`.
2. **`scripts/agent_brief.py`** is the bounded, read-only strict-JSON parser and situational projection.
3. **`scripts/agent_next.py`** is the fail-closed claim planner. It alone decides autonomous leaf eligibility.
4. **`scripts/agent_claim_guard.py`** binds the canonical graph, complete plan, policy, and schema contracts into one version-2 claim token.
5. **`scripts/agent_claim.py`** revalidates that token and performs the guarded `open` → `in_progress` Beads mutation under Git's shared-common-directory advisory lock.
6. **`scripts/generate_agent_brief.py`** renders the broad situational projection and exact leaf plan as deterministic Markdown.
7. A generated `docs/AGENT_BRIEF.md`, when deliberately committed, is only a cache of exact ledger bytes at one ledger timestamp. It never outranks Beads.

Source authority, semantic authority, task authority, and evidence remain separate:

- source code says what behavior exists;
- API schema/overlay says what compatibility status is claimed;
- Beads says what work is open or blocked;
- gates and retained artifacts say what was actually exercised.

## Safe session start and claim

```bash
# Validate and inspect the complete graph.
python3 scripts/agent_brief.py --format json --check >/dev/null

# Inspect the canonical machine-readable claim plan.
python3 scripts/agent_next.py --format json --check

# Issue one graph-, plan-, policy-, and schema-bound recommendation.
token="$(python3 scripts/agent_claim_guard.py --require)"
issue="${token##*:}"

# Inspect Beads plus all coordination state outside the ledger.
br show "$issue"
# Check current main, Agent Mail, file reservations, and active peers here.

# Optionally inspect the exact intended mutation without invoking br.
python3 scripts/agent_claim.py \
    --expect-token "$token" \
    --issue "$issue" \
    --assignee "$FMN_AGENT_ID" \
    --command-timeout-seconds 60 \
    --dry-run

# Revalidate, mutate, flush, and verify under one shared local lock.
python3 scripts/agent_claim.py \
    --expect-token "$token" \
    --issue "$issue" \
    --assignee "$FMN_AGENT_ID" \
    --command-timeout-seconds 60 \
    --transition-comment "Claimed after graph, reservation, and HEAD checks"

# Render the broad human brief plus the same leaf-safe decision.
python3 scripts/generate_agent_brief.py --stdout
```

`agent_next.py` emits either one claimable leaf ID or `none` in its default mode. Exit `1` means the task graph or activation state is unsafe, exit `2` means malformed input, an invalid bound, or an output-budget refusal, and exit `3` means the graph is valid but currently has no claimable recommendation. Every nonzero planner exit emits no stdout payload.

`agent_brief.py --format next` has been removed because the broad ready queue is not leaf-safe. It exits `2` before reading the ledger and directs callers to `agent_next.py`. There is no compatibility claim surface competing with the planner.

A recommendation and token are not a lease. Before invoking the executor, verify current `main`, current file reservations, and coordinating agent messages. The executor serializes cooperating processes only within one clone's shared Git common directory.

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

## Ledger parsing contract

The parser is an untrusted-input boundary. It opens the ledger through a descriptor-bound regular-file check and uses `O_NOFOLLOW` when the host exposes it. It refuses:

- malformed UTF-8 JSONL, blank records, missing final LF, and duplicate issue IDs;
- duplicate JSON object keys at any depth;
- unquoted `NaN`, `Infinity`, and `-Infinity` constants at any depth;
- unknown statuses;
- invalid priorities, required text fields, or a present-but-invalid `updated_at`;
- falsey non-array spellings for `dependencies` or `comments`;
- malformed comment rows or non-string comment text;
- dependency or comment arrays over their finite per-issue budgets;
- dependency rows owned by another issue;
- self-dependencies or duplicate `(target, type)` edges;
- non-regular ledger files and total ledger, line, issue, dependency, or comment counts over their limits.

Explicit `null` remains the canonical empty spelling for optional dependency and comment arrays. Quoted non-finite spellings remain ordinary strings. Invalid present data is never silently converted to absence.

The canonical claim-graph grammar is version `2`; that version is part of the claim digest. A token from the earlier permissive JSON grammar cannot revalidate under the strict decoder.

## Graph-integrity contract

The broad projection reports all missing targets. Missing non-blocking links such as an orphan historical parent link are visible diagnostics but do not disable unrelated work. A missing `blocks` target or live blocking strongly connected component makes the graph non-claimable.

The leaf planner additionally rejects live `parent-child` cycles. Both SCC analyses are iterative rather than recursive, so a valid deep blocking chain or hierarchy cannot fail merely because it exceeds Python's call-stack limit.

The deterministic brief generator and claim guard consume the exact planner integrity record. Blocking cycles, containment cycles, missing blockers, or an activation-cap breach are decided before publication, token output, or mutation.

## Machine contracts

### Leaf plan

`agent_next.py --format json` emits one canonical compact JSON record with schema `fmn.agent.next`, version `2`. It contains normalized integrity, activation state, the selected leaf, live-child containment, unblock pressure, and bounded claimable/container/assigned queues.

Field order is canonical through sorted-key JSON, output ends in one LF, and the default `as_of` is the newest ledger `updated_at`. Identical ledger bytes therefore produce identical JSON without a wall-clock override. The payload has a 4 MiB ceiling; an oversized plan exits `2` before stdout.

### Claim token

`agent_claim_guard.py` emits schema `fmn.agent.claim-guard`, version `2`, and tokens of the form:

```text
v2:<claim-sha256>:<issue-id-or-none>
```

The claim digest covers the canonical graph, complete plan, policy values, and parser/planner/guard schema contract. `graph_sha256` remains a graph-only diagnostic; `claim_sha256` is the token digest. Issue-row and dependency-array ordering do not change either canonical identity, while semantic graph, policy, schema, or planner-output changes invalidate the token.

### Claim receipt

`agent_claim.py` emits schema `fmn.agent.claim`, version `2`, only after a dry-run validation or a verified mutation. The receipt includes the guarded recommendation, exact token, pre-claim digests, planner policy, schema contracts, exact no-shell `br` argv, executor policy, and post-claim graph/status evidence when mutation occurred.

The executor policy records the requested per-command timeout and retained-byte ceiling. The CLI production path enforces that policy; injected runners exist only as a focused test seam. Receipt output is canonical compact JSON with one LF and a 1 MiB ceiling.

Executor exit `5` means no verified receipt. It does not guarantee no mutation: `br update` may have changed native state before a timeout, flush failure, or postcondition failure. Inspect tracker state before retrying and never reuse the old token blindly.

## Shared claim-lock contract

The executor resolves:

1. a real `.git` directory or linked-worktree `.git` marker;
2. the resolved Git directory's `commondir` marker, when present;
3. the shared common directory that owns `fmn-agent-claim.lock`.

Both marker files are bounded UTF-8 regular files and are opened without following symlinks where supported. The primary worktree and every linked worktree contend on one persistent lock inode.

The lock serializes only cooperating executor processes in the same clone. It does not cover another clone, direct manual `br`, an agent that ignores the executor, Agent Mail, file reservations, or unrelated changes to `main`. It is intentionally not described as a distributed lease.

## Command lifetime and output contract

Each production `br update` and `br sync --flush-only` invocation has an independent wall-clock timeout. The default is 60 seconds; callers may select a finite positive value up to 3,600 seconds with `--command-timeout-seconds`.

The child starts in its own POSIX session or Windows process group. On timeout:

- POSIX sends `SIGKILL` to the complete process group and waits under a finite cleanup bound;
- Windows attempts no-shell `taskkill.exe /T /F`, then directly kills the child if necessary;
- stdout and stderr readers are daemonized and joined under finite bounds;
- no success receipt is emitted.

If the direct child exits but a descendant keeps an inherited output pipe open, the executor forces cleanup and returns exit `5` instead of holding the repository claim lock indefinitely.

stdout and stderr are drained concurrently. Each stream retains at most `MAX_COMMAND_OUTPUT_BYTES + 1` bytes and discards later bytes while continuing to drain, so the retained-memory bound is eager and a full pipe cannot deadlock the child. The timeout bounds lifetime, but there is not yet a separate total-produced-byte ceiling; a child may generate discarded bytes until exit or timeout.

The timeout marks the command deadline, not a promise that the caller returns at that exact instant: bounded process-tree and reader cleanup may extend the failure path slightly.

## Deterministic human rendering

The generated brief's `as_of` is the newest issue `updated_at`, not wall-clock time. Its leaf-safe section is generated from the same planner record as the JSON interface.

```bash
python3 scripts/generate_agent_brief.py --stdout
python3 scripts/generate_agent_brief.py
python3 scripts/generate_agent_brief.py --check
```

Publication is fail-closed: integrity and activation checks precede output; existing and temporary symlinks are refused; exclusive temporary creation, flush, fsync, and atomic replacement are used; failed publication leaves the prior artifact untouched.

## Mutation protocol

The broad projection, planner, guard, and generator never mutate task state. Use the executor for the autonomous claim transition. Use tracker-native Beads commands for all other changes:

```bash
# Guarded claim path
python3 scripts/agent_claim.py \
    --expect-token "$token" \
    --issue "$issue" \
    --assignee "$FMN_AGENT_ID" \
    --command-timeout-seconds 60

git add .beads/
git commit -m "chore(beads): claim $issue"

# Other tracker mutations
br show <id>
br update <id> ...
br close <id> --reason "..."
br sync --flush-only
git add .beads/
git commit -m "chore(beads): ..."
```

Do not reconstruct or replace the large ledger from a truncated API response. If complete current bytes are unavailable through `br` or an exact local checkout, leave the tracker unchanged and state that limitation explicitly.

## Verification policy

`scripts/check.sh` compiles the parser, planner, guard, executor, generator, and focused regression files; runs the strict-parser, strict-JSON, planner, claim-token, claim-executor, real-process lifetime/output, deterministic-output, generator, and publication-I/O suites; validates the live plan and token; exercises the live executor in `--dry-run`; and renders the real ledger through `--stdout`. The same local gate then proceeds into Rust, Python, WASM, and structural checks.

The verification path never invokes a mutating executor call. It uses a synthetic assignee only in dry-run receipt evidence.

Hosted GitHub Actions is not part of this authority chain. The gate is intentionally runnable on local or owned build hosts, and unavailable hosted capacity is neither a waiver nor a product failure.

## Handoff

At session end, record:

- exact commits and paths;
- focused and repository-wide commands actually run;
- whether the checked source commit remained unchanged;
- remaining graph or evidence failures;
- the next concrete claimable leaf, if any;
- any tracker mutation that still needs `br sync --flush-only` or an explicit `.beads/` commit.

Never describe a partial gate, a different commit, an unrun platform, or a historical comment as current evidence.
