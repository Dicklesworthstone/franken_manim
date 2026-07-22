//! The self-golden rig (§16.3 plane 2, D-16, fm-xb3).
//!
//! Self-goldens are FrankenManim's **own** outputs, bit-locked: the regression
//! gate that actually blocks merges. This module owns the mechanism:
//!
//! - **Lock files** hold `(name, byte length, SHA-256)` rows — content hashes
//!   via fmn-hash, never the artifact bytes themselves — one file per
//!   `(suite, key)` where the key is the platform key (per-platform locks) or
//!   the literal `certified` (one lock shared by the whole certified matrix,
//!   §16.7).
//! - **Checking** an artifact recomputes its hash and compares against the
//!   lock. Any drift — changed bytes or a missing entry — is a hard error in
//!   check mode, which is what makes CI a merge blocker.
//! - **Blessing** (`UPDATE_GOLDENS=1`) rewrites the lock entry in the working
//!   tree. The rig never commits anything: a bless shows up in `git diff` for
//!   a human to review and commit, per the bead's "never auto-committing".
//! - **`.actual` sidecars**: on drift in check mode the offending bytes are
//!   written next to the lock (under `<suite>.<key>.actual/`), so a failure on
//!   CI or another machine can be diffed byte-for-byte. Sidecars are
//!   gitignored (`*.actual`).
//!
//! Artifact names are constrained to a conservative character set — they are
//! path components, and a fixture name must never be a traversal vector.

use fmn_hash::sha256;
use std::collections::BTreeMap;
use std::fmt;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

/// Serializes every load-modify-write of a lock file within this process:
/// `cargo test` runs tests in parallel, and two concurrent blesses into one
/// suite must not lose each other's entries. (Cross-process bless runs are
/// out of scope: CI checks, humans bless.)
static LOCK_FILE_GUARD: Mutex<()> = Mutex::new(());

/// Monotonic counter making concurrent tmp-file names unique.
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The lock-file format version tag; the first line of every lock file.
const LOCK_HEADER_PREFIX: &str = "# fmn-golden-lock v1";

/// Which machines a lock file speaks for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scope {
    /// One lock file per platform key (`<os>-<arch>`): the default for
    /// anything whose bits may legitimately differ across platforms until
    /// certified arithmetic covers it.
    PerPlatform,
    /// One lock file for the whole certified matrix: bits are promised
    /// identical everywhere (§16.7), so every platform checks the same lock.
    Certified,
}

/// Whether a mismatch fails (CI) or re-locks (a deliberate local bless).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Drift is an error; sidecars are written. What CI runs.
    Check,
    /// Drift updates the lock file in the working tree (never committed by
    /// the rig). Selected by `UPDATE_GOLDENS=1`.
    Bless,
}

impl Mode {
    /// Read the mode from the environment: `UPDATE_GOLDENS=1` means
    /// [`Mode::Bless`], anything else means [`Mode::Check`].
    #[must_use]
    pub fn from_env() -> Self {
        match std::env::var("UPDATE_GOLDENS") {
            Ok(v) if v == "1" => Self::Bless,
            _ => Self::Check,
        }
    }
}

/// One locked artifact: its byte length (a fast pre-check and a human-legible
/// diff hint) and its SHA-256 in lowercase hex.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LockEntry {
    /// Artifact length in bytes.
    pub len: u64,
    /// Lowercase-hex SHA-256 of the artifact bytes.
    pub sha256_hex: String,
}

/// Outcome of a passing [`GoldenStore::check_with_mode`] call.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Verdict {
    /// The artifact matched its lock entry bit-for-bit.
    Match,
    /// Bless mode re-locked the artifact.
    Blessed {
        /// The entry that was replaced, or `None` if this is a new artifact.
        previous: Option<LockEntry>,
    },
}

/// A rig failure. [`GoldenError::Drift`] is the one CI exists to surface.
#[derive(Debug)]
pub enum GoldenError {
    /// The artifact or suite name contains characters outside
    /// `[a-z0-9._-]` (names are path components; traversal is refused).
    InvalidName(String),
    /// Filesystem failure reading or writing the lock or a sidecar.
    Io {
        /// The path being read or written.
        path: PathBuf,
        /// The underlying error.
        err: std::io::Error,
    },
    /// The lock file exists but cannot be parsed.
    Corrupt {
        /// The lock file path.
        path: PathBuf,
        /// 1-based line number of the offending line.
        line: usize,
        /// What was wrong with it.
        detail: String,
    },
    /// The artifact does not match its lock (or has no entry). In check mode
    /// this is the merge-blocking failure; the actual bytes have been written
    /// to `sidecar` for inspection.
    Drift {
        /// The artifact name.
        name: String,
        /// The locked entry, or `None` when the artifact was never locked.
        expected: Option<LockEntry>,
        /// What the engine actually produced.
        actual: LockEntry,
        /// Where the actual bytes were written.
        sidecar: PathBuf,
    },
}

