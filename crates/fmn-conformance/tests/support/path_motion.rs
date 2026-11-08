//! Sequential path motion retains exact placements while reusing mover bounds.

use fmn_anim::animation::Animation as _;
use fmn_anim::{MoveAlongPath, RateFunc};
use fmn_conformance::e2e::{LogEvent, RunCtx, RunOutcome, ScenarioError};
use fmn_core::constants::ORIGIN;
use fmn_geom::QuadPath;
use fmn_mobject::bbox::BoxAccum;
use fmn_mobject::stage::{SURFACE_VALIDITY_FIELD, surface_sample_is_present};
use fmn_mobject::{BoundingBox, Mob, Mobject, Placement, Stage};

fn raw_bounds(stage: &Stage, mob: Mob) -> BoundingBox {
    let mut acc = BoxAccum::new();
    let mut has_points = false;
    for member in stage.family(mob) {
        if let Some(entry) = stage.get(member)
            && let Some(points) = entry.buffer.read_column("point")
        {
            let validity = entry.buffer.read_column(SURFACE_VALIDITY_FIELD);
            for (index, point) in points.as_chunks::<3>().0.iter().enumerate() {
                if surface_sample_is_present(validity.as_deref(), index) {
                    has_points = true;
                    acc.push(entry.placement().apply_point(point.map(f64::from)));
                }
            }
        }
    }
    if !has_points && let Some(entry) = stage.get(mob) {
        acc.push(entry.placement().apply_point(ORIGIN));
    }
    acc.finish()
}

fn box_bits(value: BoundingBox) -> [[u64; 3]; 3] {
    [value.min, value.mid, value.max].map(|point| point.map(f64::to_bits))
}

fn placement_bits(stage: &Stage, mob: Mob) -> Result<[[u64; 3]; 4], ScenarioError> {
    let placement = stage
        .get(mob)
        .ok_or_else(|| super::fail("motion family member disappeared"))?
        .placement();
    let linear = placement.linear();
    Ok([linear[0], linear[1], linear[2], placement.translation()].map(|row| row.map(f64::to_bits)))
}

