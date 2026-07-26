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
//! It is kept because the Reference's *path-construction* fixtures are locked
//! to it, and because it is the thing [`cubic_to_quadratics`] — the one
//! error-bounded converter this crate actually uses (§7.2, fm-6cf) — is
//! measured against. It is not the converter.
//!
//! # The error-bounded converter
//!
//! [`cubic_to_quadratics`] reduces any cubic to a chain of quadratics that
//! holds a stated tolerance. The tolerance is enforced by a *proven* bound —
//! never a sampled estimate — and the sizing rests on an identity worth
//! writing down, because it is exact where the usual treatment is an
//! inequality.
//!
//! Approximate a cubic `(p0, p1, p2, p3)` by the single quadratic sharing its
//! endpoints whose control point is `q = (3p1 - p0 + 3p2 - p3) / 4`. Degree-
//! elevate that quadratic back to a cubic and its inner control points are
//! `(p0 + 2q)/3` and `(p3 + 2q)/3`. Subtracting, the two control-point
//! differences come out **exactly antisymmetric**:
//!
//! ```text
//! d1 = p1 - (p0 + 2q)/3 =  (p3 - p0 + 3p1 - 3p2) / 6
//! d2 = p2 - (p3 + 2q)/3 = -(p3 - p0 + 3p1 - 3p2) / 6 = -d1
//! ```
//!
//! Two cubics sharing endpoints differ by `3(1-t)²t·d1 + 3(1-t)t²·d2`, so with
//! `d2 = -d1` the whole error collapses to a *scalar* function times one fixed
//! vector:
//!
//! ```text
//! C(t) - Q(t) = 3t(1-t)(1-2t) · d1
//! ```
//!
//! `max |3t(1-t)(1-2t)|` on `[0,1]` is `1/(2√3)` at `t = ½ ± √3/6`, so the
//! maximum deviation is exactly `|d1| / (2√3) = |Δ| / (12√3)` where
//! `Δ = p3 - 3p2 + 3p1 - p0`. There is no inequality anywhere in that
//! derivation — see [`max_single_quadratic_deviation`].
//!
//! `Δ` is the cubic's third difference, and `C'''(t) = 6Δ` is *constant*, so a
//! sub-cubic spanning a parameter interval of width `h` has `Δ_sub = Δ·h³`.
//! Splitting uniformly into `n` pieces therefore divides that deviation by
//! exactly `n³`, which gives a closed-form starting count:
//!
//! ```text
//! n = ceil( cbrt( |Δ| / (12√3 · tolerance) ) )
//! ```
//!
//! **The emitted handle is not the natural one, though**, and that costs the
//! closed form. The natural handle is C⁰ only: a chain of them kinks at every
//! subdivision join, which `QuadPath::is_smooth` rejects at 1°. The converter
//! emits [`quadratic_handle`] — the end-tangent intersection, which is G¹ at
//! every join by construction — and that handle has no antisymmetry, so its
//! error is governed by the *inequality* `¾·max(|d1|,|d2|)` (cu2qu's bound)
//! rather than by an identity. `n` therefore starts at the closed form above
//! and doubles until every piece's bound holds.
//!
//! The loop compares f64 values produced in a fixed order from the inputs, so
//! it is deterministic in the §10.5 sense — the same cubic and tolerance give
//! the same count everywhere — and it is bounded by [`MAX_SEGMENTS`]. What the
//! exact identity still buys is a *tight* starting guess and the proof that
//! the loop terminates: the natural bound falls as `1/n³`, and the accepted
//! handle is held within a constant factor of it.

use crate::GeomError;
use crate::bezier;
use crate::space_ops;
use crate::vec;
use fmn_core::types::Vec3;

