//! Generates the font+license bundle manifest and the shipped license
//! inventory (fm-aef, §15.3/§16.7).
//!
//! ```text
//! cargo run -p fmn-conformance --bin gen_font_manifest [--check] [--repo DIR] [--fmd-font DIR]
//! ```
//!
//! Generation mode (default): locates the pinned `fmd-font` sources (the
//! SUITE.lock `franken_markdown` rev under `$CARGO_HOME/git/checkouts`,
//! overridable with `--fmd-font`), copies every license set's OFL text to
//! `dist/licenses/fonts/<slug>-OFL.txt`, copies the engine's MIT+rider
//! `LICENSE` to `dist/licenses/LICENSE`, and writes the canonical
//! `dist/FONT_BUNDLE.json`.
//!
//! `--check`: verifies without writing — the committed manifest must be
//! byte-identical to regeneration from the committed license files, the
//! shipped OFL texts must be byte-identical to the pinned upstream copies,
//! and the shipped engine license must equal the repository `LICENSE`.
//! Exit codes: 0 ok, 64 usage, 65 drift/data, 74 io.

#![forbid(unsafe_code)]

use fmn_conformance::font_bundle::{
    ENGINE_LICENSE_PATH, FontBundleError, LICENSE_SLUGS, MANIFEST_PATH, build_manifest, ofl_path,
    render_manifest, suite_lock_pin,
};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const EXIT_USAGE: u8 = 64;
const EXIT_DATA: u8 = 65;
const EXIT_IO: u8 = 74;

struct Cli {
    check: bool,
    repo: PathBuf,
    fmd_font: Option<PathBuf>,
}

enum CliError {
    Usage(String),
    Data(String),
    Io(String),
}

impl CliError {
    fn exit_code(&self) -> u8 {
        match self {
            Self::Usage(_) => EXIT_USAGE,
            Self::Data(_) => EXIT_DATA,
            Self::Io(_) => EXIT_IO,
        }
    }
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Usage(m) | Self::Data(m) | Self::Io(m) => f.write_str(m),
        }
    }
}

fn parse_args(args: &[String]) -> Result<Cli, CliError> {
    let mut check = false;
    let mut repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .map_err(|e| CliError::Io(format!("locating the repository root: {e}")))?;
    let mut fmd_font = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--check" => check = true,
            "--repo" => {
                index += 1;
                repo = args
                    .get(index)
                    .map(PathBuf::from)
                    .ok_or_else(|| CliError::Usage("--repo needs a directory".to_owned()))?;
            }
            "--fmd-font" => {
                index += 1;
                fmd_font =
                    Some(args.get(index).map(PathBuf::from).ok_or_else(|| {
                        CliError::Usage("--fmd-font needs a directory".to_owned())
                    })?);
            }
            other => {
                return Err(CliError::Usage(format!(
                    "unknown argument '{other}'; usage: gen_font_manifest [--check] [--repo DIR] \
                     [--fmd-font DIR]"
                )));
            }
        }
        index += 1;
    }
    Ok(Cli {
        check,
        repo,
        fmd_font,
    })
}

/// Reads SUITE.lock's `franken_markdown` pin.
fn suite_pin(repo: &Path) -> Result<String, CliError> {
    let text = fs::read_to_string(repo.join("SUITE.lock"))
        .map_err(|e| CliError::Io(format!("reading SUITE.lock: {e}")))?;
    suite_lock_pin(&text, "franken_markdown")
        .ok_or_else(|| CliError::Data("SUITE.lock pins no franken_markdown row".to_owned()))
}

/// Locates the pinned `fmd-font` crate directory inside the cargo git
/// checkouts (the checkout directory is named by a short prefix of the
/// pinned revision).
fn discover_fmd_font(rev: &str) -> Result<PathBuf, CliError> {
    let cargo_home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")))
        .ok_or_else(|| {
            CliError::Usage("neither CARGO_HOME nor HOME is set; pass --fmd-font".to_owned())
        })?;
    let checkouts = cargo_home.join("git").join("checkouts");
    let entries = fs::read_dir(&checkouts)
        .map_err(|e| CliError::Io(format!("reading {}: {e}", checkouts.display())))?;
    let mut candidates = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with("franken_markdown-") {
            continue;
        }
        let Ok(revs) = fs::read_dir(entry.path()) else {
            continue;
        };
        for rev_dir in revs.flatten() {
            let short = rev_dir.file_name().to_string_lossy().into_owned();
            if short.len() >= 7 && rev.starts_with(&short) {
                let candidate = rev_dir.path().join("fmd-font");
                if candidate.is_dir() {
                    candidates.push(candidate);
                }
            }
        }
    }
    match candidates.len() {
        1 => candidates
            .into_iter()
            .next()
            .ok_or_else(|| CliError::Data("no fmd-font candidate".to_owned())),
        0 => Err(CliError::Data(format!(
            "no fmd-font checkout for franken_markdown rev {rev} under {}; run `cargo fetch` or \
             pass --fmd-font",
            checkouts.display()
        ))),
        _ => Err(CliError::Data(format!(
            "ambiguous fmd-font checkouts for rev {rev}: {candidates:?}; pass --fmd-font"
        ))),
    }
}

