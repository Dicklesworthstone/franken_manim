//! The no-corpus-leak enforcement check (fm-aef, §15.3): the release-CI
//! gate that fails loudly if a CC BY-NC-SA private fixture ever lands in
//! the shippable set.
//!
//! **The private fixtures** (never shipped, never committed): the harvested
//! 3b1b TeX-string corpus (`corpus/`), the Look Gallery Reference captures
//! (`gallery/reference_captures/`), and the two local Reference checkouts
//! used for fixture generation (`scripts/manim_ref/`, `scripts/videos_ref/`).
//! What *is* public: `docs/g0/g0-4-corpus/` (hashes, counts, construct
//! names — never the strings) and the `docs/g0/*-renders/` directories,
//! which carry the engine's **own** renders (`fmn-*.png`, fmd-math layout
//! SVGs, G0-8 engine-comparison PNGs) — verified by this gate to contain
//! no Reference-capture bytes.
//!
//! **The mechanism**, three independent teeth plus one hermetic oracle:
//!
//! 1. **Path tooth** — `git ls-files` (ground truth for what packaging
//!    commits) plus the `dist/` and wheel/npm staging trees must contain
//!    no path under a private-fixture directory.
//! 2. **Gitignore tooth** — every private-fixture directory must be
//!    reported ignored by `git check-ignore` (not merely believed to be;
//!    the gate runs the real matcher against the real `.gitignore`).
//! 3. **Content tooth** — the committed `denominator.tsv` carries
//!    `sha256(mode + NUL + string)` for every corpus string (the public
//!    artifact of the private harvest). Every text file on the shippable
//!    surface is scanned line-wise under the same hash convention — via
//!    fmn-hash's owned SHA-256, the same bytes the harvest recorded — so a
//!    copied corpus string is caught *without the corpus present in CI*.
//!    Where the private fixtures exist on disk (a developer machine),
//!    their whole-file digests are also collected and any byte-identical
//!    file on the surface is flagged (this catches copied Reference
//!    capture PNGs).
//!
//! The line scan hashes each line in three forms — raw, trimmed, and
//! trimmed with one layer of surrounding quotes plus a trailing comma or
//! semicolon stripped — under both harvest modes (`math`, `text`), so the
//! realistic leak forms (a copied `.jsonl`/`.tsv` fixture, a copied string
//! literal on its own line) are caught exactly. A corpus string
//! paraphrased or re-wrapped mid-line is out of scope by construction;
//! the path and whole-file teeth cover the artifact-level forms.
//!
//! The pure kernels (`path_violations`, `content_leaks`, …) take their
//! inputs as values so the negative tests need no filesystem fixtures;
//! [`run`] is the release-CI orchestration over a repository checkout and
//! fails with a named error if `git` is unavailable — this gate only makes
//! sense against the real tree, and a silent skip would be a hole.

use fmn_hash::{Digest, Sha256};
use std::collections::HashSet;
use std::fmt;
use std::fs::{File, Metadata};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::SystemTime;

/// The private-fixture directories, repository-relative, always with a
/// trailing slash for prefix matching (§15.3).
pub const PRIVATE_FIXTURE_DIRS: &[&str] = &[
    "corpus/",
    "gallery/reference_captures/",
    "scripts/manim_ref/",
    "scripts/videos_ref/",
];

/// The distributable-surface staging trees scanned in addition to the
/// git-tracked set: `dist/` (the bundle staging this bead introduces) and
/// the wheel/npm staging locations as they appear. Missing trees are
/// skipped; present ones are scanned whether or not they are tracked.
pub const STAGING_DIRS: &[&str] = &["dist", "crates/fmn-python/dist", "wasm-smoke/pkg", "npm"];

/// The committed public artifact of the private harvest: per-string
/// content hashes, never the strings (§15.3).
pub const DENOMINATOR_PATH: &str = "docs/g0/g0-4-corpus/denominator.tsv";

/// The harvest modes the denominator hashes under.
pub const CORPUS_MODES: &[&str] = &["math", "text"];

/// The minimum line-variant length the content tooth hashes. The corpus
/// holds thousands of trivially short strings (`"!"`, `" = "`, `"}"`,
/// even the empty string — harvest fragments of multi-argument `Tex()`
/// call sites); their hashes are structurally undetectable without
/// flagging ordinary source lines (every `}` line is such a fragment).
/// The NC-licensed substance — the authored strings a fixture leak would
/// actually copy — is long-form, so variants below this length are not
/// hashed. The path and whole-file teeth are length-blind and cover the
/// artifact-level leak forms regardless.
pub const MIN_PREIMAGE_LEN: usize = 16;

