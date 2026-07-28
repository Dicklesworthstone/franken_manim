//! Scene runtime acceptance corpus (fm-5xm): lifecycle, stable membership,
//! skip/range equivalence, state restore, preflight, presenter holds, 3D
//! defaults, discovery/selection, BlankScene, and output naming.

use std::cell::RefCell;
use std::rc::Rc;

use fmn_anim::{SegmentKind, fade_transform, prepare_animation, replacement_transform};
use fmn_core::rng::Pcg64Dxsm;
use fmn_mobject::animate::AnimateArgs;
use fmn_mobject::{Mobject, SceneState, Stage, StageError, UpdaterFn, UpdaterKindTag};
use fmn_scene::{
    CameraOrientation, CaptureReason, EndScene, HoldDecision, HoldKind, IntegrationError,
    LifecycleEvent, LifecyclePhase, NullSceneSink, OutputNaming, PlayOverrides, RuntimeConfig,
    Scene, SceneError, SceneProgram, SceneRegistry, SceneSelectionError, SceneSink,
    ThreeDAddOptions, ThreeDScene,
};

#[derive(Default)]
struct RecordingSink {
    events: Vec<LifecycleEvent>,
    captures: Vec<(CaptureReason, i64, i64)>,
    fail_event_at: Option<usize>,
    fail_capture_at: Option<usize>,
}

impl SceneSink for RecordingSink {
    fn event(&mut self, event: LifecycleEvent) -> Result<(), IntegrationError> {
        if self.fail_event_at == Some(self.events.len()) {
            return Err(IntegrationError::new("sink", "fixture event failure"));
        }
        self.events.push(event);
        Ok(())
    }

    fn capture(
        &mut self,
        reason: CaptureReason,
        packet: fmn_anim::FramePacket,
    ) -> Result<(), IntegrationError> {
        if self.fail_capture_at == Some(self.captures.len()) {
            return Err(IntegrationError::new("sink", "fixture failure"));
        }
        self.captures
            .push((reason, packet.frame_index(), packet.segment_frame()));
        Ok(())
    }
}

#[derive(Default)]
struct PacketSink {
    packet: Option<fmn_anim::FramePacket>,
}

impl SceneSink for PacketSink {
    fn capture(
        &mut self,
        _reason: CaptureReason,
        packet: fmn_anim::FramePacket,
    ) -> Result<(), IntegrationError> {
        self.packet = Some(packet);
        Ok(())
    }
}

fn point() -> Mobject {
    Mobject::from_points(&[[0.0, 0.0, 0.0]])
}

#[test]
fn windowed_fps_override_and_config_refusals_are_semantic() {
    let scene = Scene::new(
        RuntimeConfig {
            fps: 144,
            windowed: true,
            ..RuntimeConfig::default()
        },
        1,
    )
    .expect("window config");
    assert_eq!(scene.fps(), 30);

    let window_saves_zero_fps = Scene::new(
        RuntimeConfig {
            fps: 0,
            windowed: true,
            ..RuntimeConfig::default()
        },
        1,
    )
    .expect("window forces 30 before validation");
    assert_eq!(window_saves_zero_fps.fps(), 30);

    assert!(matches!(
        Scene::new(
            RuntimeConfig {
                fps: 0,
                ..RuntimeConfig::default()
            },
            1
        ),
        Err(SceneError::InvalidConfig(_))
    ));
    assert!(matches!(
        Scene::new(
            RuntimeConfig {
                start_at_play: Some(3),
                end_at_play: Some(2),
                ..RuntimeConfig::default()
            },
            1
        ),
        Err(SceneError::InvalidConfig(_))
    ));
}

#[test]
fn stable_scene_membership_operations_are_marionettes_one_rule() {
    let mut scene = Scene::default();
    let a = scene.stage_mut().add(point().with_z_index(1));
    let b = scene.stage_mut().add(point().with_z_index(0));
    let c = scene.stage_mut().add(point().with_z_index(1));
    scene.add(&[a, b, c]).expect("live roots");
    assert_eq!(scene.mobjects(), &[b, a, c]);

    scene.bring_to_back(&[a, c]).expect("live roots");
    assert_eq!(scene.mobjects(), &[a, c, b]);
    scene.bring_to_front(&[a]).expect("live root");
    assert_eq!(scene.mobjects(), &[b, c, a]);

    scene.remove(&[c]);
    assert_eq!(scene.mobjects(), &[b, a]);
    scene.clear();
    assert!(scene.mobjects().is_empty());
    assert!(
        scene.stage().contains(a),
        "clear is membership, not deletion"
    );
}

