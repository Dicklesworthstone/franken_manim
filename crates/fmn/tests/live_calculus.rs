//! The public renderer places live calculus geometry on moved axes.
use fmn::library::{AxisConfig, CoordinateSystem, FontBook, RiemannConfig, Style};
use fmn::prelude::*;
use fmn_config::config::{DeterminismMode, ThreadPolicy};
use fmn_geom::QuadPath;
use fmn_mobject::Placement;
use fmn_platform::fs::{FileSystem, VirtualFs};
use std::{path::Path, sync::Arc};

struct CalculusScene {
    oracle: bool,
}
impl SceneConstruct for CalculusScene {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        let axes = Axes::new()
            .x_range([-1.0, 4.0, 1.0])
            .y_range([-1.0, 3.0, 1.0])
            .width(5.0)
            .height(4.0)
            .axis_config(AxisConfig {
                include_tip: Some(false),
                include_ticks: Some(false),
                ..AxisConfig::default()
            })
            .build(&FontBook::bundled().unwrap())?;
        let root = stage.add(axes.clone())?;
        let raw = stage.scene_mut().stage_mut();
        let live = axes.bind(raw, root)?;
        let transform = Placement::new(
            [[0.5, 0.25, 0.0], [0.0, 0.5, 0.0], [0.0, 0.0, 1.0]],
            [-0.75, -0.5, 0.0],
        );
        raw.apply_affine(root, transform);
        // This oracle knows the authored affine map, not the live chart inverse.
        let point = |x, y| transform.apply_point(axes.c2p(&[x, y]));
        let curve = VMobject::from_points(vec![point(0.0, 1.0), point(0.0, 2.0), point(4.0, 3.0)]);
        let area = if self.oracle {
            // x=4t²,y=1+2t cut at .25..2.25 -> t=.25...75.
            let mut path =
                QuadPath::from_points(vec![point(0.25, 1.5), point(0.75, 2.0), point(2.25, 2.5)])
                    .unwrap();
            path.add_line_to(point(2.25, 0.0), false).unwrap();
            path.add_line_to(point(0.25, 0.0), false).unwrap();
            path.add_line_to(point(0.25, 1.5), false).unwrap();
            VMobject::from_path(&path)
                .with_style(Style::default().stroke_width(0.0).fill(BLUE, 0.5))
        } else {
            live.get_area_under_graph(raw, &curve, Some([0.25, 2.25]))?
        };
        let config = RiemannConfig {
            colors: vec![GREEN],
            fill_opacity: 0.6,
            stroke_width: 0.0,
            ..RiemannConfig::default()
        };
        let rectangles = if self.oracle {
            let mut bins = Vec::new();
            for (left, right) in [(0.0, 0.5), (0.5, 1.0), (1.0, 1.25)] {
                let height = left - 0.75;
                let fill = if height < 0.0 {
                    config.negative_color
                } else {
                    GREEN
                };
                bins.push(
                    fmn::library::Polygon::new([
                        point(left, 0.0),
                        point(right, 0.0),
                        point(right, height),
                        point(left, height),
                    ])
                    .style(
                        Style::default()
                            .stroke(config.stroke_color, 0.0, 1.0)
                            .fill(fill, config.fill_opacity),
                    )
                    .build()
                    .with_stroke_behind(config.stroke_behind),
                );
            }
            fmn::library::v_group(bins)
        } else {
            live.riemann_rectangles_styled(
                raw,
                &|x| x - 0.75,
                Some([0.0, 1.25]),
                Some(0.5),
                "left",
                &config,
            )?
        };
        stage.add(area)?;
        stage.add(rectangles)?;
        stage.add(curve.with_style(Style::default().stroke(YELLOW, 2.0, 1.0)))?;
        stage.wait(0.5)?;
        Ok(())
    }
}
fn render(oracle: bool, threads: u32, slots: usize) -> Vec<Vec<u8>> {
    let fs = Arc::new(VirtualFs::new());
    let mut options = RenderOptions::new("/calculus").unwrap();
    options.config.camera.resolution = (96, 64);
    options.config.camera.fps = 8;
    options.config.sizes.frame_height = 4.0;
    options.config.determinism.mode = DeterminismMode::Certified;
    options.config.render.threads = ThreadPolicy::Fixed(threads);
    options.frames_in_flight = slots;
    let report = render_with_fs(&mut CalculusScene { oracle }, options, fs.clone()).unwrap();
    assert_eq!(report.artifact.frame_count, 4);
    let pipeline = report.frame_pipeline.unwrap();
    assert_eq!(pipeline.outstanding_slots, 0);
    assert!(pipeline.max_in_flight <= slots);
    (0..4)
        .map(|index| {
            fs.read(&Path::new("/calculus").join(format!("frame_{index:06}.png")))
                .unwrap()
        })
        .collect()
}
#[test]
fn moved_calculus_matches_analytic_geometry_through_public_png_output() {
    let expected = render(true, 1, 1);
    for (threads, slots) in [(1, 1), (4, 2), (16, 4)] {
        let actual = render(false, threads, slots);
        for (index, (a, b)) in actual.iter().zip(&expected).enumerate() {
            assert_eq!(a, b, "frame {index}, threads {threads}");
        }
    }
}
