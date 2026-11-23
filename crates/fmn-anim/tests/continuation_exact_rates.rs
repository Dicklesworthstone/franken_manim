//! Durable exact rates must preserve the owner's actual native sampling contract.
//! Rehearsal, continuous playback and fresh-arena continuation share no callbacks.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use fmn_anim::continuation::{capture, restore};
use fmn_anim::rate_table::RateSampler;
use fmn_anim::{
    AnimConfig, AnimError, Animation, AnimationGroup, FrameMetadata, FramePacket, OpenSegment,
    RateFunc, RationalFrameClock, Transform, abort_open_play, advance_play, finish_open_play,
    open_play, play_segment,
};
use fmn_core::rng::RngRoot;
use fmn_mobject::{Mob, Mobject, Snapshot, Stage};

const CHECKPOINT_BUDGET: usize = 1024 * 1024;

#[derive(Debug, PartialEq)]
struct FrameFacts {
    metadata: FrameMetadata,
    alpha_bits: u64,
    stage_time_bits: u64,
    rng_sample: u64,
    snapshot: Vec<u8>,
}

fn frame_facts(packet: FramePacket) -> FrameFacts {
    let stage = packet.materialize_stage();
    FrameFacts {
        metadata: packet.metadata(),
        alpha_bits: packet.alpha().to_bits(),
        stage_time_bits: stage.time().to_bits(),
        rng_sample: packet.rng_fork("durable-exact-rates").next_u64(),
        snapshot: stage.snapshot().to_bytes().unwrap(),
    }
}

fn family_transform(stage: &mut Stage, x: f64, config: AnimConfig) -> Box<dyn Animation> {
    let source = stage.add(Mobject::from_points(&[[x, 0.0, 0.0], [x + 1.0, 1.0, 0.0]]));
    for y in [2.0, 4.0] {
        let child = stage.add(Mobject::from_points(&[
            [x, y, 0.0],
            [x + 0.5, y + 1.0, 0.5],
        ]));
        stage.attach(source, child).unwrap();
    }
    stage.add_to_scene(source).unwrap();
    let target = stage.copy_family(source).unwrap();
    stage.shift(target, [2.0, -1.0, 0.5]);
    Box::new(Transform::new(source, target).with_config(config))
}

fn lifecycle_handles(stage: &Stage, animations: &[Box<dyn Animation>]) -> Vec<(u32, u32)> {
    animations
        .iter()
        .flat_map(|animation| stage.family(animation.state().mobject()))
        .map(|mob| stage.snapshot_handle(mob).unwrap())
        .collect()
}

fn assert_released(stage: &Stage, handles: &[(u32, u32)]) {
    for &(slot, generation) in handles {
        let mob = stage.resolve_snapshot_handle(slot, generation).unwrap();
        assert!(!stage.is_animating(mob), "slot {slot} remains animating");
        assert!(
            !stage.is_updating_suspended(mob),
            "slot {slot} remains suspended"
        );
    }
}

fn is_visible(stage: &Stage, mob: Mob) -> bool {
    stage
        .roots()
        .iter()
        .any(|&root| stage.family(root).contains(&mob))
}

struct Fixture {
    stage: Stage,
    animations: Vec<Box<dyn Animation>>,
    lifecycle: Vec<(u32, u32)>,
    remover: (u32, u32),
}

impl Fixture {
    fn assert_finished(&self) {
        assert_released(&self.stage, &self.lifecycle);
        let removed = self
            .stage
            .resolve_snapshot_handle(self.remover.0, self.remover.1)
            .unwrap();
        assert!(!is_visible(&self.stage, removed));
    }
}

