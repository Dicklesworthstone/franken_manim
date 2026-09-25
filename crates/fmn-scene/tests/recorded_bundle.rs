//! A real imperative Scene drives both the observer and the FMTL sink.
use std::cell::Cell;
use std::rc::Rc;

use fmn_anim::{FramePacket, RationalFrameClock, prepare_animation};
use fmn_core::rng::RngRoot;
use fmn_hash::sha256;
use fmn_mobject::{AnimateArgs, Mobject, Stage};
use fmn_scene::recording::{RecordingError, SceneBundleRecorder};
use fmn_scene::{
    BundleError, BundleExportLimits, BundleSegmentKind, CaptureReason, IntegrationError,
    LifecycleEvent, PlayOverrides, RuntimeConfig, Scene, SceneSink, TimelineBundle,
};

struct ObservedRecording {
    recorder: SceneBundleRecorder,
    frames: Vec<Vec<u8>>,
}

impl SceneSink for ObservedRecording {
    fn event(&mut self, event: LifecycleEvent) -> Result<(), IntegrationError> {
        self.recorder.event(event)
    }

    fn capture(&mut self, reason: CaptureReason, packet: FramePacket) -> Result<(), IntegrationError> {
        self.frames.push(packet.state().to_bytes().unwrap());
        self.recorder.capture(reason, packet)
    }
}

fn scene(fps: u32) -> Scene {
    Scene::new(RuntimeConfig { fps, ..RuntimeConfig::default() }, 19).unwrap()
}

#[test]
fn imperative_play_wait_and_show_replay_every_observed_snapshot_without_callbacks() {
    let mut source = scene(30);
    let moving = source.add_mobject(Mobject::from_points(&[[0.0, 0.0, 0.0]])).unwrap();
    let following = source.add_mobject(Mobject::from_points(&[[0.0, 1.0, 0.0]])).unwrap();
    let calls = Rc::new(Cell::new(0));
    let counter = calls.clone();
    source.stage_mut().add_updater(following, move |stage, me| {
        counter.set(counter.get() + 1);
        // This depends on post-interpolation geometry, not just final endpoints.
        let x = stage.get_center(moving)[0];
        stage.set_x(me, x * x);
    }, false).unwrap();
    let animation = prepare_animation(
        moving.animate().set_anim_args(AnimateArgs {
            run_time: Some(0.1),
            rate_func: Some(fmn_core::rate::there_and_back),
            ..AnimateArgs::default()
        }).unwrap().shift([2.0, 0.0, 0.0]).unwrap(),
        source.stage_mut(),
    ).unwrap();
    let mut sink = ObservedRecording {
        recorder: SceneBundleRecorder::new(30, BundleExportLimits::default()).unwrap(),
        frames: Vec::new(),
    };
    source.play(vec![animation], PlayOverrides::default(), &mut sink).unwrap();
    source.wait(Some(0.0), &mut sink).unwrap();
    source.show(&mut sink).unwrap();
    source.wait(Some(0.1), &mut sink).unwrap();
    let calls_before_playback = calls.get();
    assert!(calls_before_playback > 0);
    let artifact = sink.recorder.finish().unwrap();
    assert_eq!(artifact.digest, sha256(&artifact.bytes));
    assert_eq!(artifact.frame_count as usize, sink.frames.len());
    assert_eq!(artifact.segment_count, 4);
    let bundle = TimelineBundle::from_bytes(&artifact.bytes).unwrap();
    for segment in 0..4 {
        assert_eq!(bundle.segment_kind(segment), Some(BundleSegmentKind::Stateful));
    }
    // Reverse/random-access playback must not execute the original updater.
    for index in (0..artifact.frame_count).rev() {
        assert_eq!(bundle.stage_at(index).unwrap().snapshot().to_bytes().unwrap(), sink.frames[index as usize]);
    }
    assert_eq!(calls.get(), calls_before_playback);
}