/// Longest repository-relative path accepted from Git or a staging tree.
pub const MAX_PATH_BYTES: usize = 4 * 1024;
/// Maximum NUL-delimited byte stream accepted from `git ls-files`.
pub const MAX_GIT_PATH_OUTPUT_BYTES: usize = 64 * 1024 * 1024;
/// Maximum number of distinct files on the distributable surface.
pub const MAX_SURFACE_FILES: usize = 100_000;
/// Maximum directory entries visited across optional private/staging roots.
pub const MAX_TRAVERSAL_ENTRIES: usize = 250_000;
/// Maximum bytes accepted from one scanned file. Scanning is streaming, but
/// time and I/O still need a finite release-gate budget.
pub const MAX_SCANNED_FILE_BYTES: u64 = 1024 * 1024 * 1024;
/// Maximum aggregate bytes read from one scan class (private or surface).
pub const MAX_TOTAL_SCAN_BYTES: u64 = 16 * 1024 * 1024 * 1024;
/// Maximum findings retained before the gate refuses a potentially hostile
/// surface instead of allocating an unbounded report.
pub const MAX_FINDINGS: usize = 4_096;
/// Maximum text-line size eligible for corpus-preimage matching.
pub const MAX_LINE_BYTES: usize = 4_096;

const IO_CHUNK_BYTES: usize = 64 * 1024;

/// One leak finding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Leak {
    /// Which tooth found it.
    pub kind: LeakKind,
    /// The offending repository-relative path.
    pub path: String,
    /// Human-readable detail (line number, matched form).
    pub detail: String,
}

/// The leak kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeakKind {
    /// A private-fixture path is present in the shippable set.
    PrivatePathShipped,
    /// A private-fixture directory is NOT excluded by `.gitignore`.
    PrivateDirNotIgnored,
    /// A file on the surface contains a corpus string (denominator hash
    /// match).
    CorpusStringShipped,
    /// A file on the surface is byte-identical to a private-fixture file.
    PrivateFileBytesShipped,
}

impl fmt::Display for Leak {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self.kind {
            LeakKind::PrivatePathShipped => "private-fixture path shipped",
            LeakKind::PrivateDirNotIgnored => "private-fixture directory not gitignored",
            LeakKind::CorpusStringShipped => "CC BY-NC-SA corpus string shipped",
            LeakKind::PrivateFileBytesShipped => "private-fixture bytes shipped",
        };
        write!(f, "{kind}: {} ({})", self.path, self.detail)
    }
}

/// A gate-level failure (as opposed to a finding): the check itself could
/// not run to completion.
#[derive(Debug)]
pub enum LeakError {
    /// `git` could not be run or reported failure. The gate refuses to
    /// pass without ground truth.
    Git { what: String },
    /// A required file could not be read.
    Io { path: String, what: String },
    /// A path or file type violates the closed release-surface policy.
    Policy { path: String, what: String },
    /// A finite resource budget was exceeded.
    Resource { what: String },
}

impl fmt::Display for LeakError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Git { what } => write!(f, "git ground truth unavailable: {what}"),
            Self::Io { path, what } => write!(f, "reading {path}: {what}"),
            Self::Policy { path, what } => write!(f, "surface policy for {path}: {what}"),
            Self::Resource { what } => write!(f, "no-corpus-leak resource limit: {what}"),
        }
    }
}

impl std::error::Error for LeakError {}

/// The corpus string hash convention, exactly as the G0-4 harvest recorded
/// it (`docs/g0/g0-4-corpus/denominator.tsv`): `sha256(mode + NUL + string)`.
#[must_use]
pub fn corpus_hash(mode: &str, string: &[u8]) -> Digest {
    let mut hasher = Sha256::new();
    hasher.update(mode.as_bytes());
    hasher.update(&[0]);
    hasher.update(string);
    hasher.finalize()
}

/// Parses the denominator's first (hash) column into digests. Comment
/// (`#`) and blank lines are skipped; a malformed row is a named error —
/// the oracle must be whole.
pub fn parse_denominator(text: &str) -> Result<Vec<Digest>, LeakError> {
    let mut digests = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let hex = line.split('\t').next().unwrap_or("");
        digests.push(Digest::from_hex(hex).map_err(|e| LeakError::Io {
            path: DENOMINATOR_PATH.to_owned(),
            what: format!("row {index}: malformed hash '{hex}': {e}"),
        })?);
    }
    Ok(digests)
}

/// Tooth 1: every repository-relative path that falls under a
/// private-fixture directory.
#[must_use]
pub fn path_violations(rel_paths: &[String]) -> Vec<Leak> {
    let mut leaks = Vec::new();
    for path in rel_paths {
        let normalized = path.replace('\\', "/");
        for dir in PRIVATE_FIXTURE_DIRS {
            let dir = dir.trim_end_matches('/');
            if normalized == dir || normalized.starts_with(&format!("{dir}/")) {
                leaks.push(Leak {
                    kind: LeakKind::PrivatePathShipped,
                    path: normalized.clone(),
                    detail: format!("under private-fixture directory '{dir}/' (§15.3)"),
                });
            }
        }
    }
    leaks
}

