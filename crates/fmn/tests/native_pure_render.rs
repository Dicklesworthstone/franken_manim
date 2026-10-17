//! Public scene-to-artifact integration, mixed with stateful segments.
use fmn::prelude::*;
use fmn::rendering::RenderError;
use fmn_codec::{PngLimits, decode_png};
use fmn_config::config::{DeterminismMode, ThreadPolicy};
use fmn_frame::convert::rgba16f_to_rgba8;
use fmn_frame::{FrameBuffer, FrameLayout, PixelFormat};
use fmn_platform::fs::{FileSystem, VirtualFs};
use fmn_render::{
    Camera, EngineIdentity, FrameConfig, RetainedFrameRenderer, RetainedFrameRendererConfig,
    ScreenMap, Tiling, Viewport,
};
use fmn_scene::{CaptureReason, IntegrationError, RuntimeConfig, SceneSink};
use std::path::Path;
use std::sync::Arc;

fn assert_pixels(
    actual: &[Vec<u8>],
    expected: &[Vec<u8>],
    window: usize,
    threads: u32,
    camera: bool,
) {
    assert_eq!(actual.len(), expected.len());
    for (frame, (actual, expected)) in actual.iter().zip(expected).enumerate() {
        assert_eq!(actual.len(), expected.len());
        if let Some(byte) = actual.iter().zip(expected).position(|(a, b)| a != b) {
            let changed = actual.iter().zip(expected).filter(|(a, b)| a != b).count();
            panic!(
                "frame {frame}, byte {byte}: {} != {}; {changed} differing bytes; window {window}, threads {threads}, camera {camera}",
                actual[byte], expected[byte]
            );
        }
    }
}

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
        stage.play(
            mob.animate()
                .set_anim_args(AnimateArgs {
                    run_time: Some(0.5),
                    path_arc: Some(0.8),
                    ..AnimateArgs::default()
                })?
                .shift(RIGHT)?,
        )?;
        let updater = stage.scene_mut().stage_mut().add_dt_updater(
            mob,
            |stage, mob, dt| {
                stage.shift(mob, [0.0, dt, 0.0]);
            },
            false,
        )?;
        stage.wait(0.25)?;
        stage.scene_mut().stage_mut().remove_updater(mob, updater);
        stage.play(
            mob.animate()
                .set_anim_args(AnimateArgs {
                    run_time: Some(0.5),
                    ..AnimateArgs::default()
                })?
                .shift(LEFT)?,
        )?;
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
            if camera {
                options.camera = Some(options.camera_config().unwrap());
            }
            let runtime = RuntimeConfig::from_config(&options.config);
            let seed = options.config.determinism.seed;
            let background =
                fmn_core::color::Srgb::from_hex(&options.config.camera.background_color)
                    .unwrap()
                    .to_linear(options.config.camera.background_opacity);
            let aa = options.config.render.aa;
            let reference_camera = options
                .camera
                .clone()
                .map(|config| Camera::new(config).unwrap());
            let report = render_with_fs(&mut Mixed, options, fs.clone()).unwrap();
            let stats = report.frame_pipeline.as_ref().unwrap();
            assert_eq!(report.artifact.frame_count, 10);
            assert_eq!(report.scene.play_count, 3);
            assert_eq!(stats.emitted, 10);
            assert_eq!(stats.outstanding_slots, 0);
            assert!(stats.max_in_flight <= report.execution_plan.frames_in_flight);
            let bytes: Vec<_> = (0..10)
                .map(|i| {
                    fs.read(&Path::new("/frames").join(format!("frame_{i:06}.png")))
                        .unwrap()
                })
                .collect();
            let pixels: Vec<_> = bytes
                .iter()
                .map(|bytes| decode_png(bytes, &PngLimits::default()).unwrap().rgba)
                .collect();
            let mut serial = Serial {
                renderer: RetainedFrameRenderer::new(RetainedFrameRendererConfig {
                    frame: FrameConfig::new(
                        Viewport {
                            width: 48,
                            height: 32,
                        },
                        ScreenMap {
                            scale: 8.0,
                            origin: [24.0, 16.0],
                            y_up: true,
                        },
                        background,
                    )
                    .with_aa_policy(aa),
                    tiling: Tiling {
                        macro_tile: report.execution_plan.macro_tile,
                        fine_tile: report.execution_plan.fine_tile,
                    },
                    engine: EngineIdentity::certified(),
                    threads: 1,
                })
                .unwrap(),
                camera: reference_camera,
                frames: Vec::new(),
            };
            fmn::run_scene(&mut Mixed, runtime, seed, &mut serial).unwrap();
            assert_pixels(&pixels, &serial.frames, window, threads, camera);
            assert_ne!(pixels[0], pixels[3]);
            assert_ne!(pixels[3], pixels[5]);
            if let Some(first) = &first {
                assert_eq!(&bytes, first);
            } else {
                first = Some(bytes);
            }
        }
    }
}

