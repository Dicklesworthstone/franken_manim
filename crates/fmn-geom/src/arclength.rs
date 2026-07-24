//! The proportion/length layer, correct under the original names
//! (§7.3, BN-03, fm-xci).
//!
//! The Reference's layer is chord-distance heuristics — three mutually
//! inconsistent approximations. We keep the names and fix the math
//! (D-05): `get_arc_length` returns arc length; `point_from_proportion`
//! is constant-speed via the inverse-arclength table; the equal-curve-
//! length `quick_point_from_proportion` survives as the labeled fast
//! approximation, never a silent substitute.
//!
//! **Exactness note.** The plan sketches adaptive Gauss–Legendre (via
//! fsci quad); a quadratic Bézier's speed `|B'(t)|` is the norm of a
//! *linear* function, so its arc length has a **closed form** — exact to
//! rounding, deterministic (the logarithm rides fmn-dmath), cheaper than
//! any quadrature. Degenerate configurations (straight lines, interior
//! cusps where the derivative crosses zero) get exact branches.
//!
//! **Retained-cache contract (§10.8).** [`ArcLengthTable`] is one of the
//! retained per-path artifacts: keyed by the *geometry* revision only
//! (the RecordBuffer's `point`-field revision), never by transform or
//! style revisions. [`CachedArcLength`] implements exactly that keying;
//! Lumen's compiled paths hold one per path.

use crate::bezier;
use crate::quadpath::QuadPath;
use crate::scalar;
use crate::space_ops;
use crate::vec;
use fmn_core::types::Vec3;

/// The derivative of the quadratic `(a0, h, a1)` at `t`:
/// `B'(t) = 2[(1−t)(h−a0) + t(a1−h)]`.
fn derivative(a0: Vec3, h: Vec3, a1: Vec3, t: f64) -> Vec3 {
    [
        2.0 * ((1.0 - t) * (h[0] - a0[0]) + t * (a1[0] - h[0])),
        2.0 * ((1.0 - t) * (h[1] - a0[1]) + t * (a1[1] - h[1])),
        2.0 * ((1.0 - t) * (h[2] - a0[2]) + t * (a1[2] - h[2])),
    ]
}

/// Arc length of one quadratic Bézier `(a0, h, a1)` — closed form.
///
/// With `B'(t) = v + t·u` (`v = 2(h−a0)`, `u = 2(a0 − 2h + a1)`), the
/// speed is `√(a t² + b t + c)` for `a = u·u`, `b = 2u·v`, `c = v·v`.
/// Branches:
/// - `a = 0` — constant speed (straight line or point): `√c`;
/// - `4ac − b² ≈ 0` — `u ∥ v`: speed is `√a·|t − t*|`, `t* = −b/(2a)`
///   (an interior cusp when `t* ∈ (0,1)`): exact piecewise-linear
///   integral;
/// - otherwise the standard antiderivative
///   `F(t) = (2at+b)S(t)/(4a) + (4ac−b²)/(8a^{3/2})·ln(2√a·S(t)+2at+b)`.
#[must_use]
pub fn quadratic_arc_length(a0: Vec3, h: Vec3, a1: Vec3) -> f64 {
    let v = vec::scale(vec::sub(h, a0), 2.0);
    let u = vec::scale(vec::add(vec::sub(a0, vec::scale(h, 2.0)), a1), 2.0);
    let a = space_ops::dot(u, u);
    let b = 2.0 * space_ops::dot(u, v);
    let c = space_ops::dot(v, v);

    // Near-straight curves: the general antiderivative below divides by
    // `a`, so as the quadratic term vanishes its two terms grow like
    // `|b|·S/(4a)` and cancel to the answer — at `a/c ≈ 1e-33` (a line
    // whose handle is the midpoint to within rounding, which is what
    // scaling a short segment up produces) that cancellation loses every
    // significant digit and returns zero. The test is therefore relative,
    // not `a == 0.0`: once the quadratic term cannot matter over `[0, 1]`,
    // integrate the linear speed exactly instead.
    if a <= (c + b.abs()) * 1e-12 {
        // ∫₀¹ √(c + bt) dt, written so nothing cancels: the naive
        // antiderivative `2/(3b)·[(c+b)^{3/2} − c^{3/2}]` subtracts two
        // nearly equal cubes and divides by the small `b` that made them
        // nearly equal. Factoring `s₁³ − s₀³ = (s₁ − s₀)(s₁² + s₁s₀ + s₀²)`
        // and `s₁ − s₀ = b/(s₁ + s₀)` cancels the `b` symbolically, and
        // the remaining expression is the constant-speed answer `√c` at
        // `b = 0` with no special case.
        let s0 = c.max(0.0).sqrt();
        let s1 = (c + b).max(0.0).sqrt();
        if s0 + s1 == 0.0 {
            return 0.0;
        }
        return 2.0 * (s1 * s1 + s1 * s0 + s0 * s0) / (3.0 * (s1 + s0));
    }

    let disc = 4.0 * a * c - b * b;
    if disc <= a * c * 1e-24 {
        // u ∥ v: speed = √a·|t − t*|.
        let t_star = -b / (2.0 * a);
        let integral = if t_star <= 0.0 {
            0.5 - t_star
        } else if t_star >= 1.0 {
            t_star - 0.5
        } else {
            0.5 * (t_star * t_star + (1.0 - t_star) * (1.0 - t_star))
        };
        return a.sqrt() * integral;
    }

    let sqrt_a = a.sqrt();
    let speed_at = |t: f64| -> f64 { (a * t * t + b * t + c).sqrt() };
    let antiderivative = |t: f64| -> f64 {
        (2.0 * a * t + b) * speed_at(t) / (4.0 * a)
            + disc / (8.0 * a * sqrt_a) * scalar::ln(2.0 * sqrt_a * speed_at(t) + 2.0 * a * t + b)
    };
    antiderivative(1.0) - antiderivative(0.0)
}

