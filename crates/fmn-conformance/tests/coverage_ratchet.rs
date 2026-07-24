//! The coverage ratchet's CI teeth (fm-mol).
//!
//! Always-on (every checkout):
//! - the committed baseline must name the CURRENT `SUITE.lock`
//!   franken_markdown pin — a pin bump without a ratchet re-run fails here;
//! - the dashboard's headline must match the baseline (no drift between
//!   the published page and the enforced counts);
//! - the per-construct named-error audit: every non-tier-1 construct of
//!   the G0-4 table must fail with its precise, tier-tagged error — no
//!   construct ever fails silently.
//!
//! Corpus-bearing environments (`FMN_TEX_CORPUS` set, or the default
//! `corpus/tex_corpus.jsonl` present): recompute all four counts against
//! the frozen denominator, fail on any decrease, and bless deliberate
//! advances with `RATCHET_UPDATE=1` (regenerating baseline, trend, and
//! dashboard in one stroke).

use fmn_conformance::ratchet::{Baseline, Pending, ratchet_violations, render_dashboard};
use std::collections::BTreeMap;
use std::path::PathBuf;

fn repo_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(name)
}

fn repo_file(name: &str) -> String {
    let path = repo_path(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

fn suite_lock_franken_markdown_rev() -> String {
    let lock = repo_file("SUITE.lock");
    for line in lock.lines() {
        let line = line.trim();
        if let Some((rev, _)) = line
            .strip_prefix("franken_markdown\t")
            .and_then(|rest| rest.split_once('\t'))
        {
            return rev.trim().to_owned();
        }
    }
    panic!("SUITE.lock must pin franken_markdown");
}

fn committed_baseline() -> Baseline {
    Baseline::from_tsv(&repo_file("docs/ratchet/baseline.tsv"))
        .unwrap_or_else(|| panic!("docs/ratchet/baseline.tsv is malformed"))
}

// ── Always-on: the pin coupling ─────────────────────────────────────────

#[test]
fn baseline_names_the_current_franken_markdown_pin() {
    let baseline = committed_baseline();
    let pin = suite_lock_franken_markdown_rev();
    assert_eq!(
        baseline.franken_markdown_rev, pin,
        "SUITE.lock moved franken_markdown without a ratchet re-run: \
         recompute with the corpus and RATCHET_UPDATE=1 (docs/ratchet/dashboard.md)"
    );
}

#[test]
fn dashboard_headline_matches_the_baseline() {
    let baseline = committed_baseline();
    let dashboard = repo_file("docs/ratchet/dashboard.md");
    let [po, pu, lo, lu] = baseline.percentages();
    for needle in [
        format!("| **Parse** | {po:.3} % | {pu:.3} % |"),
        format!("| **Parse + layout** | {lo:.3} % | {lu:.3} % |"),
        baseline.corpus_hash.clone(),
    ] {
        assert!(
            dashboard.contains(&needle),
            "dashboard drifted from the baseline (missing `{needle}`); re-bless"
        );
    }
}

#[test]
fn trend_is_monotone_and_ends_at_the_baseline() {
    let baseline = committed_baseline();
    let trend = parse_trend(&repo_file("docs/ratchet/trend.tsv"));
    assert!(
        !trend.is_empty(),
        "the trend log carries at least the first bless"
    );
    for pair in trend.windows(2) {
        assert!(
            ratchet_violations(&pair[0], &pair[1]).is_empty(),
            "the committed trend itself must be monotone"
        );
    }
    let last = trend.last().unwrap_or_else(|| unreachable!());
    assert_eq!(last, &baseline, "the trend's last row is the baseline");
}

// ── Always-on: the per-construct named-error audit ──────────────────────

#[test]
fn every_non_tier1_construct_fails_with_its_named_tiered_error() {
    let table = repo_file("docs/g0/g0-4-corpus/construct_table.tsv");
    let mut audited = 0_usize;
    for line in table.lines() {
        if line.starts_with('#') || line.trim().is_empty() || line.starts_with("rank\t") {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        assert!(fields.len() >= 9, "short row: {line}");
        let construct = fields[1];
        let tier = fields[7];
        if tier != "T2" || construct.starts_with("char:") {
            // char: rows are layout-coverage surface (parse-transparent);
            // T1 support is asserted by fmn-tex's tier-table cross-check.
            continue;
        }
        let probe = probe_source(construct);
        let math_err = fmd_math::parse(&probe).err();
        let text_err = fmd_math::parse_text(&probe).err();
        let hit = [math_err, text_err].into_iter().flatten().find(|e| {
            e.unsupported_construct() == Some(construct)
                && e.to_string().contains("tier T2")
                && e.to_string().contains("fm-j5t")
        });
        assert!(
            hit.is_some(),
            "`{construct}` must fail with its named tier-T2 error in some mode \
             (probe `{probe}`; math: {:?}, text: {:?})",
            fmd_math::parse(&probe).err().map(|e| e.to_string()),
            fmd_math::parse_text(&probe).err().map(|e| e.to_string()),
        );
        audited += 1;
    }
    assert!(
        audited >= 25,
        "the T2 command audit covers the table ({audited})"
    );
}

/// A minimal source string exercising a construct from the table.
fn probe_source(construct: &str) -> String {
    if let Some(env) = construct.strip_prefix("env:") {
        return format!("\\begin{{{env}}} x \\end{{{env}}}");
    }
    // Control words/symbols exercise directly; argument-takers error at
    // lookup before any argument is read, so the bare command suffices.
    construct.to_owned()
}

// ── Corpus-bearing environments: recompute + ratchet + bless ────────────

const TRACK_T2: &str = "franken_manim fm-j5t";
const TRACK_EXT: &str = "franken_manim fm-kg9";
const TRACK_FONTS: &str = "franken_markdown br-…-4vjj (Noto math-alphanumeric subset)";

#[test]
fn recompute_and_enforce_the_ratchet() {
    let corpus_path = std::env::var("FMN_TEX_CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| repo_path("corpus/tex_corpus.jsonl"));
    let Ok(data) = std::fs::read_to_string(&corpus_path) else {
        eprintln!(
            "corpus not present at {} — recompute skipped (the pin-coupling \
             test still enforces re-runs at pin bumps)",
            corpus_path.display()
        );
        return;
    };
    let engine = fmd_math::Engine::bundled().unwrap_or_else(|e| panic!("bundled faces: {e}"));
    let committed = committed_baseline();
    let mut current = Baseline {
        rules_version: committed.rules_version,
        corpus_hash: committed.corpus_hash.clone(),
        franken_markdown_rev: suite_lock_franken_markdown_rev(),
        unique_total: 0,
        occurrence_total: 0,
        parse_unique: 0,
        parse_occurrences: 0,
        layout_unique: 0,
        layout_occurrences: 0,
    };
    let mut parse_pending: BTreeMap<String, u64> = BTreeMap::new();
    let mut layout_pending: BTreeMap<String, u64> = BTreeMap::new();
    for (lineno, line) in data.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let entry =
            parse_entry(line).unwrap_or_else(|| panic!("corpus line {}: bad JSON", lineno + 1));
        current.unique_total += 1;
        current.occurrence_total += entry.count;
        let parse_result = if entry.mode == "text" {
            fmd_math::parse_text(&entry.text).map(|_| ())
        } else {
            fmd_math::parse(&entry.text).map(|_| ())
        };
        match parse_result {
            Ok(()) => {
                current.parse_unique += 1;
                current.parse_occurrences += entry.count;
            }
            Err(e) => {
                let name = e
                    .unsupported_construct()
                    .unwrap_or("(structural)")
                    .to_owned();
                *parse_pending.entry(name).or_insert(0) += entry.count;
                continue;
            }
        }
        let laid = if entry.mode == "text" {
            engine.typeset_text(&entry.text).map(|_| ())
        } else {
            engine
                .typeset(&entry.text, fmd_math::Style::Display)
                .map(|_| ())
        };
        match laid {
            Ok(()) => {
                current.layout_unique += 1;
                current.layout_occurrences += entry.count;
            }
            Err(fmd_math::MathError::UnmappedChar { ch, .. }) => {
                *layout_pending
                    .entry(format!("char:U+{:04X}", ch as u32))
                    .or_insert(0) += entry.count;
            }
            Err(e) => {
                let name = e
                    .unsupported_construct()
                    .unwrap_or("(structural)")
                    .to_owned();
                *layout_pending.entry(name).or_insert(0) += entry.count;
            }
        }
    }
    if committed.unique_total != 0 {
        // The zero-count form exists only as the pre-first-bless seed.
        assert_eq!(
            current.unique_total, committed.unique_total,
            "the denominator is frozen (G0-4)"
        );
        assert_eq!(current.occurrence_total, committed.occurrence_total);
    }
    let violations = ratchet_violations(&committed, &current);
    assert!(
        violations.is_empty(),
        "THE RATCHET: coverage regressed — a deliberate tier change updates \
         the baseline with a written note, an accident gets fixed:\n{}",
        violations.join("\n")
    );
    let advanced = current != committed;
    if std::env::var("RATCHET_UPDATE").is_ok() {
        bless(&current, &parse_pending, &layout_pending);
        eprintln!("ratchet blessed at {}", current.franken_markdown_rev);
    } else if advanced {
        panic!(
            "coverage advanced (or the pin moved) — bless deliberately with \
             RATCHET_UPDATE=1 so the public artifacts move in the same commit"
        );
    }
    let [po, pu, lo, lu] = current.percentages();
    eprintln!("ratchet: parse {po:.3}%/{pu:.3}% · layout {lo:.3}%/{lu:.3}%");
}

fn bless(
    current: &Baseline,
    parse_pending: &BTreeMap<String, u64>,
    layout_pending: &BTreeMap<String, u64>,
) {
    let dir = repo_path("docs/ratchet");
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("mkdir: {e}"));
    // Trend: append (or start) — keyed by rev; re-blessing the same rev
    // replaces its row.
    let trend_path = dir.join("trend.tsv");
    let mut trend = std::fs::read_to_string(&trend_path)
        .map(|t| parse_trend(&t))
        .unwrap_or_default();
    trend.retain(|b| b.franken_markdown_rev != current.franken_markdown_rev);
    trend.push(current.clone());
    let mut trend_out = String::from(
        "# ratchet trend (fm-mol): one row per bless, keyed by franken_markdown rev\n\
         # rev\trules\thash\tuniq_total\tocc_total\tparse_uniq\tparse_occ\tlayout_uniq\tlayout_occ\n",
    );
    for b in &trend {
        trend_out.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            b.franken_markdown_rev,
            b.rules_version,
            b.corpus_hash,
            b.unique_total,
            b.occurrence_total,
            b.parse_unique,
            b.parse_occurrences,
            b.layout_unique,
            b.layout_occurrences,
        ));
    }
    std::fs::write(&trend_path, trend_out).unwrap_or_else(|e| panic!("trend: {e}"));
    std::fs::write(dir.join("baseline.tsv"), current.to_tsv())
        .unwrap_or_else(|e| panic!("baseline: {e}"));
    let to_pending = |m: &BTreeMap<String, u64>| -> Vec<Pending> {
        let mut v: Vec<Pending> = m
            .iter()
            .map(|(construct, occ)| Pending {
                construct: construct.clone(),
                occurrences: *occ,
                tracked: track_of(construct).to_owned(),
            })
            .collect();
        v.sort_by(|a, b| {
            b.occurrences
                .cmp(&a.occurrences)
                .then(a.construct.cmp(&b.construct))
        });
        v
    };
    let dashboard = render_dashboard(
        current,
        &to_pending(parse_pending),
        &to_pending(layout_pending),
        &trend,
    );
    std::fs::write(dir.join("dashboard.md"), dashboard)
        .unwrap_or_else(|e| panic!("dashboard: {e}"));
}

