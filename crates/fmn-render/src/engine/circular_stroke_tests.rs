//! Circular saturation is an accelerator for the existing stroke solver.
//!
//! These tests retain the very same prepared general solver in the same
//! process. Clearing only its circular descriptor gives an independent route
//! comparison without a product switch, a different sampling policy, or a
//! reconstructed reference image.

use super::*;
use crate::fill::MonoTable;
use fmn_core::color::LinearRgba;
use fmn_geom::QuadPath;
use fmn_mobject::{JointType, Mob, Mobject, Placement, RecordBuffer, RecordSchema, Stage};

fn configuration() -> FrameConfig {
    FrameConfig::new(
        Viewport {
            width: 192,
            height: 144,
        },
        ScreenMap {
            scale: 48.0,
            origin: [96.375, 72.125],
            y_up: true,
        },
        LinearRgba {
            r: 0.015,
            g: 0.025,
            b: 0.04,
            a: 1.0,
        },
    )
}

fn tiling() -> Tiling {
    Tiling {
        macro_tile: 32,
        fine_tile: 16,
    }
}

fn arc_object(start: f64, angle: f64, radius: f64, center: [f64; 3]) -> Mobject {
    // This is Chisel's public constructor, including the same default density
    // and subsequent f32 narrowing used by the Atlas constructors. The separate
    // library integration target exercises those actual constructors too.
    let path = QuadPath::try_arc(start, angle, radius, center, None).unwrap();
    let mut records = RecordBuffer::new(RecordSchema::vmobject(), path.points().len()).unwrap();
    for (i, point) in path.points().iter().enumerate() {
        assert!(records.write(i, "point", &point.map(|v| v as f32)));
        assert!(records.write(i, "fill_rgba", &[0.0; 4]));
        assert!(records.write(i, "stroke_rgba", &[0.2, 0.65, 0.9, 0.8]));
        assert!(records.write(i, "stroke_width", &[8.0]));
    }
    Mobject::from_buffer(records)
}

fn add_arc(stage: &mut Stage, start: f64, angle: f64, radius: f64, center: [f64; 3]) -> Mob {
    let mob = stage.add(arc_object(start, angle, radius, center));
    stage.add_to_scene(mob).unwrap();
    mob
}

fn scene() -> Stage {
    let mut stage = Stage::new();
    add_arc(
        &mut stage,
        0.23,
        std::f64::consts::TAU,
        0.9,
        [-0.5, 0.0, 0.0],
    );
    let other = add_arc(
        &mut stage,
        1.17,
        -std::f64::consts::TAU,
        0.8,
        [0.5, 0.1, 0.0],
    );
    {
        let buffer = &mut stage.get_mut(other).unwrap().buffer;
        for i in 0..buffer.len() {
            assert!(buffer.write(i, "stroke_rgba", &[0.9, 0.2, 0.1, 0.65]));
        }
    }
    add_arc(
        &mut stage,
        -0.7,
        std::f64::consts::FRAC_PI_2,
        1.1,
        [-0.1, -0.3, 0.0],
    );
    add_arc(&mut stage, 5.7, -4.5, 0.65, [0.15, 0.3, 0.0]);
    stage
}

fn derive(stage: &Stage, config: FrameConfig) -> (RenderPlan, MonoTable, Binning) {
    let mut plan = RenderPlan::new();
    plan.sync(stage, 0).unwrap();
    let mono = MonoTable::build(&plan, config.map).unwrap();
    let bins = Binning::build(&plan, config.viewport, tiling(), config.map).unwrap();
    (plan, mono, bins)
}

