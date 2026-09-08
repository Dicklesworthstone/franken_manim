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
  - [x] Trace installed package, direct extension and embedded Gauntlet initialization; preserve module/class identities and source provenance.
  - [x] Execute the same idempotent semantic installer in every production route; remove the duplicate package-only activation step without deleting files. The named native animation suite passed on the second run, including identity preservation and missing-installer rejection; installed-wheel acceptance remains below.
  - [x] Register the existing real-extension animation acceptance suite alongside bridge acceptance; reject missing installer and guard against zero matched tests in `scripts/check.sh`.
  - [ ] Resolve any real native family/alignment, curved-path, composition, updater, cleanup or subclass-dispatch failures exposed by activation.
    - [x] Preserve numeric nested uniforms and discrete flags/joint styles during interpolation; actual native acceptance passes.
    - [x] Forward remover flags through the shared constructor and replace the obsolete arc-refusal witness with an analytic midpoint, endpoint and updater-detachment check; the initial installed wheel passes the full bridge script.
    - [x] Reproduce `Scene.play` bypassing a Transform subclass against the native library; route overrides through the existing Choreo callback boundary and observe the regression pass.
    - [x] Reverify helper `update_mobjects(dt)`, cleanup and FocusOn/Flash helper initialization after the complete callback lifecycle is enabled. Both real native suites passed in `production-composition-timelines.log` (RCH hz3, exit 0, 2 tests); explicit intermediate-alpha and exact-dt assertions remain.
    - [x] Correct composition wall-clock scaling and just-in-time Python succession begin/finish; observe both complete native suites pass.
    - [ ] Repair the subsequently reproduced mixed native/Python same-object ordering defect. `mixed-succession-native-before.log` exits 101 with incorrect intermediate geometry despite correct endpoints. Exercise both directions, simultaneous argument order, nested coarse sampling, abort cleanup and real PNG capture before calling this complete.
      - [x] Drive actual native leaves in the same ordered release window as Python leaves; both production suites pass in `production-native-leaf-drivers.log` (RCH hz3 exit 0).
      - [x] Verify expanded simultaneous-order and exception witnesses, including actual Rust RecordBuffer locks and native suspension: both native suites pass in `production-copy-abort-hz3.log` (RCH hz3 exit 0, 2 passed, 19 unrelated filtered). The fresh 05:00 wheel passes both full acceptance scripts after lock-set initialization is repaired.
      - [x] Reexecute the PNG scenario with mixed same-object succession: `animation-lifecycle-catalog-current.log` (RCH hz3 exit 0, 1 passed, 7 unrelated filtered, 111.90 seconds).
      - [x] Reproduce and repair mixed compositions calling a catalog-name string as a function; the fresh wheel passes group and play-level `rate_func="linear"` frame witnesses.
    - [x] Verify the corpus-discovered `TransformFromCopy` double-copy repair. Its public constructor lowers its existing copy through native ReplacementTransform. `corpus-after-copy-hz3-recovered.log` ends RCH hz3 exit 0: both actual corpus tests pass (20 unrelated filtered, 1455.01 seconds). All eight original structural baselines match, their facts reproduce on reexecution, and all eight PNGs are byte-identical across two passes. BeamSplitter is exactly 24 roots/194 family members; the failing pre-fix run was 25/219. This snapshot predates the final two-pass worker cleanup, which has separate E2E/native receipts.
    - [x] Preserve native-created composition roots in Python scene inspection. The real render scenario reproduced an empty `Scene.mobjects` after `Succession`; missing proxies now bind to existing native handles, retaining child identities and synchronizing parent edges. `animation-lifecycle-e2e-6.log` has terminal remote exit 0 and proves later remove/add operations too.
  - [ ] Register a real lifecycle/parity render scenario with deterministic logs and failure artifacts.
    - [x] Add `lifecycle.python_animation_semantics.v1` and its named focused test; correct the initial `ScenarioClass` spelling compile error.
    - [x] Execute the registered scenario, decode its native PNGs and inspect its deterministic log/failure-artifact behavior. One named scenario passes; its earlier assertion failures emitted the expected FMNA bundle and deterministic failure logs.
  - [ ] Build and install the current wheel in supported CPython; execute both acceptance suites and runtime parity audit.
    - [x] Qualify CPython 3.13.15, NumPy 2.5.2 and Maturin 1.14.1; build and install an actual wheel. Its full bridge script passes with the workspace version supplied independently.
    - [x] Rebuild callback, copy, cleanup and catalog-rate fixes into a fresh wheel/environment and execute both suites plus the audit. The 05:00 wheel SHA-256 is `d300c976ce0fbaae8187c1b4f5185284a59c40c8d7a72604a511bbccb23e3689`; both suites pass, then the audit rejects the same 112 contradictions. `installed-wheel-catalog-0500.log` has RCH hz3 exit 1; its raw receipt is retained in `.rch-results/portal-runtime-20260908-0500/`. This is executed acceptance, not a green compatibility handoff.
    - [x] Add both acceptance scripts to the permanent installed-wheel check, before its runtime audit.
    - [x] Execute the initial wheel's runtime audit against the exact current overlay/schema: terminal exit 1, 112 contradictions (107 placeholders and 5 missing symbols). Keep these failures visible; the receipt is not a parity pass.
  - [ ] Run required source gates, review the final diff, reverify acceptance with a fresh review pass, commit only owned work, then close only if complete.
