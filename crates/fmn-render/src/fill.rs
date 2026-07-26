//! §10.2's fill: **nonzero-winding coverage evaluated analytically on the
//! curves** — no triangulation, no signed-alpha tricks, no orientation
//! bookkeeping.
//!
//! > Tiled scanline nonzero-winding coverage evaluated analytically on the
//! > curves: quadratic segments made axis-monotone by splitting at both
//! > tangent-extremum parameters — `dy/dt = 0` for y-monotonicity, so a scanline
//! > meets a piece at most once, *and* `dx/dt = 0` for x-monotonicity, so each
//! > column crossing is a single closed-form root; per scanline, exact segment
//! > and column intersections from the closed-form quadratic root; signed
//! > trapezoidal area accumulation per cell, with each tile's rows carrying the
//! > winding deposited to their left in closed form.
//!
//! ## Where this came from, and what changed on the way
//!
//! The algorithm is G0-8b's (`spikes/g0-8-accelerator/src/analytic_fill.rs`,
//! fm-orn), inherited rather than re-derived, together with the two rules that
//! spike paid for:
//!
//! 1. **Where an exact value is known, use it.** A column crossing's `x` *is*
//!    the boundary; re-evaluating `x(t)` to recover it puts an ulp on the wrong
//!    side of an integer, which makes the walk's stepping predicate name the
//!    column it is already in. That defect deposited a three-column span as one
//!    trapezoid — and it was wrong in the `f64` reference while `f32` happened to
//!    be right, so the two-width comparison read like conditioning.
//! 2. **A tiling test must sweep tile alignments.** Comparing the scanline and
//!    per-pixel forms over one full-width window sees nothing, because no piece
//!    ever enters at an edge.
//!
//! Three things are different here, and all three are consequences of this being
//! the engine rather than a spike:
//!
//! - **The host width is `f64`.** §6.1's numeric doctrine and [`crate::table`]'s
//!   rule are the same rule: IR tables are host structs in the semantic width,
//!   and the single-typed `f32` arrays a GPU wants are a *derived* layout. The
//!   spike stored `f32` so its CPU reference and its Metal engine read identical
//!   bytes; that is a property of the derivation, not of the table. The core is
//!   still generic over [`Real`], so the `f32` instantiation remains available as
//!   the annex's arithmetic without the annex's hardware.
//! - **Instancing is a translation, not a rebuild.** §10.8 interns outlines, so
//!   the monotone split is derived **per compiled shape**, once, and an
//!   occurrence contributes only its pixel translation — folded into the piece's
//!   polynomial coefficients at the top of a row rather than materialized as
//!   geometry. A formula with two hundred `x` glyphs splits one outline.
//! - **The tile classes are consumed.** [`crate::bin`] already answers
//!   "is this tile wholly inside this shape", and §10.4's whole point is that
//!   such a tile does not evaluate coverage at all: it fills as a vectorized
//!   span at *exactly* `1`, which is also the precondition that makes occlusion
//!   pruning bit-exact (G0-8b finding F13).
//!
//! ## The tile carry, which is the part a tiled fill cannot skip
//!
//! Coverage inside a tile is not a function of the geometry inside that tile. A
//! scanline entering the tile's left edge already carries whatever winding the
//! path deposited to the left of it, so every row needs
//!
//! ```text
//! carry(row) = Σ over pieces of (signed dy of the part of the piece in this
//!              row lying left of the tile)
//! ```
//!
//! before the first cell is read. It is computed in closed form — one root solve
//! per piece — never by walking cells from the frame's left edge, which is why
//! the fill loops over **every** piece of a path rather than only the pieces
//! whose bounds meet the tile. Binning is unaffected: a path enclosing any pixel
//! of a tile has hull bounds containing that tile, so [`crate::bin::Binning`]
//! already lists it.
//!
//! ## The two dispatch shapes
//!
//! Both are built from one routine, [`accumulate_piece_row`], because the
//! per-pixel form is the scanline form with a one-cell window:
//!
//! - [`fill_row`] — the scanline shape. One accumulator of `width + 1` cells per
//!   row and one serial prefix sum along x. This is §10.2 literally, and it is
//!   the shape the CPU engine wants: a serial prefix over a row has no occupancy
//!   to lose.
//! - [`coverage_at_cell`] — the per-pixel shape. Each pixel sums, over the
//!   pieces, "the winding that passed to my left" plus "my own cell's
//!   trapezoid", with no accumulator and no scan. It is the annex's shape: one
//!   thread per pixel, the same dispatch the stroke kernel already uses.
//!
//! In exact arithmetic the two agree term for term; they differ only in the
//! association of the sum, which is why both are measured rather than assumed
//! equal.

use crate::bin::ScreenMap;
use crate::plan::RenderPlan;
use crate::table::Instance;

// ----------------------------------------------------------------- arithmetic

/// The scalar operations the fill needs, so the host (`f64`) and the annex's
/// arithmetic floor (`f32`) run **one** expression tree rather than two
/// hand-kept transcriptions.
///
/// G0-8 wrote `distance_to_quadratic` twice, once per width, and the two copies
/// were correct — but the mirror rule survived that only because a human held
/// it. Here the two instantiations are the same source by construction, which
/// leaves exactly one hand-mirrored copy to worry about: the annex kernel.
pub trait Real:
    Copy
    + PartialOrd
    + core::ops::Add<Output = Self>
    + core::ops::Sub<Output = Self>
    + core::ops::Mul<Output = Self>
    + core::ops::Div<Output = Self>
    + core::ops::Neg<Output = Self>
{
    /// `0`.
    const ZERO: Self;
    /// `1`.
    const ONE: Self;
    /// `2`.
    const TWO: Self;
    /// `3`.
    const THREE: Self;
    /// `4`.
    const FOUR: Self;
    /// `0.5`.
    const HALF: Self;
    /// The root solver's degeneracy tolerance, **relative to the polynomial's
    /// own scale and sized to this width** — G0-8's finding F8, which cost a day
    /// when a single absolute constant was shared between an `f64` engine and an
    /// `f32` one.
    const DEGENERATE_REL: Self;

    /// Narrow the table's storage width into the working width.
    fn from_f64(v: f64) -> Self;
    /// Widen a pixel index.
    fn from_u32(v: u32) -> Self;
    /// Back to `f64` for the surface and the tests.
    fn to_f64(self) -> f64;
    /// Truncate toward zero, as `as i64` does.
    fn to_i64(self) -> i64;
    /// Absolute value.
    fn abs(self) -> Self;
    /// Square root.
    fn sqrt(self) -> Self;
    /// Largest integer not greater than `self`.
    fn floor(self) -> Self;
    /// Smallest integer not less than `self`.
    fn ceil(self) -> Self;
}

impl Real for f64 {
    const ZERO: f64 = 0.0;
    const ONE: f64 = 1.0;
    const TWO: f64 = 2.0;
    const THREE: f64 = 3.0;
    const FOUR: f64 = 4.0;
    const HALF: f64 = 0.5;
    const DEGENERATE_REL: f64 = 1e-14;

    fn from_f64(v: f64) -> f64 {
        v
    }
    fn from_u32(v: u32) -> f64 {
        f64::from(v)
    }
    fn to_f64(self) -> f64 {
        self
    }
    fn to_i64(self) -> i64 {
        self as i64
    }
    fn abs(self) -> f64 {
        f64::abs(self)
    }
    fn sqrt(self) -> f64 {
        f64::sqrt(self)
    }
    fn floor(self) -> f64 {
        f64::floor(self)
    }
    fn ceil(self) -> f64 {
        f64::ceil(self)
    }
}

impl Real for f32 {
    const ZERO: f32 = 0.0;
    const ONE: f32 = 1.0;
    const TWO: f32 = 2.0;
    const THREE: f32 = 3.0;
    const FOUR: f32 = 4.0;
    const HALF: f32 = 0.5;
    const DEGENERATE_REL: f32 = 1e-6;

    fn from_f64(v: f64) -> f32 {
        v as f32
    }
    fn from_u32(v: u32) -> f32 {
        v as f32
    }
    fn to_f64(self) -> f64 {
        f64::from(self)
    }
    fn to_i64(self) -> i64 {
        self as i64
    }
    fn abs(self) -> f32 {
        f32::abs(self)
    }
    fn sqrt(self) -> f32 {
        f32::sqrt(self)
    }
    fn floor(self) -> f32 {
        f32::floor(self)
    }
    fn ceil(self) -> f32 {
        f32::ceil(self)
    }
}

/// The smaller of two values, written as a comparison rather than `f64::min`.
///
/// `f64::min` and a shader's `min` agree on ordinary values and disagree about
/// NaN and signed zero; after G0-8's `sign(0)` incident (finding F7) the house
/// rule is that any predicate the annex kernel also has to spell gets spelled
/// the same way on both sides.
fn fmin<T: Real>(a: T, b: T) -> T {
    if a < b { a } else { b }
}

/// The larger of two values. See [`fmin`].
fn fmax<T: Real>(a: T, b: T) -> T {
    if a > b { a } else { b }
}

/// `+1` for zero and positives, `-1` for negatives.
fn sign_or_positive<T: Real>(x: T) -> T {
    if x >= T::ZERO { T::ONE } else { -T::ONE }
}

/// Real roots of `a2 t² + a1 t + a0`, falling through to the linear case.
///
/// The stable pairing (`q = -½(b + sgn(b)√Δ)`, roots `q/a` and `c/q`) rather
/// than the schoolbook formula, and a degeneracy test relative to the
/// polynomial's own scale rather than an absolute epsilon.
fn solve_quadratic<T: Real>(a2: T, a1: T, a0: T, out: &mut [T; 2]) -> usize {
    let scale = fmax(fmax(a2.abs(), a1.abs()), a0.abs());
    if scale <= T::ZERO {
        return 0;
    }
    let tol = T::DEGENERATE_REL * scale;
    if a2.abs() <= tol {
        if a1.abs() <= tol {
            return 0;
        }
        out[0] = -a0 / a1;
        return 1;
    }
    let disc = a1 * a1 - T::FOUR * a2 * a0;
    if disc < T::ZERO {
        return 0;
    }
    let s = disc.sqrt();
    let q = -T::HALF * (a1 + sign_or_positive(a1) * s);
    if q == T::ZERO {
        out[0] = T::ZERO;
        out[1] = T::ZERO;
        return 2;
    }
    out[0] = q / a2;
    out[1] = a0 / q;
    2
}

// -------------------------------------------------------------- monotone table

/// A quadratic piece that is monotone in **both** axes, in screen pixels.
///
/// Shape-local: the [`ScreenMap`]'s scale and origin are applied, the
/// occurrence's offset is not. §10.8 interns outlines, so an occurrence is a
/// translation of the shared pieces and paying for it in geometry would undo the
/// interning — see [`instance_translation`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MonoPiece {
    /// Start anchor.
    pub p0: [f64; 2],
    /// Control handle.
    pub p1: [f64; 2],
    /// End anchor.
    pub p2: [f64; 2],
}

