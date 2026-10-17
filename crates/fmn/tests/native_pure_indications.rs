//! Text transitions and indications run through production native frame jobs.
use std::path::Path;
use std::sync::Arc;

use fmn::prelude::*;
use fmn_anim::AnimationGroup;
use fmn_codec::{PngLimits, decode_png};
use fmn_config::config::{DeterminismMode, ThreadPolicy};
use fmn_frame::convert::rgba16f_to_rgba8;
use fmn_frame::{FrameBuffer, FrameLayout, PixelFormat};
use fmn_platform::fs::{FileSystem, VirtualFs};
use fmn_render::{
    EngineIdentity, FrameConfig, RetainedFrameRenderer, RetainedFrameRendererConfig, ScreenMap,
    Tiling, Viewport,
};
use fmn_scene::{CaptureReason, IntegrationError, RuntimeConfig, SceneSink};

struct TransitionScene;
impl SceneConstruct for TransitionScene {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        let book = fmn::library::FontBook::bundled().unwrap();
        let text = |s| {
            fmn::library::Text::new(s)
                .font_size(32.0)
                .build(&book)
                .unwrap()
                .vmob
        };
        let source = stage.add(text("Start"))?;
        let target = stage.add(text("Middle").shifted([0.7, 0.2, 0.0]))?;
        let final_target = stage.add(text("Finish").shifted([-0.4, -0.2, 0.0]))?;
        let config = AnimConfig {
            run_time: 0.5,
            rate_func: RateFunc::linear(),
            ..AnimConfig::default()
        };
        let raw = stage.scene_mut().stage_mut();
        raw.remove_from_scene(target);
        raw.remove_from_scene(final_target);
        let fade = fmn_anim::fade_transform(raw, source, target)?.with_config(config.clone());
        let nested = AnimationGroup::new(raw, vec![Box::new(fade)])?;
        stage.play_prepared(vec![Box::new(nested)])?;
        let raw = stage.scene_mut().stage_mut();
        assert!(raw.roots().contains(&target));
        assert!(!raw.roots().contains(&source));
        let pieces =
            fmn_anim::fade_transform_pieces(raw, target, final_target)?.with_config(config.clone());
        stage.play_prepared(vec![Box::new(pieces)])?;
        assert!(
            stage
                .scene_mut()
                .stage_mut()
                .roots()
                .contains(&final_target)
        );
        stage.play_prepared(vec![Box::new(
            fmn_anim::WiggleOutThenIn::new(final_target)
                .with_scale_value(1.25)
                .with_rotation_angle(0.4)
                .with_config(config.clone()),
        )])?;
        stage.play_prepared(vec![Box::new(
            fmn_anim::VShowPassingFlash::new(final_target)
                .with_time_width(0.7)
                .with_config(config),
        )])?;
        stage.wait(0.25)?;
        Ok(())
    }
}

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
            let expected = job.time().to_f64().to_bits();
            let actual = std::thread::spawn(move || job.materialize().time().to_bits())
                .join()
                .unwrap();
            assert_eq!(actual, expected);
        }
        Ok(())
    }
    fn capture(&mut self, _: CaptureReason, _: FramePacket) -> Result<(), IntegrationError> {
        panic!("a supported eager composition fell back to serial capture")
    }
}

#[test]
fn facade_delivers_fades_wiggles_and_flashes_as_native_jobs() {
    let mut jobs = Jobs::default();
    let completed = fmn::run_scene(
        &mut TransitionScene,
        RuntimeConfig {
            fps: 8,
            ..RuntimeConfig::default()
        },
        42,
        &mut jobs,
    )
    .unwrap();
    assert_eq!(jobs.programs, 5);
    assert_eq!(jobs.frames, (1..=18).collect::<Vec<_>>());
    assert_eq!(completed.report().play_count, 5);
    assert_eq!(completed.report().time.frames(), 18);
}

struct Serial {
    renderer: RetainedFrameRenderer,
    frames: Vec<Vec<u8>>,
}
impl SceneSink for Serial {
    fn capture(&mut self, _: CaptureReason, packet: FramePacket) -> Result<(), IntegrationError> {
        self.renderer
            .render(&packet.materialize_stage(), 0)
            .unwrap();
        let mut frame = FrameBuffer::new(FrameLayout::tight(PixelFormat::Rgba8, 48, 32).unwrap());
        rgba16f_to_rgba8(self.renderer.frame(), &mut frame).unwrap();
        self.frames.push(frame.as_bytes().to_vec());
        Ok(())
    }
}

#[test]
fn text_transitions_equal_serial_pixels_with_bounded_ordered_native_output() {
    let mut baseline = None;
    for (window, threads) in [(1, 1), (2, 4), (4, 16)] {
        let fs = Arc::new(VirtualFs::new());
        let mut options = RenderOptions::new("/transitions").unwrap();
        options.config.camera.resolution = (48, 32);
        options.config.camera.fps = 8;
        options.config.sizes.frame_height = 4.0;
        options.config.determinism.mode = DeterminismMode::Certified;
        options.config.render.threads = ThreadPolicy::Fixed(threads);
        options.frames_in_flight = window;
        let runtime = RuntimeConfig::from_config(&options.config);
        let seed = options.config.determinism.seed;
        let background = fmn_core::color::Srgb::from_hex(&options.config.camera.background_color)
            .unwrap()
            .to_linear(options.config.camera.background_opacity);
        let aa = options.config.render.aa;
        let report = render_with_fs(&mut TransitionScene, options, fs.clone()).unwrap();
        assert_eq!(report.artifact.frame_count, 18);
        let stats = report.frame_pipeline.as_ref().unwrap();
        assert_eq!(
            (stats.submitted, stats.emitted, stats.outstanding_slots),
            (18, 18, 0)
        );
        assert!(stats.max_in_flight <= report.execution_plan.frames_in_flight);
        assert!(stats.render_team_frames.iter().all(|frames| *frames > 0));
        let bytes = (0..18)
            .map(|index| {
                fs.read(&Path::new("/transitions").join(format!("frame_{index:06}.png")))
                    .unwrap()
            })
            .collect::<Vec<_>>();
        let first = decode_png(&bytes[0], &PngLimits::default()).unwrap();
        let last = decode_png(&bytes[7], &PngLimits::default()).unwrap();
        assert_ne!(first.rgba, last.rgba);
        assert_eq!(bytes[16], bytes[17]);
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
            frames: Vec::new(),
        };
        fmn::run_scene(&mut TransitionScene, runtime, seed, &mut serial).unwrap();
        assert_eq!(serial.frames.len(), bytes.len());
        for (index, (png, expected)) in bytes.iter().zip(&serial.frames).enumerate() {
            let actual = decode_png(png, &PngLimits::default()).unwrap().rgba;
            assert_eq!(actual.len(), expected.len());
            if let Some(byte) = actual.iter().zip(expected).position(|(a, b)| a != b) {
                panic!(
                    "frame {index}, byte {byte}: {} != {}; window {window}, threads {threads}",
                    actual[byte], expected[byte]
                );
            }
        }
        if let Some(baseline) = &baseline {
            assert_eq!(&bytes, baseline);
        } else {
            baseline = Some(bytes);
        }
    }
}
