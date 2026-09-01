# FrankenManim implementation status

**Status date:** 2026-09-01 UTC / 2026-08-31 America/New_York  
**Evidence checkpoint:** substantive source commits through `204eabf`, followed by documentation truth-up commits.  
**Authority rule:** this document summarizes evidence; `.beads/issues.jsonl` remains the task and dependency authority.

## How to read this document

FrankenManim is pre-1.0. A capability is listed as implemented only when a concrete source surface and a checkable test or artifact boundary exist. Aspirational plan text, a reviewed Parity Ledger row, or an old Beads comment is not by itself implementation evidence. Hardware- and release-specific claims remain separate from local semantic correctness.

## Product surfaces

| Surface | Current implementation state | Evidence boundary |
|---|---|---|
| Native Rust library | The composition root, scene runtime, mobject/animation stack, native text and math, retained renderer, codecs, output pipeline, and built-in scene corpus are implemented as workspace crates. | Ordinary workspace checks and tests run through `scripts/check.sh`; platform certification remains a separate matrix. |
| `fmn` CLI | Typed render, doctor, batch, and Studio command surfaces exist. The shipped binary has executable smoke coverage for version/help, doctor robot and quiet behavior, typed exit codes, a real one-scene batch render, native artifact publication, and machine/human manifests. | `cargo test -p fmn-cli --features batch --test cli_smoke` exercises the complete shipping feature shape. The process-output helper is additionally exercised as a binary unit module by the all-target Cargo gate. |
| Studio | Supervisor/worker, replay, event, inspection, and presentation foundations are implemented. | Platform and real-browser presentation evidence is not inferred from local Rust tests. |
| `fmn-python` portal | A separately installed CPython 3.13 wheel exposes the pinned `manimlib` namespace with native and authored compatibility behavior. | The W10 semantic-surface parent remains open. The newest tracker reality-check overrides older comments that described ledger review as product completion. |
| WASM/browser | The wasm32 render/player foundations and package gate exist. | The npm/real-browser release gate is opt-in (`FMN_WASM_PACKAGE_GATE=1`) and is not claimed merely because ordinary local checks pass. |
| Distribution | Tagged `v0.1.0` through `v0.4.0` releases are prereleases. | Cross-platform artifacts, hardware-specific execution, and release matrices require their own receipts. |

## Recently completed implementation tranche

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

The operational layer now has three linked abstractions:

1. `scripts/agent_brief.py` owns bounded untrusted-ledger parsing, blocking-graph integrity, activation state, and broad situational rendering.
2. `scripts/agent_next.py` owns autonomous leaf selection and emits schema `fmn.agent.next` version `2`.
3. `scripts/generate_agent_brief.py` publishes a deterministic human brief containing the exact planner decision plus broad context.

#### Strict ledger ingestion

The parser now:

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

#### Deterministic bounded output

The default planner `as_of` is derived from the newest ledger record. Identical ledger bytes therefore produce identical canonical JSON without a caller supplying wall-clock state. JSON, Markdown, and ID payloads share a 4 MiB pre-publication ceiling; an oversized plan returns exit `2` without stdout.

The human generator refuses blocking cycles, containment cycles, missing blockers, and activation-cap breaches before any output. Existing artifacts are read through a descriptor-bound regular-file check. Failed publication removes only the temporary inode created by that attempt; if the path was substituted, the foreign path is retained and reported.

`scripts/check.sh` compiles and runs the parser, planner, deterministic-output, generator, and publication-I/O tests, validates the live plan, then continues into the Rust/Python/WASM/structural gate. Hosted GitHub Actions is not required.

## Current open obligations

### W10 semantic surface

`fm-5wq.4` remains in progress. Its history contains both an older “100% reviewed” comment and a newer reality-check describing remaining portal implementation gaps. The newer tracker state is the operative evidence boundary. Therefore:

- reviewed ledger coverage is not presented as universal callable implementation;
- generic or capability-refusing behavior must continue to match the row's declared status;
- representative clean-wheel semantics remain required across API families;
- no closure is claimed here.

### CLI library/binary quiet-policy convergence

The shipped binary correctly suppresses human doctor success output under `--quiet` while retaining typed failures. That policy is not yet centralized inside `run_with_capabilities`; an embedded library caller can still receive the complete human snapshot. A future tranche should converge those front doors without reparsing argv or weakening robot output.

### Legacy broad “next” spelling

`agent_brief.py --format next` remains only for compatibility and is not leaf-safe. Autonomous work selection must use `agent_next.py`. Retiring or delegating that old spelling remains desirable once downstream callers are migrated.

### Platform and release evidence

The following remain independent evidence lanes rather than automatic consequences of a local green tree:

- real aarch64 topology fixtures;
- platform-native execution matrices;
- SIMD-tier and certified cross-platform bit evidence;
- npm/WASM real-browser packaging;
- clean-wheel release gates on each supported platform;
- ffmpeg/video-container equivalence receipts.

### Tracker synchronization

Beads mutations must still be performed through `br`, followed by `br sync --flush-only` and a committed `.beads/` export. The read-only projections never edit or close work. Large tracker files must not be replaced from truncated connector output.

## Verification entry points

```bash
# Mandatory local or owned-host repository gate
scripts/check.sh

# Whole-graph validation and human context
python3 scripts/agent_brief.py --format json --check
python3 scripts/generate_agent_brief.py --stdout

# Leaf-safe machine claim plan
python3 scripts/agent_next.py --format json --check
python3 scripts/agent_next.py --require

# Focused control-plane regressions
python3 scripts/test_agent_brief.py
python3 scripts/test_agent_next.py
python3 scripts/test_agent_next_output.py
python3 scripts/test_generate_agent_brief.py
python3 scripts/test_generate_agent_brief_io.py
python3 scripts/check_python_helper_aliases.py
python3 scripts/test_python_helper_aliases.py

# Complete shipped CLI smoke
cargo test -p fmn-cli --features batch --test cli_smoke
```

### Evidence from the current editing environment

The strict parser's complete focused suite passed before the final documentation phase, and targeted probes passed for containment-publication refusal, descriptor-bound artifact I/O, owned-temporary cleanup, substituted-temporary preservation, deterministic planner timestamps, and pre-output size refusal. This environment did not provide an exact repository checkout, so the full Cargo, Clippy, rustdoc, WASM, wheel, browser, and `scripts/check.sh` gates were not represented as run against `204eabf`.

GitHub Actions is not a correctness dependency. Verification is designed to run on controlled local or owned build hosts; unavailable hosted capacity does not weaken or waive any gate.

## Truthfulness rules for future updates

- Record exact commands and the source commit they exercised.
- Do not convert “compiled,” “reviewed,” or “imported” into “fully implemented.”
- Do not infer hardware or artifact evidence from a different platform.
- Do not close a parent merely because one child or one census reaches 100%.
- Distinguish broad readiness from true leaf claimability.
- Keep target-state design in the comprehensive plan and current-state evidence here.
