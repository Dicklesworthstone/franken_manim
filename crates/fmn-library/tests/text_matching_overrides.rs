//! Explicit native source-span substitutions through real Choreo execution.
use std::sync::Arc;

use fmn_anim::pure_frame::PureSegment;
use fmn_anim::{Animation, RateFunc, RationalFrameClock, open_play, play_segment};
use fmn_core::rng::RngRoot;
use fmn_library::{
    SpanKindU8, SpanMapData, SpanMapEntry, Tex, TexEngine, TextMatchingConfig,
    TextMatchingError, TransformMatchingStrings, TransformMatchingTex,
};
use fmn_mobject::{Mob, Mobject, Stage};

fn fixture(stage: &mut Stage, text: &str, x: f64) -> (Mob, SpanMapData, Vec<Mob>) {
    let root = stage.add(Mobject::new());
    let mut children = Vec::new();
    let mut entries = Vec::new();
    for (index, (start, ch)) in text.char_indices().enumerate() {
        #[allow(clippy::cast_precision_loss)]
        let dx = x + index as f64;
        let child = stage.add(Mobject::from_points(&[
            [dx, 0.0, 0.0],
            [dx + 0.5, 1.0, 0.0],
            [dx + 1.0, 0.0, 0.0],
        ]));
        stage.attach(root, child).unwrap();
        children.push(child);
        entries.push(SpanMapEntry {
            start,
            end: start + ch.len_utf8(),
            kind: SpanKindU8::TextGlyph,
        });
    }
    (
        root,
        SpanMapData {
            source: Arc::from(text),
            entries,
        },
        children,
    )
}

fn mapped(
    stage: &mut Stage,
    source: Mob,
    target: Mob,
    source_map: &SpanMapData,
    target_map: &SpanMapData,
    key_map: &[(&str, &str)],
    strings: bool,
) -> Box<dyn Animation> {
    let config = TextMatchingConfig {
        run_time: 0.5,
        lag_ratio: 0.2,
        rate_func: RateFunc::Sampled(Arc::from([0.0, 0.9, 0.1, 0.8, 1.0])),
    };
    if strings {
        Box::new(
            TransformMatchingStrings::with_key_map(
                stage, source, target, source_map, target_map, key_map, config,
            )
            .unwrap(),
        )
    } else {
        Box::new(
            TransformMatchingTex::with_key_map(
                stage, source, target, source_map, target_map, key_map, config,
            )
            .unwrap(),
        )
    }
}

#[test]
fn renames_claim_real_target_handles_before_equal_keys() {
    let mut stage = Stage::new();
    let (source, sm, source_parts) = fixture(&mut stage, "xyx", 0.0);
    let (target, tm, target_parts) = fixture(&mut stage, "yxy", 10.0);
    stage.add_to_scene(source).unwrap();
    let animation = TransformMatchingTex::with_key_map(
        &mut stage,
        source,
        target,
        &sm,
        &tm,
        &[("x", "y"), ("y", "x")],
        TextMatchingConfig::default(),
    )
    .unwrap();
    assert_eq!(animation.animations().len(), 3);
    for (index, (a, b)) in [(0, 0), (2, 2), (1, 1)].into_iter().enumerate() {
        assert_eq!(
            animation.animations()[index].preflight_mobjects(),
            [source_parts[a], target_parts[b]]
        );
        assert_eq!(animation.animations()[index].state().config.name, "Transform");
    }
    assert_eq!(stage.roots(), &[source]);
}

#[test]
fn unequal_expression_alignment_wraps_both_sides_without_regrouping_native_roots() {
    for (from, to) in [("ab", "z"), ("z", "uvw"), ("abc", "xy")] {
        let mut stage = Stage::new();
        let (source, sm, source_parts) = fixture(&mut stage, from, 0.0);
        let (target, tm, target_parts) = fixture(&mut stage, to, 10.0);
        let animation = TransformMatchingTex::with_key_map(
            &mut stage,
            source,
            target,
            &sm,
            &tm,
            &[(from, to)],
            TextMatchingConfig::default(),
        )
        .unwrap();
        assert_eq!(animation.animations().len(), 1);
        let operands = animation.animations()[0].preflight_mobjects();
        assert_eq!(operands.len(), 2);
        assert_eq!(stage.get(operands[0]).unwrap().submobjects(), source_parts);
        assert_eq!(stage.get(operands[1]).unwrap().submobjects(), target_parts);
        assert_eq!(stage.get(source).unwrap().submobjects(), source_parts);
        assert_eq!(stage.get(target).unwrap().submobjects(), target_parts);
        animation.validate_begin(&stage).unwrap();
    }
}