- [ ] Restore prerequisites that prevent reliable full validation.
  - [ ] `fm-9fo8`: repair the unused internal SVG emitter without suppressing warnings or changing public SVG semantics; execute the existing SVG tests and Clippy.
    - [x] Remove only the unreachable parser-local function; preserve public safe emission. Workspace Clippy now exits 0 after four separate iterator warnings were also repaired without arithmetic/order changes.
    - [x] Execute existing SVG tests: 16 filtered unit tests pass, followed by an explicit integration invocation with 5 SVG and 2 UTF-8 tests passing and none filtered. The first filter ran zero integration tests and supplies no integration evidence.
    - [x] Repair the later Rustdoc public-to-private link by targeting `crate::SvgDocument`, with no lint suppression. The targeted `-D warnings` Rustdoc invocation passes in `rustdoc-and-full-test-current.log`; the following workspace test fails in Python worker teardown.
    - [ ] Obtain the complete workspace-test receipt before closing.
  - [ ] `fm-5wq.7`: inspect RCH source exclusions and restore the tracked corpus fixture in remote inputs; qualify pinned WASM target and CPython tools without deleting artifacts or disturbing peer jobs.
  - [ ] Obtain terminal full `cargo test`, Clippy, formatting and `scripts/check.sh` receipts; classify source failures separately from worker failures and repair actual roots.
    - [x] Execute the previous full test gate: 2247 passed, 2 corpus failures, 11 ignored across 115 completed binaries (`full-cargo-test-wasm-audit.log`, RCH exit 101). Qualify the corpus's host-only tqdm dependency without adding it to the product requirements.
    - [x] Rerun all eight original corpus seeds with tqdm: all import and execute; seven structural baselines match, BeamSplitter exposes the double copy, and the two-pass real-PNG determinism test passes (`corpus-current-tqdm.log`, RCH exit 101 overall).
    - [ ] Complete current gates after the fixes. Own stale source-gate job 30011725038223381 was cancelled after reproducing the already-fixed missing lock-set error; its partial log is not a gate verdict. Worker vmi1264463's memory-pressure admission refusal is also not a source verdict.
      - [ ] Reexecute the full gate after the Rustdoc fix. `source-gate-catalog-current-vmi.log` passed the earlier check/Clippy axes and native acceptance, then found the broken link and ended RCH 137/SIGKILL while other documentation jobs remained. It never reached `cargo test`, and predates the public-hook/output changes.
      - [x] Reproduce the next actual failure: `rustdoc-and-full-test-current.log` ends RCH vmi exit 101 after 793 passes, one failure and two ignored; the E2E catalog catches two native proxies destroyed on another thread.
      - [x] Reproduce and repair one retained owner without weakening ADR-0015's strong method-cache contract. `cache-owner-before.log` fails an actual cached class owning a native proxy; explicit cache release under the owner's GIL makes that regression pass on two distinct owner threads.
      - [ ] Trace the remaining lifetime failure. `cache-owner-and-full-after.log` passes the new regression, then the full catalog still fails with two cross-thread destructor errors (794 total passes including the first focused test). The cache repair alone is insufficient. Add weak proxy observers to identify retained owners during their original worker's teardown; preserve unraisable/thread guards and ordinary test concurrency.
        - [x] Identify the survivors: two EventDispatcher Points retained through scene-source class/global cycles (`proxy-survivor-diagnostic.log`, RCH vmi 101, two passes/two failures). Further diagnostic runs confirm the native method cache is empty.
        - [x] Test and discard the incorrect CPython lookup-cache hypothesis: `_clear_internal_caches` does not repair the failure (`dispatcher-cpython-cache-after.log`, 101), and is absent from the final change.
        - [x] Execute the bounded cleanup repair. A second collection plus an explicit PyO3 drain passes all four Python E2E scenarios (`dispatcher-deferred-refs-after.log`, RCH 0). The control without the drain also passes the previously failing output scenario (`dispatcher-two-collections-control.log`, RCH 0, one executed/eight filtered), so only the second collection is retained. Weak proxy, weak module, empty method-cache and unraisable guards remain mandatory; the final full catalog/source replay is running in `source-gate-owner-cleanup-current.log`.
        - [x] Scan the final Rust cleanup source: `ubs-owner-cleanup-current.log` exits 0, four files, zero critical, 1206 warnings and 381 informational findings. No additional suppression was required.
        - [x] Execute the complete E2E catalog on the smaller final cleanup: all nine tests pass in `source-gate-owner-cleanup-current.log`. The following source gate fails in the extracted installer fixture, before Rust checks: it omitted the new timing adapters and retained the obsolete unset-duration expectation.
        - [x] Repair that fixture boundary explicitly. It now compiles the production adapters unchanged, checks calls against four fixed interval responses, and verifies Reference group durations 1/1.05/2. It does not simulate Choreo's recurrence or supply native evidence. All 13 contract tests pass locally and on the pinned remote host. Its existing checked-in-AST execution receives one exact, explained UBS annotation; `ubs-timing-fixture-adjudicated.log` exits 0 (zero critical/warnings, two informational). The final full source replay is `source-gate-timing-fixture-current.log`.
        - [x] Reproduce the newly exposed updater leak. That replay passes fixtures, Rust checks, three real native suites, Clippy and Rustdoc, then reports a ladder failure. Its own job 30011935676170249 was cancelled (terminal 143) to isolate the failure while the long corpus was still running; it is not a full-gate receipt. `ladder-lifetime-before.log` ends RCH vmi 101 with retained native proxies. `ladder-cycle-witness-before.log` independently fails ordinary unreachable BatchedUpdater ownership before test-worker cleanup.
        - [x] Expose both Rust-owned BatchedUpdater references to Python GC and clear them through the supported PyO3 protocol. Preserve unsendable confinement, arithmetic and crossing accounting. `ladder-cycle-witness-after.log` ends RCH vmi 0: one complete ladder suite passes, including member-only and callback-owned cycles (21 unrelated tests filtered).
        - [ ] Execute the added real-scene cycle witness through bridge acceptance, registered portal E2E and a fresh wheel; obtain the whole source gate. The current source replay is `source-gate-batched-gc-current.log`. The already-running timing wheel predates this GC repair and cannot validate it.
          - [x] This replay completes the entire bare `cargo test`: 2,942 passed, zero failed, 11 ignored across 164 completed summaries, including all 22 native portal tests and doctests. The additional batch CLI and ffmpeg-feature tests also pass. The complete script remains running its Node/WASM axis; this snapshot predates the primary-color, content-fingerprint and EventDispatcher edits and cannot certify those later bytes.
          - [x] The whole `source-gate-batched-gc-current.log` subsequently completes: RCH vmi1264463 terminal exit 0, 2469.8 seconds. Node/WASM smoke passes with digest `1f248a71347b82aa`. The opt-in npm/browser release gate is explicitly skipped in this run, retaining its separate earlier execution. Final unchanged committed-source verification remains required for later edits.
          - [x] Diagnose the failed 08:03 installed replay instead of weakening collection checks. That wheel silently contains the identical old extension as the 07:30 wheel (extension SHA-256 `d26be38965cd3bb1e53533309d1986ea90cb5d22392e15a5fadd02ed543077db`). Its updater is untracked and exposes neither Rust field to GC. Current remote Rust bytes match local source, but their preserved mtime predates the older artifact. Thus this is stale binary reuse, not a second demonstrated failure of the repaired implementation.
          - [ ] Qualify the pinned Cargo manual's content-fingerprint mode in existing `.cargo/config.toml`. Both `unstable.checksum-freshness=true` and `build.fingerprint=content` are required; the first flag alone still reused the old binary (`installed-gc-color-0809-checksum.log`, exit 1). `installed-gc-color-0813-content.log` confirms both effective settings and actual recompilation. Require new wheel behavior and a complete committed-source gate; no cache deletion or timestamp touch is used.
            - [x] The content-mode wheel passes all three installed suites, including real-scene updater collection and primary-color decoding. `installed-gc-color-0813-content.log` ends RCH hz3 exit 1 only at the unchanged global audit (101 contradictions). Wheel SHA-256 `d30c88bdf1108d89c89a1d2de6ec3fcce245a711e727662a5b478b36fbda3ecc`; extension SHA-256 `1cdc206f837e8d4c8e8b2a1d097b06b7d859cc2e42b68eef1e981b0b95546216` differs from the stale binary and contains the actual GC-clear diagnostic. Raw receipt `.rch-results/portal-gc-color-20260908-0813/tmp.JErFxSqcK8`. This predates the EventDispatcher repair.
