//! A resumable native program over Proscenium's existing stepped drivers.

use std::collections::VecDeque;

use fmn_render::CameraConfig;
use fmn_scene::studio_bridge::{
    Animation, FrameMetadata, FramePacket, Purity, classify_play, classify_wait,
};
use fmn_scene::{
    CameraRig, CaptureReason, EffectClass, EventPayload, ImpureEffectTag, IntegrationError,
    InteractiveScene, NullSceneSink, PlayOverrides, Scene, SceneError, SceneSink, SteppedPlay,
    SteppedWait,
};

use crate::{InteractiveDispatch, InteractivePreview, ServiceError, SpanRegistry, WorkerErrorCode};

use super::{NativeBuild, NativeBuildContext};

/// Maximum cumulative segments admitted by one native program, including
/// deferred construction and every segment it generates, not just queue length.
pub const MAX_NATIVE_SEGMENTS: usize = 65_536;

/// Work performed by a forward native seek, independent of rendering cost.
///
/// Every ordinary frame is evaluated. A proven pure interval evaluates only
/// its last requested sample while preserving the executable continuation.
/// Segment prologues, epilogues and deferred construction still run normally.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NativeSeekWork {
    /// Number of nominal frames advanced on the Scene's rational clock.
    pub frames_advanced: u64,
    /// Number of nominal samples actually evaluated during the seek.
    pub frames_evaluated: u64,
}

// A cumulative prefix, not a per-request delta: journal replay can continue a
// live candidate while recording reconstructs the same prefix from frame zero.
// Keep the existing journal vocabulary and a canonical order for strict replay
// comparison, regardless of which segment first revealed each reason.
#[derive(Default)]
struct EffectInventory {
    reasons: u8,
}

impl EffectInventory {
    fn note(&mut self, purity: &Purity) {
        if let EffectClass::Stateful(reasons) = EffectClass::from_purity(purity) {
            for reason in reasons {
                self.reasons |= Self::bit(reason);
            }
        }
    }

    fn note_callback(&mut self) {
        // The four existing classifier tags have no narrower identity for a
        // deferred authoring or input callback. Its executable behavior remains
        // unclassified; ColdVerified supplies the separate factory contract.
        self.reasons |= Self::bit(ImpureEffectTag::UnclassifiedAnimation);
    }

    const fn bit(reason: ImpureEffectTag) -> u8 {
        match reason {
            ImpureEffectTag::DtUpdater => 1,
            ImpureEffectTag::SceneUpdater => 2,
            ImpureEffectTag::UnclassifiedAnimation => 4,
            ImpureEffectTag::StopCondition => 8,
        }
    }

    fn effect(&self) -> EffectClass {
        if self.reasons == 0 {
            EffectClass::Pure
        } else {
            EffectClass::Stateful(
                [
                    ImpureEffectTag::DtUpdater,
                    ImpureEffectTag::SceneUpdater,
                    ImpureEffectTag::UnclassifiedAnimation,
                    ImpureEffectTag::StopCondition,
                ]
                .into_iter()
                .filter(|&reason| self.reasons & Self::bit(reason) != 0)
                .collect(),
            )
        }
    }
}

/// A native animation, wait, or deferred authoring step.
///
/// Animation values and callback closures belong to one program instance. A
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
    /// Wait for an exact number of frames on the consuming Scene's grid.
    WaitFrames {
        /// Authoritative frame count; no conversion through seconds occurs.
        frames: u64,
    },
    /// Run one-shot native construction after preceding segment epilogues.
    /// Prefer `defer`, `edit`, or `play_with` for ergonomic construction.
    Build {
        /// Owned callback; returned segments run before the queued continuation.
        build: NativeBuild,
    },
}

enum ActiveSegment {
    Play(SteppedPlay),
    Wait(SteppedWait),
}

impl ActiveSegment {
    fn frames_complete(&self) -> bool {
        match self {
            Self::Play(play) => play.frames_complete(),
            Self::Wait(wait) => wait.frames_complete(),
        }
    }

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

    fn try_advance_pure(
        &mut self,
        scene: &mut Scene,
        max_frames: u64,
    ) -> Result<Option<FrameMetadata>, SceneError> {
        match self {
            Self::Play(play) => scene.try_advance_pure_play(play, max_frames),
            Self::Wait(wait) => scene.try_advance_pure_wait(wait, max_frames),
        }
    }