#[test]
fn preflight_runs_once_over_constructed_roots_before_frame_zero() {
    let mut scene = Scene::new(
        RuntimeConfig {
            fps: 4,
            ..RuntimeConfig::default()
        },
        7,
    )
    .expect("scene");
    let a = scene.add_mobject(point()).expect("root");
    let b = scene.add_mobject(point()).expect("root");
    let calls = Rc::new(RefCell::new(Vec::new()));
    let seen = Rc::clone(&calls);
    scene
        .set_preflight_hook(move |_stage, roots| {
            seen.borrow_mut().push(roots.to_vec());
            Ok(())
        })
        .expect("installs");

    let mut sink = RecordingSink::default();
    scene.wait(Some(0.5), &mut sink).expect("wait");
    scene.show(&mut sink).expect("show");

    assert_eq!(&*calls.borrow(), &[vec![a, b]]);
    let phases: Vec<_> = sink.events.iter().map(|event| event.phase).collect();
    assert_eq!(
        phases,
        [
            LifecyclePhase::Preflight,
            LifecyclePhase::PrePlay,
            LifecyclePhase::DriveSegment,
            LifecyclePhase::FinishSegment,
            LifecyclePhase::PostPlay,
        ]
    );
    assert_eq!(sink.captures[0], (CaptureReason::Segment, 1, 1));
    assert_eq!(sink.captures.last(), Some(&(CaptureReason::Show, 2, 0)));
}

#[test]
fn failed_preflight_captures_nothing_and_can_be_retried() {
    let mut scene = Scene::default();
    scene.add_mobject(point()).expect("root");
    let calls = Rc::new(RefCell::new(0usize));
    let seen = Rc::clone(&calls);
    scene
        .set_preflight_hook(move |_stage, _roots| {
            *seen.borrow_mut() += 1;
            if *seen.borrow() == 1 {
                Err(IntegrationError::new("preflight", "fixture failure"))
            } else {
                Ok(())
            }
        })
        .expect("hook");
    let mut sink = RecordingSink::default();

    assert!(matches!(
        scene.show(&mut sink),
        Err(SceneError::Integration(_))
    ));
    assert!(sink.captures.is_empty());
    scene.show(&mut sink).expect("retry succeeds");
    assert_eq!(*calls.borrow(), 2);
    assert_eq!(sink.captures.len(), 1);
}

#[test]
fn failed_preflight_event_does_not_commit_the_one_shot_barrier() {
    let mut scene = Scene::default();
    scene.add_mobject(point()).expect("root");
    let calls = Rc::new(RefCell::new(0usize));
    let seen = Rc::clone(&calls);
    scene
        .set_preflight_hook(move |_stage, _roots| {
            *seen.borrow_mut() += 1;
            Ok(())
        })
        .expect("hook");
    let mut sink = RecordingSink {
        fail_event_at: Some(0),
        ..RecordingSink::default()
    };

    assert!(matches!(
        scene.show(&mut sink),
        Err(SceneError::Integration(_))
    ));
    assert!(sink.captures.is_empty());
    sink.fail_event_at = None;
    scene.show(&mut sink).expect("retry succeeds");
    assert_eq!(
        *calls.borrow(),
        2,
        "preflight commit is atomic with its event"
    );
    assert_eq!(sink.captures.len(), 1);
}

#[test]
fn first_play_preflight_includes_unrooted_animation_closure() {
    let mut scene = Scene::default();
    let source = scene.stage_mut().add(point());
    let target = scene.stage_mut().add(point());
    scene.stage_mut().shift(target, [2.0, 0.0, 0.0]);
    let animation = replacement_transform(source, target);
    let seen = Rc::new(RefCell::new(Vec::new()));
    let captured = Rc::clone(&seen);
    scene
        .set_preflight_hook(move |_stage, anchors| {
            *captured.borrow_mut() = anchors.to_vec();
            Ok(())
        })
        .expect("hook");

    scene
        .play(
            vec![Box::new(animation)],
            PlayOverrides {
                run_time: Some(1.0 / 30.0),
                ..PlayOverrides::default()
            },
            &mut NullSceneSink,
        )
        .expect("play");
    let anchors = seen.borrow();
    assert!(anchors.contains(&source));
    assert!(
        anchors.contains(&target),
        "Transform targets may contain static typeset content too"
    );
}

