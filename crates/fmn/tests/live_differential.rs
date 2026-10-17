//! Tangent and secant diagrams render as their independently authored geometry.
use fmn::library::{AxisConfig, FontBook, Line, Style, v_group};
use fmn::prelude::*;
use fmn_config::config::{DeterminismMode, ThreadPolicy};
use fmn_mobject::Placement;
use fmn_platform::fs::{FileSystem, VirtualFs};
use std::{path::Path, sync::Arc};

struct Diagram {
    oracle: bool,
}
impl SceneConstruct for Diagram {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        let axes = Axes::new()
            .x_range([-2.0, 2.0, 1.0])
            .y_range([-2.0, 2.0, 1.0])
            .width(4.0)
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
        raw.apply_affine(
            root,
            Placement::new(
                [[0.5, 0.25, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
                [-0.5, -0.5, 0.0],
            ),
        );
        for (i, x) in [-0.5, 0.0, 0.5].into_iter().enumerate() {
            let point = |x: f64, y: f64| [-0.5 + 0.5 * x + 0.25 * y, -0.5 + y, 0.0];
            let (triangle, tangent) = if self.oracle {
                let p0 = point(x, x);
                let corner = point(x + 0.5, x);
                let p1 = point(x + 0.5, x + 0.5);
                let triangle = v_group([
                    Line::new(p0, corner)
                        .style(Style::default().stroke(YELLOW, 2.0, 1.0))
                        .build()?,
                    Line::new(corner, p1)
                        .style(Style::default().stroke(GREEN, 2.0, 1.0))
                        .build()?,
                    Line::new(p0, p1)
                        .style(Style::default().stroke(RED, 2.0, 1.0))
                        .build()?,
                ]);
                // Numeric f(x)=x maps to world direction (.75,1): a 3-4-5 triangle.
                // World length 2.5 therefore has half-displacement (.75,1).
                let tangent = Line::new(
                    [p0[0] - 0.75, p0[1] - 1.0, 0.0],
                    [p0[0] + 0.75, p0[1] + 1.0, 0.0],
                )
                .style(Style::default())
                .build()?;
                (triangle, tangent)
            } else {
                let raw = stage.scene_mut().stage_mut();
                (
                    live.get_secant_slope_group(raw, x, &|t| t, 0.5, None)?,
                    live.tangent_line_styled(raw, x, &|t| t, 2.5, 0.5, Style::default())?,
                )
            };
            let tangent = stage.add(tangent)?;
            let triangle = stage.add(triangle)?;
            stage.wait(0.25)?;
            if i < 2 {
                let raw = stage.scene_mut().stage_mut();
                raw.remove_from_scene(tangent);
                raw.remove_from_scene(triangle);
            }
        }
        Ok(())
    }
}
fn export(oracle: bool, threads: u32, slots: usize) -> Vec<Vec<u8>> {
    let fs = Arc::new(VirtualFs::new());
    let mut options = RenderOptions::new("/differential").unwrap();
    options.config.camera.resolution = (96, 64);
    options.config.camera.fps = 8;
    options.config.sizes.frame_height = 4.0;
    options.config.determinism.mode = DeterminismMode::Certified;
    options.config.render.threads = ThreadPolicy::Fixed(threads);
    options.frames_in_flight = slots;
    let report = render_with_fs(&mut Diagram { oracle }, options, fs.clone()).unwrap();
    assert_eq!(report.artifact.frame_count, 6);
    let stats = report.frame_pipeline.unwrap();
    assert_eq!(
        (stats.submitted, stats.emitted, stats.outstanding_slots),
        (6, 6, 0)
    );
    assert!(stats.max_in_flight <= slots);
    (0..6)
        .map(|i| {
            fs.read(&Path::new("/differential").join(format!("frame_{i:06}.png")))
                .unwrap()
        })
        .collect()
}
#[test]
fn moved_tangent_and_secant_diagrams_match_analytic_pngs_at_every_worker_count() {
    let expected = export(true, 1, 1);
    assert!(expected.windows(2).any(|pair| pair[0] != pair[1]));
    for (threads, slots) in [(1, 1), (4, 2), (16, 4)] {
        assert_eq!(export(false, threads, slots), expected, "threads={threads}");
    }
}
