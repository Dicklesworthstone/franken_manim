//! The fm-5xm primitive Scene corpus: 25 named scenes driven through the
//! public Proscenium lifecycle, Choreo's rational clock and FramePacket,
//! materialized only from the immutable packet at the Lumen boundary, then
//! rasterized into certified self-goldens.
//!
//! Each artifact locks the complete three-frame sequence (two play samples +
//! one wait sample), not merely terminal geometry. The terminal packet is also
//! rendered at {1,4,16} threads so the Scene integration participates in PG-5
//! rather than relying only on Lumen's isolated engine corpus.

use fmn_anim::{FramePacket, prepare_animation};
use fmn_conformance::golden::{GoldenStore, Scope};
use fmn_core::color::Srgb;
use fmn_core::constants::{BLUE_C, GREEN_B, MAROON_C, RED_C, TEAL_B, WHITE, YELLOW_C};
use fmn_hash::{Schema, Writer};
use fmn_library::style::VStyle;
use fmn_library::{
    Annulus, Arc, ArcBetweenPoints, Arrow, Circle, DashedLine, Dot, Ellipse, Line, Rectangle,
    RegularPolygon,
};
use fmn_mobject::Mobject;
use fmn_mobject::animate::AnimateArgs;
use fmn_render::bin::{Binning, ScreenMap, Tiling, Viewport};
use fmn_render::engine::{FrameConfig, FrameJob, encode_frame};
use fmn_render::fill::MonoTable;
use fmn_render::plan::RenderPlan;
use fmn_scene::{
    CaptureReason, IntegrationError, PlayOverrides, RuntimeConfig, Scene, SceneError, SceneProgram,
    SceneSink,
};
use std::path::PathBuf;

const WIDTH: u32 = 96;
const HEIGHT: u32 = 54;
const SCALE: f64 = 20.0;
const FPS: u32 = 8;
const TILING: Tiling = Tiling {
    macro_tile: 64,
    fine_tile: 8,
};
const CORPUS_SCHEMA: Schema = Schema::new(*b"FMNS", 13, 1, 0);

const NAMES: [&str; 25] = [
    "circle_shift.v1",
    "rectangle_shift.v1",
    "triangle_shift.v1",
    "pentagon_shift.v1",
    "arc_shift.v1",
    "dot_shift.v1",
    "ellipse_shift.v1",
    "annulus_shift.v1",
    "line_shift.v1",
    "dashed_line_shift.v1",
    "arrow_shift.v1",
    "circle_scale.v1",
    "rectangle_scale.v1",
    "triangle_scale.v1",
    "hexagon_scale.v1",
    "arc_scale.v1",
    "dot_scale.v1",
    "ellipse_scale.v1",
    "annulus_scale.v1",
    "line_scale.v1",
    "dashed_line_scale.v1",
    "arrow_scale.v1",
    "rounded_rectangle.v1",
    "arc_between_points.v1",
    "layered_polygon.v1",
];

const COLORS: [Srgb; 7] = [BLUE_C, GREEN_B, MAROON_C, RED_C, TEAL_B, YELLOW_C, WHITE];

fn store() -> GoldenStore {
    GoldenStore::new(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("goldens"),
        "scene_runtime",
        Scope::Certified,
    )
    .expect("golden store")
}

fn frame_config() -> FrameConfig {
    FrameConfig::new(
        Viewport {
            width: WIDTH,
            height: HEIGHT,
        },
        ScreenMap {
            scale: SCALE,
            origin: [f64::from(WIDTH) / 2.0, f64::from(HEIGHT) / 2.0],
        },
        Srgb::from_rgb8(0x22, 0x22, 0x22).to_linear(1.0),
    )
}

fn render(packet: &FramePacket, threads: usize) -> Result<Vec<u8>, IntegrationError> {
    let stage = packet.materialize_stage();
    let config = frame_config();
    let mut plan = RenderPlan::new();
    let camera_revision = u64::try_from(packet.frame_index())
        .map_err(|_| IntegrationError::new("lumen", "negative frame index reached the renderer"))?;
    plan.sync(&stage, camera_revision);
    let mono = MonoTable::build(&plan, config.map);
    let mut binning = Binning::build(&plan, config.viewport, TILING, config.map);
    binning
        .prune_occluded(&plan)
        .map_err(|error| IntegrationError::new("lumen", error.to_string()))?;
    let job = FrameJob::new(&plan, &mono, &binning, config)
        .map_err(|error| IntegrationError::new("lumen", error.to_string()))?;
    let frame = job
        .render(threads)
        .map_err(|error| IntegrationError::new("lumen", error.to_string()))?;
    encode_frame(&frame).map_err(|error| IntegrationError::new("lumen", error.to_string()))
}

