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
//! - the canonical PG-2 producer (fm-inr.2.1) — [`perf_pg2`]: fixed fill and
//!   stroke fixtures, exact benchmark/configuration digests, bounded raw
//!   samples, and a content-addressed stage trace over the real Lumen path.
//! - the canonical PG-7 producer (fm-inr.2.2) — [`perf_pg7`]: cold formula,
//!   cache-proven formula-hit, and exact 10,000-glyph native-text fixtures
//!   with result self-goldens and bounded integer latency samples.
//!
//! The rest of the Gauntlet lands with its owning workstreams; see
//! COMPREHENSIVE_PLAN §19 for the crate map.
#![forbid(unsafe_code)]

pub mod closure;
pub mod golden;
pub mod npy;
pub mod perf;
pub mod perf_pg2;
pub mod perf_pg7;
pub mod ratchet;
pub mod schema;
pub mod tolerance;
