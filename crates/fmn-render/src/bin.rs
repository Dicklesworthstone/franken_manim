//! Two-level binning, per-tile command lists, and painter-order-safe occlusion
//! pruning (§10.8, §10.5).
//!
//! > Binning is **two-level**: object bounds → ~128×128 macrotiles →
//! > engine-sized fine tiles → stable per-tile command runs, sizes tuned per
//! > platform by the execution plan (§17.4), never by semantics. Command lists
//! > keep stable draw indices; transparent runs stay in exact painter order;
//! > annex binning uses deterministic count/prefix/scatter or a stable key sort,
//! > never unordered atomic appends.
//!
//! ## Order-stable by construction, not by discipline
//!
//! The determinism obligation (§10.5) is that binning output is identical at any
//! thread count. That is met here structurally rather than by locking: the
//! algorithm is a **count → prefix-sum → scatter** over instances visited in
//! painter order, so a tile's run ascends in instance index whatever partition
//! produced it, and [`Binning::build_partitioned`] takes the partition count as a
//! parameter precisely so the property is testable without threads. A partition
//! is a *slice of the instance list*, and each partition's contribution to each
//! tile is placed at an offset computed from the partitions before it — so no
//! result depends on which partition finished first.
//!
//! G0-8 proved the same CSR shape end-to-end on Metal, where the property
//! matters more: the kernel only *reads* `draws[offsets[t]..offsets[t+1]]` in
//! index order, so painter order is structural and there is no ordering decision
//! at dispatch time to get wrong.
//!
//! ## Why two levels
//!
//! One level makes every instance test every fine tile its bounds touch. A
//! 1080p frame at 16-px tiles is 8 160 tiles, and a full-screen background tests
//! all of them. The macrotile pass cuts that by the square of the ratio: an
//! instance is tested against ~128-px macrotiles first, and the fine pass only
//! considers instances the macrotile already accepted.
//!
//! The sizes come from §17.4's `ExecutionPlan` — which does not exist yet
//! (`fmn-runtime` is a skeleton), so [`Tiling`] carries defaults and takes them
//! as parameters. That is the right shape regardless: §17.4 is explicit that
//! tile dimensions are a *scheduling* choice, and the size-sweep test here is
//! what makes "tuning changes speed, never semantics" a checked claim rather
//! than an intention.
//!
//! ## Pruning has a proof obligation per skip
//!
//! The bead's rule is absolute: **when in doubt, draw.** A command is skipped
//! only when a later command in the same tile provably covers the whole tile
//! opaquely, which means all three of: the tile is geometrically inside the
//! later shape, the later shape's fill is opaque at both ramp ends, and the
//! coverage there is exactly `1` rather than an accumulation that lands near it
//! (G0-8b's finding F13 — without that last clause the pruned and unpruned
//! frames differ in the last bit of a channel and "provably unchanged" is
//! false). The interior classification and the pruning are therefore one
//! mechanism, and they ship together.

use crate::hint::Hint;
use crate::plan::{PlanIdentity, RenderPlan};
use crate::table::{Instance, Shape};
use fmn_core::types::Vec3;

/// The command touches the tile's edge: coverage must be evaluated.
pub const CLASS_PARTIAL: u32 = 0;
/// The tile lies wholly inside the shape: coverage is `1` everywhere in it.
///
/// §10.4's "fully covered" tile class. Two things follow, and the second is the
/// one that is easy to miss: the per-pixel evaluation disappears for that
/// command, **and** its coverage becomes exactly `1` instead of an accumulation
/// landing within an ulp of it — which is the precondition that makes occlusion
/// pruning bit-exact (G0-8b/F13).
pub const CLASS_INTERIOR: u32 = 1;

/// The object→screen mapping a frame is binned under.
///
/// Deliberately a uniform scale plus an origin rather than a camera. §10.4's
/// camera, its projection conventions and `is_fixed_in_frame` are not in the
/// tree, and inventing a `Projection` here would create a second camera to
/// reconcile later — the failure this crate already avoided for the transform
/// revision channel (fm-7if).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScreenMap {
    /// Object units per pixel, inverted: pixels per object unit.
    pub scale: f64,
    /// Where object-space `[0, 0]` lands, in pixels.
    pub origin: [f64; 2],
}

impl Default for ScreenMap {
    fn default() -> Self {
        ScreenMap {
            scale: 1.0,
            origin: [0.0, 0.0],
        }
    }
}

impl ScreenMap {
    /// Map a **shape-local** point placed by an instance offset, to pixels.
    ///
    /// Both arguments are required, and that is the point: compiled outlines are
    /// shape-local so that instancing can share them, so nothing downstream may
    /// read a shape's coordinates without the offset that places it.
    #[must_use]
    pub fn place(&self, p: Vec3, offset: Vec3) -> [f64; 2] {
        [
            self.origin[0] + (p[0] + offset[0]) * self.scale,
            self.origin[1] + (p[1] + offset[1]) * self.scale,
        ]
    }
}

/// §10.8's two-level tiling.
///
/// Both sizes are scheduling choices (§17.4), never semantics — which the
/// size-sweep test in this module checks rather than assumes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tiling {
    /// Macrotile edge in pixels. §10.8 says "~128×128".
    pub macro_tile: u32,
    /// Fine tile edge in pixels — the engine's tile, and on the annex the
    /// threadgroup (G0-8 finding F4).
    pub fine_tile: u32,
}

impl Default for Tiling {
    fn default() -> Self {
        Tiling {
            macro_tile: 128,
            fine_tile: 16,
        }
    }
}

/// The pixel rectangle a frame covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Viewport {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

/// A uniform grid over the viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Grid {
    tile: u32,
    cols: u32,
    rows: u32,
}

impl Grid {
    fn new(viewport: Viewport, tile: u32) -> Grid {
        let tile = tile.max(1);
        Grid {
            tile,
            cols: viewport.width.div_ceil(tile),
            rows: viewport.height.div_ceil(tile),
        }
    }

    fn count(&self) -> usize {
        self.cols as usize * self.rows as usize
    }

