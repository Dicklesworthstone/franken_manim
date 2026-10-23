//! Dot radii are world-space records. Native class-aware scale animations
//! update those records; generic point maps and Group scaling do not.

use fmn_anim::{
    Animation, RateFunc, fade_in, fade_out, grow_from_center, indicate, prepare_animation,
    scale_in_place,
};
use fmn_core::rate;
use fmn_mobject::animate::AnimateArgs;
use fmn_mobject::record::{RecordBuffer, RecordSchema};
use fmn_mobject::{Mob, Mobject, RenderPrimitive, Stage};

fn dots(stage: &mut Stage) -> Mob {
    let schema = RecordSchema::new(
        &[("point", 3), ("radius", 1), ("rgba", 4)],
        &["point"],
        &["point"],
    )
    .unwrap();
    let mut buffer = RecordBuffer::new(schema, 2).unwrap();
    buffer.write_range("point", 0, &[-1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
    buffer.write_range("radius", 0, &[0.25, 0.5]);
    buffer.write_range("rgba", 0, &[0.0, 0.0, 1.0, 1.0].repeat(2));
    stage.add(Mobject::from_buffer(buffer).with_render_primitive(RenderPrimitive::DotCloud))
}

fn radii(stage: &Stage, mob: Mob) -> Vec<f32> {
    stage
        .get(mob)
        .unwrap()
        .buffer
        .read_column("radius")
        .unwrap()
}

#[test]
fn native_dot_scale_interpolates_radii_without_compounding_after_a_bake() {
    for explicit_pivot in [false, true] {
        let mut stage = Stage::new();
        let mob = dots(&mut stage);
        let builder = mob
            .animate()
            .set_anim_args(AnimateArgs {
                rate_func: Some(rate::linear),
                ..AnimateArgs::default()
            })
            .unwrap();
        let builder = if explicit_pivot {
            builder.scale_about(2.0, Some([0.0; 3]), None)
        } else {
            builder.scale(2.0)
        }
        .unwrap();
        let mut animation = prepare_animation(builder, &mut stage).unwrap();
        animation.begin(&mut stage).unwrap();
        assert_eq!(radii(&stage, mob), [0.25, 0.5]);
        animation.interpolate(&mut stage, 0.5);
        assert_eq!(radii(&stage, mob), [0.375, 0.75]);
        stage.bake_placement(mob).unwrap();
        animation.interpolate(&mut stage, 0.75);
        assert_eq!(radii(&stage, mob), [0.4375, 0.875]);
        animation.finish(&mut stage);
        assert_eq!(radii(&stage, mob), [0.5, 1.0]);
        assert_eq!(
            stage.get_points(mob).unwrap(),
            [[-2.0, 0.0, 0.0], [2.0, 0.0, 0.0]]
        );
    }
}

#[test]
fn native_grow_and_scale_target_recipes_include_radius_records() {
    let mut stage = Stage::new();
    let mob = dots(&mut stage);
    let mut grow = grow_from_center(&mut stage, mob, None).unwrap();
    grow.state_mut().config.rate_func = RateFunc::linear();
    grow.begin(&mut stage).unwrap();
    assert_eq!(radii(&stage, mob), [0.0, 0.0]);
    grow.interpolate(&mut stage, 0.5);
    assert_eq!(radii(&stage, mob), [0.125, 0.25]);
    grow.finish(&mut stage);
    assert_eq!(radii(&stage, mob), [0.25, 0.5]);

    let mut enlarge = scale_in_place(&mut stage, mob, 2.0).unwrap();
    enlarge.state_mut().config.rate_func = RateFunc::linear();
    enlarge.begin(&mut stage).unwrap();
    enlarge.interpolate(&mut stage, 0.5);
    assert_eq!(radii(&stage, mob), [0.375, 0.75]);
    enlarge.finish(&mut stage);
    assert_eq!(radii(&stage, mob), [0.5, 1.0]);

    // Replaying grow samples the newly enlarged records at begin.
    grow.begin(&mut stage).unwrap();
    assert_eq!(radii(&stage, mob), [0.0, 0.0]);
    grow.interpolate(&mut stage, 0.5);
    assert_eq!(radii(&stage, mob), [0.25, 0.5]);
    grow.finish(&mut stage);
    assert_eq!(radii(&stage, mob), [0.5, 1.0]);

    let mut fade = fade_in(&mut stage, mob, [0.0; 3], 2.0).unwrap();
    fade.state_mut().config.rate_func = RateFunc::linear();
    fade.begin(&mut stage).unwrap();
    assert_eq!(radii(&stage, mob), [0.25, 0.5]);
    fade.finish(&mut stage);

    let mut vanish = fade_out(&mut stage, mob, [0.0; 3], 0.0).unwrap();
    vanish.state_mut().config.rate_func = RateFunc::linear();
    vanish.begin(&mut stage).unwrap();
    vanish.interpolate(&mut stage, 0.5);
    assert_eq!(radii(&stage, mob), [0.25, 0.5]);
    vanish.interpolate(&mut stage, 1.0);
    assert_eq!(radii(&stage, mob), [0.0, 0.0]);
    vanish.finish(&mut stage);
    assert_eq!(radii(&stage, mob), [0.5, 1.0]);

    let mut pulse = indicate(&mut stage, mob, 2.0, None).unwrap();
    pulse.begin(&mut stage).unwrap();
    pulse.interpolate(&mut stage, 0.5);
    assert_eq!(radii(&stage, mob), [1.0, 2.0]);
    pulse.finish(&mut stage);
    assert_eq!(radii(&stage, mob), [0.5, 1.0]);
}

#[test]
fn group_scale_and_generic_point_maps_preserve_child_dot_radii() {
    let mut stage = Stage::new();
    let mob = dots(&mut stage);
    stage.scale(mob, 2.0).stretch(mob, 0.5, 0);
    assert_eq!(radii(&stage, mob), [0.25, 0.5]);
    let group = stage.add(Mobject::new());
    stage.attach(group, mob).unwrap();
    let builder = group.animate().scale(2.0).unwrap();
    let mut animation = prepare_animation(builder, &mut stage).unwrap();
    animation.begin(&mut stage).unwrap();
    animation.finish(&mut stage);
    assert_eq!(radii(&stage, mob), [0.25, 0.5]);
    assert_eq!(
        stage.get_points(mob).unwrap(),
        [[-2.0, 0.0, 0.0], [2.0, 0.0, 0.0]]
    );
}

#[test]
fn dot_radius_scaling_does_not_hide_invalid_values_or_clamp_zero() {
    for (factor, expected) in [(0.0, 0.0), (-2.0, -0.5), (f64::INFINITY, f32::INFINITY)] {
        let mut stage = Stage::new();
        let mob = dots(&mut stage);
        let target = mob
            .animate()
            .scale(factor)
            .unwrap()
            .build(&mut stage)
            .unwrap()
            .target;
        assert_eq!(radii(&stage, target)[0], expected);
        assert_eq!(radii(&stage, mob), [0.25, 0.5]);
    }
    let mut stage = Stage::new();
    let mob = dots(&mut stage);
    let target = mob
        .animate()
        .scale(f64::NAN)
        .unwrap()
        .build(&mut stage)
        .unwrap()
        .target;
    assert!(radii(&stage, target).iter().all(|radius| radius.is_nan()));
}
