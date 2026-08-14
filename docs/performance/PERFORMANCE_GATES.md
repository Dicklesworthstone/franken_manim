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
- PG-5's canonical producer covers the complete committed scene corpus at
  `{1,4,16}` plus the production frame-parallel and ordered-emitter paths. Its
  checked-in output remains host-unqualified because no real pinned-machine
  profile/observation is committed yet; weekly high-core and
  certified-platform receipts are separate evidence, not inferred successes.
- PG-6's primitive steady-state allocation producer covers the complete
  committed scene-golden corpus, and the leak-soak producer covers
  `one-hour-soak-leak` on residency-capable (Linux) hosts. Its peak-RSS
  producer covers a bit-locked three-view UHD 3D gallery through the public
  library and renderer path. All three checked-in runs remain host-unqualified;
  no observed pinned-host PG-6 verdict is inferred from shared-host runs.
- the ignored native Metal probes are PG-A measurement producers, not
  committed pinned-profile baselines.
- Studio's edit-to-frame report remains an input to PG-4, not a whole-gate
  result.

Pinned profile fingerprints, raw observed baseline bundles, and runnable
scenario producers land separately. Until then, target-only baselines must
report `inconclusive`.

## Robot verifier

From `crates/fmn-conformance/`,
`cargo run --manifest-path ../../Cargo.toml --profile release-perf \
-p fmn-conformance --bin fmn-perf -- ...` provides NDJSON-only policy,
evidence, and producer commands. This working directory is part of the
artifact-path contract: canonical evidence paths and baseline source rows are
rooted at this crate's `tests/artifacts/perf/`, while the policy catalog is
`../../docs/performance/PERF_GATES.tsv`.

- `catalog ../../docs/performance/PERF_GATES.tsv` validates the complete policy
  catalog and reports its canonical digest and rows.
- `verify-baseline <baseline.tsv>` validates a versioned observed baseline,
  loads its declared repository-relative raw-sample source, requires the
  canonical `fmn-perf-samples/1` schema, recomputes its robust statistics, and
  verifies identity, producer commit, and exact digest.

Every catalog, baseline, and raw-source input must classify as a regular-file
leaf without following that leaf before its bounded read begins. A declared
repository-relative raw source additionally requires real directory components
throughout its path; links, Windows reparse points, special nodes, missing
nodes, and wrong kinds are refused. Safe `std` cannot bind these checks to the
subsequent path-based open, so verification assumes the inputs are not
concurrently retargeted by their owner.
- `pg2-definitions` reports the exact compiled benchmark-definition and C7/C10
  configuration digests for both canonical raster workloads.
- `pg5-definitions` reports the exact compiled corpus lock, semantic
  configuration, fixed certified ExecutionPlan, one-thread reference
  self-golden, and all three schedule mechanisms.
- `pg6-definitions` reports the exact compiled corpus-lock, benchmark,
  configuration, and frame-result self-golden identities for both the
  primitive steady-state allocation workload and the UHD 3D peak-residency
  workload.
- `pg7-definitions` reports the exact compiled benchmark-definition,
  configuration, fixture-input, and result-self-golden digests for all three
  native-typesetting workloads.
- `measure-pg2 <baseline.tsv> <producer-commit> <trace.tsv> <raw.tsv>` checks
  that the supplied baseline identity names that exact compiled producer, runs
  it, and exclusively creates a content-addressed phase trace followed by the
  canonical raw sample bundle. Both output paths must be distinct canonical
  files below `tests/artifacts/perf/`; their parent directories must already
  exist and may not contain links or Windows reparse points, and existing paths
  are never overwritten. This component preflight has the same safe-`std`
  concurrent-retargeting boundary as replay-source validation.
- `measure-pg5 <baseline.tsv> <producer-commit> <trace.tsv> <raw.tsv>` applies
  the same identity and exclusive-publication rules to the certified schedule
  matrix. It records exactly three mismatch-count samples: direct `{1,4,16}`
  renders, the production frame-parallel runtime, and that runtime publishing
  through the preallocated ordered emitter.
- `measure-pg6 <baseline.tsv> <producer-commit> <trace.tsv> <raw.tsv>` applies
  the same identity and exclusive-publication rules to the allocation corpus.
  It renders one warm frame and one caller-buffer/arena reuse frame for every
  committed scene, retaining both frame digests and the engine-owned allocation
  ledger for each measured frame.
