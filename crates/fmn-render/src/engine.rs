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
//! | IEEE-754 basic operations only | `f64` throughout; `sqrt` is the only non-arithmetic primitive, and IEEE-754 requires it correctly rounded |
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
//! Compositing runs in `f64` in a row accumulator and is narrowed **once**, at
//! writeback. The `f64 → f32 → f16` narrowing is a double rounding, and it is
//! recorded rather than hidden: it is deterministic on every platform (both steps
//! are IEEE round-to-nearest-even), and reaching a different `f16` than a direct
//! `f64 → f16` would require the accumulated value to sit within `2⁻²⁴` of an
//! `f16` midpoint — below the width at which the 8-bit output can express a
//! difference.

use crate::bin::{Binning, CLASS_INTERIOR, ScreenMap, Tiling, Viewport};
use crate::fill::{
    self, FillKernel, GradientField, RowScratch, fill_is_flat, fill_rgba_at, fill_rgba_with_border,
};
use crate::plan::RenderPlan;
use crate::stroke::{self, JoinWedge, stroke_rgba_at, stroke_shade};
use crate::table::{Segment, Style};
use fmn_core::color::{LinearRgba, PremulRgba};
use fmn_frame::{FrameBuffer, FrameError, FrameLayout, PixelFormat};
use fmn_hash::{Digest, Schema, Writer};
use std::sync::{Mutex, PoisonError};

// --------------------------------------------------------------- the identity

/// The schema family for engine-identity and frame documents.
pub const ENGINE_SCHEMA: Schema = Schema::new(*b"FMNE", 1, 1, 0);

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
pub const RENDERER_VERSION: u32 = 1;

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

/// The SIMD build tier a CPU engine was compiled for (§17.3, C3).
///
/// Only [`Tier::Scalar`] exists today, and that is the point rather than a
/// placeholder: **within the certified engine the scalar path is the
/// definition**, and every tier fm-4wt adds must match it bit-for-bit. The
/// enumeration exists now so the harness that will check that
/// (`fmn-conformance/tests/certified_engine.rs`) is written against a real type
/// rather than retrofitted around the first tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tier {
    /// Portable scalar: the definition every other tier must reproduce.
    Scalar,
}

impl Tier {
    /// The stable name journaled into the input closure.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Scalar => "scalar",
        }
    }

    /// Every tier this build offers, scalar first.
    ///
    /// The certified harness sweeps this, so a tier that lands without being
    /// listed here is a tier nothing checks.
    pub const ALL: &'static [Tier] = &[Tier::Scalar];
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
}

