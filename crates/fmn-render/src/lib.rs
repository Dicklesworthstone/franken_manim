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
//! | [`fill`] | §10.2's analytic nonzero-winding coverage, on the curves |
//! | [`hint`] | primitive hints: kernel routing, and the invalidation rule |
//! | [`table`] | the IR's tables: segments, interned styles, interned shapes and their instances |
//! | [`bin`] | two-level binning, per-tile command lists, occlusion pruning |
//! | [`cache`] | the retained compositor's tile cache and its key |
//! | [`plan`] | the retained plan: lazy synchronization from Marionette |
//! | [`snapshot`] | the canonical, bit-lockable form of a compiled plan (§16.5) |
//!
//! The engines (§10.1), strokes (§10.3), and the retained compositor (§10.8)
//! land with their own beads and consume this.
#![forbid(unsafe_code)]

pub mod bin;
pub mod cache;
pub mod fill;
pub mod hint;
pub mod plan;
pub mod revision;
pub mod snapshot;
pub mod table;

pub use bin::{Binning, ScreenMap, Tiling, Viewport};
pub use cache::{CacheStats, OutputTransform, TileCache, TileKey, TileWork};
pub use fill::{
    FillKernel, FlattenReport, GradientField, MonoPiece, MonoTable, RationalPiece, RowScratch,
};
pub use hint::Hint;
pub use plan::{RenderPlan, SyncStats};
pub use revision::{Axis, Dependency, Revisions};
pub use table::{Instance, Segment, Shape, ShapeTable, Style, StyleTable};
