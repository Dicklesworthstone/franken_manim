use std::cell::Cell;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use fmn::prelude::*;
use fmn_config::config::DeterminismMode;
use fmn_platform::fs::{FileSystem, VirtualFs};
use fmn_platform::topology::HardwareTopology;
use fmn_runtime::{ExecutionPlan, OutputPixelFormat, PlanRequest, RenderIntent, SurfaceSpec};
use fmn_scene::{CaptureReason, IntegrationError, LifecycleEvent, RuntimeConfig, SceneSink};

use crate::{
    CancellableSceneSink, NativeFrameFormat, RenderCancellation, RenderSink, RenderTarget,
    camera_route,
};
use super::DispatchStats;

struct Mixed {
    updates: Rc<Cell<u32>>,
}

impl SceneConstruct for Mixed {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        let mob = stage.add(Circle::new().radius(0.4).color(BLUE))?;
        stage.set_fill(mob, Some(BLUE), Some(0.8), None, true);
        stage.play(mob.animate().set_anim_args(AnimateArgs {
            run_time: Some(0.5), path_arc: Some(0.8), ..AnimateArgs::default()
        })?.shift(RIGHT)?)?;
        let owner = std::thread::current().id();
        let updates = Rc::clone(&self.updates);
        let updater = stage.scene_mut().stage_mut().add_dt_updater(mob, move |stage, mob, dt| {
            assert_eq!(std::thread::current().id(), owner);
            updates.set(updates.get() + 1);
            stage.shift(mob, [0.0, dt, 0.0]);
        }, false)?;
        stage.wait(0.25)?;
        stage.scene_mut().stage_mut().remove_updater(mob, updater);
        stage.play(mob.animate().set_anim_args(AnimateArgs {
            run_time: Some(0.5), ..AnimateArgs::default()
        })?.shift(LEFT)?)?;
        stage.wait(0.25)?;
        Ok(())
    }
}

/// Independent ordinary packet dispatch, not a second pure-program sampler.
struct SerialOnly<'a>(&'a mut dyn SceneSink);
impl SceneSink for SerialOnly<'_> {
    fn event(&mut self, event: LifecycleEvent) -> Result<(), IntegrationError> {
        self.0.event(event)
    }
    fn capture(&mut self, reason: CaptureReason, packet: FramePacket) -> Result<(), IntegrationError> {
        self.0.capture(reason, packet)
    }
}

fn config() -> fmn_config::Config {
    let mut config = fmn_config::Config::resolve(&[], None).unwrap().config;
    config.camera.resolution = (48, 32);
    config.camera.fps = 8;
    config.sizes.frame_height = 4.0;
    config.determinism.mode = DeterminismMode::Certified;
    config
}

fn plan(threads: usize, window: usize) -> ExecutionPlan {
    let mut surface = SurfaceSpec::lumen(48, 32);
    surface.working_bytes_per_pixel = 32;
    ExecutionPlan::derive(
        PlanRequest::certified(RenderIntent::Offline, surface, OutputPixelFormat::Rgba8)
            .with_max_frames_in_flight(window).with_max_cpu_threads(threads),
        &HardwareTopology::from_group_sizes(&[8, 8]).unwrap(), None,
    ).unwrap()
}

fn sink(fs: Arc<VirtualFs>, camera: bool, threads: usize, window: usize) -> RenderSink {
    let config = config();
    RenderSink::new_with_camera(fs, &config, &plan(threads, window),
        &RenderTarget::Native(NativeFrameFormat::PngSequence), "/frames".into(),
        camera.then(|| camera_route::camera(&config).unwrap())).unwrap()
}