impl Default for EngineIdentity {
    fn default() -> Self {
        Self::certified()
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
}

impl FrameConfig {
    /// A frame of `viewport` on the default map over an opaque background.
    #[must_use]
    pub fn new(viewport: Viewport, map: ScreenMap, background: LinearRgba) -> FrameConfig {
        FrameConfig {
            viewport,
            map,
            background,
        }
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
    w.put_u32(PixelFormat::Rgba16F as u32);

    // C10 — the semantic constants. Measured, not tuned; see each one's own
    // documentation for the measurement that fixed it.
    w.put_u64(fill::GRADIENT_STATIONS as u64);
    w.put_f64(fill::HINT_BUDGET_PX);
    w.put_u32(fill::FLATTEN_MAX_DEPTH);
    w.put_f64(stroke::MITER_LIMIT);
    w.put_f64(crate::hint::NEARLY_LINEAR_PX);

    w.finish().expect("the identity document fits any limit")
}

/// The digest of [`journal`] — the value C7 and C10 contribute to the closure.
#[must_use]
pub fn journal_digest(identity: EngineIdentity, config: &FrameConfig, tiling: Tiling) -> Digest {
    fmn_hash::sha256(&journal(identity, config, tiling))
}

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
pub fn frame_digest(buffer: &FrameBuffer) -> Result<Digest, fmn_hash::SerialError> {
    Ok(fmn_hash::sha256(&encode_frame(buffer)?))
}

/// The canonical byte document a [`frame_digest`] is taken over — the form the
/// self-golden rig locks.
///
/// # Errors
/// Propagates the envelope's limit errors.
pub fn encode_frame(buffer: &FrameBuffer) -> Result<Vec<u8>, fmn_hash::SerialError> {
    let layout = buffer.layout();
    let width = layout.width() as usize;
    let height = layout.height() as usize;
    let stride = layout.stride(0);
    let row_bytes = width * sample_bytes(layout.format());
    let mut w = Writer::new(FRAME_SCHEMA);
    w.put_u32(layout.width());
    w.put_u32(layout.height());
    w.put_u32(layout.format() as u32);
    let plane = buffer.plane(0);
    for y in 0..height {
        w.put_bytes(&plane[y * stride..y * stride + row_bytes]);
    }
    w.finish()
}

/// Payload bytes per pixel for the formats a frame digest accepts.
fn sample_bytes(format: PixelFormat) -> usize {
    match format {
        PixelFormat::Rgba16F => 8,
        _ => 4,
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
    /// The interned style, copied so the tile loop indexes no table.
    style: Style,
    /// The hinted fill route, or [`FillKernel::General`].
    kernel: FillKernel,
    /// The joint overrides; empty for the round settings (ADR-0012).
    joins: Vec<JoinWedge>,
    /// The interior colour field — `None` when the fill is flat, which is the
    /// overwhelming majority and the case that must not pay for the field.
    field: Option<GradientField>,
    /// The one fill colour, when the fill is flat.
    flat_fill: Option<[f32; 4]>,
    /// Screen AABB of the outline hull: rows outside it have zero fill coverage.
    fill_slab: [f64; 4],
    /// Screen AABB of the stroke, hull plus the widest half-width and the AA band.
    stroke_slab: [f64; 4],
    /// Does this instance contribute a fill pass at all?
    draws_fill: bool,
    /// Does it contribute a stroke pass?
    draws_stroke: bool,
}

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
    draws: Vec<Draw>,
    cols: u32,
}

impl<'a> FrameJob<'a> {
    /// Compile a frame from a synchronized plan, its monotone table and its
    /// binning.
    ///
    /// `mono` must have been built under the same [`ScreenMap`] as `config`, and
    /// `binning` over the same plan — all three are derived from one sync and
    /// carrying them separately is what lets a retained plan skip rebuilding the
    /// ones whose axes did not move.
    #[must_use]
    pub fn new(
        plan: &'a RenderPlan,
        mono: &'a fill::MonoTable,
        binning: &'a Binning,
        config: FrameConfig,
    ) -> FrameJob<'a> {
        Self::with_identity(plan, mono, binning, config, EngineIdentity::certified())
    }

    /// [`FrameJob::new`] with an explicit engine identity.
    ///
    /// The identity does not change what this engine computes — there is one
    /// arithmetic here — it changes what the frame *claims*. A fast-CPU or annex
    /// engine reuses this front-end and substitutes its own back-end; recording
    /// which one ran is §10.5(f).
    #[must_use]
    pub fn with_identity(
        plan: &'a RenderPlan,
        mono: &'a fill::MonoTable,
        binning: &'a Binning,
        config: FrameConfig,
        identity: EngineIdentity,
    ) -> FrameJob<'a> {
        let map = config.map;
        let segments = plan.segments();
        let mut draws = Vec::with_capacity(plan.shapes().instances().len());

        for inst in plan.shapes().instances() {
            let Some(shape) = plan.shapes().shape(inst.shape) else {
                continue;
            };
            let Some(style) = plan.styles().get(inst.style).copied() else {
                continue;
            };
            let lo = shape.first_segment as usize;
            let hi = lo + shape.segment_count as usize;
            let segs = &segments[lo.min(segments.len())..hi.min(segments.len())];
            let translate = fill::instance_translation(inst, map);

            let draws_fill = style.fill_rgba[3] > 0.0 || style.fill_rgba_end[3] > 0.0;
            let draws_stroke = (style.stroke_width > 0.0 || style.stroke_width_end > 0.0)
                && (style.stroke_rgba[3] > 0.0 || style.stroke_rgba_end[3] > 0.0);
            if !draws_fill && !draws_stroke {
                // Not an optimization: an instance with no visible pass
                // composites as the identity, so dropping it here and drawing it
                // are the same bytes. Skipping keeps the tile loop's inner test
                // to `flags` alone.
                continue;
            }

            let flat = fill_is_flat(&style);
            draws.push(Draw {
                first_segment: shape.first_segment,
                segment_count: shape.segment_count,
                shape: inst.shape,
                translate,
                style,
                kernel: FillKernel::select(shape, segs, map, translate),
                joins: stroke::join_wedges(segs, &shape.subpath_starts, &style, map, translate),
                field: if draws_fill && !flat {
                    Some(GradientField::build(segs, map))
                } else {
                    None
                },
                flat_fill: if flat {
                    Some(fill_rgba_at(&style, 0.0))
                } else {
                    None
                },
                fill_slab: hull_slab(shape, map, translate),
                stroke_slab: stroke_slab(segs, &style, map, translate),
                draws_fill,
                draws_stroke,
            });
        }