- [ ] `fm-7wm.6`: rebuild current WASM target/Node/browser packages, verify fresh-consumer serial/threaded output, and only then update the stale lock-bound audit.
  - [x] Re-derive the actual wasm dependency tree: it now includes fnp-random-core and fsci-linalg/fft/runtime, contradicting the old audit's absence claim.
  - [x] Qualify the pinned wasm32 nightly target, wasm-bindgen 0.2.127 and Binaryen 117 with verified archive hashes; install Node 22.22.1, webpack 5.109.2, webpack-cli 7.2.2 and Chrome 152 on the owned worker.
  - [x] Run the target build and Node smoke with current locks; retain terminal exits and output hashes.
    - [x] `cargo check --target wasm32-unknown-unknown -p fmn-wasm` passes remotely on hz3 with the current fnp/fsci dependency closure (`wasm-target-hz3-admission.log`, exit 0).
    - [x] Execute the actual Node smoke: `wasm-node-current-2.log` exits 0 under `--locked`, digest `1f248a71347b82aa`, Node 22.22.1.
  - [x] Run the existing fresh-consumer browser/package gate, including serial/threaded byte comparison and the COOP/COEP negative case: RCH hz3 exit 0, `wasm-package-current-3.log` and `.rch-results/wasm-package-20260908-0410/receipt.json`. Source commit a40c4a88 has `source_dirty=true`; no publication occurred. Chrome's matching sandbox helper was qualified without disabling the sandbox.
  - [x] Update the audit's dependency statements and lock hashes only after those executions pass (`822ebee0`).
  - [ ] Reexecute the governed closure and full source gates against the corrected audit before closure. The full `cargo test` run is in `full-cargo-test-wasm-audit.log`.