#[test]
fn mixed_native_plays_and_waits_match_serial_captures_on_fixed_camera_and_affine_teams() {
    for camera in [false, true] {
        let mut across_threads = None;
        for (threads, window) in [(1, 1), (4, 2), (16, 4)] {
            let mut expected = None;
            let mut expected_updates = None;
            for offload in [false, true] {
                let fs = Arc::new(VirtualFs::new());
                let mut sink = sink(fs.clone(), camera, threads, window);
                assert!(sink.accepts_pure_segments());
                let updates = Rc::new(Cell::new(0));
                let mut scene = Mixed { updates: updates.clone() };
                let runtime = RuntimeConfig::from_config(&config());
                if offload {
                    // Exercise batch's actual forwarding adapter too.
                    let cancellation = RenderCancellation::default();
                    cancellation.register_emitter(sink.emitter_handle().unwrap());
                    let mut forwarding = CancellableSceneSink { inner: &mut sink, cancellation: &cancellation };
                    assert!(forwarding.accepts_pure_segments());
                    fmn::run_scene(&mut scene, runtime, 17, &mut forwarding).unwrap();
                } else {
                    fmn::run_scene(&mut scene, runtime, 17, &mut SerialOnly(&mut sink)).unwrap();
                }
                let finished = sink.finish(None).unwrap();
                assert_eq!(finished.backend.frames, 12);
                let dispatch = finished.backend.dispatch;
                assert_eq!(dispatch.segments, 4);
                assert_eq!(dispatch.pure_segments, if offload { 3 } else { 0 });
                assert_eq!(dispatch.pure_frames, if offload { 10 } else { 0 });
                assert_eq!(dispatch.compiled_frames, 0);
                let stats = finished.backend.pipeline.unwrap();
                assert_eq!((stats.submitted, stats.emitted, stats.outstanding_slots), (12, 12, 0));
                assert!(stats.max_in_flight <= window);
                assert_eq!(stats.render_team_frames.iter().sum::<u64>(), 12);
                let frames: Vec<_> = (0..12).map(|index| {
                    fs.read(&Path::new("/frames").join(format!("frame_{index:06}.png"))).unwrap()
                }).collect();
                assert_ne!(frames[0], frames[3]);
                assert_ne!(frames[3], frames[5]);
                if let Some(expected) = &expected {
                    assert_eq!(&frames, expected, "threads {threads}, window {window}, camera {camera}");
                } else { expected = Some(frames.clone()); }
                if let Some(previous) = expected_updates {
                    assert_eq!(updates.get(), previous);
                } else { expected_updates = Some(updates.get()); }
                assert!(updates.get() > 0);
                if offload {
                    if let Some(previous) = &across_threads { assert_eq!(&frames, previous); }
                    else { across_threads = Some(frames); }
                }
            }
        }
    }
}

#[test]
fn an_aborted_pure_generation_cannot_be_finished_or_restarted_and_does_not_publish() {
    let fs = Arc::new(VirtualFs::new());
    let mut sink = sink(fs.clone(), false, 4, 2);
    let mut scene = fmn_scene::Scene::new(RuntimeConfig::from_config(&config()), 0).unwrap();
    // Earlier pure samples can have reached temporary output already.
    scene.wait(Some(0.25), &mut sink).unwrap();
    assert_eq!(sink.backend.dispatch.pure_frames, 2);
    sink.emitter_handle().unwrap().cancel();
    assert!(scene.wait(Some(0.5), &mut sink).is_err());
    assert!(sink.failure.is_some());
    assert!(!sink.accepts_pure_segments());
    assert!(sink.finish(None).is_err());
    assert!(!fs.exists(Path::new("/frames")));
    let mut retry = self::sink(fs.clone(), false, 4, 2);
    let mut scene = fmn_scene::Scene::new(RuntimeConfig::from_config(&config()), 0).unwrap();
    scene.wait(Some(0.25), &mut retry).unwrap();
    assert_eq!(retry.finish(None).unwrap().backend.dispatch.pure_frames, 2);
    assert!(fs.exists(Path::new("/frames")));
}

#[test]
fn still_png_and_recording_sinks_do_not_advertise_native_program_capture() {
    let fs = Arc::new(VirtualFs::new());
    let mut sink = RenderSink::new(fs, &config(), &plan(1, 1),
        &RenderTarget::Native(NativeFrameFormat::Png), "/still.png".into()).unwrap();
    assert!(!sink.accepts_pure_segments());
    assert!(!SerialOnly(&mut sink).accepts_pure_segments());
}

#[cfg(feature = "batch")]
#[test]
fn cancelled_batch_adapter_refuses_pure_dispatch_before_scene_admission() {
    let fs = Arc::new(VirtualFs::new());
    let mut sink = sink(fs.clone(), false, 4, 2);
    let cancellation = RenderCancellation::default();
    cancellation.register_emitter(sink.emitter_handle().unwrap());
    cancellation.request(crate::RENDER_CANCEL_FAIL_FAST);
    let mut adapter = CancellableSceneSink { inner: &mut sink, cancellation: &cancellation };
    assert!(!adapter.accepts_pure_segments());
    let mut scene = fmn_scene::Scene::new(RuntimeConfig::from_config(&config()), 0).unwrap();
    assert!(scene.wait(Some(0.5), &mut adapter).is_err());
    drop(sink);
    assert!(!fs.exists(Path::new("/frames")));
}

#[test]
fn diagnostics_reject_impossible_counts_and_do_not_change_renderer_identity() {
    assert!(DispatchStats { pure_segments: 1, ..Default::default() }.validate(0).is_err());
    assert!(DispatchStats { segments: 1, pure_segments: 1, pure_frames: 2, compiled_frames: 1 }.validate(2).is_err());
    assert!(DispatchStats { pure_frames: u64::MAX, compiled_frames: 1, ..Default::default() }.validate(u64::MAX).is_err());
    let serial = DispatchStats { segments: 3, ..Default::default() };
    let pure = DispatchStats { segments: 3, pure_segments: 2, pure_frames: 8, compiled_frames: 0 };
    assert!(pure.validate(10).is_ok());
    assert!(serial.to_json().contains("\"serial_segments\":3"));
    assert!(pure.to_json().contains("\"serial_segments\":1"));
}
