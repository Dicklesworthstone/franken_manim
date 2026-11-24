//! Construct-only consumers preserve sampling and lifecycle without freezing
//! an arena for every discarded frame. Rendering stays the safe default.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use fmn_anim::{AnimConfig, FrameMetadata, FramePacket, RateFunc, RationalTime, Transform};
use fmn_mobject::{
    FrozenRenderState, ImageColorSpace, ImageResource, ImageSampler, Mob, Mobject, Stage,
};
use fmn_scene::{
    CaptureReason, EventListener, EventPayload, EventPropagation, EventTarget, EventType,
    HoldDecision, HoldKind, IntegrationError, InteractiveScene, LifecycleEvent, LifecyclePhase,
    Modifiers, MouseButton, NullSceneSink, PlayOverrides, RuntimeConfig, Scene, SceneSink,
};

#[derive(Default)]
struct Sink {
    metadata_only: bool,
    final_state: bool,
    samples: Vec<(CaptureReason, FrameMetadata)>,
    packets: Vec<FramePacket>,
    events: Vec<LifecycleEvent>,
    fail_at: Option<usize>,
}

impl Sink {
    fn observe(
        &mut self,
        reason: CaptureReason,
        metadata: FrameMetadata,
    ) -> Result<(), IntegrationError> {
        self.samples.push((reason, metadata));
        if self.fail_at == Some(self.samples.len()) {
            Err(IntegrationError::new("capture-fixture", "capture failed"))
        } else {
            Ok(())
        }
    }
}

impl SceneSink for Sink {
    fn event(&mut self, event: LifecycleEvent) -> Result<(), IntegrationError> {
        self.events.push(event);
        Ok(())
    }

    fn capture_state_required(&self) -> bool {
        !self.metadata_only
    }

    fn capture_final_state_required(&self) -> bool {
        self.final_state
    }

    fn capture(
        &mut self,
        reason: CaptureReason,
        packet: FramePacket,
    ) -> Result<(), IntegrationError> {
        assert!(
            !self.metadata_only || self.final_state,
            "a discarded frame must not be frozen"
        );
        let metadata = packet.metadata();
        self.packets.push(packet);
        self.observe(reason, metadata)
    }

    fn capture_metadata(
        &mut self,
        reason: CaptureReason,
        metadata: FrameMetadata,
    ) -> Result<(), IntegrationError> {
        assert!(self.metadata_only);
        self.observe(reason, metadata)
    }
}

#[derive(Default)]
struct PictureSink {
    observations: Sink,
    pictures: Vec<FrozenRenderState>,
}

impl SceneSink for PictureSink {
    fn event(&mut self, event: LifecycleEvent) -> Result<(), IntegrationError> {
        self.observations.event(event)
    }

    fn capture_state_required(&self) -> bool {
        self.observations.capture_state_required()
    }

    fn capture_final_state_required(&self) -> bool {
        self.observations.capture_final_state_required()
    }

    fn capture(&mut self, _: CaptureReason, _: FramePacket) -> Result<(), IntegrationError> {
        panic!("picture sinks must observe the owner before a full snapshot is made")
    }

    fn capture_stage(
        &mut self,
        reason: CaptureReason,
        stage: &Stage,
        metadata: FrameMetadata,
    ) -> Result<(), IntegrationError> {
        assert_eq!(stage.time().to_bits(), metadata.time().to_f64().to_bits());
        self.pictures.push(stage.freeze_visible_render_state());
        self.observations.observe(reason, metadata)
    }

    fn capture_metadata(
        &mut self,
        reason: CaptureReason,
        metadata: FrameMetadata,
    ) -> Result<(), IntegrationError> {
        self.observations.capture_metadata(reason, metadata)
    }
}

fn scene() -> Scene {
    Scene::new(
        RuntimeConfig {
            fps: 8,
            ..RuntimeConfig::default()
        },
        19,
    )
    .unwrap()
}

fn transform(scene: &mut Scene, mob: Mob) -> Transform {
    let stage = scene.stage_mut();
    let target = stage.copy_family(mob).unwrap();
    stage.shift(target, [3.0, 0.0, 0.0]);
    Transform::new(mob, target).with_config(AnimConfig {
        run_time: 0.375,
        rate_func: RateFunc::linear(),
        ..AnimConfig::default()
    })
}

