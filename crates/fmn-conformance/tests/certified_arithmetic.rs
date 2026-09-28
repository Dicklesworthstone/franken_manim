//! ADR-0010's binding properties, as permanent CI gates (fm-ig3).
//!
//! G0-6 measured one frame byte-identical on three platforms and concluded that
//! floating-point suffices for the certified path. That conclusion **stands on
//! four properties**, and ADR-0010 says so in the strongest terms available to a
//! decision record: "they are hereby binding on W5 rather than incidental", and
//! `docs/INPUT_CLOSURE.md` §5 adds that they are "load-bearing parts of the
//! closure, not implementation details: an engine that broke any of them would
//! produce a manifest claiming reproducibility it no longer has."
//!
//! A property that binding deserves better than a paragraph. Two of the four are
//! mechanically checkable over the source, and this file checks them on every
//! commit:
//!
//! 1. **fmn-dmath owns every transcendental on the certified path** — ADR-0010's
//!    load-bearing one, because it is what removes the platform libm from the
//!    loop. Three platforms with three different libms agreed *because the libm
//!    was not in the loop*, so a single `f64::sin` puts it back.
//! 2. **No FMA contraction** (§10.5d). rustc performs no floating-point
//!    contraction by default, so G0-6's object-code evidence confirms a default
//!    rather than a setting; the realistic regression is a hand-written
//!    `mul_add`, and that is what this refuses.
//!
//! The other two are not textual. *Fixed-order reductions* (§10.5c) is a
//! property of the engine's structure and lives in
//! `fmn_render::engine`'s draw order under ADR-0013; *IEEE-754 basic
//! operations* holds because nothing on the path uses anything weaker than
//! `+ - * / sqrt`, which check 1 is most of the argument for.
//!
//! ## What this found when it was written
//!
//! Sixteen live call sites, in four crates, all reaching pixels:
//! `srgb_eotf`/`srgb_oetf`'s `powf` and Oklab's `cbrt` in fmn-core; `wiggle`'s
//! `sin` and `exponential_decay`'s `exp` in the rate functions that drive every
//! animation's alpha; the cubic→quadratic converter's `cbrt`, which decides a
//! segment *count*; the tracker `exp`/`ln`; and nine trigonometric calls across
//! the tip, arc and brace constructors. Two of them were inside the very frame
//! G0-6 hashed — which means that frame agreed across three libms **despite**
//! calling into them, not because it did not. The evidence survives; the
//! argument was weaker than the ADR stated, and this file is what makes it as
//! strong as it claims.
//!
//! Closing them needed ADR-0014: fmn-core, fmn-mobject and fmn-library had no
//! dependency edge to fmn-dmath at all, so the funnel was unreachable from the
//! crates that most needed it.

use std::io::Read;
use std::path::{Path, PathBuf};

/// Every crate whose arithmetic can reach a certified artifact.
///
/// Deliberately "everything but the funnel and the bridge" rather than a curated
/// list: a curated list is a place for a new crate to be forgotten, and the cost
/// of scanning a crate that turns out not to compute anything is zero.
/// `fmn-dmath` is the implementation itself — its `FAST` table names
/// `f64::sin` on purpose (§6.6: "`standard` may use fast paths") — and
/// `fmn-python` is the PyO3 bridge, whose expansion is not ours to constrain.
const EXEMPT_CRATES: &[&str] = &["fmn-dmath", "fmn-python"];

/// Maximum bytes read from one Rust source authority.
///
/// Two MiB leaves ample headroom for ordinary source growth while keeping a
/// replaced or malformed authority from being allocated without bound before
/// the guard can inspect it.
const MAX_RUST_SOURCE_BYTES: u64 = 2 * 1024 * 1024;

/// The transcendental methods a certified crate may not call on a float.
///
/// `sqrt` is absent because IEEE 754 requires it correctly rounded, so it is
/// already identical everywhere. `to_degrees`, `to_radians` and `recip` are
/// absent because they lower to multiplication and division — IEEE basic
/// operations, which is property 4 rather than a violation of property 1.
/// `powi` is present because the pinned nightly lowers it through the
/// unspecified-precision `powif64`/`powif32` intrinsics, not a source-visible
/// fixed-order multiplication sequence.
const FORBIDDEN: &[&str] = &[
    "sin", "cos", "sin_cos", "tan", "asin", "acos", "atan", "atan2", "sinh", "cosh", "tanh",
    "asinh", "acosh", "atanh", "exp", "exp2", "exp_m1", "ln", "ln_1p", "log", "log2", "log10",
    "powi", "powf", "cbrt", "hypot", "gamma", "ln_gamma", "erf", "erfc",
];

/// The workspace root.
fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("the crate sits two levels under the workspace root")
        .to_path_buf()
}

/// One flagged call.
#[derive(Debug)]
struct Offence {
    path: String,
    line: usize,
    text: String,
    needle: String,
}

impl std::fmt::Display for Offence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}  [{}]  {}",
            self.path, self.line, self.needle, self.text
        )
    }
}

/// Read one UTF-8 authority through a limit-plus-one envelope.
fn read_utf8_bounded(reader: impl Read, label: &str, max_bytes: u64) -> Result<String, String> {
    let mut bytes = Vec::new();
    reader
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("reading {label}: {error}"))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max_bytes {
        return Err(format!(
            "reading {label}: source exceeds the {max_bytes}-byte limit"
        ));
    }
    String::from_utf8(bytes).map_err(|error| format!("reading {label}: not UTF-8: {error}"))
}

/// Read one Rust source without allocating past the declared authority limit.
fn read_rust_source(path: &Path) -> Result<String, String> {
    let label = path.display().to_string();
    let file = std::fs::File::open(path)
        .map_err(|error| format!("opening Rust source {label}: {error}"))?;
    read_utf8_bounded(file, &format!("Rust source {label}"), MAX_RUST_SOURCE_BYTES)
}

/// Every `.rs` file under a directory, sorted, so a failure lists the same
/// offences in the same order on every machine. Traversal errors and special
/// filesystem entries are fatal: silently omitting a source file would make a
/// clean scan meaningless.
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = std::fs::read_dir(dir)
        .map_err(|error| format!("reading source directory {}: {error}", dir.display()))?;
    let mut entries = entries
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("reading an entry under {}: {error}", dir.display()))?;
    entries.sort_by_key(std::fs::DirEntry::path);
    for entry in entries {
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("reading filesystem type for {}: {error}", path.display()))?;
        if file_type.is_dir() {
            rust_files(&path, out)?;
        } else if file_type.is_file() {
            if path.extension().is_some_and(|extension| extension == "rs") {
                out.push(path);
            }
        } else {
            return Err(format!(
                "source traversal encountered a non-file, non-directory entry: {}",
                path.display()
            ));
        }
    }
    Ok(())
}

