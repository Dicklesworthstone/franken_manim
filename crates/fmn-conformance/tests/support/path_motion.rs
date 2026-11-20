//! Motion preserves exact placements and refuses invalid live samples.

use fmn_anim::animation::Animation as _;
use fmn_anim::{
    AnimConfig, AnimError, Homotopy, MaintainPositionRelativeTo, MoveAlongPath, PhaseFlow, RateFunc,
};
use fmn_conformance::e2e::{LogEvent, RunCtx, RunOutcome, ScenarioError};
use fmn_core::constants::ORIGIN;
use fmn_geom::QuadPath;
use fmn_mobject::bbox::BoxAccum;
use fmn_mobject::stage::{SURFACE_VALIDITY_FIELD, surface_sample_is_present};
use fmn_mobject::{BoundingBox, Mob, Mobject, Placement, Stage, StageError};
use fmn_scene::{PlayOverrides, RuntimeConfig, Scene, SceneError};

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

/// Reject a later family member's invalid map before publishing any part of
/// that sample. The previous successful frame remains the recoverable state,
/// including during skip, where no packets are captured.
pub(super) fn functional_motion_failures(ctx: &mut RunCtx) -> Result<RunOutcome, ScenarioError> {
    ctx.set_fps((4, 1));
    let mut modes = 0_u64;
    let mut emitted_frames = 0_u64;
    for flow in [false, true] {
        for skip in [false, true] {
            let mut scene = Scene::new(
                RuntimeConfig {
                    fps: 4,
                    skip_animations: skip,
                    preview_while_skipping: false,
                    ..RuntimeConfig::default()
                },
                123,
            )
            .map_err(|error| super::fail(format!("functional motion scene: {error}")))?;
            let moving = scene.stage_mut().add(Mobject::new());
            let first = scene
                .stage_mut()
                .add(Mobject::from_points(&[[0.25, 0.125, 0.0]]));
            let later = scene
                .stage_mut()
                .add(Mobject::from_points(&[[2.0, 0.25, 0.0]]));
            for member in [first, later] {
                scene
                    .stage_mut()
                    .attach(moving, member)
                    .map_err(|error| super::fail(format!("functional motion family: {error}")))?;
            }
            scene
                .add(&[moving])
                .map_err(|error| super::fail(format!("add functional motion: {error}")))?;
            let config = AnimConfig {
                run_time: 1.0,
                rate_func: RateFunc::linear(),
                suspend_mobject_updating: true,
                ..AnimConfig::default()
            };
            let animation: Box<dyn fmn_anim::Animation> = if flow {
                Box::new(
                    PhaseFlow::try_new(
                        |point| {
                            if point[0] > 2.1 {
                                Ok([f64::NAN, 0.0, 0.0])
                            } else {
                                Ok([1.0, 0.0, 0.0])
                            }
                        },
                        moving,
                        Some(1.0),
                    )
                    .with_config(config),
                )
            } else {
                Box::new(
                    Homotopy::try_new(
                        |x, y, z, alpha| {
                            if x > 1.5 && alpha >= 0.5 {
                                Ok([f64::NAN, y, z])
                            } else {
                                Ok([x + alpha, y, z])
                            }
                        },
                        moving,
                    )
                    .with_config(config),
                )
            };
            let mut sink = super::PacketSink::default();
            let result = scene.play(vec![animation], PlayOverrides::default(), &mut sink);
            if !matches!(
                result,
                Err(SceneError::Animation(AnimError::Stage(
                    StageError::InvalidPointMap(_)
                )))
            ) {
                return Err(super::fail(format!(
                    "invalid functional motion lost its typed refusal (flow={flow}, skip={skip}): {result:?}"
                )));
            }
            if scene.time().frames() != 1 || sink.packets.len() != usize::from(!skip) {
                return Err(super::fail(format!(
                    "invalid functional sample advanced or captured (flow={flow}, skip={skip}, time={}, captures={})",
                    scene.time().frames(),
                    sink.packets.len()
                )));
            }
            for (member, expected) in [(first, [0.5, 0.125, 0.0]), (later, [2.25, 0.25, 0.0])] {
                if scene.stage().get_points(member).as_deref() != Some(&[expected]) {
                    return Err(super::fail(format!(
                        "invalid later map partially changed its family (flow={flow}, skip={skip})"
                    )));
                }
            }
            if scene.stage().is_updating_suspended(moving) {
                return Err(super::fail(
                    "failed functional motion left its family suspended",
                ));
            }
            emitted_frames += sink.packets.len() as u64;
            modes += 1;
        }
    }
    ctx.event(
        LogEvent::new("e2e.failure.functional_motion")
            .field("modes_checked", modes)
            .field("emitted_frames", emitted_frames)
            .field("failed_sample_unchanged", "true"),
    );
    Ok(RunOutcome::ok()
        .with_counter("functional_motion_failure_modes", modes)
        .with_counter("functional_motion_failure_emitted_frames", emitted_frames))
}