fn track_of(construct: &str) -> &'static str {
    if construct.starts_with("char:") {
        TRACK_FONTS
    } else if construct.starts_with("env:")
        || matches!(
            construct,
            "\\overbrace"
                | "\\underbrace"
                | "\\overrightarrow"
                | "\\overleftarrow"
                | "\\widehat"
                | "\\widetilde"
        )
    {
        TRACK_EXT
    } else {
        TRACK_T2
    }
}

fn parse_trend(text: &str) -> Vec<Baseline> {
    text.lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            if f.len() < 9 {
                return None;
            }
            Some(Baseline {
                franken_markdown_rev: f[0].to_owned(),
                rules_version: f[1].parse().ok()?,
                corpus_hash: f[2].to_owned(),
                unique_total: f[3].parse().ok()?,
                occurrence_total: f[4].parse().ok()?,
                parse_unique: f[5].parse().ok()?,
                parse_occurrences: f[6].parse().ok()?,
                layout_unique: f[7].parse().ok()?,
                layout_occurrences: f[8].parse().ok()?,
            })
        })
        .collect()
}

// ── A minimal JSON-object reader for the corpus lines (governed closure:
//    no serde) ──────────────────────────────────────────────────────────

struct Entry {
    mode: String,
    text: String,
    count: u64,
}

