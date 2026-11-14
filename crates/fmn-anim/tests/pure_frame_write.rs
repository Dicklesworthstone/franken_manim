//! Writing must be absolute in alpha, including when a group reverses time.
use fmn_anim::pure_frame::{PureFrameRefusal, PureSegment};
use fmn_anim::{
    AnimConfig, Animation, AnimationGroup, DrawBorderThenFill, RateFunc, RationalFrameClock,
    advance_play, finish_open_play, open_play, write,
};
use fmn_core::rate::CatalogRate;
use fmn_core::rng::RngRoot;
use fmn_mobject::{Mob, Mobject, Snapshot, Stage};

fn shape(stage: &mut Stage) -> Mob {
    let mob = stage.add(Mobject::vector_from_points(&[
        [-1.0, -1.0, 0.0],
        [0.0, -1.0, 0.0],
        [1.0, -1.0, 0.0],
        [1.0, 0.0, 0.0],
        [1.0, 1.0, 0.0],
        [0.0, 1.0, 0.0],
        [-1.0, 1.0, 0.0],
        [-1.0, 0.0, 0.0],
        [-1.0, -1.0, 0.0],
    ]));
    let entry = stage.get_mut(mob).unwrap();
    entry.uniforms_mut().shading = [0.13, 0.27, 0.41];
    entry.uniforms_mut().anti_alias_width = 1.3;
    let len = entry.buffer.len();
    entry
        .buffer
        .write_range("fill_rgba", 0, &[0.2, 0.6, 0.9, 0.8].repeat(len));
    entry
        .buffer
        .write_range("stroke_rgba", 0, &[0.8, 0.3, 0.1, 0.9].repeat(len));
    entry.buffer.write_range("stroke_width", 0, &vec![5.0; len]);
    stage.add_to_scene(mob).unwrap();
    mob
}

fn facts(stage: &Stage) -> Vec<String> {
    let mut out = vec![format!("time:{:x}", stage.time().to_bits())];
    for &root in stage.roots() {
        let family = stage.family(root);
        out.push(format!("family:{}", family.len()));
        for mob in family {
            let entry = stage.get(mob).unwrap();
            out.push(format!(
                "{:?}/{:?}/{:?}",
                entry.placement().coefficients().map(f64::to_bits),
                entry.uniforms(),
                (stage.is_animating(mob), stage.is_updating_suspended(mob))
            ));
            for field in entry.buffer.schema().fields() {
                let bits: Vec<_> = entry
                    .buffer
                    .read_column(&field.name)
                    .unwrap()
                    .iter()
                    .map(|value| value.to_bits())
                    .collect();
                out.push(format!("{}:{bits:?}", field.name));
            }
        }
    }
    out
}

fn scene(rate: RateFunc, nested: bool, use_write: bool) -> (Stage, Vec<Box<dyn Animation>>) {
    let mut stage = Stage::new();
    let first = shape(&mut stage);
    let second = stage.copy_family(first).unwrap();
    stage.shift(second, [3.0, 0.0, 0.0]);
    let group = stage.add(Mobject::vector_from_points(&[]));
    stage.attach(group, first).unwrap();
    stage.attach(group, second).unwrap();
    stage.remove_from_scene(first);
    stage.add_to_scene(group).unwrap();
    let mut animation = if use_write {
        write(&stage, group)
    } else {
        DrawBorderThenFill::new(group).with_config(AnimConfig {
            lag_ratio: 0.3,
            ..AnimConfig::default()
        })
    };
    animation.state_mut().config.run_time = 1.0;
    animation.state_mut().config.time_span = Some((0.1, 0.95));
    let animation: Box<dyn Animation> = if nested {
        Box::new(
            AnimationGroup::new(&mut stage, vec![Box::new(animation)])
                .unwrap()
                .with_rate_func(rate),
        )
    } else {
        animation.state_mut().config.rate_func = rate;
        Box::new(animation)
    };
    (stage, vec![animation])
}

