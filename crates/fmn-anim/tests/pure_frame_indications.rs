//! Cross-fades and indications must be absolute in alpha on native workers.
use fmn_anim::pure_frame::{PureFrameRefusal, PureSegment};
use fmn_anim::{
    AnimConfig, Animation, AnimationGroup, RateFunc, RationalFrameClock, VShowPassingFlash,
    WiggleOutThenIn, advance_play, fade_transform, fade_transform_pieces, finish_open_play,
    open_play,
};
use fmn_core::{rate::CatalogRate, rng::RngRoot};
use fmn_mobject::{Mob, Mobject, Stage};

fn family(stage: &mut Stage, x: f64) -> Mob {
    let root = stage.add(Mobject::vector_from_points(&[]));
    for offset in [0.0, 1.7] {
        let child = stage.add(Mobject::vector_from_points(&[
            [x + offset, 0.0, 0.0],
            [x + offset + 0.2, 0.8, 0.0],
            [x + offset + 0.9, 0.3, 0.0],
            [x + offset + 0.5, -0.4, 0.0],
            [x + offset, 0.0, 0.0],
        ]));
        let e = stage.get_mut(child).unwrap();
        e.uniforms_mut().shading = [0.13, 0.27, 0.41];
        e.buffer
            .write_range("fill_rgba", 0, &[0.2, 0.6, 0.9, 0.7].repeat(5));
        e.buffer
            .write_range("stroke_rgba", 0, &[0.9, 0.4, 0.1, 0.8].repeat(5));
        e.buffer
            .write_range("stroke_width", 0, &[2.0, 3.0, 5.0, 3.0, 2.0]);
        stage.attach(root, child).unwrap();
    }
    stage.shift(root, [0.13, -0.27, 0.1]);
    stage.add_to_scene(root).unwrap();
    root
}
fn scene(
    kind: usize,
    rate: RateFunc,
    nested: bool,
    pivots: bool,
) -> (Stage, Vec<Box<dyn Animation>>) {
    let mut stage = Stage::new();
    let root = family(&mut stage, -1.0);
    let config = AnimConfig {
        run_time: 0.5,
        lag_ratio: 0.17,
        rate_func: if nested {
            RateFunc::linear()
        } else {
            rate.clone()
        },
        ..AnimConfig::default()
    };
    let animation: Box<dyn Animation> = match kind {
        0 | 1 => {
            let target = family(&mut stage, 2.3);
            stage.scale(target, 0.7);
            stage.remove_from_scene(target);
            let fade = if kind == 0 {
                fade_transform(&mut stage, root, target)
            } else {
                fade_transform_pieces(&mut stage, root, target)
            }
            .unwrap();
            Box::new(fade.with_config(config))
        }
        2 => {
            let mut wiggle = WiggleOutThenIn::new(root)
                .with_scale_value(1.4)
                .with_rotation_angle(0.6)
                .with_n_wiggles(3.0)
                .with_config(config);
            if pivots {
                wiggle = wiggle
                    .with_scale_about_point([0.3, -0.4, 0.0])
                    .with_rotate_about_point([-0.1, 0.8, 0.0]);
            }
            Box::new(wiggle)
        }
        _ => Box::new(
            VShowPassingFlash::new(root)
                .with_time_width(0.6)
                .with_taper_width(0.2)
                .with_config(config),
        ),
    };
    let animation: Box<dyn Animation> = if nested {
        Box::new(
            AnimationGroup::new(&mut stage, vec![animation])
                .unwrap()
                .with_rate_func(rate),
        )
    } else {
        animation
    };
    for root in stage.roots().to_vec() {
        stage.remove_from_scene(root);
    }
    stage.add_to_scene(animation.state().mobject()).unwrap();
    (stage, vec![animation])
}
fn facts(stage: &Stage) -> Vec<String> {
    let mut out = vec![format!("time:{:x}", stage.time().to_bits())];
    for &root in stage.roots() {
        let family = stage.family(root);
        out.push(format!("family:{}", family.len()));
        for mob in family {
            let e = stage.get(mob).unwrap();
            out.push(format!(
                "{:?}/{:?}/{:?}",
                e.placement().coefficients().map(f64::to_bits),
                e.uniforms(),
                (stage.is_animating(mob), stage.is_updating_suspended(mob))
            ));
            for field in e.buffer.schema().fields() {
                let bits: Vec<_> = e
                    .buffer
                    .read_column(&field.name)
                    .unwrap()
                    .iter()
                    .map(|x| x.to_bits())
                    .collect();
                out.push(format!("{}:{bits:?}", field.name));
            }
        }
    }
    out
}
fn compare(kind: usize, rate: RateFunc, nested: bool, pivots: bool) {
    let (mut serial, mut a) = scene(kind, rate.clone(), nested, pivots);
    let (mut owner, mut b) = scene(kind, rate, nested, pivots);
    let mut clock = RationalFrameClock::new(32).unwrap();
    let mut owner_clock = clock;
    let rng = RngRoot::from_seed(719);
    let mut open = open_play(&mut serial, &clock, &mut a).unwrap();
    let owner_open = open_play(&mut owner, &owner_clock, &mut b).unwrap();
    let program = PureSegment::freeze(&owner, &owner_clock, &rng, &b, &owner_open).unwrap();
    let mut expected = Vec::new();
    advance_play(
        &mut serial,
        &mut clock,
        &rng,
        &mut a,
        &mut open,
        program.frame_count(),
        &mut |packet| expected.push(facts(&packet.materialize_stage())),
    )
    .unwrap();
    for workers in [1, 4, 16] {
        std::thread::scope(|scope| {
            for worker in 0..workers {
                let program = &program;
                let expected = &expected;
                scope.spawn(move || {
                    for index in (worker..expected.len()).step_by(workers).rev() {
                        let actual = facts(&program.job(index as i64 + 1).unwrap().materialize());
                        assert_eq!(actual.len(), expected[index].len());
                        for (field, (actual, expected)) in actual.iter().zip(&expected[index]).enumerate() {
                            assert_eq!(actual, expected,
                                "kind={kind}, nested={nested}, pivots={pivots}, frame={index}, field={field}, workers={workers}");
                        }
                    }
                });
            }
        });
    }
    finish_open_play(&mut serial, &mut a, open).unwrap();
    program
        .finish_on_owner(&mut owner, &mut owner_clock, &mut b, owner_open)
        .unwrap();
    assert_eq!(facts(&owner), facts(&serial));
}
#[test]
fn fade_transform_and_pieces_keep_ghosts_cleanup_and_every_frame() {
    for kind in 0..2 {
        for nested in [false, true] {
            for rate in [
                RateFunc::linear(),
                RateFunc::Catalog(CatalogRate::ThereAndBack),
            ] {
                compare(kind, rate, nested, false);
            }
        }
    }
}
#[test]
fn wiggles_keep_implicit_and_explicit_pivots_across_workers() {
    for pivots in [false, true] {
        for nested in [false, true] {
            compare(2, RateFunc::linear(), nested, pivots);
        }
    }
}
#[test]
fn passing_flash_keeps_tapered_widths_and_cleanup_across_workers() {
    for nested in [false, true] {
        compare(
            3,
            RateFunc::Catalog(CatalogRate::ThereAndBack),
            nested,
            false,
        );
    }
}
#[test]
fn arbitrary_indication_rates_are_not_transferred_to_workers() {
    fn custom(alpha: f64) -> f64 {
        alpha * alpha
    }
    for kind in 0..4 {
        let (mut stage, mut a) = scene(kind, RateFunc::Base(custom), false, false);
        let clock = RationalFrameClock::new(32).unwrap();
        let open = open_play(&mut stage, &clock, &mut a).unwrap();
        assert_eq!(
            PureSegment::freeze(&stage, &clock, &RngRoot::from_seed(7), &a, &open).unwrap_err(),
            PureFrameRefusal::UnsupportedAnimation(0)
        );
        finish_open_play(&mut stage, &mut a, open).unwrap();
    }
}

