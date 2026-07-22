//! Marionette: the mobject engine — Stage arena, RecordBuffer, family tree, styles, updaters (§8).
//!
//! This crate implements the ownership model **as ratified by G0-1**
//! (D-11, `docs/g0/G0-1-object-model-ratification.md`): the [`Stage`]
//! arena with generational stage-scoped [`Mob`] handles, rooted lifetimes,
//! CoW [`Snapshot`]s, and the §8.2 record/view protocol core. The ten G0-1
//! lifetime scenarios are this crate's permanent regression suite
//! (`tests/scenarios.rs`).
//!
//! The family/positional API, bounding boxes, and the uniform inventory land
//! here (fm-jru): [`BoundingBox`] with automatic subtree-signature
//! invalidation, the positional surface on [`Stage`] (`shift`/`next_to`/
//! `move_to`/`align_to`/`arrange`/…), and the typed [`Uniforms`] inventory
//! (including the C-2/BN-07 and C-7 rulings).
//!
//! The dynamic-behavior surface (§8.6, fm-yra): insertion-ordered dt/non-dt
//! updaters with suspend/resume and the C-5 once-only `call` fix
//! ([`stage`]), ValueTrackers and `always_redraw`/`f_always` plus the C-6
//! group-addition correction ([`dynamics`]), and the `.animate` builder
//! with the Reference's real chaining rules ([`animate`]).
//!
//! Still to land here: copy semantics beyond the arena core (fm-ncq).
#![forbid(unsafe_code)]

pub mod animate;
pub mod bbox;
pub mod dynamics;
pub mod mobject;
pub mod positional;
pub mod record;
pub mod stage;
pub mod uniforms;

pub use animate::{AnimBuilder, AnimateArgs, AnimateError, BuiltAnimate, IntoAnimate};
pub use bbox::BoundingBox;
pub use dynamics::{Tracker, TrackerKind};
pub use mobject::Mobject;
pub use positional::PosTarget;
pub use record::{FieldSpec, MirrorSet, RecordBuffer, RecordSchema, RecordView};
pub use stage::{Entry, Mob, Snapshot, Stage, UpdaterFn, UpdaterId, UpdaterSlot};
pub use uniforms::{JointType, Uniforms};

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