    /// The inclusive tile-index span an AABB touches, clamped to the grid.
    ///
    /// Returns `None` for a box entirely off-grid, which must produce no command
    /// at all rather than a clamped one — the difference between "not visible"
    /// and "drawn at the edge".
    fn span(&self, aabb: [f64; 4]) -> Option<(u32, u32, u32, u32)> {
        if self.cols == 0 || self.rows == 0 {
            return None;
        }
        let w = (self.cols * self.tile) as f64;
        let h = (self.rows * self.tile) as f64;
        if aabb[2] < 0.0 || aabb[3] < 0.0 || aabb[0] > w || aabb[1] > h {
            return None;
        }
        let t = self.tile as f64;
        let lo = |v: f64, n: u32| ((v / t).floor().max(0.0) as u32).min(n.saturating_sub(1));
        let hi = |v: f64, n: u32| ((v / t).floor().max(0.0) as u32).min(n.saturating_sub(1));
        Some((
            lo(aabb[0], self.cols),
            lo(aabb[1], self.rows),
            hi(aabb[2], self.cols),
            hi(aabb[3], self.rows),
        ))
    }

    fn rect(&self, index: usize, viewport: Viewport) -> [f64; 4] {
        let tx = (index as u32 % self.cols) * self.tile;
        let ty = (index as u32 / self.cols) * self.tile;
        [
            f64::from(tx),
            f64::from(ty),
            f64::from((tx + self.tile).min(viewport.width)),
            f64::from((ty + self.tile).min(viewport.height)),
        ]
    }
}

/// What a pruning pass removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PruneReport {
    /// Commands before pruning.
    pub before: usize,
    /// Commands after.
    pub after: usize,
    /// Tiles in which at least one command was dropped.
    pub tiles_touched: usize,
}

impl PruneReport {
    /// The fraction of commands removed.
    #[must_use]
    pub fn removed_fraction(&self) -> f64 {
        if self.before == 0 {
            return 0.0;
        }
        (self.before - self.after) as f64 / self.before as f64
    }
}

/// A binning operation was paired with a different synchronized plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinningError {
    /// The painter-ordered plan no longer matches the one whose command lists
    /// this binning stores.
    PlanMismatch,
}

impl std::fmt::Display for BinningError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PlanMismatch => f.write_str("binning was derived from a different render plan"),
        }
    }
}

impl std::error::Error for BinningError {}

/// Per-fine-tile command lists in CSR form, with §10.4's class per command.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Binning {
    tiling: Tiling,
    viewport: Viewport,
    map: ScreenMap,
    plan: PlanIdentity,
    fine: Grid,
    /// `fine.count() + 1` entries; tile `i` owns `draws[offsets[i]..offsets[i+1]]`.
    offsets: Vec<u32>,
    /// Instance indices, **ascending within a tile** — which is painter order,
    /// because the instance list is painter order.
    draws: Vec<u32>,
    /// One class word per entry of `draws`.
    ///
    /// §10.8's command runs carry "stable draw indices"; G0-8b's F13 is that
    /// they want a flag word beside each index, because interior-vs-edge is a
    /// property of a *(shape, tile)* pair rather than of a tile.
    flags: Vec<u32>,
    /// Macrotiles that held at least one instance — the work the fine pass
    /// skipped, recorded because a two-level scheme that never prunes anything
    /// is just a slower one-level scheme.
    macro_hits: usize,
    macro_count: usize,
}

impl Binning {
    /// Bin a synchronized plan, serially.
    #[must_use]
    pub fn build(plan: &RenderPlan, viewport: Viewport, tiling: Tiling, map: ScreenMap) -> Binning {
        Binning::build_partitioned(plan, viewport, tiling, map, 1)
    }

    /// Bin a synchronized plan as if across `partitions` workers.
    ///
    /// The partition count is the §10.5 determinism obligation made testable
    /// without threads: a partition is a contiguous slice of the painter-ordered
    /// instance list, each partition's per-tile counts are summed **in partition
    /// order**, and each instance is scattered to a slot derived from that sum.
    /// Nothing depends on which partition would have finished first, so the
    /// output is byte-identical at every partition count — including counts that
    /// do not divide the work evenly.
    #[must_use]
    pub fn build_partitioned(
        plan: &RenderPlan,
        viewport: Viewport,
        tiling: Tiling,
        map: ScreenMap,
        partitions: usize,
    ) -> Binning {
        let partitions = partitions.max(1);
        let fine = Grid::new(viewport, tiling.fine_tile);
        let coarse = Grid::new(viewport, tiling.macro_tile.max(tiling.fine_tile));
        let instances = plan.shapes().instances();

        // Screen AABBs, once per instance. Object bounds are the control-polygon
        // hull, which contains the curve, so this is conservative by
        // construction rather than by a fudge factor.
        let boxes: Vec<Option<[f64; 4]>> = instances
            .iter()
            .map(|inst| screen_aabb(plan, inst, map))
            .collect();

        // ---- level one: macrotiles. Nothing is emitted from this pass; it
        // exists so the fine pass sees a short list per region.
        let mut macro_members: Vec<Vec<u32>> = vec![Vec::new(); coarse.count()];
        for (i, aabb) in boxes.iter().enumerate() {
            let Some(aabb) = aabb else { continue };
            let Some((x0, y0, x1, y1)) = coarse.span(*aabb) else {
                continue;
            };
            for ty in y0..=y1 {
                for tx in x0..=x1 {
                    macro_members[(ty * coarse.cols + tx) as usize].push(i as u32);
                }
            }
        }
        let macro_hits = macro_members.iter().filter(|m| !m.is_empty()).count();

        // Which fine tiles each instance actually lands in — derived through the
        // macrotiles, so an instance is only tested against fine tiles inside
        // macrotiles that already accepted it.
        let mut per_instance: Vec<Vec<u32>> = vec![Vec::new(); instances.len()];
        for (m, members) in macro_members.iter().enumerate() {
            if members.is_empty() {
                continue;
            }
            let mrect = coarse.rect(m, viewport);
            let fx0 = (mrect[0] / f64::from(fine.tile)).floor() as u32;
            let fy0 = (mrect[1] / f64::from(fine.tile)).floor() as u32;
            let fx1 = (((mrect[2] - 1.0).max(mrect[0]) / f64::from(fine.tile)).floor() as u32)
                .min(fine.cols.saturating_sub(1));
            let fy1 = (((mrect[3] - 1.0).max(mrect[1]) / f64::from(fine.tile)).floor() as u32)
                .min(fine.rows.saturating_sub(1));
            for &i in members {
                let Some(aabb) = boxes[i as usize] else {
                    continue;
                };
                let Some((x0, y0, x1, y1)) = fine.span(aabb) else {
                    continue;
                };
                for ty in y0.max(fy0)..=y1.min(fy1) {
                    for tx in x0.max(fx0)..=x1.min(fx1) {
                        per_instance[i as usize].push(ty * fine.cols + tx);
                    }
                }
            }
        }

        // ---- count → prefix → scatter, partitioned.
        let n = fine.count();
        let chunk = instances.len().div_ceil(partitions).max(1);
        let ranges: Vec<(usize, usize)> = (0..partitions)
            .map(|p| {
                let lo = (p * chunk).min(instances.len());
                let hi = ((p + 1) * chunk).min(instances.len());
                (lo, hi)
            })
            .collect();

        // counts[p][t]: how many commands partition p contributes to tile t.
        let mut counts: Vec<Vec<u32>> = vec![vec![0; n]; ranges.len()];
        for (p, &(lo, hi)) in ranges.iter().enumerate() {
            for tiles in per_instance.iter().take(hi).skip(lo) {
                for &t in tiles {
                    counts[p][t as usize] += 1;
                }
            }
        }

        // Offsets: tiles in index order, and within a tile the partitions in
        // partition order. That second clause is what keeps a tile's run in
        // painter order at any partition count.
        let mut offsets = Vec::with_capacity(n + 1);
        let mut cursors: Vec<Vec<u32>> = vec![vec![0; n]; ranges.len()];
        let mut running = 0u32;
        offsets.push(0);
        for (t, cursor) in (0..n).zip(std::iter::repeat(())).map(|(t, ())| (t, t)) {
            for p in 0..ranges.len() {
                cursors[p][cursor] = running;
                running += counts[p][t];
            }
            offsets.push(running);
        }

        let mut draws = vec![0u32; running as usize];
        for (p, &(lo, hi)) in ranges.iter().enumerate() {
            for (i, tiles) in per_instance.iter().enumerate().take(hi).skip(lo) {
                for &t in tiles {
                    let slot = cursors[p][t as usize] as usize;
                    draws[slot] = i as u32;
                    cursors[p][t as usize] += 1;
                }
            }
        }

        // ---- §10.4's class, per command.
        let mut flags = vec![CLASS_PARTIAL; draws.len()];
        for t in 0..n {
            let rect = fine.rect(t, viewport);
            for k in offsets[t] as usize..offsets[t + 1] as usize {
                let inst = &instances[draws[k] as usize];
                if covers_tile(plan, inst, map, rect) {
                    flags[k] = CLASS_INTERIOR;
                }
            }
        }

        Binning {
            tiling,
            viewport,
            map,
            plan: plan.identity(),
            fine,
            offsets,
            draws,
            flags,
            macro_hits,
            macro_count: coarse.count(),
        }
    }

