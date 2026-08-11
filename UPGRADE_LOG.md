# Dependency Upgrade Log

**Date:** 2026-08-11
**Dependency-graph audit HEAD:** `fe7f1b19a61dbebbb70188f7224db7bbfb50aee6`
**Final validation HEAD:** `02ac48443b2c701da15e52b1fd4f43487be2b373`
**Project:** FrankenManim
**Manifests:** root workspace, `fuzz`, `wasm-smoke`, and the non-member G0 spikes

## Outcome

| Result | Count |
|---|---:|
| Direct external dependency or patch records audited | 10 |
| Ordinary latest-stable updates available | 0 |
| Already current | 2 |
| Governed exact pins preserved | 6 |
| Deliberate migrations requiring attention | 2 |
| Updates applied | 0 |

This repository does not admit routine semver drift. `SUITE.lock`,
`SUITE_ALLOWLIST.tsv`, ADR-0008, and `docs/GOVERNANCE.md` require exact
dependency identity and a Gauntlet-backed review for every admitted change.
The audit therefore preserved all authority files and lockfiles.

## Requires attention

### PyO3: 0.26.0 -> 0.29.2

**Priority:** Security migration; tracked by `fm-kg6g`.

`cargo audit` found two vulnerabilities in the exact ADR-0015 pin:

