//! The update-order corpus (fm-x79 acceptance): the six-step frame order
//! as semantic regression tests — each scene asserts observable state at
//! specific frames, never pixels — plus the FramePacket freeze contract
//! (immutability by construction, O(touched) cost, derivable RNG forks).

use std::cell::RefCell;
use std::rc::Rc;

use fmn_anim::frame::{FramePacket, play_segment, wait_segment};
use fmn_anim::{Animation, FrameSample, RationalFrameClock, RationalTime, prepare_animation};
use fmn_core::rng::RngRoot;
use fmn_mobject::animate::AnimateArgs;
use fmn_mobject::{Mob, Mobject, Stage};

fn square(stage: &mut Stage) -> Mob {
    stage.add(Mobject::from_points(&[
        [-0.5, -0.5, 0.0],
        [0.5, -0.5, 0.0],
        [0.5, 0.5, 0.0],
        [-0.5, 0.5, 0.0],
    ]))
}

/// A linear-rate `.animate` shift of `dx`, prepared into a boxed animation.
fn shift_animation(stage: &mut Stage, mob: Mob, dx: f64, run_time: f64) -> Box<dyn Animation> {
    let builder = mob
        .animate()
        .set_anim_args(AnimateArgs {
            run_time: Some(run_time),
            rate_func: Some(fmn_core::rate::linear),
            ..AnimateArgs::default()
        })
        .and_then(|b| b.shift([dx, 0.0, 0.0]))
        .expect("chain records");
    prepare_animation(builder, stage).expect("prepares")
}

fn rng() -> RngRoot {
    RngRoot::from_seed(42)
}

// ---------------------------------------------------------- the six steps

#[test]
fn scene_updaters_observe_post_interpolation_state() {
    // The always_redraw property: a follower updater reads the animated
    // mobject's position and must see THIS frame's interpolation, not the
    // previous frame's.
    let mut stage = Stage::new();
    let animated = square(&mut stage);
    let follower = square(&mut stage);
    stage.add_to_scene(animated).expect("rooted");
    stage.add_to_scene(follower).expect("rooted");
    stage
        .add_updater(
            follower,
            move |stage, me| {
                let x = stage.get_center(animated)[0];
                stage.set_x(me, x);
            },
            false,
        )
        .expect("registers");

    let mut clock = RationalFrameClock::new(30).expect("fps");
    let rng = rng();
    let mut animations = vec![shift_animation(&mut stage, animated, 2.0, 1.0)];

    let mut packets: Vec<FramePacket> = Vec::new();
    play_segment(
        &mut stage,
        &mut clock,
        &rng,
        &mut animations,
        false,
        &mut |p| packets.push(p),
    )
    .expect("plays");

    // Each packet froze at step 5; restoring it reconstructs that frame's
    // post-step-4 state exactly.
    assert_eq!(packets.len(), 30);
    for packet in &packets {
        stage.restore(packet.state());
        let animated_x = stage.get_center(animated)[0];
        let follower_x = stage.get_center(follower)[0];
        let alpha = packet.alpha();
        assert!(
            (animated_x - 2.0 * alpha).abs() < 1e-5,
            "animated at alpha {alpha}: {animated_x}"
        );
        assert!(
            (follower_x - animated_x).abs() < 1e-5,
            "follower must see this frame's interpolation: {follower_x} vs {animated_x}"
        );
    }
}

#[test]
fn time_advances_before_scene_updaters_run() {
    let mut stage = Stage::new();
    let mob = square(&mut stage);
    stage.add_to_scene(mob).expect("rooted");
    let times: Rc<RefCell<Vec<f64>>> = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&times);
    stage
        .add_updater(
            mob,
            move |stage, me| {
                // The starting/target copies share this callable and tick in
                // step 1 (pre-advance); isolate step 4's scene pass.
                if me == mob {
                    sink.borrow_mut().push(stage.time());
                }
            },
            false,
        )
        .expect("registers");

    let mut clock = RationalFrameClock::new(30).expect("fps");
    let mut animations = vec![shift_animation(&mut stage, mob, 1.0, 0.1)];
    play_segment(
        &mut stage,
        &mut clock,
        &rng(),
        &mut animations,
        false,
        &mut |_| {},
    )
    .expect("plays");

    let observed = times.borrow();
    // 0.1 s at 30 fps → 4 frames (BN-02 upward rounding), plus the
    // finish-pass dt=0 tick.
    assert_eq!(observed.len(), 5);
    assert!(
        (observed[0] - 1.0 / 30.0).abs() < 1e-12,
        "the first updater tick observes post-advance time, got {}",
        observed[0]
    );
}