/// Per-curve and cumulative true lengths of a path — the retained
/// arc-length artifact (§10.8). Immutable once built; see
/// [`CachedArcLength`] for the revision-keyed holder.
#[derive(Debug, Clone, PartialEq)]
pub struct ArcLengthTable {
    curve_lengths: Vec<f64>,
    cumulative: Vec<f64>,
}

impl ArcLengthTable {
    /// Build for a path (exact per-curve closed forms; O(curves)).
    #[must_use]
    pub fn for_path(path: &QuadPath) -> Self {
        let curve_lengths: Vec<f64> = path
            .bezier_tuples()
            .map(|[a0, h, a1]| quadratic_arc_length(a0, h, a1))
            .collect();
        let mut cumulative = Vec::with_capacity(curve_lengths.len() + 1);
        let mut total = 0.0;
        cumulative.push(0.0);
        for &len in &curve_lengths {
            total += len;
            cumulative.push(total);
        }
        Self {
            curve_lengths,
            cumulative,
        }
    }

    /// The path's total arc length.
    #[must_use]
    pub fn total(&self) -> f64 {
        *self.cumulative.last().unwrap_or(&0.0)
    }

    /// Per-curve true lengths, in curve order.
    #[must_use]
    pub fn curve_lengths(&self) -> &[f64] {
        &self.curve_lengths
    }

