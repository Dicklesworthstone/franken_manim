//! A resumable native program over Proscenium's existing stepped drivers.

use std::collections::VecDeque;

use fmn_hash::{Limits, Schema, Writer};
use fmn_render::CameraConfig;
use fmn_scene::studio_bridge::{
    Animation, Duration, FrameMetadata, FramePacket, FrameSegment, ImpureEffect,
    OpenWaitCheckpoint, Purity, RationalFrameClock, classify_play, classify_wait,
};
use fmn_scene::{
    CameraRig, CaptureReason, EffectClass, EventPayload, EventType, ImpureEffectTag,
    IntegrationError, InteractiveScene, NullSceneSink, PlayOverrides, Scene, SceneError, SceneSink,
    SteppedPlay, SteppedWait,
};

use crate::{
    InteractiveDispatch, InteractivePreview, ProtocolDigest, ServiceError, SpanRegistry,
    WorkerErrorCode, protocol_digest,
};

use super::{NativeBuild, NativeBuildContext, NativeUpdaterRegistry};

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

/// Why a native checkpoint requires independent execution from its factory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeCheckpointFallback {
    /// An older checkpoint contains scene data without an executable cursor.
    LegacyCheckpoint,
    /// An active animation or pending executable step has no durable continuation.
    UnsupportedSchedule,
    /// A live callback is not an unchanged function registered for restoration.
    UnregisteredUpdater,
    /// Editor input, event history, native attributes or sound need reconstruction.
    TransientSceneState,
    /// Committed input must reconstruct editor selection and undo history.
    EditedInput,
    /// The program is not paused at a supported completed-frame boundary.
    IncompleteContinuation,
}

impl NativeCheckpointFallback {
    /// Stable diagnostic code, suitable for inspector and journal logs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LegacyCheckpoint => "legacy_checkpoint",
            Self::UnsupportedSchedule => "unsupported_schedule",
            Self::UnregisteredUpdater => "unregistered_updater",
            Self::TransientSceneState => "transient_scene_state",
            Self::EditedInput => "edited_input",
            Self::IncompleteContinuation => "incomplete_continuation",
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum WaitSpec {
    Seconds(f64),
    Frames(u64),
}

impl WaitSpec {
    fn from_segment(segment: &NativeSegment, default_wait: f64) -> Option<Self> {
        match segment {
            NativeSegment::Wait { duration } => {
                Some(Self::Seconds(duration.unwrap_or(default_wait)))
            }
            NativeSegment::WaitFrames { frames } => Some(Self::Frames(*frames)),
            NativeSegment::Play { .. } | NativeSegment::Build { .. } => None,
        }
    }

    fn segment(self, fps: u32) -> Result<FrameSegment, ServiceError> {
        let clock = RationalFrameClock::new(fps).map_err(execution_error)?;
        clock
            .segment(match self {
                Self::Seconds(seconds) => Duration::Seconds(seconds),
                Self::Frames(frames) => Duration::Frames(frames),
            })
            .map_err(execution_error)
    }
}

/// The original, declarative suffix. Deferred segments are always prepended,
/// so none can execute after the first of these waits has opened.
struct WaitTail {
    schedule: Vec<WaitSpec>,
    prefix_segments: usize,
    has_build: bool,
    factory_listeners_safe: bool,
}

fn only_editor_listeners(scene: &InteractiveScene) -> bool {
    // Initial author listeners can share mutable captures with the completed
    // prefix even when it never changes their registration. Those captures
    // have no durable representation. The editor's private state changes only
    // through input, which the existing transient-state checks exclude.
    let dispatcher = scene.scene().event_dispatcher();
    let owned = scene.listener_ids();
    dispatcher.listener_count() == owned.len()
        && [
            EventType::MouseMotion,
            EventType::MousePress,
            EventType::MouseRelease,
            EventType::MouseDrag,
            EventType::MouseScroll,
            EventType::KeyPress,
            EventType::KeyRelease,
        ]
        .into_iter()
        .flat_map(|kind| dispatcher.listener_ids(kind))
        .all(|id| owned.contains(&id))
}

fn wait_schedule_frames(schedule: &[WaitSpec], fps: u32) -> Result<i64, ServiceError> {
    schedule.iter().try_fold(0_i64, |frames, &spec| {
        frames
            .checked_add(spec.segment(fps)?.n_frames())
            .ok_or_else(|| invalid("native checkpoint schedule overflows its clock"))
    })
}

/// Data only: no animation, closure, editor or renderer is deserialized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NativeProgramCheckpoint {
    pub frame: u64,
    pub frame_shape: [u64; 2],
    pub started_segments: usize,
    pub effects: u8,
    pub active: Option<NativeWaitCursor>,
    pub tail: Option<NativeWaitTailCursor>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NativeWaitCursor {
    pub base_frame: i64,
    pub stepped: i64,
    pub purity: u8,
}

/// A completed executable prefix is represented only by its resulting scene
/// data and counters. Its callbacks and animation lifecycle are never resumed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NativeWaitTailCursor {
    pub index: usize,
    pub prefix_frames: i64,
    pub prefix_segments: usize,
    pub prefix_plays: u64,
}

