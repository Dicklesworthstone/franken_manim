//! Prepared successions resume their remaining lifecycle without replaying begin.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use fmn_anim::continuation::{capture, restore};
use fmn_anim::rate_table::RateSampler;
use fmn_anim::{
    AnimConfig, AnimError, AnimState, Animation, AnimationGroup, AnimationSignature, FrameMetadata,
    FramePacket, RateFunc, RationalFrameClock, Succession, Transform, abort_open_play,
    advance_play, complete_play_frame, finish_open_play, open_play, play_segment,
    prepare_play_frame,
};
use fmn_core::rate;
use fmn_core::rng::RngRoot;
use fmn_hash::{Limits, Reader, Schema, UnknownPolicy, Writer};
use fmn_mobject::{Mob, Mobject, Snapshot, Stage};

const BUDGET: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug)]
enum Nesting {
    Plain,
    Succession,
    EagerGroup,
    FutureSuccession,
    FutureEagerGroup,
}

#[derive(Debug, PartialEq)]
struct FrameFacts {
    metadata: FrameMetadata,
    alpha: u64,
    stage_time: u64,
    rng_sample: u64,
    snapshot: Vec<u8>,
}

fn frame_facts(packet: FramePacket) -> FrameFacts {
    let stage = packet.materialize_stage();
    FrameFacts {
        metadata: packet.metadata(),
        alpha: packet.alpha().to_bits(),
        stage_time: stage.time().to_bits(),
        rng_sample: packet.rng_fork("succession-continuation").next_u64(),
        snapshot: stage.snapshot().to_bytes().unwrap(),
    }
}

fn source_family(stage: &mut Stage, x: f64) -> Mob {
    let root = stage.add(Mobject::from_points(&[
        [x - 0.5, -0.5, 0.0],
        [x + 0.5, 0.5, 0.0],
    ]));
    let child = stage.add(Mobject::from_points(&[
        [x - 0.25, 1.0, 0.0],
        [x + 0.25, 1.5, 0.5],
    ]));
    stage.attach(root, child).unwrap();
    stage.add_to_scene(root).unwrap();
    root
}

fn transform_to(
    stage: &mut Stage,
    source: Mob,
    x: f64,
    config: AnimConfig,
) -> (Box<dyn Animation>, Mob) {
    let target = stage.copy_family(source).unwrap();
    stage.shift(target, [x - stage.get_center(target)[0], 0.0, 0.0]);
    (
        Box::new(Transform::new(source, target).with_config(config)),
        target,
    )
}

struct Fixture {
    stage: Stage,
    animations: Vec<Box<dyn Animation>>,
    lifecycle: Vec<(u32, u32)>,
    removed: (u32, u32),
    future_target: (u32, u32),
    unrelated: (u32, u32),
}