- [ ] `fm-5wq.4.143`: bind authored parity claims to actual native and installed-wheel semantic witnesses, including intermediate state and wrong-behavior negative controls; coordinate existing `.141` helper scope.
  - [x] Convert its blocking edges to `.141`/`.142` into related links after those mechanisms execute successfully. Their required global parity gate needs this remaining work; leave both predecessors open and preserve every acceptance criterion. Claim `.143` with a verified fresh guard/executor receipt.
  - [ ] Verify the first public-hook batch against both native and fresh-wheel suites: base `Mobject.surround`, SHA-256 `hash_string`, Scene seed/orientation defaults and camera reset, `MoveToTarget.check_validity_of_input`, both Write timing helpers, and both subset-reveal public update methods.
    - [x] Native production suites pass: `production-public-hooks-batch1-hz3-3.log`, RCH hz3 exit 0, 2 passed, 19 unrelated filtered, 162.78 seconds. Earlier failed runs exposed the obsolete seed-absence assertion and my missing parentheses on `Scene.time()`; both failures are retained. The admission-only exit 103 is not a test result.
    - [x] First-batch UBS completes: `ubs-public-hooks-batch1.log`, exit 0, two files, zero critical, 627 warnings and 661 informational findings. Its input precedes the two test-assertion corrections; it does not cover the subsequent output wiring.
    - [x] Execute the current installed wheel and measure the complete audit delta. Wheel `c9b20e0fc83f9748498c886692a107a5ac5f106968f8eef28135cc10847f3109` passes both semantic suites; its complete audit drops from 112 to 103 contradictions, exactly the nine symbols above. The unchanged overlay/schema still reject 100 placeholders and three missing symbols. `installed-native-output-0550.log` ends RCH exit 1, not a full parity pass.
  - [ ] Complete composition timing inspection, border-outline and fade-ghost helpers, Transform configuration, and matching-pair/block hooks over their real mechanisms.
    - [ ] Verify the second timing slice: initialize actual public group intervals/max duration and bind both public timing methods to Choreo's existing builder. The mixed Python/native callback route now consumes that builder instead of a Python recurrence. The first run passes bridge/output but rejects an obsolete `run_time is None` witness. Reference defaults require Group/LaggedStart/Succession durations 1/1.05/2; that corrected witness passes in the second run. Own superseded job 30011860413579281 was cancelled after identity/dry-run checks when progress stalled; its unfinished suites, E2E and Clippy are not passing evidence. Current complete replay remains required.
      - [x] Execute all three native suites and the fresh timing wheel. `installed-timing-0730-one-job.log` ends RCH hz3 exit 1 after all three installed acceptances pass. Wheel SHA-256 `66f5ab259e944263ed92300bdd5da82e17c844da493a12eae12d2aedae419990`; unchanged global audit now rejects 101 contradictions (98 placeholders, three missing). The exact two removed contradictions are AnimationGroup's timing methods; no new contradiction or authority change. Raw receipt `.rch-results/portal-timing-20260908-0730/tmp.VYUaCwPFLb`. This predates the BatchedUpdater GC fix and additional primary-color witness.
  - [ ] Complete native-span selection/grouping and content helpers for StringMobject/Tex/MarkupText/Text/OldTex; preserve Unicode source spans, live glyph identity and the single native typesetting pass.
  - [ ] Exercise SVG path command/arc helpers and StreamLines sampling, integration, line construction and style helpers against the existing Chisel/Atlas primitives.
  - [ ] Connect config, console entry points, render-group helpers and remaining writer initialization to the existing front doors; coordinate `.145` rather than inventing a parallel output engine.
  - [ ] Adjudicate Reference-imported third-party names against the governed substrate and existing exclusions using actual source evidence; do not remove placeholder markers or downgrade claims to manufacture a green audit.
  - [ ] Build the requested symbol-to-witness mapping over existing semantic suites and e2e registrations; report import, invocation, rendered-state and corpus coverage separately.
  - [ ] Execute missing-witness, zero-case, placeholder and plausible-wrong-geometry negative controls; require current native/wheel identities and deterministic NDJSON/FMNA evidence.
  - [ ] Audit EventDispatcher's authored semantics against the pinned `event_handler/event_dispatcher.py`: the current Python fan-out lacks Reference mouse hit-testing and drag capture, and add/remove return values differ. Bind those existing scene/event mechanisms with behavioral witnesses; an importable method or lower placeholder count alone cannot certify them.
    - [x] Reproduce the spatial failure in the actual installed timing wheel: `installed-event-hit-before.log`, direct read-only hz3 SSH execution, exit 1. Moving over the left square calls both separated squares' listeners. This is executed wrong behavior, not a schema-marker inference.
    - [x] Bind existing native `is_point_touching` to mouse dispatch and capture drag listeners on press until release. Restore Reference coordinate-array state, last-callback return values, duplicate registrations/remove-all behavior and dispatcher operators; retain Scene's distinct live Point mobjects. Add native-shape, exact-False propagation, state and actual Scene-entrypoint witnesses to the permanent bridge acceptance. Correct prior fixtures that had no geometry at their claimed hit position or skipped the required hover event.
    - [ ] Execute the changed bridge through the real extension, registered E2E and fresh installed wheel; rerun scanner and full source gates. Syntax and whitespace checks pass; no behavioral success is claimed yet. The already-running 08:13 wheel and source gate predate these edits.
      - [x] Intermediate source-class probe passes all new spatial/capture/state/Scene-entrypoint witnesses using the current dispatcher definition and the existing installed native geometry (`event-source-native-probe.log`, direct hz3 SSH exit 0). This isolated in-process definition substitution is not fresh-extension or wheel acceptance; those runs are `native-event-color-current.log` and `installed-events-0833.log`.
      - [x] The new installed wheel passes all three acceptance suites, including the actual dispatcher and Scene routes. Wheel SHA-256 `1c6a20549d7f153e6b2984cb86a90204d7809ce9b92099ef54b1c4619293986c`; extension SHA-256 `d84f36ede8fe2ae65a7499995c66974cb3106c4f28fb2e0ebd593e3aaf9dbef8`. The full unchanged audit still rejects 101 claims (`installed-events-0833.log`, RCH hz3 terminal 1; raw `tmp.bBRd4CiAj7`). Native production executes three suites successfully, 19 unrelated filtered, 47.78 seconds; subsequent E2E/Clippy remain pending. Later local edits only clarify help, documentation and diagnostic wording.
      - [x] Registered Python E2E execution completes on the same native snapshot: four pass, five unrelated filtered, 12.84 seconds. `ubs-event-dispatch-current.log` exits 0 after scanning bootstrap and bridge (zero critical, 630 warnings, 663 informational); the later help change only corrects DIRECTORY to PATH. `ubs-final-native-comments.log` exits 0 for the final Rust runtime/diagnostic text (zero critical, 882 warnings, 169 informational). Current Python syntax, shell syntax, refusal inventory, crate DAG and whitespace checks pass. Full committed-source replay remains required.
        - Scanner's `is False` warning is intentionally not applied: Reference propagation stops only on the exact Boolean singleton. The new real-wheel witness proves integer zero and NumPy false continue; replacing identity with equality would introduce a bug. Assert-in-test warnings likewise do not justify removing behavioral assertions.
      - [x] `native-event-color-current.log` finishes with workspace Clippy success and terminal RCH vmi1264463 exit 0 (1313.2 seconds). Its three production suites and four Python E2E scenarios passed first. The next source checkpoint includes only subsequent help/documentation/diagnostic wording and tracker updates beyond that executed implementation; final complete `scripts/check.sh` remains pending on the checkpoint.
  - [ ] `fm-5wq.4.141` is now guarded-claimed by SilentTurtle for verification of the already-landed 16 Reference-named helpers. Confirm constructor and absence contracts in the current native/wheel acceptance and full gates before closure; do not invent snake_case aliases.
