# FrankenManim implementation status

**Status date:** 2026-09-07 (America/New_York; execution receipts extend into September 8 UTC)

**Reality-check source:** `d784048148002acdb7d3a914a8e764afcfad3b1d` on `main`; initially clean. This audit changes this report and Beads only.

**Historical September 4 repair checkpoint:** `225b589d6281800723990479c991bc6e13e113fd` (animation fixes `ff49cd4b` / `dd2bdce0`, runtime-audit test restoration `225b589d`).
**Historical runtime-audit checkpoint:** `7aeb3f40a763998d07b43b74613f3c6becc49207`.  
**Agent-governance checkpoint:** ADR-0023 and `docs/GOVERNANCE.md` through `19e1e8b0014f5e4dd68aebc0520cd7ba9fb98283`.  
**Authority rule:** this document summarizes evidence. `.beads/issues.jsonl` remains the task, status, and dependency authority; the Revision-4 comprehensive plan remains the design authority.

## 2026-09-07 comprehensive reality check

**FrankenManim is a substantial, usable prerelease engine. It does not yet deliver the complete 1.0 product contract.** Native rendering, native typesetting infrastructure, scene execution, output publication, a working Python render path, and a real Studio worker/host exist. The remaining distance is concentrated in production portal semantics and front-door composition, a finished Studio interaction surface, current platform/artifact proof, and qualified performance evidence. Closing a large number of small compatibility beads has not retired those integration obligations.

This assessment used the complete AGENTS.md, README.md and Revision-4 plan, the binding decisions and relevant subsystem specifications, live source and tests, gate packets including their later corrections, all 628 initial Beads records, the canonical planner, and published release artifacts. Source inspection sampled implementations and their callers across all ten subsystems; it was not a line-by-line correctness proof of every function. Earlier checkpoints below remain historical evidence rather than current passing-gate claims.

The measuring stick is the promised combination of a sovereign native library/CLI, beautiful mathematical output, source-compatible optional Python, useful live Studio, certified artifacts, and measured scaling. Deliberate Behavior Notes, custom-GLSL exclusions, user-supplied drawing assets, standard-only GPU/WASM operation, and Windows' exclusion from certification are binding product decisions. They are not missing work to be undone by this audit.

### What was actually executed

The shipped artifact and current source have different identities. Published v0.4.0 comes from `d1e4274aa32ba5c59934029db397bbbeb9bfa18c`, not the audited September source. Successful release probes below establish that useful software ships; they do not prove that every subsequent source change is correct.

| Boundary | Observation from this audit | Limit |
|---|---|---|
| Published Linux native install | Downloaded the v0.4.0 portable archive and verified its published SHA-256; the extracted `fmn` reports the expected source/build identity. | One Linux artifact; not the complete installer/platform/tier matrix. |
| README release/example pairing | The published binary rejects the current `demo/wasm/bundle.fmtl` with scene exit 5: snapshot minor 7 is newer than its minor-6 reader. The README puts this command in the shipped-v0.4.0 block. Current `tex_span.v1` and `sound_cue.v1` builtins are also absent from that release. | Strict format/unknown-scene refusal is correct. Source capabilities and examples need a matching release; the current FMTL README pairing is a reproduced user-facing documentation/artifact gap. |
| Native primitive rendering | All 25 shipped primitive scenes rendered at 96×54, 8 fps, four threads, through PNG-sequence publication; all commands exited 0. A 320×180, 30 fps circle sequence had 12 frames and eight distinct PNG hashes. | Small smoke scenes and resolutions; no throughput or beauty verdict. |
| Certified thread independence | The same circle scene at 160×90 and four fps produced identical PNG bytes, artifact digest and closure digest at 1, 4 and 16 render threads. | Two emitted frames at this low sample rate; same Linux release build only. No full corpus, weekly high-core or cross-platform certification claim. |
| Optional ffmpeg boundary | A real three-frame MP4 encoded with installed ffmpeg 8.0.1/libx264; output recorded the executable hash, exact-image process mechanism, argv, artifact digest and manifest; exit 0. | An encoded-video capability check. The MP4 is not a certified artifact. |
| Published Python wheel | Verified the cp313 Linux wheel checksum, installed it with NumPy 2.5.2 in a fresh CPython 3.13.1 venv, and rendered a source scene using `ShowCreation`, square-to-circle `Transform`, and `wait`; five PNG frames, exit 0. | Real extension and real output, but the August wheel and one scene. No current-wheel semantic census or universal compatibility claim. |
| Published wheel mathematical output | `Tex(r'e^{i\pi} + 1 = 0')` rendered with an empty PATH and exit 0 at 320×180 and 960×540, but visual inspection showed vertically inverted glyphs and the exponent below the baseline. | A reproduced shipped defect. Source fix `fd6bea1db` (`fm-5wq.4.20`) routes capture through the camera projection and is in audited HEAD, but is not an ancestor of the published v0.4.0 source. This needs a verified repaired wheel, not another speculative source fix. |
| Python certification refusal | The same wheel's `--reproducible` returned capability exit 4 naming the incomplete input closure/provenance sidecar. | A correct current refusal still leaves a promised 1.0 capability incomplete. |
| Published Studio | Launched the real native supervisor/worker with an empty PATH, requested its loopback routes, observed 403 without authentication, 200 for authenticated UI/inspect/scrub, frame index 1 after scrub, two Kitty frame payloads, and graceful exit 0. | Real process/socket/terminal protocol proof; not a browser usability test, security review, Python edit workflow or PG-4 measurement. |
| Current source control plane and Python policy tests | `scripts/check.sh` passed its parser/planner/guard/executor tests, refusal inventory, runtime-audit contract tests, helper/constructor policy tests and native installer smoke. | Hermetic Python tests include fixture-based behavior; they are not clean-wheel native acceptance. |
| Current Rust build/CLI checks | `cargo fmt --check`, `cargo check --all-targets`, batch all-target check, CLI-only negative control, CLI smoke (one parser-floor and seven shipped-binary tests), and ffmpeg-fixture all-target check passed. RCH returned terminal exit 0 for the heavy steps on `hz3`. | Build and focused execution evidence; the complete gate did not pass. |
| Full check script | Terminal exit 101 at the wasm32 render-axis check: `E0463`, missing `core` because `wasm32-unknown-unknown` was absent for the worker's pinned nightly. | Environment failure. Clippy, rustdoc and full Cargo tests were not reached by this invocation. |
| Standalone Clippy | `cargo clippy --all-targets -- -D warnings` on `vmi1264463` returned terminal exit 101: unused internal `emit_svg_document` at `crates/fmn-geom/src/svg.rs:2930`. | A current source failure. `svg_safe.rs` already owns the public emitter; the parser-local duplicate is unreachable after the admission-boundary refactor. `fm-9fo8` owns the narrow repair. |
| Standalone full Cargo tests | `cargo test` on `hz3` finished with terminal exit 101 after 38 test binaries reported 765 passing tests, one failure and two ignored tests. The failing corpus-leak gate could not read `fuzz/corpus/svg_document/002b95e04785fddde71f5561161cbf643a40679b`. | Verified locally as a tracked four-byte regular file; the remote directory was absent. This is an incomplete remote test-input snapshot, not a detected corpus leak. Later test binaries were not reached. `fm-5wq.7` owns environment/input qualification. |
| Separate scene-runtime test | `cargo test -p fmn-conformance --test scene_runtime --locked` on `vmi1227854` returned terminal exit 101 after dependency builds exhausted the worker's disk. A requested concurrent `hz3` retry was refused with exit 103 because that project was already active there. | Infrastructure failure and scheduling refusal; no scene test verdict. No artifacts were deleted to make space. |
| Current governed package closure | Compiled and directly executed the unmodified production `closure.rs::audit_with_aux` using the pinned nightly: 315 workspace packages, four auxiliary locks, zero violations, exit 0. | A focused Rust auditor invocation, not the full workspace `cargo test` gate. |
| Current governed-closure test binary | `cargo test -p fmn-conformance --test governed_closure --locked` on `hz3` returned terminal exit 101: 14 passed, one failed. `workspace_closure_is_exactly_the_governed_universe` passed; `wasm_audit_is_bound_to_current_locks` rejected the stale audit fingerprint. | A current source/evidence failure, separate from the earlier missing WASM target. `fm-7wm.6` requires real target and package qualification before changing the recorded hashes. |
| Current video-corpus authority | `python3 scripts/video_corpus.py verify` reproduced `VIDEO_CORPUS.lock` byte-for-byte from the pinned local checkout, with eight allowlisted scenes; exit 0. | Source/curation identity proof; no fresh execution of those eight scenes is claimed by this command. |