fn read(path: &Path, what: &str) -> Result<Vec<u8>, CliError> {
    fs::read(path).map_err(|e| CliError::Io(format!("reading {what} ({}): {e}", path.display())))
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), CliError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| CliError::Io(format!("creating {}: {e}", parent.display())))?;
    }
    fs::write(path, bytes).map_err(|e| CliError::Io(format!("writing {}: {e}", path.display())))
}

/// Gathers the OFL texts `(slug, bytes)` from a directory of upstream
/// `fonts/<slug>/OFL.txt` files.
fn gather_upstream_ofl(fmd_font: &Path) -> Result<Vec<(&'static str, Vec<u8>)>, CliError> {
    LICENSE_SLUGS
        .iter()
        .map(|slug| {
            let path = fmd_font.join("fonts").join(slug).join("OFL.txt");
            read(&path, "upstream OFL text").map(|bytes| (*slug, bytes))
        })
        .collect()
}

fn run(cli: &Cli) -> Result<String, CliError> {
    let rev = suite_pin(&cli.repo)?;
    let fmd_font = match &cli.fmd_font {
        Some(dir) => dir.clone(),
        None => discover_fmd_font(&rev)?,
    };
    let upstream_ofl = gather_upstream_ofl(&fmd_font)?;
    let engine_license = read(&cli.repo.join("LICENSE"), "the engine LICENSE")?;
    let dist = cli.repo.join("dist");

    let mut summary = String::new();
    if !cli.check {
        // Ship the license inventory: upstream OFL texts + the engine's
        // own license, byte-identical to their sources.
        for (slug, bytes) in &upstream_ofl {
            let dest = dist.join(ofl_path(slug));
            write(&dest, bytes)?;
            let _ = writeln!(
                summary,
                "shipped {} ({} bytes)",
                dest.display(),
                bytes.len()
            );
        }
        let dest = dist.join(ENGINE_LICENSE_PATH);
        write(&dest, &engine_license)?;
        let _ = writeln!(
            summary,
            "shipped {} ({} bytes)",
            dest.display(),
            engine_license.len()
        );
    }

    // The manifest is built from what actually ships: the OFL texts just
    // written (or, under --check, the committed ones — which must equal
    // the upstream copies, verified below) and the engine LICENSE.
    let committed_ofl: Vec<(&'static str, Vec<u8>)> = if cli.check {
        LICENSE_SLUGS
            .iter()
            .map(|slug| read(&dist.join(ofl_path(slug)), "shipped OFL text").map(|b| (*slug, b)))
            .collect::<Result<_, _>>()?
    } else {
        Vec::new()
    };
    if cli.check {
        for ((slug, committed_bytes), (_, upstream_bytes)) in
            committed_ofl.iter().zip(upstream_ofl.iter())
        {
            if committed_bytes != upstream_bytes {
                return Err(CliError::Data(format!(
                    "shipped {} drifted from the pinned upstream copy (rev {rev}); regenerate \
                     with `cargo run -p fmn-conformance --bin gen_font_manifest`",
                    ofl_path(slug)
                )));
            }
        }
    }
    let ofl_refs: Vec<(&str, &[u8])> = if cli.check {
        committed_ofl
            .iter()
            .map(|(s, b)| (*s, b.as_slice()))
            .collect()
    } else {
        upstream_ofl
            .iter()
            .map(|(s, b)| (*s, b.as_slice()))
            .collect()
    };

    if cli.check {
        let shipped_engine = read(&dist.join(ENGINE_LICENSE_PATH), "shipped engine LICENSE")?;
        if shipped_engine != engine_license {
            return Err(CliError::Data(format!(
                "shipped {ENGINE_LICENSE_PATH} != repository LICENSE; regenerate"
            )));
        }
    }

    let faces: Vec<(&str, &[u8])> = fmd_font::bundled::ALL_FACES.to_vec();
    let manifest = build_manifest(&rev, &faces, &ofl_refs, &engine_license)
        .map_err(|e: FontBundleError| CliError::Data(e.to_string()))?;
    let rendered = render_manifest(&manifest);

    if cli.check {
        let committed = read(&cli.repo.join(MANIFEST_PATH), "the committed manifest")?;
        if committed != rendered.as_bytes() {
            return Err(CliError::Data(format!(
                "{MANIFEST_PATH} drifted from the bundled faces at SUITE.lock rev {rev}; \
                 regenerate with `cargo run -p fmn-conformance --bin gen_font_manifest`"
            )));
        }
        let _ = writeln!(
            summary,
            "ok: {MANIFEST_PATH} matches {} faces + {} license texts at rev {rev}",
            manifest.faces.len(),
            manifest.licenses.len()
        );
    } else {
        let dest = cli.repo.join(MANIFEST_PATH);
        write(&dest, rendered.as_bytes())?;
        let _ = writeln!(
            summary,
            "wrote {} ({} faces, {} license texts, rev {rev})",
            dest.display(),
            manifest.faces.len(),
            manifest.licenses.len()
        );
    }
    Ok(summary)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match parse_args(&args).and_then(|cli| run(&cli)) {
        Ok(summary) => {
            print!("{summary}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("gen_font_manifest: {error}");
            ExitCode::from(error.exit_code())
        }
    }
}
