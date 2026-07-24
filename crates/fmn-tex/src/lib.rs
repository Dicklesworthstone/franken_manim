//! Scribe II: Tex/TexText typesetting, span-map consumption, typeset
//! caching, and the pre-play preflight over fmd-math (§11.4–11.5).
//!
//! The layering, per §19: this crate turns strings into **typeset data**
//! — [`Typeset`]: fmd-math's placed layout, the submobject table, and the
//! span-identity selection surface — behind the [`TexEngine`], which owns
//! the preamble pack (resolved from `tex.template` through fmn-config's
//! registry), the content-addressed cache (fmn-cache, keyed on the
//! complete semantic inputs including a self-maintaining engine
//! fingerprint), and the parallel [`TexEngine::preflight`]. The geometry
//! and Stage wiring — [`Typeset`] → `QuadPath`s → the VMobject family —
//! belong to the library tier (Menagerie), which sits above both this
//! crate and fmn-geom.
//!
//! Three contracts to know:
//!
//! - **Spans are the compatibility surface.** `isolate=`,
//!   `tex_to_color_map`, substring slicing, and `TransformMatchingTex`
//!   consume [`Typeset::occurrences`] — source-identity matching through
//!   the native span map (§11.3). The Reference's render-twice-and-align
//!   hack is dead; ordinal parity with its SVG ordering is deliberately
//!   not promised (spans are).
//! - **The cache is an optimization, never an oracle.** A hit is
//!   bit-identical to a recompute (the codec round-trips exactly, tested),
//!   the key contains everything semantic (fingerprint + macros + mode +
//!   source), and every cache failure degrades to computing.
//! - **Errors are named at construction time.** An unsupported construct
//!   surfaces fmd-math's precise, tier-tagged, tracked error text the
//!   moment the string is typeset — never silence, never a blank render.
//!
//! The TexText text-mode tier-1 surface is documented in ADR-0006 (the
//! OQ-4 resolution): a text mainland with `$…$`/`$$…$$` math islands,
//! `\textbf`/`\emph`/`\underline`, the escape set, and the Reference-era
//! missing-`$` recovery.
#![forbid(unsafe_code)]

mod engine;
mod error;
mod typeset;

pub use engine::{Mode, TexEngine};
pub use error::TexError;
pub use typeset::{Prim, Sub, TYPESET_FORMAT_VERSION, Typeset};

// The math surface consumers need alongside the engine.
pub use fmd_math::{MacroSet, Span, Style};
