//! TransformMatchingParts semantics (fm-cye): the normalized shape
//! probe, the claim ordering (user pairs → same-shape product →
//! fade-out/fade-in leftovers), and the null-piece guards — against
//! transform_matching_parts.py at the pin.

use std::sync::Arc;

use fmn_anim::pure_frame::{PureFrameRefusal, PureSegment};
use fmn_anim::{
    AnimError, Animation, MatchingPartsConfig, RateFunc, RationalFrameClock,
    TransformMatchingParts, TransformMatchingShapes, finish_open_play, has_same_shape_as,
    open_play, play_segment, transform_matching_keys, transform_matching_parts,
};
use fmn_core::rng::RngRoot;
use fmn_mobject::record::{RecordBuffer, RecordSchema};
use fmn_mobject::{Mob, Mobject, Stage, StageError};

fn vmob(stage: &mut Stage, points: &[[f64; 3]]) -> Mob {
    let mob = stage.add(Mobject::new());
    let entry = stage.get_mut(mob).unwrap();
    entry.buffer = RecordBuffer::new(RecordSchema::vmobject(), points.len()).unwrap();
    #[allow(clippy::cast_possible_truncation)]
    let flat: Vec<f32> = points
        .iter()
        .flat_map(|p| p.iter().map(|v| *v as f32))
        .collect();
    entry.buffer.write_range("point", 0, &flat);
    stage.refresh_family_joint_angles(mob).unwrap();
    mob
}

/// A 1-curve "vee" with distinct shape: (0,0) → (1,2) → (2,0).
fn vee(stage: &mut Stage, dx: f64) -> Mob {
    vmob(
        stage,
        &[[dx, 0.0, 0.0], [dx + 1.0, 2.0, 0.0], [dx + 2.0, 0.0, 0.0]],
    )
}

/// A flat 1-curve bump: (0,0) → (1,0.2) → (2,0).
fn bump(stage: &mut Stage, dx: f64) -> Mob {
    vmob(
        stage,
        &[[dx, 0.0, 0.0], [dx + 1.0, 0.2, 0.0], [dx + 2.0, 0.0, 0.0]],
    )
}

#[test]
fn shape_probe_normalizes_position_and_scale() {
    let mut stage = Stage::new();
    let a = vee(&mut stage, 0.0);
    let b = vee(&mut stage, 5.0);
    assert!(has_same_shape_as(&stage, a, b), "shifted copy matches");
    stage.scale(b, 3.0);
    assert!(has_same_shape_as(&stage, a, b), "scaled copy matches");
    let c = bump(&mut stage, 0.0);
    assert!(!has_same_shape_as(&stage, a, c), "different shapes differ");
}

#[test]
fn matching_parts_pairs_shapes_and_fades_leftovers() {
    let mut stage = Stage::new();
    let source = stage.add(Mobject::new());
    let s_vee = vee(&mut stage, 0.0);
    let s_bump = bump(&mut stage, 3.0);
    stage.attach(source, s_vee).unwrap();
    stage.attach(source, s_bump).unwrap();
    let target = stage.add(Mobject::new());
    let t_vee = vee(&mut stage, 10.0);
    stage.attach(target, t_vee).unwrap();

    let anims = transform_matching_parts(&mut stage, source, target, &[]).unwrap();
    // s_vee → t_vee transforms; s_bump fades out; nothing fades in.
    let names: Vec<String> = anims
        .iter()
        .map(|a| a.state().config.name.clone())
        .collect();
    assert_eq!(names, vec!["Transform", "FadeOutToPoint"]);
    assert_eq!(anims[0].state().mobject(), s_vee);
    assert_eq!(anims[1].state().mobject(), s_bump);
    assert!(anims[1].is_remover());
}

#[test]
fn unmatched_targets_fade_in_from_source_center() {
    let mut stage = Stage::new();
    let source = stage.add(Mobject::new());
    let s_vee = vee(&mut stage, 0.0);
    stage.attach(source, s_vee).unwrap();
    let target = stage.add(Mobject::new());
    let t_bump = bump(&mut stage, 4.0);
    stage.attach(target, t_bump).unwrap();

    let anims = transform_matching_parts(&mut stage, source, target, &[]).unwrap();
    let names: Vec<String> = anims
        .iter()
        .map(|a| a.state().config.name.clone())
        .collect();
    assert_eq!(names, vec!["FadeOutToPoint", "FadeInFromPoint"]);
    assert_eq!(anims[1].state().mobject(), t_bump);
}

