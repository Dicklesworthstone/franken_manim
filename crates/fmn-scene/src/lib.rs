//! Proscenium — the Scene runtime and replay foundations (§13.1, §13.4).
//!
//! The [`runtime`] module owns the Scene state machine on Choreo's rational
//! clock: lifecycle orchestration, frame/preflight/presenter integration
//! boundaries, stable Stage membership operations, skip/range semantics,
//! SceneState restore, 3D defaults, discovery/selection, and output naming.
//! [`events`] is the typed, insertion-ordered dispatcher and production input
//! inbox; [`interactive`] layers Reference-compatible selection/editing
//! behavior over that one serial pre-capture seam.
//! The [`journal`] module is the replay journal + effect model — the one
//! record with three consumers: supervisor edit-replay, the purity
//! classifier's journaled evidence, and the pipeline's barrier vocabulary.
#![forbid(unsafe_code)]

pub mod events;
pub mod interactive;
pub mod journal;
pub mod runtime;

pub use events::{
    DispatchState, EventDispatcher, EventError, EventInbox, EventListener, EventListenerId,
    EventPayload, EventPropagation, EventTarget, EventType, InputEvent, Key, Modifiers,
    MouseButton,
};
pub use interactive::{
    InteractiveAction, InteractiveClipboard, InteractiveScene, KeyBinding, REFERENCE_KEYBOARD_MAP,
    SelectionHighlight, SelectionRectangle,
};
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
    SceneStateRestore, SoundRequest, ThreeDAddOptions, ThreeDScene,
};
