//! Real native interpolation, independently executed on worker threads.
use std::cell::Cell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicUsize, Ordering};

use fmn_anim::pure_frame::{PureFrameJob, PureFrameRefusal, PureSegment};
use fmn_anim::{
    AnimConfig, AnimState, Animation, AnimationSignature, RateFunc, RationalFrameClock,
    Transform, advance_play, fade_in, fade_out, finish_open_play, grow_from_point,
    open_play, open_play_with_mode, prepare_animation, replacement_transform,
};
use fmn_core::rate::CatalogRate;
use fmn_core::rng::RngRoot;
use fmn_mobject::{Mob, Mobject, Stage};

fn vector_curve(stage: &mut Stage, x: f32) -> Mob {
    use fmn_mobject::{RecordBuffer, RecordSchema};
    let mob = stage.add(Mobject::new());
    let entry = stage.get_mut(mob).unwrap();
    entry.buffer = RecordBuffer::new(RecordSchema::vmobject(), 5).unwrap();
    entry.buffer.write_range("point", 0, &[
        x, 0.0, 0.0, x + 0.5, 1.0, 0.0, x + 1.0, 0.0, 0.0,
        x + 1.5, -0.5, 0.0, x + 2.0, 0.5, 0.0,
    ]);
    for field in ["fill_rgba", "stroke_rgba"] {
        entry.buffer.write_range(field, 0, &[0.2, 0.4, 0.8, 0.75].repeat(5));
    }
    stage.add_to_scene(mob).unwrap();
    mob
}

fn extra_mechanism(kind: usize, mob: Mob) -> Box<dyn Animation> {
    match kind {
        0 => Box::new(fmn_anim::show_creation(mob)),
        1 => Box::new(fmn_anim::show_passing_flash(mob, 0.35)),
        2 => Box::new(fmn_anim::v_fade_in(mob)),
        3 => Box::new(fmn_anim::v_fade_out(mob)),
        4 => Box::new(fmn_anim::v_fade_in_then_out(mob)),
        _ => Box::new(fmn_anim::Rotating::new(mob)
            .with_angle(1.3).with_axis([0.3, 0.5, 1.0])
            .with_about_edge([1.0, 0.0, 0.0])),
    }
}

fn square(stage: &mut Stage, x: f64) -> Mob {
    let mob = stage.add(Mobject::from_points(&[
        [x, 0.0, 0.0], [x + 1.0, 0.0, 0.0],
        [x + 1.0, 1.0, 0.0], [x, 1.0, 0.0],
    ]));
    stage.add_to_scene(mob).unwrap();
    mob
}

fn facts(stage: &Stage) -> (Vec<Vec<Mob>>, Vec<u64>, Vec<String>) {
    let mut topology = Vec::new();
    let mut bits = vec![stage.time().to_bits()];
    let mut metadata = Vec::new();
    for &root in stage.roots() {
        let family = stage.family(root);
        for &mob in &family {
            let entry = stage.get(mob).unwrap();
            bits.push(entry.buffer.len() as u64);
            for field in entry.buffer.schema().fields() {
                bits.extend(entry.buffer.read_column(&field.name).unwrap()
                    .iter().map(|n| u64::from(n.to_bits())));
            }
            bits.extend(entry.placement().coefficients().map(f64::to_bits));
            metadata.push(format!("{:?}/{:?}/{:?}", entry.uniforms(), stage.tracker(mob),
                (stage.is_animating(mob), stage.is_updating_suspended(mob))));
        }
        topology.push(family);
    }
    (topology, bits, metadata)
}

