//! Public text reveals exercise the native job route, not a test-only renderer.
use fmn::prelude::*;
use fmn_anim::{add_text_letter_by_letter, add_text_word_by_word};
use fmn_codec::{PngLimits, decode_png};
use fmn_config::config::{DeterminismMode, ThreadPolicy};
use fmn_frame::{FrameBuffer, FrameLayout, PixelFormat, convert::rgba16f_to_rgba8};
use fmn_platform::fs::{FileSystem, VirtualFs};
use fmn_render::{
    EngineIdentity, FrameConfig, RetainedFrameRenderer, RetainedFrameRendererConfig, ScreenMap,
    Tiling, Viewport,
};
use fmn_scene::{CaptureReason, IntegrationError, RuntimeConfig, SceneSink};
use std::{path::Path, sync::Arc};

struct RevealText {
    words: Vec<VMobject>,
    letters: VMobject,
}
impl RevealText {
    fn new() -> Self {
        let book = fmn::library::FontBook::bundled().unwrap();
        let words = ["Native", "words", "appear"]
            .iter()
            .enumerate()
            .map(|(i, text)| {
                fmn::library::Text::new(text)
                    .font_size(24.0)
                    .build(&book)
                    .unwrap()
                    .vmob
                    .shifted([-2.5 + i as f64 * 1.8, 0.6, 0.0])
            })
            .collect();
        let letters = fmn::library::Text::new("Letter by letter")
            .font_size(24.0)
            .build(&book)
            .unwrap()
            .vmob
            .shifted([-1.5, -0.7, 0.0]);
        Self { words, letters }
    }
}
impl SceneConstruct for RevealText {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        let mut words = Vec::new();
        for word in &self.words {
            words.push(stage.add(word.clone())?);
        }
        let letters = stage.add(self.letters.clone())?;
        let raw = stage.scene_mut().stage_mut();
        let group = raw.add(fmn::mobject::Mobject::vector_from_points(&[]));
        for word in words {
            raw.remove_from_scene(word);
            raw.attach(group, word)?;
        }
        raw.add_to_scene(group)?;
        let words = add_text_word_by_word(raw, group, 0.1, 1.0)?;
        let letters = add_text_letter_by_letter(raw, letters, 0.1, 1.0)?;
        let group = fmn_anim::AnimationGroup::with_lag_ratio(
            raw,
            vec![Box::new(words), Box::new(letters)],
            0.3,
        )?
        .with_run_time(1.0);
        stage.play_prepared(vec![Box::new(group)])?;
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
            let expected = job.time().to_f64();
            let actual = std::thread::spawn(move || job.materialize().time())
                .join()
                .unwrap();
            assert_eq!(actual.to_bits(), expected.to_bits());
        }
        Ok(())
    }
    fn capture(&mut self, _: CaptureReason, _: FramePacket) -> Result<(), IntegrationError> {
        panic!("a supported text reveal fell back to per-frame capture")
    }
}
#[test]
fn authored_text_reveals_are_native_jobs() {
    let mut jobs = Jobs::default();
    let completed = fmn::run_scene(
        &mut RevealText::new(),
        RuntimeConfig {
            fps: 8,
            ..RuntimeConfig::default()
        },
        41,
        &mut jobs,
    )
    .unwrap();
    assert_eq!(jobs.programs, 2);
    assert_eq!(jobs.frames, (1..=10).collect::<Vec<_>>());
    assert_eq!(completed.report().time.frames(), 10);
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
        let mut frame = FrameBuffer::new(FrameLayout::tight(PixelFormat::Rgba8, 96, 64).unwrap());
        rgba16f_to_rgba8(self.renderer.frame(), &mut frame).unwrap();
        self.frames.push(frame.as_bytes().to_vec());
        Ok(())
    }
}
#[test]
fn word_and_letter_pngs_equal_serial_pixels_and_each_other_across_threads() {
    let mut baseline = None;
    for (window, threads) in [(1, 1), (2, 4), (4, 16)] {
        let fs = Arc::new(VirtualFs::new());
        let mut options = RenderOptions::new("/reveals").unwrap();
        options.config.camera.resolution = (96, 64);
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
        let report = render_with_fs(&mut RevealText::new(), options, fs.clone()).unwrap();
        let stats = report.frame_pipeline.as_ref().unwrap();
        assert_eq!(
            (stats.submitted, stats.emitted, stats.outstanding_slots),
            (10, 10, 0)
        );
        assert!(stats.max_in_flight <= report.execution_plan.frames_in_flight);
        let bytes = (0..10)
            .map(|i| {
                fs.read(&Path::new("/reveals").join(format!("frame_{i:06}.png")))
                    .unwrap()
            })
            .collect::<Vec<_>>();
        let mut serial = Serial {
            renderer: RetainedFrameRenderer::new(RetainedFrameRendererConfig {
                frame: FrameConfig::new(
                    Viewport {
                        width: 96,
                        height: 64,
                    },
                    ScreenMap {
                        scale: 16.0,
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
        fmn::run_scene(&mut RevealText::new(), runtime, seed, &mut serial).unwrap();
        assert_eq!(serial.frames.len(), 10);
        assert!(serial.frames.windows(2).any(|pair| pair[0] != pair[1]));
        for (i, (png, expected)) in bytes.iter().zip(&serial.frames).enumerate() {
            let actual = decode_png(png, &PngLimits::default()).unwrap().rgba;
            assert_eq!(actual.len(), expected.len());
            if let Some(byte) = actual.iter().zip(expected).position(|(a, b)| a != b) {
                panic!(
                    "frame {i}, byte {byte}, workers {threads}: {} != {}",
                    actual[byte], expected[byte]
                );
            }
        }
        assert_eq!(bytes[7], bytes[8]);
        assert_eq!(bytes[8], bytes[9]);
        if let Some(baseline) = &baseline {
            assert_eq!(&bytes, baseline);
        } else {
            baseline = Some(bytes);
        }
    }
}
