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

/// Two native animations share one observation after the source is deleted.
/// The otherwise invisible updater gives the control the ordinary serial path.
struct CapturedMotionScene {
    force_serial: bool,
    path_materializations: u64,
}

impl fmn::SceneConstruct for CapturedMotionScene {
    fn construct(&mut self, stage: &mut fmn::Stage<'_>) -> fmn::Result<()> {
        use fmn_anim::{AnimConfig, AnimationGroup, MoveAlongPath, RateFunc};
        use fmn_core::constants::{BLUE, RED};
        use fmn_library::Circle;

        let path = stage.arena_mut().add(Mobject::from_points(&[
            [-2.0, -0.75, 0.0],
            [-1.9, -0.5, 0.0],
            [-1.75, -0.25, 0.0],
            [0.0, 2.25, 0.0],
            [2.0, 0.0, 0.0],
        ]));
        let mut sampler = PathSampler::default();
        let captured = sampler.observe(stage.arena(), path)?;
        self.path_materializations = sampler.materializations();
        stage.arena_mut().delete(path)?;
        let mut animations: Vec<Box<dyn fmn_anim::Animation>> = Vec::new();
        for (index, color) in [BLUE, RED].into_iter().enumerate() {
            let moving = stage.add(Circle::new().radius(0.18).color(color))?;
            stage.set_fill(moving, Some(color), Some(1.0), None, true);
            stage.set_stroke(moving, None, Some(0.0), None, None, true);
            animations.push(Box::new(
                MoveAlongPath::from_observation(moving, captured.clone()).with_config(AnimConfig {
                    run_time_frames: Some(if index == 0 { 5 } else { 7 }),
                    rate_func: RateFunc::linear(),
                    ..AnimConfig::default()
                }),
            ));
        }
        if self.force_serial {
            let sentinel = stage.add(Mobject::new())?;
            stage
                .arena_mut()
                .add_dt_updater(sentinel, |_, _, _| {}, false)?;
        }
        let group = AnimationGroup::with_lag_ratio(stage.arena_mut(), animations, 0.25)?
            .with_run_time_frames(8);
        stage.play(group)?;
        Ok(())
    }
}

#[derive(Default)]
struct MotionDispatch {
    programs: u64,
    frames: u64,
    serial_captures: u64,
}

impl fmn_scene::SceneSink for MotionDispatch {
    fn accepts_pure_segments(&self) -> bool {
        true
    }

    fn capture_pure_segment(
        &mut self,
        segment: &fmn_anim::pure_frame::PureSegment,
    ) -> Result<(), fmn_scene::IntegrationError> {
        self.programs += 1;
        self.frames += segment.frame_count().cast_unsigned();
        Ok(())
    }

    fn capture(
        &mut self,
        _: fmn_scene::CaptureReason,
        _: fmn_anim::FramePacket,
    ) -> Result<(), fmn_scene::IntegrationError> {
        self.serial_captures += 1;
        Ok(())
    }
}

fn motion_options(directory: &str, threads: u32) -> Result<fmn::RenderOptions, ScenarioError> {
    let mut options = fmn::RenderOptions::new(directory).map_err(motion_failure)?;
    options.format = fmn::RenderFormat::PngSequence;
    options.config.camera.resolution = (48, 32);
    options.config.camera.fps = 8;
    options.config.sizes.frame_height = 4.0;
    options.config.determinism.mode = fmn_config::config::DeterminismMode::Certified;
    options.config.render.threads = fmn_config::config::ThreadPolicy::Fixed(threads);
    options.frames_in_flight = 2;
    Ok(options)
}

fn motion_failure(error: impl std::fmt::Display) -> ScenarioError {
    super::fail(format!("captured path motion: {error}"))
}

fn motion_frames(
    fs: &fmn_platform::fs::VirtualFs,
    directory: &str,
) -> Result<Vec<Vec<u8>>, ScenarioError> {
    use fmn_platform::fs::FileSystem;

    (0..8)
        .map(|index| {
            fs.read(&std::path::Path::new(directory).join(format!("frame_{index:06}.png")))
                .map_err(motion_failure)
        })
        .collect()
}

pub(super) fn run_animation(ctx: &mut RunCtx) -> Result<RunOutcome, ScenarioError> {
    ctx.set_fps((8, 1));
    let mut scene = CapturedMotionScene {
        force_serial: false,
        path_materializations: 0,
    };
    let mut dispatch = MotionDispatch::default();
    fmn::run_scene(
        &mut scene,
        fmn_scene::RuntimeConfig {
            fps: 8,
            ..fmn_scene::RuntimeConfig::default()
        },
        37,
        &mut dispatch,
    )
    .map_err(motion_failure)?;
    if (dispatch.programs, dispatch.frames, dispatch.serial_captures) != (1, 8, 0)
        || scene.path_materializations != 1
    {
        return Err(super::fail(
            "captured path motion did not share one observation in one native pure program",
        ));
    }

    let fs = Arc::new(fmn_platform::fs::VirtualFs::new());
    scene.force_serial = true;
    let serial = fmn::render_with_fs(&mut scene, motion_options("/serial", 1)?, fs.clone())
        .map_err(motion_failure)?;
    if serial.artifact.frame_count != 8 {
        return Err(super::fail("serial captured motion lost nominal frames"));
    }
    let expected = motion_frames(&fs, "/serial")?;
    let first = fmn_codec::decode_png(&expected[0], &fmn_codec::PngLimits::default())
        .map_err(motion_failure)?;
    let last = fmn_codec::decode_png(&expected[7], &fmn_codec::PngLimits::default())
        .map_err(motion_failure)?;
    if first.rgba == last.rgba
        || first
            .rgba
            .chunks_exact(4)
            .all(|pixel| pixel == &first.rgba[..4])
    {
        return Err(super::fail(
            "captured motion pixel control did not discriminate",
        ));
    }

    scene.force_serial = false;
    for threads in [1, 4, 16] {
        let directory = format!("/captured-{threads}");
        let actual =
            fmn::render_with_fs(&mut scene, motion_options(&directory, threads)?, fs.clone())
                .map_err(motion_failure)?;
        let stats = actual
            .frame_pipeline
            .ok_or_else(|| super::fail("captured motion did not use the bounded frame pipeline"))?;
        if actual.artifact.frame_count != 8
            || (stats.submitted, stats.emitted, stats.outstanding_slots) != (8, 8, 0)
            || stats.max_in_flight > actual.execution_plan.frames_in_flight
            || scene.path_materializations != 1
            || motion_frames(&fs, &directory)? != expected
        {
            return Err(super::fail(format!(
                "captured motion changed serial pixels or frame accounting at {threads} workers"
            )));
        }
    }
    ctx.event(
        LogEvent::new("e2e.lifecycle.captured_path_motion")
            .field("source_removed", "true")
            .field("exact_frames", "true")
            .field("programs", dispatch.programs)
            .field("frames", dispatch.frames)
            .field("comparisons", 24_u64)
            .field("path_materializations", scene.path_materializations),
    );
    Ok(RunOutcome::ok()
        .with_artifact("captured-motion.png", expected[3].clone())
        .with_counter("captured_motion_programs", dispatch.programs)
        .with_counter("captured_motion_frames", dispatch.frames)
        .with_counter("captured_motion_thread_configs", 3)
        .with_counter("captured_motion_compared_frames", 24)
        .with_counter(
            "captured_motion_path_materializations",
            scene.path_materializations,
        ))
}