fn assert_parallel_matches_serial(mut stage: Stage, mut animations: Vec<Box<dyn Animation>>) {
    let rng = RngRoot::from_seed(729);
    let mut clock = RationalFrameClock::new(30).unwrap();
    clock.advance_frames(11).unwrap();
    stage.set_time_from_clock(clock.now().to_f64());
    let mut open = open_play(&mut stage, &clock, &mut animations).unwrap();
    let program = PureSegment::freeze(&stage, &clock, &rng, &animations, &open).unwrap();
    let before = stage.snapshot();
    let mut expected = Vec::new();
    advance_play(&mut stage, &mut clock, &rng, &mut animations, &mut open,
        program.frame_count(), &mut |packet| {
            let random = packet.rng_fork("test").next_u64();
            expected.push((facts(&packet.materialize_stage()), random));
        }).unwrap();
    finish_open_play(&mut stage, &mut animations, open).unwrap();
    let final_facts = facts(&stage);
    assert!(expected.len() > 1);
    assert_ne!(&expected[0].0.1[1..], &expected.last().unwrap().0.1[1..]);
    for workers in [1, 4, 16] {
        std::thread::scope(|scope| {
            for worker in 0..workers {
                let program = &program;
                let expected = &expected;
                scope.spawn(move || {
                    for index in (worker..expected.len()).step_by(workers).rev() {
                        let job = program.job(index as i64 + 1).unwrap();
                        assert_eq!(job.time().frames(), 12 + index as i64);
                        assert_eq!(job.sample().frame, 1 + index as i64);
                        assert_eq!(facts(&job.materialize()), expected[index].0,
                            "frame {index}, workers {workers}");
                        assert_eq!(job.rng_fork("test").next_u64(), expected[index].1);
                    }
                });
            }
        });
    }
    // The independent jobs cannot alter the live owner or its immutable start.
    assert_eq!(facts(&stage), final_facts);
    assert!(stage.can_restore(&before));
    assert!(program.job(0).is_err());
    assert!(program.job(program.frame_count() + 1).is_err());
}

#[test]
fn native_arcs_lag_time_spans_and_nonmonotone_rates_keep_serial_bits() {
    let mut stage = Stage::new();
    let a = square(&mut stage, 0.0);
    let child = stage.add(Mobject::from_points(&[[0.5, 2.0, 0.0]]));
    stage.attach(a, child).unwrap();
    let target = stage.copy_family(a).unwrap();
    stage.shift(target, [2.0, 1.0, 0.5]);
    stage.get_mut(target).unwrap().uniforms_mut().shading = [0.2, 0.4, 0.6];
    let b = square(&mut stage, -3.0);
    let other = stage.copy_family(b).unwrap();
    stage.scale(other, 0.6);
    let animations: Vec<Box<dyn Animation>> = vec![
        Box::new(Transform::new(a, target).with_path_arc(1.3, [0.3, 0.5, 1.0])
            .with_config(AnimConfig {
                run_time: 0.2, time_span: Some((0.03, 0.22)), lag_ratio: 0.25,
                rate_func: RateFunc::Catalog(CatalogRate::ThereAndBack).squish(0.1, 0.9),
                ..AnimConfig::default()
            })),
        Box::new(Transform::new(b, other).with_config(AnimConfig {
            run_time: 0.1, rate_func: RateFunc::linear().not_quite_there(0.8),
            ..AnimConfig::default()
        })),
    ];
    assert_parallel_matches_serial(stage, animations);
}

#[test]
fn method_chains_and_transform_based_fades_and_growth_use_the_native_mechanism() {
    for kind in 0..5 {
        let mut stage = Stage::new();
        let mob = square(&mut stage, 0.0);
        let animation: Box<dyn Animation> = match kind {
            0 => prepare_animation(mob.animate().shift([1.0, 2.0, 0.0]).unwrap(), &mut stage).unwrap(),
            1 => Box::new(fade_in(&mut stage, mob, [1.0, 0.0, 0.0], 1.5).unwrap()),
            2 => Box::new(fade_out(&mut stage, mob, [1.0, 0.0, 0.0], 0.5).unwrap()),
            3 => Box::new(grow_from_point(&mut stage, mob, [-2.0, 3.0, 0.0], None).unwrap()),
            _ => {
                let target = stage.copy_family(mob).unwrap();
                stage.shift(target, [2.0, 0.0, 0.0]);
                Box::new(replacement_transform(mob, target))
            }
        };
        assert_parallel_matches_serial(stage, vec![animation]);
    }
}

