//! §10.1's **certified CPU engine** — the arithmetic whose output *is* the
//! definition of the bits.
//!
//! > … the **certified CPU engine** (canonical arithmetic; the definition of the
//! > bits), the **fast CPU engine** … and the standard-only **annex engines** …
//! > which share the IR and the test suite, never a lowest-common-denominator
//! > kernel. … Fast-CPU and annex engines are held to an explicit, versioned
//! > visual-equivalence budget against certified reference frames; **the
//! > certified engine is held to bits.**
//!
//! Everything above this module computes coverage and colour; this module is
//! where a *frame* exists, and therefore where §10.5's parallelism contract
//! stops being a doctrine and becomes a data structure.
//!
//! ## The four properties, and where each one lives
//!
//! ADR-0010 retired §10.5's fixed-point raster boundary on measured evidence and
//! made four properties binding in its place. They are not implementation
//! details — an engine that broke one would emit a provenance manifest claiming
//! a reproducibility it no longer has — so each one has an address:
//!
//! | Property | Where it is enforced |
//! |---|---|
//! | fmn-dmath owns every certified transcendental | `fmn-geom`'s `scalar` funnel, `fmn-frame`'s `transfer`, and the repo-wide guard `fmn-conformance/tests/certified_arithmetic.rs` |
//! | No FMA contraction (§10.5d) | the same guard, over every certified crate root |
//! | Fixed-order reductions (§10.5c) | this module: [`FrameJob`]'s draw order, and ADR-0013 |
//! | IEEE-754 basic operations only | certified accumulation is `f64`; `sqrt` is the only non-arithmetic primitive, and IEEE-754 requires it correctly rounded |
//!
//! ## Why a band is the unit of parallelism
//!
//! §10.5(b) asks for tiles that are *write-disjoint* and composite in a *fixed
//! order*. Those are two different requirements and it is worth separating them,
//! because only one of them is about threads.
//!
//! Compositing order is **painter order within a tile**, and it is fixed by the
//! command list [`crate::bin::Binning`] built — a property of the scene, not of
//! the schedule. Across tiles there is no compositing at all: a pixel belongs to
//! exactly one tile, so two tiles cannot disagree about it.
//!
//! That leaves write-disjointness, which is a *typing* problem: the frame is one
//! allocation and several threads must write it. Rather than hand out overlapping
//! views and argue about safety, the engine slices the frame into **bands** — one
//! fine-tile row of pixels each — with [`slice::chunks_mut`], which yields
//! provably disjoint `&mut [u8]`. Disjointness is then something the borrow
//! checker knows rather than something a comment claims, and `#![forbid(unsafe_code)]`
//! survives a multi-threaded rasterizer without a single escape hatch.
//!
//! Bands are handed out from a shared queue, so the *assignment* is dynamic (a
//! band full of glyphs does not stall a thread that drew empty sky) while the
//! *bytes* are not: band `b`'s bytes are a pure function of `b`, the plan, and
//! the config. That is the whole of thread-count independence, and
//! `the_frame_is_identical_at_every_thread_count` is what proves it.
//!
//! ## The raw frame is linear-light `Rgba16F`
//!
//! §16.7's certified artifact kinds are *raw frames*, *canonical PNG* and *WAV*.
//! The raw frame is the linear-light master: [`fmn_frame::PixelFormat::Rgba16F`],
//! straight alpha, output orientation (D-23 — row 0 is the top row, and there is
//! no vflip anywhere in the system). The canonical PNG is one table lookup away
//! through [`fmn_frame::convert::rgba16f_to_rgba8`], which is bit-exact by
//! construction because it does no arithmetic at all.
//!
//! Certified compositing runs in `f64` in a row accumulator and is narrowed
//! **once**, at writeback. The `f64 → f32 → f16` narrowing is a double rounding,
//! and it is recorded rather than hidden: it is deterministic on every platform
//! (both steps are IEEE round-to-nearest-even), and reaching a different `f16`
//! than a direct `f64 → f16` would require the accumulated value to sit within
//! `2⁻²⁴` of an `f16` midpoint — below the width at which the 8-bit output can
//! express a difference. The standard-only fast engine instead accumulates
//! colour and alpha in `f32`; its error is held to the versioned visual budget
//! below, while coverage and the ill-conditioned stroke-distance solve stay
//! `f64`.

use crate::bin::{Binning, CLASS_INTERIOR, ScreenMap, Tiling, Viewport};
use crate::fill::{
    self, FillKernel, GradientField, RowScratch, fill_is_flat, fill_rgba_at, fill_rgba_with_border,
};
use crate::plan::RenderPlan;
#[cfg(test)]
use crate::stroke::stroke_shade;
use crate::stroke::{self, JoinWedge, half_width_px, stroke_rgba_at};
use crate::table::{Segment, Style, reparameterize_arc_length};
pub use fmn_core::AaPolicy;
use fmn_core::color::{LinearRgba, PremulRgba};
use fmn_frame::{FrameBuffer, FrameError, FrameLayout, PixelFormat};
use fmn_hash::{Digest, Schema, Writer};
use std::sync::{Mutex, PoisonError};

// A binary that requires an improvised subset must not journal itself as the
// portable tier. ADR-0016 supports only the exact SUITE.lock feature sets.
#[cfg(all(
    target_arch = "x86_64",
    any(
        target_feature = "avx2",
        target_feature = "bmi2",
        target_feature = "fma",
        target_feature = "avx512f",
        target_feature = "avx512bw",
        target_feature = "avx512dq",
        target_feature = "avx512vl"
    ),
    not(any(
        all(
            target_feature = "avx2",
            target_feature = "bmi2",
            target_feature = "fma",
            not(target_feature = "avx512f")
        ),
        all(
            target_feature = "avx512f",
            target_feature = "avx512bw",
            target_feature = "avx512dq",
            target_feature = "avx512vl"
        )
    ))
))]
compile_error!(
    "unsupported partial x86 SIMD tier; use SUITE.lock's portable, x86-64-v3, or x86-64-v4 flags"
);

// --------------------------------------------------------------- the identity

/// The schema family for engine-identity and frame documents.
pub const ENGINE_SCHEMA: Schema = Schema::new(*b"FMNE", 1, 2, 0);

/// The schema family for a canonical raw-frame document.
pub const FRAME_SCHEMA: Schema = Schema::new(*b"FMNE", 2, 1, 0);

/// The **semantic renderer version**: C7's first component.
///
/// Bumped when the renderer's *meaning* changes — a different coverage
/// definition, a different composite, a different colour pipeline — so that two
/// provenance manifests cannot claim equal inputs across a change that moves
/// every pixel. It is deliberately not the crate version: a refactor that cannot
/// move a bit must not invalidate a manifest, and a one-line change to the AA
/// profile must.
pub const RENDERER_VERSION: u32 = 4;

/// Boundary-sheet count at which an adaptive cell escalates to 2×2 samples.
///
/// G0-2 fixed the criterion — more than one boundary in a cell — and assigned
/// the numeric threshold to fm-gmr. Two is the first count for which the
/// ordinary one-edge analytic model is no longer sufficient.
pub const AA_COMPLEX_2X_CROSSINGS: u8 = 2;

/// Boundary-sheet count at which an adaptive cell escalates from 2×2 to 4×4.
///
/// Four independent crossings identify the dense/cusped end of the measured
/// corpus. The count is deliberately an integer geometric fact, not a
/// machine- or frame-load-dependent heuristic.
pub const AA_COMPLEX_4X_CROSSINGS: u8 = 4;

/// Minimum full stroke width, in AA-band widths, that contributes complexity.
///
/// G0-2 measured cap/join distinctions below roughly two AA bands as wholly
/// contained by the analytic smoothstep. Supersampling those strokes buys no
/// new silhouette information, so they cannot promote an otherwise-simple
/// cell merely by overlapping.
pub const AA_STROKE_COMPLEX_MIN_WIDTH_BANDS: f64 = 2.0;

/// Version-1 maximum linear channel error versus forced 4× on the W5 corpus.
///
/// The measured adaptive result is `0.219482421875`; the blocking budget leaves
/// roughly five percent headroom for corpus growth while still catching a
/// changed edge profile or a missed complex-cell class.
pub const AA_VISUAL_BUDGET_V1_MAX_CHANNEL_ERROR: f64 = 0.23;

/// Version-1 RMS linear channel error versus forced 4× on the W5 corpus.
///
/// The measured adaptive result is `0.008526512734628627`.
pub const AA_VISUAL_BUDGET_V1_RMS_CHANNEL_ERROR: f64 = 0.009;

/// Version-1 maximum linear channel error for fast CPU versus certified.
///
/// The locked three-frame corpus measures `0.087890625` at one pentagram edge;
/// the blocking budget leaves roughly five percent headroom. A controlled
/// canonical-AA run measures mixed precision alone at `0.00048828125`, so the
/// larger residual belongs to standard mode's adaptive edge sampling rather
/// than an f32 distance solve.
pub const FAST_VISUAL_BUDGET_V1_MAX_CHANNEL_ERROR: f64 = 0.092;

/// Version-1 RMS linear channel error for fast CPU versus certified.
///
/// The locked corpus measures `0.0004011347492380775`; the blocking budget
/// leaves roughly five percent headroom.
pub const FAST_VISUAL_BUDGET_V1_RMS_CHANNEL_ERROR: f64 = 0.000_42;

/// Version-1 minimum global sRGB-luma SSIM for fast CPU versus certified.
///
/// The locked corpus minimum is `0.9999983928862303`. SSIM is the perceptual
/// half of §16.3's engine-equivalence budget; max and RMS channel error remain
/// the local-error tripwires.
pub const FAST_VISUAL_BUDGET_V1_MIN_SSIM: f64 = 0.999_99;

/// Which execution engine produced a frame (§10.1, C7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EngineKind {
    /// The certified CPU engine: canonical arithmetic, the definition of the bits.
    CertifiedCpu,
    /// The fast CPU engine: mixed precision, SIMD tiers, FMA permitted (§17.3).
    FastCpu,
    /// The Metal annex engine (§10.7) — `standard` only, never certified.
    Metal,
    /// The CUDA annex engine (§10.7) — `standard` only, never certified.
    Cuda,
}

impl EngineKind {
    /// The stable name journaled into the input closure.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::CertifiedCpu => "certified-cpu",
            Self::FastCpu => "fast-cpu",
            Self::Metal => "metal",
            Self::Cuda => "cuda",
        }
    }

    /// May this engine serve a `certified` run?
    ///
    /// §16.7's C7 says it plainly — "`certified` requires the certified CPU
    /// engine" — and D-18 makes GPU work in the certified path one of the three
    /// permanent refusals. The predicate exists so that rule is enforced by a
    /// match rather than by everyone remembering it.
    #[must_use]
    pub const fn certifiable(self) -> bool {
        matches!(self, Self::CertifiedCpu)
    }
}

/// The scalar definition and the one SIMD build tier this artifact offers
/// (§17.3, C3).
///
/// A distributed artifact contains exactly two arithmetic routes: [`Tier::Scalar`]
/// for the certified equivalence oracle, and the tier selected by the
/// SUITE.lock-governed crate flags. There is no per-call feature detection and
/// no artifact can claim a tier it was not compiled to execute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tier {
    /// Portable scalar: the definition every other tier must reproduce.
    Scalar,
    /// The baseline artifact with no extra target-feature flags.
    #[cfg(not(any(
        all(
            target_arch = "x86_64",
            target_feature = "avx2",
            target_feature = "bmi2",
            target_feature = "fma",
            not(target_feature = "avx512f")
        ),
        all(
            target_arch = "x86_64",
            target_feature = "avx512f",
            target_feature = "avx512bw",
            target_feature = "avx512dq",
            target_feature = "avx512vl"
        ),
        all(target_arch = "aarch64", target_feature = "neon")
    )))]
    Portable,
    /// The x86-64-v3 artifact (`+avx2,+bmi2,+fma`).
    #[cfg(all(
        target_arch = "x86_64",
        target_feature = "avx2",
        target_feature = "bmi2",
        target_feature = "fma",
        not(target_feature = "avx512f")
    ))]
    X86_64V3,
    /// The x86-64-v4 artifact (the four SUITE.lock AVX-512 features).
    #[cfg(all(
        target_arch = "x86_64",
        target_feature = "avx512f",
        target_feature = "avx512bw",
        target_feature = "avx512dq",
        target_feature = "avx512vl"
    ))]
    X86_64V4,
    /// The aarch64 + NEON artifact.
    #[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
    Aarch64Neon,
}

impl Tier {
    /// The stable name journaled into the input closure.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Scalar => "scalar",
            #[cfg(not(any(
                all(
                    target_arch = "x86_64",
                    target_feature = "avx2",
                    target_feature = "bmi2",
                    target_feature = "fma",
                    not(target_feature = "avx512f")
                ),
                all(
                    target_arch = "x86_64",
                    target_feature = "avx512f",
                    target_feature = "avx512bw",
                    target_feature = "avx512dq",
                    target_feature = "avx512vl"
                ),
                all(target_arch = "aarch64", target_feature = "neon")
            )))]
            Self::Portable => "portable",
            #[cfg(all(
                target_arch = "x86_64",
                target_feature = "avx2",
                target_feature = "bmi2",
                target_feature = "fma",
                not(target_feature = "avx512f")
            ))]
            Self::X86_64V3 => "x86-64-v3",
            #[cfg(all(
                target_arch = "x86_64",
                target_feature = "avx512f",
                target_feature = "avx512bw",
                target_feature = "avx512dq",
                target_feature = "avx512vl"
            ))]
            Self::X86_64V4 => "x86-64-v4",
            #[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
            Self::Aarch64Neon => "aarch64-neon",
        }
    }

    /// Every tier this build offers: the scalar oracle, then the built tier.
    ///
    /// The certified harness sweeps this, so a tier that lands without being
    /// listed here is a tier nothing checks.
    pub const ALL: &'static [Tier] = &[
        Tier::Scalar,
        #[cfg(not(any(
            all(
                target_arch = "x86_64",
                target_feature = "avx2",
                target_feature = "bmi2",
                target_feature = "fma",
                not(target_feature = "avx512f")
            ),
            all(
                target_arch = "x86_64",
                target_feature = "avx512f",
                target_feature = "avx512bw",
                target_feature = "avx512dq",
                target_feature = "avx512vl"
            ),
            all(target_arch = "aarch64", target_feature = "neon")
        )))]
        Tier::Portable,
        #[cfg(all(
            target_arch = "x86_64",
            target_feature = "avx2",
            target_feature = "bmi2",
            target_feature = "fma",
            not(target_feature = "avx512f")
        ))]
        Tier::X86_64V3,
        #[cfg(all(
            target_arch = "x86_64",
            target_feature = "avx512f",
            target_feature = "avx512bw",
            target_feature = "avx512dq",
            target_feature = "avx512vl"
        ))]
        Tier::X86_64V4,
        #[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
        Tier::Aarch64Neon,
    ];

    /// The SIMD tier selected by this artifact's crate-wide build flags.
    #[cfg(not(any(
        all(
            target_arch = "x86_64",
            target_feature = "avx2",
            target_feature = "bmi2",
            target_feature = "fma",
            not(target_feature = "avx512f")
        ),
        all(
            target_arch = "x86_64",
            target_feature = "avx512f",
            target_feature = "avx512bw",
            target_feature = "avx512dq",
            target_feature = "avx512vl"
        ),
        all(target_arch = "aarch64", target_feature = "neon")
    )))]
    pub const COMPILED: Tier = Tier::Portable;

    /// The SIMD tier selected by this x86-64-v3 artifact.
    #[cfg(all(
        target_arch = "x86_64",
        target_feature = "avx2",
        target_feature = "bmi2",
        target_feature = "fma",
        not(target_feature = "avx512f")
    ))]
    pub const COMPILED: Tier = Tier::X86_64V3;

    /// The SIMD tier selected by this x86-64-v4 artifact.
    #[cfg(all(
        target_arch = "x86_64",
        target_feature = "avx512f",
        target_feature = "avx512bw",
        target_feature = "avx512dq",
        target_feature = "avx512vl"
    ))]
    pub const COMPILED: Tier = Tier::X86_64V4;

    /// The SIMD tier selected by this aarch64 + NEON artifact.
    #[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
    pub const COMPILED: Tier = Tier::Aarch64Neon;
}

/// C7's "execution-engine and backend identities", as a hashable document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EngineIdentity {
    /// Which engine.
    pub engine: EngineKind,
    /// Which SIMD build tier.
    pub tier: Tier,
    /// [`RENDERER_VERSION`] at the time of the render.
    pub renderer_version: u32,
}

