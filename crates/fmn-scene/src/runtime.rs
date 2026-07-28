//! The Proscenium Scene state machine (§13.1, §1.4, fm-5xm).
//!
//! This layer deliberately owns orchestration, not mechanisms already owned
//! below it:
//!
//! - Marionette's [`Stage`] owns family membership, stable z-ordering, and CoW
//!   snapshots.
//! - Choreo's [`play_segment`] / [`wait_segment`] own the six-step frame order
//!   and immutable [`FramePacket`] boundary.
//! - Scribe plugs into [`Scene::set_preflight_hook`] and sees the complete
//!   constructed root walk plus the first play's animation closure exactly
//!   once before the first captured frame.
//! - Lumen/Reel/Studio plug into [`SceneSink`]. `fmn-scene` cannot depend on
//!   `fmn-output` without inverting §19's crate DAG, so the composition root
//!   converts packets to pixels and submits those pixels to Reel.
//!
//! The sink also receives typed lifecycle events. They are the structured,
//! assertion-friendly surface for “preflight happened before frame zero” and
//! “pre/play/post ran in order”; human decoration belongs above this crate.

use std::fmt;
use std::path::{Path, PathBuf};

use fmn_anim::{
    AnimError, Animation, FramePacket, ImpureEffect, Purity, RateFunc, RationalFrameClock,
    RationalTime, SegmentKind, SegmentReport, play_segment, wait_segment,
};
use fmn_core::rng::{Pcg64Dxsm, RngRoot};
use fmn_hash::SerialError;
use fmn_mobject::{
    Mob, Mobject, PersistError, SceneState, Stage, StageError, UpdaterFn, UpdaterId,
    UpdaterKindTag, UpdaterManifest,
};

const DEFAULT_FPS: u32 = 30;
const DEFAULT_WAIT_TIME: f64 = 1.0;
const THREE_D_SAMPLES: u8 = 4;
const THREE_D_THETA_DEGREES: f64 = -30.0;
const THREE_D_PHI_DEGREES: f64 = 70.0;

/// Runtime configuration after CLI/front-door flags have been resolved.
#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeConfig {
    /// Requested frame rate. A windowed scene overrides this to 30.
    pub fps: u32,
    /// Whether an interactive window/Studio preview is attached.
    pub windowed: bool,
    /// Initial `-s` status.
    pub skip_animations: bool,
    /// First play index to render (`-n START`).
    pub start_at_play: Option<u64>,
    /// Exclusive play index at which to raise [`EndScene`] (`-n START,END`).
    pub end_at_play: Option<u64>,
    /// Force one current-state preview at skipped segment boundaries.
    pub preview_while_skipping: bool,
    /// Presenter waits are driven by the installed [`HoldController`].
    pub presenter_mode: bool,
    /// `wait(None)` duration.
    pub default_wait_time: f64,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            fps: DEFAULT_FPS,
            windowed: false,
            skip_animations: false,
            start_at_play: None,
            end_at_play: None,
            preview_while_skipping: true,
            presenter_mode: false,
            default_wait_time: DEFAULT_WAIT_TIME,
        }
    }
}

impl RuntimeConfig {
    /// Build the semantic defaults from the resolved project configuration.
    ///
    /// CLI-only state (`windowed`, skip, range, presenter) remains explicit on
    /// this type and defaults off.
    #[must_use]
    pub fn from_config(config: &fmn_config::Config) -> Self {
        Self {
            fps: config.camera.fps,
            preview_while_skipping: config.scene.preview_while_skipping,
            default_wait_time: config.scene.default_wait_time,
            ..Self::default()
        }
    }

    fn effective_fps(&self) -> u32 {
        if self.windowed { 30 } else { self.fps }
    }

    fn validate(&self) -> Result<(), SceneError> {
        if self.effective_fps() == 0 {
            return Err(SceneError::InvalidConfig(
                "fps must be nonzero (unless windowed, which fixes it at 30)",
            ));
        }
        if !self.default_wait_time.is_finite() || self.default_wait_time < 0.0 {
            return Err(SceneError::InvalidConfig(
                "default_wait_time must be finite and non-negative",
            ));
        }
        if let (Some(start), Some(end)) = (self.start_at_play, self.end_at_play)
            && end < start
        {
            return Err(SceneError::InvalidConfig(
                "end_at_play must not precede start_at_play",
            ));
        }
        Ok(())
    }
}

/// Play-level timing overrides. `None` preserves each animation's own value.
#[derive(Clone, Default)]
pub struct PlayOverrides {
    /// Override every member's run time.
    pub run_time: Option<f64>,
    /// Override every member's rate function.
    pub rate_func: Option<RateFunc>,
    /// Override every member's lag ratio.
    pub lag_ratio: Option<f64>,
}

impl fmt::Debug for PlayOverrides {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PlayOverrides")
            .field("run_time", &self.run_time)
            .field("has_rate_func", &self.rate_func.is_some())
            .field("lag_ratio", &self.lag_ratio)
            .finish()
    }
}

/// A named integration boundary failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationError {
    point: &'static str,
    message: String,
}

impl IntegrationError {
    /// Construct an error at a stable integration point (`sink`, `preflight`,
    /// `presenter`, …).
    #[must_use]
    pub fn new(point: &'static str, message: impl Into<String>) -> Self {
        Self {
            point,
            message: message.into(),
        }
    }

    /// Stable machine-readable integration point.
    #[must_use]
    pub fn point(&self) -> &'static str {
        self.point
    }

    /// Human-readable detail supplied by the adapter.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for IntegrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} integration failed: {}", self.point, self.message)
    }
}

impl std::error::Error for IntegrationError {}

/// The Rust spelling of the Reference's control-flow exception.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndScene;

impl fmt::Display for EndScene {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("scene ended")
    }
}

impl std::error::Error for EndScene {}