/// The default conversion tolerance, **in output pixels**, fixed by the G0-2
/// look study (`docs/g0/G0-2-look-study-ratification.md`, decision (f)).
///
/// The justification is the antialiasing band: coverage transitions over
/// 1.5 px, so 0.1 px of curve error perturbs it by roughly 7 % of one band
/// edge — invisible. For scale, the Reference's fixed two-quad split
/// ([`quadratic_approximation_of_cubic`]) was measured at up to **7.71 px** of
/// deviation at 1080p, over five times the entire AA band; 0.1 px is 77×
/// tighter than that worst case.
///
/// It is expressed in pixels *deliberately*: it is a visibility criterion, so
/// it must scale with resolution. Callers working in scene units convert with
/// [`tolerance_for_scale`].
pub const DEFAULT_TOLERANCE_PX: f64 = 0.1;

/// The largest number of quadratics [`cubic_to_quadratics`] will emit.
///
/// A resource guard, not a quality knob. `n` is closed-form, so a pathological
/// tolerance (or a non-finite input) would otherwise size an allocation
/// directly from arithmetic — a decompression-bomb shape, and §16's fuzzing
/// plane treats those as real. Reaching the cap is an error
/// ([`GeomError::ToleranceUnreachable`]) rather than a silently coarser curve.
pub const MAX_SEGMENTS: usize = 4096;

/// `1 / (12√3)` — the exact single-quadratic deviation constant, derived in
/// the module docs.
const DEVIATION_CONSTANT: f64 = 0.048112522432468816;

/// The default tolerance in *scene units*, given the render scale.
///
/// `px_per_unit` is the Reference's mapping: `FRAME_WIDTH / pixel_width`
/// inverted, i.e. 135.0 at 1920×1080 with the default frame.
#[must_use]
pub fn tolerance_for_scale(px_per_unit: f64) -> f64 {
    DEFAULT_TOLERANCE_PX / px_per_unit
}

/// The quadratic control point that makes the single-quadratic error
/// antisymmetric, and therefore exactly computable (module docs).
///
/// Used to *size* the subdivision, not to build it: it is C⁰ only, and a
/// chain of these has a visible kink at every join. The converter emits
/// [`quadratic_handle`] instead.
#[must_use]
pub fn natural_quadratic_handle(a0: Vec3, h0: Vec3, h1: Vec3, a1: Vec3) -> Vec3 {
    let mut q = [0.0; 3];
    for i in 0..3 {
        q[i] = (3.0 * h0[i] - a0[i] + 3.0 * h1[i] - a1[i]) * 0.25;
    }
    q
}

/// The quadratic control point where the cubic's two end tangents meet — the
/// handle the converter actually emits.
///
/// **This is what keeps the output smooth.** A handle on the end-tangent line
/// makes the quadratic share the cubic's tangent at both anchors, so two
/// adjacent pieces meet with their handles collinear through the shared
/// anchor: G¹ by construction, at every subdivision join. The natural handle
/// has a smaller *maximum* error but is only C⁰, and a chain of natural
/// handles fails `QuadPath::is_smooth` at 1° — which is how this was caught.
///
/// Falls back to the natural handle whenever the tangent construction is not
/// actually better — which covers three real cases, not just parallel
/// tangents:
///
/// - **parallel tangents**, where there is no intersection to find;
/// - **skew tangents**, which is the *general* case in 3D: four control points
///   need not be coplanar, so a non-planar cubic has end tangents that never
///   meet. (The Reference sidesteps this by only ever calling its two-quad
///   split on curves already rotated into the xy-plane.)
/// - **an intersection far outside the span**, where the handle would bow the
///   quadratic away from the curve it is meant to approximate.
///
/// The choice between them is by deviation bound, with slack: the tangent
/// handle is taken unless it is more than [`TANGENT_HANDLE_SLACK`] times worse
/// than the natural one, which is what a skew or parallel pair produces. A
/// plain "smaller bound wins" rule does not work — the natural handle usually
/// has the smaller maximum error, so it would win almost everywhere and the
/// output would be C⁰ again.
///
#[must_use]
pub fn quadratic_handle(a0: Vec3, h0: Vec3, h1: Vec3, a1: Vec3) -> Vec3 {
    let natural = natural_quadratic_handle(a0, h0, h1, a1);
    let t0 = vec::sub(h0, a0);
    let t1 = vec::sub(a1, h1);
    let q = space_ops::find_intersection(a0, t0, a1, t1, 1e-9);
    if !q.iter().all(|c| c.is_finite()) {
        return natural;
    }
    let p = [a0, h0, h1, a1];
    if deviation_bound(p, q) <= TANGENT_HANDLE_SLACK * deviation_bound(p, natural) {
        q
    } else {
        natural
    }
}