fn fixture(nested: bool, frame_native: bool, rate: RateFunc) -> Fixture {
    let mut stage = Stage::new();
    let first = family_transform(
        &mut stage,
        -4.0,
        AnimConfig {
            run_time: 0.43,
            run_time_frames: frame_native.then_some(13),
            time_span: Some((0.03, 0.37)),
            lag_ratio: 0.3,
            rate_func: rate.clone(),
            final_alpha_value: 0.41,
            suspend_mobject_updating: true,
            ..AnimConfig::default()
        },
    );
    let second = family_transform(
        &mut stage,
        0.0,
        AnimConfig {
            run_time: 0.31,
            run_time_frames: frame_native.then_some(10),
            lag_ratio: 0.2,
            rate_func: rate.clone().squish(0.1, 0.9).not_quite_there(0.8),
            final_alpha_value: 0.67,
            remover: true,
            suspend_mobject_updating: true,
            ..AnimConfig::default()
        },
    );
    let remover = stage.snapshot_handle(second.state().mobject()).unwrap();
    let animations: Vec<Box<dyn Animation>> = if nested {
        let mut inner = AnimationGroup::with_lag_ratio(&mut stage, vec![first, second], 0.35)
            .unwrap()
            .with_duration_scale(1.3)
            .with_rate_func(rate.clone().squish(0.15, 0.85));
        inner.state_mut().config.time_span = Some((0.05, 0.55));
        let third = family_transform(
            &mut stage,
            4.0,
            AnimConfig {
                // This member stays seconds-native inside a frame-native group.
                run_time: 0.24,
                lag_ratio: 0.45,
                rate_func: rate.clone().not_quite_there(0.7).squish(0.05, 0.95),
                final_alpha_value: 0.53,
                suspend_mobject_updating: true,
                ..AnimConfig::default()
            },
        );
        let outer = AnimationGroup::with_lag_ratio(&mut stage, vec![Box::new(inner), third], 0.25)
            .unwrap()
            .with_rate_func(rate.not_quite_there(0.9));
        let outer = if frame_native {
            outer.with_run_time_frames(23)
        } else {
            // The last nominal sample overshoots this non-grid duration.
            outer.with_run_time(0.73)
        };
        vec![Box::new(outer)]
    } else {
        vec![first, second]
    };
    let lifecycle = lifecycle_handles(&stage, &animations);
    Fixture {
        stage,
        animations,
        lifecycle,
        remover,
    }
}

fn starting_clock(stage: &mut Stage) -> RationalFrameClock {
    let mut clock = RationalFrameClock::new(30).unwrap();
    clock.advance_frames(7).unwrap();
    stage.set_time_from_clock(clock.now().to_f64());
    clock
}

fn restore_stage(snapshot: &[u8], clock: &RationalFrameClock) -> Stage {
    let mut stage = Stage::new();
    let decoded = Snapshot::from_bytes(snapshot, &stage).unwrap();
    stage.restore(&decoded.snapshot);
    // The arena document intentionally excludes the runtime clock.
    stage.set_time_from_clock(clock.now().to_f64());
    stage
}

