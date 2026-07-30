//! The process capability: **the one sanctioned subprocess mechanism** (D2).
//!
//! ffmpeg is the only program the engine will ever spawn, and every rule of
//! the D2 security protocol that belongs to the *mechanism* lives here:
//!
//! - **argv-only.** [`ProcessSpec`] is a program path plus an argument
//!   vector. There is no shell, no string splitting, no interpolation —
//!   a shell cannot be reached through this API at all.
//! - **Environment allowlist.** The child's environment is cleared and
//!   rebuilt from [`ProcessSpec::env`] alone; nothing ambient leaks in.
//! - **Timeout.** [`ProcessSpec::timeout`] bounds wall-clock runtime; on
//!   expiry the child is killed and the outcome says so.
//! - **Output-size limits.** stdout and stderr are each capped at
//!   [`ProcessSpec::max_output_bytes`]; exceeding a cap kills the child with
//!   [`ProcessTermination::OutputLimitExceeded`] — a runaway encoder cannot
//!   fill the disk or the heap.
//! - **Bounded streaming stdin.** [`ProcessRunner::start`] yields a supervised
//!   session with explicit per-chunk and cumulative limits. The OS pipe
//!   supplies backpressure, cooperative cancellation interrupts a blocked
//!   write, and every terminal path reaps the child.
//! - **Audited ffmpeg discovery.** [`FfmpegLocator`] is deliberately not a
//!   generic executable finder. Its host implementation snapshots one explicit
//!   `PATH` value, rejects the complete search policy when any entry is empty,
//!   relative, or otherwise ambiguous, rejects interpreter scripts through a
//!   target-specific native-image check, and returns a canonical absolute
//!   [`FfmpegExecutable`] for the output boundary to fingerprint.
//!
//! - **Process-tree cancellation.** On supported Unix targets, every child
//!   leads a fresh process group and every terminal path kills that complete
//!   group through the pinned nightly's safe standard-library API. Targets
//!   without an equivalent safe mechanism are refused before spawn rather than
//!   silently weakening D2.
//!   Higher layers (job-scoped temp dirs and their `fm-yw7h` hardening,
//!   atomic publication, provenance fingerprinting) belong to the W8
//!   boundary, not the mechanism.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{self, Read as _, Write as _};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

/// The one executable identity discovery may return.
///
/// The path is canonical, absolute, UTF-8 representable for versioned
/// provenance, and names a regular host-native executable image at selection
/// time. fmn-output still owns byte hashing, private-copy binding, and protocol
/// probing: this type closes ambient search, interpreter scripts, and symlink
/// retargeting, not every safe-`std` pathname race.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FfmpegExecutable {
    canonical_path: PathBuf,
}

impl FfmpegExecutable {
    /// The canonical absolute path that fmn-output must fingerprint.
    #[must_use]
    pub fn canonical_path(&self) -> &Path {
        &self.canonical_path
    }
}

/// Typed failures from the ffmpeg-only executable locator.
#[derive(Debug)]
pub enum FfmpegLocatorError {
    /// A relative configured value was not the one fixed search name.
    UnsupportedConfiguredName {
        /// The refused configured value.
        configured: PathBuf,
    },
    /// No explicit search-path snapshot was supplied.
    SearchPathUnavailable,
    /// Search semantics are unavailable on this target.
    SearchUnsupported {
        /// The target operating-system identity.
        platform: &'static str,
    },
    /// The raw search list is structurally malformed.
    MalformedSearchPath {
        /// Stable reason that does not lossily echo hostile bytes.
        reason: &'static str,
    },
    /// One search entry was empty (and therefore must not mean the cwd).
    EmptySearchEntry {
        /// Zero-based entry position.
        index: usize,
    },
    /// One search entry could not be represented in versioned provenance.
    NonUtf8SearchEntry {
        /// Zero-based entry position.
        index: usize,
    },
    /// One search entry contained terminal/control characters.
    ControlSearchEntry {
        /// Zero-based entry position.
        index: usize,
    },
    /// The hostile-input resource bound was exceeded.
    SearchPathLimit {
        /// Stable bounded resource.
        resource: &'static str,
        /// Observed size.
        got: usize,
        /// Accepted maximum.
        max: usize,
    },
    /// One search entry was not absolute.
    RelativeSearchEntry {
        /// Zero-based entry position.
        index: usize,
        /// The refused entry.
        entry: PathBuf,
    },
    /// One search entry contained lexical parent traversal.
    ParentTraversalSearchEntry {
        /// Zero-based entry position.
        index: usize,
        /// The refused entry.
        entry: PathBuf,
    },
    /// A candidate could not be inspected or canonicalized.
    CandidateIo {
        /// Candidate path at the failing operation.
        candidate: PathBuf,
        /// Stable operation identity.
        operation: &'static str,
        /// Host I/O diagnostic.
        err: io::Error,
    },
    /// Canonicalization returned an identity unsuitable for provenance.
    InvalidCanonicalIdentity {
        /// The canonical path.
        path: PathBuf,
        /// Stable reason.
        reason: &'static str,
    },
    /// An explicitly configured path was not a regular executable file.
    NotExecutable {
        /// The inspected canonical path.
        path: PathBuf,
    },
    /// An explicitly configured file was not a host-native executable image.
    UnsupportedExecutableFormat {
        /// The inspected canonical path.
        path: PathBuf,
    },
    /// Host-native executable-image validation is unsupported on this target.
    NativeImageUnsupported {
        /// The target operating-system identity.
        platform: &'static str,
    },
    /// No host-native executable ffmpeg candidate existed in the search list.
    NotFound,
}

