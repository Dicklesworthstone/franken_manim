//! Ordinary Scene routing: no observer is bypassed to acquire parallelism.
use fmn_anim::{AnimConfig, FramePacket, RateFunc, Transform};
use fmn_anim::pure_frame::PureSegment;
use fmn_mobject::{Mobject, Stage};
use fmn_scene::{CaptureReason, EventListener, EventPayload, EventPropagation, EventTarget,
    EventType, IntegrationError, LifecycleEvent, LifecyclePhase, Modifiers, MouseButton,
    PlayOverrides, RuntimeConfig, Scene, SceneSink};

#[derive(Default)]
struct Sink {
    opt_in: bool,
    fail: bool,
    programs: usize,
    packets: usize,
    frames: Vec<(i64, [u64; 3])>,
    events: Vec<LifecyclePhase>,
}
fn center(stage: &Stage) -> [u64; 3] {
    stage.get_center(stage.roots()[0]).map(f64::to_bits)
}
impl SceneSink for Sink {
    fn event(&mut self, event: LifecycleEvent) -> Result<(), IntegrationError> {
        self.events.push(event.phase);
        Ok(())
    }
    fn capture(&mut self, _: CaptureReason, packet: FramePacket) -> Result<(), IntegrationError> {
        self.packets += 1;
        self.frames.push((packet.frame_index(), center(&packet.materialize_stage())));
        if self.fail { Err(IntegrationError::new("fixture", "capture failed")) } else { Ok(()) }
    }
    fn accepts_pure_segments(&self) -> bool { self.opt_in }
    fn capture_pure_segment(&mut self, segment: &PureSegment) -> Result<(), IntegrationError> {
        self.programs += 1;
        // Real native jobs run on another thread, then return to frame order.
        let program = segment.clone();
        let mut frames = std::thread::spawn(move || {
            (1..=program.frame_count()).rev().map(|i| {
                let job = program.job(i).unwrap();
                (job.time().frames(), center(&job.materialize()))
            }).collect::<Vec<_>>()
        }).join().unwrap();
        frames.sort_by_key(|(i, _)| *i);
        self.frames.extend(frames);
        if self.fail { Err(IntegrationError::new("fixture", "capture failed")) } else { Ok(()) }
    }
}
fn animation(scene: &mut Scene) -> (fmn_mobject::Mob, Transform) {
    let stage = scene.stage_mut();
    let mob = stage.add(Mobject::from_points(&[[0.0, 0.0, 0.0]]));
    stage.add_to_scene(mob).unwrap();
    let target = stage.copy_family(mob).unwrap();
    stage.shift(target, [3.0, 1.0, 0.0]);
    (mob, Transform::new(mob, target).with_config(AnimConfig {
        run_time: 0.5, rate_func: RateFunc::linear(), ..AnimConfig::default()
    }))
}
fn press() -> EventPayload {
    EventPayload::MousePress { point: [0.0; 3], button: MouseButton::Left,
        modifiers: Modifiers::NONE }
}

#[test]
fn opted_in_native_play_matches_the_recording_sink_and_continues_statefully() {
    let mut outcomes = Vec::new();
    for opt_in in [false, true] {
        let mut scene = Scene::new(RuntimeConfig { fps: 8, ..RuntimeConfig::default() }, 19).unwrap();
        let (mob, animation) = animation(&mut scene);
        let mut sink = Sink { opt_in, ..Sink::default() };
        let report = scene.play(vec![Box::new(animation)], PlayOverrides::default(), &mut sink)
            .unwrap().unwrap();
        assert!(report.purity.is_pure());
        assert_eq!(sink.programs, usize::from(opt_in));
        assert_eq!(sink.packets, if opt_in { 0 } else { 4 });
        assert_eq!(scene.play_count(), 1);
        scene.stage_mut().add_dt_updater(mob, |stage, mob, dt| {
            stage.shift(mob, [dt, 0.0, 0.0]);
        }, false).unwrap();
        scene.wait(Some(0.25), &mut sink).unwrap();
        assert_eq!(sink.packets, if opt_in { 2 } else { 6 });
        outcomes.push((sink.frames, sink.events, center(scene.stage()), scene.time(), scene.play_count()));
    }
    assert_eq!(outcomes[0], outcomes[1]);
    assert_ne!(outcomes[0].0[0].1, outcomes[0].0[3].1);
}

#[test]
fn events_inboxes_views_and_updaters_keep_every_serial_capture() {
    for kind in 0..6 {
        let mut scene = Scene::new(RuntimeConfig {
            fps: 8, windowed: kind == 5, ..RuntimeConfig::default()
        }, 2).unwrap();
        let (mob, animation) = animation(&mut scene);
        let mut inbox = None;
        let mut view = None;
        match kind {
            0 => inbox = Some(scene.event_inbox()), // EMPTY but another producer exists.
            1 => { scene.event_dispatcher_mut().add_listener(EventListener::new(
                EventType::MousePress, EventTarget::Global,
                |_, _, _, _| EventPropagation::Continue,
            )).unwrap(); }
            2 => { scene.queue_event(press()).unwrap(); }
            3 => view = Some(scene.stage_mut().get_mut(mob).unwrap().buffer.export_view(false)),
            4 => { scene.stage_mut().add_dt_updater(mob, |_, _, _| {}, false).unwrap(); }
            _ => {}
        }
        let mut sink = Sink { opt_in: true, ..Sink::default() };
        let report = scene.play(vec![Box::new(animation)], PlayOverrides::default(), &mut sink)
            .unwrap().unwrap();
        assert_eq!(sink.programs, 0, "kind {kind}");
        assert_eq!(sink.packets as i64, report.n_frames, "kind {kind}");
        assert!(sink.packets >= 4);
        drop(view);
        drop(inbox);
    }
}

#[test]
fn sink_failure_completes_owner_cleanup_without_replaying_the_program() {
    for opt_in in [false, true] {
        let mut scene = Scene::new(RuntimeConfig { fps: 8, ..RuntimeConfig::default() }, 0).unwrap();
        let (mob, animation) = animation(&mut scene);
        let mut sink = Sink { opt_in, fail: true, ..Sink::default() };
        let error = scene.play(vec![Box::new(animation)], PlayOverrides::default(), &mut sink)
            .unwrap_err();
        assert!(error.to_string().contains("capture failed"));
        assert_eq!(sink.programs, usize::from(opt_in));
        assert_eq!(sink.packets, usize::from(!opt_in));
        assert_eq!(scene.play_count(), 1);
        assert_eq!(scene.time().frames(), 4);
        assert_eq!(scene.stage().get_center(mob), [3.0, 1.0, 0.0]);
        assert!(!scene.stage().is_animating(mob));
    }
}

#[test]
fn releasing_an_empty_inbox_restores_admission_but_skips_never_offload() {
    for skip in [false, true] {
        let mut scene = Scene::new(RuntimeConfig {
            fps: 8, skip_animations: skip, preview_while_skipping: false,
            ..RuntimeConfig::default()
        }, 0).unwrap();
        let (mob, animation) = animation(&mut scene);
        let inbox = scene.event_inbox();
        drop(inbox);
        let mut sink = Sink { opt_in: true, ..Sink::default() };
        scene.play(vec![Box::new(animation)], PlayOverrides::default(), &mut sink).unwrap();
        assert_eq!(sink.programs, usize::from(!skip));
        assert_eq!(sink.packets, 0);
        assert_eq!(scene.stage().get_center(mob), [3.0, 1.0, 0.0]);
        assert_eq!(scene.time().frames(), 4);
    }
}