#[test]
fn immutable_packets_materialize_with_live_handles_and_cow_isolation() {
    let mut scene = Scene::default();
    let mob = scene.add_mobject(point()).expect("root");
    let live_center = scene.stage().get_center(mob);
    let mut sink = PacketSink::default();
    scene.show(&mut sink).expect("barrier");
    let packet = sink.packet.expect("packet");

    let mut materialized = packet.materialize_stage();
    assert_eq!(materialized.get_center(mob), live_center);
    materialized.shift(mob, [4.0, -3.0, 0.0]);
    assert_ne!(materialized.get_center(mob), live_center);
    assert_eq!(
        scene.stage().get_center(mob),
        live_center,
        "materialized record writes cannot reach the live Scene"
    );
    assert_eq!(
        packet.materialize_stage().get_center(mob),
        live_center,
        "the immutable packet itself is unchanged"
    );
}

#[test]
fn show_runs_the_reference_zero_dt_updater_pass_before_capture() {
    let mut scene = Scene::default();
    let mob = scene.add_mobject(point()).expect("root");
    let calls = Rc::new(RefCell::new(0usize));
    let seen = Rc::clone(&calls);
    scene
        .stage_mut()
        .add_updater(
            mob,
            move |stage, me| {
                *seen.borrow_mut() += 1;
                stage.shift(me, [1.0, 0.0, 0.0]);
            },
            false,
        )
        .expect("updater");
    let mut sink = PacketSink::default();
    scene.show(&mut sink).expect("show");

    assert_eq!(*calls.borrow(), 1);
    let packet = sink.packet.expect("show packet");
    assert_eq!(packet.materialize_stage().get_center(mob), [1.0, 0.0, 0.0]);
    assert_eq!(packet.frame_index(), 0, "show advances no clock frames");
}

#[test]
fn played_and_skipped_segments_finish_in_identical_scene_state() {
    fn run(skip: bool) -> (Vec<u8>, usize) {
        let mut scene = Scene::new(
            RuntimeConfig {
                fps: 8,
                skip_animations: skip,
                ..RuntimeConfig::default()
            },
            99,
        )
        .expect("scene");
        let mob = scene.add_mobject(point()).expect("root");
        let nonlinear = scene.add_mobject(point()).expect("updater root");
        scene.stage_mut().shift(nonlinear, [1.0, 0.0, 0.0]);
        scene
            .stage_mut()
            .add_dt_updater(
                nonlinear,
                |stage, me, dt| {
                    let x = stage.get_center(me)[0];
                    stage.shift(me, [dt * (1.0 + x), 0.0, 0.0]);
                },
                false,
            )
            .expect("nonlinear updater");
        let builder = mob
            .animate()
            .set_anim_args(AnimateArgs {
                run_time: Some(0.5),
                rate_func: Some(fmn_core::rate::linear),
                ..AnimateArgs::default()
            })
            .and_then(|builder| builder.shift([3.0, -2.0, 0.0]))
            .expect("records");
        let animation = prepare_animation(builder, scene.stage_mut()).expect("prepares");
        let mut sink = RecordingSink::default();
        scene
            .play(vec![animation], PlayOverrides::default(), &mut sink)
            .expect("plays");
        (
            scene.state_bytes().expect("state bytes"),
            sink.captures.len(),
        )
    }

    let (played, played_frames) = run(false);
    let (skipped, skipped_frames) = run(true);
    assert_eq!(played, skipped, "skip changes cost, never terminal state");
    assert_eq!(played_frames, 4);
    assert_eq!(skipped_frames, 0);
}

