//! Real Atlas circles and arcs entering Lumen's conservative stroke shortcut.
//!
//! The engine's paired tests prove exact general-route equality. This target
//! checks the separate library-to-record-to-plan boundary, including ordinary
//! default component counts and untracked writes through live point views.

use fmn_core::color::LinearRgba;
use fmn_library::{Arc, Circle};
use fmn_mobject::{JointType, Mob, Mobject, Stage};
use fmn_render::fill::MonoTable;
use fmn_render::{
    AaPolicy, Binning, EngineIdentity, FrameConfig, FrameJob, RenderPlan, RetainedFrameRenderer,
    RetainedFrameRendererConfig, ScreenMap, Tiling, Viewport,
};

fn config(aa: AaPolicy) -> FrameConfig {
    FrameConfig::new(
        Viewport {
            width: 256,
            height: 192,
        },
        ScreenMap {
            scale: 48.0,
            origin: [128.125, 96.375],
            y_up: true,
        },
        LinearRgba {
            r: 0.01,
            g: 0.025,
            b: 0.04,
            a: 1.0,
        },
    )
    .with_aa_policy(aa)
}

fn tiling() -> Tiling {
    Tiling {
        macro_tile: 32,
        fine_tile: 16,
    }
}

fn insert(stage: &mut Stage, object: Mobject) -> Mob {
    let mob = stage.add(object);
    stage.add_to_scene(mob).unwrap();
    mob
}

fn fixture() -> Stage {
    let mut stage = Stage::new();
    insert(
        &mut stage,
        Circle::new().arc_center([-1.0, 0.0, 0.0]).build().into(),
    );
    insert(
        &mut stage,
        Arc::new()
            .arc_center([1.0, -0.3, 0.0])
            .build()
            .unwrap()
            .into(),
    );
    insert(
        &mut stage,
        Arc::new()
            .start_angle(5.9)
            .angle(-4.8)
            .arc_center([0.2, 0.2, 0.0])
            .build()
            .unwrap()
            .into(),
    );
    stage
}

fn prepare(stage: &Stage, config: FrameConfig) -> (RenderPlan, MonoTable, Binning) {
    let mut plan = RenderPlan::new();
    plan.sync(stage, 0).unwrap();
    let mono = MonoTable::build(&plan, config.map).unwrap();
    let bins = Binning::build(&plan, config.viewport, tiling(), config.map).unwrap();
    (plan, mono, bins)
}

#[test]
fn circular_stroke_atlas_constructors_reach_fast_route_with_exact_worker_parity() {
    let stage = fixture();
    for aa in [AaPolicy::Adaptive, AaPolicy::Ssaa2x, AaPolicy::Ssaa4x] {
        let config = config(aa);
        let (plan, mono, bins) = prepare(&stage, config);
        for (identity, expected_count) in [
            (EngineIdentity::certified(), 0),
            (EngineIdentity::fast(), 3),
        ] {
            let job = FrameJob::with_identity(&plan, &mono, &bins, config, identity).unwrap();
            assert_eq!(
                job.circular_stroke_count(),
                expected_count,
                "{identity:?}, {aa:?}"
            );
            let (first, first_stats) = job.render_with_stats(1).unwrap();
            for threads in [4, 16] {
                let (frame, stats) = job.render_with_stats(threads).unwrap();
                assert!(
                    frame.as_bytes() == first.as_bytes(),
                    "Atlas pixels changed at {threads} workers, {identity:?}, {aa:?}"
                );
                assert_eq!(stats, first_stats);
            }
        }
    }
    // The ordinary unit-radius/default16 circle is admitted at the Reference's
    // 1080p scale too. This is not limited to oversampled or tiny circles.
    let mut stage = Stage::new();
    insert(&mut stage, Circle::new().build().into());
    let mut full_scale = config(AaPolicy::Adaptive);
    full_scale.map.scale = 135.0;
    let (plan, mono, bins) = prepare(&stage, full_scale);
    let job =
        FrameJob::with_identity(&plan, &mono, &bins, full_scale, EngineIdentity::fast()).unwrap();
    assert_eq!(job.circular_stroke_count(), 1);
}

#[test]
fn circular_stroke_atlas_live_views_rebuild_the_current_retained_frame() {
    let mut stage = Stage::new();
    let mob = insert(&mut stage, Circle::new().build().into());
    let points = stage
        .get_mut(mob)
        .unwrap()
        .buffer
        .export_field_view("point", true)
        .unwrap();
    let revision = stage.get(mob).unwrap().buffer.field_revision("point");
    let config = config(AaPolicy::Adaptive);
    let mut retained = RetainedFrameRenderer::new(RetainedFrameRendererConfig {
        frame: config,
        tiling: tiling(),
        engine: EngineIdentity::fast(),
        threads: 4,
    })
    .unwrap();
    let mut previous: Option<Vec<u8>> = None;
    for edit in 0..4 {
        if edit > 0 {
            for index in 0..points.len() {
                let mut point = points.read(index, "point").unwrap();
                match edit {
                    1 => point[0] += 0.125,
                    2 if index == 1 => point[1] += 0.3,
                    3 => point[0] += 0.3 * point[1],
                    _ => {}
                }
                assert!(points.write_foreign(index, "point", &point));
            }
        }
        assert_eq!(
            stage.get(mob).unwrap().buffer.field_revision("point"),
            revision
        );
        retained.render(&stage, 0).unwrap();
        let (plan, mono, bins) = prepare(&stage, config);
        assert!(plan.shapes().instances()[0].volatile);
        let cold = FrameJob::with_identity(&plan, &mono, &bins, config, EngineIdentity::fast())
            .unwrap()
            .render(1)
            .unwrap();
        assert!(
            retained.frame().as_bytes() == cold.as_bytes(),
            "stale retained Atlas stroke after edit {edit}"
        );
        if let Some(previous) = &previous {
            assert!(
                cold.as_bytes() != previous.as_slice(),
                "edit {edit} did not move the image"
            );
        }
        previous = Some(cold.as_bytes().to_vec());
    }
    drop(points);
}

#[test]
fn circular_stroke_atlas_profiles_and_join_overrides_keep_their_general_semantics() {
    for case in ["interior_width", "interior_rgba", "bevel", "miter"] {
        let mut stage = Stage::new();
        let mob = insert(&mut stage, Circle::new().build().into());
        match case {
            "interior_width" => {
                assert!(
                    stage
                        .get_mut(mob)
                        .unwrap()
                        .buffer
                        .write(7, "stroke_width", &[11.0])
                );
            }
            "interior_rgba" => {
                assert!(stage.get_mut(mob).unwrap().buffer.write(
                    7,
                    "stroke_rgba",
                    &[0.1, 0.8, 0.2, 0.4]
                ));
            }
            "bevel" => stage.uniforms_mut(mob).unwrap().joint_type = JointType::Bevel,
            "miter" => stage.uniforms_mut(mob).unwrap().joint_type = JointType::Miter,
            _ => unreachable!(),
        }
        let config = config(AaPolicy::Ssaa2x);
        let (plan, mono, bins) = prepare(&stage, config);
        let job =
            FrameJob::with_identity(&plan, &mono, &bins, config, EngineIdentity::fast()).unwrap();
        assert_eq!(job.circular_stroke_count(), 0, "{case}");
        let first = job.render(1).unwrap();
        assert!(
            first.as_bytes() == job.render(4).unwrap().as_bytes(),
            "{case}"
        );
    }
}