/// The candidate byte forms of one text line the content tooth hashes: the
/// raw line, the trimmed line, and the trimmed line with one layer of
/// surrounding single/double quotes plus a trailing `,`/`;` stripped (the
/// string-literal-on-its-own-line form).
#[must_use]
pub fn line_variants(line: &str) -> Vec<&str> {
    let mut variants = vec![line];
    let trimmed = line.trim();
    if trimmed != line {
        variants.push(trimmed);
    }
    let body = trimmed
        .strip_suffix(',')
        .or_else(|| trimmed.strip_suffix(';'))
        .unwrap_or(trimmed);
    let unquoted = body
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .or_else(|| {
            body.strip_prefix('\'')
                .and_then(|rest| rest.strip_suffix('\''))
        });
    if let Some(unquoted) = unquoted
        && unquoted != trimmed
        && unquoted != line
    {
        variants.push(unquoted);
    }
    variants
}

/// Tooth 3 (content): the leaks inside one surface file. `corpus` is the
/// denominator digest set; `private_files` is the whole-file digest set of
/// any private fixtures present on disk (empty where they are absent).
#[must_use]
pub fn content_leaks(
    rel_path: &str,
    bytes: &[u8],
    corpus: &HashSet<Digest>,
    private_files: &HashSet<Digest>,
) -> Vec<Leak> {
    let mut leaks = Vec::new();
    if !bytes.is_empty() && private_files.contains(&fmn_hash::sha256(bytes)) {
        leaks.push(Leak {
            kind: LeakKind::PrivateFileBytesShipped,
            path: rel_path.to_owned(),
            detail: "byte-identical to a private fixture on disk".to_owned(),
        });
    }
    let Ok(text) = std::str::from_utf8(bytes) else {
        return leaks;
    };
    for (index, line) in text.lines().enumerate() {
        if line.len() > 4096 {
            continue;
        }
        for variant in line_variants(line) {
            if variant.len() < MIN_PREIMAGE_LEN {
                continue;
            }
            for mode in CORPUS_MODES {
                if corpus.contains(&corpus_hash(mode, variant.as_bytes())) {
                    leaks.push(Leak {
                        kind: LeakKind::CorpusStringShipped,
                        path: rel_path.to_owned(),
                        detail: format!(
                            "line {} matches a denominator corpus hash (mode {mode})",
                            index + 1
                        ),
                    });
                }
            }
        }
    }
    leaks
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FileSnapshot {
    len: u64,
    modified: SystemTime,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    ctime_seconds: i64,
    #[cfg(unix)]
    ctime_nanoseconds: i64,
    #[cfg(windows)]
    volume_serial_number: Option<u32>,
    #[cfg(windows)]
    file_index: Option<u64>,
    #[cfg(windows)]
    last_write_time: u64,
}

impl FileSnapshot {
    fn from_metadata(metadata: &Metadata, label: &str) -> Result<Self, LeakError> {
        let modified = metadata.modified().map_err(|error| LeakError::Io {
            path: label.to_owned(),
            what: format!("reading modification identity: {error}"),
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            Ok(Self {
                len: metadata.len(),
                modified,
                device: metadata.dev(),
                inode: metadata.ino(),
                ctime_seconds: metadata.ctime(),
                ctime_nanoseconds: metadata.ctime_nsec(),
            })
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt as _;
            Ok(Self {
                len: metadata.len(),
                modified,
                volume_serial_number: metadata.volume_serial_number(),
                file_index: metadata.file_index(),
                last_write_time: metadata.last_write_time(),
            })
        }
        #[cfg(not(any(unix, windows)))]
        {
            Ok(Self {
                len: metadata.len(),
                modified,
            })
        }
    }
}

#[derive(Debug)]
struct ScanBudget {
    label: &'static str,
    files: usize,
    bytes: u64,
    max_files: usize,
}

impl ScanBudget {
    fn new(label: &'static str, max_files: usize) -> Self {
        Self {
            label,
            files: 0,
            bytes: 0,
            max_files,
        }
    }

    fn admit(&mut self, path: &str, len: u64) -> Result<(), LeakError> {
        if len > MAX_SCANNED_FILE_BYTES {
            return Err(LeakError::Resource {
                what: format!(
                    "{} file {path} declares {len} bytes, above the per-file limit {MAX_SCANNED_FILE_BYTES}",
                    self.label
                ),
            });
        }
        let files = self.files.checked_add(1).ok_or_else(|| LeakError::Resource {
            what: format!("{} file counter overflow", self.label),
        })?;
        if files > self.max_files {
            return Err(LeakError::Resource {
                what: format!(
                    "{} file count {files} exceeds limit {}",
                    self.label, self.max_files
                ),
            });
        }
        let bytes = self
            .bytes
            .checked_add(len)
            .ok_or_else(|| LeakError::Resource {
                what: format!("{} byte counter overflow", self.label),
            })?;
        if bytes > MAX_TOTAL_SCAN_BYTES {
            return Err(LeakError::Resource {
                what: format!(
                    "{} declared bytes {bytes} exceed aggregate limit {MAX_TOTAL_SCAN_BYTES}",
                    self.label
                ),
            });
        }
        self.files = files;
        self.bytes = bytes;
        Ok(())
    }
}

fn validate_relative_path(path: &str) -> Result<(), LeakError> {
    if path.is_empty() {
        return Err(LeakError::Policy {
            path: "<empty>".to_owned(),
            what: "repository-relative paths must not be empty".to_owned(),
        });
    }
    if path.len() > MAX_PATH_BYTES {
        return Err(LeakError::Resource {
            what: format!(
                "path has {} bytes, above the per-path limit {MAX_PATH_BYTES}",
                path.len()
            ),
        });
    }
    if path.chars().any(char::is_control) {
        return Err(LeakError::Policy {
            path: path.escape_default().to_string(),
            what: "control characters are not accepted in release-surface paths".to_owned(),
        });
    }
    for component in Path::new(path).components() {
        if !matches!(component, std::path::Component::Normal(_)) {
            return Err(LeakError::Policy {
                path: path.to_owned(),
                what: "path must contain only normal relative components".to_owned(),
            });
        }
    }
    Ok(())
}

fn relative_utf8(root: &Path, path: &Path) -> Result<String, LeakError> {
    let relative = path.strip_prefix(root).map_err(|error| LeakError::Policy {
        path: path.display().to_string(),
        what: format!("path escaped repository root: {error}"),
    })?;
    let relative = relative.to_str().ok_or_else(|| LeakError::Policy {
        path: path.display().to_string(),
        what: "non-UTF-8 release-surface paths are refused".to_owned(),
    })?;
    let normalized = relative.replace('\\', "/");
    validate_relative_path(&normalized)?;
    Ok(normalized)
}

fn walk_optional_roots<F>(
    root: &Path,
    relative_roots: &[&str],
    mut visit_file: F,
) -> Result<(), LeakError>
where
    F: FnMut(&Path, &str) -> Result<(), LeakError>,
{
    let mut visited = 0_usize;
    for relative_root in relative_roots {
        let root_path = root.join(relative_root);
        let root_metadata = match std::fs::symlink_metadata(&root_path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(LeakError::Io {
                    path: relative_root.to_string(),
                    what: format!("inspecting optional root: {error}"),
                });
            }
        };
        if !root_metadata.file_type().is_dir() {
            return Err(LeakError::Policy {
                path: relative_root.to_string(),
                what: "present optional root must be a real directory, not a symlink or special entry"
                    .to_owned(),
            });
        }

        let mut stack = vec![root_path];
        while let Some(directory) = stack.pop() {
            let label = relative_utf8(root, &directory)?;
            let metadata = std::fs::symlink_metadata(&directory).map_err(|error| {
                LeakError::Io {
                    path: label.clone(),
                    what: format!("revalidating traversed directory: {error}"),
                }
            })?;
            if !metadata.file_type().is_dir() {
                return Err(LeakError::Policy {
                    path: label,
                    what: "traversed directory changed type or became a symlink".to_owned(),
                });
            }
            let entries = std::fs::read_dir(&directory).map_err(|error| LeakError::Io {
                path: label,
                what: format!("reading present directory: {error}"),
            })?;
            for entry in entries {
                let entry = entry.map_err(|error| LeakError::Io {
                    path: directory.display().to_string(),
                    what: format!("reading directory entry: {error}"),
                })?;
                visited = visited
                    .checked_add(1)
                    .ok_or_else(|| LeakError::Resource {
                        what: "traversal entry counter overflow".to_owned(),
                    })?;
                if visited > MAX_TRAVERSAL_ENTRIES {
                    return Err(LeakError::Resource {
                        what: format!(
                            "optional-root traversal visited {visited} entries, above limit {MAX_TRAVERSAL_ENTRIES}"
                        ),
                    });
                }
                let path = entry.path();
                let relative = relative_utf8(root, &path)?;
                let file_type = entry.file_type().map_err(|error| LeakError::Io {
                    path: relative.clone(),
                    what: format!("reading directory-entry type: {error}"),
                })?;
                if file_type.is_symlink() {
                    return Err(LeakError::Policy {
                        path: relative,
                        what: "symlinks are refused on private and staging scan surfaces"
                            .to_owned(),
                    });
                }
                if file_type.is_dir() {
                    stack.push(path);
                } else if file_type.is_file() {
                    visit_file(&path, &relative)?;
                } else {
                    return Err(LeakError::Policy {
                        path: relative,
                        what: "only regular files and real directories are accepted".to_owned(),
                    });
                }
            }
        }
    }
    Ok(())
}

fn open_regular_file(
    path: &Path,
    label: &str,
    budget: &mut ScanBudget,
) -> Result<(File, FileSnapshot), LeakError> {
    let path_metadata = std::fs::symlink_metadata(path).map_err(|error| LeakError::Io {
        path: label.to_owned(),
        what: format!("opening enumerated file: {error}"),
    })?;
    if !path_metadata.file_type().is_file() {
        return Err(LeakError::Policy {
            path: label.to_owned(),
            what: "enumerated surface entry is not a regular file".to_owned(),
        });
    }
    let path_snapshot = FileSnapshot::from_metadata(&path_metadata, label)?;
    budget.admit(label, path_snapshot.len)?;
    let file = File::open(path).map_err(|error| LeakError::Io {
        path: label.to_owned(),
        what: format!("opening enumerated regular file: {error}"),
    })?;
    let handle_metadata = file.metadata().map_err(|error| LeakError::Io {
        path: label.to_owned(),
        what: format!("reading opened-file identity: {error}"),
    })?;
    if !handle_metadata.file_type().is_file() {
        return Err(LeakError::Policy {
            path: label.to_owned(),
            what: "opened surface handle is not a regular file".to_owned(),
        });
    }
    let handle_snapshot = FileSnapshot::from_metadata(&handle_metadata, label)?;
    if path_snapshot != handle_snapshot {
        return Err(LeakError::Policy {
            path: label.to_owned(),
            what: "file identity changed between enumeration and open".to_owned(),
        });
    }
    Ok((file, handle_snapshot))
}

fn validate_regular_file_postflight(
    path: &Path,
    label: &str,
    file: &File,
    expected: &FileSnapshot,
    bytes_read: u64,
) -> Result<(), LeakError> {
    if bytes_read != expected.len {
        return Err(LeakError::Policy {
            path: label.to_owned(),
            what: format!(
                "stream read {bytes_read} bytes but preflight declared {}",
                expected.len
            ),
        });
    }
    let handle_after = file.metadata().map_err(|error| LeakError::Io {
        path: label.to_owned(),
        what: format!("reading postflight handle identity: {error}"),
    })?;
    let handle_after = FileSnapshot::from_metadata(&handle_after, label)?;
    if &handle_after != expected {
        return Err(LeakError::Policy {
            path: label.to_owned(),
            what: "file changed while it was being scanned".to_owned(),
        });
    }
    let path_after = std::fs::symlink_metadata(path).map_err(|error| LeakError::Io {
        path: label.to_owned(),
        what: format!("revalidating scanned path: {error}"),
    })?;
    if !path_after.file_type().is_file() {
        return Err(LeakError::Policy {
            path: label.to_owned(),
            what: "scanned path changed type or became a symlink".to_owned(),
        });
    }
    let path_after = FileSnapshot::from_metadata(&path_after, label)?;
    if &path_after != expected {
        return Err(LeakError::Policy {
            path: label.to_owned(),
            what: "scanned path was replaced during the gate".to_owned(),
        });
    }
    Ok(())
}

fn stream_digest<R: Read>(reader: &mut R, label: &str) -> Result<(Digest, u64), LeakError> {
    let mut hasher = Sha256::new();
    let mut bytes_read = 0_u64;
    let mut chunk = [0_u8; IO_CHUNK_BYTES];
    loop {
        let count = reader.read(&mut chunk).map_err(|error| LeakError::Io {
            path: label.to_owned(),
            what: format!("streaming file contents: {error}"),
        })?;
        if count == 0 {
            break;
        }
        bytes_read = bytes_read
            .checked_add(u64::try_from(count).map_err(|error| LeakError::Resource {
                what: format!("read-size conversion failed: {error}"),
            })?)
            .ok_or_else(|| LeakError::Resource {
                what: format!("byte counter overflow while scanning {label}"),
            })?;
        if bytes_read > MAX_SCANNED_FILE_BYTES {
            return Err(LeakError::Resource {
                what: format!(
                    "streamed bytes for {label} exceed per-file limit {MAX_SCANNED_FILE_BYTES}"
                ),
            });
        }
        hasher.update(&chunk[..count]);
    }
    Ok((hasher.finalize(), bytes_read))
}

fn push_finding(findings: &mut Vec<Leak>, leak: Leak) -> Result<(), LeakError> {
    if findings.len() == MAX_FINDINGS {
        return Err(LeakError::Resource {
            what: format!("finding count exceeds limit {MAX_FINDINGS}"),
        });
    }
    findings.push(leak);
    Ok(())
}

fn extend_findings(
    findings: &mut Vec<Leak>,
    additions: impl IntoIterator<Item = Leak>,
) -> Result<(), LeakError> {
    for leak in additions {
        push_finding(findings, leak)?;
    }
    Ok(())
}

fn line_leaks(
    rel_path: &str,
    line_number: usize,
    bytes: &[u8],
    corpus: &HashSet<Digest>,
    findings: &mut Vec<Leak>,
) -> Result<(), LeakError> {
    let bytes = bytes.strip_suffix(b"\r").unwrap_or(bytes);
    let Ok(line) = std::str::from_utf8(bytes) else {
        return Ok(());
    };
    for variant in line_variants(line) {
        if variant.len() < MIN_PREIMAGE_LEN {
            continue;
        }
        for mode in CORPUS_MODES {
            if corpus.contains(&corpus_hash(mode, variant.as_bytes())) {
                push_finding(
                    findings,
                    Leak {
                        kind: LeakKind::CorpusStringShipped,
                        path: rel_path.to_owned(),
                        detail: format!(
                            "line {line_number} matches a denominator corpus hash (mode {mode})"
                        ),
                    },
                )?;
            }
        }
    }
    Ok(())
}

fn stream_surface<R: Read>(
    reader: &mut R,
    rel_path: &str,
    corpus: &HashSet<Digest>,
) -> Result<(Digest, u64, Vec<Leak>), LeakError> {
    let mut hasher = Sha256::new();
    let mut bytes_read = 0_u64;
    let mut chunk = [0_u8; IO_CHUNK_BYTES];
    let mut line = Vec::with_capacity(MAX_LINE_BYTES);
    let mut overlong_line = false;
    let mut line_number = 1_usize;
    let mut findings = Vec::new();
    loop {
        let count = reader.read(&mut chunk).map_err(|error| LeakError::Io {
            path: rel_path.to_owned(),
            what: format!("streaming surface contents: {error}"),
        })?;
        if count == 0 {
            break;
        }
        bytes_read = bytes_read
            .checked_add(u64::try_from(count).map_err(|error| LeakError::Resource {
                what: format!("read-size conversion failed: {error}"),
            })?)
            .ok_or_else(|| LeakError::Resource {
                what: format!("byte counter overflow while scanning {rel_path}"),
            })?;
        if bytes_read > MAX_SCANNED_FILE_BYTES {
            return Err(LeakError::Resource {
                what: format!(
                    "streamed bytes for {rel_path} exceed per-file limit {MAX_SCANNED_FILE_BYTES}"
                ),
            });
        }
        hasher.update(&chunk[..count]);
        for &byte in &chunk[..count] {
            if byte == b'\n' {
                if !overlong_line {
                    line_leaks(rel_path, line_number, &line, corpus, &mut findings)?;
                }
                line.clear();
                overlong_line = false;
                line_number = line_number
                    .checked_add(1)
                    .ok_or_else(|| LeakError::Resource {
                        what: format!("line counter overflow while scanning {rel_path}"),
                    })?;
            } else if !overlong_line {
                if line.len() == MAX_LINE_BYTES {
                    line.clear();
                    overlong_line = true;
                } else {
                    line.push(byte);
                }
            }
        }
    }
    if !overlong_line && !line.is_empty() {
        line_leaks(rel_path, line_number, &line, corpus, &mut findings)?;
    }
    Ok((hasher.finalize(), bytes_read, findings))
}

fn scan_surface_file(
    root: &Path,
    rel_path: &str,
    corpus: &HashSet<Digest>,
    private_files: &HashSet<Digest>,
    budget: &mut ScanBudget,
) -> Result<Vec<Leak>, LeakError> {
    validate_relative_path(rel_path)?;
    let path = root.join(rel_path);
    let (mut file, snapshot) = open_regular_file(&path, rel_path, budget)?;
    let (digest, bytes_read, line_findings) = stream_surface(&mut file, rel_path, corpus)?;
    validate_regular_file_postflight(&path, rel_path, &file, &snapshot, bytes_read)?;

    let mut findings = Vec::new();
    if bytes_read != 0 && private_files.contains(&digest) {
        push_finding(
            &mut findings,
            Leak {
                kind: LeakKind::PrivateFileBytesShipped,
                path: rel_path.to_owned(),
                detail: "byte-identical to a private fixture on disk".to_owned(),
            },
        )?;
    }
    extend_findings(&mut findings, line_findings)?;
    Ok(findings)
}

/// Collects the whole-file digests of every regular file under private
/// fixture roots that exist on disk. Missing roots are optional; every error
/// beneath a present root is a named gate failure. Files are hashed in fixed
/// chunks and revalidated after reading, so no whole-file allocation or
/// silent read race can turn into a pass.
pub fn collect_private_file_digests(root: &Path) -> Result<HashSet<Digest>, LeakError> {
    let mut digests = HashSet::new();
    let mut budget = ScanBudget::new("private fixture", MAX_TRAVERSAL_ENTRIES);
    walk_optional_roots(root, PRIVATE_FIXTURE_DIRS, |path, relative| {
        let (mut file, snapshot) = open_regular_file(path, relative, &mut budget)?;
        let (digest, bytes_read) = stream_digest(&mut file, relative)?;
        validate_regular_file_postflight(path, relative, &file, &snapshot, bytes_read)?;
        if bytes_read != 0 {
            digests.insert(digest);
        }
        Ok(())
    })?;
    Ok(digests)
}

fn parse_git_paths(bytes: &[u8]) -> Result<Vec<String>, LeakError> {
    if bytes.len() > MAX_GIT_PATH_OUTPUT_BYTES {
        return Err(LeakError::Resource {
            what: format!(
                "git ls-files emitted {} bytes, above limit {MAX_GIT_PATH_OUTPUT_BYTES}",
                bytes.len()
            ),
        });
    }
    if !bytes.is_empty() && !bytes.ends_with(&[0]) {
        return Err(LeakError::Git {
            what: "git ls-files -z output lacks its final NUL terminator".to_owned(),
        });
    }
    let mut paths = Vec::new();
    for (index, raw) in bytes.split(|byte| *byte == 0).enumerate() {
        if raw.is_empty() {
            if index + 1 == bytes.split(|byte| *byte == 0).count() {
                continue;
            }
            return Err(LeakError::Git {
                what: format!("git ls-files emitted an empty path at index {index}"),
            });
        }
        let path = std::str::from_utf8(raw).map_err(|error| LeakError::Policy {
            path: format!("git path #{index}"),
            what: format!("non-UTF-8 paths are refused: {error}"),
        })?;
        validate_relative_path(path)?;
        paths.push(path.to_owned());
        if paths.len() > MAX_SURFACE_FILES {
            return Err(LeakError::Resource {
                what: format!(
                    "git tracked-file count exceeds release-surface limit {MAX_SURFACE_FILES}"
                ),
            });
        }
    }
    Ok(paths)
}

/// The git-tracked file set, repository-relative with forward slashes —
/// the ground truth of what a commit (and therefore any package built
/// from one) carries.
pub fn git_tracked_files(root: &Path) -> Result<Vec<String>, LeakError> {
    let mut child = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| LeakError::Git {
            what: format!("spawning git ls-files: {e}"),
        })?;
    let mut stdout = child.stdout.take().ok_or_else(|| LeakError::Git {
        what: "git ls-files did not provide its requested stdout pipe".to_owned(),
    })?;
    let mut bytes = Vec::new();
    let read_result = stdout
        .by_ref()
        .take(u64::try_from(MAX_GIT_PATH_OUTPUT_BYTES).unwrap_or(u64::MAX) + 1)
        .read_to_end(&mut bytes);
    drop(stdout);
    if let Err(error) = read_result {
        let _ = child.kill();
        let _ = child.wait();
        return Err(LeakError::Git {
            what: format!("reading git ls-files output: {error}"),
        });
    }
    if bytes.len() > MAX_GIT_PATH_OUTPUT_BYTES {
        let _ = child.kill();
        let _ = child.wait();
        return Err(LeakError::Resource {
            what: format!(
                "git ls-files output exceeds {MAX_GIT_PATH_OUTPUT_BYTES} bytes"
            ),
        });
    }
    let status = child.wait().map_err(|error| LeakError::Git {
        what: format!("waiting for git ls-files: {error}"),
    })?;
    if !status.success() {
        return Err(LeakError::Git {
            what: format!("git ls-files exited with {status}"),
        });
    }
    parse_git_paths(&bytes)
}