/// Errors surfaced by the Scene state machine.
#[derive(Debug)]
pub enum SceneError {
    /// Invalid semantic configuration.
    InvalidConfig(&'static str),
    /// An operation is illegal in the current lifecycle state.
    InvalidLifecycle(&'static str),
    /// A restored state cannot be represented on this scene's clock.
    InvalidState(&'static str),
    /// A durable restore contains updater identities whose callables have not
    /// yet been explicitly rebound.
    UnboundUpdaters,
    /// Marionette ownership/family failure.
    Stage(StageError),
    /// Choreo lifecycle/clock failure.
    Animation(AnimError),
    /// Durable-state decode failure.
    Persist(PersistError),
    /// Durable-state encode failure.
    Serialize(SerialError),
    /// Scribe/Reel/Studio/presenter adapter failure.
    Integration(IntegrationError),
    /// Normal early scene termination, caught by [`Scene::run`].
    EndScene(EndScene),
}

impl fmt::Display for SceneError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig(message) => write!(f, "invalid scene configuration: {message}"),
            Self::InvalidLifecycle(message) => write!(f, "invalid scene lifecycle: {message}"),
            Self::InvalidState(message) => write!(f, "invalid scene state: {message}"),
            Self::UnboundUpdaters => f.write_str(
                "decoded scene state carries updater identities whose callables are not rebound",
            ),
            Self::Stage(error) => write!(f, "scene stage failed: {error}"),
            Self::Animation(error) => write!(f, "scene animation failed: {error}"),
            Self::Persist(error) => write!(f, "scene-state decode failed: {error}"),
            Self::Serialize(error) => write!(f, "scene-state encode failed: {error}"),
            Self::Integration(error) => error.fmt(f),
            Self::EndScene(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for SceneError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Stage(error) => Some(error),
            Self::Animation(error) => Some(error),
            Self::Persist(error) => Some(error),
            Self::Serialize(error) => Some(error),
            Self::Integration(error) => Some(error),
            Self::EndScene(error) => Some(error),
            Self::InvalidConfig(_)
            | Self::InvalidLifecycle(_)
            | Self::InvalidState(_)
            | Self::UnboundUpdaters => None,
        }
    }
}

impl From<StageError> for SceneError {
    fn from(error: StageError) -> Self {
        Self::Stage(error)
    }
}

impl From<AnimError> for SceneError {
    fn from(error: AnimError) -> Self {
        Self::Animation(error)
    }
}

impl From<PersistError> for SceneError {
    fn from(error: PersistError) -> Self {
        Self::Persist(error)
    }
}

impl From<SerialError> for SceneError {
    fn from(error: SerialError) -> Self {
        Self::Serialize(error)
    }
}

impl From<IntegrationError> for SceneError {
    fn from(error: IntegrationError) -> Self {
        Self::Integration(error)
    }
}

impl From<EndScene> for SceneError {
    fn from(error: EndScene) -> Self {
        Self::EndScene(error)
    }
}

/// Why an immutable packet crossed the Scene boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureReason {
    /// An ordinary sampled play/wait frame.
    Segment,
    /// An explicit [`Scene::show`] request.
    Show,
    /// The configured windowed preview at the end of a skipped segment.
    SkippedPreview,
    /// A frame emitted while presenter mode is holding.
    PresenterHold,
}

/// A structured Scene lifecycle phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecyclePhase {
    /// `Scene::run` began.
    SceneBegin,
    /// The program's setup hook is about to run.
    Setup,
    /// The program's construct hook is about to run.
    Construct,
    /// The program's interactive tail is about to run.
    Interact,
    /// The program's tear-down hook is about to run.
    TearDown,
    /// `Scene::run` completed.
    SceneEnd,
    /// The one-shot constructed-scene preflight completed.
    Preflight,
    /// Range/skip/presenter preparation completed.
    PrePlay,
    /// Choreo is about to begin/progress/finish the segment.
    DriveSegment,
    /// Choreo completed the segment.
    FinishSegment,
    /// Writer/preview bookkeeping is running.
    PostPlay,
    /// A presenter hold loop began.
    HoldBegin,
    /// A presenter hold loop released.
    HoldEnd,
}

/// Stable structured metadata for a lifecycle phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LifecycleEvent {
    /// Phase.
    pub phase: LifecyclePhase,
    /// Current completed-play count (the index about to run during pre-play).
    pub play_index: u64,
    /// Exact scene time.
    pub time: RationalTime,
    /// Segment kind when the phase belongs to a play/wait.
    pub segment: Option<SegmentKind>,
    /// Effective skip status at this point.
    pub skipping: bool,
}

/// The lower-boundary adapter implemented by the CLI/Studio composition root.
///
/// `capture` receives immutable front-end state, not pixels. An implementation
/// synchronizes Lumen from the packet, renders a frame, then submits that frame
/// to Reel's ordered emitter. This preserves the downward crate DAG.
pub trait SceneSink {
    /// Observe a structured lifecycle event.
    fn event(&mut self, event: LifecycleEvent) -> Result<(), IntegrationError> {
        let _ = event;
        Ok(())
    }

    /// Consume one immutable capture.
    fn capture(
        &mut self,
        reason: CaptureReason,
        packet: FramePacket,
    ) -> Result<(), IntegrationError>;
}

/// A headless sink that discards captures and events.
#[derive(Debug, Default)]
pub struct NullSceneSink;

impl SceneSink for NullSceneSink {
    fn capture(
        &mut self,
        _reason: CaptureReason,
        _packet: FramePacket,
    ) -> Result<(), IntegrationError> {
        Ok(())
    }
}

trait PreflightHook {
    fn preflight(&mut self, stage: &Stage, anchors: &[Mob]) -> Result<(), IntegrationError>;
}

impl<F> PreflightHook for F
where
    F: FnMut(&Stage, &[Mob]) -> Result<(), IntegrationError>,
{
    fn preflight(&mut self, stage: &Stage, anchors: &[Mob]) -> Result<(), IntegrationError> {
        self(stage, anchors)
    }
}

#[derive(Debug)]
struct NoPreflight;

impl PreflightHook for NoPreflight {
    fn preflight(&mut self, _stage: &Stage, _roots: &[Mob]) -> Result<(), IntegrationError> {
        Ok(())
    }
}

/// Which presenter pause is being driven.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldKind {
    /// The Reference's first-pre-play presenter pause.
    Initial,
    /// A presenter-mode `wait`.
    Wait,
}

/// One deterministic poll result from a presenter host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldDecision {
    /// Advance exactly one rational-clock frame, capture, and poll again.
    Continue,
    /// Release this hold.
    Release,
    /// Terminate the scene normally.
    EndScene,
}

/// Host input for presenter mode.
///
/// The callback is polled only in the serial front-end, before each hold frame.
/// A Studio/window implementation folds its queued input into this decision;
/// tests can provide a counter. The headless default releases immediately, so
/// accidentally enabling presenter mode in a batch cannot hang forever.
pub trait HoldController {
    /// Decide whether the current hold advances, releases, or ends the scene.
    fn poll(
        &mut self,
        kind: HoldKind,
        stage: &Stage,
        time: RationalTime,
    ) -> Result<HoldDecision, IntegrationError>;
}

impl<F> HoldController for F
where
    F: FnMut(HoldKind, &Stage, RationalTime) -> Result<HoldDecision, IntegrationError>,
{
    fn poll(
        &mut self,
        kind: HoldKind,
        stage: &Stage,
        time: RationalTime,
    ) -> Result<HoldDecision, IntegrationError> {
        self(kind, stage, time)
    }
}

#[derive(Debug)]
struct ImmediateRelease;

impl HoldController for ImmediateRelease {
    fn poll(
        &mut self,
        _kind: HoldKind,
        _stage: &Stage,
        _time: RationalTime,
    ) -> Result<HoldDecision, IntegrationError> {
        Ok(HoldDecision::Release)
    }
}

