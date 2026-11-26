//! fm-xci acceptance: arc-length oracles against closed forms, the
//! inverse-LUT property, constant-speed metamorphic behavior, and the
//! geometry-revision-only cache invalidation.

use fmn_core::constants::TAU;
use fmn_geom::arclength::{quadratic_arc_length, t_at_arc_fraction};
use fmn_geom::{ArcLengthTable, CachedArcLength, QuadPath};

fn p(x: f64, y: f64) -> [f64; 3] {
    [x, y, 0.0]
}

// For controls (0,0), (s,0), (s,s), the exact length is
// s * (1 + asinh(1) / sqrt(2)). These inverse parameters come from
// independently inverting its analytic partial integral.
const RIGHT_ANGLE_LENGTH: f64 = 1.623_225_240_140_230_5;
const RIGHT_ANGLE_INVERSE: [(f64, f64); 5] = [
    (0.015625, 0.012_762_538_576_773_48),
    (0.25, 0.226_180_422_283_570_44),
    (0.5, 0.5),
    (0.75, 0.773_819_577_716_429_4),
    (0.984375, 0.987_237_461_423_226_4),
];

fn extreme_scales() -> [f64; 10] {
    [
        f64::from_bits(1),
        f64::from_bits(1 << 24),
        f64::MIN_POSITIVE,
        2.0f64.powi(-600),
        2.0f64.powi(-300),
        1.0,
        2.0f64.powi(300),
        2.0f64.powi(600),
        2.0f64.powi(1000),
        2.0f64.powi(1022),
    ]
}

fn assert_positive_length(actual: f64, expected: f64) {
    assert!(actual.is_finite() && actual > 0.0, "length {actual:e}");
    // A relative tolerance alone is meaningless for subnormal lengths.
    let tolerance = (8.0 * f64::EPSILON * expected).max(f64::from_bits(1));
    assert!(
        (actual - expected).abs() <= tolerance,
        "length {actual:e} versus analytic {expected:e} (tolerance {tolerance:e})"
    );
}

#[test]
fn analytic_lengths_survive_the_finite_binary64_scale_range() {
    for scale in extreme_scales() {
        let a = p(0.0, 0.0);
        let h = p(scale, 0.0);
        let b = p(scale, scale);
        let expected = RIGHT_ANGLE_LENGTH * scale;
        assert_positive_length(quadratic_arc_length(a, h, b), expected);
        assert_positive_length(quadratic_arc_length(b, h, a), expected);
        assert_positive_length(quadratic_arc_length(a, h, a), scale);
        assert_positive_length(quadratic_arc_length(a, h, p(2.0 * scale, 0.0)), 2.0 * scale);
    }
    assert_eq!(
        quadratic_arc_length(
            p(0.0, 0.0),
            p(f64::from_bits(1), 0.0),
            p(f64::from_bits(1), f64::from_bits(1))
        ),
        f64::from_bits(2),
        "the smallest right-angle curve rounds to two subnormal units"
    );
}

#[test]
fn inverse_arc_fraction_is_independent_of_extreme_scale() {
    for scale in extreme_scales() {
        let a = p(0.0, 0.0);
        let h = p(scale, 0.0);
        let b = p(scale, scale);
        for (fraction, expected) in RIGHT_ANGLE_INVERSE {
            let t = t_at_arc_fraction(a, h, b, fraction);
            let reversed = t_at_arc_fraction(b, h, a, 1.0 - fraction);
            assert!(
                (t - expected).abs() < 3e-13,
                "scale {scale:e}, fraction {fraction}: t={t}, expected {expected}"
            );
            assert!((reversed - (1.0 - expected)).abs() < 3e-13);
        }
        assert_eq!(t_at_arc_fraction(a, h, b, -1.0), 0.0);
        assert_eq!(t_at_arc_fraction(a, h, b, 2.0), 1.0);
    }
}