#[test]
fn animated_mobject_updaters_run_in_step_four_unless_suspended() {
    for suspend in [false, true] {
        let mut stage = Stage::new();
        let mob = square(&mut stage);
        stage.add_to_scene(mob).expect("rooted");
        let source_ticks = Rc::new(RefCell::new(0usize));
        let sink = Rc::clone(&source_ticks);
        stage
            .add_dt_updater(
                mob,
                move |_stage, me, _dt| {
                    // Copies share this callable; count only the scene's own.
                    if me == mob {
                        *sink.borrow_mut() += 1;
                    }
                },
                false,
            )
            .expect("registers");

        let mut clock = RationalFrameClock::new(30).expect("fps");
        let mut animations = vec![shift_animation(&mut stage, mob, 1.0, 1.0)];
        animations[0].state_mut().config.suspend_mobject_updating = suspend;
        play_segment(
            &mut stage,
            &mut clock,
            &rng(),
            &mut animations,
            false,
            &mut |_| {},
        )
        .expect("plays");

        let ticks = *source_ticks.borrow();
        if suspend {
            // Nothing during the play; then finish's resume runs its single
            // update(0) (C-5: exactly once), and the now-resumed mobject
            // ticks once more in the shared finish pass — exactly as the
            // Reference's finish_animations sequence does.
            assert_eq!(ticks, 2, "suspended play ticks only at finish");
        } else {
            // 30 frames + the finish-pass dt=0 tick.
            assert_eq!(ticks, 31, "unsuspended play ticks every frame");
        }
    }
}

#[test]
fn starting_and_target_copies_tick_through_animation_update_mobjects() {
    let mut stage = Stage::new();
    let mob = square(&mut stage);
    stage.add_to_scene(mob).expect("rooted");
    let all_ticks = Rc::new(RefCell::new(Vec::<f64>::new()));
    let sink = Rc::clone(&all_ticks);
    stage
        .add_dt_updater(
            mob,
            move |_stage, _me, dt| {
                sink.borrow_mut().push(dt);
            },
            false,
        )
        .expect("registers");

    let mut clock = RationalFrameClock::new(30).expect("fps");
    let mut animations = vec![shift_animation(&mut stage, mob, 1.0, 1.0)];
    play_segment(
        &mut stage,
        &mut clock,
        &rng(),
        &mut animations,
        false,
        &mut |_| {},
    )
    .expect("plays");

    // Per frame: starting + target (step 1) + the source (step 4) = 3
    // ticks; 30 frames → 90; plus the finish-pass dt=0 source tick.
    let ticks = all_ticks.borrow();
    assert_eq!(ticks.len(), 91);
    let total: f64 = ticks.iter().sum();
    assert!(
        (total - 3.0).abs() < 1e-9,
        "three full run_times of dt across source+copies, got {total}"
    );
}

#[test]
fn skip_mode_matches_played_final_state_and_emits_nothing() {
    // BN-10: a skipped segment must leave dt-updaters and positions exactly
    // where a played segment does.
    let run = |skip: bool| -> (f64, f64, f64, usize, i64) {
        let mut stage = Stage::new();
        let mob = square(&mut stage);
        stage.add_to_scene(mob).expect("rooted");
        let acc = Rc::new(RefCell::new(0.0f64));
        let sink = Rc::clone(&acc);
        stage
            .add_dt_updater(
                mob,
                move |_stage, me, dt| {
                    if me == mob {
                        *sink.borrow_mut() += dt;
                    }
                },
                false,
            )
            .expect("registers");
        let mut clock = RationalFrameClock::new(30).expect("fps");
        let mut animations = vec![shift_animation(&mut stage, mob, 2.0, 1.0)];
        let mut emitted = 0usize;
        play_segment(
            &mut stage,
            &mut clock,
            &rng(),
            &mut animations,
            skip,
            &mut |_| emitted += 1,
        )
        .expect("plays");
        let x = stage.get_center(mob)[0];
        (
            x,
            *acc.borrow(),
            stage.time(),
            emitted,
            clock.now().frames(),
        )
    };

    let (x_played, dt_played, time_played, emitted_played, frames_played) = run(false);
    let (x_skipped, dt_skipped, time_skipped, emitted_skipped, frames_skipped) = run(true);

    assert!((x_played - 2.0).abs() < 1e-5);
    assert_eq!(x_played, x_skipped, "final position identical");
    assert!(
        (dt_played - dt_skipped).abs() < 1e-9,
        "total updater dt identical: played {dt_played} vs skipped {dt_skipped}"
    );
    assert!((time_played - time_skipped).abs() < 1e-9);
    assert_eq!(frames_played, frames_skipped, "the clock advances equally");
    assert_eq!(emitted_played, 30);
    assert_eq!(emitted_skipped, 0, "skip captures and emits nothing");
}