fn fixture(reused: bool, nesting: Nesting, exact_frames: bool, rate: RateFunc) -> Fixture {
    let mut stage = Stage::new();
    let first = source_family(&mut stage, 0.0);
    let sources = if reused {
        [first; 3]
    } else {
        [
            first,
            source_family(&mut stage, 10.0),
            source_family(&mut stage, -10.0),
        ]
    };
    let destinations = if reused {
        [2.0, 6.0, -2.0]
    } else {
        [2.0, 14.0, -7.0]
    };
    let mut children = Vec::new();
    let mut future_target = None;
    for index in 0..3 {
        let (animation, target) = transform_to(
            &mut stage,
            sources[index],
            destinations[index],
            AnimConfig {
                run_time: [0.37, 0.53, 0.23][index],
                run_time_frames: exact_frames.then_some([3, 5, 2][index]),
                rate_func: RateFunc::linear(),
                final_alpha_value: [0.7, 0.4, 0.6][index],
                suspend_mobject_updating: true,
                remover: index == 2,
                ..AnimConfig::default()
            },
        );
        children.push(animation);
        future_target = Some(stage.snapshot_handle(target).unwrap());
    }
    let sequence: Box<dyn Animation> = match nesting {
        Nesting::Plain => Box::new(
            Succession::new(&mut stage, children)
                .unwrap()
                .with_rate_func(rate),
        ),
        Nesting::Succession => {
            let last = children.pop().unwrap();
            let inner = Succession::new(&mut stage, children).unwrap();
            Box::new(
                Succession::new(&mut stage, vec![Box::new(inner), last])
                    .unwrap()
                    .with_rate_func(rate),
            )
        }
        Nesting::EagerGroup => {
            let sequence = Succession::new(&mut stage, children)
                .unwrap()
                .with_rate_func(rate);
            Box::new(AnimationGroup::new(&mut stage, vec![Box::new(sequence)]).unwrap())
        }
        Nesting::FutureSuccession => {
            let first = children.remove(0);
            let future = Succession::new(&mut stage, children).unwrap();
            Box::new(
                Succession::new(&mut stage, vec![first, Box::new(future)])
                    .unwrap()
                    .with_rate_func(rate),
            )
        }
        Nesting::FutureEagerGroup => {
            let middle = children.remove(1);
            let future = AnimationGroup::new(&mut stage, vec![middle]).unwrap();
            children.insert(1, Box::new(future));
            Box::new(
                Succession::new(&mut stage, children)
                    .unwrap()
                    .with_rate_func(rate),
            )
        }
    };
    let lifecycle = stage
        .family(sequence.state().mobject())
        .into_iter()
        .map(|mob| stage.snapshot_handle(mob).unwrap())
        .collect();
    let removed = stage.snapshot_handle(sources[2]).unwrap();
    let unrelated = stage.add(Mobject::from_points(&[[42.0, 3.0, -1.0]]));
    stage.add_to_scene(unrelated).unwrap();
    let unrelated = stage.snapshot_handle(unrelated).unwrap();
    Fixture {
        stage,
        animations: vec![sequence],
        lifecycle,
        removed,
        future_target: future_target.unwrap(),
        unrelated,
    }
}

fn starting_clock(stage: &mut Stage) -> RationalFrameClock {
    let mut clock = RationalFrameClock::new(30).unwrap();
    clock.advance_frames(7).unwrap();
    stage.set_time_from_clock(clock.now().to_f64());
    clock
}

fn restore_stage(bytes: &[u8], clock: &RationalFrameClock) -> Stage {
    let mut stage = Stage::new();
    let decoded = Snapshot::from_bytes(bytes, &stage).unwrap();
    stage.restore(&decoded.snapshot);
    stage.set_time_from_clock(clock.now().to_f64());
    stage
}

fn resolve(stage: &Stage, handle: (u32, u32)) -> Mob {
    stage.resolve_snapshot_handle(handle.0, handle.1).unwrap()
}

fn assert_finished(stage: &Stage, fixture: &Fixture) {
    for &handle in &fixture.lifecycle {
        let mob = resolve(stage, handle);
        assert!(
            !stage.is_animating(mob),
            "slot {} remains animating",
            handle.0
        );
        assert!(
            !stage.is_updating_suspended(mob),
            "slot {} remains suspended",
            handle.0
        );
    }
    let removed = resolve(stage, fixture.removed);
    assert!(
        !stage
            .roots()
            .iter()
            .any(|&root| stage.family(root).contains(&removed))
    );
    let unrelated = resolve(stage, fixture.unrelated);
    assert_eq!(stage.get_points(unrelated).unwrap(), [[42.0, 3.0, -1.0]]);
    assert!(stage.roots().contains(&unrelated));
}