impl EngineIdentity {
    /// This build's certified identity.
    #[must_use]
    pub const fn certified() -> EngineIdentity {
        EngineIdentity {
            engine: EngineKind::CertifiedCpu,
            tier: Tier::Scalar,
            renderer_version: RENDERER_VERSION,
        }
    }

    /// This artifact's standard-mode default: fast CPU on its compiled tier.
    #[must_use]
    pub const fn fast() -> EngineIdentity {
        EngineIdentity {
            engine: EngineKind::FastCpu,
            tier: Tier::COMPILED,
            renderer_version: RENDERER_VERSION,
        }
    }
}

impl Default for EngineIdentity {
    fn default() -> Self {
        Self::fast()
    }
}

/// §10.4's aggregate class for one fine tile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoverageClass {
    /// No visible command reaches the tile.
    Empty,
    /// Every visible command is a classified interior fill.
    FullyCovered,
    /// An edge reaches the tile, but every cell contains at most one boundary.
    SimpleEdge,
    /// At least one cell crosses the adaptive 2× threshold.
    ComplexEdge,
}

/// Deterministic instrumentation for one adaptive-AA render.
///
/// Counts are accumulated per write-disjoint band and merged after workers
/// finish, so the report is as thread-count-independent as the frame itself.
/// `native_cells` includes the native classifier pass for adaptive complex
/// cells; `ssaa*_cells` records the fused replacement work on top of it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AaStats {
    /// Native output cells written.
    pub output_cells: u64,
    /// Native-resolution cell passes, including background/interior fast paths.
    pub native_cells: u64,
    /// Cells immediately resolved from 2×2 subcells.
    pub ssaa2x_cells: u64,
    /// Cells immediately resolved from 4×4 subcells.
    pub ssaa4x_cells: u64,
    /// Fine tiles with no visible command.
    pub empty_tiles: u64,
    /// Fine tiles containing only classified interior fills.
    pub fully_covered_tiles: u64,
    /// Fine tiles whose edge cells stay below the 2× threshold.
    pub simple_edge_tiles: u64,
    /// Fine tiles containing at least one escalated cell.
    pub complex_edge_tiles: u64,
}

impl AaStats {
    /// Native-equivalent cell-sample units.
    ///
    /// A native pass counts one, 2×2 counts four and 4×4 counts sixteen.
    /// Adaptive's native classifier pass remains included for cells it later
    /// replaces. This intentionally measures coverage-grid work, not
    /// per-command kernel cost or wall time.
    #[must_use]
    pub fn sample_evaluations(&self) -> u64 {
        self.native_cells
            .saturating_add(self.ssaa2x_cells.saturating_mul(4))
            .saturating_add(self.ssaa4x_cells.saturating_mul(16))
    }

    /// The work a full-frame forced-4× comparison performs.
    #[must_use]
    pub fn forced_4x_evaluations(&self) -> u64 {
        self.output_cells.saturating_mul(16)
    }

    /// Fraction of coverage/sample work avoided versus full-frame forced 4×.
    ///
    /// A pathological frame may return a negative value: adaptive first
    /// evaluates the native classifier, so escalating every cell to 4× costs
    /// `17/16` of forced 4×. Reporting that honestly is more useful than
    /// clamping an instrumentation result into looking successful.
    #[must_use]
    pub fn work_reduction_vs_forced_4x(&self) -> f64 {
        let forced = self.forced_4x_evaluations();
        if forced == 0 {
            0.0
        } else {
            1.0 - self.sample_evaluations() as f64 / forced as f64
        }
    }

    /// Fine tiles represented by the four class counters.
    #[must_use]
    pub fn classified_tiles(&self) -> u64 {
        self.empty_tiles
            .saturating_add(self.fully_covered_tiles)
            .saturating_add(self.simple_edge_tiles)
            .saturating_add(self.complex_edge_tiles)
    }

    fn merge(&mut self, other: AaStats) {
        self.output_cells = self.output_cells.saturating_add(other.output_cells);
        self.native_cells = self.native_cells.saturating_add(other.native_cells);
        self.ssaa2x_cells = self.ssaa2x_cells.saturating_add(other.ssaa2x_cells);
        self.ssaa4x_cells = self.ssaa4x_cells.saturating_add(other.ssaa4x_cells);
        self.empty_tiles = self.empty_tiles.saturating_add(other.empty_tiles);
        self.fully_covered_tiles = self
            .fully_covered_tiles
            .saturating_add(other.fully_covered_tiles);
        self.simple_edge_tiles = self
            .simple_edge_tiles
            .saturating_add(other.simple_edge_tiles);
        self.complex_edge_tiles = self
            .complex_edge_tiles
            .saturating_add(other.complex_edge_tiles);
    }

    fn count_tile(&mut self, class: CoverageClass) {
        match class {
            CoverageClass::Empty => self.empty_tiles = self.empty_tiles.saturating_add(1),
            CoverageClass::FullyCovered => {
                self.fully_covered_tiles = self.fully_covered_tiles.saturating_add(1);
            }
            CoverageClass::SimpleEdge => {
                self.simple_edge_tiles = self.simple_edge_tiles.saturating_add(1);
            }
            CoverageClass::ComplexEdge => {
                self.complex_edge_tiles = self.complex_edge_tiles.saturating_add(1);
            }
        }
    }
}

/// The rest of what a frame's bits depend on: C10's **declared certified
/// configuration**, plus the semantic constants §10.2/§10.3 fixed by
/// measurement.
///
/// The constants are journaled explicitly even though they are `const` in this
/// crate, and the reason is what a manifest is *for*. C2 already hashes the
/// engine commit, so a changed constant does move the closure digest — but only
/// as an opaque commit difference. Naming them makes a manifest say *which*
/// number moved, which is the difference between a provenance record and a
/// receipt.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameConfig {
    /// The pixel rectangle.
    pub viewport: Viewport,
    /// The object→screen mapping.
    pub map: ScreenMap,
    /// The background, linear light, straight alpha. Opaque for ordinary
    /// renders; a zero alpha is `--transparent`.
    pub background: LinearRgba,
    /// Standard-mode coverage policy. Certified jobs always execute the
    /// canonical analytic path, whatever A/B policy was requested.
    pub aa: AaPolicy,
}

impl FrameConfig {
    /// A frame of `viewport` on the default map over an opaque background.
    #[must_use]
    pub fn new(viewport: Viewport, map: ScreenMap, background: LinearRgba) -> FrameConfig {
        FrameConfig {
            viewport,
            map,
            background,
            aa: AaPolicy::Adaptive,
        }
    }

    /// Select the standard-mode adaptive/forced-AA policy.
    #[must_use]
    pub fn with_aa_policy(mut self, aa: AaPolicy) -> FrameConfig {
        self.aa = aa;
        self
    }

    /// The layout of the raw certified frame this config renders into.
    ///
    /// Tight rows: the certified raw frame carries no stride padding, so its
    /// bytes are a function of the picture and nothing else. A padded layout is
    /// legal for an intermediate ([`FrameJob::render_into`] accepts one), but
    /// [`frame_digest`] hashes payload rows precisely so the choice cannot reach
    /// a golden either way.
    ///
    /// # Errors
    /// [`FrameError::ZeroDimension`] for an empty viewport.
    pub fn layout(&self) -> Result<FrameLayout, FrameError> {
        FrameLayout::tight(
            PixelFormat::Rgba16F,
            self.viewport.width,
            self.viewport.height,
        )
    }
}

/// The engine's full identity document, hashed into the input closure (§10.5f,
/// §16.7 C7 and C10).
///
/// Ordered, versioned and canonical: [`fmn_hash::Writer`] pins the field order
/// and canonicalizes floats at the boundary, so `-0.0` and `+0.0` cannot make
/// two identical configurations hash differently.
#[must_use]
pub fn journal(identity: EngineIdentity, config: &FrameConfig, tiling: Tiling) -> Vec<u8> {
    let mut w = Writer::new(ENGINE_SCHEMA);

    // C7 — execution-engine and backend identities.
    w.put_str(identity.engine.name());
    w.put_bool(identity.engine.certifiable());
    w.put_str(identity.tier.name());
    w.put_u32(identity.renderer_version);

    // C10 — the declared configuration.
    w.put_u32(config.viewport.width);
    w.put_u32(config.viewport.height);
    w.put_u32(tiling.macro_tile);
    w.put_u32(tiling.fine_tile);
    w.put_f64(config.map.scale);
    w.put_f64(config.map.origin[0]);
    w.put_f64(config.map.origin[1]);
    w.put_f64(config.background.r);
    w.put_f64(config.background.g);
    w.put_f64(config.background.b);
    w.put_f64(config.background.a);
    w.put_str(match config.aa {
        AaPolicy::Adaptive => "adaptive",
        AaPolicy::Ssaa2x => "ssaa2x",
        AaPolicy::Ssaa4x => "ssaa4x",
    });
    // The format's **name**, not `as u32`. A discriminant is a property of
    // fmn-frame's declaration order, so reordering that enum would silently move
    // every manifest digest ever issued — and a closure digest whose meaning can
    // change without anyone editing it is worse than no digest. `cache.rs` may
    // use the discriminant because a tile key lives for one run; a provenance
    // manifest is durable (§16.7, D-17).
    w.put_str(RAW_FRAME_FORMAT_NAME);

    // C10 — the semantic constants. Measured, not tuned; see each one's own
    // documentation for the measurement that fixed it.
    w.put_u64(fill::GRADIENT_STATIONS as u64);
    w.put_f64(fill::HINT_BUDGET_PX);
    w.put_u32(fill::FLATTEN_MAX_DEPTH);
    w.put_f64(stroke::MITER_LIMIT);
    w.put_f64(crate::hint::NEARLY_LINEAR_PX);
    w.put_u32(u32::from(AA_COMPLEX_2X_CROSSINGS));
    w.put_u32(u32::from(AA_COMPLEX_4X_CROSSINGS));
    w.put_f64(AA_STROKE_COMPLEX_MIN_WIDTH_BANDS);

    w.finish().expect("the identity document fits any limit")
}

/// The digest of [`journal`] — the value C7 and C10 contribute to the closure.
#[must_use]
pub fn journal_digest(identity: EngineIdentity, config: &FrameConfig, tiling: Tiling) -> Digest {
    fmn_hash::sha256(&journal(identity, config, tiling))
}

/// Why a frame could not be reduced to its canonical document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameEncodeError {
    /// The buffer has more than one plane, so a single-plane document would
    /// describe the luma and silently drop the chroma.
    MultiPlane {
        /// The format that was refused.
        format: PixelFormat,
    },
    /// The canonical envelope refused the document (a limit, in practice).
    Serial(fmn_hash::SerialError),
}

impl std::fmt::Display for FrameEncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MultiPlane { format } => write!(
                f,
                "{format:?} has {} planes; a canonical frame document describes one",
                format.plane_count()
            ),
            Self::Serial(e) => write!(f, "canonical frame document: {e:?}"),
        }
    }
}

impl std::error::Error for FrameEncodeError {}

/// The canonical digest of a rendered raw frame.
///
/// Hashes the geometry and the **payload rows only** — never the whole
/// allocation. A pooled [`FrameBuffer`] (PG-6's steady state) carries whatever a
/// previous frame left in its stride padding, and hashing that would make a
/// golden depend on pool history rather than on the picture. Rows go through
/// [`fmn_hash::Writer`], so the digest is a function of a declared schema rather
/// than of a `Vec<u8>`'s layout.
///
/// # Errors
/// Propagates the envelope's limit errors for a frame larger than the writer
/// admits.
pub fn frame_digest(buffer: &FrameBuffer) -> Result<Digest, FrameEncodeError> {
    Ok(fmn_hash::sha256(&encode_frame(buffer)?))
}

/// The canonical byte document a [`frame_digest`] is taken over — the form the
/// self-golden rig locks.
///
/// The document is materialized rather than streamed, which puts a ceiling on it:
/// [`fmn_hash::Limits::DEFAULT`] admits 256 MiB, so `Rgba16F` frames up to about
/// 4K (66 MB) encode comfortably and 8K (265 MB) does not. That is a deliberate
/// trade for the versioned envelope — a streamed digest would have no schema and
/// no float canonicalization — and it is stated here because the failure is a
/// typed error at an unusual resolution rather than anything a test would meet.
///
/// # Errors
/// [`FrameEncodeError::MultiPlane`] for a planar format;
/// [`FrameEncodeError::Serial`] for a frame past the envelope's limits.
pub fn encode_frame(buffer: &FrameBuffer) -> Result<Vec<u8>, FrameEncodeError> {
    let layout = buffer.layout();
    // Single-plane only, and refused rather than guessed. This walks plane 0, so
    // handing it an NV12 or P010 buffer would hash the luma and silently drop the
    // chroma — a document that looks like a frame and is not one. The certified
    // raw frame is always `Rgba16F`; `Rgba8` is admitted because it is the
    // canonical-PNG payload and locking that is the obvious next want.
    let format = layout.format();
    if format.plane_count() != 1 {
        return Err(FrameEncodeError::MultiPlane { format });
    }
    let width = layout.width() as usize;
    let height = layout.height() as usize;
    let stride = layout.stride(0);
    // The payload width the format itself declares, rather than a table here
    // that could drift from it.
    let row_bytes = format.min_row_bytes(layout.width(), 0).unwrap_or(width * 4);
    let mut w = Writer::new(FRAME_SCHEMA);
    w.put_u32(layout.width());
    w.put_u32(layout.height());
    w.put_str(format_name(format));
    let plane = buffer.plane(0);
    for y in 0..height {
        w.put_bytes(&plane[y * stride..y * stride + row_bytes]);
    }
    w.finish().map_err(FrameEncodeError::Serial)
}

/// The certified raw frame's pixel format, by name.
///
/// The engine's output format is a constant of the engine rather than a knob, so
/// it enters the closure as a fixed name.
pub const RAW_FRAME_FORMAT_NAME: &str = "rgba16f";

/// A stable name for a pixel format, for documents that outlive a build.
const fn format_name(format: PixelFormat) -> &'static str {
    match format {
        PixelFormat::Rgba8 => "rgba8",
        PixelFormat::Bgra8 => "bgra8",
        PixelFormat::Rgba16F => RAW_FRAME_FORMAT_NAME,
        PixelFormat::Nv12 => "nv12",
        PixelFormat::P010 => "p010",
    }
}

// ------------------------------------------------------------ the frame job

/// One instance's per-frame derived draw state.
///
/// Derived **once per frame, before any tile is touched**, for two reasons that
/// pull the same way. It is faster — `FillKernel::select` measures the compiled
/// outline's deviation, `join_wedges` solves every corner, and a
/// [`GradientField`] samples 64 stations, none of which may happen per tile per
/// row. And it makes the tile function take `&self`: nothing a worker touches is
/// mutable except its own scratch and its own band, which is what lets
/// [`FrameJob::render_into`] hand bands to threads with no synchronization
/// beyond the queue that assigns them.
#[derive(Debug, Clone)]
struct Draw {
    /// Index into [`RenderPlan::segments`] of this shape's first segment.
    first_segment: u32,
    /// How many segments the shape has.
    segment_count: u32,
    /// The shape index, for [`crate::fill::MonoTable::pieces_of`].
    shape: u32,
    /// The instance's screen translation.
    translate: [f64; 2],
    /// Frame-local world-space segments for a non-translation affine
    /// placement. Pure translations keep borrowing the retained object-space
    /// table and pay only [`Draw::translate`].
    transformed_segments: Option<Vec<Segment>>,
    /// Doubly-monotone pieces derived from [`Draw::transformed_segments`].
    transformed_pieces: Option<Vec<fill::MonoPiece>>,
    /// The interned style, copied so the tile loop indexes no table.
    style: Style,
    /// The hinted fill route, or [`FillKernel::General`].
    kernel: FillKernel,
    /// The joint overrides; empty for the round settings (ADR-0012).
    joins: Vec<JoinWedge>,
    /// Per-segment slabs and arc lengths derived for this styled occurrence.
    stroke: Option<stroke::PreparedStroke>,
    /// The interior colour field — `None` when the fill is flat, which is the
    /// overwhelming majority and the case that must not pay for the field.
    field: Option<GradientField>,
    /// The one fill colour, when the fill is flat.
    flat_fill: Option<[f32; 4]>,
    /// Screen AABB of the outline hull: rows outside it have zero fill coverage.
    fill_slab: [f64; 4],
    /// Does this instance contribute a fill pass at all?
    draws_fill: bool,
    /// Does it contribute a stroke pass?
    draws_stroke: bool,
}