impl fmt::Display for FfmpegLocatorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedConfiguredName { configured } => write!(
                f,
                "relative ffmpeg configuration {:?} is not the fixed ffmpeg search name",
                configured.as_os_str()
            ),
            Self::SearchPathUnavailable => {
                f.write_str("ffmpeg search requested without an explicit PATH snapshot")
            }
            Self::SearchUnsupported { platform } => {
                write!(f, "ffmpeg PATH search is unsupported on {platform}")
            }
            Self::MalformedSearchPath { reason } => {
                write!(f, "ffmpeg search PATH is malformed: {reason}")
            }
            Self::EmptySearchEntry { index } => write!(
                f,
                "ffmpeg search PATH entry {index} is empty; cwd lookup is forbidden"
            ),
            Self::NonUtf8SearchEntry { index } => write!(
                f,
                "ffmpeg search PATH entry {index} is not valid UTF-8; lossy executable provenance is forbidden"
            ),
            Self::ControlSearchEntry { index } => write!(
                f,
                "ffmpeg search PATH entry {index} contains a control character"
            ),
            Self::SearchPathLimit { resource, got, max } => write!(
                f,
                "ffmpeg search PATH {resource} {got} exceeds the limit {max}"
            ),
            Self::RelativeSearchEntry { index, .. } => {
                write!(f, "ffmpeg search PATH entry {index} is relative")
            }
            Self::ParentTraversalSearchEntry { index, .. } => write!(
                f,
                "ffmpeg search PATH entry {index} contains parent traversal"
            ),
            Self::CandidateIo {
                candidate,
                operation,
                err,
            } => write!(
                f,
                "cannot {operation} ffmpeg candidate {:?}: {err}",
                candidate.as_os_str()
            ),
            Self::InvalidCanonicalIdentity { path, reason } => {
                write!(
                    f,
                    "invalid canonical ffmpeg identity {:?}: {reason}",
                    path.as_os_str()
                )
            }
            Self::NotExecutable { path } => write!(
                f,
                "configured ffmpeg path {:?} is not a regular executable file",
                path.as_os_str()
            ),
            Self::UnsupportedExecutableFormat { path } => write!(
                f,
                "configured ffmpeg path {:?} is not a host-native executable image; interpreter scripts are forbidden",
                path.as_os_str()
            ),
            Self::NativeImageUnsupported { platform } => write!(
                f,
                "host-native ffmpeg executable-image validation is unsupported on {platform}"
            ),
            Self::NotFound => f.write_str(
                "no regular host-native executable ffmpeg was found in the validated PATH snapshot",
            ),
        }
    }
}

impl std::error::Error for FfmpegLocatorError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::CandidateIo { err, .. } => Some(err),
            _ => None,
        }
    }
}

/// The ffmpeg-only discovery capability.
///
/// There is intentionally no requested-program argument and no generic
/// executable-locator sibling. Implementations either canonicalize an explicit
/// absolute configuration or resolve the one fixed ffmpeg basename.
pub trait FfmpegLocator: Send + Sync {
    /// Resolve one configured ffmpeg value to a canonical absolute identity.
    ///
    /// # Errors
    ///
    /// [`FfmpegLocatorError`] when the configuration or complete search policy
    /// is invalid, or no regular executable candidate is available.
    fn locate_ffmpeg(&self, configured: &Path) -> Result<FfmpegExecutable, FfmpegLocatorError>;
}

/// Host ffmpeg discovery over a snapshotted, explicitly supplied `PATH`.
#[derive(Clone, Debug, Default)]
pub struct StdFfmpegLocator {
    search_path: Option<OsString>,
}

impl StdFfmpegLocator {
    /// Construct the host locator from an explicit native path-list value.
    #[must_use]
    pub fn from_search_path(search_path: Option<OsString>) -> Self {
        Self { search_path }
    }

    /// Snapshot the host's `PATH` at the outer composition boundary.
    ///
    /// This is the only ambient read in executable discovery. The resulting
    /// capability owns the bytes, so later environment mutation cannot change
    /// its search policy.
    #[must_use]
    pub fn from_host_path() -> Self {
        Self::from_search_path(std::env::var_os("PATH"))
    }

    fn validated_search_directories(&self) -> Result<Vec<PathBuf>, FfmpegLocatorError> {
        let raw = self
            .search_path
            .as_deref()
            .ok_or(FfmpegLocatorError::SearchPathUnavailable)?;
        validate_raw_search_path(raw)?;

        let mut directories = Vec::new();
        for (index, entry) in std::env::split_paths(raw).enumerate() {
            if index >= MAX_FFMPEG_SEARCH_ENTRIES {
                return Err(FfmpegLocatorError::SearchPathLimit {
                    resource: "entry count",
                    got: index.saturating_add(1),
                    max: MAX_FFMPEG_SEARCH_ENTRIES,
                });
            }
            if entry.as_os_str().is_empty() {
                return Err(FfmpegLocatorError::EmptySearchEntry { index });
            }
            let Some(entry_text) = entry.to_str() else {
                return Err(FfmpegLocatorError::NonUtf8SearchEntry { index });
            };
            if entry_text.chars().any(char::is_control) {
                return Err(FfmpegLocatorError::ControlSearchEntry { index });
            }
            if !entry.is_absolute() {
                return Err(FfmpegLocatorError::RelativeSearchEntry { index, entry });
            }
            if entry
                .components()
                .any(|component| component == Component::ParentDir)
            {
                return Err(FfmpegLocatorError::ParentTraversalSearchEntry { index, entry });
            }
            directories.push(entry);
        }
        Ok(directories)
    }
}

