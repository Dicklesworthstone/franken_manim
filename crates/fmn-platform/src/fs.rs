//! The filesystem capability: every byte the engine reads or writes flows
//! through [`FileSystem`], so the input closure can record it and the
//! deterministic lab can virtualize it (see the crate-level doctrine).

use std::collections::BTreeMap;
use std::fmt;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

/// Process-wide uniquifier for temp-file names: the pid alone is not enough,
/// because two threads of one process writing the same destination would
/// collide on the temp path and race each other's rename.
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// A temp-file sibling name unique across processes (pid) and within this
/// process (sequence), for the write-then-rename and write-then-link
/// protocols.
fn unique_temp_name(prefix: &str, path: &Path) -> String {
    format!(
        "{prefix}.{}.{}.{}",
        std::process::id(),
        TEMP_SEQ.fetch_add(1, Ordering::Relaxed),
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    )
}

/// A filesystem failure, carrying the path it happened at.
#[derive(Debug)]
pub enum FsError {
    /// The path does not exist (or a parent directory is missing).
    NotFound {
        /// The missing path.
        path: PathBuf,
    },
    /// The bytes at `path` were expected to be UTF-8 and are not.
    NotUtf8 {
        /// The offending path.
        path: PathBuf,
    },
    /// Any other I/O failure.
    Io {
        /// The path being accessed.
        path: PathBuf,
        /// The underlying error.
        err: std::io::Error,
    },
}

impl fmt::Display for FsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound { path } => write!(f, "not found: {}", path.display()),
            Self::NotUtf8 { path } => write!(f, "not UTF-8: {}", path.display()),
            Self::Io { path, err } => write!(f, "I/O failure at {}: {err}", path.display()),
        }
    }
}

impl std::error::Error for FsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { err, .. } => Some(err),
            _ => None,
        }
    }
}

/// The filesystem capability. Implementations must be deterministic in
/// listing order ([`FileSystem::list_dir`] returns sorted paths) so no
/// consumer inherits host directory-iteration order.
pub trait FileSystem: Send + Sync {
    /// Read the full contents of a file.
    ///
    /// # Errors
    /// [`FsError::NotFound`] or [`FsError::Io`].
    fn read(&self, path: &Path) -> Result<Vec<u8>, FsError>;

    /// Write `bytes` to `path` atomically: the destination either keeps its
    /// old contents or holds exactly `bytes`, never a torn intermediate.
    /// Parent directories are created as needed.
    ///
    /// # Errors
    /// [`FsError::Io`].
    fn write_atomic(&self, path: &Path, bytes: &[u8]) -> Result<(), FsError>;

    /// Create `path` with `bytes` **only if nothing exists there** — the
    /// lock-file primitive. Returns `Ok(true)` if this call created the file,
    /// `Ok(false)` if something already existed (no mutation). The created
    /// file appears with its full contents (never empty-then-filled), so a
    /// concurrent reader sees either absence or the complete bytes. Parent
    /// directories are created as needed.
    ///
    /// # Errors
    /// [`FsError::Io`].
    fn create_new(&self, path: &Path, bytes: &[u8]) -> Result<bool, FsError>;

    /// Remove the file at `path`. Exists for *defined* lifecycle operations —
    /// cache eviction, stale-lock breaking, `--clear-cache` — never for
    /// ad-hoc cleanup; every deletion a consumer performs must be part of a
    /// specified policy.
    ///
    /// # Errors
    /// [`FsError::NotFound`] if there is no file, [`FsError::Io`] otherwise.
    fn remove_file(&self, path: &Path) -> Result<(), FsError>;

    /// Remove the directory at `path` and everything under it. Same doctrine
    /// as [`remove_file`](Self::remove_file): defined lifecycle operations
    /// only (namespace-version purges, `--clear-cache`).
    ///
    /// # Errors
    /// [`FsError::NotFound`] if the path does not exist, [`FsError::Io`]
    /// otherwise.
    fn remove_dir_all(&self, path: &Path) -> Result<(), FsError>;

    /// Whether a file exists at `path`.
    fn exists(&self, path: &Path) -> bool;

    /// The entries directly under `path`, sorted byte-lexicographically.
    ///
    /// # Errors
    /// [`FsError::NotFound`] or [`FsError::Io`].
    fn list_dir(&self, path: &Path) -> Result<Vec<PathBuf>, FsError>;

    /// Read a file and decode it as UTF-8.
    ///
    /// # Errors
    /// [`FsError`] from the read, or [`FsError::NotUtf8`].
    fn read_to_string(&self, path: &Path) -> Result<String, FsError> {
        let bytes = self.read(path)?;
        String::from_utf8(bytes).map_err(|_| FsError::NotUtf8 {
            path: path.to_path_buf(),
        })
    }
}