#[test]
fn scene_play_consumes_specialized_animation_cleanup_and_family_membership() {
    let mut replacement = Scene::new(
        RuntimeConfig {
            fps: 4,
            ..RuntimeConfig::default()
        },
        0,
    )
    .expect("scene");
    let source = replacement.add_mobject(point()).expect("source");
    let target = replacement.stage_mut().add(point());
    replacement.stage_mut().shift(target, [2.0, 0.0, 0.0]);
    replacement
        .play(
            vec![Box::new(replacement_transform(source, target))],
            PlayOverrides {
                run_time: Some(0.25),
                ..PlayOverrides::default()
            },
            &mut NullSceneSink,
        )
        .expect("replacement transform");
    assert_eq!(
        replacement.mobjects(),
        [target],
        "ReplacementTransform removes the source and roots the real target"
    );

    let mut fade = Scene::new(
        RuntimeConfig {
            fps: 4,
            ..RuntimeConfig::default()
        },
        0,
    )
    .expect("scene");
    let source = fade.add_mobject(point()).expect("source");
    let source_before = fade
        .stage()
        .get(source)
        .expect("source entry")
        .buffer
        .read_column("point")
        .expect("point column")
        .to_vec();
    let target = fade.stage_mut().add(point());
    fade.stage_mut().shift(target, [3.0, 1.0, 0.0]);
    let animation = fade_transform(fade.stage_mut(), source, target).expect("fade transform");
    fade.play(
        vec![Box::new(animation)],
        PlayOverrides {
            run_time: Some(0.25),
            ..PlayOverrides::default()
        },
        &mut NullSceneSink,
    )
    .expect("fade transform plays");
    assert_eq!(
        fade.mobjects(),
        [target],
        "the temporary group replaces the formerly rooted source"
    );
    assert_eq!(
        fade.stage()
            .get(source)
            .expect("restored source")
            .buffer
            .read_column("point")
            .expect("restored point column")
            .to_vec(),
        source_before,
        "FadeTransform restores its saved source even though it stays off-stage"
    );
}

#[test]
fn start_and_end_range_flags_use_completed_play_indices() {
    let mut scene = Scene::new(
        RuntimeConfig {
            fps: 4,
            start_at_play: Some(2),
            end_at_play: Some(4),
            ..RuntimeConfig::default()
        },
        0,
    )
    .expect("scene");
    let mut sink = RecordingSink::default();
    for _ in 0..4 {
        scene.wait(Some(0.25), &mut sink).expect("in range");
    }
    assert_eq!(scene.play_count(), 4);
    assert_eq!(
        sink.captures
            .iter()
            .filter(|(reason, _, _)| *reason == CaptureReason::Segment)
            .count(),
        2,
        "indices 0 and 1 skip; 2 and 3 render"
    );
    assert!(matches!(
        scene.wait(Some(0.25), &mut sink),
        Err(SceneError::EndScene(EndScene))
    ));
}

#[test]
fn in_memory_and_durable_scene_state_restore_time_count_rng_and_roots() {
    let mut scene = Scene::new(
        RuntimeConfig {
            fps: 8,
            ..RuntimeConfig::default()
        },
        123,
    )
    .expect("scene");
    let root = scene.add_mobject(point()).expect("root");
    scene.wait(Some(0.5), &mut NullSceneSink).expect("wait");
    let _ = scene.rng_mut().next_u64();
    let state = scene.state().expect("state");
    let expected = state.to_bytes().expect("encodes");

    scene.clear();
    let _other = scene.add_mobject(point()).expect("other");
    scene.wait(Some(0.25), &mut NullSceneSink).expect("mutates");
    let _ = scene.rng_mut().next_u64();
    scene.restore_state(&state).expect("restores");
    assert_eq!(scene.mobjects(), &[root]);
    assert_eq!(scene.time().frames(), 4);
    assert_eq!(scene.play_count(), 1);
    assert_eq!(scene.state_bytes().expect("re-encodes"), expected);

    let mut fresh = Scene::new(
        RuntimeConfig {
            fps: 8,
            ..RuntimeConfig::default()
        },
        999,
    )
    .expect("fresh");
    let restored = fresh
        .restore_state_bytes(&expected)
        .expect("durable restore");
    assert!(restored.updaters.entries.is_empty());
    assert_eq!(fresh.time().frames(), 4);
    assert_eq!(fresh.play_count(), 1);
    assert_eq!(fresh.state_bytes().expect("canonical"), expected);
}