#[test]
fn restored_successions_match_every_remaining_frame_and_final_cleanup() {
    let rng = RngRoot::from_seed(0x5acce5510);
    for reused in [false, true] {
        for nesting in [
            Nesting::Plain,
            Nesting::Succession,
            Nesting::EagerGroup,
            Nesting::FutureSuccession,
            Nesting::FutureEagerGroup,
        ] {
            for exact_frames in [false, true] {
                for rate in [
                    RateFunc::linear(),
                    RateFunc::Base(rate::there_and_back),
                    RateFunc::Sampled(Arc::from([0.0, 1.0, 0.2, 0.8, 0.0])),
                ] {
                    let context = format!(
                        "reused={reused}, nesting={nesting:?}, exact={exact_frames}, rate={rate:?}"
                    );
                    let mut oracle = fixture(reused, nesting, exact_frames, rate.clone());
                    let mut oracle_clock = starting_clock(&mut oracle.stage);
                    let mut expected = Vec::new();
                    let report = play_segment(
                        &mut oracle.stage,
                        &mut oracle_clock,
                        &rng,
                        &mut oracle.animations,
                        false,
                        &mut |packet| expected.push(frame_facts(packet)),
                    )
                    .unwrap();
                    assert!(
                        !report.purity.is_pure(),
                        "{context}: generic succession purity stays conservative"
                    );
                    assert_eq!(
                        report.n_frames,
                        if exact_frames { 10 } else { 34 },
                        "{context}"
                    );
                    assert_finished(&oracle.stage, &oracle);
                    let terminal = oracle.stage.snapshot().to_bytes().unwrap();
                    let total = report.n_frames;
                    let first_boundary = if exact_frames { 3 } else { 12 };
                    for cut in [
                        0,
                        1,
                        first_boundary - 1,
                        first_boundary,
                        first_boundary + 1,
                        total / 2,
                        total - 1,
                        total,
                    ] {
                        let mut direct = fixture(reused, nesting, exact_frames, rate.clone());
                        let mut clock = starting_clock(&mut direct.stage);
                        let mut open =
                            open_play(&mut direct.stage, &clock, &mut direct.animations).unwrap();
                        let mut prefix = Vec::new();
                        advance_play(
                            &mut direct.stage,
                            &mut clock,
                            &rng,
                            &mut direct.animations,
                            &mut open,
                            cut,
                            &mut |packet| prefix.push(frame_facts(packet)),
                        )
                        .unwrap();
                        let split = usize::try_from(cut).unwrap();
                        assert_eq!(prefix, expected[..split], "{context}, cut={cut}: prefix");
                        let before = direct.stage.snapshot().to_bytes().unwrap();
                        let checkpoint =
                            capture(&direct.stage, &clock, &direct.animations, &open, BUDGET)
                                .unwrap()
                                .expect("a closed native succession has a durable lifecycle");
                        assert_eq!(
                            direct.stage.snapshot().to_bytes().unwrap(),
                            before,
                            "capture is read-only"
                        );
                        let mut restored = restore_stage(&before, &clock);
                        let (mut resumed, mut resumed_open) =
                            restore(&restored, &clock, &checkpoint).unwrap();
                        let mut resumed_clock = clock;
                        assert_eq!(
                            restored.snapshot().to_bytes().unwrap(),
                            before,
                            "restore does not begin members"
                        );
                        assert_eq!(
                            resumed_open.report().purity,
                            open.report().purity,
                            "{context}: classification survives"
                        );
                        assert!(
                            resumed_open.report().begin_state.is_none(),
                            "later state is not a fabricated begin snapshot"
                        );
                        assert_eq!(resumed_open.stepped(), cut);
                        assert_eq!(
                            capture(&restored, &resumed_clock, &resumed, &resumed_open, BUDGET)
                                .unwrap()
                                .unwrap(),
                            checkpoint,
                            "{context}, cut={cut}: canonical carrier round trip"
                        );
                        let mut suffix = Vec::new();
                        advance_play(
                            &mut direct.stage,
                            &mut clock,
                            &rng,
                            &mut direct.animations,
                            &mut open,
                            total,
                            &mut |packet| suffix.push(frame_facts(packet)),
                        )
                        .unwrap();
                        let mut restored_suffix = Vec::new();
                        advance_play(
                            &mut restored,
                            &mut resumed_clock,
                            &rng,
                            &mut resumed,
                            &mut resumed_open,
                            total,
                            &mut |packet| restored_suffix.push(frame_facts(packet)),
                        )
                        .unwrap();
                        assert_eq!(
                            suffix,
                            expected[split..],
                            "{context}, cut={cut}: uninterrupted suffix"
                        );
                        assert_eq!(
                            restored_suffix, suffix,
                            "{context}, cut={cut}: restored frames"
                        );
                        finish_open_play(&mut direct.stage, &mut direct.animations, open).unwrap();
                        finish_open_play(&mut restored, &mut resumed, resumed_open).unwrap();
                        assert_finished(&direct.stage, &direct);
                        assert_finished(&restored, &direct);
                        assert_eq!(
                            direct.stage.snapshot().to_bytes().unwrap(),
                            terminal,
                            "{context}: direct terminal"
                        );
                        assert_eq!(
                            restored.snapshot().to_bytes().unwrap(),
                            terminal,
                            "{context}, cut={cut}: restored terminal"
                        );
                        assert_eq!(clock.now(), oracle_clock.now());
                        assert_eq!(resumed_clock.now(), oracle_clock.now());
                    }
                }
            }
        }
    }
}

