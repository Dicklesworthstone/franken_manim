# WASM-target audit of the governed closure (fm-7wm.4, R15)

**Status:** the final MathML consumer snapshot (`3312ad47` plus the reviewed
patch, provider `33ff7d7db9a671b92c34fb8e55f79e498797cb28`) was audited on
2026-11-08 through all four original commands, owned-host DSR/RCH
`20261108T131544-3352414`. The locked normal tree, WASM target build and
release Node smoke exit 0. Node retains digest `1f248a71347b82aa`,
`process=capability-absent`, `topology=1-cpu`, `semantic_oracles=111111`,
and `mirrored=101000`.

The original package command builds the serial and shared-memory artifacts
with its required tools, then exits 1: the serial bundler WASM is
**1,021,982 bytes**, over the unchanged **514,521-byte** budget. npm and
browser checks are **unreached**. The combined four-command gate remains
**failed**; fm-8j70 remains open. This dirty-source diagnostic does not qualify
a release, an installed Python portal, or browser-native typography.

The frozen input manifest is
`/run/user/1000/fmn-execute-20261108-review8-mathml-consumer-inputs-v4.json`,
SHA-256 `890ddc36d3f88c7fe7bde5756f64d9ec5d90ddd6922c3956da7a7ab74606b64b`.
All 8,116 input rows, executable bits, HEAD and index stay unchanged across
execution, as does DSR's source fingerprint. The actual package artifacts are
under `/data/ssd/tmp/fmn-execute-20261108-review8-mathml-consumer-wasm-package-v2`.
The current three lock hashes below bind this audit; the auxiliary smoke lock
is unchanged. This audit does not qualify native typography in the browser.

The preceding ellipsis audit (`b6cc942`, DSR `20261108T111527-2370711`)
also exited 0/0/0/1 with the same package size. Its frozen manifest
`/run/user/1000/fmn-execute-20261108-review8-ellipsis-consumer-inputs-v2.json`
has SHA-256 `20c63bd0d64d7bb9ceb6984631aea322c6b36ad32649c8a1e2965f2fb2695689`;
its artifacts and recorded lock hashes remain historical evidence.

**Previous boxed-display snapshot:** (`3312ad47` plus the reviewed
patch, provider `7840e8e0`) was audited on 2026-11-08 through the original four
commands, owned-host DSR/RCH `20261108T065411-658201/1`. The locked normal tree, WASM target build and
release Node smoke exit 0. Node `v22.22.1` reports digest `1f248a71347b82aa`,
`process=capability-absent`, `topology=1-cpu`, `semantic_oracles=111111`, and
`mirrored=101000`.

The original package command now runs with its exact required tools:
wasm-pack 0.15.0, wasm-bindgen 0.2.127, wasm-opt 117, webpack 5.109.2 and
webpack-cli 7.2.2. It builds both serial and shared-memory artifacts, then
exits 1: the serial bundler WASM is **1,021,982 bytes**, over the unchanged
**514,521-byte** budget. npm packaging and browser checks are **unreached**.
The complete four-command gate remains **failed**; this dirty-source diagnostic
does not qualify a release.

All 8,114 complete input rows, executable bits, HEAD and index remain unchanged
across execution, as does DSR's source fingerprint. The input manifest is
`/run/user/1000/fmn-execute-20261108-review7-boxed-consumer-inputs-v1.json`
(SHA-256 `99836180aa7368e509c6ebaf22a319fcecaa57869f724517976031ea7e372f74`).
The 801 selected compiled/data inputs and the three audited locks match the
primary checkout. Logs are in the named DSR run; actual artifacts remain at
`/data/ssd/tmp/fmn-execute-20261108-review7-boxed-consumer-ownedhost-wasm-package-v1`.
RCH used its supported private local configuration; no worker-receipt or
clean-source release qualification is claimed. The earlier November 7 run
(`20261107T055826-2902599/1`) separately failed the same size budget at
1,015,022 bytes. Neither failure is waived; fm-8j70 remains open.

Re-run, 2026-11-08, against `Cargo.lock` `553d4849`, `SUITE.lock` `bab43ba6` and
`wasm-smoke/Cargo.lock` `e307b83a`, for the franken_numpy repin from `a15d5c32` (fnp 0.2.0)
to `6492ff31` (fnp 0.5.0). Every governed lock's delta is exactly its fnp packages
(`fnp-dtype`, `fnp-ndarray`, `fnp-random-core`, and `fnp-io` in `Cargo.lock`) moving
0.2.0 -> 0.5.0, plus `fnp-io`'s new edge to `fnp-dtype`; no other package, version,
source or checksum changes. fnp is in the wasm graph, so this re-run covers it.

- **VERIFIED (mechanical):** `cargo tree -p fmn-wasm --target wasm32-unknown-unknown -e
  normal --prefix none --locked` gives 134 lines; against the asupersync re-run below,
  the path-normalized difference is exactly the `fnp-random-core`, `fnp-dtype` and
  `fnp-ndarray` lines (0.2.0 at `a15d5c32` -> 0.5.0 at `6492ff31`).
- **VERIFIED (build):** `cargo build --locked -p fmn-wasm --target wasm32-unknown-unknown`
  exits 0 on the macos-aarch64 dev host.
- **VERIFIED (execution):** `wasm-smoke/run.sh` exits 0 under Node `v26.8.1` with the
  unchanged digest `1f248a71347b82aa`, `process=capability-absent`, `topology=1-cpu`,
  `semantic_oracles=111111`, `mirrored=101000`.
- Not re-run: the package/browser gate (`wasm-pack` is not installed on this host). This
  is no package, browser or size-budget pass.

Earlier re-run, 2026-11-08, against `Cargo.lock` `8067eb7d` and `SUITE.lock` `68c4edc4`, for the
frankentorch repin from `523aaf82` to `c611501c` (ft-kernel-metal 0.1.0 -> 0.1.1). The
`Cargo.lock` delta is that one package plus its new edge to `half` 2.7.1, which the lock
already held; `wasm-smoke/Cargo.lock` is unchanged. ft-kernel-metal enters only through
fmn-render's `metal` feature, which no wasm graph enables.

- **VERIFIED (mechanical):** the path-normalized `cargo tree -p fmn-wasm --target
  wasm32-unknown-unknown -e normal --prefix none --locked` is byte-identical to the
  asupersync re-run below (134 lines).
- **VERIFIED (build):** `cargo build --locked -p fmn-wasm --target wasm32-unknown-unknown`
  exits 0 on the macos-aarch64 dev host.
- Not re-run: the wasm-smoke Node execution (its lock and graph are unchanged) and the
  package/browser gate. This is no fresh smoke, package, browser or size-budget pass.

Earlier re-run, 2026-11-08, against `Cargo.lock` `94d01d32`, `SUITE.lock` `56b01581` and
`wasm-smoke/Cargo.lock` `1a623bb7`, for the asupersync repin from `c48399f2` (0.3.10)
to the `v0.6.2` tag `a5d23e8d`. Both lock deltas are exactly the four asupersync-repo
packages (`asupersync` 0.3.10 -> 0.6.2; `franken-decision`, `franken-evidence` and
`franken-kernel` 0.3.10 -> 0.6.0) with no other package, version, source or checksum
change. asupersync is gated to `cfg(not(target_arch = "wasm32"))` in fmn-platform,
so none of the four is reachable from a wasm graph. Run on the macos-aarch64 dev host
(Apple M5 Ultra) with the pinned nightly.

- **VERIFIED (mechanical):** `cargo tree -p fmn-wasm --target wasm32-unknown-unknown -e
  normal --prefix none --locked` gives 134 lines with no asupersync-repo package, and
  the wasm-smoke tree likewise contains none.
- **VERIFIED (build):** `cargo build --locked -p fmn-wasm --target wasm32-unknown-unknown`
  exits 0.
- **VERIFIED (execution):** `wasm-smoke/run.sh` exits 0 under Node `v26.8.1` with the
  unchanged digest `1f248a71347b82aa`, `process=capability-absent`, `topology=1-cpu`,
  `semantic_oracles=111111`, `mirrored=101000`.
- Not re-run: the package/browser gate (`wasm-pack` is not installed on this host, and
  the gate already fails its size budget as recorded above). This is no package,
  browser or size-budget pass.
- Before this re-run the lock markers at the foot of this audit had not been re-bound
  since `SUITE.lock` `f855c38e` / `Cargo.lock` `19af6e49`; the HEAD it starts from
  (`20f6db41`, `SUITE.lock` `5b4792e2`, `Cargo.lock` `896d4a40`) already failed
  `wasm_audit_is_bound_to_current_locks`. This re-run re-bound the markers to its locks.

Earlier, the timing/audio merge (`fd50648d`) was also attempted through
the original four commands, DSR `20261105T152601-24491/1`. Its frozen 4,413-entry
source is `b27d0bb7489b4aa53d8aca30f4b8279a0a2765f68901242f8d04bee3d0cfde23`;
all source, 19,163 SDK/test inputs and audited tools stayed unchanged. Locked
tree, WASM target and Node smoke exit 0; digest and semantic flags below remain
unchanged. The package/browser command exits 1 for missing `wasm-pack`, so this
whole gate remains failed. Its actual outputs are under
`.rch-results/fmn-execute-20261105-underline-consumer-wasm-current-merge-v1-122/`
in the same isolated checkout. Normal RCH recovery of
`rchw-f0993c36-03cf-4439-9756-512d757524b9` retrieves the original output and
acknowledges native exit 1; it does not replay or convert this to a pass.
The three lock hashes at the foot of this audit are unchanged by that merge.

Earlier re-run, 2026-11-05, against `Cargo.lock` `0125bee5` and `SUITE.lock`
`463ee0b7` (underline candidate and merged timing repair). The original locked
normal tree, WASM target build and release Node smoke exit 0 on `vmi1227854`.
All 4,411 product entries remain unchanged at
`de20026c37c0027fb50354c309ec8147ea7f2806ab5f7313ba67a30bc78a4504`, as do
19,163 SDK/test inputs and all audited scanner inputs. Execution is at
`d33ad52f` plus the reviewed patch; native Git archive independently matches
all those product bytes to subsequently observed commit `7096eeba`.