#[test]
fn interrupted_pure_program_publishes_nothing_and_allows_same_path_retry() {
    for window in [1, 2, 4] {
        let fs = Arc::new(VirtualFs::new());
        let mut limited = options("/retry", window, 4);
        limited.max_frames = 2;
        assert!(matches!(
            render_with_fs(&mut Mixed, limited, fs.clone()),
            Err(RenderError::InvalidOptions("scene exceeded max_frames"))
        ));
        assert!(!fs.exists(Path::new("/retry")));
        let report = render_with_fs(&mut Mixed, options("/retry", window, 4), fs.clone()).unwrap();
        assert_eq!(report.artifact.frame_count, 10);
        assert_eq!(report.frame_pipeline.unwrap().outstanding_slots, 0);
    }
}

#[test]
fn public_typesetting_wrapper_forwards_native_pure_programs() {
    #[derive(Default)]
    struct CountingPrograms {
        programs: usize,
        packets: usize,
        samples: i64,
    }
    impl SceneSink for CountingPrograms {
        fn accepts_pure_segments(&self) -> bool {
            true
        }
        fn capture_pure_segment(
            &mut self,
            segment: &fmn_anim::pure_frame::PureSegment,
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
    let completed = fmn::run_scene(
        &mut Mixed,
        RuntimeConfig {
            fps: 8,
            ..RuntimeConfig::default()
        },
        42,
        &mut sink,
    )
    .unwrap();
    assert_eq!(sink.programs, 2);
    assert_eq!(sink.samples, 8);
    assert_eq!(sink.packets, 2); // The updater-driven wait is still serial.
    assert_eq!(completed.report().play_count, 3);
    assert_eq!(completed.report().time.frames(), 10);
}

struct DrawFadeRotate;
impl SceneConstruct for DrawFadeRotate {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        let mob = stage.add(Circle::new().radius(0.4).color(BLUE))?;
        stage.set_fill(mob, Some(BLUE), Some(0.8), None, true);
        let config = AnimConfig {
            run_time: 0.5,
            rate_func: RateFunc::linear(),
            ..AnimConfig::default()
        };
        stage.play_prepared(vec![Box::new(
            fmn_anim::show_creation(mob).with_config(config.clone()),
        )])?;
        stage.wait(0.25)?;
        stage.play_prepared(vec![Box::new(fmn_anim::v_fade_out(mob).with_config(
            AnimConfig {
                remover: true,
                ..config.clone()
            },
        ))])?;
        let rotor = stage.add(Circle::new().radius(0.4).color(BLUE))?;
        stage.set_fill(rotor, Some(BLUE), Some(0.8), None, true);
        stage.play_prepared(vec![Box::new(
            fmn_anim::Rotating::new(rotor)
                .with_angle(1.2)
                .with_about_point([0.7, 0.0, 0.0])
                .with_config(config),
        )])?;
        Ok(())
    }
}

#[test]
fn public_drawing_fading_and_rotation_admit_every_nominal_job() {
    #[derive(Default)]
    struct Jobs(Vec<i64>);
    impl SceneSink for Jobs {
        fn accepts_pure_segments(&self) -> bool {
            true
        }
        fn capture_pure_segment(
            &mut self,
            segment: &fmn_anim::pure_frame::PureSegment,
        ) -> Result<(), IntegrationError> {
            for frame in 1..=segment.frame_count() {
                let job = segment.job(frame).unwrap();
                self.0.push(job.time().frames());
                let time = job.time().to_f64();
                // No owner or animation value is borrowed while this frame runs.
                let observed = std::thread::spawn(move || job.materialize().time())
                    .join()
                    .unwrap();
                assert_eq!(observed.to_bits(), time.to_bits());
            }
            Ok(())
        }
        fn capture(&mut self, _: CaptureReason, _: FramePacket) -> Result<(), IntegrationError> {
            panic!("a supported pure mechanism unexpectedly used serial capture")
        }
    }
    let mut jobs = Jobs::default();
    let completed = fmn::run_scene(
        &mut DrawFadeRotate,
        RuntimeConfig {
            fps: 8,
            ..RuntimeConfig::default()
        },
        42,
        &mut jobs,
    )
    .unwrap();
    assert_eq!(jobs.0, (1..=14).collect::<Vec<_>>());
    assert_eq!(completed.report().play_count, 4);
    assert_eq!(completed.report().time.frames(), 14);
}

#[test]
fn public_drawing_fading_and_rotation_match_serial_pngs_across_worker_counts() {
    let mut previous = None;
    for (window, threads) in [(1, 1), (2, 4), (4, 16)] {
        let fs = Arc::new(VirtualFs::new());
        let options = options("/mechanisms", window, threads);
        let runtime = RuntimeConfig::from_config(&options.config);
        let seed = options.config.determinism.seed;
        let background = fmn_core::color::Srgb::from_hex(&options.config.camera.background_color)
            .unwrap()
            .to_linear(options.config.camera.background_opacity);
        let aa = options.config.render.aa;
        let report = render_with_fs(&mut DrawFadeRotate, options, fs.clone()).unwrap();
        let stats = report.frame_pipeline.as_ref().unwrap();
        assert_eq!(
            (stats.submitted, stats.emitted, stats.outstanding_slots),
            (14, 14, 0)
        );
        assert!(stats.max_in_flight <= report.execution_plan.frames_in_flight);
        assert!(stats.render_team_frames.iter().all(|count| *count > 0));
        let bytes = (0..14)
            .map(|i| {
                fs.read(&Path::new("/mechanisms").join(format!("frame_{i:06}.png")))
                    .unwrap()
            })
            .collect::<Vec<_>>();
        let pixels = bytes
            .iter()
            .map(|bytes| decode_png(bytes, &PngLimits::default()).unwrap().rgba)
            .collect::<Vec<_>>();
        let mut serial = Serial {
            renderer: RetainedFrameRenderer::new(RetainedFrameRendererConfig {
                frame: FrameConfig::new(
                    Viewport {
                        width: 48,
                        height: 32,
                    },
                    ScreenMap {
                        scale: 8.0,
                        origin: [24.0, 16.0],
                        y_up: true,
                    },
                    background,
                )
                .with_aa_policy(aa),
                tiling: Tiling {
                    macro_tile: report.execution_plan.macro_tile,
                    fine_tile: report.execution_plan.fine_tile,
                },
                engine: EngineIdentity::certified(),
                threads: 1,
            })
            .unwrap(),
            camera: None,
            frames: Vec::new(),
        };
        fmn::run_scene(&mut DrawFadeRotate, runtime, seed, &mut serial).unwrap();
        assert_pixels(&pixels, &serial.frames, window, threads, false);
        assert_eq!(pixels[3], pixels[4]);
        assert_eq!(pixels[4], pixels[5]);
        for start in [0, 6, 10] {
            assert_ne!(
                pixels[start],
                pixels[start + 3],
                "mechanism at frame {start}"
            );
        }
        if let Some(previous) = &previous {
            assert_eq!(&bytes, previous);
        } else {
            previous = Some(bytes);
        }
    }
}

/// Real Scribe provenance feeds both native matchers. The original targets
/// remain detached until the owner completes each replacement lifecycle.
struct MatchingText;
impl SceneConstruct for MatchingText {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        let source = Tex::new(r"a^2+b^2=c^2")
            .font_size(36.0)
            .build(stage.tex_engine()?)?;
        let target = Tex::new(r"c^2=a^2+b^2")
            .font_size(36.0)
            .build(stage.tex_engine()?)?;
        let (source_spans, target_spans) = (source.span_map(), target.span_map());
        let source = stage.add(source)?;
        let target = stage.arena_mut().add(target);
        stage.move_to(source, [-0.4, 0.8, 0.0], ORIGIN);
        stage.move_to(target, [0.3, 0.8, 0.0], ORIGIN);
        let config = fmn::library::TextMatchingConfig {
            run_time: 0.5,
            lag_ratio: 0.2,
            ..fmn::library::TextMatchingConfig::default()
        };
        let animation = fmn::library::TransformMatchingTex::with_config(
            stage.arena_mut(),
            source,
            target,
            &source_spans,
            &target_spans,
            config.clone(),
        )?;
        stage.play(animation)?;
        assert_eq!(stage.roots(), &[target]);

        let book = fmn::library::FontBook::bundled().unwrap();
        let before = Text::new("AB CD")
            .font("IBM Plex Sans")
            .font_size(32.0)
            .build(&book)?;
        let after = Text::new("CD AB!")
            .font("IBM Plex Sans")
            .font_size(32.0)
            .build(&book)?;
        let (before_spans, after_spans) = (before.span_map(), after.span_map());
        let before = stage.add(before)?;
        let after = stage.arena_mut().add(after);
        stage.move_to(before, [-0.4, -0.8, 0.0], ORIGIN);
        stage.move_to(after, [0.3, -0.8, 0.0], ORIGIN);
        let animation = fmn::library::TransformMatchingStrings::with_config(
            stage.arena_mut(),
            before,
            after,
            &before_spans,
            &after_spans,
            config,
        )?;
        stage.play(animation)?;
        assert_eq!(stage.roots(), &[target, after]);
        Ok(())
    }
}

