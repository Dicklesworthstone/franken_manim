//! Structural reveal jobs must preserve serial membership, not just pixels.
use fmn_anim::pure_frame::{PureFrameRefusal, PureSegment};
use fmn_anim::{
    AnimConfig, Animation, AnimationGroup, RateFunc, RationalFrameClock, ShowIncreasingSubsets,
    add_text_letter_by_letter, add_text_word_by_word, advance_play, finish_open_play, open_play,
    show_increasing_subsets, show_submobjects_one_by_one,
};
use fmn_core::rate::CatalogRate;
use fmn_core::rng::RngRoot;
use fmn_mobject::{Mob, Mobject, Stage};

fn family(stage: &mut Stage, count: usize) -> (Mob, Vec<Mob>) {
    let root = stage.add(Mobject::vector_from_points(&[]));
    let mut children = Vec::new();
    for index in 0..count {
        let x = index as f64;
        let child = stage.add(Mobject::vector_from_points(&[
            [x, 0.0, 0.0],
            [x + 0.2, 0.5, 0.0],
            [x + 0.4, 0.0, 0.0],
        ]));
        stage.attach(root, child).unwrap();
        let grandchild = stage.add(Mobject::from_points(&[[x, -0.25, 0.0]]));
        stage.attach(child, grandchild).unwrap();
        children.push(child);
    }
    stage.add_to_scene(root).unwrap();
    (root, children)
}

fn reveal(stage: &Stage, root: Mob, kind: usize) -> ShowIncreasingSubsets {
    match kind {
        0 => show_increasing_subsets(stage, root),
        1 => show_submobjects_one_by_one(stage, root),
        2 => add_text_word_by_word(stage, root, 0.1, 1.0),
        _ => add_text_letter_by_letter(stage, root, 0.1, 1.0),
    }
    .unwrap()
}

fn facts(stage: &Stage) -> Vec<String> {
    let mut out = vec![format!("{:x}", stage.time().to_bits())];
    for &root in stage.roots() {
        for mob in stage.family(root) {
            let e = stage.get(mob).unwrap();
            out.push(format!("{mob:?}:{:?}:{:?}", e.submobjects(), e.placement()));
            for f in e.buffer.schema().fields() {
                let bits: Vec<_> = e
                    .buffer
                    .read_column(&f.name)
                    .unwrap()
                    .iter()
                    .map(|x| x.to_bits())
                    .collect();
                out.push(format!("{}:{bits:?}", f.name));
            }
        }
    }
    out
}

#[test]
fn structural_reveals_reconstruct_every_serial_sample_out_of_order() {
    for kind in 0..4 {
        for nested in [false, true] {
            for rate in [
                RateFunc::linear(),
                RateFunc::Catalog(CatalogRate::ThereAndBack),
            ] {
                let mut stage = Stage::new();
                let (root, children) = family(&mut stage, 5);
                let mut animation = reveal(&stage, root, kind).with_config(AnimConfig {
                    run_time: 1.0,
                    rate_func: rate.clone(),
                    ..AnimConfig::default()
                });
                let animation: Box<dyn Animation> = if nested {
                    animation.state_mut().config.rate_func = RateFunc::linear();
                    Box::new(
                        AnimationGroup::with_lag_ratio(&mut stage, vec![Box::new(animation)], 0.3)
                            .unwrap()
                            .with_rate_func(rate),
                    )
                } else {
                    Box::new(animation)
                };
                let mut animations = vec![animation];
                let mut clock = RationalFrameClock::new(16).unwrap();
                clock.advance_frames(7).unwrap();
                stage.set_time_from_clock(clock.now().to_f64());
                let rng = RngRoot::from_seed(619);
                let mut open = open_play(&mut stage, &clock, &mut animations).unwrap();
                let program =
                    PureSegment::freeze(&stage, &clock, &rng, &animations, &open).unwrap();
                let mut expected = Vec::new();
                advance_play(
                    &mut stage,
                    &mut clock,
                    &rng,
                    &mut animations,
                    &mut open,
                    program.frame_count(),
                    &mut |packet| {
                        expected.push((
                            facts(&packet.materialize_stage()),
                            packet.rng_fork("reveals").next_u64(),
                        ));
                    },
                )
                .unwrap();
                assert!(
                    expected
                        .windows(2)
                        .any(|pair| pair[0].0[1..] != pair[1].0[1..])
                );
                for workers in [1, 4, 16] {
                    std::thread::scope(|scope| {
                        for worker in 0..workers {
                            let program = &program;
                            let expected = &expected;
                            scope.spawn(move || {
                                for index in (worker..expected.len()).step_by(workers).rev() {
                                    let job = program.job(index as i64 + 1).unwrap();
                                    assert_eq!(facts(&job.materialize()), expected[index].0,
                                        "kind={kind}, nested={nested}, frame={index}, workers={workers}");
                                    assert_eq!(job.rng_fork("reveals").next_u64(), expected[index].1);
                                }
                            });
                        }
                    });
                }
                finish_open_play(&mut stage, &mut animations, open).unwrap();
                assert!(children.iter().all(|child| stage.contains(*child)));
            }
        }
    }
}