/// A Scene program's lifecycle hooks.
///
/// Passing the sink explicitly keeps the program able to call `play`, `wait`,
/// and `show` without storing a self-referential adapter inside [`Scene`].
pub trait SceneProgram {
    /// Stable scene name used by output naming and registry surfaces.
    fn name(&self) -> &str {
        "Scene"
    }

    /// Common subclass setup.
    fn setup(&mut self, _scene: &mut Scene, _sink: &mut dyn SceneSink) -> Result<(), SceneError> {
        Ok(())
    }

    /// Where scene construction and playback occur.
    fn construct(&mut self, scene: &mut Scene, sink: &mut dyn SceneSink) -> Result<(), SceneError>;

    /// Interactive tail; W9 events/Studio override this later.
    fn interact(
        &mut self,
        _scene: &mut Scene,
        _sink: &mut dyn SceneSink,
    ) -> Result<(), SceneError> {
        Ok(())
    }

    /// Common subclass teardown.
    fn tear_down(
        &mut self,
        _scene: &mut Scene,
        _sink: &mut dyn SceneSink,
    ) -> Result<(), SceneError> {
        Ok(())
    }
}

/// Summary of one [`Scene::run`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SceneRunReport {
    /// Whether [`EndScene`] ended construct/interact early.
    pub ended_early: bool,
    /// Completed play/wait calls.
    pub play_count: u64,
    /// Exact final time.
    pub time: RationalTime,
}

/// Result metadata from a durable state restore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SceneStateRestore {
    /// Identities whose callables durable bytes cannot contain.
    pub updaters: UpdaterManifest,
}

/// The Scene state machine.
pub struct Scene {
    config: RuntimeConfig,
    stage: Stage,
    clock: RationalFrameClock,
    rng_root: RngRoot,
    scene_rng: Pcg64Dxsm,
    play_count: u64,
    skipping: bool,
    original_skipping: bool,
    preflight_done: bool,
    preflight: Box<dyn PreflightHook>,
    hold_controller: Box<dyn HoldController>,
    unbound_updaters: Option<UpdaterManifest>,
}

impl fmt::Debug for Scene {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Scene")
            .field("config", &self.config)
            .field("time", &self.clock.now())
            .field("play_count", &self.play_count)
            .field("skipping", &self.skipping)
            .field("preflight_done", &self.preflight_done)
            .field("has_unbound_updaters", &self.unbound_updaters.is_some())
            .finish_non_exhaustive()
    }
}

impl Default for Scene {
    fn default() -> Self {
        Self::new(RuntimeConfig::default(), 0).expect("default scene config is valid")
    }
}

impl Scene {
    /// Construct a scene from resolved runtime semantics and a deterministic
    /// root seed.
    pub fn new(config: RuntimeConfig, seed: u64) -> Result<Self, SceneError> {
        config.validate()?;
        let fps = config.effective_fps();
        let rng_root = RngRoot::from_seed(seed);
        let scene_rng = rng_root.substream("scene").sequential();
        let original_skipping = config.skip_animations;
        let skipping = original_skipping || config.start_at_play.is_some();
        Ok(Self {
            config,
            stage: Stage::new(),
            clock: RationalFrameClock::new(fps).map_err(AnimError::Clock)?,
            rng_root,
            scene_rng,
            play_count: 0,
            skipping,
            original_skipping,
            preflight_done: false,
            preflight: Box::new(NoPreflight),
            hold_controller: Box::new(ImmediateRelease),
            unbound_updaters: None,
        })
    }

    /// Resolved runtime config. `fps()` is the effective value after the
    /// windowed override.
    #[must_use]
    pub fn config(&self) -> &RuntimeConfig {
        &self.config
    }

    /// Effective frame rate.
    #[must_use]
    pub fn fps(&self) -> u32 {
        self.clock.fps()
    }

    /// Exact scene time.
    #[must_use]
    pub fn time(&self) -> RationalTime {
        self.clock.now()
    }

    /// Completed segment count.
    #[must_use]
    pub fn play_count(&self) -> u64 {
        self.play_count
    }

    /// Current effective skip status.
    #[must_use]
    pub fn is_skipping(&self) -> bool {
        self.skipping
    }

    /// Scene arena.
    #[must_use]
    pub fn stage(&self) -> &Stage {
        &self.stage
    }

    /// Mutable scene arena for construction and direct mobject operations.
    pub fn stage_mut(&mut self) -> &mut Stage {
        &mut self.stage
    }

    /// The scene-serial named RNG substream. Frame work must use
    /// [`FramePacket::rng_fork`] instead.
    pub fn rng_mut(&mut self) -> &mut Pcg64Dxsm {
        &mut self.scene_rng
    }

    /// Install Scribe's constructed-scene preflight.
    ///
    /// The anchors are every current scene root plus any otherwise-unrooted
    /// mobjects in the first play's animation closure. This ensures a static
    /// typeset target cannot first appear after the one-shot walk.
    ///
    /// It must be installed before the first segment/show so an adapter cannot
    /// silently miss frame zero.
    pub fn set_preflight_hook(
        &mut self,
        hook: impl FnMut(&Stage, &[Mob]) -> Result<(), IntegrationError> + 'static,
    ) -> Result<(), SceneError> {
        if self.preflight_done {
            return Err(SceneError::InvalidLifecycle(
                "preflight hook cannot change after preflight has run",
            ));
        }
        self.preflight = Box::new(hook);
        Ok(())
    }

    /// Install the presenter/window event adapter.
    pub fn set_hold_controller(&mut self, controller: impl HoldController + 'static) {
        self.hold_controller = Box::new(controller);
    }

    /// Add a detached mobject to the arena and root it in one operation.
    pub fn add_mobject(&mut self, mobject: impl Into<Mobject>) -> Result<Mob, SceneError> {
        let mob = self.stage.add(mobject);
        self.stage.add_to_scene(mob)?;
        Ok(mob)
    }

    /// Root live handles using Marionette's stable `(z_index, position)` rule.
    pub fn add(&mut self, mobs: &[Mob]) -> Result<&mut Self, SceneError> {
        self.stage.add_many_to_scene(mobs)?;
        Ok(self)
    }

    /// Remove handles from the draw list with rooted-family splice semantics.
    pub fn remove(&mut self, mobs: &[Mob]) -> &mut Self {
        self.stage.remove_many_from_scene(mobs);
        self
    }

    /// Re-add handles, which is exactly the Reference's promotion operation.
    pub fn bring_to_front(&mut self, mobs: &[Mob]) -> Result<&mut Self, SceneError> {
        self.add(mobs)
    }

    /// Prepend handles without re-sorting.
    pub fn bring_to_back(&mut self, mobs: &[Mob]) -> Result<&mut Self, SceneError> {
        self.stage.bring_many_to_back(mobs)?;
        Ok(self)
    }

