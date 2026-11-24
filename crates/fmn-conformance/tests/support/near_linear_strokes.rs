//! Shared production fixtures for CPU admission of bounded near-line strokes.

use fmn_conformance::scene_goldens::{TILING, frame_config};
use fmn_frame::FrameBuffer;
use fmn_mobject::{Mobject, RecordBuffer, RecordSchema, Stage};
use fmn_render::bin::Binning;
use fmn_render::engine::{EngineIdentity, FrameJob};
use fmn_render::fill::MonoTable;
use fmn_render::plan::RenderPlan;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Case {
    Uniform,
    WidthProfile,
    ColorProfile,
    SharpAntialiasing,
    Bowed,
}

impl Case {
    pub const ALL: [Self; 5] = [
        Self::Uniform,
        Self::WidthProfile,
        Self::ColorProfile,
        Self::SharpAntialiasing,
        Self::Bowed,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Uniform => "uniform_near_line",
            Self::WidthProfile => "near_line_width_profile",
            Self::ColorProfile => "near_line_color_profile",
            Self::SharpAntialiasing => "near_line_sharp_antialiasing",
            Self::Bowed => "bowed_quadratic",
        }
    }

    pub const fn admitted(self) -> usize {
        if matches!(self, Self::Uniform) {
            4
        } else {
            0
        }
    }
}

/// Four untagged quadratics with different directions and overlapping ink.
/// Their controls have a real, f32-representable deviation from their chords;
/// no semantic Line hint or primitive constructor can admit them instead.
pub fn build(case: Case) -> Stage {
    let mut stage = Stage::new();
    let edges = [
        ([-2.0_f32, -1.0], [-0.3, -0.8]),
        ([-2.0, 1.0], [-0.3, 0.8]),
        ([0.3, -1.0], [2.0, 1.0]),
        ([0.3, 1.0], [2.0, -1.0]),
    ];
    for (start, end) in edges {
        let deviation = if case == Case::Bowed {
            0.25
        } else {
            1.0 / 1_048_576.0
        };
        let points = [
            [start[0], start[1], 0.0],
            [
                0.5 * (start[0] + end[0]),
                0.5 * (start[1] + end[1]) + deviation,
                0.0,
            ],
            [end[0], end[1], 0.0],
        ];
        let mut records =
            RecordBuffer::new(RecordSchema::vmobject(), points.len()).expect("three vector records");
        for (index, point) in points.iter().enumerate() {
            records.write(index, "point", point);
            records.write(index, "fill_rgba", &[0.0; 4]);
            records.write(
                index,
                "stroke_rgba",
                if case == Case::ColorProfile && index == 1 {
                    &[0.9, 0.2, 0.1, 0.75]
                } else {
                    &[0.1, 0.6, 0.9, 0.75]
                },
            );
            records.write(
                index,
                "stroke_width",
                &[if case == Case::WidthProfile && index == 1 {
                    24.0
                } else {
                    12.0
                }],
            );
        }
        let mut mobject = Mobject::from_buffer(records);
        if case == Case::SharpAntialiasing {
            mobject.uniforms.anti_alias_width = 0.125;
        }
        let mob = stage.add(mobject);
        stage.add_to_scene(mob).expect("new vector is live");
    }
    stage
}

/// Exercise the real plan, binning and FrameJob production route and report
/// how many segments actually selected the new bounded CPU approximation.
pub fn render(
    stage: &Stage,
    identity: EngineIdentity,
    threads: usize,
) -> Result<(FrameBuffer, usize), String> {
    let config = frame_config();
    let mut plan = RenderPlan::new();
    plan.sync(stage, 0).map_err(|error| error.to_string())?;
    let mono = MonoTable::build(&plan, config.map).map_err(|error| error.to_string())?;
    let bins =
        Binning::build(&plan, config.viewport, TILING, config.map).map_err(|error| error.to_string())?;
    let job = FrameJob::with_identity(&plan, &mono, &bins, config, identity)
        .map_err(|error| error.to_string())?;
    let count = job.bounded_line_stroke_count();
    let frame = job.render(threads).map_err(|error| error.to_string())?;
    Ok((frame, count))
}