fn compare(rate: RateFunc, nested: bool, use_write: bool) {
    let (mut serial, mut serial_animations) = scene(rate.clone(), nested, use_write);
    let (mut owner, mut owner_animations) = scene(rate, nested, use_write);
    let mut clock = RationalFrameClock::new(30).unwrap();
    let mut owner_clock = RationalFrameClock::new(30).unwrap();
    let rng = RngRoot::from_seed(9);
    let mut open = open_play(&mut serial, &clock, &mut serial_animations).unwrap();
    let owner_open = open_play(&mut owner, &owner_clock, &mut owner_animations).unwrap();
    let frozen =
        PureSegment::freeze(&owner, &owner_clock, &rng, &owner_animations, &owner_open).unwrap();
    let mut expected = Vec::new();
    advance_play(
        &mut serial,
        &mut clock,
        &rng,
        &mut serial_animations,
        &mut open,
        frozen.frame_count(),
        &mut |packet| expected.push(facts(&packet.materialize_stage())),
    )
    .unwrap();
    assert_eq!(expected.len(), 30);
    for workers in [1, 4, 16] {
        std::thread::scope(|scope| {
            for worker in 0..workers {
                let frozen = &frozen;
                let expected = &expected;
                scope.spawn(move || {
                    for index in (worker..expected.len()).step_by(workers).rev() {
                        let actual = facts(&frozen.job(index as i64 + 1).unwrap().materialize());
                        assert_eq!(actual.len(), expected[index].len());
                        for (field, (actual, expected)) in actual.iter().zip(&expected[index]).enumerate() {
                            assert_eq!(actual, expected,
                                "frame {index}, field {field}, workers {workers}, nested {nested}, Write {use_write}");
                        }
                    }
                });
            }
        });
    }
    finish_open_play(&mut serial, &mut serial_animations, open).unwrap();
    frozen
        .finish_on_owner(
            &mut owner,
            &mut owner_clock,
            &mut owner_animations,
            owner_open,
        )
        .unwrap();
    assert_eq!(facts(&owner), facts(&serial));
    assert_eq!(clock.now(), owner_clock.now());
}

#[test]
fn border_fill_and_write_match_serial_at_every_sample_and_owner_finish() {
    for use_write in [false, true] {
        for nested in [false, true] {
            for rate in [
                RateFunc::linear(),
                RateFunc::Catalog(CatalogRate::Smooth),
                RateFunc::Catalog(CatalogRate::ThereAndBack),
                RateFunc::Catalog(CatalogRate::ThereAndBack).squish(0.1, 0.9),
            ] {
                compare(rate, nested, use_write);
            }
        }
    }
}

#[test]
fn returning_to_the_outline_half_restores_transparent_fill_and_outline_stroke() {
    let mut stage = Stage::new();
    let mob = shape(&mut stage);
    let mut animation = DrawBorderThenFill::new(mob).with_config(AnimConfig {
        rate_func: RateFunc::linear(),
        ..AnimConfig::default()
    });
    animation.begin(&mut stage).unwrap();
    animation.interpolate(&mut stage, 0.8);
    assert!(
        stage
            .get(mob)
            .unwrap()
            .buffer
            .read_column("fill_rgba")
            .unwrap()[3]
            > 0.0
    );
    animation.interpolate(&mut stage, 0.2);
    let entry = stage.get(mob).unwrap();
    assert!(
        entry
            .buffer
            .read_column("fill_rgba")
            .unwrap()
            .iter()
            .skip(3)
            .step_by(4)
            .all(|alpha| *alpha == 0.0)
    );
    assert!(
        entry
            .buffer
            .read_column("stroke_width")
            .unwrap()
            .iter()
            .all(|width| *width == 2.0)
    );
    animation.interpolate(&mut stage, 0.8);
    assert!(
        stage
            .get(mob)
            .unwrap()
            .buffer
            .read_column("fill_rgba")
            .unwrap()[3]
            > 0.0
    );
    animation.finish(&mut stage);
}