/// How much worse than the natural handle the tangent handle may be before it
/// is treated as degenerate.
///
/// On a well-conditioned planar piece the two are within a small factor; a
/// skew pair — the *general* case in 3D, where four control points need not be
/// coplanar and the end tangents never meet — overshoots by orders of
/// magnitude. Bounding the accepted handle by a constant multiple of the
/// natural one is also the termination argument for
/// [`segments_for_tolerance`]: the natural bound falls as `1/n³`, so the
/// accepted bound does too.
const TANGENT_HANDLE_SLACK: f64 = 4.0;

/// A **proven upper bound** on the deviation between the cubic `p` and the
/// quadratic with control point `q` sharing its endpoints.
///
/// Degree-elevating the quadratic gives inner control points `(p0+2q)/3` and
/// `(p3+2q)/3`; two cubics sharing endpoints differ by
/// `3(1-t)²t·d1 + 3(1-t)t²·d2`, and `max_t 3t(1-t) = 3/4`, so the deviation is
/// at most `¾·max(|d1|, |d2|)`. This is cu2qu's bound, and unlike
/// [`max_single_quadratic_deviation`] it really is an inequality — the price
/// of a handle chosen for smoothness rather than for symmetry.
#[must_use]
fn deviation_bound(p: [Vec3; 4], q: Vec3) -> f64 {
    let mut worst: f64 = 0.0;
    for (inner, end) in [(p[1], p[0]), (p[2], p[3])] {
        let mut d = [0.0; 3];
        for i in 0..3 {
            d[i] = inner[i] - (end[i] + 2.0 * q[i]) / 3.0;
        }
        worst = worst.max(space_ops::get_norm(d));
    }
    0.75 * worst
}

/// The **exact** maximum deviation between this cubic and its single-quadratic
/// approximation (module docs). Not an upper bound: the error polynomial is a
/// scalar function times one vector, so its maximum is closed-form.
#[must_use]
pub fn max_single_quadratic_deviation(a0: Vec3, h0: Vec3, h1: Vec3, a1: Vec3) -> f64 {
    // Δ = p3 - 3p2 + 3p1 - p0, the third difference.
    let mut delta = [0.0; 3];
    for i in 0..3 {
        delta[i] = a1[i] - 3.0 * h1[i] + 3.0 * h0[i] - a0[i];
    }
    space_ops::get_norm(delta) * DEVIATION_CONSTANT
}

