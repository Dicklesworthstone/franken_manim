//! The public Rust data animation uses ordinary capture and native frame jobs.

#[allow(dead_code)]
#[path = "../examples/live_bar_chart.rs"]
mod example;

use std::path::Path;
use std::sync::Arc;

use fmn::prelude::*;
use fmn_anim::pure_frame::PureSegment;
use fmn_codec::{PngLimits, decode_png};
use fmn_config::config::{DeterminismMode, ThreadPolicy};
use fmn_frame::convert::rgba16f_to_rgba8;
use fmn_frame::{FrameBuffer, FrameLayout, PixelFormat};
use fmn_platform::fs::{FileSystem, VirtualFs};
use fmn_render::{
    EngineIdentity, FrameConfig, RetainedFrameRenderer, RetainedFrameRendererConfig, ScreenMap,
    Tiling, Viewport,
};
use fmn_scene::{CaptureReason, IntegrationError, SceneSink};

#[derive(Default)]
struct Captures {
    parallel: bool,
    programs: usize,
    frames: Vec<Vec<u8>>,
}

impl SceneSink for Captures {
    fn capture(&mut self, _: CaptureReason, packet: FramePacket) -> Result<(), IntegrationError> {
        self.frames.push(
            packet
                .materialize_stage()
                .snapshot()
                .to_render_bytes()
                .unwrap(),
        );
        Ok(())
    }

    fn accepts_pure_segments(&self) -> bool {
        self.parallel
    }

    fn capture_pure_segment(&mut self, segment: &PureSegment) -> Result<(), IntegrationError> {
        self.programs += 1;
        let program = segment.clone();
        let mut frames = std::thread::spawn(move || {
            (1..=program.frame_count())
                .rev()
                .map(|index| {
                    let job = program.job(index).unwrap();
                    (
                        index,
                        job.materialize().snapshot().to_render_bytes().unwrap(),
                    )
                })
                .collect::<Vec<_>>()
        })
        .join()
        .unwrap();
        frames.sort_by_key(|(index, _)| *index);
        self.frames
            .extend(frames.into_iter().map(|(_, bytes)| bytes));
        Ok(())
    }
}

#[test]
fn live_chart_animations_freeze_without_changing_serial_captures_or_owner_cleanup() {
    let mut outcomes = Vec::new();
    for parallel in [false, true] {
        let mut sink = Captures {
            parallel,
            ..Captures::default()
        };
        let completed = fmn::run_scene(
            &mut example::LiveBarChart,
            RuntimeConfig {
                fps: 8,
                ..RuntimeConfig::default()
            },
            17,
            &mut sink,
        )
        .unwrap();
        assert_eq!(sink.programs, if parallel { 5 } else { 0 });
        assert_eq!(sink.frames.len(), 14);
        assert_eq!(completed.report().play_count, 5);
        assert_eq!(completed.report().time.frames(), 14);
        outcomes.push(sink.frames);
    }
    assert_eq!(outcomes[0], outcomes[1]);
    assert_ne!(outcomes[0][1], outcomes[0][5]);
    assert_ne!(outcomes[0][7], outcomes[0][11]);
    assert_ne!(outcomes[0][11], outcomes[0][12]);
}

struct SerialPixels {
    renderer: RetainedFrameRenderer,
    frames: Vec<Vec<u8>>,
}

impl SceneSink for SerialPixels {
    fn capture(&mut self, _: CaptureReason, packet: FramePacket) -> Result<(), IntegrationError> {
        self.renderer
            .render(&packet.materialize_stage(), 0)
            .unwrap();
        let mut frame = FrameBuffer::new(FrameLayout::tight(PixelFormat::Rgba8, 96, 64).unwrap());
        rgba16f_to_rgba8(self.renderer.frame(), &mut frame).unwrap();
        self.frames.push(frame.as_bytes().to_vec());
        Ok(())
    }
}

#[test]
fn live_chart_png_pipeline_matches_serial_pixels_at_one_four_and_sixteen_threads() {
    let mut baseline = None;
    for (window, threads) in [(1, 1), (2, 4), (4, 16)] {
        let fs = Arc::new(VirtualFs::new());
        let mut options = RenderOptions::new("/live-bars").unwrap();
        options.config.camera.resolution = (96, 64);
        options.config.camera.fps = 8;
        options.config.sizes.frame_height = 6.0;
        options.config.determinism.mode = DeterminismMode::Certified;
        options.config.render.threads = ThreadPolicy::Fixed(threads);
        options.frames_in_flight = window;
        options.typeset_cache = false;
        let runtime = RuntimeConfig::from_config(&options.config);
        let seed = options.config.determinism.seed;
        let background = fmn_core::color::Srgb::from_hex(&options.config.camera.background_color)
            .unwrap()
            .to_linear(options.config.camera.background_opacity);
        let aa = options.config.render.aa;
        let report = render_with_fs(&mut example::LiveBarChart, options, fs.clone()).unwrap();
        let pipeline = report.frame_pipeline.as_ref().unwrap();
        assert_eq!(report.artifact.frame_count, 14);
        assert_eq!(
            (
                pipeline.submitted,
                pipeline.emitted,
                pipeline.outstanding_slots
            ),
            (14, 14, 0)
        );
        assert!(pipeline.max_in_flight <= report.execution_plan.frames_in_flight);
        let frames = (0..14)
            .map(|index| {
                fs.read(&Path::new("/live-bars").join(format!("frame_{index:06}.png")))
                    .unwrap()
            })
            .collect::<Vec<_>>();
        let mut serial = SerialPixels {
            renderer: RetainedFrameRenderer::new(RetainedFrameRendererConfig {
                frame: FrameConfig::new(
                    Viewport {
                        width: 96,
                        height: 64,
                    },
                    ScreenMap {
                        scale: 64.0 / 6.0,
                        origin: [48.0, 32.0],
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
        fmn::run_scene(&mut example::LiveBarChart, runtime, seed, &mut serial).unwrap();
        assert_eq!(serial.frames.len(), frames.len());
        for (index, (png, expected)) in frames.iter().zip(&serial.frames).enumerate() {
            let actual = decode_png(png, &PngLimits::default()).unwrap().rgba;
            assert_eq!(&actual, expected, "frame {index} at {threads} threads");
        }
        assert_ne!(frames[1], frames[5]);
        assert_ne!(frames[7], frames[11]);
        assert_eq!(frames[12], frames[13]);
        if let Some(expected) = &baseline {
            assert_eq!(&frames, expected);
        } else {
            baseline = Some(frames);
        }
    }
}
