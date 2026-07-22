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
pub mod creation;
pub mod fading;
pub mod frame;
pub mod growing;
pub mod indication;
pub mod movement;
pub mod purity;
pub mod rotation;
pub mod transform;
pub mod transform_matching;
pub mod update;

pub use animation::{
    AnimConfig, AnimError, AnimState, Animation, AnimationSignature, DEFAULT_ANIMATION_LAG_RATIO,
    DEFAULT_ANIMATION_RUN_TIME, IntoAnimation, MethodAnimation, RateFunc, prepare_animation,
    sub_alpha, time_spanned_alpha,
};
pub use clock::{ClockError, FrameSample, FrameSegment, RationalFrameClock, RationalTime};
pub use creation::{
    DrawBorderThenFill, IntRound, RevealBounds, ShowIncreasingSubsets, ShowPartial, show_creation,
    show_increasing_subsets, show_passing_flash, show_submobjects_one_by_one, uncreate, write,
};
pub use fading::{
    FadeTransform, VFade, fade_in, fade_in_from_point, fade_out, fade_out_to_point, fade_transform,
    fade_transform_pieces, v_fade_in, v_fade_in_then_out, v_fade_out,
};
pub use frame::{FramePacket, play_segment, wait_segment};
pub use growing::{grow_arrow, grow_from_center, grow_from_edge, grow_from_point};
pub use indication::{
    INDICATION_YELLOW, VShowPassingFlash, WiggleOutThenIn, apply_wave, indicate,
    show_creation_then_destruction, turn_inside_out,
};
pub use movement::{Homotopy, MoveAlongPath, PhaseFlow, complex_homotopy, smoothed_homotopy};
pub use purity::{
    ImpureEffect, Purity, SegmentKind, SegmentReport, classify_play, classify_wait,
    reconstruct_pure_frame,
};
pub use rotation::{Rotating, rotate, rotate_default};
pub use transform::{
    PathFunc, STRAIGHT_PATH_THRESHOLD, StartPrep, Transform, apply_complex_function,
    apply_function, apply_matrix, apply_matrix_2d, apply_pointwise_function,
    apply_pointwise_function_to_center, cyclic_replace, fade_to_color, interpolate_fields,
    move_to_target, replacement_transform, restore, scale_in_place, shrink_to_center, swap,
    transform_from_copy,
};
pub use transform_matching::{has_same_shape_as, transform_matching_parts};
pub use update::{MaintainPositionRelativeTo, UpdateFromFunc};