fn general_only(mut job: FrameJob<'_>) -> FrameJob<'_> {
    let ArenaStorage::Owned(arena) = &mut job.arena else {
        panic!("the comparison job must own its prepared data");
    };
    for draw in arena.draws.slice_mut(job.draws).iter_mut().flatten() {
        if let Some(stroke) = &mut draw.stroke {
            stroke.circular = None;
        }
    }
    assert_eq!(job.circular_stroke_count(), 0);
    job
}

fn assert_same(a: &FrameBuffer, b: &FrameBuffer, label: &str) {
    assert_eq!(a.layout(), b.layout(), "{label}: frame layouts");
    assert!(a.as_bytes() == b.as_bytes(), "{label}: frame bytes differ");
}

#[test]
fn circular_stroke_preserves_general_pixels_sampling_and_worker_counts() {
    let stage = scene();
    for aa in [AaPolicy::Adaptive, AaPolicy::Ssaa2x, AaPolicy::Ssaa4x] {
        let config = configuration().with_aa_policy(aa);
        let (plan, mono, bins) = derive(&stage, config);
        for identity in [EngineIdentity::certified(), EngineIdentity::fast()] {
            let accelerated =
                FrameJob::with_identity(&plan, &mono, &bins, config, identity).unwrap();
            assert_eq!(
                accelerated.circular_stroke_count(),
                if identity.engine == EngineKind::FastCpu {
                    4
                } else {
                    0
                },
                "route admission for {identity:?}, {aa:?}",
            );
            let general = general_only(
                FrameJob::with_identity(&plan, &mono, &bins, config, identity).unwrap(),
            );
            let (expected, expected_stats) = general.render_with_stats(1).unwrap();
            for threads in [1, 4, 16] {
                let (actual, stats) = accelerated.render_with_stats(threads).unwrap();
                assert_same(
                    &actual,
                    &expected,
                    "circular saturation must preserve the existing solver",
                );
                assert_eq!(
                    stats, expected_stats,
                    "AA changed for {aa:?}, {threads} workers"
                );
            }
        }
    }
}

#[test]
fn circular_stroke_keeps_station_profiles_and_nonround_joins_on_general_solver() {
    for case in [
        "uniform",
        "no_joint",
        "interior_width",
        "interior_color",
        "miter",
        "bevel",
    ] {
        let mut stage = Stage::new();
        let mob = add_arc(&mut stage, 0.1, std::f64::consts::TAU, 0.9, [0.0; 3]);
        match case {
            "interior_width" => {
                // Endpoints alone are insufficient: their values still agree.
                assert!(
                    stage
                        .get_mut(mob)
                        .unwrap()
                        .buffer
                        .write(9, "stroke_width", &[16.0])
                );
            }
            "interior_color" => {
                assert!(stage.get_mut(mob).unwrap().buffer.write(
                    9,
                    "stroke_rgba",
                    &[0.8, 0.1, 0.4, 0.3]
                ));
            }
            "no_joint" => stage.uniforms_mut(mob).unwrap().joint_type = JointType::NoJoint,
            "miter" => stage.uniforms_mut(mob).unwrap().joint_type = JointType::Miter,
            "bevel" => stage.uniforms_mut(mob).unwrap().joint_type = JointType::Bevel,
            _ => {}
        }
        let config = configuration().with_aa_policy(AaPolicy::Ssaa2x);
        let (plan, mono, bins) = derive(&stage, config);
        let accelerated =
            FrameJob::with_identity(&plan, &mono, &bins, config, EngineIdentity::fast()).unwrap();
        assert_eq!(
            accelerated.circular_stroke_count(),
            usize::from(matches!(case, "uniform" | "no_joint")),
            "{case}"
        );
        let general = general_only(
            FrameJob::with_identity(&plan, &mono, &bins, config, EngineIdentity::fast()).unwrap(),
        );
        assert_same(
            &accelerated.render(4).unwrap(),
            &general.render(1).unwrap(),
            case,
        );
    }
}

#[test]
fn circular_stroke_reads_live_foreign_edits_and_preserves_frozen_jobs() {
    let mut stage = Stage::new();
    let mob = add_arc(&mut stage, 0.17, std::f64::consts::TAU, 0.85, [0.0; 3]);
    let view = stage
        .get_mut(mob)
        .unwrap()
        .buffer
        .export_field_view("point", true)
        .unwrap();
    let revision = stage.get(mob).unwrap().buffer.field_revision("point");
    let config = configuration().with_aa_policy(AaPolicy::Ssaa4x);
    let frozen = stage.snapshot().materialize();
    let (old_plan, old_mono, old_bins) = derive(&frozen, config);
    let old_job = FrameJob::with_identity(
        &old_plan,
        &old_mono,
        &old_bins,
        config,
        EngineIdentity::fast(),
    )
    .unwrap();
    assert_eq!(old_job.circular_stroke_count(), 1);
    let before = old_job.render(1).unwrap();
    let mut previous = before.as_bytes().to_vec();
    for edit in 0..3 {
        for index in 0..view.len() {
            let mut point = view.read(index, "point").unwrap();
            match edit {
                0 => {
                    point[0] += 0.125;
                    point[1] -= 0.0625;
                }
                1 if index == 1 => {
                    point[0] += 0.18;
                    point[1] += 0.21;
                }
                2 => point[0] += 0.35 * point[1],
                _ => {}
            }
            assert!(view.write_foreign(index, "point", &point));
        }
        assert_eq!(
            stage.get(mob).unwrap().buffer.field_revision("point"),
            revision
        );
        let current = stage.snapshot().materialize();
        let (plan, mono, bins) = derive(&current, config);
        assert!(plan.shapes().instances()[0].volatile);
        let accelerated =
            FrameJob::with_identity(&plan, &mono, &bins, config, EngineIdentity::fast()).unwrap();
        let general = general_only(
            FrameJob::with_identity(&plan, &mono, &bins, config, EngineIdentity::fast()).unwrap(),
        );
        let expected = general.render(1).unwrap();
        assert!(
            expected.as_bytes() != previous.as_slice(),
            "edit {edit} did not alter the actual image"
        );
        for threads in [1, 4, 16] {
            assert_same(
                &accelerated.render(threads).unwrap(),
                &expected,
                "live circular geometry",
            );
        }
        previous = expected.as_bytes().to_vec();
        assert_same(
            &old_job.render(4).unwrap(),
            &before,
            "a frozen job observed later foreign writes",
        );
    }
    drop(view);
}

#[test]
fn circular_stroke_preparation_reuses_storage_and_observes_affine_changes() {
    let mut stage = Stage::new();
    let mob = add_arc(&mut stage, -0.2, 4.8, 0.9, [0.0; 3]);
    let config = configuration().with_aa_policy(AaPolicy::Adaptive);
    let mut arena = FrameArena::new();
    let mut frame = FrameBuffer::new(config.layout().unwrap());
    let mut warm = false;
    for placement in [
        Placement::IDENTITY,
        Placement::new(
            [[0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]],
            [0.1, -0.2, 0.0],
        ),
        Placement::new(
            [[-1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            [-0.2, 0.15, 0.0],
        ),
        Placement::new(
            [[1.0, 0.3, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            [0.0; 3],
        ),
        Placement::IDENTITY,
    ] {
        stage.set_placement(mob, placement).unwrap();
        let (plan, mono, bins) = derive(&stage, config);
        let general = general_only(
            FrameJob::with_identity(&plan, &mono, &bins, config, EngineIdentity::fast()).unwrap(),
        );
        let expected = general.render(1).unwrap();
        for repetition in 0..3 {
            let job = FrameJob::with_identity_in(
                &mut arena,
                &plan,
                &mono,
                &bins,
                config,
                EngineIdentity::fast(),
            )
            .unwrap();
            job.render_into(4, &mut frame).unwrap();
            assert_same(&frame, &expected, "reused circular descriptor arena");
            if repetition > 0 && warm {
                assert_eq!(job.allocation_stats().heap_allocs_this_frame, 0);
            }
            warm = true;
        }
    }
}

fn timing_scene(arcs: bool) -> Stage {
    let mut stage = Stage::new();
    for row in 0..5 {
        for col in 0..7 {
            add_arc(
                &mut stage,
                0.11 + 0.07 * f64::from(row),
                if arcs { -4.8 } else { std::f64::consts::TAU },
                0.24,
                [
                    -1.5 + 0.5 * f64::from(col),
                    -1.0 + 0.5 * f64::from(row),
                    0.0,
                ],
            );
        }
    }
    stage
}

/// Both routes are prepared by this build; only the accelerator binding is
/// cleared in the general reference. Report preparation separately so a raster
/// improvement is never presented as an old-implementation end-to-end speedup.
#[test]
#[ignore = "run in an optimized build for paired preparation and raster diagnostics"]
fn circular_stroke_interleaved_preparation_and_raster_timing() {
    use std::time::{Duration, Instant};
    const PAIRS: u32 = 8;
    for arcs in [false, true] {
        let stage = timing_scene(arcs);
        for aa in [AaPolicy::Adaptive, AaPolicy::Ssaa4x] {
            let config = configuration().with_aa_policy(aa);
            let (plan, mono, bins) = derive(&stage, config);
            let accelerated =
                FrameJob::with_identity(&plan, &mono, &bins, config, EngineIdentity::fast())
                    .unwrap();
            assert_eq!(accelerated.circular_stroke_count(), 35);
            let general = general_only(
                FrameJob::with_identity(&plan, &mono, &bins, config, EngineIdentity::fast())
                    .unwrap(),
            );
            assert_same(
                &accelerated.render(1).unwrap(),
                &general.render(1).unwrap(),
                "timed route pixels",
            );
            let mut frame = FrameBuffer::new(config.layout().unwrap());
            for _ in 0..3 {
                general.render_into(1, &mut frame).unwrap();
                accelerated.render_into(1, &mut frame).unwrap();
            }
            let mut preparation = Duration::ZERO;
            for _ in 0..PAIRS {
                let start = Instant::now();
                let job =
                    FrameJob::with_identity(&plan, &mono, &bins, config, EngineIdentity::fast())
                        .unwrap();
                preparation += start.elapsed();
                std::hint::black_box(job.circular_stroke_count());
            }
            let mut elapsed = [Duration::ZERO; 2];
            for pair in 0..PAIRS {
                for route in if pair % 2 == 0 { [0, 1] } else { [1, 0] } {
                    let start = Instant::now();
                    if route == 0 {
                        general.render_into(1, &mut frame).unwrap();
                    } else {
                        accelerated.render_into(1, &mut frame).unwrap();
                    }
                    elapsed[route] += start.elapsed();
                    std::hint::black_box(frame.as_bytes());
                }
            }
            println!(
                "circular stroke arcs={arcs} aa={aa:?} draws=35 pairs={PAIRS} threads=1 prepare_ms={:.4} general_raster_ms={:.4} accelerated_raster_ms={:.4} raster_ratio={:.5}",
                preparation.as_secs_f64() * 1000.0 / f64::from(PAIRS),
                elapsed[0].as_secs_f64() * 1000.0 / f64::from(PAIRS),
                elapsed[1].as_secs_f64() * 1000.0 / f64::from(PAIRS),
                elapsed[1].as_secs_f64() / elapsed[0].as_secs_f64(),
            );
        }
    }
}