#[test]
fn picture_capture_preserves_native_and_stepped_samples_and_lifecycle() {
    for stepped in [false, true] {
        for final_only in [false, true] {
            let mut outcomes = Vec::new();
            for direct in [false, true] {
                let mut scene = scene();
                let mob = scene
                    .add_mobject(Mobject::from_points(&[[0.0; 3]]))
                    .unwrap();
                let updated = scene
                    .add_mobject(Mobject::from_points(&[[0.0; 3]]))
                    .unwrap();
                let trace = Rc::new(RefCell::new(Vec::new()));
                let seen = Rc::clone(&trace);
                scene
                    .stage_mut()
                    .add_dt_updater(
                        updated,
                        move |stage, mob, dt| {
                            seen.borrow_mut()
                                .push((dt.to_bits(), stage.time().to_bits()));
                            stage.shift(mob, [0.0, dt, 0.0]);
                        },
                        false,
                    )
                    .unwrap();
                let mut packets = Sink {
                    metadata_only: final_only,
                    final_state: final_only,
                    ..Sink::default()
                };
                let mut pictures = PictureSink {
                    observations: Sink {
                        metadata_only: final_only,
                        final_state: final_only,
                        ..Sink::default()
                    },
                    ..PictureSink::default()
                };
                let sink: &mut dyn SceneSink = if direct { &mut pictures } else { &mut packets };
                let animation = transform(&mut scene, mob);
                if stepped {
                    let mut play = scene
                        .begin_stepped_play(
                            vec![Box::new(animation)],
                            PlayOverrides::default(),
                            sink,
                        )
                        .unwrap()
                        .unwrap();
                    while scene
                        .prepare_stepped_play_frame(&mut play)
                        .unwrap()
                        .is_some()
                    {
                        scene.complete_stepped_play_frame(&mut play, sink).unwrap();
                    }
                    scene.finish_stepped_play(play, sink).unwrap();
                    let mut wait = scene.begin_stepped_wait_frames(2, sink).unwrap();
                    while scene
                        .prepare_stepped_wait_frame(&mut wait)
                        .unwrap()
                        .is_some()
                    {
                        scene.complete_stepped_wait_frame(&mut wait, sink).unwrap();
                    }
                    scene.finish_stepped_wait(wait, sink).unwrap();
                } else {
                    scene
                        .play(vec![Box::new(animation)], PlayOverrides::default(), sink)
                        .unwrap();
                    scene.wait_frames(2, sink).unwrap();
                }
                scene
                    .wait_until_frames(8, &mut |stage| stage.time() >= 0.875, sink)
                    .unwrap();
                scene.show(sink).unwrap();
                let idle = scene.prepare_idle_frame(sink).unwrap();
                scene.complete_idle_frame(idle, sink).unwrap();

                let summarize = |stage: Stage| {
                    (
                        stage.time().to_bits(),
                        stage.get_center(mob).map(f64::to_bits),
                        stage.get_center(updated).map(f64::to_bits),
                    )
                };
                let (samples, events, frames) = if direct {
                    (
                        pictures.observations.samples,
                        pictures.observations.events,
                        pictures
                            .pictures
                            .into_iter()
                            .map(|state| summarize(state.materialize()))
                            .collect::<Vec<_>>(),
                    )
                } else {
                    (
                        packets.samples,
                        packets.events,
                        packets
                            .packets
                            .into_iter()
                            .map(|packet| summarize(packet.materialize_stage()))
                            .collect::<Vec<_>>(),
                    )
                };
                assert_eq!(samples.len(), 9);
                assert_eq!(frames.len(), if final_only { 6 } else { 9 });
                outcomes.push((
                    samples,
                    events,
                    frames,
                    trace.borrow().clone(),
                    scene.time(),
                ));
            }
            assert_eq!(
                outcomes[0], outcomes[1],
                "stepped={stepped}, final_only={final_only}"
            );
        }
    }
}

