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
- `pg7-definitions` reports the exact compiled benchmark-definition,
  configuration, fixture-input, and result-self-golden digests for all three
  native-typesetting workloads.
- `measure-pg2 <baseline.tsv> <producer-commit> <trace.tsv> <raw.tsv>` checks
  that the supplied baseline identity names that exact compiled producer, runs
  it, and exclusively creates a content-addressed phase trace followed by the
  canonical raw sample bundle. Both output paths must be distinct canonical
  files below `tests/artifacts/perf/`; their parent directories must already
  exist and may not contain symlinks, and existing paths are never overwritten.
- `measure-pg7 <baseline.tsv> <producer-commit> <cache-root-or-dash>
  <trace.tsv> <raw.tsv>` applies the same identity and exclusive-publication
  rules to PG-7. `formula-cached` requires a fresh, nonexistent cache root
  below `tests/artifacts/perf/`; the other scenarios require `-`. The producer
  leaves the owned cache root intact as evidence-supporting state and never
  cleans it up or reuses it.

Exit `0` means the requested structural/evidence check succeeded; `64` is
usage error, `65` is malformed, missing, or mismatched data, and `74` is an
output I/O failure. A measurement record has status
`measured-not-evaluated`: the producer does not certify the machine it happens
to run on and cannot turn a target-only baseline into green evidence. Until
fm-inr.1 lands live pinned-host attestation, PG-2 output forcibly records
`bare_metal=false` and `isolated=false` even when a supplied context claims
otherwise; PG-7 does the same. The profile name and fingerprints are retained
for calibration, but the common evaluator must classify the bundle as
host-unqualified or an identity mismatch rather than pass it.

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

## Canonical PG-7 workloads

The `fmn-perf-pg7-definition/1` bytes bind the source, semantic configuration,
engine identity, cache state, fixed sample plan, and an exact output
self-golden. All initialization, priming, cache proof, warmup, and final-result
checks remain outside the timed regions and are recorded separately in the
`fmn-perf-pg7-trace/1` evidence.

| Scenario | Timed production path | Required state | Strict target |
|---|---|---|---|
| `formula-cold` | `fmn-tex` math-display layout of the fixed synthetic structural isomorph of the G0-4 corpus median | content cache disabled; engine and CPU warmed | < 3 ms |
| `formula-cached` | `fmn-tex` lookup and bit-exact payload decode for that formula | exact-key miss proven, one-item production `preflight` succeeds, exact stored payload decodes and matches the self-golden, the entry is pinned against in-process eviction, exact-key hit proven again after timing | < 100 µs |
| `text-10k-glyph` | `fmn-text::layout_text` with the bundled `FontBook` and plain-text defaults | cache absent; deterministic ASCII source produces exactly 10,000 non-whitespace glyphs, one line, and no decorations | < 20 ms |

The formula result digest is SHA-256 over `Typeset::to_bytes`; the native-text
digest is a versioned canonical hash of every layout, line, glyph, face,
position, span, and style field. A producer refuses to emit timing evidence if
either primed or final output differs. The cached path also refuses an already
warm store: the miss-to-hit transition must be observed in this run, never
declared by caller metadata.

The formula workload derives reproducibly from G0-4's ratified corpus rules
v1: math-mode entries are ordered by UTF-8 byte length, construct count, then
`sha256(mode + NUL + string)`, while occurrence counts supply the weight. The
lower median is occurrence 6,010 of 12,020; its corpus-pair digest is
`5d79ab0a3d5eaf9db101f5dcef2630326c1eff6766a31734e1155f2487988a4d`.
The authored corpus string remains private under G0-4 §15.3. The public timed
fixture `q^7+z` is a synthetic structural isomorph with the median's five
UTF-8 bytes and one harvested construct. The definition binds that fixture
relation, selection rule, rank, median pair digest, rules version, and the
corpus digest
`a8325e49e0ce78fcc735533952740e9adeaaa5cb10f9c13d73aaa3ba4bf883fc`.

Each scenario performs three untimed warmups and retains 24 raw integer
nanosecond repetitions, requiring 21 valid observations and permitting three
explicit host-quality failures. Zero elapsed time or a value outside `u64` is
retained as an invalid sample. A native typesetting error is preserved
verbatim inside the typed PG-7 workload error, including unsupported-construct
name and tier; it is never converted into an empty layout or a timing sample.
