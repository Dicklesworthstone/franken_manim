//! Nearest-point and distance queries against the quadratic path model (§7.1).
//!
//! ## Why this lives in Chisel and not in a renderer
//!
//! Two consumers need the same answer. §10.3's strokes are *defined* by distance
//! to the curve — "exact/high-accuracy signed distance to the quadratic within
//! conservative slabs" — and §10.2's `fill_border_width` needs the nearest
//! boundary point to know how far inside the boundary a pixel is and what the
//! boundary ramp says there. D4 puts the geometry kernel here, so the primitive
//! lives here once and both read it; the alternative is two root-finds that agree
//! until one of them is edited.
//!
//! ## The mathematics, and the one place it is delicate
//!
//! With `B(t) = A + Bt + Ct²` (`A = a0`, `B = 2(h − a0)`, `C = a0 − 2h + a1`) and
//! `D = A − p`, the squared distance is a quartic in `t` and its derivative is a
//! **cubic**:
//!
//! ```text
//! ½ f'(t) = D·B + (B·B + 2 D·C) t + 3 (B·C) t² + 2 (C·C) t³
//! ```
//!
//! so the nearest point is one of the cubic's real roots in `[0, 1]` or an
//! endpoint. The delicate part is the cubic solve, not the setup: a straight
//! segment (`C = 0`) degenerates to a linear equation, a cusp-adjacent curve puts
//! two roots within an ulp of each other, and the trigonometric branch's
//! `acos` argument leaves `[−1, 1]` by rounding exactly when the discriminant is
//! near zero. Each of those is handled where it arises rather than by widening a
//! tolerance until the tests pass — §6.1's precision-exception posture.
//!
//! Every transcendental routes through `crate::scalar` to fmn-dmath (§6.6,
//! D-17), because a stroke's silhouette is part of the certified image and
//! `f64::cbrt` defers to the platform's libm.

use crate::scalar;
use crate::space_ops;
use crate::vec;
use fmn_core::types::Vec3;

/// The nearest point on a curve to a query point.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Nearest {
    /// The curve parameter, in `[0, 1]`.
    pub t: f64,
    /// The point itself.
    pub point: Vec3,
    /// Its distance from the query point.
    pub distance: f64,
}

/// Real roots of `a3 t³ + a2 t² + a1 t + a0`, ascending, degenerate cases
/// included.
///
/// Falls through to the quadratic and then the linear case on a *relative* test
/// against the polynomial's own scale — an absolute epsilon would call a
/// steeply-scaled cubic degenerate and a finely-scaled one well conditioned, and
/// the two arise from the same curve viewed at two zoom levels.
///
/// Roots are polished with a fixed three Newton steps. Fixed, not
/// convergence-tested, because the count is part of the answer's identity: a
/// loop that stops when it has converged stops at a different iteration on a
/// different platform, and §6.6 exists to keep that from happening.
#[must_use]
pub fn solve_cubic_real(a3: f64, a2: f64, a1: f64, a0: f64, out: &mut [f64; 3]) -> usize {
    let scale = a3.abs().max(a2.abs()).max(a1.abs()).max(a0.abs());
    if scale == 0.0 {
        return 0;
    }
    if a3.abs() <= 1e-14 * scale {
        return solve_quadratic_real(a2, a1, a0, out);
    }

    // Monic, then depressed: x = t + p/3 turns t³ + pt² + qt + r into x³ + Px + Q.
    let p = a2 / a3;
    let q = a1 / a3;
    let r = a0 / a3;
    let shift = p / 3.0;
    let big_p = q - p * p / 3.0;
    let big_q = 2.0 * p * p * p / 27.0 - p * q / 3.0 + r;

    let n = if big_p < 0.0 {
        // Three real roots are possible: the trigonometric branch. `radius` is
        // `√(−P³/27)`, and `−Q/(2 radius)` is a cosine that rounding can push a
        // hair outside `[−1, 1]` precisely when the discriminant is near zero —
        // so it is clamped, which turns a `NaN` into the triple root it is.
        let radius = (-big_p * big_p * big_p / 27.0).sqrt();
        if radius == 0.0 {
            out[0] = -shift;
            1
        } else {
            let cos_phi = (-big_q / (2.0 * radius)).clamp(-1.0, 1.0);
            let phi = scalar::acos(cos_phi);
            let amp = 2.0 * (-big_p / 3.0).sqrt();
            for (k, slot) in out.iter_mut().enumerate() {
                let angle = (phi + core::f64::consts::TAU * k as f64) / 3.0;
                *slot = amp * scalar::cos(angle) - shift;
            }
            3
        }
    } else {
        // One real root: Cardano. `P ≥ 0` makes the discriminant non-negative, so
        // there is nothing to clamp and no complex arithmetic to carry.
        let disc = (big_q / 2.0) * (big_q / 2.0) + fmn_dmath::powi(big_p / 3.0, 3);
        let s = disc.max(0.0).sqrt();
        out[0] = scalar::cbrt(-big_q / 2.0 + s) + scalar::cbrt(-big_q / 2.0 - s) - shift;
        1
    };

    // Polish on the original coefficients, not the depressed ones: the shift and
    // the division by `a3` both moved the roots, and Newton on what was actually
    // asked recovers what those steps cost.
    for root in out.iter_mut().take(n) {
        *root = polish_cubic_root(a3, a2, a1, a0, *root);
    }
    sort_prefix(out, n);
    n
}