#[test]
fn picture_capture_never_snapshots_hidden_history_and_isolates_writable_views() {
    let mut scene = scene();
    let visible = scene
        .add_mobject(Mobject::from_points(&[[0.0; 3]]))
        .unwrap();
    let hidden = scene.stage_mut().add(Mobject::from_points(&[[10.0; 3]]));
    let image = ImageResource::rgba8(
        1,
        1,
        vec![1, 2, 3, 255],
        ImageColorSpace::Srgb,
        ImageSampler::default(),
    )
    .unwrap();
    let hidden_pixels = image.shared_pixels();
    scene
        .stage_mut()
        .set_image_resource(hidden, Some(image))
        .unwrap();
    let view = scene
        .stage_mut()
        .get_mut(visible)
        .unwrap()
        .buffer
        .export_view(true);
    let mut sink = PictureSink::default();
    scene
        .wait_until_frames(3, &mut |_| false, &mut sink)
        .unwrap();
    scene.show(&mut sink).unwrap();

    // A full snapshot would clone the detached image into the arena's snapshot
    // cache even if that packet were immediately projected and discarded.
    assert_eq!(Arc::strong_count(&hidden_pixels), 2);
    assert_eq!(sink.pictures.len(), 4);
    assert!(view.write(0, "point", &[7.0, 8.0, 9.0]));
    for picture in sink.pictures {
        assert_eq!(picture.retained_object_count(), 1);
        let stage = picture.materialize();
        assert!(!stage.contains(hidden));
        assert_eq!(stage.get_center(visible), [0.0; 3]);
    }
    assert_eq!(scene.stage().get_center(visible), [7.0, 8.0, 9.0]);
    assert!(scene.stage().contains(hidden));

    // Replay consumers still receive complete, immutable state by default.
    let mut replay = Sink::default();
    scene.show(&mut replay).unwrap();
    assert!(Arc::strong_count(&hidden_pixels) > 2);
    assert!(view.write(0, "point", &[20.0; 3]));
    let snapshot = replay.packets[0].materialize_stage();
    assert!(snapshot.contains(hidden));
    assert_eq!(snapshot.get_center(visible), [7.0, 8.0, 9.0]);
}

#[test]
fn picture_capture_failure_preserves_segment_completion_and_first_error() {
    let mut scene = scene();
    let mob = scene
        .add_mobject(Mobject::from_points(&[[0.0; 3]]))
        .unwrap();
    let animation = transform(&mut scene, mob);
    let mut sink = PictureSink {
        observations: Sink {
            fail_at: Some(2),
            ..Sink::default()
        },
        ..PictureSink::default()
    };
    let error = scene
        .play(
            vec![Box::new(animation)],
            PlayOverrides::default(),
            &mut sink,
        )
        .unwrap_err();
    assert!(error.to_string().contains("capture failed"));
    assert_eq!(sink.pictures.len(), 2);
    assert_eq!(sink.observations.samples.len(), 2);
    assert_eq!(scene.time().frames(), 3);
    assert_eq!(scene.play_count(), 1);
    assert_eq!(scene.stage().get_center(mob), [3.0, 0.0, 0.0]);
    assert!(!scene.stage().is_animating(mob));
    assert!(
        !sink
            .observations
            .events
            .iter()
            .any(|event| event.phase == LifecyclePhase::PostPlay)
    );
}

#[test]
fn metadata_only_play_and_wait_preserve_updates_samples_and_lifecycle() {
    let mut results = Vec::new();
    for metadata_only in [false, true] {
        let mut scene = scene();
        let animated = scene
            .add_mobject(Mobject::from_points(&[[0.0; 3]]))
            .unwrap();
        let updated = scene
            .add_mobject(Mobject::from_points(&[[0.0; 3]]))
            .unwrap();
        let trace = Rc::new(RefCell::new(Vec::new()));
        let seen = Rc::clone(&trace);
        scene
            .stage_mut()
            .add_dt_updater(
                updated,
                move |stage, mob, dt| {
                    let position = stage.get_center(mob);
                    seen.borrow_mut()
                        .push((dt.to_bits(), stage.time().to_bits()));
                    stage.shift(mob, [0.0, dt * (1.0 + position[1]), 0.0]);
                },
                false,
            )
            .unwrap();
        let animation = transform(&mut scene, animated);
        let mut sink = Sink {
            metadata_only,
            ..Sink::default()
        };
        let report = scene
            .play(
                vec![Box::new(animation)],
                PlayOverrides::default(),
                &mut sink,
            )
            .unwrap()
            .unwrap();
        assert_eq!(report.n_frames, 3);
        scene.wait(Some(0.25), &mut sink).unwrap();
        let stopped = scene
            .wait_until(1.0, &mut |stage| stage.time() >= 0.875, &mut sink)
            .unwrap();
        assert_eq!(stopped.n_frames, 2);
        scene.show(&mut sink).unwrap();
        assert_eq!(sink.samples.len(), 8);
        assert_eq!(sink.packets.len(), if metadata_only { 0 } else { 8 });
        assert_eq!(scene.play_count(), 3);
        results.push((
            sink.samples,
            sink.events,
            trace.borrow().clone(),
            scene.stage().get_center(animated).map(f64::to_bits),
            scene.stage().get_center(updated).map(f64::to_bits),
            scene.time(),
        ));
    }
    assert_eq!(results[0], results[1]);
}