- `measure-pg6-peak <baseline.tsv> <producer-commit> <trace.tsv> <raw.tsv>`
  runs eleven independent complete passes over the certified three-view UHD 3D
  gallery. A concurrent one-millisecond resident-set sampler plus explicit
  before/after probes retains one peak byte count per pass; unsupported hosts
  retain eleven named invalid samples and do no synthetic rendering work.
- `measure-pg6-soak <baseline.tsv> <producer-commit> <trace.tsv> <raw.tsv>
  <iterations-per-window>` applies the same identity and exclusive-publication
  rules to the leak soak: three windows of full-corpus steady-state rendering,
  one `leaked-bytes` RSS-delta sample per window through the fmn-platform
  residency capability, with unsupported hosts retained as invalid samples.
- `measure-pg7 <baseline.tsv> <producer-commit> <cache-root-or-dash>
  <trace.tsv> <raw.tsv>` applies the same identity and exclusive-publication
  rules to PG-7. `formula-cached` requires a fresh, nonexistent cache root
  below `tests/artifacts/perf/`; the other scenarios require `-`. The producer
  leaves the owned cache root intact as evidence-supporting state and never
  cleans it up or reuses it.

Every `measure-*` command above accepts one optional, indivisible final pair:
`<host-profile.tsv> <host-attestation.tsv>`. Omitting both is explicit
calibration mode and always records `bare_metal=false`, `isolated=false`, and
null attestation fields in the robot result. Supplying only one is a usage
error. Supplying both invokes the live authority described below before any
benchmark work and repeats the same live checks after the workload; the
attestation output, trace, and raw bundle must be three distinct nonexistent
files below `tests/artifacts/perf/`.

Exit `0` means the requested structural/evidence check succeeded; `64` is
usage error, `65` is malformed, missing, or mismatched data, and `74` is an
output I/O failure. A measurement record has status
`measured-not-evaluated`: even a qualified producer cannot turn a target-only
baseline into green evidence. Without the optional host pair, every producer
forcibly records `bare_metal=false` and `isolated=false` even when its baseline
claims otherwise. With the pair, only the opaque in-process token minted by
the live authority may set both fields; the baseline's profile, host, compiler,
`SUITE.lock`, and build-profile identities must exactly match that token. A
mismatch fails before workload execution. Postflight host drift fails before
any output is published. The common evaluator still requires a committed
observed baseline and replayed raw source before it can pass.

## Pinned-host profile and live-attestation authority

`fmn_conformance::perf_host` owns two bounded schemas:
`fmn-perf-host-profile/1` and `fmn-perf-host-attestation/1`. A profile is an
exact key/value TSV manifest for one machine generation. It names the profile
and platform and pins hashes of `/etc/os-release`, stable CPU identity rows,
non-serial DMI identity, the complete `HardwareTopology` snapshot, and the
storage source. It also pins the exact kernel, eight benchmark CPU IDs,
cgroup-v2 path, governor, turbo/boost leaf and value, thermal sensor and
ceiling, load ceiling, mount point, and filesystem type. Unknown, duplicate,
oversized, traversal-bearing, caller-controlled thermal, and incomplete
profiles are rejected.

On `linux-x86_64`, qualification re-reads every pinned value through bounded
host capabilities and additionally requires all of the following:

- exactly eight physical cores and eight distinct physical cores in the
  benchmark CPU set;
- no CPU hypervisor flag, `/sys/hypervisor/type`, nested PID namespace, or
  known virtual/cloud DMI marker;
- exact process affinity plus exact `isolated`, `nohz_full`, and `rcu_nocbs`
  CPU lists;
- one unified cgroup-v2 whose effective cpuset is exact and whose
  `cgroup.procs` contains only the direct measurement binary's PID;
- the pinned governor on every benchmark CPU, exact turbo policy, temperature
  and one-minute load within their ceilings, and exact mount/source identity
  for the raw artifact path.

The package build embeds `rustc --version --verbose` from Cargo's actual
compiler, the exact `rust-toolchain.toml`, and the exact `SUITE.lock` bytes.
Those compiled identities—not caller text—must match the baseline. The live
authority repeats the complete identity, affinity, isolation, power, thermal,
load, cgroup, and storage checks after the producer returns. Only then is the
preflight attestation published as content-addressed `host-attestation`
evidence, followed by the phase trace and raw bundle. Publication is
create-new/no-clobber throughout; a later race can leave an obviously partial
evidence set, and the tool never deletes it. These start/end observations do
not claim continuous detection of a transient excursion that fully recovers
inside a long measurement window; continuous monitoring remains open under
fm-inr.1.