/// A fixed three Newton steps on `a3 t³ + a2 t² + a1 t + a0` from `root`.
///
/// Fixed, not convergence-tested, for the reason [`solve_cubic_real`] gives.
fn polish_cubic_root(a3: f64, a2: f64, a1: f64, a0: f64, mut root: f64) -> f64 {
    for _ in 0..3 {
        let f = ((a3 * root + a2) * root + a1) * root + a0;
        let df = (3.0 * a3 * root + 2.0 * a2) * root + a1;
        if df == 0.0 {
            break;
        }
        let step = f / df;
        if !step.is_finite() {
            break;
        }
        root -= step;
    }
    root
}

/// Real roots of `a2 t² + a1 t + a0`, ascending.
fn solve_quadratic_real(a2: f64, a1: f64, a0: f64, out: &mut [f64; 3]) -> usize {
    let scale = a2.abs().max(a1.abs()).max(a0.abs());
    if scale == 0.0 {
        return 0;
    }
    if a2.abs() <= 1e-14 * scale {
        if a1.abs() <= 1e-14 * scale {
            return 0;
        }
        out[0] = -a0 / a1;
        return 1;
    }
    let disc = a1 * a1 - 4.0 * a2 * a0;
    if disc < 0.0 {
        return 0;
    }
    let s = disc.sqrt();
    // The stable pairing, so the small root is not a difference of near-equals.
    let sign = if a1 >= 0.0 { 1.0 } else { -1.0 };
    let big = -0.5 * (a1 + sign * s);
    if big == 0.0 {
        out[0] = 0.0;
        return 1;
    }
    out[0] = big / a2;
    out[1] = a0 / big;
    sort_prefix(out, 2);
    2
}

/// Insertion sort of the first `n` entries — `n ≤ 3`, so this is the whole
/// algorithm and not a placeholder for one.
fn sort_prefix(out: &mut [f64; 3], n: usize) {
    for i in 1..n {
        let mut j = i;
        while j > 0 && out[j - 1] > out[j] {
            out.swap(j - 1, j);
            j -= 1;
        }
    }
}

