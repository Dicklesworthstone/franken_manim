//! Native CLI interpolation offload. Choreo owns the purity proof; the CLI
//! only admits its closed programs to the same bounded, ordered CPU pipeline.

use fmn::animation::pure_frame::{PureFrameJob, PureSegment};
use fmn_scene::IntegrationError;

use super::super::{CliError, OfflineFrameRenderer, RenderSink};
use super::CpuRenderer;

/// Joined-render diagnostics, not semantic renderer identity or input hashes.
/// Compiled FMTL reconstruction is counted separately from native interpolation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DispatchStats {
    pub(crate) segments: u64,
    pub(crate) pure_segments: u64,
    pub(crate) pure_frames: u64,
    pub(crate) compiled_frames: u64,
}

impl DispatchStats {
    pub(crate) fn begin_segment(&mut self) -> Result<(), IntegrationError> {
        self.segments = increment(self.segments)?;
        Ok(())
    }

    pub(crate) fn compiled_frame(&mut self) -> Result<(), IntegrationError> {
        self.compiled_frames = increment(self.compiled_frames)?;
        Ok(())
    }

    pub(crate) fn validate(&self, frames: u64) -> Result<(), CliError> {
        if self.pure_segments > self.segments
            || self.pure_frames.checked_add(self.compiled_frames).is_none_or(|jobs| jobs > frames)
        {
            return Err(CliError::new("render", "native dispatch counters disagree with emitted frames"));
        }
        Ok(())
    }

    pub(crate) fn to_json(self) -> String {
        format!(
            "{{\"schema\":\"fmn.cli.dispatch\",\"version\":1,\"pure_segments\":{},\"serial_segments\":{},\"pure_frames\":{},\"compiled_frames\":{}}}",
            self.pure_segments, self.segments.saturating_sub(self.pure_segments),
            self.pure_frames, self.compiled_frames,
        )
    }
}

fn increment(value: u64) -> Result<u64, IntegrationError> {
    value.checked_add(1).ok_or_else(|| IntegrationError::new("lumen", "native dispatch counter exhausted"))
}

impl CpuRenderer {
    fn capture_pure(&mut self, job: PureFrameJob, sequence: u64) -> Result<(), IntegrationError> {
        let pipeline = self.pipeline.as_mut().ok_or_else(|| {
            IntegrationError::new("lumen", "CPU frame pipeline was already finalized")
        })?;
        let result = pipeline.capture_pure(job, sequence);
        self.capture_result(result)
    }
}

impl RenderSink {
    pub(crate) fn accepts_native_programs(&self) -> bool {
        self.pure_enabled && self.failure.is_none() && match &self.renderer {
            OfflineFrameRenderer::Cpu(renderer) => renderer.pipeline.is_some(),
            #[cfg(feature = "metal")]
            OfflineFrameRenderer::Metal { .. } => false,
        }
    }

    pub(crate) fn render_pure(&mut self, segment: &PureSegment) -> Result<(), IntegrationError> {
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        let result = (|| {
            if !self.accepts_native_programs() {
                return Err(IntegrationError::new("lumen", "this CLI sink cannot consume native pure programs"));
            }
            if self.backend.dispatch.pure_segments >= self.backend.dispatch.segments {
                return Err(IntegrationError::new("lumen", "pure program arrived without a scene segment boundary"));
            }
            // Do not materialize here: interpolation and arena reconstruction
            // happen inside NativeFramePipeline's affine or camera render team.
            // Source sample numbers are retained; output numbering is rebased.
            for sample in 1..=segment.frame_count() {
                let sequence = self.next_sequence;
                let next = increment(sequence)?;
                let job = segment.job(sample).map_err(|error| {
                    IntegrationError::new("lumen", error.to_string())
                })?;
                match &mut self.renderer {
                    OfflineFrameRenderer::Cpu(renderer) => renderer.capture_pure(job, sequence)?,
                    #[cfg(feature = "metal")]
                    OfflineFrameRenderer::Metal { .. } => unreachable!("annex refused before admission"),
                }
                self.backend.record_cpu_frame()?;
                self.backend.dispatch.pure_frames = increment(self.backend.dispatch.pure_frames)?;
                self.next_sequence = next;
            }
            self.backend.dispatch.pure_segments = increment(self.backend.dispatch.pure_segments)?;
            Ok(())
        })();
        self.latch_capture(result)
    }

    pub(crate) fn latch_capture(&mut self, result: Result<(), IntegrationError>) -> Result<(), IntegrationError> {
        if let Err(error) = &result {
            self.failure = Some(error.clone());
            if let Some(emitter) = &self.emitter {
                emitter.cancel();
            }
            match &mut self.renderer {
                OfflineFrameRenderer::Cpu(renderer) => renderer.abort(),
                #[cfg(feature = "metal")]
                OfflineFrameRenderer::Metal { .. } => {},
            }
        }
        result
    }
}

#[cfg(test)]
mod tests;