    fn purity(&self, scene: &Scene) -> Purity {
        match self {
            Self::Play(play) => scene.stepped_play_purity(play),
            Self::Wait(_) => classify_wait(scene.stage(), false),
        }
    }

    fn finish(self, scene: &mut Scene) -> Result<Purity, SceneError> {
        let report = match self {
            Self::Play(play) => scene.finish_stepped_play(play, &mut NullSceneSink)?,
            Self::Wait(wait) => scene.finish_stepped_wait(wait, &mut NullSceneSink)?,
        };
        Ok(report.purity)
    }

    fn abort(self, scene: &mut Scene) {
        match self {
            Self::Play(play) => scene.abort_stepped_play(play, &mut NullSceneSink),
            Self::Wait(wait) => scene.abort_stepped_wait(wait, &mut NullSceneSink),
        }
    }
}

struct CaptureOne {
    metadata: Option<FrameMetadata>,
    packet: Option<FramePacket>,
    state_required: bool,
    invalid: bool,
}

impl CaptureOne {
    fn new(state_required: bool) -> Self {
        Self {
            metadata: None,
            packet: None,
            state_required,
            invalid: false,
        }
    }

    fn record(
        &mut self,
        reason: CaptureReason,
        metadata: FrameMetadata,
    ) -> Result<(), IntegrationError> {
        if reason != CaptureReason::Segment || self.metadata.is_some() {
            self.invalid = true;
            return Err(IntegrationError::new(
                "studio",
                "expected one native segment capture",
            ));
        }
        self.metadata = Some(metadata);
        Ok(())
    }
}

impl SceneSink for CaptureOne {
    fn capture(
        &mut self,
        reason: CaptureReason,
        packet: FramePacket,
    ) -> Result<(), IntegrationError> {
        self.record(reason, packet.metadata())?;
        self.packet = Some(packet);
        Ok(())
    }

    fn capture_state_required(&self) -> bool {
        self.state_required
    }

    fn capture_metadata(
        &mut self,
        reason: CaptureReason,
        metadata: FrameMetadata,
    ) -> Result<(), IntegrationError> {
        self.record(reason, metadata)
    }
}

/// A live, pausable native scene with its animations, callbacks and input owner.
///
/// Frame zero is the factory's constructed state; leading deferred steps run
/// only when advancing toward the first capture. Subsequent indices are actual
/// rational clock indices, not clock relabels. At most one capture is retained
/// per step. Pausing never runs later construction or a segment epilogue early.
/// An optional CameraRig lives in the same snapshotted Stage as the geometry.
pub struct NativeSceneProgram {
    preview: InteractivePreview,
    segments: VecDeque<NativeSegment>,
    active: Option<ActiveSegment>,
    active_seekable: bool,
    effects: EffectInventory,
    frame: u64,
    frame_limit: u64,
    failed: bool,
    spans: SpanRegistry,
    camera_rig: Option<CameraRig>,
    admitted_segments: usize,
    started_segments: usize,
    segment_limit: usize,
}

