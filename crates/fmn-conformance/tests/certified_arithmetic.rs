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
/// already identical everywhere. `powi`, `to_degrees`, `to_radians` and `recip`
/// are absent because they lower to multiplication and division — IEEE basic
/// operations, which is property 4 rather than a violation of property 1.
const FORBIDDEN: &[&str] = &[
    "sin", "cos", "sin_cos", "tan", "asin", "acos", "atan", "atan2", "sinh", "cosh", "tanh",
    "asinh", "acosh", "atanh", "exp", "exp2", "exp_m1", "ln", "ln_1p", "log", "log2", "log10",
    "powf", "cbrt", "hypot",
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

/// Strip a trailing `//` comment, honouring string literals.
///
/// A guard that reads comments flags its own documentation: `fill.rs` explains
/// why the disc antiderivative uses `atan2` "because `f64::asin` defers to the
/// platform's libm", and `distance.rs` says the same about `cbrt`. Both
/// sentences are the *reason this file exists* and neither is a call.
///
/// The string-literal handling is not fussiness. A naive `find("//")` truncates
/// at the first `//` anywhere on the line, so one URL in a string would silently
/// blind the rest of that line — and a guard's false *negative* is the failure
/// that matters, because it reads as a pass.
fn code_of(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut in_string = false;
    let mut escaped = false;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            _ if escaped => escaped = false,
            b'\\' if in_string => escaped = true,
            b'"' => in_string = !in_string,
            b'/' if !in_string && i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                return &line[..i];
            }
            _ => {}
        }
        i += 1;
    }
    line
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
fn scan(path: &Path, label: &str, needles: &[(String, String)], out: &mut Vec<Offence>) -> usize {
    let Ok(text) = std::fs::read_to_string(path) else {
        return 0;
    };
    let mut scanned = 0;
    let mut skip_depth: Option<i32> = None;
    let mut skip_started = false;

    for (i, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        let code = code_of(line);

        // Inside a `#[cfg(test)]` item: consume it, then resume.
        if let Some(depth) = skip_depth.as_mut() {
            let opens = code.matches('{').count() as i32;
            let closes = code.matches('}').count() as i32;
            if opens > 0 {
                skip_started = true;
            }
            *depth += opens - closes;
            let ends_block = skip_started && *depth <= 0;
            let ends_statement = !skip_started && code.trim_end().ends_with(';');
            if ends_block || ends_statement {
                skip_depth = None;
                skip_started = false;
            }
            continue;
        }

        if trimmed.starts_with("#[cfg(test)]") {
            skip_depth = Some(0);
            skip_started = false;
            continue;
        }

        scanned += 1;
        for (needle, name) in needles {
            if code.contains(needle.as_str()) {
                out.push(Offence {
                    path: format!("{label}/{}", path.file_name().unwrap().to_string_lossy()),
                    line: i + 1,
                    text: trimmed.to_string(),
                    needle: name.clone(),
                });
            }
        }
    }
    scanned
}

/// The needles for property 1: a method call and both path forms.
fn transcendental_needles() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for name in FORBIDDEN {
        out.push((format!(".{name}("), (*name).to_string()));
        out.push((format!("f64::{name}"), format!("f64::{name}")));
        out.push((format!("f32::{name}"), format!("f32::{name}")));
    }
    out
}

/// The needles for property 2. Assembled rather than written out so this file
/// does not match itself — the spike's version of this guard failed on its own
/// source the first time it ran.
fn fma_needles() -> Vec<(String, String)> {
    let dot = concat!(".mul", "_add(");
    vec![
        (dot.to_string(), "mul_add".to_string()),
        (
            concat!("f64::mul", "_add").to_string(),
            "f64::mul_add".to_string(),
        ),
        (
            concat!("f32::mul", "_add").to_string(),
            "f32::mul_add".to_string(),
        ),
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
let g = t.sqrt();
let h = u.powi(3);
let i = fmn_dmath::sin(v);
let pair = r.sin_cos();
// this comment mentions .sin() and f64::cbrt and must not count
/// nor must this doc comment's `.exp()`
let j = k.to_radians();
let url = \"https://example.invalid/a\"; let leak = q.tanh();
#[cfg(test)]
mod tests {
    fn helper() { let hidden = z.acos(); }
}
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
    //  * the method and path forms are both caught, and reported under distinct
    //    names — `f64::cos(y)` contains no `.cos(`, so it appears once, which is
    //    what makes a failure message point at the right line;
    //  * comments, `sqrt`, `powi`, `to_radians` and a qualified `fmn_dmath::`
    //    call are all legal and must not appear;
    //  * `sin_cos` is covered as a combined libm entry point, and `tanh` IS
    //    caught even though a `//`-bearing string literal precedes it on the
    //    same line — the false-negative class `code_of` now closes;
    //  * `acos` is NOT caught, because it sits inside the `#[cfg(test)] mod`,
    //    while everything after the `#[cfg(test)] use` on line 1 still is —
    //    the coverage hole `bezier.rs` exposed.
    assert_eq!(
        names,
        [
            "atan2", "cbrt", "f64::cos", "powf", "sin", "sin_cos", "tanh"
        ],
        "the transcendental needles do not catch what they must, or catch what \
         they must not"
    );

    let mut fma = Vec::new();
    scan(&file, "sample", &fma_needles(), &mut fma);
    assert_eq!(fma.len(), 1, "the FMA needle missed a hand-written mul_add");

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