/// The crate source roots this guard covers.
fn certified_roots() -> Result<Vec<(String, PathBuf)>, String> {
    let root = workspace().join("crates");
    let entries = std::fs::read_dir(&root)
        .map_err(|error| format!("reading crate directory {}: {error}", root.display()))?;
    let mut entries = entries
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("reading an entry under {}: {error}", root.display()))?;
    entries.sort_by_key(std::fs::DirEntry::path);
    let mut roots = Vec::new();
    for entry in entries {
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("reading filesystem type for {}: {error}", path.display()))?;
        if !file_type.is_dir() {
            if file_type.is_symlink() {
                return Err(format!(
                    "crate traversal encountered a symbolic link: {}",
                    path.display()
                ));
            }
            continue;
        }
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| format!("crate directory name is not UTF-8: {}", path.display()))?;
        if EXEMPT_CRATES.contains(&name.as_str()) {
            continue;
        }
        let src = path.join("src");
        match std::fs::symlink_metadata(&src) {
            Ok(metadata) if metadata.file_type().is_dir() => roots.push((name, src)),
            Ok(_) => {
                return Err(format!(
                    "crate source root is not a real directory: {}",
                    src.display()
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "reading crate source root {}: {error}",
                    src.display()
                ));
            }
        }
    }
    Ok(roots)
}

/// Return source with comments and literals blanked, preserving line breaks.
///
/// A guard that reads comments flags its own documentation: `fill.rs` explains
/// why the disc antiderivative uses `atan2` "because `f64::asin` defers to the
/// platform's libm", and `distance.rs` says the same about `cbrt`. Both
/// sentences are the *reason this file exists* and neither is a call.
///
/// Blanking rather than deleting keeps every diagnostic on its original line.
/// It also makes the brace counter below operate on Rust tokens rather than on
/// braces that happen to occur inside a test string or block comment.
fn code_only(text: &str) -> String {
    #[derive(Clone, Copy)]
    enum State {
        Code,
        LineComment,
        BlockComment(usize),
        String,
        RawString(usize),
    }

    fn raw_string_open(bytes: &[u8], start: usize) -> Option<(usize, usize)> {
        if bytes.get(start) != Some(&b'r') {
            return None;
        }
        let mut quote = start + 1;
        while bytes.get(quote) == Some(&b'#') {
            quote += 1;
        }
        (bytes.get(quote) == Some(&b'"')).then_some((quote + 1, quote - start - 1))
    }

    fn char_literal_end(text: &str, start: usize) -> Option<usize> {
        let bytes = text.as_bytes();
        let mut next = start + 1;
        match *bytes.get(next)? {
            b'\n' | b'\r' | b'\'' => return None,
            b'\\' => {
                next += 1;
                match *bytes.get(next)? {
                    b'x' => next += 3,
                    b'u' => {
                        next += 1;
                        if bytes.get(next) != Some(&b'{') {
                            return None;
                        }
                        next += 1;
                        while !matches!(bytes.get(next), None | Some(b'}' | b'\n' | b'\r')) {
                            next += 1;
                        }
                        if bytes.get(next) != Some(&b'}') {
                            return None;
                        }
                        next += 1;
                    }
                    _ => next += 1,
                }
            }
            _ => next += text[next..].chars().next()?.len_utf8(),
        }
        (bytes.get(next) == Some(&b'\'')).then_some(next + 1)
    }

    let bytes = text.as_bytes();
    let mut out = vec![b' '; bytes.len()];
    let mut state = State::Code;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\n' {
            out[i] = b'\n';
            if matches!(state, State::LineComment) {
                state = State::Code;
            }
            i += 1;
            continue;
        }

        match state {
            State::Code => {
                if bytes[i..].starts_with(b"//") {
                    state = State::LineComment;
                    i += 2;
                } else if bytes[i..].starts_with(b"/*") {
                    state = State::BlockComment(1);
                    i += 2;
                } else if let Some((after_quote, hashes)) = raw_string_open(bytes, i) {
                    state = State::RawString(hashes);
                    i = after_quote;
                } else if bytes[i] == b'"' {
                    state = State::String;
                    i += 1;
                } else if bytes[i] == b'\'' {
                    if let Some(end) = char_literal_end(text, i) {
                        i = end;
                    } else {
                        out[i] = bytes[i];
                        i += 1;
                    }
                } else {
                    out[i] = bytes[i];
                    i += 1;
                }
            }
            State::LineComment => i += 1,
            State::BlockComment(depth) => {
                if bytes[i..].starts_with(b"/*") {
                    state = State::BlockComment(depth + 1);
                    i += 2;
                } else if bytes[i..].starts_with(b"*/") {
                    state = if depth == 1 {
                        State::Code
                    } else {
                        State::BlockComment(depth - 1)
                    };
                    i += 2;
                } else {
                    i += 1;
                }
            }
            State::String => {
                if bytes[i] == b'\\' {
                    if bytes.get(i + 1) == Some(&b'\n') {
                        out[i + 1] = b'\n';
                    }
                    i = (i + 2).min(bytes.len());
                } else if bytes[i] == b'"' {
                    state = State::Code;
                    i += 1;
                } else {
                    i += 1;
                }
            }
            State::RawString(hashes) => {
                if bytes[i] == b'"'
                    && bytes
                        .get(i + 1..i + 1 + hashes)
                        .is_some_and(|suffix| suffix.iter().all(|b| *b == b'#'))
                {
                    state = State::Code;
                    i += 1 + hashes;
                } else {
                    i += 1;
                }
            }
        }
    }
    String::from_utf8(out).expect("blanking valid UTF-8 with ASCII preserves UTF-8")
}

/// Whether a cfg expression can only be true when `test` is true.
///
/// This is deliberately conservative: an expression we do not understand is
/// scanned as production. `all` requires only one test-only conjunct, while
/// `any` is test-only only when every branch is.
fn cfg_requires_test(expr: &str) -> bool {
    fn arguments<'a>(expr: &'a str, operator: &str) -> Option<Vec<&'a str>> {
        let body = expr
            .strip_prefix(operator)?
            .strip_prefix('(')?
            .strip_suffix(')')?;
        let mut args = Vec::new();
        let mut depth = 0_u32;
        let mut start = 0;
        for (i, byte) in body.bytes().enumerate() {
            match byte {
                b'(' => depth += 1,
                b')' => depth = depth.saturating_sub(1),
                b',' if depth == 0 => {
                    args.push(&body[start..i]);
                    start = i + 1;
                }
                _ => {}
            }
        }
        args.push(&body[start..]);
        Some(args)
    }

    if expr == "test" {
        return true;
    }
    if let Some(args) = arguments(expr, "all") {
        return args.into_iter().any(cfg_requires_test);
    }
    if let Some(args) = arguments(expr, "any") {
        return !args.is_empty() && args.into_iter().all(cfg_requires_test);
    }
    false
}