    /// The tiling this binning used.
    #[must_use]
    pub fn tiling(&self) -> Tiling {
        self.tiling
    }

    /// The exact viewport this binning covered.
    #[must_use]
    pub(crate) fn viewport(&self) -> Viewport {
        self.viewport
    }

    /// The exact object-to-screen mapping used to classify and scatter draws.
    #[must_use]
    pub(crate) fn map(&self) -> ScreenMap {
        self.map
    }

    /// Whether every command index and class word still names the same
    /// painter-ordered draw.
    #[must_use]
    pub(crate) fn matches_plan(&self, plan: &RenderPlan) -> bool {
        self.plan == plan.identity()
    }

    /// Fine tiles in the grid.
    #[must_use]
    pub fn tile_count(&self) -> usize {
        self.fine.count()
    }

    /// The CSR offsets.
    #[must_use]
    pub fn offsets(&self) -> &[u32] {
        &self.offsets
    }

    /// The CSR instance indices.
    #[must_use]
    pub fn draws(&self) -> &[u32] {
        &self.draws
    }

    /// The per-command class words.
    #[must_use]
    pub fn flags(&self) -> &[u32] {
        &self.flags
    }

    /// One tile's command run, in painter order.
    #[must_use]
    pub fn tile(&self, index: usize) -> &[u32] {
        &self.draws[self.offsets[index] as usize..self.offsets[index + 1] as usize]
    }

    /// One tile's class words, parallel to [`Binning::tile`].
    #[must_use]
    pub fn tile_flags(&self, index: usize) -> &[u32] {
        &self.flags[self.offsets[index] as usize..self.offsets[index + 1] as usize]
    }

    /// The fine tile containing a pixel.
    #[must_use]
    pub fn tile_of(&self, x: u32, y: u32) -> usize {
        let tx = x / self.fine.tile;
        let ty = y / self.fine.tile;
        (ty * self.fine.cols + tx) as usize
    }

    /// How many macrotiles held anything, out of how many there are.
    ///
    /// The two-level scheme's own justification: a pass that accepts every
    /// macrotile is a slower one-level pass, and this is the number that says so.
    #[must_use]
    pub fn macro_occupancy(&self) -> (usize, usize) {
        (self.macro_hits, self.macro_count)
    }

    /// Drop commands that are provably invisible, keeping painter order.
    ///
    /// Walks each tile's run forward, remembers the **last** command that
    /// provably paints the whole tile opaque, and drops everything before it.
    /// The proof obligation per skip is [`covers_tile`] plus an opaque fill at
    /// both ramp ends plus the [`CLASS_INTERIOR`] classification that makes the
    /// covering command's coverage exactly `1`. Anything failing a clause is
    /// simply drawn.
    ///
    /// # Errors
    /// [`BinningError::PlanMismatch`] if `plan` is not the synchronized plan
    /// from which these command lists were built. The binning is left unchanged
    /// on error.
    pub fn prune_occluded(&mut self, plan: &RenderPlan) -> Result<PruneReport, BinningError> {
        if !self.matches_plan(plan) {
            return Err(BinningError::PlanMismatch);
        }
        let instances = plan.shapes().instances();
        let mut report = PruneReport {
            before: self.draws.len(),
            after: 0,
            tiles_touched: 0,
        };
        let mut offsets = Vec::with_capacity(self.offsets.len());
        let mut draws = Vec::with_capacity(self.draws.len());
        let mut flags = Vec::with_capacity(self.flags.len());
        offsets.push(0u32);

        for t in 0..self.fine.count() {
            let lo = self.offsets[t] as usize;
            let hi = self.offsets[t + 1] as usize;
            let mut start = lo;
            for k in lo..hi {
                if self.flags[k] == CLASS_INTERIOR
                    && is_opaque_fill(plan, &instances[self.draws[k] as usize])
                {
                    start = k;
                }
            }
            if start > lo {
                report.tiles_touched += 1;
            }
            draws.extend_from_slice(&self.draws[start..hi]);
            flags.extend_from_slice(&self.flags[start..hi]);
            offsets.push(draws.len() as u32);
        }

        report.after = draws.len();
        self.offsets = offsets;
        self.draws = draws;
        self.flags = flags;
        Ok(report)
    }
}

