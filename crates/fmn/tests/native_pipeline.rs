//! Public camera export must use the bounded pipeline without revising pixels.

use fmn::prelude::*;
use fmn_anim::FramePacket;
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

fn options(path: &str, format: RenderFormat, window: usize, threads: u32) -> RenderOptions {
    let mut options = RenderOptions::new(path).unwrap();
    options.format = format;
    options.config.camera.resolution = (48, 32);
    options.config.camera.fps = 10;
    options.config.sizes.frame_height = 4.0;
    options.config.determinism.mode = DeterminismMode::Certified;
    options.config.render.threads = ThreadPolicy::Fixed(threads);
    options.frames_in_flight = window;
    let mut camera = options.camera_config().unwrap();
    camera
        .frame
        .set_euler_angles(Some(0.4), Some(0.7), None)
        .unwrap();
    options.camera = Some(camera);
    options
}

struct Moving;
impl SceneConstruct for Moving {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        let cube = stage.add(Cube::new(1.1).color(BLUE))?;
        let circle = stage.add(Circle::new().radius(0.3).color(WHITE))?;
        stage.set_fill(circle, Some(WHITE), Some(1.0), None, true);
        stage.shift(circle, [-1.5, 0.0, 0.0]);
        stage.play(
            cube.animate()
                .set_anim_args(AnimateArgs {
                    run_time: Some(0.4),
                    rate_func: Some(fmn::core::rate::linear),
                    ..AnimateArgs::default()
                })?
                .shift(RIGHT)?,
        )?;
        Ok(())
    }
}

struct Static;
impl SceneConstruct for Static {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        stage.add(Cube::new(1.1).color(BLUE))?;
        Ok(())
    }
}

struct SerialReference {
    camera: Camera,
    renderer: RetainedFrameRenderer,
    frames: Vec<Vec<u8>>,
}
impl SceneSink for SerialReference {
    fn capture(&mut self, _: CaptureReason, packet: FramePacket) -> Result<(), IntegrationError> {
        // Deliberately bypass FrameStream and PreparedCameraFrame's owned path.
        self.renderer
            .render_with_camera(&packet.materialize_stage(), &self.camera)
            .unwrap();
        let mut output = FrameBuffer::new(FrameLayout::tight(PixelFormat::Rgba8, 48, 32).unwrap());
        rgba16f_to_rgba8(self.renderer.frame(), &mut output).unwrap();
        self.frames.push(output.as_bytes().to_vec());
        Ok(())
    }
}

#[test]
fn queued_camera_pixels_equal_serial_capture_at_every_window() {
    let mut expected: Option<Vec<Vec<u8>>> = None;
    for (window, threads) in [(1, 1), (2, 2), (4, 4)] {
        let fs = Arc::new(VirtualFs::new());
        let options = options("/frames", RenderFormat::PngSequence, window, threads);
        let camera = Camera::new(options.camera.clone().unwrap()).unwrap();
        let runtime = RuntimeConfig::from_config(&options.config);
        let seed = options.config.determinism.seed;
        let report = render_with_fs(&mut Moving, options, fs.clone()).unwrap();
        let stats = report
            .frame_pipeline
            .as_ref()
            .expect("camera pipeline was bypassed");
        // Exact f64 0.4 is above 2/5, so four samples do not cover it.
        assert_eq!(report.artifact.frame_count, 5);
        assert_eq!(stats.submitted, 5);
        assert_eq!(stats.emitted, 5);
        assert_eq!(stats.outstanding_slots, 0);
        assert!(stats.max_in_flight <= report.execution_plan.frames_in_flight);
        assert!(report.execution_plan.frames_in_flight <= window);
        let pixels: Vec<Vec<u8>> = (0..5)
            .map(|sequence| {
                let bytes = fs
                    .read(&Path::new("/frames").join(format!("frame_{sequence:06}.png")))
                    .unwrap();
                decode_png(&bytes, &PngLimits::default()).unwrap().rgba
            })
            .collect();
        assert_ne!(pixels[0], pixels[4], "animation must move the visible cube");
        let mut reference = SerialReference {
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
                    camera.background(),
                ),
                tiling: Tiling {
                    macro_tile: report.execution_plan.macro_tile,
                    fine_tile: report.execution_plan.fine_tile,
                },
                engine: EngineIdentity::certified(),
                threads: 1,
            })
            .unwrap(),
            camera,
            frames: Vec::new(),
        };
        fmn::run_scene(&mut Moving, runtime, seed, &mut reference).unwrap();
        assert_eq!(pixels, reference.frames);
        if let Some(expected) = &expected {
            assert_eq!(&pixels, expected);
        } else {
            expected = Some(pixels);
        }
    }
}