/// Return the byte immediately after a leading test-only cfg attribute.
fn test_only_cfg_end(code: &str) -> Option<usize> {
    let trimmed = code.trim_start();
    let leading = code.len() - trimmed.len();
    let close = trimmed.find(']')?;
    let attribute = &trimmed[..=close];
    let compact: String = attribute.chars().filter(|c| !c.is_whitespace()).collect();
    let expr = compact.strip_prefix("#[cfg(")?.strip_suffix(")]")?;
    cfg_requires_test(expr).then_some(leading + close + 1)
}

/// Consume one line of a test-only item and report whether the item ended.
fn test_item_line_ends(code: &str, depth: &mut usize, started: &mut bool) -> bool {
    let opens = code.matches('{').count();
    let closes = code.matches('}').count();
    *started |= opens > 0;
    *depth = depth.saturating_add(opens).saturating_sub(closes);
    (*started && *depth == 0) || (!*started && code.trim_end().ends_with(';'))
}

/// Whether an associated-call match names one of the certified funnels.
///
/// Associated needles deliberately match every path rather than only the
/// literal primitive names: `type Float = f64; Float::sin(x)` reaches the same
/// platform intrinsic as `f64::sin(x)`. Direct `fmn_dmath` calls are the
/// contract, and fmn-geom's private `scalar` module is its audited local facade.
fn approved_associated_funnel(label: &str, text: &str, offset: usize) -> bool {
    let prefix = &text[..offset];
    let segment = prefix
        .rsplit(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '#')))
        .next()
        .unwrap_or_default();
    matches!(segment, "fmn_dmath" | "r#fmn_dmath")
        || (label == "fmn-geom" && matches!(segment, "scalar" | "r#scalar"))
}

/// Scan one file's **non-test** region for `needles`, returning the lines read.
///
/// Test code is excluded on purpose: a test computing an expected value with
/// `f64::sin` is comparing against the platform, which is exactly what a test
/// should be free to do.
///
/// Excluding it correctly took two attempts, and the first one is worth
/// recording because it failed *silently*. Stopping at the first `#[cfg(test)]`
/// assumes an inline test module is the last thing in a file — usually true, and
/// false in `fmn-geom/src/bezier.rs`, whose line 6 is a `#[cfg(test)] use`
/// bringing in a helper. That single line hid the remaining 173 lines of
/// production geometry from the sweep, and the sweep reported clean.
///
/// So the attribute now skips **the item it introduces** and nothing more: a
/// `use`/statement form runs to its `;`, and a block form (`mod`, `fn`, `impl`)
/// runs until its braces balance.
///
/// Production lines are searched through a rolling whitespace-free window.
/// Rust permits a method name and its call parentheses on different physical
/// lines, so matching each line independently would leave `.ln_1p\n()` and
/// `.mul_add\n()` as silent holes. The window retains only the longest needle's
/// prefix budget and resets across a skipped test item.
fn scan(
    path: &Path,
    label: &str,
    needles: &[(String, String)],
    out: &mut Vec<Offence>,
) -> Result<usize, String> {
    let text = read_rust_source(path)?;
    let code_text = code_only(&text);
    let mut scanned = 0;
    let mut skip_depth: Option<usize> = None;
    let mut skip_started = false;
    let carry_limit = needles
        .iter()
        .map(|(needle, _)| needle.len())
        .max()
        .unwrap_or(0)
        .saturating_sub(1);
    let mut carry = String::new();
    let mut carry_lines = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let source_lines: Vec<_> = text.lines().collect();

    for (i, code) in code_text.lines().enumerate() {
        // Inside a `#[cfg(test)]` item: consume it, then resume.
        if let Some(depth) = skip_depth.as_mut() {
            carry.clear();
            carry_lines.clear();
            if test_item_line_ends(code, depth, &mut skip_started) {
                skip_depth = None;
                skip_started = false;
            }
            continue;
        }

        if let Some(attribute_end) = test_only_cfg_end(code) {
            carry.clear();
            carry_lines.clear();
            let mut depth = 0;
            skip_started = false;
            let item_on_attribute_line = &code[attribute_end..];
            if !test_item_line_ends(item_on_attribute_line, &mut depth, &mut skip_started) {
                skip_depth = Some(depth);
            } else {
                skip_started = false;
            }
            continue;
        }

        scanned += 1;
        let searchable: String = code.chars().filter(|c| !c.is_whitespace()).collect();
        let carry_len = carry.len();
        let mut combined = String::with_capacity(carry_len + searchable.len());
        combined.push_str(&carry);
        combined.push_str(&searchable);
        let mut combined_lines = carry_lines.clone();
        combined_lines.resize(combined.len(), i + 1);
        for (needle, name) in needles {
            for (offset, _) in combined.match_indices(needle.as_str()) {
                if offset + needle.len() <= carry_len {
                    continue;
                }
                if needle.starts_with("::") {
                    let after = &combined[offset + needle.len()..];
                    if after
                        .chars()
                        .next()
                        .is_some_and(|c| c == '_' || c.is_alphanumeric())
                        || approved_associated_funnel(label, &combined, offset)
                    {
                        continue;
                    }
                }
                let line = combined_lines.get(offset).copied().unwrap_or(i + 1);
                if !seen.insert((line, name.clone())) {
                    continue;
                }
                out.push(Offence {
                    path: format!("{label}/{}", path.file_name().unwrap().to_string_lossy()),
                    line,
                    text: source_lines
                        .get(line.saturating_sub(1))
                        .map_or_else(String::new, |source| source.trim_start().to_owned()),
                    needle: name.clone(),
                });
            }
        }
        let mut carry_start = combined.len().saturating_sub(carry_limit);
        while !combined.is_char_boundary(carry_start) {
            carry_start += 1;
        }
        carry = combined[carry_start..].to_owned();
        carry_lines = combined_lines[carry_start..].to_vec();
    }
    Ok(scanned)
}

/// The needles for property 1: ordinary/raw method and associated-call forms.
fn transcendental_needles() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for name in FORBIDDEN {
        out.push((format!(".{name}("), (*name).to_string()));
        out.push((format!(".r#{name}("), format!("r#{name}")));
        out.push((format!("::{name}"), format!("associated::{name}")));
        out.push((format!("::r#{name}"), format!("associated::r#{name}")));
    }
    out
}

/// The needles for property 2. Assembled rather than written out so this file
/// does not match itself — the spike's version of this guard failed on its own
/// source the first time it ran.
fn fma_needles() -> Vec<(String, String)> {
    let dot = concat!(".mul", "_add(");
    let name = concat!("mul", "_add");
    let raw_name = concat!("r#mul", "_add");
    vec![
        (dot.to_string(), "mul_add".to_string()),
        (format!(".{raw_name}("), raw_name.to_string()),
        (format!("::{name}"), format!("associated::{name}")),
        (format!("::{raw_name}"), format!("associated::{raw_name}")),
    ]
}

