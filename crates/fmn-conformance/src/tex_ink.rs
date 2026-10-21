//! Ink-fidelity oracle for native TeX (fm-tex-ink-oracle-5ehf).
//!
//! The box oracles (`tex_reference_boxes`, `tex_corpus_boxes`,
//! `tex_reference_widths`) measure where TeX puts the ink. This one measures
//! the ink itself: a box can match while the glyph inside it is the wrong
//! letter (θ drawn as ϑ) or the wrong weight (a sans fallback beside
//! Computer Modern).
//!
//! The Reference side is the pinned Reference's own `Tex`/`TexText` outline
//! geometry, captured once by `scripts/capture_tex_ink.py` through a private
//! TeX cache. Nothing here invokes LaTeX (D-03). Both sides are drawn by the
//! certified engine, white fill on black, at the 1080p density
//! ([`PX_PER_UNIT`]), so what is compared is typesetting, not two
//! rasterizers.
//!
//! - **Registration.** A similarity transform (uniform scale and
//!   translation) maps the native ink box onto the Reference's: centre onto
//!   centre, scaled by the geometric mean of the width and height ratios
//!   ([`ink_box`], the exact extrema of the quadratic outlines). The fitted
//!   scale is reported on its own: it is the box metric, and the shape
//!   metrics below are free of it. Matching the area centroids and radii of
//!   gyration instead lets a glyph of the wrong weight drag every other
//!   glyph off: in `f(x) = x^2 + 1`, fmd-math's light `+` and `=` moved the
//!   centroid 6 px and the row's IoU fell to 0.12 with every other glyph
//!   right.
//! - **Mass ratio**: the native raster's total coverage over the
//!   Reference's, after registration. Below 1 the ink is lighter.
//! - **Hausdorff** ([`hausdorff_px`]): the symmetric Hausdorff distance
//!   between the two ink boundaries at half coverage, in pixels, through an
//!   exact Euclidean distance transform. It is a worst case, so a missing or
//!   extra piece shows.
//! - **Sliced Wasserstein-1** ([`sliced_w1_px`]): earth mover's distance
//!   between the normalized coverage distributions, in pixels, as the mean
//!   exact 1-D W1 of their projections on 64 directions, times π/2. It is a
//!   lower bound on the 2-D W1 that equals it for translations and radial
//!   scalings, and it uses every pixel at full resolution. It moves in
//!   proportion to a displacement, sub-pixel ones included, and responds to
//!   shape and stroke weight.
//!
//!   Entropic Sinkhorn, the first design, could not serve: on formula-sized
//!   grids at a sub-pixel ε, single-grid and multiscale Sinkhorn alike needed
//!   thousands to tens of thousands of iterations (measured 2026-10-21), and
//!   at a pixel-scale ε the divergence reads a 3 px shift as 1.6 px. An exact
//!   2-D W1 over binned cells kept a quantization floor near the signal (a
//!   pure 0.3 px shift of a formula read 0.7 px at 4 px cells).
//! - **IoU** ([`soft_iou`]): Σ min / Σ max of the two coverages after
//!   registration.

use fmn_core::constants::{BLACK, WHITE};
use fmn_core::types::Vec3;
use fmn_library::style::Style;
use fmn_library::vmobject::{VMobject, v_group};
use fmn_mobject::Stage;
use fmn_render::{
    Binning, EngineIdentity, FrameConfig, FrameJob, MonoTable, RenderPlan, ScreenMap, Tiling,
    Viewport,
};
use std::fmt::Write as _;

/// The public Reference ink: the box oracle's authored formulas plus a few
/// authored `TexText` strings, named by source.
pub const INK_FIXTURE: &str = include_str!("../fixtures/tex_ink_reference.v1.txt");

/// Raster density: 1080 pixels over the Reference's 8-unit frame height.
pub const PX_PER_UNIT: f64 = 135.0;
/// Empty pixels around the union of both inks.
pub const PAD_PX: f64 = 6.0;
/// Directions of the sliced W1, equispaced over half a turn.
pub const SLICED_W1_DIRECTIONS: usize = 64;
/// The fixture's coordinate unit, in scene units.
const FIXTURE_UNIT: f64 = 1e-4;
/// The certified configuration's tiles (the scene goldens').
const TILING: Tiling = Tiling {
    macro_tile: 128,
    fine_tile: 16,
};

/// Each family member's shared-anchor quadratic point run.
pub type Members = Vec<Vec<Vec3>>;

/// Which builder made the row: `Tex` or `TexText`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InkKind {
    /// `Tex(source)`: display mathematics.
    Math,
    /// `TexText(source)`: text mode.
    Text,
}

impl InkKind {
    /// The fixture's spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Math => "math",
            Self::Text => "text",
        }
    }
}

/// One captured Reference formula.
#[derive(Debug, Clone)]
pub struct InkRow {
    /// `Tex` or `TexText`.
    pub kind: InkKind,
    /// The source (public set) or `sha256(mode NUL text)` (private set).
    pub key: String,
    /// The width capture's font class: plain, traced or fallback.
    pub class: String,
    /// The Reference's outlines.
    pub members: Members,
}

/// Parse an ink capture (`scripts/capture_tex_ink.py`'s format).
///
/// # Errors
/// A row that is not four tab-separated fields, an unknown kind, or a member
/// run that is not whole x/y integer pairs.
pub fn parse_ink_fixture(text: &str) -> Result<Vec<InkRow>, String> {
    let mut rows = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        let [kind, key, class, parts] = fields[..] else {
            return Err(format!("line {}: expected 4 fields", index + 1));
        };
        let kind = match kind {
            "math" => InkKind::Math,
            "text" => InkKind::Text,
            other => return Err(format!("line {}: unknown kind {other:?}", index + 1)),
        };
        let mut members = Vec::new();
        for part in parts.split('|') {
            let values = part
                .split_ascii_whitespace()
                .map(str::parse::<i64>)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| format!("line {}: {error}", index + 1))?;
            if values.is_empty() || values.len() % 2 != 0 {
                return Err(format!("line {}: odd coordinate count", index + 1));
            }
            let (mut x, mut y) = (0_i64, 0_i64);
            let points = values
                .as_chunks::<2>()
                .0
                .iter()
                .map(|[dx, dy]| {
                    x += dx;
                    y += dy;
                    [unit(x), unit(y), 0.0]
                })
                .collect();
            members.push(points);
        }
        rows.push(InkRow {
            kind,
            key: key.to_owned(),
            class: class.to_owned(),
            members,
        });
    }
    Ok(rows)
}

