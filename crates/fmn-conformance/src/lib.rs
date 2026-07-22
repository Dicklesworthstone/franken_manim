//! The Gauntlet: Parity Ledger, correctness oracles, self-goldens, Look Gallery tooling, perf gates (§16).
//!
//! Landed so far: the governed-closure audit (D1, fm-g2c) — see
//! [`closure`] and `tests/governed_closure.rs`, the CI teeth of
//! SUITE.lock + SUITE_ALLOWLIST.tsv. The rest of the Gauntlet lands with
//! its owning workstreams; see COMPREHENSIVE_PLAN §19 for the crate map.
#![forbid(unsafe_code)]

pub mod closure;
