//! Quadratic-Bézier primitives: evaluation, restriction to a subinterval,
//! and the unit-circle arc construction — ports of `manimlib/utils/bezier.py`
//! (`3b1b/manim` @ `6199a00d`), computed in f64 per §6.1.

use crate::scalar;
#[cfg(test)]
use crate::space_ops;
use crate::vec;
use fmn_core::constants::TAU;
use fmn_core::types::Vec3;

/// Evaluate the quadratic Bézier `(p0, p1, p2)` at `t`, using the Reference's
/// exact expression `p0(1-t)² + 2 p1 t(1-t) + p2 t²`.
#[must_use]
pub fn quadratic_point(p0: Vec3, p1: Vec3, p2: Vec3, t: f64) -> Vec3 {
    let u = 1.0 - t;
    let mut out = [0.0; 3];
    for i in 0..3 {
        out[i] = p0[i] * u * u + 2.0 * p1[i] * t * u + p2[i] * t * t;
    }
    out
}

/// Evaluate the cubic Bézier `(a0, h0, h1, a1)` at `t` (Bernstein form).
#[must_use]
pub fn cubic_point(a0: Vec3, h0: Vec3, h1: Vec3, a1: Vec3, t: f64) -> Vec3 {
    let u = 1.0 - t;
    let mut out = [0.0; 3];
    for i in 0..3 {
        out[i] = a0[i] * u * u * u
            + 3.0 * h0[i] * t * u * u
            + 3.0 * h1[i] * t * t * u
            + a1[i] * t * t * t;
    }
    out
}

/// `partial_quadratic_bezier_points`: control points for the restriction of a
/// quadratic to the parameter interval `[a, b]`, matching the Reference's
/// formulas (including the `a == 1` triple-endpoint special case).
#[must_use]
pub fn partial_quadratic(points: &[Vec3; 3], a: f64, b: f64) -> [Vec3; 3] {
    if a == 1.0 {
        return [points[2], points[2], points[2]];
    }
    let curve = |t: f64| quadratic_point(points[0], points[1], points[2], t);
    let h0 = if a > 0.0 { curve(a) } else { points[0] };
    let h2 = if b < 1.0 { curve(b) } else { points[2] };
    let h1_prime = vec::lerp(points[1], points[2], a);
    let end_prop = (b - a) / (1.0 - a);
    let h1 = vec::lerp(h0, h1_prime, end_prop);
    [h0, h1, h2]
}

/// `np.linspace(a, b, n)` with the endpoint included, matching numpy's
/// `start + i * step` evaluation.
#[must_use]
pub(crate) fn linspace(a: f64, b: f64, n: usize) -> Vec<f64> {
    if n == 0 {
        return Vec::new();
    }
    if n == 1 {
        return vec![a];
    }
    let step = (b - a) / (n as f64 - 1.0);
    let mut out: Vec<f64> = (0..n).map(|i| a + i as f64 * step).collect();
    // numpy pins the endpoint exactly.
    out[n - 1] = b;
    out
}

/// `quadratic_bezier_points_for_arc`: `2n+1` shared-anchor points tracing the
/// unit-circle arc from angle 0 to `angle`, handles pushed out by
/// `1/cos(θ/2)` so each quadratic component interpolates its arc chord.
#[must_use]
pub fn quadratic_points_for_arc(angle: f64, n_components: usize) -> Vec<Vec3> {
    let n_points = 2 * n_components + 1;
    let angles = linspace(0.0, angle, n_points);
    let mut points: Vec<Vec3> = angles
        .iter()
        .map(|&a| [scalar::cos(a), scalar::sin(a), 0.0])
        .collect();
    let theta = angle / n_components as f64;
    let handle_scale = 1.0 / scalar::cos(theta / 2.0);
    for point in points.iter_mut().skip(1).step_by(2) {
        *point = vec::scale(*point, handle_scale);
    }
    points
}