#[test]
fn unequal_run_times_in_one_play_clip_through_the_pipeline() {
    let mut stage = Stage::new();
    let mob_a = square(&mut stage);
    let mob_b = square(&mut stage);
    stage.add_to_scene(mob_a).expect("rooted");
    stage.add_to_scene(mob_b).expect("rooted");

    let mut clock = RationalFrameClock::new(30).expect("fps");
    let mut animations = vec![
        shift_animation(&mut stage, mob_a, 1.0, 0.5),
        shift_animation(&mut stage, mob_b, 2.0, 1.0),
    ];

    let mut packets: Vec<FramePacket> = Vec::new();
    play_segment(
        &mut stage,
        &mut clock,
        &rng(),
        &mut animations,
        false,
        &mut |p| packets.push(p),
    )
    .expect("plays");

    let mut at = |index: usize| -> (f64, f64) {
        stage.restore(packets[index].state());
        (stage.get_center(mob_a)[0], stage.get_center(mob_b)[0])
    };
    assert_eq!(packets.len(), 30, "the progression covers the longest");
    // Frame 15 (t = 0.5): the short animation is exactly done, the long
    // one halfway.
    let (a_mid, b_mid) = at(14);
    assert!((a_mid - 1.0).abs() < 1e-5, "short done at its run_time");
    assert!((b_mid - 1.0).abs() < 1e-5, "long halfway");
    // Beyond its run_time the short animation's alpha exceeds 1 and the
    // pipeline clips: it must hold, not overshoot.
    let (a_late, _) = at(22);
    assert!((a_late - 1.0).abs() < 1e-5, "clipped alpha holds the pose");
    let (a_final, b_final) = at(29);
    assert!((a_final - 1.0).abs() < 1e-5);
    assert!((b_final - 2.0).abs() < 1e-5);
}

#[test]
fn final_sample_exceeds_run_time_and_clamps_to_one() {
    let mut stage = Stage::new();
    let mob = square(&mut stage);
    stage.add_to_scene(mob).expect("rooted");
    let mut clock = RationalFrameClock::new(30).expect("fps");
    // 0.71 s at 30 fps → 22 frames; the last grid time 22/30 > 0.71.
    let mut animations = vec![shift_animation(&mut stage, mob, 1.0, 0.71)];
    let mut packets: Vec<FramePacket> = Vec::new();
    play_segment(
        &mut stage,
        &mut clock,
        &rng(),
        &mut animations,
        false,
        &mut |p| packets.push(p),
    )
    .expect("plays");

    assert_eq!(packets.len(), 22);
    let last = packets.last().expect("nonempty");
    assert_eq!(last.alpha(), 1.0, "the packet's segment alpha clamps");
    assert!(
        (stage.get_center(mob)[0] - 1.0).abs() < 1e-6,
        "exactly at the target"
    );
}

