//! Choreo: the animation engine — Animation trait, RationalFrameClock, timeline algebra (§9).
//!
//! Landed so far: the [`RationalFrameClock`] (§9.2, D-07, BN-02) — exact
//! rational time over the frame grid, with the Reference's emission
//! semantics and no off-grid API — and the Animation contract (§9.1,
//! fm-67a): the constructor surface, the `begin → interpolate(alpha) →
//! finish` lifecycle, the one normalized-alpha pipeline (`time_span` →
//! lag → rate), and the typed [`prepare_animation`] boundary shared with
//! fmn-mobject's `.animate` builder. Still to land: the six-step frame
//! order + FramePacket (fm-x79), the five mechanism families (fm-cye),
//! composition and timeline algebra (fm-hfe), segment purity
//! classification.
#![forbid(unsafe_code)]

pub mod animation;
pub mod clock;

pub use animation::{
    AnimConfig, AnimError, AnimState, Animation, DEFAULT_ANIMATION_LAG_RATIO,
    DEFAULT_ANIMATION_RUN_TIME, IntoAnimation, MethodAnimation, RateFunc, prepare_animation,
    sub_alpha, time_spanned_alpha,
};
pub use clock::{ClockError, FrameSample, FrameSegment, RationalFrameClock, RationalTime};