#[allow(clippy::cast_precision_loss)]
fn unit(value: i64) -> f64 {
    value as f64 * FIXTURE_UNIT
}

/// The point runs of every family member with points, in family order.
#[must_use]
pub fn members_of(vmob: &VMobject) -> Members {
    let mut out = Vec::new();
    collect_members(vmob, &mut out);
    out
}

fn collect_members(vmob: &VMobject, out: &mut Members) {
    if !vmob.points().is_empty() {
        out.push(vmob.points().to_vec());
    }
    for child in vmob.children() {
        collect_members(child, out);
    }
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Area moments of filled outlines: the ink's mass, centroid and radius of
/// gyration, in scene units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AreaMoments {
    /// Total filled area.
    pub mass: f64,
    /// Area centroid.
    pub centroid: [f64; 2],
    /// Radius of gyration about the centroid: sqrt(∫|p - c|² dA / area).
    pub spread: f64,
}

/// The subpaths of a shared-anchor run, by the Reference's rule
/// (`get_subpath_end_indices_from_points`): an anchor ends a subpath when
/// the handle after it sits on it and the next anchor is distinct.
fn subpaths(points: &[Vec3]) -> Vec<&[Vec3]> {
    let mut out = Vec::new();
    if points.len() < 3 {
        return out;
    }
    let mut start = 0;
    let mut k = 0;
    while k + 2 < points.len() {
        let (a0, h, a1) = (points[k], points[k + 1], points[k + 2]);
        let is_end = a0 == h && (0..3).any(|i| (h[i] - a1[i]).abs() > 1e-4);
        if is_end {
            out.push(&points[start..=k]);
            start = k + 2;
        }
        k += 2;
    }
    out.push(&points[start..]);
    out
}

/// Five-point Gauss–Legendre nodes and weights on [0, 1]: exact for the
/// degree-7 integrands below.
const GAUSS: [(f64, f64); 5] = [
    (0.046_910_077_030_668, 0.118_463_442_528_095),
    (0.230_765_344_947_158, 0.239_314_335_249_683),
    (0.5, 0.284_444_444_444_444),
    (0.769_234_655_052_842, 0.239_314_335_249_683),
    (0.953_089_922_969_332, 0.118_463_442_528_095),
];

/// Signed ∫∫ [1, x, y, x², y²] dA over one member by Green's theorem: each
/// subpath's quadratic curves, closed by a line back to its start.
fn member_integrals(points: &[Vec3]) -> [f64; 5] {
    let mut sums = [0.0; 5];
    let mut curve = |p0: [f64; 2], p1: [f64; 2], p2: [f64; 2]| {
        for &(t, w) in &GAUSS {
            let s = 1.0 - t;
            let x = s * s * p0[0] + 2.0 * s * t * p1[0] + t * t * p2[0];
            let y = s * s * p0[1] + 2.0 * s * t * p1[1] + t * t * p2[1];
            let dx = 2.0 * (s * (p1[0] - p0[0]) + t * (p2[0] - p1[0]));
            let dy = 2.0 * (s * (p1[1] - p0[1]) + t * (p2[1] - p1[1]));
            // ∫∫1 = ∮x dy, ∫∫x = ∮x²/2 dy, ∫∫y = -∮y²/2 dx,
            // ∫∫x² = ∮x³/3 dy, ∫∫y² = -∮y³/3 dx.
            sums[0] += w * x * dy;
            sums[1] += w * 0.5 * x * x * dy;
            sums[2] -= w * 0.5 * y * y * dx;
            sums[3] += w * x * x * x * dy / 3.0;
            sums[4] -= w * y * y * y * dx / 3.0;
        }
    };
    for path in subpaths(points) {
        let xy = |p: Vec3| [p[0], p[1]];
        let mut k = 0;
        while k + 2 < path.len() {
            curve(xy(path[k]), xy(path[k + 1]), xy(path[k + 2]));
            k += 2;
        }
        let (first, last) = (xy(path[0]), xy(path[path.len() - 1]));
        if first != last {
            let mid = [0.5 * (first[0] + last[0]), 0.5 * (first[1] + last[1])];
            curve(last, mid, first);
        }
    }
    sums
}

/// The ink's exact area moments. Each member's sign is normalized, so a
/// glyph wound either way adds its area (the fill is nonzero-winding).
/// `None` when the outlines enclose no area.
#[must_use]
pub fn area_moments(members: &[Vec<Vec3>]) -> Option<AreaMoments> {
    let mut total = [0.0; 5];
    for points in members {
        let sums = member_integrals(points);
        let sign = if sums[0] < 0.0 { -1.0 } else { 1.0 };
        for (t, s) in total.iter_mut().zip(sums) {
            *t += sign * s;
        }
    }
    let mass = total[0];
    if mass <= 1e-12 {
        return None;
    }
    let centroid = [total[1] / mass, total[2] / mass];
    let second =
        (total[3] + total[4]) / mass - centroid[0] * centroid[0] - centroid[1] * centroid[1];
    Some(AreaMoments {
        mass,
        centroid,
        spread: fmn_dmath::sqrt(second.max(0.0)),
    })
}

/// A uniform scale and a translation: `p ↦ scale · p + shift`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Similarity {
    /// Uniform scale.
    pub scale: f64,
    /// Translation, scene units.
    pub shift: [f64; 2],
}

/// The exact extent of filled quadratic outlines: every curve's endpoints
/// and its interior extremum on each axis, where the derivative vanishes.
/// `None` without a curve.
#[must_use]
pub fn ink_box(members: &[Vec<Vec3>]) -> Option<([f64; 2], [f64; 2])> {
    let mut min = [f64::INFINITY; 2];
    let mut max = [f64::NEG_INFINITY; 2];
    let mut grow = |x: f64, axis: usize| {
        min[axis] = min[axis].min(x);
        max[axis] = max[axis].max(x);
    };
    for points in members {
        let mut k = 0;
        while k + 2 < points.len() {
            let (p0, p1, p2) = (points[k], points[k + 1], points[k + 2]);
            for axis in 0..2 {
                grow(p0[axis], axis);
                grow(p2[axis], axis);
                let curvature = p0[axis] - 2.0 * p1[axis] + p2[axis];
                if curvature != 0.0 {
                    let t = (p0[axis] - p1[axis]) / curvature;
                    if t > 0.0 && t < 1.0 {
                        let u = 1.0 - t;
                        grow(
                            u * u * p0[axis] + 2.0 * u * t * p1[axis] + t * t * p2[axis],
                            axis,
                        );
                    }
                }
            }
            k += 2;
        }
    }
    (min[0] <= max[0]).then_some((min, max))
}

