# Performance-gate evidence contract

`PERF_GATES.tsv` is the machine-readable target catalog for plan §17.2.
It is policy, not a table of achieved numbers. In particular, parsing a row
does not create a passing PG result.

The decision core is `fmn_conformance::perf`. A gate result is attributable
only when all of these identities match a committed observed baseline:

- named pinned bare-metal profile, build profile, and canonical host
  fingerprint;
- exact toolchain and `SUITE.lock` bytes;
- exact benchmark-definition digest;
- gate/scenario, engine, build tier, thread profile, derived ExecutionPlan,
  semantic configuration, cache state, output mode, and optional ffmpeg/tool
  fingerprint.

Every run retains raw repetitions in canonical `fmn-perf-samples/1` TSV. A
host-quality failure is recorded with its measured value and reason, excluded
from statistics, and never silently dropped or averaged in. Evidence records
are path-canonicalized before the bundle is hashed. An observed baseline's
`raw-samples` digest is derived from those exact bytes rather than supplied by
the caller. The evaluator requires that raw bundle and replays its identity,
producer commit, robust statistics, and digest before any `pass`; merely parsing
a plausible baseline TSV is not trusted evidence. Comparable valid repetitions
produce an integer median, nearest-rank p95/p99, median absolute deviation, and
MAD ratio in basis points. Each policy also bounds invalid repetitions, and a
current run must retain the same total repetition count as its observed
baseline; too few valid samples, an exceeded invalid budget, a changed sample
plan, or excessive dispersion is `inconclusive`, not pass. For `exactly`
policies, every valid repetition must equal the target; a median of zero cannot
hide one nonzero mismatch, allocation, or leak sample, and variation among
those values cannot downgrade an attributable exact failure to a
dispersion-only `inconclusive`.
For strict plan bounds expressed in integer nanoseconds (`< 150 ms`, for
example), the catalog stores the greatest admissible integer
(`149,999,999 ns`) so equality at the prose boundary still fails.

## Verdicts

| Verdict | Meaning |
|---|---|
| `pass` | Comparable observed baseline, target met, regression envelope met |
| `alert` | Comparable miss under observe/alert policy, or an alert-band regression |
| `block` | Comparable miss under blocking policy and within the policy's change scope |
| `inconclusive` | Unobserved baseline, identity/host mismatch, malformed evidence, insufficient samples, or excessive dispersion |

PG-8 has `python-only` scope. PG-A has `annex-only` scope, so it cannot mask or
block an ordinary CPU/core change (R21). All alerting timing regressions require
a content-addressed flamegraph or CPU profile below
`tests/artifacts/perf/<run-id>/`.

## Current coverage boundary

Existing repository evidence remains distinct:

- W1's Python Reference report is calibration-only shared-host evidence, not a
  PG-1 denominator.
- PG-5's `{1,4,16}` certified corpus is already a blocking per-commit proof.
- the ignored native Metal probes are PG-A measurement producers, not
  committed pinned-profile baselines.
- Studio's edit-to-frame report and the frame-pool structural tests are inputs
  to PG-4/PG-6; neither alone closes the whole gate.

Pinned profile fingerprints, raw observed baseline bundles, and runnable
scenario producers land separately. Until then, target-only baselines must
report `inconclusive`.

## Robot verifier

`cargo run --profile release-perf -p fmn-conformance --bin fmn-perf -- ...`
provides NDJSON-only policy, evidence, and producer commands:

- `catalog docs/performance/PERF_GATES.tsv` validates the complete policy
  catalog and reports its canonical digest and rows.
- `verify-baseline <baseline.tsv>` validates a versioned observed baseline,
  loads its declared repository-relative raw-sample source, requires the
  canonical `fmn-perf-samples/1` schema, recomputes its robust statistics, and
  verifies identity, producer commit, and exact digest.
- `pg2-definitions` reports the exact compiled benchmark-definition and C7/C10
  configuration digests for both canonical raster workloads.
- `measure-pg2 <baseline.tsv> <producer-commit> <trace.tsv> <raw.tsv>` checks
  that the supplied baseline identity names that exact compiled producer, runs
  it, and exclusively creates a content-addressed phase trace followed by the
  canonical raw sample bundle. Both output paths must be distinct canonical
  files below `tests/artifacts/perf/`; their parent directories must already
  exist and may not contain symlinks, and existing paths are never overwritten.

Exit `0` means the requested structural/evidence check succeeded; `64` is
usage error, `65` is malformed, missing, or mismatched data, and `74` is an
output I/O failure. A measurement record has status
`measured-not-evaluated`: the producer does not certify the machine it happens
to run on and cannot turn a target-only baseline into green evidence. Until
fm-inr.1 lands live pinned-host attestation, PG-2 output forcibly records
`bare_metal=false` and `isolated=false` even when a supplied context claims
otherwise. The profile name and fingerprints are retained for calibration,
but the common evaluator must classify the bundle as host-unqualified or an
identity mismatch rather than pass it.

## Canonical PG-2 workloads

The `fmn-perf-pg2-definition/1` bytes state every workload axis required by
plan §17.2. Both execute Lumen's fast CPU engine at the artifact's compiled
SIMD tier, fixed at eight threads, adaptive AA, 128 px macrotiles, 16 px fine
tiles, and pooled raw RGBA16F output. Fixture construction, render-plan sync,
monotone-table construction, binning, output allocation/prime, warmup, and
every timed raster repetition are distinguished in the
`fmn-perf-pg2-trace/1` evidence. Each definition also binds an exact raw-frame
self-golden. The producer checks the primed frame before timing and refuses
output if the rendered workload drifts; it checks the final frame again so
nondeterminism during the repetitions cannot masquerade as throughput.

| Scenario | Viewport and geometry | Overdraw / style | Throughput denominator |
|---|---|---|---|
| `fill-canonical` | 512×512; 32 closed rectangles, four flat quadratic-line segments each | 32 translucent layers; alpha 0.08 | one output pixel evaluated against one fill layer |
| `stroke-canonical` | 512×256; one 64-quadratic chain with alternating 56 px control and 20 px endpoint offsets | opaque six-pixel miter stroke | one output pixel |

Each run performs three untimed warmups and retains 24 repetitions: 21 valid
observations required by policy plus room for the three explicit invalid
observations the policy permits. Fill times one frame per repetition; stroke
times four to keep timer quantization below the work being measured. Integer
conversion produces `mpx-per-second-milli` without floating-point rounding.
Zero elapsed time or numeric overflow is retained as an invalid sample, never
replaced by a plausible throughput. The final frame is canonically hashed
outside the timed region, and the trace itself is the batch's
content-addressed `phase-trace` evidence row.
