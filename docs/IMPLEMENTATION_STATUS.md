# FrankenManim implementation status

**Status date:** 2026-09-01 UTC / America/New_York  
**Evidence checkpoint:** substantive source commits through `5629a41`; documentation reconciled through the current status tranche.  
**Authority rule:** this document summarizes evidence; `.beads/issues.jsonl` remains the task and dependency authority.

## How to read this document

FrankenManim is pre-1.0. A capability is listed as implemented only when a concrete source surface and a checkable test or artifact boundary exist. Aspirational plan text, a reviewed Parity Ledger row, or an old Beads comment is not by itself implementation evidence. Hardware- and release-specific claims remain separate from local semantic correctness.

## Product surfaces

| Surface | Current implementation state | Evidence boundary |
|---|---|---|
| Native Rust library | The composition root, scene runtime, mobject/animation stack, native text and math, retained renderer, codecs, output pipeline, and built-in scene corpus are implemented as workspace crates. | Ordinary workspace checks and tests run through `scripts/check.sh`; platform certification remains a separate matrix. |
| `fmn` CLI | Typed render, doctor, batch, and Studio command surfaces exist. The shipped binary has executable smoke coverage for version/help, doctor robot and quiet behavior, typed exit codes, a real one-scene batch render, native artifact publication, and machine/human manifests. | `cargo test -p fmn-cli --features batch --test cli_smoke` exercises the complete shipping feature shape. The process-output helper is additionally exercised as a binary unit module by the all-target Cargo gate. |
| Studio | Supervisor/worker, replay, event, inspection, and presentation foundations are implemented. | Platform and real-browser presentation evidence is not inferred from local Rust tests. |
| `fmn-python` portal | A separately installed CPython 3.13 wheel exposes the pinned `manimlib` namespace with native and authored compatibility behavior. Every explicit bootstrap refusal is mechanically inventoried and must be named. | The W10 semantic-surface parent remains open. A named refusal is evidence of an honest missing capability, not evidence that the capability is implemented. |
| WASM/browser | The wasm32 render/player foundations and package gate exist. | The npm/real-browser release gate is opt-in (`FMN_WASM_PACKAGE_GATE=1`) and is not claimed merely because ordinary local checks pass. |
| Distribution | Tagged `v0.1.0` through `v0.4.0` releases are prereleases. | Cross-platform artifacts, hardware-specific execution, and release matrices require their own receipts. |

## Recently completed implementation tranche

### Graph-and-policy-bound autonomous claim revalidation

The operational control plane now has a fourth linked layer: `scripts/agent_claim_guard.py` version 2. `agent_next.py` still decides which dependency-ready unassigned leaf is best; the guard binds that decision to the exact state and semantics used to produce it before an agent mutates Beads.

A v2 token has the form:

```text
v2:<claim-sha256>:<issue-id-or-none>
```

The claim digest commits to:

- the complete canonical parsed issue graph, dependency edges, and comments;
- the complete canonical `fmn.agent.next` plan, not merely the selected issue;
- normalized `as_of`, stale-day policy, activation cap, and queue limit;
- the snapshot, planner, graph, claim-input, guard, and token schema contracts.

The JSON report exposes `graph_sha256` separately from `claim_sha256`. The former is graph-only diagnostic identity. The latter is the complete claim input and is the digest carried by the token. A policy or planner-schema change can therefore invalidate a claim without pretending the graph itself changed.

The literal issue ID `none` is reserved because `none` is the valid token subject for a graph with no recommendation. Issue-row order and dependency-array order remain semantically irrelevant; graph, comment, policy, schema, and planner-output changes are not.

The focused suite now covers:

- v2 issue/revalidate round trips;
- graph, comment, and recommendation changes;
- canonical issue/dependency order independence;
- explicit `as_of`, stale-day, activation-cap, and limit drift;
- brief, planner, and guard schema drift;
- full planner-output drift that leaves the recommendation unchanged;
- the reserved sentinel and no-work exit;
- malformed, legacy, and noncanonical tokens;
- bounded report publication;
- integrity precedence over token parsing.

`scripts/check.sh` compiles and runs the suite, issues a token against the complete live ledger, and immediately revalidates that exact token before entering the Rust gates. The guard remains a compare-before-set boundary, not a lease or an atomic `br` transaction. Current `main`, Agent Mail, file reservations, and the selected issue still require inspection immediately before `br update`.

### Python portal refusal truthfulness

`scripts/audit_portal_refusals.py` turns the remaining fail-closed Python compatibility surface into a canonical bounded inventory. The current schema is `fmn.portal.refusals` version `2`.

The audit:

