//! The store: versioned namespaces of content-addressed entries over the
//! filesystem capability, with advisory maintenance locking, an LRU-class
//! index, pinning, and defined eviction.
//!
//! # On-disk shape
//!
//! ```text
//! <root>/
//!   STORE_OWNER                 path-bound ownership marker
//!   STORE_FORMAT                  the store-format stamp ("fmn-cache 1")
//!   ns/<name>/v<version>/         one versioned namespace
//!     objects/<hh>/<hex…>         entries, sharded by the first digest byte
//!     index                       the advisory LRU index (rebuildable)
//!     lock                        the advisory maintenance lock (transient)
//! ```
//!
//! Every path component below `<root>` is either a fixed literal, a validated
//! namespace name (`[a-z0-9][a-z0-9_-]*`, at most 64 bytes), a `v<u32>`
//! version directory, or digest hex — arbitrary key bytes never reach a path,
//! so no key can escape the root (the traversal-protection contract, fuzzed
//! in the crate's tests).
//!
//! # Concurrency model
//!
//! Entry writes are atomic and content-addressed: two processes racing on the
//! same address write byte-identical files through rename, so put/get take no
//! lock. The LRU index is advisory (last-writer-wins, merged on flush,
//! reconciled against disk truth by eviction; a lost or stale index degrades
//! LRU accuracy, never correctness). Only maintenance — eviction — takes the
//! per-namespace lock file, and a crashed holder is broken by wall-clock
//! staleness; a maintainer that cannot get the lock skips, it never blocks.

use crate::CacheError;
use crate::entry::{self, EntryKind};
use crate::key::CacheKey;
use fmn_hash::{Digest, Limits, Reader, Schema, UnknownPolicy, Writer, sha256};
use fmn_platform::clock::Clock;
use fmn_platform::fs::{FileSystem, FsError};
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::env;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

/// The exact store-format stamp this build reads and writes. A different
/// stamp on disk is [`CacheError::FormatUnsupported`]; the remedy is
/// `--clear-cache`.
const FORMAT_STAMP: &str = "fmn-cache 1";
/// The stamp's file name under the store root.
const FORMAT_FILE: &str = "STORE_FORMAT";
/// Stable ownership-marker prefix, independent of the replaceable store
/// format. The suffix binds the marker to the canonical absolute host path.
const OWNER_PREFIX: &str = "franken-manim cache root 1 ";
/// The ownership marker's file name under the store root.
const OWNER_FILE: &str = "STORE_OWNER";

/// The advisory LRU index document.
const INDEX_SCHEMA: Schema = Schema::new(*b"FMNC", 2, 1, 0);
/// The advisory maintenance-lock token document.
const LOCK_SCHEMA: Schema = Schema::new(*b"FMNC", 4, 1, 0);

/// Process-wide store-instance counter, distinguishing lock tokens from two
/// stores (or two openings) in one process.
static NEXT_INSTANCE: AtomicU64 = AtomicU64::new(1);
/// Process-local uniquifier for clear-quarantine directory names.
static NEXT_CLEAR: AtomicU64 = AtomicU64::new(1);
/// Bound collision work so a hostile directory full of guessed names cannot
/// make a lifecycle command spin forever.
const MAX_QUARANTINE_ATTEMPTS: usize = 4_096;

fn lock_poisoned<T>(err: PoisonError<T>) -> T {
    err.into_inner()
}

fn root_refused(root: &Path, reason: impl Into<String>) -> CacheError {
    CacheError::RootRefused {
        root: root.to_path_buf(),
        reason: reason.into(),
    }
}

fn owner_stamp(root: &Path) -> Vec<u8> {
    let digest = sha256(root.as_os_str().as_encoded_bytes());
    format!("{OWNER_PREFIX}{}\n", digest.to_hex()).into_bytes()
}

fn verify_stamp(
    fs: &dyn FileSystem,
    path: &Path,
    expected: &[u8],
    root: &Path,
    label: &str,
) -> Result<(), CacheError> {
    let found = fs.read(path)?;
    if found == expected {
        Ok(())
    } else {
        Err(root_refused(
            root,
            format!("{label} is foreign or bound to a different path"),
        ))
    }
}

#[derive(Clone, Copy, Debug)]
enum RootOpenState {
    /// The root existed before this opener inspected it.
    Existing,
    /// This opener atomically created the exact host leaf.
    CreatedHostLeaf,
    /// A non-host capability may establish an isolated missing root.
    CapabilityManaged,
}

/// Establish ownership only when this opener proved it created the exact host
/// leaf, or when a non-host capability reports a missing isolated root.
fn ensure_owned_root(
    fs: &dyn FileSystem,
    root: &Path,
    state: RootOpenState,
) -> Result<(), CacheError> {
    if !root.is_absolute() {
        return Err(root_refused(root, "the store root must be absolute"));
    }
    let owner_path = root.join(OWNER_FILE);
    let expected_owner = owner_stamp(root);
    match fs.read(&owner_path) {
        Ok(found) => {
            if found != expected_owner {
                return Err(root_refused(
                    root,
                    "ownership marker is foreign or bound to a different path",
                ));
            }
        }
        Err(FsError::NotFound { .. }) => {
            match state {
                RootOpenState::Existing => {
                    return Err(root_refused(
                        root,
                        "an existing directory has no ownership marker",
                    ));
                }
                RootOpenState::CreatedHostLeaf => {}
                RootOpenState::CapabilityManaged => match fs.list_dir(root) {
                    Ok(_) => {
                        return Err(root_refused(
                            root,
                            "an existing directory has no ownership marker",
                        ));
                    }
                    Err(FsError::NotFound { .. }) => {}
                    Err(err) => return Err(err.into()),
                },
            }
            if !fs.create_new(&owner_path, &expected_owner)? {
                // Another claimant won. Its complete published bytes, not our
                // intent, decide whether this is the same owned root.
                verify_stamp(fs, &owner_path, &expected_owner, root, "ownership marker")?;
            }
        }
        Err(err) => return Err(err.into()),
    }

    let format_path = root.join(FORMAT_FILE);
    let expected_format = format!("{FORMAT_STAMP}\n");
    match fs.read_to_string(&format_path) {
        Ok(found) if found.trim_end() == FORMAT_STAMP => Ok(()),
        Ok(found) => Err(CacheError::FormatUnsupported {
            found: found.trim_end().to_owned(),
        }),
        Err(FsError::NotFound { .. }) => {
            if !fs.create_new(&format_path, expected_format.as_bytes())? {
                verify_stamp(
                    fs,
                    &format_path,
                    expected_format.as_bytes(),
                    root,
                    "format stamp",
                )?;
            }
            Ok(())
        }
        Err(err) => Err(err.into()),
    }
}

