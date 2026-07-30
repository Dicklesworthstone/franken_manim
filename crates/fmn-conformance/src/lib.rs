//! The Gauntlet: Parity Ledger, correctness oracles, self-goldens, Look Gallery tooling, perf gates (§16).
//!
//! Landed so far:
//! - the governed-closure audit (D1, fm-g2c) — see [`closure`] and
//!   `tests/governed_closure.rs`, the CI teeth of SUITE.lock +
//!   SUITE_ALLOWLIST.tsv;
//! - the Gauntlet bootstrap (fm-xb3): the self-golden rig ([`golden`],
//!   D-16 — bit-locked artifacts, per-platform lock files, the
//!   `UPDATE_GOLDENS=1` bless flow, `.actual` drift sidecars), the tolerance
//!   doctrine as reusable checks ([`tolerance`], §16.4), and the `.npy`
//!   fixture-interchange subset ([`npy`], §16.3) the Reference-fixture
//!   scripts emit.
//!
//! - the public coverage ratchet (§11.5, fm-mol) — [`ratchet`] and
//!   `tests/coverage_ratchet.rs`: the four public numbers against G0-4's
//!   frozen denominator, monotone by CI, pin-coupled so a SUITE.lock bump
//!   of franken_markdown without a ratchet re-run fails.
//!
//! - the one API schema (§16.2, fm-vn6) — [`schema`] and
//!   `tests/api_schema.rs`: the extracted Reference surface
//!   (`API_SCHEMA.tsv`) merged with the authored rulings
//!   (`API_OVERLAY.tsv`), and the generators that turn the merge into
//!   `fmn-config`'s typed extraction, the Parity Ledger rows, the CLI flag
//!   table, and the docs. Drift between a generated artifact and the schema
//!   fails the build.
//!
//! - the performance-gate decision core (§17.2, fm-inr) — [`perf`]:
//!   comparable-run identities, raw valid/invalid repetitions, robust
//!   median/MAD summaries, versioned observed baselines, and explicit
//!   pass/alert/block/inconclusive verdicts. It never turns an unpinned or
//!   unobserved run into passing PG evidence.
//! - the correctness oracles (§16.3 plane 1, fm-t1v) — [`oracles`] and
//!   `tests/oracles.rs`: analytic ground truths (arc length vs closed
//!   forms, path-boolean identities, winding invariants, color-model
//!   round-trips, TeX Appendix-G placement parameters), the three
//!   metamorphic laws restricted to their valid domains, and the
//!   structural-fixture corpus against the pinned Reference.
//! - the canonical PG-2 producer (fm-inr.2.1) — [`perf_pg2`]: fixed fill and
//!   stroke fixtures, exact benchmark/configuration digests, bounded raw
//!   samples, and a content-addressed stage trace over the real Lumen path.
//! - the canonical PG-7 producer (fm-inr.2.2) — [`perf_pg7`]: cold formula,
//!   cache-proven formula-hit, and exact 10,000-glyph native-text fixtures
//!   with result self-goldens and bounded integer latency samples.
//!
//! - self-goldens at scale (§16.3 plane 2, fm-t1v) — [`scene_goldens`] and
//!   `tests/scene_goldens.rs`: the ~25-scene primitive-and-feature corpus
//!   over the landed class families, bit-locked at the post-construct and
//!   post-transform lifecycle points through the D-16 rig.
//! - the engine-equivalence suite (§16.3 plane 4, fm-t1v) — [`equivalence`]
//!   and `tests/engine_equivalence.rs`: certified CPU vs fast CPU under the
//!   versioned v1 visual budget (max/RMS channel error + luma SSIM), the
//!   merge blocker for engine arithmetic changes.
//!
//! - the Look Gallery (§16.3 plane 3, fm-t1v) — [`gallery`] and
//!   `tests/look_gallery.rs`: global luma SSIM, symmetric chamfer edge
//!   distance, and local-error percentiles as smoke alarms (verdict inputs,
//!   never gates), plus the versioned `fixtures/look_gallery.tsv` manifest
//!   and the render/record/regressions review workflow.
//!
//! - the deterministic in-crate fuzzing campaign (§16.5 plane 4, fm-t1v) —
//!   [`fuzz`] and `tests/fuzz_campaign.rs`: the seeded xorshift driver,
//!   structure-aware mutators, per-target resource-budget assertions, the
//!   persisted interesting-input corpus, and the versioned
//!   `fixtures/fuzz_corpus/MANIFEST.tsv` campaign record.
//!
//! The rest of the Gauntlet lands with its owning workstreams; see
//! COMPREHENSIVE_PLAN §19 for the crate map.
#![forbid(unsafe_code)]

pub mod closure;
pub mod equivalence;
pub mod fuzz;
pub mod gallery;
pub mod golden;
pub mod npy;
pub mod oracles;
pub mod perf;
pub mod perf_pg2;
pub mod perf_pg7;
pub mod ratchet;
pub mod scene_goldens;
pub mod schema;
pub mod tolerance;