impl FfmpegLocator for StdFfmpegLocator {
    fn locate_ffmpeg(&self, configured: &Path) -> Result<FfmpegExecutable, FfmpegLocatorError> {
        if configured.is_absolute() {
            return resolve_ffmpeg_candidate(configured.to_path_buf(), false)?.ok_or_else(|| {
                FfmpegLocatorError::NotExecutable {
                    path: configured.to_path_buf(),
                }
            });
        }
        if !is_default_ffmpeg_name(configured) {
            return Err(FfmpegLocatorError::UnsupportedConfiguredName {
                configured: configured.to_path_buf(),
            });
        }

        // Validate the complete list before touching a single candidate. A
        // successful early entry must not launder forbidden later policy.
        let directories = self.validated_search_directories()?;
        for directory in directories {
            let candidate = directory.join(ffmpeg_search_leaf());
            if let Some(executable) = resolve_ffmpeg_candidate(candidate, true)? {
                return Ok(executable);
            }
        }
        Err(FfmpegLocatorError::NotFound)
    }
}

const MAX_FFMPEG_SEARCH_UNITS: usize = 64 * 1024;
const MAX_FFMPEG_SEARCH_ENTRIES: usize = 256;

#[cfg(any(unix, windows))]
fn validate_raw_search_path(raw: &OsStr) -> Result<(), FfmpegLocatorError> {
    let units = os_str_units(raw);
    if units > MAX_FFMPEG_SEARCH_UNITS {
        return Err(FfmpegLocatorError::SearchPathLimit {
            resource: "size",
            got: units,
            max: MAX_FFMPEG_SEARCH_UNITS,
        });
    }
    if os_str_contains_nul(raw) {
        return Err(FfmpegLocatorError::MalformedSearchPath {
            reason: "contains a NUL code point",
        });
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt as _;
        if raw
            .encode_wide()
            .filter(|unit| *unit == u16::from(b'"'))
            .count()
            % 2
            != 0
        {
            return Err(FfmpegLocatorError::MalformedSearchPath {
                reason: "contains an unbalanced double quote",
            });
        }
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn validate_raw_search_path(_raw: &OsStr) -> Result<(), FfmpegLocatorError> {
    Err(FfmpegLocatorError::SearchUnsupported {
        platform: std::env::consts::OS,
    })
}

#[cfg(unix)]
fn os_str_contains_nul(value: &OsStr) -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    value.as_bytes().contains(&0)
}

#[cfg(unix)]
fn os_str_units(value: &OsStr) -> usize {
    use std::os::unix::ffi::OsStrExt as _;
    value.as_bytes().len()
}

#[cfg(windows)]
fn os_str_contains_nul(value: &OsStr) -> bool {
    use std::os::windows::ffi::OsStrExt as _;
    value.encode_wide().any(|unit| unit == 0)
}

#[cfg(windows)]
fn os_str_units(value: &OsStr) -> usize {
    use std::os::windows::ffi::OsStrExt as _;
    value.encode_wide().count()
}

#[cfg(windows)]
fn is_default_ffmpeg_name(configured: &Path) -> bool {
    let mut components = configured.components();
    let Some(Component::Normal(name)) = components.next() else {
        return false;
    };
    components.next().is_none()
        && name.to_str().is_some_and(|name| {
            name.eq_ignore_ascii_case("ffmpeg") || name.eq_ignore_ascii_case("ffmpeg.exe")
        })
}

#[cfg(not(windows))]
fn is_default_ffmpeg_name(configured: &Path) -> bool {
    configured == Path::new("ffmpeg")
}

#[cfg(windows)]
fn ffmpeg_search_leaf() -> &'static OsStr {
    OsStr::new("ffmpeg.exe")
}

#[cfg(not(windows))]
fn ffmpeg_search_leaf() -> &'static OsStr {
    OsStr::new("ffmpeg")
}