#[test]
fn segments_without_a_pure_program_consumer_keep_no_begin_snapshot() {
    // A begin snapshot copies the whole arena, and only a pure frame program
    // reads it. These sinks accept no pure program and the stepped host path
    // never offers one, so pure segments carry no snapshot, whether or not
    // their frames are frozen. Each segment used to copy the arena once.
    for metadata_only in [false, true] {
        let mut scene = scene();
        let mob = scene
            .add_mobject(Mobject::from_points(&[[0.0; 3]]))
            .unwrap();
        let animation = transform(&mut scene, mob);
        let mut sink = Sink {
            metadata_only,
            ..Sink::default()
        };
        let play = scene
            .play(
                vec![Box::new(animation)],
                PlayOverrides::default(),
                &mut sink,
            )
            .unwrap()
            .unwrap();
        let wait = scene.wait(Some(0.25), &mut sink).unwrap();
        let mut stepped = scene.begin_stepped_wait(Some(0.25), &mut sink).unwrap();
        while scene
            .prepare_stepped_wait_frame(&mut stepped)
            .unwrap()
            .is_some()
        {
            scene
                .complete_stepped_wait_frame(&mut stepped, &mut sink)
                .unwrap();
        }
        let stepped = scene.finish_stepped_wait(stepped, &mut sink).unwrap();
        for report in [&play, &wait, &stepped] {
            assert!(report.purity.is_pure(), "{report:?}");
            assert!(report.begin_state.is_none(), "{report:?}");
        }
        assert_eq!(sink.samples.len(), 7);
    }
}

#[test]
fn stepped_host_path_observes_the_same_metadata_as_native_playback() {
    let mut results = Vec::new();
    for metadata_only in [false, true] {
        let mut scene = scene();
        let mob = scene
            .add_mobject(Mobject::from_points(&[[0.0; 3]]))
            .unwrap();
        let animation = transform(&mut scene, mob);
        let mut sink = Sink {
            metadata_only,
            ..Sink::default()
        };
        let mut play = scene
            .begin_stepped_play(
                vec![Box::new(animation)],
                PlayOverrides::default(),
                &mut sink,
            )
            .unwrap()
            .unwrap();
        while scene
            .prepare_stepped_play_frame(&mut play)
            .unwrap()
            .is_some()
        {
            scene
                .complete_stepped_play_frame(&mut play, &mut sink)
                .unwrap();
        }
        scene.finish_stepped_play(play, &mut sink).unwrap();
        let mut wait = scene.begin_stepped_wait(Some(0.25), &mut sink).unwrap();
        while scene
            .prepare_stepped_wait_frame(&mut wait)
            .unwrap()
            .is_some()
        {
            scene
                .complete_stepped_wait_frame(&mut wait, &mut sink)
                .unwrap();
        }
        scene.finish_stepped_wait(wait, &mut sink).unwrap();
        assert_eq!(sink.samples.len(), 5);
        assert_eq!(sink.packets.len(), if metadata_only { 0 } else { 5 });
        results.push((
            sink.samples,
            sink.events,
            scene.time(),
            scene.play_count(),
            scene.stage().get_center(mob).map(f64::to_bits),
        ));
    }
    assert_eq!(results[0], results[1]);
}

