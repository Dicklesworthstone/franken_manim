//! A resumable native program over Proscenium's existing stepped drivers.

use std::collections::VecDeque;

use fmn_scene::studio_bridge::{Animation, FramePacket};
use fmn_scene::{
    CaptureReason, EventPayload, IntegrationError, InteractiveScene, NullSceneSink, PlayOverrides,
    Scene, SceneError, SceneSink, SteppedPlay, SteppedWait,
};

use crate::{InteractiveDispatch, InteractivePreview, ServiceError, SpanRegistry, WorkerErrorCode};

/// Maximum number of declarative segments admitted by one native program.
pub const MAX_NATIVE_SEGMENTS: usize = 65_536;

/// A native animation or wait, executed by the ordinary Scene frame machinery.
///
/// Animation values and updater closures belong to one program instance. A
/// replay factory must construct fresh values, not clone their mutable captures.
pub enum NativeSegment {
    /// Play the supplied animations together with the ordinary timing overrides.
    Play {
        /// Owned native animations.
        animations: Vec<Box<dyn Animation>>,
        /// Scene-level timing overrides.
        overrides: PlayOverrides,
    },
    /// Wait for the supplied duration (or the Scene's configured default).
    Wait {
        /// Duration in seconds; sampling stays on the Scene's rational grid.
        duration: Option<f64>,
    },
}

enum ActiveSegment {
    Play(SteppedPlay),
    Wait(SteppedWait),
}

impl ActiveSegment {
    fn advance(&mut self, scene: &mut Scene, sink: &mut CaptureOne) -> Result<bool, SceneError> {
        match self {
            Self::Play(play) => {
                if scene.prepare_stepped_play_frame(play)?.is_none() {
                    return Ok(false);
                }
                scene.complete_stepped_play_frame(play, sink)?;
            }
            Self::Wait(wait) => {
                if scene.prepare_stepped_wait_frame(wait)?.is_none() {
                    return Ok(false);
                }
                scene.complete_stepped_wait_frame(wait, sink)?;
            }
        }
        Ok(true)
    }

    fn finish(self, scene: &mut Scene) -> Result<(), SceneError> {
        match self {
            Self::Play(play) => {
                scene.finish_stepped_play(play, &mut NullSceneSink)?;
            }
            Self::Wait(wait) => {
                scene.finish_stepped_wait(wait, &mut NullSceneSink)?;
            }
        }
        Ok(())
    }

    fn abort(self, scene: &mut Scene) {
        match self {
            Self::Play(play) => scene.abort_stepped_play(play, &mut NullSceneSink),
            Self::Wait(wait) => scene.abort_stepped_wait(wait, &mut NullSceneSink),
        }
    }
}

#[derive(Default)]
struct CaptureOne {
    packet: Option<FramePacket>,
    invalid: bool,
}

impl SceneSink for CaptureOne {
    fn capture(&mut self, reason: CaptureReason, packet: FramePacket) -> Result<(), IntegrationError> {
        if reason != CaptureReason::Segment || self.packet.is_some() {
            self.invalid = true;
            return Err(IntegrationError::new("studio", "expected one native segment capture"));
        }
        self.packet = Some(packet);
        Ok(())
    }
}

/// A live, pausable native scene with its animations, callbacks and input owner.
///
/// Frame zero is the constructed state before playback. Subsequent indices are
/// the *actual rational clock indices* of Scene captures, not clock relabels.
/// Only one capture is retained per step; an entire movie is never buffered.
/// Empty/zero-duration segments complete through the engine between captures.
/// Pausing at a capture does not prematurely run segment-finalization callbacks.
pub struct NativeSceneProgram {
    preview: InteractivePreview,
    segments: VecDeque<NativeSegment>,
    active: Option<ActiveSegment>,
    frame: u64,
    frame_limit: u64,
    failed: bool,
    spans: SpanRegistry,
}

impl NativeSceneProgram {
    /// Adopt an initial native Scene and a bounded declarative schedule.
    ///
    /// Skip/range/presenter modes are refused: they do not have the one-capture
    /// per rational-clock-step contract required by interactive seeking.
    pub fn new(
        scene: Scene,
        segments: Vec<NativeSegment>,
        frame_limit: u64,
    ) -> Result<Self, ServiceError> {
        Self::from_interactive(
            InteractiveScene::new(scene).map_err(execution_error)?,
            segments,
            frame_limit,
        )
    }

    /// Adopt an existing editor without installing duplicate input listeners.
    pub fn from_interactive(
        scene: InteractiveScene,
        segments: Vec<NativeSegment>,
        frame_limit: u64,
    ) -> Result<Self, ServiceError> {
        let config = scene.config();
        if scene.time().frames() != 0
            || scene.play_count() != 0
            || config.skip_animations
            || config.start_at_play.is_some()
            || config.end_at_play.is_some()
            || config.presenter_mode
        {
            return Err(invalid("native program requires a fresh non-skipping, non-presenter Scene"));
        }
        if segments.len() > MAX_NATIVE_SEGMENTS || frame_limit > i64::MAX.cast_unsigned() {
            return Err(invalid("native program exceeds its segment or clock limit"));
        }
        let preview = InteractivePreview::from_interactive(scene).map_err(execution_error)?;
        Ok(Self {
            preview,
            segments: segments.into(),
            active: None,
            frame: 0,
            frame_limit,
            failed: false,
            spans: SpanRegistry::new(),
        })
    }

