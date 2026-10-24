//! Native scene programs use only the public prelude; decoding is an independent oracle.

use std::path::Path;
use std::sync::Arc;

use fmn::prelude::*;
use fmn_codec::{PngLimits, decode_png};
use fmn_platform::fs::{FileSystem, VirtualFs};

#[derive(Clone, Copy, Debug)]
enum Family {
    Creation,
    Focus,
    Function,
    Cycle,
    Grow,
    Homotopy,
    Fade,
}

fn timing() -> AnimConfig {
    AnimConfig {
        run_time: 0.25,
        rate_func: RateFunc::linear(),
        ..AnimConfig::default()
    }
}

impl SceneConstruct for Family {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        let square = stage.add(Square::new().side_length(1.0).color(BLUE))?;
        stage.set_fill(square, Some(BLUE), Some(1.0), None, true);
        match self {
            Self::Creation => {
                let fade = fade_out(stage, square, ORIGIN, 1.0)?.with_config(timing());
                let sequence = show_creation_then_fade_out(
                    stage,
                    vec![Box::new(show_creation(square)), Box::new(fade)],
                    1.0,
                    true,
                )?
                .with_run_time(0.5);
                stage.play(sequence)?;
                assert!(!stage.roots().contains(&square));
            }
            Self::Focus => {
                let dot = stage.add(Dot::new().radius(1.0).color(YELLOW))?;
                let target = stage.add(Dot::new().radius(0.0).color(YELLOW))?;
                stage.shift(target, RIGHT);
                stage.play(focus_on(dot, target, false).with_config(timing()))?;
                assert_eq!(stage.get_center(dot), RIGHT);
            }
            Self::Function => {
                let transform = apply_function(stage, square, |arena, target| {
                    arena.shift(target, RIGHT);
                })?;
                stage.play(transform.with_config(timing()))?;
                assert_eq!(stage.get_center(square), RIGHT);
            }
            Self::Cycle => {
                stage.shift(square, LEFT);
                let circle = stage.add(Circle::new().radius(0.5).color(RED))?;
                stage.shift(circle, RIGHT);
                let transforms = cyclic_replace(stage, &[square, circle], 0.0)?;
                let prepared = transforms
                    .into_iter()
                    .map(|transform| {
                        Box::new(transform.with_config(timing())) as Box<dyn Animation>
                    })
                    .collect();
                stage.play_prepared(prepared)?;
                assert_eq!(stage.get_center(square), RIGHT);
                assert_eq!(stage.get_center(circle), LEFT);
            }
            Self::Grow => {
                let grow = grow_from_point(stage, square, LEFT, Some(RED))?;
                stage.play(grow.with_config(timing()))?;
                assert_eq!(stage.get_center(square), ORIGIN);
            }
            Self::Homotopy => {
                stage.play(
                    Homotopy::new(|x, y, z, alpha| [x + alpha, y, z], square).with_config(timing()),
                )?;
                assert_eq!(stage.get_center(square), RIGHT);
            }
            Self::Fade => {
                let fade = fade_in(stage, square, LEFT, 1.0)?;
                stage.play(fade.with_config(timing()))?;
                assert_eq!(stage.get_center(square), ORIGIN);
            }
        }
        Ok(())
    }
}

#[test]
fn prelude_families_render_real_frames_and_keep_thread_independent_pixels() {
    for family in [
        Family::Creation,
        Family::Focus,
        Family::Function,
        Family::Cycle,
        Family::Grow,
        Family::Homotopy,
        Family::Fade,
    ] {
        let mut baseline = None;
        for threads in [1, 4] {
            let fs = Arc::new(VirtualFs::new());
            let mut options = RenderOptions::new("/prelude").unwrap();
            options.format = RenderFormat::PngSequence;
            options.config.camera.resolution = (64, 48);
            options.config.camera.fps = 8;
            options.config.determinism.mode = DeterminismMode::Certified;
            options.config.render.threads = ThreadPolicy::Fixed(threads);
            let mut scene = family;
            let report = render_with_fs(&mut scene, options, fs.clone()).unwrap();
            assert!(report.artifact.frame_count >= 2, "{family:?}");
            let frames: Vec<_> = (0..report.artifact.frame_count)
                .map(|index| {
                    let path = format!("/prelude/frame_{index:06}.png");
                    let bytes = fs.read(Path::new(&path)).unwrap();
                    let decoded = decode_png(&bytes, &PngLimits::default()).unwrap();
                    assert_eq!((decoded.width, decoded.height), (64, 48));
                    (bytes, decoded.rgba)
                })
                .collect();
            assert_ne!(
                frames.first().map(|(_, pixels)| pixels),
                frames.last().map(|(_, pixels)| pixels),
                "{family:?} must animate"
            );
            if let Some(expected) = &baseline {
                assert_eq!(&frames, expected, "{family:?} differs by thread count");
            } else {
                baseline = Some(frames);
            }
        }
    }
}