/// Below this parameter distance from an endpoint a split is skipped.
///
/// A split at `t = 1e-12` yields a piece whose y-extent is smaller than any
/// scanline band, so it can never contribute coverage — it only costs a table
/// row and a per-row rejection. The threshold is on the *parameter*, which is
/// dimensionless, so it needs no relation to screen scale.
const SPLIT_EPS: f64 = 1e-9;

/// The fill's derived geometry: every compiled shape's segments cut into
/// doubly-monotone pieces, plus the per-shape ranges.
///
/// **A second table, not a replacement for the IR's segments.** Strokes read
/// unsplit segments and would be wrong if they did not: §10.3 interpolates width
/// and colour by the normalized arc-length span `(s0, s1)` each
/// [`crate::table::Segment`] carries, and splitting a segment invalidates that
/// span unless it is recomputed, while a nearest-point search over more, shorter
/// pieces is strictly more work for the same answer. So the fill gets its own
/// derived table — keyed, like every other derived artifact in §10.8, by the
/// **geometry** revision and the screen scale alone: a colour change must not
/// rebuild it, and a pan must not either.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MonoTable {
    pieces: Vec<MonoPiece>,
    ranges: Vec<(u32, u32)>,
    map: ScreenMap,
}

impl MonoTable {
    /// Derive the table from a synchronized plan under a screen mapping.
    #[must_use]
    pub fn build(plan: &RenderPlan, map: ScreenMap) -> MonoTable {
        let shapes = plan.shapes().shapes();
        let segments = plan.segments();
        let mut pieces = Vec::with_capacity(segments.len() * 2);
        let mut ranges = Vec::with_capacity(shapes.len());
        // Shape-local screen space: scale and origin, no offset. A uniform scale
        // and a translation preserve monotonicity in both axes, so the split
        // parameters are the same ones object space would give — which is what
        // lets one split serve every occurrence.
        let s = |p: fmn_core::types::Vec3| {
            [
                map.origin[0] + p[0] * map.scale,
                map.origin[1] + p[1] * map.scale,
            ]
        };
        let mut curves: Vec<[[f64; 2]; 3]> = Vec::new();
        for shape in shapes {
            let first = pieces.len() as u32;
            let lo = (shape.first_segment as usize).min(segments.len());
            let hi = (lo + shape.segment_count as usize).min(segments.len());
            let own = &segments[lo..hi];
            // §10.2 fills each *subpath* as if closed, so the pieces are grouped
            // by subpath and each group gets its closing chord. The boundaries
            // come from `Shape::subpath_starts` rather than from anchor
            // continuity: reconstructing them is almost right and silently wrong
            // when one subpath happens to begin exactly where the previous one
            // ended.
            let starts = &shape.subpath_starts;
            for (k, &start) in starts.iter().enumerate() {
                let end = starts.get(k + 1).copied().unwrap_or(own.len() as u32);
                let (a, b) = (start as usize, (end as usize).min(own.len()));
                if a >= b {
                    continue;
                }
                curves.clear();
                curves.extend(own[a..b].iter().map(|g| [s(g.p0), s(g.p1), s(g.p2)]));
                append_subpath(&curves, &mut pieces);
            }
            ranges.push((first, pieces.len() as u32 - first));
        }
        MonoTable {
            pieces,
            ranges,
            map,
        }
    }

    /// The pieces belonging to compiled shape `index`.
    ///
    /// Empty for an index the table does not know, which is the honest answer
    /// for a plan that resynchronized under this table: a missing shape draws
    /// nothing rather than someone else's outline.
    #[must_use]
    pub fn pieces_of(&self, index: u32) -> &[MonoPiece] {
        match self.ranges.get(index as usize) {
            Some(&(first, count)) => {
                &self.pieces[first as usize..(first as usize + count as usize)]
            }
            None => &[],
        }
    }

    /// Every piece, contiguous and grouped by shape.
    #[must_use]
    pub fn pieces(&self) -> &[MonoPiece] {
        &self.pieces
    }

    /// How many compiled shapes this table covers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ranges.len()
    }

    /// Is the table empty?
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    /// The mapping the pieces were derived under.
    ///
    /// Carried so a caller can tell a stale table from a current one without
    /// keeping the mapping beside it — the same reason
    /// [`crate::cache::TileKey`] carries its camera revision.
    #[must_use]
    pub fn map(&self) -> ScreenMap {
        self.map
    }
}

/// The pixel translation one occurrence contributes.
///
/// [`MonoTable`]'s pieces already carry the map's origin, so an occurrence adds
/// only its offset scaled to pixels. Splitting the placement this way is what
/// makes interning pay: N copies of a glyph are N of these and one outline.
#[must_use]
pub fn instance_translation(inst: &Instance, map: ScreenMap) -> [f64; 2] {
    [inst.offset[0] * map.scale, inst.offset[1] * map.scale]
}

/// The parameter of a quadratic component's extremum, if it lies strictly inside
/// the segment.
///
/// `v(t) = a + b t + c t²` has `v'(t) = b + 2 c t`, so the extremum is at
/// `-b / 2c`. A vanishing `c` is a straight component with no extremum at all,
/// which is the common case — every line, every glyph stem — and must not
/// divide.
fn extremum(v0: f64, v1: f64, v2: f64) -> Option<f64> {
    let b = 2.0 * (v1 - v0);
    let c = (v2 - v1) - (v1 - v0);
    let scale = v0.abs().max(v1.abs()).max(v2.abs()).max(1.0);
    if c.abs() <= 1e-14 * scale {
        return None;
    }
    let t = -b / (2.0 * c);
    if t > SPLIT_EPS && t < 1.0 - SPLIT_EPS {
        Some(t)
    } else {
        None
    }
}

/// Split one subpath's screen-space curves into monotone pieces, **closing it**.
///
/// §10.2 fills a path, and a path's interior is only defined for closed
/// boundaries, so an open subpath fills as if its closing chord were present —
/// SVG's rule for `fill`, and manim's (`get_triangulation` closes each subpath
/// before triangulating). Leaving it open is not a smaller answer, it is a
/// different picture: the winding accumulated by an unterminated boundary never
/// returns to zero, so every pixel to its right stays inside. Measured on the
/// open triangle in this module's tests, the unclosed form filled a
/// **rectangle running off the right edge of the window** — 48 pixels against
/// the triangle's 96 — which is the kind of wrong that looks like a plausible
/// picture.
///
/// The chord's handle sits at its midpoint, which makes it an exactly straight
/// quadratic and doubly monotone by construction, so it needs no split.
fn append_subpath(curves: &[[[f64; 2]; 3]], out: &mut Vec<MonoPiece>) {
    let Some(first) = curves.first() else {
        return;
    };
    for c in curves {
        split_monotone(c[0], c[1], c[2], out);
    }
    let start = first[0];
    let end = curves[curves.len() - 1][2];
    if start != end {
        out.push(MonoPiece {
            p0: end,
            p1: [0.5 * (end[0] + start[0]), 0.5 * (end[1] + start[1])],
            p2: start,
        });
    }
}

/// Split one segment at its y- and x-extrema, appending doubly-monotone pieces.
///
/// A quadratic has at most one extremum per axis, so this appends at most three
/// pieces — a bound a device layout can rely on when sizing the table.
fn split_monotone(p0: [f64; 2], p1: [f64; 2], p2: [f64; 2], out: &mut Vec<MonoPiece>) {
    let mut ts = [0.0f64; 2];
    let mut n = 0;
    if let Some(t) = extremum(p0[1], p1[1], p2[1]) {
        ts[n] = t;
        n += 1;
    }
    if let Some(t) = extremum(p0[0], p1[0], p2[0]) {
        ts[n] = t;
        n += 1;
    }
    if n == 2 && ts[0] > ts[1] {
        ts.swap(0, 1);
    }

    // Successive de Casteljau splits, each reparameterized into the remaining
    // right-hand piece.
    let mut cur = [p0, p1, p2];
    let mut t_base = 0.0;
    for &t in ts.iter().take(n) {
        let local = (t - t_base) / (1.0 - t_base);
        if !(local > SPLIT_EPS && local < 1.0 - SPLIT_EPS) {
            continue;
        }
        let (left, right) = split_at(cur, local);
        out.push(MonoPiece {
            p0: left[0],
            p1: left[1],
            p2: left[2],
        });
        cur = right;
        t_base = t;
    }
    out.push(MonoPiece {
        p0: cur[0],
        p1: cur[1],
        p2: cur[2],
    });
}