/// A frame was assembled from derived artifacts that do not share one input
/// closure.
///
/// Each variant names the stale axis directly so callers can rebuild only the
/// artifact whose key moved. No mismatch is recoverable by rendering anyway:
/// doing so would return a valid-looking, deterministically wrong frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameJobError {
    /// The caller supplied an identity for renderer semantics other than the
    /// code that will execute.
    RendererVersionMismatch {
        /// Version named by the requested identity.
        requested: u32,
        /// Version implemented by this artifact.
        compiled: u32,
    },
    /// The requested engine has no implementation in this artifact.
    UnsupportedEngine {
        /// The engine that was requested.
        engine: EngineKind,
    },
    /// The monotone pieces were transformed under another screen mapping.
    MonoMapMismatch,
    /// The monotone table's shape-indexed geometry is stale.
    MonoPlanMismatch,
    /// The tile command lists were scattered under another screen mapping.
    BinningMapMismatch,
    /// The tile command lists cover a different exact viewport.
    BinningViewportMismatch,
    /// The tile command indices name a different painter-ordered plan.
    BinningPlanMismatch,
    /// The affine-only frame job received vector semantics that require
    /// [`crate::three_d::ThreeDJob`]'s captured camera and shared painter
    /// sequence.
    CameraProjectionRequired {
        /// Painter-sequence instance that requires the camera route.
        instance: u32,
    },
}

impl std::fmt::Display for FrameJobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RendererVersionMismatch {
                requested,
                compiled,
            } => write!(
                f,
                "renderer identity names version {requested}, but this artifact implements {compiled}"
            ),
            Self::UnsupportedEngine { engine } => {
                write!(
                    f,
                    "{} has no rendering backend in this artifact",
                    engine.name()
                )
            }
            Self::MonoMapMismatch => {
                f.write_str("monotone table screen map does not match the frame")
            }
            Self::MonoPlanMismatch => {
                f.write_str("monotone table geometry does not match the render plan")
            }
            Self::BinningMapMismatch => f.write_str("binning screen map does not match the frame"),
            Self::BinningViewportMismatch => {
                f.write_str("binning viewport does not match the frame")
            }
            Self::BinningPlanMismatch => f.write_str("binning does not match the render plan"),
            Self::CameraProjectionRequired { instance } => write!(
                f,
                "retained instance {instance} requires camera projection; render it through ThreeDJob"
            ),
        }
    }
}

impl std::error::Error for FrameJobError {}

/// A frame, compiled and ready to rasterize.
///
/// Borrows the retained plan, its derived monotone table and its binning;
/// owns only the per-instance derivation above. Construction is the serial
/// front-end; [`FrameJob::render_into`] is the parallel back-end, and the split
/// between them is exactly §9.3's FramePacket boundary one layer down.
#[derive(Debug)]
pub struct FrameJob<'a> {
    plan: &'a RenderPlan,
    mono: &'a fill::MonoTable,
    binning: &'a Binning,
    config: FrameConfig,
    identity: EngineIdentity,
    /// Index-aligned with `plan.shapes().instances()`; `None` where the
    /// instance contributes no pass. See [`FrameJob::with_identity`].
    draws: Vec<Option<Draw>>,
    cols: u32,
}

/// The render path after certified-mode normalization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RenderAa {
    /// The canonical analytic definition; no adaptive classifier executes.
    Canonical,
    /// Native analytic classifier plus fused resolve on complex cells.
    Adaptive,
    /// Full-frame forced sampling for A/B and debugging.
    Forced(u32),
}

/// One fixed subcell in a native output pixel.
#[derive(Debug, Clone, Copy)]
struct Subcell {
    px: u32,
    py: u32,
    samples: u32,
    x: u32,
    y: u32,
}

impl Subcell {
    fn centre(self) -> [f64; 2] {
        let inverse = 1.0 / f64::from(self.samples);
        [
            f64::from(self.px) + (f64::from(self.x) + 0.5) * inverse,
            f64::from(self.py) + (f64::from(self.y) + 0.5) * inverse,
        ]
    }
}