/// Removing a tracked object between frames must not move its follower to
/// an invented origin. The stale dependency reaches the real Scene boundary.
pub(super) fn tracked_position_failure(ctx: &mut RunCtx) -> Result<RunOutcome, ScenarioError> {
    ctx.set_fps((4, 1));
    let mut emitted_frames = 0_u64;
    for skip in [false, true] {
        let mut scene = Scene::new(
            RuntimeConfig {
                fps: 4,
                skip_animations: skip,
                preview_while_skipping: false,
                ..RuntimeConfig::default()
            },
            123,
        )
        .map_err(|error| super::fail(format!("tracked motion scene: {error}")))?;
        let moving = scene
            .stage_mut()
            .add(Mobject::from_points(&[[3.0, 2.0, 0.0]]));
        let tracked = scene
            .stage_mut()
            .add(Mobject::from_points(&[[1.0, 1.0, 0.0]]));
        let controller = scene.stage_mut().add(Mobject::new());
        scene
            .add(&[moving, tracked, controller])
            .map_err(|error| super::fail(format!("add tracked actors: {error}")))?;
        scene
            .stage_mut()
            .add_updater(
                controller,
                move |stage, _| {
                    if stage.time() > 0.0 && stage.contains(tracked) {
                        stage.delete(tracked).expect("the tracked object is live");
                    }
                },
                false,
            )
            .map_err(|error| super::fail(format!("attach tracked removal: {error}")))?;
        let animation = MaintainPositionRelativeTo::new(scene.stage(), moving, tracked)
            .with_config(AnimConfig {
                run_time: 1.0,
                suspend_mobject_updating: true,
                ..AnimConfig::default()
            });
        let mut sink = super::PacketSink::default();
        let result = scene.play(
            vec![Box::new(animation)],
            PlayOverrides::default(),
            &mut sink,
        );
        if !matches!(
            result,
            Err(SceneError::Animation(AnimError::StaleHandle(mob))) if mob == tracked
        ) {
            return Err(super::fail(format!(
                "removed tracked input did not reach Scene.play (skip={skip}): {result:?}"
            )));
        }
        if scene.time().frames() != 1 || sink.packets.len() != usize::from(!skip) {
            return Err(super::fail(format!(
                "failed follower advanced or captured (skip={skip}, time={}, captures={})",
                scene.time().frames(),
                sink.packets.len()
            )));
        }
        if scene.stage().get_center(moving) != [3.0, 2.0, 0.0]
            || scene.stage().is_updating_suspended(moving)
        {
            return Err(super::fail(
                "failed follower moved or retained its suspension",
            ));
        }
        emitted_frames += sink.packets.len() as u64;
    }
    ctx.event(
        LogEvent::new("e2e.failure.tracked_position")
            .field("modes_checked", 2_u64)
            .field("emitted_frames", emitted_frames)
            .field("follower_unchanged", "true"),
    );
    Ok(RunOutcome::ok()
        .with_counter("tracked_position_failure_modes", 2)
        .with_counter("tracked_position_failure_emitted_frames", emitted_frames))
}
