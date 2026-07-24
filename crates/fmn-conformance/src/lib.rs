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
//! The rest of the Gauntlet lands with its owning workstreams; see
//! COMPREHENSIVE_PLAN §19 for the crate map.
#![forbid(unsafe_code)]

pub mod closure;
pub mod golden;
pub mod npy;
pub mod ratchet;
pub mod tolerance;