fn resolve_ffmpeg_candidate(
    candidate: PathBuf,
    searched: bool,
) -> Result<Option<FfmpegExecutable>, FfmpegLocatorError> {
    let canonical = match std::fs::canonicalize(&candidate) {
        Ok(canonical) => canonical,
        Err(err)
            if searched
                && matches!(
                    err.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
        {
            return Ok(None);
        }
        Err(err) => {
            return Err(FfmpegLocatorError::CandidateIo {
                candidate,
                operation: "canonicalize",
                err,
            });
        }
    };
    if !canonical.is_absolute() {
        return Err(FfmpegLocatorError::InvalidCanonicalIdentity {
            path: canonical,
            reason: "path is not absolute",
        });
    }
    if canonical.to_str().is_none() {
        return Err(FfmpegLocatorError::InvalidCanonicalIdentity {
            path: canonical,
            reason: "path is not valid UTF-8",
        });
    }
    if canonical
        .to_str()
        .is_some_and(|path| path.chars().any(char::is_control))
    {
        return Err(FfmpegLocatorError::InvalidCanonicalIdentity {
            path: canonical,
            reason: "path contains a control character",
        });
    }
    // Reject special files before open: opening a FIFO can block indefinitely.
    // Safe `std` cannot make the later pathname open atomic with this check, so
    // a same-identity actor can still race it; the opened handle is rechecked
    // before any header bytes are trusted.
    let preopen_metadata =
        std::fs::metadata(&canonical).map_err(|err| FfmpegLocatorError::CandidateIo {
            candidate: canonical.clone(),
            operation: "inspect",
            err,
        })?;
    if !preopen_metadata.is_file() || !metadata_is_executable(&preopen_metadata) {
        if searched {
            return Ok(None);
        }
        return Err(FfmpegLocatorError::NotExecutable { path: canonical });
    }

    // Metadata and the executable-container header below are derived from the
    // same opened file, so a race cannot mix those two observations.
    let mut file =
        std::fs::File::open(&canonical).map_err(|err| FfmpegLocatorError::CandidateIo {
            candidate: canonical.clone(),
            operation: "open",
            err,
        })?;
    let metadata = file
        .metadata()
        .map_err(|err| FfmpegLocatorError::CandidateIo {
            candidate: canonical.clone(),
            operation: "inspect",
            err,
        })?;
    if !metadata.is_file() || !metadata_is_executable(&metadata) {
        if searched {
            return Ok(None);
        }
        return Err(FfmpegLocatorError::NotExecutable { path: canonical });
    }
    if !is_host_native_image(&mut file, metadata.len(), &canonical)? {
        if searched {
            return Ok(None);
        }
        return Err(FfmpegLocatorError::UnsupportedExecutableFormat { path: canonical });
    }
    Ok(Some(FfmpegExecutable {
        canonical_path: canonical,
    }))
}

fn read_exact_or_short(
    file: &mut std::fs::File,
    buffer: &mut [u8],
    path: &Path,
) -> Result<bool, FfmpegLocatorError> {
    match file.read_exact(buffer) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => Ok(false),
        Err(err) => Err(FfmpegLocatorError::CandidateIo {
            candidate: path.to_path_buf(),
            operation: "read executable header",
            err,
        }),
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn is_host_native_image(
    file: &mut std::fs::File,
    _file_len: u64,
    path: &Path,
) -> Result<bool, FfmpegLocatorError> {
    let mut magic = [0_u8; 4];
    Ok(read_exact_or_short(file, &mut magic, path)? && magic == *b"\x7fELF")
}

#[cfg(target_os = "macos")]
fn is_host_native_image(
    file: &mut std::fs::File,
    _file_len: u64,
    path: &Path,
) -> Result<bool, FfmpegLocatorError> {
    let mut magic = [0_u8; 4];
    if !read_exact_or_short(file, &mut magic, path)? {
        return Ok(false);
    }
    Ok(matches!(
        magic,
        [0xfe, 0xed, 0xfa, 0xce]
            | [0xce, 0xfa, 0xed, 0xfe]
            | [0xfe, 0xed, 0xfa, 0xcf]
            | [0xcf, 0xfa, 0xed, 0xfe]
            | [0xca, 0xfe, 0xba, 0xbe]
            | [0xbe, 0xba, 0xfe, 0xca]
            | [0xca, 0xfe, 0xba, 0xbf]
            | [0xbf, 0xba, 0xfe, 0xca]
    ))
}

#[cfg(windows)]
fn is_host_native_image(
    file: &mut std::fs::File,
    file_len: u64,
    path: &Path,
) -> Result<bool, FfmpegLocatorError> {
    use std::io::Seek as _;

    const DOS_HEADER_BYTES: usize = 64;
    const MAX_PE_HEADER_OFFSET: u64 = 1024 * 1024;

    let mut dos_header = [0_u8; DOS_HEADER_BYTES];
    if !read_exact_or_short(file, &mut dos_header, path)? || dos_header[..2] != *b"MZ" {
        return Ok(false);
    }
    let pe_offset = u64::from(u32::from_le_bytes([
        dos_header[0x3c],
        dos_header[0x3d],
        dos_header[0x3e],
        dos_header[0x3f],
    ]));
    let Some(pe_end) = pe_offset.checked_add(4) else {
        return Ok(false);
    };
    if pe_offset > MAX_PE_HEADER_OFFSET || pe_end > file_len {
        return Ok(false);
    }
    file.seek(io::SeekFrom::Start(pe_offset))
        .map_err(|err| FfmpegLocatorError::CandidateIo {
            candidate: path.to_path_buf(),
            operation: "seek executable header",
            err,
        })?;
    let mut pe_signature = [0_u8; 4];
    Ok(read_exact_or_short(file, &mut pe_signature, path)?
        && matches!(pe_signature, [b'P', b'E', 0, 0]))
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    windows
)))]
fn is_host_native_image(
    _file: &mut std::fs::File,
    _file_len: u64,
    _path: &Path,
) -> Result<bool, FfmpegLocatorError> {
    Err(FfmpegLocatorError::NativeImageUnsupported {
        platform: std::env::consts::OS,
    })
}

#[cfg(unix)]
fn metadata_is_executable(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn metadata_is_executable(_metadata: &std::fs::Metadata) -> bool {
    true
}

/// A complete, self-contained description of one subprocess invocation.
#[derive(Clone, Debug)]
pub struct ProcessSpec {
    /// The program to execute (a path, never a shell string).
    pub program: PathBuf,
    /// The argument vector, passed verbatim.
    pub argv: Vec<String>,
    /// The child's entire environment: cleared, then exactly these pairs.
    pub env: Vec<(String, String)>,
    /// Working directory, or inherit.
    pub cwd: Option<PathBuf>,
    /// Bytes written to the child's stdin by [`ProcessRunner::run`] (then
    /// closed); `None` for a null stdin. [`ProcessRunner::start`] requires
    /// this field to be `None` and accepts ordered chunks through
    /// [`RunningProcess::write_stdin`].
    pub stdin: Option<Vec<u8>>,
    /// Wall-clock bound; on expiry the child is killed.
    pub timeout: Duration,
    /// Per-stream cap on captured stdout/stderr bytes; on overflow the child
    /// is killed with [`ProcessTermination::OutputLimitExceeded`].
    pub max_output_bytes: u64,
}

/// Why process supervision stopped.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProcessTermination {
    /// The child exited; an OS exit code may be unavailable.
    Exited(Option<i32>),
    /// The wall-clock bound expired.
    TimedOut,
    /// stdout or stderr exceeded its capture cap.
    OutputLimitExceeded,
    /// Cooperative cancellation won before exit was observed.
    Cancelled,
}

/// What happened when a spawned process finished (or was stopped).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ProcessOutcome {
    /// The authoritative terminal reason.
    pub termination: ProcessTermination,
    /// Captured stdout (up to the cap).
    pub stdout: Vec<u8>,
    /// Captured stderr (up to the cap).
    pub stderr: Vec<u8>,
}