- binds opening to regular-file identity with `lstat`/`fstat` checks and no-follow opening where supported;
- caps source bytes, AST nodes, AST depth, refusal sites, and rendered report bytes;
- traverses the AST iteratively rather than depending on Python recursion depth;
- inventories direct `NotImplementedError` raises and `_refuse_unrouted` calls with source location, qualified scope, subject, detail expression, abstract status, and source SHA-256;
- permits a bare `NotImplementedError` only in the exact function decorated as `abstractmethod`;
- resets abstract-method permission across nested functions, classes, and lambdas;
- rejects bare concrete raises, zero-argument or blank-message errors, blank f-string messages, missing or blank subjects, missing or statically empty entry collections, and ambiguous helper calls involving duplicate arguments, extra positional arguments, `*args`, or `**kwargs`;
- emits canonical compact JSON or deterministic Markdown;
- produces no stdout report when `--check` finds an anonymous refusal.

This is intentionally not a refusal-count completion claim. The inventory makes each remaining W10 gap addressable and prevents a new anonymous placeholder from entering the mandatory gate. Actual reductions still require implementing the owning native seam or recording a deliberate tier/exclusion with its evidence.

### Process-output lifecycle

The binary entry point no longer treats stdout as a prerequisite for stderr publication.

- stdout and stderr are attempted independently and exactly once;
- a closed downstream pipe (`BrokenPipe`) is ordinary CLI lifecycle and does not replace the command's already-decided typed exit code;
- a stdout failure cannot prevent a typed stderr diagnostic from being attempted;
- non-broken-pipe output failures still map to the internal exit class;
- simultaneous non-broken failures have deterministic stdout-first precedence;
- empty streams perform no writes.

The policy lives in `crates/fmn-cli/src/process_output.rs`, with deterministic writer regressions, and `main.rs` retains the semantic command code whenever publication succeeds or only encounters closed pipes.

One prior status statement remains explicitly corrected: human `doctor --quiet` suppression still occurs in the binary entry point after argument classification. The library dispatcher continues to construct the complete human snapshot. Embedded callers therefore do not yet share a central quiet-output policy.

### Executable CLI boundaries

- Batch robot output includes a terminal per-job success record rather than requiring consumers to infer completion.
- A real shipped-binary smoke renders a tiny synthetic FMTL scene and verifies the native PNG plus `manifest.fmnp` and `manifest.txt` publication.
- Typed usage and capability exits, doctor robot schema, and quiet-mode error visibility remain covered by the shipping-feature smoke.

### Python/Rust API boundary

Rust-native ergonomic helpers such as `small_dot`, `rounded_rectangle`, and `v_highlight` are not Python wildcard exports unless the pinned Reference exports those exact names. The policy maps sixteen Rust helpers to their Reference CamelCase classes and verifies three surfaces agree:

1. the extracted API schema contains the exported Reference class;
2. the wheel/package wrapper rejects a leaked snake_case helper or missing class;
3. the clean-wheel smoke constructs the complete class family.

This preserves source compatibility rather than inventing a second Python API.

### Agent control plane

The operational layer now has four linked executable abstractions plus one human projection:

1. `scripts/agent_brief.py` owns bounded untrusted-ledger parsing, blocking-graph integrity, activation state, and broad situational rendering. It has no autonomous claim output.
2. `scripts/agent_next.py` owns autonomous leaf selection and emits schema `fmn.agent.next` version `2`.
3. `scripts/agent_claim_guard.py` owns graph-and-policy-bound pre-mutation revalidation and emits schema `fmn.agent.claim-guard` version `2`.
4. `scripts/generate_agent_brief.py` publishes a deterministic human brief containing the planner decision plus broad context.
5. Beads remains the sole task mutation authority.

#### Strict ledger ingestion

The parser:

- rejects duplicate JSON keys at any object depth;
- distinguishes absent or explicit-null optional arrays from falsey malformed values;
- rejects malformed comment rows and non-string comment text rather than dropping them;
- treats a present invalid `updated_at` as invalid instead of silently falling back to `created_at`;
- caps comments as well as lines, issues, and dependencies;
- opens the ledger through a bounded regular-file descriptor and uses no-follow semantics where the host exposes them.

#### Graph and recommendation truthfulness

The planner:

- excludes assigned work and epic or topology-derived containers;
- treats a non-epic task with live `parent-child` descendants as a container;
- rejects parent-child containment cycles as well as blocking cycles and missing blockers;
- uses iterative SCC algorithms, including regression coverage for 1,500-node valid chains;
- prefers an eligible leaf in an already-active workstream;
- preserves explicit numeric priority before immediate-unblock pressure, direct blocker pressure, scope, recency, and lexical ID.

The broad projection now labels its priority as situational and explicitly non-claim-safe. The former `agent_brief.py --format next` alias has been retired; autonomous selection no longer has two competing command surfaces.

#### Deterministic bounded output

The default planner `as_of` is derived from the newest ledger record. Identical ledger bytes therefore produce identical canonical JSON without a caller supplying wall-clock state. JSON, Markdown, and ID payloads share a 4 MiB pre-publication ceiling; an oversized plan returns exit `2` without stdout.

The human generator refuses blocking cycles, containment cycles, missing blockers, and activation-cap breaches before any output. Existing artifacts are read through a descriptor-bound regular-file check. Failed publication removes only the temporary inode created by that attempt; if the path was substituted, the foreign path is retained and reported.

