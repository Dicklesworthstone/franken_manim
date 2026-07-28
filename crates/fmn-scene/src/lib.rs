//! Proscenium — the Scene runtime and replay foundations (§13.1, §13.4).
//!
//! The [`runtime`] module owns the Scene state machine on Choreo's rational
//! clock: lifecycle orchestration, frame/preflight/presenter integration
//! boundaries, stable Stage membership operations, skip/range semantics,
//! SceneState restore, 3D defaults, discovery/selection, and output naming.
//! The [`journal`] module is the replay journal + effect model — the one
//! record with three consumers: supervisor edit-replay, the purity
//! classifier's journaled evidence, and the pipeline's barrier vocabulary.
#![forbid(unsafe_code)]

pub mod journal;
pub mod runtime;

pub use journal::{
    AssetRead, BundleDivergence, CommandKind, CommandRecord, EffectClass, Entry, ImpureEffectTag,
    InvalidationReason, Journal, JournalError, ReplayAudit, ReplayPlan, ReproBundle,
    SubprocessRecord, plan_replay,
};
pub use runtime::{
    BlankScene, CameraOrientation, CaptureReason, CompletionRequest, EndScene, HoldController,
    HoldDecision, HoldKind, IntegrationError, LifecycleEvent, LifecyclePhase, NullSceneSink,
    OutputNaming, PlayOverrides, RuntimeConfig, Scene, SceneError, SceneProgram, SceneRegistration,
    SceneRegistry, SceneRunReport, SceneSelection, SceneSelectionError, SceneSink,
    SceneStateRestore, ThreeDAddOptions, ThreeDScene,
};