#[test]
fn user_pairs_claim_before_the_product() {
    let mut stage = Stage::new();
    let source = stage.add(Mobject::new());
    let s_vee = vee(&mut stage, 0.0);
    stage.attach(source, s_vee).unwrap();
    let target = stage.add(Mobject::new());
    let t_vee = vee(&mut stage, 10.0);
    let t_bump = bump(&mut stage, 20.0);
    stage.attach(target, t_vee).unwrap();
    stage.attach(target, t_bump).unwrap();

    // Force the vee onto the bump; the same-shape t_vee is left to fade.
    let anims = transform_matching_parts(&mut stage, source, target, &[(s_vee, t_bump)]).unwrap();
    let names: Vec<String> = anims
        .iter()
        .map(|a| a.state().config.name.clone())
        .collect();
    assert_eq!(names, vec!["Transform", "FadeInFromPoint"]);
    assert_eq!(anims[0].state().mobject(), s_vee);
    assert_eq!(anims[1].state().mobject(), t_vee);
}

#[test]
fn empty_sides_produce_no_null_animations() {
    let mut stage = Stage::new();
    let source = stage.add(Mobject::new());
    let target = stage.add(Mobject::new());
    let anims = transform_matching_parts(&mut stage, source, target, &[]).unwrap();
    assert!(anims.is_empty());
}

#[test]
fn native_span_keys_match_stably_without_shape_assignment() {
    let mut stage = Stage::new();
    let source = stage.add(Mobject::new());
    let source_x0 = vee(&mut stage, 0.0);
    let source_plus = bump(&mut stage, 3.0);
    let source_x1 = vee(&mut stage, 6.0);
    stage.attach(source, source_x0).unwrap();
    stage.attach(source, source_plus).unwrap();
    stage.attach(source, source_x1).unwrap();

    let target = stage.add(Mobject::new());
    let target_x = bump(&mut stage, 10.0); // deliberately a different shape
    let target_minus = vee(&mut stage, 13.0);
    stage.attach(target, target_x).unwrap();
    stage.attach(target, target_minus).unwrap();

    let source_parts = vec![
        (source_x0, "x".to_owned()),
        (source_plus, "+".to_owned()),
        (source_x1, "x".to_owned()),
    ];
    let target_parts = vec![(target_x, "x".to_owned()), (target_minus, "-".to_owned())];
    let anims =
        transform_matching_keys(&mut stage, source, target, &source_parts, &target_parts).unwrap();
    let names: Vec<&str> = anims
        .iter()
        .map(|animation| animation.state().config.name.as_str())
        .collect();
    assert_eq!(
        names,
        [
            "Transform",
            "FadeOutToPoint",
            "FadeOutToPoint",
            "FadeInFromPoint"
        ]
    );
    assert_eq!(anims[0].state().mobject(), source_x0);
}

fn replacement_fixture(stage: &mut Stage, empty_source: bool, empty_target: bool) -> (Mob, Mob) {
    let source = stage.add(Mobject::new());
    if !empty_source {
        let nested = stage.add(Mobject::new());
        let matched = vee(stage, 0.0);
        let unmatched = bump(stage, 3.0);
        stage.attach(nested, matched).unwrap();
        stage.attach(nested, unmatched).unwrap();
        stage.attach(source, nested).unwrap();
    }
    let target = stage.add(Mobject::new());
    if !empty_target {
        let nested = stage.add(Mobject::new());
        let matched = vee(stage, 10.0);
        // The extra curve cannot match either source piece. Two connected
        // quadratic curves share their middle anchor, giving five points.
        let unmatched = vmob(
            stage,
            &[
                [13.0, 0.0, 0.0],
                [14.0, 1.0, 0.0],
                [15.0, 0.0, 0.0],
                [16.0, -1.0, 0.0],
                [17.0, 0.0, 0.0],
            ],
        );
        stage.attach(nested, matched).unwrap();
        stage.attach(nested, unmatched).unwrap();
        stage.attach(target, nested).unwrap();
    }
    (source, target)
}

fn family_facts(stage: &Stage, root: Mob) -> Vec<String> {
    let family = stage.family(root);
    let mut facts = vec![format!("family {family:?}")];
    for mob in family {
        let entry = stage.get(mob).unwrap();
        facts.push(format!(
            "{:?}/{:?}/{:?}/{:?}",
            entry.submobjects(),
            entry.placement().coefficients().map(f64::to_bits),
            entry.uniforms(),
            (stage.is_animating(mob), stage.is_updating_suspended(mob))
        ));
        for field in entry.buffer.schema().fields() {
            let values = entry.buffer.read_column(&field.name).unwrap();
            let bits: Vec<_> = values.iter().map(|value| value.to_bits()).collect();
            facts.push(format!("{}:{bits:?}", field.name));
        }
    }
    facts
}

