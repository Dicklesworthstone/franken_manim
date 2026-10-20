//! Live numeric TeX spans must survive native animation and final publication.

use std::path::Path;
use std::sync::Arc;

use fmn::library::numbers::change_decimal_to_value;
use fmn::library::{TexSpanBinding, TransformMatchingTex};
use fmn::prelude::*;
use fmn_codec::{PngLimits, decode_png};
use fmn_config::config::{DeterminismMode, ThreadPolicy};
use fmn_platform::fs::{FileSystem, VirtualFs};

#[derive(Clone, Copy)]
enum NumberMode {
    Animate,
    Endpoint,
    Original,
}

struct LiveFormula(NumberMode);

impl SceneConstruct for LiveFormula {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        let formula = Tex::new("a=12.0+12.0")
            .font_size(64.0)
            .build(stage.tex_engine()?)?;
        let spans = formula.span_map();
        let occurrences = formula.occurrences("12.0");
        let selected = &occurrences[1];
        let root = stage.add(formula)?;
        stage.scale(root, 1.3);
        stage.rotate(root, 0.23, OUT, None, None);
        stage.shift(root, [-0.4, 0.2, 0.0]);
        let children = stage.get(root).unwrap().submobjects().to_vec();
        for &index in &occurrences[0] {
            stage.set_fill(children[index], Some(YELLOW), Some(1.0), None, false);
        }
        for &index in selected {
            stage.set_fill(children[index], Some(BLUE), Some(1.0), None, false);
        }
        let untouched = children
            .iter()
            .enumerate()
            .filter(|(index, _)| !selected.contains(index))
            .map(|(_, &mob)| {
                (
                    mob,
                    stage.get_points(mob).unwrap(),
                    stage.get_fill_color(mob).unwrap(),
                )
            })
            .collect::<Vec<_>>();
        let book = FontBook::bundled().expect("bundled fonts parse");
        let mut binding = TexSpanBinding::new(stage.arena(), root, spans)?;
        let number = binding.make_number_changeable(
            stage.arena_mut(),
            "12.0",
            1,
            DecimalNumber::new(0.0)
                .num_decimal_places(1)
                .group_with_commas(false)
                .edge_to_fix(LEFT),
            &book,
        )?;
        let (slot, decimal) = number.into_parts();
        assert!(stage.get(root).unwrap().submobjects().contains(&slot));
        assert_eq!(decimal.value(), 12.0);
        assert_eq!(
            stage.get_fill_color(slot).unwrap().to_rgb8(),
            BLUE.to_rgb8(),
            "the selected glyph supplies paint, independently of root defaults"
        );
        assert_eq!(binding.span_map().source.as_ref(), "a=12.0+\\decimalmob");
        let anchor = stage.get_bounding_box_point(slot, LEFT);
        match self.0 {
            NumberMode::Animate => {
                stage.play(
                    change_decimal_to_value(slot, decimal, book, 1234.5)?.with_config(AnimConfig {
                        run_time: 0.5,
                        rate_func: RateFunc::linear(),
                        ..AnimConfig::default()
                    }),
                )?;
            }
            NumberMode::Endpoint => {
                // Build the final glyphs directly and apply the installed
                // slot's affine placement and fixed edge. No animation
                // callback supplies the expected final image.
                let placement = stage.get(slot).unwrap().placement();
                let mut endpoint = decimal;
                endpoint.set_value(1234.5, &book)?;
                // Fresh construction starts in canonical layout coordinates;
                // the world anchor is applied after record conversion, just
                // as it is for any independently built numeric mobject.
                let endpoint = endpoint.build(&book)?;
                stage.arena_mut().install(slot, endpoint.into_vmob())?;
                for member in stage.arena().family(slot) {
                    stage.arena_mut().set_placement(member, placement)?;
                }
                stage.set_fill(slot, Some(BLUE), Some(1.0), None, true);
                stage.move_to(slot, anchor, LEFT);
            }
            NumberMode::Original => {}
        }
        for (mob, points, color) in untouched {
            assert!(stage.get(root).unwrap().submobjects().contains(&mob));
            assert_eq!(stage.get_points(mob).unwrap(), points);
            assert_eq!(stage.get_fill_color(mob).unwrap(), color);
        }
        let current_anchor = stage.get_bounding_box_point(slot, LEFT);
        for axis in 0..3 {
            assert!((current_anchor[axis] - anchor[axis]).abs() < 2e-6);
        }
        Ok(())
    }
}

