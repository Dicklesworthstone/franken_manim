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

/// Every `.rs` file under a directory, sorted, so a failure lists the same
/// offences in the same order on every machine.
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for p in paths {
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// The crate source roots this guard covers.
fn certified_roots() -> Vec<(String, PathBuf)> {
    let root = workspace().join("crates");
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&root)
        .expect("crates/ is readable")
        .flatten()
        .map(|e| e.path())
        .collect();
    entries.sort();
    entries
        .into_iter()
        .filter_map(|p| {
            let name = p.file_name()?.to_str()?.to_string();
            if EXEMPT_CRATES.contains(&name.as_str()) {
                return None;
            }
            let src = p.join("src");
            src.is_dir().then_some((name, src))
        })
        .collect()
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
fn scan(path: &Path, label: &str, needles: &[(String, String)], out: &mut Vec<Offence>) -> usize {
    let Ok(text) = std::fs::read_to_string(path) else {
        return 0;
    };
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
    scanned
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
fn sweep(needles: &[(String, String)]) -> (Vec<Offence>, usize) {
    let mut offences = Vec::new();
    let mut scanned = 0;
    for (name, src) in certified_roots() {
        let mut files = Vec::new();
        rust_files(&src, &mut files);
        for f in &files {
            scanned += scan(f, &name, needles, &mut offences);
        }
    }
    (offences, scanned)
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
    let (offences, scanned) = sweep(&transcendental_needles());
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
    let (offences, scanned) = sweep(&fma_needles());
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
    scan(&file, "sample", &transcendental_needles(), &mut hits);
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
    scan(&file, "sample", &fma_needles(), &mut fma);
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
fn the_exemptions_are_the_two_that_are_argued_for() {
    // The allowlist is one line long and it must stay that way: an exemption is
    // a hole in a property ADR-0010 calls load-bearing, so growing this list is
    // an ADR rather than an edit.
    assert_eq!(EXEMPT_CRATES, &["fmn-dmath", "fmn-python"]);
    // And the sweep must actually be reaching the crates that matter.
    let names: Vec<String> = certified_roots().into_iter().map(|(n, _)| n).collect();
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