fn validate_existing_host_store_root(root: &Path, cwd: &Path) -> Result<PathBuf, CacheError> {
    reject_symlink_components(root)?;
    let canonical_root = fs::canonicalize(root).map_err(|err| host_io(root, err))?;
    let metadata =
        fs::symlink_metadata(&canonical_root).map_err(|err| host_io(&canonical_root, err))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(root_refused(root, "root is not a real directory"));
    }
    reject_protected_host_root(&canonical_root, root, cwd)?;
    Ok(canonical_root)
}

fn prepare_host_store_root(root: &Path) -> Result<(PathBuf, RootOpenState), CacheError> {
    if !root.is_absolute() {
        return Err(root_refused(root, "the store root must be absolute"));
    }
    if root
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(root_refused(
            root,
            "parent-directory components are not accepted in a store root",
        ));
    }
    let cwd = env::current_dir().map_err(|err| host_io(Path::new("."), err))?;
    match fs::symlink_metadata(root) {
        Ok(_) => validate_existing_host_store_root(root, &cwd)
            .map(|root| (root, RootOpenState::Existing)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            let parent = root.parent().ok_or_else(|| {
                root_refused(root, "a store root must have an existing parent directory")
            })?;
            match fs::symlink_metadata(parent) {
                Ok(_) => {}
                Err(err) if err.kind() == io::ErrorKind::NotFound => {
                    return Err(root_refused(
                        root,
                        "a store root's immediate parent must already exist",
                    ));
                }
                Err(err) => return Err(host_io(parent, err)),
            }
            reject_symlink_components(parent)?;
            let canonical_parent = match fs::canonicalize(parent) {
                Ok(parent) => parent,
                Err(err) if err.kind() == io::ErrorKind::NotFound => {
                    return Err(root_refused(
                        root,
                        "a store root's immediate parent must already exist",
                    ));
                }
                Err(err) => return Err(host_io(parent, err)),
            };
            let metadata = fs::symlink_metadata(&canonical_parent)
                .map_err(|err| host_io(&canonical_parent, err))?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(root_refused(
                    root,
                    "a store root's parent is not a real directory",
                ));
            }
            let leaf = root.file_name().ok_or_else(|| {
                root_refused(root, "a store root must name a dedicated leaf directory")
            })?;
            let canonical_root = canonical_parent.join(leaf);
            reject_protected_host_root(&canonical_root, root, &cwd)?;
            match fs::create_dir(&canonical_root) {
                Ok(()) => Ok((canonical_root, RootOpenState::CreatedHostLeaf)),
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                    validate_existing_host_store_root(root, &cwd)
                        .map(|root| (root, RootOpenState::Existing))
                }
                Err(err) => Err(host_io(&canonical_root, err)),
            }
        }
        Err(err) => Err(host_io(root, err)),
    }
}

/// Result of a one-shot cache clear.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheClearOutcome {
    /// A managed namespace tree was quarantined and removed.
    Cleared,
    /// The configured root or its managed namespace tree was already absent.
    AlreadyAbsent,
}

#[derive(Debug)]
enum ClearState {
    Absent,
    Owned {
        canonical_root: PathBuf,
        owner: Vec<u8>,
    },
}

/// A non-creating, single-use authorization for `--clear-cache`.
///
/// Authorization is intentionally host-only: it binds the configured path to
/// its canonical identity, rejects symlinked components and protected roots,
/// and verifies the path-bound ownership marker plus a format stamp. Clearing
/// retains the root and both stamps; only the managed `ns` subtree is atomically
/// quarantined and recursively removed.
#[derive(Debug)]
pub struct CacheClearAuthorization {
    configured_root: PathBuf,
    state: ClearState,
}

impl CacheClearAuthorization {
    /// Inspect `root` without creating or stamping it.
    ///
    /// # Errors
    ///
    /// [`CacheError::RootRefused`] for dangerous, symlinked, foreign, or
    /// unstamped roots, and [`CacheError::Storage`] for host I/O failures.
    pub fn authorize(root: impl AsRef<Path>) -> Result<Self, CacheError> {
        let cwd = env::current_dir().map_err(|err| host_io(Path::new("."), err))?;
        let configured_root = absolute_without_parent_components(root.as_ref(), &cwd)?;
        let state = match fs::symlink_metadata(&configured_root) {
            Ok(_) => {
                let (canonical_root, owner) = validate_host_owned_root(&configured_root, &cwd)?;
                ClearState::Owned {
                    canonical_root,
                    owner,
                }
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => ClearState::Absent,
            Err(err) => return Err(host_io(&configured_root, err)),
        };
        Ok(Self {
            configured_root,
            state,
        })
    }

    /// The absolute configured root this authorization names.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.configured_root
    }