        let cols = config
            .viewport
            .width
            .div_ceil(binning.tiling().fine_tile.max(1));

        FrameJob {
            plan,
            mono,
            binning,
            config,
            identity,
            draws,
            cols,
        }
    }

    /// The engine identity this frame will claim.
    #[must_use]
    pub fn identity(&self) -> EngineIdentity {
        self.identity
    }

    /// This frame's contribution to the input closure (C7 + C10).
    #[must_use]
    pub fn journal_digest(&self) -> Digest {
        journal_digest(self.identity, &self.config, self.binning.tiling())
    }

    /// How many instances survived to contribute a pass.
    #[must_use]
    pub fn draw_count(&self) -> usize {
        self.draws.len()
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
        if dst.layout().format() != PixelFormat::Rgba16F {
            return Err(FrameError::FormatMismatch {
                expected: "Rgba16F raw frame",
                got: dst.layout().format(),
            });
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
            let mut worker = Worker::new(tile);
            for (band, bytes) in bands {
                self.render_band(&mut worker, band, bytes, stride);
            }
            return Ok(());
        }

        // Popped from the end, so the queue is a stack; which worker takes which
        // band is deliberately unspecified, because a band's bytes do not depend
        // on who computed them.
        bands.reverse();
        let queue = Mutex::new(bands);
        std::thread::scope(|scope| {
            for _ in 0..threads {
                scope.spawn(|| {
                    let mut worker = Worker::new(tile);
                    loop {
                        let next = queue.lock().unwrap_or_else(PoisonError::into_inner).pop();
                        let Some((band, bytes)) = next else { break };
                        self.render_band(&mut worker, band, bytes, stride);
                    }
                });
            }
        });
        Ok(())
    }

    /// Rasterize one band: `tile` pixel rows spanning the full frame width.
    fn render_band(&self, worker: &mut Worker, band: usize, bytes: &mut [u8], stride: usize) {
        let tile = self.binning.tiling().fine_tile.max(1);
        let width = self.config.viewport.width;
        let height = self.config.viewport.height;
        let y0 = band as u32 * tile;
        let y1 = (y0 + tile).min(height);

        // The background is written first and unconditionally, so a band with no
        // commands still costs exactly one pass — and so a tile that draws
        // nothing is not distinguishable from one that never ran.
        let bg = self.config.background.premultiply();
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

                let t = band * self.cols as usize + tx as usize;
                if t < self.binning.tile_count() {
                    self.composite_row(worker, t, py, x_lo, x_hi);
                }

                let base = x_lo as usize * 8;
                write_row(&worker.acc[..w], &mut row[base..base + w * 8]);
            }
        }
    }

    /// Composite one tile's command list over one pixel row of the accumulator.
    ///
    /// The draw loop is the **outer** one and painter order is its order: that is
    /// what lets a fill contribute a whole row of coverage from one pass over its
    /// pieces while a stroke still shades per pixel, without either being able to
    /// reorder the composite.
    fn composite_row(&self, worker: &mut Worker, tile: usize, py: u32, x_lo: u32, x_hi: u32) {
        let draws = self.binning.tile(tile);
        let flags = self.binning.tile_flags(tile);
        for (k, &d) in draws.iter().enumerate() {
            let Some(rec) = self.draws.get(d as usize) else {
                continue;
            };
            let interior = flags.get(k).copied() == Some(CLASS_INTERIOR);
            // R-5: within one object the fill draws before the stroke, unless
            // `stroke_behind` swaps them (docs/RENDER_ORDER.md).
            if rec.style.stroke_behind {
                self.stroke_pass(worker, rec, py, x_lo, x_hi);
                self.fill_pass(worker, rec, interior, py, x_lo, x_hi);
            } else {
                self.fill_pass(worker, rec, interior, py, x_lo, x_hi);
                self.stroke_pass(worker, rec, py, x_lo, x_hi);
            }
        }
    }

    /// §10.2's fill over one row of one tile.
    fn fill_pass(
        &self,
        worker: &mut Worker,
        rec: &Draw,
        interior: bool,
        py: u32,
        x_lo: u32,
        x_hi: u32,
    ) {
        if !rec.draws_fill || row_misses(rec.fill_slab, py) {
            return;
        }
        let w = (x_hi - x_lo) as usize;

        // Exactly one of three sources fills the row, and the order of the tests
        // is the order of §10.4's own argument: a classified interior costs
        // nothing, a hinted kernel costs a closed form, and the general machinery
        // is the fallback rather than the toll road.
        if interior {
            worker.cov[..w].copy_from_slice(worker.scratch.interior_row(x_lo, x_hi));
        } else if !rec.kernel.row(py, x_lo, x_hi, &mut worker.cov[..w]) {
            let pieces = self.mono.pieces_of(rec.shape);
            worker.cov[..w].copy_from_slice(worker.scratch.fill_row(
                pieces,
                rec.translate,
                py,
                x_lo,
                x_hi,
            ));
        }

        let segments = self.segments_of(rec);
        for i in 0..w {
            let coverage = worker.cov[i];
            if coverage <= 0.0 {
                continue;
            }
            let rgba = match rec.flat_fill {
                Some(c) => c,
                None => {
                    let p = [f64::from(x_lo + i as u32) + 0.5, f64::from(py) + 0.5];
                    let field = rec.field.as_ref().expect("a non-flat fill carries a field");
                    fill_rgba_with_border(
                        &rec.style,
                        field,
                        segments,
                        self.config.map,
                        rec.translate,
                        p,
                    )
                }
            };
            worker.acc[i] = source_over(rgba, coverage, worker.acc[i]);
        }
    }

    /// §10.3's stroke over one row of one tile.
    fn stroke_pass(&self, worker: &mut Worker, rec: &Draw, py: u32, x_lo: u32, x_hi: u32) {
        if !rec.draws_stroke || row_misses(rec.stroke_slab, py) {
            return;
        }
        let segments = self.segments_of(rec);
        let w = (x_hi - x_lo) as usize;
        for i in 0..w {
            let p = [f64::from(x_lo + i as u32) + 0.5, f64::from(py) + 0.5];
            if p[0] < rec.stroke_slab[0] || p[0] > rec.stroke_slab[2] {
                continue;
            }
            let (coverage, s) = stroke_shade(
                segments,
                &rec.joins,
                &rec.style,
                self.config.map,
                rec.translate,
                p,
            );
            if coverage <= 0.0 {
                continue;
            }
            worker.acc[i] = source_over(stroke_rgba_at(&rec.style, s), coverage, worker.acc[i]);
        }
    }

    /// This draw's slice of the plan's one flat segment table.
    fn segments_of(&self, rec: &Draw) -> &[Segment] {
        let all = self.plan.segments();
        let lo = (rec.first_segment as usize).min(all.len());
        let hi = (rec.first_segment as usize + rec.segment_count as usize).min(all.len());
        &all[lo..hi]
    }
}