#[test]
fn durable_updater_restore_fails_closed_until_manifest_is_atomically_rebound() {
    let mut source = Scene::default();
    let mob = source.add_mobject(point()).expect("root");
    source
        .stage_mut()
        .add_dt_updater(mob, |_stage, _mob, _dt| {}, false)
        .expect("updater");
    source
        .stage_mut()
        .add_dt_updater(mob, |_stage, _mob, _dt| {}, false)
        .expect("second updater");
    let bytes = source.state_bytes().expect("state");

    let mut restored = Scene::default();
    let manifest = restored.restore_state_bytes(&bytes).expect("decode");
    assert_eq!(manifest.updaters.entries.len(), 1);
    assert_eq!(manifest.updaters.entries[0].1.len(), 2);
    assert!(matches!(
        restored.wait(Some(0.1), &mut NullSceneSink),
        Err(SceneError::UnboundUpdaters)
    ));
    assert!(matches!(restored.state(), Err(SceneError::UnboundUpdaters)));
    assert!(matches!(
        restored.state_bytes(),
        Err(SceneError::UnboundUpdaters)
    ));

    let mut partial = Scene::default();
    partial.restore_state_bytes(&bytes).expect("decode");
    let mut resolutions = 0;
    assert!(matches!(
        partial.rebind_updaters(|_bound, _id, _kind| {
            resolutions += 1;
            if resolutions == 1 {
                Ok(UpdaterFn::Dt(Rc::new(RefCell::new(
                    |_stage: &mut Stage, _mob: fmn_mobject::Mob, _dt: f64| {},
                ))))
            } else {
                Err(IntegrationError::new(
                    "replay",
                    "fixture resolution failure",
                ))
            }
        }),
        Err(SceneError::Integration(_))
    ));
    assert!(
        partial
            .stage()
            .updater_ids(partial.mobjects()[0])
            .is_empty(),
        "resolver failure cannot install a prefix"
    );
    assert!(matches!(
        partial.wait(Some(0.1), &mut NullSceneSink),
        Err(SceneError::UnboundUpdaters)
    ));

    let mut seen_identities = Vec::new();
    restored
        .rebind_updaters(|bound, id, kind| {
            assert_eq!(kind, UpdaterKindTag::Dt);
            seen_identities.push(id.raw());
            Ok(UpdaterFn::Dt(Rc::new(RefCell::new(
                move |stage: &mut Stage, _mob: fmn_mobject::Mob, dt: f64| {
                    stage.shift(bound, [dt, 0.0, 0.0]);
                },
            ))))
        })
        .expect("manifest callback is rebound");
    assert_eq!(seen_identities.len(), 2);
    assert_eq!(
        restored.state_bytes().expect("rebound state"),
        bytes,
        "rebind restores the durable state without identity drift"
    );
    restored
        .wait(Some(0.1), &mut NullSceneSink)
        .expect("real rebinding opens playback");
    let restored_mob = restored.mobjects()[0];
    assert!(
        restored.stage().get_center(restored_mob)[0] > 0.0,
        "the rebound callback executes"
    );
    assert_eq!(
        restored
            .stage()
            .updater_ids(restored_mob)
            .into_iter()
            .map(|id| id.raw())
            .collect::<Vec<_>>(),
        seen_identities,
        "the durable removal identity is retained"
    );
}

#[test]
fn off_grid_or_nonfinite_state_time_is_refused() {
    let mut scene = Scene::new(
        RuntimeConfig {
            fps: 4,
            ..RuntimeConfig::default()
        },
        0,
    )
    .expect("scene");
    let mut state = scene.state().expect("state");
    state.time = 0.1;
    assert!(matches!(
        scene.restore_state(&state),
        Err(SceneError::InvalidState(_))
    ));
    state.time = f64::NAN;
    assert!(matches!(
        scene.restore_state(&state),
        Err(SceneError::InvalidState(_))
    ));
    state.time = i64::MAX as f64 / 4.0;
    assert!(matches!(
        scene.restore_state(&state),
        Err(SceneError::InvalidState(
            "time exceeds the rational frame counter"
        ))
    ));

    let mut exhausted = scene.state().expect("state");
    exhausted.time = 0.0;
    exhausted.play_count = u64::MAX;
    scene
        .restore_state(&exhausted)
        .expect("representable state");
    assert!(matches!(
        scene.wait(Some(0.25), &mut NullSceneSink),
        Err(SceneError::InvalidState("play count is exhausted"))
    ));
    assert_eq!(scene.time().frames(), 0, "refusal is atomic");

    let mut other = Scene::default();
    let foreign = other.state().expect("state");
    assert!(matches!(
        scene.restore_state(&foreign),
        Err(SceneError::InvalidState(
            "an in-memory SceneState belongs to a different scene"
        ))
    ));
}

