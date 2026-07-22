//! Choreo: the animation engine — Animation trait, RationalFrameClock, timeline algebra (§9).
//!
//! Landed so far: the [`RationalFrameClock`] (§9.2, D-07, BN-02) — exact
//! rational time over the frame grid, with the Reference's emission
//! semantics and no off-grid API — and the Animation contract (§9.1,
//! fm-67a): the constructor surface, the `begin → interpolate(alpha) →
//! finish` lifecycle, the one normalized-alpha pipeline (`time_span` →
//! lag → rate), and the typed [`prepare_animation`] boundary shared with
//! fmn-mobject's `.animate` builder — and the six-step frame order with
//! the [`FramePacket`] freeze (§9.3, D-19, fm-x79): [`play_segment`] /
//! [`wait_segment`] drive the load-bearing order exactly (BN-10 corrects
//! the Reference's skip-mode double-dt), and capture freezes an immutable
//! CoW packet with derivable keyed RNG forks. The §9.5 segment-purity
//! classifier (fm-3xk) rides the drivers: every segment is classified
//! against a closed effect vocabulary (unknown demotes — R20), reported
//! for the replay journal, and pure segments carry their begin-state
//! snapshot so [`reconstruct_pure_frame`] can rebuild any frame
//! bit-identically from (snapshot, alpha, keyed RNG fork). Still to land:
//! the five mechanism families (fm-cye), composition and timeline algebra
//! (fm-hfe).
#![forbid(unsafe_code)]

pub mod animation;
pub mod clock;
pub mod frame;
pub mod purity;
pub mod transform;

pub use animation::{
    AnimConfig, AnimError, AnimState, Animation, AnimationSignature, DEFAULT_ANIMATION_LAG_RATIO,
    DEFAULT_ANIMATION_RUN_TIME, IntoAnimation, MethodAnimation, RateFunc, prepare_animation,
    sub_alpha, time_spanned_alpha,
};
pub use clock::{ClockError, FrameSample, FrameSegment, RationalFrameClock, RationalTime};
pub use frame::{FramePacket, play_segment, wait_segment};
pub use purity::{
    ImpureEffect, Purity, SegmentKind, SegmentReport, classify_play, classify_wait,
    reconstruct_pure_frame,
};
pub use transform::{
    PathFunc, STRAIGHT_PATH_THRESHOLD, Transform, apply_function, cyclic_replace,
    interpolate_fields, move_to_target, replacement_transform, restore, scale_in_place,
    shrink_to_center, swap, transform_from_copy,
};