fn frame_facts(stage: &Stage) -> Vec<String> {
    let mut facts = vec![format!(
        "time {:x}, roots {:?}",
        stage.time().to_bits(),
        stage.roots()
    )];
    for &root in stage.roots() {
        facts.extend(family_facts(stage, root));
    }
    facts
}

#[test]
fn native_matching_defaults_and_explicit_claims_preserve_the_raw_plan() {
    let mut stage = Stage::new();
    let source = stage.add(Mobject::new());
    let source_vee = vee(&mut stage, 0.0);
    stage.attach(source, source_vee).unwrap();
    let target = stage.add(Mobject::new());
    let target_vee = vee(&mut stage, 10.0);
    let target_bump = bump(&mut stage, 20.0);
    stage.attach(target, target_vee).unwrap();
    stage.attach(target, target_bump).unwrap();
    let outside = bump(&mut stage, -10.0);
    stage.add_to_scene(source).unwrap();
    let animation = TransformMatchingShapes::with_matched_pairs(
        &mut stage,
        source,
        target,
        &[
            (outside, target_vee),
            (source_vee, target_bump),
            (source_vee, target_vee),
        ],
    )
    .unwrap();
    assert_eq!(animation.get_run_time(), 2.0);
    assert_eq!(animation.state().config.lag_ratio, 0.0);
    assert_eq!(animation.state().config.rate_func.eval(0.25), 0.25);
    assert_eq!(animation.target(), target);
    let names: Vec<_> = animation
        .animations()
        .iter()
        .map(|child| child.state().config.name.as_str())
        .collect();
    assert_eq!(names, ["Transform", "FadeInFromPoint"]);
    assert_eq!(animation.animations()[0].state().mobject(), source_vee);
    assert!(
        animation.animations()[0]
            .preflight_mobjects()
            .contains(&target_bump)
    );
    assert_eq!(animation.animations()[1].state().mobject(), target_vee);
    assert_eq!(
        stage.roots(),
        &[source],
        "construction must not publish the target"
    );
}

#[test]
fn native_matching_refuses_shared_families_before_mutating_the_stage() {
    for kind in 0..3 {
        let mut stage = Stage::new();
        let source = stage.add(Mobject::new());
        let target = if kind == 0 {
            source
        } else {
            stage.add(Mobject::new())
        };
        if kind == 1 {
            // Even a shared point-free grandchild is part of both operands.
            let shared = stage.add(Mobject::new());
            let nested = stage.add(Mobject::new());
            stage.attach(nested, shared).unwrap();
            stage.attach(source, nested).unwrap();
            stage.attach(target, shared).unwrap();
        } else if kind == 2 {
            stage.attach(source, target).unwrap();
        }
        stage.add_to_scene(source).unwrap();
        let before = (
            stage.snapshot_bytes().unwrap(),
            frame_facts(&stage),
            family_facts(&stage, target),
        );
        assert!(matches!(
            TransformMatchingParts::new(&mut stage, source, target),
            Err(AnimError::AliasedMatchingFamilies)
        ));
        assert_eq!(
            (
                stage.snapshot_bytes().unwrap(),
                frame_facts(&stage),
                family_facts(&stage, target)
            ),
            before
        );
    }
}

#[test]
fn native_matching_rechecks_nested_topology_before_any_begin_side_effect() {
    for kind in 0..4 {
        let mut stage = Stage::new();
        let (source, target) = replacement_fixture(&mut stage, false, false);
        stage.add_to_scene(source).unwrap();
        let mut animation = TransformMatchingParts::new(&mut stage, source, target).unwrap();
        let changed = if kind == 1 { target } else { source };
        let nested = stage.get(changed).unwrap().submobjects()[0];
        if kind < 2 {
            let extra = vee(&mut stage, 50.0);
            stage.attach(nested, extra).unwrap();
        } else if kind == 2 {
            stage.get_mut(changed).unwrap().buffer =
                RecordBuffer::new(RecordSchema::vmobject(), 3).unwrap();
        } else {
            let mut children = stage.get(nested).unwrap().submobjects().to_vec();
            children.reverse();
            stage.replace_children(nested, &children).unwrap();
        }
        let before = (
            stage.snapshot_bytes().unwrap(),
            frame_facts(&stage),
            family_facts(&stage, target),
        );
        assert_eq!(
            animation.begin(&mut stage),
            Err(AnimError::Stage(StageError::FamilyShapeMismatch))
        );
        assert_eq!(
            (
                stage.snapshot_bytes().unwrap(),
                frame_facts(&stage),
                family_facts(&stage, target)
            ),
            before
        );
        assert!(!stage.is_animating(animation.state().mobject()));
        assert!(animation.freeze_interpolation().is_none());
    }
}