For a qualified run, change to `crates/fmn-conformance/` and run the already
built `release-perf` binary directly inside the dedicated cgroup. Store the
reviewed profile under `tests/artifacts/perf/hosts/`. `cargo run` keeps Cargo in
the same cgroup, so the exclusive-process check correctly refuses it. RCH,
containers, VMs, shared runners, missing
sysfs/cgroup leaves, and ordinary developer machines are calibration-only.
The `macos-aarch64` schema family is reserved, but qualification currently
returns a precise unavailable error: `HardwareTopology::fallback` is not
evidence, and no subprocess-based `sysctl` escape is allowed by D-02. A safe
native macOS topology/power/isolation capability and real profile still remain
open under fm-inr.1.

### Baseline-version update ritual

1. Change one reviewed host profile generation; never edit a prior profile in
   place. A kernel, firmware, topology, power, storage, compiler, or suite-lock
   change therefore produces a new `BenchmarkKey`.
2. Build `fmn-perf` under `release-perf`, enter the exact dedicated cgroup,
   cool/quiesce the host, and run every applicable canonical producer with the
   profile/attestation pair. Retain invalid repetitions and partial publication
   artifacts; do not delete or average them away.
3. Commit the profile, host attestation, phase trace, and canonical raw TSV.
   Derive the observed baseline only from those exact bytes and verify it with
   `fmn-perf verify-baseline`; hand-authored medians or copied qualification
   booleans are invalid.
4. Run the complete Gauntlet, compare the prior generation, adjudicate every
   alert/block, then update the scheduled host lane. Until both declared Linux
   and macOS profiles have real replayable observations for applicable keys,
   fm-inr.1 and whole-gate claims remain open.

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

## Canonical PG-5 schedule workload

The `fmn-perf-pg5-definition/1` bytes bind all 27 committed `scene_goldens`
cases in corpus order, their certified lock digest, the 320×180 semantic
configuration, certified CPU at the artifact's crate-wide compiled tier, raw
RGBA16F digest output, and an aggregate one-thread reference self-golden. They
also bind the fixed certified offline `ExecutionPlan`: a synthetic 64-core
topology whose planner capability is pinned to the portable scalar definition,
two 32-thread render teams, and at most two frames in flight. The synthetic
topology defines a host-independent replayable schedule contract; it is not a
claim about the measurement host or the separately recorded compiled renderer
tier.

The producer emits exactly three valid `mismatches` samples:

| Sample | Production path | Comparison |
|---|---|---|
| `direct-thread-matrix` | Each prepared scene renders directly at requested thread counts `{1,4,16}` | Four- and sixteen-thread digests against the one-thread digest |
| `frame-parallel` | `FramePipeline` fans the corpus across both certified render teams and emits in sequence | Every emitted digest against its one-thread scene digest |
| `ordered-pipeline` | The same runtime reserves a capacity-two `OrderedEmitter` ring, renders into reservations, and publishes through a reliable digest sink | Every sink digest against its one-thread scene digest |

The exact blocking policy requires all three samples to be zero; one candidate
drift therefore reaches the common verifier as a valid nonzero sample instead
of being hidden by a median. Independently, the ordered one-thread aggregate
must match the compiled self-golden before any evidence is emitted, preventing
corpus or renderer drift from silently redefining the reference.

The `fmn-perf-pg5-trace/1` artifact retains every per-scene digest, all three
mismatch counts, maximum in-flight observations, per-team frame counts, and
the ordered emitter's maximum outstanding reservations. The producer validates
that both teams did real work, every frame was emitted in order, and the emitter
finished with no outstanding or dropped frames. Its batch forces host
qualification false in calibration mode and accepts true only from the live
profile token.

This is the permanent per-commit `{1,4,16}` surface only. Weekly `{32,96}+`
runs and certified-platform receipts are separate lifecycle evidence. When
those receipts are absent or a DSR platform cannot stage the repository, their
status is `inconclusive`; the per-commit producer does not manufacture a
platform verdict.