#[test]
fn presenter_holds_advance_on_the_clock_and_release_deterministically() {
    let mut scene = Scene::new(
        RuntimeConfig {
            fps: 4,
            presenter_mode: true,
            ..RuntimeConfig::default()
        },
        0,
    )
    .expect("scene");
    let mut initial = 0;
    let mut wait = 0;
    scene.set_hold_controller(move |kind, _stage: &Stage, _time| {
        let counter = match kind {
            HoldKind::Initial => &mut initial,
            HoldKind::Wait => &mut wait,
        };
        let limit = match kind {
            HoldKind::Initial => 1,
            HoldKind::Wait => 2,
        };
        if *counter < limit {
            *counter += 1;
            Ok(HoldDecision::Continue)
        } else {
            Ok(HoldDecision::Release)
        }
    });
    let mut sink = RecordingSink::default();
    let report = scene.wait(Some(99.0), &mut sink).expect("presenter wait");
    assert_eq!(report.kind, SegmentKind::Wait);
    assert_eq!(report.n_frames, 2, "initial hold is not the wait segment");
    assert_eq!(scene.time().frames(), 3);
    assert_eq!(scene.play_count(), 1);
    assert_eq!(
        sink.captures
            .iter()
            .map(|capture| capture.0)
            .collect::<Vec<_>>(),
        vec![
            CaptureReason::PresenterHold,
            CaptureReason::PresenterHold,
            CaptureReason::PresenterHold,
        ]
    );

    let mut ranged = Scene::new(
        RuntimeConfig {
            fps: 4,
            presenter_mode: true,
            start_at_play: Some(1),
            ..RuntimeConfig::default()
        },
        0,
    )
    .expect("ranged presenter");
    let mut polls = 0;
    ranged.set_hold_controller(move |_kind, _stage: &Stage, _time| {
        polls += 1;
        Ok(if polls == 1 {
            HoldDecision::Continue
        } else {
            HoldDecision::Release
        })
    });
    let mut sink = RecordingSink::default();
    ranged
        .wait(Some(0.25), &mut sink)
        .expect("skipped initial wait");
    assert!(
        sink.captures.is_empty(),
        "skip mode suppresses presenter captures as well as segment captures"
    );
}

#[test]
fn three_d_defaults_depth_and_ambient_rotation_are_snapshotted_stage_state() {
    let mut scene = ThreeDScene::new(
        RuntimeConfig {
            fps: 4,
            ..RuntimeConfig::default()
        },
        0,
    )
    .expect("3D scene");
    assert_eq!(scene.samples(), 4);
    assert!(scene.always_depth_test());
    let expected = CameraOrientation::from_degrees(-30.0, 70.0, 0.0);
    let actual = scene.orientation();
    assert!((actual.theta - expected.theta).abs() < 1e-15);
    assert!((actual.phi - expected.phi).abs() < 1e-15);
    assert!(matches!(
        scene.set_orientation(CameraOrientation {
            theta: f64::NAN,
            phi: 0.0,
            gamma: 0.0,
        }),
        Err(SceneError::InvalidConfig(_))
    ));

    let mob = scene.stage_mut().add(point());
    scene
        .add(&[mob], ThreeDAddOptions::default())
        .expect("3D add");
    assert!(scene.stage().uniforms(mob).expect("uniforms").depth_test);

    scene.add_ambient_rotation(1.0).expect("ambient updater");
    scene
        .wait(Some(0.5), &mut NullSceneSink)
        .expect("clock advances");
    assert!((scene.orientation().theta - (expected.theta + 0.5)).abs() < 1e-12);

    let state = scene.state().expect("state");
    scene
        .set_orientation(CameraOrientation::from_degrees(0.0, 0.0, 0.0))
        .expect("changes");
    scene
        .restore_state(&state)
        .expect("restores camera trackers");
    assert!((scene.orientation().theta - (expected.theta + 0.5)).abs() < 1e-12);
}