/// How many uniform pieces this cubic needs to hold `tolerance`.
///
/// The closed form of the module docs sizes the *first guess* — it is the
/// exact answer for the natural handle, and the natural handle is optimal, so
/// no smaller count can work for any handle. From there the emitted
/// tangent-intersection handles are checked piece by piece against
/// [`deviation_bound`] and the count is raised until every piece holds.
///
/// The loop is deterministic: it compares f64 values produced in a fixed
/// order from the inputs, so the same cubic and tolerance yield the same count
/// on every machine (§10.5). What it is *not* is unbounded — see
/// [`MAX_SEGMENTS`].
pub fn segments_for_tolerance(
    a0: Vec3,
    h0: Vec3,
    h1: Vec3,
    a1: Vec3,
    tolerance: f64,
) -> Result<usize, GeomError> {
    if !tolerance.is_finite() || tolerance <= 0.0 {
        return Err(GeomError::InvalidTolerance);
    }
    let deviation = max_single_quadratic_deviation(a0, h0, h1, a1);
    if !deviation.is_finite() {
        // Non-finite control points. Refuse rather than size an allocation
        // from a NaN.
        return Err(GeomError::ToleranceUnreachable { needed: usize::MAX });
    }
    let mut n = if deviation <= tolerance {
        1
    } else {
        let exact = (deviation / tolerance).cbrt();
        if exact >= MAX_SEGMENTS as f64 {
            return Err(GeomError::ToleranceUnreachable { needed: usize::MAX });
        }
        (exact.ceil() as usize).max(1)
    };

    let p = [a0, h0, h1, a1];
    while n <= MAX_SEGMENTS {
        if (0..n).all(|i| {
            let sub = subsegment(p, i as f64 / n as f64, (i + 1) as f64 / n as f64);
            let q = quadratic_handle(sub[0], sub[1], sub[2], sub[3]);
            deviation_bound(sub, q) <= tolerance
        }) {
            return Ok(n);
        }
        // Geometric growth, not `n += 1`: the selected handle's error falls as
        // 1/n³, so doubling converges in a handful of steps, where stepping by
        // one can walk thousands of counts on a tight tolerance.
        n *= 2;
    }
    Err(GeomError::ToleranceUnreachable { needed: n })
}

/// **The one converter** (§7.2): reduce a cubic to a chain of quadratics whose
/// deviation from it is at most `tolerance`, in shared-anchor layout
/// `[a0, h, a1, h, a2, …]` (odd length, `2n + 1` points for `n` quadratics).
///
/// Every cubic source in the program routes here — API cubics, SVG path data,
/// smoothing output. (TrueType glyf outlines are already quadratic and are a
/// zero-loss passthrough that never reaches this function.) One audited
/// converter is what makes curve fidelity a property of the system rather than
/// of the call site.
///
/// # Failure
///
/// **This cannot fail on geometry.** Every finite cubic converts, and the
/// degenerate ones — zero length, collinear controls, an exact quadratic —
/// all take the one-piece path with zero or sub-tolerance error. The only
/// failures are a caller's `tolerance` that is not positive and finite
/// ([`GeomError::InvalidTolerance`]), and a request whose piece count would
/// exceed [`MAX_SEGMENTS`] ([`GeomError::ToleranceUnreachable`]) — which for
/// any sane tolerance means non-finite input.
pub fn cubic_to_quadratics(
    a0: Vec3,
    h0: Vec3,
    h1: Vec3,
    a1: Vec3,
    tolerance: f64,
) -> Result<Vec<Vec3>, GeomError> {
    let n = segments_for_tolerance(a0, h0, h1, a1, tolerance)?;
    let mut out = Vec::with_capacity(2 * n + 1);
    out.push(a0);
    for i in 0..n {
        let piece = subsegment(
            [a0, h0, h1, a1],
            i as f64 / n as f64,
            (i + 1) as f64 / n as f64,
        );
        out.push(quadratic_handle(piece[0], piece[1], piece[2], piece[3]));
        // The anchor is the sub-cubic's own endpoint, so every anchor in the
        // chain lies exactly on the original curve.
        out.push(piece[3]);
    }
    Ok(out)
}

/// de Casteljau split at `t`, returning `(left, right)` control quadruples.
fn split(p: [Vec3; 4], t: f64) -> ([Vec3; 4], [Vec3; 4]) {
    let l = |a: Vec3, b: Vec3| -> Vec3 {
        let mut o = [0.0; 3];
        for i in 0..3 {
            o[i] = a[i] + (b[i] - a[i]) * t;
        }
        o
    };
    let q1 = l(p[0], p[1]);
    let r = l(p[1], p[2]);
    let s1 = l(p[2], p[3]);
    let q2 = l(q1, r);
    let s = l(r, s1);
    let q3 = l(q2, s);
    ([p[0], q1, q2, q3], [q3, s, s1, p[3]])
}

