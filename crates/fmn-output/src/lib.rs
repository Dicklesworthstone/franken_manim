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
//! Still to land in this crate: the ordered asynchronous emitter
//! (fm-hv4) and the sound mixer (fm-0m7).
#![forbid(unsafe_code)]

pub mod ffmpeg;
pub mod negotiate;

pub use ffmpeg::{
    Boundary, BoundaryError, BoundaryReport, EncoderCapabilities, FfmpegTool, HARDWARE_ENCODERS,
    JobLimits, NATIVE_ALTERNATIVE, Provenance,
};
pub use negotiate::{
    ColorDescription, Container, EncoderChoice, NegotiationError, Primaries, Transfer, VideoJob,
    WireFormat,
};