#[test]
fn owner_completion_preserves_final_alpha_cleanup_and_later_updates() {
    for remover in [false, true] {
        // Same logical Stage and prepared native animation, two separate plays.
        let mut stage = Stage::new();
        let mob = square(&mut stage, 0.0);
        let target = stage.copy_family(mob).unwrap();
        stage.shift(target, [4.0, 0.0, 0.0]);
        let animation = Transform::new(mob, target).with_config(AnimConfig {
            run_time: 0.2, final_alpha_value: 0.35, remover,
            suspend_mobject_updating: true, ..AnimConfig::default()
        });
        let baseline = stage.snapshot();
        let mut owner = baseline.materialize();
        let rng = RngRoot::from_seed(4);
        let mut clock = RationalFrameClock::new(30).unwrap();
        let mut serial: Vec<Box<dyn Animation>> = vec![Box::new(animation.clone())];
        fmn_anim::play_segment(&mut stage, &mut clock, &rng, &mut serial, false, &mut |_| {}).unwrap();
        let mut offload: Vec<Box<dyn Animation>> = vec![Box::new(animation)];
        let mut owner_clock = RationalFrameClock::new(30).unwrap();
        let open = open_play(&mut owner, &owner_clock, &mut offload).unwrap();
        let program = PureSegment::freeze(&owner, &owner_clock, &rng, &offload, &open).unwrap();
        assert_eq!(program.frame_count(), 7);
        let report = program.finish_on_owner(&mut owner, &mut owner_clock, &mut offload, open).unwrap();
        assert_eq!(report.n_frames, 7);
        assert_eq!(owner_clock.now(), clock.now());
        assert_eq!(facts(&owner), facts(&stage));
        assert_eq!(owner.get_center(mob), stage.get_center(mob));
        assert!(!owner.is_animating(mob));
        assert!(!owner.is_updating_suspended(mob));
        let called = Rc::new(Cell::new(0));
        let counter = Rc::clone(&called);
        owner.add_dt_updater(mob, move |_, _, _| counter.set(counter.get() + 1), false).unwrap();
        owner.add_to_scene(mob).unwrap();
        owner.update(0.1);
        assert_eq!(called.get(), 1);
    }
}

static RATE_CALLS: AtomicUsize = AtomicUsize::new(0);
fn authored_rate(t: f64) -> f64 {
    RATE_CALLS.fetch_add(1, Ordering::SeqCst);
    t
}

#[test]
fn unknown_rates_and_unknown_pure_claims_never_become_worker_callbacks() {
    let mut stage = Stage::new();
    let mob = square(&mut stage, 0.0);
    let target = stage.copy_family(mob).unwrap();
    let mut animations: Vec<Box<dyn Animation>> = vec![Box::new(Transform::new(mob, target)
        .with_config(AnimConfig { rate_func: RateFunc::Base(authored_rate).squish(0.0, 1.0),
            ..AnimConfig::default() }))];
    let clock = RationalFrameClock::new(30).unwrap();
    let rng = RngRoot::from_seed(7);
    let open = open_play(&mut stage, &clock, &mut animations).unwrap();
    let calls = RATE_CALLS.load(Ordering::SeqCst);
    assert_eq!(PureSegment::freeze(&stage, &clock, &rng, &animations, &open).unwrap_err(),
        PureFrameRefusal::UnsupportedAnimation(0));
    assert_eq!(RATE_CALLS.load(Ordering::SeqCst), calls);

    finish_open_play(&mut stage, &mut animations, open).unwrap();

    struct Unknown(AnimState);
    impl Animation for Unknown {
        fn state(&self) -> &AnimState { &self.0 }
        fn state_mut(&mut self) -> &mut AnimState { &mut self.0 }
        fn effect_signature(&self) -> AnimationSignature { AnimationSignature::Pure }
        fn interpolate_submobject(&mut self, _: &mut Stage, _: &[Mob], _: f64) {}
    }
    let mut custom: Vec<Box<dyn Animation>> = vec![Box::new(Unknown(AnimState::new(mob, AnimConfig::default())))];
    let open = open_play(&mut stage, &clock, &mut custom).unwrap();
    assert_eq!(PureSegment::freeze(&stage, &clock, &rng, &custom, &open).unwrap_err(),
        PureFrameRefusal::UnsupportedAnimation(0));
}