#[test]
fn native_matching_completes_skipped_play_and_aborts_without_replacement() {
    for skip in [false, true] {
        let mut stage = Stage::new();
        let unrelated = vee(&mut stage, -5.0);
        let (source, target) = replacement_fixture(&mut stage, false, false);
        stage.add_many_to_scene(&[unrelated, source]).unwrap();
        let target_before = family_facts(&stage, target);
        let animation = TransformMatchingParts::with_config(
            &mut stage,
            source,
            target,
            &[],
            MatchingPartsConfig {
                run_time: 0.5,
                ..MatchingPartsConfig::default()
            },
        )
        .unwrap();
        let mut animations: Vec<Box<dyn Animation>> = vec![Box::new(animation)];
        let mut clock = RationalFrameClock::new(12).unwrap();
        let mut captures = 0;
        play_segment(
            &mut stage,
            &mut clock,
            &RngRoot::from_seed(7),
            &mut animations,
            skip,
            &mut |_| {
                captures += 1;
            },
        )
        .unwrap();
        assert_eq!(captures, if skip { 0 } else { 6 });
        assert_eq!(clock.now().frames(), 6);
        assert_eq!(stage.roots(), &[unrelated, target]);
        assert_eq!(family_facts(&stage, target), target_before);
        let after = frame_facts(&stage);
        animations[0].finish(&mut stage);
        animations[0].clean_up_from_scene(&mut stage);
        assert_eq!(frame_facts(&stage), after, "repeated cleanup must be inert");
        assert!(animations[0].deferred_error().is_none());
        assert!(animations[0].freeze_interpolation().is_none());
    }

    let mut stage = Stage::new();
    let (source, target) = replacement_fixture(&mut stage, false, false);
    stage.add_to_scene(source).unwrap();
    let mut animation = TransformMatchingParts::new(&mut stage, source, target).unwrap();
    assert!(animation.freeze_interpolation().is_none());
    animation.begin(&mut stage).unwrap();
    assert!(animation.freeze_interpolation().is_some());
    animation.interpolate(&mut stage, 0.25);
    animation.abort(&mut stage);
    assert!(animation.freeze_interpolation().is_none());
    animation.clean_up_from_scene(&mut stage);
    assert!(matches!(
        animation.deferred_error(),
        Some(AnimError::InvalidFramePhase(_))
    ));
    assert_eq!(stage.roots(), &[source]);
    for mob in stage.family(animation.state().mobject()) {
        assert!(!stage.is_animating(mob));
        assert!(!stage.is_updating_suspended(mob));
    }
}

#[test]
fn native_matching_does_not_publish_after_a_latched_lifecycle_error() {
    let mut stage = Stage::new();
    let (source, target) = replacement_fixture(&mut stage, false, false);
    stage.add_to_scene(source).unwrap();
    let mut animation = TransformMatchingParts::new(&mut stage, source, target).unwrap();
    animation.begin(&mut stage).unwrap();
    animation.clean_up_from_scene(&mut stage);
    let error = animation.deferred_error();
    assert!(matches!(error, Some(AnimError::InvalidFramePhase(_))));
    assert!(animation.freeze_interpolation().is_none());

    // Finishing after a bad cleanup call must not turn that failed attempt
    // into a published replacement while its original error remains latched.
    animation.finish(&mut stage);
    animation.clean_up_from_scene(&mut stage);
    assert_eq!(animation.deferred_error(), error);
    assert_eq!(stage.roots(), &[source]);
    assert!(!stage.roots().contains(&target));
    assert!(!stage.is_animating(animation.state().mobject()));
}

