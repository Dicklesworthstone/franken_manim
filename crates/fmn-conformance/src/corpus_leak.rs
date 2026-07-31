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
use std::path::{Path, PathBuf};
use std::process::Command;

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
}

impl fmt::Display for LeakError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Git { what } => write!(f, "git ground truth unavailable: {what}"),
            Self::Io { path, what } => write!(f, "reading {path}: {what}"),
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

/// Collects the whole-file digests of every file under the
/// private-fixture directories that exist on disk. Where none exist
/// (release CI), the set is empty and the tooth degrades to the
/// denominator hash scan.
#[must_use]
pub fn collect_private_file_digests(root: &Path) -> HashSet<Digest> {
    let mut digests = HashSet::new();
    for dir in PRIVATE_FIXTURE_DIRS {
        let mut stack = vec![root.join(dir)];
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
                    && let Ok(bytes) = std::fs::read(&path)
                    && !bytes.is_empty()
                {
                    digests.insert(fmn_hash::sha256(&bytes));
                }
            }
        }
    }
    digests
}

/// The git-tracked file set, repository-relative with forward slashes —
/// the ground truth of what a commit (and therefore any package built
/// from one) carries.
pub fn git_tracked_files(root: &Path) -> Result<Vec<String>, LeakError> {
    let output = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(root)
        .output()
        .map_err(|e| LeakError::Git {
            what: format!("spawning git ls-files: {e}"),
        })?;
    if !output.status.success() {
        return Err(LeakError::Git {
            what: format!("git ls-files exited with {}", output.status),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect())
}

/// Tooth 2: every private-fixture directory must be reported ignored by
/// the real `.gitignore` matcher.
pub fn gitignore_violations(root: &Path) -> Result<Vec<Leak>, LeakError> {
    let mut leaks = Vec::new();
    for dir in PRIVATE_FIXTURE_DIRS {
        let dir = dir.trim_end_matches('/');
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
                path: format!("{dir}/"),
                detail: "git check-ignore does not exclude it — a private fixture could be \
                         committed (§15.3)"
                    .to_owned(),
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
