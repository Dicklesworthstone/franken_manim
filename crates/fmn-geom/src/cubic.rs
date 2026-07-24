//! Cubic→quadratic reduction, single-curve form.
//!
//! This is the exact port of the Reference's
//! `get_quadratic_approximation_of_cubic` (`manimlib/utils/bezier.py` @
//! `6199a00d`): split the cubic at an interior inflection point when one
//! exists (else at t = ½) and approximate each half with one quadratic whose
//! handle is the tangent-line intersection. The inflection search uses xy
//! cross products, so — like the Reference — it assumes the curve has been
//! brought to the xy-plane.
//!
//! **fm-6cf** replaces call sites with the one error-bounded converter
//! (cu2qu-class monotone subdivision to a stated tolerance, §7.2); this
//! two-quad split then remains only as that converter's terminal case.

use crate::bezier;
use crate::space_ops;
use crate::vec;
use fmn_core::types::Vec3;

/// Approximate the cubic `(a0, h0, h1, a1)` with two joined quadratics,
/// returned in shared-anchor layout: `[a0, i0, mid, i1, a1]`.
#[must_use]
pub fn quadratic_approximation_of_cubic(a0: Vec3, h0: Vec3, h1: Vec3, a1: Vec3) -> [Vec3; 5] {
    // Tangent directions at the ends.
    let t0 = vec::sub(h0, a0);
    let t1 = vec::sub(a1, h1);

    // Inflection points of the planar cubic, per
    // caffeineowl.com/graphics/2d/vectorial/cubic-inflexion.html.
    let p = vec::sub(h0, a0);
    let q = vec::add(vec::sub(h1, vec::scale(h0, 2.0)), a0);
    let r = vec::sub(
        vec::add(a1, vec::scale(h0, 3.0)),
        vec::add(vec::scale(h1, 3.0), a0),
    );

    let a = space_ops::cross2d(q, r);
    let b = space_ops::cross2d(p, r);
    let c = space_ops::cross2d(p, q);

    let disc = b * b - 4.0 * a * c;
    let has_infl = disc > 0.0;
    let sqrt_disc = disc.abs().sqrt();
    let root = |sgn: f64| -> f64 {
        if a == 0.0 {
            if b == 0.0 { 0.0 } else { -c / b }
        } else {
            (-b + sgn * sqrt_disc) / (2.0 * a)
        }
    };
    let ti_min = root(-1.0);
    let ti_max = root(1.0);

    // t starts at ½ and is replaced by an interior inflection if one exists;
    // when both roots are interior the Reference lets the larger win.
    let mut t_mid = 0.5;
    if has_infl && 0.0 < ti_min && ti_min < 1.0 {
        t_mid = ti_min;
    }
    if has_infl && 0.0 < ti_max && ti_max < 1.0 {
        t_mid = ti_max;
    }

    let mid = bezier::cubic_point(a0, h0, h1, a1, t_mid);
    // The derivative direction, via the quadratic on the difference points.
    let tm = bezier::quadratic_point(vec::sub(h0, a0), vec::sub(h1, h0), vec::sub(a1, h1), t_mid);

    let i0 = space_ops::find_intersection(a0, t0, mid, tm, 1e-5);
    let i1 = space_ops::find_intersection(a1, t1, mid, tm, 1e-5);

    [a0, i0, mid, i1, a1]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approximation_interpolates_endpoints_and_midpoint() {
        let a0 = [0.0, 0.0, 0.0];
        let h0 = [0.0, 1.0, 0.0];
        let h1 = [1.0, 2.0, 0.0];
        let a1 = [2.0, 2.0, 0.0];
        let out = quadratic_approximation_of_cubic(a0, h0, h1, a1);
        assert_eq!(out[0], a0);
        assert_eq!(out[4], a1);
        // The split point lies on the cubic.
        let on_curve = bezier::cubic_point(a0, h0, h1, a1, 0.5);
        for i in 0..3 {
            assert!((out[2][i] - on_curve[i]).abs() < 1e-12);
        }
    }

    #[test]
    fn degenerate_collinear_cubic_stays_on_line() {
        let out = quadratic_approximation_of_cubic(
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [3.0, 0.0, 0.0],
        );
        for p in out {
            assert!(p[1].abs() < 1e-12 && p[2].abs() < 1e-12);
        }
    }
}