    /// Clear the scene draw list without deleting arena entries.
    pub fn clear(&mut self) -> &mut Self {
        for mob in self.stage.roots().to_vec() {
            self.stage.remove_from_scene(mob);
        }
        self
    }

    /// The current top-level draw list.
    #[must_use]
    pub fn mobjects(&self) -> &[Mob] {
        self.stage.roots()
    }

    /// Drive one play through `prepare overrides → pre_play →
    /// begin/progress/finish → post_play`.
    ///
    /// An empty list is the Reference's warning-only no-op, represented as
    /// `Ok(None)`. A sink failure during capture is surfaced at the segment
    /// boundary after Choreo has deterministically finished the segment and
    /// post-play has incremented the counter; later captures are suppressed.
    pub fn play(
        &mut self,
        mut animations: Vec<Box<dyn Animation>>,
        overrides: PlayOverrides,
        sink: &mut dyn SceneSink,
    ) -> Result<Option<SegmentReport>, SceneError> {
        if animations.is_empty() {
            return Ok(None);
        }
        for animation in &mut animations {
            animation.update_rate_info(
                overrides.run_time,
                overrides.rate_func.clone(),
                overrides.lag_ratio,
            );
        }
        let animation_anchors: Vec<Mob> = animations
            .iter()
            .flat_map(|animation| animation.preflight_mobjects())
            .collect();
        self.pre_play(SegmentKind::Play, &animation_anchors, sink)?;
        self.emit_event(sink, LifecyclePhase::DriveSegment, Some(SegmentKind::Play))?;

        let mut sink_error = None;
        let report = {
            let mut emit = |packet| {
                if sink_error.is_none()
                    && let Err(error) = sink.capture(CaptureReason::Segment, packet)
                {
                    sink_error = Some(error);
                }
            };
            play_segment(
                &mut self.stage,
                &mut self.clock,
                &self.rng_root,
                &mut animations,
                self.skipping,
                &mut emit,
            )?
        };
        self.sync_stage_time();

        let finish_result = if sink_error.is_none() {
            self.emit_event(sink, LifecyclePhase::FinishSegment, Some(SegmentKind::Play))
        } else {
            Ok(())
        };
        let post_result = self.post_play(SegmentKind::Play, sink, sink_error.is_none());
        if let Some(error) = sink_error {
            return Err(error.into());
        }
        finish_result?;
        post_result?;
        Ok(Some(report))
    }

    /// Wait for `duration`, or the configured default when `None`.
    pub fn wait(
        &mut self,
        duration: Option<f64>,
        sink: &mut dyn SceneSink,
    ) -> Result<SegmentReport, SceneError> {
        self.wait_impl(duration, None, false, sink)
    }

    /// Wait until `stop_condition` turns true, or `max_time` elapses.
    pub fn wait_until(
        &mut self,
        max_time: f64,
        stop_condition: &mut dyn FnMut(&Stage) -> bool,
        sink: &mut dyn SceneSink,
    ) -> Result<SegmentReport, SceneError> {
        self.wait_impl(Some(max_time), Some(stop_condition), false, sink)
    }

    /// Wait while explicitly bypassing presenter mode.
    pub fn wait_ignoring_presenter(
        &mut self,
        duration: Option<f64>,
        sink: &mut dyn SceneSink,
    ) -> Result<SegmentReport, SceneError> {
        self.wait_impl(duration, None, true, sink)
    }

    fn wait_impl(
        &mut self,
        duration: Option<f64>,
        stop_condition: Option<&mut dyn FnMut(&Stage) -> bool>,
        ignore_presenter: bool,
        sink: &mut dyn SceneSink,
    ) -> Result<SegmentReport, SceneError> {
        let duration = duration.unwrap_or(self.config.default_wait_time);
        if !duration.is_finite() || duration < 0.0 {
            return Err(SceneError::InvalidConfig(
                "wait duration must be finite and non-negative",
            ));
        }
        self.pre_play(SegmentKind::Wait, &[], sink)?;
        self.emit_event(sink, LifecyclePhase::DriveSegment, Some(SegmentKind::Wait))?;

        let report = if self.config.presenter_mode && !self.skipping && !ignore_presenter {
            self.presenter_wait(sink)?
        } else {
            let mut sink_error = None;
            let report = {
                let mut emit = |packet| {
                    if sink_error.is_none()
                        && let Err(error) = sink.capture(CaptureReason::Segment, packet)
                    {
                        sink_error = Some(error);
                    }
                };
                wait_segment(
                    &mut self.stage,
                    &mut self.clock,
                    &self.rng_root,
                    duration,
                    stop_condition,
                    self.skipping,
                    &mut emit,
                )?
            };
            self.sync_stage_time();
            if let Some(error) = sink_error {
                self.post_play(SegmentKind::Wait, sink, false)?;
                return Err(error.into());
            }
            report
        };

        let finish_result =
            self.emit_event(sink, LifecyclePhase::FinishSegment, Some(SegmentKind::Wait));
        let post_result = self.post_play(SegmentKind::Wait, sink, finish_result.is_ok());
        finish_result?;
        post_result?;
        Ok(report)
    }

    /// Force a current-state capture without advancing time.
    pub fn show(&mut self, sink: &mut dyn SceneSink) -> Result<(), SceneError> {
        self.ensure_preflight(&[], sink)?;
        for root in self.stage.roots().to_vec() {
            self.stage.update_mobject(root, 0.0);
        }
        self.sync_stage_time();
        sink.capture(
            CaptureReason::Show,
            FramePacket::freeze_barrier(&self.stage, &self.clock, &self.rng_root),
        )?;
        Ok(())
    }

    /// Capture the in-memory SceneState. Updater callables remain shared in
    /// the CoW snapshot.
    ///
    /// # Errors
    /// [`SceneError::UnboundUpdaters`] while a durable manifest is pending;
    /// capturing that callable-free intermediate state would create an
    /// apparent escape from the replay barrier.
    pub fn state(&mut self) -> Result<SceneState, SceneError> {
        self.ensure_ready()?;
        self.sync_stage_time();
        Ok(SceneState::capture(
            &self.stage,
            self.play_count,
            &self.scene_rng,
        ))
    }

    /// Capture canonical durable SceneState bytes.
    pub fn state_bytes(&mut self) -> Result<Vec<u8>, SceneError> {
        Ok(self.state()?.to_bytes()?)
    }

    /// Restore an in-memory state, including updater callables.
    pub fn restore_state(&mut self, state: &SceneState) -> Result<(), SceneError> {
        self.apply_state(
            state.time,
            state.play_count,
            state.rng_state,
            &state.snapshot,
        )?;
        self.unbound_updaters = None;
        Ok(())
    }