impl fmt::Display for GoldenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName(name) => {
                write!(f, "invalid golden name {name:?}: use only [a-z0-9._-]")
            }
            Self::Io { path, err } => write!(f, "golden I/O failure at {}: {err}", path.display()),
            Self::Corrupt { path, line, detail } => {
                write!(
                    f,
                    "corrupt lock file {} line {line}: {detail}",
                    path.display()
                )
            }
            Self::Drift {
                name,
                expected,
                actual,
                sidecar,
            } => match expected {
                Some(e) => write!(
                    f,
                    "self-golden drift for {name:?}: locked {} bytes sha256 {}, \
                     got {} bytes sha256 {} (actual bytes at {}; if deliberate, \
                     re-bless with UPDATE_GOLDENS=1 and commit the lock diff)",
                    e.len,
                    e.sha256_hex,
                    actual.len,
                    actual.sha256_hex,
                    sidecar.display()
                ),
                None => write!(
                    f,
                    "self-golden {name:?} has no lock entry: got {} bytes sha256 {} \
                     (actual bytes at {}; lock it with UPDATE_GOLDENS=1 and commit)",
                    actual.len,
                    actual.sha256_hex,
                    sidecar.display()
                ),
            },
        }
    }
}

impl std::error::Error for GoldenError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { err, .. } => Some(err),
            _ => None,
        }
    }
}

/// The platform key per-platform locks are filed under: `<os>-<arch>` from
/// the running build (e.g. `linux-x86_64`, `macos-aarch64`).
#[must_use]
pub fn platform_key() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
        && !name.starts_with('.')
}

/// A suite of bit-locked artifacts rooted at one directory.
#[derive(Clone, Debug)]
pub struct GoldenStore {
    dir: PathBuf,
    suite: String,
    key: String,
}

impl GoldenStore {
    /// Open (or designate) a golden store. `dir` is the directory holding the
    /// lock files (conventionally a committed `goldens/` under the crate);
    /// `suite` names the lock-file family.
    ///
    /// # Errors
    /// [`GoldenError::InvalidName`] if `suite` is not a safe path component.
    pub fn new(dir: impl Into<PathBuf>, suite: &str, scope: Scope) -> Result<Self, GoldenError> {
        if !valid_name(suite) {
            return Err(GoldenError::InvalidName(suite.to_string()));
        }
        let key = match scope {
            Scope::PerPlatform => platform_key(),
            Scope::Certified => "certified".to_string(),
        };
        Ok(Self {
            dir: dir.into(),
            suite: suite.to_string(),
            key,
        })
    }

    /// The lock file this store reads and (in bless mode) rewrites.
    #[must_use]
    pub fn lock_path(&self) -> PathBuf {
        self.dir.join(format!("{}.{}.lock", self.suite, self.key))
    }

    /// The directory drift sidecars are written into.
    #[must_use]
    pub fn sidecar_dir(&self) -> PathBuf {
        self.dir.join(format!("{}.{}.actual", self.suite, self.key))
    }

    /// Check `bytes` against the lock under the mode selected by the
    /// `UPDATE_GOLDENS` environment variable ([`Mode::from_env`]).
    ///
    /// # Errors
    /// See [`GoldenStore::check_with_mode`].
    pub fn check(&self, name: &str, bytes: &[u8]) -> Result<Verdict, GoldenError> {
        self.check_with_mode(name, bytes, Mode::from_env())
    }

    /// Check `bytes` against the lock entry for `name`.
    ///
    /// In [`Mode::Check`], a mismatch or missing entry writes the actual
    /// bytes to a `.actual` sidecar and returns [`GoldenError::Drift`]. In
    /// [`Mode::Bless`], the lock file is rewritten in place (sorted, atomic
    /// via tmp-and-rename) and the call succeeds with [`Verdict::Blessed`].
    ///
    /// # Errors
    /// [`GoldenError::InvalidName`] for a bad `name`; [`GoldenError::Io`] /
    /// [`GoldenError::Corrupt`] for lock-file trouble; [`GoldenError::Drift`]
    /// for a mismatch in check mode.
    pub fn check_with_mode(
        &self,
        name: &str,
        bytes: &[u8],
        mode: Mode,
    ) -> Result<Verdict, GoldenError> {
        if !valid_name(name) {
            return Err(GoldenError::InvalidName(name.to_string()));
        }
        // Hold the guard across load-modify-write so parallel tests blessing
        // into one suite cannot lose entries. A poisoned guard means another
        // test panicked mid-section; the lock file itself is still consistent
        // (writes are atomic renames), so continue rather than cascade.
        let _guard = LOCK_FILE_GUARD
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut entries = self.load_entries()?;
        let actual = LockEntry {
            len: bytes.len() as u64,
            sha256_hex: sha256(bytes).to_hex(),
        };
        if entries.get(name) == Some(&actual) {
            return Ok(Verdict::Match);
        }
        match mode {
            Mode::Bless => {
                let previous = entries.insert(name.to_string(), actual);
                self.write_entries(&entries)?;
                Ok(Verdict::Blessed { previous })
            }
            Mode::Check => {
                let sidecar = self.write_sidecar(name, bytes)?;
                Err(GoldenError::Drift {
                    name: name.to_string(),
                    expected: entries.remove(name),
                    actual,
                    sidecar,
                })
            }
        }
    }