/// The host filesystem, via `std::fs`. The engine's production capability.
#[derive(Clone, Copy, Debug, Default)]
pub struct StdFs;

impl FileSystem for StdFs {
    fn read(&self, path: &Path) -> Result<Vec<u8>, FsError> {
        std::fs::read(path).map_err(|err| io_error(path, err))
    }

    fn write_atomic(&self, path: &Path, bytes: &[u8]) -> Result<(), FsError> {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent).map_err(|err| io_error(parent, err))?;
        // Unique sibling temp name, then rename into place.
        let tmp = parent.join(unique_temp_name(".fmn-tmp", path));
        let mut f = std::fs::File::create(&tmp).map_err(|err| io_error(&tmp, err))?;
        f.write_all(bytes).map_err(|err| io_error(&tmp, err))?;
        f.sync_all().map_err(|err| io_error(&tmp, err))?;
        drop(f);
        std::fs::rename(&tmp, path).map_err(|err| io_error(path, err))
    }

    fn create_new(&self, path: &Path, bytes: &[u8]) -> Result<bool, FsError> {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent).map_err(|err| io_error(parent, err))?;
        // Write the contents to a unique sibling, then `hard_link` it into
        // place: link creation is atomic and fails if the destination exists,
        // so the file appears fully written or not at all — the lock-file
        // guarantee. A plain `File::create_new` + write would expose an
        // empty-then-filled window to concurrent readers.
        let tmp = parent.join(unique_temp_name(".fmn-new", path));
        let mut f = std::fs::File::create(&tmp).map_err(|err| io_error(&tmp, err))?;
        f.write_all(bytes).map_err(|err| io_error(&tmp, err))?;
        f.sync_all().map_err(|err| io_error(&tmp, err))?;
        drop(f);
        let linked = match std::fs::hard_link(&tmp, path) {
            Ok(()) => Ok(true),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(err) => Err(io_error(path, err)),
        };
        // Best-effort: the unique temp is invisible to consumers either way.
        let _ = std::fs::remove_file(&tmp);
        linked
    }

    fn remove_file(&self, path: &Path) -> Result<(), FsError> {
        std::fs::remove_file(path).map_err(|err| io_error(path, err))
    }

    fn remove_dir_all(&self, path: &Path) -> Result<(), FsError> {
        std::fs::remove_dir_all(path).map_err(|err| io_error(path, err))
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn list_dir(&self, path: &Path) -> Result<Vec<PathBuf>, FsError> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(path).map_err(|err| io_error(path, err))? {
            out.push(entry.map_err(|err| io_error(path, err))?.path());
        }
        out.sort();
        Ok(out)
    }
}

fn io_error(path: &Path, err: std::io::Error) -> FsError {
    if err.kind() == std::io::ErrorKind::NotFound {
        FsError::NotFound {
            path: path.to_path_buf(),
        }
    } else {
        FsError::Io {
            path: path.to_path_buf(),
            err,
        }
    }
}

/// The in-memory test double: a `path → bytes` map with implicit
/// directories. Deterministic by construction (BTreeMap ordering); shared
/// mutability behind an `RwLock` so a populated instance can be handed to
/// consumers as `&dyn FileSystem`.
#[derive(Debug, Default)]
pub struct VirtualFs {
    files: RwLock<BTreeMap<PathBuf, Vec<u8>>>,
}

impl VirtualFs {
    /// An empty virtual filesystem.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert (or replace) a file.
    pub fn insert(&self, path: impl Into<PathBuf>, bytes: impl Into<Vec<u8>>) {
        self.files
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(path.into(), bytes.into());
    }

    /// Load a `path<TAB>contents` manifest (one file per line, `\n` in
    /// contents escaped as `\\n`) — the format the committed synthetic
    /// sysfs fixtures use.
    pub fn load_manifest(&self, manifest: &str) {
        for line in manifest.lines() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((path, contents)) = line.split_once('\t') {
                self.insert(path, contents.replace("\\n", "\n").into_bytes());
            }
        }
    }

    fn with_files<T>(&self, f: impl FnOnce(&BTreeMap<PathBuf, Vec<u8>>) -> T) -> T {
        f(&self
            .files
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner))
    }
}

impl FileSystem for VirtualFs {
    fn read(&self, path: &Path) -> Result<Vec<u8>, FsError> {
        self.with_files(|files| {
            files.get(path).cloned().ok_or(FsError::NotFound {
                path: path.to_path_buf(),
            })
        })
    }