#[test]
fn clock_rounding_and_early_stop_preserve_emitted_counts() {
    for fps in [1, 3, 10, 24, 30, 60, 144, 1001] {
        let mut source = scene(fps);
        let mut sink = SceneBundleRecorder::new(fps, BundleExportLimits::default()).unwrap();
        source.wait(Some(0.1), &mut sink).unwrap();
        source.wait_until(1.0, &mut |_| true, &mut sink).unwrap();
        let observed = sink.frame_count();
        let artifact = sink.finish().unwrap();
        assert_eq!(u64::from(artifact.frame_count), observed);
        assert_eq!(TimelineBundle::from_bytes(&artifact.bytes).unwrap().fps(), fps);
    }
}

#[test]
fn static_terminal_capture_is_explicit_and_deterministic() {
    let mut stage = Stage::new();
    let mob = stage.add(Mobject::from_points(&[[1.0, 2.0, 0.0]]));
    stage.add_to_scene(mob).unwrap();
    let export = || {
        let mut sink = SceneBundleRecorder::new(30, BundleExportLimits::default()).unwrap();
        sink.capture_terminal_still(&stage).unwrap();
        sink.finish().unwrap()
    };
    let a = export();
    let b = export();
    assert_eq!(a.bytes, b.bytes);
    assert_eq!(a.frame_count, 1);
    let restored = TimelineBundle::from_bytes(&a.bytes).unwrap().stage_at(0).unwrap();
    assert_eq!(restored.snapshot().to_bytes().unwrap(), stage.snapshot().to_bytes().unwrap());
}

#[test]
fn frame_limit_is_typed_sticky_and_never_yields_a_partial_bundle() {
    let mut source = scene(30);
    let mut sink = SceneBundleRecorder::new(30, BundleExportLimits {
        max_frames: 1,
        ..BundleExportLimits::default()
    }).unwrap();
    assert!(source.wait(Some(0.1), &mut sink).is_err());
    assert_eq!(sink.frame_count(), 1);
    assert!(source.show(&mut sink).is_err());
    assert!(matches!(sink.finish(), Err(RecordingError::Bundle(
        BundleError::FrameLimitExceeded { frames: 2, max_frames: 1 }
    ))));
}

#[test]
fn capture_budget_includes_destination_tables() {
    let stage = Stage::new();
    let snapshot_bytes = stage.snapshot().to_bytes().unwrap().len();
    let mut sink = SceneBundleRecorder::new(30, BundleExportLimits {
        max_frames: 10,
        max_capture_bytes: snapshot_bytes,
    }).unwrap();
    assert!(sink.capture_terminal_still(&stage).is_err());
    assert!(matches!(sink.finish(), Err(RecordingError::Bundle(
        BundleError::CaptureLimitExceeded { .. }
    ))));
}

#[test]
fn unsupported_capture_and_clock_mismatch_fail_closed() {
    let stage = Stage::new();
    let clock = RationalFrameClock::new(30).unwrap();
    let rng = RngRoot::from_seed(0);
    for reason in [CaptureReason::SkippedPreview, CaptureReason::PresenterHold, CaptureReason::Segment] {
        let mut sink = SceneBundleRecorder::new(30, BundleExportLimits::default()).unwrap();
        assert!(sink.capture(reason, FramePacket::freeze_barrier(&stage, &clock, &rng)).is_err());
        assert!(sink.finish().is_err());
    }
    let mut source = scene(24);
    let mut sink = SceneBundleRecorder::new(30, BundleExportLimits::default()).unwrap();
    assert!(source.wait(Some(0.25), &mut sink).is_err());
    assert!(sink.finish().is_err());
}

#[test]
fn zero_frame_budget_refuses_even_terminal_stills() {
    let mut sink = SceneBundleRecorder::new(30, BundleExportLimits {
        max_frames: 0,
        ..BundleExportLimits::default()
    }).unwrap();
    assert!(sink.capture_terminal_still(&Stage::new()).is_err());
    assert_eq!(sink.frame_count(), 0);
    assert!(sink.finish().is_err());
    assert!(SceneBundleRecorder::new(0, BundleExportLimits::default()).is_err());
}