    /// Consume this authorization and clear only the managed namespace tree.
    ///
    /// The root is revalidated immediately before the linearization point.
    /// `ns` is then renamed to a unique sibling inside the retained root; all
    /// stale Store handles still address the original `ns` path and can only
    /// recreate fresh cache content, never write into the quarantined tree.
    pub fn clear(self) -> Result<CacheClearOutcome, CacheError> {
        let ClearState::Owned {
            canonical_root,
            owner,
        } = self.state
        else {
            return Ok(CacheClearOutcome::AlreadyAbsent);
        };
        let cwd = env::current_dir().map_err(|err| host_io(Path::new("."), err))?;
        let (current_root, current_owner) = validate_host_owned_root(&self.configured_root, &cwd)?;
        if current_root != canonical_root || current_owner != owner {
            return Err(root_refused(
                &self.configured_root,
                "root identity changed after clear authorization",
            ));
        }

        let namespaces = canonical_root.join("ns");
        let mut quarantine = None;
        for _ in 0..MAX_QUARANTINE_ATTEMPTS {
            let candidate = canonical_root.join(format!(
                ".fmn-clear.{}.{}",
                std::process::id(),
                NEXT_CLEAR.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&candidate) {
                Ok(()) => {
                    quarantine = Some(candidate);
                    break;
                }
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
                Err(err) => return Err(host_io(&candidate, err)),
            }
        }
        let quarantine = quarantine.ok_or_else(|| {
            root_refused(
                &self.configured_root,
                format!(
                    "could not reserve a quarantine directory after \
                     {MAX_QUARANTINE_ATTEMPTS} collision attempts"
                ),
            )
        })?;
        let (final_root, final_owner) = validate_host_owned_root(&self.configured_root, &cwd)?;
        if final_root != canonical_root || final_owner != owner {
            return Err(root_refused(
                &self.configured_root,
                "root identity changed while reserving the clear quarantine",
            ));
        }
        let quarantined_namespaces = quarantine.join("ns");
        match fs::rename(&namespaces, &quarantined_namespaces) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                fs::remove_dir(&quarantine).map_err(|cleanup_err| {
                    host_io(
                        &quarantine,
                        io::Error::new(
                            cleanup_err.kind(),
                            format!(
                                "cache namespace was absent but its empty quarantine could not be removed: {cleanup_err}"
                            ),
                        ),
                    )
                })?;
                refresh_host_format_stamp(&canonical_root)?;
                return Ok(CacheClearOutcome::AlreadyAbsent);
            }
            Err(err) => {
                let _ = fs::remove_dir(&quarantine);
                return Err(host_io(&namespaces, err));
            }
        }

        fs::remove_dir_all(&quarantine).map_err(|err| host_io(&quarantine, err))?;
        refresh_host_format_stamp(&canonical_root)?;
        Ok(CacheClearOutcome::Cleared)
    }
}

fn refresh_host_format_stamp(root: &Path) -> Result<(), CacheError> {
    fmn_platform::fs::StdFs
        .write_atomic(
            &root.join(FORMAT_FILE),
            format!("{FORMAT_STAMP}\n").as_bytes(),
        )
        .map_err(CacheError::Storage)
}

fn absolute_without_parent_components(path: &Path, cwd: &Path) -> Result<PathBuf, CacheError> {
    if !path.is_absolute() {
        return Err(root_refused(
            path,
            format!(
                "destructive cache targets must be absolute (current directory: {})",
                cwd.display()
            ),
        ));
    }
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(root_refused(
            path,
            "parent-directory components are not accepted for destructive targets",
        ));
    }
    Ok(path.to_path_buf())
}

fn validate_host_owned_root(
    configured_root: &Path,
    cwd: &Path,
) -> Result<(PathBuf, Vec<u8>), CacheError> {
    reject_symlink_components(configured_root)?;
    let canonical_root =
        fs::canonicalize(configured_root).map_err(|err| host_io(configured_root, err))?;
    let metadata =
        fs::symlink_metadata(&canonical_root).map_err(|err| host_io(&canonical_root, err))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(root_refused(
            configured_root,
            "root is not a real directory",
        ));
    }
    reject_protected_host_root(&canonical_root, configured_root, cwd)?;
    let owner_path = canonical_root.join(OWNER_FILE);
    reject_symlink_leaf(&owner_path, configured_root, "ownership marker")?;
    let owner = fs::read(&owner_path).map_err(|err| host_io(&owner_path, err))?;
    let expected = owner_stamp(&canonical_root);
    if owner != expected {
        return Err(root_refused(
            configured_root,
            "ownership marker is absent, foreign, or bound to another path",
        ));
    }
    let format_path = canonical_root.join(FORMAT_FILE);
    reject_symlink_leaf(&format_path, configured_root, "format stamp")?;
    let format = fs::read_to_string(&format_path).map_err(|err| host_io(&format_path, err))?;
    if !format.trim_end().starts_with("fmn-cache ") {
        return Err(root_refused(
            configured_root,
            "format stamp is absent or foreign",
        ));
    }
    Ok((canonical_root, owner))
}

fn reject_protected_host_root(
    canonical_root: &Path,
    configured_root: &Path,
    cwd: &Path,
) -> Result<(), CacheError> {
    if canonical_root.parent().is_none() {
        return Err(root_refused(
            configured_root,
            "filesystem roots are never cache roots",
        ));
    }
    let canonical_cwd = fs::canonicalize(cwd).map_err(|err| host_io(cwd, err))?;
    if canonical_cwd.starts_with(canonical_root) {
        return Err(root_refused(
            configured_root,
            "root is the current directory or one of its ancestors",
        ));
    }
    let home = host_home().ok_or_else(|| {
        root_refused(
            configured_root,
            "home directory is unavailable; destructive authorization fails closed",
        )
    })?;
    let canonical_home = fs::canonicalize(&home).map_err(|err| host_io(&home, err))?;
    if canonical_home.starts_with(canonical_root) {
        return Err(root_refused(
            configured_root,
            "root is the home directory or one of its ancestors",
        ));
    }
    Ok(())
}

fn reject_symlink_components(path: &Path) -> Result<(), CacheError> {
    let mut ancestors: Vec<&Path> = path.ancestors().collect();
    ancestors.reverse();
    for component in ancestors {
        let metadata = fs::symlink_metadata(component).map_err(|err| host_io(component, err))?;
        if metadata.file_type().is_symlink() {
            return Err(root_refused(
                path,
                "root contains a symlinked path component",
            ));
        }
    }
    Ok(())
}

