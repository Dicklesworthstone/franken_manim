//! Callback-free plays on the same bounded native pipeline as the Rust CLI.
//!
//! The owner retains begin/finish, camera callbacks, output publication and
//! soundtrack ownership. Only Choreo's already-admitted immutable jobs cross
//! this boundary, and every worker joins before authored code resumes.

use fmn::rendering::{
    NativeAdaptiveCamera, NativeAdaptiveRoutes, NativeFrameError, NativeFramePipeline, RenderError,
};
use fmn_anim::pure_frame::{PureFrameJob, PureSegment};
use fmn_output::{EmitterError, EmitterHandle};
use fmn_render::RetainedFrameRendererConfig;
use fmn_runtime::{ExecutionPlan, FrameStreamError, PipelineError, PipelineStats, TeamRole};
use fmn_scene::IntegrationError;

pub(super) struct Completed {
    pub alphas: Vec<f64>,
    pub next_sequence: u64,
    pub routes: NativeAdaptiveRoutes,
    pub pipeline: Option<PipelineStats>,
    pub owner_frames: u64,
    pub teams_used: u64,
}

pub(super) fn render<F>(
    mut plan: ExecutionPlan,
    config: RetainedFrameRendererConfig,
    output: EmitterHandle,
    segment: PureSegment,
    camera: NativeAdaptiveCamera,
    first_sequence: u64,
    mut capture_owner: F,
) -> Result<Completed, IntegrationError>
where
    F: FnMut(PureFrameJob, u64) -> Result<(), IntegrationError>,
{
    let error = |error: RenderError| IntegrationError::new("lumen", error.to_string());
    if plan.render_teams.is_empty() {
        return Err(IntegrationError::new(
            "lumen",
            "pure play has no render team",
        ));
    }
    // Use every admitted team for the prefix, including one-team plans:
    // bypassing its pipeline also loses preparation/conversion overlap.
    // Join before the owner renders the final sample, preserving its retained
    // state without reserving an entire team for one frame of a long play.
    // Rotate owner team zero to the last prefix slot, so even a one-frame
    // prefix uses another team when one exists. Roles remain pipeline-local.
    let mut pipeline = if segment.frame_count() > 1 {
        plan.render_teams.rotate_left(1);
        for (index, team) in plan.render_teams.iter_mut().enumerate() {
            team.role = TeamRole::Render(index);
        }
        let mut prefix_config = config;
        prefix_config.threads = plan.render_teams[0].threads();
        Some(NativeFramePipeline::new(plan, prefix_config, None, output.clone()).map_err(error)?)
    } else {
        None
    };
    let mut next_sequence = first_sequence;
    let mut alphas = Vec::new();
    let mut owner_frames = 0;
    let mut completed_pipeline = None;
    let submitted = (|| {
        for frame in 1..=segment.frame_count() {
            let next = next_sequence
                .checked_add(1)
                .ok_or_else(|| IntegrationError::new("lumen", "frame sequence exhausted"))?;
            let job = segment.job(frame).map_err(|_| {
                IntegrationError::new("lumen", "native pure segment omitted its nominal sample")
            })?;
            // This is the exact Choreo sample, including fractional-duration
            // final frames. Never construct a second floating-point grid.
            let alpha = job.sample().alpha;
            if frame != segment.frame_count()
                && let Some(pipeline) = &mut pipeline
            {
                pipeline
                    .capture_pure_adaptive(job, next_sequence, camera.clone())
                    .map_err(error)?;
            } else {
                completed_pipeline = pipeline
                    .take()
                    .map(NativeFramePipeline::finish)
                    .transpose()
                    .map_err(error)?;
                capture_owner(job, next_sequence)?;
                owner_frames += 1;
            }
            alphas.push(alpha);
            next_sequence = next;
        }
        Ok::<(), IntegrationError>(())
    })();
    if let Err(admission) = submitted {
        // Wake output reservations before joining workers. A closed stream
        // can conceal a raster/encoding failure; recover that original cause,
        // preserving admission when the join reports only its cancellation.
        output.cancel();
        if let Some(pipeline) = pipeline {
            return Err(match pipeline.finish() {
                Err(RenderError::Pipeline(FrameStreamError::Pipeline(failure)))
                    if secondary_cancellation(&failure.error, next_sequence) =>
                {
                    admission
                }
                Err(joined) => error(joined),
                Ok(_) => admission,
            });
        }
        return Err(admission);
    }
    let pipeline = completed_pipeline;
    let teams_used = pipeline
        .as_ref()
        .map_or(u64::from(owner_frames != 0), |stats| {
            let prefix = stats
                .render_team_frames
                .iter()
                .filter(|frames| **frames != 0)
                .count() as u64;
            let owner_missing = stats
                .render_team_frames
                .last()
                .is_none_or(|frames| *frames == 0);
            prefix + u64::from(owner_frames != 0 && owner_missing)
        });
    let routes = camera
        .routes()
        .map_err(|error| IntegrationError::new("lumen", error.to_string()))?;
    Ok(Completed {
        alphas,
        next_sequence,
        routes,
        pipeline,
        owner_frames,
        teams_used,
    })
}