#[test]
fn seekable_owners_match_serial_state_and_resume_with_dormant_editor_listeners() {
    let mut outcomes = Vec::new();
    for jumping in [false, true] {
        let mut interactive = InteractiveScene::new(scene()).unwrap();
        let scene = interactive.scene_mut();
        assert_eq!(scene.event_dispatcher().listener_count(), 7);
        assert!(!scene.allows_pure_segments());
        let mob = scene
            .add_mobject(Mobject::from_points(&[[0.0; 3]]))
            .unwrap();
        let animation = transform(scene, mob);
        let mut sink = Sink {
            metadata_only: true,
            ..Sink::default()
        };
        let mut play = scene
            .begin_seekable_play(
                vec![Box::new(animation)],
                PlayOverrides {
                    run_time_frames: Some(9),
                    ..PlayOverrides::default()
                },
                &mut sink,
            )
            .unwrap()
            .unwrap();
        let mut observed = Vec::new();
        for (index, frame) in [3, 4, 9].into_iter().enumerate() {
            let metadata = if jumping && index != 1 {
                let count = (frame - scene.time().frames()).cast_unsigned();
                scene
                    .try_advance_pure_play(&mut play, count)
                    .unwrap()
                    .unwrap()
            } else {
                while scene.time().frames() < frame {
                    scene
                        .prepare_stepped_play_frame(&mut play)
                        .unwrap()
                        .unwrap();
                    scene
                        .complete_stepped_play_frame(&mut play, &mut sink)
                        .unwrap();
                }
                sink.samples.last().unwrap().1
            };
            assert_eq!(scene.play_count(), 0);
            assert!(scene.stage().is_animating(mob));
            observed.push((scene.state_bytes().unwrap(), metadata));
        }
        assert!(play.frames_complete());
        assert!(scene.try_advance_pure_play(&mut play, 1).unwrap().is_none());
        let report = scene.finish_stepped_play(play, &mut sink).unwrap();
        assert_eq!(report.n_frames, 9);
        assert!(!scene.stage().is_animating(mob));
        assert_eq!(scene.play_count(), 1);
        let play_state = scene.state_bytes().unwrap();

        let mut wait = scene.begin_seekable_wait_frames(11, &mut sink).unwrap();
        for (index, frame) in [13, 14, 20].into_iter().enumerate() {
            let metadata = if jumping && index != 1 {
                let count = (frame - scene.time().frames()).cast_unsigned();
                scene
                    .try_advance_pure_wait(&mut wait, count)
                    .unwrap()
                    .unwrap()
            } else {
                while scene.time().frames() < frame {
                    scene
                        .prepare_stepped_wait_frame(&mut wait)
                        .unwrap()
                        .unwrap();
                    scene
                        .complete_stepped_wait_frame(&mut wait, &mut sink)
                        .unwrap();
                }
                sink.samples.last().unwrap().1
            };
            assert_eq!(scene.play_count(), 1);
            observed.push((scene.state_bytes().unwrap(), metadata));
        }
        assert!(wait.frames_complete());
        assert!(scene.try_advance_pure_wait(&mut wait, 1).unwrap().is_none());
        let report = scene.finish_stepped_wait(wait, &mut sink).unwrap();
        assert_eq!(report.n_frames, 11);
        assert_eq!(scene.play_count(), 2);

        // Seconds-based overshoot keeps its original nominal sampling grid;
        // max_frames is only a cap and cannot advance beyond the wait.
        let mut wait = scene.begin_seekable_wait(Some(0.3), &mut sink).unwrap();
        let metadata = if jumping {
            scene
                .try_advance_pure_wait(&mut wait, u64::MAX)
                .unwrap()
                .unwrap()
        } else {
            while scene
                .prepare_stepped_wait_frame(&mut wait)
                .unwrap()
                .is_some()
            {
                scene
                    .complete_stepped_wait_frame(&mut wait, &mut sink)
                    .unwrap();
            }
            sink.samples.last().unwrap().1
        };
        assert_eq!(metadata.segment_frame(), 3);
        assert_eq!(metadata.frame_index(), 23);
        observed.push((scene.state_bytes().unwrap(), metadata));
        scene.finish_stepped_wait(wait, &mut sink).unwrap();
        assert_eq!(sink.samples.len(), if jumping { 2 } else { 23 });
        outcomes.push((
            observed,
            play_state,
            scene.state_bytes().unwrap(),
            sink.events,
        ));
    }
    assert_eq!(outcomes[0], outcomes[1]);
}