`scripts/check.sh` compiles and runs the parser, planner, claim guard, deterministic-output, generator, publication-I/O, and portal-refusal tests; validates the live plan, guarded claim input, and real portal source; then continues into the Rust/Python/WASM/structural gate. Hosted GitHub Actions is not required.

## Current open obligations

### W10 semantic surface

`fm-5wq.4` remains in progress. Its history contains both an older “100% reviewed” comment and a newer reality-check describing remaining portal implementation gaps. The newer tracker state is the operative evidence boundary. Therefore:

- reviewed ledger coverage is not presented as universal callable implementation;
- the refusal inventory is not presented as implementation coverage;
- generic or capability-refusing behavior must continue to match the row's declared status;
- representative clean-wheel semantics remain required across API families;
- each inventoried refusal should fall only by landing the owning behavior or an evidence-backed tier/exclusion;
- no closure is claimed here.

### CLI library/binary quiet-policy convergence

The shipped binary correctly suppresses human doctor success output under `--quiet` while retaining typed failures. That policy is not yet centralized inside `run_with_capabilities`; an embedded library caller can still receive the complete human snapshot. A future tranche should converge those front doors without reparsing argv or weakening robot output.

### Strict JSON constant rejection

The ledger decoder rejects duplicate keys and malformed structures, but it still calls Python's default `json.loads` constant policy. Python accepts non-standard `NaN`, `Infinity`, and `-Infinity` spellings by default. Typed required fields reject those values when they reach them, and the claim guard's canonical encoder rejects non-finite values retained in its input, but ignored extension fields can still pass the initial decode. The parser should eventually supply a rejecting `parse_constant` hook so its “JSON” contract is literal at the first boundary.

### Platform and release evidence

The following remain independent evidence lanes rather than automatic consequences of a local green tree:

- real aarch64 topology fixtures;
- platform-native execution matrices;
- SIMD-tier and certified cross-platform bit evidence;
- npm/WASM real-browser packaging;
- clean-wheel release gates on each supported platform;
- ffmpeg/video-container equivalence receipts.

### Tracker synchronization

Beads mutations must still be performed through `br`, followed by `br sync --flush-only` and a committed `.beads/` export. The read-only projections, claim guard, and refusal audit never edit or close work. Large tracker files must not be replaced from truncated connector output.

## Verification entry points

```bash
# Mandatory local or owned-host repository gate
scripts/check.sh

# Portal refusal truthfulness
python3 scripts/audit_portal_refusals.py --check
python3 scripts/audit_portal_refusals.py --format markdown
python3 scripts/test_audit_portal_refusals.py

# Whole-graph validation and human context
python3 scripts/agent_brief.py --format json --check
python3 scripts/generate_agent_brief.py --stdout

# Leaf-safe plan and guarded compare-before-set
python3 scripts/agent_next.py --format json --check
token="$(python3 scripts/agent_claim_guard.py --require)"
python3 scripts/agent_claim_guard.py --expect-token "$token" --require --format id

# Focused control-plane regressions
python3 scripts/test_agent_brief.py
python3 scripts/test_agent_next.py
python3 scripts/test_agent_next_output.py
python3 scripts/test_agent_claim_guard.py
python3 scripts/test_generate_agent_brief.py
python3 scripts/test_generate_agent_brief_io.py
python3 scripts/check_python_helper_aliases.py
python3 scripts/test_python_helper_aliases.py

# Complete shipped CLI smoke
cargo test -p fmn-cli --features batch --test cli_smoke
```

### Evidence from the current editing environment

The exact replacement bytes for `scripts/agent_claim_guard.py` and `scripts/test_agent_claim_guard.py` passed Python bytecode compilation before publication. The implementation, tests, and mandatory-gate wiring were committed incrementally to `main` as `6a6b3c8`, `93877d4`, and `5629a41`.

This environment did not provide an exact repository checkout containing the full portal source and Rust workspace. Therefore the focused claim-guard unit suite against the real parser/planner modules, the audit against the real `manimlib_bootstrap.py`, the complete Cargo/Clippy/rustdoc/WASM/wheel/browser axes, and the repository-wide `scripts/check.sh` invocation are not represented here as completed against `5629a41`. Hosted CI was queued during the documentation reconciliation and is not promoted into local/owned-host evidence.

GitHub Actions is not a correctness dependency. Verification is designed to run on controlled local or owned build hosts; unavailable hosted capacity does not weaken or waive any gate.

## Truthfulness rules for future updates

- Record exact commands and the source commit they exercised.
- Do not convert “compiled,” “reviewed,” “inventoried,” or “imported” into “fully implemented.”
- Do not infer hardware or artifact evidence from a different platform.
- Do not close a parent merely because one child or one census reaches 100%.
- Distinguish broad readiness from true leaf claimability.
- Revalidate a v2 claim token immediately before any autonomous `br update`.
- Keep target-state design in the comprehensive plan and current-state evidence here.
