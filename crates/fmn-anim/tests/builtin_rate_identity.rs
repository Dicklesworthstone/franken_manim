//! Built-in animations know their rate by construction; never rediscover it.
use fmn_anim::{
    Animation, DrawBorderThenFill, RateFunc, circle_indicate, clock_passes_time, indicate,
    rate_tag, v_fade_in_then_out,
};
use fmn_core::rate::CatalogRate;
use fmn_mobject::{Mobject, Stage};

fn assert_catalog(animation: &dyn Animation, expected: CatalogRate) {
    let rate = &animation.state().config.rate_func;
    assert!(matches!(rate, RateFunc::Catalog(value) if *value == expected));
    assert_eq!(rate_tag(&rate.clone()), Some(expected.tag()));
    for sample in 0..=1024 {
        let alpha = f64::from(sample) / 1024.0;
        assert_eq!(
            rate.eval(alpha).to_bits(),
            expected.function()(alpha).to_bits()
        );
    }
}

#[test]
fn built_in_effects_keep_their_named_rate_through_construction() {
    let mut stage = Stage::new();
    let mob = stage.add(Mobject::from_points(&[[0.0, 0.0, 0.0]]));
    let other = stage.add(Mobject::from_points(&[[0.0, 0.5, 0.0]]));
    assert_catalog(
        &indicate(&mut stage, mob, 1.2, None).unwrap(),
        CatalogRate::ThereAndBack,
    );
    assert_catalog(
        &circle_indicate(mob, other, true),
        CatalogRate::ThereAndBack,
    );
    assert_catalog(&v_fade_in_then_out(mob), CatalogRate::ThereAndBack);
    assert_catalog(&DrawBorderThenFill::new(mob), CatalogRate::DoubleSmooth);
    let clock = stage.add(Mobject::from_points(&[[0.0; 3]]));
    let group = clock_passes_time(&mut stage, clock, mob, other, 5.0, 12.0).unwrap();
    assert_catalog(&group, CatalogRate::Linear);
    for member in group.animations() {
        assert_catalog(member.as_ref(), CatalogRate::Linear);
    }
}