#[test]
fn updaters_live_views_skips_and_animated_endpoints_demote() {
    for kind in 0..5 {
        let mut stage = Stage::new();
        let a = square(&mut stage, 0.0);
        let b = square(&mut stage, 2.0);
        let mut view = None;
        if kind == 0 { stage.add_dt_updater(b, |_, _, _| {}, false).unwrap(); }
        if kind == 1 { view = Some(stage.get_mut(b).unwrap().buffer.export_view(false)); }
        let target = stage.copy_family(a).unwrap();
        let mut animations: Vec<Box<dyn Animation>> = vec![Box::new(Transform::new(a, target))];
        if kind == 3 { animations.push(Box::new(Transform::new(target, b))); }
        if kind == 4 { animations.push(Box::new(Transform::new(a, b))); }
        let clock = RationalFrameClock::new(30).unwrap();
        let rng = RngRoot::from_seed(7);
        let open = open_play_with_mode(&mut stage, &clock, &mut animations, kind == 2).unwrap();
        let refusal = PureSegment::freeze(&stage, &clock, &rng, &animations, &open).unwrap_err();
        match kind {
            0 | 2 => assert_eq!(refusal, PureFrameRefusal::NotAtPureBegin),
            1 => assert_eq!(refusal, PureFrameRefusal::LiveView(b)),
            3 => assert_eq!(refusal, PureFrameRefusal::MutableEndpoint(target)),
            _ => assert_eq!(refusal, PureFrameRefusal::OverlappingOutput(a)),
        }
        drop(view);
    }
}

#[test]
fn worker_jobs_outlive_the_owner_without_moving_callback_destructors() {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<PureSegment>();
    send_sync::<PureFrameJob>();
    struct DropProbe(Arc<Mutex<Vec<std::thread::ThreadId>>>);
    impl Drop for DropProbe {
        fn drop(&mut self) { self.0.lock().unwrap().push(std::thread::current().id()); }
    }
    let dropped = Arc::new(Mutex::new(Vec::new()));
    let thread = std::thread::current().id();
    let program = {
        let mut stage = Stage::new();
        let mob = square(&mut stage, 0.0);
        let target = stage.copy_family(mob).unwrap();
        stage.shift(target, [2.0, 0.0, 0.0]);
        let detached = stage.add(Mobject::from_points(&[[8.0, 0.0, 0.0]]));
        let probe = DropProbe(Arc::clone(&dropped));
        let non_send = Rc::new(Cell::new(0));
        stage.add_updater(detached, move |_, _| {
            let _ = &probe;
            non_send.set(non_send.get() + 1);
        }, false).unwrap();
        let mut animations: Vec<Box<dyn Animation>> = vec![Box::new(Transform::new(mob, target))];
        let clock = RationalFrameClock::new(30).unwrap();
        let open = open_play(&mut stage, &clock, &mut animations).unwrap();
        PureSegment::freeze(&stage, &clock, &RngRoot::from_seed(5), &animations, &open).unwrap()
    };
    assert_eq!(*dropped.lock().unwrap(), vec![thread]);
    let job = program.job(15).unwrap();
    let expected = facts(&job.materialize());
    drop(program);
    assert_eq!(std::thread::spawn(move || facts(&job.materialize())).join().unwrap(), expected);
    assert_eq!(*dropped.lock().unwrap(), vec![thread]);
}

#[test]
fn deformed_geometry_and_sampled_rates_keep_serial_records() {
    let mut stage = Stage::new();
    let mob = square(&mut stage, 0.0);
    let target = stage.copy_family(mob).unwrap();
    stage.get_mut(target).unwrap().buffer.write_range("point", 0, &[
        -1.0, 0.5, 0.0, 1.5, -0.5, 1.0,
        2.0, 1.5, 0.0, 0.5, 2.0, -0.5,
    ]);
    stage.shift(mob, [0.2, 0.3, 0.4]);
    stage.shift(target, [-0.5, 0.0, 0.25]);
    let animations: Vec<Box<dyn Animation>> = vec![Box::new(
        Transform::new(mob, target).with_path_arc(0.7, [0.0, 0.0, 1.0])
            .with_config(AnimConfig {
                run_time: 0.5,
                rate_func: RateFunc::Sampled(Arc::from([0.0, 0.9, 0.1, 1.0])),
                ..AnimConfig::default()
            }),
    )];
    assert_parallel_matches_serial(stage, animations);
}

