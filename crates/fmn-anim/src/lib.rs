//! Choreo: the animation engine — Animation trait, RationalFrameClock, timeline algebra (§9).
//!
//! Landed so far: the [`RationalFrameClock`] (§9.2, D-07, BN-02) — exact
//! rational time over the frame grid, with the Reference's emission
//! semantics and no off-grid API. Still to land: the Animation contract
//! (fm-67a), the six-step frame order + FramePacket (fm-x79), composition
//! and timeline algebra (fm-hfe), segment purity classification.
#![forbid(unsafe_code)]

pub mod clock;

pub use clock::{ClockError, FrameSample, FrameSegment, RationalFrameClock, RationalTime};