impl ProcessOutcome {
    /// Whether the process ran to completion with exit code 0.
    #[must_use]
    pub fn success(&self) -> bool {
        self.termination == ProcessTermination::Exited(Some(0))
    }
}

/// Cooperative cancellation shared by a process session and its owner.
///
/// The std runner polls this token while the child is alive, including while
/// another thread is blocked feeding stdin. Cancelling therefore closes the
/// process side of a full pipe and unblocks bounded sink backpressure.
#[derive(Clone, Debug, Default)]
pub struct ProcessCancellation {
    cancelled: Arc<AtomicBool>,
}

impl ProcessCancellation {
    /// A fresh, live cancellation token.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Request cancellation. Repeated requests are idempotent.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    /// Whether cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

/// Bounds enforced by the process mechanism for incremental stdin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessStdinLimits {
    /// Largest accepted call to [`RunningProcess::write_stdin`].
    pub max_chunk_bytes: u64,
    /// Largest cumulative stdin payload.
    pub max_total_bytes: u64,
}

impl ProcessStdinLimits {
    /// Construct explicit stdin bounds.
    #[must_use]
    pub const fn new(max_chunk_bytes: u64, max_total_bytes: u64) -> Self {
        Self {
            max_chunk_bytes,
            max_total_bytes,
        }
    }
}

/// A process-mechanism failure (distinct from a process that ran and
/// failed, which is a [`ProcessOutcome`] with a nonzero code).
#[derive(Debug)]
pub enum ProcessError {
    /// The program could not be spawned at all.
    Spawn {
        /// The program that failed to spawn.
        program: PathBuf,
        /// The underlying error.
        err: std::io::Error,
    },
    /// I/O plumbing to the child failed mid-run.
    Plumbing {
        /// The program being run.
        program: PathBuf,
        /// What broke.
        detail: String,
    },
    /// A [`ScriptedRunner`] was asked for a program it has no script for.
    NotScripted {
        /// The unscripted program.
        program: PathBuf,
    },
    /// The spec's program path is not absolute. The mechanism refuses PATH
    /// resolution outright: the D2 boundary resolves its one tool to an
    /// absolute path (and content-hashes it into provenance) before any
    /// spawn, so an ambient `PATH` can never choose the executable.
    ProgramNotAbsolute {
        /// The offending program path.
        program: PathBuf,
    },
    /// The host target cannot provide process-tree cancellation through the
    /// pinned safe standard-library surface. D2 requires a refusal, not a
    /// direct-child-only downgrade.
    ProcessTreeCancellationUnsupported {
        /// The program that was not spawned.
        program: PathBuf,
    },
    /// A streaming start was supplied preloaded stdin bytes.
    ///
    /// Long-lived sessions receive input only through
    /// [`RunningProcess::write_stdin`], so accepting both channels would make
    /// byte order ambiguous.
    StreamingInputPreloaded {
        /// The program whose invalid specification was refused.
        program: PathBuf,
    },
    /// One stdin chunk exceeded the mechanism's declared bound.
    StdinChunkLimit {
        /// The program receiving input.
        program: PathBuf,
        /// Attempted bytes.
        attempted: u64,
        /// Configured maximum.
        max: u64,
    },
    /// Cumulative stdin exceeded the mechanism's declared bound.
    StdinTotalLimit {
        /// The program receiving input.
        program: PathBuf,
        /// Attempted bytes.
        attempted: u64,
        /// Configured maximum.
        max: u64,
    },
}

impl fmt::Display for ProcessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn { program, err } => {
                write!(f, "cannot spawn {}: {err}", program.display())
            }
            Self::Plumbing { program, detail } => {
                write!(f, "I/O plumbing to {} failed: {detail}", program.display())
            }
            Self::NotScripted { program } => {
                write!(f, "no scripted outcome for {}", program.display())
            }
            Self::ProgramNotAbsolute { program } => {
                write!(
                    f,
                    "program path {} is not absolute; the process capability \
                     refuses PATH resolution (D2: resolve and fingerprint the \
                     tool first)",
                    program.display()
                )
            }
            Self::ProcessTreeCancellationUnsupported { program } => write!(
                f,
                "cannot spawn {}: this target has no safe process-tree \
                 cancellation mechanism required by D2",
                program.display()
            ),
            Self::StreamingInputPreloaded { program } => write!(
                f,
                "streaming process {} must start without preloaded stdin bytes",
                program.display()
            ),
            Self::StdinChunkLimit {
                program,
                attempted,
                max,
            } => write!(
                f,
                "stdin chunk of {attempted} bytes for {} exceeds limit {max}",
                program.display()
            ),
            Self::StdinTotalLimit {
                program,
                attempted,
                max,
            } => write!(
                f,
                "stdin total of {attempted} bytes for {} exceeds limit {max}",
                program.display()
            ),
        }
    }
}

impl std::error::Error for ProcessError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn { err, .. } => Some(err),
            _ => None,
        }
    }
}

/// A started child process with bounded, backpressured stdin.
///
/// Implementations must enforce the start specification's timeout and output
/// caps for the whole session, not only while [`Self::finish`] is waiting.
/// Dropping a live session must cancel and reap it.
pub trait RunningProcess: Send {
    /// Write the next ordered stdin chunk, blocking when the child applies
    /// backpressure.
    ///
    /// # Errors
    /// [`ProcessError`] when the pipe or supervision mechanism fails.
    fn write_stdin(&mut self, bytes: &[u8]) -> Result<(), ProcessError>;

    /// Close stdin, wait for the bounded child, and return its outcome.
    ///
    /// # Errors
    /// [`ProcessError`] when process supervision fails.
    fn finish(self: Box<Self>) -> Result<ProcessOutcome, ProcessError>;

    /// Cancel, close stdin, and reap the child.
    ///
    /// # Errors
    /// [`ProcessError`] when process supervision fails.
    fn cancel(self: Box<Self>) -> Result<(), ProcessError>;
}

