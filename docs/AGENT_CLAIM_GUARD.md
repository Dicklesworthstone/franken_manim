# Graph-bound agent claim guards

`agent_next.py` deterministically selects the best currently claimable Beads leaf. Selection alone is not enough for concurrent agents: the graph can change after an agent reads the plan but before it executes `br update`. `scripts/agent_claim_guard.py` adds an explicit compare-before-set boundary around that interval.

The guard never mutates Beads and is not a lease. It proves only that the claim input and recommendation are still the same at the instant of revalidation. File reservations, current assignees, current `main`, and coordinating messages must still be checked immediately before the mutation.

For mutation, use `scripts/agent_claim.py`. It revalidates the guard, executes `br update`, flushes JSONL, and verifies the postcondition under one repository-local advisory lock. Its complete contract is in [`AGENT_CLAIM_EXECUTOR.md`](AGENT_CLAIM_EXECUTOR.md).

## Canonical workflow

```bash
# 1. Validate the graph and capture the exact guarded recommendation.
token="$(python3 scripts/agent_claim_guard.py --require)"
issue="${token##*:}"

# 2. Inspect the selected task and external coordination state.
br show "$issue"
# Check Agent Mail, file reservations, active peers, and current main here.

# 3. Optionally inspect the exact intended mutation without changing Beads.
python3 scripts/agent_claim.py \
    --expect-token "$token" \
    --issue "$issue" \
    --assignee "$FMN_AGENT_ID" \
    --dry-run

# 4. Revalidate, mutate, flush, and verify under one local lock.
python3 scripts/agent_claim.py \
    --expect-token "$token" \
    --issue "$issue" \
    --assignee "$FMN_AGENT_ID" \
    --transition-comment "Claimed after graph, reservation, and HEAD checks"
```

A successful guard comparison prints the current recommendation according to `--format`. Every nonzero guard result emits no stdout payload, so shell automation cannot accidentally consume a failed plan. The executor similarly emits a success receipt only after its complete transaction shape succeeds.

## Token contract

Version 2 tokens have this form:

```text
v2:<64-lowercase-hex-claim-digest>:<issue-id-or-none>
```

The claim digest covers:

- the strict `agent_brief` snapshot schema version;
- the `agent_next` schema name and version;
- the claim-guard, claim-input, and canonical-graph schema names and versions;
- normalized `as_of`, stale-day policy, activation cap, and queue limit;
- every parsed issue's identity, title, status, priority, type, assignee, and timestamp;
- every dependency edge, canonically ordered;
- every parsed comment, in ledger order;
- the complete canonical `fmn.agent.next` plan, including integrity, activation, queue evidence, and recommendation;
- the selected recommendation identity, redundantly encoded as the token subject for shell-safe extraction.

Issue-record order and dependency-array order do not affect the digest. Semantic changes do. The literal issue ID `none` is reserved because `none` denotes a valid graph with no claimable recommendation.

The canonical claim-graph grammar is version `2`. It uses strict JSON decoding: unquoted `NaN`, `Infinity`, and `-Infinity` are forbidden anywhere in an issue row, including ignored extension fields. Tokens issued under the earlier permissive grammar are invalidated by the schema contract.

JSON output exposes two intentionally different hashes:

- `graph_sha256` identifies only the canonical parsed Beads graph and is useful for graph-level diagnostics;
- `claim_sha256` identifies the complete graph, policy, schema, and planner input and is the digest carried by the v2 token.

A graph can therefore keep the same `graph_sha256` while a policy or planner-schema change correctly produces a different `claim_sha256` and invalidates the old token.

## Guard exit codes

| Exit | Meaning | Safe automation response |
|---:|---|---|
| `0` | Current claim input matches, and any requested recommendation exists. | Continue immediately to the checked mutation. |
| `1` | Blocking/containment integrity or activation state is unsafe. | Repair governance state; do not claim. |
| `2` | Ledger, arguments, token, canonicalization, or output contract is malformed. | Repair the input or invocation; do not claim. |
| `3` | The graph is valid but `--require` found no recommendation. | Stop or work on coordination/graph repair. |
| `4` | The supplied token is stale. | Discard it, refresh the plan, and repeat all coordination checks. |

Integrity and activation failures take precedence over token parsing. A malformed token therefore cannot obscure a newly unsafe graph.

## Determinism and boundedness

The default `as_of` comes from the newest ledger timestamp through `agent_next`, so identical ledger bytes and policy produce identical tokens. An explicit `--as-of` becomes part of the digest.

The command inherits the bounded, descriptor-safe Beads parser and additionally caps its emitted report at 1 MiB. JSON output is canonical compact UTF-8 with sorted keys and one terminal LF.

```bash
python3 scripts/agent_claim_guard.py --format json
python3 scripts/agent_claim_guard.py --format token --require
python3 scripts/agent_claim_guard.py --format id --expect-token "$token" --require
```

## What the guard does not prove

The guard alone cannot make `br update` atomic with the preceding read. The executor narrows that local interval by holding one Git-directory lock across revalidation and mutation, but neither mechanism is a distributed lease. Another clone, a direct manual `br` invocation, or external coordination can still conflict.

Use Agent Mail reservations, inspect current `main`, and keep the interval between those checks and executor invocation minimal. A failed mutation is a fresh conflict; never override it or blindly reuse the old token.

The guard itself does not edit, close, assign, export, or commit issues. The executor performs only the guarded `open` → `in_progress` claim path through `br`, followed by `br sync --flush-only` and parsed-ledger verification. Committing the resulting `.beads/` export remains an explicit repository action.

## Verification

`scripts/test_agent_claim_guard.py` covers v2 token round trips, graph and recommendation changes, canonical order independence, every policy input, schema-version changes, unchanged-recommendation planner drift, the reserved sentinel, no-work behavior, malformed and legacy tokens, output bounds, and integrity precedence.

`scripts/test_agent_claim.py` covers the locked mutation path and failure semantics. `scripts/test_agent_brief_strict_json.py` covers the strict JSON grammar and claim-graph version. `scripts/check.sh` compiles and runs all three suites, issues a token against the complete live ledger, immediately revalidates it, and exercises the executor's live-ledger dry-run path before entering the Rust gates.