#[test]
fn durable_exact_rates_preserve_native_rehearsals_and_every_continuation_suffix() {
    let rng = RngRoot::from_seed(0xeca7);
    for nested in [false, true] {
        for frame_native in [false, true] {
            let calls = Arc::new(AtomicUsize::new(0));
            let observed = Arc::clone(&calls);
            let sampler = RateSampler::new(4096, move |alpha| {
                observed.fetch_add(1, Ordering::SeqCst);
                // Nonlinear, with a discontinuity that interpolation between
                // samples or replacement with a catalog curve would erase.
                Ok(if alpha < 0.375 {
                    alpha * alpha
                } else {
                    0.25 + 0.75 * alpha * alpha
                })
            });
            let mut rehearsal = fixture(
                nested,
                frame_native,
                RateFunc::Preparing(Arc::clone(&sampler)),
            );
            let mut rehearsal_clock = starting_clock(&mut rehearsal.stage);
            let mut expected_frames = Vec::new();
            let report = play_segment(
                &mut rehearsal.stage,
                &mut rehearsal_clock,
                &rng,
                &mut rehearsal.animations,
                false,
                &mut |packet| expected_frames.push(frame_facts(packet)),
            )
            .unwrap();
            rehearsal.assert_finished();
            let expected_final = rehearsal.stage.snapshot().to_bytes().unwrap();
            assert!(report.n_frames > 3);
            assert_eq!(
                expected_frames.len(),
                usize::try_from(report.n_frames).unwrap()
            );
            assert_ne!(
                expected_frames.last().unwrap().snapshot,
                expected_final,
                "finish must execute final-alpha, remover and lifecycle cleanup"
            );

            // Only the complete native lifecycle decides which inputs belong
            // in this table: begin, nominal frames, nested timing and finish.
            let exact = sampler.finish().unwrap();
            let prepared_calls = calls.load(Ordering::SeqCst);
            assert!(prepared_calls > 2);
            drop(rehearsal);
            drop(sampler);

            for cut in 0..5 {
                let mut serial = fixture(nested, frame_native, exact.clone());
                let mut clock = starting_clock(&mut serial.stage);
                let mut open =
                    open_play(&mut serial.stage, &clock, &mut serial.animations).unwrap();
                let total = open.report().n_frames;
                assert_eq!(total, report.n_frames);
                let boundary = [0, 1, total / 2, total - 1, total][cut];
                let split = usize::try_from(boundary).unwrap();
                let mut prefix = Vec::new();
                advance_play(
                    &mut serial.stage,
                    &mut clock,
                    &rng,
                    &mut serial.animations,
                    &mut open,
                    boundary,
                    &mut |packet| prefix.push(frame_facts(packet)),
                )
                .unwrap();
                assert_eq!(
                    prefix.as_slice(),
                    &expected_frames[..split],
                    "nested={nested}, frames={frame_native}, boundary={boundary}: prefix"
                );

                let checkpoint = capture(
                    &serial.stage,
                    &clock,
                    &serial.animations,
                    &open,
                    CHECKPOINT_BUDGET,
                )
                .unwrap()
                .expect("a fully prepared exact rate has a durable native lifecycle");
                let snapshot = serial.stage.snapshot().to_bytes().unwrap();
                let mut restored = restore_stage(&snapshot, &clock);
                let (mut resumed, mut resumed_open) =
                    restore(&restored, &clock, &checkpoint).unwrap();
                let mut resumed_clock = clock;
                assert_eq!(restored.snapshot().to_bytes().unwrap(), snapshot);
                assert_eq!(
                    capture(
                        &restored,
                        &resumed_clock,
                        &resumed,
                        &resumed_open,
                        CHECKPOINT_BUDGET,
                    )
                    .unwrap()
                    .unwrap(),
                    checkpoint,
                    "native exact-rate tables must round trip byte for byte"
                );

                let mut direct_suffix = Vec::new();
                advance_play(
                    &mut serial.stage,
                    &mut clock,
                    &rng,
                    &mut serial.animations,
                    &mut open,
                    total,
                    &mut |packet| direct_suffix.push(frame_facts(packet)),
                )
                .unwrap();
                let mut resumed_suffix = Vec::new();
                advance_play(
                    &mut restored,
                    &mut resumed_clock,
                    &rng,
                    &mut resumed,
                    &mut resumed_open,
                    total,
                    &mut |packet| resumed_suffix.push(frame_facts(packet)),
                )
                .unwrap();
                assert_eq!(
                    direct_suffix.as_slice(),
                    &expected_frames[split..],
                    "nested={nested}, frames={frame_native}, boundary={boundary}: native oracle"
                );
                assert_eq!(
                    resumed_suffix, direct_suffix,
                    "nested={nested}, frames={frame_native}, boundary={boundary}: restored suffix"
                );

                finish_open_play(&mut serial.stage, &mut serial.animations, open).unwrap();
                finish_open_play(&mut restored, &mut resumed, resumed_open).unwrap();
                serial.assert_finished();
                assert_released(&restored, &serial.lifecycle);
                let removed = restored
                    .resolve_snapshot_handle(serial.remover.0, serial.remover.1)
                    .unwrap();
                assert!(!is_visible(&restored, removed));
                assert_eq!(serial.stage.snapshot().to_bytes().unwrap(), expected_final);
                assert_eq!(restored.snapshot().to_bytes().unwrap(), expected_final);
                assert_eq!(clock.now(), rehearsal_clock.now());
                assert_eq!(resumed_clock.now(), rehearsal_clock.now());
                assert_eq!(
                    restored.time().to_bits(),
                    rehearsal_clock.now().to_f64().to_bits()
                );
            }
            assert_eq!(
                calls.load(Ordering::SeqCst),
                prepared_calls,
                "exact playback and restore must never invoke the authored callback"
            );
        }
    }
}