pub(super) fn run(ctx: &mut RunCtx) -> Result<RunOutcome, ScenarioError> {
    let mut stage = Stage::new();
    let path = stage.add(Mobject::from_points(&[
        [0.0, 0.0, 0.0],
        [0.5, 1.0, 0.0],
        [1.0, 1.5, 0.0],
        [1.5, 2.0, 0.0],
        [5.0, 0.0, 0.0],
    ]));
    let moving = stage.add(Mobject::new());
    let fraction = stage.add(Mobject::from_points(&[[0.1, 0.0, 0.0], [0.3, 0.0, 0.0]]));
    let dense_points: Vec<_> = (0..257_u16)
        .map(|index| {
            let x = f64::from(index) / 257.0;
            [x, x * x, x / 7.0]
        })
        .collect();
    let dense = stage.add(Mobject::from_points(&dense_points));
    for child in [fraction, dense] {
        stage
            .attach(moving, child)
            .map_err(|error| super::fail(format!("motion family: {error}")))?;
    }
    stage
        .set_placement(
            dense,
            Placement::new(
                [[1.5, 0.25, 0.0], [0.0, 0.5, 0.125], [0.0, 0.0, 2.0]],
                [-1.0, -0.5, 0.25],
            ),
        )
        .map_err(|error| super::fail(format!("moving placement: {error}")))?;

    let mut motion = MoveAlongPath::new(moving, path);
    motion.state_mut().config.rate_func = RateFunc::linear();
    motion
        .begin(&mut stage)
        .map_err(|error| super::fail(format!("motion begin: {error}")))?;
    let mut oracle = stage.snapshot().materialize();
    let family = stage.family(moving);
    let warm = stage.linear_bounds_stats().geometry_materializations;
    let mut frames = 0_u64;
    for frame in 0..65_u16 {
        if frame == 16
            && !stage
                .get_mut(path)
                .ok_or_else(|| super::fail("motion path disappeared"))?
                .buffer
                .write(1, "point", &[0.75, 1.25, 0.0])
        {
            return Err(super::fail("motion path point edit failed"));
        }
        if frame == 32 {
            stage
                .set_placement(path, Placement::from_translation([0.25, -0.5, 0.125]))
                .map_err(|error| super::fail(format!("motion path placement: {error}")))?;
        }
        let alpha = f64::from(frame) / 64.0;
        let cold_path = QuadPath::from_points(
            stage
                .get_points(path)
                .ok_or_else(|| super::fail("motion path lost its points"))?,
        )
        .map_err(|error| super::fail(format!("cold motion path: {error}")))?;
        let target = cold_path
            .point_from_proportion(alpha)
            .ok_or_else(|| super::fail("cold motion sample failed"))?;
        let center = raw_bounds(&oracle, moving).mid;
        let delta = std::array::from_fn(|axis| target[axis] - center[axis]);
        oracle.shift(moving, delta);
        motion.interpolate(&mut stage, alpha);
        if let Some(error) = motion.deferred_error() {
            return Err(super::fail(format!("motion frame {frame}: {error}")));
        }
        if box_bits(stage.get_bounding_box(moving)) != box_bits(raw_bounds(&oracle, moving)) {
            return Err(super::fail(format!(
                "motion bounds changed bits at frame {frame}"
            )));
        }
        for &member in &family {
            if placement_bits(&stage, member)? != placement_bits(&oracle, member)? {
                return Err(super::fail(format!(
                    "motion placement changed bits at frame {frame}"
                )));
            }
        }
        let stats = stage.linear_bounds_stats();
        if stats.geometry_materializations != warm || stats.fallback_queries != 0 {
            return Err(super::fail(format!(
                "translation motion rescanned geometry at frame {frame}: {stats:?}"
            )));
        }
        frames += 1;
    }
    // finish() performs its own final interpolation. Check that last state
    // transition too: repeated endpoint motion still has historical rounding.
    let final_path = QuadPath::from_points(
        stage
            .get_points(path)
            .ok_or_else(|| super::fail("motion path lost its final points"))?,
    )
    .map_err(|error| super::fail(format!("cold final path: {error}")))?;
    let target = final_path
        .point_from_proportion(motion.state().config.final_alpha_value)
        .ok_or_else(|| super::fail("cold final sample failed"))?;
    let center = raw_bounds(&oracle, moving).mid;
    oracle.shift(
        moving,
        std::array::from_fn(|axis| target[axis] - center[axis]),
    );
    motion.finish(&mut stage);
    if let Some(error) = motion.deferred_error() {
        return Err(super::fail(format!("motion finish: {error}")));
    }
    if box_bits(stage.get_bounding_box(moving)) != box_bits(raw_bounds(&oracle, moving)) {
        return Err(super::fail("motion finish changed bounding-box bits"));
    }
    for &member in &family {
        if placement_bits(&stage, member)? != placement_bits(&oracle, member)? {
            return Err(super::fail("motion finish changed placement bits"));
        }
    }
    let stats = stage.linear_bounds_stats();
    let path_materializations = motion.path_materializations();
    if path_materializations != 3
        || stats.cache_hits == 0
        || stats.geometry_materializations != warm
        || stats.fallback_queries != 0
    {
        return Err(super::fail(
            "motion finish lost retained geometry or path reuse",
        ));
    }

    ctx.event(
        LogEvent::new("e2e.parity.retained_path_motion")
            .field("exact_placements", "true")
            .field("frames", frames)
            .field("warm_geometry_scans", 0_u64)
            .field("path_materializations", path_materializations),
    );
    ctx.counter("retained_motion_frames", frames);
    Ok(RunOutcome::ok()
        .with_counter("retained_motion_frames", frames)
        .with_counter("retained_motion_warm_geometry_scans", 0)
        .with_counter(
            "retained_motion_path_materializations",
            path_materializations,
        ))
}