#[test]
fn public_text_matching_offloads_every_frame_and_keeps_owner_cleanup() {
    #[derive(Default)]
    struct Jobs {
        programs: usize,
        frames: Vec<i64>,
    }
    impl SceneSink for Jobs {
        fn accepts_pure_segments(&self) -> bool {
            true
        }
        fn capture_pure_segment(
            &mut self,
            segment: &fmn_anim::pure_frame::PureSegment,
        ) -> Result<(), IntegrationError> {
            self.programs += 1;
            for index in 1..=segment.frame_count() {
                let job = segment.job(index).unwrap();
                self.frames.push(job.time().frames());
                let expected = job.time().to_f64();
                let actual = std::thread::spawn(move || job.materialize().time())
                    .join()
                    .unwrap();
                assert_eq!(actual.to_bits(), expected.to_bits());
            }
            Ok(())
        }
        fn capture(&mut self, _: CaptureReason, _: FramePacket) -> Result<(), IntegrationError> {
            panic!("native text matching unexpectedly fell back to serial capture")
        }
    }
    let mut jobs = Jobs::default();
    let completed = fmn::run_scene(
        &mut MatchingText,
        RuntimeConfig {
            fps: 8,
            ..RuntimeConfig::default()
        },
        42,
        &mut jobs,
    )
    .unwrap();
    assert_eq!(jobs.programs, 2);
    assert_eq!(jobs.frames, (1..=8).collect::<Vec<_>>());
    assert_eq!(completed.report().play_count, 2);
    assert_eq!(completed.report().time.frames(), 8);
}