Node `v22.22.1` retains digest `1f248a71347b82aa`,
`process=capability-absent`, `topology=1-cpu`, `semantic_oracles=111111`,
`mirrored=101000`. The normal tree still excludes native typography and Python.
The fourth, unchanged package/browser command actually exits 1 because
`wasm-pack` is unavailable. No package, browser, shared-memory or size result
exists for this run. The whole four-command gate remains **failed**.

Evidence: `.rch-results/fmn-execute-20261105-underline-consumer-wasm-v1-122/`
in `/data/projects/franken_manim-underline-consumer-20261105`, DSR
`20261105T151426-3973835/1`. Initial RCH retrieval stopped at its staging identity
guard; normal recovery of `rchw-c9728a0b-392d-4293-b789-5d178cf36855` retrieved
the original outputs and acknowledged terminal native exit 1 without replay.
The earlier frontend-only phase-guard failure is retained separately.
No assertion, golden, workload, budget or release gate was relaxed.

Earlier re-run, 2026-11-05, against `Cargo.lock` `0b8d890a` and `SUITE.lock`
`2783427a` (fm-sup-accented-nucleus-r12-a529 and fm-7ojgl). The locked
normal tree, WASM target build and unchanged release Node smoke each exit 0
through ordinary DSR/RCH on `vmi1153651`. All 4,410 product source files,
19,163 SDK/test inputs and audited scanner tools stayed unchanged. Source
aggregate: `b8610c30c855ce7adaaa354f925c130c774ad353cd61fecaa30b97df603488db`.
The tested checkout is main `39139cf1` with the reviewed consumer overlays;
this is not a clean-source or complete native source-policy qualification.

Node `v22.22.1` reports digest `1f248a71347b82aa`,
`process=capability-absent`, `topology=1-cpu`, `semantic_oracles=111111`,
`mirrored=101000`. The 134-line normal tree excludes native typography and
Python. Each of the three changed locks retains its package versions,
checksums and dependency edges; only three provider Git identities change.
The font manifest changes only its provider revision, with all font and
license bytes fixed. The auxiliary WASM lock remains byte-identical.

The original affected package/browser gate was attempted and exits 1 at
admission: `required WASM package-gate tool is unavailable: wasm-pack`.
No package, browser, shared-memory or size result was produced. The font
manifest remains affected packaged data; that gate, the historical size
failure and fm-8j70 remain open. The combined DSR run is **failed**.

Evidence: `.rch-results/fmn-execute-20261105-script-metrics-consumer-wasm-v1-135/`
in `/data/projects/franken_manim-a529-consumer-20261105`,
DSR `20261104T212850-1149580`. No assertion, output golden, workload or budget
changed for this audit.

Earlier re-run, 2026-11-04, against `Cargo.lock` `8608f187` and `SUITE.lock`
`e98d676b` (fm-j4tni). The locked normal tree, WASM target build and unchanged
release Node smoke each exit 0 through ordinary DSR/RCH on `vmi1227854`.
All 4,410 source files and the three audited locks stayed unchanged throughout
the run; source aggregate
`7ab03063fa16f6124dd18e4eb6d64ec528a857260970989c168cf54267f7f5a1`.
This is a WIP audit, not clean-source or complete native qualification.

Node `v22.22.1` reports digest `1f248a71347b82aa`,
`process=capability-absent`, `topology=1-cpu`, `semantic_oracles=111111`,
`mirrored=101000`. The 134-line normal tree excludes native typography and
Python. Only the three Markdown-provider source identities change in the
root lock; versions, dependency edges and checksums remain fixed. The font
manifest changes only its provider revision; all font and license bytes stay
fixed. The auxiliary WASM lock is byte-identical.

The original affected package/browser gate was attempted and exits 1 at tool
admission: `required WASM package-gate tool is unavailable: wasm-pack`.
No package, browser, shared-memory or size result was produced. The font
manifest remains affected packaged data, and the historical size failure and
fm-8j70 remain open. The combined DSR run is **failed**.

Evidence: `.rch-results/fmn-execute-20261104-j4tni-wasm-audit-v1-worker122/`
in `/data/projects/franken_manim-j4tni-consumer-20261104`,
DSR `20261104T153313-3265678`. No assertion, output golden or budget changed.

Earlier re-run, 2026-11-03, against `Cargo.lock` `6778419d` and `SUITE.lock`
`a17fa45e` (fm-dbinom-in-scripts-r12-x6zi). The locked normal tree, WASM
target build and unchanged release Node smoke each exit 0 through normal
DSR/RCH on `vmi1152480`. This audit includes the concurrent renderer/frame
changes on `ac26152d`, plus the owned provider-pin changes. All 4,408 source
files matched the primary checkout when staged; the WIP source aggregate
`9bc98b855ab2ec1d975e4b9cbae5ee315555d504c73535c28eb17780397d47b2`
and all three locks remained unchanged throughout execution. This is not
a clean-source or complete native source-gate qualification.

Node `v22.22.1` again reports digest `1f248a71347b82aa`,
`process=capability-absent`, `topology=1-cpu`, `semantic_oracles=111111`,
`mirrored=101000`. The normal tree excludes native typography and Python.
Only the three provider Git identities change in the root lock; versions,
dependency edges and checksums remain fixed. The two direct fuzz pins now
also select the same provider, without introducing duplicate packages.

The original affected package/browser gate exits 1 at tool admission:
`required WASM package-gate tool is unavailable: wasm-pack`. No package,
browser, shared-memory or size result was produced. The font manifest is
affected packaged data; the package gate, historical size failure and
fm-8j70 remain open. The combined DSR run is therefore **failed**.

Evidence: `.rch-results/fmn-execute-20261103-dbinom-wasm-audit-v2-worker115/`
inside `fmn-execute-20261103-dbinom-final-source-v1`,
DSR `20261103T184224-733014`. No assertion, gate, output golden or budget
changed for this audit.

Earlier re-run, 2026-11-03, against `Cargo.lock` `4948c712` and `SUITE.lock`
`94783488` (fm-q364). At clean source `eed97585ec5a950de67192837cf2c5e41fa713af`,
the locked normal tree, WASM target build and unchanged release Node smoke
each exit 0 through normal DSR/RCH on `vmi1152480`. The 134-line tree excludes
the native typography and Python crates. Node `v22.22.1` reports digest
`1f248a71347b82aa`, `process=capability-absent`, `topology=1-cpu`,
`semantic_oracles=111111`, `mirrored=101000`.

The unchanged affected package/browser gate was also attempted. It exits 1
at admission: `required WASM package-gate tool is unavailable: wasm-pack`.
It produces no package, browser, shared-memory or size result. The historical
size failure and fm-8j70 remain open; this is not a release pass. The combined
DSR run is therefore **failed**, not green.

Evidence: `.rch-results/fmn-execute-20261103-q364-wasm-audit-v1-worker115/`
inside the frozen `fmn-execute-20261103-q364-consumer-qualified-source-v1`
checkout, DSR `20261103T165129-4017611`. All 4,408 product source files and
the three lockfiles were unchanged across the run; source aggregate SHA-256
`3ad0e10a0c76db85df74811cb6ad4d75d9b49b293cb7564654f6e16d7c784eeb`.
The root lock delta changes only the three franken_markdown source revisions
from `bde70a76` to `d2450292`; package versions, dependency edges, checksums
and the auxiliary WASM lock are unchanged. The refreshed font manifest
remains affected packaged data, so the missing package/browser checks stay
blocked rather than being declared unaffected.

Earlier re-run, 2026-11-03, against `Cargo.lock` `7f7994e5` and `SUITE.lock`
`bac64bf5` (fm-jefi). The locked tree, target build and Node execution pass;
the affected package/browser gate is blocked by missing pinned tools. This is
not a current package, browser, shared-memory, size-budget or release pass.

- **VERIFIED (mechanical):** the locked normal `fmn-wasm` tree has 134 lines
  and excludes `fmd-font`, `fmd-math`, `franken_markdown`, PyO3 and the native
  text, TeX, library and Python crates. The root lock delta from `b19d34da`
  changes only three franken_markdown source identities, from `e7b4309d` to
  `bde70a76`; package versions, dependencies, features and checksums are
  unchanged. The auxiliary WASM lock is byte-identical.
- **VERIFIED (build):** `cargo build --locked -p fmn-wasm --target
  wasm32-unknown-unknown` exits 0 through normal RCH on `ovh-b`, using the
  pinned `nightly-2026-08-31` and a fresh private compiler target.
- **VERIFIED (execution):** the unchanged `bash wasm-smoke/run.sh` builds
  its locked release probe and executes it in Node `v22.22.1`: digest
  `1f248a71347b82aa`, `process=capability-absent`, `topology=1-cpu`,
  `semantic_oracles=111111`, `mirrored=101000`. All three audit stages exit 0.
- **BLOCKED (package/browser):** the unchanged
  `bash scripts/check_wasm_package.sh` was attempted and exits 1 at tool
  admission: `required WASM package-gate tool is unavailable: wasm-pack`.
  `wasm-bindgen` and `wasm-opt` are also absent on this host. No package,
  Chromium, shared-memory runtime or size result was produced. The updated
  font manifest is packaged data, and shared Rust sources changed since the
  October 31 audit, so those checks cannot be called unaffected. The
  historical size failure and fm-8j70 remain open.
- Evidence: `.rch-results/fmn-execute-20261103-jefi-wasm-audit-v10-ovhb/`
  contains the tree, lock identities, native logs and source audit;
  `.rch-results/fmn-execute-20261103-jefi-wasm-package-v11-ovhb/` retains the
  failed package attempt. DSR receipts are respectively
  `20261103T052848-4030534` and `20261103T053450-31171`. All 4,406 product
  source files remain unchanged across both jobs, aggregate SHA-256
  `571e73232ba3ac45c69a18138d3c91fce09f8382344fb29c618ad8589a4f0db7`.
  The private checkout is `b19d34da` with reviewed overlays matching primary
  `c753ea84`; it is not a clean-source release qualification.