/// One instance's conservative screen-space AABB.
///
/// Two things beyond placing the hull, and both are load-bearing.
///
/// **The per-draw width expansion (§10.3).** "Conservative slabs are the
/// retained control-polygon hull *plus a per-draw width expansion*, and the
/// expansion is deliberately not retained — width is a *style* property, so a
/// retained slab would be invalidated by a restyle." Without it a stroke is
/// binned to the tiles its *centreline* hull touches, and everything the
/// half-width and the antialiasing band reach beyond that is simply never
/// listed: a stroke whose outline ends on a tile boundary loses `w/2 + aa`
/// pixels of edge, cleanly and silently. It cost nothing until an engine
/// existed to draw the missing band, which is why this arrives with fm-ig3
/// rather than with the binner.
///
/// **Normalization.** A negative `ScreenMap::scale` — the natural way to spell
/// a y-flip — maps `bounds.min` above `bounds.max`, and an inverted AABB makes
/// [`Grid::span`]'s `lo..=hi` loops yield nothing at all, so every instance
/// vanishes from the frame. Taking componentwise min/max costs two comparisons
/// and removes a silent-drop.
fn screen_aabb(plan: &RenderPlan, inst: &Instance, map: ScreenMap) -> Option<[f64; 4]> {
    let shape = plan.shapes().shape(inst.shape)?;
    if shape.segment_count == 0 {
        return None;
    }
    // Shape-local bounds, placed by the instance. Reading `shape.bounds`
    // without `inst.offset` is the bug this signature exists to prevent: every
    // copy of an interned outline would bin at the first copy's position.
    let b = shape.bounds;
    let lo = map.place(b.min, inst.offset);
    let hi = map.place(b.max, inst.offset);
    let pad = stroke_expansion(plan, inst, map);
    Some([
        lo[0].min(hi[0]) - pad,
        lo[1].min(hi[1]) - pad,
        lo[0].max(hi[0]) + pad,
        lo[1].max(hi[1]) + pad,
    ])
}

/// How far beyond its outline a draw's stroke can reach, in screen pixels.
///
/// The widest half-width the ramp attains plus the antialiasing band — the same
/// pad [`crate::stroke::segment_slab`] applies per segment, computed here once
/// per instance. Zero for a style that strokes nothing, so a pure fill is binned
/// to exactly its own hull: the analytic fill's coverage *is* its antialiasing,
/// and it is exactly zero outside the path.
fn stroke_expansion(plan: &RenderPlan, inst: &Instance, map: ScreenMap) -> f64 {
    let Some(style) = plan.styles().get(inst.style) else {
        return 0.0;
    };
    if style.stroke_width <= 0.0 && style.stroke_width_end <= 0.0 {
        return 0.0;
    }
    let widest = crate::stroke::width_px(style.stroke_width, map)
        .max(crate::stroke::width_px(style.stroke_width_end, map));
    0.5 * widest + f64::from(style.anti_alias_width)
}

/// Does this instance's shape contain the whole tile?
///
/// Conservative in the safe direction: a `false` costs a classification and a
/// pruning opportunity, and there is no input for which a `true` is wrong. Only
/// the hints whose containment is a closed form answer `true`; everything else
/// draws.
#[must_use]
pub fn covers_tile(plan: &RenderPlan, inst: &Instance, map: ScreenMap, rect: [f64; 4]) -> bool {
    let Some(shape) = plan.shapes().shape(inst.shape) else {
        return false;
    };
    // A margin so that the containment holds for the whole pixel cell and not
    // merely for the tile's corner coordinates. Tiles are pixel-aligned, so one
    // pixel is enough; the extra is against the predicate's own edge cases
    // rather than the geometry's.
    const MARGIN: f64 = 1.0;
    let grown = [
        rect[0] - MARGIN,
        rect[1] - MARGIN,
        rect[2] + MARGIN,
        rect[3] + MARGIN,
    ];
    match shape.hint {
        Hint::Rect {
            center,
            width,
            height,
        } => {
            let c = map.place(center, inst.offset);
            let hw = 0.5 * width * map.scale;
            let hh = 0.5 * height * map.scale;
            grown[0] >= c[0] - hw
                && grown[2] <= c[0] + hw
                && grown[1] >= c[1] - hh
                && grown[3] <= c[1] + hh
        }
        Hint::RoundedRect {
            center,
            width,
            height,
            corner_radius,
        } => {
            // Conservatively the inscribed rectangle, shrunk by the corner
            // radius on every side: any point inside that is inside the rounded
            // rectangle, whatever the corners do.
            let c = map.place(center, inst.offset);
            let hw = (0.5 * width - corner_radius).max(0.0) * map.scale;
            let hh = (0.5 * height - corner_radius).max(0.0) * map.scale;
            grown[0] >= c[0] - hw
                && grown[2] <= c[0] + hw
                && grown[1] >= c[1] - hh
                && grown[3] <= c[1] + hh
        }
        Hint::Circle { center, radius } | Hint::Dot { center, radius } => {
            let c = map.place(center, inst.offset);
            let r = radius * map.scale;
            // A rectangle is inside a disc iff its farthest corner is.
            let dx = (c[0] - grown[0]).abs().max((c[0] - grown[2]).abs());
            let dy = (c[1] - grown[1]).abs().max((c[1] - grown[3]).abs());
            dx * dx + dy * dy <= r * r
        }
        Hint::Polyline { closed: true } | Hint::General => {
            covers_convex(plan, shape, map, inst.offset, grown)
        }
        _ => false,
    }
}

