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
//! Copy semantics (§8.3, fm-ncq) are complete: the [`stage::CopyMap`]
//! remap hook for the binding tier, the `generate_target` / `save_state` /
//! `restore_mobject` machinery with the Reference's exact link topology,
//! and `become` (over already-aligned families; `align_family` itself
//! lands with the Transform machinery, fm-cye).
#![forbid(unsafe_code)]

pub mod align;
pub mod animate;
pub mod bbox;
pub mod dynamics;
pub mod mobject;
pub mod persist;
pub mod positional;
pub mod record;
pub mod shape;
pub mod stage;
pub mod uniforms;

pub use animate::{AnimBuilder, AnimateArgs, AnimateError, BuiltAnimate, IntoAnimate};
pub use bbox::BoundingBox;
pub use dynamics::{Tracker, TrackerKind};
pub use mobject::Mobject;
pub use persist::{
    DecodedSceneState, DecodedSnapshot, PersistError, SCENE_STATE_SCHEMA, SNAPSHOT_SCHEMA,
    SceneState, UpdaterKindTag, UpdaterManifest,
};
pub use positional::PosTarget;
pub use record::{FieldSpec, MirrorSet, RecordBuffer, RecordSchema, RecordView};
pub use shape::ShapeTag;
pub use stage::{CopyMap, Entry, Mob, Snapshot, Stage, UpdaterFn, UpdaterId, UpdaterSlot};
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
    /// `restore` without a prior `save_state` — the Reference raises
    /// "Trying to restore without having saved".
    NoSavedState,
    /// `become` between families of different shapes. The Reference aligns
    /// families first (`align_family`); alignment lands with the Transform
    /// machinery (fm-cye), and until then the shapes must already agree.
    FamilyShapeMismatch,
    /// `become` between records of different schemas — the Reference's
    /// `set_data` asserts dtype equality; this is the typed refusal.
    SchemaMismatch,
    /// A point field whose contents violate the shared-anchor layout
    /// reached the geometry kernel (alignment reads point runs as
    /// [`fmn_geom::QuadPath`]s).
    Geometry(fmn_geom::GeomError),
    /// `put_start_and_end_on` on a family with no points, or whose first
    /// and last points coincide — the Reference's "Cannot position
    /// endpoints of closed loop". There is no rotation that separates two
    /// identical points, so the caller has to decide (`Line` rebuilds its
    /// path from the new ends instead).
    DegenerateEndpoints,
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
            Self::NoSavedState => {
                write!(f, "trying to restore without having saved")
            }
            Self::FamilyShapeMismatch => {
                write!(
                    f,
                    "become between families of different shapes \
                     (family alignment lands with fm-cye)"
                )
            }
            Self::SchemaMismatch => {
                write!(f, "become between records of different schemas")
            }
            Self::Geometry(err) => {
                write!(f, "malformed point run in alignment: {err}")
            }
            Self::DegenerateEndpoints => {
                write!(f, "cannot position endpoints of a closed or empty path")
            }
        }
    }
}

impl std::error::Error for StageError {}
