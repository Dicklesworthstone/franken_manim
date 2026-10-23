//! Callback-free plays on the same bounded native pipeline as the Rust CLI.
//!
//! The owner retains begin/finish, camera callbacks, output publication and
//! soundtrack ownership. Only Choreo's already-admitted immutable jobs cross
//! this boundary, and every worker joins before authored code resumes.

use fmn::rendering::{
    NativeAdaptiveCamera, NativeAdaptiveRoutes, NativeFramePipeline, RenderError,
};
use fmn_anim::pure_frame::PureSegment;
use fmn_output::EmitterHandle;
use fmn_render::RetainedFrameRendererConfig;
use fmn_runtime::{ExecutionPlan, FrameStreamError, PipelineError, PipelineStats};
use fmn_scene::IntegrationError;

pub(super) struct Completed {
    pub alphas: Vec<f64>,
    pub next_sequence: u64,
    pub routes: NativeAdaptiveRoutes,
    pub pipeline: PipelineStats,
}

pub(super) fn render(
    plan: ExecutionPlan,
    config: RetainedFrameRendererConfig,
    output: EmitterHandle,
    segment: PureSegment,
    camera: NativeAdaptiveCamera,
    first_sequence: u64,
) -> Result<Completed, IntegrationError> {
    let error = |error: RenderError| IntegrationError::new("lumen", error.to_string());
    let mut pipeline =
        NativeFramePipeline::new(plan, config, None, output.clone()).map_err(error)?;
    let mut next_sequence = first_sequence;
    let mut alphas = Vec::new();
    let submitted = (|| {
        for frame in 1..=segment.frame_count() {
            let next = next_sequence
                .checked_add(1)
                .ok_or(RenderError::InvalidOptions("frame sequence exhausted"))?;
            let job = segment.job(frame).map_err(|_| {
                RenderError::InvalidOptions("native pure segment omitted its nominal sample")
            })?;
            // This is the exact Choreo sample, including fractional-duration
            // final frames. Never construct a second floating-point grid.
            let alpha = job.sample().alpha;
            pipeline.capture_pure_adaptive(job, next_sequence, camera.clone())?;
            alphas.push(alpha);
            next_sequence = next;
        }
        Ok::<(), RenderError>(())
    })();
    if let Err(admission) = submitted {
        // Wake output reservations before joining workers. A closed stream
        // can conceal a raster/encoding failure; recover that original cause,
        // preserving admission when the join reports only its cancellation.
        output.cancel();
        let cause = match pipeline.finish() {
            Err(RenderError::Pipeline(FrameStreamError::Pipeline(failure)))
                if matches!(&failure.error, PipelineError::Cancelled) =>
            {
                admission
            }
            Err(joined) => joined,
            Ok(_) => admission,
        };
        return Err(error(cause));
    }
    let pipeline = pipeline.finish().map_err(error)?;
    let routes = camera
        .routes()
        .map_err(|error| IntegrationError::new("lumen", error.to_string()))?;
    Ok(Completed {
        alphas,
        next_sequence,
        routes,
        pipeline,
    })
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