    /// Invert: the `(curve index, local t)` where accumulated arc length
    /// reaches `alpha · total`. Newton on the exact partial-length closed
    /// form with a bisection safeguard, fixed iteration count
    /// (deterministic on every platform).
    #[must_use]
    pub fn curve_and_t_at(&self, path: &QuadPath, alpha: f64) -> Option<(usize, f64)> {
        if self.curve_lengths.is_empty() {
            return None;
        }
        let alpha = alpha.clamp(0.0, 1.0);
        let target = alpha * self.total();
        let mut index = self
            .cumulative
            .partition_point(|&len| len <= target)
            .saturating_sub(1);
        index = index.min(self.curve_lengths.len() - 1);
        // Zero-length curves cannot host an interior parameter.
        while index + 1 < self.curve_lengths.len() && self.curve_lengths[index] == 0.0 {
            index += 1;
        }
        let curve_len = self.curve_lengths[index];
        if curve_len == 0.0 {
            return Some((index, 0.0));
        }
        let local_target = target - self.cumulative[index];
        let [a0, h, a1] = path.nth_curve_points(index)?;
        let partial = |t: f64| -> f64 {
            if t <= 0.0 {
                return 0.0;
            }
            let sub = bezier::partial_quadratic(&[a0, h, a1], 0.0, t.min(1.0));
            quadratic_arc_length(sub[0], sub[1], sub[2])
        };
        let (mut lo, mut hi) = (0.0f64, 1.0f64);
        let mut t = (local_target / curve_len).clamp(0.0, 1.0);
        for _ in 0..24 {
            let err = partial(t) - local_target;
            if err.abs() <= 1e-14 * curve_len {
                break; // converged (a spot-on initial guess must not bisect away)
            }
            if err > 0.0 {
                hi = t;
            } else {
                lo = t;
            }
            let dsdt = space_ops::get_norm(derivative(a0, h, a1, t));
            let newton = if dsdt > 0.0 { t - err / dsdt } else { f64::NAN };
            t = if newton.is_finite() && newton > lo && newton < hi {
                newton
            } else {
                0.5 * (lo + hi)
            };
        }
        Some((index, t))
    }
}

/// The revision-keyed holder for the retained table: rebuilt only when
/// the **geometry** revision moves (the RecordBuffer's `point`-field
/// revision), never for transform or style changes (§10.8).
#[derive(Debug, Default)]
pub struct CachedArcLength {
    entry: Option<(u64, ArcLengthTable)>,
    rebuilds: u64,
}

impl CachedArcLength {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of rebuilds so far — the invalidation tests' observable.
    #[must_use]
    pub fn rebuilds(&self) -> u64 {
        self.rebuilds
    }

    /// The table for `path` at `geometry_revision`, rebuilding only when
    /// the revision moved.
    pub fn get(&mut self, geometry_revision: u64, path: &QuadPath) -> &ArcLengthTable {
        let stale = match &self.entry {
            Some((revision, _)) => *revision != geometry_revision,
            None => true,
        };
        if stale {
            self.entry = Some((geometry_revision, ArcLengthTable::for_path(path)));
            self.rebuilds += 1;
        }
        &self.entry.as_ref().expect("just ensured").1
    }
}

impl QuadPath {
    /// `get_arc_length` — the actual arc length (BN-03), exact per-curve
    /// closed forms. (The Reference returned a chord/handle-polygon blend.)
    #[must_use]
    pub fn get_arc_length(&self) -> f64 {
        ArcLengthTable::for_path(self).total()
    }

    /// `point_from_proportion` — constant-speed: the point `alpha` of the
    /// way along the path *by true arc length* (BN-03). Builds a fresh
    /// table; hot paths hold a [`CachedArcLength`] and use
    /// [`QuadPath::point_from_proportion_with`].
    #[must_use]
    pub fn point_from_proportion(&self, alpha: f64) -> Option<Vec3> {
        if !self.has_points() {
            return None;
        }
        self.point_from_proportion_with(&ArcLengthTable::for_path(self), alpha)
    }

    /// [`QuadPath::point_from_proportion`] against a retained table.
    #[must_use]
    pub fn point_from_proportion_with(&self, table: &ArcLengthTable, alpha: f64) -> Option<Vec3> {
        if !self.has_points() {
            return None;
        }
        if self.num_curves() == 0 {
            return Some(self.points()[0]);
        }
        let (index, t) = table.curve_and_t_at(self, alpha)?;
        self.nth_curve_point(index, t)
    }

    /// `quick_point_from_proportion` — the Reference's equal-curve-length
    /// approximation, kept verbatim as the labeled fast path (never a
    /// silent substitute for the true math above).
    #[must_use]
    pub fn quick_point_from_proportion(&self, alpha: f64) -> Option<Vec3> {
        let num_curves = self.num_curves();
        if num_curves == 0 {
            return if self.has_points() {
                Some(self.points()[0])
            } else {
                None
            };
        }
        let (n, residue) = bezier::integer_interpolate(0, num_curves as i64, alpha);
        self.nth_curve_point(n.max(0) as usize, residue)
    }
}