fn reject_symlink_leaf(path: &Path, root: &Path, label: &str) -> Result<(), CacheError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Err(root_refused(root, format!("{label} is absent")));
        }
        Err(err) => return Err(host_io(path, err)),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(root_refused(root, format!("{label} is not a regular file")));
    }
    Ok(())
}

fn host_home() -> Option<PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .or_else(|| {
            let drive = env::var_os("HOMEDRIVE")?;
            let path = env::var_os("HOMEPATH")?;
            Some(PathBuf::from(drive).join(path))
        })
}

fn host_io(path: &Path, err: io::Error) -> CacheError {
    let err = if err.kind() == io::ErrorKind::NotFound {
        FsError::NotFound {
            path: path.to_path_buf(),
        }
    } else {
        FsError::Io {
            path: path.to_path_buf(),
            err,
        }
    };
    CacheError::Storage(err)
}

/// Config-visible store knobs (surfaced through fmn-config once fm-3gl's
/// typed config lands; constructed directly until then).
#[derive(Clone, Copy, Debug)]
pub struct StoreConfig {
    /// The per-entry payload ceiling. An over-limit payload is a precise
    /// [`CacheError::EntryTooLarge`] and the value simply goes uncached.
    pub max_entry_bytes: usize,
    /// How long a maintenance lock may sit unrenewed before another process
    /// may break it as stale. Must exceed any plausible eviction duration.
    pub lock_stale_after: Duration,
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            max_entry_bytes: 256 * 1024 * 1024,
            lock_stale_after: Duration::from_secs(60),
        }
    }
}

/// Per-namespace eviction policy, config-visible.
#[derive(Clone, Copy, Debug, Default)]
pub struct NamespacePolicy {
    /// The size ceiling automatic eviction trims toward. `None` is the manual
    /// policy: no automatic eviction ever (the replay journal's namespace —
    /// its lifecycle is explicit).
    pub ceiling_bytes: Option<u64>,
}

/// What [`Namespace::evict_to_ceiling`] did.
#[derive(Debug)]
pub enum EvictOutcome {
    /// Eviction ran; the report says what happened.
    Done(EvictReport),
    /// Another maintainer holds a fresh lock; nothing was done. Callers just
    /// retry on their next maintenance tick.
    SkippedLockHeld,
    /// The namespace has no ceiling ([`NamespacePolicy::ceiling_bytes`] is
    /// `None`); automatic eviction is disabled by policy.
    Unlimited,
}

/// The accounting from one eviction pass.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct EvictReport {
    /// Entries found on disk before eviction.
    pub examined: usize,
    /// Entries removed to reach the ceiling.
    pub evicted: usize,
    /// Bytes those removals reclaimed.
    pub evicted_bytes: u64,
    /// Bytes remaining after the pass.
    pub retained_bytes: u64,
    /// Entries the pass would have evicted but skipped because they were
    /// pinned.
    pub skipped_pinned: usize,
    /// Unrecognized files swept from the object directories (orphaned temp
    /// files from killed writers, junk).
    pub swept_unrecognized: usize,
}

/// In-use marking per namespace directory, shared by every [`Namespace`]
/// handle onto that directory so pinning is a property of the store, not of
/// handle discipline.
#[derive(Debug, Default)]
struct PinSet {
    counts: Mutex<BTreeMap<Digest, usize>>,
}

impl PinSet {
    fn pin(&self, digest: Digest) {
        let mut counts = self.counts.lock().unwrap_or_else(lock_poisoned);
        *counts.entry(digest).or_insert(0) += 1;
    }

    fn unpin(&self, digest: &Digest) {
        let mut counts = self.counts.lock().unwrap_or_else(lock_poisoned);
        if let Some(n) = counts.get_mut(digest) {
            *n -= 1;
            if *n == 0 {
                counts.remove(digest);
            }
        }
    }

    fn snapshot(&self) -> BTreeSet<Digest> {
        self.counts
            .lock()
            .unwrap_or_else(lock_poisoned)
            .keys()
            .copied()
            .collect()
    }
}

/// An in-use marker: while any [`Pin`] for an address is alive, eviction will
/// not remove that entry. Dropping the pin releases it.
#[derive(Debug)]
pub struct Pin {
    set: Arc<PinSet>,
    digest: Digest,
}

impl Drop for Pin {
    fn drop(&mut self) {
        self.set.unpin(&self.digest);
    }
}

/// One entry's advisory bookkeeping.
#[derive(Clone, Copy, Debug)]
struct IndexEntry {
    /// The entry file's size in bytes (the full envelope, as stored).
    size: u64,
    /// The logical access sequence at last touch — the LRU ordinate.
    last_seq: u64,
}

/// The in-memory access log: the loaded index plus everything this handle has
/// touched since. Flushed (merged, last-writer-wins) on put, on eviction, on
/// [`Namespace::flush`], and best-effort on drop.
#[derive(Debug, Default)]
struct AccessLog {
    next_seq: u64,
    entries: BTreeMap<Digest, IndexEntry>,
}

struct StoreInner {
    fs: Arc<dyn FileSystem>,
    clock: Arc<dyn Clock>,
    root: PathBuf,
    config: StoreConfig,
    /// Serial limits for entry envelopes, derived from `max_entry_bytes`.
    entry_limits: Limits,
    /// This store opening's instance id (lock-token uniqueness).
    instance: u64,
    /// Shared pin sets, one per namespace directory.
    pins: Mutex<HashMap<PathBuf, Arc<PinSet>>>,
}

/// The persistent content-addressed store. Cheap to clone conceptually — open
/// namespaces via [`Store::namespace`]; the store itself is just the root,
/// the capabilities, and the config.
pub struct Store {
    inner: Arc<StoreInner>,
}

