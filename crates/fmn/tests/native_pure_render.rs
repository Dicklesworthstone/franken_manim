//! Public scene-to-artifact integration, mixed with stateful segments.
use std::path::Path;
use std::sync::Arc;
use fmn::prelude::*;
use fmn::rendering::RenderError;
use fmn_codec::{PngLimits, decode_png};
use fmn_config::config::{DeterminismMode, ThreadPolicy};
use fmn_frame::{FrameBuffer, FrameLayout, PixelFormat};
use fmn_frame::convert::rgba16f_to_rgba8;
use fmn_platform::fs::{FileSystem, VirtualFs};
use fmn_render::{Camera, EngineIdentity, FrameConfig, RetainedFrameRenderer,
    RetainedFrameRendererConfig, ScreenMap, Tiling, Viewport};
use fmn_scene::{CaptureReason, IntegrationError, RuntimeConfig, SceneSink};

fn options(path: &str, window: usize, threads: u32) -> RenderOptions {
    let mut options = RenderOptions::new(path).unwrap();
    options.config.camera.resolution = (48, 32);
    options.config.camera.fps = 8;
    options.config.sizes.frame_height = 4.0;
    options.config.determinism.mode = DeterminismMode::Certified;
    options.config.render.threads = ThreadPolicy::Fixed(threads);
    options.frames_in_flight = window;
    options
}
struct Mixed;
impl SceneConstruct for Mixed {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        let mob = stage.add(Circle::new().radius(0.4).color(BLUE))?;
        stage.set_fill(mob, Some(BLUE), Some(0.8), None, true);
        stage.play(mob.animate().set_anim_args(AnimateArgs {
            run_time: Some(0.5), path_arc: Some(0.8), ..AnimateArgs::default()
        })?.shift(RIGHT)?)?;
        let updater = stage.scene_mut().stage_mut().add_dt_updater(mob, |stage, mob, dt| {
            stage.shift(mob, [0.0, dt, 0.0]);
        }, false)?;
        stage.wait(0.25)?;
        stage.scene_mut().stage_mut().remove_updater(mob, updater);
        stage.play(mob.animate().set_anim_args(AnimateArgs {
            run_time: Some(0.5), ..AnimateArgs::default()
        })?.shift(LEFT)?)?;
        Ok(())
    }
}
struct Serial {
    renderer: RetainedFrameRenderer,
    camera: Option<Camera>,
    frames: Vec<Vec<u8>>,
}
impl SceneSink for Serial {
    fn capture(&mut self, _: CaptureReason, packet: FramePacket) -> Result<(), IntegrationError> {
        let stage = packet.materialize_stage();
        if let Some(camera) = &self.camera {
            self.renderer.render_with_camera(&stage, camera).unwrap();
        } else {
            self.renderer.render(&stage, 0).unwrap();
        }
        let mut frame = FrameBuffer::new(FrameLayout::tight(PixelFormat::Rgba8, 48, 32).unwrap());
        rgba16f_to_rgba8(self.renderer.frame(), &mut frame).unwrap();
        self.frames.push(frame.as_bytes().to_vec());
        Ok(())
    }
}