fn parse_entry(line: &str) -> Option<Entry> {
    let mut mode = None;
    let mut text = None;
    let mut count = None;
    let bytes = line.as_bytes();
    let mut i = skip_ws(bytes, 0);
    if bytes.get(i) != Some(&b'{') {
        return None;
    }
    i += 1;
    loop {
        i = skip_ws(bytes, i);
        match bytes.get(i) {
            Some(b'}') => break,
            Some(b',') => {
                i += 1;
                continue;
            }
            Some(b'"') => {}
            _ => return None,
        }
        let (key, ni) = read_string(line, i)?;
        i = skip_ws(bytes, ni);
        if bytes.get(i) != Some(&b':') {
            return None;
        }
        i = skip_ws(bytes, i + 1);
        match key.as_str() {
            "mode" => {
                let (v, ni) = read_string(line, i)?;
                mode = Some(v);
                i = ni;
            }
            "text" => {
                let (v, ni) = read_string(line, i)?;
                text = Some(v);
                i = ni;
            }
            "count" => {
                let (v, ni) = read_number(bytes, i)?;
                count = Some(v);
                i = ni;
            }
            _ => i = skip_value(line, i)?,
        }
    }
    Some(Entry {
        mode: mode?,
        text: text?,
        count: count?,
    })
}