impl Similarity {
    /// The transform taking the `candidate` box's centre onto the
    /// `reference` box's, scaled by the geometric mean of the width and
    /// height ratios (the width ratio alone for a flat box).
    #[must_use]
    pub fn register(reference: ([f64; 2], [f64; 2]), candidate: ([f64; 2], [f64; 2])) -> Self {
        let size = |b: ([f64; 2], [f64; 2])| [b.1[0] - b.0[0], b.1[1] - b.0[1]];
        let (r, c) = (size(reference), size(candidate));
        let ratio = |axis: usize| (c[axis] > 0.0).then(|| r[axis] / c[axis]);
        let scale = match (ratio(0), ratio(1)) {
            (Some(x), Some(y)) => fmn_dmath::sqrt(x * y),
            (Some(x), None) | (None, Some(x)) => x,
            (None, None) => 1.0,
        };
        let centre = |b: ([f64; 2], [f64; 2]), axis: usize| 0.5 * (b.0[axis] + b.1[axis]);
        Self {
            scale,
            shift: [
                centre(reference, 0) - scale * centre(candidate, 0),
                centre(reference, 1) - scale * centre(candidate, 1),
            ],
        }
    }

    /// The outlines, transformed.
    #[must_use]
    pub fn apply(&self, members: &[Vec<Vec3>]) -> Members {
        members
            .iter()
            .map(|points| {
                points
                    .iter()
                    .map(|p| {
                        [
                            self.scale * p[0] + self.shift[0],
                            self.scale * p[1] + self.shift[1],
                            p[2],
                        ]
                    })
                    .collect()
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Rasterization
// ---------------------------------------------------------------------------

/// A pixel grid over the scene plane at [`PX_PER_UNIT`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Grid {
    /// Columns.
    pub width: usize,
    /// Rows.
    pub height: usize,
    /// Scene x of the left edge.
    pub left: f64,
    /// Scene y of the top edge.
    pub top: f64,
}

impl Grid {
    /// The grid around every point of `sets`, padded by [`PAD_PX`]. `None`
    /// when there are no points.
    #[must_use]
    pub fn around(sets: &[&[Vec<Vec3>]]) -> Option<Self> {
        let mut min = [f64::INFINITY; 2];
        let mut max = [f64::NEG_INFINITY; 2];
        for p in sets.iter().flat_map(|set| set.iter().flatten()) {
            for i in 0..2 {
                min[i] = min[i].min(p[i]);
                max[i] = max[i].max(p[i]);
            }
        }
        if !(min[0].is_finite() && max[0].is_finite()) {
            return None;
        }
        let pad = PAD_PX / PX_PER_UNIT;
        let size = |lo: f64, hi: f64| {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let n = ((hi - lo) * PX_PER_UNIT + 2.0 * PAD_PX).ceil() as usize;
            n.max(1)
        };
        Some(Self {
            width: size(min[0], max[0]),
            height: size(min[1], max[1]),
            left: min[0] - pad,
            top: max[1] + pad,
        })
    }

    fn screen_map(&self) -> ScreenMap {
        ScreenMap {
            scale: PX_PER_UNIT,
            origin: [-self.left * PX_PER_UNIT, self.top * PX_PER_UNIT],
            y_up: true,
        }
    }
}

/// Coverage in [0, 1], row-major, row 0 at the top.
#[derive(Debug, Clone, PartialEq)]
pub struct Mask {
    /// Columns.
    pub width: usize,
    /// Rows.
    pub height: usize,
    /// One value per pixel.
    pub coverage: Vec<f64>,
}

/// Fill `members` white on black over `grid` through the certified engine
/// and read coverage from the linear frame.
///
/// # Errors
/// The grid is too large for a `u32` viewport, or the engine refuses the
/// frame.
pub fn rasterize(members: &[Vec<Vec3>], grid: Grid) -> Result<Mask, String> {
    let style = Style::default()
        .fill(WHITE, 1.0)
        .fill_border_width(0.0)
        .stroke(WHITE, 0.0, 0.0);
    let root = v_group(
        members
            .iter()
            .map(|points| VMobject::from_points(points.clone()).with_style(style)),
    );
    let mut stage = Stage::new();
    let mob = stage.add(root);
    stage.add_to_scene(mob).map_err(|e| e.to_string())?;
    let viewport = Viewport {
        width: u32::try_from(grid.width).map_err(|e| e.to_string())?,
        height: u32::try_from(grid.height).map_err(|e| e.to_string())?,
    };
    let map = grid.screen_map();
    let config = FrameConfig::new(viewport, map, BLACK.to_linear(1.0));
    let mut plan = RenderPlan::new();
    plan.sync(&stage, 0).map_err(|e| e.to_string())?;
    let mono = MonoTable::build(&plan, map).map_err(|e| e.to_string())?;
    let mut binning = Binning::build(&plan, viewport, TILING, map).map_err(|e| e.to_string())?;
    binning.prune_occluded(&plan).map_err(|e| e.to_string())?;
    let frame =
        FrameJob::with_identity(&plan, &mono, &binning, config, EngineIdentity::certified())
            .map_err(|e| e.to_string())?
            .render(1)
            .map_err(|e| e.to_string())?;
    // The raw certified frame is linear-light RGBA in little-endian halves;
    // white over opaque black, its red channel is the coverage.
    let stride = frame.layout().stride(0);
    let bytes = frame.plane(0);
    let mut coverage = Vec::with_capacity(grid.width * grid.height);
    for row in 0..grid.height {
        let line = &bytes[row * stride..row * stride + 8 * grid.width];
        coverage.extend(
            line.as_chunks::<8>()
                .0
                .iter()
                .map(|px| fmn_frame::half::f16_to_f64(u16::from_le_bytes([px[0], px[1]]))),
        );
    }
    Ok(Mask {
        width: grid.width,
        height: grid.height,
        coverage,
    })
}

// ---------------------------------------------------------------------------
// Metrics
// ---------------------------------------------------------------------------

/// Σ min / Σ max of two coverages on one grid: 1 for identical ink.
#[must_use]
pub fn soft_iou(a: &Mask, b: &Mask) -> f64 {
    let (mut low, mut high) = (0.0, 0.0);
    for (&x, &y) in a.coverage.iter().zip(&b.coverage) {
        low += x.min(y);
        high += x.max(y);
    }
    if high > 0.0 { low / high } else { 1.0 }
}

/// Pixels at or above half coverage that touch a pixel below it (or the
/// grid edge): the ink boundary.
fn boundary(mask: &Mask) -> Vec<bool> {
    let (w, h) = (mask.width, mask.height);
    let inside = |c: usize, r: usize| mask.coverage[r * w + c] >= 0.5;
    let mut out = vec![false; w * h];
    for r in 0..h {
        for c in 0..w {
            if !inside(c, r) {
                continue;
            }
            let edge = c == 0 || r == 0 || c + 1 == w || r + 1 == h;
            out[r * w + c] = edge
                || !inside(c - 1, r)
                || !inside(c + 1, r)
                || !inside(c, r - 1)
                || !inside(c, r + 1);
        }
    }
    out
}

/// One-dimensional squared distance transform of `f` (Felzenszwalb and
/// Huttenlocher's lower envelope of parabolas), into `d`.
fn edt_1d(f: &[f64], d: &mut [f64], v: &mut [usize], z: &mut [f64]) {
    let n = f.len();
    if n == 0 {
        return;
    }
    #[allow(clippy::cast_precision_loss)]
    let sq = |q: usize| (q * q) as f64;
    #[allow(clippy::cast_precision_loss)]
    let meet =
        |q: usize, p: usize| ((f[q] + sq(q)) - (f[p] + sq(p))) / (2.0 * (q as f64 - p as f64));
    let mut k = 0;
    v[0] = 0;
    z[0] = f64::NEG_INFINITY;
    z[1] = f64::INFINITY;
    for q in 1..n {
        // z[0] is -inf, so the search stops at the first parabola.
        let mut s = meet(q, v[k]);
        while s <= z[k] {
            k -= 1;
            s = meet(q, v[k]);
        }
        k += 1;
        v[k] = q;
        z[k] = s;
        z[k + 1] = f64::INFINITY;
    }
    k = 0;
    for (q, out) in d.iter_mut().enumerate() {
        #[allow(clippy::cast_precision_loss)]
        let x = q as f64;
        while z[k + 1] < x {
            k += 1;
        }
        let p = v[k];
        #[allow(clippy::cast_precision_loss)]
        let dx = x - p as f64;
        *out = dx * dx + f[p];
    }
}

/// Squared Euclidean distance from every pixel to the nearest `feature`
/// pixel.
fn squared_distance(feature: &[bool], w: usize, h: usize) -> Vec<f64> {
    // Far beyond any squared distance on a grid, yet exact to well below a pixel² in sums.
    const FAR: f64 = 1e12;
    let n = w.max(h);
    let (mut f, mut d) = (vec![0.0; n], vec![0.0; n]);
    let (mut v, mut z) = (vec![0_usize; n], vec![0.0; n + 1]);
    let mut grid: Vec<f64> = feature.iter().map(|&x| if x { 0.0 } else { FAR }).collect();
    for c in 0..w {
        for r in 0..h {
            f[r] = grid[r * w + c];
        }
        edt_1d(&f[..h], &mut d[..h], &mut v, &mut z);
        for r in 0..h {
            grid[r * w + c] = d[r];
        }
    }
    for r in 0..h {
        f[..w].copy_from_slice(&grid[r * w..(r + 1) * w]);
        edt_1d(&f[..w], &mut d[..w], &mut v, &mut z);
        grid[r * w..(r + 1) * w].copy_from_slice(&d[..w]);
    }
    grid
}

/// The symmetric Hausdorff distance between the two half-coverage ink
/// boundaries, in pixels. A side with no boundary at all is the grid
/// diagonal away.
#[must_use]
pub fn hausdorff_px(a: &Mask, b: &Mask) -> f64 {
    let (w, h) = (a.width, a.height);
    let (ea, eb) = (boundary(a), boundary(b));
    #[allow(clippy::cast_precision_loss)]
    let diagonal = fmn_dmath::sqrt((w * w + h * h) as f64);
    if !ea.contains(&true) || !eb.contains(&true) {
        return if ea.contains(&true) == eb.contains(&true) {
            0.0
        } else {
            diagonal
        };
    }
    let (da, db) = (squared_distance(&ea, w, h), squared_distance(&eb, w, h));
    let one_way = |edge: &[bool], dist: &[f64]| {
        edge.iter()
            .zip(dist)
            .filter(|(e, _)| **e)
            .map(|(_, d)| *d)
            .fold(0.0_f64, f64::max)
    };
    fmn_dmath::sqrt(one_way(&ea, &db).max(one_way(&eb, &da)))
}

/// The exact Wasserstein-1 distance between two one-dimensional weighted
/// samples of equal total mass: ∫ |F_a(t) − F_b(t)| dt over their merged,
/// sorted positions.
fn w1_line(samples: &mut [(f64, f64)]) -> f64 {
    samples.sort_by(|x, y| x.0.total_cmp(&y.0));
    let mut cumulative = 0.0;
    let mut total = 0.0;
    for pair in samples.windows(2) {
        cumulative += pair[0].1;
        total += cumulative.abs() * (pair[1].0 - pair[0].0);
    }
    total
}

/// The sliced Wasserstein-1 distance between the two normalized coverages,
/// in pixels, scaled by π/2: the mean over [`SLICED_W1_DIRECTIONS`]
/// equispaced directions of the exact 1-D W1 between the projected pixel
/// masses. For any plan, a direction's 1-D cost is at most the plan's mean
/// |⟨x − y, θ⟩|, and the mean of |cos| over directions is 2/π, so the result
/// never exceeds the true W1. It equals it for a translation and for a radial
/// scaling: a 3 px shift reads 3 px, a 1 px dilation of a radius-10 disk 2/3.
///
/// # Errors
/// Either mask is empty.
pub fn sliced_w1_px(a: &Mask, b: &Mask) -> Result<f64, String> {
    let total = |mask: &Mask| mask.coverage.iter().sum::<f64>();
    let (ta, tb) = (total(a), total(b));
    if ta <= 0.0 || tb <= 0.0 {
        return Err("a mask has no ink".to_owned());
    }
    // (column, row, signed mass): a's mass positive, b's negative.
    let mut points = Vec::new();
    for (mask, sign) in [(a, 1.0 / ta), (b, -1.0 / tb)] {
        for (i, &cover) in mask.coverage.iter().enumerate() {
            if cover > 0.0 {
                #[allow(clippy::cast_precision_loss)]
                let (x, y) = ((i % mask.width) as f64, (i / mask.width) as f64);
                points.push((x, y, sign * cover));
            }
        }
    }
    let mut samples = vec![(0.0, 0.0); points.len()];
    let mut sum = 0.0;
    for k in 0..SLICED_W1_DIRECTIONS {
        #[allow(clippy::cast_precision_loss)]
        let angle = std::f64::consts::PI * k as f64 / SLICED_W1_DIRECTIONS as f64;
        let (cos, sin) = (fmn_dmath::cos(angle), fmn_dmath::sin(angle));
        for (sample, &(x, y, mass)) in samples.iter_mut().zip(&points) {
            *sample = (x * cos + y * sin, mass);
        }
        sum += w1_line(&mut samples);
    }
    #[allow(clippy::cast_precision_loss)]
    let mean = sum / SLICED_W1_DIRECTIONS as f64;
    Ok(mean * std::f64::consts::FRAC_PI_2)
}

/// One formula's ink comparison.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InkMetrics {
    /// The registration's uniform scale (native → Reference): the box metric.
    pub scale: f64,
    /// Native coverage over the Reference's, after registration.
    pub mass_ratio: f64,
    /// Symmetric boundary Hausdorff distance, px.
    pub hausdorff_px: f64,
    /// Sliced W1 (× π/2), px.
    pub sliced_w1_px: f64,
    /// Soft IoU after registration.
    pub iou: f64,
}

/// The registration of `candidate` onto `reference`, and both drawn on one
/// grid through the certified engine.
///
/// # Errors
/// Either side has no curve, or rasterization fails.
pub fn registered_masks(
    reference: &[Vec<Vec3>],
    candidate: &[Vec<Vec3>],
) -> Result<(Similarity, Mask, Mask), String> {
    let fit = Similarity::register(
        ink_box(reference).ok_or("the Reference has no curve")?,
        ink_box(candidate).ok_or("the native ink has no curve")?,
    );
    let registered = fit.apply(candidate);
    let grid = Grid::around(&[reference, &registered]).ok_or("no points")?;
    Ok((
        fit,
        rasterize(reference, grid)?,
        rasterize(&registered, grid)?,
    ))
}

/// The side-by-side crop of one comparison as a PNG: the Reference's ink,
/// the native ink, and the two overlaid (the Reference red, native green, so
/// agreement is yellow), on a dark ground.
///
/// # Panics
/// Never for masks from [`registered_masks`], which share one grid.
#[must_use]
pub fn side_by_side_png(reference: &Mask, native: &Mask) -> Vec<u8> {
    const GAP: usize = 8;
    const GROUND: u8 = 0x33;
    let (w, h) = (reference.width, reference.height);
    let width = 3 * w + 2 * GAP;
    let mut rgba = vec![0_u8; width * h * 4];
    let level = |c: f64| {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let v = (f64::from(GROUND) + c.clamp(0.0, 1.0) * f64::from(255 - GROUND)).round() as u8;
        v
    };
    for r in 0..h {
        for c in 0..w {
            let (x, y) = (reference.coverage[r * w + c], native.coverage[r * w + c]);
            let panels = [
                (0, [level(x), level(x), level(x)]),
                (w + GAP, [level(y), level(y), level(y)]),
                (2 * (w + GAP), [level(x), level(y), GROUND]),
            ];
            for (offset, rgb) in panels {
                let at = (r * width + offset + c) * 4;
                rgba[at..at + 3].copy_from_slice(&rgb);
                rgba[at + 3] = 255;
            }
        }
        for gap in [w..w + GAP, 2 * w + GAP..2 * (w + GAP)] {
            for c in gap {
                let at = (r * width + c) * 4;
                rgba[at..at + 4].copy_from_slice(&[0, 0, 0, 255]);
            }
        }
    }
    fmn_codec::png::encode_rgba8(
        u32::try_from(width).expect("a crop narrower than u32"),
        u32::try_from(h).expect("a crop shorter than u32"),
        &rgba,
        fmn_codec::CompressionLevel::Default,
    )
}

/// Register `candidate` onto `reference`, draw both on one grid through the
/// certified engine, and measure.
///
/// # Errors
/// Either side has no curve or no ink, or rasterization fails.
pub fn compare_ink(reference: &[Vec<Vec3>], candidate: &[Vec<Vec3>]) -> Result<InkMetrics, String> {
    let (fit, a, b) = registered_masks(reference, candidate)?;
    Ok(InkMetrics {
        scale: fit.scale,
        mass_ratio: b.coverage.iter().sum::<f64>() / a.coverage.iter().sum::<f64>(),
        hausdorff_px: hausdorff_px(&a, &b),
        sliced_w1_px: sliced_w1_px(&a, &b)?,
        iou: soft_iou(&a, &b),
    })
}

// ---------------------------------------------------------------------------
// The ratchet
// ---------------------------------------------------------------------------

/// A formula's ink matches the Reference's when every shape metric is
/// within these bounds.
pub const MATCH_HAUSDORFF_PX: f64 = 2.0;
/// See [`MATCH_HAUSDORFF_PX`].
pub const MATCH_SLICED_W1_PX: f64 = 0.5;
/// See [`MATCH_HAUSDORFF_PX`].
pub const MATCH_IOU: f64 = 0.9;

/// Whether one formula's ink matches.
#[must_use]
pub fn ink_matches(metrics: &InkMetrics) -> bool {
    metrics.hausdorff_px <= MATCH_HAUSDORFF_PX
        && metrics.sliced_w1_px <= MATCH_SLICED_W1_PX
        && metrics.iou >= MATCH_IOU
}

/// The distribution the floors ratchet. Rows that failed to compare count as
/// non-matching and as the worst value of every metric.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InkSummary {
    /// Rows compared (failures included).
    pub rows: usize,
    /// Rows whose ink matches ([`ink_matches`]).
    pub matches: usize,
    /// Median Hausdorff, px.
    pub median_hausdorff_px: f64,
    /// 95th-percentile Hausdorff, px.
    pub p95_hausdorff_px: f64,
    /// Median W1, px.
    pub median_sliced_w1_px: f64,
    /// 95th-percentile W1, px.
    pub p95_sliced_w1_px: f64,
    /// Median IoU.
    pub median_iou: f64,
    /// 5th-percentile IoU: the IoU 95% of rows reach.
    pub p05_iou: f64,
}

/// Nearest-rank quantile of sorted values.
fn quantile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    let rank = ((q * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len());
    sorted[rank - 1]
}

/// Summarize per-row results.
#[must_use]
pub fn summarize_ink(results: &[Result<InkMetrics, String>]) -> InkSummary {
    let metric = |f: fn(&InkMetrics) -> f64, worst: f64| {
        let mut values: Vec<f64> = results
            .iter()
            .map(|r| r.as_ref().map_or(worst, f))
            .collect();
        values.sort_by(f64::total_cmp);
        values
    };
    let hausdorff = metric(|m| m.hausdorff_px, f64::INFINITY);
    let w1 = metric(|m| m.sliced_w1_px, f64::INFINITY);
    let iou = metric(|m| m.iou, 0.0);
    InkSummary {
        rows: results.len(),
        matches: results
            .iter()
            .filter(|r| r.as_ref().is_ok_and(ink_matches))
            .count(),
        median_hausdorff_px: quantile(&hausdorff, 0.5),
        p95_hausdorff_px: quantile(&hausdorff, 0.95),
        median_sliced_w1_px: quantile(&w1, 0.5),
        p95_sliced_w1_px: quantile(&w1, 0.95),
        median_iou: quantile(&iou, 0.5),
        p05_iou: quantile(&iou, 0.05),
    }
}

/// The locked floors of one ink set: the match count may only rise, the
/// distance quantiles only fall, the IoU quantiles only rise.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InkFloors {
    /// Minimum matching rows.
    pub matches: usize,
    /// Ceiling on the median Hausdorff, px.
    pub median_hausdorff_px: f64,
    /// Ceiling on the 95th-percentile Hausdorff, px.
    pub p95_hausdorff_px: f64,
    /// Ceiling on the median W1, px.
    pub median_sliced_w1_px: f64,
    /// Ceiling on the 95th-percentile W1, px.
    pub p95_sliced_w1_px: f64,
    /// Floor on the median IoU.
    pub median_iou: f64,
    /// Floor on the 5th-percentile IoU.
    pub p05_iou: f64,
}

