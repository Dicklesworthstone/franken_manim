# Guarded Beads claim execution

`scripts/agent_claim.py` is the mutation companion to `scripts/agent_claim_guard.py`. The guard produces a deterministic token for one planner-approved leaf. The executor keeps token revalidation, Beads' storage-level atomic claim, structured-response validation, explicit JSONL export, and post-export delta verification inside one process while holding an advisory lock in Git's shared common directory.

The executor narrows local races. It is not a distributed lease. Current `main`, Agent Mail, file reservations, direct Beads activity, and agents operating in other clones still require inspection immediately before invocation.

## Canonical workflow

```bash
python3 scripts/agent_brief.py --format json --check >/dev/null
python3 scripts/agent_next.py --format json --check

token="$(python3 scripts/agent_claim_guard.py --require)"
issue="${token##*:}"

br show "$issue"
# Check current main, Agent Mail, reservations, and active peers.

# Optional nonmutating composition check.
python3 scripts/agent_claim.py \
    --expect-token "$token" \
    --issue "$issue" \
    --assignee "$FMN_AGENT_ID" \
    --command-timeout-seconds 60 \
    --command-output-budget-bytes 16777216 \
    --transition-comment "Claimed after graph, reservation, and HEAD checks" \
    --dry-run

# Atomic guarded claim.
python3 scripts/agent_claim.py \
    --expect-token "$token" \
    --issue "$issue" \
    --assignee "$FMN_AGENT_ID" \
    --command-timeout-seconds 60 \
    --command-output-budget-bytes 16777216 \
    --transition-comment "Claimed after graph, reservation, and HEAD checks"
```

On success, the command emits one canonical compact JSON receipt. It emits no success receipt unless the planner-v4 scope contract, token, Beads atomic response, explicit export, and post-export delta all agree.

## Transaction shape

While holding the claim lock, the executor performs these steps in order:

1. Resolve a real `.git` directory or a bounded linked-worktree `.git` marker.
2. Resolve `commondir`, when present, to Git's shared common directory.
3. Non-blockingly lock the persistent `fmn-agent-claim.lock` inode in that common directory.
4. Read `.beads/issues.jsonl` through the shared bounded parser.
5. Rebuild the version-2 guard, including `fmn.agent.next` version 4.
6. Match the supplied token and optional explicit issue to the live recommendation.
7. Require the selected row to remain unassigned, `open`, and scoped to `G0` or `W1`–`W11`.
8. Invoke Beads without a shell using its atomic claim primitive:

   ```text
   br update ISSUE --claim --actor ASSIGNEE --json [--transition-comment TEXT]
   ```

9. Require a strict bounded UTF-8 JSON success response and no success-path stderr.
10. Require exactly one returned issue with the guarded ID, unchanged title and priority, status `in_progress`, the requested assignee, and a valid timestamp.
11. Run `br sync --flush-only` under the same bounded subprocess policy.
12. Re-read the exported JSONL and require the permitted claim delta.
13. Require the Beads response timestamp to equal the exported row timestamp.
14. Emit the receipt and release the advisory lock.

The executor intentionally does **not** express an autonomous claim as a generic `--status in_progress --assignee ...` update. Beads' `--claim` path owns the storage-level unassigned compare-and-set. The outer token and clone-local lock protect the stronger FrankenManim planner and coordination contract that Beads does not know.

## Scoped recommendation prerequisite

The executor cannot claim an issue that the planner classifies as `UNSCOPED`.

The exact governed prefixes are:

```text
G0
W1 through W11
```

Open unscoped leaves remain diagnostic only. An active unscoped issue invalidates the planner and guard before process execution. `G0` counts toward the four-workstream cap.

## Two independent success proofs

A mutating version-6 receipt proves the claim twice.

### 1. Atomic Beads response

The executor parses `br update --claim --json` with:

- UTF-8 validation;
- duplicate-key rejection;
- `NaN` and infinity rejection;
- maximum JSON depth 64;
- maximum decoded node count 100,000;
- a bounded command-output budget;
- no success-path stderr.

It refuses empty or malformed JSON, malformed warning envelopes, missing or multiple updated rows, and any mismatch in guarded ID, title, priority, status, assignee, or timestamp. Both Beads' ordinary array response and its `{updated, warnings}` capacity-warning envelope are accepted.

A process exit of zero is not sufficient evidence. Malformed structured output returns exit `5` before explicit export. The native Beads store may already have changed, so recovery begins with inspection rather than token replay.

### 2. Exported planning-graph delta

After `br sync --flush-only`, the executor compares the canonical parsed planning graph before and after the claim. The only permitted transition in that field set is:

```text
selected issue:
  status      open -> in_progress
  assignee    null -> requested assignee
  updated_at  may advance, never regress
  comments    unchanged, or exactly one requested transition comment appended

all other represented selected-issue fields: unchanged
all other represented issues: unchanged
represented issue membership: unchanged
```