/// The nearest point on one quadratic Bézier to `p`.
///
/// Exact up to the cubic solve: the candidates are the stationary points of the
/// squared distance plus the two endpoints, and the endpoints are always tested
/// because a curve's nearest point to an outside query is very often one of them
/// and no interior root reports it.
///
/// **Nearly straight segments** (GH #1) are where the closed-form cubic is
/// ill-conditioned: `set_points_as_corners` puts each handle at its chord's
/// midpoint, storage rounds it off the line by an ulp, and `C` becomes tiny but
/// not zero. Then `a3 = 2 C·C` is far below the other coefficients yet above
/// the relative degeneracy test, the monic form divides by it, and the roots in
/// `[0, 1]` come back as cancellation noise — the solve silently reports only
/// the endpoints, and a stroke loses its middle wherever a segment is longer
/// than the stroke is wide. There the in-range stationary points are those of
/// the quadratic `a2 t² + a1 t + a0` perturbed by `a3 t³`, so the deflated
/// quadratic's roots, Newton-polished on the full cubic, are candidates too.
/// Adding candidates cannot make the answer worse: each is judged by its actual
/// squared distance, and ties keep the incumbent.
#[must_use]
pub fn nearest_on_quadratic(a0: Vec3, h: Vec3, a1: Vec3, p: Vec3) -> Nearest {
    let b = vec::scale(vec::sub(h, a0), 2.0);
    let c = vec::add(vec::sub(a0, vec::scale(h, 2.0)), a1);
    let d = vec::sub(a0, p);

    let a3 = 2.0 * space_ops::dot(c, c);
    let a2 = 3.0 * space_ops::dot(b, c);
    let a1c = space_ops::dot(b, b) + 2.0 * space_ops::dot(d, c);
    let a0c = space_ops::dot(d, b);

    let mut roots = [0.0f64; 3];
    let n = solve_cubic_real(a3, a2, a1c, a0c, &mut roots);

    let at = |t: f64| -> Vec3 {
        let t2 = t * t;
        [
            a0[0] + b[0] * t + c[0] * t2,
            a0[1] + b[1] * t + c[1] * t2,
            a0[2] + b[2] * t + c[2] * t2,
        ]
    };
    let mut best_t = 0.0;
    let mut best_d2 = space_ops::dot(d, d);
    let end = vec::sub(at(1.0), p);
    let end_d2 = space_ops::dot(end, end);
    if end_d2 < best_d2 {
        best_t = 1.0;
        best_d2 = end_d2;
    }
    let mut deflated = [0.0f64; 3];
    let m = solve_quadratic_real(a2, a1c, a0c, &mut deflated);
    for t in deflated.iter_mut().take(m) {
        *t = polish_cubic_root(a3, a2, a1c, a0c, *t);
    }
    for &t in roots.iter().take(n).chain(deflated.iter().take(m)) {
        if !(0.0..=1.0).contains(&t) {
            continue;
        }
        let v = vec::sub(at(t), p);
        let d2 = space_ops::dot(v, v);
        if d2 < best_d2 {
            best_t = t;
            best_d2 = d2;
        }
    }
    let point = at(best_t);
    Nearest {
        t: best_t,
        point,
        distance: best_d2.max(0.0).sqrt(),
    }
}