#[test]
fn partly_prepared_or_finished_segments_cannot_be_frozen() {
    let mut stage = Stage::new();
    let mob = square(&mut stage, 0.0);
    let target = stage.copy_family(mob).unwrap();
    stage.shift(target, [2.0, 0.0, 0.0]);
    let mut animations: Vec<Box<dyn Animation>> = vec![Box::new(Transform::new(mob, target))];
    let clock = RationalFrameClock::new(30).unwrap();
    let rng = RngRoot::from_seed(7);
    let mut open = open_play(&mut stage, &clock, &mut animations).unwrap();
    fmn_anim::prepare_play_animation(&mut stage, &clock, &mut animations, &mut open).unwrap();
    assert_eq!(open.stepped(), 0);
    assert_eq!(PureSegment::freeze(&stage, &clock, &rng, &animations, &open).unwrap_err(),
        PureFrameRefusal::NotAtPureBegin);
    fmn_anim::abort_open_play(&mut stage, &mut animations, open);
    let mut open = open_play(&mut stage, &clock, &mut animations).unwrap();
    fmn_anim::finish_open_play_animations(&mut stage, &mut animations, &mut open).unwrap();
    assert_eq!(PureSegment::freeze(&stage, &clock, &rng, &animations, &open).unwrap_err(),
        PureFrameRefusal::NotAtPureBegin);
}

#[test]
fn dispatch_and_sink_refusal_keep_one_begin_and_the_same_serial_epilogue() {
    use fmn_anim::pure_frame::{PlayCapture, play_segment_with_pure_dispatch};
    #[derive(Default)]
    struct Consumer {
        accept: bool,
        offered: usize,
        packets: usize,
        frames: Vec<(Vec<Vec<Mob>>, Vec<u64>, Vec<String>)>,
    }
    impl PlayCapture for Consumer {
        fn accepts_pure_segments(&self) -> bool { true }
        fn capture(&mut self, packet: fmn_anim::FramePacket) {
            self.packets += 1;
            self.frames.push(facts(&packet.materialize_stage()));
        }
        fn capture_pure_segment(&mut self, segment: &PureSegment) -> bool {
            self.offered += 1;
            if !self.accept { return false; }
            for i in 1..=segment.frame_count() {
                self.frames.push(facts(&segment.job(i).unwrap().materialize()));
            }
            true
        }
    }
    let mut stage = Stage::new();
    let mob = square(&mut stage, 0.0);
    let target = stage.copy_family(mob).unwrap();
    stage.shift(target, [2.0, 0.0, 0.0]);
    let begin = stage.snapshot();
    let mut outputs = Vec::new();
    for (allow, accept) in [(false, true), (true, false), (true, true)] {
        let mut stage = begin.materialize();
        let mut clock = RationalFrameClock::new(30).unwrap();
        let mut animations: Vec<Box<dyn Animation>> = vec![Box::new(Transform::new(mob, target)
            .with_config(AnimConfig {run_time: 0.2, ..AnimConfig::default()}))];
        let mut consumer = Consumer {accept, ..Consumer::default()};
        let mut boundaries = 0;
        let report = play_segment_with_pure_dispatch(
            &mut stage, &mut clock, &RngRoot::from_seed(19), &mut animations,
            false, allow, &mut |_, _| boundaries += 1, &mut consumer,
        ).unwrap();
        assert_eq!(consumer.offered, usize::from(allow));
        assert_eq!(consumer.packets, if allow && accept {0} else {7});
        assert_eq!(boundaries, consumer.packets);
        assert_eq!(report.n_frames, 7);
        // Family handles include begin's starting-copy allocations: an accidental
        // second begin changes the observed graph and fails this comparison.
        outputs.push((consumer.frames, facts(&stage), animations[0].state().starting_mobject()));
    }
    assert_eq!(outputs[0], outputs[1]);
    assert_eq!(outputs[0], outputs[2]);
}