/// The control points of the cubic restricted to `[t0, t1] ⊆ [0, 1]`.
fn subsegment(p: [Vec3; 4], t0: f64, t1: f64) -> [Vec3; 4] {
    if t0 <= 0.0 {
        return split(p, t1).0;
    }
    let right = split(p, t0).1;
    // Re-normalize t1 into the right piece's own parameter.
    let u = (t1 - t0) / (1.0 - t0);
    split(right, u.clamp(0.0, 1.0)).0
}

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

    // ---------------------------------------------------------------- fm-6cf

    /// Deterministic pseudo-random cubics from the one RNG (§6.5), so these
    /// property tests are the same test on every machine and every run.
    fn random_cubics(n: usize) -> Vec<[Vec3; 4]> {
        let mut rng = fmn_core::rng::RngRoot::from_seed(0x6cf)
            .substream("cubic-converter")
            .sequential();
        let mut span = || (rng.next_f64() - 0.5) * 12.0;
        (0..n)
            .map(|_| {
                [
                    [span(), span(), span()],
                    [span(), span(), span()],
                    [span(), span(), span()],
                    [span(), span(), span()],
                ]
            })
            .collect()
    }

    fn dist(a: Vec3, b: Vec3) -> f64 {
        space_ops::get_norm(vec::sub(a, b))
    }

    /// The largest parameter-matched deviation between a cubic and its single
    /// natural quadratic, by dense sampling.
    fn sampled_single_deviation(p: [Vec3; 4], steps: usize) -> f64 {
        let q = natural_quadratic_handle(p[0], p[1], p[2], p[3]);
        (0..=steps)
            .map(|k| {
                let t = k as f64 / steps as f64;
                dist(
                    bezier::cubic_point(p[0], p[1], p[2], p[3], t),
                    bezier::quadratic_point(p[0], q, p[3], t),
                )
            })
            .fold(0.0, f64::max)
    }

    #[test]
    fn single_quadratic_deviation_is_exact_not_a_bound() {
        // The claim in the module docs is equality, not `<=`. If the closed
        // form were merely an upper bound this test would show it drifting
        // above the sampled maximum on curved inputs.
        for (i, p) in random_cubics(200).into_iter().enumerate() {
            let closed = max_single_quadratic_deviation(p[0], p[1], p[2], p[3]);
            let sampled = sampled_single_deviation(p, 20_000);
            let scale = closed.max(1e-12);
            assert!(
                (closed - sampled).abs() / scale < 1e-6,
                "case {i}: closed form {closed} vs sampled {sampled}"
            );
        }
    }

    #[test]
    fn converted_chain_holds_the_tolerance() {
        // Parameter-matched deviation is an upper bound on geometric
        // deviation, so holding it is the stronger statement.
        for tolerance in [1.0, 0.1, 0.01, 1e-3, 1e-5] {
            for (i, p) in random_cubics(60).into_iter().enumerate() {
                let n = segments_for_tolerance(p[0], p[1], p[2], p[3], tolerance).unwrap();
                let out = cubic_to_quadratics(p[0], p[1], p[2], p[3], tolerance).unwrap();
                assert_eq!(out.len(), 2 * n + 1);
                let mut worst: f64 = 0.0;
                for piece in 0..n {
                    let sub = subsegment(p, piece as f64 / n as f64, (piece + 1) as f64 / n as f64);
                    let a = out[2 * piece];
                    let h = out[2 * piece + 1];
                    let b = out[2 * piece + 2];
                    for k in 0..=400 {
                        let u = k as f64 / 400.0;
                        worst = worst.max(dist(
                            bezier::cubic_point(sub[0], sub[1], sub[2], sub[3], u),
                            bezier::quadratic_point(a, h, b, u),
                        ));
                    }
                }
                assert!(
                    worst <= tolerance * (1.0 + 1e-9),
                    "tol {tolerance}, case {i}: worst deviation {worst} over {n} pieces"
                );
            }
        }
    }

    #[test]
    fn every_anchor_lies_on_the_original_curve() {
        for p in random_cubics(50) {
            let tol = 0.01;
            let out = cubic_to_quadratics(p[0], p[1], p[2], p[3], tol).unwrap();
            let n = (out.len() - 1) / 2;
            for piece in 0..=n {
                let t = piece as f64 / n as f64;
                let on_curve = bezier::cubic_point(p[0], p[1], p[2], p[3], t);
                assert!(dist(out[2 * piece], on_curve) < 1e-9);
            }
        }
    }

    #[test]
    fn deviation_falls_as_the_cube_of_the_piece_count() {
        // The closed-form n rests on Δ_sub = Δ·h³. If that were wrong the
        // converter would still "work" but would over- or under-subdivide.
        let p = [
            [-1.0, -1.0, 0.0],
            [-1.0, 1.6, 0.0],
            [1.0, 1.6, 0.0],
            [1.0, -1.0, 0.0],
        ];
        let one = max_single_quadratic_deviation(p[0], p[1], p[2], p[3]);
        for n in [2usize, 3, 5, 8] {
            let sub = subsegment(p, 0.0, 1.0 / n as f64);
            let got = max_single_quadratic_deviation(sub[0], sub[1], sub[2], sub[3]);
            let want = one / (n as f64).powi(3);
            assert!(
                (got - want).abs() / want < 1e-9,
                "n={n}: piece deviation {got}, expected {want}"
            );
        }
    }

    #[test]
    fn an_exact_quadratic_converts_to_one_piece_with_no_error() {
        // Degree-elevate a quadratic to a cubic: Δ is exactly zero, so the
        // converter must not subdivide at all and must lose nothing.
        let (a, q, b) = ([0.0, 0.0, 0.0], [1.0, 2.0, 0.0], [3.0, 0.0, 0.0]);
        let h0 = [
            a[0] + 2.0 / 3.0 * (q[0] - a[0]),
            a[1] + 2.0 / 3.0 * (q[1] - a[1]),
            0.0,
        ];
        let h1 = [
            b[0] + 2.0 / 3.0 * (q[0] - b[0]),
            b[1] + 2.0 / 3.0 * (q[1] - b[1]),
            0.0,
        ];
        assert!(max_single_quadratic_deviation(a, h0, h1, b) < 1e-15);
        let out = cubic_to_quadratics(a, h0, h1, b, 1e-9).unwrap();
        assert_eq!(out.len(), 3);
        assert!(dist(out[1], q) < 1e-12, "recovered handle {:?}", out[1]);
    }

    #[test]
    fn degenerate_inputs_have_defined_output() {
        let tol = 0.01;
        // Zero-length: every control point coincident.
        let z = [7.0, -2.0, 1.0];
        let out = cubic_to_quadratics(z, z, z, z, tol).unwrap();
        assert_eq!(out.len(), 3);
        assert!(out.iter().all(|p| dist(*p, z) < 1e-15));

        // Collinear controls stay on the line (a straight cubic has Δ ≠ 0 in
        // general, so this exercises subdivision, not the trivial path).
        let out = cubic_to_quadratics(
            [0.0, 0.0, 0.0],
            [4.0, 0.0, 0.0],
            [-2.0, 0.0, 0.0],
            [3.0, 0.0, 0.0],
            tol,
        )
        .unwrap();
        assert!(out.iter().all(|p| p[1].abs() < 1e-12 && p[2].abs() < 1e-12));

        // An inflection exactly at a parameter bound.
        let out = cubic_to_quadratics(
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [2.0, 1.0, 0.0],
            [3.0, 0.0, 0.0],
            tol,
        )
        .unwrap();
        assert!(out.len() % 2 == 1 && out.len() >= 3);

        // A cusp.
        let out = cubic_to_quadratics(
            [0.0, 0.0, 0.0],
            [1.4, 0.0, 0.0],
            [-1.4, 0.0, 0.0],
            [0.0, 0.2, 0.0],
            tol,
        )
        .unwrap();
        assert!(out.len() % 2 == 1);
    }

    #[test]
    fn the_converter_fails_only_on_the_request() {
        let (a0, h0, h1, a1) = (
            [0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [1.0, 2.0, 0.0],
            [2.0, 2.0, 0.0],
        );
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(
                cubic_to_quadratics(a0, h0, h1, a1, bad),
                Err(GeomError::InvalidTolerance),
                "tolerance {bad} should be refused"
            );
        }
        // A tolerance so fine it would blow the segment cap is refused by
        // name rather than silently coarsened.
        assert!(matches!(
            cubic_to_quadratics(a0, h0, h1, a1, 1e-300),
            Err(GeomError::ToleranceUnreachable { .. })
        ));
        // Non-finite geometry is refused, never turned into an allocation.
        assert!(matches!(
            cubic_to_quadratics(a0, [f64::NAN, 0.0, 0.0], h1, a1, 0.1),
            Err(GeomError::ToleranceUnreachable { .. })
        ));
    }

    #[test]
    fn conversion_is_bit_identical_across_calls() {
        for p in random_cubics(40) {
            let a = cubic_to_quadratics(p[0], p[1], p[2], p[3], 0.01).unwrap();
            let b = cubic_to_quadratics(p[0], p[1], p[2], p[3], 0.01).unwrap();
            assert_eq!(a.len(), b.len());
            for (x, y) in a.iter().zip(b.iter()) {
                for i in 0..3 {
                    assert_eq!(x[i].to_bits(), y[i].to_bits());
                }
            }
        }
    }

    #[test]
    fn the_default_tolerance_beats_the_reference_by_the_margin_g0_2_measured() {
        // G0-2 L8 measured the Reference's fixed two-quad split at up to
        // 7.71 px on this cubic at 135 px/unit. Ours must land at or under
        // the 0.1 px default — the claim that "curve fidelity visibly exceeds
        // the Reference's" is this assertion.
        const PX_PER_UNIT: f64 = 135.0;
        let p = [
            [-1.0, -1.0, 0.0],
            [-1.0, 1.6, 0.0],
            [1.0, 1.6, 0.0],
            [1.0, -1.0, 0.0],
        ];
        let tol = tolerance_for_scale(PX_PER_UNIT);
        let n = segments_for_tolerance(p[0], p[1], p[2], p[3], tol).unwrap();
        let out = cubic_to_quadratics(p[0], p[1], p[2], p[3], tol).unwrap();
        let mut worst: f64 = 0.0;
        for piece in 0..n {
            let sub = subsegment(p, piece as f64 / n as f64, (piece + 1) as f64 / n as f64);
            for k in 0..=500 {
                let u = k as f64 / 500.0;
                worst = worst.max(dist(
                    bezier::cubic_point(sub[0], sub[1], sub[2], sub[3], u),
                    bezier::quadratic_point(
                        out[2 * piece],
                        out[2 * piece + 1],
                        out[2 * piece + 2],
                        u,
                    ),
                ));
            }
        }
        let worst_px = worst * PX_PER_UNIT;
        assert!(
            worst_px <= DEFAULT_TOLERANCE_PX * (1.0 + 1e-9),
            "{n} pieces, worst {worst_px} px"
        );
        // And the Reference's own splitter on the same curve, for the record.
        let reference = quadratic_approximation_of_cubic(p[0], p[1], p[2], p[3]);
        let mut ref_worst: f64 = 0.0;
        for k in 0..=2000 {
            let t = k as f64 / 2000.0;
            let c = bezier::cubic_point(p[0], p[1], p[2], p[3], t);
            let mut best = f64::INFINITY;
            for half in 0..2 {
                for j in 0..=400 {
                    let u = j as f64 / 400.0;
                    let q = bezier::quadratic_point(
                        reference[2 * half],
                        reference[2 * half + 1],
                        reference[2 * half + 2],
                        u,
                    );
                    best = best.min(dist(c, q));
                }
            }
            ref_worst = ref_worst.max(best);
        }
        assert!(
            ref_worst * PX_PER_UNIT > 5.0,
            "the Reference splitter was measured at 7.71 px here; got {}",
            ref_worst * PX_PER_UNIT
        );
    }
}