#[test]
fn public_pure_stateful_pure_exports_match_serial_pixels_with_and_without_camera() {
    for camera in [false, true] {
        let mut first = None;
        for (window, threads) in [(1, 1), (2, 4), (4, 16)] {
            let fs = Arc::new(VirtualFs::new());
            let mut options = options("/frames", window, threads);
            if camera { options.camera = Some(options.camera_config().unwrap()); }
            let runtime = RuntimeConfig::from_config(&options.config);
            let seed = options.config.determinism.seed;
            let background = fmn_core::color::Srgb::from_hex(&options.config.camera.background_color)
                .unwrap().to_linear(options.config.camera.background_opacity);
            let aa = options.config.render.aa;
            let reference_camera = options.camera.clone().map(|config| Camera::new(config).unwrap());
            let report = render_with_fs(&mut Mixed, options, fs.clone()).unwrap();
            let stats = report.frame_pipeline.as_ref().unwrap();
            assert_eq!(report.artifact.frame_count, 10);
            assert_eq!(report.scene.play_count, 3);
            assert_eq!(stats.emitted, 10);
            assert_eq!(stats.outstanding_slots, 0);
            assert!(stats.max_in_flight <= report.execution_plan.frames_in_flight);
            let bytes: Vec<_> = (0..10).map(|i| fs.read(&Path::new("/frames")
                .join(format!("frame_{i:06}.png"))).unwrap()).collect();
            let pixels: Vec<_> = bytes.iter().map(|bytes| decode_png(bytes, &PngLimits::default())
                .unwrap().rgba).collect();
            let mut serial = Serial {
                renderer: RetainedFrameRenderer::new(RetainedFrameRendererConfig {
                    frame: FrameConfig::new(Viewport { width: 48, height: 32 },
                        ScreenMap { scale: 8.0, origin: [24.0, 16.0], y_up: true }, background)
                        .with_aa_policy(aa),
                    tiling: Tiling { macro_tile: report.execution_plan.macro_tile,
                        fine_tile: report.execution_plan.fine_tile },
                    engine: EngineIdentity::certified(), threads: 1,
                }).unwrap(),
                camera: reference_camera, frames: Vec::new(),
            };
            fmn::run_scene(&mut Mixed, runtime, seed, &mut serial).unwrap();
            assert_eq!(pixels, serial.frames);
            assert_ne!(pixels[0], pixels[3]);
            assert_ne!(pixels[3], pixels[5]);
            if let Some(first) = &first { assert_eq!(&bytes, first); } else { first = Some(bytes); }
        }
    }
}

#[test]
fn interrupted_pure_program_publishes_nothing_and_allows_same_path_retry() {
    for window in [1, 2, 4] {
        let fs = Arc::new(VirtualFs::new());
        let mut limited = options("/retry", window, 4);
        limited.max_frames = 2;
        assert!(matches!(render_with_fs(&mut Mixed, limited, fs.clone()),
            Err(RenderError::InvalidOptions("scene exceeded max_frames"))));
        assert!(!fs.exists(Path::new("/retry")));
        let report = render_with_fs(&mut Mixed, options("/retry", window, 4), fs.clone()).unwrap();
        assert_eq!(report.artifact.frame_count, 10);
        assert_eq!(report.frame_pipeline.unwrap().outstanding_slots, 0);
    }
}

#[test]
fn public_typesetting_wrapper_forwards_native_pure_programs() {
    #[derive(Default)]
    struct CountingPrograms { programs: usize, packets: usize, samples: i64 }
    impl SceneSink for CountingPrograms {
        fn accepts_pure_segments(&self) -> bool { true }
        fn capture_pure_segment(
            &mut self, segment: &fmn_anim::pure_frame::PureSegment,
        ) -> Result<(), IntegrationError> {
            self.programs += 1;
            self.samples += segment.frame_count();
            // Exercise the callback-free program rather than acknowledging
            // a capability that silently discards invalid jobs.
            for frame in 1..=segment.frame_count() {
                let _stage = segment.job(frame).unwrap().materialize();
            }
            Ok(())
        }
        fn capture(&mut self, _: CaptureReason, _: FramePacket) -> Result<(), IntegrationError> {
            self.packets += 1;
            Ok(())
        }
    }
    let mut sink = CountingPrograms::default();
    let completed = fmn::run_scene(&mut Mixed,
        RuntimeConfig { fps: 8, ..RuntimeConfig::default() }, 42, &mut sink).unwrap();
    assert_eq!(sink.programs, 2);
    assert_eq!(sink.samples, 8);
    assert_eq!(sink.packets, 2); // The updater-driven wait is still serial.
    assert_eq!(completed.report().play_count, 3);
    assert_eq!(completed.report().time.frames(), 10);
}