Earlier re-run, 2026-10-31, against `Cargo.lock` `01a419e5` and `SUITE.lock`
`908c6e62` (fm-restore-green-ci-tqsr.14). Commit `0c1b3483` adds exactly one
lock edge, `fmn -> fmn-cache`; it adds no package, version, checksum, feature or
WASM-manifest change. The host facade `fmn` is absent from the shipped WASM
dependency graph; `fmn-cache` was already reached there. The serial and
shared-memory artifacts use the same crate dependency graph, so this host-only
edge changes neither browser closure.

- **VERIFIED (mechanical):** the current locked `fmn-wasm` normal tree has
  134 lines and excludes the host facade. The complete lock/manifest diff from
  the previous audit contains only the edge above.
- **VERIFIED (build):** `cargo build --locked -p fmn-wasm --target
  wasm32-unknown-unknown` exits 0 through RCH on `vmi1149989`.
- **VERIFIED (execution):** the unchanged `bash wasm-smoke/run.sh` builds its
  locked release probe and executes it in Node: digest `1f248a71347b82aa`,
  `process=capability-absent`, `topology=1-cpu`, `semantic_oracles=111111`,
  `mirrored=101000`. The combined audit job exits 0.
- Evidence: `.rch-results/fmn-execute-20261030-wasm-audit/` contains the tree,
  all three lock digests, Node output and source boundary. The checkout was
  `7fba6606` with only tracker and CLI-test overlays, both outside the reached
  WASM graph. The original log is
  `/data/tmp/fmn-execute-20261030-wasm-audit-build.log`.
- Not re-run: package/Chromium, shared-memory runtime or size-budget gates.
  No browser/package dependency is affected by this lock delta; this receipt
  does not establish those release properties or cross-platform certification.

Earlier re-run, 2026-10-29, against `Cargo.lock` `722725cc` and `SUITE.lock` `908c6e62`,
for the franken_markdown repin from `4f2bcc95` to `e7b4309d` (fm-fmd-math-fuzz-budgets-k8fe:
fmd-math's named resource budgets, a layout recursion that fits a 2 MiB stack, and physics
arguments parsed once, plus nine Markdown-engine commits and a docs fix on franken_markdown's
main).
The lock delta changes only the franken_markdown Git revisions; no package, version, checksum or
WASM manifest changes.

- **VERIFIED (mechanical):** `cargo tree -p fmn-wasm --target wasm32-unknown-unknown -e
  normal --prefix none --locked` gives 134 lines, byte-identical to the tree at `4f2bcc95`
  once each workspace path is replaced by one placeholder, with no fmd, franken_markdown, PyO3
  or fmn-python package.
- **VERIFIED (build):** `cargo build --locked -p fmn-wasm --target wasm32-unknown-unknown`
  passes through RCH at this tree.
- Not re-run: the wasm-smoke Node execution and the package/browser gate. fmn-wasm's tree does
  not reach franken_markdown. This is no fresh smoke, package, browser or size-budget pass.

Earlier re-run, 2026-10-28, against `Cargo.lock` `e854a472` and `SUITE.lock` `491d0436`,
for the franken_markdown repin from `04d811db` to `4f2bcc95` (fm-script-greek-capitals-r12-nbcf:
script and scriptscript Greek capitals take cmr7's and cmr5's own designs).
The lock delta changes only the franken_markdown Git revisions; no package, version, checksum or
WASM manifest changes.

- **VERIFIED (mechanical):** `cargo tree -p fmn-wasm --target wasm32-unknown-unknown -e
  normal --prefix none --locked` through RCH gives 134 lines, byte-identical to the tree at
  `04d811db`, with no fmd, franken_markdown, PyO3 or fmn-python package.
- **VERIFIED (build):** `cargo build --locked -p fmn-wasm --target wasm32-unknown-unknown`
  passes through RCH at this tree.
- Not re-run: the wasm-smoke Node execution and the package/browser gate. fmn-wasm's tree does
  not reach franken_markdown. This is no fresh smoke, package, browser or size-budget pass.

