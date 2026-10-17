//! Native matching uses source provenance and the ordinary Choreo lifecycle.
use std::sync::Arc;

use fmn_anim::pure_frame::{PureFrameRefusal, PureSegment};
use fmn_anim::{
    AnimError, Animation, RateFunc, RationalFrameClock, Transform, finish_open_play, open_play,
    play_segment,
};
use fmn_core::rng::RngRoot;
use fmn_library::{
    FontBook, SpanKindU8, SpanMapData, SpanMapEntry, Tex, TexEngine, Text, TextMatchingConfig,
    TextMatchingError, TransformMatchingStrings, TransformMatchingTex,
};
use fmn_mobject::{Mob, Mobject, Stage, StageError};

fn fixture(stage: &mut Stage, text: &str, x: f64) -> (Mob, SpanMapData, Vec<Mob>) {
    let root = stage.add(Mobject::new());
    let mut children = Vec::new();
    let mut entries = Vec::new();
    for (index, (start, ch)) in text.char_indices().enumerate() {
        #[allow(clippy::cast_precision_loss)]
        let dx = x + index as f64;
        // Every key deliberately has the SAME outline. Shape matching would
        // incorrectly transform unrelated source slices instead of fading them.
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

fn names(animation: &TransformMatchingTex) -> Vec<&str> {
    animation
        .animations()
        .iter()
        .map(|a| a.state().config.name.as_str())
        .collect()
}

#[test]
fn tex_matches_occurrences_not_outlines_and_does_not_root_target_early() {
    let mut stage = Stage::new();
    let (source, sm, s) = fixture(&mut stage, "xx+", 0.0);
    let (target, tm, t) = fixture(&mut stage, "x-", 10.0);
    stage.add_to_scene(source).unwrap();
    let animation = TransformMatchingTex::new(&mut stage, source, target, &sm, &tm).unwrap();
    assert_eq!(
        names(&animation),
        [
            "Transform",
            "FadeOutToPoint",
            "FadeOutToPoint",
            "FadeInFromPoint"
        ]
    );
    assert_eq!(animation.animations()[0].preflight_mobjects(), [s[0], t[0]]);
    assert_eq!(animation.animations()[1].state().mobject(), s[1]);
    assert_eq!(animation.animations()[3].state().mobject(), t[1]);
    assert_eq!(stage.roots(), &[source]);
    assert_eq!(animation.get_run_time(), 2.0);
    assert!(animation.preflight_mobjects().contains(&target));
}

#[test]
fn strings_move_reordered_runs_as_whole_blocks() {
    let mut stage = Stage::new();
    let (source, sm, s) = fixture(&mut stage, "ABCD", 0.0);
    let (target, tm, t) = fixture(&mut stage, "CDAB", 10.0);
    let animation = TransformMatchingStrings::new(&mut stage, source, target, &sm, &tm).unwrap();
    assert_eq!(animation.animations().len(), 2);
    for (index, (from, to)) in [(&s[..2], &t[2..]), (&s[2..], &t[..2])].iter().enumerate() {
        let pair = animation.animations()[index].preflight_mobjects();
        assert_eq!(stage.get(pair[0]).unwrap().submobjects(), *from);
        assert_eq!(stage.get(pair[1]).unwrap().submobjects(), *to);
    }
}

#[test]
fn consuming_a_block_does_not_invent_adjacency_across_its_old_position() {
    let mut stage = Stage::new();
    let (source, sm, _) = fixture(&mut stage, "AXB", 0.0);
    let (target, tm, _) = fixture(&mut stage, "XAB", 10.0);
    let animation = TransformMatchingStrings::new(&mut stage, source, target, &sm, &tm).unwrap();
    // All longest runs have length one. Deleting claimed slots would wrongly
    // create an AB block after consuming X.
    assert_eq!(animation.animations().len(), 3);
    assert!(animation.animations().iter().all(|a| {
        stage
            .get(a.state().mobject())
            .unwrap()
            .submobjects()
            .is_empty()
    }));
}

#[test]
fn playback_publishes_the_original_target_and_preserves_unrelated_roots() {
    let mut stage = Stage::new();
    let (other, _, _) = fixture(&mut stage, "keep", -10.0);
    let (source, sm, _) = fixture(&mut stage, "AB", 0.0);
    let (target, tm, t) = fixture(&mut stage, "AC", 10.0);
    stage.add_many_to_scene(&[other, source]).unwrap();
    let target_points: Vec<_> = t.iter().map(|&mob| stage.get_points(mob)).collect();
    let animation = TransformMatchingStrings::new(&mut stage, source, target, &sm, &tm).unwrap();
    let mut animations: Vec<Box<dyn Animation>> = vec![Box::new(animation)];
    let mut frames = 0;
    let report = play_segment(
        &mut stage,
        &mut RationalFrameClock::new(10).unwrap(),
        &RngRoot::from_seed(7),
        &mut animations,
        false,
        &mut |_| frames += 1,
    )
    .unwrap();
    assert_eq!(frames, 20);
    assert_eq!(report.n_frames, 20);
    assert!(report.purity.is_pure());
    assert_eq!(stage.roots(), &[other, target]);
    assert_eq!(
        t.iter()
            .map(|&mob| stage.get_points(mob))
            .collect::<Vec<_>>(),
        target_points
    );
    animations[0].clean_up_from_scene(&mut stage);
    assert_eq!(stage.roots(), &[other, target], "cleanup is idempotent");
}

#[test]
fn empty_operands_keep_replacement_and_clock_semantics_in_both_modes() {
    for skip in [false, true] {
        for (from, to) in [("", ""), ("", "A"), ("A", "")] {
            let mut stage = Stage::new();
            let (source, sm, _) = fixture(&mut stage, from, 0.0);
            let (target, tm, _) = fixture(&mut stage, to, 10.0);
            stage.add_to_scene(source).unwrap();
            let animation =
                TransformMatchingTex::new(&mut stage, source, target, &sm, &tm).unwrap();
            let mut animations: Vec<Box<dyn Animation>> = vec![Box::new(animation)];
            let mut clock = RationalFrameClock::new(10).unwrap();
            play_segment(
                &mut stage,
                &mut clock,
                &RngRoot::from_seed(7),
                &mut animations,
                skip,
                &mut |_| {},
            )
            .unwrap();
            assert_eq!(clock.now().frames(), 20);
            assert_eq!(stage.roots(), &[target]);
        }
    }
}

#[test]
fn topology_changes_after_planning_fail_before_begin_mutates_the_arena() {
    let mut stage = Stage::new();
    let (source, sm, s) = fixture(&mut stage, "AB", 0.0);
    let (target, tm, _) = fixture(&mut stage, "AB", 10.0);
    let mut animation = TransformMatchingTex::new(&mut stage, source, target, &sm, &tm).unwrap();
    stage.replace_children(source, &[s[1], s[0]]).unwrap();
    let epoch = stage.topology_epoch();
    assert_eq!(
        animation.begin(&mut stage),
        Err(AnimError::Stage(StageError::FamilyShapeMismatch))
    );
    assert_eq!(stage.topology_epoch(), epoch);
}

#[test]
fn malformed_utf8_spans_and_missing_maps_do_not_allocate_groups() {
    let mut stage = Stage::new();
    let (source, mut sm, _) = fixture(&mut stage, "é", 0.0);
    let (target, tm, _) = fixture(&mut stage, "é", 10.0);
    for invalid in [
        SpanMapEntry {
            start: 1,
            end: 2,
            kind: SpanKindU8::TextGlyph,
        },
        SpanMapEntry {
            start: 0,
            end: 99,
            kind: SpanKindU8::TextGlyph,
        },
        SpanMapEntry {
            start: 2,
            end: 1,
            kind: SpanKindU8::TextGlyph,
        },
    ] {
        sm.entries[0] = invalid;
        let epoch = stage.topology_epoch();
        assert!(matches!(
            TransformMatchingTex::new(&mut stage, source, target, &sm, &tm),
            Err(TextMatchingError::InvalidSpan {
                side: "source",
                index: 0
            })
        ));
        assert_eq!(stage.topology_epoch(), epoch);
    }
    sm.entries.clear();
    assert!(matches!(
        TransformMatchingTex::new(&mut stage, source, target, &sm, &tm),
        Err(TextMatchingError::InvalidLayout { .. })
    ));
}

#[test]
fn shared_members_are_refused_before_any_planning_side_effect() {
    let mut stage = Stage::new();
    let (source, sm, s) = fixture(&mut stage, "A", 0.0);
    let target = stage.add(Mobject::new());
    stage.attach(target, s[0]).unwrap();
    let epoch = stage.topology_epoch();
    assert!(matches!(
        TransformMatchingTex::new(&mut stage, source, target, &sm, &sm),
        Err(TextMatchingError::AliasedFamilies)
    ));
    assert_eq!(stage.topology_epoch(), epoch);
}

#[test]
fn decorations_not_in_the_glyph_map_are_faded_not_dropped() {
    let mut stage = Stage::new();
    let (source, sm, _) = fixture(&mut stage, "A", 0.0);
    let (target, tm, _) = fixture(&mut stage, "A", 10.0);
    let decoration = stage.add(Mobject::from_points(&[[0.0, -0.2, 0.0], [1.0, -0.2, 0.0]]));
    stage.attach(source, decoration).unwrap();
    let animation = TransformMatchingTex::new(&mut stage, source, target, &sm, &tm).unwrap();
    assert_eq!(names(&animation), ["Transform", "FadeOutToPoint"]);
    assert_eq!(animation.animations()[1].state().mobject(), decoration);
}

#[test]
fn actual_native_tex_and_unicode_text_maps_drive_matching_without_a_portal() {
    let mut stage = Stage::new();
    let engine = TexEngine::new("fmd-math/pack/default", None).unwrap();
    let a = Tex::new(r"x^2+1").build(&engine).unwrap();
    let b = Tex::new(r"x^3-1").build(&engine).unwrap();
    let (am, bm) = (a.span_map(), b.span_map());
    let (source, target) = (stage.add(a), stage.add(b));
    let animation = TransformMatchingTex::new(&mut stage, source, target, &am, &bm).unwrap();
    assert!(names(&animation).contains(&"Transform"));
    assert!(names(&animation).contains(&"FadeOutToPoint"));
    let book = FontBook::bundled().unwrap();
    let a = Text::new("éA").font("IBM Plex Sans").build(&book).unwrap();
    let b = Text::new("Aé").font("IBM Plex Sans").build(&book).unwrap();
    let (am, bm) = (a.span_map(), b.span_map());
    let (source, target) = (stage.add(a), stage.add(b));
    let mut animation =
        TransformMatchingStrings::new(&mut stage, source, target, &am, &bm).unwrap();
    assert_eq!(animation.animations().len(), 2);
    animation.begin(&mut stage).unwrap();
    animation.interpolate(&mut stage, 0.5);
    animation.finish(&mut stage);
    animation.clean_up_from_scene(&mut stage);
    assert!(stage.roots().contains(&target));
}

#[test]
fn explicit_timing_and_abort_use_shared_animation_lifecycle() {
    let mut stage = Stage::new();
    let (source, sm, _) = fixture(&mut stage, "AB", 0.0);
    let (target, tm, _) = fixture(&mut stage, "BC", 10.0);
    stage.add_to_scene(source).unwrap();
    let mut animation = TransformMatchingTex::with_config(
        &mut stage,
        source,
        target,
        &sm,
        &tm,
        TextMatchingConfig {
            run_time: 0.5,
            lag_ratio: 0.2,
            ..TextMatchingConfig::default()
        },
    )
    .unwrap();
    assert_eq!(animation.get_run_time(), 0.5);
    assert!(animation.freeze_interpolation().is_none());
    animation.begin(&mut stage).unwrap();
    assert!(animation.freeze_interpolation().is_some());
    animation.interpolate(&mut stage, 0.25);
    animation.abort(&mut stage);
    assert!(animation.freeze_interpolation().is_none());
    assert!(!stage.roots().contains(&target));
    animation.clean_up_from_scene(&mut stage);
    assert!(matches!(
        animation.deferred_error(),
        Some(AnimError::InvalidFramePhase(_))
    ));
}

/// Observe every rendered family field, placement and lifetime flag, not only
/// its final center. A worker must reproduce the serial sample bit for bit.
fn frame_facts(stage: &Stage) -> Vec<String> {
    let mut facts = vec![format!("time {:x}, roots {:?}", stage.time().to_bits(), stage.roots())];
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
fn matching_workers_preserve_each_sample_and_owner_replacement_cleanup() {
    for strings in [false, true] {
        for (from, to) in [
            ("ABXAB", "ABABY"),
            ("ABCD X", "CDAB Y"),
            ("", ""),
            ("", "A"),
            ("A", ""),
        ] {
            let mut initial = Stage::new();
            let (other, _, _) = fixture(&mut initial, "keep", -10.0);
            let (source, sm, _) = fixture(&mut initial, from, 0.0);
            let (target, tm, _) = fixture(&mut initial, to, 10.0);
            initial.add_many_to_scene(&[other, source]).unwrap();
            initial.scale_about(source, 0.75, Some([0.2, 0.3, 0.4]), None);
            initial.shift(target, [0.2, -0.5, 0.3]);
            let baseline = initial.snapshot();
            let rng = RngRoot::from_seed(71);
            let mut expected = None;

            for workers in [0, 1, 4, 16] {
                let mut stage = baseline.materialize();
                let config = TextMatchingConfig {
                    run_time: 0.5,
                    lag_ratio: 0.2,
                    // Revisit earlier phases; a worker cannot rely on any
                    // previously sampled animation or family state.
                    rate_func: RateFunc::Sampled(Arc::from([0.0, 0.9, 0.1, 0.8, 1.0])),
                };
                let mut animation: Box<dyn Animation> = if strings {
                    Box::new(TransformMatchingStrings::with_config(
                        &mut stage, source, target, &sm, &tm, config,
                    ).unwrap())
                } else {
                    Box::new(TransformMatchingTex::with_config(
                        &mut stage, source, target, &sm, &tm, config,
                    ).unwrap())
                };
                animation.state_mut().config.time_span = Some((0.03125, 0.4375));
                animation.state_mut().config.final_alpha_value = 0.35;
                assert!(animation.freeze_interpolation().is_none());
                let mut animations = vec![animation];
                let mut clock = RationalFrameClock::new(24).unwrap();
                clock.advance_frames(5).unwrap();
                stage.set_time_from_clock(clock.now().to_f64());
                let frames = if workers == 0 {
                    let mut frames = Vec::new();
                    play_segment(&mut stage, &mut clock, &rng, &mut animations, false, &mut |packet| {
                        frames.push(frame_facts(&packet.materialize_stage()));
                    }).unwrap();
                    frames
                } else {
                    let open = open_play(&mut stage, &clock, &mut animations).unwrap();
                    let program = PureSegment::freeze(&stage, &clock, &rng, &animations, &open)
                        .expect("prepared text matching must offload its native group");
                    let owner_before = frame_facts(&stage);
                    let mut indexed = std::thread::scope(|scope| {
                        let handles: Vec<_> = (0..workers).map(|worker| {
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
                        }).collect();
                        handles.into_iter().flat_map(|handle| handle.join().unwrap()).collect::<Vec<_>>()
                    });
                    assert_eq!(frame_facts(&stage), owner_before, "workers mutated the scene owner");
                    indexed.sort_by_key(|(index, _)| *index);
                    program.finish_on_owner(&mut stage, &mut clock, &mut animations, open).unwrap();
                    indexed.into_iter().map(|(_, facts)| facts).collect()
                };
                assert_eq!(frames.len(), 12);
                assert_eq!(clock.now().frames(), 17);
                assert_eq!(stage.roots(), &[other, target]);
                assert!(!stage.is_animating(source));
                assert!(!stage.is_updating_suspended(source));
                assert!(animations[0].freeze_interpolation().is_none());
                let after = frame_facts(&stage);
                animations[0].finish(&mut stage);
                animations[0].clean_up_from_scene(&mut stage);
                assert_eq!(frame_facts(&stage), after, "finish and cleanup repeat work");
                let observed = (frames, after);
                if let Some(expected) = &expected {
                    assert_eq!(&observed, expected, "strings={strings}, {from:?}->{to:?}, workers={workers}");
                } else {
                    expected = Some(observed);
                }
            }
        }
    }
}

#[test]
fn matching_offload_retains_callback_view_and_alias_refusals() {
    fn custom_rate(alpha: f64) -> f64 { alpha * alpha }
    for case in 0..6 {
        let mut stage = Stage::new();
        let (source, sm, _) = fixture(&mut stage, "AB", 0.0);
        // Equal object-space geometry keeps the actual target as the
        // interpolation endpoint, so the alias case cannot be hidden by
        // Transform's legitimate private geometry-baking copy.
        let (target, tm, target_children) = fixture(&mut stage, "AB", 0.0);
        stage.shift(target, [10.0, 0.0, 0.0]);
        stage.add_to_scene(source).unwrap();
        let mut view = None;
        let mut config = TextMatchingConfig { run_time: 0.25, ..TextMatchingConfig::default() };
        match case {
            0 => { stage.add_dt_updater(source, |_, _, _| {}, false).unwrap(); }
            1 => { stage.add_updater(target, |_, _| {}, false).unwrap(); }
            2 => { view = Some(stage.get_mut(target).unwrap().buffer.export_view(false)); }
            3 => { config.rate_func = RateFunc::Base(custom_rate); }
            _ => {}
        }
        let animation = TransformMatchingTex::with_config(&mut stage, source, target, &sm, &tm, config).unwrap();
        let mut animations: Vec<Box<dyn Animation>> = vec![Box::new(animation)];
        if case == 4 {
            let changed = stage.copy_family(target_children[0]).unwrap();
            stage.shift(changed, [1.0, 0.0, 0.0]);
            animations.push(Box::new(Transform::new(target_children[0], changed)));
        } else if case == 5 {
            let source_child = stage.get(source).unwrap().submobjects()[0];
            let changed = stage.copy_family(source_child).unwrap();
            stage.shift(changed, [1.0, 0.0, 0.0]);
            animations.push(Box::new(Transform::new(source_child, changed)));
        }
        let clock = RationalFrameClock::new(24).unwrap();
        let open = open_play(&mut stage, &clock, &mut animations).unwrap();
        let refusal = PureSegment::freeze(&stage, &clock, &RngRoot::from_seed(3), &animations, &open).unwrap_err();
        match case {
            0 | 1 => assert_eq!(refusal, PureFrameRefusal::NotAtPureBegin),
            2 => assert_eq!(refusal, PureFrameRefusal::LiveView(target)),
            3 => assert_eq!(refusal, PureFrameRefusal::UnsupportedAnimation(0)),
            4 => assert_eq!(refusal, PureFrameRefusal::MutableEndpoint(target_children[0])),
            _ => assert!(matches!(refusal, PureFrameRefusal::OverlappingOutput(_))),
        }
        drop(view);
        finish_open_play(&mut stage, &mut animations, open).unwrap();
    }
}