fn missing_input_refuses_before_capture(
    stage: &mut Stage,
    clock: &mut RationalFrameClock,
    rng: &RngRoot,
    animations: &mut [Box<dyn Animation>],
    open: &mut OpenSegment,
) -> AnimError {
    let before = stage.snapshot().to_bytes().unwrap();
    let time = clock.now();
    let stage_time = stage.time().to_bits();
    let stepped = open.stepped();
    let mut captured = 0;
    let error = advance_play(stage, clock, rng, animations, open, 1, &mut |_| {
        captured += 1;
    })
    .unwrap_err();
    assert!(error.to_string().contains("unprepared rate input"));
    assert_eq!(captured, 0, "a refused sample must never reach capture");
    assert_eq!(clock.now(), time);
    assert_eq!(stage.time().to_bits(), stage_time);
    assert_eq!(open.stepped(), stepped);
    assert_eq!(
        stage.snapshot().to_bytes().unwrap(),
        before,
        "the group's missing rate input must fail before any child mutation"
    );
    error
}

#[test]
fn incomplete_exact_group_rates_round_trip_without_inventing_intermediate_samples() {
    let rng = RngRoot::from_seed(0xeca7);
    for frame_native in [false, true] {
        let sampler = RateSampler::new(2, Ok);
        let preparing = RateFunc::Preparing(Arc::clone(&sampler));
        assert_eq!(preparing.eval(0.0), 0.0);
        assert_eq!(preparing.eval(1.0), 1.0);
        let mut stage = Stage::new();
        let child = family_transform(
            &mut stage,
            0.0,
            AnimConfig {
                run_time: 1.0,
                run_time_frames: frame_native.then_some(2),
                rate_func: RateFunc::linear(),
                remover: true,
                suspend_mobject_updating: true,
                ..AnimConfig::default()
            },
        );
        let source = stage.snapshot_handle(child.state().mobject()).unwrap();
        let group = AnimationGroup::new(&mut stage, vec![child])
            .unwrap()
            .with_rate_func(sampler.finish().unwrap());
        let mut animations: Vec<Box<dyn Animation>> = vec![Box::new(group)];
        let lifecycle = lifecycle_handles(&stage, &animations);
        let mut clock = RationalFrameClock::new(2).unwrap();
        clock.advance_frames(3).unwrap();
        stage.set_time_from_clock(clock.now().to_f64());
        let mut open = open_play(&mut stage, &clock, &mut animations).unwrap();
        assert_eq!(open.report().n_frames, 2);
        let checkpoint = capture(&stage, &clock, &animations, &open, CHECKPOINT_BUDGET)
            .unwrap()
            .expect("incomplete exact tables are structurally valid until a missing lookup");
        let snapshot = stage.snapshot().to_bytes().unwrap();
        let mut restored = restore_stage(&snapshot, &clock);
        let (mut resumed, mut resumed_open) = restore(&restored, &clock, &checkpoint).unwrap();
        let mut resumed_clock = clock;
        assert_eq!(restored.snapshot().to_bytes().unwrap(), snapshot);
        assert!(resumed[0].state().config.rate_func.error().is_none());
        assert_eq!(
            capture(
                &restored,
                &resumed_clock,
                &resumed,
                &resumed_open,
                CHECKPOINT_BUDGET,
            )
            .unwrap()
            .unwrap(),
            checkpoint
        );

        let direct_error = missing_input_refuses_before_capture(
            &mut stage,
            &mut clock,
            &rng,
            &mut animations,
            &mut open,
        );
        // Failure in the original table must not poison the independently
        // decoded table, nor may decode fill its absent intermediate input.
        assert!(resumed[0].state().config.rate_func.error().is_none());
        let restored_error = missing_input_refuses_before_capture(
            &mut restored,
            &mut resumed_clock,
            &rng,
            &mut resumed,
            &mut resumed_open,
        );
        assert_eq!(restored_error, direct_error);

        let _ = abort_open_play(&mut stage, &mut animations, open);
        let _ = abort_open_play(&mut restored, &mut resumed, resumed_open);
        assert_released(&stage, &lifecycle);
        assert_released(&restored, &lifecycle);
        for arena in [&stage, &restored] {
            let mob = arena.resolve_snapshot_handle(source.0, source.1).unwrap();
            assert!(
                is_visible(arena, mob),
                "abort must not execute remover cleanup"
            );
        }
        assert_eq!(
            restored.snapshot().to_bytes().unwrap(),
            stage.snapshot().to_bytes().unwrap()
        );
    }
}