struct LumenSink {
    frames: Vec<(CaptureReason, i64, i64, Vec<u8>)>,
    last: Option<FramePacket>,
}

impl LumenSink {
    fn new() -> Self {
        Self {
            frames: Vec::new(),
            last: None,
        }
    }

    fn artifact(&self, name: &str) -> Vec<u8> {
        let mut writer = Writer::new(CORPUS_SCHEMA);
        writer.put_str(name).put_u64(self.frames.len() as u64);
        for (reason, frame, segment_frame, bytes) in &self.frames {
            writer
                .put_u8(match reason {
                    CaptureReason::Segment => 0,
                    CaptureReason::Show => 1,
                    CaptureReason::SkippedPreview => 2,
                    CaptureReason::PresenterHold => 3,
                })
                .put_i64(*frame)
                .put_i64(*segment_frame)
                .put_bytes(bytes);
        }
        writer.finish().expect("corpus artifact encodes")
    }
}

impl SceneSink for LumenSink {
    fn capture(
        &mut self,
        reason: CaptureReason,
        packet: FramePacket,
    ) -> Result<(), IntegrationError> {
        let bytes = render(&packet, 1)?;
        self.frames
            .push((reason, packet.frame_index(), packet.segment_frame(), bytes));
        self.last = Some(packet);
        Ok(())
    }
}

struct PrimitiveScene {
    index: usize,
    name: &'static str,
}

impl SceneProgram for PrimitiveScene {
    fn name(&self) -> &str {
        self.name
    }

    fn construct(&mut self, scene: &mut Scene, sink: &mut dyn SceneSink) -> Result<(), SceneError> {
        let color = COLORS[self.index % COLORS.len()];
        let main = scene.add_mobject(primitive(self.index, color))?;
        scene.stage_mut().set_fill(
            main,
            Some(color),
            Some(0.28 + 0.08 * (self.index % 6) as f64),
            Some(0.0),
            true,
        );
        scene.stage_mut().set_stroke(
            main,
            Some(WHITE),
            Some(1.2 + 0.25 * (self.index % 5) as f64),
            Some(0.95),
            None,
            true,
        );

        // A quiet back layer makes painter ordering and alpha composition part
        // of every sequence instead of only the dedicated layered fixture.
        let back = scene.add_mobject(
            Circle::new()
                .radius(0.32 + 0.015 * self.index as f64)
                .arc_center([-0.55, 0.28, 0.0]),
        )?;
        scene.stage_mut().set_fill(
            back,
            Some(COLORS[(self.index + 3) % COLORS.len()]),
            Some(0.32),
            Some(0.0),
            true,
        );
        scene
            .stage_mut()
            .set_stroke(back, None, Some(0.0), Some(0.0), None, true);
        scene.stage_mut().set_z_index(back, -1, true);
        scene.add(&[back])?;

        let start_x = -0.72 + 0.06 * (self.index % 5) as f64;
        let start_y = -0.32 + 0.11 * (self.index % 7) as f64;
        scene.stage_mut().shift(main, [start_x, start_y, 0.0]);

        let builder = main
            .animate()
            .set_anim_args(AnimateArgs {
                run_time: Some(0.25),
                rate_func: Some(fmn_core::rate::linear),
                ..AnimateArgs::default()
            })
            .expect("primitive animation args");
        let builder = if self.index < 11 {
            builder.shift([
                1.0 + 0.04 * (self.index % 4) as f64,
                0.18 - 0.03 * (self.index % 3) as f64,
                0.0,
            ])
        } else if self.index < 22 {
            builder.scale(0.82 + 0.035 * (self.index % 6) as f64)
        } else {
            builder
                .shift([0.76, 0.16, 0.0])
                .and_then(|builder| builder.scale(0.9))
        }
        .expect("primitive animation records");
        let animation = prepare_animation(builder, scene.stage_mut())?;
        scene.play(vec![animation], PlayOverrides::default(), sink)?;
        scene.wait(Some(0.125), sink)?;
        Ok(())
    }
}