/// de Casteljau's split of a quadratic at `t`.
fn split_at(p: [[f64; 2]; 3], t: f64) -> ([[f64; 2]; 3], [[f64; 2]; 3]) {
    let lerp = |a: [f64; 2], b: [f64; 2]| [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
    let q0 = lerp(p[0], p[1]);
    let q1 = lerp(p[1], p[2]);
    let r = lerp(q0, q1);
    ([p[0], q0, r], [r, q1, p[2]])
}

// ---------------------------------------------------------------- the fill core

/// One piece's component polynomials, `v(t) = a + b t + c t²`, at the working
/// width, with the occurrence's translation folded into the constant term.
///
/// `c` is formed as `(v2 - v1) - (v1 - v0)` rather than `v0 - 2 v1 + v2`:
/// algebraically the same, numerically not, and G0-8 measured the difference on
/// near-straight screen-space curves at `f32`.
#[derive(Debug, Clone, Copy)]
struct Coeffs<T: Real> {
    ax: T,
    bx: T,
    cx: T,
    ay: T,
    by: T,
    cy: T,
}

impl<T: Real> Coeffs<T> {
    fn of(piece: &MonoPiece, translate: [f64; 2]) -> Coeffs<T> {
        let f = T::from_f64;
        let (tx, ty) = (f(translate[0]), f(translate[1]));
        let (x0, x1, x2) = (f(piece.p0[0]), f(piece.p1[0]), f(piece.p2[0]));
        let (y0, y1, y2) = (f(piece.p0[1]), f(piece.p1[1]), f(piece.p2[1]));
        Coeffs {
            ax: x0 + tx,
            bx: T::TWO * (x1 - x0),
            cx: (x2 - x1) - (x1 - x0),
            ay: y0 + ty,
            by: T::TWO * (y1 - y0),
            cy: (y2 - y1) - (y1 - y0),
        }
    }

    fn x(&self, t: T) -> T {
        self.ax + self.bx * t + self.cx * t * t
    }

    fn y(&self, t: T) -> T {
        self.ay + self.by * t + self.cy * t * t
    }

    /// The parameter in `[t_lo, t_hi]` at which `x` reaches `target`.
    fn t_at_x(&self, target: T, t_lo: T, t_hi: T) -> T {
        invert(
            self.ax,
            self.bx,
            self.cx,
            target,
            t_lo,
            t_hi,
            self.x(t_lo),
            self.x(t_hi),
        )
    }

    /// The **dy-weighted mean of `x`** over `[t0, t1]`: `(∫ x dy) / (∫ dy)`,
    /// in closed form.
    ///
    /// This is the number that makes the fill exact on curves rather than on
    /// chords, and it is the one place this module departs from the spike it
    /// inherited. The per-cell deposit needs `∫ (c + 1 − x) dy` over the span,
    /// i.e. `dy · (c + 1 − x̄)`; every signed-area rasterizer in the wild —
    /// font-rs, `stb_truetype`, the G0-8b spike — substitutes the *chord*
    /// midpoint `½(x(t0) + x(t1))` for `x̄`, which is exact only when `x` is
    /// linear in `y`. On a curve it carries an `O(curvature · h²)` error, and
    /// the way that error announces itself is a **subdivision-invariance
    /// failure**: splitting a curve at its midpoint halves the per-span
    /// curvature and moves every partially-covered pixel. Measured on a
    /// 15-pixel-radius circle of 8 quadratics, a de Casteljau split at `t = ½`
    /// — which changes the point array and not the curve — moved coverage by
    /// **5.7e-3**, one and a half 8-bit levels and past §10.2's own
    /// sub-`1/256` acceptance. "Evaluated analytically on the curves" has to
    /// mean the integral, not a chord through it.
    ///
    /// With `x(t) = ax + bx t + cx t²` and `y'(t) = by + 2 cy t`:
    ///
    /// ```text
    /// ∫ x y' dt = Δ · [k0 + k1·S1/2 + k2·S2/3 + k3·S3/4]
    /// ∫ y'   dt = Δ · [by + cy·S1]
    /// ```
    ///
    /// with `Δ = t1 − t0`, `S1 = t1 + t0`, `S2 = t1² + t1 t0 + t0²`,
    /// `S3 = S1 (t1² + t0²)`, and `k0..k3` the product's coefficients. **`Δ`
    /// cancels**, which is why this is written as a ratio of the bracketed
    /// forms rather than as `F(t1) − F(t0)` over `y(t1) − y(t0)`: the
    /// difference-of-antiderivatives spelling loses every significant digit as
    /// a span shrinks toward a column boundary, and spans that shrink toward
    /// column boundaries are the common case, not the corner one.
    ///
    /// `None` when the mean of `y'` vanishes — a zero-height span, which the
    /// caller has already discarded on `dy == 0`, kept here so the division is
    /// unconditional at the call site rather than trusted.
    fn mean_x_over_dy(&self, t0: T, t1: T) -> Option<T> {
        let s1 = t1 + t0;
        let s2 = t1 * t1 + t1 * t0 + t0 * t0;
        let s3 = s1 * (t1 * t1 + t0 * t0);
        let denom = self.by + self.cy * s1;
        if denom == T::ZERO {
            return None;
        }
        let k0 = self.ax * self.by;
        let k1 = self.bx * self.by + T::TWO * self.ax * self.cy;
        let k2 = self.cx * self.by + T::TWO * self.bx * self.cy;
        let k3 = T::TWO * self.cx * self.cy;
        let num = k0 + k1 * s1 * T::HALF + k2 * s2 / T::THREE + k3 * s3 / T::FOUR;
        Some(num / denom)
    }

    /// The parameter in `[t_lo, t_hi]` at which `y` reaches `target`.
    fn t_at_y(&self, target: T, t_lo: T, t_hi: T) -> T {
        invert(
            self.ay,
            self.by,
            self.cy,
            target,
            t_lo,
            t_hi,
            self.y(t_lo),
            self.y(t_hi),
        )
    }
}

/// Invert a component that is monotone on `[t_lo, t_hi]`.
///
/// Total by construction: a target outside the endpoint values clamps to the
/// endpoint attaining the nearer extreme, and a root solve finding nothing in
/// range falls back to the secant through the endpoints. Neither fallback is a
/// fudge — monotonicity guarantees the answer exists, and the fallbacks only
/// decide *which* representable parameter to name when the closed form has run
/// out of precision to distinguish them.
#[allow(clippy::too_many_arguments)]
fn invert<T: Real>(a: T, b: T, c: T, target: T, t_lo: T, t_hi: T, v_lo: T, v_hi: T) -> T {
    let ascending = v_hi >= v_lo;
    let vmin = fmin(v_lo, v_hi);
    let vmax = fmax(v_lo, v_hi);
    if target <= vmin {
        return if ascending { t_lo } else { t_hi };
    }
    if target >= vmax {
        return if ascending { t_hi } else { t_lo };
    }

    let mut roots = [T::ZERO; 2];
    let n = solve_quadratic(c, b, a - target, &mut roots);
    for &r in roots.iter().take(n) {
        if r >= t_lo && r <= t_hi {
            return r;
        }
    }
    // The secant fallback. `vmax > vmin` here, so the division is safe.
    t_lo + (t_hi - t_lo) * ((target - v_lo) / (v_hi - v_lo))
}

/// Deposit one sub-span's exact signed area into the row.
///
/// `cells` is `x_hi - x_lo + 1` wide: the extra entry catches the spill from the
/// last in-tile cell and is never read, which keeps the deposit branch-free at
/// the tile's right edge.
///
/// Two separate decisions, and keeping them separate is what makes this safe:
///
/// - **Which column** the span belongs to is decided by the *chord* midpoint,
///   exactly as before. The walk guarantees the span lies inside one column, so
///   any interior point names it; using the endpoints' mean keeps the choice
///   agreeing with the clamp that the walk was tuned against, and keeps a span
///   sitting exactly on a boundary landing on the same side it always did.
/// - **How much of that column** lies right of the curve is
///   [`Coeffs::mean_x_over_dy`] — the integral, not the chord. The clamp to
///   `[0, 1]` repairs floating point only: the span is inside the column by
///   construction, so `x̄` is too.
#[allow(clippy::too_many_arguments)]
fn deposit<T: Real>(
    cells: &mut [T],
    carry: &mut T,
    x_lo: u32,
    x_hi: u32,
    c: &Coeffs<T>,
    t_a: T,
    t_b: T,
    x_a: T,
    x_b: T,
    y_a: T,
    y_b: T,
) {
    let d = y_b - y_a;
    if d == T::ZERO {
        return;
    }
    let xm = T::HALF * (x_a + x_b);
    let cell_f = xm.floor();
    let cell = cell_f.to_i64();
    if cell < i64::from(x_lo) {
        *carry = *carry + d;
        return;
    }
    if cell >= i64::from(x_hi) {
        return;
    }
    let xbar = c.mean_x_over_dy(t_a, t_b).unwrap_or(xm);
    let mut f = xbar - cell_f;
    if f < T::ZERO {
        f = T::ZERO;
    } else if f > T::ONE {
        f = T::ONE;
    }
    let i = (cell - i64::from(x_lo)) as usize;
    cells[i] = cells[i] + d * (T::ONE - f);
    cells[i + 1] = cells[i + 1] + d * f;
}

/// Accumulate one piece's contribution to one pixel row of one tile.
///
/// This is the whole algorithm, and both dispatch shapes are built from it:
/// [`fill_row`] calls it once per piece with the tile's full width, and
/// [`coverage_at_cell`] calls it once per piece with a one-cell window. The
/// per-cell work is bounded by `x_hi - x_lo + 2` iterations, which is what lets
/// an annex twin write a counted loop instead of a `while`.
pub fn accumulate_piece_row<T: Real>(
    piece: &MonoPiece,
    translate: [f64; 2],
    row_y: u32,
    x_lo: u32,
    x_hi: u32,
    cells: &mut [T],
    carry: &mut T,
) {
    let c = Coeffs::<T>::of(piece, translate);
    let row = T::from_u32(row_y);
    let row_end = row + T::ONE;

    // The piece's y-extent, and the part of it inside this scanline band.
    // Monotone in y, so the endpoints are the extremes.
    let y0 = c.y(T::ZERO);
    let y1 = c.y(T::ONE);
    let band_lo = fmax(fmin(y0, y1), row);
    let band_hi = fmin(fmax(y0, y1), row_end);
    if band_hi <= band_lo {
        return;
    }
    let u = c.t_at_y(band_lo, T::ZERO, T::ONE);
    let v = c.t_at_y(band_hi, T::ZERO, T::ONE);
    let ta = fmin(u, v);
    let tb = fmax(u, v);
    if tb <= ta {
        return;
    }

    let xa = c.x(ta);
    let xb = c.x(tb);
    let increasing = xb >= xa;
    let left = T::from_u32(x_lo);
    let right = T::from_u32(x_hi);

    // Where the piece crosses the tile's vertical edges. Monotone in x, so each
    // crossing is a single closed-form root, and `invert`'s clamp handles a
    // piece that never reaches an edge.
    let t_left = c.t_at_x(left, ta, tb);
    let t_right = c.t_at_x(right, ta, tb);

    // Everything left of the tile contributes its full signed dy to the row's
    // carry — in one subtraction, never by walking cells from the frame edge.
    let (l0, l1) = if increasing {
        (ta, t_left)
    } else {
        (t_left, tb)
    };
    if l1 > l0 {
        *carry = *carry + (c.y(l1) - c.y(l0));
    }

    // The in-tile span, walked column boundary by column boundary.
    let (mut t_prev, t_end) = if increasing {
        (t_left, t_right)
    } else {
        (t_right, t_left)
    };
    if t_end <= t_prev {
        return;
    }
    // Clamped into the tile, and this is load-bearing rather than tidy. The
    // walk's span is by construction the part of the piece inside the tile, so
    // its endpoints cannot lie outside `[x_lo, x_hi]` — but `x(t_prev)` is a
    // root solve's answer re-evaluated, so it lands an ulp *outside* about half
    // the time. Without the clamp, an entry at exactly the tile's right edge
    // computes as `208.0009`, the next boundary comes out as the column the walk
    // is already in, the step makes no progress, and the fallback below deposits
    // the entire remaining span — which crosses three columns — as a single
    // trapezoid in whichever column its midpoint lands in.
    let clamp_x = |v: T| {
        if v < left {
            left
        } else if v > right {
            right
        } else {
            v
        }
    };
    let mut x_prev = clamp_x(c.x(t_prev));
    let mut y_prev = c.y(t_prev);
    let x_end = clamp_x(c.x(t_end));
    let y_end = c.y(t_end);

    let steps = (x_hi - x_lo) as usize + 2;
    for _ in 0..steps {
        // The next integer column boundary strictly beyond `x_prev` in the
        // direction of travel.
        let boundary = if increasing {
            x_prev.floor() + T::ONE
        } else {
            x_prev.ceil() - T::ONE
        };
        let past = if increasing {
            boundary >= x_end
        } else {
            boundary <= x_end
        };
        let (mut t_next, mut x_next) = if past {
            (t_end, x_end)
        } else {
            // The crossing's x is the boundary *by construction*; using the
            // exact integer rather than re-evaluating `x(t)` keeps the cell
            // index unambiguous when the root solve lands an ulp to one side.
            (c.t_at_x(boundary, t_prev, t_end), boundary)
        };
        if t_next <= t_prev && !past {
            // The closed form could not separate two adjacent column
            // boundaries. Fall back to the secant in x — monotone, and it
            // advances unless the span is degenerate. Advancing to the boundary
            // matters more than the parameter's exactness here: what must never
            // happen is a multi-column span deposited as one trapezoid.
            let denom = x_end - x_prev;
            if denom != T::ZERO {
                t_next = t_prev + (t_end - t_prev) * ((boundary - x_prev) / denom);
            }
        }
        if t_next <= t_prev {
            // Genuinely degenerate: the remaining span cannot be subdivided, so
            // it fits in one column and finishing it here is exact.
            t_next = t_end;
            x_next = x_end;
        }
        let y_next = if t_next >= t_end { y_end } else { c.y(t_next) };
        deposit(
            cells, carry, x_lo, x_hi, &c, t_prev, t_next, x_prev, x_next, y_prev, y_next,
        );
        if t_next >= t_end {
            break;
        }
        t_prev = t_next;
        x_prev = x_next;
        y_prev = y_next;
    }
}

/// Nonzero-winding coverage of one path over one pixel row of one tile.
///
/// `cells` is scratch of length `x_hi - x_lo + 1`; `out` is `x_hi - x_lo` wide
/// and receives coverage in `[0, 1]`. Both are caller-owned so a frame allocates
/// once (PG-6's zero-steady-state-allocation requirement).
///
/// The absolute value **is** the nonzero rule: a region wound twice accumulates
/// to `2` and clamps to full coverage, and a region wound `+1` then `-1` cancels
/// to nothing. No orientation bookkeeping, exactly as §10.2 promises.
pub fn fill_row<T: Real>(
    pieces: &[MonoPiece],
    translate: [f64; 2],
    row_y: u32,
    x_lo: u32,
    x_hi: u32,
    cells: &mut [T],
    out: &mut [T],
) {
    let w = (x_hi - x_lo) as usize;
    debug_assert_eq!(out.len(), w);
    debug_assert_eq!(cells.len(), w + 1);
    for c in cells.iter_mut() {
        *c = T::ZERO;
    }
    let mut carry = T::ZERO;
    for piece in pieces {
        accumulate_piece_row(piece, translate, row_y, x_lo, x_hi, cells, &mut carry);
    }
    let mut running = carry;
    for i in 0..w {
        running = running + cells[i];
        let a = running.abs();
        out[i] = if a > T::ONE { T::ONE } else { a };
    }
}

/// Nonzero-winding coverage of one path at one pixel, with no accumulator and no
/// scan.
///
/// The same terms as [`fill_row`] in a different association: per piece, the
/// winding that passed to this cell's left plus this cell's own trapezoid. That
/// makes the per-pixel dispatch shape available to the annex — the same
/// one-thread-per-pixel shape the stroke kernel already uses — at the cost of
/// re-deriving each piece's scanline band once per pixel instead of once per
/// row.
pub fn coverage_at_cell<T: Real>(
    pieces: &[MonoPiece],
    translate: [f64; 2],
    row_y: u32,
    cell: u32,
) -> T {
    let mut acc = T::ZERO;
    let mut window = [T::ZERO; 2];
    for piece in pieces {
        window[0] = T::ZERO;
        window[1] = T::ZERO;
        let mut carry = T::ZERO;
        accumulate_piece_row(
            piece,
            translate,
            row_y,
            cell,
            cell + 1,
            &mut window,
            &mut carry,
        );
        acc = acc + carry + window[0];
    }
    let a = acc.abs();
    if a > T::ONE { T::ONE } else { a }
}

/// The winding number of a filled path at a screen point.
///
/// Not a coverage query — an exact integer, used by the interior colour field to
/// decide insideness and by tests to state the nonzero rule directly. Counts
/// signed crossings of the horizontal ray to the **left** of `p`, which is the
/// same bookkeeping the row accumulator does continuously, so the two can never
/// disagree about which side of a boundary a pixel is on.
///
/// Sign convention: a counter-clockwise boundary yields `+1`, the standard
/// winding number. With a leftward ray that means an *upward* crossing counts
/// `-1` — the mirror of the textbook rightward-ray rule, and worth spelling out
/// because getting it backwards passes every `abs()`-based coverage test and
/// then reports the wrong sign to the one caller that cares.
#[must_use]
pub fn winding_at(pieces: &[MonoPiece], translate: [f64; 2], p: [f64; 2]) -> i32 {
    let mut w = 0;
    for piece in pieces {
        let c = Coeffs::<f64>::of(piece, translate);
        let y0 = c.y(0.0);
        let y1 = c.y(1.0);
        // Half-open in y so a shared anchor is counted once, not twice or zero
        // times — the classic off-by-one that makes a closed path leak.
        let up = y1 > y0;
        let (lo, hi) = if up { (y0, y1) } else { (y1, y0) };
        if p[1] < lo || p[1] >= hi {
            continue;
        }
        let t = c.t_at_y(p[1], 0.0, 1.0);
        if c.x(t) < p[0] {
            w += if up { -1 } else { 1 };
        }
    }
    w
}

/// Per-frame scratch for the fill's row loop, allocated once and reused.
///
/// PG-6 forbids steady-state per-frame heap allocation, so the row buffers are
/// owned by the caller and sized for the widest tile a frame will use.
#[derive(Debug, Clone, Default)]
pub struct RowScratch {
    cells: Vec<f64>,
    out: Vec<f64>,
}

impl RowScratch {
    /// Scratch sized for a tile `tile` pixels wide.
    #[must_use]
    pub fn for_tile(tile: u32) -> RowScratch {
        let w = tile as usize;
        RowScratch {
            cells: vec![0.0; w + 1],
            out: vec![0.0; w],
        }
    }

    /// Grow to hold a `width`-pixel row, if it does not already.
    ///
    /// Amortized: after the first frame no tile is wider than the widest tile
    /// seen, so this stops allocating. Sizing up rather than reallocating per
    /// row is the difference between PG-6 passing and PG-6 being a comment.
    pub fn reserve(&mut self, width: usize) {
        if self.out.len() < width {
            self.out.resize(width, 0.0);
        }
        if self.cells.len() < width + 1 {
            self.cells.resize(width + 1, 0.0);
        }
    }

    /// Coverage of one path over one pixel row, at the host width.
    ///
    /// The engine's entry point: [`fill_row`] with the scratch managed and the
    /// result handed back as a slice.
    pub fn fill_row(
        &mut self,
        pieces: &[MonoPiece],
        translate: [f64; 2],
        row_y: u32,
        x_lo: u32,
        x_hi: u32,
    ) -> &[f64] {
        let w = (x_hi.saturating_sub(x_lo)) as usize;
        self.reserve(w);
        fill_row(
            pieces,
            translate,
            row_y,
            x_lo,
            x_hi,
            &mut self.cells[..w + 1],
            &mut self.out[..w],
        );
        &self.out[..w]
    }
}

// ------------------------------------------------- primitive-hint fill kernels

/// The screen-space budget under which a *bounded-error* fill hint is admitted.
///
/// One 1/256th of a pixel — §10.2's own acceptance tolerance, reused here rather
/// than a second number nobody reconciled.
pub const HINT_BUDGET_PX: f64 = 1.0 / 256.0;

/// A hinted fill's coverage: a closed form with no walk and no accumulator.
///
/// ## The rule these kernels have to obey, and where it bites
///
/// [`crate::hint`]'s rule is absolute — *"a kernel selected by a hint must
/// produce the same answer the general path would, so dropping every hint can
/// only cost speed"*. For [`FillKernel::Rect`] that is free: an axis-aligned
/// rectangle's coverage is a product of two clamped intervals, and the general
/// path computes the same region's exact area from straight segments, so the two
/// agree to floating point.
///
/// For a disc it is **not** free, and pretending otherwise would be the
/// interesting bug in this module. A `Circle`'s or `Dot`'s compiled outline is
/// manim's *quadratic approximation* of a circle, which encloses `(π/n)⁴/6` more
/// area than the circle does — 2.5e-4 relative at the default 16 components,
/// 6.3e-2 at 4. A kernel that rasterizes the true disc therefore draws a
/// **different shape** from the general path, by up to `r·(π/n)⁴/8` pixels of
/// edge displacement. So the disc kernel is admitted the way §10.8 admits its
/// one other approximating route — *"nearly-linear quadratic → line fast path
/// under an explicit screen-space error bound"* — except that the bound here is
/// **measured off the compiled outline** rather than assumed: [`FillKernel::select`]
/// evaluates each segment's midpoint, takes the worst radial deviation from the
/// hinted radius, scales it to pixels, and declines the hint when it exceeds
/// [`HINT_BUDGET_PX`].
///
/// The practical effect is the shape of the rule, not a compromise with it: a
/// `Dot` at manim's default radius (0.08 units, ~10.8 px at the Reference's
/// 135 px/unit) deviates ~0.002 px and takes the kernel; a two-unit `Circle`
/// deviates ~0.05 px and draws through the general path. Dots are what the
/// radial kernel was for.
///
/// `Arc` is deliberately absent. Filling a partial arc closes it with a chord,
/// so the region is a circular *segment* rather than a disc, and the hint would
/// have to carry the chord to be right. It routes to the general path, which is
/// exact.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FillKernel {
    /// No hinted route: the general quadratic machinery.
    General,
    /// Axis-aligned rectangle, screen pixels, `[x0, y0, x1, y1]`.
    Rect {
        /// The rectangle.
        rect: [f64; 4],
    },
    /// A filled disc, screen pixels.
    Disc {
        /// Centre.
        center: [f64; 2],
        /// Radius.
        radius: f64,
    },
    /// An axis-aligned rectangle with circular corners, screen pixels.
    RoundedRect {
        /// The bounding rectangle.
        rect: [f64; 4],
        /// Corner radius, already clamped to half the shorter side.
        radius: f64,
    },
}