- [ ] `fm-5wq.4.144`: wire Python rendering to the native complete input closure, canonical artifact/provenance publication and replay; test successful certification and actual incomplete-input refusals.
- [ ] `fm-5wq.4.145`: connect remaining Python console/output options to Reel; exercise real decoded outputs and encoder/error paths.
  - [x] Recheck ADR-0017, the crate graph, actual Reel sink/configuration APIs, source HEAD, inbox and reservations. Convert the already-executed initializer's blocking edge to related without closing it; guarded claim succeeds as SilentTurtle (`claim-output.json`).
  - [x] Wire existing native GIF/y4m sinks into the existing retained portal render session and parser. Preserve the rational clock, ordered emitter, atomic publication, standard-mode boundary and no-subprocess feature graph. y4m uses the existing RGBA-to-NV12 conversion with limited BT.709 matrix and left chroma siting, plus one reusable scratch buffer.
  - [x] Check the initial sink wiring: `native-output-check.log`, RCH hz3 exit 0, `cargo check -p fmn-python --all-targets`; later acceptance/Gauntlet additions require their own execution.
  - [ ] Execute independent decoded GIF/y4m witnesses for actual frame count, centisecond timing, orientation, intermediate movement, color conversion, byte replay across thread counts, four post-capture publication failures and odd-dimension refusal.
    - [x] Add one cohesive permanent `tests/native_outputs.py` suite shared by native acceptance, the installed-wheel gate and registered Gauntlet scenario; native suite inventory rejects a missing test. This is executable feature coverage, not a second tracking artifact.
    - [ ] Obtain terminal native, registered e2e and fresh installed-wheel receipts; retain failures without weakening assertions.
      - [x] Fresh supported-CPython wheel passes the complete output suite: actual four-frame GIF (657 bytes) and y4m (31170 bytes), with decoded timing/orientation/motion, 1/4-thread byte replay, four preserved-destination failure cases and odd-dimension refusal. Artifacts are under `.rch-results/portal-native-outputs-20260908-0550/fmn-native-output-kaj02pz5/`. The first native failure was my three-frame expectation for binary 0.1; existing BN-02 explicitly requires four. Corrected the test and additionally checked the repeated endpoint frame, without changing the clock.
      - [x] Native and e2e source validation completes in `native-output-source-validation.log`: RCH hz3 exit 0, all three production suites pass, one registered scenario passes, and workspace Clippy passes. This snapshot precedes the countermodels and WAV adapter.
      - [x] Reexecute the subsequent countermodels: the same verifier rejects shared vertical reflection and reversed publication even when GIF and y4m agree. The expanded native output suite, E2E and Clippy pass in `native-wav-source-first.log`; the 06:11 fresh wheel also passes them.
      - [ ] Close the white-motion witness's channel-swap blind spot with decoded primary-color GIF and Y/Cb/Cr swatches, retained by the same E2E scenario. Initial replay `installed-gc-color-0800.log` exits 1 before wheel construction because I supplied RGB tuples to Reference's scalar-color-or-color-list style API. The direct traceback and pinned ManimColor definition confirm invalid test input; corrected to explicit hex colors without changing product code or expected BT.709 values. Replay is `installed-gc-color-0803.log`.
        - [x] The corrected primary-color/output suite passes against the existing timing wheel in the 08:03 replay. The strengthened authored-position checks also pass in the 08:09/08:13 prebuild runs, rejecting a shared RGB permutation as well as wrong Y/Cb/Cr values. Those jobs' subsequent new-wheel lifetime checks are separately governed by the stale-binary repair above.
  - [ ] Connect WAV/sound publication through the existing mixer and sample clock.
    - [x] Adapt the existing `SoundRequest`, exact sample clock, native PCM decoder, default bounded mixer and atomic WAV publisher. Run the complete scene lifecycle without Lumen rasterization or any new subprocess/dependency path.
    - [x] Execute independent PCM decoding with exact 48 kHz cue placement, mono-to-stereo mapping, overlapping dB gain/background attenuation, silent timeline tail, sampled updaters and 1/4-thread byte replay. Ten missing/malformed/oversized/no-cue/scene-exception cases preserve absent or preexisting destinations; native, E2E and 06:11 wheel receipts below pass.
    - [x] Complete native/e2e/Clippy and fresh-wheel execution. `native-wav-source-first.log` ends RCH hz3 exit 0. Fresh wheel SHA-256 `f6a0cd5b0829b336b9b808fc528af26660aea3a7dfc656e728754e8c6a0cecce` passes all three acceptance suites, including both wrong-image countermodels. `installed-wav-0611.log` ends RCH exit 1 because the unchanged global audit still rejects 103 contradictions; raw receipt `.rch-results/portal-wav-20260908-0611/tmp.G4dqKj7sKP`. This snapshot precedes the subsequent composition timing slice.
    - [x] Complete output/timing scanner replay: `ubs-output-timing-current.log` reports seven Rust critical heuristics and zero Python criticals. All seven are exact public format/flag/schema comparisons or a frame-session finish call, incorrectly classified as secret comparison/token randomness. Specific line comments explain these domains; unchanged Rust scanner rerun `ubs-cache-rust-after.log` exits 0 with zero criticals (1177 warnings, 380 informational). Later proxy-lifetime diagnostics require their own check; no broad rule suppression or severity change.
  - [ ] Route optional ffmpeg output through the governed exact-image process boundary; test absent encoder, codec/format negotiation, cancellation, timeouts, size bounds and preserved destinations.
  - [ ] Complete quality/transparency, naming, scene/range selection, skip/final still, write-all, prerun, subdivisions and opener controls through the shared configuration contract. Do not add a second parser or scheduler.
  - [ ] Run complete committed-source and clean-wheel gates before promoting README shipped rows or closing the bead.
    - [x] Update the README and wheel guide's current repository-built output forms only after real native and clean-wheel GIF/y4m/WAV acceptance passes. Keep certification, ffmpeg containers, other console controls, Studio and the native platform wheel matrix explicit as remaining work. Clarify that `--video_dir` names a sequence directory or a single destination file; no release publication is claimed.
- [ ] `fm-ffj.70`: connect host-owned Python workers, edit/reload/embed and checkpoint/replay to Studio; preserve the standalone no-CPython boundary.
- [ ] `fm-ffj.71`: complete the browser timeline, navigable inspector, span/overlay controls and usable restart/error interactions; verify with a real browser.
- [ ] `fm-5wq.6`: reconcile stale RNG, closure-admission, peak-RSS and census assertions against landed code and current tests; preserve assignments, history and still-unmet acceptance.
- [ ] Existing release owners: build a repaired wheel with asymmetric-text orientation acceptance; make documented examples match the released snapshot reader; qualify install/publication/signing prerequisites.
- [ ] Existing performance owners: run real front-door producers on qualified hosts, then use measured bottlenecks to choose CPU work; retain optional CUDA's actual external prerequisites.
- [ ] After each substantive unit: update this TODO and its owning bead with exact evidence, run appropriate checks, and preserve all remaining acceptance until fulfilled.

Current execution receipts live in `/tmp/fmn-gap-execution-20260908/`. The first native run executed two suites and exposed nested uniform interpolation and invalid legacy lifecycle witnesses. The second ran the animation suite successfully and found one additional bridge witness constructing a targetless base Animation; that witness now supplies a real Mobject. Supported CPython 3.13.15 with NumPy 2.5.2 and the exact pinned WASM target have been installed in the selected worker's development environment. The CPython test launcher also needs that interpreter's shared-library directory; the failed launch is retained separately from semantic test results. Source-gate and installed-wheel qualification remain in progress.