Full standalone Cargo-test and final documentation/tracker validation receipts are recorded in the final validation subsection below. This audit does not close a product gate or implementation bead.

### Vision checklist against implementation

`WORKING` below means the stated bounded behavior was executed in this audit. `PARTIAL` means concrete functionality exists but the whole promise is incomplete. `UNPROVEN` means source and often historical tests exist, but the required current proof has not been obtained. These labels apply to the stated goal, not to every function in a crate.

| Goal from the plan | Present implementation and evidence | Assessment / remaining obligation | Existing owner or audit follow-up |
|---|---|---|---|
| Substrate: deterministic arithmetic, color, hashing and one RNG | `fmn-core`, `fmn-dmath`, `fmn-hash`, config/platform code; `rng.rs` delegates the primitive to `fnp-random-core` and owns named streams/frame forks. | **PARTIAL proof.** Native certified output works in the bounded probe; full current arithmetic/platform suite remains required. The migration is not absent merely because its bead remains active. | `fm-ai1`, `fm-yp0`; tracker reconciliation `fm-5wq.6`. |
| Governed closure and safe-code doctrine | `SUITE.lock`, 15-column allowlist, closure parser/auditor, forbid roots and ADR-0015's two-slot portal boundary. Direct current audit has zero violations. | **WORKING bounded audit.** No current unlisted-transitive defect reproduced. This does not substitute for transitive unsafe reviews, target builds or the full suite. | Existing closure reports; `fm-5wq.6`. |
| Chisel: geometry, real arc length, SVG, booleans, rotations | Real implementations in `fmn-geom`; analytic/property/Reference structural fixtures; error-bounded curve conversion and bounded SVG/boolean paths. | **Implemented; current gate REGRESSED.** Clippy rejects the unused internal SVG emitter. Certified flatten-first booleans are the permanent correct fallback, not a stub. More sophisticated curve routes must earn admission separately. | `fm-9fo8`; existing W2 owners and `fm-parser-libfuzzer-targets-rrc0`. |
| Marionette: arena, records, live views, ownership, copy, updaters | `Stage`, generational handles, CoW records, render revisions, ordered families and native/PyO3 tests exist. | **PARTIAL across front doors.** The real wheel executes basic animation; full MRO/view/pickle/native-alignment interaction needs the current native and installed-wheel suites. | `fm-5wq.4.142`, `.143`. |
| Choreo: rational clock, lifecycle, composition and pure segments | Native animation modules, six-step scene capture, purity/pipeline machinery; real animation output. | **PARTIAL portal integration.** Installed semantic overrides and direct embedded initialization differ at HEAD. Native composition behavior cannot be inferred from fixture storage tests. | `fm-5wq.4.142`; native Choreo tests. |
| Lumen: analytic fill/stroke, retained IR, cache, 3D and images | Real curve coverage, stroke distance, binning, revisioned retained plans, texture/image sampling, depth/lighting; the released CPU path rendered the corpus. | **WORKING bounded CPU path; full quality/performance UNPROVEN.** Current engine/gallery/equivalence tests and qualified performance are still needed. | `fm-sq8`, `fm-inr`, G3/G4. |
| CPU fast paths and farm-scale execution | Runtime plans, memory-bounded pipeline, frame purity, ordered output; build-tier kernels and instrumentation exist. | **PARTIAL.** PixelKernel build tiers currently delegate to scalar implementations. Source documents measured reasons for rejecting an unhelpful SIMD prototype; SIMD is not an automatic improvement. Profile retained work, glyph instancing, binning and allocation before replacing kernels. | `fm-render-simd-fast-paths-8mr4`, `fm-render-image-glyph-hints-xna3`, `fm-inr.4`. |
| Scribe: native fonts, text and mathematics, source spans | `fmn-text`, `fmn-tex` consume pinned `fmd-font`/`fmd-math`; native span maps and TransformMatchingTex integration exist. | **PARTIAL proof.** Committed ratchet reports 99.994% occurrence-weighted parse and 99.797% parse+layout on its frozen corpus. This is neither universal TeX support nor a fresh run in this audit. G2 still needs qualified PG-1/PG-7 evidence. | `fm-i1q`, `fm-inr`; upstream typography ledger. |
| Menagerie + Atlas: the class library, fields, data and graphs | Real modules for curves, coordinates, solids, images, fields, drawings, graph/data/neural compositions. Constructor authority policy passes. | **Broad implementation; complete semantic coverage UNPROVEN.** Asset-backed defaults may correctly refuse without user files. Native constructors do not alone prove every Python method. | `fm-5wq.4.141`, `.143`; census bead and `fm-5wq.6`. |
| Native Rust front door and standalone CLI | Public `fmn` facade and examples; CLI resolves 25 builtins plus special native scenes and FMTL bundles. PNG/GIF/y4m/video/batch/doctor paths and tests exist. | **WORKING bounded shipping path.** Arbitrary `.rs` files are not a CLI loader; Rust scene authors embed the facade. Native `.py` refusal is required by ADR-0017. | `fm-c53`, `fm-c53.7`; release tasks. |
| Proscenium: live Studio and interactive iteration | Real isolated worker, supervisor, journal, replay/effect model, authenticated host and preview; live scrub/inspect succeeded. | **PARTIAL product.** Browser UI is currently an image, numeric frame input, three buttons and raw response text. Full interactive inspection/overlays, browser UX and Python worker/edit/embed integration remain separate obligations. | `fm-ffj.70`, `.71`, G3. |
| Reel: codecs, sound, ordered publication and ffmpeg | Native PNG/JPEG/GIF/WAV/y4m, sound mixing, ordered emitter, bounded sink/provenance and exact-image ffmpeg boundary. Real PNG and MP4 output verified. | **WORKING bounded native path.** Whole-format/platform/failure matrices and full Python console composition remain incomplete proof or work. | `fm-5wq.4.145`, `fm-yp0`; native output tests. |
| Full `manimlib` compatibility, source-unedited scenes | Published wheel rendered a real scene; current bootstrap is substantial authored code; eight allowlisted video scenes have structural baseline rows. | **PARTIAL.** A reviewed ledger is not an invocation proof; eight seed scenes are not the 257-class/public-method surface. Current wheel, source-unedited gallery, intermediate states and lifecycle witnesses must agree. | `fm-5wq.4`, `.142`–`.145`, `fm-d3gt`, `fm-rqc`, G4a. |
| Certified full input closure and platform matrix | Native manifests and canonical output, replay/input hashing and historical three-platform golden evidence exist. | **PARTIAL.** Native small-scene thread identity succeeded. The portal still refuses certification; current full raw/PNG/WAV cross-platform and replay receipts are owed. | `fm-5wq.4.144`, `fm-yp0`. |
| Accelerator Annex | Real feature-gated Metal renderer and earlier Mac evidence; `fmn-render` has no production CUDA module. | **PARTIAL by explicit decision.** CUDA needs upstream resident-buffer APIs and a PTX closure ruling. GPU remains standard-only; optional CUDA must not hold CPU work hostage. No current Mac/RTX measurement was made here. | `fm-ktj`, ADR-0007, `fm-inr.4`, `fm-5wq.6`. |
| WASM renderer/player/package | Real `FmnScene`, FMTL `FmnPlayer`, serial/shared-memory package code and earlier fresh-consumer Chromium receipts. | **PARTIAL / stale proof.** Current audit fingerprints differ from both locks; the source gate explicitly checks this. Browser Scribe/font/library breadth is not in the present package graph. Registry publication is distinct from dry-run packaging. | `fm-zsu`, `fm-7wm.6`. |
| Distribution: zero-dependency native install and optional wheels | GitHub v0.1.0–v0.4.0 prereleases; latest has three native archives and three cp313 wheels. Linux archive/wheel checksums and actual use verified. | **PARTIAL.** Full per-tier/platform matrix and signed-release promise remain outstanding. PyPI's package endpoint returns 404; README's bare pip command is not a working public-index install path today. | `fm-m54`, `fm-vsq`, `fm-zsu`. |
| Gauntlet: oracles, goldens, gallery, equivalence, fuzz and repros | Real conformance modules and registered scenarios, deterministic logging/FMNA bundles, reduced fuzz campaigns and opt-in full suites. | **PARTIAL proof.** Test names and import success are insufficient; default Cargo tests do not activate every corpus/full-browser/hardware/performance lane. Current full gate has not passed. | W10, `fm-parser-libfuzzer-targets-rrc0`, all gate owners. |
| Performance: speed, latency, memory, typography and scaling promises | `PERF_GATES.tsv`, strict evidence verifier, native/portal producers and unqualified historical samples. | **UNPROVEN at the promised qualification.** No fresh pinned-host pass establishes 2×/2.86× Reference speed, 60 fps 1080p, 30 fps 4K, <150 ms cold start, all-class zero steady allocations, binding budgets or 96-core scaling. | `fm-inr.1`, `.2.3`, `.4`, `fm-0q0g`. |