/// The process capability.
pub trait ProcessRunner: Send + Sync {
    /// Start a long-lived process whose stdin is supplied incrementally.
    ///
    /// `spec.stdin` must be `None`; the returned session is the only stdin
    /// channel. `cancellation` is polled for the complete child lifetime.
    ///
    /// # Errors
    /// [`ProcessError`] when the process cannot be started.
    fn start(
        &self,
        spec: &ProcessSpec,
        cancellation: ProcessCancellation,
        stdin_limits: ProcessStdinLimits,
    ) -> Result<Box<dyn RunningProcess>, ProcessError>;

    /// Run the process to completion under the spec's bounds.
    ///
    /// # Errors
    /// [`ProcessError`] when the mechanism itself fails; a process that
    /// runs and exits nonzero is an `Ok` outcome.
    fn run(&self, spec: &ProcessSpec) -> Result<ProcessOutcome, ProcessError> {
        let mut start_spec = spec.clone();
        let stdin = start_spec.stdin.take();
        let stdin_bytes = stdin.as_ref().map_or(0, Vec::len);
        let stdin_bytes = u64::try_from(stdin_bytes).unwrap_or(u64::MAX);
        let mut process = self.start(
            &start_spec,
            ProcessCancellation::new(),
            ProcessStdinLimits::new(stdin_bytes.max(1), stdin_bytes),
        )?;
        if let Some(bytes) = stdin
            && let Err(error) = process.write_stdin(&bytes)
        {
            let _ = process.cancel();
            return Err(error);
        }
        process.finish()
    }
}

/// How often the std runner polls the child while enforcing the timeout.
const POLL_INTERVAL: Duration = Duration::from_millis(5);

/// Every runner (std and scripted alike) refuses relative program paths:
/// the trait contract, not an implementation detail.
fn require_absolute(spec: &ProcessSpec) -> Result<(), ProcessError> {
    if spec.program.is_absolute() {
        Ok(())
    } else {
        Err(ProcessError::ProgramNotAbsolute {
            program: spec.program.clone(),
        })
    }
}

/// The host implementation over `std::process::Command`.
#[derive(Clone, Copy, Debug, Default)]
pub struct StdProcessRunner;

#[cfg(all(unix, not(target_os = "espidf")))]
fn configure_process_tree(
    command: &mut std::process::Command,
    _program: &std::path::Path,
) -> Result<(), ProcessError> {
    use std::os::unix::process::CommandExt as _;

    command.process_group(0);
    Ok(())
}

#[cfg(not(all(unix, not(target_os = "espidf"))))]
fn configure_process_tree(
    _command: &mut std::process::Command,
    program: &std::path::Path,
) -> Result<(), ProcessError> {
    Err(ProcessError::ProcessTreeCancellationUnsupported {
        program: program.to_path_buf(),
    })
}

#[cfg(all(unix, not(target_os = "espidf")))]
fn kill_process_tree(child: &mut std::process::Child) -> std::io::Result<()> {
    use std::os::unix::process::ChildExt as _;

    child.kill_process_group()
}

#[cfg(not(all(unix, not(target_os = "espidf"))))]
fn kill_process_tree(_child: &mut std::process::Child) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "safe process-tree cancellation is unavailable",
    ))
}

/// Drain one output pipe on its own thread, capturing up to `cap` bytes and
/// discarding the rest (so the child is never back-pressured into a pipe
/// deadlock). Sets `overflow` the moment the cap is exceeded — the poll loop
/// watches it and kills the child promptly. Returns the captured bytes.
fn drain(
    mut pipe: impl std::io::Read + Send + 'static,
    cap: u64,
    overflow: Arc<AtomicBool>,
) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let cap = usize::try_from(cap).unwrap_or(usize::MAX);
        let mut captured = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            match pipe.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let room = cap.saturating_sub(captured.len());
                    captured.extend_from_slice(&buf[..n.min(room)]);
                    if n > room {
                        overflow.store(true, Ordering::Relaxed);
                        // Keep reading (and discarding) until the kill
                        // closes the pipe.
                    }
                }
            }
        }
        captured
    })
}

impl ProcessRunner for StdProcessRunner {
    fn start(
        &self,
        spec: &ProcessSpec,
        cancellation: ProcessCancellation,
        stdin_limits: ProcessStdinLimits,
    ) -> Result<Box<dyn RunningProcess>, ProcessError> {
        require_absolute(spec)?;
        if spec.stdin.is_some() {
            return Err(ProcessError::StreamingInputPreloaded {
                program: spec.program.clone(),
            });
        }
        // The program is an absolute, caller-resolved path (checked above),
        // never a PATH lookup or user-composed string.
        // The trusted absolute executable capability is resolved and fingerprinted
        // by the ffmpeg boundary before it reaches this runner.
        let mut cmd = std::process::Command::new(&spec.program); // ubs:ignore
        cmd.args(&spec.argv)
            .env_clear()
            .envs(spec.env.iter().map(|(k, v)| (k, v)))
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .stdin(std::process::Stdio::piped());
        configure_process_tree(&mut cmd, &spec.program)?;
        if let Some(cwd) = &spec.cwd {
            cmd.current_dir(cwd);
        }
        let mut child = cmd.spawn().map_err(|err| ProcessError::Spawn {
            program: spec.program.clone(),
            err,
        })?;

        let plumbing = |detail: &str| ProcessError::Plumbing {
            program: spec.program.clone(),
            detail: detail.to_string(),
        };
        let Some(stdin) = child.stdin.take() else {
            let _ = kill_process_tree(&mut child);
            let _ = child.wait();
            return Err(plumbing("no stdin pipe"));
        };
        let overflow = Arc::new(AtomicBool::new(false));
        let Some(stdout) = child.stdout.take() else {
            let _ = kill_process_tree(&mut child);
            let _ = child.wait();
            return Err(plumbing("no stdout pipe"));
        };
        let Some(stderr) = child.stderr.take() else {
            let _ = kill_process_tree(&mut child);
            let _ = child.wait();
            return Err(plumbing("no stderr pipe"));
        };
        let stdout_thread = drain(stdout, spec.max_output_bytes, Arc::clone(&overflow));
        let stderr_thread = drain(stderr, spec.max_output_bytes, Arc::clone(&overflow));
        let program = spec.program.clone();
        let timeout = spec.timeout;
        let supervisor_cancellation = cancellation.clone();
        let (outcome_tx, outcome_rx) = mpsc::sync_channel(1);
        let supervisor = std::thread::spawn(move || {
            let result = supervise_child(
                child,
                &program,
                timeout,
                supervisor_cancellation,
                overflow,
                stdout_thread,
                stderr_thread,
            );
            let _ = outcome_tx.send(result);
        });
        Ok(Box::new(StdRunningProcess {
            program: spec.program.clone(),
            stdin: Some(stdin),
            outcome_rx,
            supervisor: Some(supervisor),
            cancellation,
            stdin_limits,
            stdin_bytes: 0,
            finished: false,
        }))
    }
}

