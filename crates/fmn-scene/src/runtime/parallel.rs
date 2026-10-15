//! Pure-program sink handoff; the ordinary capture error policy is unchanged.
use super::{CaptureReason, IntegrationError, SceneSink};
use fmn_anim::FramePacket;
use fmn_anim::pure_frame::{PlayCapture, PureSegment};

pub(super) struct NativePlayCapture<'a> {
    sink: &'a mut dyn SceneSink,
    error: &'a mut Option<IntegrationError>,
}

impl<'a> NativePlayCapture<'a> {
    pub(super) fn new(sink: &'a mut dyn SceneSink, error: &'a mut Option<IntegrationError>) -> Self {
        Self { sink, error }
    }
}

impl PlayCapture for NativePlayCapture<'_> {
    fn capture(&mut self, packet: FramePacket) {
        if self.error.is_none()
            && let Err(error) = self.sink.capture(CaptureReason::Segment, packet)
        {
            *self.error = Some(error);
        }
    }

    fn accepts_pure_segments(&self) -> bool {
        self.error.is_none() && self.sink.accepts_pure_segments()
    }

    fn capture_pure_segment(&mut self, segment: &PureSegment) -> bool {
        if let Err(error) = self.sink.capture_pure_segment(segment) {
            *self.error = Some(error);
        }
        // Even on a partial sink failure, do not replay or publish the segment
        // twice. Choreo completes the original owner's epilogue, and Scene
        // surfaces this same latched error at its ordinary segment boundary.
        true
    }
}
