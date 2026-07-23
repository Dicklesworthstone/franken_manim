//! Proscenium — the Scene runtime: state machine on the rational
//! clock, events, InteractiveScene (§13.1–13.4).
//!
//! Landed so far (fm-y7u): the **replay journal + effect model**
//! ([`journal`]) — the one record with three consumers: the
//! supervisor's edit-replay, the purity classifier's journaled
//! evidence, and the pipeline's barrier vocabulary — plus the §18
//! repro bundle. The scene state machine itself lands with fm-5xm.
#![forbid(unsafe_code)]

pub mod journal;

pub use journal::{
    AssetRead, BundleDivergence, CommandKind, CommandRecord, EffectClass, Entry, ImpureEffectTag,
    InvalidationReason, Journal, JournalError, ReplayAudit, ReplayPlan, ReproBundle,
    SubprocessRecord, plan_replay,
};