fn vector_family(stage: &mut Stage, x: f64) -> Mob {
    let root = stage.add(Mobject::vector_from_points(&[]));
    for offset in [0.0, 1.5] {
        let child = stage.add(Mobject::vector_from_points(&[
            [x + offset, 0.0, 0.0],
            [x + offset + 0.2, 0.8, 0.0],
            [x + offset + 0.9, 0.3, 0.0],
            [x + offset + 0.5, -0.4, 0.0],
            [x + offset, 0.0, 0.0],
        ]));
        let entry = stage.get_mut(child).unwrap();
        entry
            .buffer
            .write_range("fill_rgba", 0, &[0.2, 0.6, 0.9, 0.7].repeat(5));
        entry
            .buffer
            .write_range("stroke_rgba", 0, &[0.9, 0.4, 0.1, 0.8].repeat(5));
        entry
            .buffer
            .write_range("stroke_width", 0, &[2.0, 3.0, 5.0, 3.0, 2.0]);
        stage.attach(root, child).unwrap();
    }
    stage.add_to_scene(root).unwrap();
    root
}

fn carrier_scene(kind: usize, rate: RateFunc) -> (Stage, Vec<Box<dyn Animation>>) {
    let mut stage = Stage::new();
    let preceding = source_family(&mut stage, -8.0);
    let source = vector_family(&mut stage, 0.0);
    let target = stage.copy_family(source).unwrap();
    stage.shift(target, [4.0, 1.0, 0.0]);
    let following = source_family(&mut stage, 8.0);
    let mut carrier: Box<dyn Animation> = match kind {
        0 => Box::new(fmn_anim::TransformMatchingParts::new(&mut stage, source, target).unwrap()),
        1 => Box::new(fmn_anim::fade_transform(&mut stage, source, target).unwrap()),
        2 => Box::new(fmn_anim::DrawBorderThenFill::new(source)),
        3 => Box::new(
            fmn_anim::VShowPassingFlash::new(source)
                .with_time_width(0.6)
                .with_taper_width(0.2),
        ),
        4 => Box::new(fmn_anim::show_increasing_subsets(&stage, source).unwrap()),
        5 => Box::new(fmn_anim::fade_in(&mut stage, source, [1.0, 0.2, 0.0], 1.3).unwrap()),
        6 => Box::new(fmn_anim::grow_from_center(&mut stage, source, None).unwrap()),
        7 => Box::new(fmn_anim::indicate(&mut stage, source, 1.3, Some([1.0, 0.5, 0.2])).unwrap()),
        _ => unreachable!(),
    };
    let config = AnimConfig {
        run_time_frames: Some(4),
        rate_func: RateFunc::linear(),
        final_alpha_value: 0.375,
        suspend_mobject_updating: true,
        ..AnimConfig::default()
    };
    carrier.state_mut().config = config.clone();
    let prefix = transform_to(&mut stage, preceding, -6.0, config.clone()).0;
    let suffix = transform_to(&mut stage, following, 10.0, config).0;
    let sequence = Succession::new(&mut stage, vec![prefix, carrier, suffix])
        .unwrap()
        .with_rate_func(rate);
    (stage, vec![Box::new(sequence)])
}