// ------------------------------------------------------------------ the worker

/// One thread's scratch, allocated once per band-loop rather than per row.
///
/// PG-6 forbids steady-state per-frame heap allocation. A worker's buffers are
/// sized for the widest tile the frame can present and then reused for every
/// band it takes.
#[derive(Debug)]
struct Worker {
    /// The row accumulator, premultiplied linear light.
    acc: Vec<PremulRgba>,
    /// One draw's coverage over the row.
    cov: Vec<f64>,
    /// The fill's own scratch.
    scratch: RowScratch,
}

impl Worker {
    fn new(tile: usize) -> Worker {
        Worker {
            acc: vec![PremulRgba::TRANSPARENT; tile],
            cov: vec![0.0; tile],
            scratch: RowScratch::for_tile(tile as u32),
        }
    }
}

// ------------------------------------------------------------------ the pieces

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

/// A compiled outline's screen-space AABB.
///
/// Built from the two extreme corners and then normalized, because a negative
/// `ScreenMap::scale` maps `min` above `max` and an un-normalized box would
/// reject every row.
fn hull_slab(shape: &crate::table::Shape, map: ScreenMap, translate: [f64; 2]) -> [f64; 4] {
    let to_px = |p: fmn_core::types::Vec3| {
        [
            map.origin[0] + p[0] * map.scale + translate[0],
            map.origin[1] + p[1] * map.scale + translate[1],
        ]
    };
    let a = to_px(shape.bounds.min);
    let b = to_px(shape.bounds.max);
    [
        a[0].min(b[0]),
        a[1].min(b[1]),
        a[0].max(b[0]),
        a[1].max(b[1]),
    ]
}