#[test]
fn native_gif_and_y4m_are_window_independent() {
    for format in [RenderFormat::Gif, RenderFormat::Y4m] {
        let mut expected = None;
        for window in [1, 2, 4] {
            let fs = Arc::new(VirtualFs::new());
            let report = render_with_fs(
                &mut Moving,
                options("/movie", format, window, 4),
                fs.clone(),
            )
            .unwrap();
            assert_eq!(report.frame_pipeline.unwrap().emitted, 5);
            assert_eq!(report.artifact.frame_count, 5);
            let bytes = fs.read(Path::new("/movie")).unwrap();
            if let Some(expected) = &expected {
                assert_eq!(&bytes, expected);
            } else {
                expected = Some(bytes);
            }
        }
    }
}

#[test]
fn failed_scene_cancels_camera_workers_before_immediate_retry() {
    for window in [1, 2, 4] {
        let fs = Arc::new(VirtualFs::new());
        let mut limited = options("/retry", RenderFormat::PngSequence, window, 4);
        limited.max_frames = 1;
        assert!(matches!(
            render_with_fs(&mut Moving, limited, fs.clone()),
            Err(RenderError::InvalidOptions("scene exceeded max_frames"))
        ));
        assert!(!fs.exists(Path::new("/retry")));
        let report = render_with_fs(
            &mut Static,
            options("/retry", RenderFormat::PngSequence, window, 4),
            fs.clone(),
        )
        .unwrap();
        assert_eq!(report.frame_pipeline.unwrap().emitted, 1);
        assert_eq!(report.artifact.frame_count, 1);
    }
}

#[test]
fn unwinding_scene_joins_camera_pipeline_and_aborts_artifact() {
    struct Panicking;
    impl SceneConstruct for Panicking {
        fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
            stage.add(Cube::new(1.1))?;
            stage.wait(0.1)?;
            panic!("intentional panic after queued camera capture")
        }
    }
    let fs = Arc::new(VirtualFs::new());
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = render_with_fs(
            &mut Panicking,
            options("/panic", RenderFormat::PngSequence, 4, 4),
            fs.clone(),
        );
    }));
    assert!(result.is_err());
    assert!(!fs.exists(Path::new("/panic")));
    render_with_fs(
        &mut Static,
        options("/panic", RenderFormat::PngSequence, 4, 4),
        fs,
    )
    .unwrap();
}

#[test]
fn cached_affine_exports_use_the_shared_frame_pipeline() {
    let fs = Arc::new(VirtualFs::new());
    let mut options = options("/affine", RenderFormat::PngSequence, 2, 1);
    options.camera = None;
    struct Empty;
    impl SceneConstruct for Empty {
        fn construct(&mut self, _: &mut Stage<'_>) -> fmn::Result<()> {
            Ok(())
        }
    }
    let report = render_with_fs(&mut Empty, options, fs).unwrap();
    let stats = report.frame_pipeline.expect("affine pipeline was bypassed");
    assert_eq!(stats.emitted, 1);
    assert_eq!(stats.outstanding_slots, 0);
    assert_eq!(report.artifact.frame_count, 1);
}