#[test]
fn reveal_vector_fade_and_rotation_use_native_pair_interpolation_on_workers() {
    for kind in 0..6 {
        let mut stage = Stage::new();
        let root = stage.add(Mobject::new());
        let first = vector_curve(&mut stage, -1.0);
        let second = vector_curve(&mut stage, 1.0);
        stage.attach(root, first).unwrap();
        stage.attach(root, second).unwrap();
        stage.add_to_scene(root).unwrap();
        stage.shift(root, [0.2, 0.3, 0.4]);
        let mut animation = extra_mechanism(kind, root);
        animation.state_mut().config.run_time = 0.5;
        animation.state_mut().config.time_span = Some((0.03125, 0.4375));
        animation.state_mut().config.lag_ratio = 0.3;
        // Force a backwards traversal as well as forward and endpoint samples;
        // rebuilding a worker from begin must not depend on prior phase visits.
        animation.state_mut().config.rate_func = RateFunc::Sampled(
            Arc::from([0.0, 0.9, 0.1, 0.65, 1.0]),
        );
        assert_parallel_matches_serial(stage, vec![animation]);
    }
}

#[test]
fn additional_mechanisms_keep_owner_final_alpha_removal_and_flash_restoration() {
    for kind in 0..6 {
        let mut initial = Stage::new();
        let mob = vector_curve(&mut initial, -1.0);
        let begin = initial.snapshot();
        let mut outcomes = Vec::new();
        for offload in [false, true] {
            let mut stage = begin.materialize();
            let mut clock = RationalFrameClock::new(8).unwrap();
            clock.advance_frames(3).unwrap();
            let rng = RngRoot::from_seed(912);
            let mut animation = extra_mechanism(kind, mob);
            animation.state_mut().config.run_time = 0.5;
            animation.state_mut().config.final_alpha_value = 0.375;
            let mut animations = vec![animation];
            let report = if offload {
                let open = open_play(&mut stage, &clock, &mut animations).unwrap();
                let program = PureSegment::freeze(&stage, &clock, &rng, &animations, &open).unwrap();
                program.finish_on_owner(&mut stage, &mut clock, &mut animations, open).unwrap()
            } else {
                fmn_anim::play_segment(&mut stage, &mut clock, &rng, &mut animations,
                    false, &mut |_| {}).unwrap()
            };
            assert!(!stage.is_animating(mob));
            assert!(!stage.is_updating_suspended(mob));
            assert_eq!(clock.now().frames(), 7);
            // Compare removed objects too: the visible roots alone cannot prove
            // that passing-flash teardown restored its full source geometry.
            let entry = stage.get(mob).unwrap();
            outcomes.push((facts(&stage), stage.get_points(mob),
                entry.placement().coefficients().map(f64::to_bits),
                entry.buffer.read_column("fill_rgba"),
                entry.buffer.read_column("stroke_rgba"), report.n_frames));
        }
        assert_eq!(outcomes[0], outcomes[1], "mechanism {kind}");
    }
}

#[test]
fn pair_mechanisms_refuse_live_views_updaters_and_cross_mechanism_aliases() {
    for kind in 0..6 {
        for effect in 0..3 {
            let mut stage = Stage::new();
            let mob = vector_curve(&mut stage, 0.0);
            let mut view = None;
            if effect == 0 {
                stage.add_dt_updater(mob, |_, _, _| {}, false).unwrap();
            } else if effect == 1 {
                view = Some(stage.get_mut(mob).unwrap().buffer.export_view(true));
            }
            let mut animations = vec![extra_mechanism(kind, mob)];
            if effect == 2 {
                let target = stage.copy_family(mob).unwrap();
                stage.shift(target, [2.0, 0.0, 0.0]);
                animations.push(Box::new(Transform::new(mob, target)));
            }
            let clock = RationalFrameClock::new(8).unwrap();
            let open = open_play(&mut stage, &clock, &mut animations).unwrap();
            let error = PureSegment::freeze(&stage, &clock, &RngRoot::from_seed(7),
                &animations, &open).unwrap_err();
            match effect {
                0 => assert_eq!(error, PureFrameRefusal::NotAtPureBegin),
                1 => assert!(matches!(error, PureFrameRefusal::LiveView(_))),
                _ => assert_eq!(error, PureFrameRefusal::OverlappingOutput(mob)),
            }
            finish_open_play(&mut stage, &mut animations, open).unwrap();
            drop(view);
        }
    }
}

