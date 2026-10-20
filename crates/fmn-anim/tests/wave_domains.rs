//! Collapsed wave domains stay finite without changing ordinary wave formulas.
use fmn_anim::{Animation, RateFunc, apply_wave};
use fmn_core::rate;
use fmn_mobject::{Mobject, Stage};

#[test]
fn zero_width_wave_has_finite_midpoint_phase() {
    let mut stage = Stage::new();
    let points = [[2.0, -1.0, 0.0], [2.0, 0.0, 0.0], [2.0, 1.0, 0.0]];
    let mob = stage.add(Mobject::vector_from_points(&points));
    let mut wave = apply_wave(&stage, mob, [0.0, 0.0, 1.0], 0.5);
    wave.state_mut().config.rate_func = RateFunc::linear();
    wave.begin(&mut stage).unwrap();
    for alpha in [0.5, 0.2, 0.8, 0.0, 1.0] {
        wave.interpolate(&mut stage, alpha);
        for (actual, start) in stage.get_points(mob).unwrap().iter().zip(points) {
            assert!(actual.iter().all(|v| v.is_finite()));
            assert_eq!(actual[0], start[0]);
            assert_eq!(actual[1], start[1]);
            assert!((actual[2] - 0.5 * rate::there_and_back(alpha)).abs() < 1e-7);
        }
    }
    wave.finish(&mut stage);
}

#[test]
fn ordinary_wave_matches_its_original_formula_under_repeated_scrubbing() {
    let mut stage = Stage::new();
    let points = [[-1.0, 0.0, 0.0], [0.0, 0.5, 0.25], [1.0, 0.0, 0.5]];
    let mob = stage.add(Mobject::vector_from_points(&points));
    let mut wave = apply_wave(&stage, mob, [0.0, 1.0, 0.5], 0.25);
    wave.state_mut().config.rate_func = RateFunc::linear();
    wave.begin(&mut stage).unwrap();
    for alpha in [0.25, 0.75, 0.25, 0.5, 0.0, 1.0] {
        wave.interpolate(&mut stage, alpha);
        for (actual, point) in stage.get_points(mob).unwrap().iter().zip(points) {
            let phase = (point[0] + 1.0) / 2.0;
            let power = fmn_dmath::exp(2.0 * (phase - 0.5));
            let nudge = rate::there_and_back(fmn_dmath::pow(alpha, power));
            let expected = [point[0], point[1] + nudge * 0.25, point[2] + nudge * 0.125];
            for (a, b) in actual.iter().zip(expected) {
                assert!((*a - b).abs() < 1e-7);
            }
        }
    }
    wave.finish(&mut stage);
}
