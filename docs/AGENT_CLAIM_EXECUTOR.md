# Guarded Beads claim execution

`scripts/agent_claim.py` is the mutation companion to `scripts/agent_claim_guard.py`. The guard produces a deterministic token for one recommended leaf. The executor keeps token revalidation, Beads' atomic claim compare-and-set, explicit JSONL export, and exact graph-delta verification inside one process while holding an advisory lock in Git's shared common directory.

The executor narrows local races; it is not a distributed lease. Current `main`, Agent Mail, file reservations, and agents operating in other clones still require inspection immediately before invocation.

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

On success, the command emits one canonical compact JSON receipt. It emits no success receipt unless the atomic Beads response, explicit export, and exact parsed-ledger postconditions all agree.

## Transaction shape

While holding the claim lock, the executor performs these steps in order:

1. Resolve a real `.git` directory or a bounded linked-worktree `.git` marker.
2. Resolve `commondir`, when present, to Git's shared common directory.
3. Non-blockingly lock the persistent `fmn-agent-claim.lock` inode in that common directory.
4. Read `.beads/issues.jsonl` through the strict bounded parser.
5. Rebuild the complete version-2 graph, plan, policy, and schema guard.
6. Match the supplied token and optional explicit issue to the live recommendation.
7. Require the selected row to remain unassigned and `open`.
8. Invoke Beads without a shell using the atomic claim primitive:

   ```text
   br update ISSUE --claim --actor ASSIGNEE --json [--transition-comment TEXT]
   ```

9. Require a strict UTF-8 JSON success response and no success-path stderr.
10. Require exactly one returned issue with the guarded ID, unchanged title and priority, status `in_progress`, the requested assignee, and a valid timestamp. Both Beads' ordinary array response and its `{updated, warnings}` capacity-warning envelope are accepted.
11. Run `br sync --flush-only` under the same bounded subprocess policy.
12. Re-read the exported JSONL and require one exact claim-only graph delta.
13. Require the Beads response timestamp to equal the exported row timestamp.
14. Emit the receipt and release the advisory lock.

The executor intentionally does **not** spell a claim as a generic `--status in_progress --assignee ...` update. Beads' `--claim` path owns the storage-level unassigned check and atomic assignee/status transition. The outer token and clone-local lock remain necessary because they protect stronger FrankenManim graph and policy semantics that Beads itself does not know.

The `.git` and `commondir` markers are read through no-follow bounded regular-file descriptors where supported. Malformed, non-UTF-8, oversized, symlinked, or non-directory targets fail before lock acquisition. The lock file is intentionally persistent: deleting and recreating a lock pathname could split contenders across different inodes.

## Two independent success proofs

A mutating version-5 receipt proves the claim twice.

### 1. Atomic Beads response

The executor parses `br update --claim --json` with duplicate-key and non-finite-number rejection. It refuses:

- empty, non-UTF-8, or malformed JSON;
- duplicate object keys or `NaN`/infinity constants;
- unexpected success-path stderr;
- missing, multiple, or non-object updated rows;
- a different issue ID, status, assignee, title, or priority;
- a missing or invalid `updated_at`;
- a malformed warning envelope.

A process exit of zero is therefore not sufficient evidence. Malformed structured output returns exit `5` before the explicit sync step. The native Beads store may already have changed, so recovery begins with inspection rather than token replay.

### 2. Exported graph delta

After `br sync --flush-only`, the executor compares the complete parsed graph before and after the claim. The only permitted transition is:

```text
selected issue:
  status      open -> in_progress
  assignee    null -> requested assignee
  updated_at  may advance, never regress
  comments    unchanged, or exactly one requested transition comment appended

all other selected-issue fields: unchanged
all other issues: unchanged
issue membership: unchanged
```

The receipt's `claim_delta` records that proof. Any additional target mutation, unrelated issue change, or membership change returns exit `5`. This detects concurrent writes from another clone or a direct manual `br` invocation that the local advisory lock cannot serialize.

The comparison deliberately uses the same canonical parsed graph as the guard rather than raw JSONL byte identity. Unknown extension fields outside that graph are not promoted into task-selection semantics.

## Command lifetime and output budgets

Every `br` command has two independent resource controls:

- `--command-timeout-seconds`: finite positive wall-clock deadline per command; default 60 seconds, maximum 3,600 seconds.
- `--command-output-budget-bytes`: combined stdout plus stderr production ceiling per command; default 16 MiB, maximum 1 GiB.