#[test]
fn three_d_add_refuses_a_stale_batch_before_mutating_live_members() {
    let mut scene = ThreeDScene::default();
    let live = scene.stage_mut().add(point());
    scene
        .stage_mut()
        .uniforms_mut(live)
        .expect("live uniforms")
        .flat_stroke = true;
    let stale = scene.stage_mut().add(point());
    scene.stage_mut().delete(stale).expect("detached mobject");

    assert!(matches!(
        scene.add(&[live, stale], ThreeDAddOptions::default()),
        Err(SceneError::Stage(StageError::StaleHandle))
    ));
    let uniforms = scene.stage().uniforms(live).expect("live uniforms");
    assert!(!uniforms.depth_test);
    assert!(uniforms.flat_stroke);
    assert!(
        !scene.mobjects().contains(&live),
        "a refused batch must not partially root its live prefix"
    );
}

#[test]
fn three_d_orientation_refuses_a_broken_tracker_before_partial_mutation() {
    let mut scene = ThreeDScene::default();
    let before = scene.orientation();
    let phi = scene
        .stage()
        .family(scene.camera_root())
        .get(2)
        .copied()
        .expect("phi tracker");
    scene.stage_mut().delete(phi).expect("delete phi tracker");

    assert!(matches!(
        scene.set_orientation(CameraOrientation::from_degrees(10.0, 20.0, 30.0)),
        Err(SceneError::Stage(StageError::StaleHandle))
    ));
    assert_eq!(
        scene.orientation().theta.to_bits(),
        before.theta.to_bits(),
        "theta must not mutate before the missing phi tracker is refused"
    );
}

#[derive(Default)]
struct EndsInConstruct;

impl SceneProgram for EndsInConstruct {
    fn name(&self) -> &str {
        "EndsInConstruct"
    }

    fn construct(
        &mut self,
        scene: &mut Scene,
        _sink: &mut dyn SceneSink,
    ) -> Result<(), SceneError> {
        scene.end()
    }
}

#[test]
fn run_catches_end_scene_and_still_tears_down() {
    let mut scene = Scene::default();
    let mut program = EndsInConstruct;
    let mut sink = RecordingSink::default();
    let report = scene.run(&mut program, &mut sink).expect("normal end");
    assert!(report.ended_early);
    assert_eq!(
        sink.events
            .iter()
            .map(|event| event.phase)
            .collect::<Vec<_>>(),
        vec![
            LifecyclePhase::SceneBegin,
            LifecyclePhase::Setup,
            LifecyclePhase::Construct,
            LifecyclePhase::TearDown,
            LifecyclePhase::SceneEnd,
        ]
    );
}

#[derive(Default)]
struct FailsInTearDown;

impl SceneProgram for FailsInTearDown {
    fn construct(
        &mut self,
        _scene: &mut Scene,
        _sink: &mut dyn SceneSink,
    ) -> Result<(), SceneError> {
        Ok(())
    }

    fn tear_down(
        &mut self,
        _scene: &mut Scene,
        _sink: &mut dyn SceneSink,
    ) -> Result<(), SceneError> {
        Err(SceneError::InvalidLifecycle("fixture tear-down failure"))
    }
}

#[test]
fn scene_end_is_attempted_even_when_program_teardown_fails() {
    let mut scene = Scene::default();
    let mut program = FailsInTearDown;
    let mut sink = RecordingSink::default();
    assert!(matches!(
        scene.run(&mut program, &mut sink),
        Err(SceneError::InvalidLifecycle("fixture tear-down failure"))
    ));
    assert_eq!(
        sink.events.last().map(|event| event.phase),
        Some(LifecyclePhase::SceneEnd)
    );
}