fn supervise_child(
    mut child: std::process::Child,
    program: &std::path::Path,
    timeout: Duration,
    cancellation: ProcessCancellation,
    overflow: Arc<AtomicBool>,
    stdout_thread: std::thread::JoinHandle<Vec<u8>>,
    stderr_thread: std::thread::JoinHandle<Vec<u8>>,
) -> Result<ProcessOutcome, ProcessError> {
    let start = Instant::now();
    let termination = loop {
        // Reaping the group leader makes std's safe group-signal handle a
        // no-op. Once both inherited pipes close, kill the isolated group
        // before reaping. For an already-exited leader this preserves its
        // status while terminating redirected descendants; a leader that
        // closed both supervision pipes early is failed closed.
        if stdout_thread.is_finished() && stderr_thread.is_finished() {
            match kill_process_tree(&mut child) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    // No member remains in the isolated group.
                }
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(ProcessError::Plumbing {
                        program: program.to_path_buf(),
                        detail: format!("process-tree completion kill failed: {error}"),
                    });
                }
            }
            match child.try_wait() {
                Ok(Some(status)) => break ProcessTermination::Exited(status.code()),
                Ok(None) => {}
                Err(error) => {
                    let _ = kill_process_tree(&mut child);
                    let _ = child.wait();
                    return Err(ProcessError::Plumbing {
                        program: program.to_path_buf(),
                        detail: format!(
                            "try_wait after process-tree completion kill failed: {error}"
                        ),
                    });
                }
            }
        }
        let terminal = if overflow.load(Ordering::Acquire) {
            Some(ProcessTermination::OutputLimitExceeded)
        } else if cancellation.is_cancelled() {
            Some(ProcessTermination::Cancelled)
        } else if start.elapsed() >= timeout {
            Some(ProcessTermination::TimedOut)
        } else {
            None
        };
        if let Some(terminal) = terminal {
            if let Err(err) = kill_process_tree(&mut child) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ProcessError::Plumbing {
                    program: program.to_path_buf(),
                    detail: format!("process-tree kill failed: {err}"),
                });
            }
            break terminal;
        }
        std::thread::sleep(POLL_INTERVAL);
    };
    // Reap after a terminal kill so no zombie outlives the session. A
    // pipe-closed completion observed through `try_wait` was already reaped.
    if !matches!(termination, ProcessTermination::Exited(_)) {
        let _ = child.wait();
    }
    let plumbing = |detail: &str| ProcessError::Plumbing {
        program: program.to_path_buf(),
        detail: detail.to_string(),
    };
    let stdout = stdout_thread
        .join()
        .map_err(|_| plumbing("stdout drain panicked"))?;
    let stderr = stderr_thread
        .join()
        .map_err(|_| plumbing("stderr drain panicked"))?;
    // A cap can also trip between process exit and the final drain. Output
    // overflow is authoritative even when an exit was observed first.
    let termination = if overflow.load(Ordering::Acquire) {
        ProcessTermination::OutputLimitExceeded
    } else {
        termination
    };
    Ok(ProcessOutcome {
        termination,
        stdout,
        stderr,
    })
}

struct StdRunningProcess {
    program: PathBuf,
    stdin: Option<std::process::ChildStdin>,
    outcome_rx: mpsc::Receiver<Result<ProcessOutcome, ProcessError>>,
    supervisor: Option<std::thread::JoinHandle<()>>,
    cancellation: ProcessCancellation,
    stdin_limits: ProcessStdinLimits,
    stdin_bytes: u64,
    finished: bool,
}

impl StdRunningProcess {
    fn wait(&mut self) -> Result<ProcessOutcome, ProcessError> {
        self.stdin.take();
        let outcome = self.outcome_rx.recv().map_err(|_| ProcessError::Plumbing {
            program: self.program.clone(),
            detail: "process supervisor exited without an outcome".to_string(),
        })?;
        if let Some(supervisor) = self.supervisor.take() {
            supervisor.join().map_err(|_| ProcessError::Plumbing {
                program: self.program.clone(),
                detail: "process supervisor panicked".to_string(),
            })?;
        }
        self.finished = true;
        outcome
    }
}