#[test]
fn huge_translation_preserves_the_represented_curve() {
    let base = 2.0f64.powi(1023);
    let delta = 2.0f64.powi(971); // One ULP at base, retained in all three points.
    let a = p(base, base);
    let h = p(base + delta, base);
    let b = p(base + delta, base + delta);
    assert_positive_length(quadratic_arc_length(a, h, b), RIGHT_ANGLE_LENGTH * delta);
    for (fraction, expected) in RIGHT_ANGLE_INVERSE {
        assert!((t_at_arc_fraction(a, h, b, fraction) - expected).abs() < 3e-13);
    }
    assert_eq!(
        quadratic_arc_length([f64::MAX; 3], [f64::MAX; 3], [f64::MAX; 3]),
        0.0
    );
    assert_eq!(
        t_at_arc_fraction([f64::MAX; 3], [f64::MAX; 3], [f64::MAX; 3], 0.5),
        0.0
    );
}

#[test]
fn an_overflowing_control_edge_can_have_finite_arc_length() {
    // The derivative crosses zero at t=4/5. Integrating its absolute
    // linear speed gives (17/5)q even though h-a overflows to infinity.
    let q = 2.0f64.powi(1022);
    let a = p(-3.0 * q, 0.0);
    let h = p(q, 0.0);
    let b = p(0.0, 0.0);
    assert!((h[0] - a[0]).is_infinite());
    assert_positive_length(quadratic_arc_length(a, h, b), (17.0 / 5.0) * q);
    assert_positive_length(quadratic_arc_length(b, h, a), (17.0 / 5.0) * q);
    let half_parameter = (4.0 - 7.5f64.sqrt()) / 5.0;
    assert!((t_at_arc_fraction(a, h, b, 0.5) - half_parameter).abs() < 3e-13);
}

#[test]
fn overflowing_physical_length_keeps_a_defined_arc_fraction() {
    let a = p(-f64::MAX, 0.0);
    let h = p(0.0, 0.0);
    let b = p(f64::MAX, 0.0);
    assert_eq!(quadratic_arc_length(a, h, b), f64::INFINITY);
    for fraction in [0.0, 0.125, 0.25, 0.5, 0.75, 1.0] {
        assert!((t_at_arc_fraction(a, h, b, fraction) - fraction).abs() < 3e-13);
    }
}

#[test]
fn retained_weights_preserve_unequal_curves_at_extreme_scales() {
    for scale in extreme_scales() {
        // Two right-angle curves with lengths in ratio 1:2. Rounded
        // physical lengths have ratio 2:3 at the smallest subnormal scale,
        // and their sum overflows at the largest scale in this test.
        let path = QuadPath::from_points(vec![
            p(0.0, 0.0),
            p(scale, 0.0),
            p(scale, scale),
            p(scale, 3.0 * scale),
            p(3.0 * scale, 3.0 * scale),
        ])
        .unwrap();
        let table = ArcLengthTable::for_path(&path);
        assert_eq!(table.curve_lengths().len(), 2);
        assert_positive_length(table.curve_lengths()[0], RIGHT_ANGLE_LENGTH * scale);
        assert_positive_length(table.curve_lengths()[1], (2.0 * RIGHT_ANGLE_LENGTH) * scale);
        let expected_total = (3.0 * RIGHT_ANGLE_LENGTH) * scale;
        if expected_total.is_infinite() {
            assert_eq!(table.total(), f64::INFINITY);
        } else {
            assert_positive_length(table.total(), expected_total);
        }
        for (alpha, expected_index, expected_t) in [
            (1.0 / 12.0, 0, RIGHT_ANGLE_INVERSE[1].1),
            (1.0 / 6.0, 0, 0.5),
            (0.25, 0, RIGHT_ANGLE_INVERSE[3].1),
            (0.5, 1, RIGHT_ANGLE_INVERSE[1].1),
            (2.0 / 3.0, 1, 0.5),
            (5.0 / 6.0, 1, RIGHT_ANGLE_INVERSE[3].1),
        ] {
            let (index, t) = table.curve_and_t_at(&path, alpha).unwrap();
            assert_eq!(index, expected_index, "scale {scale:e}, alpha {alpha}");
            assert!(
                (t - expected_t).abs() < 3e-13,
                "scale {scale:e}, alpha {alpha}: t={t}, expected {expected_t}"
            );
        }
    }
}