#[test]
fn heterogeneous_native_members_resume_in_fresh_active_and_completed_phases() {
    let rng = RngRoot::from_seed(0xca771e7);
    for (kind, name) in [
        "matching",
        "crossfade",
        "border-fill",
        "passing-flash",
        "subset-reveal",
        "deferred-fade",
        "deferred-grow",
        "deferred-indicate",
    ]
    .into_iter()
    .enumerate()
    {
        for rate in [RateFunc::linear(), RateFunc::Base(rate::there_and_back)] {
            let (mut oracle, mut oracle_animations) = carrier_scene(kind, rate.clone());
            let mut oracle_clock = starting_clock(&mut oracle);
            let mut expected = Vec::new();
            let report = play_segment(
                &mut oracle,
                &mut oracle_clock,
                &rng,
                &mut oracle_animations,
                false,
                &mut |packet| expected.push(frame_facts(packet)),
            )
            .unwrap();
            assert_eq!(report.n_frames, 12, "{name}");
            let terminal = oracle.snapshot().to_bytes().unwrap();
            for cut in [0, 4, 5, 7, 8, 9, 12] {
                let context = format!("{name}, rate={rate:?}, cut={cut}");
                let (mut stage, mut animations) = carrier_scene(kind, rate.clone());
                let mut clock = starting_clock(&mut stage);
                let mut open = open_play(&mut stage, &clock, &mut animations).unwrap();
                let mut prefix = Vec::new();
                advance_play(
                    &mut stage,
                    &mut clock,
                    &rng,
                    &mut animations,
                    &mut open,
                    cut,
                    &mut |packet| prefix.push(frame_facts(packet)),
                )
                .unwrap();
                let split = usize::try_from(cut).unwrap();
                assert_eq!(prefix, expected[..split], "{context}: prefix");
                let before = stage.snapshot().to_bytes().unwrap();
                let checkpoint = capture(&stage, &clock, &animations, &open, BUDGET)
                    .unwrap()
                    .unwrap_or_else(|| panic!("{context}: native carrier was refused"));
                assert_eq!(stage.snapshot().to_bytes().unwrap(), before);
                let mut restored = restore_stage(&before, &clock);
                let (mut resumed, mut resumed_open) =
                    restore(&restored, &clock, &checkpoint).unwrap();
                assert_eq!(restored.snapshot().to_bytes().unwrap(), before);
                assert_eq!(resumed_open.report().purity, open.report().purity);
                let mut suffix = Vec::new();
                advance_play(
                    &mut restored,
                    &mut clock,
                    &rng,
                    &mut resumed,
                    &mut resumed_open,
                    report.n_frames,
                    &mut |packet| suffix.push(frame_facts(packet)),
                )
                .unwrap();
                assert_eq!(suffix, expected[split..], "{context}: remaining frames");
                finish_open_play(&mut restored, &mut resumed, resumed_open).unwrap();
                assert_eq!(
                    restored.snapshot().to_bytes().unwrap(),
                    terminal,
                    "{context}: final cleanup"
                );
                let _ = abort_open_play(&mut stage, &mut animations, open);
            }
        }
    }
}

#[test]
fn subset_replay_retains_hidden_member_lifetimes_and_abort_releases_them() {
    let (mut stage, mut animations) = carrier_scene(4, RateFunc::linear());
    let sequence = (animations[0].as_ref() as &dyn std::any::Any)
        .downcast_ref::<Succession>()
        .unwrap();
    let subset = sequence.animations()[1].state().mobject();
    let owned = stage.family(subset);
    assert_eq!(owned.len(), 3);
    let clock = starting_clock(&mut stage);
    let open = open_play(&mut stage, &clock, &mut animations).unwrap();
    // The middle member first hides its children, then finishes, then is
    // revisited at its empty start. Hidden rows retain the same begin-time
    // lifetime as visible rows; a root-only checkpoint loses those flags.
    for frame in [4, 9, 4] {
        animations[0]
            .interpolate_at_position(
                &mut stage,
                &fmn_anim::FramePosition::from_frame(frame),
                clock.fps(),
            )
            .unwrap();
        if frame == 4 {
            assert!(stage.get(subset).unwrap().submobjects().is_empty());
            for &member in &owned {
                assert!(
                    stage.is_animating(member),
                    "hidden member lost its lifetime"
                );
                assert!(
                    stage.is_updating_suspended(member),
                    "hidden member escaped updater suspension"
                );
            }
        }
    }
    let _ = abort_open_play(&mut stage, &mut animations, open);
    assert!(stage.get(subset).unwrap().submobjects().is_empty());
    for member in owned {
        assert!(
            !stage.is_animating(member),
            "abort left a hidden member active"
        );
        assert!(
            !stage.is_updating_suspended(member),
            "abort left a hidden member suspended"
        );
    }
}