static PAIR_RATE_CALLS: AtomicUsize = AtomicUsize::new(0);
fn pair_authored_rate(t: f64) -> f64 {
    PAIR_RATE_CALLS.fetch_add(1, Ordering::SeqCst);
    t * t
}

#[test]
fn pair_mechanisms_never_admit_an_authored_rate_callback() {
    for kind in 0..6 {
        let mut stage = Stage::new();
        let mob = vector_curve(&mut stage, 0.0);
        let mut animation = extra_mechanism(kind, mob);
        animation.state_mut().config.rate_func = RateFunc::Base(pair_authored_rate)
            .squish(0.1, 0.9).not_quite_there(0.8);
        let mut animations = vec![animation];
        let clock = RationalFrameClock::new(8).unwrap();
        let open = open_play(&mut stage, &clock, &mut animations).unwrap();
        let calls = PAIR_RATE_CALLS.load(Ordering::SeqCst);
        assert_eq!(PureSegment::freeze(&stage, &clock, &RngRoot::from_seed(0),
            &animations, &open).unwrap_err(), PureFrameRefusal::UnsupportedAnimation(0));
        assert_eq!(PAIR_RATE_CALLS.load(Ordering::SeqCst), calls);
        finish_open_play(&mut stage, &mut animations, open).unwrap();
    }
}

#[test]
fn static_wait_jobs_keep_exact_clocks_rng_and_storage_after_owner_mutation() {
    use fmn_anim::pure_frame::{PlayCapture, wait_segment_with_pure_dispatch};
    #[derive(Default)]
    struct Sink {
        accept: bool,
        offered: usize,
        packets: Vec<(i64, (Vec<Vec<Mob>>, Vec<u64>, Vec<String>), u64)>,
        jobs: Vec<PureFrameJob>,
    }
    impl PlayCapture for Sink {
        fn accepts_pure_segments(&self) -> bool { true }
        fn capture(&mut self, packet: fmn_anim::FramePacket) {
            self.packets.push((packet.time().frames(), facts(&packet.materialize_stage()),
                packet.rng_fork("wait").next_u64()));
        }
        fn capture_pure_segment(&mut self, segment: &PureSegment) -> bool {
            self.offered += 1;
            if !self.accept { return false; }
            self.jobs.extend((1..=segment.frame_count()).map(|i| segment.job(i).unwrap()));
            true
        }
    }
    for duration in [0.0, 0.2, 0.5] {
        let mut initial = Stage::new();
        let mob = vector_curve(&mut initial, 0.0);
        let begin = initial.snapshot();
        let mut outcomes = Vec::new();
        for (allow, accept) in [(false, true), (true, false), (true, true)] {
            let mut stage = begin.materialize();
            let mut clock = RationalFrameClock::new(30).unwrap();
            clock.advance_frames(11).unwrap();
            stage.set_time_from_clock(clock.now().to_f64());
            let mut sink = Sink { accept, ..Sink::default() };
            let mut boundaries = 0;
            let report = wait_segment_with_pure_dispatch(&mut stage, &mut clock,
                &RngRoot::from_seed(729), duration, false, allow,
                &mut |_, _| boundaries += 1, &mut sink).unwrap();
            assert_eq!(report.kind, fmn_anim::SegmentKind::Wait);
            assert_eq!(sink.offered, usize::from(allow && report.n_frames > 0));
            assert_eq!(boundaries, sink.packets.len());
            assert_eq!(clock.now().frames(), 11 + report.n_frames);
            let final_facts = facts(&stage);
            stage.shift(mob, [100.0, 0.0, 0.0]);
            // Jobs still own their pre-mutation static frame. Evaluate on a
            // worker in reverse order after the sink and program have returned.
            if !sink.jobs.is_empty() {
                sink.packets = std::thread::spawn(move || sink.jobs.into_iter().rev().map(|job| {
                    (job.time().frames(), facts(&job.materialize()), job.rng_fork("wait").next_u64())
                }).collect()).join().unwrap();
                sink.packets.sort_by_key(|(frame, _, _)| *frame);
            }
            outcomes.push((sink.packets, final_facts, report.n_frames));
        }
        assert_eq!(outcomes[0], outcomes[1], "sink refusal at duration {duration}");
        assert_eq!(outcomes[0], outcomes[2], "worker path at duration {duration}");
    }
}