/// Distance from `p` to one quadratic Bézier.
#[must_use]
pub fn distance_to_quadratic(a0: Vec3, h: Vec3, a1: Vec3, p: Vec3) -> f64 {
    nearest_on_quadratic(a0, h, a1, p).distance
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Brute-force nearest point, for the oracle.
    fn brute(a0: Vec3, h: Vec3, a1: Vec3, p: Vec3, n: usize) -> (f64, f64) {
        let mut best = (0.0f64, f64::INFINITY);
        for k in 0..=n {
            let t = k as f64 / n as f64;
            let q = crate::bezier::quadratic_point(a0, h, a1, t);
            let d = space_ops::get_norm(vec::sub(q, p));
            if d < best.1 {
                best = (t, d);
            }
        }
        best
    }

    #[test]
    fn a_cubic_with_three_known_roots_gives_them_back() {
        // (t-1)(t-2)(t-3) = t³ - 6t² + 11t - 6
        let mut out = [0.0f64; 3];
        assert_eq!(solve_cubic_real(1.0, -6.0, 11.0, -6.0, &mut out), 3);
        for (got, want) in out.iter().zip(&[1.0, 2.0, 3.0]) {
            assert!((got - want).abs() < 1e-12, "{got} vs {want}");
        }
    }

    #[test]
    fn a_cubic_with_one_real_root_gives_exactly_one() {
        // t³ + t + 1: monotone, so one real root near -0.6823.
        let mut out = [0.0f64; 3];
        assert_eq!(solve_cubic_real(1.0, 0.0, 1.0, 1.0, &mut out), 1);
        assert!((out[0] + 0.682_327_803_828_019).abs() < 1e-12, "{}", out[0]);
    }

    #[test]
    fn a_triple_root_does_not_become_a_nan() {
        // (t - 2)³ = t³ - 6t² + 12t - 8. The trigonometric branch's cosine is
        // exactly the argument rounding pushes outside [-1, 1] here, which is
        // why it is clamped rather than trusted.
        let mut out = [0.0f64; 3];
        let n = solve_cubic_real(1.0, -6.0, 12.0, -8.0, &mut out);
        assert!(n >= 1);
        for r in out.iter().take(n) {
            assert!(r.is_finite(), "{r}");
            assert!((r - 2.0).abs() < 1e-5, "{r}");
        }
    }

    #[test]
    fn a_degenerate_cubic_falls_through_to_lower_degree() {
        let mut out = [0.0f64; 3];
        // Quadratic: t² - 3t + 2 = (t-1)(t-2).
        assert_eq!(solve_cubic_real(0.0, 1.0, -3.0, 2.0, &mut out), 2);
        assert!((out[0] - 1.0).abs() < 1e-12 && (out[1] - 2.0).abs() < 1e-12);
        // Linear: 2t - 6.
        assert_eq!(solve_cubic_real(0.0, 0.0, 2.0, -6.0, &mut out), 1);
        assert!((out[0] - 3.0).abs() < 1e-12);
        // Nothing at all.
        assert_eq!(solve_cubic_real(0.0, 0.0, 0.0, 5.0, &mut out), 0);
        assert_eq!(solve_cubic_real(0.0, 0.0, 0.0, 0.0, &mut out), 0);
    }

    #[test]
    fn a_scaled_cubic_is_not_called_degenerate() {
        // The same roots at two scales: a relative degeneracy test must find
        // three roots both times. An absolute epsilon would call the small one
        // degenerate, which is the same curve seen at a different zoom.
        let mut out = [0.0f64; 3];
        for k in [1e-8f64, 1.0, 1e8] {
            let n = solve_cubic_real(k, -6.0 * k, 11.0 * k, -6.0 * k, &mut out);
            assert_eq!(n, 3, "scale {k}");
            for (got, want) in out.iter().zip(&[1.0, 2.0, 3.0]) {
                assert!((got - want).abs() < 1e-9, "scale {k}: {got} vs {want}");
            }
        }
    }

    #[test]
    fn the_nearest_point_matches_brute_force_over_a_corpus() {
        // The oracle §10.3's acceptance names, applied to the primitive: a dense
        // sampling of the curve can only do worse, so the closed form must be at
        // least as close everywhere.
        let curves: [[Vec3; 3]; 5] = [
            // A genuine curve.
            [[0.0, 0.0, 0.0], [1.0, 2.0, 0.0], [3.0, 0.0, 0.0]],
            // A straight segment: C = 0, so the cubic degenerates to linear.
            [[0.0, 0.0, 0.0], [1.0, 1.0, 0.0], [2.0, 2.0, 0.0]],
            // A cusp: the handle beyond an endpoint, so two roots collide.
            [[0.0, 0.0, 0.0], [2.0, 0.0, 0.0], [0.0, 0.0, 0.0]],
            // Out of plane.
            [[0.0, 0.0, 0.0], [1.0, 1.0, 1.0], [2.0, 0.0, 2.0]],
            // Nearly straight: the case that makes an absolute epsilon wrong.
            [[0.0, 0.0, 0.0], [1.0, 1e-9, 0.0], [2.0, 0.0, 0.0]],
        ];
        let queries: [Vec3; 9] = [
            [0.0, 0.0, 0.0],
            [1.5, 1.5, 0.0],
            [-1.0, -1.0, 0.0],
            [4.0, 1.0, 0.0],
            [1.0, -3.0, 0.0],
            [1.0, 0.5, 0.0],
            [0.5, 0.0, 1.0],
            [2.0, 2.0, -1.0],
            [1.234, 0.567, 0.0],
        ];
        for [a0, h, a1] in curves {
            for p in queries {
                let got = nearest_on_quadratic(a0, h, a1, p);
                let (_, brute_d) = brute(a0, h, a1, p, 20_000);
                assert!(got.distance.is_finite(), "non-finite distance");
                assert!(
                    got.distance <= brute_d + 1e-9,
                    "closed form {} worse than brute force {brute_d} for {p:?} on {a0:?}{h:?}{a1:?}",
                    got.distance
                );
                assert!((0.0..=1.0).contains(&got.t), "t out of range: {}", got.t);
                // And the reported point is the reported parameter's point.
                let at = crate::bezier::quadratic_point(a0, h, a1, got.t);
                assert!(space_ops::get_norm(vec::sub(at, got.point)) < 1e-12);
            }
        }
    }

    /// GH #1: `set_points_as_corners` segments whose midpoint handle was
    /// rounded to `f32` storage. `C` is ~1e-8 against a chord of ~0.1, so the
    /// closed-form cubic lost every interior root and the nearest point
    /// collapsed to an endpoint — half a chord away from a query on the line.
    #[test]
    fn a_rounded_midpoint_handle_still_finds_the_interior_nearest_point() {
        let segments: [[Vec3; 3]; 3] = [
            [
                [0.440_766_543_149_948_1, 1.937_205_195_426_941, 0.0],
                [0.459_930_300_712_585_45, 1.850_354_909_896_850_6, 0.0],
                [0.479_094_088_077_545_17, 1.763_504_743_576_049_8, 0.0],
            ],
            [
                [2.395_470_380_783_081, 2.271_468_400_955_2, 0.0],
                [2.414_634_227_752_685_5, 2.165_081_977_844_238_3, 0.0],
                [2.433_797_836_303_711, 2.058_695_793_151_855_5, 0.0],
            ],
            [
                [3.966_898_918_151_855_5, 0.563_384_294_509_887_7, 0.0],
                [3.986_062_765_121_46, 0.519_783_735_275_268_6, 0.0],
                [4.005_226_612_091_064_5, 0.476_183_146_238_327, 0.0],
            ],
        ];
        for [a0, h, a1] in segments {
            let chord = space_ops::get_norm(vec::sub(a1, a0));
            let normal = [-(a1[1] - a0[1]) / chord, (a1[0] - a0[0]) / chord, 0.0];
            for k in 1..10 {
                let t = f64::from(k) / 10.0;
                for offset in [0.0, 0.01, -0.02] {
                    let on = crate::bezier::quadratic_point(a0, h, a1, t);
                    let p = vec::add(on, vec::scale(normal, offset));
                    let got = nearest_on_quadratic(a0, h, a1, p);
                    let (_, brute_d) = brute(a0, h, a1, p, 20_000);
                    assert!(
                        got.distance <= brute_d + 1e-12,
                        "t={t} offset={offset}: closed form {} vs brute force {brute_d} \
                         on {a0:?}{h:?}{a1:?}",
                        got.distance
                    );
                    assert!(
                        (got.distance - offset.abs()).abs() < 1e-7,
                        "t={t} offset={offset}: distance {} (chord {chord})",
                        got.distance
                    );
                    assert!((got.t - t).abs() < 1e-6, "t={t}: got {}", got.t);
                }
            }
        }
    }

    #[test]
    fn a_point_on_the_curve_has_zero_distance() {
        let (a0, h, a1) = ([0.0, 0.0, 0.0], [1.0, 2.0, 0.0], [3.0, 0.0, 0.0]);
        for k in 0..=10 {
            let t = f64::from(k) / 10.0;
            let p = crate::bezier::quadratic_point(a0, h, a1, t);
            let got = nearest_on_quadratic(a0, h, a1, p);
            assert!(got.distance < 1e-9, "t={t}: {}", got.distance);
        }
    }

    #[test]
    fn a_query_beyond_an_end_lands_on_that_end() {
        // No interior root reports an endpoint, which is why both are always
        // candidates.
        let (a0, h, a1) = ([0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [2.0, 0.0, 0.0]);
        let left = nearest_on_quadratic(a0, h, a1, [-5.0, 0.0, 0.0]);
        assert_eq!(left.t, 0.0);
        assert!((left.distance - 5.0).abs() < 1e-12);
        let right = nearest_on_quadratic(a0, h, a1, [7.0, 0.0, 0.0]);
        assert!((right.t - 1.0).abs() < 1e-12);
        assert!((right.distance - 5.0).abs() < 1e-12);
    }

    #[test]
    fn the_distance_helper_agrees_with_the_nearest_point() {
        let (a0, h, a1) = ([0.0, 0.0, 0.0], [1.0, 2.0, 0.0], [3.0, 0.0, 0.0]);
        let p = [1.7, 0.3, 0.0];
        assert_eq!(
            distance_to_quadratic(a0, h, a1, p),
            nearest_on_quadratic(a0, h, a1, p).distance
        );
    }
}