impl<'a> FrameJob<'a> {
    /// Compile a frame from a synchronized plan, its monotone table and its
    /// binning.
    ///
    /// All inputs are checked against collision-resistant content identities and
    /// exact map/viewport values. Carrying them separately is what lets a
    /// retained plan skip rebuilding the ones whose axes did not move; checking
    /// their keys is what makes that reuse safe.
    ///
    /// # Errors
    /// [`FrameJobError`] names the first derived artifact whose plan, map, or
    /// viewport does not match.
    pub fn new(
        plan: &'a RenderPlan,
        mono: &'a fill::MonoTable,
        binning: &'a Binning,
        config: FrameConfig,
    ) -> Result<FrameJob<'a>, FrameJobError> {
        Self::with_identity(plan, mono, binning, config, EngineIdentity::certified())
    }

    /// [`FrameJob::new`] with an explicit engine identity.
    ///
    /// The identity selects the certified refusal boundary as well as recording
    /// what the frame claims: [`EngineKind::CertifiedCpu`] always executes the
    /// canonical analytic path, while standard-only identities honor
    /// [`FrameConfig::aa`]. Every CPU route shares the semantic front-end;
    /// certified uses the scalar `f64` definition, while fast CPU substitutes
    /// its budgeted `f32` compositor. Recording which one ran is §10.5(f).
    ///
    /// # Errors
    /// [`FrameJobError::RendererVersionMismatch`] when the requested identity
    /// does not name this artifact's renderer semantics;
    /// [`FrameJobError::UnsupportedEngine`] for an annex backend that has not
    /// landed; otherwise see [`FrameJob::new`].
    pub fn with_identity(
        plan: &'a RenderPlan,
        mono: &'a fill::MonoTable,
        binning: &'a Binning,
        config: FrameConfig,
        identity: EngineIdentity,
    ) -> Result<FrameJob<'a>, FrameJobError> {
        if identity.renderer_version != RENDERER_VERSION {
            return Err(FrameJobError::RendererVersionMismatch {
                requested: identity.renderer_version,
                compiled: RENDERER_VERSION,
            });
        }
        if matches!(identity.engine, EngineKind::Metal | EngineKind::Cuda) {
            return Err(FrameJobError::UnsupportedEngine {
                engine: identity.engine,
            });
        }
        if mono.map() != config.map {
            return Err(FrameJobError::MonoMapMismatch);
        }
        if !mono.matches_plan(plan) {
            return Err(FrameJobError::MonoPlanMismatch);
        }
        if binning.map() != config.map {
            return Err(FrameJobError::BinningMapMismatch);
        }
        if binning.viewport() != config.viewport {
            return Err(FrameJobError::BinningViewportMismatch);
        }
        if !binning.matches_plan(plan) {
            return Err(FrameJobError::BinningPlanMismatch);
        }

        let map = config.map;
        let segments = plan.segments();
        let mut draws: Vec<Option<Draw>> = Vec::with_capacity(plan.shapes().instances().len());

        for (instance_index, inst) in plan.shapes().instances().iter().enumerate() {
            // Every instance gets a slot, including the ones that draw nothing.
            // `Binning`'s command lists hold **instance** indices, so a compacted
            // draw list would silently pair each command with the wrong shape and
            // style from the first skipped instance onward. `None` is the slot
            // for "this instance contributes no pass"; it costs a word and it is
            // the difference between an index and a coincidence.
            let Some(shape) = plan.shapes().shape(inst.shape) else {
                draws.push(None);
                continue;
            };
            let Some(style) = plan.styles().get(inst.style).copied() else {
                draws.push(None);
                continue;
            };
            let lo = shape.first_segment as usize;
            let hi = lo + shape.segment_count as usize;
            let segs = &segments[lo.min(segments.len())..hi.min(segments.len())];
            let camera_uniform = style.is_fixed_in_frame != 0.0
                || style.shading != [0.0; 3]
                || style
                    .clip_planes
                    .iter()
                    .flatten()
                    .any(|component| *component != 0.0)
                || style.scale_stroke_with_zoom
                || style.depth_test;
            let transformed_segments = (!inst.placement.is_translation()).then(|| {
                let mut transformed = segs
                    .iter()
                    .map(|segment| Segment {
                        p0: inst.placement.apply_vector(segment.p0),
                        p1: inst.placement.apply_vector(segment.p1),
                        p2: inst.placement.apply_vector(segment.p2),
                        s0: segment.s0,
                        s1: segment.s1,
                    })
                    .collect::<Vec<_>>();
                // Translation cannot change length. Keep it out of the
                // quadrature so a large world origin cannot perturb normalized
                // spans through cancellation, then place the coefficients.
                reparameterize_arc_length(&mut transformed);
                let translation = inst.placement.translation();
                for segment in &mut transformed {
                    for point in [&mut segment.p0, &mut segment.p1, &mut segment.p2] {
                        for axis in 0..3 {
                            point[axis] += translation[axis];
                        }
                    }
                }
                transformed
            });
            let effective_segments = transformed_segments.as_deref().unwrap_or(segs);
            let placement_z = if transformed_segments.is_some() {
                0.0
            } else {
                inst.placement.translation()[2]
            };
            let leaves_view_plane = effective_segments.iter().any(|segment| {
                [segment.p0, segment.p1, segment.p2]
                    .iter()
                    .any(|point| point[2] != 0.0)
            }) || placement_z != 0.0;
            if camera_uniform || leaves_view_plane {
                return Err(FrameJobError::CameraProjectionRequired {
                    instance: instance_index as u32,
                });
            }
            let translate = if transformed_segments.is_some() {
                [0.0; 2]
            } else {
                fill::instance_translation(inst, map)
            };
            let transformed_pieces = transformed_segments.as_ref().map(|segments| {
                fill::MonoTable::pieces_for_segments(segments, &shape.subpath_starts, map)
            });

            let draws_fill = style.fill_rgba[3] > 0.0 || style.fill_rgba_end[3] > 0.0;
            let draws_stroke = (style.stroke_width > 0.0 || style.stroke_width_end > 0.0)
                && (style.stroke_rgba[3] > 0.0 || style.stroke_rgba_end[3] > 0.0);
            if !draws_fill && !draws_stroke {
                // An instance with no visible pass composites as the identity, so
                // skipping its work cannot change a byte — but it still holds its
                // index. Both halves of that sentence matter.
                draws.push(None);
                continue;
            }

            let flat = fill_is_flat(&style);
            let kernel = if inst.hint_unsafe || !inst.placement.is_translation() {
                FillKernel::General
            } else {
                FillKernel::select(shape, segs, map, translate)
            };
            let joins = stroke::join_wedges(
                effective_segments,
                &shape.subpath_starts,
                &style,
                map,
                translate,
            );
            let stroke = draws_stroke
                .then(|| stroke::PreparedStroke::new(effective_segments, &style, map, translate));
            let field = if draws_fill && !flat {
                Some(GradientField::build(effective_segments, map))
            } else {
                None
            };
            let fill_slab = hull_slab(effective_segments, map, translate);
            draws.push(Some(Draw {
                first_segment: shape.first_segment,
                segment_count: shape.segment_count,
                shape: inst.shape,
                translate,
                transformed_segments,
                transformed_pieces,
                style,
                kernel,
                joins,
                stroke,
                field,
                flat_fill: if flat {
                    Some(fill_rgba_at(&style, 0.0))
                } else {
                    None
                },
                fill_slab,
                draws_fill,
                draws_stroke,
            }));
        }
        debug_assert_eq!(
            draws.len(),
            plan.shapes().instances().len(),
            "the draw list must be index-aligned with the instance list"
        );

        let cols = config
            .viewport
            .width
            .div_ceil(binning.tiling().fine_tile.max(1));

        Ok(FrameJob {
            plan,
            mono,
            binning,
            config,
            identity,
            draws,
            cols,
        })
    }

    /// The engine identity this frame will claim.
    #[must_use]
    pub fn identity(&self) -> EngineIdentity {
        self.identity
    }

    /// The requested policy after applying certified's permanent analytic-path
    /// refusal.
    fn render_aa(&self) -> RenderAa {
        if self.identity.engine.certifiable() {
            return RenderAa::Canonical;
        }
        match self.config.aa {
            AaPolicy::Adaptive => RenderAa::Adaptive,
            AaPolicy::Ssaa2x => RenderAa::Forced(2),
            AaPolicy::Ssaa4x => RenderAa::Forced(4),
        }
    }

    /// This frame's contribution to the input closure (C7 + C10).
    #[must_use]
    pub fn journal_digest(&self) -> Digest {
        journal_digest(self.identity, &self.config, self.binning.tiling())
    }

    /// How many instances contribute a pass.
    ///
    /// Not `draws.len()`: that is the instance count, because the draw list is
    /// index-aligned with the instance list and carries a `None` for every
    /// instance that draws nothing.
    #[must_use]
    pub fn draw_count(&self) -> usize {
        self.draws.iter().flatten().count()
    }

    /// Rasterize into a freshly allocated raw frame.
    ///
    /// # Errors
    /// [`FrameError::ZeroDimension`] for an empty viewport.
    pub fn render(&self, threads: usize) -> Result<FrameBuffer, FrameError> {
        let mut buffer = FrameBuffer::new(self.config.layout()?);
        self.render_into(threads, &mut buffer)?;
        Ok(buffer)
    }

    /// Rasterize a raw frame and return deterministic adaptive-AA work counters.
    ///
    /// The image is identical to [`FrameJob::render`] for the same job and
    /// thread count; instrumentation is integer-only, accumulated outside the
    /// pixel arithmetic.
    ///
    /// # Errors
    /// See [`FrameJob::render`].
    pub fn render_with_stats(&self, threads: usize) -> Result<(FrameBuffer, AaStats), FrameError> {
        let mut buffer = FrameBuffer::new(self.config.layout()?);
        let stats = self.render_into_profiled(threads, &mut buffer)?;
        Ok((buffer, stats))
    }

    /// Rasterize into a caller-owned frame — the pooled-buffer path PG-6 measures.
    ///
    /// `threads` is a **scheduling** choice and nothing else: the bytes written
    /// are identical at every value, which is PG-5's per-commit property and is
    /// asserted directly rather than argued.
    ///
    /// # Errors
    /// [`FrameError::FormatMismatch`] unless `dst` is `Rgba16F`;
    /// [`FrameError::DimensionMismatch`] unless it matches the configured
    /// viewport.
    pub fn render_into(&self, threads: usize, dst: &mut FrameBuffer) -> Result<(), FrameError> {
        self.render_into_profiled(threads, dst).map(|_| ())
    }

    /// Shared render body for ordinary and instrumented entry points.
    fn render_into_profiled(
        &self,
        threads: usize,
        dst: &mut FrameBuffer,
    ) -> Result<AaStats, FrameError> {
        match self.identity.engine {
            EngineKind::CertifiedCpu => {
                if self.identity.tier == Tier::Scalar {
                    self.render_into_profiled_with::<CertifiedScalar>(threads, dst)
                } else {
                    self.render_into_profiled_with::<CertifiedBuildTier>(threads, dst)
                }
            }
            EngineKind::FastCpu => {
                if self.identity.tier == Tier::Scalar {
                    self.render_into_profiled_with::<FastScalar>(threads, dst)
                } else {
                    self.render_into_profiled_with::<FastBuildTier>(threads, dst)
                }
            }
            // Construction rejects annex identities until their back-ends land.
            // Reaching this arm would otherwise publish CPU bytes under a false
            // engine identity, so fail closed rather than substitute.
            EngineKind::Metal | EngineKind::Cuda => {
                unreachable!("unimplemented annex engines are refused by FrameJob::with_identity")
            }
        }
    }

    /// Monomorphized render body selected once per frame.
    fn render_into_profiled_with<K: PixelKernel>(
        &self,
        threads: usize,
        dst: &mut FrameBuffer,
    ) -> Result<AaStats, FrameError> {
        if dst.layout().format() != PixelFormat::Rgba16F {
            return Err(FrameError::FormatMismatch {
                expected: "Rgba16F raw frame",
                got: dst.layout().format(),
            });
        }
        // The binning must have been built for this viewport and this tiling.
        // If it was not, `cols` disagrees with the grid the command lists were
        // scattered into and every tile index names the wrong tile — a wrong
        // picture with no error anywhere, which is the failure mode this crate
        // is least able to afford. Checked here rather than in `new` because
        // this is where the grid is walked.
        let rows = self
            .config
            .viewport
            .height
            .div_ceil(self.binning.tiling().fine_tile.max(1));
        if self.binning.tile_count() != self.cols as usize * rows as usize {
            return Err(FrameError::DimensionMismatch);
        }
        if dst.layout().width() != self.config.viewport.width
            || dst.layout().height() != self.config.viewport.height
        {
            return Err(FrameError::DimensionMismatch);
        }

        let tile = self.binning.tiling().fine_tile.max(1) as usize;
        let stride = dst.layout().stride(0);
        let band_bytes = stride * tile;
        let plane = dst.plane_mut(0);

        // `chunks_mut` is the whole safety argument: it yields provably disjoint
        // `&mut [u8]`, one per band, so write-disjointness (§10.5b) is a fact the
        // borrow checker enforces rather than a claim a comment makes.
        let mut bands: Vec<(usize, &mut [u8])> = plane.chunks_mut(band_bytes).enumerate().collect();

        if threads <= 1 {
            let mut worker = Worker::<K>::new(tile, self.cols as usize);
            for (band, bytes) in bands {
                self.render_band::<K>(&mut worker, band, bytes, stride);
            }
            return Ok(worker.stats);
        }

        // Popped from the end, so the queue is a stack; which worker takes which
        // band is deliberately unspecified, because a band's bytes do not depend
        // on who computed them.
        bands.reverse();
        let queue = Mutex::new(bands);
        let stats = Mutex::new(AaStats::default());
        std::thread::scope(|scope| {
            for _ in 0..threads {
                scope.spawn(|| {
                    let mut worker = Worker::<K>::new(tile, self.cols as usize);
                    loop {
                        let next = queue.lock().unwrap_or_else(PoisonError::into_inner).pop();
                        let Some((band, bytes)) = next else { break };
                        self.render_band::<K>(&mut worker, band, bytes, stride);
                    }
                    stats
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .merge(worker.stats);
                });
            }
        });
        Ok(stats.into_inner().unwrap_or_else(PoisonError::into_inner))
    }

    /// Rasterize one band: `tile` pixel rows spanning the full frame width.
    fn render_band<K: PixelKernel>(
        &self,
        worker: &mut Worker<K>,
        band: usize,
        bytes: &mut [u8],
        stride: usize,
    ) {
        let tile = self.binning.tiling().fine_tile.max(1);
        let width = self.config.viewport.width;
        let height = self.config.viewport.height;
        let y0 = band as u32 * tile;
        let y1 = (y0 + tile).min(height);
        let aa = self.render_aa();

        for tx in 0..self.cols {
            let t = band * self.cols as usize + tx as usize;
            worker.tile_classes[tx as usize] = if t < self.binning.tile_count() {
                self.initial_tile_class(t)
            } else {
                CoverageClass::Empty
            };
        }

        // The background is written first and unconditionally, so a band with no
        // commands still costs exactly one pass — and so a tile that draws
        // nothing is not distinguishable from one that never ran.
        let bg = K::from_premul(self.config.background.premultiply());
        for py in y0..y1 {
            let row = &mut bytes[(py - y0) as usize * stride..];
            for tx in 0..self.cols {
                let x_lo = tx * tile;
                let x_hi = (x_lo + tile).min(width);
                if x_lo >= x_hi {
                    continue;
                }
                let w = (x_hi - x_lo) as usize;
                worker.acc[..w].fill(bg);
                worker.edges[..w].fill(0);
                worker.stats.output_cells = worker.stats.output_cells.saturating_add(w as u64);

                let t = band * self.cols as usize + tx as usize;
                if t < self.binning.tile_count() {
                    match aa {
                        RenderAa::Forced(samples) => {
                            for i in 0..w {
                                worker.acc[i] = self.composite_pixel_supersampled::<K>(
                                    t,
                                    x_lo + i as u32,
                                    py,
                                    samples,
                                );
                            }
                            if samples == 2 {
                                worker.stats.ssaa2x_cells =
                                    worker.stats.ssaa2x_cells.saturating_add(w as u64);
                            } else {
                                worker.stats.ssaa4x_cells =
                                    worker.stats.ssaa4x_cells.saturating_add(w as u64);
                            }
                        }
                        RenderAa::Canonical | RenderAa::Adaptive => {
                            let classify = aa == RenderAa::Adaptive;
                            self.composite_row::<K>(worker, t, py, x_lo, x_hi, classify);
                            worker.stats.native_cells =
                                worker.stats.native_cells.saturating_add(w as u64);
                            if classify {
                                for i in 0..w {
                                    let crossings = worker.edges[i];
                                    let samples = if crossings >= AA_COMPLEX_4X_CROSSINGS {
                                        4
                                    } else if crossings >= AA_COMPLEX_2X_CROSSINGS {
                                        2
                                    } else {
                                        continue;
                                    };
                                    worker.tile_classes[tx as usize] = CoverageClass::ComplexEdge;
                                    worker.acc[i] = self.composite_pixel_supersampled::<K>(
                                        t,
                                        x_lo + i as u32,
                                        py,
                                        samples,
                                    );
                                    if samples == 2 {
                                        worker.stats.ssaa2x_cells =
                                            worker.stats.ssaa2x_cells.saturating_add(1);
                                    } else {
                                        worker.stats.ssaa4x_cells =
                                            worker.stats.ssaa4x_cells.saturating_add(1);
                                    }
                                }
                            }
                        }
                    }
                }

                let base = x_lo as usize * 8;
                K::write_row(&worker.acc[..w], &mut row[base..base + w * 8]);
            }
        }

        for class in worker.tile_classes.iter().take(self.cols as usize) {
            worker.stats.count_tile(*class);
        }
    }

    /// Initial aggregate class from the command flags alone.
    ///
    /// Adaptive's per-cell pass can upgrade `SimpleEdge` to `ComplexEdge`; it
    /// can never downgrade a conservative edge classification to an interior
    /// promise.
    fn initial_tile_class(&self, tile: usize) -> CoverageClass {
        let draws = self.binning.tile(tile);
        let flags = self.binning.tile_flags(tile);
        let mut visible = false;
        let mut edge = false;
        for (k, &d) in draws.iter().enumerate() {
            let Some(Some(rec)) = self.draws.get(d as usize) else {
                continue;
            };
            visible = true;
            if rec.draws_stroke || flags.get(k).copied() != Some(CLASS_INTERIOR) {
                edge = true;
            }
        }
        if !visible {
            CoverageClass::Empty
        } else if edge {
            CoverageClass::SimpleEdge
        } else {
            CoverageClass::FullyCovered
        }
    }

    /// Composite one tile's command list over one pixel row of the accumulator.
    ///
    /// The draw loop is the **outer** one and painter order is its order: that is
    /// what lets a fill contribute a whole row of coverage from one pass over its
    /// pieces while a stroke still shades per pixel, without either being able to
    /// reorder the composite.
    fn composite_row<K: PixelKernel>(
        &self,
        worker: &mut Worker<K>,
        tile: usize,
        py: u32,
        x_lo: u32,
        x_hi: u32,
        classify: bool,
    ) {
        let draws = self.binning.tile(tile);
        let flags = self.binning.tile_flags(tile);
        for (k, &d) in draws.iter().enumerate() {
            // `d` is an instance index, and `draws` is indexed by instance
            // index. A `None` is an instance with no pass; an out-of-range index
            // would be a binning built against a different plan.
            let Some(Some(rec)) = self.draws.get(d as usize) else {
                continue;
            };
            let interior = flags.get(k).copied() == Some(CLASS_INTERIOR);
            // R-5: within one object the fill draws before the stroke, unless
            // `stroke_behind` swaps them (docs/RENDER_ORDER.md).
            if rec.style.stroke_behind {
                self.stroke_pass::<K>(worker, rec, py, x_lo, x_hi, classify);
                self.fill_pass::<K>(worker, rec, interior, py, x_lo..x_hi, classify);
            } else {
                self.fill_pass::<K>(worker, rec, interior, py, x_lo..x_hi, classify);
                self.stroke_pass::<K>(worker, rec, py, x_lo, x_hi, classify);
            }
        }
    }

    /// §10.2's fill over one row of one tile.
    fn fill_pass<K: PixelKernel>(
        &self,
        worker: &mut Worker<K>,
        rec: &Draw,
        interior: bool,
        py: u32,
        x: std::ops::Range<u32>,
        classify: bool,
    ) {
        let (x_lo, x_hi) = (x.start, x.end);
        if !rec.draws_fill || row_misses(rec.fill_slab, py) {
            return;
        }
        let w = (x_hi - x_lo) as usize;

        // Exactly one of three sources fills the row, and the order of the tests
        // is the order of §10.4's own argument: a classified interior costs
        // nothing, a hinted kernel costs a closed form, and the general machinery
        // is the fallback rather than the toll road.
        let pieces = self.pieces_of(rec);
        let mut general_classified = false;
        if interior {
            worker.cov[..w].copy_from_slice(worker.scratch.interior_row(x_lo, x_hi));
        } else if !rec.kernel.row(py, x_lo, x_hi, &mut worker.cov[..w]) {
            if classify {
                let (coverage, crossings) =
                    worker
                        .scratch
                        .fill_row_classified(pieces, rec.translate, py, x_lo, x_hi);
                worker.cov[..w].copy_from_slice(coverage);
                for (edge, count) in worker.edges[..w].iter_mut().zip(crossings) {
                    *edge = edge.saturating_add(*count);
                }
                general_classified = true;
            } else {
                worker.cov[..w].copy_from_slice(worker.scratch.fill_row(
                    pieces,
                    rec.translate,
                    py,
                    x_lo,
                    x_hi,
                ));
            }
        }

        let segments = self.segments_of(rec);
        for i in 0..w {
            let coverage = worker.cov[i];
            if coverage <= 0.0 {
                continue;
            }
            if classify && !interior && !general_classified {
                let crossings =
                    fill::boundary_crossings_at_cell(pieces, rec.translate, py, x_lo + i as u32);
                let contribution = if coverage < 1.0 && crossings == 0 {
                    // A partial cell whose boundary escaped all six interior
                    // probes contains a sub-probe feature. Conservatively call
                    // it complex rather than silently missing it.
                    AA_COMPLEX_2X_CROSSINGS
                } else {
                    crossings
                };
                worker.edges[i] = worker.edges[i].saturating_add(contribution);
            }
            if rec.flat_fill.is_none() {
                let p = [f64::from(x_lo + i as u32) + 0.5, f64::from(py) + 0.5];
                let field = rec.field.as_ref().expect("a non-flat fill carries a field");
                let rgba = fill_rgba_with_border(
                    &rec.style,
                    field,
                    segments,
                    self.config.map,
                    rec.translate,
                    p,
                );
                worker.acc[i] = K::source_over(rgba, coverage, worker.acc[i]);
            }
        }
        if let Some(rgba) = rec.flat_fill {
            K::source_over_span(rgba, &worker.cov[..w], &mut worker.acc[..w]);
        }
    }

    /// §10.3's stroke over one row of one tile.
    fn stroke_pass<K: PixelKernel>(
        &self,
        worker: &mut Worker<K>,
        rec: &Draw,
        py: u32,
        x_lo: u32,
        x_hi: u32,
        classify: bool,
    ) {
        let Some(stroke) = rec.stroke.as_ref() else {
            return;
        };
        let slab = stroke.slab();
        if !rec.draws_stroke || row_misses(slab, py) {
            return;
        }
        let segments = self.segments_of(rec);
        let w = (x_hi - x_lo) as usize;
        for i in 0..w {
            let p = [f64::from(x_lo + i as u32) + 0.5, f64::from(py) + 0.5];
            if p[0] < slab[0] || p[0] > slab[2] {
                continue;
            }
            let (coverage, s) = stroke.shade(
                segments,
                &rec.joins,
                &rec.style,
                self.config.map,
                rec.translate,
                p,
            );
            if classify {
                let centre_is_eligible_edge = coverage > 0.0
                    && coverage < 1.0
                    && stroke_contributes_complexity(&rec.style, self.config.map, s);
                if centre_is_eligible_edge
                    || self.stroke_has_eligible_subcell_edge(
                        rec,
                        segments,
                        py,
                        x_lo + i as u32,
                        coverage,
                    )
                {
                    worker.edges[i] = worker.edges[i].saturating_add(1);
                }
            }
            if coverage <= 0.0 {
                continue;
            }
            worker.acc[i] = K::source_over(stroke_rgba_at(&rec.style, s), coverage, worker.acc[i]);
        }
    }

    /// Does an eligible off-centre stroke edge reach a 2×2 probe point?
    ///
    /// The canonical stroke pass shades the centre, which can be fully inside or
    /// outside while an edge still crosses a corner of the cell. Adaptive
    /// classification therefore probes the four 2× positions when the centre
    /// did not already establish an eligible edge. Each draw contributes at
    /// most one boundary sheet.
    fn stroke_has_eligible_subcell_edge(
        &self,
        rec: &Draw,
        segments: &[Segment],
        py: u32,
        px: u32,
        centre_coverage: f64,
    ) -> bool {
        // For a constant-width stroke, a saturated smoothstep proves that the
        // silhouette is at least half an AA band from the centre. The furthest
        // 2× probe is only `sqrt(1/8)` pixels away, so at the default 1.5 px
        // band no probe can possibly cross the edge. A width ramp does not have
        // that Lipschitz bound: its silhouette can move between the centre and
        // a probe, so ramped strokes retain the probes.
        let aa_band = effective_aa_band(&rec.style);
        let centre_is_saturated = centre_coverage <= 0.0 || centre_coverage >= 1.0;
        let constant_width = rec.style.stroke_width == rec.style.stroke_width_end;
        if constant_width && centre_is_saturated && aa_band > std::f64::consts::FRAC_1_SQRT_2 {
            return false;
        }
        let Some(stroke) = rec.stroke.as_ref() else {
            return false;
        };
        for dy in [0.25, 0.75] {
            for dx in [0.25, 0.75] {
                let p = [f64::from(px) + dx, f64::from(py) + dy];
                let (coverage, s) = stroke.shade(
                    segments,
                    &rec.joins,
                    &rec.style,
                    self.config.map,
                    rec.translate,
                    p,
                );
                if !stroke_contributes_complexity(&rec.style, self.config.map, s) {
                    continue;
                }
                let sample_is_edge = coverage > 0.0 && coverage < 1.0;
                let crosses_from_centre = centre_coverage <= 0.0 && coverage > 0.0
                    || centre_coverage >= 1.0 && coverage < 1.0;
                if sample_is_edge || crosses_from_centre {
                    return true;
                }
            }
        }
        false
    }

    /// Recompose one native pixel from a fixed subcell grid and resolve it
    /// immediately.
    ///
    /// Painter order is repeated independently at every sample; averaging
    /// already-composited samples is what makes overlapping edges correct.
    /// Only the four `f64` sums below survive the loop, so there is no
    /// supersampled frame or second resolve pass.
    fn composite_pixel_supersampled<K: PixelKernel>(
        &self,
        tile: usize,
        px: u32,
        py: u32,
        samples: u32,
    ) -> K::Pixel {
        let samples = samples.max(1);
        let draws = self.binning.tile(tile);
        let flags = self.binning.tile_flags(tile);
        let mut sum = PremulRgba::TRANSPARENT;

        for sample_y in 0..samples {
            for sample_x in 0..samples {
                let subcell = Subcell {
                    px,
                    py,
                    samples,
                    x: sample_x,
                    y: sample_y,
                };
                let mut acc = K::from_premul(self.config.background.premultiply());
                for (k, &d) in draws.iter().enumerate() {
                    let Some(Some(rec)) = self.draws.get(d as usize) else {
                        continue;
                    };
                    let interior = flags.get(k).copied() == Some(CLASS_INTERIOR);
                    if rec.style.stroke_behind {
                        acc = self.stroke_sample::<K>(rec, subcell, acc);
                        acc = self.fill_sample::<K>(rec, interior, subcell, acc);
                    } else {
                        acc = self.fill_sample::<K>(rec, interior, subcell, acc);
                        acc = self.stroke_sample::<K>(rec, subcell, acc);
                    }
                }
                let acc = K::to_premul(acc);
                sum.r += acc.r;
                sum.g += acc.g;
                sum.b += acc.b;
                sum.a += acc.a;
            }
        }

        let inverse = 1.0 / f64::from(samples * samples);
        K::from_premul(PremulRgba {
            r: sum.r * inverse,
            g: sum.g * inverse,
            b: sum.b * inverse,
            a: sum.a * inverse,
        })
    }

    /// One fill pass at one subcell, preserving the native exact coverage
    /// kernel (primitive hint or general curve-area integral).
    fn fill_sample<K: PixelKernel>(
        &self,
        rec: &Draw,
        interior: bool,
        subcell: Subcell,
        dst: K::Pixel,
    ) -> K::Pixel {
        if !rec.draws_fill {
            return dst;
        }
        let coverage = if interior {
            1.0
        } else {
            rec.kernel
                .coverage_subcell(
                    subcell.px,
                    subcell.py,
                    subcell.samples,
                    subcell.x,
                    subcell.y,
                )
                .unwrap_or_else(|| {
                    fill::coverage_at_subcell(
                        self.pieces_of(rec),
                        rec.translate,
                        subcell.py,
                        subcell.px,
                        subcell.samples,
                        subcell.x,
                        subcell.y,
                    )
                })
        };
        if coverage <= 0.0 {
            return dst;
        }
        let p = subcell.centre();
        let rgba = match rec.flat_fill {
            Some(c) => c,
            None => fill_rgba_with_border(
                &rec.style,
                rec.field.as_ref().expect("a non-flat fill carries a field"),
                self.segments_of(rec),
                self.config.map,
                rec.translate,
                p,
            ),
        };
        K::source_over(rgba, coverage, dst)
    }

    /// One stroke pass at one subcell centre.
    fn stroke_sample<K: PixelKernel>(
        &self,
        rec: &Draw,
        subcell: Subcell,
        dst: K::Pixel,
    ) -> K::Pixel {
        let Some(stroke) = rec.stroke.as_ref() else {
            return dst;
        };
        let p = subcell.centre();
        let (coverage, s) = stroke.shade(
            self.segments_of(rec),
            &rec.joins,
            &rec.style,
            self.config.map,
            rec.translate,
            p,
        );
        if coverage <= 0.0 {
            dst
        } else {
            K::source_over(stroke_rgba_at(&rec.style, s), coverage, dst)
        }
    }

    /// This draw's slice of the plan's one flat segment table.
    fn segments_of<'draw>(&'draw self, rec: &'draw Draw) -> &'draw [Segment] {
        if let Some(segments) = rec.transformed_segments.as_deref() {
            return segments;
        }
        let all = self.plan.segments();
        let lo = (rec.first_segment as usize).min(all.len());
        let hi = (rec.first_segment as usize + rec.segment_count as usize).min(all.len());
        &all[lo..hi]
    }

    /// This draw's fill pieces, transformed once per frame when its placement
    /// has a non-identity linear part.
    fn pieces_of<'draw>(&'draw self, rec: &'draw Draw) -> &'draw [fill::MonoPiece] {
        rec.transformed_pieces
            .as_deref()
            .unwrap_or_else(|| self.mono.pieces_of(rec.shape))
    }
}