### The concrete gaps that should drive work

1. **Make the tested Python product the same product that is installed.** `execute_bootstrap` in `crates/fmn-python/src/lib.rs` executes only the bootstrap; `python/manimlib/__init__.py` additionally installs `_animation_semantics.py`. The existing native `production_bridge_acceptance_suite` runs `tests/bridge.py`; the separate `tests/animation_semantics.py` is not wired into the current embedded acceptance module. The composition workflow first proposes a source patch and later names `production_animation_semantics_acceptance_suite`; that name is absent at audited HEAD. This is not proof that the workflow successfully ran zero tests: its source-mutation step comes first, and the compressed composition payload itself fails decoding at this commit. Neither queued automation nor an unapplied embedded patch is landed integration. The repair is ordinary reviewed source plus permanent native and wheel acceptance, with a nonzero test-count assertion.
2. **Replace aggregate compatibility counts with attributable behavior.** The generated Ledger contains 2,482 rows: 2,071 same, 170 improved, 99 tiered, 142 excluded, zero unreviewed. Its authored Python overlay has 2,275 rows: 1,896 same, 140 improved, 97 tiered, 142 excluded. The refusal inventory separately counts 103 sites: 63 explicit `NotImplementedError`, 40 `_refuse_unrouted`, no anonymous violations. These numbers measure different things. The runtime audit catches missing values and placeholder markers; it does not execute every method. A callable returning the wrong geometry may satisfy an existence audit. Bind rows to actual native/wheel witnesses, including negative controls and intermediate animation states.
3. **Finish the portal's output, certification and interactive front doors.** A five-frame wheel render proves a useful path, but does not make the target CLI block shipped. Reuse Reel and Proscenium; do not rebuild scheduling, encoders, RNG or host capabilities in Python. Certification additionally needs all scene imports, wheel/CPython/NumPy identities and effects in C1/C3/C9. Treat this as implementation work, not documentation cleanup.
4. **Finish Studio as a user tool and verify its real composition.** The existing protocol is functional. The browser shell has not yet demonstrated the complete inspection, event/overlay, edit/replay and usability experience promised by the plan. `e2e_scenarios.rs::studio_preview_run` directly composes a preview frame; it is a useful lower-layer scenario, but does not itself start a worker or open a socket despite its surrounding boundary language. The separate CLI runtime tests do start processes/sockets. Add real browser coverage over those production boundaries rather than pretending the in-process row proves them all.
5. **Restore current artifact/lock evidence.** `docs/wasm_audit.md` records August lock digests, while actual SUITE/Cargo digests are `d38bb18884f060d3867dfbb26b1c985e90ddf8ce716589dd13d2e68aa404b3d3` and `24c65127892ce9b523360cd67c4ad4c201731ff5117a9822f5cbf9e26367aec6`. The always-on `wasm_audit_is_bound_to_current_locks` assertion cannot succeed with these bytes. Re-run the build, Node and applicable browser package lanes before updating those markers. The audit also says frankenscipy is consumed in one section and absent in another. The zero-violation allowlist result does not fix either discrepancy.
6. **Qualify performance and release claims on the artifacts users receive.** The benchmark verifier and producers are real, but target rows are not observations. The old PG-8 builtins result around 3.545767× native against a 1.10× budget is historical and uses an older definition/ABI; it is evidence of earlier risk, not a current verdict. Measure exact current code and wheels on named profiles, retain invalid repetitions, prove the negative regression controls, then optimize the measured bottleneck. Use retained-work elimination, glyph/path reuse and allocation discipline before speculative vectorization. Release gates need actual platform/tier assets, clean installs and signing/publication evidence. Execute the shipped README examples against the downloaded artifacts: the current FMTL example already fails against v0.4.0 because its snapshot minor version advanced. More seriously, a successful published-wheel TeX render is upside down even though its orientation fix is already in newer source. `fm-m54` and `fm-vsq` now explicitly require version-matched example and asymmetric-text orientation acceptance.