/// Run one guard over the whole certified path, returning `(offences, lines)`.
fn sweep(needles: &[(String, String)]) -> Result<(Vec<Offence>, usize), String> {
    let mut offences = Vec::new();
    let mut scanned = 0;
    for (name, src) in certified_roots()? {
        let mut files = Vec::new();
        rust_files(&src, &mut files)?;
        for f in &files {
            scanned += scan(f, &name, needles, &mut offences)?;
        }
    }
    Ok((offences, scanned))
}

/// The floor the sweep must clear before a clean result means anything.
///
/// A scanner that stops finding files reports zero offences and looks like a
/// pass, so a clean sweep is only evidence if the sweep was large. The certified
/// crates read ~47 000 lines today; the floor sits close enough under that to
/// catch a walk that lost a crate or a `#[cfg(test)]` skip that swallowed a file,
/// and far enough under it that ordinary churn never trips it. It is a tripwire,
/// not a coverage target — raise it when it stops being one.
const MIN_SCANNED_LINES: usize = 40_000;

#[test]
fn every_certified_transcendental_routes_through_fmn_dmath() {
    let (offences, scanned) = sweep(&transcendental_needles())
        .expect("the certified arithmetic sweep must read every source authority");
    assert!(
        scanned > MIN_SCANNED_LINES,
        "the sweep only read {scanned} lines — the walk is broken, not the code clean"
    );
    assert!(
        offences.is_empty(),
        "ADR-0010's first binding property: fmn-dmath owns EVERY transcendental \
         on the certified path, because that is what removes the platform libm \
         from the loop. {} call site(s) reach std instead:\n{}\n\n\
         Route each through `fmn_dmath::<fn>` (or fmn-geom's `scalar` funnel, or \
         `fmn_frame::transfer` for the colour transfer functions). If a crate has \
         no edge to fmn-dmath, adding one is ADR-0014's precedent, not a reason \
         to leave the call.",
        offences.len(),
        offences
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn no_fma_contraction_on_the_certified_path() {
    let (offences, scanned) = sweep(&fma_needles())
        .expect("the certified arithmetic sweep must read every source authority");
    assert!(
        scanned > MIN_SCANNED_LINES,
        "the sweep read only {scanned} lines"
    );
    assert!(
        offences.is_empty(),
        "§10.5(d) forbids FMA on certified paths; G0-6 verified zero \
         fmadd/fmla in the aarch64 object code, on a target where FMA is \
         baseline. {} hand-written contraction(s):\n{}",
        offences.len(),
        offences
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn the_guard_notices_what_it_claims_to_notice() {
    // A source scanner that finds nothing is indistinguishable from a source
    // scanner that is broken, so the needles are exercised against text whose
    // answer is known. Every form that appeared in the real sweep is here,
    // including the two that must NOT be flagged.
    let sample = "\
#[cfg(test)]
use crate::helper;
let a = x.sin();
let b = f64::cos(y);
let c = z.powf(2.4);
let d = w.cbrt();
let e = p.atan2(q);
let f = m.mul_add(n, o);
let f_qualified = <f64>::mul_add(m, n, o);
let g = t.sqrt();
let h = u.powi(3);
let i = fmn_dmath::sin(v);
let pair = r.sin_cos();
let qualified = <f64>::acos(q);
let raw_method = raw.r#sin();
let raw_qualified = f64::r#cos(raw);
type FloatAlias = f64;
let alias_associated = FloatAlias::exp2(raw);
let alias_fully_qualified = <FloatAlias>::log2(raw);
let alias_function_item = FloatAlias::hypot;
let alias_prefix_boundary = FloatAlias::sin_cos(raw);
let comment_between_call_tokens = z.cosh /* retained comment */ ();
let gamma = aa.gamma();
let ln_gamma = bb.ln_gamma();
let erf = cc.erf();
let erfc = dd.erfc();
let split_transcendental = ee
    .ln_1p
    ();
let split_fma = ff
    .mul_add
    (gg, hh);
let raw_fma = raw.r#mul_add(aa, bb);
let raw_fma_qualified = f64::r#mul_add(raw, aa, bb);
// this comment mentions .sin() and f64::cbrt and must not count
/// nor must this doc comment's `.exp()`
let string_only = \".log(\";
let raw_string_only = r#\".log2(\"#;
/* this block comment's `.log10()` must not count */
let j = k.to_radians();
let url = \"https://example.invalid/a\"; let leak = q.tanh();
#[cfg(test)]
mod tests {
    const OPEN_BRACE_INSIDE_A_STRING: &str = \"{\";
    fn helper() { let hidden = z.acos(); }
}
let after_test_string_brace = q.asinh();
#[cfg(test)] mod inline_tests { fn helper() { let hidden = z.acosh(); } }
let after_inline_test_item = q.atanh();
#[cfg(all(test, unix))]
mod unix_tests { fn helper() { let hidden = z.log10(); } }
let after_test_only_cfg = q.exp_m1();
";
    let dir = std::env::temp_dir().join(format!(
        "fmn-guard-selftest-{}-{}",
        std::process::id(),
        line!()
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let file = dir.join("sample.rs");
    std::fs::write(&file, sample).expect("write sample");

    let mut hits = Vec::new();
    scan(&file, "sample", &transcendental_needles(), &mut hits)
        .expect("self-test source is readable");
    let mut names: Vec<&str> = hits.iter().map(|o| o.needle.as_str()).collect();
    names.sort_unstable();
    names.dedup();
    // Four claims in one comparison:
    //  * the method and path forms are both caught, including fully qualified
    //    and aliased primitive paths, raw identifiers, whitespace left by an
    //    intervening comment, and a method name whose call parentheses begin
    //    on the next source line;
    //  * comments, strings, `sqrt`, `to_radians` and a qualified
    //    `fmn_dmath::` call are all legal and must not appear;
    //  * `powi`, `sin_cos`, and the four pinned-nightly libc-backed functions
    //    are covered, and `tanh` IS caught even though a `//`-bearing string
    //    literal precedes it on the same line — the false-negative class
    //    `code_only` now closes;
    //  * `acos`, `acosh`, and `log10` are NOT caught because they sit inside
    //    test-only items, while `asinh`, `atanh`, and `exp_m1` after those items
    //    are caught even when a test string contains an unmatched source brace
    //    or the entire test item shares its attribute's line.
    assert_eq!(
        names,
        [
            "asinh",
            "associated::acos",
            "associated::cos",
            "associated::exp2",
            "associated::hypot",
            "associated::log2",
            "associated::r#cos",
            "associated::sin_cos",
            "atan2",
            "atanh",
            "cbrt",
            "cosh",
            "erf",
            "erfc",
            "exp_m1",
            "gamma",
            "ln_1p",
            "ln_gamma",
            "powf",
            "powi",
            "r#sin",
            "sin",
            "sin_cos",
            "tanh"
        ],
        "the transcendental needles do not catch what they must, or catch what \
         they must not"
    );

    let mut fma = Vec::new();
    scan(&file, "sample", &fma_needles(), &mut fma).expect("self-test source is readable");
    let mut fma_names: Vec<&str> = fma.iter().map(|o| o.needle.as_str()).collect();
    fma_names.sort_unstable();
    assert_eq!(
        fma_names,
        [
            "associated::mul_add",
            "associated::r#mul_add",
            "mul_add",
            "mul_add",
            "r#mul_add"
        ],
        "the FMA needles missed a hand-written contraction"
    );

    std::fs::remove_file(&file).ok();
    std::fs::remove_dir(&dir).ok();
}

#[test]
fn source_authority_failures_are_fatal_and_reads_are_bounded() {
    let missing =
        workspace().join("crates/fmn-conformance/tests/__missing_certified_arithmetic_authority__");
    assert!(
        !missing.exists(),
        "the missing-authority test sentinel must remain absent"
    );

    let mut files = Vec::new();
    let walk_error = rust_files(&missing, &mut files).expect_err("a missing root must fail");
    assert!(
        walk_error.contains("reading source directory"),
        "unexpected walk diagnostic: {walk_error}"
    );

    let mut offences = Vec::new();
    let read_error = scan(
        &missing.with_extension("rs"),
        "missing",
        &transcendental_needles(),
        &mut offences,
    )
    .expect_err("a missing source must fail");
    assert!(
        read_error.contains("opening Rust source"),
        "unexpected source diagnostic: {read_error}"
    );

    let exact = read_utf8_bounded(std::io::Cursor::new(b"12345678"), "exact.rs", 8)
        .expect("the exact byte limit is admitted");
    assert_eq!(exact, "12345678");
    let oversize = read_utf8_bounded(std::io::Cursor::new(b"123456789"), "oversize.rs", 8)
        .expect_err("one byte over the authority limit must fail");
    assert!(
        oversize.contains("exceeds the 8-byte limit"),
        "unexpected size diagnostic: {oversize}"
    );

    struct RefusingReader;
    impl Read for RefusingReader {
        fn read(&mut self, _buffer: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "synthetic read refusal",
            ))
        }
    }

    let refusal = read_utf8_bounded(RefusingReader, "unreadable.rs", 8)
        .expect_err("an authority read error must fail");
    assert!(
        refusal.contains("synthetic read refusal"),
        "unexpected read diagnostic: {refusal}"
    );
}

#[test]
fn the_exemptions_are_the_two_that_are_argued_for() {
    // The allowlist is one line long and it must stay that way: an exemption is
    // a hole in a property ADR-0010 calls load-bearing, so growing this list is
    // an ADR rather than an edit.
    assert_eq!(EXEMPT_CRATES, &["fmn-dmath", "fmn-python"]);
    // And the sweep must actually be reaching the crates that matter.
    let names: Vec<String> = certified_roots()
        .expect("crate roots are readable")
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    for expected in [
        "fmn-core",
        "fmn-geom",
        "fmn-render",
        "fmn-library",
        "fmn-mobject",
    ] {
        assert!(
            names.iter().any(|n| n == expected),
            "{expected} is not being swept"
        );
    }
}

// ---------------------------------------------------------------------------
// The governed closure (fm-certified-libm-leak-3aja).
//
// The sweep above reads `crates/*/src` only. FrankenSuite crates also run on
// certified paths: StreamLines integrate through fsci-integrate, smoothing
// solves through fsci-linalg, the one RNG is fnp-random-core, glyphs and math
// come from fmd-font and fmd-math. A platform-libm call there puts the libm
// back in the loop exactly as a call in our own crates would. So the same
// needles run over those crates' sources at the SUITE.lock pins, from Cargo's
// git checkouts. A missing checkout is an error, never a pass.

/// One suite crate on a certified path.
struct SuiteCrate {
    /// The SUITE.lock repository.
    repo: &'static str,
    /// The crate directory in that repository.
    dir: &'static str,
    /// Modules under `src/` to read; empty reads the whole crate.
    modules: &'static [&'static str],
    /// The functions fmn calls. When non-empty, only calls inside functions
    /// reachable from these by name (a conservative over-approximation: every
    /// function of a called name, and everything any of them calls) count.
    /// Empty means every function counts.
    entry_points: &'static [&'static str],
    /// The fmn call sites that put the crate on the path.
    reason: &'static str,
}

const SUITE_CERTIFIED: &[SuiteCrate] = &[
    SuiteCrate {
        repo: "frankenscipy",
        dir: "crates/fsci-integrate",
        modules: &[],
        entry_points: &[],
        reason: "fmn-library fields.rs: StreamLines solve_ivp(Rk45)",
    },
    SuiteCrate {
        repo: "frankenscipy",
        dir: "crates/fsci-linalg",
        modules: &[],
        entry_points: &["solve", "solve_banded"],
        reason: "fmn-geom smoothing.rs: fsci_linalg::solve and solve_banded only",
    },
    SuiteCrate {
        repo: "franken_numpy",
        dir: "crates/fnp-random-core",
        modules: &[],
        entry_points: &[],
        reason: "fmn-core rng.rs: PCG64DXSM, the one RNG",
    },
    SuiteCrate {
        repo: "franken_numpy",
        dir: "crates/fnp-ndarray",
        modules: &[],
        entry_points: &[],
        reason: "fmn-mobject record.rs, numpy.rs: NdLayout and MemoryOrder",
    },
    SuiteCrate {
        repo: "franken_numpy",
        dir: "crates/fnp-dtype",
        modules: &[],
        entry_points: &["item_size", "name"],
        reason: "fmn-mobject numpy.rs: the DType enum and StructuredField as data, and \
                 DType::item_size (name is included conservatively)",
    },
    SuiteCrate {
        repo: "franken_markdown",
        dir: "fmd-font",
        modules: &[],
        entry_points: &[],
        reason: "fmn-text: sfnt parsing and glyph outlines",
    },
    SuiteCrate {
        repo: "franken_markdown",
        dir: "fmd-math",
        modules: &[],
        entry_points: &[],
        reason: "fmn-tex: TeX math layout",
    },
    SuiteCrate {
        repo: "franken_markdown",
        dir: ".",
        modules: &["ast.rs", "parse", "highlight.rs"],
        entry_points: &[],
        reason: "fmn-library markdown.rs, markdown_document.rs, code.rs: parser, AST and highlighter only",
    },
    SuiteCrate {
        repo: "franken_networkx",
        dir: "crates/fnx-classes",
        modules: &[],
        entry_points: &[],
        reason: "fmn-library network_graph.rs",
    },
];

/// Suite dependencies of the swept crates that this sweep does not read, each
/// with the argument: either no certified output reaches them, or the calls
/// that do reach them perform no floating-point arithmetic.
const SUITE_NOT_SWEPT: &[(&str, &str)] = &[
    (
        "ft-kernel-metal",
        "Accelerator Annex: standard-only; certified mode refuses annex engines \
         (fmn-runtime plan::certified_refuses_standard_only_engines)",
    ),
    (
        "asupersync",
        "batch farms and process supervision; never in the frame loop (§3 D4)",
    ),
    (
        "fp-frame",
        "TableMobject calls DataFrame::from_csv, shape() and cell access only: correctly \
         rounded float parsing and text formatting. The crate's arithmetic (6.8 MB lib.rs of \
         dataframe analytics) is never called by fmn",
    ),
    (
        "fp-types",
        "the Scalar cell type TableMobject formats; no arithmetic",
    ),
    (
        "fnp-io",
        "fmn-conformance reads and writes .npy fixtures with it; no certified artifact is \
         computed through it",
    ),
];

/// Calls this guard reports but does not yet fail on, each owned by an open
/// bead: (label, file, needle, exact count, owner). An entry whose count no
/// longer matches fails, so a fixed leak forces its entry out and a new call
/// in the same file is still caught.
const SUITE_KNOWN_LEAKS: &[(&str, &str, &str, usize, &str)] = &[
    (
        "frankenscipy/crates/fsci-integrate",
        "step_size.rs",
        "powf",
        5,
        "fm-9esi: 1 in select_initial_step's fallback arm for a non-integral error order (RK45's \
         order is 5, so it takes the deterministic kth_root arm, frankenscipy 5a7aafa2) + 4 \
         EPSILON.powf constants in num_jac, whose only callers are bdf.rs and radau.rs \
         (unreachable below); this file carries no call on fmn's RK45 path",
    ),
    (
        "frankenscipy/crates/fsci-linalg",
        "lib.rs",
        "log10",
        1,
        "fm-wi8l: solve's policy signal condition_signal_from_rcond; decides FailClosed/FullValidate \
         at thresholds, never the solution values",
    ),
    (
        "frankenscipy/crates/fsci-linalg",
        "lib.rs",
        "mul_add",
        12,
        "fm-bp82: SPD blocked-Cholesky panel TRSM/SYRK kernels, run only for symmetric systems of \
         n >= 128; fmn's one dense solve (closed smoothing) never builds a symmetric matrix, pinned \
         by fmn-geom's closed_smoothing_matrix_is_never_symmetric",
    ),
];

/// Files of a swept suite crate that the fmn call sites cannot reach, with
/// the reason. Each is argued, not assumed: fmn calls solve_ivp with
/// SolverKind::Rk45 only, so the quadrature, BDF, Radau and BVP modules
/// never run for fmn.
const SUITE_UNREACHABLE_FILES: &[(&str, &str, &str)] = &[
    (
        "frankenscipy/crates/fsci-integrate",
        "quad.rs",
        "quadrature: no fmn call site",
    ),
    (
        "frankenscipy/crates/fsci-integrate",
        "quadpack.rs",
        "private QUADPACK kernels whose only caller is quad.rs (above)",
    ),
    (
        "frankenscipy/crates/fsci-integrate",
        "bdf.rs",
        "BDF: fmn uses SolverKind::Rk45 only",
    ),
    (
        "frankenscipy/crates/fsci-integrate",
        "radau.rs",
        "Radau: fmn uses SolverKind::Rk45 only",
    ),
    (
        "frankenscipy/crates/fsci-integrate",
        "bvp.rs",
        "boundary-value solver: no fmn call site",
    ),
    (
        "frankenscipy/crates/fsci-integrate",
        "lebedev.rs",
        "spherical quadrature: no fmn call site",
    ),
    (
        "frankenscipy/crates/fsci-integrate",
        "complex.rs",
        "complex integration: no fmn call site",
    ),
];

/// The pinned revision of a suite repository, from SUITE.lock.
fn suite_rev(repo: &str) -> Result<String, String> {
    let lock = workspace().join("SUITE.lock");
    let text = read_utf8_bounded(
        std::fs::File::open(&lock).map_err(|error| format!("opening SUITE.lock: {error}"))?,
        "SUITE.lock",
        1024 * 1024,
    )?;
    text.lines()
        .find_map(|line| {
            let mut fields = line.split('\t');
            (fields.next() == Some(repo)).then(|| fields.next().map(str::to_owned))?
        })
        .ok_or_else(|| format!("SUITE.lock has no row for {repo}"))
}

/// Cargo's checkout of `repo` at its pinned revision: exactly one
/// `$CARGO_HOME/git/checkouts/<repo>-<hash>/<rev[..7]>`.
fn suite_checkout(repo: &str) -> Result<PathBuf, String> {
    let rev = suite_rev(repo)?;
    let cargo_home = std::env::var_os("CARGO_HOME").map_or_else(
        || {
            std::env::var_os("HOME")
                .map(|home| PathBuf::from(home).join(".cargo"))
                .ok_or_else(|| "neither CARGO_HOME nor HOME is set".to_owned())
        },
        |home| Ok(PathBuf::from(home)),
    )?;
    let checkouts = cargo_home.join("git").join("checkouts");
    let entries = std::fs::read_dir(&checkouts)
        .map_err(|error| format!("reading {}: {error}", checkouts.display()))?;
    let mut found = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("reading {}: {error}", checkouts.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name
            .rsplit_once('-')
            .is_some_and(|(prefix, _)| prefix == repo)
        {
            let candidate = entry.path().join(&rev[..7]);
            if candidate.is_dir() {
                found.push(candidate);
            }
        }
    }
    match found.len() {
        1 => Ok(found.remove(0)),
        0 => Err(format!(
            "no Cargo checkout of {repo} at {} under {}: build the workspace first; \
             a missing source is not a clean one",
            &rev[..7],
            checkouts.display()
        )),
        _ => Err(format!(
            "{} checkouts of {repo} at {}: ambiguous",
            found.len(),
            &rev[..7]
        )),
    }
}

/// Every swept suite file, grouped by crate: (crate, label, files).
/// Binary targets (`src/bin`) are never linked into fmn and are not read.
/// One swept crate with its label and the files read.
type SuiteGroup = (&'static SuiteCrate, String, Vec<PathBuf>);

fn suite_files() -> Result<Vec<SuiteGroup>, String> {
    let mut out = Vec::new();
    for krate in SUITE_CERTIFIED {
        let src = suite_checkout(krate.repo)?.join(krate.dir).join("src");
        let label = format!("{}/{}", krate.repo, krate.dir.trim_start_matches("./"));
        let mut files = Vec::new();
        if krate.modules.is_empty() {
            rust_files(&src, &mut files)?;
        } else {
            for module in krate.modules {
                let path = src.join(module);
                if path.is_dir() {
                    rust_files(&path, &mut files)?;
                } else if path.is_file() {
                    files.push(path);
                } else {
                    return Err(format!("{label}: declared module {module} does not exist"));
                }
            }
        }
        files.retain(|file| !file.starts_with(src.join("bin")));
        if files.is_empty() {
            return Err(format!("{label}: no Rust sources under {}", src.display()));
        }
        out.push((krate, label, files));
    }
    Ok(out)
}

/// One function body in a suite source.
struct SuiteFn {
    file: PathBuf,
    name: String,
    /// First and last line of the body, 1-based and inclusive.
    lines: (usize, usize),
    callees: Vec<String>,
}

fn is_ident_byte(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphanumeric()
}

/// Index every `fn` body of `files`, with the names each body calls. The text
/// is `code_only`, so braces in comments and literals do not count.
fn suite_functions(files: &[PathBuf]) -> Result<Vec<SuiteFn>, String> {
    let mut out = Vec::new();
    for file in files {
        let code = code_only(&read_rust_source(file)?);
        let bytes = code.as_bytes();
        let line_of = |offset: usize| bytes[..offset].iter().filter(|&&b| b == b'\n').count() + 1;
        let mut i = 0;
        while let Some(found) = code[i..].find("fn ") {
            let at = i + found;
            i = at + 3;
            if at > 0 && is_ident_byte(bytes[at - 1]) {
                continue;
            }
            let mut j = at + 3;
            while j < bytes.len() && bytes[j] == b' ' {
                j += 1;
            }
            let name_start = j;
            while j < bytes.len() && is_ident_byte(bytes[j]) {
                j += 1;
            }
            if j == name_start {
                continue;
            }
            let name = code[name_start..j].to_owned();
            let Some(open_rel) = code[j..].find(['{', ';']) else {
                continue;
            };
            let open = j + open_rel;
            if bytes[open] == b';' {
                continue; // a declaration without a body
            }
            let mut depth = 0usize;
            let mut close = open;
            for (k, &byte) in bytes.iter().enumerate().skip(open) {
                match byte {
                    b'{' => depth += 1,
                    b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            close = k;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let body = &code[open..=close];
            let body_bytes = body.as_bytes();
            let mut callees = Vec::new();
            let mut k = 0;
            while k < body_bytes.len() {
                if is_ident_byte(body_bytes[k]) && (k == 0 || !is_ident_byte(body_bytes[k - 1])) {
                    let s = k;
                    while k < body_bytes.len() && is_ident_byte(body_bytes[k]) {
                        k += 1;
                    }
                    let mut after = k;
                    while after < body_bytes.len() && body_bytes[after].is_ascii_whitespace() {
                        after += 1;
                    }
                    // `name(` and turbofish `name::<T>(` are calls.
                    if after < body_bytes.len()
                        && (body_bytes[after] == b'(' || body[after..].starts_with("::<"))
                    {
                        callees.push(body[s..k].to_owned());
                    }
                } else {
                    k += 1;
                }
            }
            out.push(SuiteFn {
                file: file.clone(),
                name,
                lines: (line_of(open), line_of(close)),
                callees,
            });
        }
    }
    Ok(out)
}

/// A function body's file and inclusive 1-based line range.
type LineRange = (PathBuf, (usize, usize));

/// Line ranges of the functions reachable by name from `entry_points`.
fn reachable_ranges(
    functions: &[SuiteFn],
    entry_points: &[&str],
) -> Result<Vec<LineRange>, String> {
    let mut reached: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    let mut queue: Vec<&str> = Vec::new();
    for entry in entry_points {
        if !functions.iter().any(|f| f.name == *entry) {
            return Err(format!("entry point {entry} is not defined in the crate"));
        }
        queue.push(entry);
    }
    while let Some(name) = queue.pop() {
        if !reached.insert(name) {
            continue;
        }
        for function in functions.iter().filter(|f| f.name == name) {
            for callee in &function.callees {
                if !reached.contains(callee.as_str()) {
                    queue.push(callee.as_str());
                }
            }
        }
    }
    Ok(functions
        .iter()
        .filter(|f| reached.contains(f.name.as_str()))
        .map(|f| (f.file.clone(), f.lines))
        .collect())
}

/// Scan the suite, split into (offences, known-leak counts, lines read,
/// calls dropped as unreachable from the declared entry points).
type LeakCounts = std::collections::BTreeMap<(String, String, String), usize>;

fn suite_sweep(
    needles: &[(String, String)],
) -> Result<(Vec<Offence>, LeakCounts, usize, usize), String> {
    let mut offences = Vec::new();
    let mut scanned = 0;
    let mut unreachable = 0;
    for (krate, label, files) in suite_files()? {
        let ranges = if krate.entry_points.is_empty() {
            None
        } else {
            Some(reachable_ranges(
                &suite_functions(&files)?,
                krate.entry_points,
            )?)
        };
        for file in files {
            let name = file
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            if SUITE_UNREACHABLE_FILES
                .iter()
                .any(|(l, f, _)| *l == label && *f == name)
            {
                continue;
            }
            let mut found = Vec::new();
            scanned += scan(&file, &label, needles, &mut found)?;
            if let Some(ranges) = &ranges {
                let before = found.len();
                found.retain(|offence| {
                    ranges.iter().any(|(path, (first, last))| {
                        *path == file && (*first..=*last).contains(&offence.line)
                    })
                });
                unreachable += before - found.len();
            }
            offences.extend(found);
        }
    }
    let mut leaks = std::collections::BTreeMap::new();
    offences.retain(|offence| {
        let (label, file) = offence
            .path
            .rsplit_once('/')
            .map_or((offence.path.as_str(), ""), |(l, f)| (l, f));
        let known = SUITE_KNOWN_LEAKS
            .iter()
            .any(|(l, f, n, _, _)| *l == label && *f == file && *n == offence.needle);
        if known {
            *leaks
                .entry((label.to_owned(), file.to_owned(), offence.needle.clone()))
                .or_insert(0) += 1;
        }
        !known
    });
    Ok((offences, leaks, scanned, unreachable))
}

/// The suite sweep reads roughly 60 000 lines at today's pins; the floor
/// catches a walk that lost a crate.
const MIN_SUITE_SCANNED_LINES: usize = 20_000;

#[test]
fn every_certified_suite_transcendental_is_accounted_for() {
    let mut needles = transcendental_needles();
    needles.extend(fma_needles());
    let (offences, leaks, scanned, unreachable) =
        suite_sweep(&needles).expect("the suite sweep must read every pinned source");
    assert!(
        scanned > MIN_SUITE_SCANNED_LINES,
        "the suite sweep read only {scanned} lines: the walk is broken, not the code clean"
    );
    // The reachability filter drops calls in functions no fmn entry point can
    // reach; report how many so a filter that drops everything is visible.
    eprintln!(
        "suite sweep: {scanned} lines, {unreachable} call(s) unreachable from fmn entry points"
    );
    assert!(
        offences.is_empty(),
        "ADR-0010 property 1 over the governed closure: {} platform-libm or FMA call(s) \
         in suite crates on certified paths, neither routed through a deterministic \
         implementation nor owned by a known-leak entry:\n{}",
        offences.len(),
        offences
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    );
    for (label, file, needle, count, owner) in SUITE_KNOWN_LEAKS {
        let observed = leaks
            .get(&(
                (*label).to_owned(),
                (*file).to_owned(),
                (*needle).to_owned(),
            ))
            .copied()
            .unwrap_or(0);
        assert_eq!(
            observed, *count,
            "known leak {label}/{file} [{needle}] ({owner}): expected {count} call(s), found \
             {observed}. Fewer means the leak is closing: update or remove the entry. More \
             means a new call arrived: it is not covered by the old audit."
        );
    }
}

#[test]
fn every_suite_dependency_of_a_certified_crate_is_classified() {
    // A suite crate added to a certified crate's [dependencies] must be swept
    // or argued outside the certified path; otherwise it would be forgotten.
    let manifest = read_utf8_bounded(
        std::fs::File::open(workspace().join("Cargo.toml")).expect("workspace manifest"),
        "Cargo.toml",
        1024 * 1024,
    )
    .expect("workspace manifest is readable");
    let suite: Vec<&str> = manifest
        .lines()
        .filter(|line| line.contains("git = \"https://github.com/Dicklesworthstone/"))
        .filter_map(|line| line.split_once(" = ").map(|(name, _)| name.trim()))
        .collect();
    assert!(
        suite.len() >= 10,
        "found only {} suite git dependencies",
        suite.len()
    );
    let classified: Vec<&str> = SUITE_CERTIFIED
        .iter()
        .map(|krate| {
            if krate.dir == "." {
                krate.repo
            } else {
                krate.dir.rsplit('/').next().unwrap_or(krate.dir)
            }
        })
        .chain(SUITE_NOT_SWEPT.iter().map(|(name, _)| *name))
        .collect();
    let mut unclassified = Vec::new();
    for (crate_name, _) in certified_roots().expect("crate roots are readable") {
        let path = workspace()
            .join("crates")
            .join(&crate_name)
            .join("Cargo.toml");
        let text = read_utf8_bounded(
            std::fs::File::open(&path).expect("crate manifest"),
            "crate manifest",
            1024 * 1024,
        )
        .expect("crate manifest is readable");
        let mut in_dependencies = false;
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('[') {
                in_dependencies = trimmed == "[dependencies]";
                continue;
            }
            if !in_dependencies {
                continue;
            }
            let name = trimmed.split(['.', ' ', '=']).next().unwrap_or("");
            if suite.contains(&name) && !classified.contains(&name) {
                unclassified.push(format!("{crate_name} -> {name}"));
            }
        }
    }
    assert!(
        unclassified.is_empty(),
        "suite dependencies of certified crates with no classification: {unclassified:?}. \
         Add each to SUITE_CERTIFIED (swept) or SUITE_NOT_SWEPT (with the argument)."
    );
}

#[test]
fn every_swept_suite_crate_states_why_it_is_on_the_path() {
    for krate in SUITE_CERTIFIED {
        assert!(
            krate.reason.len() > 10 && krate.reason.contains("fmn"),
            "{}/{}: name the fmn call sites that put it on a certified path",
            krate.repo,
            krate.dir
        );
    }
    for (name, reason) in SUITE_NOT_SWEPT {
        assert!(reason.len() > 20, "{name}: argue why it is not swept");
    }
}

#[test]
fn reachability_keeps_calls_an_entry_point_reaches_and_drops_the_rest() {
    // Negative and positive control for the entry-point filter: a transcendental
    // two calls deep from the entry point is kept; one in an unrelated function
    // is dropped; a brace inside a comment or string does not end a body.
    let dir = std::env::temp_dir().join(format!("fmn-suite-reach-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let file = dir.join("lib.rs");
    std::fs::write(
        &file,
        "pub fn solve(x: f64) -> f64 { let s = \"}\"; /* } */ refine(x) }\n\
         fn refine(x: f64) -> f64 { step::<f64>(x) }\n\
         fn step<T>(x: f64) -> f64 { x.powf(0.2) }\n\
         pub fn unrelated(y: f64) -> f64 { y.sin() }\n",
    )
    .expect("write planted crate");
    let functions = suite_functions(std::slice::from_ref(&file)).expect("index functions");
    let ranges = reachable_ranges(&functions, &["solve"]).expect("reachable from solve");
    let mut offences = Vec::new();
    scan(&file, "planted", &transcendental_needles(), &mut offences).expect("scan");
    let kept: Vec<&str> = offences
        .iter()
        .filter(|o| ranges.iter().any(|(_, (a, b))| (*a..=*b).contains(&o.line)))
        .map(|o| o.needle.as_str())
        .collect();
    assert_eq!(kept, ["powf"], "{offences:?}");
    assert!(
        reachable_ranges(&functions, &["missing"]).is_err(),
        "an entry point that does not exist is an error, not an empty (clean) scope"
    );
}

#[test]
fn the_suite_guard_notices_a_planted_powf() {
    // Negative control: a suite-labelled source calling f64::powf on a certified
    // path is flagged, and a known-leak entry cannot hide a call in another file.
    let dir = std::env::temp_dir().join(format!("fmn-suite-guard-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let planted = dir.join("planted.rs");
    std::fs::write(
        &planted,
        "pub fn step(err: f64, e: f64) -> f64 { 0.9 * err.powf(e) }\n\
         pub fn fine(x: f64) -> f64 { x.sqrt() }\n",
    )
    .expect("write planted source");
    let mut offences = Vec::new();
    scan(
        &planted,
        "frankenscipy/crates/fsci-integrate",
        &transcendental_needles(),
        &mut offences,
    )
    .expect("scan the planted source");
    assert_eq!(offences.len(), 1, "{offences:?}");
    assert_eq!(offences[0].needle, "powf");
    assert!(
        !SUITE_KNOWN_LEAKS
            .iter()
            .any(|(l, f, n, _, _)| *l == "frankenscipy/crates/fsci-integrate"
                && *f == "planted.rs"
                && *n == "powf"),
        "a known-leak entry names files, so a new file is never covered by an old audit"
    );
}
