//! Pure-program sink handoff; the ordinary capture error policy is unchanged.
use super::{CaptureReason, IntegrationError, SceneSink};
use fmn_anim::FramePacket;
use fmn_anim::pure_frame::{PlayCapture, PureSegment};

pub(super) struct NativeSegmentCapture<'a> {
    sink: &'a mut dyn SceneSink,
    error: &'a mut Option<IntegrationError>,
}

impl<'a> NativeSegmentCapture<'a> {
    pub(super) fn new(sink: &'a mut dyn SceneSink, error: &'a mut Option<IntegrationError>) -> Self {
        Self { sink, error }
    }
}

impl PlayCapture for NativeSegmentCapture<'_> {
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

impl super::Scene {
    // An empty inbox may still have an external producer. Share this proof
    // between plays and waits rather than giving quiet segments weaker rules.
    pub(super) fn allows_pure_dispatch(&self) -> bool {
        !self.skipping
            && !self.config.windowed
            && !self.config.presenter_mode
            && self.event_dispatcher.listener_count() == 0
            && self.event_inbox.is_exclusive_empty()
            && self.queued_events.is_empty()
            && self.event_error.is_none()
    }
}
