//! Chisel: the geometry kernel — quadratic-Bézier paths, true arc length, booleans, SVG processing (§7).
//!
//! This crate owns the shared-anchor `QuadPath` model (§7.1) — the shared
//! vocabulary of Chisel, Marionette's point data, Scribe's glyph outlines,
//! and Lumen's compiled paths — plus the Bézier/smoothing layers under it.
//! Object-space geometry computes in f64 (§6.1). Semantics are ported from
//! the pinned Reference (`3b1b/manim` @ `6199a00d4c1b1127ebe45cb629c3f22538b10e13`)
//! and locked by the fixtures in `fixtures/` and `tests/`.
//!
//! Still to land in this crate: the error-bounded cubic→quadratic converter
//! (fm-6cf), true arc length + inverse-arclength LUTs (fm-xci), path
//! booleans (fm-8dx), the SVG document processor (fm-6nm), isolines +
//! ear-clip (fm-81u), and the full space_ops surface (fm-ngx).
#![forbid(unsafe_code)]

pub mod arclength;
pub mod bezier;
pub mod cubic;
pub mod quadpath;
pub mod smoothing;

mod scalar;
mod vec;

pub use arclength::{ArcLengthTable, CachedArcLength};
pub use quadpath::{AnchorMode, DEFAULT_TOLERANCE_FOR_POINT_EQUALITY, QuadPath};

/// Errors from the geometry kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeomError {
    /// A point run that would leave the shared-anchor layout with a nonzero
    /// even length (the invariant requires 0 or odd).
    EvenPointCount {
        /// The offending length.
        len: usize,
    },
    /// An operation that needs an existing path end was called on an empty
    /// path.
    EmptyPath,
    /// `set_anchors_and_handles` requires exactly one more anchor than
    /// handles.
    MismatchedAnchorsAndHandles {
        /// Number of anchors supplied.
        anchors: usize,
        /// Number of handles supplied.
        handles: usize,
    },
    /// A smoothing solve hit a singular system.
    SingularSystem,
}

impl std::fmt::Display for GeomError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EvenPointCount { len } => write!(
                f,
                "shared-anchor point runs must have odd length when nonempty (got {len})"
            ),
            Self::EmptyPath => write!(f, "operation requires a path with at least one point"),
            Self::MismatchedAnchorsAndHandles { anchors, handles } => write!(
                f,
                "need exactly one more anchor than handles (got {anchors} anchors, {handles} handles)"
            ),
            Self::SingularSystem => write!(f, "smoothing solve hit a singular linear system"),
        }
    }
}

impl std::error::Error for GeomError {}