/// Every floor `summary` breaks, as sentences (empty when it holds them all).
#[must_use]
pub fn floor_failures(summary: &InkSummary, floors: &InkFloors) -> Vec<String> {
    let mut out = Vec::new();
    if summary.matches < floors.matches {
        out.push(format!(
            "{} of {} formulas match, below the floor {}",
            summary.matches, summary.rows, floors.matches
        ));
    }
    let mut ceiling = |name: &str, value: f64, bound: f64| {
        if value.is_nan() || value > bound {
            out.push(format!("{name} {value:.4} above the ceiling {bound}"));
        }
    };
    ceiling(
        "median Hausdorff px",
        summary.median_hausdorff_px,
        floors.median_hausdorff_px,
    );
    ceiling(
        "p95 Hausdorff px",
        summary.p95_hausdorff_px,
        floors.p95_hausdorff_px,
    );
    ceiling(
        "median sliced W1 px",
        summary.median_sliced_w1_px,
        floors.median_sliced_w1_px,
    );
    ceiling(
        "p95 sliced W1 px",
        summary.p95_sliced_w1_px,
        floors.p95_sliced_w1_px,
    );
    if summary.median_iou.is_nan() || summary.median_iou < floors.median_iou {
        out.push(format!(
            "median IoU {:.4} below the floor {}",
            summary.median_iou, floors.median_iou
        ));
    }
    if summary.p05_iou.is_nan() || summary.p05_iou < floors.p05_iou {
        out.push(format!(
            "p05 IoU {:.4} below the floor {}",
            summary.p05_iou, floors.p05_iou
        ));
    }
    out
}

fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// NDJSON (schema `fmn.tex-ink.v1`): one record per row, worst IoU first,
/// then the summary. `label` names a row (its source, or a digest prefix
/// for private rows).
#[must_use]
pub fn ink_ndjson(
    set: &str,
    rows: &[InkRow],
    labels: &[String],
    results: &[Result<InkMetrics, String>],
    summary: &InkSummary,
) -> String {
    let mut order: Vec<usize> = (0..rows.len()).collect();
    let iou = |i: usize| results[i].as_ref().map_or(-1.0, |m| m.iou);
    order.sort_by(|&x, &y| iou(x).total_cmp(&iou(y)).then(x.cmp(&y)));
    let mut out = String::new();
    for i in order {
        let row = &rows[i];
        let _ = write!(
            out,
            "{{\"schema\":\"fmn.tex-ink.v1\",\"set\":{},\"formula\":{},\"kind\":\"{}\",\"class\":{}",
            json_string(set),
            json_string(&labels[i]),
            row.kind.as_str(),
            json_string(&row.class)
        );
        match &results[i] {
            Ok(m) => {
                let _ = writeln!(
                    out,
                    ",\"scale\":{:.5},\"mass_ratio\":{:.4},\"hausdorff_px\":{:.3},\"sliced_w1_px\":{:.4},\"iou\":{:.4},\"match\":{}}}",
                    m.scale,
                    m.mass_ratio,
                    m.hausdorff_px,
                    m.sliced_w1_px,
                    m.iou,
                    ink_matches(m)
                );
            }
            Err(error) => {
                let _ = writeln!(out, ",\"error\":{}}}", json_string(error));
            }
        }
    }
    let _ = writeln!(
        out,
        "{{\"schema\":\"fmn.tex-ink.v1\",\"set\":{},\"summary\":{{\"rows\":{},\"matches\":{},\
         \"median_hausdorff_px\":{:.3},\"p95_hausdorff_px\":{:.3},\"median_sliced_w1_px\":{:.4},\
         \"p95_sliced_w1_px\":{:.4},\"median_iou\":{:.4},\"p05_iou\":{:.4}}}}}",
        json_string(set),
        summary.rows,
        summary.matches,
        summary.median_hausdorff_px,
        summary.p95_hausdorff_px,
        summary.median_sliced_w1_px,
        summary.p95_sliced_w1_px,
        summary.median_iou,
        summary.p05_iou
    );
    out
}