Earlier re-run, 2026-10-27, against `Cargo.lock` `acaa8946` and `SUITE.lock` `bd60055e`,
for the franken_markdown repin from `450111a4` to `04d811db` (fm-fm-delim-size-chain-r11-khsh:
delimiters walk TeX's size chain, null delimiters unscaled, a radical's bar its sign's height).
The lock delta changes only the franken_markdown Git revisions; no package, version, checksum or
WASM manifest changes.

- **VERIFIED (mechanical):** `cargo tree -p fmn-wasm --target wasm32-unknown-unknown -e
  normal --prefix none --locked` through RCH gives 134 lines, byte-identical to the tree at
  `450111a4` once each workspace path is replaced by one placeholder, with no fmd,
  franken_markdown, PyO3 or fmn-python package.
- **VERIFIED (build):** `cargo build --locked -p fmn-wasm --target wasm32-unknown-unknown`
  passes through RCH at this tree.
- Not re-run: the wasm-smoke Node execution and the package/browser gate. fmn-wasm's tree does
  not reach franken_markdown. This is no fresh smoke, package, browser or size-budget pass.

Earlier re-run, 2026-10-27, against `Cargo.lock` `716d4655` and `SUITE.lock` `41856fe9`,
for the franken_markdown repin from `cc470d1a` to `450111a4` (fm-texttext-line-breaking-r11-bfdx:
TexText paragraphs break at the 345 pt line, a blank line ends a paragraph, alignment blocks
keep their first baseline). The lock delta changes only the franken_markdown Git revisions; no
package, version, checksum or WASM manifest changes.

- **VERIFIED (mechanical):** `cargo tree -p fmn-wasm --target wasm32-unknown-unknown -e
  normal --prefix none --locked` through RCH gives 134 lines, byte-identical to the tree at
  `cc470d1a` once each workspace path is replaced by one placeholder, with no fmd,
  franken_markdown, PyO3 or fmn-python package.
- **VERIFIED (build):** `cargo build --locked -p fmn-wasm --target wasm32-unknown-unknown`
  passes through RCH at this tree.
- Not re-run: the wasm-smoke Node execution and the package/browser gate. fmn-wasm's tree does
  not reach franken_markdown. This is no fresh smoke, package, browser or size-budget pass.

Earlier re-run, 2026-10-26, against `Cargo.lock` `ba9ed445` and `SUITE.lock` `a87e525e`,
for one dev-only lock change (fm-counting-allocator-harness-r4-ndz8): `stats_alloc 0.1.10`
enters as an ADR-0003 class=dev package of fmn-conformance's test binaries. No shipped package,
version, checksum or WASM manifest changes.

- **VERIFIED (mechanical):** `cargo tree -p fmn-wasm --target wasm32-unknown-unknown -e
  normal --prefix none --locked` through RCH gives 134 lines, byte-identical to the tree at
  `cc470d1a` once each workspace path is replaced by one placeholder, with no stats_alloc, fmd,
  franken_markdown, PyO3 or fmn-python package.
- **VERIFIED (build):** `cargo build --locked -p fmn-wasm --target wasm32-unknown-unknown`
  passes through RCH at this tree.
- Not re-run: the wasm-smoke Node execution and the package/browser gate. A dev-dependency of
  the conformance crate cannot reach them. This is no fresh smoke, package, browser or
  size-budget pass.

Earlier re-run, 2026-10-26, against `Cargo.lock` `4b933aa3` and `SUITE.lock` `a87e525e`,
for the franken_markdown repin from `d99fee96` to `cc470d1a` (fm-fm-physics-trig-parens-r11-nou8:
physics' function arguments in the default pack, TeX's class primitives and operator names'
italic correction; franken_markdown's image/PDF commits `b692f04` and `29eb12a` ride along with
no dependency change). The lock delta changes only the franken_markdown Git revisions; no
package, version, checksum or WASM manifest changes.

- **VERIFIED (mechanical):** `cargo tree -p fmn-wasm --target wasm32-unknown-unknown -e
  normal --prefix none --locked` through RCH gives 134 lines, byte-identical to the tree at
  `d99fee96` once each workspace path is replaced by one placeholder. It has no fmd,
  franken_markdown, PyO3 or fmn-python package.
- **VERIFIED (build):** `cargo build --locked -p fmn-wasm --target wasm32-unknown-unknown`
  passes through RCH at this tree.
- The font manifest is unchanged at 29 faces: this pin changes layout code only.
- Not re-run: the wasm-smoke Node execution and the package/browser gate. Their source and
  complete dependency graph are unchanged by this pin. This is no fresh smoke, package,
  browser or size-budget pass.

Earlier re-run, 2026-10-26, against `Cargo.lock` `07c8a6d8` and `SUITE.lock` `76c506fe`,
for the franken_markdown repin from `8667819a` to `d99fee96` (fm-fm-long-arrows-joinrel-r11-b2c5:
the long arrows from CM pieces and `\binrel@`'s class for `\overset`/`\underset`;
fm-fm-amsmath-dots-context-r11-ymdd: amsmath's dots read the next token). The lock delta
changes only the franken_markdown Git revisions; no package, version, checksum or WASM
manifest changes.

- **VERIFIED (mechanical):** `cargo tree -p fmn-wasm --target wasm32-unknown-unknown -e
  normal --prefix none --locked` through RCH gives 134 lines, byte-identical to the tree at
  `8667819a` once each workspace path is replaced by one placeholder (SHA-256 `2b47e553`
  both). It has no fmd, franken_markdown, PyO3 or fmn-python package.
- **VERIFIED (build):** `cargo build --locked -p fmn-wasm --target wasm32-unknown-unknown`
  passes through RCH at this tree.
- The font manifest is unchanged at 29 faces: this pin changes layout code only.
- Not re-run: the wasm-smoke Node execution and the package/browser gate. Their source and
  complete dependency graph are unchanged by this pin. This is no fresh smoke, package,
  browser or size-budget pass.

Earlier re-run, 2026-10-26, against `Cargo.lock` `e6b4be94` and `SUITE.lock` `f6c4b865`,
for the franken_markdown repin from `4b705d14` to `8667819a` (fm-ew7o: `\dddot` as amsmath's
operator under a row of text periods, and amsgen's `\ex@` without `powf`; franken_markdown's PDF baseline-grid, compression and
pagination commits ride along with no dependency change). The lock delta changes only the
franken_markdown Git revisions; no package, version, checksum or WASM manifest changes.

- **VERIFIED (mechanical):** `cargo tree -p fmn-wasm --target wasm32-unknown-unknown -e
  normal --prefix none --locked` through RCH gives 134 lines, byte-identical to the tree at
  `4b705d14` once each workspace path is replaced by one placeholder. It has no fmd,
  franken_markdown, PyO3 or fmn-python package.
- **VERIFIED (build):** `cargo build --locked -p fmn-wasm --target wasm32-unknown-unknown`
  passes through RCH at this tree.
- The font manifest is unchanged at 29 faces: this pin changes layout code only.
- Not re-run: the wasm-smoke Node execution and the package/browser gate. Their source and
  complete dependency graph are unchanged by this pin. This is no fresh smoke, package,
  browser or size-budget pass.

Earlier re-run, 2026-10-26, for the franken_markdown repin from `37632d2d` to `4b705d14`:
`Cargo.lock` `16ccf642` and `SUITE.lock` `a2daacdf` (fm-eo8l: amsmath's `\mod` and `\pmod`
kerns follow `\if@display`). The lock delta changes only the franken_markdown Git revisions;
no package, version, checksum or WASM manifest changes.

- **VERIFIED (mechanical):** `cargo tree -p fmn-wasm --target wasm32-unknown-unknown -e
  normal --prefix none --locked` through RCH gives 134 lines, byte-identical to the tree at
  `37632d2d` once each workspace path is replaced by one placeholder. It has no fmd,
  franken_markdown, PyO3 or fmn-python package.
- **VERIFIED (build):** `cargo build --locked -p fmn-wasm --target wasm32-unknown-unknown`
  passes through RCH at this tree.
- The font manifest is unchanged at 29 faces: this pin changes layout code only.
- Not re-run: the wasm-smoke Node execution and the package/browser gate. Their source and
  complete dependency graph are unchanged by this pin. This is no fresh smoke, package,
  browser or size-budget pass.

Earlier re-run, 2026-10-26, for the franken_markdown repin from `2d820c52` to `37632d2d`:
`Cargo.lock` `d251f51d` and `SUITE.lock` `37286bbc` (fm-0byq: LaTeX's fractions as Ord atoms,
and amssymb's names on their authored AMS glyphs). The lock delta changes only the
franken_markdown Git revisions; no package, version, checksum or WASM manifest changes.

- **VERIFIED (mechanical):** `cargo tree -p fmn-wasm --target wasm32-unknown-unknown -e
  normal --prefix none --locked` through RCH gives 134 lines, byte-identical to the tree at
  `e6da6ee1` and `2d820c52` once each workspace path is replaced by one placeholder. It has no
  fmd, franken_markdown, PyO3 or fmn-python package.
- **VERIFIED (build):** `cargo build --locked -p fmn-wasm --target wasm32-unknown-unknown`
  passes through RCH at this tree.
- The font manifest is unchanged at 29 faces: this pin changes layout code only.
- Not re-run: the wasm-smoke Node execution and the package/browser gate. Their source and
  complete dependency graph are unchanged by this pin. This is no fresh smoke, package,
  browser or size-budget pass.

Earlier re-run, 2026-10-26, for the franken_markdown repin from `e6da6ee1` to `2d820c52`:
`Cargo.lock` `2ee6d41d` and `SUITE.lock` `b0cec729` (fm-tex-constructs-t2-1wfq:
`\mathfrak` from Euler Fraktur, the cancel package's strokes, display `align*`'s spread
pairs). The lock delta changes only the franken_markdown Git revisions; no package,
version, checksum or WASM manifest changes.

- **VERIFIED (mechanical):** `cargo tree -p fmn-wasm --target wasm32-unknown-unknown -e
  normal --prefix none --locked` through RCH gives 134 lines, byte-identical to the same
  command at `e6da6ee1` once each workspace path is replaced by one placeholder (SHA-256
  prefix `2b47e553b5b86be8`). It has no fmd, franken_markdown, PyO3 or fmn-python package.
- **VERIFIED (build):** `cargo build --locked -p fmn-wasm --target wasm32-unknown-unknown`
  passes through RCH at this tree.
- **Distribution boundary:** the native font manifest now has 29 faces. The three Euler
  Fraktur faces (58,904 bytes) are native-only; no font enters the Wasm graph, so the
  package size budget (fm-8j70) is untouched by them.
- Not re-run: the wasm-smoke Node execution and the package/browser gate. Their source and
  complete dependency graph are unchanged by this pin. This is no fresh smoke, package,
  browser or size-budget pass.

Earlier re-run, 2026-10-25, against `Cargo.lock` `01520fc6` and `SUITE.lock` `4e769b6b`,
for the franken_markdown repin from `c81208f1` to `e6da6ee1` (fm-tex-constructs-t2-1wfq:
the `\TeX` and `\LaTeX` logos and `\mathchoice`). The lock delta changes only the
franken_markdown Git revisions; no package, version, checksum or WASM manifest changes.

- **VERIFIED (mechanical):** a strict RCH metadata job derives a 134-line normal-edge
  wasm32 tree identical to the one derived at `439a910c` (the same SHA-256 once the
  workspace path is replaced by one placeholder), with no fmd, franken_markdown, PyO3 or
  fmn-python package.
- **VERIFIED (build):** a pinned-nightly `cargo build --locked -p fmn-wasm --target
  wasm32-unknown-unknown` passes through RCH at this tree.
- Not re-run: the wasm-smoke Node execution and the package/browser gate. Their source and
  complete dependency graph are unchanged by this pin, which is no fresh smoke, package,
  browser or size-budget pass.

Earlier re-run: foundation re-run, 2026-10-25, at source
`f8eb01f3db11b4cc2dc217416b2f89aee322bb41`, with the native AMS and structured
macro-overlay provider `c81208f19a81103daf41aa1d8e4a6153c22f7b71`.
The current lock identities are recorded below. This is a native typesetting
pin change: the three provider Git sources move, with no package/version,
checksum, feature or dependency-edge change. No fmd, franken_markdown,
fmn-python or PyO3 package enters the browser execution graph.

- **VERIFIED (mechanical):** actual locked before/after normal-edge wasm32
  trees have the same134 rows after replacing workspace paths only, SHA-256
  `d580244094a527237784d5bdd5ac2b82ecb2c8661659f6d1040c4c2649c47177`.
  The browser Rust/JS sources are unchanged by this native increment.
- **VERIFIED (build):** fresh locked release builds of `fmn-wasm` and the
  separate `wasm-smoke` probe pass through strict RCH on ts1, bound to clean
  source f8eb01f3 by their clean-overlay receipts. No local Rust build ran.
- **VERIFIED (execution):** the returned raw smoke helper is536078bytes,
  SHA-256`cb086493fec936bfaebd0c06980240cd337474c42ea86842e4432c88d9bda6dc`,
  matching the worker bytes. It passes Node22.22.1 on Linux and26.8.1 on
  macOS: digest`1f248a71347b82aa`, semantic masks111111/101000, real host
  clocks, named process-capability refusal and the one-CPU topology shim.
- **Distribution boundary:** the native font manifest has26 faces; all20
  earlier face records and all five raw license notices remain unchanged.
  Its six additional font records do not put those fonts in the Wasm graph.
- **Package/browser BLOCKED at preflight:** the actual package script exits1
  because `webpack` is unavailable, before compilation, package construction
  or Chromium. The available wasm-pack0.13.1, wasm-bindgen0.2.128 and
  Binaryen125 also differ from its governed0.15.0/0.2.127/117 tool pins.
  This is a host-tooling limitation, not a fresh product-size result. The
  historical555759-versus514521-byte package breach (fm-8j70) remains open;
  the536078-byte smoke helper is a separate artifact. No current npm,
  serial/shared-memory package, browser, size-budget or certification pass
  is inferred from these foundation checks.

Earlier re-run, 2026-10-25, at source
`dd56137d8bea843f906b6934c1ba067e4bba9f17`, against `Cargo.lock` `94652d0c`,
`SUITE.lock` `ebfd3073`, and auxiliary lock `93a7c2dc`. The lock security
delta updates only `ctutils` from0.4.2 to0.4.4 for RUSTSEC-2026-0385. The
reviewed archive checksum, unchanged dependency/features/licenses, absent
build script and `forbid(unsafe_code)` posture are in the governed allowlist.
This integration also changes geometry and Stage family reversal; unchanged
package membership alone does not qualify that code.

- **VERIFIED (mechanical):** actual before/after normal-edge trees have134
  identical rows after replacing workspace paths only, SHA-256
  `1face7d9ec9ca0ffba2e3f5bd826c2de43f5c0afcc804e02e2a1992db958e95c`.
  `ctutils` is absent from that execution graph. DD retains that package tree;
  the geometry/object implementation changes are covered by fresh builds.
- **VERIFIED (build):** pinned-nightly wasm32 target and separate locked
  release-smoke builds pass through strict RCH at DD, with successful terminal
  acknowledgements and source-bound clean-overlay receipts. Earlier trj
  delivery failures and a ts1 queue timeout remain recorded failed attempts.
- **VERIFIED (execution):** the actual returned smoke artifact, SHA-256
  `0f4e2b8d81787729240bcdb84847bc15288ed1ba4047a913c21176a03d9ef87e`
  (536101bytes), matches the worker bytes. Frozen copies pass Node22.22.1 on
  ts1, Node26.0.0 on trj and Node26.8.1 on macOS with digest
  `1f248a71347b82aa`, semantic masks111111/101000, real host clocks, named
  process-capability refusal and the one-CPU topology shim.
- The package/browser gate is **UNRUN at DD**. The historical package-size
  failurefm-8j70 remains open. These Node probes exercise the foundation;
  they do not execute the new family-reversal scenario in a browser or qualify
  the package size. Native full-workspace and installed-wheel qualification
  are separate gates.

Earlier re-run, 2026-10-24, against `Cargo.lock` `2ff0686f` and `SUITE.lock`
`ebfd3073`, for the franken_markdown repin from `2f1e00c0` to `439a910c`
(fm-zxxy: native CM integral rows, vector accents and radical extensibles).
The lock delta changes only the three franken_markdown Git revisions; no
package, version, checksum or WASM manifest changes. The auxiliary lock is
byte-identical.

- **VERIFIED (mechanical):** two strict RCH metadata jobs derive identical
  134-line normal-edge wasm32 trees before and after, with no fmd,
  franken_markdown, PyO3 or fmn-python package. After replacing workspace
  paths only, the tree SHA-256 is
  `ae6a50f567eaadcefc0e6a09e693ca46423124f8cda7c60127cd0bb1e923b6a5`.
- **VERIFIED (build):** a fresh pinned-nightly `cargo build --locked -p
  fmn-wasm --target wasm32-unknown-unknown` passes through strict RCH at
  `cf31e50a`. A separate fresh locked release build of `wasm-smoke` passes
  at `f4038c9a`; both use the lock identities recorded below.
- **VERIFIED (execution):** that smoke artifact, SHA-256
  `4798f438e02b40e100d73224707763e28e0e870c1530de5c307df5f96704512b`,
  passes under Node 22.22.1 on ts1 and Node 26.8.1 on macOS, with unchanged
  digest `1f248a71347b82aa`, semantic masks `111111` / `101000`, real host
  clocks, process-capability refusal and the one-CPU topology shim.
- The package/browser gate is not re-run: its source and complete dependency
  graph are unchanged by this pin. The historical package-size failure
  (fm-8j70) remains open. This is no fresh package, browser or size-budget pass.

Earlier re-run, 2026-10-23, against `Cargo.lock` `7e931713` and `SUITE.lock`
`762141b7`, for 5bb2621c (fm-5wq.51: pure Python plays on native teams). The only
lock change is one new edge, `fmn-python` -> the workspace `fmn` facade. No
package, version, source or checksum moved, and `fmn-python` is not in the wasm
graph. `SUITE.lock` and `wasm-smoke/Cargo.lock` are unchanged.
**The release package gate still FAILS at its size budget** (fm-8j70), so this
audit does not claim a passing package.

- **VERIFIED (mechanical):** `cargo tree -p fmn-wasm --target
  wasm32-unknown-unknown --edges normal --locked` is unchanged at 134 lines, with
  no fmd, franken_markdown, PyO3 or `fmn-python` package.
- The `--locked` wasm32 build and wasm-smoke were not re-run: no package in the
  wasm graph changed since the 2026-10-12 run below.

Earlier re-run, 2026-10-22, against `Cargo.lock` `ad78c8a7` and `SUITE.lock`
`762141b7`, for the franken_markdown repin from `83d363c5` to `2f1e00c0` (fm-u5sb:
TeX's own small delimiters and brace pieces), found the same 134 lines.

Earlier re-run, 2026-10-22, against `Cargo.lock` `4bb26e88` and `SUITE.lock`
`678607fb`, for the franken_markdown repin from `abbb957d` to `83d363c5` (fm-shqq:
`\cong` built from cmsy and cmr), found the same 134 lines.

Earlier re-run, 2026-10-22, against `Cargo.lock` `78326ad1` and `SUITE.lock`
`7b54391a`, for the franken_markdown repin from `4db6f439` to `abbb957d` (fm-giir:
Appendix G parameters read at the current size), found the same 134 lines.

Earlier re-run, 2026-10-21, against `Cargo.lock` `618c7824` and `SUITE.lock`
`04dca2ad`, for the franken_markdown repin from `49466bc2` to `4db6f439`
(fm-4ptw: rule 18a's drops read at the script size). The previous re-run, the same
day against `Cargo.lock` `a15ed74a` and `SUITE.lock` `27cdf3b2` for the repin from
`216c7c3b` to `49466bc2` (fm-8zy2), found the same 134 lines.

Earlier re-run, 2026-10-20, against `Cargo.lock` `284b9c98` and `SUITE.lock`
`20d8fec5`, for the franken_markdown repin from `206bb639` to `216c7c3b`
(fm-tex-metrics-glyphs-ru72: fmd-math lays out from TeX's TFM metrics). The three lock
files move only the three franken_markdown packages' rev; their versions and manifests are
unchanged, and no fmd or franken_markdown package is in this graph. The tree was unchanged
at 134 lines.

Earlier re-run, 2026-10-20, against `Cargo.lock` `1887214b` and `SUITE.lock`
`8ee4f259`, for the franken_markdown repin from `23c4d0c5` to `206bb639`
(fm-tex-color-silent-i0p2: xcolor `\color`/`\textcolor` in fmd-math). The three lock
files move only the three franken_markdown packages' rev; their versions and manifests are
unchanged, and no fmd or franken_markdown package is in this graph.
**The release package gate still FAILS at its size budget** (fm-8j70), so this
audit does not claim a passing package.

- **VERIFIED (mechanical):** `cargo tree -p fmn-wasm --target
  wasm32-unknown-unknown --edges normal --locked` is unchanged at 134 lines, with
  no fmd or franken_markdown package.
- The `--locked` wasm32 build and wasm-smoke were not re-run: no package in the
  wasm graph changed since the 2026-10-12 run below.

Earlier re-run, 2026-10-19, against `Cargo.lock` `9bc3615a` and `SUITE.lock`
`8a1c6af0`, for the franken_markdown repin from `80cbadcd` to `23c4d0c5`
(fm-cm-math-faces-impl-n12u: the CM math faces of ADR-0028). The three lock files
move the three franken_markdown packages' rev and version (franken_markdown
0.5.0 -> 0.6.0, fmd-font 0.3.3 -> 0.3.4, fmd-math 0.1.2 -> 0.2.0), and no fmd or
franken_markdown package is in this graph.
**The release package gate still FAILS at its size budget** (fm-8j70), so this
audit does not claim a passing package.

- **VERIFIED (mechanical):** `cargo tree -p fmn-wasm --target
  wasm32-unknown-unknown --edges normal --locked` is unchanged at 134 lines, with
  no fmd or franken_markdown package. `wasm-smoke/Cargo.lock` has no
  franken_markdown entry.
- The `--locked` wasm32 build and wasm-smoke were not re-run: no package in the
  wasm graph changed since the 2026-10-12 run below.

Earlier re-run, 2026-10-13, against `Cargo.lock` `558c0348` and `SUITE.lock`
`c7030d6c`, for the franken_markdown repin from `e0dd66e7` to `80cbadcd`
(fm-tex-layout-oracle-bkbc). No package version changes. The three lock files move
only the three franken_markdown packages' rev, and no fmd or franken_markdown
package is in this graph.
**The release package gate still FAILS at its size budget** (fm-8j70), so this
audit does not claim a passing package.

- **VERIFIED (mechanical):** `cargo tree -p fmn-wasm --target
  wasm32-unknown-unknown --edges normal --locked` is unchanged at 134 lines, with
  no fmd or franken_markdown package.
- The `--locked` wasm32 build and wasm-smoke were not re-run: no package in the
  wasm graph changed since the run below.

Earlier re-run, 2026-10-12, against `Cargo.lock` `5457a108` and `SUITE.lock`
`bd9bb2e0`, for the franken_markdown repin from `126a5304` to `e0dd66e7`
(fm-5wq.58). No package version changes. The three lock files move only the
three franken_markdown packages' rev, and no fmd or franken_markdown package
is in this graph.
**The release package gate still FAILS at its size budget** (fm-8j70), so this
audit does not claim a passing package.

- **VERIFIED (mechanical):** `cargo tree -p fmn-wasm --target
  wasm32-unknown-unknown --edges normal --locked` is unchanged at 134 lines, with
  no fmd or franken_markdown package. `wasm-smoke/Cargo.lock` has no
  franken_markdown entry.
- The `--locked` wasm32 build and wasm-smoke were not re-run: no package in the
  wasm graph changed since the run below.

Earlier re-run, 2026-10-12, against `Cargo.lock` `a6dfd97a` and `SUITE.lock`
`e5995b50`, for the frankenscipy repin from `ed84bdd1` to `ae52b654` (fm-7qx3).
fsci-linalg's solve-certificate two_prod drops its fused multiply-add
(fm-g1ey); that one function is the whole upstream delta. No package version
changes: the four locks move only the fsci revs.
**The release package gate still FAILS at its size budget** (fm-8j70), so this
audit does not claim a passing package.

- **VERIFIED (mechanical):** `cargo build --locked -p fmn-wasm --target
  wasm32-unknown-unknown` passes through rch. `wasm-smoke/run.sh` reports
  `wasm-smoke OK` with the unchanged Node digest `1f248a71347b82aa` and semantic
  oracle masks `111111` and `101000`.
- `scripts/check_wasm_package.sh` was not re-run: the wasm graph is unchanged.

Earlier re-run, 2026-10-11, against `Cargo.lock` `2f4e99d5` and `SUITE.lock`
`385b7677`, for the frankenscipy repin from `85edd7a9` to `ed84bdd1` (fm-pexg).
That repin brings the libm-free solve policy, solver portfolio and n <= 4 rcond
(fm-wi8l, fm-dboe). fsci-linalg, fsci-runtime and fsci-fft are in this graph,
so the build and smoke were re-run.
**The release package gate still FAILS at its size budget** (fm-8j70), so this
audit does not claim a passing package.

- **VERIFIED (mechanical):** `cargo tree -p fmn-wasm --target
  wasm32-unknown-unknown --edges normal --locked` is 134 lines, the same graph
  once the fsci entries' version (0.2.0 -> 0.3.0) and rev are normalized. The
  root lock change is those six fsci packages plus rand 0.10.2 -> 0.10.3, and
  rand is not in this graph. `wasm-smoke/Cargo.lock` moves only its three fsci
  entries.
- **VERIFIED (mechanical):** `cargo build --locked -p fmn-wasm --target
  wasm32-unknown-unknown` passes through rch. `wasm-smoke/run.sh` reports
  `wasm-smoke OK` with the unchanged Node digest `1f248a71347b82aa` and semantic
  oracle masks `111111` (witness) and `101000` (planted mirror).
- `scripts/check_wasm_package.sh` was not re-run: the wasm graph is unchanged.

Earlier re-run, 2026-10-10, against `Cargo.lock` `d5593dcf` and `SUITE.lock`
`e4758123`, for the franken_markdown repin from `3ed1d332` to `126a5304`
(fm-15k9, fm-tex-layout-oracle-bkbc): the same mechanical check, unchanged at 134
lines. The `--locked` wasm32 build and wasm-smoke were not re-run then.

Earlier re-run, 2026-10-09, against `Cargo.lock` `0d6dfdaa` and `SUITE.lock`
`af634982`, for the franken_markdown repin from `bd00a247` to `3ed1d332`
(fm-tex-layout-oracle-bkbc, fm-aia1): the same mechanical check, unchanged at 134 lines.

Earlier re-run, 2026-10-09, against `Cargo.lock` `726da0b5` and `SUITE.lock`
`a6e3520b`, for the franken_markdown repin from `798de11b` to `bd00a247`
(fm-tex-layout-oracle-bkbc): the same mechanical check, unchanged at 134 lines.

Earlier re-run, 2026-10-09, against `Cargo.lock` `7fcc7866` and `SUITE.lock`
`4385c9af`, for the franken_markdown repin from `e4e8e2ee` to `798de11b`
(fm-tex-layout-oracle-bkbc): the same mechanical check, unchanged at 134 lines.

Earlier re-run, 2026-10-09, against `Cargo.lock` `1e9b139f` and `SUITE.lock`
`d7608aa8`, for the franken_markdown repin from `d664c138` to `e4e8e2ee`
(fm-tex-layout-oracle-bkbc): the same mechanical check, unchanged at 134 lines.

Earlier re-run, 2026-10-09, against `Cargo.lock` `c88db1db` and `SUITE.lock`
`1a4972d1`, for the franken_markdown repin from `a8aab0db` to `d664c138`
(fm-tex-layout-oracle-bkbc): the same mechanical check, unchanged at 134 lines.

Earlier re-run, 2026-10-08, against `Cargo.lock` `ce8fdfe2` and `SUITE.lock`
`565f56fe`, for the franken_markdown repin from `a3578a0f` to `a8aab0db`
(fm-binom-delimiters-glyph-gaps-ed1k): the fmd-math drawn paren and `\tbinom`/`\dbinom`. No
package version changes in this graph.
**The release package gate still FAILS at its size budget** (fm-8j70), so this
audit does not claim a passing package.

- **VERIFIED (mechanical):** `cargo tree -p fmn-wasm --target
  wasm32-unknown-unknown --edges normal --locked` is unchanged at 134 lines with
  no fmd or franken_markdown package. The lock change moves only the three
  franken_markdown packages' rev, none of which is in that graph.
- The `--locked` wasm32 build and wasm-smoke were not re-run: no package in the
  wasm graph changed since the run below.

Earlier re-run, 2026-10-07, against `Cargo.lock` `b40a0537` and `SUITE.lock`
`a162ded1`, for the franken_markdown repin from `0a028127` to `a3578a0f` (fm-y5fl):
fmd-math `\ldots`/`\cdots` as three punctuation dots, `\pmod`, and text-mode spaces. No package version changes.
**The release package gate still FAILS at its size budget** (fm-8j70), so this
audit does not claim a passing package.

- **VERIFIED (mechanical):** `cargo tree -p fmn-wasm --target
  wasm32-unknown-unknown --edges normal --locked` is unchanged at 134 lines with
  no fmd or franken_markdown package. The lock change moves only the three
  franken_markdown packages' rev, none of which is in that graph.
- The `--locked` wasm32 build and wasm-smoke were not re-run: no package in the
  wasm graph changed since the run below.

Earlier re-run, 2026-10-07, against `Cargo.lock` `a1f1461f` and `SUITE.lock`
`c4d868ff`, for the franken_markdown repin from `bfb3221c` to `0a028127` (fm-5wq.56):
fmd-math text-mode quote and dash ligatures. No package version changes.
**The release package gate still FAILS at its size budget** (fm-8j70), so this
audit does not claim a passing package.

- **VERIFIED (mechanical):** `cargo tree -p fmn-wasm --target
  wasm32-unknown-unknown --edges normal --locked` is unchanged at 134 lines with
  no fmd or franken_markdown package. The lock change moves only the three
  franken_markdown packages' rev, none of which is in that graph.
- The `--locked` wasm32 build and wasm-smoke were not re-run: no package in the
  wasm graph changed since the run below.

Earlier re-run, 2026-10-06, against `Cargo.lock` `90803e2a` and `SUITE.lock`
`1b2c97d0`, for the franken_markdown repin from `f059cac6` to `bfb3221c` (fm-5wq.60):
fmd-math rule 11 `\root` placement. No package version changes.
**The release package gate still FAILS at its size budget** (fm-8j70), so this
audit does not claim a passing package.

- **VERIFIED (mechanical):** `cargo tree -p fmn-wasm --target
  wasm32-unknown-unknown --edges normal --locked` is unchanged at 134 lines with
  no fmd or franken_markdown package. The lock change moves only the three
  franken_markdown packages' rev, none of which is in that graph.
- The `--locked` wasm32 build and wasm-smoke were not re-run: no package in the
  wasm graph changed since the run below.

Earlier re-run, 2026-10-06, against `Cargo.lock` `c3737f70` and `SUITE.lock`
`f2118084`, for the franken_markdown repin from `4c8caa25` to `f059cac6` (fm-djcw):
PDF inline `$…$` through fmd-math `Layout`. fmd-font and fmd-math are
byte-identical between the two revisions, and no package version changes.
**The release package gate still FAILS at its size budget** (fm-8j70), so this
audit does not claim a passing package.

- **VERIFIED (mechanical):** `cargo tree -p fmn-wasm --target
  wasm32-unknown-unknown --edges normal --locked` is unchanged at 134 lines with
  no fmd or franken_markdown package. The lock change moves only the three
  franken_markdown packages' rev. `wasm-smoke/Cargo.lock` is unchanged.
- **VERIFIED (mechanical):** `cargo build --locked -p fmn-wasm --target
  wasm32-unknown-unknown` passes through rch, and `wasm-smoke/run.sh` reports
  `wasm-smoke OK` with the unchanged Node digest `1f248a71347b82aa` and semantic
  oracle masks `111111` (witness) and `101000` (planted mirror).
- `scripts/check_wasm_package.sh` was not re-run: the wasm graph is unchanged.

Earlier re-run, 2026-10-06, against `Cargo.lock` `f719a42b` and `SUITE.lock`
`9fc53227`, for fm-5wq. fmn-conformance gains a dev-dependency on fmn-wasm, so
its semantic sanity oracles can read the browser player's frame path. The lock
change adds only that edge to fmn-conformance's dependency list; no package,
source or checksum changes. wasm-smoke also gains in-VM semantic oracles.
**The release package gate still FAILS at its size budget** (fm-8j70), so this
audit does not claim a passing package.

- **VERIFIED (mechanical):** `cargo tree -p fmn-wasm --target
  wasm32-unknown-unknown --edges normal --locked` is unchanged at 134 lines with
  no fmd or franken_markdown package. `wasm-smoke/Cargo.lock` is unchanged.
- `cargo build --locked -p fmn-wasm --target wasm32-unknown-unknown` exited 0.
  `wasm-smoke/run.sh` (Node 22.2.0) exited 0 with the unchanged Node digest
  `1f248a71347b82aa`. Its new semantic-oracle mask is `111111` for the witness,
  and `101000` for the planted y-down mirror: only the left-dot and background
  oracles survive a vertical flip.
- `scripts/check_wasm_package.sh` was not re-run: the wasm graph is unchanged.

Earlier re-run, 2026-10-05, against `Cargo.lock` `1b192ac9` and `SUITE.lock`
`9fc53227`, for the franken_markdown repin from `68fe29b8` to `4c8caa25`
(fm-tex-metrics-glyphs-ru72): fmd-math's rule-17 italic corrections plus
upstream editor/WASM-profile work. No package version changes.
**The release package gate still FAILS at its size budget** (fm-8j70), so this
audit does not claim a passing package.

- **VERIFIED (mechanical):** `cargo tree -p fmn-wasm --target
  wasm32-unknown-unknown --edges normal --locked` is 134 lines and contains no
  fmd or franken_markdown package. The lock change moves only the three
  franken_markdown packages' rev. `wasm-smoke/Cargo.lock` has no
  franken_markdown entry and is unchanged.
- `cargo build --locked -p fmn-wasm --target wasm32-unknown-unknown` exited 0.
  `wasm-smoke/run.sh` (Node 22.2.0) exited 0 with the unchanged Node digest
  `1f248a71347b82aa`.
- `scripts/check_wasm_package.sh` was not re-run: the wasm graph is unchanged.

Earlier re-run, 2026-10-04, against `Cargo.lock` `38e2003d` and `SUITE.lock`
`bb530724`, for the franken_markdown repin from `e911be2a` to `68fe29b8`
(fm-tex-display-style-nclg). That repin covers the display-integral fix and the
upstream fmd-font 0.3.3 / fmd-math 0.1.2 / franken_markdown 0.5.0 release.

- **VERIFIED (mechanical):** `cargo tree -p fmn-wasm --target
  wasm32-unknown-unknown --edges normal --locked` is 134 lines, the same
  graph as below, and contains no fmd or franken_markdown package. The lock
  change touches only those three packages. `wasm-smoke/Cargo.lock` has no
  franken_markdown entry and is unchanged.
- `cargo build --locked -p fmn-wasm --target wasm32-unknown-unknown` exited 0.
  `wasm-smoke/run.sh` (Node 22.2.0) exited 0 with the unchanged Node digest
  `1f248a71347b82aa`.
- `scripts/check_wasm_package.sh` was not re-run: wasm-bindgen and wasm-opt
  are absent on this host, and the wasm graph is unchanged.

Earlier re-run, 2026-09-28, against `Cargo.lock` `d3bec838` and `SUITE.lock`
`9858f262`, for the frankenscipy repin from `5a7aafa2` to `85edd7a9` (fm-9esi):
- the fsci-sparse `npz` feature gate, which removes 11 packages none of which are
  in the wasm graph;
- the num_jac `sqrt` thresholds.

**The release package gate still FAILS at its size budget** (fm-8j70), so this
audit does not claim a passing package.

- **VERIFIED (mechanical):** the path-normalized `cargo tree -p fmn-wasm --target
  wasm32-unknown-unknown --edges normal --locked` is identical to the `5a7aafa2`
  tree below (134 lines) once the rev string is normalized. `wasm-smoke/Cargo.lock`
  moves only its three fsci entries to the new rev.
- `cargo build --locked -p fmn-wasm --target wasm32-unknown-unknown` exited 0.
  `wasm-smoke/run.sh` (Node 22.2.0) exited 0 with the unchanged Node digest
  `1f248a71347b82aa`.
- `scripts/check_wasm_package.sh` was not re-run, for the same reason as below:
  the qualified wasm-pack 0.15.0 is absent on this host, and the wasm graph is
  unchanged.

Earlier re-run, 2026-09-28, against `Cargo.lock` `02ce9708` and `SUITE.lock`
`8fed8803`, for the frankenscipy repin from `5b1441b1` to `5a7aafa2` (fm-9esi).
**The release package gate still FAILS at its size budget** (fm-8j70), so this
audit does not claim a passing package.

- This lock change does reach the wasm graph, through fmn-geom's solver gateway.
  **VERIFIED (mechanical):** the path-normalized `cargo tree -p fmn-wasm --target
  wasm32-unknown-unknown --edges normal --locked` goes from 130 to 134 lines:
  - moved to the new rev: `fsci-linalg`, `fsci-fft` and `fsci-runtime` (0.1.0 to
    0.2.0);
  - moved version: `nalgebra` 0.34.2 to 0.35.0, `simba` 0.9.1 to 0.10.2, `wide`
    0.7.33 to 1.7.1, and `blake3` 1.8.5 to 1.8.7;
  - added: `rayon` 1.12.0, `rayon-core` 1.13.0, `crossbeam-deque` 0.8.8,
    `crossbeam-epoch` 0.9.21, `crossbeam-utils` 0.8.22 and `either` 1.17.0
    (fsci-linalg's thread pool);
  - dropped: `arrayref`, `paste` and `safe_arch`.

  `wasm-smoke/Cargo.lock` was re-resolved for the same fsci entries only. Every
  package has its `SUITE_ALLOWLIST.tsv` row.
- `cargo build --locked -p fmn-wasm --target wasm32-unknown-unknown` exited 0.
  `wasm-smoke/run.sh` (Node 22.2.0) exited 0 with the unchanged Node digest
  `1f248a71347b82aa`: the probe's frames are byte-identical. The probe does not
  reach fsci-linalg's n >= 128 parallel factorizations. How rayon behaves there on
  wasm32 (no spawnable threads in the serial build) is **ASSESSED**, not verified.
- `scripts/check_wasm_package.sh` was not re-run. The qualified wasm-pack 0.15.0 is
  not installed on this host (0.13.1 is), and the gate has stopped at the fm-8j70
  size budget since `b492564b`. A size proxy, the release `fmn_wasm.wasm` cdylib
  before and after the bump:
  - raw: 789,937 to 790,184 bytes (+247);
  - after Binaryen 117 `wasm-opt -Oz`: 583,513 to 583,520 bytes (+7).

  So the repin does not move the fm-8j70 breach materially. The qualified bundler
  artifact is still unmeasured.

Earlier re-run, 2026-09-26, against `Cargo.lock` `7207b6fe` and `SUITE.lock`
`33ee2568`, for the build-identity digest of ADR-0025
(fm-certified-closure-integrity-4fei). **The release package gate still FAILS at
its size budget** (fm-8j70), so this audit does not claim a passing package.

- The lock change adds one workspace edge: `fmn-cli` now lists `fmn-hash`, as a
  build- and dev-dependency for the uncommitted-state digest. No package is
  added, and `fmn-cli` is not in any wasm graph. **VERIFIED (mechanical):** the
  path-normalized output of `cargo tree -p fmn-wasm --target wasm32-unknown-unknown
  --edges normal --locked` is identical at `636e7fd3` (lock `1947c1f5`) and in the
  changed tree (130 lines, with no `fmn-cli`). `SUITE.lock` and
  `wasm-smoke/Cargo.lock` are unchanged.
- `cargo build --locked -p fmn-wasm --target wasm32-unknown-unknown` exited 0.
  `wasm-smoke/run.sh` exited 0 with the unchanged Node digest `1f248a71347b82aa`.
- `scripts/check_wasm_package.sh` was not re-run: the wasm graph is unchanged, and
  the gate has stopped at the fm-8j70 size budget since `b492564b`.

Earlier re-run, 2026-09-25, against `Cargo.lock` `1947c1f5` and `SUITE.lock`
`33ee2568`, for the franken_markdown repin from `82588865` to `e911be2a`
(fm-fmd-repin-g2-truth-y3gr). **The release package gate still FAILS at its size
budget** (fm-8j70), so this audit does not claim a passing package.

- The lock change moves only `fmd-font` (0.1.0 to 0.3.2), `fmd-math` (0.1.0 to
  0.1.1) and `franken_markdown` (0.3.5 to 0.4.5) to the new rev, adding no
  packages. None of the three is in any wasm graph. **VERIFIED (mechanical):** the
  path-normalized output of `cargo tree -p fmn-wasm --target wasm32-unknown-unknown
  --edges normal --locked` is identical before and after (130 lines, with no fmd
  crate), and `wasm-smoke/Cargo.lock` is unchanged.
- `cargo build --locked -p fmn-wasm --target wasm32-unknown-unknown` exited 0.
  `wasm-smoke/run.sh` exited 0 with the unchanged Node digest `1f248a71347b82aa`.
- `scripts/check_wasm_package.sh` was not re-run: the wasm graph is unchanged, and
  the gate has stopped at the fm-8j70 size budget since `b492564b`.

Earlier re-run, 2026-09-24, against `Cargo.lock` `6ce5886a`, at `acd19004`.
The measurements ran on that commit's pre-rebase twin `86f2896d`; the only
commits rebased beneath it, f1d4afd5 and 92d3b889, touch `crates/fmn` and CI,
which are outside every wasm graph. **The release package gate still FAILS at
its size budget**, so this audit does not claim a passing package.

- The lock change behind this re-run is d121ddd5. It adds five workspace edges to
  the `fmn` umbrella crate (`fmn-codec`, `fmn-frame`, `fmn-output`, `fmn-render`,
  `fmn-runtime`), and `fmn` is not in any wasm graph. **VERIFIED (mechanical):**
  `git diff ad0b7d25 HEAD -- Cargo.lock` is those five lines, and `SUITE.lock` and
  `wasm-smoke/Cargo.lock` are unchanged. The path-normalized output of `cargo tree
  -p fmn-wasm --target wasm32-unknown-unknown --edges normal --locked` is identical
  at `ad0b7d25` and `acd19004` (130 lines).
- `cargo build --locked -p fmn-wasm --target wasm32-unknown-unknown` exited 0.
  `wasm-smoke/run.sh` exited 0 with the unchanged Node digest `1f248a71347b82aa`.
- `scripts/check_wasm_package.sh` was not re-run. It has failed at the size
  budget since `b492564b` (fm-8j70) and stops before the Chromium stage, and the
  wasm graph is unchanged. The qualified wasm-pack 0.15.0 bundler artifact at
  `acd19004` is 555,763 bytes against the 514,521-byte budget. The attribution
  is on fm-8j70.

Earlier re-run, 2026-09-24, against `Cargo.lock` `b97f1d76` from a clean worktree
at `c5f50e1e`, with the qualified tools (wasm-pack 0.15.0, wasm-bindgen 0.2.127,
wasm-opt 117, webpack 5.109.2 / webpack-cli 7.2.2):

- The lock change behind that re-run is a single edge: `fmn-python` now depends on
  `fsci-opt` (6bc0ddb2), which the lock already held. `fmn-python` is not in any
  wasm graph. **VERIFIED (mechanical):** `git diff 2649c18b HEAD -- Cargo.lock` is
  that one line, and `SUITE.lock` and `wasm-smoke/Cargo.lock` are unchanged.
- `cargo tree -p fmn-wasm --target wasm32-unknown-unknown --edges normal --locked`
  exited 0, and `cargo build --locked -p fmn-wasm --target wasm32-unknown-unknown`
  exited 0.
- `wasm-smoke/run.sh` exited 0 with the unchanged Node digest `1f248a71347b82aa`,
  after 3837dac0 restored its build (it had missed fm-sq8.9's `ScreenMap::y_up`).
- `scripts/check_wasm_package.sh` exited 1 at the size budget: the bundler wasm is
  555,759 bytes against the 514,521-byte `crates/fmn-wasm/SIZE_BUDGET.tsv` budget.
  The breach is older than this lock change. The same build at `b492564b` (before
  fm-sq8.9) is already 554,530 bytes; fm-sq8.9 adds 1,229. The growth landed after
  the 2026-09-17 pass below and needs bisecting (bead fm-8j70). The
  Chromium stage was not reached. The browser digests and the
  `certified-cpu:scalar:6` identity recorded below predate fm-sq8.9, which flips
  front-door frames to +Y-up and bumps the renderer to `scalar:7`; they are
  historical until the package gate passes again.

Previous full pass: re-derived 2026-09-17 on the local qualified-tool host at commit
`efdf8e4041bbbe9f2a63b4b91f30b59286861b89`. The locked dependency tree, wasm32
target build, locked Node smoke, and the complete serial / shared-memory package
and Chromium gate all exited 0 on a **clean tree** (`source_dirty=false`, receipt
schema `fmn-wasm-package-receipt/2`). Node smoke digest `1f248a71347b82aa`;
browser digests `f5b7f4b9` / `c864b03d` / `8d9ea9c9`.
The always-on `wasm_audit_is_bound_to_current_locks` Gauntlet test fails when
either authority changes. That forces this audit to be re-run and its outcome
recorded, instead of leaving a plausible but stale "current pins" claim behind.

- `SUITE.lock` SHA-256: `e9670cf8bd437d12352deb191afed6a437d55bfa6b2b333fd572fb2fd9283c93`
- `Cargo.lock` SHA-256: `37f6231804430a71d1bedece0a8f00278d5327fc792655a396eb17edd52d5e41`
- Auxiliary `wasm-smoke/Cargo.lock` SHA-256: `e307b83ae1686cb4369dcc2e8e6a7edc421c08b3b46de209a0bc073406401739`

The earlier 0.5.0 release bump re-bound the lock identities at that revision.
That bump changed
only the lockstep workspace package versions (0.4.0 -> 0.5.0, path packages
without a registry source); no third-party package, source or checksum in either
lock changed, so that version-only dependency graph was unchanged. The full wasm
package/browser gate was not re-run for that version-only change.

Method labels are deliberately narrow:

- **VERIFIED (build):** compiled for `wasm32-unknown-unknown` by a named gate.
- **VERIFIED (execution):** instantiated and exercised in a named JavaScript or
  browser runtime.
- **VERIFIED (mechanical):** derived from the committed manifests, locks, and
  governed-closure checker without claiming runtime behavior.
- **ASSESSED:** a future path whose design posture is known but whose artifact
  is not part of the current compiled graph.

## Shipped serial and shared-memory graphs

The tree derivation command `cargo tree -p fmn-wasm --target
wasm32-unknown-unknown --edges normal --locked` exited 0. Cargo also emitted a
diagnostic about the intentionally malformed upstream asupersync fixture;
that diagnostic did not prevent tree generation or the subsequent builds.
Both packaged execution variants compiled the workspace crates below with
the exact `nightly-2026-08-31` toolchain.

| workspace packages | verdict | executable evidence |
|---|---|---|
| `fmn-wasm`, `fmn-anim`, `fmn-cache`, `fmn-codec`, `fmn-config`, `fmn-core`, `fmn-dmath`, `fmn-frame`, `fmn-geom`, `fmn-hash`, `fmn-mobject`, `fmn-platform`, `fmn-render`, `fmn-scene` | **VERIFIED (build)** | `scripts/check_wasm_package.sh` runs wasm-pack 0.15.0 over the actual `fmn-wasm` cdylib; `scripts/check.sh` also compiles the render-axis crates directly for `wasm32-unknown-unknown` |

`fmn-core` consumes `fnp-random-core` 0.2.0 at
`a15d5c32e9330b555b0a653058bcfcff22fcb4ec` for the governed RNG.
fmn-geom enters with its doctrine-D4 solver gateway: the pinned
frankenscipy crates `fsci-linalg` 0.3.0, `fsci-fft` 0.3.0, and `fsci-runtime`
0.3.0 at `ae52b65454841cd4976f6fbac907be0539508d9b` (the last bringing its
audit-ledger blake3 hashing chain; fsci-linalg brings the rayon/crossbeam
pool), plus their
transitive numeric and serialization stacks (`nalgebra` 0.35.0 with the
`simba`/`matrixmultiply`/`wide`/`approx`/num-* chain, `serde`
1.0.229 with derive, `serde_json` 1.0.151 with `preserve_order`, and the
wasm-bindgen family already listed above). The complete admission — exact
versions, checksums, features, licenses, proc-macro/build-script posture, and
unsafe reviews for every package this gateway adds — is enforced row-by-row by
`SUITE_ALLOWLIST.tsv`; `workspace_closure_is_exactly_the_governed_universe`
checks the primary and auxiliary locks on every `cargo test` run.

The pinned nightly has `wasm32-unknown-unknown` installed on the release host.
This is exercised rather than inferred: `scripts/check.sh` target-compiles the
render axis and `wasm-smoke/run.sh` builds a fresh release probe, instantiates it
under Node, checks deterministic render bytes, reads the browser clock shim,
and verifies that process access fails closed. The 2026-09-08 run under Node
22.22.1 emitted the locked smoke digest `1f248a71347b82aa`, reported
`process=capability-absent`, and used the one-CPU browser topology shim.

## Product surfaces

| surface | historical package verdict | tested boundary at the historical receipt |
|---|---|---|
| Tier 1: `FmnScene` fixed-scene renderer | **VERIFIED (execution)** | The packaged ESM/TypeScript surface enumerates `circle_shift`, `parametric_wave`, and `orbit_duet`, renders RGBA8 into caller-reused buffers, and precisely refuses a wrong destination length. wasm-bindgen copies the mutable JS view into Wasm memory; this is buffer reuse, not a zero-copy claim. |
| Tier 2: `FmnPlayer` FMTL/1 player | **VERIFIED (execution)** | The packaged player parses the governed timeline bundle, refuses engine-major mismatches, exposes labels/seek state, and renders RGBA8 through the same semantic Lumen path. |
| npm bundler artifact | **VERIFIED (historical execution)** | `scripts/check_wasm_package.sh` builds the self-contained package, checks its exact file/license inventory and recorded size budgets, runs `npm pack` plus `npm publish --dry-run`, installs the tarball into a fresh consumer, bundles it with webpack 5.109.2, and exercises both tiers through Canvas write/readback in headless Chrome. The historical receipt and its limits are recorded below; current preflight is blocked as recorded above. |
| threads artifact | **VERIFIED (execution)** | The separate `fmn-wasm/threads` export is built with atomics and imported shared memory. Its coordinator compiles the module once, verifies the instantiated buffer is a `SharedArrayBuffer`, passes the same module and memory to two module workers, and renders independent whole frames concurrently. The Chromium gate byte-compares serial and threaded results, checks repeatability, and proves a document without COOP/COEP receives `FMN_WASM_CROSS_ORIGIN_ISOLATION_REQUIRED` before worker startup. This is frame-batch parallelism, not intra-frame parallelism. |

That historical browser/package evidence proves compilation, packaging, deterministic
same-build execution, and the tested failure paths. It does **not** prove
browser performance, cross-browser behavior, the certified platform matrix,
cross-platform bit identity, registry publication, or service-worker/CDN
deployment. WASM remains standard-mode only.

### Historical full package receipt (2026-09-17)

The release-grade run exited 0 in 111.9 seconds on a clean clone of commit
`efdf8e4041bbbe9f2a63b4b91f30b59286861b89` (`source_dirty=false`); retained
evidence is `.rch-results/wasm-package-20260917-fm7wm6/release-receipt.json`
(evidence root `/tmp/fmn-fm7wm6-release-20260917`). An independent dirty-tree
diagnostic run at the same commit produced byte-identical browser digests,
retained at `.rch-results/wasm-package-20260917-fm7wm6/receipt.json`. The
gate script now accepts both npm receipt shapes (npm 10 bare object and the
npm 9 `{package-name: receipt}` mapping); this host's npm is 11.19.1.
No registry publication occurred.

| Identity or result | Executed value |
|---|---|
| Tools | wasm-pack 0.15.0; wasm-bindgen 0.2.127; Binaryen 117; Node 22.22.1; npm 11.19.1; webpack 5.109.2 / CLI 7.2.2; Chrome 153.0.8010.47 |
| npm tarball | 409,605 bytes; SHA-256 `2a7a7f4de062b4f112619baebec2aa79db953c78e4c303dc179013ea3c2d2a18` |
| Serial Wasm | 487,503 raw bytes; 194,668 gzip bytes |
| Shared-memory Wasm | 487,117 raw bytes; 195,145 gzip bytes |
| `FmnScene` | 45 rendered frames; digest `f5b7f4b9` |
| `FmnPlayer` | 45 rendered frames; digest `c864b03d`; engine identity `certified-cpu:scalar:6` |
| Workers | 2 workers over shared memory; 4 frames; digest `8d9ea9c9`; serial/threaded byte equality passed |
| Isolation refusal | `FMN_WASM_CROSS_ORIGIN_ISOLATION_REQUIRED` before worker startup in the nonisolated document |

Chrome ran with its sandbox enabled. The asupersync malformed-fixture cargo
diagnostic is the intentionally non-fatal upstream diagnostic tracked by
bead fm-asupersync-fixture-diagnostic-5bxf; it did not affect tree
generation or any build. The player engine identifier names the semantic
renderer serialized in the timeline; it does not change the browser's
standard-only certification scope.

Historical evidence: the v0.4.0 release-commit receipt passed at
`d1e4274aa32ba5c59934029db397bbbeb9bfa18c` with `source_dirty=false`; its
395,038-byte tarball had SHA-256
`f531940b5849ed2f3e091424931581bf4450e2811ec66e850aebfe3a76dc371d`.
The 2026-09-08 a40c4a88 receipt (npm 10.9.4, Chrome 152, tarball
`41a94cd2…cf85`) qualified the August locks and is superseded by the
receipt above.

## Intentionally absent from the current wasm graph

| packages or capability | posture |
|---|---|
| `fmn-studio`, `fmn-cli` | host front doors: supervisor/subprocess, terminal, filesystem, and CLI behavior; the browser package is a separate W11 artifact |
| `fmn-python`, PyO3, NumPy | supported host-CPython portal only; never part of a standalone browser artifact |
| `asupersync` | host-side multi-scene batch farms and scheduler laboratory; never in the frame loop |
| `frankentorch`, `ft-kernel-metal`, CUDA/Metal annexes | native standard-mode accelerator annexes; no GPU annex enters this wasm package |
| `fmn-output`, ffmpeg | host publication and optional external-tool boundary; browser frames are returned to the consumer instead |
| `fmn-text`, `fmn-tex`, `fmn-library`, `fmd-font`, `fmd-math`, the complete bundled font set | not reached by the current three-scene Tier-1 package or FMTL demo graph; future browser text/font consumption must be built, size-measured, and re-audited before it is called verified |
| Remaining `franken_numpy` / `frankenscipy` crates; `franken_networkx`, `frankenpandas` | only `fnp-random-core` and the three named `fsci-*` crates are consumed; other subsets require their own target build and governed-closure update |

## Standing rules

1. A crate enters the shipped wasm graph only after an actual
   `wasm32-unknown-unknown` build and the governed closure both pass.
2. Any `SUITE.lock` or `Cargo.lock` change invalidates this audit mechanically.
   Update the recorded digests only after re-running the tree, target build,
   Node smoke, and every affected browser/package check.
3. An absent host-only package is not evidence that it is wasm-compatible.
   Future text, fonts, or suite libraries stay **ASSESSED** until their real
   artifact is compiled and exercised.
4. Threads remain a separate explicit subpath behind atomics, shared memory,
   and cross-origin isolation. The default import stays serial and no fallback
   is implicit. Certified mode and native accelerator annexes remain explicit
   refusals in the browser package.
