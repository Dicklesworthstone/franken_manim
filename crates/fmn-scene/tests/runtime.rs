//! Scene runtime acceptance corpus (fm-5xm): lifecycle, stable membership,
//! skip/range equivalence, state restore, preflight, presenter holds, 3D
//! defaults, discovery/selection, BlankScene, and output naming.

use std::cell::RefCell;
use std::rc::Rc;

use fmn_anim::{
    AnimError, ClockError, SegmentKind, Succession, clock_passes_time, fade_transform,
    prepare_animation, replacement_transform,
};
use fmn_core::rng::Pcg64Dxsm;
use fmn_mobject::animate::AnimateArgs;
use fmn_mobject::{Mobject, SceneState, Stage, StageError, UpdaterFn, UpdaterKindTag};
use fmn_scene::{
    CameraOrientation, CaptureReason, EndScene, HoldDecision, HoldKind, IntegrationError,
    LifecycleEvent, LifecyclePhase, NullSceneSink, OutputNaming, PlayOverrides, RuntimeConfig,
    Scene, SceneError, SceneProgram, SceneRegistry, SceneSelectionError, SceneSink, SoundRequest,
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
fn clock_passes_time_preserves_the_clock_root_and_its_updater_lifecycle() {
    for initially_rooted in [false, true] {
        for skip in [false, true] {
            let mut scene = Scene::new(
                RuntimeConfig {
                    fps: 12,
                    skip_animations: skip,
                    preview_while_skipping: false,
                    ..RuntimeConfig::default()
                },
                7,
            )
            .unwrap();
            // The Reference Clock's child order is face, hour, minute, ticks.
            let clock = scene.stage_mut().add(Mobject::group(vec![
                Mobject::from_points(&[[-1.0, -1.0, 0.0], [1.0, 1.0, 0.0]]),
                Mobject::from_points(&[[0.0, 0.0, 0.0], [0.0, 0.3, 0.0]]),
                Mobject::from_points(&[[0.0, 0.0, 0.0], [0.0, 0.6, 0.0]]),
                Mobject::from_points(&[[-0.9, 0.0, 0.0], [0.9, 0.0, 0.0]]),
            ]));
            if initially_rooted {
                scene.add(&[clock]).unwrap();
            }
            let children = scene.stage().get(clock).unwrap().submobjects().to_vec();
            let updates = Rc::new(RefCell::new(Vec::new()));
            let observed = Rc::clone(&updates);
            scene
                .stage_mut()
                .add_dt_updater(
                    clock,
                    move |stage, me, dt| {
                        if dt > 0.0 {
                            observed.borrow_mut().push((me, stage.is_animating(me), dt));
                        }
                    },
                    false,
                )
                .unwrap();
            let animation =
                clock_passes_time(scene.stage_mut(), clock, children[1], children[2], 0.5, 3.0)
                    .unwrap();
            let mut sink = PacketSink::default();
            let report = scene
                .play(
                    vec![Box::new(animation)],
                    PlayOverrides::default(),
                    &mut sink,
                )
                .unwrap()
                .unwrap();

            assert_eq!(report.n_frames, 6);
            assert_eq!(scene.time().frames(), 6);
            assert_eq!(scene.play_count(), 1);
            assert_eq!(scene.mobjects(), &[clock]);
            assert_eq!(scene.stage().get(clock).unwrap().submobjects(), children);
            assert_eq!(updates.borrow().as_slice(), &[(clock, true, 1.0 / 12.0); 6]);
            for member in scene.stage().family(clock) {
                assert!(!scene.stage().is_animating(member));
            }
            for (hand, expected) in [
                (children[1], [0.15, 0.0, 0.0]),
                (children[2], [0.0, 0.3, 0.0]),
            ] {
                let actual = scene.stage().get_center(hand);
                assert!(
                    actual
                        .iter()
                        .zip(expected)
                        .all(|(a, b)| (a - b).abs() < 1e-6)
                );
            }
            assert_eq!(scene.stage().get_center(children[0]), [0.0; 3]);
            assert_eq!(scene.stage().get_center(children[3]), [0.0; 3]);
            if skip {
                assert!(sink.packet.is_none());
            } else {
                let captured = sink.packet.expect("last clock frame").materialize_stage();
                assert_eq!(captured.roots(), &[clock]);
                assert_eq!(captured.get(clock).unwrap().submobjects(), children);
            }
        }
    }
}

#[test]
fn exact_frame_segments_keep_audio_cues_and_reports_on_the_requested_grid() {
    // Issue #10's complete alternating play/wait example, including every
    // duration whose n/24 representation lies above its intended boundary.
    const FRAMES: [u64; 20] = [
        43, 17, 61, 29, 101, 7, 55, 13, 89, 31, 47, 19, 73, 11, 37, 23, 67, 3, 97, 41,
    ];
    for fps in [1, 24, 25, 30, 60, 120] {
        let mut scene = Scene::new(
            RuntimeConfig {
                fps,
                ..RuntimeConfig::default()
            },
            10,
        )
        .unwrap();
        let mob = scene.add_mobject(point()).unwrap();
        let mut sink = RecordingSink::default();
        let mut elapsed = 0_i64;
        for (index, frames) in FRAMES.into_iter().enumerate() {
            scene.add_sound("click.wav", 0.0, None, None).unwrap();
            let report = if index % 2 == 0 {
                let builder = mob.animate().shift([0.05, 0.0, 0.0]).unwrap();
                let animation = prepare_animation(builder, scene.stage_mut()).unwrap();
                scene
                    .play(
                        vec![animation],
                        PlayOverrides {
                            run_time_frames: Some(frames),
                            ..PlayOverrides::default()
                        },
                        &mut sink,
                    )
                    .unwrap()
                    .unwrap()
            } else {
                scene.wait_frames(frames, &mut sink).unwrap()
            };
            assert_eq!(report.base_frame, elapsed);
            assert_eq!(report.n_frames, frames as i64);
            assert_eq!(scene.sound_requests()[index].time.frames(), elapsed);
            assert_eq!(scene.sound_requests()[index].time.fps(), fps);
            elapsed += frames as i64;
            assert_eq!(scene.time(), fmn_anim::RationalTime::zero(fps) + elapsed);
        }
        assert_eq!(elapsed, 864);
        assert_eq!(scene.play_count(), 20);
        assert_eq!(sink.captures.len(), 864);
        assert!(sink.captures.iter().enumerate().all(|(index, capture)| {
            capture.0 == CaptureReason::Segment && capture.1 == index as i64 + 1
        }));
        assert!((scene.stage().get_center(mob)[0] - 0.5).abs() < 1e-6);
    }
}

#[test]
fn frame_waits_mix_with_seconds_and_zero_segments_without_cumulative_rounding() {
    let mut scene = Scene::new(
        RuntimeConfig {
            fps: 30,
            ..RuntimeConfig::default()
        },
        0,
    )
    .unwrap();
    let mob = scene.add_mobject(point()).unwrap();
    let updates = Rc::new(RefCell::new(Vec::new()));
    let observed = updates.clone();
    scene
        .stage_mut()
        .add_dt_updater(
            mob,
            move |_, _, dt| {
                if dt > 0.0 {
                    observed.borrow_mut().push(dt);
                }
            },
            false,
        )
        .unwrap();
    let mut sink = RecordingSink::default();
    // 0.1 remains four frames at 30 fps; exact three-frame requests do not
    // change the established meaning of that second-based input.
    assert_eq!(scene.wait(Some(0.1), &mut sink).unwrap().n_frames, 4);
    assert_eq!(scene.wait_frames(0, &mut sink).unwrap().n_frames, 0);
    for _ in 0..2_000 {
        assert_eq!(scene.wait_frames(3, &mut sink).unwrap().n_frames, 3);
        assert_eq!(scene.wait_frames(0, &mut sink).unwrap().n_frames, 0);
    }
    assert_eq!(scene.time().frames(), 6_004);
    assert_eq!(sink.captures.len(), 6_004);
    assert_eq!(updates.borrow().len(), 6_004);
    assert!(updates.borrow().iter().all(|dt| *dt == 1.0 / 30.0));
}

#[test]
fn exact_frame_stepped_and_skipped_routes_keep_each_updater_boundary() {
    for skip in [false, true] {
        let mut scene = Scene::new(
            RuntimeConfig {
                fps: 24,
                skip_animations: skip,
                preview_while_skipping: false,
                ..RuntimeConfig::default()
            },
            0,
        )
        .unwrap();
        let mob = scene.add_mobject(point()).unwrap();
        let mut sink = RecordingSink::default();
        let mut wait = scene.begin_stepped_wait_frames(17, &mut sink).unwrap();
        let mut boundaries = 0;
        while let Some(boundary) = scene.prepare_stepped_wait_frame(&mut wait).unwrap() {
            boundaries += 1;
            assert_eq!(boundary.time.frames(), boundaries);
            scene.stage_mut().shift(mob, [0.25, 0.0, 0.0]);
            scene
                .complete_stepped_wait_frame(&mut wait, &mut sink)
                .unwrap();
        }
        assert_eq!(
            scene.finish_stepped_wait(wait, &mut sink).unwrap().n_frames,
            17
        );
        let animation = prepare_animation(
            mob.animate().shift([1.0, 0.0, 0.0]).unwrap(),
            scene.stage_mut(),
        )
        .unwrap();
        let mut play = scene
            .begin_stepped_play(
                vec![animation],
                PlayOverrides {
                    run_time_frames: Some(43),
                    ..PlayOverrides::default()
                },
                &mut sink,
            )
            .unwrap()
            .unwrap();
        while scene
            .prepare_stepped_play_frame(&mut play)
            .unwrap()
            .is_some()
        {
            boundaries += 1;
            scene
                .complete_stepped_play_frame(&mut play, &mut sink)
                .unwrap();
        }
        assert_eq!(
            scene.finish_stepped_play(play, &mut sink).unwrap().n_frames,
            43
        );
        assert_eq!(boundaries, 60);
        assert_eq!(scene.time().frames(), 60);
        assert_eq!(sink.captures.len(), if skip { 0 } else { 60 });
        assert_eq!(scene.stage().get_center(mob)[0], 5.25);
    }
}

#[test]
fn exact_frame_limits_and_ambiguous_overrides_fail_before_lifecycle() {
    let mut scene = Scene::new(
        RuntimeConfig {
            fps: 24,
            ..RuntimeConfig::default()
        },
        0,
    )
    .unwrap();
    let mob = scene.add_mobject(point()).unwrap();
    let mut sink = RecordingSink::default();
    assert!(matches!(
        scene.wait_frames(i64::MAX as u64 + 1, &mut sink),
        Err(SceneError::Animation(AnimError::Clock(
            ClockError::RunTimeTooLong
        )))
    ));
    let animation = prepare_animation(
        mob.animate().shift([1.0, 0.0, 0.0]).unwrap(),
        scene.stage_mut(),
    )
    .unwrap();
    assert!(matches!(
        scene.play(
            vec![animation],
            PlayOverrides {
                run_time: Some(1.0),
                run_time_frames: Some(24),
                ..PlayOverrides::default()
            },
            &mut sink
        ),
        Err(SceneError::InvalidConfig(
            "run_time and run_time_frames are mutually exclusive"
        ))
    ));
    assert!(sink.events.is_empty());
    assert!(sink.captures.is_empty());
    assert_eq!(scene.time().frames(), 0);
    assert_eq!(scene.play_count(), 0);

    let mut state = scene.state().unwrap();
    state.frames_elapsed = i64::MAX - 1;
    scene.restore_state(&state).unwrap();
    assert!(matches!(
        scene.wait_frames(2, &mut sink),
        Err(SceneError::Animation(AnimError::Clock(
            ClockError::FrameCounterOverflow
        )))
    ));
    assert!(sink.events.is_empty());
    scene.wait_frames(1, &mut sink).unwrap();
    assert_eq!(scene.time().frames(), i64::MAX);
    assert_eq!(scene.wait_frames(0, &mut sink).unwrap().n_frames, 0);
    assert_eq!(scene.time().frames(), i64::MAX);
}

#[test]
fn frame_waits_use_the_active_grid_and_explicit_stop_bounds() {
    let mut scene = Scene::new(
        RuntimeConfig {
            fps: 24,
            windowed: true,
            presenter_mode: true,
            ..RuntimeConfig::default()
        },
        0,
    )
    .unwrap();
    // Exact finite waits do not turn into presenter-controlled holds.
    let mut sink = RecordingSink::default();
    assert_eq!(scene.wait_frames(17, &mut sink).unwrap().n_frames, 17);
    assert_eq!(scene.time().fps(), 30);
    assert_eq!(scene.time().frames(), 17);
    let mut observed = 0;
    let report = scene
        .wait_until_frames(
            10,
            &mut |_| {
                observed += 1;
                observed == 3
            },
            &mut sink,
        )
        .unwrap();
    assert_eq!(report.n_frames, 3);
    assert_eq!(scene.time().frames(), 20);
    scene
        .wait_until_frames(0, &mut |_| panic!("no frame, no predicate"), &mut sink)
        .unwrap();
    assert_eq!(scene.time().frames(), 20);
}

#[test]
fn the_scene_stage_aligns_on_the_configured_frame() {
    // The Reference derives FRAME_WIDTH from the configured resolution
    // (constants.py:17-20): a portrait 1080x1920 render has the frame
    // (4.5, 8), so to_edge(LEFT, buff=0) puts a left edge at -2.25 (fm-8azr).
    let portrait = fmn_config::Config::resolve(
        &[fmn_config::config::Layer {
            name: "custom_config.yml",
            text: "camera:\n  resolution: (1080, 1920)\n",
        }],
        None,
    )
    .expect("portrait config resolves")
    .config;
    let runtime = RuntimeConfig::from_config(&portrait);
    assert_eq!(runtime.frame_shape, [4.5, 8.0]);
    let mut scene = Scene::new(runtime, 0).expect("portrait scene");
    assert_eq!(scene.stage().frame_shape(), [4.5, 8.0]);
    let square = scene
        .stage_mut()
        .add(Mobject::from_points(&[[-0.5, -0.5, 0.0], [0.5, 0.5, 0.0]]));
    scene
        .stage_mut()
        .to_edge(square, fmn_core::constants::LEFT, 0.0);
    assert!((scene.stage().get_left(square)[0] + 2.25).abs() < 1e-12);

    // The default config keeps the 16:9 frame bit for bit.
    let default = fmn_config::Config::resolve(&[], None)
        .expect("defaults")
        .config;
    assert_eq!(
        RuntimeConfig::from_config(&default).frame_shape,
        [
            fmn_core::constants::FRAME_WIDTH,
            fmn_core::constants::FRAME_HEIGHT
        ]
    );
    assert_eq!(
        RuntimeConfig::default().frame_shape,
        RuntimeConfig::from_config(&default).frame_shape
    );

    // An audio-only render (a zero resolution side) keeps the default frame.
    let mut audio_only = default.clone();
    audio_only.camera.resolution = (0, 0);
    assert_eq!(
        RuntimeConfig::from_config(&audio_only).frame_shape,
        RuntimeConfig::default().frame_shape
    );

    // A frame that is not finite and positive is a typed refusal.
    let bad = RuntimeConfig {
        frame_shape: [0.0, 8.0],
        ..RuntimeConfig::default()
    };
    assert!(matches!(
        Scene::new(bad, 0),
        Err(SceneError::InvalidConfig(_))
    ));
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

    let x = scene.stage_mut().add(point());
    let y = scene.stage_mut().add(point());
    scene.replace(c, &[x, y]).expect("live replacements");
    assert_eq!(scene.mobjects(), &[b, x, y, a]);

    let absent = scene.stage_mut().add(point());
    scene
        .replace(absent, &[a])
        .expect("absent source is a no-op");
    assert_eq!(scene.mobjects(), &[b, x, y, a]);

    scene.replace(x, &[]).expect("zero replacements remove");
    assert_eq!(scene.mobjects(), &[b, y, a]);
    scene.remove(&[y]);
    assert_eq!(scene.mobjects(), &[b, a]);

    scene
        .remove_all_except(&[a, b, a])
        .expect("live batch replaces the draw list");
    assert_eq!(scene.mobjects(), &[b, a, a]);

    let stale = scene.stage_mut().add(point());
    scene
        .stage_mut()
        .delete(stale)
        .expect("detached handle dies");
    assert!(matches!(
        scene.remove_all_except(&[stale]),
        Err(SceneError::Stage(StageError::StaleHandle))
    ));
    assert_eq!(
        scene.mobjects(),
        &[b, a, a],
        "stale batch refusal must not clear the scene"
    );

    scene
        .remove_all_except(&[])
        .expect("empty batch clears the draw list");
    assert!(scene.mobjects().is_empty());

    scene.add(&[a]).expect("cleared handles remain live");
    scene.clear();
    assert!(scene.mobjects().is_empty());
    assert!(
        scene.stage().contains(a),
        "clear is membership, not deletion"
    );
}

#[test]
fn add_sound_keeps_exact_scene_time_and_reference_arguments() {
    let mut scene = Scene::new(
        RuntimeConfig {
            fps: 8,
            ..RuntimeConfig::default()
        },
        3,
    )
    .expect("scene");
    scene
        .wait(Some(0.25), &mut NullSceneSink)
        .expect("advance to frame two");
    scene
        .add_sound("click.wav", -0.125, Some(-6.0), Some(-12.0))
        .expect("sound request");

    assert_eq!(
        scene.sound_requests(),
        &[SoundRequest {
            sound_file: "click.wav".into(),
            time: fmn_anim::RationalTime::zero(8) + 2,
            time_offset: -0.125,
            gain: Some(-6.0),
            gain_to_background: Some(-12.0),
        }]
    );

    scene.force_skipping();
    scene
        .add_sound("ignored.wav", f64::NAN, Some(f64::INFINITY), None)
        .expect("skip is the Reference's early no-op");
    assert_eq!(scene.sound_requests().len(), 1);
    scene.revert_to_original_skipping_status();
    assert!(matches!(
        scene.add_sound("bad.wav", 0.0, None, Some(f64::NEG_INFINITY)),
        Err(SceneError::InvalidConfig(_))
    ));
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
fn stepped_play_releases_before_native_scene_updaters_and_capture() {
    let mut scene = Scene::new(
        RuntimeConfig {
            fps: 10,
            ..RuntimeConfig::default()
        },
        7,
    )
    .expect("scene");
    let animated = scene.add_mobject(point()).expect("animated root");
    let follower = scene.add_mobject(point()).expect("follower root");
    scene
        .stage_mut()
        .add_updater(
            follower,
            move |stage, me| {
                stage.set_x(me, stage.get_center(animated)[0]);
            },
            false,
        )
        .expect("native updater");
    let builder = animated
        .animate()
        .set_anim_args(AnimateArgs {
            run_time: Some(1.0),
            rate_func: Some(fmn_core::rate::linear),
            ..AnimateArgs::default()
        })
        .and_then(|builder| builder.shift([2.0, 0.0, 0.0]))
        .expect("records");
    let animation = prepare_animation(builder, scene.stage_mut()).expect("prepares");
    let mut sink = PacketSink::default();
    let mut play = scene
        .begin_stepped_play(vec![animation], PlayOverrides::default(), &mut sink)
        .expect("begins")
        .expect("nonempty play");

    while let Some(boundary) = scene
        .prepare_stepped_play_frame(&mut play)
        .expect("prepares frame")
    {
        assert_eq!(scene.time(), boundary.time);
        // This mutation stands in for Python work performed while the
        // BridgeScene RefCell borrow is released.
        scene.stage_mut().shift(animated, [0.25, 0.0, 0.0]);
        scene
            .complete_stepped_play_frame(&mut play, &mut sink)
            .expect("completes frame");
    }
    scene
        .finish_stepped_play(play, &mut sink)
        .expect("finishes play");

    let packet = sink.packet.expect("last frame captured");
    let captured = packet.materialize_stage();
    assert!((captured.get_center(animated)[0] - 2.25).abs() < 1e-5);
    assert!((captured.get_center(follower)[0] - 2.25).abs() < 1e-5);
    assert_eq!(scene.play_count(), 1);
    assert!((scene.stage().get_center(animated)[0] - 2.0).abs() < 1e-5);
    assert!((scene.stage().get_center(follower)[0] - 2.0).abs() < 1e-5);
}

#[test]
fn stepped_wait_uses_the_same_scene_updater_release_window() {
    let mut scene = Scene::new(
        RuntimeConfig {
            fps: 10,
            ..RuntimeConfig::default()
        },
        7,
    )
    .expect("scene");
    let source = scene.add_mobject(point()).expect("source root");
    let follower = scene.add_mobject(point()).expect("follower root");
    scene
        .stage_mut()
        .add_updater(
            follower,
            move |stage, me| {
                stage.set_x(me, stage.get_center(source)[0]);
            },
            false,
        )
        .expect("native updater");
    let mut sink = PacketSink::default();
    let mut wait = scene
        .begin_stepped_wait(Some(1.0), &mut sink)
        .expect("begins");
    while scene
        .prepare_stepped_wait_frame(&mut wait)
        .expect("prepares")
        .is_some()
    {
        scene.stage_mut().shift(source, [0.1, 0.0, 0.0]);
        scene
            .complete_stepped_wait_frame(&mut wait, &mut sink)
            .expect("completes");
    }
    let report = scene
        .finish_stepped_wait(wait, &mut sink)
        .expect("finishes");
    assert_eq!(report.n_frames, 10);
    let captured = sink.packet.expect("last frame").materialize_stage();
    assert!((captured.get_center(source)[0] - 1.0).abs() < 1e-5);
    assert!((captured.get_center(follower)[0] - 1.0).abs() < 1e-5);
}

#[test]
fn durable_wait_continuation_restores_updates_rng_and_pending_lifecycle_without_replay() {
    fn update(stage: &mut Stage, mob: fmn_mobject::Mob, dt: f64) {
        let x = stage.get_center(mob)[0];
        // Zero-dt is deliberately observable, so reopening the remainder of
        // the wait instead of restoring its cursor cannot pass this oracle.
        stage.set_x(mob, x * 1.01 + 0.125 + dt);
    }
    let config = RuntimeConfig {
        fps: 24,
        ..RuntimeConfig::default()
    };
    let mut source = Scene::new(config.clone(), 19).unwrap();
    let mob = source.add_mobject(point()).unwrap();
    source
        .stage_mut()
        .add_dt_updater(mob, update, false)
        .unwrap();
    source.wait_frames(3, &mut NullSceneSink).unwrap();
    source.rng_mut().next_u64();
    let mut original = source
        .begin_stepped_wait_frames(17, &mut NullSceneSink)
        .unwrap();
    for _ in 0..4 {
        source
            .prepare_stepped_wait_frame(&mut original)
            .unwrap()
            .unwrap();
        source
            .complete_stepped_wait_frame(&mut original, &mut NullSceneSink)
            .unwrap();
    }
    let cursor = source.checkpoint_stepped_wait(&original).unwrap();
    assert_eq!((cursor.base_frame, cursor.stepped), (3, 4));
    let bytes = source.state_bytes().unwrap();
    let mut restored = Scene::new(config, 19).unwrap();
    let preflights = Rc::new(RefCell::new(0));
    let observed = Rc::clone(&preflights);
    restored
        .set_preflight_hook(move |_, _| {
            *observed.borrow_mut() += 1;
            Ok(())
        })
        .unwrap();
    restored.restore_state_bytes(&bytes).unwrap();
    assert!(!restored.can_resume_wait_checkpoint());
    assert!(matches!(
        restored.resume_stepped_wait(cursor.clone()),
        Err(SceneError::UnboundUpdaters)
    ));
    restored
        .rebind_updaters(|_, _, kind| {
            assert_eq!(kind, UpdaterKindTag::Dt);
            Ok(UpdaterFn::Dt(Rc::new(RefCell::new(update))))
        })
        .unwrap();
    assert!(
        !restored.can_resume_wait_checkpoint(),
        "the explicit low-level resume does not admit opaque preflight captures"
    );
    let mut invalid = cursor.clone();
    invalid.stepped += 1;
    assert!(restored.resume_stepped_wait(invalid).is_err());
    assert_eq!(restored.state_bytes().unwrap(), bytes);
    let mut resumed = restored.resume_stepped_wait(cursor).unwrap();
    assert_eq!(
        restored.state_bytes().unwrap(),
        bytes,
        "resume cannot run any updater"
    );
    assert_eq!(restored.play_count(), 1);
    assert_eq!(restored.rng_mut().next_u64(), source.rng_mut().next_u64());
    for frame in 8..=20 {
        let mut expected = PacketSink::default();
        let mut actual = PacketSink::default();
        source
            .prepare_stepped_wait_frame(&mut original)
            .unwrap()
            .unwrap();
        source
            .complete_stepped_wait_frame(&mut original, &mut expected)
            .unwrap();
        restored
            .prepare_stepped_wait_frame(&mut resumed)
            .unwrap()
            .unwrap();
        restored
            .complete_stepped_wait_frame(&mut resumed, &mut actual)
            .unwrap();
        let expected = expected.packet.unwrap();
        let actual = actual.packet.unwrap();
        assert_eq!(actual.frame_index(), frame);
        assert_eq!(actual.segment_frame(), expected.segment_frame());
        assert_eq!(actual.alpha().to_bits(), expected.alpha().to_bits());
        assert_eq!(actual.time(), expected.time());
        assert_eq!(
            actual.rng_fork("checkpoint").next_u64(),
            expected.rng_fork("checkpoint").next_u64()
        );
        assert_eq!(
            actual.state().to_bytes().unwrap(),
            expected.state().to_bytes().unwrap()
        );
        assert_eq!(
            restored.state_bytes().unwrap(),
            source.state_bytes().unwrap()
        );
    }
    assert!(resumed.frames_complete());
    assert_eq!(
        restored.play_count(),
        1,
        "last capture leaves the epilogue pending"
    );
    let mut expected_events = RecordingSink::default();
    let mut actual_events = RecordingSink::default();
    let expected_report = source
        .finish_stepped_wait(original, &mut expected_events)
        .unwrap();
    let actual_report = restored
        .finish_stepped_wait(resumed, &mut actual_events)
        .unwrap();
    assert_eq!((actual_report.base_frame, actual_report.n_frames), (3, 17));
    assert_eq!(actual_report.purity, expected_report.purity);
    assert_eq!(actual_events.events, expected_events.events);
    assert_eq!(restored.play_count(), 2);
    source.wait_frames(0, &mut NullSceneSink).unwrap();
    restored.wait_frames(0, &mut NullSceneSink).unwrap();
    assert_eq!(
        *preflights.borrow(),
        0,
        "the resumed opener already passed preflight"
    );
    assert_eq!(
        restored.state_bytes().unwrap(),
        source.state_bytes().unwrap()
    );
}

#[test]
fn pure_wait_continuation_jumps_match_serial_grids_and_pending_lifecycle() {
    for (seconds, targets) in [
        (None, [7_i64, 8, 16, 17]),
        (Some(43.0 / 24.0), [7, 8, 43, 44]),
    ] {
        let config = RuntimeConfig {
            fps: 24,
            ..RuntimeConfig::default()
        };
        let mut source = Scene::new(config.clone(), 19).unwrap();
        let mob = source.add_mobject(point()).unwrap();
        source.stage_mut().shift(mob, [0.125, -0.5, 0.0]);
        source.wait_frames(3, &mut NullSceneSink).unwrap();
        source.rng_mut().next_u64();
        let mut original = if let Some(duration) = seconds {
            source
                .begin_stepped_wait(Some(duration), &mut NullSceneSink)
                .unwrap()
        } else {
            source
                .begin_stepped_wait_frames(17, &mut NullSceneSink)
                .unwrap()
        };
        for _ in 0..4 {
            source
                .prepare_stepped_wait_frame(&mut original)
                .unwrap()
                .unwrap();
            source
                .complete_stepped_wait_frame(&mut original, &mut NullSceneSink)
                .unwrap();
        }
        let cursor = source.checkpoint_stepped_wait(&original).unwrap();
        let bytes = source.state_bytes().unwrap();
        let mut restored = Scene::new(config, 19).unwrap();
        restored.restore_state_bytes(&bytes).unwrap();
        let mut resumed = restored.resume_stepped_wait(cursor).unwrap();
        assert_eq!(restored.state_bytes().unwrap(), bytes);

        for (index, target) in targets.into_iter().enumerate() {
            let mut expected = PacketSink::default();
            while source.time().frames() < 3 + target {
                source
                    .prepare_stepped_wait_frame(&mut original)
                    .unwrap()
                    .unwrap();
                source
                    .complete_stepped_wait_frame(&mut original, &mut expected)
                    .unwrap();
            }
            let actual = if index == 1 {
                // Ordinary capture must still work between two fast jumps.
                let mut sink = PacketSink::default();
                restored
                    .prepare_stepped_wait_frame(&mut resumed)
                    .unwrap()
                    .unwrap();
                restored
                    .complete_stepped_wait_frame(&mut resumed, &mut sink)
                    .unwrap();
                sink.packet.unwrap().metadata()
            } else {
                let count = if index == 3 {
                    u64::MAX
                } else {
                    (3 + target - restored.time().frames()).cast_unsigned()
                };
                restored
                    .try_advance_pure_wait(&mut resumed, count)
                    .unwrap()
                    .unwrap()
            };
            let expected = expected.packet.unwrap();
            assert_eq!(actual, expected.metadata());
            assert_eq!(actual.alpha().to_bits(), expected.alpha().to_bits());
            assert_eq!(
                actual.rng_fork("continued-wait").next_u64(),
                expected.rng_fork("continued-wait").next_u64(),
            );
            assert_eq!(restored.play_count(), 1);
            assert_eq!(
                restored.state_bytes().unwrap(),
                source.state_bytes().unwrap()
            );
        }
        assert!(resumed.frames_complete());
        assert!(
            restored
                .try_advance_pure_wait(&mut resumed, 1)
                .unwrap()
                .is_none()
        );
        let mut expected_events = RecordingSink::default();
        let mut actual_events = RecordingSink::default();
        let expected_report = source
            .finish_stepped_wait(original, &mut expected_events)
            .unwrap();
        let actual_report = restored
            .finish_stepped_wait(resumed, &mut actual_events)
            .unwrap();
        assert_eq!(actual_report.purity, expected_report.purity);
        assert!(actual_report.begin_state.is_none());
        assert_eq!(actual_events.events, expected_events.events);
        assert_eq!(restored.play_count(), 2);
        assert_eq!(
            restored.state_bytes().unwrap(),
            source.state_bytes().unwrap()
        );
    }
}

#[test]
fn pure_wait_continuation_refuses_late_observers_and_stateful_history() {
    use std::cell::Cell;

    use fmn_anim::{ImpureEffect, Purity};
    use fmn_scene::{
        EventListener, EventPayload, EventPropagation, EventTarget, EventType, Modifiers,
        MouseButton,
    };

    for effect in 0..5 {
        let config = RuntimeConfig {
            fps: 24,
            ..RuntimeConfig::default()
        };
        let mut source = Scene::new(config.clone(), 19).unwrap();
        source.add_mobject(point()).unwrap();
        let mut original = source
            .begin_stepped_wait_frames(12, &mut NullSceneSink)
            .unwrap();
        source
            .prepare_stepped_wait_frame(&mut original)
            .unwrap()
            .unwrap();
        source
            .complete_stepped_wait_frame(&mut original, &mut NullSceneSink)
            .unwrap();
        let mut cursor = source.checkpoint_stepped_wait(&original).unwrap();
        if effect == 4 {
            // No live callback remains, but a historical host-updater phase
            // must never acquire a pure continuation proof.
            cursor.purity = Purity::Stateful(vec![ImpureEffect::SceneUpdater]);
        }
        let bytes = source.state_bytes().unwrap();
        let mut restored = Scene::new(config, 19).unwrap();
        restored.restore_state_bytes(&bytes).unwrap();
        let mut resumed = restored.resume_stepped_wait(cursor.clone()).unwrap();
        let mob = restored.stage().roots()[0];
        let updates = Rc::new(Cell::new(0));
        let events = Rc::new(Cell::new(0));
        let observed_events = Rc::clone(&events);
        restored
            .event_dispatcher_mut()
            .add_listener(EventListener::new(
                EventType::MousePress,
                EventTarget::Global,
                move |_, _, _, _| {
                    observed_events.set(observed_events.get() + 1);
                    EventPropagation::Continue
                },
            ))
            .unwrap();
        let mut view = None;
        let mut inbox = None;
        match effect {
            0 => {
                let observed = Rc::clone(&updates);
                restored
                    .stage_mut()
                    .add_dt_updater(mob, move |_, _, _| observed.set(observed.get() + 1), false)
                    .unwrap();
            }
            1 => {
                view = Some(
                    restored
                        .stage_mut()
                        .get_mut(mob)
                        .unwrap()
                        .buffer
                        .export_view(false),
                );
            }
            2 => inbox = Some(restored.event_inbox()),
            3 => {
                restored
                    .queue_event(EventPayload::MousePress {
                        point: [0.0; 3],
                        button: MouseButton::Left,
                        modifiers: Modifiers::NONE,
                    })
                    .unwrap();
            }
            _ => {}
        }
        let before = restored.state_bytes().unwrap();
        assert!(
            restored
                .try_advance_pure_wait(&mut resumed, 10)
                .unwrap()
                .is_none(),
            "effect {effect}",
        );
        assert_eq!(restored.state_bytes().unwrap(), before, "effect {effect}");
        assert_eq!(restored.checkpoint_stepped_wait(&resumed).unwrap(), cursor);
        assert_eq!(updates.get(), 0);
        assert_eq!(events.get(), 0);
        restored
            .prepare_stepped_wait_frame(&mut resumed)
            .unwrap()
            .unwrap();
        restored
            .complete_stepped_wait_frame(&mut resumed, &mut NullSceneSink)
            .unwrap();
        assert_eq!(restored.time().frames(), 2);
        assert_eq!(updates.get(), usize::from(effect == 0));
        assert_eq!(events.get(), usize::from(effect == 3));
        drop(view);
        drop(inbox);
    }
}

#[test]
fn frame_rng_root_identity_survives_equal_serial_streams() {
    let mut first = Scene::new(RuntimeConfig::default(), 7).unwrap();
    let mut second = Scene::new(RuntimeConfig::default(), 11).unwrap();
    *second.rng_mut() = first.rng_mut().clone();
    assert_eq!(first.state_bytes().unwrap(), second.state_bytes().unwrap());
    assert_eq!(first.rng_seed(), 7);
    assert_eq!(second.rng_seed(), 11);
    let mut first_capture = PacketSink::default();
    let mut second_capture = PacketSink::default();
    first.wait_frames(1, &mut first_capture).unwrap();
    second.wait_frames(1, &mut second_capture).unwrap();
    assert_ne!(
        first_capture.packet.unwrap().rng_fork("root").state(),
        second_capture.packet.unwrap().rng_fork("root").state(),
        "matching serial RNG bytes do not bind frame-indexed randomness"
    );
}

#[test]
fn wait_checkpoint_admission_refuses_unserialized_scene_history() {
    let mut scene = Scene::default();
    assert!(scene.can_resume_wait_checkpoint());
    let mut preflight = Scene::default();
    preflight.set_preflight_hook(|_, _| Ok(())).unwrap();
    assert!(!preflight.can_resume_wait_checkpoint());
    preflight.wait_frames(1, &mut NullSceneSink).unwrap();
    assert!(!preflight.can_resume_wait_checkpoint());
    let preflight_bytes = preflight.state_bytes().unwrap();
    preflight.restore_state_bytes(&preflight_bytes).unwrap();
    assert!(
        !preflight.can_resume_wait_checkpoint(),
        "restoring data cannot erase an opaque preflight declaration"
    );
    let inbox = scene.event_inbox();
    assert!(!scene.can_resume_wait_checkpoint());
    drop(inbox);
    assert!(scene.can_resume_wait_checkpoint());
    scene
        .queue_event(fmn_scene::EventPayload::MouseMotion {
            point: [1.0, 0.0, 0.0],
            delta: [1.0, 0.0, 0.0],
            modifiers: fmn_scene::Modifiers::NONE,
        })
        .unwrap();
    assert!(!scene.can_resume_wait_checkpoint());
    scene.dispatch_pending_events().unwrap();
    assert!(!scene.can_resume_wait_checkpoint());
    scene.take_recorded_events();
    assert!(
        !scene.can_resume_wait_checkpoint(),
        "draining a log cannot erase input history"
    );
    let mut sound = Scene::default();
    sound.add_sound("tone.wav", 0.0, None, None).unwrap();
    assert!(!sound.can_resume_wait_checkpoint());
    for config in [
        RuntimeConfig {
            skip_animations: true,
            ..RuntimeConfig::default()
        },
        RuntimeConfig {
            presenter_mode: true,
            ..RuntimeConfig::default()
        },
        RuntimeConfig {
            start_at_play: Some(0),
            ..RuntimeConfig::default()
        },
        RuntimeConfig {
            end_at_play: Some(1),
            ..RuntimeConfig::default()
        },
    ] {
        assert!(!Scene::new(config, 0).unwrap().can_resume_wait_checkpoint());
    }
    let mut failed = Scene::default();
    let mut sink = RecordingSink {
        fail_capture_at: Some(0),
        ..RecordingSink::default()
    };
    let mut wait = failed.begin_stepped_wait_frames(1, &mut sink).unwrap();
    failed
        .prepare_stepped_wait_frame(&mut wait)
        .unwrap()
        .unwrap();
    // Capture errors are latched so the ordinary epilogue can run. They must
    // still refuse a checkpoint before finish surfaces the original error.
    failed
        .complete_stepped_wait_frame(&mut wait, &mut sink)
        .unwrap();
    assert!(failed.checkpoint_stepped_wait(&wait).is_err());
    assert!(failed.finish_stepped_wait(wait, &mut sink).is_err());
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
fn invalid_scene_clock_state_is_refused_before_mutation() {
    let mut scene = Scene::new(
        RuntimeConfig {
            fps: 4,
            ..RuntimeConfig::default()
        },
        0,
    )
    .expect("scene");
    let mut state = scene.state().expect("state");
    state.frames_elapsed = -1;
    assert!(matches!(
        scene.restore_state(&state),
        Err(SceneError::InvalidState(
            "elapsed frame count must be non-negative"
        ))
    ));
    assert_eq!(scene.time().frames(), 0, "negative-frame refusal is atomic");

    state.frames_elapsed = 0;
    state.fps = 5;
    assert!(matches!(
        scene.restore_state(&state),
        Err(SceneError::InvalidState(
            "state frame grid does not match this scene"
        ))
    ));
    assert_eq!(scene.time().frames(), 0, "grid refusal is atomic");

    state.fps = 4;
    state.frames_elapsed = i64::MAX;
    scene
        .restore_state(&state)
        .expect("the exact frame-counter boundary is representable");
    assert_eq!(scene.time().frames(), i64::MAX);

    let mut exhausted = scene.state().expect("state");
    exhausted.frames_elapsed = 0;
    exhausted.play_count = u64::MAX;
    scene
        .restore_state(&exhausted)
        .expect("representable state");
    assert!(matches!(
        scene.wait(Some(0.25), &mut NullSceneSink),
        Err(SceneError::InvalidState("play count is exhausted"))
    ));
    assert_eq!(scene.time().frames(), 0, "refusal is atomic");

    let mut other = Scene::new(
        RuntimeConfig {
            fps: 4,
            ..RuntimeConfig::default()
        },
        0,
    )
    .expect("other scene");
    let foreign = other.state().expect("state");
    assert!(matches!(
        scene.restore_state(&foreign),
        Err(SceneError::InvalidState(
            "an in-memory SceneState belongs to a different scene"
        ))
    ));
}

#[test]
fn durable_scene_state_restores_adjacent_large_frames_exactly() {
    let mut scene = Scene::new(
        RuntimeConfig {
            fps: 1,
            ..RuntimeConfig::default()
        },
        0,
    )
    .expect("scene");
    let expected_frame = (1_i64 << 53) + 1;
    let mut state = scene.state().expect("state");
    state.frames_elapsed = expected_frame;
    scene.restore_state(&state).expect("in-memory restore");
    assert_eq!(scene.time().frames(), expected_frame);

    let bytes = scene.state_bytes().expect("durable state");
    let mut restored = Scene::new(
        RuntimeConfig {
            fps: 1,
            ..RuntimeConfig::default()
        },
        1,
    )
    .expect("fresh scene");
    restored
        .restore_state_bytes(&bytes)
        .expect("durable restore");
    assert_eq!(restored.time().frames(), expected_frame);
    assert_eq!(restored.state_bytes().expect("re-encode"), bytes);
}

#[test]
fn invalid_nested_play_setup_is_refused_before_lifecycle_or_stage_mutation() {
    let mut scene = Scene::default();
    let first = scene.add_mobject(point()).expect("first root");
    let later = scene.add_mobject(point()).expect("later root");
    let later_target = scene.stage_mut().add(point());
    scene.stage_mut().shift(later_target, [3.0, 0.0, 0.0]);
    let updater_calls = Rc::new(RefCell::new(0usize));
    let seen_updater_calls = Rc::clone(&updater_calls);
    let updater_id = scene
        .stage_mut()
        .add_updater(
            first,
            move |_stage, _me| *seen_updater_calls.borrow_mut() += 1,
            false,
        )
        .expect("updater");

    let builder = first
        .animate()
        .set_anim_args(AnimateArgs {
            run_time: Some(0.25),
            rate_func: Some(fmn_core::rate::linear),
            ..AnimateArgs::default()
        })
        .and_then(|builder| builder.shift([1.0, 0.0, 0.0]))
        .expect("records");
    let mut first_animation =
        prepare_animation(builder, scene.stage_mut()).expect("first animation");
    first_animation.state_mut().config.suspend_mobject_updating = true;
    let later_animation = Box::new(replacement_transform(later, later_target));
    let succession = Succession::new(scene.stage_mut(), vec![first_animation, later_animation])
        .expect("succession");
    let group = succession.group();
    scene
        .stage_mut()
        .delete(later_target)
        .expect("stale later target");

    let before = scene.stage().snapshot_bytes().expect("begin state encodes");
    let before_roots = scene.mobjects().to_vec();
    let preflight_calls = Rc::new(RefCell::new(0usize));
    let seen_preflight_calls = Rc::clone(&preflight_calls);
    scene
        .set_preflight_hook(move |_stage, _roots| {
            *seen_preflight_calls.borrow_mut() += 1;
            Ok(())
        })
        .expect("preflight hook");
    let mut sink = RecordingSink::default();

    let error = scene
        .play(
            vec![Box::new(succession)],
            PlayOverrides::default(),
            &mut sink,
        )
        .expect_err("a later succession target is stale");

    assert!(matches!(
        error,
        SceneError::Animation(AnimError::StaleHandle(handle)) if handle == later_target
    ));
    assert_eq!(*preflight_calls.borrow(), 0);
    assert_eq!(*updater_calls.borrow(), 0);
    assert!(sink.events.is_empty());
    assert!(sink.captures.is_empty());
    assert_eq!(scene.time().frames(), 0);
    assert_eq!(scene.play_count(), 0);
    assert_eq!(scene.mobjects(), before_roots);
    assert_eq!(scene.stage().updater_ids(first), vec![updater_id]);
    assert!(!scene.stage().is_updating_suspended(first));
    assert!(!scene.stage().is_animating(first));
    assert!(!scene.stage().is_animating(later));
    assert!(!scene.stage().is_animating(group));
    assert_eq!(
        scene
            .stage()
            .snapshot_bytes()
            .expect("refused state encodes"),
        before,
        "the complete arena is unchanged"
    );
}

#[test]
fn unrepresentable_play_is_refused_before_lifecycle_side_effects() {
    let mut scene = Scene::default();
    let mob = scene.add_mobject(point()).expect("root");
    let builder = mob.animate().shift([1.0, 0.0, 0.0]).expect("records");
    let animation = prepare_animation(builder, scene.stage_mut()).expect("animation");
    let before = scene.stage().snapshot_bytes().expect("begin state encodes");
    let preflight_calls = Rc::new(RefCell::new(0usize));
    let seen_preflight_calls = Rc::clone(&preflight_calls);
    scene
        .set_preflight_hook(move |_stage, _roots| {
            *seen_preflight_calls.borrow_mut() += 1;
            Ok(())
        })
        .expect("preflight hook");
    let mut sink = RecordingSink::default();

    let error = scene
        .play(
            vec![animation],
            PlayOverrides {
                run_time: Some(1e18),
                ..PlayOverrides::default()
            },
            &mut sink,
        )
        .expect_err("run time cannot fit the rational frame counter");

    assert!(matches!(
        error,
        SceneError::Animation(AnimError::Clock(ClockError::RunTimeTooLong))
    ));
    assert_eq!(*preflight_calls.borrow(), 0);
    assert!(sink.events.is_empty());
    assert!(sink.captures.is_empty());
    assert_eq!(scene.time().frames(), 0);
    assert_eq!(scene.play_count(), 0);
    assert!(!scene.stage().is_animating(mob));
    assert_eq!(
        scene
            .stage()
            .snapshot_bytes()
            .expect("refused state encodes"),
        before
    );
}

#[test]
fn unrepresentable_wait_is_refused_before_lifecycle_side_effects() {
    let mut scene = Scene::default();
    let mob = scene.add_mobject(point()).expect("root");
    let updater_calls = Rc::new(RefCell::new(0usize));
    let seen_updater_calls = Rc::clone(&updater_calls);
    scene
        .stage_mut()
        .add_updater(
            mob,
            move |stage, me| {
                *seen_updater_calls.borrow_mut() += 1;
                stage.shift(me, [1.0, 0.0, 0.0]);
            },
            false,
        )
        .expect("updater");
    let preflight_calls = Rc::new(RefCell::new(0usize));
    let seen_preflight_calls = Rc::clone(&preflight_calls);
    scene
        .set_preflight_hook(move |_stage, _roots| {
            *seen_preflight_calls.borrow_mut() += 1;
            Ok(())
        })
        .expect("preflight hook");
    let mut sink = RecordingSink::default();

    let error = scene
        .wait(Some(1e18), &mut sink)
        .expect_err("duration cannot fit the rational frame counter");

    assert!(matches!(
        error,
        SceneError::Animation(AnimError::Clock(ClockError::RunTimeTooLong))
    ));
    assert_eq!(*preflight_calls.borrow(), 0);
    assert_eq!(*updater_calls.borrow(), 0);
    assert!(sink.events.is_empty());
    assert!(sink.captures.is_empty());
    assert_eq!(scene.time().frames(), 0);
    assert_eq!(scene.play_count(), 0);
    assert_eq!(scene.stage().get_center(mob), [0.0, 0.0, 0.0]);
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
    let actual = scene.orientation().expect("camera orientation");
    assert!((actual.theta - expected.theta).abs() < 1e-15);
    assert!((actual.phi - expected.phi).abs() < 1e-15);
    let mut camera_config = scene
        .three_d_camera_config()
        .expect("scene camera defaults");
    camera_config.fps = 4;
    let camera = scene
        .three_d_camera(camera_config)
        .expect("tracker-backed camera");
    assert_eq!(camera.samples(), 4);
    let camera_angles = camera.frame().euler_angles();
    assert!((camera_angles[0] - expected.theta).abs() < 1e-12);
    assert!((camera_angles[1] - expected.phi).abs() < 1e-12);
    assert!((camera_angles[2] - expected.gamma).abs() < 1e-12);
    let explicit_zero = scene
        .three_d_camera(fmn_render::CameraConfig::default())
        .expect("explicit zero-sample camera");
    assert_eq!(explicit_zero.samples(), 0);
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
    assert!(
        (scene.orientation().expect("rotated orientation").theta - (expected.theta + 0.5)).abs()
            < 1e-12
    );

    let state = scene.state().expect("state");
    scene
        .set_orientation(CameraOrientation::from_degrees(0.0, 0.0, 0.0))
        .expect("changes");
    scene
        .restore_state(&state)
        .expect("restores camera trackers");
    assert!(
        (scene.orientation().expect("restored orientation").theta - (expected.theta + 0.5)).abs()
            < 1e-12
    );
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
fn three_d_camera_surfaces_refuse_every_broken_tracker_before_mutation() {
    for broken_index in 0..3 {
        let mut scene = ThreeDScene::default();
        let family = scene.stage().family(scene.camera_root());
        let trackers = [
            *family.get(1).expect("theta tracker"),
            *family.get(2).expect("phi tracker"),
            *family.get(3).expect("gamma tracker"),
        ];
        let before = trackers.map(|tracker| scene.stage().tracker_value(tracker));
        let broken_tracker = *trackers
            .get(broken_index)
            .expect("broken tracker index is in range");
        scene
            .stage_mut()
            .delete(broken_tracker)
            .expect("delete camera tracker");

        assert!(matches!(
            scene.orientation(),
            Err(SceneError::Stage(StageError::StaleHandle))
        ));
        assert!(matches!(
            scene.camera_frame(),
            Err(SceneError::Stage(StageError::StaleHandle))
        ));
        assert!(matches!(
            scene.three_d_camera_config(),
            Err(SceneError::Stage(StageError::StaleHandle))
        ));
        assert!(matches!(
            scene.three_d_camera(fmn_render::CameraConfig::default()),
            Err(SceneError::Stage(StageError::StaleHandle))
        ));
        assert!(matches!(
            scene.set_orientation(CameraOrientation::from_degrees(10.0, 20.0, 30.0)),
            Err(SceneError::Stage(StageError::StaleHandle))
        ));

        for (index, (tracker, before_value)) in trackers.into_iter().zip(before).enumerate() {
            if index != broken_index {
                assert_eq!(
                    scene.stage().tracker_value(tracker),
                    before_value,
                    "a surviving camera tracker must not mutate before refusal"
                );
            }
        }
    }
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

#[derive(Default)]
struct BeginFailureSink {
    attempted: Vec<LifecyclePhase>,
    failed_begin: bool,
}

impl SceneSink for BeginFailureSink {
    fn event(&mut self, event: LifecycleEvent) -> Result<(), IntegrationError> {
        self.attempted.push(event.phase);
        if event.phase == LifecyclePhase::SceneBegin && !self.failed_begin {
            self.failed_begin = true;
            return Err(IntegrationError::new("sink", "fixture begin failure"));
        }
        Ok(())
    }

    fn capture(
        &mut self,
        _reason: CaptureReason,
        _packet: fmn_anim::FramePacket,
    ) -> Result<(), IntegrationError> {
        Ok(())
    }
}

#[derive(Default)]
struct TracksTearDown {
    tear_down_calls: usize,
}

impl SceneProgram for TracksTearDown {
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
        self.tear_down_calls += 1;
        Ok(())
    }
}

#[test]
fn scene_begin_failure_still_runs_the_common_cleanup_path() {
    let mut scene = Scene::default();
    let mut program = TracksTearDown::default();
    let mut sink = BeginFailureSink::default();

    let error = scene
        .run(&mut program, &mut sink)
        .expect_err("the original SceneBegin failure must surface");
    let SceneError::Integration(error) = error else {
        std::panic::panic_any(format!(
            "expected the begin integration error, found {error}"
        ));
    };
    assert_eq!(error.point(), "sink");
    assert_eq!(error.message(), "fixture begin failure");
    assert_eq!(program.tear_down_calls, 1);
    assert_eq!(
        sink.attempted,
        vec![
            LifecyclePhase::SceneBegin,
            LifecyclePhase::TearDown,
            LifecyclePhase::SceneEnd,
        ]
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

// W5 wasm tier 1 (fm-l97): the host-side proxy for the wasm32 determinism
// contract. The browser build runs single-threaded —
// `HardwareTopology::current` reports one logical CPU and
// `fmn_render::effective_threads` collapses any fan-out request to the
// serial band loop — so the wasm32-equivalent configuration is `threads = 1`.
// This proxy runs the full Proscenium lifecycle (play + wait, packets
// materialized at the Lumen boundary exactly as the conformance corpus does)
// twice and demands byte-identical canonical frames. The in-VM half of the
// proof — the same property observed executing inside a real wasm32 VM —
// lives in `wasm-smoke/` and runs under node; this test pins the property to
// the host gates so it cannot rot when no wasm runner is wired into CI.
mod wasm_tier1_determinism_proxy {
    use fmn_core::color::Srgb;
    use fmn_render::bin::{Binning, ScreenMap, Tiling, Viewport};
    use fmn_render::engine::{FrameConfig, FrameJob, encode_frame};
    use fmn_render::fill::MonoTable;
    use fmn_render::plan::RenderPlan;

    use super::*;

    const WIDTH: u32 = 96;
    const HEIGHT: u32 = 54;
    const TILING: Tiling = Tiling {
        macro_tile: 64,
        fine_tile: 8,
    };

    fn frame_config() -> FrameConfig {
        FrameConfig::new(
            Viewport {
                width: WIDTH,
                height: HEIGHT,
            },
            ScreenMap {
                scale: 20.0,
                origin: [f64::from(WIDTH) / 2.0, f64::from(HEIGHT) / 2.0],
                y_up: true,
            },
            Srgb::from_rgb8(0x22, 0x22, 0x22).to_linear(1.0),
        )
    }

    /// A filled square — the smallest primitive scene with real coverage.
    /// The point run is the quad-path anchor/handle interleave a library
    /// builder would emit (`set_points_as_corners` over the closed corner
    /// ring: anchors with midpoint handles, first corner repeated to close),
    /// written over the VMobject record schema with an opaque fill. A bare
    /// `Mobject::from_points` carries neither the path structure nor a style
    /// and would rasterize to background, making the byte-equality assertion
    /// below vacuous.
    fn square() -> Mobject {
        let points: [[f32; 3]; 9] = [
            [-1.0, -1.0, 0.0],
            [0.0, -1.0, 0.0],
            [1.0, -1.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
            [-1.0, 1.0, 0.0],
            [-1.0, 0.0, 0.0],
            [-1.0, -1.0, 0.0],
        ];
        let mut buffer =
            fmn_mobject::RecordBuffer::new(fmn_mobject::RecordSchema::vmobject(), points.len())
                .unwrap();
        for (i, point) in points.iter().enumerate() {
            buffer.write(i, "point", point);
            buffer.write(i, "fill_rgba", &[1.0, 0.0, 0.0, 1.0]);
            buffer.write(i, "stroke_rgba", &[1.0, 1.0, 1.0, 1.0]);
            buffer.write(i, "stroke_width", &[4.0]);
        }
        Mobject::from_buffer(buffer)
    }

    /// Materialize one packet and rasterize it single-threaded — the wasm32
    /// configuration — returning the canonical encoded frame bytes.
    fn render_single_thread(packet: &fmn_anim::FramePacket) -> Vec<u8> {
        let stage = packet.materialize_stage();
        let config = frame_config();
        let mut plan = RenderPlan::new();
        let camera_revision = u64::try_from(packet.frame_index()).expect("frame index");
        plan.sync(&stage, camera_revision)
            .expect("valid scene runtime fixture");
        let mono = MonoTable::build(&plan, config.map).expect("bounded scene test monotone table");
        let mut binning = Binning::build(&plan, config.viewport, TILING, config.map)
            .expect("bounded scene test binning");
        binning.prune_occluded(&plan).expect("prune");
        let job = FrameJob::new(&plan, &mono, &binning, config).expect("job");
        let frame = job.render(1).expect("render");
        encode_frame(&frame).expect("encode")
    }

    struct FrameBytes {
        frames: Vec<Vec<u8>>,
    }

    impl SceneSink for FrameBytes {
        fn capture(
            &mut self,
            _reason: CaptureReason,
            packet: fmn_anim::FramePacket,
        ) -> Result<(), IntegrationError> {
            self.frames.push(render_single_thread(&packet));
            Ok(())
        }
    }

    /// Run one small scene (a shifting square, then a wait hold) and collect
    /// every captured frame's canonical bytes.
    fn run_scene_frames() -> Vec<Vec<u8>> {
        let mut scene = Scene::new(
            RuntimeConfig {
                fps: 8,
                ..RuntimeConfig::default()
            },
            5,
        )
        .expect("scene");
        let mob = scene.add_mobject(square()).expect("root");
        let builder = mob
            .animate()
            .set_anim_args(AnimateArgs {
                run_time: Some(0.5),
                rate_func: Some(fmn_core::rate::linear),
                ..AnimateArgs::default()
            })
            .and_then(|builder| builder.shift([2.0, 1.0, 0.0]))
            .expect("records");
        let animation = prepare_animation(builder, scene.stage_mut()).expect("prepares");
        let mut sink = FrameBytes { frames: Vec::new() };
        scene
            .play(vec![animation], PlayOverrides::default(), &mut sink)
            .expect("plays");
        sink.frames
    }

    #[test]
    fn single_thread_render_is_byte_identical_across_runs() {
        let first = run_scene_frames();
        let second = run_scene_frames();
        assert!(
            !first.is_empty(),
            "the play segment captured at least one frame"
        );
        assert_eq!(
            first.len(),
            second.len(),
            "frame counts diverged between identical scene runs"
        );
        for (index, (a, b)) in first.iter().zip(&second).enumerate() {
            assert_eq!(
                a, b,
                "frame {index} differs between identical single-threaded runs"
            );
        }

        // The proxy must prove the scene actually drew something, or the
        // equality above would hold vacuously over background-only frames.
        let background_only = {
            let stage = Stage::default();
            let config = frame_config();
            let mut plan = RenderPlan::new();
            plan.sync(&stage, 0).expect("valid empty scene fixture");
            let mono = MonoTable::build(&plan, config.map).expect("bounded empty monotone table");
            let mut binning = Binning::build(&plan, config.viewport, TILING, config.map)
                .expect("bounded scene test binning");
            binning.prune_occluded(&plan).expect("prune");
            let job = FrameJob::new(&plan, &mono, &binning, config).expect("job");
            let frame = job.render(1).expect("render");
            encode_frame(&frame).expect("encode")
        };
        assert_ne!(
            first[0], background_only,
            "the square scene rendered background-only bytes — the proxy \
             would be vacuous"
        );
    }
}
