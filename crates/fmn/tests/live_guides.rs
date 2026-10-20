//! Public PNG output from live guides versus independently placed line geometry.
use fmn::library::{AxisConfig, DashedLine, FontBook, Style};
use fmn::prelude::*;
use fmn_config::config::{DeterminismMode, ThreadPolicy};
use fmn_core::constants::GREY_A;
use fmn_mobject::Placement;
use fmn_platform::fs::{FileSystem, VirtualFs};
use std::{path::Path, sync::Arc};

struct GuidesScene {
    oracle: bool,
}
impl SceneConstruct for GuidesScene {
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
                [[0.5, 0.25, 0.0], [0.0, 0.5, 0.0], [0.0, 0.0, 1.0]],
                [-0.5, -0.25, 0.0],
            ),
        );
        for (i, x) in [0.5, 1.0, 1.5].into_iter().enumerate() {
            let y = x * x * 0.5;
            // Explicit authored matrix: no frame inverse or projection helper.
            let point = [-0.5 + 0.5 * x + 0.25 * y, -0.25 + 0.5 * y, 0.0];
            let (v, h) = if self.oracle {
                let vertical = [point[0], -0.25, 0.0];
                // y-axis direction (1,2): squared norm=5, dot=offset.x+2*offset.y.
                let t = ((point[0] + 0.5) + 2.0 * (point[1] + 0.25)) / 5.0;
                let horizontal = [-0.5 + t, -0.25 + 2.0 * t, 0.0];
                let style = Style::default().stroke(GREY_A, 2.0, 1.0);
                (
                    DashedLine::new(vertical, point).style(style).build()?,
                    DashedLine::new(horizontal, point).style(style).build()?,
                )
            } else {
                let raw = stage.scene_mut().stage_mut();
                (
                    live.get_v_line_to_graph(raw, x, &|t| t * t * 0.5)?,
                    live.get_h_line_to_graph(raw, x, &|t| t * t * 0.5)?,
                )
            };
            let v = stage.add(v)?;
            let h = stage.add(h)?;
            let dot = stage.add(Dot::new().point(point).radius(0.05).color(YELLOW))?;
            stage.wait(0.25)?;
            if i < 2 {
                let raw = stage.scene_mut().stage_mut();
                raw.remove_from_scene(v);
                raw.remove_from_scene(h);
                raw.remove_from_scene(dot);
            }
        }
        Ok(())
    }
}
fn export(oracle: bool, threads: u32, slots: usize) -> Vec<Vec<u8>> {
    let fs = Arc::new(VirtualFs::new());
    let mut options = RenderOptions::new("/guides").unwrap();
    options.config.camera.resolution = (96, 64);
    options.config.camera.fps = 8;
    options.config.sizes.frame_height = 4.0;
    options.config.determinism.mode = DeterminismMode::Certified;
    options.config.render.threads = ThreadPolicy::Fixed(threads);
    options.frames_in_flight = slots;
    let report = render_with_fs(&mut GuidesScene { oracle }, options, fs.clone()).unwrap();
    assert_eq!(report.artifact.frame_count, 6);
    let stats = report.frame_pipeline.unwrap();
    assert_eq!(
        (stats.submitted, stats.emitted, stats.outstanding_slots),
        (6, 6, 0)
    );
    assert!(stats.max_in_flight <= slots);
    (0..6)
        .map(|i| {
            fs.read(&Path::new("/guides").join(format!("frame_{i:06}.png")))
                .unwrap()
        })
        .collect()
}
#[test]
fn live_guides_match_analytic_pngs_at_every_worker_count() {
    let expected = export(true, 1, 1);
    assert!(expected.windows(2).any(|pair| pair[0] != pair[1]));
    for (threads, slots) in [(1, 1), (4, 2), (16, 4)] {
        assert_eq!(export(false, threads, slots), expected, "threads={threads}");
    }
}
