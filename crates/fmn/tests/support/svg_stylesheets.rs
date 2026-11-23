//! Embedded SVG styles reach native editable geometry, animation and PNG output.

use std::path::Path;
use std::sync::Arc;

use fmn::prelude::*;
use fmn_codec::{PngLimits, decode_png};
use fmn_config::config::{DeterminismMode, ThreadPolicy};
use fmn_geom::{FillRule, Paint, SvgDocument};
use fmn_library::svg::{svg_mobject_with_paints, svg_reference_frame};
use fmn_platform::fs::{FileSystem, VirtualFs};

const STYLED: &str = r##"<svg width="12" height="8">
  <g opacity=".8" fill="black">
    <path class="panel" d="M1 1H7V7H1Z M2 2H6V6H2Z"/>
    <path id="accent" class="hot" fill="black" style="fill:green;fill-opacity:.5"
          d="M8 1H11V4H8Z"/>
    <use href="#trace" y="6"/>
    <g class="hidden"><rect class="shown" x="8" y="5" width="3" height="2"/></g>
    <g class="off"><rect style="display:inline" width="12" height="8"/></g>
  </g>
  <defs>
    <path id="trace" class="trace" d="M8 0Q9.5 -2 11 0"/>
    <style type="text/css" media="all"><![CDATA[
      * { fill:#224488 }
      .panel { fill:#4263eb; fill-rule:evenodd }
      path.trace { fill:NONE; stroke:gold; stroke-width:2;
        stroke-dasharray:.4 .2; stroke-linecap:round; opacity:.75 }
      #accent.hot { fill:rgba(255,64,32,.5) !important }
      .hot { fill:lime }
      .hidden { visibility:hidden }
      .shown { visibility:visible; fill:teal }
      .off { display:none }
    ]]></style>
  </defs>
</svg>"##;

// Independently authored resolved paints, without CSS, ids, classes or `use`.
// The half-alpha orange paint and half channel opacity become exactly 1/4.
const PRESENTATION: &str = r##"<svg width="12" height="8">
  <g opacity=".8">
    <path fill="#4263eb" fill-rule="evenodd" d="M1 1H7V7H1Z M2 2H6V6H2Z"/>
    <path fill="#ff4020" fill-opacity=".25" d="M8 1H11V4H8Z"/>
    <path fill="none" stroke="gold" stroke-width="2" stroke-dasharray=".4 .2"
          stroke-linecap="round" opacity=".75" transform="translate(0 6)"
          d="M8 0Q9.5 -2 11 0"/>
    <rect fill="teal" x="8" y="5" width="3" height="2"/>
  </g>
</svg>"##;

struct ImportedSvgScene {
    object: Option<Mobject>,
}

impl SceneConstruct for ImportedSvgScene {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        let object = stage.add(self.object.take().expect("one scene construction"))?;
        stage.play(
            object
                .animate()
                .set_anim_args(AnimateArgs {
                    run_time_frames: Some(3),
                    rate_func: Some(fmn_core::rate::linear),
                    ..AnimateArgs::default()
                })?
                .shift([0.6, 0.2, 0.0])?,
        )?;
        Ok(())
    }
}

fn render(source: &str, threads: u32) -> Result<Vec<Vec<u8>>, String> {
    let family = svg_mobject_with_paints(source.as_bytes(), Default::default())
        .map_err(|error| format!("native SVG paint admission: {error}"))?;
    if family.children().len() != 4 || family.children()[0].children().is_empty() {
        return Err("stylesheet import lost its authored children or even-odd fill layer".into());
    }
    let object = svg_reference_frame(family)
        .scaled_about(0.4, [0.0; 3])
        .into();
    let fs = Arc::new(VirtualFs::new());
    let mut options = RenderOptions::new("/svg-stylesheets").map_err(|error| error.to_string())?;
    options.config.camera.resolution = (72, 48);
    options.config.camera.fps = 8;
    options.config.camera.background_opacity = 0.0;
    options.config.sizes.frame_height = 4.0;
    options.config.determinism.mode = DeterminismMode::Certified;
    options.config.render.threads = ThreadPolicy::Fixed(threads);
    options.frames_in_flight = 2;
    options.typeset_cache = false;
    let report = render_with_fs(
        &mut ImportedSvgScene {
            object: Some(object),
        },
        options,
        fs.clone(),
    )
    .map_err(|error| format!("native SVG frame render: {error}"))?;
    let pipeline = report
        .frame_pipeline
        .ok_or("SVG render omitted its frame pipeline")?;
    if report.artifact.frame_count != 3
        || report.scene.time.frames() != 3
        || pipeline.submitted != 3
        || pipeline.emitted != 3
        || pipeline.outstanding_slots != 0
    {
        return Err("imported SVG animation lost its frame timing or ordered publication".into());
    }
    (0..3)
        .map(|frame| {
            fs.read(&Path::new("/svg-stylesheets").join(format!("frame_{frame:06}.png")))
                .map_err(|error| error.to_string())
        })
        .collect()
}

pub struct Witness {
    pub first: Vec<u8>,
    pub last: Vec<u8>,
    pub frames: u64,
}

/// Compare against independently stated presentation attributes, then exercise
/// the checked native importer and real camera/animation/output path at 1/4/16
/// workers. Complete PNG bytes must agree, including all intermediate frames.
pub fn verify() -> Result<Witness, String> {
    let styled = SvgDocument::parse(STYLED.as_bytes()).map_err(|error| error.to_string())?;
    let expected =
        SvgDocument::parse(PRESENTATION.as_bytes()).map_err(|error| error.to_string())?;
    if styled != expected || styled.shapes.len() != 4 {
        return Err("CSS cascade differs from the independent resolved SVG document".into());
    }
    let accent = &styled.shapes[1].style;
    let accent_color = Srgb::from_rgb8(255, 64, 32);
    if styled.shapes[0].style.fill_rule != FillRule::EvenOdd
        || accent.fill != Some(Paint::Color(accent_color))
        || accent.fill_opacity != 0.25
        || accent.opacity != 0.8
        || styled.shapes[2].style.stroke_dasharray != [0.4, 0.2]
    {
        return Err("stylesheet fixture lost its analytic paint/cascade expectations".into());
    }
    let expected = render(PRESENTATION, 1)?;
    let first =
        decode_png(&expected[0], &PngLimits::default()).map_err(|error| error.to_string())?;
    if first
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|pixel| pixel[3] > 0)
        .count()
        < 100
    {
        return Err("stylesheet fixture did not produce substantial visible geometry".into());
    }
    if expected[0] == expected[2] {
        return Err("stylesheet fixture did not animate its imported geometry".into());
    }
    let mut frames = 3;
    for threads in [1, 4, 16] {
        let actual = render(STYLED, threads)?;
        frames += 3;
        if actual != expected {
            return Err(format!(
                "SVG stylesheet frames differ from presentation attributes at {threads} workers"
            ));
        }
    }
    Ok(Witness {
        first: expected[0].clone(),
        last: expected[2].clone(),
        frames,
    })
}