#[test]
fn time_span_re_windows_inside_a_longer_play() {
    let mut stage = Stage::new();
    let spanned = square(&mut stage);
    let steady = square(&mut stage);
    stage.add_to_scene(spanned).expect("rooted");
    stage.add_to_scene(steady).expect("rooted");

    let spanned_builder = spanned
        .animate()
        .set_anim_args(AnimateArgs {
            rate_func: Some(fmn_core::rate::linear),
            time_span: Some((0.5, 1.5)),
            ..AnimateArgs::default()
        })
        .and_then(|b| b.shift([2.0, 0.0, 0.0]))
        .expect("records");

    let mut clock = RationalFrameClock::new(30).expect("fps");
    let mut animations = vec![
        prepare_animation(spanned_builder, &mut stage).expect("prepares"),
        shift_animation(&mut stage, steady, 1.0, 2.0),
    ];

    let mut packets: Vec<FramePacket> = Vec::new();
    play_segment(
        &mut stage,
        &mut clock,
        &rng(),
        &mut animations,
        false,
        &mut |p| packets.push(p),
    )
    .expect("plays");

    let mut at = |index: usize| -> f64 {
        stage.restore(packets[index].state());
        stage.get_center(spanned)[0]
    };
    assert_eq!(
        packets.len(),
        60,
        "the play runs the longest run_time (2 s)"
    );
    assert!(at(14).abs() < 1e-5, "still parked before the span opens");
    assert!(
        (at(29) - 1.0).abs() < 1e-5,
        "halfway through the span at t = 1"
    );
    assert!(
        (at(44) - 2.0).abs() < 1e-5,
        "done exactly as the span closes at t = 1.5"
    );
    assert!((at(59) - 2.0).abs() < 1e-5, "holds after the span");
}

// -------------------------------------------------------- wait semantics

#[test]
fn wait_emits_frames_and_runs_an_initial_zero_dt_pass() {
    let mut stage = Stage::new();
    let mob = square(&mut stage);
    stage.add_to_scene(mob).expect("rooted");
    let dts: Rc<RefCell<Vec<f64>>> = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&dts);
    stage
        .add_dt_updater(
            mob,
            move |_stage, _me, dt| sink.borrow_mut().push(dt),
            false,
        )
        .expect("registers");

    let mut clock = RationalFrameClock::new(30).expect("fps");
    let mut emitted = 0usize;
    wait_segment(
        &mut stage,
        &mut clock,
        &rng(),
        0.5,
        None,
        false,
        &mut |_| emitted += 1,
    )
    .expect("waits");

    assert_eq!(emitted, 15);
    let observed = dts.borrow();
    assert_eq!(observed.len(), 16, "initial dt=0 pass + one per frame");
    assert_eq!(observed[0], 0.0, "the Reference's update_mobjects(dt=0)");
    assert!((observed[1] - 1.0 / 30.0).abs() < 1e-12);
}

#[test]
fn wait_until_stops_after_the_frame_where_the_condition_turns_true() {
    let mut stage = Stage::new();
    let mob = square(&mut stage);
    stage.add_to_scene(mob).expect("rooted");
    let mut clock = RationalFrameClock::new(30).expect("fps");
    let mut emitted = 0usize;
    // Condition: scene time reaches 0.2 s — true first at frame 6 (6/30).
    let mut condition = |stage: &Stage| stage.time() >= 0.2 - 1e-12;
    wait_segment(
        &mut stage,
        &mut clock,
        &rng(),
        60.0,
        Some(&mut condition),
        false,
        &mut |_| emitted += 1,
    )
    .expect("waits");

    assert_eq!(emitted, 6, "the triggering frame is emitted, then it stops");
    assert_eq!(clock.now().frames(), 6);
}

#[test]
fn skipped_wait_advances_time_without_emitting() {
    let mut stage = Stage::new();
    let mob = square(&mut stage);
    stage.add_to_scene(mob).expect("rooted");
    let mut clock = RationalFrameClock::new(30).expect("fps");
    let mut emitted = 0usize;
    wait_segment(&mut stage, &mut clock, &rng(), 1.0, None, true, &mut |_| {
        emitted += 1
    })
    .expect("waits");
    assert_eq!(emitted, 0);
    assert_eq!(clock.now().frames(), 30);
    assert!((stage.time() - 1.0).abs() < 1e-9);
}

// ------------------------------------------------- scene membership edges

#[test]
fn begin_roots_an_unrooted_animated_mobject() {
    let mut stage = Stage::new();
    let mob = square(&mut stage); // never added to the scene
    let mut clock = RationalFrameClock::new(30).expect("fps");
    let mut animations = vec![shift_animation(&mut stage, mob, 1.0, 0.1)];
    play_segment(
        &mut stage,
        &mut clock,
        &rng(),
        &mut animations,
        false,
        &mut |_| {},
    )
    .expect("plays");
    assert!(stage.roots().contains(&mob), "begin adds it to the scene");
}