/// Tooth 2: every private-fixture directory must be reported ignored by
/// the real `.gitignore` matcher.
pub fn gitignore_violations(root: &Path) -> Result<Vec<Leak>, LeakError> {
    let mut leaks = Vec::new();
    for &dir in PRIVATE_FIXTURE_DIRS {
        // Keep the trailing slash on the query: git only applies dir-only
        // patterns (`/corpus/`) to paths it knows are directories — a
        // slash-suffixed query matches even where the directory does not
        // exist on disk (fresh clones, CI checkouts), while a bare query
        // matches only when git can stat a real directory, which would
        // make this gate vacuous exactly where it matters most.
        let status = Command::new("git")
            .args(["check-ignore", "-q", "--", dir])
            .current_dir(root)
            .status()
            .map_err(|e| LeakError::Git {
                what: format!("spawning git check-ignore: {e}"),
            })?;
        if !status.success() {
            leaks.push(Leak {
                kind: LeakKind::PrivateDirNotIgnored,
                path: dir.to_string(),
                detail: "git check-ignore does not exclude it — a private fixture could be \
                         committed (§15.3)"
                    .to_string(),
            });
        }
    }
    Ok(leaks)
}

/// Lists the files under a staging tree, repository-relative. A missing
/// tree yields an empty list.
fn staging_files(root: &Path, staging: &str) -> Vec<String> {
    let mut files = Vec::new();
    let mut stack = vec![root.join(staging)];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                stack.push(path);
            } else if kind.is_file()
                && let Ok(rel) = path.strip_prefix(root)
            {
                files.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    files
}