#[test]
fn seekable_owners_recheck_late_event_ingress_live_views_and_updaters() {
    for waiting in [false, true] {
        for effect in 0..4 {
            let mut scene = scene();
            let mob = scene
                .add_mobject(Mobject::from_points(&[[0.0; 3]]))
                .unwrap();
            let updated = scene
                .add_mobject(Mobject::from_points(&[[5.0, 0.0, 0.0]]))
                .unwrap();
            let event_time = Rc::new(Cell::new(None));
            let seen = Rc::clone(&event_time);
            scene
                .event_dispatcher_mut()
                .add_listener(EventListener::new(
                    EventType::MousePress,
                    EventTarget::Global,
                    move |_, _, _, stage| {
                        seen.set(Some(stage.time().to_bits()));
                        EventPropagation::Continue
                    },
                ))
                .unwrap();
            let mut sink = Sink {
                metadata_only: true,
                ..Sink::default()
            };
            let mut wait = waiting.then(|| scene.begin_seekable_wait_frames(4, &mut sink).unwrap());
            let mut play = if waiting {
                None
            } else {
                let animation = transform(&mut scene, mob);
                scene
                    .begin_seekable_play(
                        vec![Box::new(animation)],
                        PlayOverrides::default(),
                        &mut sink,
                    )
                    .unwrap()
            };
            let mut inbox = None;
            let mut view = None;
            let updates = Rc::new(Cell::new(0));
            match effect {
                0 => inbox = Some(scene.event_inbox()),
                1 => {
                    scene
                        .queue_event(EventPayload::MousePress {
                            point: [0.0; 3],
                            button: MouseButton::Left,
                            modifiers: Modifiers::NONE,
                        })
                        .unwrap();
                }
                2 => {
                    view = Some(
                        scene
                            .stage_mut()
                            .get_mut(mob)
                            .unwrap()
                            .buffer
                            .export_view(false),
                    )
                }
                _ => {
                    let seen = Rc::clone(&updates);
                    scene
                        .stage_mut()
                        .add_dt_updater(
                            updated,
                            move |stage, mob, dt| {
                                seen.set(seen.get() + 1);
                                stage.shift(mob, [dt, 0.0, 0.0]);
                            },
                            false,
                        )
                        .unwrap();
                }
            }
            let before = scene.state_bytes().unwrap();
            let refused = if let Some(wait) = &mut wait {
                scene.try_advance_pure_wait(wait, 3).unwrap()
            } else {
                scene
                    .try_advance_pure_play(play.as_mut().unwrap(), 3)
                    .unwrap()
            };
            assert!(refused.is_none(), "waiting {waiting}, observer {effect}");
            assert_eq!(scene.state_bytes().unwrap(), before);
            assert_eq!(event_time.get(), None);
            assert_eq!(updates.get(), 0);
            assert!(sink.samples.is_empty());
            if let Some(wait) = &mut wait {
                scene.prepare_stepped_wait_frame(wait).unwrap().unwrap();
                scene.complete_stepped_wait_frame(wait, &mut sink).unwrap();
            } else {
                let play = play.as_mut().unwrap();
                scene.prepare_stepped_play_frame(play).unwrap().unwrap();
                scene.complete_stepped_play_frame(play, &mut sink).unwrap();
            }
            assert_eq!(sink.samples[0].1.frame_index(), 1);
            assert_eq!(
                event_time.get(),
                (effect == 1).then_some(0.125_f64.to_bits())
            );
            assert_eq!(updates.get(), usize::from(effect == 3));
            drop(view);
            drop(inbox);
        }
    }
}

#[test]
fn capture_demand_is_rechecked_after_each_updater_boundary() {
    struct SwitchingSink {
        state_required: Rc<Cell<bool>>,
        metadata: Vec<i64>,
        packets: Vec<FramePacket>,
    }
    impl SceneSink for SwitchingSink {
        fn capture_state_required(&self) -> bool {
            self.state_required.get()
        }
        fn capture(
            &mut self,
            _: CaptureReason,
            packet: FramePacket,
        ) -> Result<(), IntegrationError> {
            self.packets.push(packet);
            Ok(())
        }
        fn capture_metadata(
            &mut self,
            _: CaptureReason,
            frame: FrameMetadata,
        ) -> Result<(), IntegrationError> {
            self.metadata.push(frame.frame_index());
            Ok(())
        }
    }
    let mut scene = scene();
    let mob = scene
        .add_mobject(Mobject::from_points(&[[0.0; 3]]))
        .unwrap();
    let state_required = Rc::new(Cell::new(false));
    let demand = Rc::clone(&state_required);
    scene
        .stage_mut()
        .add_dt_updater(
            mob,
            move |stage, mob, dt| {
                stage.shift(mob, [dt, 0.0, 0.0]);
                demand.set(stage.time() == 0.25 || stage.time() == 0.375);
            },
            false,
        )
        .unwrap();
    let mut sink = SwitchingSink {
        state_required,
        metadata: Vec::new(),
        packets: Vec::new(),
    };
    scene.wait(Some(0.5), &mut sink).unwrap();
    assert_eq!(sink.metadata, [1, 4]);
    assert_eq!(
        sink.packets
            .iter()
            .map(FramePacket::frame_index)
            .collect::<Vec<_>>(),
        [2, 3]
    );
    for packet in sink.packets {
        assert_eq!(
            packet.materialize_stage().get_center(mob)[0],
            packet.time().to_f64()
        );
    }
    assert_eq!(scene.stage().get_center(mob)[0], 0.5);
}