#[test]
fn exact_rates_resume_rewound_members_after_the_preparation_callback_is_gone() {
    let rng = RngRoot::from_seed(0xe7ac7);
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&calls);
    let sampler = RateSampler::new(1024, move |alpha| {
        counted.fetch_add(1, Ordering::SeqCst);
        Ok(if alpha <= 0.5 {
            2.0 * alpha
        } else {
            2.0 * (1.0 - alpha)
        })
    });
    let weak_sampler = Arc::downgrade(&sampler);
    let mut oracle = fixture(
        true,
        Nesting::Succession,
        true,
        RateFunc::Preparing(Arc::clone(&sampler)),
    );
    let mut oracle_clock = starting_clock(&mut oracle.stage);
    let mut expected = Vec::new();
    let report = play_segment(
        &mut oracle.stage,
        &mut oracle_clock,
        &rng,
        &mut oracle.animations,
        false,
        &mut |packet| expected.push(frame_facts(packet)),
    )
    .unwrap();
    let terminal = oracle.stage.snapshot().to_bytes().unwrap();
    let rate = sampler.finish().unwrap();
    let prepared_calls = calls.load(Ordering::SeqCst);
    assert!(prepared_calls > 2);
    drop(oracle);
    drop(sampler);
    assert!(
        weak_sampler.upgrade().is_none(),
        "the runtime has no preparation callback to replay"
    );

    let mut direct = fixture(true, Nesting::Succession, true, rate);
    let mut clock = starting_clock(&mut direct.stage);
    let mut open = open_play(&mut direct.stage, &clock, &mut direct.animations).unwrap();
    let mut prefix = Vec::new();
    advance_play(
        &mut direct.stage,
        &mut clock,
        &rng,
        &mut direct.animations,
        &mut open,
        7,
        &mut |packet| prefix.push(frame_facts(packet)),
    )
    .unwrap();
    assert_eq!(prefix, expected[..7]);
    let checkpoint = capture(&direct.stage, &clock, &direct.animations, &open, BUDGET)
        .unwrap()
        .unwrap();
    let snapshot = direct.stage.snapshot().to_bytes().unwrap();
    let mut restored = restore_stage(&snapshot, &clock);
    let (mut resumed, mut resumed_open) = restore(&restored, &clock, &checkpoint).unwrap();
    let mut resumed_clock = clock;
    let mut suffix = Vec::new();
    advance_play(
        &mut restored,
        &mut resumed_clock,
        &rng,
        &mut resumed,
        &mut resumed_open,
        report.n_frames,
        &mut |packet| suffix.push(frame_facts(packet)),
    )
    .unwrap();
    assert_eq!(suffix, expected[7..]);
    finish_open_play(&mut restored, &mut resumed, resumed_open).unwrap();
    assert_finished(&restored, &direct);
    assert_eq!(restored.snapshot().to_bytes().unwrap(), terminal);
    assert_eq!(resumed_clock.now(), oracle_clock.now());
    assert_eq!(
        calls.load(Ordering::SeqCst),
        prepared_calls,
        "capture and resume never execute an authored rate"
    );
    let _ = abort_open_play(&mut direct.stage, &mut direct.animations, open);
}