#[test]
fn implicit_wiggle_pivot_is_the_start_center_even_after_scrubbing() {
    let mut implicit_stage = Stage::new();
    let mut explicit_stage = Stage::new();
    let implicit = family(&mut implicit_stage, 0.0);
    let explicit = family(&mut explicit_stage, 0.0);
    let center = explicit_stage.get_center(explicit);
    let mut a = WiggleOutThenIn::new(implicit)
        .with_scale_value(1.4)
        .with_rotation_angle(0.6)
        .with_config(AnimConfig {
            rate_func: RateFunc::linear(),
            lag_ratio: 0.17,
            ..AnimConfig::default()
        });
    let mut b = WiggleOutThenIn::new(explicit)
        .with_scale_value(1.4)
        .with_rotation_angle(0.6)
        .with_scale_about_point(center)
        .with_rotate_about_point(center)
        .with_config(a.state().config.clone());
    a.begin(&mut implicit_stage).unwrap();
    b.begin(&mut explicit_stage).unwrap();
    for alpha in [0.2, 0.8, 0.2, 0.5, 0.0, 0.9, 0.2, 1.0] {
        a.interpolate(&mut implicit_stage, alpha);
        b.interpolate(&mut explicit_stage, alpha);
        assert_eq!(
            facts(&implicit_stage),
            facts(&explicit_stage),
            "alpha={alpha}"
        );
        let once = facts(&implicit_stage);
        a.interpolate(&mut implicit_stage, alpha);
        assert_eq!(
            facts(&implicit_stage),
            once,
            "same-alpha interpolation must be idempotent"
        );
    }
    a.finish(&mut implicit_stage);
    b.finish(&mut explicit_stage);
    assert_eq!(facts(&implicit_stage), facts(&explicit_stage));
}