#[test]
fn metadata_failure_is_latched_while_the_segment_finishes_normally() {
    let mut results = Vec::new();
    for metadata_only in [false, true] {
        let mut scene = scene();
        let mob = scene
            .add_mobject(Mobject::from_points(&[[0.0; 3]]))
            .unwrap();
        let animation = transform(&mut scene, mob);
        let mut sink = Sink {
            metadata_only,
            fail_at: Some(2),
            ..Sink::default()
        };
        let error = scene
            .play(
                vec![Box::new(animation)],
                PlayOverrides::default(),
                &mut sink,
            )
            .unwrap_err();
        assert!(error.to_string().contains("capture failed"));
        assert_eq!(sink.samples.len(), 2);
        assert_eq!(scene.time().frames(), 3);
        assert_eq!(scene.play_count(), 1);
        assert!(
            !sink
                .events
                .iter()
                .any(|event| event.phase == LifecyclePhase::PostPlay)
        );
        results.push((
            sink.samples,
            sink.events,
            scene.stage().get_center(mob).map(f64::to_bits),
        ));
    }
    assert_eq!(results[0], results[1]);
}

#[test]
fn show_idle_presenter_and_skipped_preview_obey_metadata_demand() {
    let mut sink = Sink {
        metadata_only: true,
        ..Sink::default()
    };
    let mut scene = scene();
    scene.show(&mut sink).unwrap();
    let idle = scene.prepare_idle_frame(&mut sink).unwrap();
    scene.complete_idle_frame(idle, &mut sink).unwrap();
    assert_eq!(
        sink.samples
            .iter()
            .map(|(reason, _)| *reason)
            .collect::<Vec<_>>(),
        [CaptureReason::Show, CaptureReason::PresenterHold]
    );

    let mut presenter = Scene::new(
        RuntimeConfig {
            fps: 8,
            presenter_mode: true,
            ..RuntimeConfig::default()
        },
        19,
    )
    .unwrap();
    let mut held = false;
    presenter.set_hold_controller(move |kind: HoldKind, _: &Stage, _: RationalTime| {
        if kind == HoldKind::Wait && !held {
            held = true;
            Ok(HoldDecision::Continue)
        } else {
            Ok(HoldDecision::Release)
        }
    });
    presenter.wait(Some(1.0), &mut sink).unwrap();
    assert_eq!(sink.samples.last().unwrap().0, CaptureReason::PresenterHold);
    assert_eq!(presenter.time().frames(), 1);

    let mut skipped = Scene::new(
        RuntimeConfig {
            windowed: true,
            skip_animations: true,
            preview_while_skipping: true,
            ..RuntimeConfig::default()
        },
        19,
    )
    .unwrap();
    skipped.wait(Some(0.1), &mut sink).unwrap();
    assert_eq!(
        sink.samples.last().unwrap().0,
        CaptureReason::SkippedPreview
    );
    assert!(sink.packets.is_empty());
}

#[test]
fn custom_sinks_still_receive_frozen_state_unless_they_opt_out() {
    #[derive(Default)]
    struct FullSink(Vec<FramePacket>);
    impl SceneSink for FullSink {
        fn capture(
            &mut self,
            _: CaptureReason,
            packet: FramePacket,
        ) -> Result<(), IntegrationError> {
            self.0.push(packet);
            Ok(())
        }
    }
    let mut scene = scene();
    let mob = scene
        .add_mobject(Mobject::from_points(&[[0.0; 3]]))
        .unwrap();
    let mut sink = FullSink::default();
    assert!(sink.capture_state_required());
    assert!(!NullSceneSink.capture_state_required());
    scene.wait(Some(0.25), &mut sink).unwrap();
    scene.stage_mut().shift(mob, [9.0, 0.0, 0.0]);
    assert_eq!(sink.0.len(), 2);
    assert_eq!(sink.0[0].materialize_stage().get_center(mob), [0.0; 3]);
}