#[test]
fn capture_refuses_pending_frames_and_small_budgets_without_changing_state() {
    let mut fixture = fixture(true, Nesting::Plain, true, RateFunc::linear());
    let mut clock = starting_clock(&mut fixture.stage);
    let mut open = open_play(&mut fixture.stage, &clock, &mut fixture.animations).unwrap();
    let before = fixture.stage.snapshot().to_bytes().unwrap();
    assert!(capture(&fixture.stage, &clock, &fixture.animations, &open, 1).is_err());
    assert_eq!(fixture.stage.snapshot().to_bytes().unwrap(), before);
    assert!(
        prepare_play_frame(
            &mut fixture.stage,
            &mut clock,
            &mut fixture.animations,
            &mut open
        )
        .unwrap()
        .is_some()
    );
    let pending = fixture.stage.snapshot().to_bytes().unwrap();
    let error = capture(&fixture.stage, &clock, &fixture.animations, &open, BUDGET).unwrap_err();
    assert!(error.to_string().contains("pending frame"));
    assert_eq!(fixture.stage.snapshot().to_bytes().unwrap(), pending);
    let rng = RngRoot::from_seed(1);
    complete_play_frame(
        &mut fixture.stage,
        &clock,
        &rng,
        &mut open,
        false,
        &mut |_, _| {},
        &mut |_| {},
    )
    .unwrap();
    assert!(
        capture(&fixture.stage, &clock, &fixture.animations, &open, BUDGET)
            .unwrap()
            .is_some()
    );
    let _ = abort_open_play(&mut fixture.stage, &mut fixture.animations, open);
}

#[test]
fn restore_refuses_wrong_clocks_stale_future_targets_and_damaged_documents() {
    let rng = RngRoot::from_seed(2);
    let mut fixture = fixture(true, Nesting::Succession, true, RateFunc::linear());
    let mut clock = starting_clock(&mut fixture.stage);
    let mut open = open_play(&mut fixture.stage, &clock, &mut fixture.animations).unwrap();
    advance_play(
        &mut fixture.stage,
        &mut clock,
        &rng,
        &mut fixture.animations,
        &mut open,
        1,
        &mut |_| {},
    )
    .unwrap();
    let checkpoint = capture(&fixture.stage, &clock, &fixture.animations, &open, BUDGET)
        .unwrap()
        .unwrap();
    let snapshot = fixture.stage.snapshot().to_bytes().unwrap();
    let mut restored = restore_stage(&snapshot, &clock);
    let mut wrong_cursor = clock;
    wrong_cursor.advance_frames(1).unwrap();
    assert!(restore(&restored, &wrong_cursor, &checkpoint).is_err());
    let mut wrong_grid = RationalFrameClock::new(31).unwrap();
    wrong_grid.advance_frames(clock.now().frames()).unwrap();
    assert!(restore(&restored, &wrong_grid, &checkpoint).is_err());
    for length in [0, 1, checkpoint.len() / 2, checkpoint.len() - 1] {
        assert!(restore(&restored, &clock, &checkpoint[..length]).is_err());
    }
    let mut damaged = checkpoint.clone();
    let middle = damaged.len() / 2;
    damaged[middle] ^= 0x80;
    assert!(restore(&restored, &clock, &damaged).is_err());
    let schema = Schema::new(*b"FMPC", 1, 1, 7);
    for forged_phase in [0, 2, u8::MAX] {
        let mut reader =
            Reader::open(&checkpoint, schema, Limits::DEFAULT, UnknownPolicy::Strict).unwrap();
        assert_eq!(reader.version(), (1, 7));
        let mut writer = Writer::new(schema);
        writer
            .put_u32(reader.get_u32().unwrap())
            .put_i64(reader.get_i64().unwrap())
            .put_i64(reader.get_i64().unwrap());
        let count = reader.get_u32().unwrap();
        assert_eq!(count, 1);
        writer.put_u32(count);
        let kind = reader.get_u8().unwrap();
        assert_eq!(kind, 12);
        writer.put_u8(kind);
        assert_eq!(reader.get_u8().unwrap(), 1);
        writer.put_u8(forged_phase);
        while reader.remaining() != 0 {
            writer.put_u8(reader.get_u8().unwrap());
        }
        reader.finish().unwrap();
        let forged = writer.finish().unwrap();
        // This document has valid framing and a new checksum. Refusal must
        // come from lifecycle validation, not from accidental corruption.
        Reader::open(&forged, schema, Limits::DEFAULT, UnknownPolicy::Strict).unwrap();
        assert!(matches!(
            restore(&restored, &clock, &forged),
            Err(fmn_anim::continuation::Error::Invalid(_))
        ));
    }
    assert_eq!(restored.snapshot().to_bytes().unwrap(), snapshot);
    let future = resolve(&restored, fixture.future_target);
    restored.delete(future).unwrap();
    let after_delete = restored.snapshot().to_bytes().unwrap();
    assert!(restore(&restored, &clock, &checkpoint).is_err());
    assert_eq!(restored.snapshot().to_bytes().unwrap(), after_delete);
    let _ = abort_open_play(&mut fixture.stage, &mut fixture.animations, open);
}

