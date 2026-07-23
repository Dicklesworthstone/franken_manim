# UPSTREAM_LEDGER.md — the upstream contributions ledger (§2.9)

Primitives that belong in a foundation crate **land there**, never in
FrankenManim — this ledger tracks each one from proposal to pinned
consumption. The ritual per entry is `docs/GOVERNANCE.md` §6: propose the
row → land in the foundation repo → bump the `SUITE.lock` pin (and the
`SUITE_ALLOWLIST.tsv` rows it affects) in a commit that does nothing
else → run the full Gauntlet and diff self-goldens + the Look Gallery →
adjudicate in the pin-bump commit message and land only green (R8, R17).

Status vocabulary: **proposed** (row exists, upstream work not started) ·
**spiked** (feasibility proven, e.g. a G0 spike) · **in-flight** (upstream
work underway) · **landed-upstream** (merged in the foundation repo, pin
not yet bumped) · **pinned** (consumed via SUITE.lock; done) ·
**tiered-out** (deliberately deferred, with its revisit trigger).

| # | Primitive | Target repo · crate | Owner | Status | Coordination — the step it waits on |
|---|---|---|---|---|---|
| 1 | fmd-font factoring + glyf outline decoder — factor the Font module out of fmd into a standalone crate; sfnt parsing, glyf decoding, the bundled OFL faces | franken_markdown · `fmd-font` | Jeffrey Emanuel; W6 sessions (fm-ydw) | in-flight | fm-ydw is ready work; API freezes at its G0-spike shape until G2 (R8); SUITE.lock pin follows the first upstream land |
| 2 | fmd-math — the clean-room TeX-mathematics layout engine (the largest upstream contribution in suite history): atom classes, spacing table, Appendix-G placement, extensible delimiters, span provenance | franken_markdown · `fmd-math` | Jeffrey Emanuel; W6 sessions | proposed | G0-3 architecture spike (fm-3g6 children) proves the engine shape before the crate API freezes; tier-1 scope set by the G0-4 corpus harvest (fm-or4) |
| 3 | fmd CFF/CFF2 outline support — beyond glyf; needed for non-bundled user fonts | franken_markdown · `fmd-font` | Jeffrey Emanuel | tiered-out | revisit trigger: user-font demand post-G2 (plan "Limitations": typography's honest fringe) |
| 4 | fnx layout kernels — deterministic graph-layout primitives for the enhanced Graph mobjects | franken_networkx · `fnx-*` | Jeffrey Emanuel; W7 sessions | proposed | audit first (OQ-5, owned by W7): determinism + quality of existing kernels decides upstream work vs adapter |
| 5 | fsci Rotation-conventions exposure — the scipy `Rotation` convention surface as a stable public API | frankenscipy · `fsci-*` | Jeffrey Emanuel; W2 sessions | proposed | fm-ngx lands the semantics locally in fmn-geom first; upstream exposure proposed once the convention fixtures are green at singularities |
| 6 | fnp structured-record lerp fast path — vectorized interpolation over structured records, serving the §8.2 field-lerp hot loop | franken_numpy · `fnp-*` | Jeffrey Emanuel; W3/W5 sessions | proposed | justified by W5 profiling under §17.1 instrumentation (fm-bgr baseline) before any upstream work — eliminate work first, per doctrine rule 8 |
| 7 | ft CUDA device path — the Accelerator Annex's second backend, via frankentorch only (D-22) | frankentorch · `ft-*` | Jeffrey Emanuel; annex sessions | proposed | spiked on this ledger before any production claim (OQ-10, opened by G0-8/fm-ekx, which needs Apple hardware for its Metal half); standard-only, PG-A-gated regardless of outcome |