stdout and stderr are drained concurrently. At most 1 MiB per stream is retained for response parsing or diagnostics. Every produced chunk is also charged to the shared configured production budget before surplus retained bytes are discarded. Exact-bound output succeeds; the first byte beyond the total budget triggers bounded process-tree cleanup and exit `5`.

On POSIX, each child starts in a new session and timeout/budget cleanup kills its process group. On Windows, each child starts in a new process group; cleanup attempts no-shell `taskkill.exe /T /F` and retains direct-child fallback. Windows descendant cleanup is best effort, not a complete guarantee.

Reader threads are daemonized and joined under finite cleanup bounds. A direct child that exits while a descendant retains stdout or stderr cannot hold the claim lock indefinitely.

The timeout and output budget apply independently to `br update` and `br sync --flush-only`; they are not one transaction-wide budget. The production runner counts produced bytes while reading. An injected test runner can validate only the payload it returns.

## Receipt contract

`agent_claim.py` emits schema `fmn.agent.claim`, version `5`.

Every receipt records:

- mode (`claim` or `dry-run`);
- issue and requested assignee;
- exact guard token;
- pre-claim graph and claim digests;
- planner policy and schema contracts;
- executor policy, including `beads.update.claim/v1`, timeout, retained bytes per stream, and total produced-output budget;
- guarded recommendation evidence;
- exact no-shell command vectors.

A successful mutating receipt additionally records:

- normalized `atomic_claim` response evidence and warning count;
- post-export graph digest;
- exact `claim_delta` evidence.

Receipt JSON is canonical UTF-8, key-sorted, terminated by one LF, and capped at 1 MiB.

## Exit codes

| Exit | Meaning | Required response |
|---:|---|---|
| `0` | Dry-run validation succeeded, or atomic claim, export, and both proofs succeeded. | Consume the JSON receipt. |
| `1` | Blocking/containment integrity or activation state is unsafe. | Repair governance state; do not claim. |
| `2` | Arguments, paths, token, ledger, or resource-policy input are malformed. | Repair input; do not claim. |
| `3` | The guarded graph has no recommendation. | Stop or repair coordination/graph state. |
| `4` | The token or explicitly requested issue is stale. | Discard the token and repeat every external check. |
| `5` | Locking, atomic response validation, timeout, cleanup, output bounds, Beads, export, or exact-delta verification failed. | Inspect Beads and the working tree before any retry. |

Exit `5` means **no verified transaction receipt**, not necessarily **no mutation**. `br update --claim` may have committed before malformed output, timeout, export failure, or postcondition failure. Inspect:

```bash
br show "$issue"
br sync --status
git status
```

Never replay the old token blindly.

## Concurrency boundary

The persistent advisory lock serializes cooperating executor processes in the primary checkout and linked worktrees sharing one Git common directory. It does not serialize:

- another clone;
- a direct manual `br` command;
- an agent that ignores the executor;
- Agent Mail or file reservations;
- unrelated changes to `main`.

Beads' `--claim` adds a storage-level unassigned compare-and-set for the selected issue. The post-export graph comparison detects additional writes visible in the exported authority. Neither mechanism replaces cross-clone coordination.

## Strict ledger grammar

The shared Beads reader accepts strict JSON. Unquoted `NaN`, `Infinity`, and `-Infinity` are rejected wherever they occur, including ignored extension fields. Quoted spellings remain ordinary strings. The canonical claim-graph grammar is version `2`, so a token issued under the earlier permissive decoder cannot survive the grammar change.

## Verification

`scripts/test_agent_claim.py` covers atomic argv composition, version-5 receipts, ordinary and warning-envelope Beads responses, strict JSON response validation, response/export timestamp agreement, exact transition comments, graph and membership drift, stale tokens, failure ordering, linked-worktree locking, injected-runner resource bounds, and no-stdout failure behavior.

`scripts/test_agent_claim_subprocess.py` uses real children to cover exact and excessive retained output, configurable combined-output ceilings, prompt termination of continuously producing children, dual-stream draining, bounded diagnostics, timeout, POSIX descendant cancellation, inherited-pipe cleanup, and CLI resource-policy refusal.

`scripts/test_agent_brief_strict_json.py` covers the shared ledger grammar. `scripts/check.sh` runs all focused suites and exercises the live executor through `--dry-run`; the mandatory gate never mutates Beads.