#[test]
fn native_layout_animations_render_the_same_final_scene_as_explicit_placement() {
    struct LayoutScene {
        animated: bool,
    }
    impl SceneConstruct for LayoutScene {
        fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
            let row = stage.add(Mobject::group(vec![
                Rectangle::new()
                    .width(0.5)
                    .height(0.75)
                    .build()?
                    .shifted([-0.75, 0.0, 0.0])
                    .into(),
                Rectangle::new().width(0.5).height(0.75).build()?.into(),
                Rectangle::new()
                    .width(0.5)
                    .height(0.75)
                    .build()?
                    .shifted([0.75, 0.0, 0.0])
                    .into(),
            ]))?;
            stage.set_fill(row, Some(BLUE), Some(1.0), None, true);
            stage.set_stroke(row, None, Some(0.0), None, None, true);
            let children = stage.get(row).unwrap().submobjects().to_vec();
            let highlight = stage.add(Rectangle::new().width(1.0).height(1.0).build()?)?;
            stage.set_fill(highlight, None, Some(0.0), None, true);
            stage.set_stroke(highlight, Some(WHITE), Some(2.0), Some(1.0), None, true);
            let args = AnimateArgs {
                run_time: Some(0.25),
                rate_func: Some(fmn::core::rate::linear),
                ..AnimateArgs::default()
            };
            if self.animated {
                stage.play(
                    row.animate()
                        .set_anim_args(args)?
                        .arrange_to_fit_width(4.5, ORIGIN)?
                        .space_out_submobjects(0.75)?
                        .set_max_height(1.0, false)?,
                )?;
                stage.play(
                    highlight
                        .animate()
                        .set_anim_args(args)?
                        .surround(row, 0, true, 0.5)?,
                )?;
            } else {
                // Independent endpoint: centers at -1.5, 0, 1.5; each bar
                // remains 0.5 by 0.75. The row is 3.5 wide; the highlight's
                // total-width buffer is 0.5, so its height is 0.75 * 4/3.5.
                for (&child, x) in children.iter().zip([-1.5, 0.0, 1.5]) {
                    stage.move_to(child, [x, 0.0, 0.0], ORIGIN);
                }
                stage.set_width(highlight, 4.0, true);
                stage.set_height(highlight, 6.0 / 7.0, true);
            }
            for &child in &children {
                assert!((stage.get_width(child) - 0.5).abs() < 1e-6);
                assert!((stage.get_height(child) - 0.75).abs() < 1e-6);
            }
            assert!((stage.get_width(row) - 3.5).abs() < 1e-6);
            Ok(())
        }
    }

    let fs = Arc::new(VirtualFs::new());
    for (path, animated) in [("/layout.png", true), ("/expected-layout.png", false)] {
        let mut render_options = options(path, RenderFormat::Png, 2, 2);
        render_options.camera = None;
        render_options.config.camera.fps = 8;
        let report =
            render_with_fs(&mut LayoutScene { animated }, render_options, fs.clone()).unwrap();
        assert_eq!(report.artifact.frame_count, 1);
        assert_eq!(report.scene.play_count, if animated { 2 } else { 0 });
        assert_eq!(report.frame_pipeline.unwrap().outstanding_slots, 0);
    }
    let actual = fs.read(Path::new("/layout.png")).unwrap();
    let expected = fs.read(Path::new("/expected-layout.png")).unwrap();
    let actual = decode_png(&actual, &PngLimits::default()).unwrap();
    let expected = decode_png(&expected, &PngLimits::default()).unwrap();
    assert_eq!(actual.rgba, expected.rgba);
    assert!(
        actual
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .any(|pixel| { pixel[2] > pixel[0] && pixel[2] > 64 }),
        "the laid-out blue bars must actually be visible"
    );
}

struct NumericReadout {
    animated: bool,
}

impl SceneConstruct for NumericReadout {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        use fmn::library::numbers::{add_decimal_for_value_animation, change_decimal_to_value};

        let book = FontBook::bundled().expect("bundled fonts parse");
        let decimal = DecimalNumber::new(if self.animated { 1.0 } else { 1234.0 })
            .num_decimal_places(0)
            .font_size(56.0)
            .include_background_rectangle(true)
            .unit("m")
            .build(&book)?;
        let mob = if self.animated {
            let mob = add_decimal_for_value_animation(stage.arena_mut(), &decimal, &book)?;
            stage.add_to_scene(mob)?;
            mob
        } else {
            // The expected image constructs the final value directly and
            // uses the ordinary public Stage::add path.
            stage.add(decimal.clone())?
        };
        stage.scale(mob, 1.8);
        stage.move_to(mob, [-2.2, 0.6, 0.0], LEFT);
        stage.set_fill(mob, Some(BLUE), Some(1.0), None, true);
        let background = stage.get(mob).unwrap().submobjects()[0];
        stage.set_fill(background, Some(BLACK), Some(1.0), None, true);
        stage.set_z_index(mob, 3, true);
        stage.set_fixed_in_frame(mob, 1.0, true);
        if self.animated {
            stage.play(
                change_decimal_to_value(mob, decimal, book, 1234.0)?.with_config(AnimConfig {
                    run_time: 0.5,
                    rate_func: RateFunc::linear(),
                    ..AnimConfig::default()
                }),
            )?;
        }
        let anchor = stage.get_bounding_box_point(mob, LEFT);
        for (actual, expected) in anchor.into_iter().zip([-2.2, 0.6, 0.0]) {
            assert!((actual - expected).abs() < 1e-6);
        }
        assert_eq!(stage.z_index(mob), 3);
        Ok(())
    }
}

fn numeric_options(path: &str, format: RenderFormat) -> RenderOptions {
    let mut options = options(path, format, 2, 2);
    options.config.camera.resolution = (160, 96);
    options.config.camera.fps = 8;
    options.config.camera.background_color = "#808080".to_owned();
    options.camera = Some(options.camera_config().unwrap());
    options
}