#[test]
fn write_does_not_transfer_an_unknown_rate_callback() {
    fn custom(alpha: f64) -> f64 {
        alpha * alpha
    }
    let (mut stage, mut animations) = scene(RateFunc::Base(custom), false, true);
    let clock = RationalFrameClock::new(30).unwrap();
    let open = open_play(&mut stage, &clock, &mut animations).unwrap();
    assert_eq!(
        PureSegment::freeze(&stage, &clock, &RngRoot::from_seed(9), &animations, &open)
            .unwrap_err(),
        PureFrameRefusal::UnsupportedAnimation(0)
    );
    finish_open_play(&mut stage, &mut animations, open).unwrap();
}

#[test]
fn durable_write_resumes_each_draw_and_fill_phase_without_rebeginning() {
    for use_write in [false, true] {
        for rate in [
            RateFunc::linear(),
            RateFunc::Catalog(CatalogRate::ThereAndBack),
            RateFunc::Sampled(std::sync::Arc::from([0.0, 1.0, 0.0, 1.0])),
        ] {
            let (mut serial, mut animations) = scene(rate, false, use_write);
            let mut clock = RationalFrameClock::new(30).unwrap();
            let rng = RngRoot::from_seed(9);
            let mut open = open_play(&mut serial, &clock, &mut animations).unwrap();
            for checkpoint_frame in [0, 4, 9, 15, 22, 29, 30] {
                advance_play(
                    &mut serial,
                    &mut clock,
                    &rng,
                    &mut animations,
                    &mut open,
                    checkpoint_frame,
                    &mut |_| {},
                )
                .unwrap();
                let checkpoint = fmn_anim::continuation::capture(
                    &serial,
                    &clock,
                    &animations,
                    &open,
                    1024 * 1024,
                )
                .unwrap()
                .expect("native writing has a durable lifecycle");
                let snapshot = serial.snapshot().to_bytes().unwrap();
                let mut restored = Stage::new();
                let decoded = Snapshot::from_bytes(&snapshot, &restored).unwrap();
                restored.restore(&decoded.snapshot);
                let (mut resumed, mut resumed_open) =
                    fmn_anim::continuation::restore(&restored, &clock, &checkpoint).unwrap();
                let mut resumed_clock = clock;
                assert_eq!(restored.snapshot().to_bytes().unwrap(), snapshot);
                assert_eq!(
                    fmn_anim::continuation::capture(
                        &restored,
                        &resumed_clock,
                        &resumed,
                        &resumed_open,
                        1024 * 1024,
                    )
                    .unwrap()
                    .unwrap(),
                    checkpoint,
                    "recapture must preserve the prepared outline and phase set"
                );

                // A fresh ordinary run supplies every remaining frame and the
                // cleanup result independently of the serialized carrier.
                let (mut expected, mut expected_animations) = scene(
                    animations[0].state().config.rate_func.clone(),
                    false,
                    use_write,
                );
                let mut expected_clock = RationalFrameClock::new(30).unwrap();
                let mut expected_open =
                    open_play(&mut expected, &expected_clock, &mut expected_animations).unwrap();
                advance_play(
                    &mut expected,
                    &mut expected_clock,
                    &rng,
                    &mut expected_animations,
                    &mut expected_open,
                    checkpoint_frame,
                    &mut |_| {},
                )
                .unwrap();
                let mut expected_frames = Vec::new();
                let mut resumed_frames = Vec::new();
                advance_play(
                    &mut expected,
                    &mut expected_clock,
                    &rng,
                    &mut expected_animations,
                    &mut expected_open,
                    30,
                    &mut |frame| expected_frames.push(facts(&frame.materialize_stage())),
                )
                .unwrap();
                advance_play(
                    &mut restored,
                    &mut resumed_clock,
                    &rng,
                    &mut resumed,
                    &mut resumed_open,
                    30,
                    &mut |frame| resumed_frames.push(facts(&frame.materialize_stage())),
                )
                .unwrap();
                assert_eq!(resumed_frames, expected_frames);
                finish_open_play(&mut expected, &mut expected_animations, expected_open).unwrap();
                finish_open_play(&mut restored, &mut resumed, resumed_open).unwrap();
                assert_eq!(facts(&restored), facts(&expected));
                assert_eq!(resumed_clock.now(), expected_clock.now());
            }
            finish_open_play(&mut serial, &mut animations, open).unwrap();
        }
    }
}