#[test]
fn sink_failure_surfaces_after_deterministic_segment_completion() {
    let mut scene = Scene::new(
        RuntimeConfig {
            fps: 4,
            ..RuntimeConfig::default()
        },
        0,
    )
    .expect("scene");
    let mut sink = RecordingSink {
        fail_capture_at: Some(0),
        ..RecordingSink::default()
    };
    let error = scene.wait(Some(0.5), &mut sink).expect_err("sink fails");
    assert!(matches!(error, SceneError::Integration(_)));
    assert_eq!(scene.time().frames(), 2);
    assert_eq!(scene.play_count(), 1);
    assert!(sink.captures.is_empty());
}

#[derive(Default)]
struct A;

impl SceneProgram for A {
    fn name(&self) -> &str {
        "A"
    }

    fn construct(
        &mut self,
        _scene: &mut Scene,
        _sink: &mut dyn SceneSink,
    ) -> Result<(), SceneError> {
        Ok(())
    }
}

#[derive(Default)]
struct B;

impl SceneProgram for B {
    fn name(&self) -> &str {
        "B"
    }

    fn construct(
        &mut self,
        _scene: &mut Scene,
        _sink: &mut dyn SceneSink,
    ) -> Result<(), SceneError> {
        Ok(())
    }
}

#[test]
fn registry_selection_and_blank_scene_match_reference_control_flow() {
    let blank = SceneRegistry::blank();
    let selected = blank.select(&["Missing"], false).expect("one means all");
    assert_eq!(selected.scenes[0].name(), "BlankScene");
    let mut blank_program = selected.scenes[0].instantiate();
    Scene::default()
        .run(blank_program.as_mut(), &mut NullSceneSink)
        .expect("BlankScene smoke");

    let mut registry = SceneRegistry::default();
    registry.register::<B>("B").expect("register");
    registry.register::<A>("A").expect("register");
    let all = registry.select(&[], true).expect("write all");
    assert_eq!(
        all.scenes
            .iter()
            .map(|scene| scene.name())
            .collect::<Vec<_>>(),
        ["B", "A"]
    );
    let one = registry.select(&["A", "Missing"], false).expect("select");
    assert_eq!(one.scenes[0].name(), "A");
    assert_eq!(one.missing, ["Missing"]);
    assert!(matches!(
        registry.select(&[], false),
        Err(SceneSelectionError::SelectionRequired { .. })
    ));
    assert!(matches!(
        registry.register::<A>("A"),
        Err(SceneSelectionError::DuplicateName(_))
    ));
}

#[test]
fn output_naming_matrix_preserves_range_partial_and_host_open_contracts() {
    let naming = OutputNaming {
        output_directory: "/tmp/videos".into(),
        start_at_play: Some(3),
        end_at_play: Some(6),
        open_on_completion: true,
        ..OutputNaming::default()
    };
    assert_eq!(
        naming.artifact("Demo", ".mp4"),
        std::path::PathBuf::from("/tmp/videos/Demo_3_6.mp4")
    );
    assert_eq!(
        naming.artifact("Demo", "png"),
        std::path::PathBuf::from("/tmp/videos/Demo_3_6.png")
    );
    assert_eq!(
        naming.partial_artifact("Demo", 12, "gif"),
        std::path::PathBuf::from("/tmp/videos/Demo_3_6/00012.gif")
    );
    let completion = naming.completion_request("Demo", "y4m");
    assert!(completion.open);
    assert_eq!(
        completion.artifact(),
        std::path::Path::new("/tmp/videos/Demo_3_6.y4m")
    );

    let explicit = OutputNaming {
        output_directory: "/tmp".into(),
        file_name: Some("custom.name.mp4".into()),
        ..OutputNaming::default()
    };
    assert_eq!(
        explicit.artifact("Ignored", "png"),
        std::path::PathBuf::from("/tmp/custom.name.png")
    );
}

#[test]
fn direct_scene_state_fixture_can_restore_rng_exactly() {
    let mut scene = Scene::default();
    let captured = scene.state().expect("state");
    let mut expected = Pcg64Dxsm::restore(captured.rng_state.0, captured.rng_state.1);
    let expected_draw = expected.next_u64();
    let _ = scene.rng_mut().next_u64();
    scene.restore_state(&captured).expect("restore");
    assert_eq!(scene.rng_mut().next_u64(), expected_draw);

    // Keep the public persistence type in the acceptance surface.
    let _: &SceneState = &captured;
}