    /// Decode and restore durable SceneState bytes.
    ///
    /// Updater callables are intentionally absent from the format. When the
    /// returned manifest is non-empty, playback fails closed with
    /// [`SceneError::UnboundUpdaters`] until
    /// [`Scene::rebind_updaters`] resolves every original identity.
    pub fn restore_state_bytes(&mut self, bytes: &[u8]) -> Result<SceneStateRestore, SceneError> {
        let decoded = SceneState::from_bytes(bytes, &self.stage)?;
        self.apply_state(
            decoded.time,
            decoded.play_count,
            decoded.rng_state,
            &decoded.snapshot,
        )?;
        let updaters = decoded.updaters;
        self.unbound_updaters = (!updaters.entries.is_empty()).then(|| updaters.clone());
        Ok(SceneStateRestore { updaters })
    }

    /// Reinstall every callable named by a decoded updater manifest.
    ///
    /// Resolution completes before Marionette mutates any updater list. The
    /// callback kind is checked against the durable manifest and the original
    /// [`UpdaterId`] is retained, so replay cannot silently substitute a
    /// different execution order or removal token.
    ///
    /// # Errors
    /// [`SceneError::InvalidLifecycle`] when no manifest is pending,
    /// [`SceneError::InvalidState`] when a resolver returns the wrong updater
    /// kind, or the resolver's [`IntegrationError`].
    pub fn rebind_updaters(
        &mut self,
        mut resolve: impl FnMut(Mob, UpdaterId, UpdaterKindTag) -> Result<UpdaterFn, IntegrationError>,
    ) -> Result<(), SceneError> {
        let manifest = self
            .unbound_updaters
            .as_ref()
            .ok_or(SceneError::InvalidLifecycle(
                "no decoded updater manifest is pending",
            ))?;
        let identities = manifest.identities(&self.stage)?;
        let mut bindings = Vec::with_capacity(identities.len());
        for identity in identities {
            let func = resolve(identity.mob, identity.id, identity.kind)?;
            let matches = matches!(
                (identity.kind, &func),
                (UpdaterKindTag::NonDt, UpdaterFn::NonDt(_))
                    | (UpdaterKindTag::Dt, UpdaterFn::Dt(_))
            );
            if !matches {
                return Err(SceneError::InvalidState(
                    "rebound updater kind does not match the durable manifest",
                ));
            }
            bindings.push((identity, func));
        }
        self.stage.restore_updater_bindings(bindings)?;
        self.unbound_updaters = None;
        Ok(())
    }

    /// Force skip mode while remembering the status to which revert returns.
    pub fn force_skipping(&mut self) -> &mut Self {
        self.original_skipping = self.skipping;
        self.skipping = true;
        self
    }

    /// Restore the skip status remembered by [`Scene::force_skipping`].
    pub fn revert_to_original_skipping_status(&mut self) -> &mut Self {
        self.skipping = self.original_skipping;
        self
    }

    /// Produce normal scene-termination control flow.
    pub fn end<T>(&mut self) -> Result<T, SceneError> {
        Err(EndScene.into())
    }

    /// Run setup → construct → interact, catch [`EndScene`], and always run
    /// teardown/end notifications.
    pub fn run(
        &mut self,
        program: &mut dyn SceneProgram,
        sink: &mut dyn SceneSink,
    ) -> Result<SceneRunReport, SceneError> {
        self.emit_event(sink, LifecyclePhase::SceneBegin, None)?;
        let body = (|| {
            self.emit_event(sink, LifecyclePhase::Setup, None)?;
            program.setup(self, sink)?;
            self.emit_event(sink, LifecyclePhase::Construct, None)?;
            program.construct(self, sink)?;
            self.emit_event(sink, LifecyclePhase::Interact, None)?;
            program.interact(self, sink)
        })();

        let (ended_early, body_error) = match body {
            Ok(()) => (false, None),
            Err(SceneError::EndScene(_)) => (true, None),
            Err(error) => (false, Some(error)),
        };

        // Cleanup is best-effort in the strong sense: neither a lifecycle
        // adapter failure nor a program tear-down failure suppresses the
        // remaining cleanup phases. Error precedence still preserves the
        // original body failure, then the earliest cleanup failure.
        let teardown_event = self.emit_event(sink, LifecyclePhase::TearDown, None);
        let teardown_body = program.tear_down(self, sink);
        let scene_end = self.emit_event(sink, LifecyclePhase::SceneEnd, None);

        if let Some(error) = body_error {
            return Err(error);
        }
        teardown_event?;
        teardown_body?;
        scene_end?;
        Ok(SceneRunReport {
            ended_early,
            play_count: self.play_count,
            time: self.clock.now(),
        })
    }

    fn ensure_ready(&self) -> Result<(), SceneError> {
        if self.unbound_updaters.is_some() {
            Err(SceneError::UnboundUpdaters)
        } else {
            Ok(())
        }
    }

    fn ensure_preflight(
        &mut self,
        animation_anchors: &[Mob],
        sink: &mut dyn SceneSink,
    ) -> Result<(), SceneError> {
        self.ensure_ready()?;
        if self.preflight_done {
            return Ok(());
        }
        let mut anchors = self.stage.roots().to_vec();
        let mut covered: Vec<Mob> = anchors
            .iter()
            .flat_map(|&root| self.stage.family(root))
            .collect();
        for &anchor in animation_anchors {
            if self.stage.contains(anchor) && !covered.contains(&anchor) {
                anchors.push(anchor);
                for member in self.stage.family(anchor) {
                    if !covered.contains(&member) {
                        covered.push(member);
                    }
                }
            }
        }
        self.preflight.preflight(&self.stage, &anchors)?;
        self.emit_event(sink, LifecyclePhase::Preflight, None)?;
        self.preflight_done = true;
        Ok(())
    }

    fn pre_play(
        &mut self,
        kind: SegmentKind,
        animation_anchors: &[Mob],
        sink: &mut dyn SceneSink,
    ) -> Result<(), SceneError> {
        if self.play_count == u64::MAX {
            return Err(SceneError::InvalidState("play count is exhausted"));
        }
        self.ensure_preflight(animation_anchors, sink)?;
        if self.config.presenter_mode && self.play_count == 0 {
            self.hold_loop(HoldKind::Initial, kind, sink)?;
        }
        self.update_skipping_status()?;
        self.emit_event(sink, LifecyclePhase::PrePlay, Some(kind))
    }

    fn post_play(
        &mut self,
        kind: SegmentKind,
        sink: &mut dyn SceneSink,
        notify_sink: bool,
    ) -> Result<(), SceneError> {
        let result = if notify_sink {
            if self.config.preview_while_skipping && self.skipping && self.config.windowed {
                self.sync_stage_time();
                let preview = sink.capture(
                    CaptureReason::SkippedPreview,
                    FramePacket::freeze_barrier(&self.stage, &self.clock, &self.rng_root),
                );
                if let Err(error) = preview {
                    self.play_count += 1;
                    return Err(error.into());
                }
            }
            self.emit_event(sink, LifecyclePhase::PostPlay, Some(kind))
        } else {
            Ok(())
        };
        self.play_count += 1;
        result
    }

