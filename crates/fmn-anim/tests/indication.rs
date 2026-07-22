//! Indication-family semantics (fm-cye, §9.4 family 4): Indicate's
//! there-and-back swell, TurnInsideOut's reversal under the C-1 ruling,
//! WiggleOutThenIn's absolute pose, the VShowPassingFlash gaussian
//! window with style restore, and ApplyWave's phased nudge.

use fmn_anim::animation::Animation;
use fmn_anim::{
    INDICATION_YELLOW, RateFunc, VShowPassingFlash, WiggleOutThenIn, apply_wave, indicate,
    show_creation_then_destruction, turn_inside_out,
};
use fmn_mobject::record::{RecordBuffer, RecordSchema};
use fmn_mobject::{Mob, Mobject, Stage};

fn vmob(stage: &mut Stage, points: &[[f64; 3]]) -> Mob {
    let mob = stage.add(Mobject::new());
    let entry = stage.get_mut(mob).unwrap();
    entry.buffer = RecordBuffer::new(RecordSchema::vmobject(), points.len());
    #[allow(clippy::cast_possible_truncation)]
    let flat: Vec<f32> = points
        .iter()
        .flat_map(|p| p.iter().map(|v| *v as f32))
        .collect();
    entry.buffer.write_range("point", 0, &flat);
    entry
        .buffer
        .write_range("fill_rgba", 0, &[0.2, 0.4, 0.6, 1.0].repeat(points.len()));
    entry
        .buffer
        .write_range("stroke_rgba", 0, &[1.0, 0.0, 0.0, 0.8].repeat(points.len()));
    entry
        .buffer
        .write_range("stroke_width", 0, &vec![2.0; points.len()]);
    mob
}

/// A 2-curve straight-line vmobject with x = [0, 1, 2, 3, 4].
fn line5(stage: &mut Stage) -> Mob {
    vmob(
        stage,
        &[
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [3.0, 0.0, 0.0],
            [4.0, 0.0, 0.0],
        ],
    )
}

fn coords(stage: &Stage, mob: Mob, lane: usize) -> Vec<f32> {
    stage
        .get(mob)
        .unwrap()
        .buffer
        .read_column("point")
        .unwrap()
        .as_chunks::<3>()
        .0
        .iter()
        .map(|c| c[lane])
        .collect()
}

fn column(stage: &Stage, mob: Mob, field: &str) -> Vec<f32> {
    stage.get(mob).unwrap().buffer.read_column(field).unwrap()
}

fn assert_close(actual: &[f32], expected: &[f32], tol: f32) {
    assert_eq!(actual.len(), expected.len());
    for (a, e) in actual.iter().zip(expected) {
        assert!((a - e).abs() <= tol, "{actual:?} !~ {expected:?}");
    }
}

#[test]
fn indicate_swells_flushes_and_returns() {
    let mut stage = Stage::new();
    let mob = line5(&mut stage);
    let mut anim = indicate(&mut stage, mob, 1.2, None).unwrap();
    anim.begin(&mut stage).unwrap();
    // there_and_back peaks at α = 0.5: fully at the scaled, yellow target.
    anim.interpolate(&mut stage, 0.5);
    assert_close(&coords(&stage, mob, 0), &[-0.4, 0.8, 2.0, 3.2, 4.4], 1e-4);
    let fill = column(&stage, mob, "fill_rgba");
    assert_close(&fill[..3], &INDICATION_YELLOW, 1e-6);
    // And back: the finish state is the original.
    anim.finish(&mut stage);
    assert_close(&coords(&stage, mob, 0), &[0.0, 1.0, 2.0, 3.0, 4.0], 1e-4);
    assert_close(
        &column(&stage, mob, "fill_rgba")[..3],
        &[0.2, 0.4, 0.6],
        1e-5,
    );
}

#[test]
fn turn_inside_out_lands_on_the_reversed_run() {
    let mut stage = Stage::new();
    let mob = line5(&mut stage);
    stage
        .get_mut(mob)
        .unwrap()
        .buffer
        .write_range("stroke_width", 0, &[1.0, 2.0, 3.0, 4.0, 5.0]);
    let mut anim = turn_inside_out(&mut stage, mob, std::f64::consts::FRAC_PI_2).unwrap();
    anim.begin(&mut stage).unwrap();
    anim.finish(&mut stage);
    // data[::-1]: points and their row-mates travel together.
    assert_close(&coords(&stage, mob, 0), &[4.0, 3.0, 2.0, 1.0, 0.0], 1e-5);
    assert_close(
        &column(&stage, mob, "stroke_width"),
        &[5.0, 4.0, 3.0, 2.0, 1.0],
        1e-5,
    );
}

#[test]
fn wiggle_is_an_absolute_pose_and_returns_home() {
    let mut stage = Stage::new();
    let mob = line5(&mut stage);
    let original = coords(&stage, mob, 0);
    let mut anim = WiggleOutThenIn::new(mob);
    assert_eq!(anim.state().config.run_time, 2.0);
    anim.begin(&mut stage).unwrap();
    anim.interpolate(&mut stage, 0.3);
    let pose = coords(&stage, mob, 0);
    anim.interpolate(&mut stage, 0.3);
    assert_close(&coords(&stage, mob, 0), &pose, 1e-6);
    anim.finish(&mut stage);
    assert_close(&coords(&stage, mob, 0), &original, 1e-4);
}

#[test]
fn v_show_passing_flash_windows_widths_then_restores_style() {
    let mut stage = Stage::new();
    let mob = line5(&mut stage);
    let mut anim = VShowPassingFlash::new(mob)
        .with_time_width(0.3)
        .with_taper_width(0.0);
    anim.state_mut().config.rate_func = RateFunc::linear();
    assert!(anim.is_remover());
    anim.begin(&mut stage).unwrap();
    // μ at α=0.5 is 0.5: full width at the middle point, zero outside 3σ.
    anim.interpolate(&mut stage, 0.5);
    let widths = column(&stage, mob, "stroke_width");
    assert!((widths[2] - 2.0).abs() < 1e-5, "peak keeps its width");
    assert_eq!(widths[0], 0.0, "outside the 3σ support");
    assert_eq!(widths[4], 0.0);
    // finish restores every member's style from the start.
    anim.finish(&mut stage);
    assert_close(&column(&stage, mob, "stroke_width"), &[2.0; 5], 1e-6);
}

#[test]
fn show_creation_then_destruction_is_the_wide_flash() {
    let mut stage = Stage::new();
    let mob = line5(&mut stage);
    let anim = show_creation_then_destruction(mob);
    assert_eq!(anim.state().config.name, "ShowCreationThenDestruction");
    assert!(anim.is_remover());
}

#[test]
fn apply_wave_nudges_mid_animation_and_settles() {
    let mut stage = Stage::new();
    let mob = line5(&mut stage);
    let mut anim = apply_wave(&stage, mob, [0.0, 1.0, 0.0], 0.2);
    assert_eq!(anim.state().config.run_time, 1.0);
    anim.state_mut().config.rate_func = RateFunc::linear();
    anim.begin(&mut stage).unwrap();
    anim.interpolate(&mut stage, 0.5);
    assert!(
        coords(&stage, mob, 1).iter().any(|&y| y.abs() > 1e-3),
        "the wave displaces interior points"
    );
    anim.finish(&mut stage);
    assert_close(&coords(&stage, mob, 1), &[0.0; 5], 1e-4);
}