The G2 packet deserves particular care: its August 24 update records the later SVG/span and visual-review repairs. The remaining stated G2 blocker is pinned-host PG-1(G2)/PG-7 observations. Repeating the packet's earlier missing-SVG/ratification rows would understate current implementation. G1 is recorded passed; G2, G3, G4a, G4b and G5 remain open in authoritative Beads. None was certified by this audit.

### Would completing the original open backlog close the whole gap?

**It would not provide a reliable, executable route to completion as written.** The original 628-row graph had 593 closed, 26 open and nine in progress. That is task accounting, not a meaningful percentage of product completion.

There is no major mission pillar with literally zero mention anywhere in the backlog: broad epics and convergence gates cover native semantics, the full Python surface, Studio, determinism, performance and distribution. The problem is actionable coverage. Concrete initialization and installed-wheel proof gaps were buried under the general portal umbrella; Python certification and interaction remained broad promises without dedicated runnable leaves; a closed WASM audit now has stale lock identity; the Studio browser experience lacked a specific remaining task. Those are gaps in executable ownership, not justification to invent an entirely new roadmap.

Conversely, some open beads describe work already present: the RNG primitive migration, two duplicate unlisted-transitive reports, the UHD peak-RSS producer, and portions of the drawing/window census. CPU SIMD/glyph tasks also block on the W5 container that still waits for optional CUDA, conflicting with ADR-0007's independence rule. A swarm following only this graph can duplicate landed work and spend effort on policy-blocked CUDA while the ordinary Python product remains inconsistently initialized.

If every broad acceptance criterion were interpreted rigorously and expanded as needed, those epics could discover the missing work. That discovery is precisely what the new explicit leaves and evidence links supply. Merely executing the original ready list and closing its current records is not enough.

### Bridge plan and dependency order

The following ten tasks were created with `br`, using existing containers where applicable. Each includes source evidence, scope boundaries and concrete validation criteria. User-visible changes require real e2e output and failure logs; the narrow Clippy repair reuses the existing SVG semantic tests. Existing implementation and peer assignments are preserved.

| Order / track | Concrete deliverable | Bead and dependency |
|---|---|---|
| First: production semantic integration | Unify wheel/embedded initialization; execute real bridge and animation acceptance; fail on a missing installer or zero matched tests. | `fm-5wq.4.142` under `fm-5wq.4`; P1. |
| Source gate repair | Resolve the unreachable parser-local SVG emitter while preserving the public safe admission and parse/emit semantics; restore Clippy without blanket suppression. | `fm-9fo8`; independent W2 P1 follow-up to the closed geometry epic. |
| Owned-host qualification | Restore exact tracked-fixture inputs, pinned target/tool readiness and sufficient worker capacity; retain real source-bound terminal results and explicit skipped-lane identities. | `fm-5wq.7`; P1, explicitly external. Independent source defects remain separate. |
| Semantic proof | Bind reviewed symbols to native and clean-wheel witnesses; distinguish import, semantic, render and corpus evidence; inject plausible wrong implementations as negative controls. | `fm-5wq.4.143`, after `.142` and existing `.141` helper binding. |
| Certified Python route | Capture full portal input closure and publish native canonical artifacts/manifests; real replay and refusal matrix. | `fm-5wq.4.144`, after `.142`; G4b now explicitly blocks on this leaf. |
| Python output route | Complete remaining console/output flags through existing Reel, including native formats and optional governed ffmpeg, with decoded-output/failure assertions. | `fm-5wq.4.145`, after `.142`. |
| Python interactive route | Host-owned Python scene worker, reload/edit/embed, checkpoint/replay and Studio integration without CPython entering standalone `fmn`. | `fm-ffj.70`, after `.142`. |
| Studio browser | Timeline feedback, navigable inspector/live records/spans, overlay/event controls and usable failure/restart states; real browser tests. | `fm-ffj.71`; independent native UI work. |
| Current WASM proof | Re-derive current dependency graph and qualified target/Node/browser package receipts, then correct the audit in place. | `fm-7wm.6`; independent P1. Preserve `fm-zsu`'s owner. |
| Tracker truth | Review stale completion assertions and container/blocking direction using exact source and tests; retain history and real acceptance obligations. | `fm-5wq.6`; explicitly manual. |

Existing work supplies the other indispensable tracks: `fm-inr.1` for pinned hosts/observations, `.2.3` for real front-door producers and `.4` for scaling/PG-8/PG-A; `fm-d3gt`/`fm-rqc` for the source-unedited gallery; renderer SIMD/glyph leaves for measured CPU improvements; parser fuzz targets for coverage-guided campaigns; `fm-m54`/`fm-vsq`/`fm-zsu` for final artifacts and installs. Evidence comments were added to those relevant existing records rather than cloning their scope. CUDA and the real-aarch64 topology observation now carry `agent:claim:external`, preventing accidental autonomous claims on unmet external prerequisites. The SIMD/glyph edges to `fm-sq8` were changed from blocking dependencies to parent-child containment, preserving renderer ownership and all acceptance criteria while removing the accidental wait for optional CUDA.

The plan was expanded along three dimensions: complete production front-door composition, independent semantic and negative-control proof, and final-artifact/performance qualification. Refinement then checked five concerns: duplicate or already-landed work; binding exclusions and native/Python authority; containment versus actual blocking prerequisites; executable nonzero tests and source/ABI identity; and coverage from every identified gap to an existing or new owner. This retained the full product ambition without adding a substitute engine, unsupported shader adapters, or blanket refactors. Further implementation may reveal new failures; this audit does not promise exhaustive bug discovery.

### Tracker integrity and audit boundaries

At entry, the exported JSONL had 628 rows but SQLite had 626, with one newer database comment. Read-only additive planning and ordinary merge exposed the conflict; no force mode was used. `br sync --reconcile --dry-run` then showed a lossless plan: create the two missing closed rows, import the newer authoritative RNG status, and preserve the newer doctor-bead row/comment. Applying that plan preserved all 84 existing database events and deleted zero rows. A subsequent native flush retained all 628 original IDs; the only existing JSONL delta at that point was the preserved doctor comment and timestamp. All later mutations used `br`.