#[test]
fn extreme_retained_tables_skip_pen_up_moves_and_keep_endpoint_contours() {
    let scale = 2.0f64.powi(600);
    let path = QuadPath::from_points(vec![
        p(-scale, 0.0),
        p(-scale, 0.0),
        p(0.0, 0.0),
        p(scale, 0.0),
        p(scale, scale),
        p(scale, scale),
        p(8.0 * scale, 0.0),
        p(8.0 * scale, scale),
        p(9.0 * scale, scale),
        p(9.0 * scale, scale),
        p(10.0 * scale, 0.0),
    ])
    .unwrap();
    let mut cache = CachedArcLength::new();
    let table = cache.get(7, &path);
    assert_eq!(table.curve_lengths().len(), 5);
    assert_eq!(table.curve_lengths()[0], 0.0);
    assert_eq!(table.curve_lengths()[2], 0.0);
    assert_eq!(table.curve_lengths()[4], 0.0);
    assert_positive_length(table.total(), (2.0 * RIGHT_ANGLE_LENGTH) * scale);
    assert_eq!(table.curve_and_t_at(&path, 0.0), Some((0, 0.0)));
    assert_eq!(table.curve_and_t_at(&path, 0.5), Some((3, 0.0)));
    assert_eq!(table.curve_and_t_at(&path, 1.0), Some((4, 1.0)));
    let (index, t) = table.curve_and_t_at(&path, 0.75).unwrap();
    assert_eq!(index, 3);
    assert!((t - 0.5).abs() < 3e-13);
    let _ = cache.get(7, &path);
    assert_eq!(cache.rebuilds(), 1);
}

