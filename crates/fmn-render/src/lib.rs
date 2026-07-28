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
//! | [`engine`] | §10.1's certified CPU engine: the frame, the bands, the bits |
//! | [`fill`] | §10.2's analytic nonzero-winding coverage, on the curves |
//! | [`stroke`] | §10.3's true curve-distance strokes, round caps, arc-length ramps |
//! | [`hint`] | primitive hints: kernel routing, and the invalidation rule |
//! | [`table`] | the IR's tables: segments, interned styles, interned shapes and their instances |
//! | [`bin`] | two-level binning, per-tile command lists, occlusion pruning |
//! | [`cache`] | the retained compositor's tile cache and its key |
//! | [`plan`] | the retained plan: lazy synchronization from Marionette |
//! | [`snapshot`] | the canonical, bit-lockable form of a compiled plan (§16.5) |
//!
//! The retained compositor's pixel half (§10.8) lands with its own bead and
//! consumes this.
#![forbid(unsafe_code)]

pub mod bin;
pub mod cache;
pub mod engine;
pub mod fill;
pub mod hint;
pub mod plan;
pub mod revision;
pub mod snapshot;
pub mod stroke;
pub mod table;

pub use bin::{Binning, BinningError, ScreenMap, Tiling, Viewport};
pub use cache::{CacheStats, OutputTransform, TileCache, TileKey, TileWork};
pub use engine::{
    AA_COMPLEX_2X_CROSSINGS, AA_COMPLEX_4X_CROSSINGS, AA_STROKE_COMPLEX_MIN_WIDTH_BANDS,
    AA_VISUAL_BUDGET_V1_MAX_CHANNEL_ERROR, AA_VISUAL_BUDGET_V1_RMS_CHANNEL_ERROR, AaPolicy,
    AaStats, CoverageClass, EngineIdentity, EngineKind, FrameConfig, FrameJob, FrameJobError,
    RENDERER_VERSION, Tier, frame_digest,
};
pub use fill::{
    FillKernel, FlattenReport, GradientField, MonoPiece, MonoTable, RationalPiece, RowScratch,
};
pub use hint::Hint;
pub use plan::{RenderPlan, SyncStats};
pub use revision::{Axis, Dependency, Revisions};
pub use stroke::{JoinWedge, MITER_LIMIT};
pub use table::{Instance, Segment, Shape, ShapeTable, Style, StyleTable};