/// The one arc-density rule (Behavior Note BN-09): the number of quadratic
/// components used to trace an arc of the given subtended angle.
///
/// The Reference ships three inconsistent conventions
/// (`int(15·|θ|/TAU) + 1`, `ceil(8·|θ|/TAU)`, and a fixed 8); FrankenManim
/// uses `max(1, ceil(16·|θ|/TAU))` everywhere — 16 components for a full
/// circle, matching the Reference's `Arc`/`Circle` quality (its finest
/// convention) at every common angle and never coarser than it.
#[must_use]
pub fn arc_n_components(angle: f64) -> usize {
    let n = (16.0 * angle.abs() / TAU).ceil() as usize;
    n.max(1)
}

/// `integer_interpolate`: an integer in `[start, end]` plus the residue
/// toward the next one, with the Reference's exact clamping.
#[must_use]
pub fn integer_interpolate(start: i64, end: i64, alpha: f64) -> (i64, f64) {
    if alpha >= 1.0 {
        return (end - 1, 1.0);
    }
    if alpha <= 0.0 {
        return (start, 0.0);
    }
    let interpolated = (1.0 - alpha) * start as f64 + alpha * end as f64;
    let value = interpolated as i64;
    let residue = ((end - start) as f64 * alpha).rem_euclid(1.0);
    (value, residue)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmn_core::constants::PI;

    #[test]
    fn partial_quadratic_endpoints_land_on_curve() {
        let pts = [[0.0, 0.0, 0.0], [1.0, 2.0, 0.0], [2.0, 0.0, 0.0]];
        let sub = partial_quadratic(&pts, 0.25, 0.75);
        let expect_start = quadratic_point(pts[0], pts[1], pts[2], 0.25);
        let expect_end = quadratic_point(pts[0], pts[1], pts[2], 0.75);
        for i in 0..3 {
            assert!((sub[0][i] - expect_start[i]).abs() < 1e-15);
            assert!((sub[2][i] - expect_end[i]).abs() < 1e-15);
        }
        // The restriction agrees with the original at its own midpoint.
        let mid_sub = quadratic_point(sub[0], sub[1], sub[2], 0.5);
        let mid_full = quadratic_point(pts[0], pts[1], pts[2], 0.5);
        for i in 0..3 {
            assert!((mid_sub[i] - mid_full[i]).abs() < 1e-12);
        }
    }

    #[test]
    fn partial_quadratic_a_equals_one() {
        let pts = [[0.0, 0.0, 0.0], [1.0, 1.0, 0.0], [2.0, 0.0, 0.0]];
        assert_eq!(partial_quadratic(&pts, 1.0, 1.0), [pts[2], pts[2], pts[2]]);
    }

    #[test]
    fn arc_density_rule() {
        assert_eq!(arc_n_components(TAU), 16);
        assert_eq!(arc_n_components(PI), 8);
        assert_eq!(arc_n_components(TAU / 4.0), 4);
        assert_eq!(arc_n_components(-TAU / 4.0), 4);
        assert_eq!(arc_n_components(1e-9), 1);
        assert_eq!(arc_n_components(0.0), 1);
    }

    #[test]
    fn arc_points_interpolate_unit_circle() {
        let pts = quadratic_points_for_arc(PI / 2.0, 2);
        assert_eq!(pts.len(), 5);
        // Anchors sit on the unit circle.
        for p in [pts[0], pts[2], pts[4]] {
            assert!((space_ops::get_norm(p) - 1.0).abs() < 1e-12);
        }
        assert!((pts[4][0]).abs() < 1e-12 && (pts[4][1] - 1.0).abs() < 1e-12);
    }

    #[test]
    fn integer_interpolate_matches_reference_docstring() {
        let (value, residue) = integer_interpolate(0, 10, 0.46);
        assert_eq!(value, 4);
        assert!((residue - 0.6).abs() < 1e-12);
        assert_eq!(integer_interpolate(0, 10, 1.5), (9, 1.0));
        assert_eq!(integer_interpolate(0, 10, -0.5), (0, 0.0));
    }
}