#[test]
fn remover_animations_leave_the_scene_at_finish() {
    let mut stage = Stage::new();
    let mob = square(&mut stage);
    stage.add_to_scene(mob).expect("rooted");
    let mut clock = RationalFrameClock::new(30).expect("fps");
    let mut animations = vec![shift_animation(&mut stage, mob, 1.0, 0.1)];
    animations[0].state_mut().config.remover = true;
    play_segment(
        &mut stage,
        &mut clock,
        &rng(),
        &mut animations,
        false,
        &mut |_| {},
    )
    .expect("plays");
    assert!(
        !stage.roots().contains(&mob),
        "clean_up_from_scene removes the remover's mobject"
    );
    assert!(
        stage.contains(mob),
        "removal is scene membership, not death"
    );
}

// ----------------------------------------------------- FramePacket freeze

#[test]
fn packets_are_isolated_from_later_scene_mutation() {
    let mut stage = Stage::new();
    let mob = square(&mut stage);
    stage.add_to_scene(mob).expect("rooted");
    let mut clock = RationalFrameClock::new(30).expect("fps");
    let mut animations = vec![shift_animation(&mut stage, mob, 2.0, 1.0)];
    let mut packets: Vec<FramePacket> = Vec::new();
    play_segment(
        &mut stage,
        &mut clock,
        &rng(),
        &mut animations,
        false,
        &mut |p| packets.push(p),
    )
    .expect("plays");

    // Mutate the live scene hard after capture.
    stage.set_x(mob, 999.0);
    stage.update(1.0);

    // A held packet still reconstructs its frame's exact front-end state.
    let mid = &packets[14]; // frame 15, alpha 0.5
    assert_eq!(mid.segment_frame(), 15);
    stage.restore(mid.state());
    assert!(
        (stage.get_center(mob)[0] - 1.0).abs() < 1e-5,
        "the frozen frame is alpha 0.5, untouched by later writes"
    );
}

#[test]
fn packet_freeze_cost_is_o_touched() {
    let mut stage = Stage::new();
    let mobs: Vec<Mob> = (0..16).map(|_| square(&mut stage)).collect();
    for &m in &mobs {
        stage.add_to_scene(m).expect("rooted");
    }
    let ids_before: Vec<usize> = mobs
        .iter()
        .map(|m| stage.get(*m).unwrap().buffer.storage_id())
        .collect();

    let clock = RationalFrameClock::new(30).expect("fps");
    let sample = FrameSample {
        frame: 1,
        time: RationalTime::zero(30) + 1,
        alpha: 1.0 / 30.0,
    };
    let _packet = FramePacket::freeze(&stage, &clock, &rng(), &sample);

    // Freezing copied nothing.
    for (m, id) in mobs.iter().zip(&ids_before) {
        assert_eq!(stage.get(*m).unwrap().buffer.storage_id(), *id);
    }
    // Touching one entry unshares exactly that entry.
    stage.set_x(mobs[3], 5.0);
    for (i, (m, id)) in mobs.iter().zip(&ids_before).enumerate() {
        let now = stage.get(*m).unwrap().buffer.storage_id();
        if i == 3 {
            assert_ne!(now, *id, "touched entry unshares");
        } else {
            assert_eq!(now, *id, "untouched entries still share");
        }
    }
}

#[test]
fn packet_rng_forks_are_pure_and_derivable() {
    let mut stage = Stage::new();
    let mob = square(&mut stage);
    stage.add_to_scene(mob).expect("rooted");
    let mut clock = RationalFrameClock::new(30).expect("fps");
    let root = rng();
    let mut animations = vec![shift_animation(&mut stage, mob, 1.0, 0.2)];
    let mut packets: Vec<FramePacket> = Vec::new();
    play_segment(
        &mut stage,
        &mut clock,
        &root,
        &mut animations,
        false,
        &mut |p| packets.push(p),
    )
    .expect("plays");

    let packet = &packets[2];
    // The packet's fork is exactly the keyed per-frame fork of the root.
    let mut from_packet = packet.rng_fork("streamlines");
    let mut direct = root
        .substream("streamlines")
        .fork_frame(packet.frame_index().cast_unsigned());
    for _ in 0..64 {
        assert_eq!(from_packet.next_u64(), direct.next_u64());
    }
    // Clones agree (pure derivation, no hidden shared state).
    let clone = packet.clone();
    let mut a = packet.rng_fork("dots");
    let mut b = clone.rng_fork("dots");
    for _ in 0..64 {
        assert_eq!(a.next_u64(), b.next_u64());
    }
}
