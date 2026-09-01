# Graph-bound agent claim guards

`agent_next.py` deterministically selects the best currently claimable Beads leaf. Selection alone is not enough for concurrent agents: the graph can change after an agent reads the plan but before it executes `br update`. `scripts/agent_claim_guard.py` adds an explicit compare-before-set boundary around that interval.

The guard never mutates Beads and is not a lease. It proves only that the claim input and recommendation are still the same at the instant of revalidation. File reservations, current assignees, current `main`, and coordinating messages must still be checked immediately before the mutation.

## Canonical workflow

```bash
# 1. Validate the graph and capture the exact guarded recommendation.
token="$(python3 scripts/agent_claim_guard.py --require)"
issue="${token##*:}"

# 2. Inspect the selected task and external coordination state.
br show "$issue"
# Check Agent Mail and file reservations here.

# 3. Immediately before mutation, compare the current graph and policy to the token.
python3 scripts/agent_claim_guard.py \
    --expect-token "$token" \
    --require \
    --format id

# 4. Only an exit-0 revalidation authorizes the ordinary Beads mutation.
br update "$issue" --status=in_progress
br sync --flush-only
```

A successful comparison prints the current recommendation according to `--format`. Every nonzero result emits no stdout payload, so shell automation cannot accidentally consume a failed plan.

## Token contract

Version 2 tokens have this form:

```text
v2:<64-lowercase-hex-claim-digest>:<issue-id-or-none>
```

The claim digest covers:

- the strict `agent_brief` snapshot schema version;
- the `agent_next` schema name and version;
- the claim-guard schema name and version;
- normalized `as_of`, stale-day policy, activation cap, and queue limit;
- every parsed issue's identity, title, status, priority, type, assignee, and timestamp;
- every dependency edge, canonically ordered;
- every parsed comment, in ledger order;
- the selected recommendation identity, encoded separately in the token.

Issue-record order and dependency-array order do not affect the digest. Semantic changes do. The literal issue ID `none` is reserved because `none` denotes a valid graph with no claimable recommendation.

## Exit codes

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

The guard cannot make `br update` atomic with the preceding read. Another actor can still change the ledger or coordination state in the remaining interval. Keep that interval minimal, use Agent Mail reservations, and treat a failed subsequent `br` mutation as a fresh conflict rather than overriding it.

The guard also does not edit, close, assign, export, or commit issues. All tracker changes continue through `br`, followed by `br sync --flush-only` and an explicit `.beads/` commit.

## Verification

`scripts/test_agent_claim_guard.py` covers token round trips, comment and recommendation changes, canonical order independence, policy binding, the reserved sentinel, no-work behavior, malformed tokens, output bounds, and integrity precedence. `scripts/check.sh` runs that suite and renders a guard against the complete live ledger before entering the Rust gates.