struct CheckedWaitContinuation {
    active: Option<OpenWaitCheckpoint>,
    completed_plays: u64,
    remaining_segments: usize,
    admitted_segments: usize,
}

pub(super) enum CheckpointAdmission {
    Resume(NativeProgramCheckpoint),
    Replay(NativeCheckpointFallback),
}

// A cumulative prefix, not a per-request delta: journal replay can continue a
// live candidate while recording reconstructs the same prefix from frame zero.
// Keep the existing journal vocabulary and a canonical order for strict replay
// comparison, regardless of which segment first revealed each reason.
#[derive(Default)]
struct EffectInventory {
    reasons: u8,
}

fn purity_from_bits(bits: u8) -> Result<Purity, ServiceError> {
    if bits & !15 != 0 {
        return Err(invalid("native checkpoint contains unknown effect bits"));
    }
    if bits == 0 {
        return Ok(Purity::Pure);
    }
    Ok(Purity::Stateful(
        [
            (1, ImpureEffect::DtUpdater),
            (2, ImpureEffect::SceneUpdater),
            (4, ImpureEffect::UnclassifiedAnimation),
            (8, ImpureEffect::StopCondition),
        ]
        .into_iter()
        .filter_map(|(mask, effect)| (bits & mask != 0).then_some(effect))
        .collect(),
    ))
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
    wait_schedule: Option<Vec<WaitSpec>>,
    wait_tail: Option<WaitTail>,
    checkpoint_registry: NativeUpdaterRegistry,
    factory_checkpoint_fallback: Option<NativeCheckpointFallback>,
    changed_bindings: bool,
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
        let wait_schedule = segments
            .iter()
            .map(|segment| WaitSpec::from_segment(segment, config.default_wait_time))
            .collect::<Option<Vec<_>>>();
        let wait_tail = if wait_schedule.is_none() {
            let mut schedule: Vec<_> = segments
                .iter()
                .rev()
                .map_while(|segment| WaitSpec::from_segment(segment, config.default_wait_time))
                .collect();
            schedule.reverse();
            (!schedule.is_empty()).then(|| WaitTail {
                prefix_segments: segments.len() - schedule.len(),
                has_build: segments
                    .iter()
                    .any(|segment| matches!(segment, NativeSegment::Build { .. })),
                factory_listeners_safe: only_editor_listeners(&scene),
                schedule,
            })
        } else {
            None
        };
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
            wait_schedule,
            wait_tail,
            checkpoint_registry: NativeUpdaterRegistry::new(),
            factory_checkpoint_fallback: None,
            changed_bindings: false,
        };
        program.observe_scene_effects();
        Ok(program)
    }

    /// Declare the function-pointer callbacks whose mutable state is entirely
    /// stored in this Scene's durable records. Registration is checked again
    /// at every checkpoint; replacing a callback, attaching native attributes,
    /// or adding an opaque callback makes that checkpoint require cold replay.
    ///
    /// Install the registry before executing any segment. The worker must also
    /// opt in through [`super::NativeReplayPolicy::Checkpointed`].
    pub fn with_checkpoint_registry(
        mut self,
        registry: NativeUpdaterRegistry,
    ) -> Result<Self, ServiceError> {
        self.require_healthy()?;
        if self.frame != 0 || self.started_segments != 0 || self.active.is_some() {
            return Err(invalid(
                "bind the checkpoint registry before native execution",
            ));
        }
        self.checkpoint_registry = registry;
        Ok(self)
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
        self.changed_bindings |= self.frame != 0 || self.started_segments != 0;
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

    /// Bind actual factory data and declarations, including the original seed,
    /// without evaluating a frame or a wait prologue. Addresses never enter it.
    pub(super) fn factory_fingerprint(
        &mut self,
        max_bytes: usize,
    ) -> Result<ProtocolDigest, ServiceError> {
        self.require_healthy()?;
        if self.frame != 0 || self.started_segments != 0 || self.active.is_some() {
            return Err(invalid(
                "checkpoint factory identity requires the initial program",
            ));
        }
        // A later prefix may remove an unregistered callback or transient
        // attribute. Do not emit a continuation that this fresh factory would
        // itself refuse before the durable data can be installed.
        self.factory_checkpoint_fallback = self.checkpoint_state_fallback();
        let state = self.state_bytes()?;
        let mut writer = Writer::with_limits(
            Schema::new(*b"FNCF", 1, 0, 0),
            Limits {
                max_total: max_bytes,
                max_field: max_bytes,
            },
        );
        writer
            .put_u64(self.preview.scene().rng_seed())
            .put_u64(self.preview.stage().frame_shape()[0].to_bits())
            .put_u64(self.preview.stage().frame_shape()[1].to_bits())
            .put_u64(self.frame_limit)
            .put_u64(self.segment_limit as u64)
            .put_bytes(self.checkpoint_registry.fingerprint().as_bytes())
            .put_bytes(&state);
        if let Some(schedule) = &self.wait_schedule {
            writer.put_u8(1).put_u32(schedule.len() as u32);
            for segment in schedule {
                match segment {
                    WaitSpec::Seconds(seconds) => {
                        writer.put_u8(0).put_u64(seconds.to_bits());
                    }
                    WaitSpec::Frames(frames) => {
                        writer.put_u8(1).put_u64(*frames);
                    }
                }
            }
        } else if self.wait_tail.is_some() {
            // Executable prefix implementations remain bound by the worker's
            // source/build reads. Bind its declared shape and every wait here
            // too; no callback, begin hook or animation validation is invoked.
            writer.put_u8(2).put_u32(self.segments.len() as u32);
            for segment in &self.segments {
                match segment {
                    NativeSegment::Play { .. } => {
                        writer.put_u8(0);
                    }
                    NativeSegment::Build { .. } => {
                        writer.put_u8(1);
                    }
                    NativeSegment::Wait { duration } => {
                        writer.put_u8(2).put_u64(
                            duration
                                .unwrap_or(self.preview.scene().config().default_wait_time)
                                .to_bits(),
                        );
                    }
                    NativeSegment::WaitFrames { frames } => {
                        writer.put_u8(3).put_u64(*frames);
                    }
                }
            }
        } else {
            writer.put_u8(0);
        }
        Ok(protocol_digest(&writer.finish().map_err(execution_error)?))
    }

    pub(super) fn checkpoint_admission(&self) -> Result<CheckpointAdmission, ServiceError> {
        self.require_healthy()?;
        let replay = |reason| Ok(CheckpointAdmission::Replay(reason));
        if self.wait_schedule.is_none()
            && !self
                .wait_tail
                .as_ref()
                .is_some_and(|tail| self.segments.len() < tail.schedule.len())
        {
            return replay(NativeCheckpointFallback::UnsupportedSchedule);
        }
        if let Some(reason) = self
            .factory_checkpoint_fallback
            .or_else(|| self.checkpoint_state_fallback())
        {
            return replay(reason);
        }
        let scene = self.preview.scene();
        let stage = self.preview.stage();
        let active = match &self.active {
            Some(ActiveSegment::Wait(wait)) => {
                let checkpoint = scene
                    .checkpoint_stepped_wait(wait)
                    .map_err(execution_error)?;
                let mut purity = EffectInventory::default();
                purity.note(&checkpoint.purity);
                Some(NativeWaitCursor {
                    base_frame: checkpoint.base_frame,
                    stepped: checkpoint.stepped,
                    purity: purity.reasons,
                })
            }
            Some(ActiveSegment::Play(_)) => {
                return replay(NativeCheckpointFallback::UnsupportedSchedule);
            }
            None if self.frame == 0 && self.started_segments == 0 => None,
            None => return replay(NativeCheckpointFallback::IncompleteContinuation),
        };
        let tail = if let Some(tail) = &self.wait_tail {
            let index = tail.schedule.len() - self.segments.len() - 1;
            let Some(wait) = &active else {
                return replay(NativeCheckpointFallback::IncompleteContinuation);
            };
            let earlier_frames = wait_schedule_frames(&tail.schedule[..index], scene.fps())?;
            let Some(prefix_frames) = wait.base_frame.checked_sub(earlier_frames) else {
                return replay(NativeCheckpointFallback::IncompleteContinuation);
            };
            let Some(prefix_segments) = self.started_segments.checked_sub(index + 1) else {
                return replay(NativeCheckpointFallback::IncompleteContinuation);
            };
            let Some(prefix_plays) = scene.play_count().checked_sub(index as u64) else {
                return replay(NativeCheckpointFallback::IncompleteContinuation);
            };
            Some(NativeWaitTailCursor {
                index,
                prefix_frames,
                prefix_segments,
                prefix_plays,
            })
        } else {
            None
        };
        let checkpoint = NativeProgramCheckpoint {
            frame: self.frame,
            frame_shape: stage.frame_shape().map(f64::to_bits),
            started_segments: self.started_segments,
            effects: self.effects.reasons,
            active,
            tail,
        };
        if self.checked_wait_cursor(&checkpoint).is_err() {
            return replay(NativeCheckpointFallback::IncompleteContinuation);
        }
        Ok(CheckpointAdmission::Resume(checkpoint))
    }

    fn checkpoint_state_fallback(&self) -> Option<NativeCheckpointFallback> {
        let stage = self.preview.stage();
        if self.changed_bindings
            || self
                .wait_tail
                .as_ref()
                .is_some_and(|tail| !tail.factory_listeners_safe)
            || !self.preview.scene().can_resume_wait_checkpoint()
            || stage.has_native_updater_state()
            || stage.has_padding_sources()
            || stage.has_rollback_identity_history()
        {
            return Some(NativeCheckpointFallback::TransientSceneState);
        }
        if !self
            .checkpoint_registry
            .supports(stage, &stage.updater_manifest())
        {
            return Some(NativeCheckpointFallback::UnregisteredUpdater);
        }
        None
    }

    fn checked_wait_cursor(
        &self,
        checkpoint: &NativeProgramCheckpoint,
    ) -> Result<CheckedWaitContinuation, ServiceError> {
        if checkpoint.frame > self.frame_limit
            || checkpoint.started_segments > self.segment_limit
            || checkpoint.effects & !15 != 0
            || checkpoint.frame_shape.iter().any(|&bits| {
                let value = f64::from_bits(bits);
                !value.is_finite() || value <= 0.0
            })
        {
            return Err(invalid(
                "native checkpoint frame, schedule or effects are invalid",
            ));
        }
        let Some(active) = &checkpoint.active else {
            if checkpoint.frame != 0
                || checkpoint.started_segments != 0
                || checkpoint.tail.is_some()
            {
                return Err(invalid("a noninitial checkpoint requires an active wait"));
            }
            let schedule = self
                .wait_schedule
                .as_ref()
                .ok_or_else(|| invalid("initial checkpoint requires a complete wait schedule"))?;
            return Ok(CheckedWaitContinuation {
                active: None,
                completed_plays: 0,
                remaining_segments: schedule.len(),
                admitted_segments: schedule.len(),
            });
        };
        let fps = self.preview.scene().fps();
        let (schedule, index, prefix_frames, completed_plays, admitted_segments) =
            if let Some(cursor) = &checkpoint.tail {
                let tail = self
                    .wait_tail
                    .as_ref()
                    .ok_or_else(|| invalid("checkpoint factory has no trailing wait schedule"))?;
                let admitted = cursor
                    .prefix_segments
                    .checked_add(tail.schedule.len())
                    .filter(|&count| count <= self.segment_limit)
                    .ok_or_else(|| invalid("native checkpoint exceeds its segment budget"))?;
                if cursor.index >= tail.schedule.len()
                    || cursor.prefix_frames < 0
                    || cursor.prefix_segments < tail.prefix_segments
                    || (!tail.has_build && cursor.prefix_segments != tail.prefix_segments)
                    || cursor.prefix_plays > cursor.prefix_segments as u64
                    || (tail.has_build && checkpoint.effects & 4 == 0)
                    || cursor.prefix_segments.checked_add(cursor.index + 1)
                        != Some(checkpoint.started_segments)
                {
                    return Err(invalid("native checkpoint has an invalid completed prefix"));
                }
                let plays = cursor
                    .prefix_plays
                    .checked_add(cursor.index as u64)
                    .ok_or_else(|| invalid("native checkpoint play count overflows"))?;
                (
                    &tail.schedule,
                    cursor.index,
                    cursor.prefix_frames,
                    plays,
                    admitted,
                )
            } else {
                let schedule = self
                    .wait_schedule
                    .as_ref()
                    .ok_or_else(|| invalid("checkpoint factory has no complete wait schedule"))?;
                let index = checkpoint
                    .started_segments
                    .checked_sub(1)
                    .filter(|&index| index < schedule.len())
                    .ok_or_else(|| invalid("native checkpoint has an invalid wait index"))?;
                (schedule, index, 0, index as u64, schedule.len())
            };
        let base = prefix_frames
            .checked_add(wait_schedule_frames(&schedule[..index], fps)?)
            .ok_or_else(|| invalid("native checkpoint schedule overflows its clock"))?;
        let segment = schedule[index].segment(fps)?;
        if active.base_frame != base
            || active.stepped <= 0
            || active.stepped > segment.n_frames()
            || base.checked_add(active.stepped) != i64::try_from(checkpoint.frame).ok()
            || active.purity & !15 != 0
            || checkpoint.effects & active.purity != active.purity
        {
            return Err(invalid(
                "native checkpoint does not match its original wait grid",
            ));
        }
        Ok(CheckedWaitContinuation {
            active: Some(OpenWaitCheckpoint {
                segment,
                base_frame: base,
                stepped: active.stepped,
                purity: purity_from_bits(active.purity)?,
            }),
            completed_plays,
            remaining_segments: schedule.len() - index - 1,
            admitted_segments,
        })
    }

    /// Install data into an independent fresh factory instance. All executable
    /// callbacks come from its declared registry, never from the prior owner.
    pub(super) fn restore_checkpoint(
        &mut self,
        bytes: &[u8],
        checkpoint: &NativeProgramCheckpoint,
    ) -> Result<(), ServiceError> {
        self.require_healthy()?;
        if self.frame != 0 || self.started_segments != 0 || self.active.is_some() {
            return Err(invalid(
                "native checkpoint needs an independent fresh factory",
            ));
        }
        if self.checkpoint_state_fallback().is_some() {
            return Err(invalid(
                "native factory cannot restore registered wait state",
            ));
        }
        let continuation = self.checked_wait_cursor(checkpoint)?;
        if checkpoint.effects & self.effects.reasons != self.effects.reasons {
            return Err(invalid(
                "native checkpoint lost its factory effect inventory",
            ));
        }
        self.failed = true;
        let scene = self.preview.scene_mut();
        let decoded = scene.restore_state_bytes(bytes).map_err(execution_error)?;
        scene
            .stage_mut()
            .set_frame_shape(checkpoint.frame_shape.map(f64::from_bits))
            .map_err(execution_error)?;
        self.checkpoint_registry.rebind(scene, &decoded.updaters)?;
        if scene.time().frames().cast_unsigned() != checkpoint.frame
            || scene.play_count() != continuation.completed_plays
            || !scene.can_resume_wait_checkpoint()
            || !self
                .checkpoint_registry
                .supports(scene.stage(), &decoded.updaters)
        {
            return Err(invalid(
                "native checkpoint scene does not match its continuation",
            ));
        }
        let mut current = EffectInventory::default();
        current.note(&classify_wait(scene.stage(), false));
        if checkpoint.effects & current.reasons != current.reasons {
            return Err(invalid(
                "native checkpoint lost its current updater effects",
            ));
        }
        self.active = continuation
            .active
            .map(|wait| scene.resume_stepped_wait(wait).map(ActiveSegment::Wait))
            .transpose()
            .map_err(execution_error)?;
        let discard = self
            .segments
            .len()
            .checked_sub(continuation.remaining_segments)
            .ok_or_else(|| invalid("native checkpoint continuation exceeds its schedule"))?;
        // The fresh factory's prefix remains unopened. Only the original
        // declarative suffix survives; no saved closure or animation is run.
        drop(self.segments.drain(..discard));
        self.started_segments = checkpoint.started_segments;
        self.admitted_segments = continuation.admitted_segments;
        self.frame = checkpoint.frame;
        self.effects.reasons = checkpoint.effects;
        self.active_seekable = self.camera_rig.is_none();
        self.validate_camera()?;
        self.failed = false;
        Ok(())
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
                            changed_bindings: &mut self.changed_bindings,
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

#[cfg(test)]
mod tests {
    use super::*;
    use fmn_mobject::Mobject;
    use fmn_scene::RuntimeConfig;

    fn static_waits() -> NativeSceneProgram {
        let mut scene = Scene::new(RuntimeConfig::default(), 71).unwrap();
        scene
            .add_mobject(Mobject::from_points(&[[1.0, 2.0, 0.0]]))
            .unwrap();
        NativeSceneProgram::new(
            scene,
            vec![
                NativeSegment::WaitFrames { frames: 120_000 },
                NativeSegment::WaitFrames { frames: 7 },
            ],
            120_007,
        )
        .unwrap()
    }

    #[test]
    fn resumed_static_wait_evaluates_one_sample_and_preserves_its_pending_epilogue() {
        let mut uninterrupted = static_waits();
        uninterrupted.advance_to(7).unwrap();
        let CheckpointAdmission::Resume(cursor) = uninterrupted.checkpoint_admission().unwrap()
        else {
            panic!("static wait must admit a continuation");
        };
        let bytes = uninterrupted.state_bytes().unwrap();
        let mut resumed = static_waits();
        resumed.restore_checkpoint(&bytes, &cursor).unwrap();
        let work = resumed.advance_to_with_work(120_000).unwrap();
        assert_eq!(work.frames_advanced, 119_993);
        assert_eq!(work.frames_evaluated, 1);
        assert_eq!(resumed.frame_index(), 120_000);
        assert!(resumed.at_segment_boundary());
        assert_eq!(resumed.preview().scene().play_count(), 0);

        uninterrupted.advance_to(120_000).unwrap();
        assert_eq!(
            resumed.state_bytes().unwrap(),
            uninterrupted.state_bytes().unwrap()
        );
        let actual = resumed.next_frame().unwrap().unwrap();
        let expected = uninterrupted.next_frame().unwrap().unwrap();
        assert_eq!(actual.frame_index(), 120_001);
        assert_eq!(actual.segment_frame(), 1);
        assert_eq!(actual.frame_index(), expected.frame_index());
        assert_eq!(actual.alpha().to_bits(), expected.alpha().to_bits());
        assert_eq!(
            actual.rng_fork("resumed-static-wait").next_u64(),
            expected.rng_fork("resumed-static-wait").next_u64(),
        );
        assert_eq!(resumed.preview().scene().play_count(), 1);
        assert!(!resumed.at_segment_boundary());
        assert_eq!(
            resumed.state_bytes().unwrap(),
            uninterrupted.state_bytes().unwrap()
        );
    }
}