#[test]
fn public_text_matching_pngs_match_serial_scribe_and_lumen_across_workers() {
    let mut previous = None;
    for (window, threads) in [(1, 1), (2, 4), (4, 16)] {
        let fs = Arc::new(VirtualFs::new());
        let options = options("/matching", window, threads);
        let runtime = RuntimeConfig::from_config(&options.config);
        let seed = options.config.determinism.seed;
        let background = fmn_core::color::Srgb::from_hex(&options.config.camera.background_color)
            .unwrap()
            .to_linear(options.config.camera.background_opacity);
        let aa = options.config.render.aa;
        let report = render_with_fs(&mut MatchingText, options, fs.clone()).unwrap();
        let stats = report.frame_pipeline.as_ref().unwrap();
        assert_eq!(report.artifact.frame_count, 8);
        assert_eq!(
            (stats.submitted, stats.emitted, stats.outstanding_slots),
            (8, 8, 0)
        );
        assert!(stats.render_team_frames.iter().all(|count| *count > 0));
        let bytes = (0..8)
            .map(|i| {
                fs.read(&Path::new("/matching").join(format!("frame_{i:06}.png")))
                    .unwrap()
            })
            .collect::<Vec<_>>();
        let pixels = bytes
            .iter()
            .map(|bytes| decode_png(bytes, &PngLimits::default()).unwrap().rgba)
            .collect::<Vec<_>>();
        let mut serial = Serial {
            renderer: RetainedFrameRenderer::new(RetainedFrameRendererConfig {
                frame: FrameConfig::new(
                    Viewport {
                        width: 48,
                        height: 32,
                    },
                    ScreenMap {
                        scale: 8.0,
                        origin: [24.0, 16.0],
                        y_up: true,
                    },
                    background,
                )
                .with_aa_policy(aa),
                tiling: Tiling {
                    macro_tile: report.execution_plan.macro_tile,
                    fine_tile: report.execution_plan.fine_tile,
                },
                engine: EngineIdentity::certified(),
                threads: 1,
            })
            .unwrap(),
            camera: None,
            frames: Vec::new(),
        };
        fmn::run_scene(&mut MatchingText, runtime, seed, &mut serial).unwrap();
        assert_pixels(&pixels, &serial.frames, window, threads, false);
        assert_ne!(
            pixels[0], pixels[3],
            "the equation transition must visibly move"
        );
        assert_ne!(
            pixels[4], pixels[7],
            "the string transition must visibly move"
        );
        if let Some(previous) = &previous {
            assert_eq!(&bytes, previous);
        } else {
            previous = Some(bytes);
        }
    }
}
