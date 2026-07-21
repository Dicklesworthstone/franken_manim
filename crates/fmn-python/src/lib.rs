//! The PyO3 `manimlib` bridge — the one crate in the workspace permitted to expand `unsafe` (D3, §15.2).
//!
//! Skeleton crate stood up by W1 (fm-bsz). Subsystem contracts land with
//! their owning workstreams; see COMPREHENSIVE_PLAN §19 for the crate map.
// D3: the sole crate allowed to expand `unsafe` (PyO3 macro output only).
// Project-authored `unsafe` remains forbidden by review even here.
#![deny(unsafe_code)]