#[test]
fn wait_dispatch_keeps_the_initial_zero_dt_and_every_stateful_skip_step() {
    use std::cell::RefCell;
    use fmn_anim::pure_frame::{PlayCapture, wait_segment_with_pure_dispatch};
    #[derive(Default)]
    struct Sink(usize);
    impl PlayCapture for Sink {
        fn accepts_pure_segments(&self) -> bool { true }
        fn capture(&mut self, _: fmn_anim::FramePacket) { self.0 += 1; }
        fn capture_pure_segment(&mut self, _: &PureSegment) -> bool {
            panic!("an updater-driven wait cannot become a static worker program")
        }
    }
    for duration in [0.0, 0.5] {
        for skip in [false, true] {
            let mut outcomes = Vec::new();
            for dispatch in [false, true] {
                let mut stage = Stage::new();
                let mob = square(&mut stage, 0.0);
                let calls = Rc::new(RefCell::new(Vec::new()));
                let recorded = Rc::clone(&calls);
                stage.add_dt_updater(mob, move |stage, mob, dt| {
                    recorded.borrow_mut().push(dt.to_bits());
                    let x = stage.get_center(mob)[0];
                    stage.shift(mob, [(x + 1.0) * dt, 0.0, 0.0]);
                }, false).unwrap();
                let mut clock = RationalFrameClock::new(8).unwrap();
                let mut sink = Sink::default();
                let report = if dispatch {
                    wait_segment_with_pure_dispatch(&mut stage, &mut clock, &RngRoot::from_seed(0),
                        duration, skip, true, &mut |_, _| {}, &mut sink).unwrap()
                } else {
                    fmn_anim::wait_segment(&mut stage, &mut clock, &RngRoot::from_seed(0),
                        duration, None, skip, &mut |packet| sink.capture(packet)).unwrap()
                };
                assert!(!report.purity.is_pure());
                assert_eq!(calls.borrow().len() as i64, report.n_frames + 1);
                assert_eq!(calls.borrow()[0], 0.0_f64.to_bits());
                outcomes.push((stage.get_center(mob).map(f64::to_bits), clock.now(),
                    calls.borrow().clone(), sink.0));
            }
            assert_eq!(outcomes[0], outcomes[1]);
        }
    }
}

#[test]
fn waits_with_live_views_or_open_host_phases_refuse_compact_jobs() {
    for kind in 0..4 {
        let mut stage = Stage::new();
        let mob = vector_curve(&mut stage, 0.0);
        let view = (kind == 0).then(|| stage.get_mut(mob).unwrap().buffer.export_view(true));
        let mut clock = RationalFrameClock::new(8).unwrap();
        let mut open = if kind == 1 {
            fmn_anim::open_wait_after_updaters(&mut stage, &clock, 0.5, false).unwrap()
        } else {
            fmn_anim::open_wait(&mut stage, &clock, 0.5, kind == 2).unwrap()
        };
        if kind == 3 {
            fmn_anim::prepare_wait_frame(&mut stage, &mut clock, &mut open).unwrap();
        }
        let refusal = PureSegment::freeze_wait(&stage, &clock, &RngRoot::from_seed(0), &open)
            .unwrap_err();
        if kind == 0 {
            assert_eq!(refusal, PureFrameRefusal::LiveView(mob));
        } else {
            assert_eq!(refusal, PureFrameRefusal::NotAtPureBegin);
        }
        drop(view);
    }
}
