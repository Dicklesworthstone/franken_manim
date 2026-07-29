//! The Studio: supervisor, crash-isolated worker protocol, preview server, and
//! TUI (§13.3, §13.5).
//!
//! The stable [`Supervisor`] owns rebuilds,
//! checkpoints, the warm cache, and replay planning.  A disposable scene
//! worker runs [`serve_worker`] over the canonical,
//! bounded [`protocol`] pipe.  Rebuilds and crashes replace the whole worker
//! executable — never `dlopen`, never hot-patching — then restore the nearest
//! reusable `SceneState` and replay the journal's verified suffix.
#![forbid(unsafe_code)]

pub mod host;
pub mod inspect;
pub mod protocol;
pub mod scrub;
pub mod supervisor;
pub mod tui;
pub mod worker;

pub use host::{
    CapabilityToken, FrameHub, HostError, MULTIPART_BOUNDARY, PngFrame, StudioHost,
    StudioHostConfig, StudioWorkerSession, TokenError,
};
pub use inspect::{
    DebugOverlaySnapshot, InspectError, InspectorLimits, InspectorNode, InspectorSnapshot,
    NativeSpanBinding, NodeOverlay, RecordFieldSnapshot, SourceSpanSnapshot, SpanKind,
    SpanRegistry, TileOverlay, UniformSnapshot, WindingDirection,
};
pub use protocol::{
    CURRENT_VERSION, Checkpoint, CrashReport, DebugLayerSet, FrameEncoding, FramePayload,
    FrameStream, FramingError, JournalReplay, ProtocolError, ProtocolLimits, ProtocolVersion,
    REQUEST_SCHEMA, RESPONSE_SCHEMA, RequestEnvelope, ResponseEnvelope, StudioDataKind,
    SupervisorRequest, TransportCapabilities, WorkerErrorCode, WorkerResponse, read_request,
    read_response, write_request, write_response,
};
pub use scrub::{
    ScrubMode, ScrubResult, commit_timeline_frame, preview_timeline_frame, scrub_timeline,
};
pub use supervisor::{
    BuildError, ChannelError, ChannelFailureKind, CheckpointSource, LaunchError, RebuildDriver,
    RecoveryReport, StdWorkerLauncher, Supervisor, SupervisorConfig, SupervisorError,
    SupervisorReply, WorkerArtifact, WorkerChannel, WorkerLauncher,
};
pub use tui::{TerminalPreview, TerminalProtocol, TuiError, TuiLimits};
pub use worker::{ServiceError, WorkerServeError, WorkerServeOutcome, WorkerService, serve_worker};

#[cfg(feature = "metal")]
pub use fmn_render::metal::{PreviewFallback, PreviewFrame, PreviewRenderer, PreviewRoute};