#[test]
fn final_capture_precedes_removers_and_zero_dt_cleanup_without_freezing_earlier_frames() {
    let mut scene = scene();
    let mob = scene
        .add_mobject(Mobject::from_points(&[[0.0; 3]]))
        .unwrap();
    let marker = scene
        .add_mobject(Mobject::from_points(&[[0.0; 3]]))
        .unwrap();
    scene
        .stage_mut()
        .add_dt_updater(
            marker,
            |stage, marker, dt| {
                if dt == 0.0 {
                    stage.shift(marker, [0.0, 100.0, 0.0]);
                }
            },
            false,
        )
        .unwrap();
    let target = scene.stage_mut().copy_family(mob).unwrap();
    scene.stage_mut().shift(target, [3.0, 0.0, 0.0]);
    let animation = Transform::new(mob, target).with_config(AnimConfig {
        run_time: 0.375,
        rate_func: RateFunc::linear(),
        remover: true,
        ..AnimConfig::default()
    });
    let mut sink = Sink {
        metadata_only: true,
        final_state: true,
        ..Sink::default()
    };
    scene
        .play(
            vec![Box::new(animation)],
            PlayOverrides::default(),
            &mut sink,
        )
        .unwrap();
    assert_eq!(sink.samples.len(), 3);
    assert_eq!(sink.packets.len(), 1);
    assert_eq!(sink.packets[0].frame_index(), 3);
    let frozen = sink.packets[0].materialize_stage();
    assert!(frozen.roots().contains(&mob));
    assert!(!scene.stage().roots().contains(&mob));
    assert_eq!(frozen.get_center(mob), [3.0, 0.0, 0.0]);
    assert_eq!(
        scene.stage().get_center(marker)[1] - frozen.get_center(marker)[1],
        100.0
    );
    scene.stage_mut().shift(marker, [7.0, 0.0, 0.0]);
    scene.wait_frames(4, &mut sink).unwrap();
    assert_eq!(sink.samples.len(), 7);
    assert_eq!(sink.packets.len(), 2);
    assert_eq!(sink.packets[1].frame_index(), 7);
    assert_eq!(
        sink.packets[0].materialize_stage().get_center(marker)[0],
        0.0
    );
    assert_eq!(
        sink.packets[1].materialize_stage().get_center(marker)[0],
        7.0
    );

    scene.wait_frames(0, &mut sink).unwrap();
    scene.set_skipping(true);
    scene.wait_frames(3, &mut sink).unwrap();
    assert_eq!(sink.packets.len(), 2);
    assert_eq!(sink.samples.len(), 7);
}

#[test]
fn final_capture_works_at_stepped_boundaries_and_native_stop_predicates() {
    let mut scene = scene();
    let mob = scene
        .add_mobject(Mobject::from_points(&[[0.0; 3]]))
        .unwrap();
    let animation = transform(&mut scene, mob);
    let mut sink = Sink {
        metadata_only: true,
        final_state: true,
        ..Sink::default()
    };
    let mut play = scene
        .begin_stepped_play(
            vec![Box::new(animation)],
            PlayOverrides::default(),
            &mut sink,
        )
        .unwrap()
        .unwrap();
    while scene
        .prepare_stepped_play_frame(&mut play)
        .unwrap()
        .is_some()
    {
        scene
            .complete_stepped_play_frame(&mut play, &mut sink)
            .unwrap();
    }
    assert_eq!(sink.packets.len(), 1);
    assert_eq!(sink.packets[0].frame_index(), 3);
    scene.finish_stepped_play(play, &mut sink).unwrap();
    let mut wait = scene.begin_stepped_wait_frames(3, &mut sink).unwrap();
    while scene
        .prepare_stepped_wait_frame(&mut wait)
        .unwrap()
        .is_some()
    {
        scene
            .complete_stepped_wait_frame(&mut wait, &mut sink)
            .unwrap();
    }
    assert_eq!(sink.packets.len(), 2);
    assert_eq!(sink.packets[1].frame_index(), 6);
    scene.finish_stepped_wait(wait, &mut sink).unwrap();

    let report = scene
        .wait_until_frames(8, &mut |stage| stage.time() >= 1.0, &mut sink)
        .unwrap();
    assert_eq!(report.n_frames, 2);
    assert_eq!(sink.packets.len(), 4);
    assert_eq!(sink.packets[2].frame_index(), 7);
    assert_eq!(sink.packets[3].frame_index(), 8);
    scene.show(&mut sink).unwrap();
    assert_eq!(sink.packets.len(), 5);
    assert_eq!(sink.samples.last().unwrap().0, CaptureReason::Show);
    assert!(!NullSceneSink.capture_final_state_required());
}