## Canonical PG-6 allocation workload

The `fmn-perf-pg6-definition/1` bytes bind all 27 committed
`scene_goldens` cases in corpus order, the certified lock-file digest, the
320×180 semantic configuration, certified CPU at the artifact's crate-wide
compiled tier, fixed four-thread scheduling, raw RGBA16F output, and the exact
aggregate frame-result self-golden. This is the
`primitive-steady-allocations` policy row only; it neither estimates nor
substitutes for `gallery-4k-3d-peak` or `one-hour-soak-leak`.

For each scene the producer creates a fresh `FrameArena`, renders one excluded
warm frame to size every typed bump pool and worker scratch slot, then creates a
second `FrameJob` over the same arena and renders into the same frame buffer.
The second job's `heap_allocs_this_frame` counter is retained as one valid
`allocations` sample. The engine owns that counter: every typed-pool capacity
growth and new/outgrown worker slot is counted at its allocation point, without
an allocator shim. The exact policy requires every sample to equal zero, so a
single nonzero scene blocks even when the median remains zero.

The `fmn-perf-pg6-trace/1` artifact retains, per scene, the warm and measured
frame digests, warm allocation count, measured allocation count, reserved arena
bytes, and worker-pool slot count. Warm and measured frame digests must be equal,
and the ordered corpus aggregate must match the compiled self-golden before the
producer emits evidence. The measured batch still forces host qualification
false in calibration mode; this makes shared-host output useful without
inventing a closeable whole-PG-6 verdict.

## Canonical PG-6 4K 3D peak-residency workload

The `fmn-perf-pg6-peak-definition/1` bytes bind three camera states over one
native gallery assembled from `Sphere`, `Torus`, `Cylinder`, and all six
`Cube` faces. Each state renders at 3840×2160 through Lumen's public certified
`ThreeDJob`, with four camera samples, four render threads, fixed 128/16 tiling,
raw RGBA16F output, and default fail-closed 3D preparation limits. The three
resulting frame digests are exact rows in
`gallery_3d.certified.lock`; corpus, camera, renderer, or library drift is a
hard producer error before evidence publication.

One sample is one complete front/quarter/orbit gallery pass. The producer runs
eleven real passes: nine required by policy plus two retained-invalid slots.
It never repeats one observation to enlarge the denominator. While each pass
is live, a one-millisecond sampler polls `current_rss_bytes` concurrently and
combines those observations with explicit before/after probes. The largest
observed `VmRSS` becomes that pass's `bytes` sample, while the phase trace
retains every frame digest and each job's conservative preparation-byte
admission. If the host cannot report resident set size, all eleven observations
are retained as invalid with reason `rss-unsupported-host`; no plausible value
is substituted.

The strict blocking target is at most 1.5 GB. Like the other current producers,
the batch forcibly records `bare_metal=false` and `isolated=false` unless the
live profile token is supplied. The full-tier e2e catalog runs
the exact production UHD gallery and certified lock path; scheduling a weekly
measurement and committing an observed host-qualified baseline remain separate
evidence work, not properties manufactured by the renderer test.

## Canonical PG-6 leak-soak workload

The `fmn-perf-pg6-soak-definition/1` bytes bind the same 27-scene certified
corpus, configuration, engine identity, and four-thread schedule as the
allocation workload, plus the two soak axes: three measurement windows and the
per-window full-corpus iteration count. The count is a real benchmark-identity
axis — the weekly lane sizes it so three windows fill roughly an hour on the
pinned host, and a shorter definition hashes to a different benchmark, so
abbreviated soaks can never masquerade as the scheduled one.

After one excluded warm frame per scene, each window records resident-set size
through `fmn-platform`'s `current_rss_bytes` capability (Linux `VmRSS`),
renders the full corpus for the window's iterations with every frame identity
re-checked against its warm digest, and retains `rss_end - rss_start` (floored
at zero) as one valid `leaked-bytes` sample. The exact policy requires every
window to report zero. Hosts without a residency capability (macOS, Windows,
wasm) retain all three samples as invalid with reason `rss-unsupported-host`
and skip the burn: the gate is inconclusive there, never synthesized. The
`fmn-perf-pg6-soak-trace/1` artifact retains each window's start/end resident
bytes, leaked bytes, and iteration count. `fmn-perf measure-pg6-soak` is the
producing front door.

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
