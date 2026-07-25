//! # G0-8 — the accelerator proof spike (fm-ekx)
//!
//! Plan §20.1 spike 8: express the compiled render IR's highest-ROI stage —
//! **stroke signed-distance evaluation + AA resolve** — as frankentorch Metal
//! kernels running end-to-end into a Studio-preview frame on Apple silicon,
//! *before* W5 (fm-gw7) freezes the IR and before CPU-specific assumptions
//! harden. The written IR→GPU mapping report is the deliverable fm-gw7
//! consumes; `docs/g0/G0-8-accelerator-ratification.md` is that report.
//!
//! ## Shape
//!
//! | module | what it is |
//! |---|---|
//! | [`ir`] | the prototype compiled render IR (§10.8) and its Metal-shaped flattening |
//! | [`sdf`] | the stage's mathematics in `f64`, mirrored by `shaders/stroke_aa.metal` in `f32` |
//! | [`cpu`] | the CPU reference engine — what the stage *means* |
//! | [`annex`] | the Metal engine, dispatched through frankentorch's compute gateway |
//! | [`compare`] | the §16.3 engine-equivalence measurement |
//! | [`scene`] | the preview frame, built to exercise every property the stage owns |
//!
//! ## Constraints that survive regardless of outcome
//!
//! - The annex is **standard-mode only, never `certified`** — D-18's permanent
//!   refusal of GPU work in the certified path.
//! - It is **excluded from the core gates** and measured under **PG-A** alone,
//!   so acceleration can never mask a core CPU regression (§17.2).
//! - **frankentorch is the only GPU gateway** (D-22, §10.7). No wgpu, ever.
//! - Backend identity is **journaled into the input closure** (§10.5f, §16.7);
//!   [`annex::AnnexReport`] carries the fields that record does.
//!
//! ## What this crate is not
//!
//! It is not a renderer, and it is not W5's IR. It is one stage, at full
//! strength, over a faithful subset of the IR, run on both engines and
//! measured — the smallest artifact that can actually answer "does the IR map
//! onto a GPU?" with evidence instead of confidence. Per ADR-0003's
//! non-shipped tier it is not a workspace member and nothing links against it.

#![forbid(unsafe_code)]

pub mod annex;
pub mod compare;
pub mod cpu;
pub mod ir;
pub mod scene;
pub mod sdf;
