# Guarded Beads claim execution

`scripts/agent_claim.py` is the mutation companion to `scripts/agent_claim_guard.py`. The guard produces a deterministic token for one recommended leaf; the executor keeps token revalidation, the Beads mutation, the explicit JSONL flush, and the postcondition check inside one process while holding an advisory lock in Git's shared common directory.

The executor narrows the local compare-before-set race. It does not create a distributed lease and does not replace Agent Mail, file reservations, current-HEAD review, or communication with agents operating in other clones.

## Canonical workflow

```bash
# 1. Capture one exact graph-and-policy-bound recommendation.
token="$(python3 scripts/agent_claim_guard.py --require)"
issue="${token##*:}"

# 2. Inspect the selected task and all external coordination state.
br show "$issue"
# Check Agent Mail, reservations, active peers, and current main here.

# 3. Optionally inspect the exact mutation argv without changing Beads.
python3 scripts/agent_claim.py \
    --expect-token "$token" \
    --issue "$issue" \
    --assignee "$FMN_AGENT_ID" \
    --transition-comment "Claimed after graph, reservation, and HEAD checks" \
    --dry-run

# 4. Perform the guarded mutation.
python3 scripts/agent_claim.py \
    --expect-token "$token" \
    --issue "$issue" \
    --assignee "$FMN_AGENT_ID" \
    --transition-comment "Claimed after graph, reservation, and HEAD checks"
```

On success, the command emits one canonical compact JSON receipt. It emits no success receipt unless `br update`, `br sync --flush-only`, and the parsed-ledger postcondition all succeed.

## Transaction shape

While holding the claim lock, the executor performs these steps in order:

1. Resolve the repository's Git directory from either a real `.git` directory or a linked-worktree `.git` marker.
2. If the resolved Git directory contains `commondir`, resolve that bounded regular-file marker to Git's shared common directory.
3. Open and non-blockingly lock the persistent `fmn-agent-claim.lock` file in the common directory. The primary worktree and every linked worktree therefore contend on the same inode.
4. Read `.beads/issues.jsonl` through the strict bounded parser.
5. Rebuild the complete v2 guard with the caller's exact policy arguments.
6. Compare the supplied token's claim digest and issue subject with the live guard.
7. Verify the selected row remains an unassigned `open` issue.
8. Invoke `br` without a shell using exact argv:

   ```text
   br update ISSUE --status in_progress --assignee ASSIGNEE [--transition-comment TEXT]
   ```

9. Concurrently drain stdout and stderr while retaining at most the configured limit plus one detection byte for each stream.
10. Invoke `br sync --flush-only` under the same output policy.
11. Re-read the exported JSONL and require the selected row to be `in_progress` with the requested assignee.
12. Emit the receipt and release the advisory lock.

The `.git` and `commondir` markers are read through no-follow bounded regular-file descriptors where supported. Malformed, non-UTF-8, oversized, symlinked, or non-directory targets fail before lock acquisition.

The lock file is intentionally persistent. Deleting and recreating a lock pathname can split contenders across different inodes, so cleanup is neither required nor permitted by the normal workflow.

## Child-process output policy

The executor does not use `subprocess.run(..., stdout=PIPE, stderr=PIPE)`, because that interface retains complete child output before a later length check can reject it. Instead it starts one reader for each pipe, drains both concurrently, and stores no more than `MAX_COMMAND_OUTPUT_BYTES + 1` bytes per stream. Once overflow is known, the reader discards further bytes while continuing to drain the pipe so the child cannot deadlock on a full stdout or stderr buffer.

Exact-limit payloads remain available for diagnostics. A limit-plus-one payload causes exit `5` after the child terminates, and no success receipt is emitted. Both pipes are always drained concurrently, including when the child writes large stdout and stderr payloads at the same time.

This is a retained-output memory bound, not a child wall-clock or total-produced-byte limit. A malicious or hung `br` process remains outside the executor's trust model and requires separate process-lifecycle policy rather than pretending the diagnostic buffer is a timeout.

## Receipt contract

The version-1 receipt records:

- mode (`claim` or `dry-run`);
- issue and assignee;
- the exact guard token;
- pre-claim graph and claim digests;
- policy and schema contracts;
- the guarded recommendation evidence;
- the exact `br` argv vectors;
- post-claim status and graph digest after successful mutation.

Receipt JSON is canonical, UTF-8, sorted by key, terminated by one LF, and capped at 1 MiB. Child-process diagnostics are eagerly retained under a 1 MiB-per-stream ceiling rather than collected without bound and checked afterward.

## Exit codes

| Exit | Meaning | Required response |
|---:|---|---|
| `0` | Dry-run validation succeeded, or the claim was flushed and postcondition-verified. | Consume the JSON receipt. |
| `1` | Blocking/containment integrity or activation state is unsafe. | Repair governance state; do not claim. |
| `2` | Arguments, paths, token, ledger, or other input are malformed. | Repair input; do not claim. |
| `3` | The guarded graph has no recommendation. | Stop or repair coordination/graph state. |
| `4` | The token or explicitly requested issue is stale. | Discard the token and repeat all external checks. |
| `5` | Lock acquisition, `br`, flush, command-output bounds, or postcondition verification failed. | Inspect Beads before retrying; the update may have occurred even though no success receipt was emitted. |

A `br update` can durably mutate its native store before a later flush or verification fails. Exit `5` therefore means “no verified transaction receipt,” not necessarily “no state changed.” Inspect `br show ISSUE`, `br sync --status`, and the working tree before retrying. Never blindly reissue the old token.

## Concurrency boundary

The advisory lock serializes cooperating executor processes in the primary and linked worktrees that share one Git common directory. It does not serialize:

- direct manual `br` invocations;
- agents that ignore the executor;
- another clone with a different Git common directory;
- remote coordination or file reservations;
- changes to `main` outside Beads.

The guard comparison remains necessary inside the lock because a token can already be stale when lock acquisition succeeds. External coordination checks remain necessary immediately before invoking the executor.

## Strict ledger grammar

The shared Beads reader accepts strict JSON, not Python's historical JSON extension. Unquoted `NaN`, `Infinity`, and `-Infinity` are rejected at decode time wherever they occur, including ignored extension fields. Quoted spellings remain ordinary strings. The canonical claim-graph grammar is version `2`, so a token issued under the earlier permissive decoder cannot survive the grammar change.

## Verification

`scripts/test_agent_claim.py` covers dry-run argv, successful update/flush/postcondition flow, stale tokens, issue mismatch, update and sync failures, missing mutation, graph-integrity and no-work exits, normal and linked-worktree Git resolution, shared-common-directory contention, local contention, injected-runner output bounds, and no-stdout failure behavior.

`scripts/test_agent_claim_subprocess.py` uses real child processes to prove exact-limit stdout and stderr retention, independent stdout and stderr overflow, simultaneous large-stream draining without deadlock, and bounded diagnostics for a nonzero child exit.

`scripts/test_agent_brief_strict_json.py` covers all three non-finite spellings at top level and in nested ignored data, quoted-string acceptance, no-projection CLI failure, and claim-graph grammar versioning.

`scripts/check.sh` runs all three suites and, when the live ledger has a recommendation, invokes the executor in `--dry-run` mode against the freshly issued token. The mandatory gate never mutates Beads.