impl fmt::Debug for Store {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Store")
            .field("root", &self.inner.root)
            .field("config", &self.inner.config)
            .finish_non_exhaustive()
    }
}

impl Store {
    /// Open (creating if needed) the owned store at `root`.
    ///
    /// A missing absolute leaf under an existing real parent may be claimed.
    /// Any existing unstamped directory (including an empty one), a symlinked
    /// host path, a copied/path-mismatched ownership marker, or a lost stamp
    /// race is refused.
    ///
    /// # Errors
    /// [`CacheError::RootRefused`] if ownership cannot be proven,
    /// [`CacheError::FormatUnsupported`] if the owned root carries a stamp
    /// from a different store format, or [`CacheError::Storage`].
    pub fn open(
        fs: Arc<dyn FileSystem>,
        clock: Arc<dyn Clock>,
        root: impl Into<PathBuf>,
        config: StoreConfig,
    ) -> Result<Self, CacheError> {
        let requested_root = root.into();
        let (root, state) = if fs.grants_host_destructive_lifecycle() {
            prepare_host_store_root(&requested_root)?
        } else {
            (requested_root, RootOpenState::CapabilityManaged)
        };
        ensure_owned_root(fs.as_ref(), &root, state)?;

        let entry_limits = Limits {
            max_field: config.max_entry_bytes,
            // Envelope overhead (header, kind, address, length prefix,
            // checksum) is well under this slack.
            max_total: config.max_entry_bytes.saturating_add(4096),
        };
        Ok(Self {
            inner: Arc::new(StoreInner {
                fs,
                clock,
                root,
                config,
                entry_limits,
                instance: NEXT_INSTANCE.fetch_add(1, Ordering::Relaxed),
                pins: Mutex::new(HashMap::new()),
            }),
        })
    }

    /// The store root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    /// Open a versioned namespace. The name is validated (the traversal
    /// boundary); the version selects the directory, so a bump is a clean,
    /// namespace-local cold start. Stale sibling versions are purged
    /// best-effort on open.
    ///
    /// # Errors
    /// [`CacheError::InvalidNamespace`].
    pub fn namespace(
        &self,
        name: &str,
        version: u32,
        policy: NamespacePolicy,
    ) -> Result<Namespace, CacheError> {
        validate_namespace_name(name)?;
        let dir = self
            .inner
            .root
            .join("ns")
            .join(name)
            .join(format!("v{version}"));
        let pins = {
            let mut registry = self.inner.pins.lock().unwrap_or_else(lock_poisoned);
            Arc::clone(registry.entry(dir.clone()).or_default())
        };
        let ns = Namespace {
            inner: Arc::clone(&self.inner),
            name: name.to_owned(),
            version,
            objects_dir: dir.join("objects"),
            index_path: dir.join("index"),
            lock_path: dir.join("lock"),
            dir,
            policy,
            pins,
            access: Mutex::new(AccessLog::default()),
            held_lock: Mutex::new(None),
        };
        ns.load_index();
        let _ = ns.purge_stale_versions();
        Ok(ns)
    }
}

/// Reject any namespace name that could perturb pathing. The rule is strict
/// on purpose: lowercase alphanumerics, `-`, `_`, first byte alphanumeric,
/// at most 64 bytes. No dots ever, so no `.`/`..`; no separators; no
/// platform-magic names.
fn validate_namespace_name(name: &str) -> Result<(), CacheError> {
    let reject = |reason: &'static str| {
        Err(CacheError::InvalidNamespace {
            name: name.to_owned(),
            reason,
        })
    };
    if name.is_empty() {
        return reject("empty");
    }
    if name.len() > 64 {
        return reject("longer than 64 bytes");
    }
    let bytes = name.as_bytes();
    if !bytes[0].is_ascii_lowercase() && !bytes[0].is_ascii_digit() {
        return reject("must start with a lowercase letter or digit");
    }
    if !bytes
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-' || *b == b'_')
    {
        return reject("only [a-z0-9_-] allowed");
    }
    Ok(())
}

/// One versioned namespace of the store. See the crate docs for the get/put,
/// pinning, and eviction contracts.
pub struct Namespace {
    inner: Arc<StoreInner>,
    name: String,
    version: u32,
    dir: PathBuf,
    objects_dir: PathBuf,
    index_path: PathBuf,
    lock_path: PathBuf,
    policy: NamespacePolicy,
    pins: Arc<PinSet>,
    access: Mutex<AccessLog>,
    /// The exact lock-token bytes we hold, if any (release verifies them).
    held_lock: Mutex<Option<Vec<u8>>>,
}

impl fmt::Debug for Namespace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Namespace")
            .field("name", &self.name)
            .field("version", &self.version)
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

impl Namespace {
    /// The namespace name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The namespace schema version.
    #[must_use]
    pub fn version(&self) -> u32 {
        self.version
    }

    fn fs(&self) -> &dyn FileSystem {
        self.inner.fs.as_ref()
    }

    /// The object path for an address: `objects/<hh>/<hex…>`, derived from
    /// digest hex only — the traversal protection.
    fn object_path(&self, digest: &Digest) -> PathBuf {
        let hex = digest.to_hex();
        self.objects_dir.join(&hex[..2]).join(&hex[2..])
    }

    // ------------------------------------------------------------------
    // Keyed entries
    // ------------------------------------------------------------------

    /// Look up a keyed entry. `Ok(None)` is a miss — including the corrupt
    /// case, where the bad entry is first evicted (never trusted, never
    /// fatal).
    ///
    /// # Errors
    /// [`CacheError::Storage`] only for real filesystem failures (not
    /// absence, not corruption).
    pub fn get(&self, key: &CacheKey) -> Result<Option<Vec<u8>>, CacheError> {
        self.get_at(key.digest(), EntryKind::Keyed)
    }