#[test]
fn native_matching_workers_reproduce_serial_frames_and_empty_replacements() {
    for (empty_source, empty_target) in [(false, false), (true, false), (false, true), (true, true)]
    {
        let mut initial = Stage::new();
        let unrelated = vee(&mut initial, -10.0);
        let (source, target) = replacement_fixture(&mut initial, empty_source, empty_target);
        initial.add_many_to_scene(&[unrelated, source]).unwrap();
        initial.scale_about(source, 0.75, Some([0.2, 0.3, 0.4]), None);
        initial.shift(target, [0.2, -0.5, 0.3]);
        let snapshot = initial.snapshot();
        let mut expected = None;
        for workers in [0, 1, 4, 16] {
            let mut stage = snapshot.materialize();
            let animation = TransformMatchingParts::with_config(
                &mut stage,
                source,
                target,
                &[],
                MatchingPartsConfig {
                    run_time: 0.5,
                    lag_ratio: 0.2,
                    rate_func: RateFunc::Sampled(Arc::from([0.0, 0.9, 0.1, 0.8, 1.0])),
                },
            )
            .unwrap();
            let mut animations: Vec<Box<dyn Animation>> = vec![Box::new(animation)];
            animations[0].state_mut().config.time_span = Some((0.03125, 0.4375));
            animations[0].state_mut().config.final_alpha_value = 0.35;
            let mut clock = RationalFrameClock::new(12).unwrap();
            clock.advance_frames(3).unwrap();
            stage.set_time_from_clock(clock.now().to_f64());
            let rng = RngRoot::from_seed(71);
            let frames: Vec<Vec<String>> = if workers == 0 {
                let mut frames = Vec::new();
                play_segment(
                    &mut stage,
                    &mut clock,
                    &rng,
                    &mut animations,
                    false,
                    &mut |packet| {
                        frames.push(frame_facts(&packet.materialize_stage()));
                    },
                )
                .unwrap();
                frames
            } else {
                let open = open_play(&mut stage, &clock, &mut animations).unwrap();
                let program =
                    PureSegment::freeze(&stage, &clock, &rng, &animations, &open).unwrap();
                let before = frame_facts(&stage);
                let mut indexed = std::thread::scope(|scope| {
                    let handles: Vec<_> = (0..workers)
                        .map(|worker| {
                            let program = &program;
                            scope.spawn(move || {
                                (worker..usize::try_from(program.frame_count()).unwrap())
                                    .step_by(workers)
                                    .rev()
                                    .map(|index| {
                                        let frame = i64::try_from(index + 1).unwrap();
                                        (
                                            frame,
                                            frame_facts(&program.job(frame).unwrap().materialize()),
                                        )
                                    })
                                    .collect::<Vec<_>>()
                            })
                        })
                        .collect();
                    handles
                        .into_iter()
                        .flat_map(|handle| handle.join().unwrap())
                        .collect::<Vec<_>>()
                });
                assert_eq!(
                    frame_facts(&stage),
                    before,
                    "workers must leave owner state untouched"
                );
                indexed.sort_by_key(|(index, _)| *index);
                program
                    .finish_on_owner(&mut stage, &mut clock, &mut animations, open)
                    .unwrap();
                indexed.into_iter().map(|(_, frame)| frame).collect()
            };
            assert_eq!(frames.len(), 6);
            assert_eq!(clock.now().frames(), 9);
            assert_eq!(stage.roots(), &[unrelated, target]);
            assert!(animations[0].deferred_error().is_none());
            let actual = (frames, frame_facts(&stage));
            if let Some(expected) = &expected {
                assert_eq!(
                    &actual, expected,
                    "empty={empty_source}/{empty_target}, workers={workers}"
                );
            } else {
                expected = Some(actual);
            }
        }
    }
}

#[test]
fn native_matching_demotes_original_operand_updaters_live_views_and_custom_rates() {
    fn custom_rate(alpha: f64) -> f64 {
        alpha * alpha
    }
    for case in 0..4 {
        let mut stage = Stage::new();
        let (source, target) = replacement_fixture(&mut stage, false, false);
        stage.add_to_scene(source).unwrap();
        let mut view = None;
        let mut config = MatchingPartsConfig::default();
        match case {
            0 => {
                stage.add_updater(source, |_, _| {}, false).unwrap();
            }
            1 => {
                stage.add_updater(target, |_, _| {}, false).unwrap();
            }
            2 => {
                view = Some(stage.get_mut(target).unwrap().buffer.export_view(false));
            }
            _ => {
                config.rate_func = RateFunc::Base(custom_rate);
            }
        }
        let animation =
            TransformMatchingParts::with_config(&mut stage, source, target, &[], config).unwrap();
        let mut animations: Vec<Box<dyn Animation>> = vec![Box::new(animation)];
        let clock = RationalFrameClock::new(12).unwrap();
        let open = open_play(&mut stage, &clock, &mut animations).unwrap();
        let refusal =
            PureSegment::freeze(&stage, &clock, &RngRoot::from_seed(3), &animations, &open)
                .unwrap_err();
        match case {
            0 | 1 => assert_eq!(refusal, PureFrameRefusal::NotAtPureBegin),
            2 => assert_eq!(refusal, PureFrameRefusal::LiveView(target)),
            _ => assert_eq!(refusal, PureFrameRefusal::UnsupportedAnimation(0)),
        }
        drop(view);
        finish_open_play(&mut stage, &mut animations, open).unwrap();
    }
}