Later receipts: `installed-bridge-with-version.log` exits 0 for the initial installed wheel; `subclass-before.log` reproduces the real Scene.play override bypass. `production-callback-hooks.log` executes two native suites: animation acceptance passes, while bridge acceptance exposes missing FocusOn helper state after helper updates are connected. That state initialization is now repaired and awaits the next run. `clippy-4.log` has terminal remote exit 0. The RCH native build compiled successfully but artifact retrieval failed with exit 102; the separately recorded Maturin wheel build and installation supply the actual wheel evidence, not a claim that RCH retrieval succeeded. No implementation bead has been closed on these partial receipts.

### Work-block audit through `a40c4a88` (September 8)

The purpose remains a deterministic native mathematical-animation engine with a source-compatible optional Python portal. The checked window is `d7840481..a40c4a88`, plus the current smoke-lock repair; the test/gate diff was read in full. The ledger has no closures during this window. Another actor committed the shared edits; commit count is not capability credit.

**Creation worksheet.** The TODO is PROCESS: runtime does not branch on it. (1) Its consumers are the requesting user and SilentTurtle. (2) The user's explicit request for a complete granular TODO gates declaring the requested campaign complete. (3) It prevents losing the still-unmet integration work after earlier partial reports. (4) Retire it into completed session history when each item has terminal evidence or an explicit unresolved prerequisite. The integrity exception is not invoked: the user request already supplies the creation gate; a second recovery artifact would be unnecessary. Opportunity cost: native/wheel animation acceptance is the highest-value current capability, with `.141` the planner's next ready verification leaf. An hour of more reporting would deliver less value than either. Verdict: a legitimate, bounded requested checklist, with zero capability credit.

**Real-work worksheet.** Classifying all 18 commits by their primary effect: USER 6 (`142bacc9`, `97839da9`, `d452c382`, `54f1aebd`, `d48e1689`, `b1b015a6`); ENABLER 5 (`41daa8a5`, `274f8b88`, `c1adfaff`, `dd3126d4`, `fc0ae428`); PROCESS 7 (the remaining documentation/tracker commits); UNKNOWN 0. These counts diagnose the window, not a productivity target. (1) The strongest demo is the actual curved-transform/composition PNG scenario, now passing with native group identities visible in Python. (2) Removing the process items would change no rendered frame; their value is preserving the user's requested queue and honest evidence limits. (3) The native bootstrap, qualified CPython environment, corpus transfer and focused tests have all been exercised, so those enablers are concrete. (4) The oldest user-facing open areas include Lumen and the CLI/Studio/portal epics; initialization was selected because it unlocks already-written behavior across the portal. (5) No subagents or swarm panes were delegated, and no closes were produced. (6) No plan/spec change substitutes for code and no original was closed by moving its acceptance into a follow-up. Verdict: capability progress is real, but the documentation share and repeated environment qualification are expensive. Keep further bookkeeping bounded and finish the executable gates.

**Honesty worksheet.** Answers cover this window only unless stated otherwise.

1. No weakening found (checked: the complete test/gate diff), but existing witnesses changed and deserve explicit review: two now call `begin` before progression/finish, a base Animation now receives its required Mobject, and the old curved-path refusal became an analytic midpoint/endpoint/updater-detachment test. These correct invalid or obsolete expectations; no original positive assertion was dropped. The new E2E's initial dt expectation, render-helper name, root expectation and two compile errors were my mistakes, recorded in retained failing logs and bead comments before correction.
2. No (checked: the complete acceptance/conformance diff). No new storage mock, fixture or hard-coded result was introduced; fixture-only installer tests are explicitly lower evidence than native execution.
3. No (checked: the full changed-path inventory). No self-golden or snapshot was regenerated.
4. Yes: the source/wheel gates now execute both actual suites and reject a missing named native test; the new conformance scenario reads real output. The smoke build now requires `--locked`, and wheel audit receipts remain on disk. No assertion tolerance, lint suppression, skip flag or product timeout was relaxed. The scanner's execution budget increased from 300 to 1800 seconds after two incomplete scans of the large Python files; this permits completion, changes no scanner rule, and is not a passing receipt.
5. No (checked: bootstrap, native bridge and conformance changes). Production uses the shared initializer and real Stage handles; there is no demo-only success branch.
6. Yes: the first SVG name filter ran zero integration tests. That zero-run result was not accepted; the explicit integration targets subsequently ran seven tests successfully. Native acceptance now inventories both required names before filtering.
7. No (checked: saved terminal logs against the TODO and bead comments). Queued jobs, source inspection and compilation without artifact retrieval are not reported as executed success.
8. No (checked: the evidence descriptions). The fixture suite, initial wheel, later native tests and installed-wheel parity audit are separately identified.
9. No (checked: current TODO and comments). The 112 runtime contradictions, incomplete full gates, RCH retrieval/admission failures and scanner timeouts remain explicit; no bead is closed.
10. No (checked: commands cited in the receipts). Their stderr is retained with stdout or displayed; data-only redirection is not used to hide failures.
11. No (checked: exported ledger creation/closure timestamps). No incomplete item was closed or moved out to manufacture completion.
12. No (checked: changed-path inventory). The plan, decisions, requirements and golden authorities were not weakened; the stale WASM audit hashes have not been promoted.
13–17. Not applicable: no subagents/panes were dispatched and no peer report was accepted as independent proof. Shared commits were inspected as source changes, not verification.
18. No (checked: claims in this window). The runtime audit denominator is all authored SAME/IMPROVED rows, not a selected subset; focused test counts identify their excluded tests and do not imply full coverage.
19. I would explain the repeated wrong test setup and tool-environment attempts before a replay: they consumed time and exposed my own mistakes. The corrections are visible, and those failures are not reclassified as product bugs.
20. The strongest re-executable evidence is the registered native PNG lifecycle scenario, plus both complete real-extension acceptance suites passing on CPython 3.13.14/NumPy 2.5.2. These are self-executed checks, not independent verification, and do not establish full portal parity.

Older-session check: `cass status` reports missing lexical index metadata and incomplete archive coverage. Two bounded searches (the touched native acceptance name and golden regeneration) each timed out at 120 seconds before search setup completed. They provide no clean-history evidence; rebuilding that unrelated index was not pursued during product work.

