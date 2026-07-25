//! Lumen: the compiled render IR and engines — analytic coverage, strokes,
//! lighting, adaptive AA (§10).
//!
//! ## What is here today
//!
//! §10.8's **retained core** (fm-gw7): between Marionette's authoritative state
//! and the execution engines sits a compiled, backend-neutral render IR,
//! synchronized *lazily* under §8.2's mirror rule, with backend layouts derived
//! from it rather than baked into it.
//!
//! | module | what it owns |
//! |---|---|
//! | [`revision`] | the seven independent revision axes and the staleness rule |
//! | [`hint`] | primitive hints: kernel routing, and the invalidation rule |
//!
//! The engines (§10.1), the analytic fill (§10.2), strokes (§10.3), and the
//! retained compositor (§10.8) land with their own beads and consume this.
#![forbid(unsafe_code)]

pub mod hint;
pub mod revision;

pub use hint::Hint;
pub use revision::{Axis, Dependency, Revisions};
