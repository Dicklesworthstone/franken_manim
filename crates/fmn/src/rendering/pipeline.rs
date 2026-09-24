//! Owned camera jobs between the serial scene owner and the existing runtime.

use std::fmt;
use fmn_frame::convert::{rgba16f_to_rgba8, rgba_to_nv12};
use fmn_frame::{ChromaSiting, ColorRange, FrameBuffer, FrameError, FrameLayout, PixelFormat};
use fmn_output::{EmitterError, FrameReservation};
use fmn_render::{PreparedCameraFrame, RetainedFrameRendererError};
use fmn_runtime::{PipelineStages, TeamPlan};

/// A failure in worker-owned camera rasterization, conversion, or publication.
#[derive(Debug)]
pub enum NativeFrameError {
    /// The camera renderer refused the frozen input.
    Renderer(RetainedFrameRendererError),
    /// Frame layout or output conversion failed.
    Frame(FrameError),
    /// The ordered output stream failed or was cancelled.
    Emitter(EmitterError),
}

impl fmt::Display for NativeFrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Renderer(error) => error.fmt(f),
            Self::Frame(error) => error.fmt(f),
            Self::Emitter(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for NativeFrameError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Renderer(error) => Some(error),
            Self::Frame(error) => Some(error),
            Self::Emitter(error) => Some(error),
        }
    }
}

pub(super) struct NativeFrameJob {
    pub frame: PreparedCameraFrame,
    // Reserve on the scene owner, in capture order, not on raster workers.
    pub output: FrameReservation,
}

pub(super) struct NativeFrameStages;

impl PipelineStages for NativeFrameStages {
    type Frame = NativeFrameJob;
    type Prepared = NativeFrameJob;
    type Rasterized = (FrameBuffer, FrameReservation);
    type Output = FrameReservation;
    type Error = NativeFrameError;

    fn prepare(&self, frame: NativeFrameJob, _: &TeamPlan) -> Result<NativeFrameJob, Self::Error> {
        // All live scene access has finished on its serial owner.
        Ok(frame)
    }

    fn rasterize(
        &self, job: NativeFrameJob, team: &TeamPlan,
    ) -> Result<Self::Rasterized, Self::Error> {
        let frame = job.frame.render(team.threads()).map_err(NativeFrameError::Renderer)?;
        Ok((frame, job.output))
    }

    fn convert(
        &self, (frame, mut output): Self::Rasterized, _: &TeamPlan,
    ) -> Result<FrameReservation, Self::Error> {
        if output.frame().layout().format() == PixelFormat::Nv12 {
            let layout = FrameLayout::tight(
                PixelFormat::Rgba8, frame.layout().width(), frame.layout().height(),
            ).map_err(NativeFrameError::Frame)?;
            let mut scratch = FrameBuffer::new(layout);
            rgba16f_to_rgba8(&frame, &mut scratch).map_err(NativeFrameError::Frame)?;
            rgba_to_nv12(
                &scratch, output.frame_mut(), ColorRange::Limited, ChromaSiting::Left,
            ).map_err(NativeFrameError::Frame)?;
        } else {
            rgba16f_to_rgba8(&frame, output.frame_mut()).map_err(NativeFrameError::Frame)?;
        }
        Ok(output)
    }
}