    fn update_skipping_status(&mut self) -> Result<(), SceneError> {
        if self.config.start_at_play == Some(self.play_count) && !self.original_skipping {
            self.skipping = false;
        }
        if self
            .config
            .end_at_play
            .is_some_and(|end| self.play_count >= end)
        {
            return Err(EndScene.into());
        }
        Ok(())
    }

    fn presenter_wait(&mut self, sink: &mut dyn SceneSink) -> Result<SegmentReport, SceneError> {
        let base_frame = self.clock.now().frames();
        for root in self.stage.roots().to_vec() {
            self.stage.update_mobject(root, 0.0);
        }
        self.hold_loop(HoldKind::Wait, SegmentKind::Wait, sink)?;
        let n_frames = self.clock.now().frames() - base_frame;
        Ok(SegmentReport {
            kind: SegmentKind::Wait,
            purity: Purity::Stateful(vec![ImpureEffect::StopCondition]),
            begin_state: None,
            base_frame,
            n_frames,
            run_time: (RationalTime::zero(self.clock.fps()) + n_frames).to_f64(),
        })
    }

    fn hold_loop(
        &mut self,
        kind: HoldKind,
        segment: SegmentKind,
        sink: &mut dyn SceneSink,
    ) -> Result<(), SceneError> {
        self.emit_event(sink, LifecyclePhase::HoldBegin, Some(segment))?;
        loop {
            match self
                .hold_controller
                .poll(kind, &self.stage, self.clock.now())?
            {
                HoldDecision::Release => break,
                HoldDecision::EndScene => return Err(EndScene.into()),
                HoldDecision::Continue => {
                    self.clock.advance_frames(1).map_err(AnimError::Clock)?;
                    self.stage
                        .update_at_time(self.clock.dt().to_f64(), self.clock.now().to_f64());
                    if !self.skipping {
                        sink.capture(
                            CaptureReason::PresenterHold,
                            FramePacket::freeze_barrier(&self.stage, &self.clock, &self.rng_root),
                        )?;
                    }
                }
            }
        }
        self.emit_event(sink, LifecyclePhase::HoldEnd, Some(segment))
    }

    fn emit_event(
        &self,
        sink: &mut dyn SceneSink,
        phase: LifecyclePhase,
        segment: Option<SegmentKind>,
    ) -> Result<(), SceneError> {
        sink.event(LifecycleEvent {
            phase,
            play_index: self.play_count,
            time: self.clock.now(),
            segment,
            skipping: self.skipping,
        })?;
        Ok(())
    }

    fn sync_stage_time(&mut self) {
        self.stage.set_time_from_clock(self.clock.now().to_f64());
    }

    fn apply_state(
        &mut self,
        time: f64,
        play_count: u64,
        rng_state: ([u64; 2], [u64; 2]),
        snapshot: &fmn_mobject::Snapshot,
    ) -> Result<(), SceneError> {
        let frames = frames_for_restored_time(time, self.clock.fps())?;
        if !self.stage.can_restore(snapshot) {
            return Err(SceneError::InvalidState(
                "an in-memory SceneState belongs to a different scene",
            ));
        }
        self.stage.restore(snapshot);
        self.clock = RationalFrameClock::new(self.clock.fps()).map_err(AnimError::Clock)?;
        self.clock
            .advance_frames(frames)
            .map_err(AnimError::Clock)?;
        self.sync_stage_time();
        self.play_count = play_count;
        self.scene_rng = Pcg64Dxsm::restore(rng_state.0, rng_state.1);
        self.preflight_done = false;
        self.skipping = self.original_skipping
            || self
                .config
                .start_at_play
                .is_some_and(|start| play_count < start);
        Ok(())
    }
}

fn frames_for_restored_time(time: f64, fps: u32) -> Result<i64, SceneError> {
    if !time.is_finite() || time < 0.0 {
        return Err(SceneError::InvalidState(
            "time must be finite and non-negative",
        ));
    }
    let scaled = time * f64::from(fps);
    // `i64::MAX as f64` rounds upward to 2^63, which is already one frame
    // outside the counter. Reject equality as well as larger values.
    if !scaled.is_finite() || scaled >= i64::MAX as f64 {
        return Err(SceneError::InvalidState(
            "time exceeds the rational frame counter",
        ));
    }
    let rounded = scaled.round();
    let tolerance = f64::EPSILON * scaled.abs().max(1.0) * 4.0;
    if (scaled - rounded).abs() > tolerance {
        return Err(SceneError::InvalidState(
            "time is not on this scene's frame grid",
        ));
    }
    Ok(rounded as i64)
}

/// Camera-frame orientation, stored in radians.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CameraOrientation {
    /// ZXZ theta.
    pub theta: f64,
    /// ZXZ phi.
    pub phi: f64,
    /// ZXZ gamma.
    pub gamma: f64,
}

impl CameraOrientation {
    /// Construct from the Reference's degree-facing convenience surface.
    #[must_use]
    pub fn from_degrees(theta: f64, phi: f64, gamma: f64) -> Self {
        Self {
            theta: theta.to_radians(),
            phi: phi.to_radians(),
            gamma: gamma.to_radians(),
        }
    }
}

/// Options on [`ThreeDScene::add`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThreeDAddOptions {
    /// Apply depth testing to non-fixed objects when the scene default allows.
    pub set_depth_test: bool,
    /// Make stroked vector objects camera-facing (`flat_stroke = false`).
    pub perpendicular_stroke: bool,
}

impl Default for ThreeDAddOptions {
    fn default() -> Self {
        Self {
            set_depth_test: true,
            perpendicular_stroke: true,
        }
    }
}

/// Scene plus the Reference's ThreeDScene defaults.
///
/// The camera frame is represented by one rooted empty mobject with three
/// tracker children. That makes orientation and ambient rotation ordinary,
/// snapshotted Marionette state today; the future projection layer can consume
/// these handles without replacing the Scene semantics.
pub struct ThreeDScene {
    scene: Scene,
    camera_root: Mob,
    theta: Mob,
    phi: Mob,
    gamma: Mob,
    samples: u8,
    always_depth_test: bool,
}

impl fmt::Debug for ThreeDScene {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ThreeDScene")
            .field("scene", &self.scene)
            .field("orientation", &self.orientation())
            .field("samples", &self.samples)
            .field("always_depth_test", &self.always_depth_test)
            .finish()
    }
}