    /// Latest completed capture, or zero for the initial constructed state.
    #[must_use]
    pub const fn frame_index(&self) -> u64 {
        self.frame
    }

    /// Highest admitted capture index (zero is always available initially).
    #[must_use]
    pub const fn frame_limit(&self) -> u64 {
        self.frame_limit
    }

    /// The actual Scene/editor owner; its clock and callbacks are never rebuilt
    /// from a callable-free durable snapshot.
    #[must_use]
    pub fn preview(&self) -> &InteractivePreview {
        &self.preview
    }

    /// Source spans bound by the factory to this instance's original handles.
    #[must_use]
    pub fn spans(&self) -> &SpanRegistry {
        &self.spans
    }

    /// Bind source spans while constructing the program.
    pub fn spans_mut(&mut self) -> &mut SpanRegistry {
        &mut self.spans
    }

    /// Dispatch native editing or application input at the paused boundary.
    /// A failed callback boundary poisons the program until its owner rebuilds.
    pub fn dispatch(&mut self, event: EventPayload) -> Result<InteractiveDispatch, ServiceError> {
        self.require_healthy()?;
        event.validate().map_err(|error| invalid(error.to_string()))?;
        let result = self.preview.dispatch(event).map_err(execution_error);
        self.failed |= result.is_err();
        result
    }

    /// Capture the actual Scene clock, RNG and Stage; no synthetic RNG fork or
    /// journal-position-as-play-count is substituted for runtime state.
    pub fn state_bytes(&mut self) -> Result<Vec<u8>, ServiceError> {
        self.require_healthy()?;
        self.preview.scene_mut().state_bytes().map_err(execution_error)
    }

    /// Execute the next ordinary native capture, retaining executable state for
    /// the next call. Execution errors poison the cursor instead of reusing partial
    /// work. At the frame limit, admission fails before any finalization or new
    /// segment prologue runs; the last captured state remains available.
    pub fn next_frame(&mut self) -> Result<Option<FramePacket>, ServiceError> {
        self.require_healthy()?;
        if self.frame >= self.frame_limit {
            return Err(invalid("native execution reached its frame budget"));
        }
        let result = self.next_frame_inner();
        self.failed |= result.is_err();
        result
    }

    /// Advance by real execution only. Backward movement requires a fresh
    /// factory instance; changing the clock alone is never treated as replay.
    pub fn advance_to(&mut self, target: u64) -> Result<(), ServiceError> {
        self.require_healthy()?;
        if target < self.frame || target > self.frame_limit {
            return Err(invalid("native seek requires a forward target within the frame limit"));
        }
        while self.frame < target {
            if self.next_frame()?.is_none() {
                return Err(invalid("native program ended before the requested frame"));
            }
        }
        Ok(())
    }

    fn require_healthy(&self) -> Result<(), ServiceError> {
        if self.failed {
            Err(execution_error("native program failed; reconstruct it from its factory"))
        } else {
            Ok(())
        }
    }

    fn next_frame_inner(&mut self) -> Result<Option<FramePacket>, ServiceError> {
        loop {
            if self.active.is_none() {
                let Some(segment) = self.segments.pop_front() else {
                    return Ok(None);
                };
                self.active = match segment {
                    NativeSegment::Play { animations, overrides } => self.preview.scene_mut()
                        .begin_stepped_play(animations, overrides, &mut NullSceneSink)
                        .map_err(execution_error)?.map(ActiveSegment::Play),
                    NativeSegment::Wait { duration } => Some(ActiveSegment::Wait(
                        self.preview.scene_mut().begin_stepped_wait(duration, &mut NullSceneSink)
                            .map_err(execution_error)?,
                    )),
                };
                continue;
            }
            let Some(mut active) = self.active.take() else {
                return Err(execution_error("native segment cursor disappeared"));
            };
            let mut capture = CaptureOne::default();
            match active.advance(self.preview.scene_mut(), &mut capture) {
                Ok(false) => active.finish(self.preview.scene_mut()).map_err(execution_error)?,
                Ok(true) => {
                    self.active = Some(active);
                    let expected = self.frame + 1;
                    let packet = capture.packet.ok_or_else(|| execution_error("native step omitted its capture"))?;
                    if capture.invalid
                        || u64::try_from(packet.frame_index()).ok() != Some(expected)
                        || self.preview.scene().time() != packet.time()
                    {
                        return Err(execution_error("native capture disagreed with its rational clock"));
                    }
                    self.frame = expected;
                    return Ok(Some(packet));
                }
                Err(error) => {
                    active.abort(self.preview.scene_mut());
                    return Err(execution_error(error));
                }
            }
        }
    }
}

pub(super) fn execution_error(error: impl std::fmt::Display) -> ServiceError {
    ServiceError::new(WorkerErrorCode::ExecutionFailed, error.to_string())
}

pub(super) fn invalid(message: impl Into<String>) -> ServiceError {
    ServiceError::new(WorkerErrorCode::InvalidRequest, message)
}
