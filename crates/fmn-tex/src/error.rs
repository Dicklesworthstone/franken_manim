//! Scribe II's error surface: fmd-math's precise construct errors pass
//! through untouched (the user sees `` `\substack` is not yet supported;
//! tier T2, tracked at … `` at construction time — never a blank render),
//! and everything else is equally named.

use core::fmt;

/// A Tex/TexText failure.
#[derive(Debug)]
pub enum TexError {
    /// fmd-math refused the string: unsupported construct (named,
    /// tier-tagged, tracked), malformed input (byte-positioned), or an
    /// unmapped character. The Display text is the ratchet's stable
    /// format, surfaced verbatim.
    Math(fmd_math::MathError),
    /// The `tex.template` config value refused to resolve (an out-of-tier
    /// or unknown Reference template — the registry's named refusal).
    Pack(fmn_config::PackError),
    /// A pack content id from the registry names no fmd-math pack —
    /// registry/pack drift, which is a wiring bug worth its own name.
    UnknownPack {
        /// The content id that failed.
        content_id: &'static str,
    },
    /// The bundled faces failed to load (build corruption).
    Faces {
        /// The underlying failure.
        what: String,
    },
    /// Cache wiring failed (opening the namespace). Typesetting itself
    /// never fails on cache trouble — reads/writes degrade to recompute.
    Cache {
        /// The underlying failure.
        what: String,
    },
}

impl fmt::Display for TexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Math(e) => e.fmt(f),
            Self::Pack(e) => e.fmt(f),
            Self::UnknownPack { content_id } => write!(
                f,
                "pack content id {content_id:?} names no fmd-math pack (registry/pack drift)"
            ),
            Self::Faces { what } => write!(f, "bundled faces failed to load: {what}"),
            Self::Cache { what } => write!(f, "typeset cache unavailable: {what}"),
        }
    }
}

impl std::error::Error for TexError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Math(e) => Some(e),
            Self::Pack(e) => Some(e),
            _ => None,
        }
    }
}

impl From<fmd_math::MathError> for TexError {
    fn from(e: fmd_math::MathError) -> Self {
        Self::Math(e)
    }
}
