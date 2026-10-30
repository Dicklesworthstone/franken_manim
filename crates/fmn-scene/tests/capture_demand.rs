//! Construct-only consumers preserve sampling and lifecycle without freezing
//! an arena for every discarded frame. Rendering stays the safe default.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use fmn_anim::{AnimConfig, FrameMetadata, FramePacket, RateFunc, RationalTime, Transform};
use fmn_mobject::{Mob, Mobject, Stage};
use fmn_scene::{
    CaptureReason, HoldDecision, HoldKind, IntegrationError, LifecycleEvent, LifecyclePhase,
    NullSceneSink, PlayOverrides, RuntimeConfig, Scene, SceneSink,
};

#[derive(Default)]
struct Sink {
    metadata_only: bool,
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

    fn capture(
        &mut self,
        reason: CaptureReason,
        packet: FramePacket,
    ) -> Result<(), IntegrationError> {
        assert!(!self.metadata_only, "a discarded frame must not be frozen");
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