/// Reference numeric integration: fine composite Simpson on |B'(t)| —
/// the independent oracle the closed form must agree with.
fn simpson_length(a0: [f64; 3], h: [f64; 3], a1: [f64; 3]) -> f64 {
    let speed = |t: f64| -> f64 {
        let d = [
            2.0 * ((1.0 - t) * (h[0] - a0[0]) + t * (a1[0] - h[0])),
            2.0 * ((1.0 - t) * (h[1] - a0[1]) + t * (a1[1] - h[1])),
            2.0 * ((1.0 - t) * (h[2] - a0[2]) + t * (a1[2] - h[2])),
        ];
        (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
    };
    let n = 20_000;
    let step = 1.0 / n as f64;
    let mut sum = speed(0.0) + speed(1.0);
    for i in 1..n {
        let weight = if i % 2 == 1 { 4.0 } else { 2.0 };
        sum += weight * speed(i as f64 * step);
    }
    sum * step / 3.0
}

#[test]
fn closed_form_matches_numeric_oracle() {
    let cases = [
        (p(0.0, 0.0), p(1.0, 1.0), p(2.0, 0.0)),    // parabolic arc
        (p(0.0, 0.0), p(0.5, 0.0), p(1.0, 0.0)),    // straight line
        (p(-1.0, 2.0), p(3.0, 5.0), p(0.5, -4.0)),  // generic
        (p(0.0, 0.0), p(1e-3, 1e-3), p(2e-3, 0.0)), // tiny
        ([0.0, 0.0, 1.0], [1.0, 2.0, -1.0], [2.0, 0.0, 3.0]), // spatial
    ];
    for (a0, h, a1) in cases {
        let exact = quadratic_arc_length(a0, h, a1);
        let numeric = simpson_length(a0, h, a1);
        assert!(
            (exact - numeric).abs() <= 1e-9 * (1.0 + numeric),
            "closed form {exact} vs Simpson {numeric} for {a0:?} {h:?} {a1:?}"
        );
    }
}

#[test]
fn line_and_degenerate_lengths_are_exact() {
    // Straight line (handle at midpoint): exactly the chord.
    assert_eq!(
        quadratic_arc_length(p(0.0, 0.0), p(1.5, 0.0), p(3.0, 0.0)),
        3.0
    );
    // Point (all coincident): zero.
    assert_eq!(
        quadratic_arc_length(p(2.0, 2.0), p(2.0, 2.0), p(2.0, 2.0)),
        0.0
    );
    // Interior cusp: out 0.5 and back — total length exactly 1.
    let cusp = quadratic_arc_length(p(0.0, 0.0), p(1.0, 0.0), p(0.0, 0.0));
    assert!((cusp - 1.0).abs() < 1e-15, "cusp length {cusp}");
    // Collinear but forward (handle off-midpoint): still the chord.
    let skewed = quadratic_arc_length(p(0.0, 0.0), p(0.25, 0.0), p(1.0, 0.0));
    assert!((skewed - 1.0).abs() < 1e-12, "skewed-line length {skewed}");
}

#[test]
fn near_straight_curves_do_not_cancel_to_zero() {
    // The regression this guards: a segment sampled at two nearby points
    // and scaled up (a tangent line, a trimmed dash, any zoom) has a
    // handle that is the midpoint to within rounding. Its quadratic term
    // is ~1e-33 of its linear one, and the general antiderivative's two
    // terms — each ~1e16 — used to cancel to exactly zero. Now the
    // near-straight case is detected relatively and integrated as the
    // linear-speed curve it is.
    let cases = [
        (
            p(1.099_550_882_214_647_8, 4.095_488_941_355_138),
            p(-1.500_235_056_200_356, 2.598_454_180_763_514_6),
            p(-4.100_020_994_615_36, 1.101_419_420_171_891_2),
        ),
        // Deliberately perturbed midpoints across several magnitudes.
        (p(0.0, 0.0), p(500.0 + 1e-9, 1e-9), p(1000.0, 0.0)),
        (p(0.0, 0.0), p(0.5, 1e-12), p(1.0, 0.0)),
        (p(-1e6, 0.0), p(0.0, 1e-7), p(1e6, 0.0)),
    ];
    for (a0, h, a1) in cases {
        let closed = quadratic_arc_length(a0, h, a1);
        let numeric = simpson_length(a0, h, a1);
        assert!(
            closed > 0.0,
            "near-straight curve reported zero length: {a0:?} {h:?} {a1:?}"
        );
        assert!(
            (closed - numeric).abs() <= 1e-9 * numeric.max(1.0),
            "closed {closed} vs numeric {numeric}"
        );
    }
}

#[test]
fn the_near_straight_branch_meets_the_general_one() {
    // Sweep the quadratic term down through the branch threshold: the
    // reported length has to stay continuous across it, or a curve would
    // change length as it is scaled.
    let mut previous = f64::NAN;
    for k in 4..24 {
        let bend = 10f64.powi(-k);
        let length = quadratic_arc_length(p(0.0, 0.0), p(0.5, bend), p(1.0, 0.0));
        assert!(length >= 1.0 - 1e-12, "bend {bend}: length {length}");
        if previous.is_finite() {
            assert!(
                (length - previous).abs() < 1e-3,
                "discontinuity at bend {bend}: {previous} -> {length}"
            );
        }
        previous = length;
    }
    assert!((previous - 1.0).abs() < 1e-12, "flat limit {previous}");
}

#[test]
fn parabola_matches_analytic_formula() {
    // (0,0) h(1,1) (2,0) is y = x(2−x)/… as a Bézier: B(t) = (2t, 2t(1−t)).
    // Speed = 2√(1 + (1−2t)²·…) — use the standard parabola arc-length
    // closed form via substitution u = 1−2t: L = ∫₀¹ 2√(1+(2−4t+…)) …
    // Simpler: independently, L = √5 + asinh(2)/… — compute with the
    // textbook formula for y = f(x): ∫₀² √(1 + (1−x)²) dx
    //   = [ (x−1)/2·√(1+(x−1)²) + asinh(x−1)/2 ]₀²
    //   = √2 + asinh(1).
    let expected = 2.0f64.sqrt() + 1.0f64.asinh();
    let got = quadratic_arc_length(p(0.0, 0.0), p(1.0, 1.0), p(2.0, 0.0));
    assert!(
        (got - expected).abs() < 1e-12,
        "parabola: {got} vs analytic {expected}"
    );
}

#[test]
fn full_circle_approximation_length_is_close_to_tau() {
    // The BN-09 16-component circle: the quadratic spline's true length
    // is close to (and distinct from) the ideal circumference.
    let circle = QuadPath::try_arc(0.0, TAU, 1.0, [0.0; 3], None).expect("valid arc");
    let len = circle.get_arc_length();
    assert!((len - TAU).abs() < 2e-3, "spline circumference {len}");
    // The Reference's get_arc_length (chord/handle blend) would land
    // farther out; ours integrates the actual curves.
}

#[test]
fn inverse_lut_round_trips() {
    let mut path = QuadPath::new();
    path.set_points_as_corners(&[p(0.0, 0.0), p(3.0, 0.0), p(3.0, 1.0), p(7.0, 1.0)])
        .unwrap();
    let table = ArcLengthTable::for_path(&path);
    let total = table.total();
    assert!((total - 8.0).abs() < 1e-12); // 3 + 1 + 4

    // point_from_proportion(L(t)/total) lands back at parameter ≈ t.
    for i in 0..=20 {
        let alpha = i as f64 / 20.0;
        let (index, t) = table.curve_and_t_at(&path, alpha).unwrap();
        // Reconstruct accumulated length at (index, t) and compare.
        let mut acc: f64 = table.curve_lengths()[..index].iter().sum();
        let [a0, h, a1] = path.nth_curve_points(index).unwrap();
        let sub = fmn_geom::bezier::partial_quadratic(&[a0, h, a1], 0.0, t);
        acc += quadratic_arc_length(sub[0], sub[1], sub[2]);
        assert!(
            (acc - alpha * total).abs() < 1e-9 * (1.0 + total),
            "alpha {alpha}: accumulated {acc} vs target {}",
            alpha * total
        );
    }
}

#[test]
fn constant_speed_metamorphic() {
    // Equal proportion steps ⇒ equal arc steps, even on a wildly uneven
    // path (a short curve followed by a long one).
    let mut path = QuadPath::new();
    path.start_new_path(p(0.0, 0.0));
    path.add_quadratic_bezier_curve_to(p(0.05, 0.1), p(0.1, 0.0), true)
        .unwrap();
    path.add_quadratic_bezier_curve_to(p(5.0, 6.0), p(10.0, 0.0), true)
        .unwrap();
    let table = ArcLengthTable::for_path(&path);
    let n = 64;
    let points: Vec<[f64; 3]> = (0..=n)
        .map(|i| {
            path.point_from_proportion_with(&table, i as f64 / n as f64)
                .unwrap()
        })
        .collect();
    // Measure each step's arc length with a fine polyline (chord sums on
    // proportion substeps), and compare steps against the mean.
    let step_arc = |i: usize| -> f64 {
        let sub = 64;
        let mut sum = 0.0;
        let mut prev = points[i];
        for k in 1..=sub {
            let alpha = (i as f64 + k as f64 / sub as f64) / n as f64;
            let q = path.point_from_proportion_with(&table, alpha).unwrap();
            sum += ((q[0] - prev[0]).powi(2) + (q[1] - prev[1]).powi(2) + (q[2] - prev[2]).powi(2))
                .sqrt();
            prev = q;
        }
        sum
    };
    let steps: Vec<f64> = (0..n).map(step_arc).collect();
    let mean = steps.iter().sum::<f64>() / steps.len() as f64;
    for (i, s) in steps.iter().enumerate() {
        assert!(
            (s - mean).abs() < 0.02 * mean,
            "step {i}: arc {s} deviates from mean {mean}"
        );
    }
    // And the quick approximation demonstrably does NOT have this
    // property on this path (it is the labeled heuristic, not the truth).
    let quick_first = path.quick_point_from_proportion(0.5).unwrap();
    let true_first = path.point_from_proportion_with(&table, 0.5).unwrap();
    assert!(
        (quick_first[0] - true_first[0]).abs() > 1.0,
        "quick and true proportions should diverge on uneven paths"
    );
}

#[test]
fn cache_rebuilds_on_geometry_revision_only() {
    let mut path = QuadPath::new();
    path.set_points_as_corners(&[p(0.0, 0.0), p(1.0, 0.0), p(2.0, 1.0)])
        .unwrap();
    let mut cache = CachedArcLength::new();

    // Geometry revision 1: first build.
    let total_before = cache.get(1, &path).total();
    assert_eq!(cache.rebuilds(), 1);
    // Transform/style revisions do not touch the geometry key: no rebuild.
    let _ = cache.get(1, &path);
    let _ = cache.get(1, &path);
    assert_eq!(cache.rebuilds(), 1);

    // A geometry edit bumps the revision: rebuild reflects new lengths.
    path.add_line_to(p(2.0, 5.0), true).unwrap();
    let total_after = cache.get(2, &path).total();
    assert_eq!(cache.rebuilds(), 2);
    assert!(total_after > total_before);
}

#[test]
fn degenerate_paths() {
    // Empty path.
    let empty = QuadPath::new();
    assert_eq!(empty.get_arc_length(), 0.0);
    assert_eq!(empty.point_from_proportion(0.5), None);
    // Single point: proportion returns the point.
    let point = QuadPath::from_points(vec![p(1.0, 2.0)]).unwrap();
    assert_eq!(point.get_arc_length(), 0.0);
    assert_eq!(point.point_from_proportion(0.7), Some(p(1.0, 2.0)));
    // Path with a null curve: zero-length curve contributes nothing and
    // never traps the inverse.
    let mut path = QuadPath::new();
    path.start_new_path(p(0.0, 0.0));
    path.add_line_to(p(0.0, 0.0), true).unwrap();
    path.add_line_to(p(4.0, 0.0), true).unwrap();
    assert!((path.get_arc_length() - 4.0).abs() < 1e-12);
    let mid = path.point_from_proportion(0.5).unwrap();
    assert!((mid[0] - 2.0).abs() < 1e-9);
}

#[test]
fn compound_paths_measure_only_drawn_contours() {
    let mut path = QuadPath::new();
    path.start_new_path(p(0.0, 0.0));
    path.add_line_to(p(1.0, 0.0), true).unwrap();
    path.start_new_path(p(10.0, 0.0));
    path.add_line_to(p(11.0, 0.0), true).unwrap();
    let table = ArcLengthTable::for_path(&path);

    // The nine-unit move between contours is a pen-up instruction, not a
    // third line. Keeping its zero slot preserves the public curve indices.
    assert_eq!(table.curve_lengths(), &[1.0, 0.0, 1.0]);
    assert_eq!(table.total(), 2.0);
    assert_eq!(path.get_arc_length(), 2.0);
    for (alpha, expected) in [
        (0.0, p(0.0, 0.0)),
        (0.25, p(0.5, 0.0)),
        (0.5, p(10.0, 0.0)),
        (0.75, p(10.5, 0.0)),
        (1.0, p(11.0, 0.0)),
    ] {
        assert_eq!(
            path.point_from_proportion_with(&table, alpha),
            Some(expected)
        );
    }
    assert_eq!(table.curve_and_t_at(&path, 0.25), Some((0, 0.5)));
    assert_eq!(table.curve_and_t_at(&path, 0.5), Some((2, 0.0)));
    assert_eq!(table.curve_and_t_at(&path, 0.75), Some((2, 0.5)));
}

#[test]
fn contour_gap_size_cannot_change_arc_length_or_pacing() {
    for gap in [0.0, 0.25, 100.0, 1e6] {
        let mut path = QuadPath::new();
        path.start_new_path(p(0.0, 0.0));
        path.add_line_to(p(2.0, 0.0), true).unwrap();
        path.start_new_path(p(2.0 + gap, 0.0));
        path.add_line_to(p(2.0 + gap, 6.0), true).unwrap();
        let table = ArcLengthTable::for_path(&path);
        assert_eq!(table.total(), 8.0, "gap {gap}");
        for (alpha, expected) in [
            (0.125, p(1.0, 0.0)),
            (0.25, p(2.0 + gap, 0.0)),
            (0.5, p(2.0 + gap, 2.0)),
            (0.75, p(2.0 + gap, 4.0)),
        ] {
            let actual = path.point_from_proportion_with(&table, alpha).unwrap();
            for (actual, expected) in actual.into_iter().zip(expected) {
                assert!(
                    (actual - expected).abs() <= 4.0 * f64::EPSILON * expected.abs().max(1.0),
                    "gap {gap}, alpha {alpha}: {actual} versus {expected}"
                );
            }
        }
    }
}

#[test]
fn compound_path_endpoints_and_point_only_contours_are_defined() {
    let mut path = QuadPath::new();
    path.start_new_path(p(-5.0, 2.0));
    path.start_new_path(p(0.0, 0.0));
    path.add_line_to(p(0.0, 0.0), true).unwrap();
    path.add_line_to(p(4.0, 0.0), true).unwrap();
    path.start_new_path(p(9.0, 3.0));
    let table = ArcLengthTable::for_path(&path);
    assert_eq!(table.total(), 4.0);
    for (alpha, expected) in [
        (-1.0, p(-5.0, 2.0)),
        (0.0, p(-5.0, 2.0)),
        (0.5, p(2.0, 0.0)),
        (1.0, p(9.0, 3.0)),
        (2.0, p(9.0, 3.0)),
    ] {
        assert_eq!(
            path.point_from_proportion_with(&table, alpha),
            Some(expected)
        );
    }

    let mut points_only = QuadPath::new();
    points_only.start_new_path(p(1.0, 2.0));
    points_only.start_new_path(p(7.0, 9.0));
    points_only.start_new_path(p(-3.0, 4.0));
    assert_eq!(points_only.get_arc_length(), 0.0);
    // BN-07: a nonempty path of zero length always returns its start point.
    for alpha in [0.0, 0.25, 0.5, 1.0] {
        assert_eq!(points_only.point_from_proportion(alpha), Some(p(1.0, 2.0)));
    }
}

#[test]
fn stationary_handle_without_a_contour_break_keeps_its_length() {
    // The topology law disambiguates very short stationary handles from
    // contour breaks. Arc length must use that same law, not `anchor == handle`.
    let path = QuadPath::from_points(vec![p(0.0, 0.0), p(0.0, 0.0), p(1e-5, 0.0)]).unwrap();
    assert_eq!(path.subpath_end_indices(), vec![2]);
    let table = ArcLengthTable::for_path(&path);
    assert_eq!(table.curve_lengths(), &[1e-5]);
    assert_eq!(table.total(), 1e-5);
    let midpoint = path.point_from_proportion(0.5).unwrap();
    assert!((midpoint[0] - 0.5e-5).abs() < 1e-18);
}