struct FutureCallback {
    state: AnimState,
    calls: Rc<Cell<usize>>,
}

impl Animation for FutureCallback {
    fn state(&self) -> &AnimState {
        &self.state
    }
    fn state_mut(&mut self) -> &mut AnimState {
        &mut self.state
    }
    fn setup(&mut self, _stage: &mut Stage) -> Result<(), AnimError> {
        self.calls.set(self.calls.get() + 1);
        Ok(())
    }
    fn interpolate_submobject(&mut self, _stage: &mut Stage, _mobjects: &[Mob], _alpha: f64) {}
    fn effect_signature(&self) -> AnimationSignature {
        AnimationSignature::Pure
    }
}

fn unknown_rate(alpha: f64) -> f64 {
    if alpha < 0.375 {
        alpha * alpha
    } else {
        0.25 + 0.75 * alpha * alpha
    }
}

#[test]
fn unknown_future_callbacks_rates_and_updater_owned_content_stay_refused() {
    for refused in ["callback", "rate", "updater"] {
        let mut stage = Stage::new();
        let source = source_family(&mut stage, 0.0);
        let calls = Rc::new(Cell::new(0));
        let (first, _) = transform_to(
            &mut stage,
            source,
            2.0,
            AnimConfig {
                run_time_frames: Some(3),
                rate_func: RateFunc::linear(),
                ..AnimConfig::default()
            },
        );
        let second: Box<dyn Animation> = if refused == "callback" {
            Box::new(FutureCallback {
                state: AnimState::new(
                    source,
                    AnimConfig {
                        run_time_frames: Some(4),
                        ..AnimConfig::default()
                    },
                ),
                calls: Rc::clone(&calls),
            })
        } else {
            transform_to(
                &mut stage,
                source,
                4.0,
                AnimConfig {
                    run_time_frames: Some(4),
                    rate_func: if refused == "rate" {
                        RateFunc::Base(unknown_rate)
                    } else {
                        RateFunc::linear()
                    },
                    ..AnimConfig::default()
                },
            )
            .0
        };
        if refused == "updater" {
            let counted = Rc::clone(&calls);
            stage
                .add_updater(source, move |_, _| counted.set(counted.get() + 1), false)
                .unwrap();
        }
        let sequence = Succession::new(&mut stage, vec![first, second]).unwrap();
        let mut animations: Vec<Box<dyn Animation>> = vec![Box::new(sequence)];
        let clock = starting_clock(&mut stage);
        let open = open_play(&mut stage, &clock, &mut animations).unwrap();
        let before = stage.snapshot().to_bytes().unwrap();
        assert!(
            capture(&stage, &clock, &animations, &open, BUDGET)
                .unwrap()
                .is_none(),
            "{refused}"
        );
        assert_eq!(stage.snapshot().to_bytes().unwrap(), before, "{refused}");
        assert_eq!(calls.get(), 0, "admission must never execute {refused}");
        let _ = abort_open_play(&mut stage, &mut animations, open);
    }
}
