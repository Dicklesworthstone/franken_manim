//! Marionette: the mobject engine — Stage arena, RecordBuffer, family tree, styles, updaters (§8).
//!
//! This crate implements the ownership model **as ratified by G0-1**
//! (D-11, `docs/g0/G0-1-object-model-ratification.md`): the [`Stage`]
//! arena with generational stage-scoped [`Mob`] handles, rooted lifetimes,
//! CoW [`Snapshot`]s, and the §8.2 record/view protocol core. The ten G0-1
//! lifetime scenarios are this crate's permanent regression suite
//! (`tests/scenarios.rs`).
//!
//! Still to land here: the full RecordBuffer surface + lazy revisioned
//! render mirrors (fm-cus), family/positional API + bounding boxes +
//! uniforms (fm-jru), copy semantics beyond the arena core, and updaters/
//! ValueTrackers/`.animate` (fm-yra).
#![forbid(unsafe_code)]

pub mod mobject;
pub mod record;
pub mod stage;

pub use mobject::Mobject;
pub use record::{FieldSpec, MirrorSet, RecordBuffer, RecordSchema, RecordView};
pub use stage::{Entry, Mob, Snapshot, Stage, Updater};

/// Errors from the mobject engine's ownership layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageError {
    /// The handle is stale (its entry was deleted), foreign (minted by
    /// another stage — the two-scene policy), or otherwise unresolvable.
    /// Always a defined state: a dead handle can never reach a recycled
    /// slot's data.
    StaleHandle,
    /// Attaching this edge would make the family graph cyclic (the
    /// Reference would recurse forever on such a graph; we refuse it).
    CycleDetected,
}

impl std::fmt::Display for StageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StaleHandle => {
                write!(f, "stale, deleted, or foreign mobject handle")
            }
            Self::CycleDetected => {
                write!(f, "attachment would create a cycle in the family graph")
            }
        }
    }
}

impl std::error::Error for StageError {}