/// The general case: a closed convex outline containing a rectangle.
///
/// Sound because a quadratic Bézier lies within its control polygon's hull and
/// bulges toward its handle, so a convex, consistently-turning control polygon
/// bounds a convex region — and four corners inside a convex region put the
/// whole rectangle inside it. An arc kernel would answer this exactly; this is
/// the fallback for outlines that carry no such tag.
fn covers_convex(
    plan: &RenderPlan,
    shape: &Shape,
    map: ScreenMap,
    offset: Vec3,
    rect: [f64; 4],
) -> bool {
    let first = shape.first_segment as usize;
    let count = shape.segment_count as usize;
    if count < 2 {
        return false;
    }
    let segs = &plan.segments()[first..first + count];
    let close = |a: Vec3, b: Vec3| {
        (a[0] - b[0]).abs() <= 1e-9 && (a[1] - b[1]).abs() <= 1e-9 && (a[2] - b[2]).abs() <= 1e-9
    };
    if !close(segs[count - 1].p2, segs[0].p0) {
        return false;
    }

    let poly: Vec<[f64; 2]> = segs
        .iter()
        .flat_map(|s| [map.place(s.p0, offset), map.place(s.p1, offset)])
        .collect();
    let n = poly.len();
    let mut sign = 0i32;
    for i in 0..n {
        let (a, b, c) = (poly[i], poly[(i + 1) % n], poly[(i + 2) % n]);
        let cross = (b[0] - a[0]) * (c[1] - b[1]) - (b[1] - a[1]) * (c[0] - b[0]);
        let s = if cross > 1e-9 {
            1
        } else if cross < -1e-9 {
            -1
        } else {
            0
        };
        if s != 0 {
            if sign == 0 {
                sign = s;
            } else if sign != s {
                return false;
            }
        }
    }
    if sign == 0 {
        return false;
    }

    // Every corner strictly inside every edge's half-plane.
    for &(x, y) in &[
        (rect[0], rect[1]),
        (rect[2], rect[1]),
        (rect[0], rect[3]),
        (rect[2], rect[3]),
    ] {
        for i in 0..n {
            let (a, b) = (poly[i], poly[(i + 1) % n]);
            let cross = (b[0] - a[0]) * (y - a[1]) - (b[1] - a[1]) * (x - a[0]);
            if (cross * f64::from(sign)) < 0.0 {
                return false;
            }
        }
    }
    true
}