impl FillKernel {
    /// Choose a kernel for one occurrence of a compiled shape.
    ///
    /// `segments` is the shape's own slice of the IR's segment table, in object
    /// space; `translate` is [`instance_translation`]. Returns
    /// [`FillKernel::General`] whenever the hint is absent, inapplicable, or
    /// outside [`HINT_BUDGET_PX`] — the "when in doubt, draw" direction.
    #[must_use]
    pub fn select(
        shape: &crate::table::Shape,
        segments: &[crate::table::Segment],
        map: ScreenMap,
        translate: [f64; 2],
    ) -> FillKernel {
        let to_px = |p: [f64; 3]| {
            [
                map.origin[0] + p[0] * map.scale + translate[0],
                map.origin[1] + p[1] * map.scale + translate[1],
            ]
        };
        let scale = map.scale.abs();
        match shape.hint {
            crate::hint::Hint::Rect {
                center,
                width,
                height,
            } => {
                let c = to_px(center);
                let (hw, hh) = (0.5 * width * scale, 0.5 * height * scale);
                FillKernel::Rect {
                    rect: [c[0] - hw, c[1] - hh, c[0] + hw, c[1] + hh],
                }
            }
            crate::hint::Hint::RoundedRect {
                center,
                width,
                height,
                corner_radius,
            } => {
                let c = to_px(center);
                let (hw, hh) = (0.5 * width * scale, 0.5 * height * scale);
                let r = (corner_radius * scale).clamp(0.0, hw.min(hh));
                FillKernel::RoundedRect {
                    rect: [c[0] - hw, c[1] - hh, c[0] + hw, c[1] + hh],
                    radius: r,
                }
            }
            crate::hint::Hint::Circle { center, radius }
            | crate::hint::Hint::Dot { center, radius } => {
                let r = radius * scale;
                if r <= 0.0 || !Self::disc_within_budget(center, radius, segments, scale) {
                    return FillKernel::General;
                }
                FillKernel::Disc {
                    center: to_px(center),
                    radius: r,
                }
            }
            _ => FillKernel::General,
        }
    }