fn options(path: &str, threads: u32) -> RenderOptions {
    let mut options = RenderOptions::new(path).unwrap();
    options.format = RenderFormat::Png;
    options.config.camera.resolution = (160, 96);
    options.config.camera.fps = 8;
    options.config.sizes.frame_height = 4.0;
    options.config.determinism.mode = DeterminismMode::Certified;
    options.config.render.threads = ThreadPolicy::Fixed(threads);
    options.typeset_cache = false;
    options
}

fn pixels(fs: &VirtualFs, path: &str) -> Vec<u8> {
    decode_png(&fs.read(Path::new(path)).unwrap(), &PngLimits::default())
        .unwrap()
        .rgba
}

#[test]
fn second_live_tex_number_matches_independently_placed_endpoint_pixels() {
    let fs = Arc::new(VirtualFs::new());
    for (path, mode, threads) in [
        ("/live-tex.png", NumberMode::Animate, 1),
        ("/live-tex-parallel.png", NumberMode::Animate, 4),
        ("/expected-tex.png", NumberMode::Endpoint, 1),
        ("/original-tex.png", NumberMode::Original, 1),
    ] {
        let report =
            render_with_fs(&mut LiveFormula(mode), options(path, threads), fs.clone()).unwrap();
        assert_eq!(report.artifact.frame_count, 1);
        assert_eq!(report.frame_pipeline.unwrap().outstanding_slots, 0);
        assert_eq!(
            report.scene.time.frames(),
            if matches!(mode, NumberMode::Animate) {
                4
            } else {
                0
            }
        );
    }
    let actual = pixels(&fs, "/live-tex.png");
    assert_eq!(actual, pixels(&fs, "/expected-tex.png"));
    assert_eq!(actual, pixels(&fs, "/live-tex-parallel.png"));
    assert_ne!(actual, pixels(&fs, "/original-tex.png"));
    let rgba = actual.as_chunks::<4>().0;
    assert!(
        rgba.iter()
            .any(|pixel| pixel[2] > pixel[0] && pixel[2] > 64)
    );
    assert!(rgba.iter().any(|pixel| pixel[0] > 128 && pixel[1] > 128));
}

struct MatchLiveFormula;

impl SceneConstruct for MatchLiveFormula {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        let before = Tex::new("u=2+2").build(stage.tex_engine()?)?;
        let after = Tex::new("2+2=u").build(stage.tex_engine()?)?;
        let (before_spans, after_spans) = (before.span_map(), after.span_map());
        let before = stage.add(before)?;
        let after = stage.arena_mut().add(after);
        stage.move_to(before, [-0.5, 0.4, 0.0], ORIGIN);
        stage.move_to(after, [0.6, -0.2, 0.0], ORIGIN);
        let mut source = TexSpanBinding::new(stage.arena(), before, before_spans)?;
        let mut target = TexSpanBinding::new(stage.arena(), after, after_spans)?;
        let book = FontBook::bundled().expect("bundled fonts parse");
        let numbers = source.make_numbers_changeable(
            stage.arena_mut(),
            "2",
            DecimalNumber::new(0.0).num_decimal_places(0),
            &book,
        )?;
        target.make_numbers_changeable(
            stage.arena_mut(),
            "2",
            DecimalNumber::new(0.0).num_decimal_places(0),
            &book,
        )?;
        assert_eq!(numbers.len(), 2);
        let (slot, decimal) = numbers.into_iter().next().unwrap().into_parts();
        stage.play(
            change_decimal_to_value(slot, decimal, book, 125.0)?.with_config(AnimConfig {
                run_time: 0.25,
                rate_func: RateFunc::linear(),
                ..AnimConfig::default()
            }),
        )?;
        // Numeric regeneration retired the original digit descendants. A
        // fresh grouped matching plan must accept the stable logical slot.
        let animation = TransformMatchingTex::from_bindings(stage.arena_mut(), &source, &target)?;
        stage.play(animation)?;
        assert_eq!(stage.roots(), &[after]);
        assert_eq!(
            stage.get(after).unwrap().submobjects().len(),
            target.span_map().entries.len()
        );
        Ok(())
    }
}

#[test]
fn native_matching_accepts_live_slots_after_numeric_regeneration() {
    let fs = Arc::new(VirtualFs::new());
    let report = render_with_fs(
        &mut MatchLiveFormula,
        options("/matched-live-tex.png", 2),
        fs.clone(),
    )
    .unwrap();
    assert_eq!(report.scene.play_count, 2);
    assert_eq!(report.frame_pipeline.unwrap().outstanding_slots, 0);
    assert!(
        pixels(&fs, "/matched-live-tex.png")
            .as_chunks::<4>()
            .0
            .iter()
            .any(|pixel| pixel[0] > 32 || pixel[1] > 32 || pixel[2] > 32)
    );
}