// ------------------------------------------------------------------ the worker

/// Does this stroke carry silhouette detail outside the analytic AA band?
fn stroke_contributes_complexity(style: &Style, map: ScreenMap, s: f64) -> bool {
    let aa_band = effective_aa_band(style);
    let full_width = 2.0 * half_width_px(style, map, s);
    full_width >= AA_STROKE_COMPLEX_MIN_WIDTH_BANDS * aa_band
}

/// The same nonzero AA width [`crate::stroke::aa_coverage`] evaluates.
fn effective_aa_band(style: &Style) -> f64 {
    if style.anti_alias_width > 0.0 {
        f64::from(style.anti_alias_width)
    } else {
        1e-8
    }
}

/// One thread's scratch, allocated once per band-loop rather than per row.
///
/// PG-6 forbids steady-state per-frame heap allocation. A worker's buffers are
/// sized for the widest tile the frame can present and then reused for every
/// band it takes.
struct Worker<K: PixelKernel> {
    /// The row accumulator, premultiplied linear light.
    acc: Vec<K::Pixel>,
    /// One draw's coverage over the row.
    cov: Vec<f64>,
    /// Saturating independent-boundary count for each cell in the row.
    edges: Vec<u8>,
    /// The fill's own scratch.
    scratch: RowScratch,
    /// Aggregate class for each fine tile in the current band.
    tile_classes: Vec<CoverageClass>,
    /// This worker's integer-only instrumentation.
    stats: AaStats,
    marker: std::marker::PhantomData<K>,
}

impl<K: PixelKernel> Worker<K> {
    fn new(tile: usize, cols: usize) -> Worker<K> {
        Worker {
            acc: vec![K::from_premul(PremulRgba::TRANSPARENT); tile],
            cov: vec![0.0; tile],
            edges: vec![0; tile],
            scratch: RowScratch::for_tile(tile as u32),
            tile_classes: vec![CoverageClass::Empty; cols],
            stats: AaStats::default(),
            marker: std::marker::PhantomData,
        }
    }
}

// ------------------------------------------------------------------ the pieces

/// One compositing arithmetic policy.
///
/// The type is selected once in [`FrameJob::render_into_profiled`], so the hot
/// pixel loop contains neither runtime feature detection nor a function-pointer
/// dispatch. Every implementation is elementwise across RGBA: there is no
/// horizontal reduction whose association could depend on lane width.
trait PixelKernel {
    type Pixel: Copy + Send;

    fn from_premul(pixel: PremulRgba) -> Self::Pixel;
    fn to_premul(pixel: Self::Pixel) -> PremulRgba;
    fn source_over(rgba: [f32; 4], coverage: f64, dst: Self::Pixel) -> Self::Pixel;
    fn write_row(acc: &[Self::Pixel], out: &mut [u8]);

    #[inline]
    fn source_over_span(rgba: [f32; 4], coverage: &[f64], dst: &mut [Self::Pixel]) {
        for (&coverage, dst) in coverage.iter().zip(dst) {
            if coverage <= 0.0 {
                continue;
            }
            *dst = Self::source_over(rgba, coverage, *dst);
        }
    }
}

/// The certified scalar definition.
struct CertifiedScalar;

impl PixelKernel for CertifiedScalar {
    type Pixel = PremulRgba;

    #[inline]
    fn from_premul(pixel: PremulRgba) -> Self::Pixel {
        pixel
    }

    #[inline]
    fn to_premul(pixel: Self::Pixel) -> PremulRgba {
        pixel
    }

    #[inline]
    fn source_over(rgba: [f32; 4], coverage: f64, dst: Self::Pixel) -> Self::Pixel {
        source_over(rgba, coverage, dst)
    }

    #[inline]
    fn write_row(acc: &[Self::Pixel], out: &mut [u8]) {
        write_row(acc, out);
    }
}

/// Certified build-tier route.
///
/// The measured AoS compositor did not amortize gather/scatter, so it keeps the
/// faster scalar expression. Other hot kernels in the artifact still use
/// `std::simd`; build-tier selection must never force a slower implementation.
struct CertifiedBuildTier;

impl PixelKernel for CertifiedBuildTier {
    type Pixel = PremulRgba;

    #[inline]
    fn from_premul(pixel: PremulRgba) -> Self::Pixel {
        pixel
    }

    #[inline]
    fn to_premul(pixel: Self::Pixel) -> PremulRgba {
        pixel
    }

    #[inline]
    fn source_over(rgba: [f32; 4], coverage: f64, dst: Self::Pixel) -> Self::Pixel {
        source_over(rgba, coverage, dst)
    }

    #[inline]
    fn write_row(acc: &[Self::Pixel], out: &mut [u8]) {
        write_row(acc, out);
    }
}

#[derive(Debug, Clone, Copy)]
struct PremulRgba32 {
    r: f32,
    g: f32,
    b: f32,
    a: f32,
}

/// Standard-mode scalar mixed precision: f32 colour and blend arithmetic.
struct FastScalar;

impl PixelKernel for FastScalar {
    type Pixel = PremulRgba32;

    #[inline]
    #[allow(clippy::cast_possible_truncation)]
    fn from_premul(pixel: PremulRgba) -> Self::Pixel {
        PremulRgba32 {
            r: pixel.r as f32,
            g: pixel.g as f32,
            b: pixel.b as f32,
            a: pixel.a as f32,
        }
    }

    #[inline]
    fn to_premul(pixel: Self::Pixel) -> PremulRgba {
        PremulRgba {
            r: f64::from(pixel.r),
            g: f64::from(pixel.g),
            b: f64::from(pixel.b),
            a: f64::from(pixel.a),
        }
    }

    #[inline]
    #[allow(clippy::cast_possible_truncation)]
    fn source_over(rgba: [f32; 4], coverage: f64, dst: Self::Pixel) -> Self::Pixel {
        let alpha = rgba[3] * coverage as f32;
        let inverse = 1.0 - alpha;
        PremulRgba32 {
            r: rgba[0] * alpha + inverse * dst.r,
            g: rgba[1] * alpha + inverse * dst.g,
            b: rgba[2] * alpha + inverse * dst.b,
            a: alpha + inverse * dst.a,
        }
    }

    #[inline]
    fn write_row(acc: &[Self::Pixel], out: &mut [u8]) {
        write_row_f32(acc, out);
    }
}

/// Standard-mode build-tier route.
struct FastBuildTier;

impl PixelKernel for FastBuildTier {
    type Pixel = PremulRgba32;

    #[inline]
    fn from_premul(pixel: PremulRgba) -> Self::Pixel {
        FastScalar::from_premul(pixel)
    }

    #[inline]
    fn to_premul(pixel: Self::Pixel) -> PremulRgba {
        FastScalar::to_premul(pixel)
    }

    #[inline]
    fn source_over(rgba: [f32; 4], coverage: f64, dst: Self::Pixel) -> Self::Pixel {
        FastScalar::source_over(rgba, coverage, dst)
    }

    #[inline]
    fn write_row(acc: &[Self::Pixel], out: &mut [u8]) {
        FastScalar::write_row(acc, out);
    }
}

/// Porter–Duff source-over of a straight-alpha linear colour at a coverage.
///
/// Coverage multiplies **alpha only**: a partly-covered pixel is the same colour
/// over a smaller area, not a paler colour over the whole one. Doing it the other
/// way is the classic antialiasing bug that turns a red line on white into a pink
/// line rather than a red line with soft edges.
fn source_over(rgba: [f32; 4], coverage: f64, dst: PremulRgba) -> PremulRgba {
    LinearRgba {
        r: f64::from(rgba[0]),
        g: f64::from(rgba[1]),
        b: f64::from(rgba[2]),
        a: f64::from(rgba[3]) * coverage,
    }
    .premultiply()
    .over(dst)
}

/// Write one row of the accumulator as linear-light straight-alpha `Rgba16F`.
fn write_row(acc: &[PremulRgba], out: &mut [u8]) {
    for (px, dst) in acc.iter().zip(out.as_chunks_mut::<8>().0) {
        let lin = px.unpremultiply();
        for (k, v) in [lin.r, lin.g, lin.b, lin.a].into_iter().enumerate() {
            let bits = fmn_frame::half::f16_from_f32(v as f32);
            dst[k * 2..k * 2 + 2].copy_from_slice(&bits.to_le_bytes());
        }
    }
}

/// Standard-mode writeback from the f32 premultiplied accumulator.
fn write_row_f32(acc: &[PremulRgba32], out: &mut [u8]) {
    for (px, dst) in acc.iter().zip(out.as_chunks_mut::<8>().0) {
        let rgba = if px.a == 0.0 {
            [0.0, 0.0, 0.0, 0.0]
        } else {
            [px.r / px.a, px.g / px.a, px.b / px.a, px.a]
        };
        for (k, value) in rgba.into_iter().enumerate() {
            let bits = fmn_frame::half::f16_from_f32(value);
            dst[k * 2..k * 2 + 2].copy_from_slice(&bits.to_le_bytes());
        }
    }
}

/// Does the pixel row `[py, py+1]` lie entirely outside a slab?
///
/// Rejection is exact, not conservative-in-the-useful-direction: outside the
/// hull a closed path's winding is zero and a stroke's distance exceeds its
/// padded slab, so both passes would compute a coverage of zero and composite
/// the identity. `the_engine_matches_a_brute_force_reference` is what holds that
/// claim to bytes.
fn row_misses(slab: [f64; 4], py: u32) -> bool {
    let lo = f64::from(py);
    slab[3] <= lo || slab[1] >= lo + 1.0
}