    /// Is the compiled outline's worst radial deviation from the hinted circle
    /// under [`HINT_BUDGET_PX`] once scaled to pixels?
    ///
    /// Measured, not assumed. A quadratic through two points on a circle with
    /// the tangent-intersection handle deviates most at its own midpoint, so one
    /// evaluation per segment finds the worst case; using the *actual* points
    /// also means a hand-edited outline that still carries the tag cannot slip
    /// past on the strength of a formula.
    fn disc_within_budget(
        center: [f64; 3],
        radius: f64,
        segments: &[crate::table::Segment],
        scale: f64,
    ) -> bool {
        if segments.is_empty() {
            return false;
        }
        let mut worst = 0.0f64;
        for s in segments {
            // B(1/2) = (p0 + 2 p1 + p2) / 4.
            let mid = [
                0.25 * (s.p0[0] + 2.0 * s.p1[0] + s.p2[0]) - center[0],
                0.25 * (s.p0[1] + 2.0 * s.p1[1] + s.p2[1]) - center[1],
            ];
            let d = (mid[0] * mid[0] + mid[1] * mid[1]).sqrt();
            worst = worst.max((d - radius).abs());
        }
        worst * scale <= HINT_BUDGET_PX
    }

    /// Exact coverage of one pixel, or `None` for [`FillKernel::General`].
    #[must_use]
    pub fn coverage(&self, px: u32, py: u32) -> Option<f64> {
        let (x0, y0) = (f64::from(px), f64::from(py));
        let (x1, y1) = (x0 + 1.0, y0 + 1.0);
        Some(match *self {
            FillKernel::General => return None,
            FillKernel::Rect { rect } => box_overlap(rect, [x0, y0, x1, y1]),
            FillKernel::Disc { center, radius } => disc_box_area(center, radius, [x0, y0, x1, y1]),
            FillKernel::RoundedRect { rect, radius } => {
                rounded_rect_box_area(rect, radius, [x0, y0, x1, y1])
            }
        })
    }

    /// Exact coverage of a run of pixels in one row.
    ///
    /// The same values [`FillKernel::coverage`] gives, so a caller may use
    /// either; this exists because a row is the unit the engine schedules and a
    /// hinted fill should not pay a match per pixel.
    pub fn row(&self, row_y: u32, x_lo: u32, x_hi: u32, out: &mut [f64]) -> bool {
        let w = (x_hi.saturating_sub(x_lo)) as usize;
        debug_assert_eq!(out.len(), w);
        if matches!(self, FillKernel::General) {
            return false;
        }
        for (i, o) in out.iter_mut().enumerate() {
            *o = self
                .coverage(x_lo + i as u32, row_y)
                .expect("not the general kernel");
        }
        true
    }
}

/// Area of the intersection of two axis-aligned rectangles.
fn box_overlap(a: [f64; 4], b: [f64; 4]) -> f64 {
    let w = (a[2].min(b[2]) - a[0].max(b[0])).max(0.0);
    let h = (a[3].min(b[3]) - a[1].max(b[1])).max(0.0);
    w * h
}

/// `∫ √(r² − v²) dv` — the antiderivative every disc-area term is built from.
///
/// `asin` routes through fmn-dmath (§6.6, D-17): a hinted fill must produce the
/// same bits on every certified target as the general path it stands in for, and
/// `f64::asin` defers to the platform's libm. `sqrt` is used directly — IEEE 754
/// requires it correctly rounded, so it is already reproducible.
/// The half-chord `√(r² − v²)`, factored so it does not cancel.
///
/// `r*r - v*v` loses every digit of the difference as `|v| → r`, and because the
/// result is then square-rooted, an absolute error `ε` in the radicand becomes
/// `√ε` in the answer: at `r = 6` the `4e-15` left by cancellation came out as
/// `6e-8` of half-chord. `(r − v)(r + v)` is the same quantity with each factor
/// computed to its own last bit, so the relative error stays at one ulp.
///
/// This is not academic. It showed up as a **3e-8 area error in the rounded
/// rectangle** and nowhere else: a disc's coverage summed over a pixel grid
/// telescopes — adjacent boxes share their corner evaluations — so the plain
/// disc oracle reported 4e-12 and hid it completely. Only the corner
/// decomposition, which clips each square differently and cannot telescope,
/// exposed the conditioning.
fn half_chord(r: f64, v: f64) -> f64 {
    ((r - v) * (r + v)).max(0.0).sqrt()
}

/// `∫ √(r² − v²) dv = ½(v√(r² − v²) + r² asin(v/r))`, with the inverse
/// trigonometry written as `atan2` because `asin` is the wrong function here.
///
/// `asin` has an infinite derivative at ±1, so a one-ulp error in `v/r` becomes
/// an error of `1e-16/√(2δ)` in the angle when `v/r = 1 − δ`. At `r = 6` with `v`
/// a hair under `r` that is `~8e-9` of angle and, after the `r²/2` factor,
/// **1.4e-7 of area** — and pixels sit a hair under `r` at every disc's left and
/// right extremes, so this is the common case rather than a corner one.
/// `atan2(v, √(r² − v²))` is the same angle: the point `(√(r² − v²), v)` lies on
/// the circle of radius `r`, so its argument is exactly `asin(v/r)` for
/// `v ∈ [−r, r]`. It is also *well* conditioned there — as the second argument
/// goes to zero the angle goes to ±π/2 linearly in it — which turns 1.4e-7 into
/// one ulp. Both routes go through fmn-dmath (§6.6, D-17); this one is the route
/// that is right.
fn circle_antiderivative(r: f64, v: f64) -> f64 {
    let vc = v.clamp(-r, r);
    0.5 * (vc * half_chord(r, vc) + r * r * fmn_dmath::atan2(vc, half_chord(r, vc)))
}

/// Area of `{ (u, v) : u² + v² ≤ r², u ≤ x, v ≤ y }`.
///
/// The disc-area primitive, in the one form that composes: with it, a disc's
/// intersection with any axis-aligned rectangle is four evaluations and an
/// inclusion–exclusion, and no case analysis is needed at the call site.
///
/// Derivation. The area is `∫_{-r}^{min(y,r)} h(v) dv` with
/// `h(v) = clamp(x, −s, s) + s` and `s(v) = √(r² − v²)`, because `h` is the
/// length of the slice `{ u : u ≤ x, u² ≤ r² − v² }`. `h` switches form where
/// `s(v) = |x|`, i.e. at `v = ±a` with `a = √(r² − x²)`: inside `(−a, a)` it is
/// `x + s`; outside it is `2s` when `x > 0` and `0` when `x < 0`. Each piece
/// integrates in closed form via [`circle_antiderivative`].
fn disc_quadrant_area(r: f64, x: f64, y: f64) -> f64 {
    if r <= 0.0 || y <= -r || x <= -r {
        return 0.0;
    }
    let yt = y.min(r);
    let s = |v: f64| circle_antiderivative(r, v);
    if x >= r {
        return 2.0 * (s(yt) - s(-r));
    }
    let a = half_chord(r, x);
    let mut acc = 0.0;
    // The middle band, where the slice is cut by `x` on one side only.
    let hi2 = yt.min(a);
    if hi2 > -a {
        acc += x * (hi2 - (-a)) + (s(hi2) - s(-a));
    }
    if x > 0.0 {
        // The two caps, where the whole chord is left of `x`.
        let hi1 = yt.min(-a);
        if hi1 > -r {
            acc += 2.0 * (s(hi1) - s(-r));
        }
        if yt > a {
            acc += 2.0 * (s(yt) - s(a));
        }
    }
    acc
}

/// Area of a disc intersected with an axis-aligned rectangle, exactly.
fn disc_box_area(center: [f64; 2], radius: f64, rect: [f64; 4]) -> f64 {
    let f = |x: f64, y: f64| disc_quadrant_area(radius, x - center[0], y - center[1]);
    (f(rect[2], rect[3]) - f(rect[0], rect[3]) - f(rect[2], rect[1]) + f(rect[0], rect[1])).max(0.0)
}