#[test]
fn mapped_play_and_skip_publish_original_target_once_and_preserve_its_ink() {
    for strings in [false, true] {
        for skip in [false, true] {
            for (from, to) in [("ab", "z"), ("z", "uvw"), ("αβ", "界")] {
                let mut stage = Stage::new();
                let (other, _, _) = fixture(&mut stage, "keep", -10.0);
                let (source, sm, _) = fixture(&mut stage, from, 0.0);
                let (target, tm, target_parts) = fixture(&mut stage, to, 10.0);
                stage.add_many_to_scene(&[other, source]).unwrap();
                let expected_ink: Vec<_> = target_parts
                    .iter()
                    .map(|&mob| stage.get_points(mob))
                    .collect();
                let animation = mapped(
                    &mut stage, source, target, &sm, &tm, &[(from, to)], strings,
                );
                let mut animations = vec![animation];
                let mut clock = RationalFrameClock::new(24).unwrap();
                let mut emitted = 0;
                play_segment(
                    &mut stage,
                    &mut clock,
                    &RngRoot::from_seed(17),
                    &mut animations,
                    skip,
                    &mut |_| emitted += 1,
                )
                .unwrap();
                assert_eq!(clock.now().frames(), 12);
                assert_eq!(emitted, if skip { 0 } else { 12 });
                assert_eq!(stage.roots(), &[other, target]);
                assert_eq!(
                    target_parts
                        .iter()
                        .map(|&mob| stage.get_points(mob))
                        .collect::<Vec<_>>(),
                    expected_ink
                );
                assert!(animations[0].deferred_error().is_none());
                let completed = frame_facts(&stage);
                animations[0].finish(&mut stage);
                animations[0].clean_up_from_scene(&mut stage);
                assert_eq!(frame_facts(&stage), completed);
            }
        }
    }
}

#[test]
fn invalid_mapping_has_no_arena_side_effects() {
    let mut stage = Stage::new();
    let (source, sm, source_parts) = fixture(&mut stage, "x", 0.0);
    let (target, tm, target_parts) = fixture(&mut stage, "y", 10.0);
    stage.add_to_scene(source).unwrap();
    for mappings in [
        vec![("", "y")],
        vec![("x", "")],
        vec![("x", "y"), ("x", "z")],
    ] {
        let epoch = stage.topology_epoch();
        assert!(matches!(
            TransformMatchingTex::with_key_map(
                &mut stage,
                source,
                target,
                &sm,
                &tm,
                &mappings,
                TextMatchingConfig::default(),
            ),
            Err(TextMatchingError::InvalidKeyMap { .. })
        ));
        assert_eq!(stage.topology_epoch(), epoch);
        assert_eq!(stage.roots(), &[source]);
        assert_eq!(stage.get(source).unwrap().submobjects(), source_parts);
        assert_eq!(stage.get(target).unwrap().submobjects(), target_parts);
    }
}

