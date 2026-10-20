//! Paint readback and regeneration across adopted native formula lifecycles.
use fmn_anim::{Animation, RationalFrameClock, play_segment};
use fmn_core::constants::{BLACK, BLUE, LEFT, RED};
use fmn_core::rng::RngRoot;
use fmn_library::numbers::change_decimal_to_value;
use fmn_library::{DecimalNumber, FontBook, Tex, TexEngine, TexSpanBinding, VStyle};
use fmn_mobject::{Mob, Stage};

fn fixture(stage: &mut Stage) -> (TexSpanBinding, Mob, DecimalNumber) {
    let engine = TexEngine::new("fmd-math/pack/default", None).unwrap();
    let built = Tex::new("v=12.0").build(&engine).unwrap();
    let spans = built.span_map();
    let root = stage.add(built);
    stage.add_to_scene(root).unwrap();
    let mut binding = TexSpanBinding::new(stage, root, spans).unwrap();
    for &part in &binding.occurrences(stage, "12.0").unwrap()[0] {
        stage.set_fill(part, Some(BLUE), Some(0.625), Some(0.75), false);
        stage.set_stroke(part, Some(RED), Some(2.5), Some(0.375), None, false);
    }
    let number = binding
        .make_number_changeable(
            stage,
            "12.0",
            0,
            DecimalNumber::new(0.0)
                .num_decimal_places(1)
                .group_with_commas(false)
                .include_background_rectangle(true),
            &FontBook::bundled().unwrap(),
        )
        .unwrap();
    let (slot, model) = number.into_parts();
    (binding, slot, model)
}

fn assert_paint(stage: &Stage, slot: Mob, separate_root: bool) {
    assert!(stage.get(slot).unwrap().buffer.is_empty());
    assert_eq!(
        stage.get_fill_color(slot).unwrap().to_rgb8(),
        if separate_root { RED } else { BLUE }.to_rgb8(),
    );
    assert_eq!(
        stage.get_fill_opacity(slot),
        Some(if separate_root { 0.25 } else { 0.625 }),
    );
    assert_eq!(stage.get_stroke_width(slot), Some(2.5));
    assert!(stage.has_fill(slot));
    assert!(stage.has_stroke(slot));
    assert_eq!(stage.get_color(slot), stage.get_fill_color(slot));
    let children = stage.get(slot).unwrap().submobjects();
    assert!(children.len() > 1);
    assert_eq!(
        stage.get_fill_color(children[0]).unwrap().to_rgb8(),
        BLACK.to_rgb8()
    );
    assert_eq!(stage.get_fill_opacity(children[0]), Some(1.0));
    for &digit in &children[1..] {
        assert_eq!(
            stage.get_fill_color(digit).unwrap().to_rgb8(),
            BLUE.to_rgb8()
        );
        assert_eq!(stage.get_fill_opacity(digit), Some(0.625));
        assert_eq!(
            stage.get_stroke_color(digit).unwrap().to_rgb8(),
            RED.to_rgb8()
        );
        assert_eq!(stage.get_stroke_width(digit), Some(2.5));
        assert_eq!(stage.get_stroke_opacity(digit), Some(0.375));
        let borders = stage
            .get(digit)
            .unwrap()
            .buffer
            .read_column("fill_border_width")
            .unwrap();
        assert!(borders.iter().all(|&width| width == 0.75));
    }
}

#[test]
fn numeric_root_and_digit_paint_survive_normal_and_skipped_regeneration() {
    for skip in [false, true] {
        for separate_root in [false, true] {
            let mut stage = Stage::new();
            let (binding, slot, mut model) = fixture(&mut stage);
            if separate_root {
                stage.set_fill(slot, Some(RED), Some(0.25), None, false);
            }
            let mut clock = RationalFrameClock::new(8).unwrap();
            let before_queries = stage.snapshot_bytes().unwrap();
            assert_paint(&stage, slot, separate_root);
            assert_eq!(stage.snapshot_bytes().unwrap(), before_queries);
            let anchor = stage.get_bounding_box_point(slot, LEFT);
            for target in [1234.5, -7.0, 0.0] {
                let mut animation = change_decimal_to_value(
                    slot,
                    model.clone(),
                    FontBook::bundled().unwrap(),
                    target,
                )
                .unwrap();
                animation.state_mut().config.run_time = 0.5;
                let mut animations: Vec<Box<dyn Animation>> = vec![Box::new(animation)];
                let mut captured = 0;
                play_segment(
                    &mut stage,
                    &mut clock,
                    &RngRoot::from_seed(7),
                    &mut animations,
                    skip,
                    &mut |packet| {
                        captured += 1;
                        assert_paint(&packet.materialize_stage(), slot, separate_root);
                    },
                )
                .unwrap();
                assert_eq!(captured, if skip { 0 } else { 4 });
                assert!(animations[0].deferred_error().is_none());
                assert_paint(&stage, slot, separate_root);
                assert_eq!(stage.roots(), &[binding.mobject()]);
                assert_eq!(
                    binding.occurrences(&stage, r"\decimalmob").unwrap(),
                    [vec![slot]]
                );
                for (actual, expected) in stage
                    .get_bounding_box_point(slot, LEFT)
                    .into_iter()
                    .zip(anchor)
                {
                    assert!((actual - expected).abs() < 2e-6);
                }
                model
                    .set_value(target, &FontBook::bundled().unwrap())
                    .unwrap();
            }
            assert_eq!(clock.now().frames(), 12);
        }
    }
}

#[test]
fn copied_and_restored_numeric_roots_keep_their_own_style_records() {
    let mut stage = Stage::new();
    let (binding, slot, model) = fixture(&mut stage);
    let copied = stage.copy_family_mapped(binding.mobject()).unwrap();
    let copied_binding = binding.for_copy(&stage, &copied).unwrap();
    let copied_slot = copied.get(slot).unwrap();
    let original = stage.snapshot();
    stage.set_fill(copied_slot, Some(RED), Some(0.25), None, false);
    let mut animation =
        change_decimal_to_value(copied_slot, model, FontBook::bundled().unwrap(), 987.6).unwrap();
    animation.begin(&mut stage).unwrap();
    animation.finish(&mut stage);
    assert!(animation.deferred_error().is_none());
    assert_paint(&stage, copied_slot, true);
    assert_paint(&stage, slot, false);
    assert_eq!(
        copied_binding.occurrences(&stage, r"\decimalmob").unwrap(),
        [vec![copied_slot]]
    );
    stage.restore(&original);
    assert_paint(&stage, copied_slot, false);
    assert_paint(&stage, slot, false);
}
