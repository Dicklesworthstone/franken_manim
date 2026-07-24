//! The content-addressed store behind typeset, path, and render caches (§14.4).
//!
//! One persistent store, one discipline, three consumers: **typeset results**
//! (fmd-math + text layout, keyed on the string, the preamble-pack hash, the
//! font hashes, and the engine version — the dominant win, PG-7's <100 µs
//! cached path), **fetched assets** (through the `AssetFetcher` capability,
//! addressed by content hash), and **the replay journal's storage** (§13.4 —
//! segments and checkpoint snapshots under a manually-managed namespace).
//!
//! # The discipline
//!
//! - **Content-addressed on fmn-hash.** An entry's address is a SHA-256
//!   [`Digest`](fmn_hash::Digest): either the digest of canonical key material
//!   (built with [`KeyBuilder`], serialized by fmn-hash's canonical format) or
//!   the digest of the content itself ([`Namespace::put_blob`]). Filesystem
//!   paths derive **only** from validated namespace names and digest hex —
//!   arbitrary key bytes never touch a path, which is the traversal
//!   protection: there is no key that can name a path outside the store root.
//! - **Atomic writes.** Every entry and every index lands via the capability's
//!   `write_atomic` (write-temp + rename): a reader sees the old bytes, the
//!   new bytes, or absence — never a torn intermediate, even under `kill -9`.
//! - **Checksums verified on read.** Entries ride fmn-hash's serial envelope,
//!   whose trailing SHA-256 covers the whole document; the envelope also
//!   records the address it was stored under, and blob entries additionally
//!   self-certify (payload digest = address). Any mismatch — flipped bytes,
//!   truncation, a valid envelope at the wrong address — classifies the entry
//!   as corrupt: it is **evicted and reported as a miss, never trusted, never
//!   fatal**.
//! - **Versioned namespaces.** A namespace is `(name, schema_version)`; its
//!   directory is `ns/<name>/v<version>`. Bumping the version is a clean
//!   invalidation — a cold directory — without touching unrelated namespaces;
//!   [`Namespace::purge_stale_versions`] reclaims the abandoned ones.
//! - **Cross-process safety.** Entry writes are atomic and content-addressed
//!   (two writers racing on one address write identical bytes), so ordinary
//!   put/get needs no lock at all. Maintenance (eviction) takes an advisory
//!   lock file with wall-clock staleness breaking; the LRU index is an
//!   advisory hint that eviction reconciles against the disk truth, so a lost
//!   index is a rebuild, never corruption.
//! - **Defined eviction.** LRU-class by logical access sequence with
//!   **pinning** for in-use entries ([`Namespace::pin`]) and a config-visible
//!   size ceiling per namespace ([`NamespacePolicy`]). A `None` ceiling is the
//!   manual policy (the journal namespace: explicit lifecycle, no automatic
//!   eviction).
//!
//! # Determinism
//!
//! A cache is an **optimization, never an oracle**: every key includes the
//! complete semantic inputs (that is the consumer's contract, enforced by
//! construction in [`KeyBuilder`]'s canonical serialization), so a hit is
//! definitionally equivalent to a recompute and certified renders are
//! bit-identical with a cold or warm cache. Every cache failure degrades to a
//! recompute ([`Namespace::get_or_compute`] swallows storage trouble); nothing
//! in this crate can fail a render. `--clear-cache` ([`Store::clear`]) is safe
//! at any moment: concurrent readers see misses, concurrent writers recreate
//! what they need.
//!
//! LRU bookkeeping uses a logical sequence counter — never wall time — so
//! eviction order is reproducible in the deterministic lab; the only clock
//! use is advisory-lock staleness, which is maintenance, not semantics.
#![forbid(unsafe_code)]

mod entry;
mod key;
mod store;

pub use key::{CacheKey, KeyBuilder};
pub use store::{EvictOutcome, EvictReport, Namespace, NamespacePolicy, Pin, Store, StoreConfig};

use fmn_platform::fs::FsError;
use std::fmt;

/// A cache failure. Per the never-fatal doctrine, consumers treat every one of
/// these as "skip the cache" — [`Namespace::get_or_compute`] does so
/// structurally — but each is precise for diagnostics.
#[derive(Debug)]
pub enum CacheError {
    /// A namespace name failed validation (the traversal-protection boundary).
    InvalidNamespace {
        /// The offending name.
        name: String,
        /// Why it was rejected.
        reason: &'static str,
    },
    /// The store root carries a format stamp this build does not support; the
    /// remedy is `--clear-cache`.
    FormatUnsupported {
        /// The stamp found on disk.
        found: String,
    },
    /// An entry payload exceeds the configured per-entry ceiling; the caller
    /// skips caching this value.
    EntryTooLarge {
        /// The configured cap ([`StoreConfig::max_entry_bytes`]).
        limit: usize,
        /// The payload size that was offered.
        needed: usize,
    },
    /// The filesystem capability failed.
    Storage(FsError),
    /// Canonical serialization failed (an over-limit field in key material or
    /// an entry envelope).
    Encode(fmn_hash::SerialError),
}

impl fmt::Display for CacheError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidNamespace { name, reason } => {
                write!(f, "invalid cache namespace {name:?}: {reason}")
            }
            Self::FormatUnsupported { found } => write!(
                f,
                "unsupported cache store format {found:?}; clear the cache to migrate"
            ),
            Self::EntryTooLarge { limit, needed } => {
                write!(
                    f,
                    "cache entry too large: {needed} bytes over the {limit}-byte ceiling"
                )
            }
            Self::Storage(err) => write!(f, "cache storage failure: {err}"),
            Self::Encode(err) => write!(f, "cache serialization failure: {err}"),
        }
    }
}

impl std::error::Error for CacheError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Storage(err) => Some(err),
            Self::Encode(err) => Some(err),
            _ => None,
        }
    }
}

impl From<FsError> for CacheError {
    fn from(err: FsError) -> Self {
        Self::Storage(err)
    }
}

impl From<fmn_hash::SerialError> for CacheError {
    fn from(err: fmn_hash::SerialError) -> Self {
        Self::Encode(err)
    }
}