No implementation bead was claimed or closed. No product source, dependency pin, golden or release artifact was changed. No file was deleted, and no unrelated source was reverted or staged. Existing tracker history, including the newer doctor comment, was preserved. The initial and audited implementation commit remains `d784048148002acdb7d3a914a8e764afcfad3b1d`.

The final graph contains 638 records: 593 closed, 36 open and nine in progress. All 628 original IDs, statuses, assignees, descriptions and comments were checked against the before-image and preserved. Canonical brief/planner integrity checks pass, with two active workstreams (W1 and W10) against the cap of four, eight claimable leaves and no blocking or containment cycles; BV also reports no cycles. The next recommended leaf is `fm-5wq.4.142`, production initialization and native acceptance, within already-active W10. The fresh post-mutation guard token is `v2:9ebd7a3eca0f20e08665aa9a04ba63f09abf8a633097562822146c5609ee30ae:fm-5wq.4.142`; it is a graph-bound handoff token, not a reservation, and must be refreshed if the graph changes.

### Final validation and retained receipts

The local evidence directory is `/tmp/fmn-reality-20260907/`. It retains release metadata/checksums, the downloaded archive/wheel, fresh venv, sample scene and artifacts, Studio probe/result, source gate logs, refusal inventory, direct closure probe, tracker before-images, reconciliation receipts, and planner/BV projections. It is local session evidence, not a newly published or permanently archived gate packet.