/// NDJSON (schema `fmn.tex-ink.v1`): one record per construct class, the
/// lowest share of matching formulas first. `classes[i]` are row i's
/// construct classes (a row counts in each).
#[must_use]
pub fn ink_class_ndjson(
    set: &str,
    classes: &[Vec<String>],
    results: &[Result<InkMetrics, String>],
) -> String {
    let mut groups: std::collections::BTreeMap<&str, Vec<&Result<InkMetrics, String>>> =
        std::collections::BTreeMap::new();
    for (names, result) in classes.iter().zip(results) {
        for name in names {
            groups.entry(name.as_str()).or_default().push(result);
        }
    }
    let mut lines: Vec<(f64, String)> = groups
        .into_iter()
        .map(|(name, members)| {
            let owned: Vec<Result<InkMetrics, String>> =
                members.iter().map(|r| (*r).clone()).collect();
            let summary = summarize_ink(&owned);
            #[allow(clippy::cast_precision_loss)]
            let share = summary.matches as f64 / summary.rows as f64;
            let line = format!(
                "{{\"schema\":\"fmn.tex-ink.v1\",\"set\":{},\"class\":{},\"rows\":{},\
                 \"matches\":{},\"median_hausdorff_px\":{:.3},\"median_sliced_w1_px\":{:.4},\
                 \"median_iou\":{:.4},\"p05_iou\":{:.4}}}\n",
                json_string(set),
                json_string(name),
                summary.rows,
                summary.matches,
                summary.median_hausdorff_px,
                summary.median_sliced_w1_px,
                summary.median_iou,
                summary.p05_iou
            );
            (share, line)
        })
        .collect();
    lines.sort_by(|x, y| x.0.total_cmp(&y.0).then_with(|| x.1.cmp(&y.1)));
    lines.into_iter().map(|(_, line)| line).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A disk as a coverage mask, by 8×8 supersampling.
    fn disk(w: usize, h: usize, center: [f64; 2], radius: f64) -> Mask {
        let mut coverage = vec![0.0; w * h];
        for r in 0..h {
            for c in 0..w {
                let mut hits = 0;
                for sy in 0..8 {
                    for sx in 0..8 {
                        #[allow(clippy::cast_precision_loss)]
                        let x = c as f64 + (f64::from(sx) + 0.5) / 8.0 - center[0];
                        #[allow(clippy::cast_precision_loss)]
                        let y = r as f64 + (f64::from(sy) + 0.5) / 8.0 - center[1];
                        if x * x + y * y <= radius * radius {
                            hits += 1;
                        }
                    }
                }
                coverage[r * w + c] = f64::from(hits) / 64.0;
            }
        }
        Mask {
            width: w,
            height: h,
            coverage,
        }
    }

    /// A closed square outline in the shared-anchor layout (straight quads).
    fn square(x0: f64, y0: f64, side: f64) -> Vec<Vec3> {
        let corners = [
            [x0, y0],
            [x0 + side, y0],
            [x0 + side, y0 + side],
            [x0, y0 + side],
            [x0, y0],
        ];
        let mut points = vec![[x0, y0, 0.0]];
        for pair in corners.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            points.push([0.5 * (a[0] + b[0]), 0.5 * (a[1] + b[1]), 0.0]);
            points.push([b[0], b[1], 0.0]);
        }
        points
    }

    #[test]
    fn identical_ink_measures_zero() {
        let a = disk(48, 40, [24.0, 20.0], 12.0);
        assert!(hausdorff_px(&a, &a) < 1e-12);
        assert!(sliced_w1_px(&a, &a).expect("ink") < 1e-12);
        assert!((soft_iou(&a, &a) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn a_shift_moves_every_metric_by_its_distance() {
        // A whole-pixel shift of 3 px: Hausdorff exactly 3, and W1 exactly 3
        // up to the 64 directions' ripple in the mean |cos|.
        let a = disk(64, 40, [24.0, 20.0], 10.0);
        let b = disk(64, 40, [27.0, 20.0], 10.0);
        assert!((hausdorff_px(&a, &b) - 3.0).abs() < 1e-9);
        let w1 = sliced_w1_px(&a, &b).expect("ink");
        assert!((w1 - 3.0).abs() < 2e-3, "W1 {w1}");
        // A diagonal (2, 2) shift is 2√2 away.
        let c = disk(64, 48, [24.0, 20.0], 10.0);
        let d = disk(64, 48, [26.0, 22.0], 10.0);
        let w1 = sliced_w1_px(&c, &d).expect("ink");
        assert!((w1 - 8.0_f64.sqrt()).abs() < 2e-3, "diagonal W1 {w1}");
        // A sub-pixel shift reads in proportion, through the pixels' coverage.
        let e = disk(48, 48, [24.0, 24.0], 10.0);
        let f = disk(48, 48, [24.3, 24.0], 10.0);
        let w1 = sliced_w1_px(&e, &f).expect("ink");
        assert!((w1 - 0.3).abs() < 0.01, "sub-pixel W1 {w1}");
    }

    #[test]
    fn a_one_pixel_dilation_has_its_known_distances() {
        let a = disk(48, 48, [24.0, 24.0], 10.0);
        let b = disk(48, 48, [24.0, 24.0], 11.0);
        // The half-coverage boundary moves out by the dilation.
        let hausdorff = hausdorff_px(&a, &b);
        assert!((0.5..=1.5).contains(&hausdorff), "Hausdorff {hausdorff}");
        // IoU is the area ratio (10/11)².
        let iou = soft_iou(&a, &b);
        assert!((iou - (10.0_f64 / 11.0).powi(2)).abs() < 0.01, "IoU {iou}");
        // Normalized, the radial stretch r ↦ 1.1 r is the optimal plan, with
        // cost 0.1·E[r] = 2/3 px, and every projection of it is optimal too,
        // so the sliced W1 is the full W1.
        let w1 = sliced_w1_px(&a, &b).expect("ink");
        assert!((w1 - 2.0 / 3.0).abs() < 0.01, "W1 {w1}");
    }

    #[test]
    fn the_sliced_w1_never_exceeds_a_known_plan() {
        // Mass split between two spots, moved across: one spot 6 px right,
        // the other 6 px up. The plan moving each spot costs 6 px, an upper
        // bound on W1 and so on the sliced W1.
        let mut a = disk(40, 40, [10.0, 30.0], 3.0);
        let b0 = disk(40, 40, [16.0, 30.0], 3.0);
        let up = disk(40, 40, [30.0, 30.0], 3.0);
        let up_moved = disk(40, 40, [30.0, 24.0], 3.0);
        let mut b = b0.clone();
        for i in 0..a.coverage.len() {
            a.coverage[i] += up.coverage[i];
            b.coverage[i] += up_moved.coverage[i];
        }
        let w1 = sliced_w1_px(&a, &b).expect("ink");
        assert!(w1 <= 6.0 + 1e-9 && w1 > 3.0, "sliced W1 {w1}");
    }

    #[test]
    fn green_moments_of_squares_are_exact() {
        let members = vec![square(1.0, 2.0, 2.0), square(-4.0, 2.0, 2.0)];
        let m = area_moments(&members).expect("area");
        assert!((m.mass - 8.0).abs() < 1e-12);
        assert!((m.centroid[0] - -0.5).abs() < 1e-12 && (m.centroid[1] - 3.0).abs() < 1e-12);
        // Each square: per-axis variance side²/12; the two centres sit ±2.5
        // from the centroid in x.
        let expected = (2.0_f64 * 4.0 / 12.0 + 2.5 * 2.5).sqrt();
        assert!((m.spread - expected).abs() < 1e-12, "spread {}", m.spread);
        // A reversed winding adds the same area.
        let mut reversed = square(1.0, 2.0, 2.0);
        reversed.reverse();
        let r = area_moments(&[reversed]).expect("area");
        assert!((r.mass - 4.0).abs() < 1e-12);
    }

    #[test]
    fn registration_undoes_a_similarity() {
        let members = vec![square(0.0, 0.0, 1.0), square(2.0, 0.5, 0.5)];
        let moved = Similarity {
            scale: 1.3,
            shift: [0.7, -0.2],
        }
        .apply(&members);
        let fit = Similarity::register(
            ink_box(&members).expect("box"),
            ink_box(&moved).expect("box"),
        );
        let back = area_moments(&fit.apply(&moved)).expect("area");
        let original = area_moments(&members).expect("area");
        assert!((fit.scale - 1.0 / 1.3).abs() < 1e-12);
        assert!((back.centroid[0] - original.centroid[0]).abs() < 1e-12);
        assert!((back.spread - original.spread).abs() < 1e-12);
    }

    #[test]
    fn the_ink_box_reaches_a_curve_extremum_not_its_handle() {
        // One quad from (0, 0) to (2, 0) with its handle at (1, 2): the
        // curve peaks at y = 1, halfway to the handle.
        let arc = vec![vec![[0.0, 0.0, 0.0], [1.0, 2.0, 0.0], [2.0, 0.0, 0.0]]];
        let (min, max) = ink_box(&arc).expect("box");
        assert_eq!((min, max), ([0.0, 0.0], [2.0, 1.0]));
    }

    /// The raster's coverage moments, in scene units.
    fn raster_moments(mask: &Mask, grid: Grid) -> AreaMoments {
        let (mut mass, mut sx, mut sy, mut sq) = (0.0, 0.0, 0.0, 0.0);
        for r in 0..mask.height {
            for c in 0..mask.width {
                let cover = mask.coverage[r * mask.width + c];
                #[allow(clippy::cast_precision_loss)]
                let x = grid.left + (c as f64 + 0.5) / PX_PER_UNIT;
                #[allow(clippy::cast_precision_loss)]
                let y = grid.top - (r as f64 + 0.5) / PX_PER_UNIT;
                mass += cover;
                sx += cover * x;
                sy += cover * y;
                sq += cover * (x * x + y * y);
            }
        }
        let centroid = [sx / mass, sy / mass];
        AreaMoments {
            mass: mass / (PX_PER_UNIT * PX_PER_UNIT),
            centroid,
            spread: (sq / mass - centroid[0] * centroid[0] - centroid[1] * centroid[1]).sqrt(),
        }
    }

    #[test]
    fn the_certified_raster_agrees_with_the_exact_moments() {
        // A square and the Reference's own "x" and "\theta" (a glyph with a hole).
        let rows = parse_ink_fixture(INK_FIXTURE).expect("parses");
        let glyph = |key: &str| {
            rows.iter()
                .find(|r| r.key == key)
                .expect("fixture row")
                .members
                .clone()
        };
        for (name, members) in [
            ("square", vec![square(0.1, -0.2, 0.3)]),
            ("x", glyph("x")),
            ("theta", glyph(r"\theta")),
        ] {
            let exact = area_moments(&members).expect("area");
            let grid = Grid::around(&[&members]).expect("grid");
            let mask = rasterize(&members, grid).expect("renders");
            let seen = raster_moments(&mask, grid);
            let pixel = 1.0 / PX_PER_UNIT;
            assert!(
                (seen.mass / exact.mass - 1.0).abs() < 0.01,
                "{name}: raster area {} against {}",
                seen.mass,
                exact.mass
            );
            for i in 0..2 {
                assert!(
                    (seen.centroid[i] - exact.centroid[i]).abs() < 0.05 * pixel,
                    "{name}: raster centroid {:?} against {:?}",
                    seen.centroid,
                    exact.centroid
                );
            }
            assert!(
                (seen.spread - exact.spread).abs() < 0.05 * pixel,
                "{name}: raster spread {} against {}",
                seen.spread,
                exact.spread
            );
        }
    }

    #[test]
    fn the_fixture_parses_with_its_delta_coding() {
        let rows = parse_ink_fixture("# header\nmath\tx\tplain\t10 20 5 -5 -5 5|0 0 1 1\n")
            .expect("parses");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].members.len(), 2);
        let p = &rows[0].members[0];
        assert!((p[1][0] - 0.0015).abs() < 1e-12 && (p[1][1] - 0.0015).abs() < 1e-12);
        assert!((p[2][0] - 0.001).abs() < 1e-12 && (p[2][1] - 0.002).abs() < 1e-12);
        assert!(parse_ink_fixture("math\tx\tplain\t1 2 3\n").is_err());
        assert!(parse_ink_fixture("glyph\tx\tplain\t1 2\n").is_err());
    }

    #[test]
    fn summaries_count_failures_as_the_worst_row() {
        let good = InkMetrics {
            scale: 1.0,
            mass_ratio: 1.0,
            hausdorff_px: 1.0,
            sliced_w1_px: 0.1,
            iou: 0.95,
        };
        let results = vec![Ok(good), Ok(good), Err("refused".to_owned())];
        let summary = summarize_ink(&results);
        assert_eq!((summary.rows, summary.matches), (3, 2));
        assert!(summary.p95_hausdorff_px.is_infinite());
        assert!(
            summary.p05_iou.abs() < 1e-12,
            "the refused row is the worst IoU"
        );
    }
}
