# Agent control plane

This document defines the operational layer that lets an agent reconstruct current work, select one safe leaf, bind that choice to exact graph and policy state, and claim it through Beads' atomic compare-and-set without loading the complete ledger into model context or creating a second task database.

## Authority hierarchy

1. **`.beads/issues.jsonl`** is the authoritative exported task graph after `br sync --flush-only`.
2. **`scripts/agent_brief.py`** is the bounded, read-only strict-JSON parser and situational projection.
3. **`scripts/agent_next.py`** is the fail-closed leaf planner. It alone decides autonomous claim eligibility.
4. **`scripts/agent_claim_guard.py`** binds the canonical graph, complete plan, policy, and schema contracts into one version-2 claim token.
5. **`scripts/agent_claim.py`** revalidates that token, invokes Beads' atomic `update --claim`, validates its structured response, explicitly exports JSONL, and proves the exact post-export graph delta.
6. **`scripts/generate_agent_brief.py`** renders the broad situational projection and exact leaf plan as deterministic Markdown.
7. A generated `docs/AGENT_BRIEF.md`, when deliberately committed, is only a cache of one exact ledger state. It never outranks Beads.

Source authority, semantic authority, task authority, and evidence remain separate:

- source code says what behavior exists;
- API schema and overlay say what compatibility status is claimed;
- Beads says what work is open or blocked;
- gates and retained artifacts say what was actually exercised.

## Safe session start and claim

```bash
python3 scripts/agent_brief.py --format json --check >/dev/null
python3 scripts/agent_next.py --format json --check

token="$(python3 scripts/agent_claim_guard.py --require)"
issue="${token##*:}"

br show "$issue"
# Check current main, Agent Mail, file reservations, and active peers.

python3 scripts/agent_claim.py \
    --expect-token "$token" \
    --issue "$issue" \
    --assignee "$FMN_AGENT_ID" \
    --command-timeout-seconds 60 \
    --command-output-budget-bytes 16777216 \
    --transition-comment "Claimed after graph, reservation, and HEAD checks" \
    --dry-run

python3 scripts/agent_claim.py \
    --expect-token "$token" \
    --issue "$issue" \
    --assignee "$FMN_AGENT_ID" \
    --command-timeout-seconds 60 \
    --command-output-budget-bytes 16777216 \
    --transition-comment "Claimed after graph, reservation, and HEAD checks"
```

`agent_next.py` emits one leaf ID or `none`. Exit `1` means integrity or activation state is unsafe, exit `2` means malformed input or a bounded-output refusal, and exit `3` means the graph is valid but has no claimable leaf. Every nonzero planner exit emits no stdout payload.

`agent_brief.py --format next` has been retired because a broad dependency-ready queue cannot prove leafhood. It exits `2` before reading the ledger and directs callers to `agent_next.py`.

A recommendation and token are not a lease. The executor serializes cooperating worktrees in one clone; another clone and external coordination state remain outside that lock.

## Claimability rules

An issue is recommendation-eligible only when all of the following hold:

- status is `open`;
- every `blocks` target exists and is closed or tombstoned;
- it has no assignee;
- it is not an epic;
- it has no live issue whose `parent-child` edge points to it;
- neither the live blocking graph nor live containment graph has a cycle;
- claiming it does not violate the four-workstream activation cap.

A task, bug, or feature with live children is a container regardless of its type label.

Recommendation order is deterministic:

1. leaves in an already-active workstream;
2. lower numeric priority;
3. more dependents for which completion removes the sole unresolved blocker;
4. more direct blocked dependents;
5. scoped work before `UNSCOPED` work;
6. most recently updated;
7. lexical issue ID.

## Ledger parsing contract

The parser opens the ledger through a descriptor-bound regular-file check and uses `O_NOFOLLOW` where available. It refuses:

- malformed UTF-8 JSONL, blank records, missing final LF, and duplicate issue IDs;
- duplicate JSON keys at any depth;
- unquoted `NaN`, `Infinity`, and `-Infinity` at any depth;
- unknown statuses;
- invalid priorities, required strings, or timestamps;
- falsey non-array spellings for `dependencies` or `comments`;
- malformed comments or dependency rows;
- dependency ownership errors, self-edges, and duplicate edges;
- per-line, ledger, issue, dependency, and comment budget violations;
- non-regular ledger inputs.

Explicit `null` remains the canonical empty spelling for optional arrays. Quoted non-finite spellings remain ordinary strings. Invalid present data is never silently treated as absent.

The canonical claim-graph grammar is version `2`, and that version participates in the claim digest.

## Graph-integrity contract

Missing non-blocking links remain visible diagnostics but do not suppress unrelated work. A missing blocking target, live blocking SCC, or live containment SCC makes the graph non-claimable.

Both cycle analyses are iterative. Deep valid graphs cannot fail merely because they exceed Python's call-stack depth.

The brief generator and claim guard consume the planner's exact integrity record. Integrity and activation failures are decided before publication, token output, or mutation.

## Machine contracts

### Leaf plan

`agent_next.py --format json` emits schema `fmn.agent.next`, version `2`. The compact canonical record includes integrity, activation, recommendation, containment, unblock pressure, and bounded ready/container/assigned queues.