fn primitive(index: usize, color: Srgb) -> Mobject {
    match index {
        0 | 11 => Circle::new()
            .radius(0.48 + 0.02 * (index % 4) as f64)
            .color(color)
            .into(),
        1 | 12 => Rectangle::new()
            .width(1.1)
            .height(0.62 + 0.03 * (index % 3) as f64)
            .color(color)
            .build()
            .expect("the runtime rectangle is unrounded")
            .into(),
        2 | 13 => Mobject::try_from(RegularPolygon::triangle().radius(0.6).color(color))
            .expect("three directions are within the public cap"),
        3 => Mobject::try_from(RegularPolygon::new(5).radius(0.56).color(color))
            .expect("five directions are within the public cap"),
        14 => Mobject::try_from(RegularPolygon::new(6).radius(0.56).color(color))
            .expect("six directions are within the public cap"),
        4 | 15 => Arc::new()
            .start_angle(-0.4)
            .angle(4.2)
            .radius(0.58)
            .color(color)
            .build()
            .expect("the locked runtime arc is valid")
            .into(),
        5 | 16 => Dot::new().radius(0.23).color(color).into(),
        6 | 17 => Ellipse::new().width(1.15).height(0.58).color(color).into(),
        7 | 18 => Annulus::new()
            .inner_radius(0.22)
            .outer_radius(0.52)
            .color(color)
            .into(),
        8 | 19 => Line::new([-0.58, -0.25, 0.0], [0.58, 0.25, 0.0])
            .path_arc(0.18)
            .color(color)
            .build()
            .expect("the locked runtime line arc is valid")
            .into(),
        9 | 20 => DashedLine::new([-0.62, 0.0, 0.0], [0.62, 0.0, 0.0])
            .dash_length(0.16)
            .positive_space_ratio(0.55)
            .color(color)
            .build()
            .expect("the locked runtime dash configuration is valid")
            .into(),
        10 | 21 => Arrow::new([-0.58, 0.0, 0.0], [0.58, 0.0, 0.0])
            .buff(0.0)
            .color(color)
            .build()
            .expect("the locked runtime arrow is valid")
            .into(),
        22 => Rectangle::new()
            .width(1.1)
            .height(0.66)
            .corner_radius(0.16)
            .color(color)
            .build()
            .expect("the locked runtime rounded rectangle is valid")
            .into(),
        23 => ArcBetweenPoints::new([-0.58, -0.18, 0.0], [0.58, 0.18, 0.0])
            .angle(1.2)
            .color(color)
            .build()
            .expect("the locked runtime between-points arc is valid")
            .into(),
        // The caller enumerates the fixed 25-entry NAMES table, so this is
        // index 24: the dedicated layered-polygon fixture.
        _ => Mobject::try_from(RegularPolygon::new(7).radius(0.56).color(color))
            .expect("seven directions are within the public cap"),
    }
}

#[test]
fn twenty_five_scene_sequences_are_bit_locked_and_thread_invariant() {
    assert_eq!(NAMES.len(), 25);
    let store = store();
    let mut failures = Vec::new();
    for (index, &name) in NAMES.iter().enumerate() {
        let mut scene = Scene::new(
            RuntimeConfig {
                fps: FPS,
                ..RuntimeConfig::default()
            },
            index as u64,
        )
        .expect("primitive scene");
        let mut program = PrimitiveScene { index, name };
        let mut sink = LumenSink::new();
        let report = scene.run(&mut program, &mut sink).expect("scene runs");
        assert_eq!(report.play_count, 2, "{name}");
        assert_eq!(sink.frames.len(), 3, "{name}");

        if let Err(error) = store.check(name, &sink.artifact(name)) {
            failures.push(error.to_string());
        }

        let terminal = sink.last.as_ref().expect("wait emitted a terminal frame");
        let scalar = &sink.frames.last().expect("terminal bytes").3;
        for threads in [4, 16] {
            assert_eq!(
                render(terminal, threads).expect("threaded render"),
                *scalar,
                "{name} drifted at {threads} threads"
            );
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