The receipt's `claim_delta` records that proof. Additional represented target changes, unrelated represented issue changes, or represented membership changes return exit `5`.

## Canonical planning-graph boundary

The word “exact” above applies to the field set represented by `agent_brief.Issue` and bound by `agent_claim_guard.canonical_graph`:

- ID;
- title;
- status;
- priority;
- issue type;
- assignee;
- normalized update timestamp;
- dependencies;
- comments.

Description, design, acceptance criteria, notes, owner, estimates, due/defer values, labels, and unknown extension fields are not yet included. A version-6 receipt must not be described as raw JSONL identity or a whole-Beads-record transaction proof.

This boundary is why `br show "$issue"` remains mandatory immediately before executor invocation. Expanding the canonical graph to all task-semantic fields is a valid future hardening tranche.

## Command lifetime and output budgets

Every Beads child has independent resource controls:

- `--command-timeout-seconds`: finite positive deadline per command; default 60 seconds, maximum 3,600 seconds.
- `--command-output-budget-bytes`: combined stdout plus stderr production ceiling per command; default 16 MiB, maximum 1 GiB.
- retained diagnostics: at most 1 MiB per stream.

stdout and stderr are drained concurrently. Every produced chunk is charged to the shared configured budget before retained surplus is discarded. Exact-bound output succeeds; the first byte beyond the total budget triggers bounded process-tree cleanup and exit `5`.

On POSIX, each child starts in a new session and timeout/budget cleanup kills its process group. On Windows, each child starts in a new process group; cleanup attempts no-shell `taskkill.exe /T /F` and retains direct-child fallback. Windows descendant cleanup is best effort.

Reader threads are daemonized and joined under finite cleanup bounds. A direct child that exits while a descendant retains stdout or stderr cannot hold the claim lock indefinitely.

The timeout and output budget apply independently to `br update --claim` and `br sync --flush-only`; they are not one transaction-wide budget.

## Receipt contract

`agent_claim.py` emits schema `fmn.agent.claim`, version 6.

Every receipt records:

- mode (`claim` or `dry-run`);
- issue and requested assignee;
- exact guard token;
- pre-claim graph and claim digests;
- planner policy and schema contracts;
- executor policy, including `beads.update.claim/v1`, timeout, retained bytes, total produced-output budget, and atomic-response structural bounds;
- guarded recommendation evidence;
- exact no-shell command vectors.

A successful mutating receipt additionally records normalized atomic-response evidence, the post-export graph digest, and the represented-field `claim_delta` proof.

Receipt JSON is canonical UTF-8, key-sorted, terminated by one LF, and capped at 1 MiB.

## Exit codes

| Exit | Meaning | Required response |
|---:|---|---|
| `0` | Dry-run validation succeeded, or atomic claim, export, and both proofs succeeded. | Consume the JSON receipt. |
| `1` | Blocking, containment, unscoped-active, or activation state is unsafe. | Repair governance state; do not claim. |
| `2` | Arguments, paths, token, ledger, or resource-policy input are malformed. | Repair input; do not claim. |
| `3` | The guarded graph has no governed recommendation. | Stop or repair task scope/coordination. |
| `4` | The token or explicitly requested issue is stale. | Discard the token and repeat every external check. |
| `5` | Locking, atomic response validation, timeout, cleanup, output bounds, Beads, export, or delta verification failed. | Inspect Beads and the working tree before any retry. |

Exit `5` means **no verified transaction receipt**, not necessarily **no mutation**. `br update --claim` may have committed before malformed output, timeout, export failure, or postcondition failure. Inspect:

```bash
br show "$issue"
br sync --status
git status
```

Never replay the old token blindly.

## Concurrency boundary

The persistent advisory lock serializes cooperating executor processes in the primary checkout and linked worktrees sharing one Git common directory. It does not serialize another clone, direct manual Beads commands, Agent Mail, reservations, or unrelated changes to `main`.

Beads' `--claim` adds a storage-level unassigned compare-and-set for the selected issue. The post-export planning-graph comparison detects additional represented-field writes visible in exported authority. Neither mechanism replaces cross-clone coordination.

## Verification

`scripts/test_agent_claim.py` covers atomic argv composition, version-6 receipts, ordinary and warning-envelope responses, strict response parsing, response/export timestamp agreement, transition comments, represented graph and membership drift, stale tokens, failure ordering, linked-worktree locking, injected-runner resource bounds, and no-stdout failure behavior.

`scripts/test_agent_claim_subprocess.py` uses real children to cover retained and produced-output ceilings, continuously producing children, dual-stream draining, diagnostics, timeout, POSIX descendant cancellation, inherited-pipe cleanup, and CLI resource-policy refusal.

The planner, claim-guard, and human-brief suites additionally prove that unscoped work cannot reach this executor as a valid recommendation. `scripts/check.sh` runs all focused suites and exercises the live executor only through `--dry-run`; the mandatory gate never mutates Beads.