impl NativeSceneProgram {
    /// Adopt an initial native Scene and a bounded schedule.
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
            return Err(invalid(
                "native program requires a fresh non-skipping, non-presenter Scene",
            ));
        }
        if segments.len() > MAX_NATIVE_SEGMENTS || frame_limit > i64::MAX.cast_unsigned() {
            return Err(invalid("native program exceeds its segment or clock limit"));
        }
        let preview = InteractivePreview::from_interactive(scene).map_err(execution_error)?;
        let admitted_segments = segments.len();
        let mut program = Self {
            preview,
            segments: segments.into(),
            active: None,
            active_seekable: false,
            effects: EffectInventory::default(),
            frame: 0,
            frame_limit,
            failed: false,
            spans: SpanRegistry::new(),
            camera_rig: None,
            admitted_segments,
            started_segments: 0,
            segment_limit: MAX_NATIVE_SEGMENTS,
        };
        program.observe_scene_effects();
        Ok(program)
    }

    /// Tighten cumulative schedule admission before execution. Every deferred
    /// step and every returned segment spends one slot for the program's entire
    /// lifetime. Recursive expansion cannot evade the limit by keeping a short
    /// pending queue. Zero permits only an initially empty schedule.
    pub fn with_segment_limit(mut self, limit: usize) -> Result<Self, ServiceError> {
        self.require_healthy()?;
        if self.started_segments != 0
            || limit > MAX_NATIVE_SEGMENTS
            || limit < self.admitted_segments
        {
            return Err(invalid(
                "native segment limit must cover the initial schedule and be set before execution",
            ));
        }
        self.segment_limit = limit;
        Ok(self)
    }

    /// Bind one native camera family before the first segment starts. The rig
    /// must belong to this Scene. Binding never executes an updater, and cannot
    /// be replaced partway through a program. Select CameraConfig on the worker
    /// as well; its resolution, aspect and capture policy remain authoritative.
    pub fn with_camera_rig(mut self, rig: CameraRig) -> Result<Self, ServiceError> {
        self.require_healthy()?;
        if self.frame != 0
            || self.started_segments != 0
            || self.active.is_some()
            || self.camera_rig.is_some()
            || self.preview.scene().play_count() != 0
        {
            return Err(invalid("bind a camera rig once, before native playback"));
        }
        rig.sample(self.preview.stage(), &CameraConfig::default())
            .map_err(execution_error)?;
        self.camera_rig = Some(rig);
        Ok(self)
    }

    /// Original tracker handles for the optional camera. Values are sampled
    /// from captured native state, never from a renderer-side timing callback.
    #[must_use]
    pub const fn camera_rig(&self) -> Option<CameraRig> {
        self.camera_rig
    }

    pub(super) fn camera_binding_index(&self) -> Result<Option<u64>, ServiceError> {
        self.camera_rig
            .map(|rig| {
                rig.binding_index(self.preview.stage())
                    .map_err(execution_error)
            })
            .transpose()
    }

    fn validate_camera(&self) -> Result<(), ServiceError> {
        if let Some(rig) = self.camera_rig {
            rig.sample(self.preview.stage(), &CameraConfig::default())
                .map_err(execution_error)?;
        }
        Ok(())
    }

    /// Latest completed capture, or zero for the initial constructed state.
    #[must_use]
    pub const fn frame_index(&self) -> u64 {
        self.frame
    }

    /// Conservative effects accumulated through this program's executed prefix,
    /// including initial updater registrations, deferred construction and input.
    /// A fresh reconstruction of the same prefix produces the same ordered tags.
    /// This does not grant callback replay permission: the worker's separate
    /// factory policy still turns every command opaque when replay is disabled.
    #[must_use]
    pub fn effect_class(&self) -> EffectClass {
        self.effects.effect()
    }

    /// A presenter may pause at this capture before starting the next segment.
    /// Frame zero is also a boundary. This read does not run finish callbacks,
    /// consume deferred authoring, or advance the scene's rational clock.
    /// Explicit timeline seeks remain available while presentation is paused.
    #[must_use]
    pub fn at_segment_boundary(&self) -> bool {
        self.active
            .as_ref()
            .is_none_or(ActiveSegment::frames_complete)
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

    /// Source spans bound to this instance's original handles.
    #[must_use]
    pub fn spans(&self) -> &SpanRegistry {
        &self.spans
    }

    /// Bind source spans while constructing the program. Deferred constructors
    /// can register their spans through NativeBuildContext as well.
    pub fn spans_mut(&mut self) -> &mut SpanRegistry {
        &mut self.spans
    }

    /// Dispatch native editing or application input at the paused boundary.
    /// Failure or unwinding poisons the program until its owner rebuilds it.
    pub fn dispatch(&mut self, event: EventPayload) -> Result<InteractiveDispatch, ServiceError> {
        self.require_healthy()?;
        event
            .validate()
            .map_err(|error| invalid(error.to_string()))?;
        // Input may edit an animation's outputs, endpoints or registrations.
        // The remainder of that open segment uses ordinary execution; a later
        // segment can establish a new pure proof from its edited begin state.
        self.active_seekable = false;
        self.effects.note_callback();
        self.failed = true;
        let result = self
            .preview
            .dispatch(event)
            .map_err(execution_error)
            .and_then(|receipt| {
                self.validate_camera()?;
                Ok(receipt)
            });
        self.failed = result.is_err();
        if result.is_ok() {
            self.observe_scene_effects();
        }
        result
    }

    /// Capture the actual Scene clock, RNG and Stage, including rig channels.
    /// Invalid camera values cannot become successful checkpoint receipts.
    pub fn state_bytes(&mut self) -> Result<Vec<u8>, ServiceError> {
        self.require_healthy()?;
        self.validate_camera()?;
        self.preview
            .scene_mut()
            .state_bytes()
            .map_err(execution_error)
    }

    /// Execute the next ordinary native capture, retaining executable state for
    /// the next call. At the frame limit, refusal happens before any epilogue or
    /// deferred callback. Errors and panics poison the cursor: a caller catching
    /// an unwind cannot accidentally reuse partially consumed authoring work.
    pub fn next_frame(&mut self) -> Result<Option<FramePacket>, ServiceError> {
        self.advance_one(true)
            .map(|capture| capture.and_then(|capture| capture.packet))
    }

    fn advance_one(&mut self, state_required: bool) -> Result<Option<CaptureOne>, ServiceError> {
        self.advance_by(state_required, 1)
    }

    fn advance_by(
        &mut self,
        state_required: bool,
        max_frames: u64,
    ) -> Result<Option<CaptureOne>, ServiceError> {
        self.require_healthy()?;
        if self.frame >= self.frame_limit {
            return Err(invalid("native execution reached its frame budget"));
        }
        let max_frames = max_frames.min(self.frame_limit - self.frame);
        if max_frames == 0 {
            return Err(invalid(
                "native execution requires a positive frame advance",
            ));
        }
        self.failed = true;
        let result = self
            .advance_one_inner(state_required, max_frames)
            .and_then(|capture| {
                self.validate_camera()?;
                Ok(capture)
            });
        self.failed = result.is_err();
        result
    }

    /// Advance through the ordinary executable lifecycle, sampling proven pure
    /// intervals directly and stepping callback-bearing intervals frame by frame.
    /// Input and deferred construction remain ordered boundaries. Intermediate
    /// captures retain timing metadata instead of freezing discarded scene state.
    /// Backward movement still requires a fresh factory instance.
    pub fn advance_to(&mut self, target: u64) -> Result<(), ServiceError> {
        self.advance_to_with_work(target).map(|_| ())
    }

    /// Seek as [`Self::advance_to`] and report how many samples were evaluated.
    ///
    /// A camera rig retains per-frame validation, and an input dispatch disables
    /// pure sampling for the remainder of its active segment. Unsupported pure
    /// carriers, updaters and event ingress likewise retain ordinary stepping.
    pub fn advance_to_with_work(&mut self, target: u64) -> Result<NativeSeekWork, ServiceError> {
        self.require_healthy()?;
        if target < self.frame || target > self.frame_limit {
            return Err(invalid(
                "native seek requires a forward target within the frame limit",
            ));
        }
        let mut work = NativeSeekWork {
            frames_advanced: target - self.frame,
            frames_evaluated: 0,
        };
        while self.frame < target {
            if self.advance_by(false, target - self.frame)?.is_none() {
                return Err(invalid("native program ended before the requested frame"));
            }
            work.frames_evaluated += 1;
        }
        Ok(work)
    }

    fn observe_scene_effects(&mut self) {
        self.effects
            .note(&classify_wait(self.preview.stage(), false));
        self.observe_input_effects();
    }

    fn observe_input_effects(&mut self) {
        let scene = self.preview.scene();
        if scene.has_external_event_ingress() || !scene.recorded_events().is_empty() {
            self.effects.note_callback();
        }
    }

    fn observe_active_effects(&mut self, active: &ActiveSegment) {
        self.effects.note(&active.purity(self.preview.scene()));
        self.observe_input_effects();
    }

    fn require_healthy(&self) -> Result<(), ServiceError> {
        if self.failed {
            Err(execution_error(
                "native program failed; reconstruct it from its factory",
            ))
        } else {
            Ok(())
        }
    }

    fn prepend(&mut self, segments: Vec<NativeSegment>) -> Result<(), ServiceError> {
        let admitted = self
            .admitted_segments
            .checked_add(segments.len())
            .filter(|count| *count <= self.segment_limit)
            .ok_or_else(|| {
                invalid("deferred native construction exceeded its cumulative segment budget")
            })?;
        self.segments
            .try_reserve(segments.len())
            .map_err(execution_error)?;
        for segment in segments.into_iter().rev() {
            self.segments.push_front(segment);
        }
        self.admitted_segments = admitted;
        Ok(())
    }

    fn advance_one_inner(
        &mut self,
        state_required: bool,
        max_frames: u64,
    ) -> Result<Option<CaptureOne>, ServiceError> {
        loop {
            if self.active.is_none() {
                let Some(segment) = self.segments.pop_front() else {
                    return Ok(None);
                };
                self.started_segments += 1;
                // Opening a wait executes a zero-dt pass. Capture its existing
                // updater evidence before a callback can remove its own slot.
                self.observe_scene_effects();
                // Dynamic camera validation is an observer at every nominal
                // frame. Do not skip an invalid intermediate rig value just
                // because the requested final sample would be valid again.
                self.active_seekable = self.camera_rig.is_none();
                self.active = match segment {
                    NativeSegment::Play {
                        animations,
                        overrides,
                    } => {
                        // Animation-owned endpoints can carry updaters while
                        // absent from the Scene's roots, even before begin.
                        self.effects
                            .note(&classify_play(self.preview.stage(), &animations));
                        let scene = self.preview.scene_mut();
                        let play = if self.active_seekable {
                            scene.begin_seekable_play(animations, overrides, &mut NullSceneSink)
                        } else {
                            scene.begin_stepped_play(animations, overrides, &mut NullSceneSink)
                        };
                        play.map_err(execution_error)?.map(ActiveSegment::Play)
                    }
                    NativeSegment::Wait { duration } => {
                        let scene = self.preview.scene_mut();
                        let wait = if self.active_seekable {
                            scene.begin_seekable_wait(duration, &mut NullSceneSink)
                        } else {
                            scene.begin_stepped_wait(duration, &mut NullSceneSink)
                        };
                        Some(ActiveSegment::Wait(wait.map_err(execution_error)?))
                    }
                    NativeSegment::WaitFrames { frames } => {
                        let scene = self.preview.scene_mut();
                        let wait = if self.active_seekable {
                            scene.begin_seekable_wait_frames(frames, &mut NullSceneSink)
                        } else {
                            scene.begin_stepped_wait_frames(frames, &mut NullSceneSink)
                        };
                        Some(ActiveSegment::Wait(wait.map_err(execution_error)?))
                    }
                    NativeSegment::Build { build } => {
                        self.effects.note_callback();
                        let generated = build(&mut NativeBuildContext {
                            scene: self.preview.scene_mut(),
                            spans: &mut self.spans,
                        })
                        .map_err(execution_error)?;
                        self.validate_camera()?;
                        self.prepend(generated)?;
                        None
                    }
                };
                self.observe_scene_effects();
                if let Some(active) = &self.active {
                    self.effects.note(&active.purity(self.preview.scene()));
                }
                continue;
            }
            let Some(mut active) = self.active.take() else {
                return Err(execution_error("native segment cursor disappeared"));
            };
            self.observe_active_effects(&active);
            let mut capture = CaptureOne::new(state_required);
            let advance = (|| {
                if !state_required && self.active_seekable && max_frames > 1 {
                    let metadata = active
                        .try_advance_pure(self.preview.scene_mut(), max_frames)
                        .map_err(execution_error)?;
                    if let Some(metadata) = metadata {
                        let frame =
                            u64::try_from(metadata.frame_index()).map_err(execution_error)?;
                        let advanced = frame
                            .checked_sub(self.frame)
                            .filter(|&count| count > 0 && count <= max_frames)
                            .ok_or_else(|| {
                                execution_error("native pure seek exceeded its frame range")
                            })?;
                        capture
                            .record(CaptureReason::Segment, metadata)
                            .map_err(execution_error)?;
                        return Ok(Some(advanced));
                    }
                }
                active
                    .advance(self.preview.scene_mut(), &mut capture)
                    .map(|advanced| advanced.then_some(1))
                    .map_err(execution_error)
            })();
            if advance.is_ok() {
                self.observe_active_effects(&active);
            }
            match advance {
                Ok(None) => {
                    let purity = active
                        .finish(self.preview.scene_mut())
                        .map_err(execution_error)?;
                    self.effects.note(&purity);
                    self.observe_scene_effects();
                }
                Ok(Some(advanced)) => {
                    self.active = Some(active);
                    let expected = self.frame + advanced;
                    let metadata = capture
                        .metadata
                        .ok_or_else(|| execution_error("native step omitted its capture"))?;
                    if capture.invalid
                        || (state_required && capture.packet.is_none())
                        || u64::try_from(metadata.frame_index()).ok() != Some(expected)
                        || self.preview.scene().time() != metadata.time()
                    {
                        return Err(execution_error(
                            "native capture disagreed with its rational clock",
                        ));
                    }
                    self.frame = expected;
                    return Ok(Some(capture));
                }
                Err(error) => {
                    active.abort(self.preview.scene_mut());
                    return Err(error);
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
