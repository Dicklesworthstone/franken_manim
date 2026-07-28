//! Sink orchestration, the negotiated ffmpeg boundary, and sound (§14).
//!
//! Landed so far (fm-wj3): the **negotiated ffmpeg boundary v2** —
//! FFMPEG_PROTOCOL.md's implementation. [`negotiate`] is the pure
//! negotiation model and deterministic argv construction (no `vflip`,
//! no `eq`, structurally); [`ffmpeg`] is the sandboxed execution layer
//! over fmn-platform's process capability: fingerprinted tool
//! resolution, private per-job directories, environment allowlist,
//! artifact verification, and atomic publication. ffmpeg is optional —
//! its absence is a capability error naming the native alternative.
//!
//! [`emitter`] is the bounded, preallocated, frame-index-ordered handoff
//! between Lumen/runtime workers and every sink. Still to land in this
//! crate: the sound mixer (fm-0m7).
#![forbid(unsafe_code)]

pub mod emitter;
pub mod ffmpeg;
pub mod negotiate;

pub use emitter::{
    EmitterConfig, EmitterError, EmitterFailure, EmitterHandle, EmitterReport, EmitterStats,
    FrameReservation, FrameSink, OrderedEmitter, SinkBinding, SinkFailure, SinkMode, SinkReport,
    SinkWrite,
};
pub use ffmpeg::{
    Boundary, BoundaryError, BoundaryReport, EncoderCapabilities, FfmpegTool, HARDWARE_ENCODERS,
    JobLimits, NATIVE_ALTERNATIVE, Provenance,
};
pub use negotiate::{
    ColorDescription, Container, EncoderChoice, NegotiationError, Primaries, Transfer, VideoJob,
    WireFormat,
};