Disposition: the changed witnesses and zero-run pitfall are corrected on the record; failures and evidence limits are disclosed. Countermeasures are the native zero-match guard and missing-installer negative (RH-1), analytic intermediate geometry and subclass hooks that reject stock endpoint-only behavior (RH-5), and retaining exact source/ABI/terminal receipts without closing the parent contract (RH-2/RH-9). Full source and final-wheel gates remain required; this worksheet grants no completion credit.

### Work-block audit through `ce158fc7` and the GIF/y4m wiring (September 8)

The purpose remains the README's deterministic mathematical-animation engine and source-compatible optional Python portal. The reviewed window is `a40c4a88..ce158fc7` plus the current GIF/y4m diff. All 572 lines of the test/gate diff, the new output suite, native lock assertions, scanner annotations, and recent reflog were inspected. Native ledger timestamps show no closures after this campaign began.

**Real-work worksheet.** The 15 shared commits classify as USER 3 (`3dcc4c01`, `e1f9bdaa`, `c7172c4a`), ENABLER 4 (`2f478623`, `318d55d1`, `822ebee0`, `8b5aedbb`), PROCESS 8, UNKNOWN 0. Another actor authored those commits; neither their number nor this classification is capability credit. Current uncommitted GIF/y4m code is additional USER work with successful fresh-wheel execution, still awaiting complete gates. (1) The short demo is the actual wheel rendering the same moving square to GIF and y4m, with four decoded frames and correct orientation. Mixed native/Python succession also has real intermediate-state and PNG witnesses. (2) Removing process changes would change no pixel, but would lose the user's explicitly requested queue and precise remaining acceptance. (3) The corrected WASM audit was consumed by governed-closure tests; the browser/package gate executed, and the shared acceptance suites execute in real native and installed-wheel routes. (4) The oldest user-facing open work includes Lumen and CLI/Studio/portal epics; portal integration was selected because it connects already-finished native mechanisms. (5) No swarm was dispatched and no closes were produced. (6) No plan/spec replacement or follow-up closure occurred. Two predecessor blocking edges for `.143`, and one for `.145`, became related after initializer execution succeeded; the predecessors' full global gates remain unmet and open. Verdict: reporting overhead is high by commit count. Keep it bounded; the next work block belongs to positive output/API capability and terminal validation, with no new process artifact.

**Honesty inventory.**

1. No weakening found (checked: full test/gate diff and original Reference/BN-02 contracts). Changed expectations are explicit: Write's `None` sentinels become actual helper results; Scene's old seed-absence assertion becomes the Reference's class default; the composition callback witness gives both children equal intervals so its compressed two-frame window actually contains an intermediate decimal state. New analytic mixed-window witnesses also check unequal-duration behavior. The output test's initial three-frame expectation was wrong under existing BN-02, corrected to four plus repeated-endpoint assertions. My missing `Scene.time()` parentheses were also corrected. Failed logs remain retained.
2. No (checked: production paths and new acceptance source). The small scene is executed through the real console, native Stage, Lumen and Reel; GIF LZW and y4m readers decode emitted bytes. No result mock or success-only production branch was added.
3. No (checked: full changed-path inventory and corpus diff). No golden or structural corpus baseline was regenerated; BeamSplitter's original 24/194 authority remains required.
4. Yes. The source and wheel gates now additionally require the native-output suite, and its e2e registration uses actual reports/artifacts. Five narrowly annotated UBS false positives cover numeric `sign`, the keyboard reset key, Python signature inspection, and the specified trusted-host pickle API/self-roundtrip; scanner rules and severity stay unchanged. One new `too_many_arguments` allowance covers the private typed PyO3 output entry point's format plus existing render arguments; it suppresses a style limit, not validation or safety. No assertion tolerance was widened, and the scanner budget remains the previously disclosed 1800 seconds.
5. No (checked: native session and bootstrap diff). The console selects existing sinks and native conversion; it does not special-case the acceptance scene, replace the scheduler, or invent encoded success.
6. No (checked: terminal named-test counts). The first output test ran one test and failed; the earlier public-hook run passed two, with unrelated filters identified. The native inventory now requires all three named suites.
7. No (checked: retained logs against current comments/TODO). The 05:50 wheel ran all three suites and failed the subsequent whole audit at 103 contradictions. The broad source gate remains unfinished and predates later changes.
8. No (checked: evidence identities). Source inspection, standalone tests, fresh wheel execution, browser execution and the raw runtime audit are separately identified. Self-execution is not independent verification.
9. No (checked: current comments and TODO). The remaining 103 contradictions, earlier native test mistakes, still-unverified corpus repair and missing final committed-source gate remain explicit. No bead is closed.
10. No (checked: commands and retained combined logs). Stderr was retained. `RUST_LOG=error` is used for Beads so its guarded executor stays inside the documented response budget; errors remain visible.
11. No (checked: exported issue history). No incomplete item was closed. A first `.143` executor returned exit 5 after actually claiming because its INFO stderr exceeded the receipt budget; native state was inspected, only that own claim was released, and a fresh token/retry with bounded logging produced a verified receipt. The failed token was not replayed.
12. No (checked: plan, overlay, schema and golden authorities). No requirement was lowered; the WASM audit was corrected only after actual locked target/Node/browser execution.
13–17. Not applicable: no delegation, swarm verdict, independent verifier or agreement claim. Agent Mail lists only this identity; the earlier session-history search limitations remain unchanged.
18. No (checked: both raw audit receipts). The denominator remains all 2036 authored SAME/IMPROVED claims among 2275 rows, with identical overlay/schema hashes. Nine resolved contradictions are measured by set difference.
19. The author would disclose the mistaken frame-count and `Scene.time` witnesses, the receipt-budget mistake, and repeated RCH qualification cost before replay. These consumed time; they are not product wins. Two own superseded source-gate jobs were cancelled after explicit identity checks; their partial logs are not verdicts. RCH's later capability probe returned an empty cached snapshot despite separately executable pinned Rust/Python tools. Supported remote job execution still produced the actual fresh-wheel result; admission failures do not count as source failures.
20. Strongest evidence: freshly installed wheel `c9b20e0f...` executes the full native-output suite and both semantic suites; the unchanged global audit still refuses compatibility completion. The new verifier countermodels and exact committed-source replay remain pending. No self-certification or release claim follows from this audit.