impl ThreeDScene {
    /// Construct at the defaults: 4 samples, `(-30°, 70°, 0°)`, depth test
    /// enabled.
    pub fn new(config: RuntimeConfig, seed: u64) -> Result<Self, SceneError> {
        let mut scene = Scene::new(config, seed)?;
        let defaults =
            CameraOrientation::from_degrees(THREE_D_THETA_DEGREES, THREE_D_PHI_DEGREES, 0.0);
        let theta = scene.stage.add_value_tracker(defaults.theta);
        let phi = scene.stage.add_value_tracker(defaults.phi);
        let gamma = scene.stage.add_value_tracker(defaults.gamma);
        let camera_root = scene.stage.add(Mobject::new());
        for child in [theta, phi, gamma] {
            scene.stage.attach(camera_root, child)?;
        }
        scene.stage.add_to_scene(camera_root)?;
        Ok(Self {
            scene,
            camera_root,
            theta,
            phi,
            gamma,
            samples: THREE_D_SAMPLES,
            always_depth_test: true,
        })
    }

    /// Shared Scene surface.
    #[must_use]
    pub fn scene(&self) -> &Scene {
        &self.scene
    }

    /// Mutable shared Scene surface.
    pub fn scene_mut(&mut self) -> &mut Scene {
        &mut self.scene
    }

    /// Camera frame root handle.
    #[must_use]
    pub fn camera_root(&self) -> Mob {
        self.camera_root
    }

    /// Multisample default.
    #[must_use]
    pub fn samples(&self) -> u8 {
        self.samples
    }

    /// Whether ordinary 3D additions receive depth testing.
    #[must_use]
    pub fn always_depth_test(&self) -> bool {
        self.always_depth_test
    }

    /// Set the scene-level depth default.
    pub fn set_always_depth_test(&mut self, value: bool) {
        self.always_depth_test = value;
    }

    /// Current camera orientation.
    #[must_use]
    pub fn orientation(&self) -> CameraOrientation {
        CameraOrientation {
            theta: self.scene.stage.tracker_value(self.theta).unwrap_or(0.0),
            phi: self.scene.stage.tracker_value(self.phi).unwrap_or(0.0),
            gamma: self.scene.stage.tracker_value(self.gamma).unwrap_or(0.0),
        }
    }

    /// Set all three Euler angles.
    pub fn set_orientation(
        &mut self,
        orientation: CameraOrientation,
    ) -> Result<&mut Self, SceneError> {
        if !orientation.theta.is_finite()
            || !orientation.phi.is_finite()
            || !orientation.gamma.is_finite()
        {
            return Err(SceneError::InvalidConfig(
                "camera orientation angles must be finite",
            ));
        }
        if [self.theta, self.phi, self.gamma]
            .iter()
            .any(|&tracker| self.scene.stage.tracker_value(tracker).is_none())
        {
            return Err(StageError::StaleHandle.into());
        }
        self.scene
            .stage
            .set_tracker_value(self.theta, orientation.theta)?;
        self.scene
            .stage
            .set_tracker_value(self.phi, orientation.phi)?;
        self.scene
            .stage
            .set_tracker_value(self.gamma, orientation.gamma)?;
        Ok(self)
    }

    /// Add the Reference's ambient theta rotation as a dt updater.
    pub fn add_ambient_rotation(&mut self, angular_speed: f64) -> Result<UpdaterId, SceneError> {
        if !angular_speed.is_finite() {
            return Err(SceneError::InvalidConfig(
                "ambient angular speed must be finite",
            ));
        }
        Ok(self.scene.stage.add_dt_updater(
            self.theta,
            move |stage, theta, dt| {
                if let Some(current) = stage.tracker_value(theta) {
                    let _ = stage.set_tracker_value(theta, current + angular_speed * dt);
                }
            },
            false,
        )?)
    }

    /// Remove a previously installed ambient-rotation updater.
    pub fn remove_ambient_rotation(&mut self, updater: UpdaterId) {
        self.scene.stage.remove_updater(self.theta, updater);
    }

    /// Add 3D mobjects with depth/perpendicular-stroke defaults, then use the
    /// ordinary stable Scene ordering.
    pub fn add(
        &mut self,
        mobs: &[Mob],
        options: ThreeDAddOptions,
    ) -> Result<&mut Self, SceneError> {
        if mobs.iter().any(|&mob| !self.scene.stage.contains(mob)) {
            return Err(StageError::StaleHandle.into());
        }
        for &mob in mobs {
            let fixed = self
                .scene
                .stage
                .uniforms(mob)
                .is_some_and(|uniforms| uniforms.is_fixed_in_frame == 1.0);
            let family = self.scene.stage.family(mob);
            if options.set_depth_test && self.always_depth_test && !fixed {
                for member in &family {
                    if let Some(uniforms) = self.scene.stage.uniforms_mut(*member) {
                        uniforms.depth_test = true;
                    }
                }
            }
            if options.perpendicular_stroke {
                for member in family {
                    if has_stroke(&self.scene.stage, member)
                        && let Some(uniforms) = self.scene.stage.uniforms_mut(member)
                    {
                        uniforms.flat_stroke = false;
                    }
                }
            }
        }
        self.scene.add(mobs)?;
        Ok(self)
    }
}

impl Default for ThreeDScene {
    fn default() -> Self {
        Self::new(RuntimeConfig::default(), 0).expect("default 3D scene config is valid")
    }
}

impl std::ops::Deref for ThreeDScene {
    type Target = Scene;

    fn deref(&self) -> &Self::Target {
        &self.scene
    }
}

impl std::ops::DerefMut for ThreeDScene {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.scene
    }
}

fn has_stroke(stage: &Stage, mob: Mob) -> bool {
    let Some(entry) = stage.get(mob) else {
        return false;
    };
    let widths = entry.buffer.read_column("stroke_width");
    let rgba = entry.buffer.read_column("stroke_rgba");
    match (widths, rgba) {
        (Some(widths), Some(rgba)) => {
            let (colors, _) = rgba.as_chunks::<4>();
            widths
                .iter()
                .zip(colors)
                .any(|(width, color)| *width > 0.0 && color[3] > 0.0)
        }
        _ => false,
    }
}

/// Blank fallback used when no source module/registry is supplied.
#[derive(Debug, Default)]
pub struct BlankScene;

impl SceneProgram for BlankScene {
    fn name(&self) -> &str {
        "BlankScene"
    }

    fn construct(
        &mut self,
        _scene: &mut Scene,
        _sink: &mut dyn SceneSink,
    ) -> Result<(), SceneError> {
        Ok(())
    }
}

type SceneFactory = fn() -> Box<dyn SceneProgram>;

fn scene_factory<P>() -> Box<dyn SceneProgram>
where
    P: SceneProgram + Default + 'static,
{
    Box::new(P::default())
}

/// One named Rust scene registration.
#[derive(Clone)]
pub struct SceneRegistration {
    name: String,
    factory: SceneFactory,
}

