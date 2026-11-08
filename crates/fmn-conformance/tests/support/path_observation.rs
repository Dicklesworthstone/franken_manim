//! Owned world-space path sampling after the mutable scene has been destroyed.

use std::sync::Arc;
use std::thread;

use fmn_conformance::e2e::{LogEvent, RunCtx, RunOutcome, ScenarioError};
use fmn_geom::QuadPath;
use fmn_mobject::path_sampling::{PathObservation, PathSampler};
use fmn_mobject::{Mob, Mobject, Placement, Stage};

const ALPHAS: [f64; 7] = [0.0, 0.125, 0.25, 0.5, 0.75, 0.875, 1.0];

struct CapturedPath {
    observation: PathObservation,
    expected: [[u64; 3]; ALPHAS.len()],
}

fn capture(
    stage: &Stage,
    mob: Mob,
    sampler: &mut PathSampler,
) -> Result<CapturedPath, ScenarioError> {
    let points = stage
        .get_points(mob)
        .ok_or_else(|| super::fail("path observation lost its source points"))?;
    let path = QuadPath::from_points(points)
        .map_err(|error| super::fail(format!("cold path construction: {error}")))?;
    let mut expected = [[0; 3]; ALPHAS.len()];
    for (index, alpha) in ALPHAS.into_iter().enumerate() {
        // This kernel builds its own arc-length table on every query.
        expected[index] = path
            .point_from_proportion(alpha)
            .ok_or_else(|| super::fail("cold path sampling failed"))?
            .map(f64::to_bits);
    }
    let observation = sampler
        .observe(stage, mob)
        .map_err(|error| super::fail(format!("path observation: {error}")))?;
    if observation.point_count() != 5
        || observation.total_length().to_bits() != path.get_arc_length().to_bits()
    {
        return Err(super::fail(
            "path observation changed its geometry or length",
        ));
    }
    Ok(CapturedPath {
        observation,
        expected,
    })
}

pub(super) fn run(ctx: &mut RunCtx) -> Result<RunOutcome, ScenarioError> {
    let mut stage = Stage::new();
    let mob = stage.add(Mobject::from_points(&[
        [-2.0, -1.0, 0.0],
        [0.0, 3.0, 2.0],
        [2.0, 0.0, 1.0],
        [4.0, -2.0, 0.0],
        [6.0, 1.0, -1.0],
    ]));
    let mut sampler = PathSampler::default();
    let mut captured = vec![capture(&stage, mob, &mut sampler)?];
    stage
        .set_placement(
            mob,
            Placement::new(
                [[1.5, 0.25, 0.0], [0.0, 0.5, 0.125], [0.0, 0.0, 2.0]],
                [4.0, -2.0, 1.0],
            ),
        )
        .map_err(|error| super::fail(format!("path placement: {error}")))?;
    captured.push(capture(&stage, mob, &mut sampler)?);
    if !stage
        .get_mut(mob)
        .ok_or_else(|| super::fail("path disappeared before point edit"))?
        .buffer
        .write(2, "point", &[3.5, 1.25, 0.5])
    {
        return Err(super::fail("path point edit failed"));
    }
    captured.push(capture(&stage, mob, &mut sampler)?);
    if captured[0].expected == captured[1].expected || captured[1].expected == captured[2].expected
    {
        return Err(super::fail(
            "path observation mutation drill did not discriminate",
        ));
    }
    let materializations = sampler.materializations();
    if materializations != 3 {
        return Err(super::fail(format!(
            "three path generations built {materializations} observations"
        )));
    }

    // Workers receive only immutable geometry and plain expected values.
    // No Stage, snapshot, record view, or live sampler remains.
    sampler.clear();
    drop(sampler);
    drop(stage);
    let captured = Arc::new(captured);
    let mut comparisons = 0_u64;
    for threads in [1, 4, 16] {
        let mut workers = Vec::new();
        for worker in 0..threads {
            let captured = Arc::clone(&captured);
            workers.push(thread::spawn(move || -> Result<u64, String> {
                let mut checked = 0;
                for path in captured.iter() {
                    let observation = path.observation.clone();
                    for offset in 0..ALPHAS.len() {
                        let index = (worker + offset * 3) % ALPHAS.len();
                        let point = observation
                            .sample(ALPHAS[index])
                            .map_err(|error| format!("worker path sampling: {error}"))?;
                        if point.map(f64::to_bits) != path.expected[index] {
                            return Err(format!(
                                "owned path changed sample {index} at {threads} workers"
                            ));
                        }
                        checked += 1;
                    }
                }
                Ok(checked)
            }));
        }
        for worker in workers {
            comparisons += worker
                .join()
                .map_err(|_| super::fail("owned path sampling worker panicked"))?
                .map_err(super::fail)?;
        }
    }
    ctx.event(
        LogEvent::new("e2e.lifecycle.owned_path_observations")
            .field("owner_dropped", "true")
            .field("exact_bits", "true")
            .field("generations", 3_u64)
            .field("samples", comparisons)
            .field("materializations", materializations),
    );
    ctx.counter("path_observation_samples", comparisons);
    Ok(RunOutcome::ok()
        .with_counter("path_observation_generations", 3)
        .with_counter("path_observation_thread_configs", 3)
        .with_counter("path_observation_samples", comparisons)
        .with_counter("path_observation_materializations", materializations))
}