    /// Store a keyed entry (write-temp + rename; concurrent writers of the
    /// same key write identical bytes).
    ///
    /// # Errors
    /// [`CacheError::EntryTooLarge`] over the per-entry ceiling,
    /// [`CacheError::Encode`], or [`CacheError::Storage`]. All are safely
    /// ignorable — an unwritten cache entry is a future recompute.
    pub fn put(&self, key: &CacheKey, payload: &[u8]) -> Result<(), CacheError> {
        self.put_at(key.digest(), EntryKind::Keyed, payload)
    }

    /// The read-through composition: a verified hit, or `compute` — with
    /// every cache failure (storage trouble included) degraded to a
    /// recompute, and the computed value stored best-effort. This is the
    /// never-fatal contract as an API shape; `compute`'s own error is the
    /// only error that escapes.
    ///
    /// # Errors
    /// Exactly the errors of `compute`.
    pub fn get_or_compute<E>(
        &self,
        key: &CacheKey,
        compute: impl FnOnce() -> Result<Vec<u8>, E>,
    ) -> Result<Vec<u8>, E> {
        if let Ok(Some(hit)) = self.get(key) {
            return Ok(hit);
        }
        let value = compute()?;
        let _ = self.put(key, &value);
        Ok(value)
    }

    // ------------------------------------------------------------------
    // Blob (content-addressed) entries
    // ------------------------------------------------------------------

    /// Store content under its own hash and return the address. Fetched
    /// assets live here: the address doubles as the integrity statement.
    ///
    /// # Errors
    /// As [`Namespace::put`].
    pub fn put_blob(&self, payload: &[u8]) -> Result<Digest, CacheError> {
        let digest = sha256(payload);
        self.put_at(&digest, EntryKind::Blob, payload)?;
        Ok(digest)
    }

    /// Look up content by its hash; the payload is verified against the
    /// address itself (self-certifying), on top of the envelope checksum.
    ///
    /// # Errors
    /// As [`Namespace::get`].
    pub fn get_blob(&self, digest: &Digest) -> Result<Option<Vec<u8>>, CacheError> {
        self.get_at(digest, EntryKind::Blob)
    }

    fn get_at(&self, digest: &Digest, kind: EntryKind) -> Result<Option<Vec<u8>>, CacheError> {
        let path = self.object_path(digest);
        let bytes = match self.fs().read(&path) {
            Ok(bytes) => bytes,
            Err(FsError::NotFound { .. }) => {
                // A ghost (evicted elsewhere): drop any bookkeeping.
                self.forget(digest);
                return Ok(None);
            }
            Err(err) => return Err(err.into()),
        };
        match entry::decode(&bytes, kind, digest, self.inner.entry_limits) {
            Ok(payload) => {
                self.touch(digest, bytes.len() as u64);
                Ok(Some(payload))
            }
            Err(_corrupt) => {
                // Evicted, never trusted, never fatal: the next lookup is a
                // clean miss and the consumer recomputes.
                let _ = self.fs().remove_file(&path);
                self.forget(digest);
                Ok(None)
            }
        }
    }

    fn put_at(&self, digest: &Digest, kind: EntryKind, payload: &[u8]) -> Result<(), CacheError> {
        if payload.len() > self.inner.config.max_entry_bytes {
            return Err(CacheError::EntryTooLarge {
                limit: self.inner.config.max_entry_bytes,
                needed: payload.len(),
            });
        }
        let doc = entry::encode(kind, digest, payload, self.inner.entry_limits)?;
        let size = doc.len() as u64;
        self.fs().write_atomic(&self.object_path(digest), &doc)?;
        self.touch(digest, size);
        // Durable bookkeeping rides the (cold) write path; read bumps stay
        // in memory until some flush point.
        let _ = self.flush();
        Ok(())
    }

    // ------------------------------------------------------------------
    // Pinning
    // ------------------------------------------------------------------

    /// Pin a keyed entry's address against eviction while the guard lives.
    /// Pinning is per-store-process and shared across every handle onto this
    /// namespace directory; pin-then-put is legitimate.
    #[must_use]
    pub fn pin(&self, key: &CacheKey) -> Pin {
        self.pin_digest(*key.digest())
    }

    /// Pin an address directly (blob addresses, journal segments).
    #[must_use]
    pub fn pin_digest(&self, digest: Digest) -> Pin {
        self.pins.pin(digest);
        Pin {
            set: Arc::clone(&self.pins),
            digest,
        }
    }

    // ------------------------------------------------------------------
    // The advisory index
    // ------------------------------------------------------------------

    fn touch(&self, digest: &Digest, size: u64) {
        let mut log = self.access.lock().unwrap_or_else(lock_poisoned);
        let seq = log.next_seq;
        log.next_seq += 1;
        log.entries.insert(
            *digest,
            IndexEntry {
                size,
                last_seq: seq,
            },
        );
    }

    fn forget(&self, digest: &Digest) {
        let mut log = self.access.lock().unwrap_or_else(lock_poisoned);
        log.entries.remove(digest);
    }

    /// Load the on-disk index into the access log; any failure (absent,
    /// corrupt, foreign version) is an empty log — the index is advisory and
    /// eviction rebuilds it from disk truth.
    fn load_index(&self) {
        if let Some(loaded) = self.read_index_file() {
            *self.access.lock().unwrap_or_else(lock_poisoned) = loaded;
        }
    }

    fn read_index_file(&self) -> Option<AccessLog> {
        let bytes = self.fs().read(&self.index_path).ok()?;
        let mut r =
            Reader::open(&bytes, INDEX_SCHEMA, Limits::DEFAULT, UnknownPolicy::Strict).ok()?;
        let next_seq = r.get_u64().ok()?;
        let count = r.get_u64().ok()?;
        let mut entries = BTreeMap::new();
        for _ in 0..count {
            let digest = r.get_digest().ok()?;
            let size = r.get_u64().ok()?;
            let last_seq = r.get_u64().ok()?;
            entries.insert(digest, IndexEntry { size, last_seq });
        }
        r.finish().ok()?;
        Some(AccessLog { next_seq, entries })
    }