impl fmt::Debug for SceneRegistration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SceneRegistration")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl SceneRegistration {
    /// Registered scene name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Construct a fresh program.
    #[must_use]
    pub fn instantiate(&self) -> Box<dyn SceneProgram> {
        (self.factory)()
    }
}

/// Ordered Rust scene registry mirroring `SCENES_IN_ORDER`.
#[derive(Debug, Clone, Default)]
pub struct SceneRegistry {
    scenes: Vec<SceneRegistration>,
}

impl SceneRegistry {
    /// No-module fallback.
    #[must_use]
    pub fn blank() -> Self {
        let mut registry = Self::default();
        registry
            .register::<BlankScene>("BlankScene")
            .expect("fresh registry");
        registry
    }

    /// Register a scene in explicit source order.
    pub fn register<P>(&mut self, name: impl Into<String>) -> Result<&mut Self, SceneSelectionError>
    where
        P: SceneProgram + Default + 'static,
    {
        let name = name.into();
        if name.is_empty() {
            return Err(SceneSelectionError::EmptyName);
        }
        if self.scenes.iter().any(|scene| scene.name == name) {
            return Err(SceneSelectionError::DuplicateName(name));
        }
        self.scenes.push(SceneRegistration {
            name,
            factory: scene_factory::<P>,
        });
        Ok(self)
    }

    /// Registrations in explicit order.
    #[must_use]
    pub fn scenes(&self) -> &[SceneRegistration] {
        &self.scenes
    }

    /// Reference selection semantics:
    ///
    /// - `write_all`, or a registry containing one class, selects all;
    /// - requested names are selected in request order and missing names are
    ///   reported without hiding valid selections;
    /// - multiple classes with no valid selection return `SelectionRequired`
    ///   instead of launching a blocking prompt inside the library.
    pub fn select<'a>(
        &'a self,
        requested: &[&str],
        write_all: bool,
    ) -> Result<SceneSelection<'a>, SceneSelectionError> {
        if self.scenes.is_empty() {
            return Err(SceneSelectionError::NoScenes);
        }
        if write_all || self.scenes.len() == 1 {
            return Ok(SceneSelection {
                scenes: self.scenes.iter().collect(),
                missing: Vec::new(),
            });
        }
        let mut scenes = Vec::new();
        let mut missing = Vec::new();
        for &name in requested {
            match self.scenes.iter().find(|scene| scene.name == name) {
                Some(scene) => scenes.push(scene),
                None => missing.push(name.to_owned()),
            }
        }
        if scenes.is_empty() {
            return Err(SceneSelectionError::SelectionRequired {
                choices: self.scenes.iter().map(|scene| scene.name.clone()).collect(),
                missing,
            });
        }
        Ok(SceneSelection { scenes, missing })
    }
}

/// A resolved scene selection plus names the registry did not contain.
#[derive(Debug)]
pub struct SceneSelection<'a> {
    /// Selected registrations, in selection order.
    pub scenes: Vec<&'a SceneRegistration>,
    /// Requested names that were not found.
    pub missing: Vec<String>,
}

/// Scene registration/selection refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SceneSelectionError {
    /// Empty registration name.
    EmptyName,
    /// Duplicate registration.
    DuplicateName(String),
    /// Registry had no scenes.
    NoScenes,
    /// A noninteractive caller must select from these choices.
    SelectionRequired {
        /// Available names in registry order.
        choices: Vec<String>,
        /// Requested names that did not exist.
        missing: Vec<String>,
    },
}

impl fmt::Display for SceneSelectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyName => f.write_str("scene registration name cannot be empty"),
            Self::DuplicateName(name) => write!(f, "duplicate scene registration {name:?}"),
            Self::NoScenes => f.write_str("no scenes are registered"),
            Self::SelectionRequired { choices, missing } => write!(
                f,
                "scene selection required; choices={choices:?}, missing={missing:?}"
            ),
        }
    }
}

impl std::error::Error for SceneSelectionError {}

/// Pure output naming/path semantics owned by Scene and consumed by Reel/CLI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputNaming {
    /// Base output directory.
    pub output_directory: PathBuf,
    /// Explicit `--file_name`, if any.
    pub file_name: Option<PathBuf>,
    /// Range suffix start.
    pub start_at_play: Option<u64>,
    /// Range suffix end.
    pub end_at_play: Option<u64>,
    /// Host request corresponding to `-o`; no subprocess is invoked here (D2).
    pub open_on_completion: bool,
}

impl Default for OutputNaming {
    fn default() -> Self {
        Self {
            output_directory: PathBuf::from("."),
            file_name: None,
            start_at_play: None,
            end_at_play: None,
            open_on_completion: false,
        }
    }
}

impl OutputNaming {
    /// Root name before an output format's extension is applied.
    #[must_use]
    pub fn root(&self, scene_name: &str) -> PathBuf {
        let name = self.file_name.clone().unwrap_or_else(|| {
            let mut name = scene_name.to_owned();
            if let Some(start) = self.start_at_play {
                name.push('_');
                name.push_str(&start.to_string());
            }
            if let Some(end) = self.end_at_play {
                name.push('_');
                name.push_str(&end.to_string());
            }
            PathBuf::from(name)
        });
        self.output_directory.join(name)
    }

    /// Final artifact path. A leading dot on `extension` is accepted.
    #[must_use]
    pub fn artifact(&self, scene_name: &str, extension: &str) -> PathBuf {
        with_extension(self.root(scene_name), extension)
    }

    /// Subdivided output directory (the extensionless root).
    #[must_use]
    pub fn partial_directory(&self, scene_name: &str) -> PathBuf {
        self.root(scene_name)
    }

    /// `00000.ext`, `00001.ext`, … inside the subdivided root.
    #[must_use]
    pub fn partial_artifact(&self, scene_name: &str, play_index: u64, extension: &str) -> PathBuf {
        let file = format!("{play_index:05}");
        with_extension(self.partial_directory(scene_name).join(file), extension)
    }

    /// Host-facing completion request. D2 forbids spawning an OS opener; the
    /// CLI/Studio host may honor this request through its own capability.
    #[must_use]
    pub fn completion_request(&self, scene_name: &str, extension: &str) -> CompletionRequest {
        CompletionRequest {
            artifact: self.artifact(scene_name, extension),
            open: self.open_on_completion,
        }
    }
}

fn with_extension(mut path: PathBuf, extension: &str) -> PathBuf {
    path.set_extension(extension.strip_prefix('.').unwrap_or(extension));
    path
}

/// Output completion metadata consumed by a host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionRequest {
    /// Published artifact.
    pub artifact: PathBuf,
    /// Whether the host was asked to open/present it.
    pub open: bool,
}

impl CompletionRequest {
    /// Borrow the artifact as a path.
    #[must_use]
    pub fn artifact(&self) -> &Path {
        &self.artifact
    }
}