impl RunningProcess for StdRunningProcess {
    fn write_stdin(&mut self, bytes: &[u8]) -> Result<(), ProcessError> {
        if self.cancellation.is_cancelled() {
            return Err(ProcessError::Plumbing {
                program: self.program.clone(),
                detail: "process was cancelled before stdin write".to_string(),
            });
        }
        let chunk_bytes = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if chunk_bytes > self.stdin_limits.max_chunk_bytes {
            return Err(ProcessError::StdinChunkLimit {
                program: self.program.clone(),
                attempted: chunk_bytes,
                max: self.stdin_limits.max_chunk_bytes,
            });
        }
        let attempted =
            self.stdin_bytes
                .checked_add(chunk_bytes)
                .ok_or(ProcessError::StdinTotalLimit {
                    program: self.program.clone(),
                    attempted: u64::MAX,
                    max: self.stdin_limits.max_total_bytes,
                })?;
        if attempted > self.stdin_limits.max_total_bytes {
            return Err(ProcessError::StdinTotalLimit {
                program: self.program.clone(),
                attempted,
                max: self.stdin_limits.max_total_bytes,
            });
        }
        self.stdin
            .as_mut()
            .ok_or_else(|| ProcessError::Plumbing {
                program: self.program.clone(),
                detail: "stdin is already closed".to_string(),
            })?
            .write_all(bytes)
            .map_err(|error| ProcessError::Plumbing {
                program: self.program.clone(),
                detail: format!("stdin write failed: {error}"),
            })?;
        self.stdin_bytes = attempted;
        Ok(())
    }

    fn finish(mut self: Box<Self>) -> Result<ProcessOutcome, ProcessError> {
        self.wait()
    }

    fn cancel(mut self: Box<Self>) -> Result<(), ProcessError> {
        self.cancellation.cancel();
        self.wait().map(|_| ())
    }
}

impl Drop for StdRunningProcess {
    fn drop(&mut self) {
        if !self.finished {
            self.cancellation.cancel();
            let _ = self.wait();
        }
    }
}

/// The test double: canned outcomes per program path, with a full log of
/// every spec it was asked to run.
#[derive(Debug, Default)]
pub struct ScriptedRunner {
    scripts: BTreeMap<PathBuf, ProcessOutcome>,
    runs: Arc<Mutex<Vec<ProcessSpec>>>,
}

impl ScriptedRunner {
    /// An empty scripted runner (every run is [`ProcessError::NotScripted`]).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Script the outcome for a program path.
    pub fn script(&mut self, program: impl Into<PathBuf>, outcome: ProcessOutcome) {
        self.scripts.insert(program.into(), outcome);
    }

    /// Every spec run so far, in order.
    #[must_use]
    pub fn runs(&self) -> Vec<ProcessSpec> {
        self.runs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl ProcessRunner for ScriptedRunner {
    fn start(
        &self,
        spec: &ProcessSpec,
        cancellation: ProcessCancellation,
        stdin_limits: ProcessStdinLimits,
    ) -> Result<Box<dyn RunningProcess>, ProcessError> {
        require_absolute(spec)?;
        if spec.stdin.is_some() {
            return Err(ProcessError::StreamingInputPreloaded {
                program: spec.program.clone(),
            });
        }
        let outcome =
            self.scripts
                .get(&spec.program)
                .cloned()
                .ok_or_else(|| ProcessError::NotScripted {
                    program: spec.program.clone(),
                })?;
        Ok(Box::new(ScriptedProcess {
            spec: spec.clone(),
            input: Vec::new(),
            outcome,
            runs: Arc::clone(&self.runs),
            cancellation,
            stdin_limits,
            stdin_bytes: 0,
            finished: false,
        }))
    }
}

struct ScriptedProcess {
    spec: ProcessSpec,
    input: Vec<u8>,
    outcome: ProcessOutcome,
    runs: Arc<Mutex<Vec<ProcessSpec>>>,
    cancellation: ProcessCancellation,
    stdin_limits: ProcessStdinLimits,
    stdin_bytes: u64,
    finished: bool,
}

impl ScriptedProcess {
    fn record(&mut self) {
        if self.finished {
            return;
        }
        if !self.input.is_empty() {
            self.spec.stdin = Some(std::mem::take(&mut self.input));
        }
        self.runs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(self.spec.clone());
        self.finished = true;
    }
}

impl RunningProcess for ScriptedProcess {
    fn write_stdin(&mut self, bytes: &[u8]) -> Result<(), ProcessError> {
        if self.cancellation.is_cancelled() {
            return Err(ProcessError::Plumbing {
                program: self.spec.program.clone(),
                detail: "scripted process was cancelled before stdin write".to_string(),
            });
        }
        let chunk_bytes = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if chunk_bytes > self.stdin_limits.max_chunk_bytes {
            return Err(ProcessError::StdinChunkLimit {
                program: self.spec.program.clone(),
                attempted: chunk_bytes,
                max: self.stdin_limits.max_chunk_bytes,
            });
        }
        let attempted =
            self.stdin_bytes
                .checked_add(chunk_bytes)
                .ok_or(ProcessError::StdinTotalLimit {
                    program: self.spec.program.clone(),
                    attempted: u64::MAX,
                    max: self.stdin_limits.max_total_bytes,
                })?;
        if attempted > self.stdin_limits.max_total_bytes {
            return Err(ProcessError::StdinTotalLimit {
                program: self.spec.program.clone(),
                attempted,
                max: self.stdin_limits.max_total_bytes,
            });
        }
        self.input.extend_from_slice(bytes);
        self.stdin_bytes = attempted;
        Ok(())
    }

    fn finish(mut self: Box<Self>) -> Result<ProcessOutcome, ProcessError> {
        self.record();
        let mut outcome = self.outcome.clone();
        if self.cancellation.is_cancelled() {
            outcome.termination = ProcessTermination::Cancelled;
        }
        Ok(outcome)
    }

    fn cancel(mut self: Box<Self>) -> Result<(), ProcessError> {
        self.cancellation.cancel();
        self.record();
        Ok(())
    }
}

impl Drop for ScriptedProcess {
    fn drop(&mut self) {
        self.record();
    }
}