/// A prepared segment run's screen-space control-hull AABB.
fn hull_slab(segments: &[Segment], map: ScreenMap, translate: [f64; 2]) -> [f64; 4] {
    if segments.is_empty() {
        return [0.0; 4];
    }
    let to_px = |p: fmn_core::types::Vec3| {
        [
            map.origin[0] + p[0] * map.scale + translate[0],
            map.origin[1] + p[1] * map.scale + translate[1],
        ]
    };
    let mut slab = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    for point in segments
        .iter()
        .flat_map(|segment| [segment.p0, segment.p1, segment.p2])
    {
        let pixel = to_px(point);
        slab[0] = slab[0].min(pixel[0]);
        slab[1] = slab[1].min(pixel[1]);
        slab[2] = slab[2].max(pixel[0]);
        slab[3] = slab[3].max(pixel[1]);
    }
    slab
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bin::covers_tile;
    use crate::fill::MonoTable;
    use fmn_mobject::{Mob, Mobject, Placement, RecordBuffer, RecordSchema, ShapeTag, Stage};

    /// A vmobject with a filled and/or stroked style written across its records.
    ///
    /// Colours are written as the record buffer holds them — **sRGB-encoded**,
    /// the way `mobject.data` presents them — so these fixtures exercise
    /// `read_style`'s decode rather than sneaking past it.
    fn vmob(points: &[[f64; 3]], fill: [f32; 4], stroke: [f32; 4], width: f32) -> Mobject {
        let mut buffer = RecordBuffer::new(RecordSchema::vmobject(), points.len());
        for (i, p) in points.iter().enumerate() {
            buffer.write(i, "point", &[p[0] as f32, p[1] as f32, p[2] as f32]);
            buffer.write(i, "fill_rgba", &fill);
            buffer.write(i, "stroke_rgba", &stroke);
            buffer.write(i, "stroke_width", &[width]);
        }
        Mobject::from_buffer(buffer)
    }

    /// A closed quadratic ring approximating a circle, as a shared-anchor point run.
    fn ring(cx: f64, cy: f64, r: f64, n: usize, ccw: bool) -> Vec<[f64; 3]> {
        let mut out = Vec::with_capacity(2 * n + 1);
        let step = std::f64::consts::TAU / n as f64;
        let sgn = if ccw { 1.0 } else { -1.0 };
        // The handle sits at the tangent intersection, which is the radius
        // scaled by sec(step/2) — the same construction QuadPath::arc uses.
        let sec = 1.0 / fmn_dmath::cos(step / 2.0);
        for k in 0..n {
            let a0 = sgn * step * k as f64;
            let a1 = sgn * step * (k as f64 + 0.5);
            let a2 = sgn * step * (k as f64 + 1.0);
            let p = |a: f64, rad: f64| {
                [
                    cx + rad * fmn_dmath::cos(a),
                    cy + rad * fmn_dmath::sin(a),
                    0.0,
                ]
            };
            if k == 0 {
                out.push(p(a0, r));
            }
            out.push(p(a1, r * sec));
            out.push(p(a2, r));
        }
        out
    }

    /// A closed axis-aligned rectangle as a shared-anchor point run: four
    /// straight quadratics, handles at the edge midpoints, `2n + 1` points.
    fn rect_points(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<[f64; 3]> {
        let corners = [[x0, y0], [x1, y0], [x1, y1], [x0, y1], [x0, y0]];
        let mut out = vec![[corners[0][0], corners[0][1], 0.0]];
        for pair in corners.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            out.push([0.5 * (a[0] + b[0]), 0.5 * (a[1] + b[1]), 0.0]);
            out.push([b[0], b[1], 0.0]);
        }
        out
    }

    /// The engine's corpus scene: overlapping translucent fills, a winding hole,
    /// an open stroke with round caps, a hairline finer than the AA band, and a
    /// four-boundary dense-stem cell.
    fn corpus() -> (Stage, Vec<Mob>) {
        let mut stage = Stage::new();
        let mut mobs = Vec::new();
        let mut add = |stage: &mut Stage, m: Mobject| {
            let h = stage.add(m);
            stage.add_to_scene(h).expect("live");
            mobs.push(h);
        };

        // A translucent filled disc.
        add(
            &mut stage,
            vmob(
                &ring(26.0, 30.0, 16.0, 8, true),
                [1.0, 0.0, 0.0, 0.7],
                [0.0; 4],
                0.0,
            ),
        );
        // A second one overlapping it, so the composite order is observable.
        add(
            &mut stage,
            vmob(
                &ring(38.0, 34.0, 14.0, 8, true),
                [0.0, 1.0, 0.0, 0.5],
                [0.0; 4],
                0.0,
            ),
        );
        // An opaque disc with a clockwise hole: the nonzero rule must leave it.
        let outer = ring(72.0, 32.0, 20.0, 8, true);
        let inner = ring(72.0, 32.0, 9.0, 8, false);
        let mut annulus_path = fmn_geom::quadpath::QuadPath::new();
        annulus_path.add_subpath(&outer).expect("outer ring");
        annulus_path.add_subpath(&inner).expect("inner ring");
        add(
            &mut stage,
            vmob(annulus_path.points(), [1.0, 1.0, 0.0, 1.0], [0.0; 4], 0.0),
        );
        // An open stroked polyline with a sharp corner.
        add(
            &mut stage,
            vmob(
                &[
                    [8.0, 60.0, 0.0],
                    [22.0, 60.0, 0.0],
                    [36.0, 60.0, 0.0],
                    [36.0, 74.0, 0.0],
                    [36.0, 88.0, 0.0],
                ],
                [0.0; 4],
                [1.0, 1.0, 1.0, 1.0],
                600.0,
            ),
        );
        // A hairline finer than the AA band.
        add(
            &mut stage,
            vmob(
                &[[6.0, 96.0, 0.0], [48.0, 100.0, 0.0], [90.0, 96.0, 0.0]],
                [0.0; 4],
                [0.0, 0.5, 1.0, 1.0],
                40.0,
            ),
        );
        // Two subpixel stems in one shape: four independent boundary sheets in
        // each occupied cell, so the 4× threshold is exercised rather than
        // merely declared.
        let left_stem = rect_points(104.1, 52.0, 104.2, 86.0);
        let right_stem = rect_points(104.5, 52.0, 104.6, 86.0);
        let mut dense_stems = fmn_geom::quadpath::QuadPath::new();
        dense_stems
            .add_subpath(&left_stem)
            .expect("left dense stem");
        dense_stems
            .add_subpath(&right_stem)
            .expect("right dense stem");
        add(
            &mut stage,
            vmob(dense_stems.points(), [1.0, 0.2, 0.8, 1.0], [0.0; 4], 0.0),
        );
        (stage, mobs)
    }

    fn config() -> FrameConfig {
        FrameConfig::new(
            Viewport {
                width: 112,
                height: 112,
            },
            ScreenMap {
                scale: 1.0,
                origin: [0.0, 0.0],
            },
            LinearRgba {
                r: 0.04,
                g: 0.04,
                b: 0.06,
                a: 1.0,
            },
        )
    }

    /// Compile the three derived structures a frame needs.
    fn derive(stage: &Stage, cfg: FrameConfig, tiling: Tiling) -> (RenderPlan, MonoTable, Binning) {
        let mut plan = RenderPlan::new();
        plan.sync(stage, 0);
        let mono = MonoTable::build(&plan, cfg.map);
        let binning = Binning::build(&plan, cfg.viewport, tiling, cfg.map);
        (plan, mono, binning)
    }

    fn default_tiling() -> Tiling {
        Tiling {
            macro_tile: 64,
            fine_tile: 16,
        }
    }

    /// The brute-force reference: the same tiling and the same classification,
    /// but **every instance considered for every tile** and no slab rejection.
    ///
    /// This is the test the rest of the engine's optimizations are measured
    /// against. Binning, its two levels, its per-command class word, and the
    /// per-row/per-column slab rejects all claim to remove only work that could
    /// not have changed a pixel; this recomputes the frame with none of them and
    /// compares bytes.
    fn render_reference(job: &FrameJob<'_>) -> FrameBuffer {
        let cfg = job.config;
        let tile = job.binning.tiling().fine_tile;
        let mut buffer = FrameBuffer::new(cfg.layout().expect("layout"));
        let stride = buffer.layout().stride(0);
        let bg = cfg.background.premultiply();
        let mut scratch = RowScratch::for_tile(tile);
        let mut cov = vec![0.0f64; tile as usize];

        let rows = cfg.viewport.height.div_ceil(tile);
        for ty in 0..rows {
            for tx in 0..job.cols {
                let x_lo = tx * tile;
                let x_hi = (x_lo + tile).min(cfg.viewport.width);
                let rect = [
                    f64::from(x_lo),
                    f64::from(ty * tile),
                    f64::from(x_hi),
                    f64::from((ty * tile + tile).min(cfg.viewport.height)),
                ];
                let w = (x_hi - x_lo) as usize;
                for py in (ty * tile)..((ty * tile + tile).min(cfg.viewport.height)) {
                    let mut acc = vec![bg; w];
                    for (d, rec) in job.draws.iter().enumerate() {
                        // The draw list is index-aligned with the instance list,
                        // so the enumeration index IS the instance index — which
                        // is exactly the property the engine relies on and this
                        // reference therefore must not paper over.
                        let Some(rec) = rec.as_ref() else { continue };
                        let inst = &job.plan.shapes().instances()[d];
                        let interior = covers_tile(job.plan, inst, cfg.map, rect);
                        let mut fill = |acc: &mut Vec<PremulRgba>| {
                            if !rec.draws_fill {
                                return;
                            }
                            if interior {
                                cov[..w].copy_from_slice(scratch.interior_row(x_lo, x_hi));
                            } else if !rec.kernel.row(py, x_lo, x_hi, &mut cov[..w]) {
                                cov[..w].copy_from_slice(scratch.fill_row(
                                    job.pieces_of(rec),
                                    rec.translate,
                                    py,
                                    x_lo,
                                    x_hi,
                                ));
                            }
                            let segments = job.segments_of(rec);
                            for i in 0..w {
                                if cov[i] <= 0.0 {
                                    continue;
                                }
                                let p = [f64::from(x_lo + i as u32) + 0.5, f64::from(py) + 0.5];
                                let rgba = match rec.flat_fill {
                                    Some(c) => c,
                                    None => fill_rgba_with_border(
                                        &rec.style,
                                        rec.field.as_ref().expect("field"),
                                        segments,
                                        cfg.map,
                                        rec.translate,
                                        p,
                                    ),
                                };
                                acc[i] = source_over(rgba, cov[i], acc[i]);
                            }
                        };
                        let stroke = |acc: &mut Vec<PremulRgba>| {
                            if !rec.draws_stroke {
                                return;
                            }
                            let segments = job.segments_of(rec);
                            for (i, dst) in acc.iter_mut().enumerate().take(w) {
                                let p = [f64::from(x_lo + i as u32) + 0.5, f64::from(py) + 0.5];
                                let (c, s) = stroke_shade(
                                    segments,
                                    &rec.joins,
                                    &rec.style,
                                    cfg.map,
                                    rec.translate,
                                    p,
                                );
                                if c <= 0.0 {
                                    continue;
                                }
                                *dst = source_over(stroke_rgba_at(&rec.style, s), c, *dst);
                            }
                        };
                        if rec.style.stroke_behind {
                            stroke(&mut acc);
                            fill(&mut acc);
                        } else {
                            fill(&mut acc);
                            stroke(&mut acc);
                        }
                    }
                    let base = py as usize * stride + x_lo as usize * 8;
                    write_row(&acc, &mut buffer.plane_mut(0)[base..base + w * 8]);
                }
            }
        }
        buffer
    }

    #[test]
    fn the_draw_list_is_index_aligned_with_the_instance_list() {
        // Stated as its own assertion because it is a *representation* invariant
        // the whole tile loop rests on, and because the first version of this
        // module compacted the list and was wrong for it.
        let mut stage = Stage::new();
        let ghost = stage.add(vmob(
            &ring(40.0, 40.0, 10.0, 8, true),
            [1.0, 1.0, 1.0, 0.0],
            [0.0; 4],
            0.0,
        ));
        stage.add_to_scene(ghost).expect("live");
        let solid = stage.add(vmob(
            &ring(70.0, 40.0, 10.0, 8, true),
            [1.0, 0.0, 0.0, 1.0],
            [0.0; 4],
            0.0,
        ));
        stage.add_to_scene(solid).expect("live");

        let cfg = config();
        let (plan, mono, binning) = derive(&stage, cfg, default_tiling());
        let job = FrameJob::new(&plan, &mono, &binning, cfg).expect("matching frame artifacts");
        assert_eq!(job.draws.len(), plan.shapes().instances().len());
        assert_eq!(job.draws.len(), 2);
        assert!(job.draws[0].is_none(), "the ghost must hold its slot");
        assert!(job.draws[1].is_some());
        assert_eq!(job.draw_count(), 1, "and must not be counted as a draw");
    }

    #[test]
    fn affine_instance_render_matches_the_same_geometry_baked_to_points() {
        let make_stage = |bake: bool| {
            let mut stage = Stage::new();
            let points = rect_points(36.0, 44.0, 76.0, 68.0);
            let mob = stage.add(vmob(
                &points,
                [0.2, 0.7, 0.9, 0.8],
                [1.0, 0.3, 0.1, 1.0],
                240.0,
            ));
            let last = points.len() - 1;
            let entry = stage.get_mut(mob).expect("live");
            entry.buffer.write(last, "fill_rgba", &[0.9, 0.2, 0.4, 0.8]);
            entry
                .buffer
                .write(last, "stroke_rgba", &[0.1, 0.8, 0.3, 1.0]);
            entry.buffer.write(last, "stroke_width", &[480.0]);
            stage.add_to_scene(mob).expect("live");
            let pivot = [56.0, 56.0, 0.0];
            stage.apply_affine(
                mob,
                Placement::about([[0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]], pivot),
            );
            stage.stretch_about(mob, 1.5, 0, Some(pivot), None);
            stage.stretch_about(mob, 0.75, 1, Some(pivot), None);
            if bake {
                stage.bake_placement(mob).expect("live");
            }
            stage
        };
        let placed = make_stage(false);
        let baked = make_stage(true);
        let cfg = config();

        let (placed_plan, placed_mono, placed_binning) = derive(&placed, cfg, default_tiling());
        let placed_frame = FrameJob::new(&placed_plan, &placed_mono, &placed_binning, cfg)
            .expect("placed artifacts")
            .render(1)
            .expect("placed render");
        let (baked_plan, baked_mono, baked_binning) = derive(&baked, cfg, default_tiling());
        let baked_frame = FrameJob::new(&baked_plan, &baked_mono, &baked_binning, cfg)
            .expect("baked artifacts")
            .render(1)
            .expect("baked render");

        assert_frames_equal(
            &placed_frame,
            &baked_frame,
            "affine placement diverged from the same world-space geometry",
        );
    }

    #[test]
    fn the_engine_matches_a_brute_force_reference() {
        // The load-bearing test of the whole module. Two-level binning, the
        // per-command interior class, the per-row slab reject and the per-column
        // one all claim to remove only work that could not have moved a pixel.
        // Here is the frame with none of them, byte for byte.
        let (stage, _) = corpus();
        let cfg = config();
        let (plan, mono, binning) = derive(&stage, cfg, default_tiling());
        let job = FrameJob::new(&plan, &mono, &binning, cfg).expect("matching frame artifacts");
        let fast = job.render(1).expect("render");
        let reference = render_reference(&job);
        assert_frames_equal(&fast, &reference, "an optimization changed the picture");
    }

    #[test]
    fn the_frame_is_identical_at_every_thread_count() {
        // PG-5, per commit. Thread count is outside the input closure (§16.7)
        // and this is the assertion that makes that true rather than hoped.
        let (stage, _) = corpus();
        let cfg = config();
        let (plan, mono, binning) = derive(&stage, cfg, default_tiling());
        let job = FrameJob::new(&plan, &mono, &binning, cfg).expect("matching frame artifacts");
        let one = job.render(1).expect("render");
        for threads in [2usize, 4, 16] {
            let many = job.render(threads).expect("render");
            assert_eq!(
                one.as_bytes(),
                many.as_bytes(),
                "the frame moved at {threads} threads"
            );
        }
    }

    #[test]
    fn certified_bits_ignore_adaptive_and_forced_aa_policies() {
        // §10.4 is absolute: certified executes the canonical analytic path.
        // The A/B knob remains journaled, but it cannot move a raw-frame bit.
        let (stage, _) = corpus();
        let tiling = default_tiling();
        let mut definition = None;
        for aa in [AaPolicy::Adaptive, AaPolicy::Ssaa2x, AaPolicy::Ssaa4x] {
            let cfg = config().with_aa_policy(aa);
            let (plan, mono, binning) = derive(&stage, cfg, tiling);
            let job = FrameJob::new(&plan, &mono, &binning, cfg).expect("matching frame artifacts");
            let (frame, stats) = job.render_with_stats(4).expect("render");
            assert_eq!(stats.native_cells, stats.output_cells);
            assert_eq!(stats.ssaa2x_cells, 0);
            assert_eq!(stats.ssaa4x_cells, 0);
            match &definition {
                Some(bytes) => {
                    assert_eq!(bytes, frame.as_bytes(), "certified bits moved under {aa:?}")
                }
                None => definition = Some(frame.as_bytes().to_vec()),
            }
        }
    }

    #[test]
    fn adaptive_classification_is_thread_independent_and_avoids_forced_work() {
        let (stage, _) = corpus();
        let tiling = default_tiling();
        let identity = EngineIdentity {
            engine: EngineKind::FastCpu,
            ..EngineIdentity::certified()
        };

        let adaptive_cfg = config().with_aa_policy(AaPolicy::Adaptive);
        let (plan, mono, binning) = derive(&stage, adaptive_cfg, tiling);
        let job = FrameJob::with_identity(&plan, &mono, &binning, adaptive_cfg, identity)
            .expect("matching frame artifacts");
        let (one, one_stats) = job.render_with_stats(1).expect("render");
        let (many, many_stats) = job.render_with_stats(4).expect("render");
        assert_frames_equal(&one, &many, "adaptive output depends on thread count");
        assert_eq!(one_stats, many_stats, "adaptive stats depend on workers");
        assert_eq!(one_stats.classified_tiles(), binning.tile_count() as u64);
        assert!(
            one_stats.complex_edge_tiles > 0,
            "the overlap corpus classified no complex tile: {one_stats:?}"
        );
        assert!(
            one_stats.ssaa2x_cells > 0 && one_stats.ssaa4x_cells > 0,
            "both adaptive thresholds must be exercised: {one_stats:?}"
        );
        assert_eq!(
            (one_stats.ssaa2x_cells, one_stats.ssaa4x_cells),
            (29, 34),
            "the documented W5 work measurement moved"
        );
        assert_eq!(
            one_stats.sample_evaluations(),
            13_204,
            "the documented W5 work measurement moved"
        );
        assert!(
            one_stats.work_reduction_vs_forced_4x() > 0.5,
            "adaptive did not avoid most forced work: {one_stats:?}"
        );

        let forced_cfg = config().with_aa_policy(AaPolicy::Ssaa4x);
        let forced_job = FrameJob::with_identity(&plan, &mono, &binning, forced_cfg, identity)
            .expect("AA policy does not stale geometry");
        let (_, forced_stats) = forced_job.render_with_stats(4).expect("render");
        assert_eq!(
            forced_stats.sample_evaluations(),
            forced_stats.forced_4x_evaluations()
        );
        assert!(
            one_stats.sample_evaluations() < forced_stats.sample_evaluations(),
            "{one_stats:?} vs {forced_stats:?}"
        );

        let forced_2x_cfg = config().with_aa_policy(AaPolicy::Ssaa2x);
        let forced_2x_job =
            FrameJob::with_identity(&plan, &mono, &binning, forced_2x_cfg, identity)
                .expect("AA policy does not stale geometry");
        let (_, forced_2x_stats) = forced_2x_job.render_with_stats(4).expect("render");
        assert_eq!(forced_2x_stats.native_cells, 0);
        assert_eq!(forced_2x_stats.ssaa2x_cells, forced_2x_stats.output_cells);
        assert_eq!(
            forced_2x_stats.sample_evaluations(),
            forced_2x_stats.output_cells * 4
        );
    }

    #[test]
    fn strokes_inside_two_aa_bands_do_not_promote_complex_cells() {
        let map = ScreenMap {
            scale: 1.0,
            origin: [0.0, 0.0],
        };
        let style = Style {
            stroke_width: 299.0,
            stroke_width_end: 299.0,
            anti_alias_width: 1.5,
            ..Style::default()
        };
        assert!(!stroke_contributes_complexity(&style, map, 0.5));

        let wide = Style {
            stroke_width: 301.0,
            stroke_width_end: 301.0,
            ..style
        };
        assert!(stroke_contributes_complexity(&wide, map, 0.5));
    }

    #[test]
    fn only_width_eligible_overlapping_strokes_escalate() {
        let render_stats = |width| {
            let mut stage = Stage::new();
            for colour in [[1.0, 0.2, 0.1, 1.0], [0.1, 0.4, 1.0, 0.8]] {
                let stroke = vmob(
                    &[[18.0, 40.0, 0.0], [40.0, 40.0, 0.0], [62.0, 40.0, 0.0]],
                    [0.0; 4],
                    colour,
                    width,
                );
                let h = stage.add(stroke);
                stage.add_to_scene(h).expect("live");
            }
            let cfg = config().with_aa_policy(AaPolicy::Adaptive);
            let (plan, mono, binning) = derive(&stage, cfg, default_tiling());
            let identity = EngineIdentity {
                engine: EngineKind::FastCpu,
                ..EngineIdentity::certified()
            };
            FrameJob::with_identity(&plan, &mono, &binning, cfg, identity)
                .expect("matching frame artifacts")
                .render_with_stats(1)
                .expect("render")
                .1
        };

        let inside_band = render_stats(299.0);
        assert_eq!(inside_band.ssaa2x_cells, 0, "{inside_band:?}");
        assert_eq!(inside_band.ssaa4x_cells, 0, "{inside_band:?}");

        let eligible = render_stats(301.0);
        assert!(eligible.ssaa2x_cells > 0, "{eligible:?}");
    }

    #[test]
    fn the_four_tile_classes_are_observable() {
        let mut stage = Stage::new();
        for points in [
            rect_points(2.25, 2.25, 50.25, 50.25),
            ring(72.0, 76.0, 17.0, 8, true),
            ring(84.0, 76.0, 17.0, 8, true),
        ] {
            let h = stage.add(vmob(&points, [1.0, 1.0, 1.0, 1.0], [0.0; 4], 0.0));
            stage.add_to_scene(h).expect("live");
        }
        let cfg = config().with_aa_policy(AaPolicy::Adaptive);
        let (plan, mono, binning) = derive(&stage, cfg, default_tiling());
        let identity = EngineIdentity {
            engine: EngineKind::FastCpu,
            ..EngineIdentity::certified()
        };
        let (_, stats) = FrameJob::with_identity(&plan, &mono, &binning, cfg, identity)
            .expect("matching frame artifacts")
            .render_with_stats(1)
            .expect("render");
        assert_eq!(stats.classified_tiles(), binning.tile_count() as u64);
        assert!(
            stats.empty_tiles > 0
                && stats.fully_covered_tiles > 0
                && stats.simple_edge_tiles > 0
                && stats.complex_edge_tiles > 0,
            "not every §10.4 class was reached: {stats:?}"
        );
    }

    #[test]
    fn adaptive_quality_stays_inside_the_forced_4x_visual_budget_v1() {
        let (stage, _) = corpus();
        let tiling = default_tiling();
        let identity = EngineIdentity {
            engine: EngineKind::FastCpu,
            ..EngineIdentity::certified()
        };
        let render = |aa| {
            let cfg = config().with_aa_policy(aa);
            let (plan, mono, binning) = derive(&stage, cfg, tiling);
            FrameJob::with_identity(&plan, &mono, &binning, cfg, identity)
                .expect("matching frame artifacts")
                .render(4)
                .expect("render")
        };
        let adaptive = render(AaPolicy::Adaptive);
        let forced = render(AaPolicy::Ssaa4x);
        let (maximum, rms) = visual_error(&adaptive, &forced);
        assert!(
            maximum <= AA_VISUAL_BUDGET_V1_MAX_CHANNEL_ERROR,
            "max={maximum}, budget={AA_VISUAL_BUDGET_V1_MAX_CHANNEL_ERROR}"
        );
        assert!(
            rms <= AA_VISUAL_BUDGET_V1_RMS_CHANNEL_ERROR,
            "rms={rms}, budget={AA_VISUAL_BUDGET_V1_RMS_CHANNEL_ERROR}"
        );
    }

    #[test]
    fn fast_cpu_stays_inside_the_certified_visual_budget_v1() {
        let (stage, _) = corpus();
        let cfg = config().with_aa_policy(AaPolicy::Adaptive);
        let (plan, mono, binning) = derive(&stage, cfg, default_tiling());
        let certified =
            FrameJob::with_identity(&plan, &mono, &binning, cfg, EngineIdentity::certified())
                .expect("matching certified artifacts")
                .render(4)
                .expect("certified render");
        let fast = FrameJob::with_identity(&plan, &mono, &binning, cfg, EngineIdentity::fast())
            .expect("matching fast artifacts")
            .render(4)
            .expect("fast render");
        let (maximum, rms) = visual_error(&fast, &certified);
        assert!(
            maximum <= FAST_VISUAL_BUDGET_V1_MAX_CHANNEL_ERROR,
            "max={maximum}, budget={FAST_VISUAL_BUDGET_V1_MAX_CHANNEL_ERROR}"
        );
        assert!(
            rms <= FAST_VISUAL_BUDGET_V1_RMS_CHANNEL_ERROR,
            "rms={rms}, budget={FAST_VISUAL_BUDGET_V1_RMS_CHANNEL_ERROR}"
        );
    }

    #[test]
    fn an_invisible_instance_does_not_shift_the_ones_behind_it() {
        // Binning's command lists hold **instance** indices, so anything the
        // engine indexes with one has to be aligned with the instance list. An
        // instance with no visible pass — zero fill alpha and zero stroke width,
        // which `set_opacity(0)` and a bare `Group` both produce — must therefore
        // still occupy its slot.
        //
        // The failure this pins is not subtle: with the draw list compacted, a
        // scene whose first object is invisible renders its second object's
        // geometry in its third object's colour, and drops the last one
        // entirely. Every corpus frame happened to have a visible pass on every
        // instance, which is exactly why a scene that does not is the test.
        let build = |with_ghost: bool| {
            let mut stage = Stage::new();
            if with_ghost {
                // Fully transparent fill, zero stroke: composites as the identity.
                let ghost = stage.add(vmob(
                    &ring(56.0, 56.0, 30.0, 8, true),
                    [1.0, 1.0, 1.0, 0.0],
                    [0.0; 4],
                    0.0,
                ));
                stage.add_to_scene(ghost).expect("live");
            }
            for (cx, colour) in [
                (30.0, [1.0f32, 0.0, 0.0, 1.0]),
                (60.0, [0.0, 0.0, 1.0, 1.0]),
                (90.0, [0.0, 1.0, 0.0, 1.0]),
            ] {
                let m = stage.add(vmob(&ring(cx, 40.0, 12.0, 8, true), colour, [0.0; 4], 0.0));
                stage.add_to_scene(m).expect("live");
            }
            let cfg = config();
            let (plan, mono, binning) = derive(&stage, cfg, default_tiling());
            FrameJob::new(&plan, &mono, &binning, cfg)
                .expect("matching frame artifacts")
                .render(1)
                .expect("render")
        };
        let without = build(false);
        let with = build(true);
        // The ghost draws nothing, so adding it cannot change one pixel.
        assert_frames_equal(&with, &without, "an invisible instance changed the frame");
        // And the three discs must actually be their own colours, which is what
        // says the alignment is right rather than uniformly wrong.
        for (cx, expect) in [(30u32, 0usize), (60, 2), (90, 1)] {
            let px = read_px(&with, cx, 40);
            assert!(
                px[expect] > 0.9,
                "the disc at x={cx} is not its own colour: {px:?}"
            );
        }
    }

    #[test]
    fn occlusion_pruning_is_byte_exact() {
        // G0-8b's finding F13: a tile whose earlier commands are provably hidden
        // must produce the same bytes whether they were drawn or skipped. It is
        // only true because the interior class is exactly 1.0 and not an
        // accumulation an ulp below it.
        let mut stage = Stage::new();
        // A small opaque square, then a larger opaque one drawn over it: inside
        // the overlap the first is provably hidden and may be skipped.
        for (rect, colour) in [
            ([24.0, 24.0, 60.0, 60.0], [1.0f32, 0.0, 0.0, 1.0]),
            ([8.0, 8.0, 96.0, 96.0], [0.0, 0.0, 1.0, 1.0]),
        ] {
            let m = vmob(
                &rect_points(rect[0], rect[1], rect[2], rect[3]),
                colour,
                [0.0; 4],
                0.0,
            );
            let h = stage.add(m);
            stage.add_to_scene(h).expect("live");
        }
        let cfg = config();
        let (plan, mono, mut binning) = derive(&stage, cfg, default_tiling());
        let unpruned = FrameJob::new(&plan, &mono, &binning, cfg)
            .expect("matching frame artifacts")
            .render(1)
            .expect("render");
        let report = binning.prune_occluded(&plan).expect("matching plan");
        let pruned = FrameJob::new(&plan, &mono, &binning, cfg)
            .expect("matching frame artifacts")
            .render(1)
            .expect("render");
        assert!(report.before > report.after, "nothing was pruned to test");
        assert_eq!(unpruned.as_bytes(), pruned.as_bytes());
    }

    #[test]
    fn tile_size_is_a_scheduling_choice_within_the_fills_own_tolerance() {
        // Tiling cannot change the picture — but the fill's per-tile carry is a
        // different *association* of the same sum than a longer row's prefix, so
        // the invariance is to floating-point tolerance and not to bits. That is
        // exactly why C10 puts the tile dimensions in the declared certified
        // configuration: a certified run pins them rather than relying on an
        // invariance the arithmetic does not owe.
        let (stage, _) = corpus();
        let cfg = config();
        let reference = {
            let (plan, mono, binning) = derive(&stage, cfg, default_tiling());
            FrameJob::new(&plan, &mono, &binning, cfg)
                .expect("matching frame artifacts")
                .render(1)
                .expect("render")
        };
        for fine in [8u32, 32, 56] {
            let tiling = Tiling {
                macro_tile: 112,
                fine_tile: fine,
            };
            let (plan, mono, binning) = derive(&stage, cfg, tiling);
            let got = FrameJob::new(&plan, &mono, &binning, cfg)
                .expect("matching frame artifacts")
                .render(1)
                .expect("render");
            let differing = reference
                .as_bytes()
                .iter()
                .zip(got.as_bytes())
                .filter(|(a, b)| a != b)
                .count();
            // A handful of f16 codes may land on either side of a rounding
            // boundary; a structural difference would move thousands.
            assert!(
                differing * 200 < reference.as_bytes().len(),
                "tile {fine}: {differing} of {} bytes differ",
                reference.as_bytes().len()
            );
        }
    }

    #[test]
    fn a_retained_plan_and_a_fresh_one_render_the_same_frame() {
        // ADR-0013's pinning, as an experiment rather than an assurance.
        //
        // The experiment has to make the interned indices actually diverge, and
        // the obvious way does not: `RenderPlan::sync` walks the draw plan in
        // painter order and interns as it goes, so on a *fresh* plan the
        // interning order IS painter order and nothing varies. Indices diverge
        // when a plan is **retained** and an object is added behind ones already
        // compiled — the newcomer draws first and interns last. That is also the
        // normal path, so it is the one worth pinning.
        let scene = |stage: &mut Stage| {
            for (cx, colour) in [
                (34.0, [1.0f32, 0.0, 0.0, 0.8]),
                (60.0, [0.0, 0.0, 1.0, 0.6]),
            ] {
                let h = stage.add(vmob(&ring(cx, 40.0, 15.0, 8, true), colour, [0.0; 4], 0.0));
                stage.add_to_scene(h).expect("live");
            }
        };
        let newcomer = |stage: &mut Stage| {
            let h = stage.add(vmob(
                &ring(46.0, 62.0, 19.0, 8, true),
                [0.0, 1.0, 0.0, 0.7],
                [0.0; 4],
                0.0,
            ));
            stage.set_z_index(h, -5, false);
            stage.add_to_scene(h).expect("live");
        };

        let cfg = config();

        // Retained: two objects compiled, then a third inserted *behind* them.
        let mut retained_stage = Stage::new();
        scene(&mut retained_stage);
        let mut retained = RenderPlan::new();
        retained.sync(&retained_stage, 0);
        newcomer(&mut retained_stage);
        retained.sync(&retained_stage, 0);

        // Fresh: the same final scene, compiled in one pass.
        let mut fresh_stage = Stage::new();
        scene(&mut fresh_stage);
        newcomer(&mut fresh_stage);
        let mut fresh = RenderPlan::new();
        fresh.sync(&fresh_stage, 0);

        // The precondition, asserted rather than assumed: the two plans really
        // do assign different table indices to the same painter position. Without
        // this the test could pass by comparing a scene with itself.
        let idx = |p: &RenderPlan| -> Vec<u32> {
            p.shapes().instances().iter().map(|i| i.shape).collect()
        };
        assert_eq!(idx(&retained).len(), 3);
        assert_ne!(
            idx(&retained),
            idx(&fresh),
            "the interning orders did not diverge, so this proves nothing"
        );

        let render = |plan: &RenderPlan| {
            let mono = MonoTable::build(plan, cfg.map);
            let binning = Binning::build(plan, cfg.viewport, default_tiling(), cfg.map);
            FrameJob::new(plan, &mono, &binning, cfg)
                .expect("matching frame artifacts")
                .render(1)
                .expect("render")
        };
        assert_frames_equal(
            &render(&retained),
            &render(&fresh),
            "the interning order reached a pixel",
        );
    }

    #[test]
    fn painter_order_decides_which_colour_wins() {
        let mut stage = Stage::new();
        let mut handles = Vec::new();
        for colour in [[1.0f32, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0]] {
            let m = vmob(&ring(40.0, 40.0, 20.0, 8, true), colour, [0.0; 4], 0.0);
            let h = stage.add(m);
            stage.add_to_scene(h).expect("live");
            handles.push(h);
        }
        let cfg = config();
        let (plan, mono, binning) = derive(&stage, cfg, default_tiling());
        let frame = FrameJob::new(&plan, &mono, &binning, cfg)
            .expect("matching frame artifacts")
            .render(1)
            .expect("render");
        // Dead centre: the second disc, drawn last and opaque, wins outright.
        let px = read_px(&frame, 40, 40);
        assert!(px[0] < 1e-3, "red leaked through an opaque green: {px:?}");
        assert!((px[1] - 1.0).abs() < 1e-3, "green is not opaque: {px:?}");
        assert!((px[3] - 1.0).abs() < 1e-3);
    }

    #[test]
    fn the_background_survives_where_nothing_draws() {
        let (stage, _) = corpus();
        let cfg = config();
        let (plan, mono, binning) = derive(&stage, cfg, default_tiling());
        let frame = FrameJob::new(&plan, &mono, &binning, cfg)
            .expect("matching frame artifacts")
            .render(1)
            .expect("render");
        let px = read_px(&frame, 108, 4);
        assert!((px[0] - 0.04).abs() < 2e-3, "{px:?}");
        assert!((px[2] - 0.06).abs() < 2e-3, "{px:?}");
        assert!((px[3] - 1.0).abs() < 1e-3, "{px:?}");
    }

    #[test]
    fn stroke_behind_swaps_the_passes() {
        // R-5: within one object the fill draws before the stroke, unless
        // `stroke_behind`. A wide stroke under a translucent fill reads
        // differently from the same stroke over it, and that difference is the
        // whole observable content of the flag.
        let make = |behind: bool| {
            let mut stage = Stage::new();
            let m = vmob(
                &ring(40.0, 40.0, 18.0, 8, true),
                [1.0, 1.0, 1.0, 0.5],
                [1.0, 0.0, 0.0, 1.0],
                800.0,
            );
            let h = stage.add(m);
            stage.set_stroke_behind(h, behind, false);
            stage.add_to_scene(h).expect("live");
            let cfg = config();
            let (plan, mono, binning) = derive(&stage, cfg, default_tiling());
            let frame = FrameJob::new(&plan, &mono, &binning, cfg)
                .expect("matching frame artifacts")
                .render(1)
                .expect("render");
            read_px(&frame, 40, 22)
        };
        let over = make(false);
        let behind = make(true);
        assert!(
            (over[1] - behind[1]).abs() > 0.05,
            "stroke_behind changed nothing: {over:?} vs {behind:?}"
        );
        // With the stroke on top the red is undiluted; behind, the translucent
        // white fill lightens it.
        assert!(over[1] < behind[1], "{over:?} vs {behind:?}");
    }

    #[test]
    fn a_pooled_buffer_renders_the_same_frame_as_a_fresh_one() {
        // PG-6's steady state: the engine writes every payload byte it is
        // responsible for, so a buffer carrying a previous frame's contents must
        // not leak any of them.
        let (stage, _) = corpus();
        let cfg = config();
        let (plan, mono, binning) = derive(&stage, cfg, default_tiling());
        let job = FrameJob::new(&plan, &mono, &binning, cfg).expect("matching frame artifacts");
        let fresh = job.render(1).expect("render");
        let mut dirty = FrameBuffer::new(cfg.layout().expect("layout"));
        dirty.as_bytes_mut().fill(0xa5);
        job.render_into(4, &mut dirty).expect("render");
        assert_eq!(fresh.as_bytes(), dirty.as_bytes());
    }

    #[test]
    fn the_frame_digest_ignores_stride_padding() {
        // A pooled frame carries whatever the last one left in its padding.
        // Hashing the whole allocation would make a self-golden a function of
        // pool history; hashing payload rows makes it a function of the picture.
        let (stage, _) = corpus();
        let cfg = config();
        let (plan, mono, binning) = derive(&stage, cfg, default_tiling());
        let job = FrameJob::new(&plan, &mono, &binning, cfg).expect("matching frame artifacts");
        let tight = job.render(1).expect("render");

        let padded_layout = FrameLayout::with_row_alignment(
            PixelFormat::Rgba16F,
            cfg.viewport.width,
            cfg.viewport.height,
            256,
        )
        .expect("padded layout");
        let mut padded = FrameBuffer::new(padded_layout);
        padded.as_bytes_mut().fill(0x5c);
        job.render_into(1, &mut padded).expect("render");

        assert!(
            padded.layout().stride(0) > tight.layout().stride(0),
            "the padded layout is not actually padded"
        );
        assert_ne!(
            padded.as_bytes(),
            tight.as_bytes(),
            "padding did not differ"
        );
        assert_eq!(
            frame_digest(&tight).expect("digest"),
            frame_digest(&padded).expect("digest"),
            "the digest saw the padding"
        );
    }

    #[test]
    fn the_raw_frame_converts_to_a_canonical_png_payload() {
        // The certified artifact chain: raw frame -> canonical RGBA8. The
        // conversion is fmn-frame's table-driven kernel, so this asserts the
        // engine's output is in the format that kernel accepts, and that an
        // opaque frame stays opaque through it.
        let (stage, _) = corpus();
        let cfg = config();
        let (plan, mono, binning) = derive(&stage, cfg, default_tiling());
        let raw = FrameJob::new(&plan, &mono, &binning, cfg)
            .expect("matching frame artifacts")
            .render(1)
            .expect("render");
        let mut rgba8 = FrameBuffer::new(
            FrameLayout::tight(PixelFormat::Rgba8, cfg.viewport.width, cfg.viewport.height)
                .expect("layout"),
        );
        fmn_frame::convert::rgba16f_to_rgba8(&raw, &mut rgba8).expect("convert");
        assert!(
            rgba8
                .plane(0)
                .as_chunks::<4>()
                .0
                .iter()
                .all(|px| px[3] == 255)
        );
        // The background is 0.04 linear, which is emphatically not 10/255.
        let corner = &rgba8.plane(0)[..4];
        assert!(corner[0] > 40 && corner[0] < 70, "{corner:?}");
    }

    #[test]
    fn an_empty_scene_is_exactly_the_background() {
        let stage = Stage::new();
        let cfg = config();
        let (plan, mono, binning) = derive(&stage, cfg, default_tiling());
        let job = FrameJob::new(&plan, &mono, &binning, cfg).expect("matching frame artifacts");
        assert_eq!(job.draw_count(), 0);
        let frame = job.render(4).expect("render");
        let expect = {
            let bg = cfg.background;
            let mut px = [0u8; 8];
            for (k, v) in [bg.r, bg.g, bg.b, bg.a].into_iter().enumerate() {
                px[k * 2..k * 2 + 2]
                    .copy_from_slice(&fmn_frame::half::f16_from_f32(v as f32).to_le_bytes());
            }
            px
        };
        assert!(
            frame
                .plane(0)
                .as_chunks::<8>()
                .0
                .iter()
                .all(|px| px.eq(&expect))
        );
    }

    #[test]
    fn the_identity_journal_separates_what_it_must() {
        let cfg = config();
        let tiling = default_tiling();
        assert_eq!(EngineIdentity::default(), EngineIdentity::fast());
        assert_eq!(EngineIdentity::fast().tier, Tier::COMPILED);
        let base = journal_digest(EngineIdentity::certified(), &cfg, tiling);
        assert_eq!(
            base,
            journal_digest(EngineIdentity::certified(), &cfg, tiling)
        );

        // A different engine is a different identity even at the same config —
        // §10.5(f) exists so a manifest cannot claim the certified engine
        // produced what an annex engine did.
        let annex = EngineIdentity {
            engine: EngineKind::Metal,
            ..EngineIdentity::certified()
        };
        assert_ne!(base, journal_digest(annex, &cfg, tiling));
        assert!(!annex.engine.certifiable());
        assert!(EngineKind::CertifiedCpu.certifiable());

        let built_tier = EngineIdentity {
            tier: Tier::COMPILED,
            ..EngineIdentity::certified()
        };
        assert_ne!(
            base,
            journal_digest(built_tier, &cfg, tiling),
            "the build tier is part of C3/C7 provenance"
        );

        // C10's declared configuration: the tile dimensions are in the closure
        // because the fill's per-tile carry is association-dependent.
        let other_tiling = Tiling {
            macro_tile: 64,
            fine_tile: 32,
        };
        assert_ne!(
            base,
            journal_digest(EngineIdentity::certified(), &cfg, other_tiling)
        );

        // And the viewport, the map, and the background.
        let mut moved = cfg;
        moved.map.origin[0] += 1.0;
        assert_ne!(
            base,
            journal_digest(EngineIdentity::certified(), &moved, tiling)
        );
        let mut recoloured = cfg;
        recoloured.background.g = 0.5;
        assert_ne!(
            base,
            journal_digest(EngineIdentity::certified(), &recoloured, tiling)
        );

        // The declared A/B policy is provenance even though certified
        // normalizes every policy to the same analytic pixel path.
        let forced = cfg.with_aa_policy(AaPolicy::Ssaa4x);
        assert_ne!(
            base,
            journal_digest(EngineIdentity::certified(), &forced, tiling)
        );
    }

    #[test]
    fn a_job_refuses_an_identity_it_cannot_truthfully_execute() {
        let (stage, _) = corpus();
        let cfg = config();
        let (plan, mono, binning) = derive(&stage, cfg, default_tiling());

        let requested = RENDERER_VERSION.wrapping_add(1);
        let stale = EngineIdentity {
            renderer_version: requested,
            ..EngineIdentity::certified()
        };
        assert!(matches!(
            FrameJob::with_identity(&plan, &mono, &binning, cfg, stale),
            Err(FrameJobError::RendererVersionMismatch {
                requested: got,
                compiled: RENDERER_VERSION,
            }) if got == requested
        ));

        for engine in [EngineKind::Metal, EngineKind::Cuda] {
            let annex = EngineIdentity {
                engine,
                ..EngineIdentity::certified()
            };
            assert!(matches!(
                FrameJob::with_identity(&plan, &mono, &binning, cfg, annex),
                Err(FrameJobError::UnsupportedEngine { engine: got }) if got == engine
            ));
        }
    }

    #[test]
    fn affine_job_refuses_camera_and_depth_vector_semantics() {
        let (mut stage, mobs) = corpus();
        stage.uniforms_mut(mobs[0]).expect("live").depth_test = true;
        let cfg = config();
        let (plan, mono, binning) = derive(&stage, cfg, default_tiling());
        assert!(matches!(
            FrameJob::new(&plan, &mono, &binning, cfg),
            Err(FrameJobError::CameraProjectionRequired { instance: 0 })
        ));
    }

    #[test]
    fn the_scalar_oracle_and_compiled_tier_are_both_listed() {
        // The harness in fmn-conformance sweeps `Tier::ALL`; a tier that lands
        // without being listed is a tier nothing checks.
        assert_eq!(Tier::ALL, &[Tier::Scalar, Tier::COMPILED]);
        assert_eq!(Tier::ALL[0].name(), "scalar");
        assert_ne!(Tier::COMPILED, Tier::Scalar);
        assert_ne!(Tier::COMPILED.name(), "scalar");
    }

    #[test]
    fn a_mismatched_destination_is_refused_rather_than_reinterpreted() {
        let (stage, _) = corpus();
        let cfg = config();
        let (plan, mono, binning) = derive(&stage, cfg, default_tiling());
        let job = FrameJob::new(&plan, &mono, &binning, cfg).expect("matching frame artifacts");

        let mut wrong_format = FrameBuffer::new(
            FrameLayout::tight(PixelFormat::Rgba8, cfg.viewport.width, cfg.viewport.height)
                .expect("layout"),
        );
        assert!(matches!(
            job.render_into(1, &mut wrong_format),
            Err(FrameError::FormatMismatch { .. })
        ));

        let mut wrong_size =
            FrameBuffer::new(FrameLayout::tight(PixelFormat::Rgba16F, 8, 8).expect("layout"));
        assert!(matches!(
            job.render_into(1, &mut wrong_size),
            Err(FrameError::DimensionMismatch)
        ));
    }

    #[test]
    fn stale_derived_artifacts_are_refused_before_rasterization() {
        let (mut stage, mobs) = corpus();
        let cfg = config();
        let (mut plan, mono, binning) = derive(&stage, cfg, default_tiling());

        // Mono pieces contain both scale and origin, so either part of the map
        // moving makes the table stale even when geometry is unchanged.
        let moved_map = ScreenMap {
            origin: [cfg.map.origin[0] + 1.0, cfg.map.origin[1]],
            ..cfg.map
        };
        let moved_mono = MonoTable::build(&plan, moved_map);
        assert!(matches!(
            FrameJob::new(&plan, &moved_mono, &binning, cfg),
            Err(FrameJobError::MonoMapMismatch)
        ));

        // A different geometry table can have the same number of shapes and
        // still make every shape index name someone else's monotone pieces.
        let mut other_stage = Stage::new();
        let other = other_stage.add(vmob(
            &rect_points(8.0, 8.0, 80.0, 80.0),
            [1.0, 0.0, 0.0, 1.0],
            [0.0; 4],
            0.0,
        ));
        other_stage.add_to_scene(other).expect("live");
        let (other_plan, _, other_binning) = derive(&other_stage, cfg, default_tiling());
        assert!(matches!(
            FrameJob::new(&other_plan, &mono, &other_binning, cfg),
            Err(FrameJobError::MonoPlanMismatch)
        ));

        // Binning consumes the map independently for AABBs and interior flags.
        let moved_binning = Binning::build(&plan, cfg.viewport, default_tiling(), moved_map);
        assert!(matches!(
            FrameJob::new(&plan, &mono, &moved_binning, cfg),
            Err(FrameJobError::BinningMapMismatch)
        ));

        // Grid cardinality is not viewport identity: both widths occupy seven
        // 16-pixel columns, but the final tile clips at a different pixel.
        let near_viewport = Viewport {
            width: cfg.viewport.width - 1,
            height: cfg.viewport.height,
        };
        let near_binning = Binning::build(&plan, near_viewport, default_tiling(), cfg.map);
        assert_eq!(near_binning.tile_count(), binning.tile_count());
        assert!(matches!(
            FrameJob::new(&plan, &mono, &near_binning, cfg),
            Err(FrameJobError::BinningViewportMismatch)
        ));

        // Re-syncing can reorder the painter list while every table length and
        // grid dimension remains unchanged. The old command indices must not be
        // reinterpreted under the new order.
        stage.set_z_index(mobs[0], 100, false);
        stage.add_to_scene(mobs[0]).expect("live");
        plan.sync(&stage, 0);
        assert!(matches!(
            FrameJob::new(&plan, &mono, &binning, cfg),
            Err(FrameJobError::BinningPlanMismatch)
        ));
    }

    #[test]
    fn a_foreign_style_view_write_changes_the_frame_without_a_revision_callback() {
        let (mut stage, mobs) = corpus();
        let cfg = config();
        let render = |stage: &Stage| {
            let (plan, mono, binning) = derive(stage, cfg, default_tiling());
            FrameJob::new(&plan, &mono, &binning, cfg)
                .expect("matching frame artifacts")
                .render(1)
                .expect("render")
        };
        let before = render(&stage);

        let revision = stage
            .get(mobs[0])
            .expect("live")
            .buffer
            .field_revision("fill_rgba");
        let view = stage
            .get_mut(mobs[0])
            .expect("live")
            .buffer
            .export_field_view("fill_rgba", true)
            .expect("fill field");
        for record in 0..view.len() {
            assert!(view.write_foreign(record, "fill_rgba", &[0.0, 0.0, 1.0, 0.7]));
        }
        assert_eq!(
            stage
                .get(mobs[0])
                .expect("live")
                .buffer
                .field_revision("fill_rgba"),
            revision,
            "the foreign writer must not accidentally exercise revision invalidation"
        );
        let after = render(&stage);
        assert_ne!(
            before.as_bytes(),
            after.as_bytes(),
            "an untracked live style edit must reach the current frame"
        );
        drop(view);
    }

    #[test]
    fn a_writable_point_view_forces_the_general_fill_kernel() {
        let mut stage = Stage::new();
        let mob = stage.add(vmob(
            &rect_points(20.0, 20.0, 60.0, 60.0),
            [1.0, 0.0, 0.0, 1.0],
            [0.0; 4],
            0.0,
        ));
        stage.set_shape(
            mob,
            ShapeTag::Rect {
                center: [40.0, 40.0, 0.0],
                width: 40.0,
                height: 40.0,
            },
        );
        stage.add_to_scene(mob).expect("live");
        let cfg = config();

        let (mut plan, mono, binning) = derive(&stage, cfg, default_tiling());
        {
            let job = FrameJob::new(&plan, &mono, &binning, cfg).expect("matching frame artifacts");
            assert!(matches!(
                job.draws[0].as_ref().expect("visible").kernel,
                FillKernel::Rect { .. }
            ));
        }

        let view = stage
            .get_mut(mob)
            .expect("live")
            .buffer
            .export_field_view("point", true)
            .expect("point field");
        plan.sync(&stage, 0);
        assert!(plan.shapes().instances()[0].hint_unsafe);
        assert!(matches!(
            FrameJob::new(&plan, &mono, &binning, cfg),
            Err(FrameJobError::BinningPlanMismatch)
        ));
        let viewed_binning = Binning::build(&plan, cfg.viewport, default_tiling(), cfg.map);
        let viewed =
            FrameJob::new(&plan, &mono, &viewed_binning, cfg).expect("matching viewed artifacts");
        assert_eq!(
            viewed.draws[0].as_ref().expect("visible").kernel,
            FillKernel::General
        );
        drop(view);
    }

    /// Assert two frames are byte-identical, reporting the **first differing
    /// pixel** rather than dumping the buffers.
    ///
    /// A raw `assert_eq!` on two hundred-kilobyte byte slices produces output
    /// nobody can read and therefore nobody debugs from; a coordinate and two
    /// decoded pixels say immediately whether the difference is one rounding or
    /// a structural fault.
    fn assert_frames_equal(a: &FrameBuffer, b: &FrameBuffer, what: &str) {
        assert_eq!(a.layout(), b.layout(), "{what}: different layouts");
        let width = a.layout().width();
        let height = a.layout().height();
        let stride = a.layout().stride(0);
        let mut differing = 0usize;
        let mut first = None;
        for y in 0..height {
            for x in 0..width {
                let base = y as usize * stride + x as usize * 8;
                if !a.plane(0)[base..base + 8].eq(&b.plane(0)[base..base + 8]) {
                    differing += 1;
                    first.get_or_insert((x, y));
                }
            }
        }
        if let Some((x, y)) = first {
            // Asserted on the bytes rather than on the decoded pixels, so the
            // check stays exact while the message stays legible: two f16 codes
            // that decode to the same `f64` are still two different frames.
            let base = y as usize * stride + x as usize * 8;
            assert_eq!(
                a.plane(0)[base..base + 8],
                b.plane(0)[base..base + 8],
                "{what}: {differing} of {} pixels differ; first at ({x}, {y}): {:?} vs {:?}",
                width as usize * height as usize,
                read_px(a, x, y),
                read_px(b, x, y),
            );
        }
    }

    /// Maximum and RMS linear-channel error over two equally laid-out frames.
    fn visual_error(a: &FrameBuffer, b: &FrameBuffer) -> (f64, f64) {
        assert_eq!(a.layout(), b.layout(), "visual comparison layouts differ");
        let mut squared = 0.0;
        let mut maximum = 0.0f64;
        let mut channels = 0u64;
        for y in 0..a.layout().height() {
            for x in 0..a.layout().width() {
                for (a, b) in read_px(a, x, y).into_iter().zip(read_px(b, x, y)) {
                    let error = (a - b).abs();
                    squared += error * error;
                    maximum = maximum.max(error);
                    channels += 1;
                }
            }
        }
        (maximum, (squared / channels as f64).sqrt())
    }

    /// Decode one pixel back to linear-light straight alpha.
    fn read_px(frame: &FrameBuffer, x: u32, y: u32) -> [f64; 4] {
        let stride = frame.layout().stride(0);
        let base = y as usize * stride + x as usize * 8;
        let px = &frame.plane(0)[base..base + 8];
        let mut out = [0.0; 4];
        for (k, o) in out.iter_mut().enumerate() {
            *o = fmn_frame::half::f16_to_f64(u16::from_le_bytes([px[k * 2], px[k * 2 + 1]]));
        }
        out
    }
}