#[test]
fn actual_tex_fraction_substitution_uses_complete_native_provenance() {
    let engine = TexEngine::new("fmd-math/pack/default", None).unwrap();
    let a = Tex::new(r"\frac{x}{y}").build(&engine).unwrap();
    let b = Tex::new(r"\alpha").build(&engine).unwrap();
    let (sm, tm) = (a.span_map(), b.span_map());
    let mut stage = Stage::new();
    let (source, target) = (stage.add(a), stage.add(b));
    stage.add_to_scene(source).unwrap();
    let animation = TransformMatchingTex::with_key_map(
        &mut stage,
        source,
        target,
        &sm,
        &tm,
        &[(r"\frac{x}{y}", r"\alpha")],
        TextMatchingConfig {
            run_time: 0.25,
            ..TextMatchingConfig::default()
        },
    )
    .unwrap();
    assert_eq!(animation.animations().len(), 1);
    assert_eq!(animation.animations()[0].state().config.name, "Transform");
    let mut animations: Vec<Box<dyn Animation>> = vec![Box::new(animation)];
    play_segment(
        &mut stage,
        &mut RationalFrameClock::new(24).unwrap(),
        &RngRoot::from_seed(17),
        &mut animations,
        false,
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(stage.roots(), &[target]);
    assert!(animations[0].deferred_error().is_none());
}

fn frame_facts(stage: &Stage) -> Vec<String> {
    let mut facts = vec![format!(
        "time {:x}, roots {:?}",
        stage.time().to_bits(),
        stage.roots()
    )];
    for &root in stage.roots() {
        let family = stage.family(root);
        facts.push(format!("family {family:?}"));
        for mob in family {
            let entry = stage.get(mob).unwrap();
            facts.push(format!(
                "{:?}/{:?}/{:?}",
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
    }
    facts
}

#[test]
fn mapped_worker_frames_match_serial_under_reordered_evaluation() {
    for strings in [false, true] {
        let mut initial = Stage::new();
        let (other, _, _) = fixture(&mut initial, "keep", -10.0);
        let (source, sm, _) = fixture(&mut initial, "AB+Q?", 0.0);
        let (target, tm, _) = fixture(&mut initial, "z+RST!", 10.0);
        initial.add_many_to_scene(&[other, source]).unwrap();
        initial.scale_about(source, 0.75, Some([0.2, 0.3, 0.4]), None);
        initial.shift(target, [0.2, -0.5, 0.3]);
        let baseline = initial.snapshot();
        let rng = RngRoot::from_seed(71);
        let mut expected = None;
        for workers in [0, 1, 4, 16] {
            let mut stage = baseline.materialize();
            let mut animation = mapped(
                &mut stage,
                source,
                target,
                &sm,
                &tm,
                &[("AB", "z"), ("Q", "RST")],
                strings,
            );
            animation.state_mut().config.time_span = Some((0.03125, 0.4375));
            animation.state_mut().config.final_alpha_value = 0.35;
            let mut animations = vec![animation];
            let mut clock = RationalFrameClock::new(24).unwrap();
            clock.advance_frames(5).unwrap();
            stage.set_time_from_clock(clock.now().to_f64());
            let frames = if workers == 0 {
                let mut frames = Vec::new();
                play_segment(
                    &mut stage,
                    &mut clock,
                    &rng,
                    &mut animations,
                    false,
                    &mut |packet| frames.push(frame_facts(&packet.materialize_stage())),
                )
                .unwrap();
                frames
            } else {
                let open = open_play(&mut stage, &clock, &mut animations).unwrap();
                let program = PureSegment::freeze(&stage, &clock, &rng, &animations, &open)
                    .expect("native substitutions must freeze their prepared Transform groups");
                let owner_before = frame_facts(&stage);
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
                                        (frame, frame_facts(&program.job(frame).unwrap().materialize()))
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
                assert_eq!(frame_facts(&stage), owner_before);
                indexed.sort_by_key(|(index, _)| *index);
                program
                    .finish_on_owner(&mut stage, &mut clock, &mut animations, open)
                    .unwrap();
                indexed.into_iter().map(|(_, facts)| facts).collect()
            };
            assert_eq!(frames.len(), 12);
            assert_eq!(clock.now().frames(), 17);
            assert_eq!(stage.roots(), &[other, target]);
            assert!(!stage.is_animating(source));
            assert!(!stage.is_updating_suspended(source));
            assert!(animations[0].freeze_interpolation().is_none());
            assert!(animations[0].deferred_error().is_none());
            let observed = (frames, frame_facts(&stage));
            if let Some(expected) = &expected {
                assert_eq!(&observed, expected, "strings={strings}, workers={workers}");
            } else {
                expected = Some(observed);
            }
        }
    }
}