The default `as_of` is the newest ledger timestamp. Identical ledger bytes therefore produce identical output without a wall clock. The payload is capped at 4 MiB.

### Claim token

`agent_claim_guard.py` emits schema `fmn.agent.claim-guard`, version `2`, and tokens of the form:

```text
v2:<claim-sha256>:<issue-id-or-none>
```

`graph_sha256` identifies only the canonical graph. `claim_sha256` additionally binds the complete plan, selection policy, and parser/planner/guard schema contract. Any semantic graph, plan, policy, or schema change invalidates the token.

### Atomic claim receipt

`agent_claim.py` emits schema `fmn.agent.claim`, version `5`.

The executor invokes:

```text
br update ISSUE --claim --actor ASSIGNEE --json [--transition-comment TEXT]
br sync --flush-only
```

The first command is Beads' storage-level unassigned compare-and-set. A generic `--status/--assignee` update is not the autonomous claim path.

Every receipt includes the token, pre-claim digests, planner policy, schema contracts, exact argv, and executor policy. A successful mutating receipt also includes:

- `atomic_claim`: normalized evidence from strict parsing of the successful Beads JSON response;
- `after_graph_sha256`: exported graph identity;
- `claim_delta`: proof that the complete parsed graph changed only through the intended claim transition.

The atomic response must report exactly one guarded issue, status `in_progress`, the requested assignee, unchanged title and priority, a valid timestamp, and no success-path stderr. Ordinary array output and `{updated, warnings}` envelopes are accepted. The response timestamp must equal the exported row timestamp.

The exact post-export delta requires unchanged issue membership, unchanged non-target issues, unchanged target identity/title/priority/type/dependencies, and only the expected status, assignee, non-regressing timestamp, and optional exact appended transition comment.

Receipt output is canonical UTF-8 JSON with sorted keys, one final LF, and a 1 MiB ceiling.

## Shared claim-lock contract

The executor resolves a real `.git` directory or linked-worktree marker, then `commondir` when present, and locks persistent `fmn-agent-claim.lock` in the shared common directory. Marker files are bounded UTF-8 regular files and are opened without following symlinks where supported.

The primary checkout and sibling linked worktrees therefore contend on one inode. The lock does not cover another clone, direct manual `br`, an agent that ignores the executor, Agent Mail, reservations, or unrelated `main` changes.

## Command lifetime and output contract

Each `br` command has independent controls:

- `--command-timeout-seconds`: default 60 seconds, finite positive maximum 3,600;
- `--command-output-budget-bytes`: default 16 MiB combined stdout/stderr, positive integer maximum 1 GiB;
- retained diagnostics: at most 1 MiB per stream.

stdout and stderr are drained concurrently. Every produced chunk is charged to the shared configured budget before retained surplus is discarded. Exact-bound output succeeds; the first byte above the total limit triggers bounded process-tree cleanup and exit `5`.

POSIX children run in a new session and cleanup targets the process group. Windows children run in a new process group and use best-effort `taskkill.exe /T /F` plus direct-child fallback. Reader joins and process waits are bounded. A descendant retaining inherited pipes cannot strand the shared claim lock indefinitely.

The timeout and output budget are per command, not transaction-wide.

## Failure semantics

Executor exit `5` means **no verified receipt**, not necessarily **no mutation**. `br update --claim` may commit before malformed output, timeout, export failure, or postcondition failure. Recovery starts with:

```bash
br show "$issue"
br sync --status
git status
```

Never replay the old token blindly.

Other exits:

- `1`: graph or activation refusal;
- `2`: malformed input or resource policy;
- `3`: no recommendation;
- `4`: stale token or issue mismatch.

Every failure emits no success receipt on stdout.

## Deterministic human rendering

```bash
python3 scripts/generate_agent_brief.py --stdout
python3 scripts/generate_agent_brief.py
python3 scripts/generate_agent_brief.py --check
```

The brief uses ledger time, refuses unsafe graphs before output, rejects symlink publication targets, and publishes through exclusive temporary creation, flush, fsync, and atomic replacement.

## Mutation protocol

The brief, planner, guard, and generator are read-only. Use the atomic executor for autonomous claim transitions and tracker-native Beads commands for every other mutation:

```bash
br show <id>
br update <id> ...
br close <id> --reason "..."
br sync --flush-only
git add .beads/
git commit -m "chore(beads): ..."
```

Never reconstruct the large ledger from truncated API output. If a tracker-native `br` execution environment is unavailable, leave `.beads/issues.jsonl` unchanged and record that limitation.

## Verification policy

`scripts/check.sh` compiles and runs the strict parser, planner, claim-token, atomic executor, real-process lifetime/output, generator, publication-I/O, portal-refusal, and namespace suites before the Rust, Python, WASM, and structural checks. It exercises the live claim composition only through `--dry-run`; the gate never mutates Beads.

Hosted GitHub Actions is not part of the authority chain. Correctness evidence comes from the exact local or owned-host gate and the named commit it exercised.

## Handoff

Record exact commits, changed paths, commands actually run, source commit identity, remaining evidence gaps, current graph state, and any `.beads/` export still requiring a commit. Never describe an unrun platform or a different commit as current evidence.