/// Does this instance composite as pure source wherever it has full coverage?
fn is_opaque_fill(plan: &RenderPlan, inst: &Instance) -> bool {
    plan.styles()
        .get(inst.style)
        .is_some_and(|s| s.fill_rgba[3] == 1.0 && s.fill_rgba_end[3] == 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::RenderPlan;
    use fmn_mobject::{Mobject, RecordBuffer, RecordSchema, ShapeTag, Stage};

    fn rect_mobject(cx: f64, cy: f64, w: f64, h: f64, fill_alpha: f32) -> Mobject {
        let (hw, hh) = (0.5 * w, 0.5 * h);
        let pts: Vec<[f64; 3]> = vec![
            [cx - hw, cy - hh, 0.0],
            [cx, cy - hh, 0.0],
            [cx + hw, cy - hh, 0.0],
            [cx + hw, cy, 0.0],
            [cx + hw, cy + hh, 0.0],
            [cx, cy + hh, 0.0],
            [cx - hw, cy + hh, 0.0],
            [cx - hw, cy, 0.0],
            [cx - hw, cy - hh, 0.0],
        ];
        let mut buffer = RecordBuffer::new(RecordSchema::vmobject(), pts.len());
        for (i, p) in pts.iter().enumerate() {
            buffer.write(i, "point", &[p[0] as f32, p[1] as f32, p[2] as f32]);
            buffer.write(i, "fill_rgba", &[0.2, 0.4, 0.6, fill_alpha]);
            buffer.write(i, "stroke_rgba", &[1.0, 1.0, 1.0, 1.0]);
        }
        Mobject::from_buffer(buffer)
    }

    /// A scene of axis-aligned rectangles, each tagged truthfully.
    fn scene(rects: &[(f64, f64, f64, f64, f32)]) -> Stage {
        let mut stage = Stage::new();
        for &(cx, cy, w, h, a) in rects {
            let mob = stage.add(rect_mobject(cx, cy, w, h, a));
            stage.set_shape(
                mob,
                ShapeTag::Rect {
                    center: [cx, cy, 0.0],
                    width: w,
                    height: h,
                },
            );
            stage.add_to_scene(mob).expect("live");
        }
        stage
    }

    fn synced(stage: &Stage) -> RenderPlan {
        let mut plan = RenderPlan::new();
        plan.sync(stage, 0);
        plan
    }

    fn viewport() -> Viewport {
        Viewport {
            width: 256,
            height: 256,
        }
    }

    /// A stroked horizontal line whose hull ends exactly on a tile boundary.
    ///
    /// Purpose-built for the expansion test: the *centreline* stops at the
    /// boundary, so a slab that is only the hull lists no tile beyond it while
    /// the stroke's half-width and AA band plainly reach into the next one.
    fn stroked_line(x0: f64, y: f64, x1: f64, width: f32) -> Mobject {
        let pts = [[x0, y, 0.0], [0.5 * (x0 + x1), y, 0.0], [x1, y, 0.0]];
        let mut buffer = RecordBuffer::new(RecordSchema::vmobject(), pts.len());
        for (i, p) in pts.iter().enumerate() {
            buffer.write(i, "point", &[p[0] as f32, p[1] as f32, p[2] as f32]);
            buffer.write(i, "stroke_rgba", &[1.0, 1.0, 1.0, 1.0]);
            buffer.write(i, "stroke_width", &[width]);
        }
        Mobject::from_buffer(buffer)
    }

    #[test]
    fn a_stroke_is_binned_to_the_tiles_its_width_reaches() {
        // §10.3's per-draw width expansion, as a test rather than a sentence.
        // The line's hull is the single row y = 64, which lands on a tile
        // boundary; a 400-unit stroke is 4 px wide, so with the AA band the
        // stroke covers y from about 60.5 to 67.5 and must be listed in the tile
        // row above the boundary as well as the one below it.
        //
        // Without the expansion this test finds one tile row instead of two, and
        // the engine draws a stroke with its top edge sliced off.
        let mut stage = Stage::new();
        let mob = stage.add(stroked_line(40.0, 64.0, 200.0, 400.0));
        stage.add_to_scene(mob).expect("live");
        let plan = synced(&stage);
        let tiling = Tiling {
            macro_tile: 128,
            fine_tile: 16,
        };
        let b = Binning::build(&plan, viewport(), tiling, ScreenMap::default());

        let listed = |x: u32, y: u32| !b.tile(b.tile_of(x, y)).is_empty();
        assert!(listed(64, 64), "the centreline's own tile row is missing");
        assert!(
            listed(64, 63),
            "the tile row above the boundary is unlisted, so the stroke's upper \
             half-width and AA band would never be drawn"
        );
    }

    #[test]
    fn a_pure_fill_is_binned_to_exactly_its_own_hull() {
        // The other half of the expansion rule: the analytic fill's coverage IS
        // its antialiasing and is exactly zero outside the path, so a fill with
        // no stroke must not pay for a band it cannot reach into.
        let stage = scene(&[(64.0, 64.0, 32.0, 32.0, 1.0)]);
        let plan = synced(&stage);
        let tiling = Tiling {
            macro_tile: 128,
            fine_tile: 16,
        };
        let b = Binning::build(&plan, viewport(), tiling, ScreenMap::default());
        // The rect spans x,y in [48, 80]: tiles 3..=5 on each axis and no more.
        assert!(!b.tile(b.tile_of(48, 48)).is_empty());
        assert!(
            b.tile(b.tile_of(32, 64)).is_empty(),
            "a fill was padded into a tile its coverage cannot reach"
        );
    }

    #[test]
    fn a_negative_scale_still_bins_every_instance() {
        // A negative `ScreenMap::scale` is the natural spelling of a y-flip, and
        // it maps `bounds.min` above `bounds.max`. An un-normalized AABB makes
        // `Grid::span`'s `lo..=hi` loops empty, so every instance silently
        // vanishes — a blank frame with no error anywhere.
        let stage = scene(&[(-64.0, -64.0, 40.0, 40.0, 1.0)]);
        let plan = synced(&stage);
        let flipped = ScreenMap {
            scale: -1.0,
            origin: [0.0, 0.0],
        };
        let b = Binning::build(&plan, viewport(), Tiling::default(), flipped);
        assert!(
            !b.draws().is_empty(),
            "a y-flip binned the whole scene out of existence"
        );
        assert!(!b.tile(b.tile_of(64, 64)).is_empty());
    }

    #[test]
    fn a_tile_run_is_ascending_which_is_painter_order() {
        let stage = scene(&[
            (60.0, 60.0, 100.0, 100.0, 1.0),
            (80.0, 80.0, 100.0, 100.0, 1.0),
            (100.0, 100.0, 100.0, 100.0, 1.0),
        ]);
        let plan = synced(&stage);
        let b = Binning::build(&plan, viewport(), Tiling::default(), ScreenMap::default());
        assert!(!b.draws().is_empty());
        for t in 0..b.tile_count() {
            let run = b.tile(t);
            assert!(
                run.windows(2).all(|w| w[0] < w[1]),
                "tile {t} out of painter order: {run:?}"
            );
        }
    }

    #[test]
    fn binning_is_identical_at_every_partition_count() {
        // §10.5's determinism obligation, made testable without threads: a
        // partition is a slice of the painter-ordered instance list, and no
        // result may depend on which slice would have finished first. The odd
        // counts matter — they are the ones that do not divide the work evenly.
        let stage = scene(&[
            (40.0, 40.0, 60.0, 60.0, 1.0),
            (90.0, 50.0, 80.0, 40.0, 0.5),
            (150.0, 150.0, 120.0, 90.0, 1.0),
            (30.0, 200.0, 50.0, 50.0, 0.25),
            (200.0, 60.0, 70.0, 70.0, 1.0),
        ]);
        let plan = synced(&stage);
        let serial = Binning::build_partitioned(
            &plan,
            viewport(),
            Tiling::default(),
            ScreenMap::default(),
            1,
        );
        for partitions in [1, 2, 3, 4, 5, 7, 16, 64] {
            let parallel = Binning::build_partitioned(
                &plan,
                viewport(),
                Tiling::default(),
                ScreenMap::default(),
                partitions,
            );
            assert_eq!(
                parallel.offsets(),
                serial.offsets(),
                "offsets differ at {partitions} partitions"
            );
            assert_eq!(
                parallel.draws(),
                serial.draws(),
                "draws differ at {partitions} partitions"
            );
            assert_eq!(
                parallel.flags(),
                serial.flags(),
                "classes differ at {partitions} partitions"
            );
        }
    }

    /// The ordered commands that can actually reach one pixel.
    ///
    /// Binning is *conservative*: a tile lists every instance whose bounds meet
    /// the tile, so a coarser tile legitimately lists commands that miss the
    /// pixel. The invariant tiling owes is therefore not "the same list" — it is
    /// that the list always **contains** every command that can reach the pixel,
    /// in painter order, and that filtering to those gives the same answer at
    /// any tile size. (The first version of this test compared raw lists and
    /// failed for exactly the right reason.)
    fn relevant_at(b: &Binning, plan: &RenderPlan, map: ScreenMap, x: u32, y: u32) -> Vec<u32> {
        let px = f64::from(x) + 0.5;
        let py = f64::from(y) + 0.5;
        let instances = plan.shapes().instances();
        b.tile(b.tile_of(x, y))
            .iter()
            .copied()
            .filter(|&d| {
                screen_aabb(plan, &instances[d as usize], map)
                    .is_some_and(|a| px >= a[0] && px <= a[2] && py >= a[1] && py <= a[3])
            })
            .collect()
    }

    /// The same thing computed without any binning at all — the ground truth a
    /// binner must reproduce.
    fn relevant_unbinned(plan: &RenderPlan, map: ScreenMap, x: u32, y: u32) -> Vec<u32> {
        let px = f64::from(x) + 0.5;
        let py = f64::from(y) + 0.5;
        plan.shapes()
            .instances()
            .iter()
            .enumerate()
            .filter(|(_, inst)| {
                screen_aabb(plan, inst, map)
                    .is_some_and(|a| px >= a[0] && px <= a[2] && py >= a[1] && py <= a[3])
            })
            .map(|(i, _)| i as u32)
            .collect()
    }

    #[test]
    fn tile_size_is_a_scheduling_choice_and_changes_no_command() {
        // §17.4: "tile and macrotile dimensions… never by semantics". Every
        // tiling must deliver every relevant command to every pixel, in painter
        // order, and agree with the unbinned ground truth.
        let stage = scene(&[
            (50.0, 50.0, 90.0, 90.0, 1.0),
            (120.0, 90.0, 110.0, 60.0, 0.5),
            (180.0, 180.0, 100.0, 100.0, 1.0),
        ]);
        let plan = synced(&stage);
        let map = ScreenMap::default();
        for tiling in [
            Tiling {
                macro_tile: 128,
                fine_tile: 16,
            },
            Tiling {
                macro_tile: 128,
                fine_tile: 8,
            },
            Tiling {
                macro_tile: 64,
                fine_tile: 32,
            },
            Tiling {
                macro_tile: 256,
                fine_tile: 4,
            },
            Tiling {
                macro_tile: 32,
                fine_tile: 32,
            },
        ] {
            let b = Binning::build(&plan, viewport(), tiling, map);
            for y in (0..256).step_by(7) {
                for x in (0..256).step_by(5) {
                    assert_eq!(
                        relevant_at(&b, &plan, map, x, y),
                        relevant_unbinned(&plan, map, x, y),
                        "pixel ({x},{y}) under {tiling:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_macrotile_pass_actually_prunes() {
        // A two-level scheme whose first level accepts everything is a slower
        // one-level scheme. One small object in a large viewport must leave most
        // macrotiles empty.
        let stage = scene(&[(20.0, 20.0, 20.0, 20.0, 1.0)]);
        let plan = synced(&stage);
        let b = Binning::build(&plan, viewport(), Tiling::default(), ScreenMap::default());
        let (hits, total) = b.macro_occupancy();
        assert!(total > 1, "the viewport must span several macrotiles");
        assert!(
            hits < total,
            "the macrotile pass pruned nothing: {hits}/{total}"
        );
    }

    #[test]
    fn an_off_screen_object_produces_no_command() {
        // The difference between "not visible" and "drawn at the edge": a
        // clamped span would put a command in the border tiles.
        let stage = scene(&[(-500.0, -500.0, 40.0, 40.0, 1.0)]);
        let plan = synced(&stage);
        let b = Binning::build(&plan, viewport(), Tiling::default(), ScreenMap::default());
        assert!(b.draws().is_empty(), "{:?}", b.draws());
    }

    #[test]
    fn a_tile_inside_an_opaque_rectangle_is_classified_interior() {
        let stage = scene(&[(128.0, 128.0, 200.0, 200.0, 1.0)]);
        let plan = synced(&stage);
        let b = Binning::build(&plan, viewport(), Tiling::default(), ScreenMap::default());
        // The centre tile is deep inside; a corner tile is not.
        let centre = b.tile_of(128, 128);
        assert_eq!(b.tile_flags(centre), &[CLASS_INTERIOR]);
        let corner = b.tile_of(30, 30);
        assert!(
            b.tile_flags(corner).iter().all(|f| *f == CLASS_PARTIAL),
            "the edge tile cannot be interior"
        );
    }

    #[test]
    fn pruning_drops_the_hidden_layer_and_keeps_the_visible_suffix() {
        // The equivalence the bead asks for, stated the way it can be checked
        // before an engine exists: for every pixel, the *visible suffix* of the
        // command list — everything from the last opaque full-cover onward — is
        // unchanged. Nothing before it can reach a pixel, whatever the shading.
        let stage = scene(&[
            (128.0, 128.0, 60.0, 60.0, 1.0),
            (128.0, 128.0, 220.0, 220.0, 1.0),
            (128.0, 128.0, 40.0, 40.0, 0.5),
        ]);
        let plan = synced(&stage);
        let before = Binning::build(&plan, viewport(), Tiling::default(), ScreenMap::default());
        let mut after = before.clone();
        let report = after.prune_occluded(&plan).expect("matching plan");

        assert!(report.removed_fraction() > 0.0, "{report:?}");
        for y in (0..256).step_by(3) {
            for x in (0..256).step_by(3) {
                let a = visible_suffix(&before, &plan, x, y);
                let b = visible_suffix(&after, &plan, x, y);
                assert_eq!(a, b, "pixel ({x},{y}) sees a different visible suffix");
            }
        }
    }

    /// Everything from the last opaque full-tile cover onward.
    fn visible_suffix(b: &Binning, plan: &RenderPlan, x: u32, y: u32) -> Vec<u32> {
        let t = b.tile_of(x, y);
        let run = b.tile(t);
        let flags = b.tile_flags(t);
        let instances = plan.shapes().instances();
        let mut start = 0;
        for (k, &d) in run.iter().enumerate() {
            if flags[k] == CLASS_INTERIOR && is_opaque_fill(plan, &instances[d as usize]) {
                start = k;
            }
        }
        run[start..].to_vec()
    }

    #[test]
    fn a_translucent_cover_prunes_nothing() {
        // The adversarial case the rule exists for: a stack under something that
        // blends. Every command still reaches the compositor.
        let stage = scene(&[
            (128.0, 128.0, 60.0, 60.0, 1.0),
            (128.0, 128.0, 220.0, 220.0, 0.5),
        ]);
        let plan = synced(&stage);
        let mut b = Binning::build(&plan, viewport(), Tiling::default(), ScreenMap::default());
        let report = b.prune_occluded(&plan).expect("matching plan");
        assert_eq!(report.before, report.after, "{report:?}");
    }

    #[test]
    fn pruning_refuses_a_stale_plan_without_mutating_the_binning() {
        let source = scene(&[(40.0, 40.0, 60.0, 60.0, 1.0)]);
        let source_plan = synced(&source);
        let mut binning = Binning::build(
            &source_plan,
            viewport(),
            Tiling::default(),
            ScreenMap::default(),
        );
        let before = binning.clone();

        // Same table and instance cardinalities, different placement. Index-only
        // validation would accept this and prune using the wrong rectangle.
        let current = scene(&[(200.0, 200.0, 60.0, 60.0, 1.0)]);
        let current_plan = synced(&current);
        assert_eq!(
            binning.prune_occluded(&current_plan),
            Err(BinningError::PlanMismatch)
        );
        assert_eq!(binning, before, "a failed prune must be transactional");
    }

    #[test]
    fn an_opaque_cover_that_does_not_reach_a_tile_prunes_only_where_it_does() {
        // Painter-order safety at the boundary: the cover is opaque and full in
        // the tiles it fills, and irrelevant in the tiles it misses.
        let stage = scene(&[
            (40.0, 40.0, 60.0, 60.0, 1.0),
            (200.0, 200.0, 60.0, 60.0, 1.0),
            (200.0, 200.0, 100.0, 100.0, 1.0),
        ]);
        let plan = synced(&stage);
        let before = Binning::build(&plan, viewport(), Tiling::default(), ScreenMap::default());
        let mut after = before.clone();
        after.prune_occluded(&plan).expect("matching plan");

        // The lone rectangle at (40,40) is instance 0 and nothing covers it.
        let lone = before.tile_of(40, 40);
        assert!(before.tile(lone).contains(&0));
        assert!(
            after.tile(lone).contains(&0),
            "an uncovered draw was dropped"
        );
        // Under the big cover, the small one is gone.
        let covered = before.tile_of(200, 200);
        assert!(before.tile(covered).contains(&1));
        assert!(!after.tile(covered).contains(&1));
    }

    #[test]
    fn pruning_never_drops_the_covering_command_itself() {
        let stage = scene(&[
            (128.0, 128.0, 40.0, 40.0, 1.0),
            (128.0, 128.0, 200.0, 200.0, 1.0),
        ]);
        let plan = synced(&stage);
        let mut b = Binning::build(&plan, viewport(), Tiling::default(), ScreenMap::default());
        b.prune_occluded(&plan).expect("matching plan");
        let centre = b.tile_of(128, 128);
        assert_eq!(b.tile(centre), &[1], "only the cover survives, and it does");
    }

    #[test]
    fn pruning_is_identical_at_every_partition_count() {
        // Pruning consumes the classification, so it inherits the determinism
        // obligation and has to be checked under it.
        let stage = scene(&[
            (128.0, 128.0, 60.0, 60.0, 1.0),
            (60.0, 60.0, 40.0, 40.0, 1.0),
            (128.0, 128.0, 240.0, 240.0, 1.0),
            (100.0, 100.0, 30.0, 30.0, 0.5),
        ]);
        let plan = synced(&stage);
        let mut reference = Binning::build_partitioned(
            &plan,
            viewport(),
            Tiling::default(),
            ScreenMap::default(),
            1,
        );
        reference
            .prune_occluded(&plan)
            .expect("matching reference plan");
        for partitions in [2, 3, 8, 32] {
            let mut other = Binning::build_partitioned(
                &plan,
                viewport(),
                Tiling::default(),
                ScreenMap::default(),
                partitions,
            );
            other.prune_occluded(&plan).expect("matching plan");
            assert_eq!(other.offsets(), reference.offsets());
            assert_eq!(other.draws(), reference.draws());
            assert_eq!(other.flags(), reference.flags());
        }
    }

    #[test]
    fn a_concave_outline_never_claims_to_cover_a_tile() {
        // The convexity clause is load-bearing: an L covers some of its bounding
        // tiles and not others, and the corner test alone cannot tell which.
        let l: Vec<[f64; 3]> = [
            [20.0, 20.0],
            [120.0, 20.0],
            [220.0, 20.0],
            [220.0, 70.0],
            [220.0, 120.0],
            [120.0, 120.0],
            [120.0, 170.0],
            [120.0, 220.0],
            [70.0, 220.0],
            [20.0, 220.0],
            [20.0, 120.0],
            [20.0, 20.0],
        ]
        .iter()
        .map(|p| [p[0], p[1], 0.0])
        .collect();
        let mut buffer = RecordBuffer::new(RecordSchema::vmobject(), l.len());
        for (i, p) in l.iter().enumerate() {
            buffer.write(i, "point", &[p[0] as f32, p[1] as f32, p[2] as f32]);
            buffer.write(i, "fill_rgba", &[0.2, 0.4, 0.6, 1.0]);
        }
        let mut stage = Stage::new();
        let mob = stage.add(Mobject::from_buffer(buffer));
        stage.add_to_scene(mob).expect("live");
        let plan = synced(&stage);
        let b = Binning::build(&plan, viewport(), Tiling::default(), ScreenMap::default());
        assert!(
            b.flags().iter().all(|f| *f == CLASS_PARTIAL),
            "a concave outline claimed to cover a tile"
        );
    }

    #[test]
    fn a_circle_covers_a_tile_only_when_its_farthest_corner_is_inside() {
        let mut stage = Stage::new();
        let mob = stage.add(rect_mobject(128.0, 128.0, 200.0, 200.0, 1.0));
        stage.set_shape(
            mob,
            ShapeTag::Circle {
                center: [128.0, 128.0, 0.0],
                radius: 100.0,
            },
        );
        stage.add_to_scene(mob).expect("live");
        let plan = synced(&stage);
        let b = Binning::build(&plan, viewport(), Tiling::default(), ScreenMap::default());
        // Dead centre: inside. On the diagonal near the rim: not.
        assert_eq!(b.tile_flags(b.tile_of(128, 128)), &[CLASS_INTERIOR]);
        let rim = b.tile_of(128 + 96, 128);
        assert!(b.tile_flags(rim).iter().all(|f| *f == CLASS_PARTIAL));
    }

    #[test]
    fn two_instances_of_one_outline_bin_at_their_own_positions() {
        // The regression test for a real bug this bead surfaced. `shape_digest`
        // excludes position — that is what makes interning find anything — but
        // compiled outlines were being stored at the first mobject's ABSOLUTE
        // coordinates. Two 60x60 rectangles at (40,40) and (200,200) therefore
        // shared a shape whose bounds were the first one's, so the second binned
        // on top of the first and an opaque cover pruned a draw that was
        // nowhere near it. Outlines are shape-local now, and the instance offset
        // places them.
        let stage = scene(&[
            (40.0, 40.0, 60.0, 60.0, 1.0),
            (200.0, 200.0, 60.0, 60.0, 1.0),
        ]);
        let plan = synced(&stage);
        assert_eq!(
            plan.shapes().shapes().len(),
            1,
            "the two rectangles are one outline — the interning must still fire"
        );

        let b = Binning::build(&plan, viewport(), Tiling::default(), ScreenMap::default());
        let near = b.tile_of(40, 40);
        let far = b.tile_of(200, 200);
        assert_eq!(b.tile(near), &[0], "instance 0 belongs at (40,40)");
        assert_eq!(b.tile(far), &[1], "instance 1 belongs at (200,200)");
        assert!(
            !b.tile(near).contains(&1) && !b.tile(far).contains(&0),
            "neither may bin at the other's position"
        );
    }

    #[test]
    fn an_empty_plan_bins_to_an_empty_but_valid_csr() {
        let plan = RenderPlan::new();
        let b = Binning::build(&plan, viewport(), Tiling::default(), ScreenMap::default());
        assert_eq!(b.offsets().len(), b.tile_count() + 1);
        assert!(b.draws().is_empty());
        assert_eq!(*b.offsets().last().expect("non-empty"), 0);
    }

    #[test]
    fn the_csr_is_well_formed() {
        let stage = scene(&[
            (60.0, 60.0, 100.0, 100.0, 1.0),
            (180.0, 180.0, 90.0, 90.0, 0.5),
        ]);
        let plan = synced(&stage);
        let b = Binning::build(&plan, viewport(), Tiling::default(), ScreenMap::default());
        assert_eq!(b.offsets().len(), b.tile_count() + 1);
        assert_eq!(
            *b.offsets().last().expect("non-empty") as usize,
            b.draws().len()
        );
        assert_eq!(b.flags().len(), b.draws().len());
        assert!(b.offsets().windows(2).all(|w| w[0] <= w[1]));
    }
}