### Work-block audit after `ce158fc7`, before the next source checkpoint

The checked window is the current uncommitted diff, including all test/gate changes, the complete new output reader suite, configuration and documentation; the recent reflog has no rewrite and the exported ledger has no closure since implementation began. The purpose remains native deterministic mathematical animation and a compatible host-Python front door.

**Real work and creation gate.** USER work is native GIF/y4m/WAV publication, shared composition timing, BatchedUpdater collection, spatial event dispatch, and their user-facing instructions. ENABLER work is owner-thread test cleanup, content-based build freshness, restored RCH source inputs, the public SVG Rustdoc link and executable acceptance registration. PROCESS is the existing requested TODO and Beads comments; no new tracking artifact was created. The two-minute demonstration is an installed wheel producing decoded moving/color images and an analytically placed soundtrack, or delivering a drag only to the pressed square. Removing the process work changes no pixel; it loses the user's requested work queue. The enablers were consumed by actual wheel builds, native suites and the complete source script. Old open portal/Studio/render work remains; these seams were selected because they connect existing native mechanisms. No agents were delegated and no original acceptance was moved into a replacement close. Verdict: capability progress is concrete, while build/cache diagnosis has been costly. Keep the next block on implemented behavior and exact terminal verification; this audit earns no capability credit.

**Honesty inventory.**

1. No weakening found (checked: the full current test/gate diff against the pinned Reference). The old composition `None` assertion becomes actual 1/1.05/2 durations; spatial fixtures now supply hit geometry and the preceding hover event. The mistaken RGB-tuple test input was corrected to scalar hex colors, retaining independent expected channel values. No golden or tolerance changed.
2. Yes: the existing non-FFI installer fixture needed four explicit native interval responses after production stopped duplicating Choreo's recurrence. It rejects unregistered inputs and verifies delegation; its pass is not native timing proof. Real native and installed suites separately execute unequal/nested intervals.
3. No (checked: changed files and corpus authorities). No self-golden, scene baseline or snapshot was regenerated.
4. Yes: acceptance gates and E2E registration changed with the feature. Required test inventories grew; no suite was dropped. Owner teardown now performs two GC passes, backed by failed one-pass controls, while weak-proxy/module, unraisable and thread-confinement checks remain. Exact UBS annotations explain public format/flag/schema comparisons, publication finalization and checked-in AST execution. The 1800-second scanner budget changes no product timeout or rule. Cargo content fingerprints repair stale reuse; they do not suppress compilation checks.
5. No (checked: native sessions and dispatcher). No fixture-name branch, fake encoder, alternative clock, weakened subprocess boundary or fabricated artifact was added.
6. No (checked: terminal executed counts for this window). Three native production tests and all three installed scripts execute. Earlier zero-filter SVG evidence remains explicitly excluded in the prior inventory.
7. A failed wheel initially appeared to contradict the native lifetime result. Byte comparison proved it contained the old extension; that diagnosis was corrected publicly and in the TODO. The stale wheel was never accepted as successful verification of the fix.
8. No (checked: current evidence descriptions). The AST dispatcher/native-geometry probe, non-FFI fixture, native binary, installed wheel and source gate are distinguished; all are self-execution, not independent verification.
9. No (checked: retained logs and current claims). The 101 global contradictions, scanner timeouts, failed one-flag freshness attempt, cancelled incomplete jobs and still-pending committed-source gate remain visible.
10. No (checked: cited command logs). Error output is retained. Beads uses bounded error-level diagnostics so the executor can issue a receipt; errors are not discarded.
11. No (checked: native exported issue status and timestamps). No bead was closed in this window, including the completed-but-not-yet-committed source prerequisite work.
12. No (checked: plan, lock, schema, overlay and baseline diffs). No requirement, reviewed status or gate owner was changed to match implementation.
13–17. Not applicable: solo execution; no subagent, pane, agreement vote or peer verification was credited.
18. No (checked: successive raw audit objects). All 2036 SAME/IMPROVED claims among 2275 rows remain the denominator. The timing delta is exactly two resolved contradictions, 103 to 101; fixing dispatch receives no placeholder-count credit.
19. The author would explain the GC diagnosis iterations, invalid initial color fixture and two stale-wheel repackages before replay. They cost time and are not product achievements. Preserved-mtime reuse now has a demonstrated mechanism and an exercised content-fingerprint repair.
20. Strongest evidence: wheel `1c6a2054...` executes the expanded bridge, animation and independent decoded-output suites; the whole earlier source script exits 0 with 2942 bare-test passes. Those are distinct source windows, and neither substitutes for the pending final unchanged committed-source gate or a green global compatibility audit.

Disposition: fixture boundaries, stale-binary attribution and changed expectations are corrected and disclosed. Native positive/negative witnesses and zero-match guards counter RH-1/RH-5; exact extension/source identities and content fingerprints counter RH-2; retaining the full failed audit and original open acceptance counters RH-9. The existing unavailable `cass` index supplies no older-session clean-history claim. No additional reporting artifact or independent-verifier claim is authorized by this inventory.

## How to read this document

FrankenManim is pre-1.0. A capability is implemented only when a concrete source surface and a checkable test or artifact boundary exist. Plan text, a reviewed Parity Ledger row, an inventoried refusal, an old Beads comment, a generated projection, or a queued hosted workflow is not implementation evidence.

Source correctness, compatibility adjudication, tracker state, release packaging, hardware execution, and gate verdicts are distinct evidence lanes. This document keeps those lanes separate.

## Executive state

The repository has a broad native Rust implementation, a separately installed Python compatibility portal, a deterministic agent control plane, and multiple platform/release gates. The highest-risk current gap is semantic convergence and evidence discipline at the boundaries rather than native crate structure.

The W10 portal distinguishes **reviewed parity claims** from **runtime truth**: its wheel audit checks every `same`/`improved` row against the imported namespace and binds the report to the embedded overlay bytes. The latest September 8 fresh-wheel execution passes all three semantic/output acceptance suites, but the complete audit still rejects 101 contradictions among 2036 authored implementation claims. Eleven contradictions were resolved by actual public-hook and native composition-timing implementation. Full committed-source verification and the newer updater-lifetime/color checks remain in progress; no clean-wheel compatibility pass or completed portal handoff is claimed.

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