// Dropping the owner's unpublished reservation records its abandonment in the
// output ring before cancellation reaches prefix emitters. That secondary
// failure must not replace the owner's actual render/conversion error. A
// different frame's abandonment or a real prefix stage failure stays visible.
fn secondary_cancellation(error: &PipelineError<NativeFrameError>, sequence: u64) -> bool {
    match error {
        PipelineError::Cancelled
        | PipelineError::Stage {
            source: NativeFrameError::Emitter(EmitterError::Cancelled),
            ..
        } => true,
        PipelineError::Stage {
            source:
                NativeFrameError::Emitter(EmitterError::ReservationAbandoned {
                    sequence: abandoned,
                }),
            ..
        } => *abandoned == sequence,
        _ => false,
    }
}

#[cfg(any(test, feature = "gauntlet"))]
pub(super) fn run_acceptance(directory: &std::path::Path) -> Result<(u64, u64, u64), String> {
    use pyo3::prelude::*;

    crate::with_python_test_module("native portal pure offload", |py, _module, globals| {
        let source = std::ffi::CString::new(include_str!("../tests/native_pure_offload.py"))
            .expect("native offload suite contains no NUL");
        py.run(&source, Some(globals), Some(globals))
            .inspect_err(|error| error.print(py))
            .map_err(|error| error.to_string())?;
        globals
            .get_item("run_native_pure_offload_acceptance")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "native offload acceptance entry point is missing".to_owned())?
            .call1((directory.to_string_lossy().as_ref(),))
            .inspect_err(|error| error.print(py))
            .and_then(|report| report.extract())
            .map_err(|error| error.to_string())
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn cancellation_keeps_the_owner_cause_and_preserves_real_prefix_failures() {
        use fmn::rendering::NativeFrameError;
        use fmn_output::EmitterError;
        use fmn_runtime::{PipelineError, PipelineStage};

        let emit = |source| PipelineError::Stage {
            sequence: Some(3),
            stage: PipelineStage::Emit,
            source: NativeFrameError::Emitter(source),
        };
        assert!(super::secondary_cancellation(&PipelineError::Cancelled, 4));
        assert!(super::secondary_cancellation(
            &emit(EmitterError::Cancelled),
            4
        ));
        assert!(super::secondary_cancellation(
            &emit(EmitterError::ReservationAbandoned { sequence: 4 }),
            4,
        ));
        assert!(!super::secondary_cancellation(
            &emit(EmitterError::ReservationAbandoned { sequence: 3 }),
            4,
        ));
        assert!(!super::secondary_cancellation(
            &PipelineError::Stage {
                sequence: Some(3),
                stage: PipelineStage::Raster,
                source: NativeFrameError::WorkerState("earlier raster failure"),
            },
            4,
        ));
    }

    #[test]
    fn real_portal_uses_native_teams_and_keeps_authored_observers() {
        let directory =
            std::env::temp_dir().join(format!("fmn-native-offload-test-{}", std::process::id()));
        std::fs::create_dir(&directory).expect("create a fresh native offload test directory");
        let result = super::run_acceptance(&directory);
        let _ = std::fs::remove_dir_all(&directory);
        let (pure, frames, fallbacks) = result.expect("native portal offload acceptance");
        assert!(pure > 0 && frames > pure);
        assert_eq!(fallbacks, 5);
    }
}