Key release hashes: native archive `84bb87f52d71cf8dd4ae633ec9bbb8fd8c2a05a097a714d25c272707894f497b`; cp313 wheel `01399d5b6361ec21531db4c44580f52a3f2ab4896b967012d89001951726175d`. Published inventory was checked at the [v0.4.0 release](https://github.com/Dicklesworthstone/franken_manim/releases/tag/v0.4.0); the [PyPI package endpoint](https://pypi.org/pypi/franken-manim/json) returned 404 during this audit. All direct HTTP probes used the user-required User-Agent.

The complete `scripts/check.sh` result is **not green**, for the missing worker WASM target described above. Standalone `cargo test` also returned 101 on the incomplete remote fixture set after 765 tests passed; standalone Clippy returned 101 on the unused internal SVG emitter. The separate governed-closure binary returned 101 with 14 passes and the stale WASM audit failure. Current-source Rust builds used the worker's Python 3.14.4 environment; the separately verified published wheel used supported CPython 3.13.1. Neither is a current CPython-3.13 wheel receipt.

The changed-file UBS invocation returned exit 3, `no-supported-languages`: only Markdown and Beads JSONL changed, so zero source files were scanned. This is not a source-security pass or a scanner defect. `git diff --check` passed after fixing the report's new trailing whitespace. Tracker-native sync reports matching database/JSONL coverage and zero dirty rows. Final `br doctor` reports healthy workspace integrity and a valid merge anchor, but returns exit 1 for warnings about two `br` executables on PATH and 83 preserved recovery artifacts; none was silently deleted or represented as an all-green doctor result.

## Execution TODO: September 8 follow-through

This checklist implements the user's explicit request for a detailed, persistent TODO list. Its consumers are the user and SilentTurtle resuming the work; it gates declaring the requested bridge plan complete, addressing the prior loss of scope during implementation. Retire it into completed session history when the listed work has terminal evidence or an explicit unresolved prerequisite. It is bookkeeping, with no capability credit. The integrity-control exception is unnecessary because the explicit request supplies the creation gate; a second tracker or dashboard would be needless. Production initialization is the highest-value ready capability; this checklist is bounded to preserving its work queue, not an hour-long planning exercise.

- [x] Recheck source, original audit changes, current planner, inbox and reservations; claim `fm-5wq.4.142` as SilentTurtle.
- [ ] `fm-5wq.4.142`: connect the existing production animation implementation.
  - [ ] Trace installed package, direct extension and embedded Gauntlet initialization; preserve module/class identities and source provenance.
  - [ ] Execute the same idempotent semantic installer in every production route; remove the duplicate package-only activation step without deleting files.
  - [ ] Register the existing real-extension animation acceptance suite alongside bridge acceptance; reject missing installer and zero matched tests.
  - [ ] Resolve any real native family/alignment, curved-path, composition, updater, cleanup or subclass-dispatch failures exposed by activation.
  - [ ] Register a real lifecycle/parity render scenario with deterministic logs and failure artifacts.
  - [ ] Build and install the current wheel in supported CPython; execute both acceptance suites and runtime parity audit.
  - [ ] Run required source gates, review the final diff, reverify acceptance with a fresh review pass, commit only owned work, then close only if complete.
- [ ] Restore prerequisites that prevent reliable full validation.
  - [ ] `fm-9fo8`: repair the unused internal SVG emitter without suppressing warnings or changing public SVG semantics; execute the existing SVG tests and Clippy.
  - [ ] `fm-5wq.7`: inspect RCH source exclusions and restore the tracked corpus fixture in remote inputs; qualify pinned WASM target and CPython tools without deleting artifacts or disturbing peer jobs.
  - [ ] Obtain terminal full `cargo test`, Clippy, formatting and `scripts/check.sh` receipts; classify source failures separately from worker failures and repair actual roots.
- [ ] `fm-7wm.6`: rebuild current WASM target/Node/browser packages, verify fresh-consumer serial/threaded output, and only then update the stale lock-bound audit.
- [ ] `fm-5wq.4.143`: bind authored parity claims to actual native and installed-wheel semantic witnesses, including intermediate state and wrong-behavior negative controls; coordinate existing `.141` helper scope.
- [ ] `fm-5wq.4.144`: wire Python rendering to the native complete input closure, canonical artifact/provenance publication and replay; test successful certification and actual incomplete-input refusals.
- [ ] `fm-5wq.4.145`: connect remaining Python console/output options to Reel; exercise real decoded outputs and encoder/error paths.
- [ ] `fm-ffj.70`: connect host-owned Python workers, edit/reload/embed and checkpoint/replay to Studio; preserve the standalone no-CPython boundary.
- [ ] `fm-ffj.71`: complete the browser timeline, navigable inspector, span/overlay controls and usable restart/error interactions; verify with a real browser.
- [ ] `fm-5wq.6`: reconcile stale RNG, closure-admission, peak-RSS and census assertions against landed code and current tests; preserve assignments, history and still-unmet acceptance.
- [ ] Existing release owners: build a repaired wheel with asymmetric-text orientation acceptance; make documented examples match the released snapshot reader; qualify install/publication/signing prerequisites.
- [ ] Existing performance owners: run real front-door producers on qualified hosts, then use measured bottlenecks to choose CPU work; retain optional CUDA's actual external prerequisites.
- [ ] After each substantive unit: update this TODO and its owning bead with exact evidence, run appropriate checks, and preserve all remaining acceptance until fulfilled.

## How to read this document

FrankenManim is pre-1.0. A capability is implemented only when a concrete source surface and a checkable test or artifact boundary exist. Plan text, a reviewed Parity Ledger row, an inventoried refusal, an old Beads comment, a generated projection, or a queued hosted workflow is not implementation evidence.

Source correctness, compatibility adjudication, tracker state, release packaging, hardware execution, and gate verdicts are distinct evidence lanes. This document keeps those lanes separate.

## Executive state

The repository has a broad native Rust implementation, a separately installed Python compatibility portal, a deterministic agent control plane, and multiple platform/release gates. The highest-risk current gap is semantic convergence and evidence discipline at the boundaries rather than native crate structure.

The W10 portal now has a machine-enforced distinction between **reviewed parity claims** and **runtime truth**: a wheel can audit every `same`/`improved` overlay row against its actually imported namespace, reject schema placeholders or missing symbols, and report the SHA-256 of the exact embedded overlay bytes. The clean-wheel wrapper additionally rejects a wheel whose embedded overlay differs from the checkout. This is a truthfulness mechanism, not evidence that the current wheel has zero contradictions; that installed-wheel gate has not been executed in this editing environment.

The remaining program-level risks are therefore explicit:

- real portal placeholders/refusals still need to be converted into callable semantics or evidence-backed tiers/exclusions;
- a reviewed ledger row must not be confused with a passing runtime self-audit;
- real-hardware, pinned-host, clean-wheel, browser, and release claims require their own receipts;
- autonomous agents must distinguish executable work from human decisions and external-evidence tasks;
- no Beads state may be reconstructed or hand-edited when tracker-native `br` execution is unavailable.

## Product surfaces

| Surface | Current implementation state | Evidence boundary |
|---|---|---|
| Native Rust library | The composition root, scene runtime, mobject/animation stack, native text and math, retained renderer, codecs, output pipeline, and built-in scene corpus exist as workspace crates. Menagerie families include boolean, data, drawing, graph, field, image, model, and 3D surfaces. | Ordinary source checks run through `scripts/check.sh`. Platform certification and gate verdicts remain separate. |
| `fmn` CLI | Typed render, doctor, batch, and Studio surfaces exist. Batch robot output includes terminal per-job records; CLI smoke covers a tiny render and native artifact/manifest publication. | The shipping feature shape is exercised by feature-specific commands in `scripts/check.sh`; a queued hosted run is not the named local/owned-host gate. |
| Studio | Supervisor/worker, replay, event, inspection, and presentation foundations exist. | Platform-native and real-browser presentation evidence is not inferred from Rust unit tests. |
| `fmn-python` portal | A separately installed CPython wheel exposes the pinned `manimlib` namespace with native and authored compatibility behavior. Explicit bootstrap refusals are mechanically inventoried. The wheel now ships `fmn-python --audit-parity [--robot]`, backed by one shared runtime audit module. | `fm-5wq.4` remains in progress. A named refusal, imported symbol, reviewed ledger row, or existence of the auditor is not callable semantic completion. A current clean wheel must actually pass `scripts/check_portal_runtime.sh`. |
| WASM/browser | wasm32 render/player foundations and an npm/package gate exist. | The npm/real-browser release gate is opt-in and independent of ordinary source checks. |
| Distribution | Tagged `v0.1.0` through `v0.4.0` are prereleases. | Cross-platform artifacts, hardware-specific execution, clean wheels, and certified reproducibility require independent receipts. |

## 2026-09-04 animation repair checkpoint

The live source audit found that earlier continuation reports overstated completion: the package initializer installs `manimlib/_animation_semantics.py`, but the direct PyO3 bootstrap has not been unified with that installer. A workflow intended to perform an activation or composition cutover is not the cutover itself. The older standalone continuation and activation machinery remain historical source, not evidence of executed native behavior.

Three incremental commits repair concrete failures without replacing native Choreo scheduling:

| Commit | Landed change | Focused evidence |
|---|---|---|
| `ff49cd4b1f8f572b5e1fd5f1175e4de5dc4f2532` | Preserve deferred composition roots and timing sentinels; validate ordinary targets; restore targetless `CyclicReplace`/`Swap` lowering. | Six real-bootstrap Python contract tests pass after reproducing constructor and target-resolution failures. |
| `dd2bdce0ebd121268a68352783fc3bf649191fea` | Route Transform through installed family/lag/path/subclass dispatch; keep whole-mobject overrides; interpolate uniforms on point-free roots; normalize the new method's provenance. | Expand the contract suite to 13 tests; five new interpolation failures reproduced before the fix and resolved afterward. |
| `225b589d6281800723990479c991bc6e13e113fd` | Restore executable runtime-audit test text while retaining the intended descriptor fixture repair. | Corrupted live blob fails byte compilation; restored UTF-8 source and all 15 runtime-audit tests pass. |

The composition exception is limited by class identity (`isinstance` against `AnimationGroup`), not by a forgeable `_native_kind` label. The Transform repair replaces the stale concrete override which previously bypassed installed submobject hooks and rejected curved paths. Geometry follows the supplied path; record styles and uniforms follow their separate interpolation rules. Locked fields and subclass whole-mobject overrides retain their meaning.

### Executed validation

All of the following passed on the exact local source bytes uploaded into those commits, with CPython 3.13.5 and NumPy 2.3.5:

| Command | Tests passed |
|---|---:|
| `python scripts/test_animation_semantics_installer.py -v` | 13 |
| `python scripts/test_schema_provenance.py` | 3 |
| `python scripts/test_audit_portal_runtime.py -v` | 15 |
| `python scripts/test_portal_parity_cli.py` | 6 |
| `python scripts/test_verify_portal_runtime_receipt.py` | 8 |
| `python scripts/test_python_helper_aliases.py` | 26 |
| `python scripts/test_library_constructor_authority.py` | 18 |

Total: **89 focused Python tests**. Changed Python files also passed `py_compile`, and `scripts/check.sh` passed `bash -n`. Git blob hashes returned by the connector matched the tested local files before publication.

### Limits and remaining integration

The new contract suite executes real bootstrap animation definitions and the shipped installer, but replaces Rust mobject storage with a NumPy fixture. It deliberately does not simulate or certify native data/family alignment. Its curved-path test proves dispatch and separation of point fields from styles, not native arc-kernel correctness. Additional cases in `crates/fmn-python/tests/animation_semantics.py` were byte-compiled but not run with the native extension.

No Rust toolchain or `br` was available in this editing environment. The full `scripts/check.sh`, native bridge acceptance, installed-wheel semantics/parity, Rust formatting/Clippy, and UBS are **not verified by this checkpoint**. Direct-extension/bootstrap unification and a permanent native acceptance invocation still require implementation and execution. There is no claim of completed `AnimationGroup`/`Succession` lifecycle support, a complete portal, or a 1.0 gate.

The complete exported Beads ledger was inspected without mutation. It still records `fm-5wq.4` and `fm-ai1` as in progress; the canonical planner recommends `fm-5wq.4.141` on that ledger. No task was claimed or closed, and JSONL was not hand-edited. The fixes were published through fast-forward updates to the existing canonical refs; no extra branch, new workflow, dependency change, or file deletion was introduced.

## W10 runtime parity truth model

The portal has two complementary truthfulness ratchets.

### Explicit refusal inventory

`scripts/audit_portal_refusals.py` statically inventories authored `NotImplementedError`/capability refusals in the bootstrap. It proves that known refusal sites are named and mechanically visible. It does **not** prove that a reviewed symbol is callable.

### Runtime SAME/IMPROVED audit

`crates/fmn-python/python/fmn_python/parity_audit.py` is the single semantic implementation used by both checkout tooling and the installed product. It parses only the authored `[status]` section of `API_OVERLAY.tsv` and treats `same` and `improved` as implementation claims.

For every such row, the audit imports the recorded module and resolves the qualified symbol. It fails the claim when:

- the module cannot be imported;
- the reviewed symbol is missing;
- the runtime value carries `_fmn_schema_placeholder=True`.

`tiered`, `excluded`, and `unreviewed` rows are intentionally not treated as implementation claims. The report is schema `fmn.portal.runtime-audit` version 1 and contains deterministic counts, sorted contradictions, and `overlay_sha256`, the SHA-256 of the exact UTF-8 overlay bytes audited.

The installed wheel exposes the same proof directly:

```bash
fmn-python --audit-parity
fmn-python --audit-parity --robot
python3 -m fmn_python --audit-parity --robot
```

Audit-mode exits are typed:

- `0`: all reviewed SAME/IMPROVED runtime claims resolved to non-placeholder values;
- `1`: at least one runtime contradiction;
- `2`: malformed embedded audit contract or invalid audit arguments;
- existing namespace-collision behavior remains capability exit `4` before native use.

`scripts/check_portal_runtime.sh` is the clean-wheel boundary. It first imports the installed `manimlib`, invokes the wheel's own robot self-audit, preserves that exit, then compares the report's `overlay_sha256` with the checkout `API_OVERLAY.tsv`. A semantically green but stale wheel therefore fails rather than proving an obsolete ledger.

The mandatory source gate does **not** pretend to execute that installed-wheel boundary. Instead `scripts/check.sh` byte-compiles and runs hermetic regressions for the shared parser/resolver and wheel-facing CLI. Actual installed-wheel behavior stays a separate release/evidence lane.

## Agent control plane

The control plane follows a one-authority/many-projections design. The exported Beads JSONL is authority; every other document is derived and disposable.

| Layer | Contract | Role |
|---|---:|---|
| Broad snapshot | `agent_brief` snapshot version 5 | Bounded parsing, blocking integrity, broad queues, stale/unowned diagnostics. Never a claim authority. |
| Leaf planner | `fmn.agent.next` version 4 | Exact scope, leafhood, containment, activation, claim-kind policy, ranking, and recommendation. |
| Claim policy | `fmn.agent.claim-policy` version 1 | Exact `agent:claim:*` label vocabulary and autonomous/manual/external classification. |
| Claim guard | `fmn.agent.claim-guard` version 2 | Stable JSON/token envelope for graph/plan/policy/schema-bound revalidation. |
| Claim input | `fmn.agent.claim-input` version 3 | Complete graph, plan, policy, and schema input to the token digest. |
| Claim graph | `fmn.agent.claim-graph` version 3 | Canonical core graph plus complete exported task semantics. |
| Task semantics | `fmn.agent.task-semantics` version 1 | Descriptions, design, acceptance, notes, labels, metadata, and every non-core extension field. |
| Claim executor | `fmn.agent.claim` version 6 | Atomic Beads claim, bounded child execution, structured response proof, explicit export, semantic invariant, and core delta receipt. |
| Human brief | deterministic Markdown | Planner-normalized context for humans and agents; never a second tracker. |

The public token remains:

```text
v2:<claim-sha256>:<issue-id-or-none>
```

The token binds the complete task graph, planner output, nested claim-policy contract, policy bounds, and schema versions. Any authoritative change makes the old token stale.

### Exact governed scope

One shared classifier accepts only an anchored `G0` or `W1` through `W11` prefix followed by a word boundary or `:`. `W0`, `W12+`, lowercase forms, zero-padded forms, and embedded prefixes are `UNSCOPED`.

Open unscoped work remains visible but never enters autonomous ranking. Any active unscoped issue invalidates the plan. `G0` consumes one of the four active-workstream slots. ADR-0022 records this single-classifier decision and `scripts/test_agent_scope.py` locks broad/planner parity.

### Exact autonomous claim kind

ADR-0023 defines one closed Beads label namespace:

```text
agent:claim:auto
agent:claim:manual
agent:claim:external
```

Unlabelled work defaults to `auto`. Manual and external leaves remain visible in `non_autonomous_ready` but cannot be recommended. Unknown, duplicate, conflicting, or malformed reserved labels on live issues invalidate planning before any usable payload.

`scripts/agent_claim_policy.py` interprets canonical labels already loaded by `agent_task_semantics`; it does not reread or edit JSONL. `scripts/test_agent_claim_policy.py` covers the default, all three modes, no-recommendation behavior, malformed labels, closed-history tolerance, and the exact machine contract. The suite is wired into `scripts/check.sh`.

### Deterministic recommendation order

Among autonomous governed dependency-ready unassigned leaves:

1. prefer work in an already-active governed workstream;
2. lower numeric priority;
3. greater immediate-unblock pressure;
4. greater direct-blocker pressure;
5. most recently updated;
6. lexical issue ID.

Non-epic issues with live `parent-child` descendants are containers, not leaves. A recommendation is not a lease; current `main`, Agent Mail, file reservations, active peers, and the exact Bead still require inspection immediately before the guarded executor runs.

## Full task-semantic claim binding

The claim graph no longer treats the narrow broad-planner model as the complete task contract. `scripts/agent_task_semantics.py` binds descriptions/design/acceptance/notes, owners and estimates, source/due/defer metadata, labels, future extension fields, extended dependency records, and complete comments.

Issue-row order, dependency-array order, object-key order, and label order are normalized as representation only. Duplicate labels remain represented. Comment order and ordinary array order remain significant.

The loader brackets the established bounded parser with stable before/after reads and requires the broad/core projection, semantic projection, and source digests to agree. Unknown nested metadata is bounded by depth and node ceilings. The executor preserves all task-semantic fields on the selected issue and every unrelated issue across an atomic claim export.

## Atomic guarded claim execution

The only autonomous mutation path is:

```text
br update ISSUE --claim --actor ASSIGNEE --json [--transition-comment TEXT]
br sync --flush-only
```

The executor keeps token replay, Beads' storage-level compare-and-set, response validation, export, semantic preservation, and core postcondition verification under one shared-common-directory advisory lock.

Each child process has a bounded wall-clock deadline, produced-output ceiling, retained-diagnostic ceiling, concurrent stream draining, and process-tree cleanup. Exit `5` means **no verified success receipt**, not proof that native tracker state is unchanged. Recovery begins with `br show`, `br sync --status`, and `git status`; the old token must not be replayed blindly.

The lock coordinates cooperating worktrees inside one clone. It cannot serialize another clone, direct manual Beads activity, Agent Mail, file reservations, or unrelated movement of `main`.

## Python portal convergence truth

`fm-5wq.4` remains in progress. Its latest reality-check in the exported ledger supersedes older “100% reviewed” commentary: the native engine was described as structurally complete, while the live Python portal still had 63 `NotImplementedError` sites, 930 placeholder methods, and 2,413 rows requiring semantic review at the time of that comment.

Those numbers are a dated diagnostic, not an implementation percentage. The governing rules are:

- reviewed ledger coverage is not universal callable implementation;
- refusal inventory coverage is not implementation coverage;
- SAME/IMPROVED requires real callable semantics and focused evidence;
- a runtime self-audit tool existing is not the same thing as that audit passing;
- representative clean-wheel semantics remain required across API families;
- each refusal falls only through implementation or an evidence-backed tier/exclusion;
- no W10, G4, or 1.0 closure is claimed here.

## Current open obligations

### 1. W10 semantic surface

Run the new runtime audit against freshly built clean wheels and treat every reported contradiction as a direct truth defect: either implement the reviewed symbol, or correct the overlay to a justified tier/exclusion. Continue converting high-value shared portal placeholders/refusals into real behavior with focused tests. Prefer load-bearing base classes and shared mechanisms over isolated leaf wrappers so each tranche collapses many downstream gaps.

### 2. Tracker-native classification and reconciliation

Known human-decision and external-evidence leaves should receive ADR-0023 labels through `br`. Existing descriptions, statuses, dependencies, and comments should be reconciled from a real checkout. Never reconstruct `.beads/issues.jsonl` from connector output.

### 3. External evidence lanes

The following remain independent and must not crowd autonomous implementation ranking once correctly labeled:

- real aarch64 topology fixtures;
- pinned-host performance-gate receipts;
- platform-native execution matrices;
- SIMD-tier and certified cross-platform bit evidence;
- npm/WASM real-browser packaging;
- clean-wheel gates on every supported platform;
- ffmpeg/video-container equivalence receipts;
- release signing, publication, and credential-bound effects.

### 4. Gate verdicts and release claims

A green source test is not a gate verdict. The named owner or delegated reviewer must record the committed evidence packet. Tagged prereleases do not imply the 1.0 contract is earned.

## Verification entry points

```bash
# Mandatory local or owned-host repository gate
scripts/check.sh

# Runtime parity truth from a current installed wheel
fmn-python --audit-parity --robot
scripts/check_portal_runtime.sh

# Checkout-side runtime auditor (useful when the portal is importable here)
python3 scripts/audit_portal_runtime.py --check

# Focused installer contracts (NumPy fixture; not a native/wheel verdict)
python3 scripts/test_animation_semantics_installer.py -v

# Whole graph, governed plan, and human context
python3 scripts/agent_brief.py --format json --check
python3 scripts/agent_next.py --format json --check
python3 scripts/generate_agent_brief.py --stdout

# Guarded atomic claim
token="$(python3 scripts/agent_claim_guard.py --require)"
issue="${token##*:}"
br show "$issue"
# Check current main, Agent Mail, reservations, peers, and task scope.
python3 scripts/agent_claim.py \
    --expect-token "$token" \
    --issue "$issue" \
    --assignee "$FMN_AGENT_ID" \
    --command-timeout-seconds 60 \
    --command-output-budget-bytes 16777216 \
    --dry-run
```

## Historical evidence for the runtime-audit checkpoint

For substantive checkpoint `7aeb3f40`:

- the repository contains one shared wheel/checkout runtime parity implementation rather than duplicated parsers;
- `same`/`improved` runtime claims fail closed on module-import failure, missing symbols, and `_fmn_schema_placeholder=True` values;
- the installed product exposes deterministic human and robot self-audit surfaces with typed exits;
- every valid audit report binds the exact embedded overlay by SHA-256;
- `scripts/check_portal_runtime.sh` rejects a wheel whose embedded overlay hash differs from the checkout;
- hermetic regressions cover real reviewed callables, placeholders, missing modules/symbols, tiered/excluded exemptions, malformed/duplicate status rows, deterministic ordering, CLI argument/error behavior, and overlay identity;
- those regression files are wired into the mandatory source gate.

At that earlier checkpoint, the connector editing environment did **not** execute the Python regressions, build or install the current wheel, run `scripts/check_portal_runtime.sh`, run the complete Cargo/Rust repository gate, execute UBS, call tracker-native `br`, use Agent Mail, exercise release credentials, or produce hardware/browser/platform receipts. Hosted GitHub Actions runs triggered by those incremental commits were pending or cancelled by newer `main` pushes and are not used as acceptance evidence. The separately bounded Python execution evidence from 2026-09-04 is recorded above.

Therefore this checkpoint claims the **implementation of the runtime truth mechanism and its committed tests**, not a passing parity verdict for the current wheel. No `.beads/` file was reconstructed or replaced.

## Protocol for the next agent

1. Read `AGENTS.md`, the comprehensive plan, this status file, ADR-0022, ADR-0023, and the exact Bead before editing.
2. Run the broad check and guarded planner. Treat every nonzero result as a refusal, not a hint.
3. For W10 work, build/install a fresh wheel and run `scripts/check_portal_runtime.sh`; use contradictions as a prioritized semantic defect queue.
4. Inspect `main`, reservations, peers, and the recommended Bead; then use the guarded atomic executor.
5. Prefer shared mechanisms and base abstractions that retire many portal or runtime gaps together.
6. Keep implementation, tests, tracker transition, and evidence in the same small tranche.
7. Commit directly to `main` only as an atomic fast-forward; never batch unrelated work into a giant final commit.
8. Record what actually ran, what remains external, and why any claimed capability is earned.

## Truthfulness rules for future updates

- Record exact commands and the source commit they exercised.
- Do not convert “compiled,” “reviewed,” “inventoried,” “imported,” “auditable,” “queued,” or “pending” into “implemented,” “compatible,” or “green.”
- A runtime-audit implementation does not prove an audit pass; preserve the actual report and overlay hash when claiming one.
- Do not infer hardware or artifact evidence from another platform.
- Do not close a parent merely because one child or one census reaches 100%.
- Distinguish broad readiness, autonomous claimability, human decisions, and external evidence.
- Preserve the one-authority model: projections may be regenerated; Beads state may only be changed through tracker-native operations.