#[test]
fn native_numeric_animation_matches_independently_placed_final_glyph_pixels() {
    let fs = Arc::new(VirtualFs::new());
    for (path, animated) in [("/number.png", true), ("/expected-number.png", false)] {
        let report = render_with_fs(
            &mut NumericReadout { animated },
            numeric_options(path, RenderFormat::Png),
            fs.clone(),
        )
        .unwrap();
        assert_eq!(report.scene.play_count, u64::from(animated));
        assert_eq!(report.artifact.frame_count, 1);
        assert_eq!(report.frame_pipeline.unwrap().outstanding_slots, 0);
    }
    let actual = decode_png(
        &fs.read(Path::new("/number.png")).unwrap(),
        &PngLimits::default(),
    )
    .unwrap();
    let expected = decode_png(
        &fs.read(Path::new("/expected-number.png")).unwrap(),
        &PngLimits::default(),
    )
    .unwrap();
    assert_eq!(actual.rgba, expected.rgba);
    let pixels = actual.rgba.as_chunks::<4>().0;
    assert!(
        pixels
            .iter()
            .any(|pixel| pixel[2] > pixel[0] && pixel[2] > 64)
    );
    assert!(pixels.iter().any(|pixel| pixel[..3] == [0, 0, 0]));
    assert!(pixels.iter().any(|pixel| pixel[..3] == [128, 128, 128]));
}

#[test]
fn failed_native_numeric_callback_aborts_the_artifact_and_allows_immediate_retry() {
    struct RefusedNumber;
    impl SceneConstruct for RefusedNumber {
        fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
            use fmn::library::numbers::{add_decimal_for_value_animation, changing_decimal};

            let book = FontBook::bundled().expect("bundled fonts parse");
            let decimal = DecimalNumber::new(1.0)
                .num_decimal_places(0)
                .character_limit(1)
                .build(&book)?;
            let mob = add_decimal_for_value_animation(stage.arena_mut(), &decimal, &book)?;
            stage.add_to_scene(mob)?;
            stage.wait(0.125)?;
            stage.play(
                changing_decimal(
                    mob,
                    decimal,
                    book,
                    |alpha| {
                        if alpha < 0.5 { 2.0 } else { 1234.0 }
                    },
                )?
                .with_config(AnimConfig {
                    run_time: 0.5,
                    rate_func: RateFunc::linear(),
                    ..AnimConfig::default()
                }),
            )?;
            panic!("a refused numeric frame cannot report successful playback")
        }
    }

    let fs = Arc::new(VirtualFs::new());
    let result = render_with_fs(
        &mut RefusedNumber,
        numeric_options("/refused-number", RenderFormat::PngSequence),
        fs.clone(),
    );
    assert!(matches!(
        result,
        Err(RenderError::Scene(Error::Animation(
            AnimError::UpdateFailed {
                operation: "DecimalNumber::set_value",
                ..
            }
        )))
    ));
    assert!(!fs.exists(Path::new("/refused-number")));
    let retry = render_with_fs(
        &mut NumericReadout { animated: false },
        numeric_options("/refused-number", RenderFormat::PngSequence),
        fs,
    )
    .unwrap();
    assert_eq!(retry.artifact.frame_count, 1);
    assert_eq!(retry.frame_pipeline.unwrap().outstanding_slots, 0);
}

#[test]
fn fallible_native_redraw_aborts_staged_output_and_allows_a_fresh_render() {
    use std::cell::Cell;
    use std::rc::Rc;

    struct RefusedRedraw {
        calls: Rc<Cell<usize>>,
    }

    impl SceneConstruct for RefusedRedraw {
        fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
            stage.add(Circle::new().radius(0.5).color(WHITE))?;
            // Start real output work before a later regeneration fails.
            stage.wait(0.125)?;
            let calls = Rc::clone(&self.calls);
            let redraw = stage.arena_mut().try_always_redraw(move |arena| {
                calls.set(calls.get() + 1);
                if calls.get() == 3 {
                    return Err(UpdaterError::callback(
                        "native redraw",
                        "the next geometry sample was refused",
                    ));
                }
                Ok(arena.add(Circle::new().radius(0.3).color(BLUE)))
            })?;
            stage.add_to_scene(redraw)?;
            stage.wait(0.5)?;
            panic!("a failed native redraw cannot report successful playback")
        }
    }

    let calls = Rc::new(Cell::new(0));
    let fs = Arc::new(VirtualFs::new());
    let error = render_with_fs(
        &mut RefusedRedraw {
            calls: Rc::clone(&calls),
        },
        options("/refused-redraw", RenderFormat::PngSequence, 2, 2),
        fs.clone(),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        RenderError::Scene(Error::Animation(AnimError::Updater(UpdaterFailure {
            error: UpdaterError::Callback {
                operation: "native redraw",
                ..
            },
            ..
        })))
    ));
    assert_eq!(calls.get(), 3, "abort must not retry the failed factory");
    assert!(!fs.exists(Path::new("/refused-redraw")));

    let retry = render_with_fs(
        &mut Static,
        options("/refused-redraw", RenderFormat::PngSequence, 2, 2),
        fs.clone(),
    )
    .unwrap();
    assert_eq!(retry.artifact.frame_count, 1);
    assert_eq!(retry.frame_pipeline.unwrap().outstanding_slots, 0);
    assert!(fs.exists(Path::new("/refused-redraw")));
    assert_eq!(calls.get(), 3);
}
