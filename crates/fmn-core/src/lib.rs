//! Types, units, constants, color, the RNG stream, and contract knobs — the shared language of every manim scene (§6).
//!
//! This crate is the semantic bedrock: the §6.2 constants kept exactly as the
//! Reference defines them, the §6.3 color pipeline (linear-light compositing
//! with manim's gradient aesthetic preserved, Behavior Note BN-04), the §6.8
//! rate-function catalog, and the §6.1 numeric-doctrine hooks. Everything here
//! is locked by parity fixtures generated from the pinned Reference
//! (`3b1b/manim @ 6199a00d4c1b1127ebe45cb629c3f22538b10e13`) by
//! `scripts/gen_reference_fixtures.py`.
#![forbid(unsafe_code)]

pub mod color;
pub mod constants;
pub mod rate;
pub mod rng;
pub mod types;