    fn write_atomic(&self, path: &Path, bytes: &[u8]) -> Result<(), FsError> {
        self.insert(path.to_path_buf(), bytes.to_vec());
        Ok(())
    }

    fn create_new(&self, path: &Path, bytes: &[u8]) -> Result<bool, FsError> {
        let mut files = self
            .files
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // One write-lock hold makes check-and-insert atomic, matching the
        // host implementation's create-if-absent guarantee.
        if files.contains_key(path) {
            return Ok(false);
        }
        files.insert(path.to_path_buf(), bytes.to_vec());
        Ok(true)
    }

    fn remove_file(&self, path: &Path) -> Result<(), FsError> {
        let mut files = self
            .files
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        files.remove(path).map(|_| ()).ok_or(FsError::NotFound {
            path: path.to_path_buf(),
        })
    }

    fn remove_dir_all(&self, path: &Path) -> Result<(), FsError> {
        let mut files = self
            .files
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let before = files.len();
        files.retain(|k, _| !k.starts_with(path));
        if files.len() == before {
            return Err(FsError::NotFound {
                path: path.to_path_buf(),
            });
        }
        Ok(())
    }

    fn exists(&self, path: &Path) -> bool {
        self.with_files(|files| {
            files.contains_key(path) || files.keys().any(|k| k.starts_with(path))
        })
    }

    fn list_dir(&self, path: &Path) -> Result<Vec<PathBuf>, FsError> {
        self.with_files(|files| {
            let mut out: Vec<PathBuf> = Vec::new();
            for key in files.keys() {
                if let Ok(rest) = key.strip_prefix(path)
                    && let Some(first) = rest.components().next()
                {
                    let child = path.join(first);
                    if out.last() != Some(&child) {
                        out.push(child);
                    }
                }
            }
            if out.is_empty() && !self.exists(path) {
                return Err(FsError::NotFound {
                    path: path.to_path_buf(),
                });
            }
            Ok(out)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtual_fs_read_write_list() {
        let fs = VirtualFs::new();
        fs.insert("/a/b/one.txt", b"1".to_vec());
        fs.insert("/a/b/two.txt", b"2".to_vec());
        fs.insert("/a/c.txt", b"3".to_vec());
        assert_eq!(fs.read(Path::new("/a/b/one.txt")).unwrap(), b"1");
        assert!(matches!(
            fs.read(Path::new("/a/missing")),
            Err(FsError::NotFound { .. })
        ));
        assert!(fs.exists(Path::new("/a/b")));
        let listed = fs.list_dir(Path::new("/a")).unwrap();
        assert_eq!(
            listed,
            vec![PathBuf::from("/a/b"), PathBuf::from("/a/c.txt")]
        );
        fs.write_atomic(Path::new("/a/c.txt"), b"replaced").unwrap();
        assert_eq!(
            fs.read_to_string(Path::new("/a/c.txt")).unwrap(),
            "replaced"
        );
    }

    #[test]
    fn virtual_fs_create_new_is_create_if_absent() {
        let fs = VirtualFs::new();
        assert!(fs.create_new(Path::new("/lock"), b"a").unwrap());
        assert!(!fs.create_new(Path::new("/lock"), b"b").unwrap());
        // The losing create mutated nothing.
        assert_eq!(fs.read(Path::new("/lock")).unwrap(), b"a");
    }

    #[test]
    fn virtual_fs_remove_file_and_dir() {
        let fs = VirtualFs::new();
        fs.insert("/ns/a/one", b"1".to_vec());
        fs.insert("/ns/a/two", b"2".to_vec());
        fs.insert("/ns/b/three", b"3".to_vec());
        fs.remove_file(Path::new("/ns/a/one")).unwrap();
        assert!(!fs.exists(Path::new("/ns/a/one")));
        assert!(matches!(
            fs.remove_file(Path::new("/ns/a/one")),
            Err(FsError::NotFound { .. })
        ));
        fs.remove_dir_all(Path::new("/ns/a")).unwrap();
        assert!(!fs.exists(Path::new("/ns/a")));
        // The sibling namespace is untouched.
        assert_eq!(fs.read(Path::new("/ns/b/three")).unwrap(), b"3");
        assert!(matches!(
            fs.remove_dir_all(Path::new("/ns/a")),
            Err(FsError::NotFound { .. })
        ));
    }

    #[test]
    fn manifest_loading_unescapes_newlines() {
        let fs = VirtualFs::new();
        fs.load_manifest("# comment\n/sys/x\t0-3\\n\n/sys/y\tabc\n");
        assert_eq!(fs.read_to_string(Path::new("/sys/x")).unwrap(), "0-3\n");
        assert_eq!(fs.read_to_string(Path::new("/sys/y")).unwrap(), "abc");
    }
}