/// The union of a shape's per-segment stroke slabs.
fn stroke_slab(
    segments: &[Segment],
    style: &Style,
    map: ScreenMap,
    translate: [f64; 2],
) -> [f64; 4] {
    let mut out = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    for seg in segments {
        let s = stroke::segment_slab(seg, style, map, translate);
        out[0] = out[0].min(s[0]);
        out[1] = out[1].min(s[1]);
        out[2] = out[2].max(s[2]);
        out[3] = out[3].max(s[3]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bin::covers_tile;
    use crate::fill::MonoTable;
    use fmn_mobject::{Mob, Mobject, RecordBuffer, RecordSchema, Stage};

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
    /// an open stroke with round caps, a tapered gradient stroke, and a hairline
    /// finer than the AA band.
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
        let mut annulus = ring(72.0, 32.0, 20.0, 8, true);
        annulus.extend(ring(72.0, 32.0, 9.0, 8, false));
        add(
            &mut stage,
            vmob(&annulus, [1.0, 1.0, 0.0, 1.0], [0.0; 4], 0.0),
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
                        let inst = &job.plan.shapes().instances()[instance_of(job, d)];
                        let interior = covers_tile(job.plan, inst, cfg.map, rect);
                        let mut fill = |acc: &mut Vec<PremulRgba>| {
                            if !rec.draws_fill {
                                return;
                            }
                            if interior {
                                cov[..w].copy_from_slice(scratch.interior_row(x_lo, x_hi));
                            } else if !rec.kernel.row(py, x_lo, x_hi, &mut cov[..w]) {
                                cov[..w].copy_from_slice(scratch.fill_row(
                                    job.mono.pieces_of(rec.shape),
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

    /// `FrameJob` drops instances with no visible pass, so its draw index is not
    /// the instance index; recover it by matching the shape and translation.
    fn instance_of(job: &FrameJob<'_>, draw: usize) -> usize {
        let rec = &job.draws[draw];
        job.plan
            .shapes()
            .instances()
            .iter()
            .position(|i| {
                i.shape == rec.shape
                    && crate::fill::instance_translation(i, job.config.map) == rec.translate
            })
            .expect("every draw came from an instance")
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
        let job = FrameJob::new(&plan, &mono, &binning, cfg);
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
        let job = FrameJob::new(&plan, &mono, &binning, cfg);
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
            .render(1)
            .expect("render");
        let report = binning.prune_occluded(&plan);
        let pruned = FrameJob::new(&plan, &mono, &binning, cfg)
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
    fn interning_order_does_not_move_a_pixel() {
        // ADR-0013's pinning, as an experiment rather than an assurance. Two
        // scenes with identical painter order but different *interning* order —
        // the shape and style table indices differ — must render the same bytes,
        // because the engine walks instances in painter order and each shape's
        // segments in the outline's own order. Neither is a property of the sync.
        let build = |reversed: bool| {
            let mut stage = Stage::new();
            let a = vmob(
                &ring(30.0, 30.0, 14.0, 8, true),
                [1.0, 0.0, 0.0, 0.8],
                [0.0; 4],
                0.0,
            );
            let b = vmob(
                &ring(50.0, 40.0, 18.0, 8, true),
                [0.0, 0.0, 1.0, 0.6],
                [0.0; 4],
                0.0,
            );
            // Creation order decides interning order; z_index decides painter
            // order. Setting them against each other is the whole experiment.
            let (first, second) = if reversed { (b, a) } else { (a, b) };
            let h1 = stage.add(first);
            let h2 = stage.add(second);
            stage.set_z_index(h1, if reversed { 1 } else { 0 }, false);
            stage.set_z_index(h2, if reversed { 0 } else { 1 }, false);
            stage.add_to_scene(h1).expect("live");
            stage.add_to_scene(h2).expect("live");
            let cfg = config();
            let (plan, mono, binning) = derive(&stage, cfg, default_tiling());
            FrameJob::new(&plan, &mono, &binning, cfg)
                .render(1)
                .expect("render")
                .as_bytes()
                .to_vec()
        };
        assert_eq!(build(false), build(true));
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
        let job = FrameJob::new(&plan, &mono, &binning, cfg);
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
        let job = FrameJob::new(&plan, &mono, &binning, cfg);
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
        let job = FrameJob::new(&plan, &mono, &binning, cfg);
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
                .all(|px| *px == expect)
        );
    }

    #[test]
    fn the_identity_journal_separates_what_it_must() {
        let cfg = config();
        let tiling = default_tiling();
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
    }

    #[test]
    fn the_scalar_tier_is_the_only_one_and_is_listed() {
        // The harness in fmn-conformance sweeps `Tier::ALL`; a tier that lands
        // without being listed is a tier nothing checks.
        assert_eq!(Tier::ALL, &[Tier::Scalar]);
        assert_eq!(Tier::ALL[0].name(), "scalar");
    }

    #[test]
    fn a_mismatched_destination_is_refused_rather_than_reinterpreted() {
        let (stage, _) = corpus();
        let cfg = config();
        let (plan, mono, binning) = derive(&stage, cfg, default_tiling());
        let job = FrameJob::new(&plan, &mono, &binning, cfg);

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
                if a.plane(0)[base..base + 8] != b.plane(0)[base..base + 8] {
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