- [RUSTSEC-2026-0176](https://rustsec.org/advisories/RUSTSEC-2026-0176):
  out-of-bounds reads in `PyList` and `PyTuple` iterator `nth`/`nth_back`;
- [RUSTSEC-2026-0177](https://rustsec.org/advisories/RUSTSEC-2026-0177):
  missing `Sync` bound on `PyCFunction::new_closure` closures.

Both are fixed by PyO3 0.29.0 or newer. The latest stable release inspected
was 0.29.2 (2026-08-05). The portal's current production tests passed, but that
does not remove vulnerable code from the dependency closure.

This is not a pin-only update. The clean migration is expected to touch 11-12
files and must:

- amend ADR-0015 while retaining `abi3 = off` and the audited FFI boundary;
- declare `#[pymodule(gil_used = true)]` unless a new review proves
  free-threaded safety;
- replace the deprecated `pyo3/extension-module` mechanism with
  `PYO3_BUILD_EXTENSION_MODULE` in extension builds;
- migrate the G0-5 spike's removed `.downcast()` calls to `.cast()`;
- refresh the production and spike locks plus every affected exact allowlist
  record;
- re-run embedded and extension imports, the G0-5 suite, NumPy live views,
  copy/deepcopy/pickle, subclass/MRO dispatch, weakrefs and dictionary cycles,
  method-cache invalidation, PG-8 state goldens, the governed closure, the
  security audit, and the full Gauntlet.

PyO3 0.29.1/0.29.2 also contain directly relevant fixes for populated
`#[pyclass(dict)]` deallocation leaks, `__dict__` reference cycles, type-object
reference leaks, weak-reference pointer arithmetic, and interpreter-shutdown
soundness. The library-updater circuit breaker requires explicit user approval
before a migration spanning ten or more files, so no partial compatibility
shim or split-version graph was introduced.

### wasm-bindgen: 0.2.126 -> 0.2.127

**Priority:** Small deliberate upgrade; not a routine update.

0.2.127 is the only ordinary version delta reported by
`cargo outdated --workspace --depth 1`. Its changelog is predominantly additive
bindings and fixes, and current FrankenManim usage is limited to the established
macro/`JsError` surface. Nevertheless, the exact pin is classified
`deliberate+gauntlet` in `SUITE_ALLOWLIST.tsv`.

A correct isolated tranche must update both workspace consumers and
`wasm-smoke`, refresh both lockfiles, reconcile every affected
`wasm-bindgen`/macro/shared/`js-sys`/`web-sys` allowlist row, and pass the
workspace, wasm32 browser smoke, native-vs-WASM bit-equivalence, bundle-size,
and full Gauntlet gates. It was not applied opportunistically during this audit.

## Preserved governed pins

| Dependency | Exact authority | Reason preserved |
|---|---|---|
| `asupersync` | Git `c48399f20a9780ef420de64fe504ff922e5afe5e` | `SUITE.lock` foundation pin |
| `fmd-font`, `fmd-math` | `franken_markdown` Git `82588865c453b175cb1263b36e30f5b9b1941a2e` | `SUITE.lock` foundation pin |
| `fsci-integrate` | `frankenscipy` Git `5b1441b13a0997901ad2f9835c30072f87ca93b2` | `SUITE.lock` foundation pin |
| `ft-kernel-metal` | `frankentorch` Git `523aaf827faf538aa541126ee222fcd7af348410` | sanctioned Accelerator Annex gateway |
| `block` patch | `rust-block` Git `b39ae859d1ee8e8cb5eef6a516471f1578d26b96` | reviewed Objective-C FFI correction |

The G0-8 accelerator spike intentionally retains its historical
`ft-kernel-metal` proof revision rather than tracking the later production pin.
Its manifest comment was corrected during this audit to make that authority
boundary explicit.

## Already current

- `rustix =1.1.4` (test-only Studio PTY/dev closure)
- `libfuzzer-sys =0.4.13` (isolated fuzz workspace)

The generated WASM demo `package.json` has no dependencies, and the repository
does not yet have a Python packaging manifest or Node lockfile.

## Security and policy checks

- `cargo audit`: **failed**, solely as a green-bar claim, with the two PyO3
  vulnerabilities above; it also reports the already-known unmaintained
  `paste 1.0.15` reached transitively through pinned `nalgebra/simba` and Metal.
- `cargo deny check`: **not an authoritative project gate** because no
  `deny.toml` exists. Its default policy rejected the project's approved git
  sources and ordinary MIT/Apache licenses, while independently confirming the
  same two PyO3 advisories and the `paste` warning.
- `cargo outdated --workspace --depth 1`: exited 0 and found only the
  `wasm-bindgen` patch delta. Cargo also printed the known malformed
  asupersync test-fixture diagnostic while walking that checkout.
- The repository's authoritative governed-closure test remains the exact
  allowlist/checksum/pin gate; dependency updates must pass it in addition to a
  vulnerability audit.

## Executable evidence on the audited graph

- `cargo test --offline -p fmn-cli --features batch,metal --test runtime_boundary`:
  25 passed.
- `cargo test --offline -p fmn-conformance --test e2e_scenarios fast_tier_scenarios_pass`:
  passed.
- `cargo test --offline -p fmn-python`: 16 passed; expected unsendable
  cross-thread refusal diagnostics were emitted by the acceptance suite.
- `cargo test --offline -p fmn-wasm`: 22 passed, 2 manual browser tests ignored.
- `cargo test --offline` at final validation HEAD: exited 0 across the complete
  workspace and doctest set. The Python bridge's expected unsendable
  cross-thread refusal diagnostics were caught by its passing negative tests.
- `cargo fmt --check`, `cargo check --offline --all-targets`, and
  `cargo clippy --offline --all-targets -- -D warnings` at final validation
  HEAD: each exited 0.
- `python3 scripts/video_corpus.py verify` at final validation HEAD: exited 0;
  all 8 allowlisted sources reproduced byte-for-byte at their locked pins.

These prove the current graph still works; they do not waive the PyO3 security
migration or the full Gauntlet required for either candidate update.

## Commands used

```text
cargo outdated --workspace --depth 1
cargo search <direct dependency>
cargo info <direct dependency>
cargo audit
cargo deny check
cargo test --offline -p fmn-cli --features batch,metal --test runtime_boundary
cargo test --offline -p fmn-conformance --test e2e_scenarios fast_tier_scenarios_pass
cargo test --offline -p fmn-python
cargo test --offline -p fmn-wasm
cargo fmt --check
cargo check --offline --all-targets
cargo clippy --offline --all-targets -- -D warnings
cargo test --offline
python3 scripts/video_corpus.py verify
```