#[test]
fn hidden_descendants_cannot_hide_updaters_or_live_record_views() {
    for with_updater in [false, true] {
        let mut stage = Stage::new();
        let (root, children) = family(&mut stage, 3);
        let hidden = stage.get(children[2]).unwrap().submobjects()[0];
        let view = if with_updater {
            stage
                .add_dt_updater(
                    hidden,
                    |s, m, dt| {
                        s.shift(m, [dt, 0.0, 0.0]);
                    },
                    false,
                )
                .unwrap();
            None
        } else {
            Some(stage.get_mut(hidden).unwrap().buffer.export_view(false))
        };
        let animation = reveal(&stage, root, 0);
        let mut animations: Vec<Box<dyn Animation>> = vec![Box::new(animation)];
        let clock = RationalFrameClock::new(16).unwrap();
        let open = open_play(&mut stage, &clock, &mut animations).unwrap();
        assert!(stage.get(root).unwrap().submobjects().is_empty());
        let refusal =
            PureSegment::freeze(&stage, &clock, &RngRoot::from_seed(1), &animations, &open)
                .unwrap_err();
        if with_updater {
            assert_eq!(refusal, PureFrameRefusal::NotAtPureBegin);
        } else {
            assert_eq!(refusal, PureFrameRefusal::LiveView(hidden));
        }
        finish_open_play(&mut stage, &mut animations, open).unwrap();
        drop(view);
    }
}

#[test]
fn unknown_reveal_rate_stays_serial() {
    fn custom(alpha: f64) -> f64 {
        alpha * alpha
    }
    let mut stage = Stage::new();
    let (root, _) = family(&mut stage, 3);
    let animation = reveal(&stage, root, 3).with_config(AnimConfig {
        rate_func: RateFunc::Base(custom),
        ..AnimConfig::default()
    });
    let mut animations: Vec<Box<dyn Animation>> = vec![Box::new(animation)];
    let clock = RationalFrameClock::new(16).unwrap();
    let open = open_play(&mut stage, &clock, &mut animations).unwrap();
    assert_eq!(
        PureSegment::freeze(&stage, &clock, &RngRoot::from_seed(1), &animations, &open)
            .unwrap_err(),
        PureFrameRefusal::UnsupportedAnimation(0)
    );
    finish_open_play(&mut stage, &mut animations, open).unwrap();
}

#[test]
fn native_finish_keeps_serial_membership_clock_and_child_lifecycle() {
    for final_alpha in [0.0, 0.4, 1.0] {
        let run = |native: bool| {
            let mut stage = Stage::new();
            let (root, children) = family(&mut stage, 5);
            let animation = reveal(&stage, root, 0).with_config(AnimConfig {
                run_time: 1.0,
                final_alpha_value: final_alpha,
                rate_func: RateFunc::linear(),
                ..AnimConfig::default()
            });
            let mut animations: Vec<Box<dyn Animation>> = vec![Box::new(animation)];
            let mut clock = RationalFrameClock::new(16).unwrap();
            let rng = RngRoot::from_seed(2);
            let mut open = open_play(&mut stage, &clock, &mut animations).unwrap();
            if native {
                let program =
                    PureSegment::freeze(&stage, &clock, &rng, &animations, &open).unwrap();
                for k in (1..=program.frame_count()).rev() {
                    let _ = program.job(k).unwrap().materialize();
                }
                program
                    .finish_on_owner(&mut stage, &mut clock, &mut animations, open)
                    .unwrap();
            } else {
                advance_play(
                    &mut stage,
                    &mut clock,
                    &rng,
                    &mut animations,
                    &mut open,
                    16,
                    &mut |_| {},
                )
                .unwrap();
                finish_open_play(&mut stage, &mut animations, open).unwrap();
            }
            let child_facts = children
                .iter()
                .map(|child| {
                    (
                        stage.get_points(*child),
                        stage.is_animating(*child),
                        stage.is_updating_suspended(*child),
                    )
                })
                .collect::<Vec<_>>();
            (
                clock.now(),
                stage.get(root).unwrap().submobjects().len(),
                child_facts,
            )
        };
        assert_eq!(run(true), run(false));
    }
}

#[test]
fn reveal_and_transform_cannot_mutate_the_same_descendant_on_workers() {
    let mut stage = Stage::new();
    let (root, children) = family(&mut stage, 3);
    let animation = reveal(&stage, root, 0);
    let target = stage.copy_family(children[1]).unwrap();
    stage.shift(target, [2.0, 0.0, 0.0]);
    let mut animations: Vec<Box<dyn Animation>> = vec![
        Box::new(animation),
        Box::new(fmn_anim::Transform::new(children[1], target)),
    ];
    let clock = RationalFrameClock::new(16).unwrap();
    let open = open_play(&mut stage, &clock, &mut animations).unwrap();
    assert_eq!(
        PureSegment::freeze(&stage, &clock, &RngRoot::from_seed(2), &animations, &open)
            .unwrap_err(),
        PureFrameRefusal::OverlappingOutput(children[1])
    );
    finish_open_play(&mut stage, &mut animations, open).unwrap();
}