/// The full release-CI gate over a repository checkout. Returns every
/// finding across all teeth; an empty vector is a pass. Gate-level
/// failures (no git, unreadable denominator) are [`LeakError`]s — the gate
/// never degrades silently.
pub fn run(root: &Path) -> Result<Vec<Leak>, LeakError> {
    let denominator_text =
        std::fs::read_to_string(root.join(DENOMINATOR_PATH)).map_err(|e| LeakError::Io {
            path: DENOMINATOR_PATH.to_owned(),
            what: e.to_string(),
        })?;
    let corpus: HashSet<Digest> = parse_denominator(&denominator_text)?.into_iter().collect();
    let private_files = collect_private_file_digests(root);

    let mut surface: Vec<String> = git_tracked_files(root)?;
    for staging in STAGING_DIRS {
        surface.extend(staging_files(root, staging));
    }
    surface.sort();
    surface.dedup();

    let mut leaks = gitignore_violations(root)?;
    leaks.extend(path_violations(&surface));
    for rel in &surface {
        let path: PathBuf = root.join(rel);
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        leaks.extend(content_leaks(rel, &bytes, &corpus, &private_files));
    }
    Ok(leaks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corpus_hash_matches_the_documented_convention() {
        // sha256("math" + NUL + "\\frac{1}{2}") — the denominator's own
        // construction, independently computable.
        let digest = corpus_hash("math", br"\frac{1}{2}");
        let mut hasher = Sha256::new();
        hasher.update(b"math\0\\frac{1}{2}");
        assert_eq!(digest, hasher.finalize());
    }

    #[test]
    fn path_violations_flag_private_dirs_and_pass_public_renders() {
        let paths = vec![
            "corpus/tex_corpus.jsonl".to_owned(),
            "gallery/reference_captures/glow.png".to_owned(),
            "scripts/manim_ref/manimlib/scene.py".to_owned(),
            "docs/g0/g0-2-renders/fmn-glow.png".to_owned(),
            "docs/g0/g0-4-corpus/denominator.tsv".to_owned(),
            "dist/FONT_BUNDLE.json".to_owned(),
        ];
        let leaks = path_violations(&paths);
        let flagged: Vec<&str> = leaks.iter().map(|l| l.path.as_str()).collect();
        assert_eq!(
            flagged,
            [
                "corpus/tex_corpus.jsonl",
                "gallery/reference_captures/glow.png",
                "scripts/manim_ref/manimlib/scene.py"
            ]
        );
    }

    #[test]
    fn content_tooth_catches_a_copied_corpus_string_in_each_form() {
        let corpus: HashSet<Digest> = [corpus_hash("math", br"\int_0^1 x^2\,dx")]
            .into_iter()
            .collect();
        let private = HashSet::new();
        for (form, bytes) in [
            (
                "raw line",
                b"header\n\\int_0^1 x^2\\,dx\nfooter\n".as_slice(),
            ),
            ("trimmed", b"   \\int_0^1 x^2\\,dx   \n".as_slice()),
            (
                "quoted literal",
                b"    \"\\int_0^1 x^2\\,dx\",\n".as_slice(),
            ),
        ] {
            let leaks = content_leaks("fixture.jsonl", bytes, &corpus, &private);
            assert_eq!(leaks.len(), 1, "{form}: {leaks:?}");
            assert_eq!(leaks[0].kind, LeakKind::CorpusStringShipped);
        }
        // A near miss is not a leak.
        assert!(content_leaks("ok.txt", b"\\int_0^1 x^2\\,dy\n", &corpus, &private).is_empty());
        // Nor is a corpus string below the minimum preimage length (short
        // harvest fragments are structurally undetectable without flagging
        // ordinary source lines — the documented boundary).
        let short: HashSet<Digest> = [corpus_hash("math", b"x")].into_iter().collect();
        assert!(content_leaks("ok.txt", b"x\n", &short, &private).is_empty());
    }

    #[test]
    fn content_tooth_catches_byte_identical_private_files() {
        let private: HashSet<Digest> = [fmn_hash::sha256(b"png-bytes")].into_iter().collect();
        let corpus = HashSet::new();
        let leaks = content_leaks(
            "docs/g0/g0-2-renders/glow.png",
            b"png-bytes",
            &corpus,
            &private,
        );
        assert_eq!(leaks.len(), 1);
        assert_eq!(leaks[0].kind, LeakKind::PrivateFileBytesShipped);
    }

    #[test]
    fn denominator_parsing_rejects_a_malformed_row() {
        assert!(parse_denominator("not-hex\tmath\t1\n").is_err());
        let good = format!("{}\tmath\t3\n", "ab".repeat(32));
        match parse_denominator(&good) {
            Ok(digests) => assert_eq!(digests.len(), 1),
            Err(e) => std::panic::panic_any(format!("a well-formed row must parse: {e}")),
        }
    }
}