/// Area of a rounded rectangle intersected with an axis-aligned rectangle.
///
/// The rounded rectangle is the full rectangle minus, at each corner, the part
/// of the corner's `r × r` square that lies outside the corner's quarter disc.
/// Both of those are box intersections, so the whole thing is
/// `box_overlap − Σ (corner-square overlap − corner-disc overlap)` — five exact
/// terms, no new geometry.
fn rounded_rect_box_area(rect: [f64; 4], radius: f64, b: [f64; 4]) -> f64 {
    let mut area = box_overlap(rect, b);
    if radius <= 0.0 {
        return area;
    }
    let r = radius;
    let corners = [
        (
            [rect[0], rect[1], rect[0] + r, rect[1] + r],
            [rect[0] + r, rect[1] + r],
        ),
        (
            [rect[2] - r, rect[1], rect[2], rect[1] + r],
            [rect[2] - r, rect[1] + r],
        ),
        (
            [rect[0], rect[3] - r, rect[0] + r, rect[3]],
            [rect[0] + r, rect[3] - r],
        ),
        (
            [rect[2] - r, rect[3] - r, rect[2], rect[3]],
            [rect[2] - r, rect[3] - r],
        ),
    ];
    for (square, centre) in corners {
        let clipped = [
            square[0].max(b[0]),
            square[1].max(b[1]),
            square[2].min(b[2]),
            square[3].min(b[3]),
        ];
        if clipped[2] <= clipped[0] || clipped[3] <= clipped[1] {
            continue;
        }
        let square_part = (clipped[2] - clipped[0]) * (clipped[3] - clipped[1]);
        let disc_part = disc_box_area(centre, r, clipped);
        area -= (square_part - disc_part).max(0.0);
    }
    area.max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmn_core::types::Vec3;
    use fmn_geom::bezier;
    use fmn_geom::quadpath::QuadPath;

    /// Screen-space pieces straight from a path, with no plan in the way.
    ///
    /// Groups by subpath and closes each one, exactly as [`MonoTable::build`]
    /// does — a helper that skipped the closing would test a different fill from
    /// the one that ships.
    fn pieces_of_path(path: &QuadPath, map: ScreenMap) -> Vec<MonoPiece> {
        let mut out = Vec::new();
        let s = |p: Vec3| {
            [
                map.origin[0] + p[0] * map.scale,
                map.origin[1] + p[1] * map.scale,
            ]
        };
        let mut curves: Vec<[[f64; 2]; 3]> = Vec::new();
        for i in 0..path.num_curves() {
            let Some([p0, p1, p2]) = path.nth_curve_points(i) else {
                continue;
            };
            if p0 == p1 && p1 == p2 {
                append_subpath(&curves, &mut out);
                curves.clear();
                continue;
            }
            curves.push([s(p0), s(p1), s(p2)]);
        }
        append_subpath(&curves, &mut out);
        out
    }

    /// The exact area a closed screen-space path encloses, by Green's theorem.
    ///
    /// `∮ x dy` summed over the monotone pieces, in closed form — the same
    /// integral [`Coeffs::mean_x_over_dy`] evaluates, taken over whole pieces.
    /// This is the *right* oracle for a fill that claims to be exact on curves:
    /// comparing a quadratic circle's coverage against `πr²` measures the
    /// approximation of the circle, whereas comparing it against this measures
    /// the rasterizer. Only valid where no region is wound more than once.
    fn enclosed_area(pieces: &[MonoPiece]) -> f64 {
        let mut a = 0.0;
        for piece in pieces {
            let c = Coeffs::<f64>::of(piece, [0.0, 0.0]);
            let s1 = 1.0;
            let s2 = 1.0;
            let s3 = 1.0;
            let k0 = c.ax * c.by;
            let k1 = c.bx * c.by + 2.0 * c.ax * c.cy;
            let k2 = c.cx * c.by + 2.0 * c.bx * c.cy;
            let k3 = 2.0 * c.cx * c.cy;
            a += k0 + k1 * s1 / 2.0 + k2 * s2 / 3.0 + k3 * s3 / 4.0;
        }
        a.abs()
    }

    fn unit() -> ScreenMap {
        ScreenMap {
            scale: 1.0,
            origin: [0.0, 0.0],
        }
    }

    fn circle_path(cx: f64, cy: f64, r: f64, n: usize) -> QuadPath {
        let pts = bezier::quadratic_points_for_arc(std::f64::consts::TAU, n);
        let placed: Vec<Vec3> = pts
            .iter()
            .map(|p| [cx + r * p[0], cy + r * p[1], 0.0])
            .collect();
        QuadPath::from_points(placed).expect("closed arc is a valid path")
    }

    fn polygon(points: &[[f64; 2]]) -> QuadPath {
        let mut p = QuadPath::default();
        p.start_new_path([points[0][0], points[0][1], 0.0]);
        for q in &points[1..] {
            p.add_line_to([q[0], q[1], 0.0], false).unwrap();
        }
        p.add_line_to([points[0][0], points[0][1], 0.0], false)
            .unwrap();
        p
    }

    /// Total coverage over a pixel window, tile by tile — the integral the area
    /// oracles compare against.
    fn total_coverage(pieces: &[MonoPiece], w: u32, h: u32, tile: u32) -> f64 {
        let mut scratch = RowScratch::for_tile(tile);
        let mut sum = 0.0;
        for y in 0..h {
            let mut x = 0;
            while x < w {
                let hi = (x + tile).min(w);
                sum += scratch
                    .fill_row(pieces, [0.0, 0.0], y, x, hi)
                    .iter()
                    .sum::<f64>();
                x = hi;
            }
        }
        sum
    }

    #[test]
    fn a_rectangle_covers_exactly_its_area() {
        // The simplest oracle, and the one that catches a sign error before any
        // curve is involved: an axis-aligned box on non-integer bounds.
        let path = polygon(&[[2.5, 3.25], [17.75, 3.25], [17.75, 12.5], [2.5, 12.5]]);
        let pieces = pieces_of_path(&path, unit());
        let want = (17.75 - 2.5) * (12.5 - 3.25);
        let got = total_coverage(&pieces, 24, 16, 8);
        assert!((got - want).abs() < 1e-9, "{got} vs {want}");
    }

    #[test]
    fn a_disc_covers_exactly_the_area_it_encloses() {
        // The area oracle §10.2 names, in the form that measures the rasterizer
        // rather than the geometry. The path is manim's own quadratic
        // approximation of a circle, so `πr²` is the wrong target by ~2.6e-4
        // relative — the approximation's error, not ours. The exact target is
        // the area the quadratics actually enclose, and the fill must hit it to
        // far better than one 8-bit level over the whole disc.
        for n in [4usize, 8, 16, 64] {
            let r = 20.0;
            let path = circle_path(32.3, 31.7, r, n);
            let pieces = pieces_of_path(&path, unit());
            let got = total_coverage(&pieces, 64, 64, 16);
            let want = enclosed_area(&pieces);
            assert!(
                (got - want).abs() < 1e-9,
                "n={n}: coverage {got} vs enclosed {want}"
            );
            // And the enclosed area is a circle's, to the arc approximation's
            // own closed-form error — not to a tolerance someone picked. With
            // `u = θ/2 = π/n`, `cos u + sec u = 2 + u⁴/4 + O(u⁶)`, so the
            // quadratic's midpoint sits at radius `r(1 + u⁴/8)` and the enclosed
            // area runs `(4/3)·u⁴/8 = u⁴/6` high. That predicts 6.3e-2 at n=4 and
            // 2.5e-4 at n=16; both are what this measures.
            let circle = std::f64::consts::PI * r * r;
            let rel = (want - circle).abs() / circle;
            let u = std::f64::consts::PI / n as f64;
            let predicted = u.powi(4) / 6.0;
            assert!(
                rel < 1.1 * predicted,
                "n={n}: rel {rel} vs predicted {predicted}"
            );
        }
    }

    #[test]
    fn a_triangle_covers_exactly_its_area() {
        let path = polygon(&[[1.0, 1.0], [21.0, 1.0], [1.0, 16.0]]);
        let pieces = pieces_of_path(&path, unit());
        let got = total_coverage(&pieces, 24, 20, 8);
        let want = 0.5 * 20.0 * 15.0;
        assert!((got - want).abs() < 1e-9, "{got} vs {want}");
    }

    #[test]
    fn opposite_windings_cancel_to_a_hole() {
        // The nonzero rule stated as an area: an outer square wound one way and
        // an inner square wound the other leaves an annulus, and the inner
        // region must read exactly zero rather than "twice, clamped".
        let outer = polygon(&[[4.0, 4.0], [28.0, 4.0], [28.0, 28.0], [4.0, 28.0]]);
        let inner = polygon(&[[12.0, 12.0], [12.0, 20.0], [20.0, 20.0], [20.0, 12.0]]);
        let mut pieces = pieces_of_path(&outer, unit());
        pieces.extend(pieces_of_path(&inner, unit()));

        let got = total_coverage(&pieces, 32, 32, 8);
        let want = 24.0 * 24.0 - 8.0 * 8.0;
        assert!((got - want).abs() < 1e-9, "{got} vs {want}");
        // And a point in the hole is genuinely empty, not merely averaged away.
        assert_eq!(coverage_at_cell::<f64>(&pieces, [0.0, 0.0], 16, 16), 0.0);
        assert_eq!(winding_at(&pieces, [0.0, 0.0], [16.5, 16.5]), 0);
        assert_eq!(winding_at(&pieces, [0.0, 0.0], [8.5, 16.5]), 1);
    }

    #[test]
    fn same_winding_twice_clamps_instead_of_doubling() {
        // Two coincident squares wound the same way accumulate to 2. The nonzero
        // rule says the region is inside, i.e. coverage 1 — not 2, and not 0.
        let sq = polygon(&[[4.0, 4.0], [12.0, 4.0], [12.0, 12.0], [4.0, 12.0]]);
        let mut pieces = pieces_of_path(&sq, unit());
        pieces.extend(pieces_of_path(&sq, unit()));
        let got = total_coverage(&pieces, 16, 16, 8);
        assert!((got - 64.0).abs() < 1e-9, "{got} vs 64");
        assert_eq!(coverage_at_cell::<f64>(&pieces, [0.0, 0.0], 8, 8), 1.0);
    }

    #[test]
    fn tile_alignment_never_changes_the_answer() {
        // G0-8b's second rule: a tiling test that does not sweep alignments sees
        // nothing, because no piece ever enters at a tile edge. Sweeping the
        // tile size sweeps every phase a piece can enter at.
        let path = circle_path(31.7, 30.3, 19.4, 16);
        let pieces = pieces_of_path(&path, unit());
        let reference = total_coverage(&pieces, 64, 64, 64);
        for tile in [1u32, 2, 3, 5, 7, 8, 11, 16, 32] {
            let got = total_coverage(&pieces, 64, 64, tile);
            assert!(
                (got - reference).abs() < 1e-9,
                "tile {tile}: {got} vs {reference}"
            );
        }
    }

    #[test]
    fn the_per_pixel_form_agrees_with_the_scanline_form() {
        // The two dispatch shapes are the same terms in a different association.
        // They are compared per pixel rather than in aggregate, and at a tile
        // width that puts pieces across tile edges.
        let path = circle_path(20.4, 19.6, 12.3, 16);
        let pieces = pieces_of_path(&path, unit());
        let mut scratch = RowScratch::for_tile(8);
        let mut worst = 0.0f64;
        for y in 0..40 {
            let mut x = 0;
            while x < 40 {
                let hi = (x + 8).min(40);
                let row = scratch.fill_row(&pieces, [0.0, 0.0], y, x, hi).to_vec();
                for (i, v) in row.iter().enumerate() {
                    let per_pixel: f64 = coverage_at_cell(&pieces, [0.0, 0.0], y, x + i as u32);
                    worst = worst.max((v - per_pixel).abs());
                }
                x = hi;
            }
        }
        assert!(worst < 1e-12, "worst per-pixel disagreement {worst}");
    }

    #[test]
    fn subdividing_a_curve_does_not_change_the_fill() {
        // §10.2's metamorphic law. Splitting every segment at its midpoint is a
        // different point array describing the same region, so every pixel's
        // coverage must survive it.
        let path = circle_path(24.3, 23.7, 15.1, 8);
        let mut split = QuadPath::default();
        for i in 0..path.num_curves() {
            let [p0, p1, p2] = path.nth_curve_points(i).unwrap();
            let mid = |a: Vec3, b: Vec3| [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0, 0.0];
            let q0 = mid(p0, p1);
            let q1 = mid(p1, p2);
            let r = mid(q0, q1);
            if i == 0 {
                split.start_new_path(p0);
            }
            split.add_quadratic_bezier_curve_to(q0, r, false).unwrap();
            split.add_quadratic_bezier_curve_to(q1, p2, false).unwrap();
        }

        let a = pieces_of_path(&path, unit());
        let b = pieces_of_path(&split, unit());
        assert!(b.len() > a.len(), "the subdivision must add pieces");

        let mut sa = RowScratch::for_tile(16);
        let mut sb = RowScratch::for_tile(16);
        let mut worst = 0.0f64;
        for y in 0..48 {
            let ra = sa.fill_row(&a, [0.0, 0.0], y, 0, 48).to_vec();
            let rb = sb.fill_row(&b, [0.0, 0.0], y, 0, 48).to_vec();
            for (u, v) in ra.iter().zip(rb.iter()) {
                worst = worst.max((u - v).abs());
            }
        }
        assert!(
            worst < 1e-12,
            "worst per-pixel drift under subdivision {worst} — the deposit is \
             using a chord where §10.2 wants the integral"
        );
    }

    #[test]
    fn a_translated_occurrence_is_the_same_coverage_moved() {
        // The interning claim, at the fill: one outline plus a translation must
        // give the same pixels as the outline built at the destination. An
        // integer translation makes the comparison exact.
        let path = circle_path(16.0, 16.0, 9.5, 16);
        let here = pieces_of_path(&path, unit());
        let there = pieces_of_path(&circle_path(16.0 + 7.0, 16.0 + 5.0, 9.5, 16), unit());

        let mut s1 = RowScratch::for_tile(16);
        let mut s2 = RowScratch::for_tile(16);
        let mut worst = 0.0f64;
        for y in 0..40 {
            let a = s1.fill_row(&here, [7.0, 5.0], y, 0, 40).to_vec();
            let b = s2.fill_row(&there, [0.0, 0.0], y, 0, 40).to_vec();
            for (u, v) in a.iter().zip(b.iter()) {
                worst = worst.max((u - v).abs());
            }
        }
        assert!(worst < 1e-12, "translation drift {worst}");
    }

    #[test]
    fn the_annex_width_tracks_the_host_width() {
        // The f32 instantiation is the annex's arithmetic without the annex's
        // hardware — the controlled experiment G0-8 ran for strokes, kept
        // answerable on a machine with no GPU. The budget is f32's, not the
        // algorithm's: a coverage in [0,1] carries ~1e-7 of representation.
        let path = circle_path(24.4, 23.6, 15.2, 16);
        let pieces = pieces_of_path(&path, unit());
        let mut cells64 = vec![0.0f64; 49];
        let mut out64 = vec![0.0f64; 48];
        let mut cells32 = vec![0.0f32; 49];
        let mut out32 = vec![0.0f32; 48];
        let mut worst = 0.0f64;
        for y in 0..48 {
            fill_row(&pieces, [0.0, 0.0], y, 0, 48, &mut cells64, &mut out64);
            fill_row(&pieces, [0.0, 0.0], y, 0, 48, &mut cells32, &mut out32);
            for (u, v) in out64.iter().zip(out32.iter()) {
                worst = worst.max((u - f64::from(*v)).abs());
            }
        }
        assert!(worst < 1e-5, "host/annex width divergence {worst}");
    }

    #[test]
    fn a_self_intersecting_path_follows_the_nonzero_rule() {
        // A pentagram: the central pentagon is wound twice and is therefore
        // *inside* under the nonzero rule — the visible difference from
        // even-odd, and the reason §10.2 names the rule rather than assuming it.
        let mut pts = Vec::new();
        let r = 20.0;
        for k in 0..5 {
            // Every second vertex, which is what makes the star self-intersect.
            let a = std::f64::consts::FRAC_PI_2 + (k as f64) * 4.0 * std::f64::consts::PI / 5.0;
            pts.push([24.0 + r * a.cos(), 24.0 + r * a.sin()]);
        }
        let path = polygon(&pts);
        let pieces = pieces_of_path(&path, unit());
        assert_eq!(
            winding_at(&pieces, [0.0, 0.0], [24.0, 24.0]).abs(),
            2,
            "the centre of a pentagram is wound twice"
        );
        assert_eq!(
            coverage_at_cell::<f64>(&pieces, [0.0, 0.0], 24, 24),
            1.0,
            "and is therefore fully inside"
        );

        // Area check: a {5/2} star polygon of circumradius r has area
        // 5 r² tan(π/5) (1 - tan(π/5) tan(π/10))... rather than trust a formula
        // transcribed from memory, compare against the polygon area computed by
        // the shoelace rule over the *filled* region, which for the nonzero rule
        // is the star's outline plus its doubly-wound core counted once.
        let got = total_coverage(&pieces, 48, 48, 8);
        // Shoelace over the traversal counts the core twice; the nonzero fill
        // counts it once, so the fill must be strictly less.
        let mut shoelace = 0.0;
        for k in 0..pts.len() {
            let a = pts[k];
            let b = pts[(k + 1) % pts.len()];
            shoelace += a[0] * b[1] - b[0] * a[1];
        }
        let traversal_area = 0.5 * shoelace.abs();
        assert!(
            got < traversal_area - 1.0,
            "nonzero coverage {got} should undercount the doubly-wound core \
             (traversal area {traversal_area})"
        );
    }

    #[test]
    fn an_open_subpath_still_fills_as_if_closed() {
        // manim fills open paths by treating the closing chord as present, which
        // is what the winding accumulation does naturally: a path that does not
        // return to its start still deposits a net dy, and the rule is the same
        // one the closed case uses. The oracle is the implied triangle.
        let mut p = QuadPath::default();
        p.start_new_path([4.0, 4.0, 0.0]);
        p.add_line_to([20.0, 4.0, 0.0], false).unwrap();
        p.add_line_to([20.0, 16.0, 0.0], false).unwrap();
        let pieces = pieces_of_path(&p, unit());
        let got = total_coverage(&pieces, 24, 20, 8);
        assert!((got - 0.5 * 16.0 * 12.0).abs() < 1e-9, "{got}");
    }

    #[test]
    fn a_screen_map_scale_scales_the_area() {
        // The mapping is the fill's only view of the camera today, so its two
        // knobs get an assertion each: a 3x scale is 9x the area, and an origin
        // shift moves the picture without changing it.
        let path = polygon(&[[1.0, 1.0], [5.0, 1.0], [5.0, 4.0], [1.0, 4.0]]);
        let big = pieces_of_path(
            &path,
            ScreenMap {
                scale: 3.0,
                origin: [2.0, 1.0],
            },
        );
        let got = total_coverage(&big, 32, 32, 8);
        assert!((got - 9.0 * 12.0).abs() < 1e-9, "{got}");
    }

    #[test]
    fn an_empty_piece_list_covers_nothing() {
        let mut scratch = RowScratch::for_tile(8);
        let row = scratch.fill_row(&[], [0.0, 0.0], 3, 0, 8);
        assert!(row.iter().all(|v| *v == 0.0));
        assert_eq!(coverage_at_cell::<f64>(&[], [0.0, 0.0], 3, 4), 0.0);
    }

    #[test]
    fn the_table_derives_one_split_per_interned_outline() {
        // The §10.8 property, checked at the fill's own table: three occurrences
        // of one glyph must split once.
        //
        // The outline's coordinates are exactly representable and the shift is an
        // integer, and that is load-bearing rather than tidy. `shape_digest`
        // hashes `p - points[0]` bitwise, and floating-point translation is not
        // exact: written with a circle's `cos`/`sin` points shifted by `3.0`,
        // `(p + 3) - (p0 + 3)` differs from `p - p0` in the low bits and all
        // three outlines hash differently — this test read "3 shapes, want 1"
        // before the coordinates were made exact. That is a real limit on
        // interning as long as producers *bake* translations into points; the
        // resolution is fm-7if's transform channel, where a placement stops
        // being part of the geometry. Recorded on that bead.
        let mut stage = fmn_mobject::Stage::new();
        let square: Vec<Vec3> = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [2.0, 1.0, 0.0],
            [2.0, 2.0, 0.0],
            [1.0, 2.0, 0.0],
            [0.0, 2.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0],
        ];
        let mut mobs = Vec::new();
        for k in 0..3 {
            let shifted: Vec<Vec3> = square
                .iter()
                .map(|p| [p[0] + 4.0 * f64::from(k), p[1], p[2]])
                .collect();
            let mob = stage.add(fmn_mobject::Mobject::from_points(&shifted));
            stage.add_to_scene(mob).expect("live");
            mobs.push(mob);
        }
        let mut plan = RenderPlan::new();
        plan.sync(&stage, 0);
        assert_eq!(plan.shapes().shapes().len(), 1, "one interned outline");

        let table = MonoTable::build(&plan, unit());
        assert_eq!(table.len(), 1);
        assert!(!table.pieces_of(0).is_empty());
        assert!(
            table.pieces_of(7).is_empty(),
            "an unknown shape draws nothing"
        );

        // And the occurrences differ only by a translation, which is the whole
        // reason the table is per shape.
        let insts = plan.shapes().instances();
        assert_eq!(insts.len(), 3);
        let t0 = instance_translation(&insts[0], unit());
        let t1 = instance_translation(&insts[1], unit());
        assert!((t1[0] - t0[0] - 4.0).abs() < 1e-12);
        assert!((t1[1] - t0[1]).abs() < 1e-12);
    }

    // ------------------------------------------------------- hinted fill kernels

    use crate::hint::Hint;
    use crate::table::{compile_shape, shape_digest};

    fn shaped(path: &QuadPath, hint: Hint) -> (crate::table::Shape, Vec<crate::table::Segment>) {
        compile_shape(shape_digest(path.points()), path, hint, 0)
    }

    /// Per-pixel coverage from a kernel over a window.
    fn kernel_grid(k: FillKernel, w: u32, h: u32) -> Vec<f64> {
        let mut out = Vec::with_capacity((w * h) as usize);
        for y in 0..h {
            for x in 0..w {
                out.push(k.coverage(x, y).expect("hinted"));
            }
        }
        out
    }

    /// Per-pixel coverage from the general path over the same window.
    fn general_grid(pieces: &[MonoPiece], w: u32, h: u32) -> Vec<f64> {
        let mut scratch = RowScratch::for_tile(w);
        let mut out = Vec::with_capacity((w * h) as usize);
        for y in 0..h {
            out.extend_from_slice(scratch.fill_row(pieces, [0.0, 0.0], y, 0, w));
        }
        out
    }

    #[test]
    fn the_disc_primitive_integrates_to_pi_r_squared() {
        // The closed form's own oracle, before any pixel is involved: the
        // quadrant primitive at both extremes is the whole disc, and summing it
        // over a pixel grid is the same number a third way.
        for r in [0.5f64, 1.0, 3.7, 20.0] {
            let whole = disc_quadrant_area(r, r, r);
            let want = std::f64::consts::PI * r * r;
            assert!((whole - want).abs() < 1e-12, "r={r}: {whole} vs {want}");

            let c = [r + 1.3, r + 0.7];
            let n = (2.0 * r).ceil() as u32 + 4;
            let mut sum = 0.0;
            for y in 0..n {
                for x in 0..n {
                    sum += disc_box_area(
                        c,
                        r,
                        [
                            f64::from(x),
                            f64::from(y),
                            f64::from(x) + 1.0,
                            f64::from(y) + 1.0,
                        ],
                    );
                }
            }
            // Relative, because this sums up to ~2000 terms and each carries an
            // ulp of `want`; an absolute 1e-12 is a tolerance about r, not about
            // the algorithm.
            assert!(
                (sum - want).abs() < 1e-13 * want,
                "r={r}: grid {sum} vs {want}"
            );
        }
    }

    #[test]
    fn the_rect_kernel_and_the_general_path_are_the_same_pixels() {
        // The exact-equivalence half of hint.rs's rule: dropping this hint may
        // only cost speed, so the two must agree to floating point. Non-integer
        // bounds, so every edge pixel is partially covered.
        let rect = [2.4f64, 3.1, 17.6, 12.9];
        let path = polygon(&[
            [rect[0], rect[1]],
            [rect[2], rect[1]],
            [rect[2], rect[3]],
            [rect[0], rect[3]],
        ]);
        let (shape, segs) = shaped(
            &path,
            Hint::Rect {
                center: [0.5 * (rect[0] + rect[2]), 0.5 * (rect[1] + rect[3]), 0.0],
                width: rect[2] - rect[0],
                height: rect[3] - rect[1],
            },
        );
        let kernel = FillKernel::select(&shape, &segs, unit(), [0.0, 0.0]);
        assert!(matches!(kernel, FillKernel::Rect { .. }));

        let a = kernel_grid(kernel, 24, 16);
        let b = general_grid(&pieces_of_path(&path, unit()), 24, 16);
        let worst = a
            .iter()
            .zip(&b)
            .map(|(u, v)| (u - v).abs())
            .fold(0.0f64, f64::max);
        assert!(worst < 1e-12, "rect hint vs general path: {worst}");
    }

    #[test]
    fn a_dot_takes_the_radial_kernel_and_a_large_circle_does_not() {
        // The bounded-error half of the rule, at the Reference's own scale:
        // 135 px per scene unit (G0-2 L-scale). manim's default Dot radius is
        // 0.08 units, and a two-unit Circle is an ordinary object.
        let px_per_unit = 135.0;
        let map = ScreenMap {
            scale: px_per_unit,
            origin: [0.0, 0.0],
        };
        for (radius, admitted) in [(0.08f64, true), (2.0f64, false)] {
            let path = circle_path(0.0, 0.0, radius, 16);
            let (shape, segs) = shaped(
                &path,
                Hint::Circle {
                    center: [0.0, 0.0, 0.0],
                    radius,
                },
            );
            let kernel = FillKernel::select(&shape, &segs, map, [0.0, 0.0]);
            let took = matches!(kernel, FillKernel::Disc { .. });
            assert_eq!(
                took,
                admitted,
                "radius {radius} units = {} px: kernel {kernel:?}",
                radius * px_per_unit
            );
        }
    }

    #[test]
    fn the_declined_disc_hint_is_declined_for_a_measured_reason() {
        // Not "because the radius is big" — because the outline's own worst
        // radial deviation, scaled to pixels, exceeds the budget. The predicted
        // deviation is r·(π/n)⁴/8; this checks the measurement against it, so a
        // regression in either the formula or the measurement shows up here.
        let r_units = 2.0;
        let scale = 135.0;
        for n in [4usize, 8, 16, 64] {
            let path = circle_path(0.0, 0.0, r_units, n);
            let (_, segs) = shaped(
                &path,
                Hint::Circle {
                    center: [0.0, 0.0, 0.0],
                    radius: r_units,
                },
            );
            let mut worst = 0.0f64;
            for s in &segs {
                let m = [
                    0.25 * (s.p0[0] + 2.0 * s.p1[0] + s.p2[0]),
                    0.25 * (s.p0[1] + 2.0 * s.p1[1] + s.p2[1]),
                ];
                worst = worst.max(((m[0] * m[0] + m[1] * m[1]).sqrt() - r_units).abs());
            }
            // The *exact* deviation, not its leading term. A quadratic through
            // two points of a circle with the tangent-intersection handle has its
            // midpoint at radius `r(cos u + sec u)/2` with `u = θ/2 = π/n`, so
            // the deviation is `r((cos u + sec u)/2 − 1)`. The `r u⁴/8` series
            // quoted elsewhere in this module is that expanded, and it is 22 %
            // low at n = 4 — which is how this assertion first failed.
            let u = std::f64::consts::PI / n as f64;
            let predicted = r_units * ((u.cos() + 1.0 / u.cos()) / 2.0 - 1.0);
            assert!(
                (worst - predicted).abs() < 1e-9 * r_units,
                "n={n}: measured {worst} vs predicted {predicted}"
            );
            let admitted = FillKernel::disc_within_budget([0.0, 0.0, 0.0], r_units, &segs, scale);
            assert_eq!(admitted, worst * scale <= HINT_BUDGET_PX, "n={n}");
        }
    }

    #[test]
    fn the_admitted_disc_kernel_stays_inside_its_own_bound() {
        // The bound is a promise about pixels, so it gets checked on pixels: the
        // hinted disc and the general path must not differ by more than the
        // outline's radial deviation, since that is how far the edge moved.
        let r = 10.8; // manim's default Dot at 135 px/unit
        let path = circle_path(16.4, 15.6, r, 16);
        let pieces = pieces_of_path(&path, unit());
        let kernel = FillKernel::Disc {
            center: [16.4, 15.6],
            radius: r,
        };
        let a = kernel_grid(kernel, 34, 32);
        let b = general_grid(&pieces, 34, 32);
        let worst = a
            .iter()
            .zip(&b)
            .map(|(u, v)| (u - v).abs())
            .fold(0.0f64, f64::max);
        let deviation = r * (std::f64::consts::PI / 16.0f64).powi(4) / 8.0;
        assert!(
            worst <= 3.0 * deviation,
            "disc kernel vs general path: {worst}, deviation {deviation}"
        );
        // And the two agree on total area to the enclosed-area difference, which
        // is the geometry's, not the kernel's.
        let sum_a: f64 = a.iter().sum();
        let sum_b: f64 = b.iter().sum();
        let circle = std::f64::consts::PI * r * r;
        assert!(
            (sum_a - circle).abs() < 1e-9,
            "the kernel is a true disc: {sum_a}"
        );
        assert!(sum_b > sum_a, "the quadratic outline encloses more");
    }

    #[test]
    fn a_rounded_rect_degenerates_to_its_two_limits() {
        // Radius 0 is the rectangle; radius = half the shorter side is the
        // stadium, and for a square it is the disc. Both limits are exact, which
        // is what says the corner decomposition is right rather than plausible.
        // Dyadic bounds, so the side is *exactly* 12 and the four corner
        // centres coincide bit-for-bit. With 3.2/15.2 the width comes out
        // 12 - 8.9e-16, `radius = 6` is a hair over half the side, and the
        // corner squares overlap — a 5e-15 artefact that would sit in this
        // test's residual pretending to be an algorithm error.
        let rect = [3.25f64, 4.75, 15.25, 16.75]; // a 12x12 square
        for (x, y) in [(0u32, 0u32), (4, 6), (9, 11), (15, 16)] {
            let b = [
                f64::from(x),
                f64::from(y),
                f64::from(x) + 1.0,
                f64::from(y) + 1.0,
            ];
            assert!(
                (rounded_rect_box_area(rect, 0.0, b) - box_overlap(rect, b)).abs() < 1e-12,
                "radius 0 is the rectangle at ({x},{y})"
            );
            let disc = disc_box_area([9.25, 10.75], 6.0, b);
            assert!(
                (rounded_rect_box_area(rect, 6.0, b) - disc).abs() < 1e-12,
                "a fully rounded square is the disc at ({x},{y})"
            );
        }
        // And the total area interpolates between them, monotonically.
        let total = |r: f64| {
            let mut s = 0.0;
            for y in 0..22 {
                for x in 0..20 {
                    s += rounded_rect_box_area(
                        rect,
                        r,
                        [
                            f64::from(x),
                            f64::from(y),
                            f64::from(x) + 1.0,
                            f64::from(y) + 1.0,
                        ],
                    );
                }
            }
            s
        };
        let (a, b, c) = (total(0.0), total(3.0), total(6.0));
        assert!((a - 144.0).abs() < 1e-9, "{a}");
        assert!((c - std::f64::consts::PI * 36.0).abs() < 1e-9, "{c}");
        assert!(a > b && b > c, "{a} > {b} > {c}");
    }

    #[test]
    fn the_row_form_and_the_pixel_form_of_a_kernel_agree() {
        let kernels = [
            FillKernel::Rect {
                rect: [2.4, 3.1, 17.6, 12.9],
            },
            FillKernel::Disc {
                center: [8.3, 7.1],
                radius: 5.5,
            },
            FillKernel::RoundedRect {
                rect: [1.5, 2.5, 15.5, 12.5],
                radius: 3.25,
            },
        ];
        for k in kernels {
            let mut row = vec![0.0; 20];
            for y in 0..16 {
                assert!(k.row(y, 0, 20, &mut row));
                for (i, v) in row.iter().enumerate() {
                    assert_eq!(*v, k.coverage(i as u32, y).unwrap(), "{k:?} at ({i},{y})");
                }
            }
        }
        let mut row = vec![0.0; 4];
        assert!(
            !FillKernel::General.row(0, 0, 4, &mut row),
            "the general kernel declines, it does not fill zeros"
        );
        assert_eq!(FillKernel::General.coverage(0, 0), None);
    }

    #[test]
    fn an_arc_and_a_line_route_to_the_general_path() {
        // Arc: filling it closes it with a chord, so the region is a circular
        // segment and a disc kernel would be the wrong shape. Line/Polyline: the
        // general path is already exact and already fast for straight segments —
        // there is nothing a kernel would save.
        let path = circle_path(0.0, 0.0, 1.0, 8);
        for hint in [
            Hint::Arc {
                center: [0.0, 0.0, 0.0],
                radius: 1.0,
                start_angle: 0.0,
                angle: std::f64::consts::PI,
            },
            Hint::Line,
            Hint::Polyline { closed: true },
            Hint::General,
        ] {
            let (shape, segs) = shaped(&path, hint);
            assert_eq!(
                FillKernel::select(&shape, &segs, unit(), [0.0, 0.0]),
                FillKernel::General,
                "{hint:?}"
            );
        }
    }

    #[test]
    fn a_kernel_is_placed_by_its_occurrence_not_by_its_outline() {
        // The instancing rule, at the hinted path: two occurrences of one
        // interned disc must land where their offsets say, not where the first
        // one's points happened to be.
        let path = circle_path(0.0, 0.0, 0.05, 16);
        let (shape, segs) = shaped(
            &path,
            Hint::Dot {
                center: [0.0, 0.0, 0.0],
                radius: 0.05,
            },
        );
        let map = ScreenMap {
            scale: 135.0,
            origin: [10.0, 20.0],
        };
        let a = FillKernel::select(&shape, &segs, map, [0.0, 0.0]);
        let b = FillKernel::select(&shape, &segs, map, [270.0, -135.0]);
        match (a, b) {
            (
                FillKernel::Disc {
                    center: ca,
                    radius: ra,
                },
                FillKernel::Disc {
                    center: cb,
                    radius: rb,
                },
            ) => {
                assert!((ra - rb).abs() < 1e-12, "the radius is the outline's");
                assert!((cb[0] - ca[0] - 270.0).abs() < 1e-12);
                assert!((cb[1] - ca[1] + 135.0).abs() < 1e-12);
                assert!((ca[0] - 10.0).abs() < 1e-12 && (ca[1] - 20.0).abs() < 1e-12);
            }
            other => panic!("both should be discs: {other:?}"),
        }
    }
}