    /// Merge this handle's access log with the on-disk index (max sequence
    /// wins per entry) and write it back atomically. Concurrent flushes are
    /// last-writer-wins — the index is advisory by design.
    ///
    /// # Errors
    /// [`CacheError::Storage`] or [`CacheError::Encode`]; callers on the hot
    /// path ignore both (bookkeeping, not data).
    pub fn flush(&self) -> Result<(), CacheError> {
        let mut log = self.access.lock().unwrap_or_else(lock_poisoned);
        if let Some(disk) = self.read_index_file() {
            log.next_seq = log.next_seq.max(disk.next_seq);
            for (digest, theirs) in disk.entries {
                log.entries
                    .entry(digest)
                    .and_modify(|ours| {
                        if theirs.last_seq > ours.last_seq {
                            ours.last_seq = theirs.last_seq;
                        }
                    })
                    .or_insert(theirs);
            }
        }
        self.write_index(&log)
    }

    fn write_index(&self, log: &AccessLog) -> Result<(), CacheError> {
        let mut w = Writer::new(INDEX_SCHEMA);
        w.put_u64(log.next_seq);
        w.put_u64(log.entries.len() as u64);
        for (digest, e) in &log.entries {
            w.put_digest(digest);
            w.put_u64(e.size);
            w.put_u64(e.last_seq);
        }
        let doc = w.finish()?;
        self.fs().write_atomic(&self.index_path, &doc)?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Maintenance: scan, eviction, version purge
    // ------------------------------------------------------------------

    /// Walk the object directories: every parseable entry address, plus the
    /// paths of unrecognized files (orphaned writer temps, junk).
    fn scan(&self) -> Result<(BTreeSet<Digest>, Vec<PathBuf>), CacheError> {
        let mut digests = BTreeSet::new();
        let mut unrecognized = Vec::new();
        let shards = match self.fs().list_dir(&self.objects_dir) {
            Ok(shards) => shards,
            Err(FsError::NotFound { .. }) => return Ok((digests, unrecognized)),
            Err(err) => return Err(err.into()),
        };
        for shard in shards {
            let Some(shard_name) = shard.file_name().map(|n| n.to_string_lossy().into_owned())
            else {
                continue;
            };
            let files = match self.fs().list_dir(&shard) {
                Ok(files) => files,
                Err(FsError::NotFound { .. }) => continue,
                Err(err) => return Err(err.into()),
            };
            for file in files {
                let Some(file_name) = file.file_name().map(|n| n.to_string_lossy().into_owned())
                else {
                    continue;
                };
                match Digest::from_hex(&format!("{shard_name}{file_name}")) {
                    Ok(digest) => {
                        digests.insert(digest);
                    }
                    Err(_) => unrecognized.push(file),
                }
            }
        }
        Ok((digests, unrecognized))
    }

    /// Total bytes currently stored in this namespace (entry envelopes as on
    /// disk).
    ///
    /// # Errors
    /// [`CacheError::Storage`].
    pub fn usage(&self) -> Result<u64, CacheError> {
        let (digests, _) = self.scan()?;
        let log = self.access.lock().unwrap_or_else(lock_poisoned);
        let mut total = 0u64;
        for digest in &digests {
            total += match log.entries.get(digest) {
                Some(e) => e.size,
                None => self
                    .fs()
                    .read(&self.object_path(digest))
                    .map(|b| b.len() as u64)
                    .unwrap_or(0),
            };
        }
        Ok(total)
    }

    /// Trim this namespace toward its ceiling: least-recently-used first
    /// (logical access order, ties by digest for determinism), skipping
    /// pinned entries, sweeping unrecognized files, and reconciling the
    /// advisory index against disk truth. Non-blocking: if another
    /// maintainer holds a fresh lock this returns
    /// [`EvictOutcome::SkippedLockHeld`].
    ///
    /// # Errors
    /// [`CacheError::Storage`] on real filesystem failures mid-pass.
    pub fn evict_to_ceiling(&self) -> Result<EvictOutcome, CacheError> {
        let Some(ceiling) = self.policy.ceiling_bytes else {
            return Ok(EvictOutcome::Unlimited);
        };
        if !self.acquire_maintenance_lock()? {
            return Ok(EvictOutcome::SkippedLockHeld);
        }
        let outcome = self.evict_under_lock(ceiling);
        self.release_maintenance_lock();
        outcome.map(EvictOutcome::Done)
    }

    fn evict_under_lock(&self, ceiling: u64) -> Result<EvictReport, CacheError> {
        let (on_disk, unrecognized) = self.scan()?;
        let mut report = EvictReport {
            examined: on_disk.len(),
            ..EvictReport::default()
        };
        for path in unrecognized {
            if self.fs().remove_file(&path).is_ok() {
                report.swept_unrecognized += 1;
            }
        }

        let mut log = self.access.lock().unwrap_or_else(lock_poisoned);
        // Reconcile: disk is the truth. Ghost log entries drop; strangers
        // (entries other processes wrote) enter with sequence 0, so they are
        // first out unless someone touches them.
        log.entries.retain(|digest, _| on_disk.contains(digest));
        for digest in &on_disk {
            log.entries.entry(*digest).or_insert_with(|| IndexEntry {
                size: self
                    .fs()
                    .read(&self.object_path(digest))
                    .map(|b| b.len() as u64)
                    .unwrap_or(0),
                last_seq: 0,
            });
        }

        let mut total: u64 = log.entries.values().map(|e| e.size).sum();
        if total > ceiling {
            let pinned = self.pins.snapshot();
            let mut order: Vec<(u64, Digest, u64)> = log
                .entries
                .iter()
                .map(|(digest, e)| (e.last_seq, *digest, e.size))
                .collect();
            order.sort_unstable();
            for (_, digest, size) in order {
                if total <= ceiling {
                    break;
                }
                if pinned.contains(&digest) {
                    report.skipped_pinned += 1;
                    continue;
                }
                match self.fs().remove_file(&self.object_path(&digest)) {
                    Ok(()) | Err(FsError::NotFound { .. }) => {
                        log.entries.remove(&digest);
                        total = total.saturating_sub(size);
                        report.evicted += 1;
                        report.evicted_bytes += size;
                    }
                    Err(err) => return Err(err.into()),
                }
            }
        }
        report.retained_bytes = total;
        self.write_index(&log)?;
        Ok(report)
    }

    /// Remove abandoned sibling versions of this namespace (`v<n>` with
    /// `n != version`), leaving every other namespace untouched. Returns how
    /// many were purged. Racing purgers and readers of a dead version are
    /// safe: a vanished entry is a miss.
    ///
    /// # Errors
    /// [`CacheError::Storage`] (absence of the namespace directory is zero,
    /// not an error).
    pub fn purge_stale_versions(&self) -> Result<usize, CacheError> {
        let parent = match self.dir.parent() {
            Some(parent) => parent.to_path_buf(),
            None => return Ok(0),
        };
        let children = match self.fs().list_dir(&parent) {
            Ok(children) => children,
            Err(FsError::NotFound { .. }) => return Ok(0),
            Err(err) => return Err(err.into()),
        };
        let keep = format!("v{}", self.version);
        let mut purged = 0;
        for child in children {
            let Some(name) = child.file_name().map(|n| n.to_string_lossy().into_owned()) else {
                continue;
            };
            if name == keep {
                continue;
            }
            // Only `v<u32>` directories are ours to reclaim.
            if let Some(rest) = name.strip_prefix('v')
                && rest.parse::<u32>().is_ok()
                && self.fs().remove_dir_all(&child).is_ok()
            {
                purged += 1;
            }
        }
        Ok(purged)
    }

    // ------------------------------------------------------------------
    // The advisory maintenance lock
    // ------------------------------------------------------------------

    fn now_wall_nanos(&self) -> u64 {
        self.inner
            .clock
            .wall()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
            .unwrap_or(0)
    }

    fn lock_token(&self) -> Result<Vec<u8>, CacheError> {
        let mut w = Writer::new(LOCK_SCHEMA);
        w.put_u64(u64::from(std::process::id()));
        w.put_u64(self.inner.instance);
        w.put_u64(self.now_wall_nanos());
        Ok(w.finish()?)
    }

    fn lock_acquired_nanos(bytes: &[u8]) -> Option<u64> {
        let mut r =
            Reader::open(bytes, LOCK_SCHEMA, Limits::DEFAULT, UnknownPolicy::Strict).ok()?;
        let _pid = r.get_u64().ok()?;
        let _instance = r.get_u64().ok()?;
        let acquired = r.get_u64().ok()?;
        r.finish().ok()?;
        Some(acquired)
    }

    /// Try to take the maintenance lock: `Ok(true)` if held after this call.
    /// A fresh foreign lock means `Ok(false)` (skip, never block); a stale or
    /// unparseable one is broken and re-contended once.
    fn acquire_maintenance_lock(&self) -> Result<bool, CacheError> {
        let token = self.lock_token()?;
        if self.fs().create_new(&self.lock_path, &token)? {
            *self.held_lock.lock().unwrap_or_else(lock_poisoned) = Some(token);
            return Ok(true);
        }
        // Occupied: fresh means skip; stale or garbage means break and
        // re-contend (create_new arbitrates the re-contention race).
        let breakable = match self.fs().read(&self.lock_path) {
            Ok(existing) => match Self::lock_acquired_nanos(&existing) {
                Some(acquired) => {
                    let age = self.now_wall_nanos().saturating_sub(acquired);
                    u128::from(age) > self.inner.config.lock_stale_after.as_nanos()
                }
                // An unparseable token can never renew or expire; treat it
                // as abandoned.
                None => true,
            },
            // Vanished between create_new and read: the holder released;
            // re-contend.
            Err(FsError::NotFound { .. }) => true,
            Err(err) => return Err(err.into()),
        };
        if !breakable {
            return Ok(false);
        }
        let _ = self.fs().remove_file(&self.lock_path);
        if self.fs().create_new(&self.lock_path, &token)? {
            *self.held_lock.lock().unwrap_or_else(lock_poisoned) = Some(token);
            return Ok(true);
        }
        Ok(false)
    }

    /// Release the maintenance lock if the file still carries our token (a
    /// staleness-breaker may have replaced it; never remove someone else's).
    fn release_maintenance_lock(&self) {
        let mut held = self.held_lock.lock().unwrap_or_else(lock_poisoned);
        if let Some(token) = held.take()
            && self
                .fs()
                .read(&self.lock_path)
                .is_ok_and(|cur| cur == token)
        {
            let _ = self.fs().remove_file(&self.lock_path);
        }
    }
}

impl Drop for Namespace {
    fn drop(&mut self) {
        // Best-effort: persist read bumps. The index is advisory, so a
        // failure here costs LRU accuracy only.
        let _ = self.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespace_names_are_strictly_validated() {
        for good in ["typeset", "a", "journal-2", "x_1", "0start"] {
            assert!(validate_namespace_name(good).is_ok(), "{good:?} rejected");
        }
        for bad in [
            "",
            "..",
            ".",
            "a/b",
            "a\\b",
            "A",
            "café",
            "-lead",
            "_lead",
            ".hidden",
            "name.ext",
            "spa ce",
            &"x".repeat(65),
        ] {
            assert!(
                validate_namespace_name(bad).is_err(),
                "{bad:?} unexpectedly accepted"
            );
        }
    }

    #[test]
    fn evict_report_default_is_zeroed() {
        let r = EvictReport::default();
        assert_eq!(
            r.examined + r.evicted + r.skipped_pinned + r.swept_unrecognized,
            0
        );
        assert_eq!(r.evicted_bytes + r.retained_bytes, 0);
    }
}
