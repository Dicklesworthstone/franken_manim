# FrankenManim implementation status

**Status date:** 2026-09-01  
**Evidence checkpoint:** product and control-plane commits through `403c1f4`, followed by documentation truth-up commits.  
**Authority rule:** this document summarizes evidence; `.beads/issues.jsonl` remains the task and dependency authority.

## How to read this document

FrankenManim is pre-1.0. A capability is listed as implemented only when a concrete source surface and a checkable test or artifact boundary exist. Aspirational plan text, a reviewed Parity Ledger row, or an old Beads comment is not by itself implementation evidence. Hardware- and release-specific claims remain separate from local semantic correctness.

## Product surfaces

| Surface | Current implementation state | Evidence boundary |
|---|---|---|
| Native Rust library | The composition root, scene runtime, mobject/animation stack, native text and math, retained renderer, codecs, output pipeline, and built-in scene corpus are implemented as workspace crates. | Ordinary workspace checks and tests run through `scripts/check.sh`; platform certification remains a separate matrix. |
| `fmn` CLI | Typed render, doctor, batch, and Studio command surfaces exist. The shipped binary has executable smoke coverage for version/help, doctor robot and quiet behavior, typed exit codes, a real one-scene batch render, native artifact publication, and machine/human manifests. | `cargo test -p fmn-cli --features batch --test cli_smoke` exercises the complete shipping feature shape. |
| Studio | Supervisor/worker, replay, event, inspection, and presentation foundations are implemented. | Platform and real-browser presentation evidence is not inferred from local Rust tests. |
| `fmn-python` portal | A separately installed CPython 3.13 wheel exposes the pinned `manimlib` namespace with native and authored compatibility behavior. | The W10 semantic-surface parent remains open. The newest tracker reality-check overrides older comments that described ledger review as product completion. |
| WASM/browser | The wasm32 render/player foundations and package gate exist. | The npm/real-browser release gate is opt-in (`FMN_WASM_PACKAGE_GATE=1`) and is not claimed merely because ordinary local checks pass. |
| Distribution | Tagged `v0.1.0` through `v0.4.0` releases are prereleases. | Cross-platform artifacts, hardware-specific execution, and release matrices require their own receipts. |

## Recently completed implementation tranche

### Executable CLI boundaries

- Batch robot output now emits an explicit terminal `batch_job` success record for every successful job.
- A real shipped-binary smoke renders a tiny built-in scene and verifies the native artifact plus `manifest.fmnp` and `manifest.txt` publication.
- `doctor --quiet` policy lives in the central dispatcher. Human success output is suppressed, typed failures remain visible, and robot output is never hidden.

### Python/Rust API boundary

Rust-native ergonomic helpers such as `small_dot`, `rounded_rectangle`, and `v_highlight` are not Python wildcard exports unless the pinned Reference exports those exact names. The policy maps all sixteen Rust helpers to their Reference CamelCase classes and verifies three surfaces agree:

1. the extracted API schema contains the exported Reference class;
2. the wheel/package wrapper rejects a leaked snake_case helper or missing class;
3. the clean-wheel smoke constructs the complete class family.

This resolves the open helper-binding proposal by preserving source compatibility rather than inventing a second Python API.

### Agent control plane

The Beads projection is now schema v4 and fail-closed:

- bounded JSONL, issue, line, dependency, and output sizes;
- known-status enforcement;
- dependency ownership, self-edge, and duplicate-edge refusal;
- missing target reporting;
- deterministic iterative strongly connected component analysis for live `blocks` cycles;
- distinct claimable, assigned-ready, epic-container, blocked, stale, unowned, and unscoped queues;
- no recommendation when dependency integrity is invalid;
- preference for an unassigned leaf in an already-active workstream;
- activation-cap enforcement before output publication.

`scripts/generate_agent_brief.py` derives deterministic Markdown using the newest ledger timestamp. `--stdout` is nonmutating; `--check` compares exact bytes when a committed projection is desired; normal generation uses an exclusive temporary file, fsync, and atomic replacement. A malformed graph or cap breach leaves any prior artifact untouched.

## Current open obligations

### W10 semantic surface

`fm-5wq.4` remains in progress. Its history contains both an older “100% reviewed” comment and a newer reality-check describing remaining portal implementation gaps. The newer tracker state is the operative evidence boundary. Therefore:

- reviewed ledger coverage is not presented as universal callable implementation;
- generic or capability-refusing behavior must continue to match the row’s declared status;
- representative clean-wheel semantics remain required across API families;
- no closure is claimed here.

### Platform and release evidence

The following remain independent evidence lanes rather than automatic consequences of a local green tree:

- real aarch64 topology fixtures;
- platform-native execution matrices;
- SIMD-tier and certified cross-platform bit evidence;
- npm/WASM real-browser packaging;
- clean-wheel release gates on each supported platform;
- ffmpeg/video-container equivalence receipts.

### Tracker synchronization

Beads mutations must still be performed through `br`, followed by `br sync --flush-only` and a committed `.beads/` export. The read-only projection never edits or closes work. Large tracker files must not be replaced from truncated connector output.

## Verification entry points

```bash
# Mandatory local repository gate
scripts/check.sh

# Read-only live task projection and safe recommendation
python3 scripts/agent_brief.py --format next --check
python3 scripts/generate_agent_brief.py --stdout

# Focused control-plane regressions
python3 scripts/test_agent_brief.py
python3 scripts/test_generate_agent_brief.py
python3 scripts/check_python_helper_aliases.py
python3 scripts/test_python_helper_aliases.py

# Complete shipped CLI smoke
cargo test -p fmn-cli --features batch --test cli_smoke
```

GitHub Actions is not a correctness dependency. Verification is designed to run on controlled local or owned build hosts; unavailable hosted Actions capacity does not weaken or waive any gate.

## Truthfulness rules for future updates

- Record exact commands and the source commit they exercised.
- Do not convert “compiled,” “reviewed,” or “imported” into “fully implemented.”
- Do not infer hardware or artifact evidence from a different platform.
- Do not close a parent merely because one child or one census reaches 100%.
- Keep target-state design in the comprehensive plan and current-state evidence here.