fn skip_ws(bytes: &[u8], mut i: usize) -> usize {
    while matches!(bytes.get(i), Some(b' ' | b'\t' | b'\n' | b'\r')) {
        i += 1;
    }
    i
}

fn read_string(s: &str, start: usize) -> Option<(String, usize)> {
    let bytes = s.as_bytes();
    if bytes.get(start) != Some(&b'"') {
        return None;
    }
    let mut out = String::new();
    let mut i = start + 1;
    loop {
        let rest = s.get(i..)?;
        let mut chars = rest.char_indices();
        let (_, c) = chars.next()?;
        match c {
            '"' => return Some((out, i + 1)),
            '\\' => {
                let (_, esc) = chars.next()?;
                i += 1 + esc.len_utf8();
                match esc {
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    '/' => out.push('/'),
                    'b' => out.push('\u{0008}'),
                    'f' => out.push('\u{000C}'),
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    'u' => {
                        let hex = s.get(i..i + 4)?;
                        let cp = u32::from_str_radix(hex, 16).ok()?;
                        i += 4;
                        if (0xD800..0xDC00).contains(&cp) {
                            if s.get(i..i + 2)? != "\\u" {
                                return None;
                            }
                            let lo = u32::from_str_radix(s.get(i + 2..i + 6)?, 16).ok()?;
                            i += 6;
                            let combined =
                                0x10000 + ((cp - 0xD800) << 10) + lo.checked_sub(0xDC00)?;
                            out.push(char::from_u32(combined)?);
                        } else {
                            out.push(char::from_u32(cp)?);
                        }
                    }
                    _ => return None,
                }
            }
            other => {
                out.push(other);
                i += other.len_utf8();
            }
        }
    }
}

fn read_number(bytes: &[u8], start: usize) -> Option<(u64, usize)> {
    let mut i = start;
    let mut val: u64 = 0;
    let mut any = false;
    while let Some(d) = bytes.get(i).copied().filter(u8::is_ascii_digit) {
        val = val.checked_mul(10)?.checked_add(u64::from(d - b'0'))?;
        i += 1;
        any = true;
    }
    any.then_some((val, i))
}

fn skip_value(s: &str, start: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    match bytes.get(start)? {
        b'"' => read_string(s, start).map(|(_, i)| i),
        b'[' => {
            let mut depth = 0_usize;
            let mut i = start;
            loop {
                match bytes.get(i)? {
                    b'"' => i = read_string(s, i)?.1,
                    b'[' => {
                        depth += 1;
                        i += 1;
                    }
                    b']' => {
                        depth -= 1;
                        i += 1;
                        if depth == 0 {
                            return Some(i);
                        }
                    }
                    _ => i += 1,
                }
            }
        }
        _ => {
            let mut i = start;
            while !matches!(bytes.get(i), None | Some(b',' | b'}' | b']')) {
                i += 1;
            }
            Some(i)
        }
    }
}
