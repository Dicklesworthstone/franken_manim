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
//! between Lumen/runtime workers and every sink. [`sound`] is the native
//! WAV/PCM path: sample-exact placement, a named deterministic resampler,
//! ordered gain/duck mixing, explicit clipping evidence, and certified WAV
//! bytes.
#![cfg_attr(
    any(
        test,
        all(
            target_arch = "x86_64",
            target_feature = "avx2",
            target_feature = "bmi2",
            target_feature = "fma"
        ),
        all(
            target_arch = "x86_64",
            target_feature = "avx512f",
            target_feature = "avx512bw",
            target_feature = "avx512dq",
            target_feature = "avx512vl"
        ),
        all(target_arch = "aarch64", target_feature = "neon")
    ),
    feature(portable_simd)
)]
#![cfg_attr(test, feature(test))]
#![forbid(unsafe_code)]

pub mod emitter;
pub mod ffmpeg;
pub mod negotiate;
pub mod sound;

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
pub use sound::{
    COMPILED_MIX_LANES, COMPILED_MIX_TIER, DitherPolicy, MixKernel, MixReport, MixerConfig,
    RESAMPLER_NAME, SoundCue, SoundError, SoundMixer,
};