    /// The locked entries, sorted by name. An absent lock file reads as empty
    /// (the bootstrap state).
    ///
    /// # Errors
    /// [`GoldenError::Io`] / [`GoldenError::Corrupt`] on unreadable or
    /// malformed lock files.
    pub fn load_entries(&self) -> Result<BTreeMap<String, LockEntry>, GoldenError> {
        let path = self.lock_path();
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(err) => return Err(GoldenError::Io { path, err }),
        };
        let mut lines = text.lines().enumerate();
        match lines.next() {
            Some((_, first)) if first.starts_with(LOCK_HEADER_PREFIX) => {}
            Some((_, first)) => {
                return Err(GoldenError::Corrupt {
                    path,
                    line: 1,
                    detail: format!("expected header {LOCK_HEADER_PREFIX:?}, found {first:?}"),
                });
            }
            None => {
                return Err(GoldenError::Corrupt {
                    path,
                    line: 1,
                    detail: "empty lock file (delete it or restore the header)".to_string(),
                });
            }
        }
        let mut entries = BTreeMap::new();
        for (idx, line) in lines {
            let line = line.trim_end();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let fields: Vec<&str> = line.split('\t').collect();
            let [name, len, hex] = fields.as_slice() else {
                return Err(GoldenError::Corrupt {
                    path,
                    line: idx + 1,
                    detail: format!("expected 3 tab-separated fields, found {}", fields.len()),
                });
            };
            if !valid_name(name) {
                return Err(GoldenError::Corrupt {
                    path,
                    line: idx + 1,
                    detail: format!("invalid artifact name {name:?}"),
                });
            }
            let len: u64 = len.parse().map_err(|_| GoldenError::Corrupt {
                path: path.clone(),
                line: idx + 1,
                detail: format!("invalid length field {len:?}"),
            })?;
            if hex.len() != 64
                || !hex
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            {
                return Err(GoldenError::Corrupt {
                    path,
                    line: idx + 1,
                    detail: format!("invalid sha256 field {hex:?}"),
                });
            }
            if entries
                .insert(
                    (*name).to_string(),
                    LockEntry {
                        len,
                        sha256_hex: (*hex).to_string(),
                    },
                )
                .is_some()
            {
                return Err(GoldenError::Corrupt {
                    path,
                    line: idx + 1,
                    detail: format!("duplicate artifact name {name:?}"),
                });
            }
        }
        Ok(entries)
    }

    /// Rewrite the lock file: versioned header, then one sorted
    /// `name\tlen\tsha256` row per artifact. Written to a `.tmp` sibling and
    /// renamed into place so a crash never leaves a torn lock.
    fn write_entries(&self, entries: &BTreeMap<String, LockEntry>) -> Result<(), GoldenError> {
        let path = self.lock_path();
        if let Some(parent) = path.parent() {
            create_dir_all(parent)?;
        }
        let mut out = String::new();
        out.push_str(LOCK_HEADER_PREFIX);
        out.push_str(&format!(" suite={} key={}\n", self.suite, self.key));
        for (name, entry) in entries {
            out.push_str(&format!("{name}\t{}\t{}\n", entry.len, entry.sha256_hex));
        }
        let tmp = path.with_extension(format!(
            "lock.tmp.{}.{}",
            std::process::id(),
            TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        write_file(&tmp, out.as_bytes())?;
        std::fs::rename(&tmp, &path).map_err(|err| GoldenError::Io { path, err })
    }

    /// Write the drift sidecar and return its path.
    fn write_sidecar(&self, name: &str, bytes: &[u8]) -> Result<PathBuf, GoldenError> {
        let dir = self.sidecar_dir();
        create_dir_all(&dir)?;
        let path = dir.join(format!("{name}.actual"));
        write_file(&path, bytes)?;
        Ok(path)
    }
}

fn create_dir_all(path: &Path) -> Result<(), GoldenError> {
    std::fs::create_dir_all(path).map_err(|err| GoldenError::Io {
        path: path.to_path_buf(),
        err,
    })
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<(), GoldenError> {
    let mut f = std::fs::File::create(path).map_err(|err| GoldenError::Io {
        path: path.to_path_buf(),
        err,
    })?;
    f.write_all(bytes).map_err(|err| GoldenError::Io {
        path: path.to_path_buf(),
        err,
    })?;
    f.sync_all().map_err(|err| GoldenError::Io {
        path: path.to_path_buf(),
        err,
    })
}
