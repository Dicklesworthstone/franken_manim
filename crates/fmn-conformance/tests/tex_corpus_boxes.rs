//! Corpus-wide TeX box oracle for fmd-math layout (fm-tex-layout-oracle-bkbc).
//!
//! `fixtures/tex_corpus_boxes.v1.tsv` holds real TeX's box (width, height and
//! depth in ems, display style, under the pinned Reference's preamble) for a
//! stratified sample of the private TeX corpus. Each string is named only by
//! its public digest (plan §15.3); `scripts/capture_tex_corpus_boxes.py`
//! captured them.
//!
//! Where the corpus is present (`FMN_TEX_CORPUS`, or `corpus/tex_corpus.jsonl`),
//! each sampled string is laid out natively. A string agrees when its width,
//! and its height and depth relative to the total, are within the tolerance.
//! The counts within 5% and 10% may only rise. The per-construct-class table
//! is printed worst first, naming rows by digest only, so each run says what
//! to fix next. Without the corpus, only the fixture's shape is checked.
#![forbid(unsafe_code)]
#![allow(clippy::expect_used, clippy::panic, clippy::print_stderr)]

use fmn_conformance::ratchet::{corpus_digest, parse_corpus_entry};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

/// Sampled strings within 5% of TeX's box: 188 of 563 when this oracle landed
/// (franken_markdown a8aab0d, 2026-10-09). Raise it when layout improves;
/// never lower it to land a change.
const WITHIN_5_PERCENT_FLOOR: usize = 188;
/// The same at 10%: 312.
const WITHIN_10_PERCENT_FLOOR: usize = 312;

struct Row {
    digest: &'static str,
    width: f64,
    height: f64,
    depth: f64,
    count: u64,
    classes: Vec<&'static str>,
}

fn rows() -> Vec<Row> {
    include_str!("../fixtures/tex_corpus_boxes.v1.tsv")
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            assert_eq!(fields.len(), 6, "malformed fixture row {line:?}");
            let number = |i: usize| -> f64 { fields[i].parse().expect("a TeX dimension") };
            Row {
                digest: fields[0],
                width: number(1),
                height: number(2),
                depth: number(3),
                count: fields[4].parse().expect("occurrence count"),
                // `|` appears in no construct name (`\,` and `\ ` do).
                classes: fields[5].split('|').collect(),
            }
        })
        .collect()
}

#[test]
fn fixture_names_corpus_strings_only_by_digest() {
    let rows = rows();
    assert!(
        rows.len() >= 500,
        "the sample thinned out: {} rows",
        rows.len()
    );
    let mut seen = HashSet::new();
    for row in &rows {
        assert!(
            row.digest.len() == 64
                && row
                    .digest
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "not a lowercase sha256: {}",
            row.digest
        );
        assert!(seen.insert(row.digest), "duplicate row {}", row.digest);
        assert!(row.width > 0.0 && row.height + row.depth > 0.0 && row.count > 0);
        assert!(row.height >= 0.0 && row.depth >= 0.0);
        assert!(!row.classes.is_empty() && row.classes.iter().all(|c| !c.is_empty()));
    }
}

/// Per-class tallies: strings, within 5%, within 10%, worst error, worst row.
#[derive(Default)]
struct Class {
    rows: usize,
    within_5: usize,
    within_10: usize,
    worst: f64,
    worst_digest: &'static str,
}

/// The larger of the relative width error and the height and depth errors
/// relative to TeX's total height, so a misplaced baseline counts too.
/// Multi-line strings (`\\` or `&`) compare width and total height only: TeX
/// boxed them as a vertically centred `aligned`, while the Reference's
/// align* surface keeps its first baseline, and only the ink is shown.
fn box_error(row: &Row, layout: &fmd_math::Layout, multiline: bool) -> f64 {
    let total = row.height + row.depth;
    let width = (layout.width / row.width - 1.0).abs();
    if multiline {
        return width.max((layout.height + layout.depth - total).abs() / total);
    }
    let height = (layout.height - row.height).abs() / total;
    let depth = (layout.depth - row.depth).abs() / total;
    width.max(height).max(depth)
}

#[test]
fn corpus_boxes_track_tex_and_never_regress() {
    let path = std::env::var("FMN_TEX_CORPUS").map_or_else(
        |_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus/tex_corpus.jsonl"),
        PathBuf::from,
    );
    let Ok(corpus) = std::fs::read_to_string(&path) else {
        fmn_core::test_inputs::skip_or_fail(
            "fmn-conformance::corpus_boxes_track_tex_and_never_regress",
            &format!("TeX corpus at {}", path.display()),
        );
        return;
    };
    let texts: HashMap<String, String> = corpus
        .lines()
        .filter_map(parse_corpus_entry)
        .filter(|entry| entry.mode == "math")
        .map(|entry| (corpus_digest("math", &entry.text), entry.text))
        .collect();
    let engine = fmd_math::Engine::bundled().expect("bundled faces");
    let pack = fmd_math::MacroSet::pack("fmd-math/pack/default").expect("default pack");
    let rows = rows();
    let mut classes: BTreeMap<&str, Class> = BTreeMap::new();
    let (mut five, mut ten, mut errors) = (0_usize, 0_usize, 0_usize);
    let (mut weight, mut weight_five) = (0_u64, 0_u64);
    for row in &rows {
        let text = texts
            .get(row.digest)
            .unwrap_or_else(|| panic!("fixture row {} is not in {}", row.digest, path.display()));
        let error = match engine.typeset_with_macros(text, fmd_math::Style::Display, &pack) {
            Ok(layout) => box_error(row, &layout, text.contains("\\\\") || text.contains('&')),
            Err(_) => {
                errors += 1;
                f64::INFINITY
            }
        };
        weight += row.count;
        five += usize::from(error <= 0.05);
        ten += usize::from(error <= 0.10);
        weight_five += if error <= 0.05 { row.count } else { 0 };
        for class in &row.classes {
            let tally = classes.entry(class).or_default();
            tally.rows += 1;
            tally.within_5 += usize::from(error <= 0.05);
            tally.within_10 += usize::from(error <= 0.10);
            if error >= tally.worst {
                tally.worst = error;
                tally.worst_digest = row.digest;
            }
        }
    }
    eprintln!(
        "tex corpus boxes: {five}/{n} within 5%, {ten}/{n} within 10%, {errors} not typeset; \
         {weight_five}/{weight} occurrences within 5% (floors {WITHIN_5_PERCENT_FLOOR}, \
         {WITHIN_10_PERCENT_FLOOR})",
        n = rows.len()
    );
    // Classes with the lowest within-5% share first: what to fix next.
    let mut table: Vec<(&&str, &Class)> = classes.iter().collect();
    table.sort_by(|a, b| {
        let share = |c: &Class| c.within_5 as f64 / c.rows as f64;
        share(a.1)
            .total_cmp(&share(b.1))
            .then(b.1.rows.cmp(&a.1.rows))
    });
    eprintln!("  class\trows\twithin5\twithin10\tworst\tworst_row");
    for (name, c) in table {
        eprintln!(
            "  {name}\t{}\t{}\t{}\t{:.3}\t{}",
            c.rows,
            c.within_5,
            c.within_10,
            c.worst,
            &c.worst_digest[..12]
        );
    }
    assert!(
        five >= WITHIN_5_PERCENT_FLOOR,
        "{five} sampled strings within 5% of TeX, below the floor {WITHIN_5_PERCENT_FLOOR}"
    );
    assert!(
        ten >= WITHIN_10_PERCENT_FLOOR,
        "{ten} sampled strings within 10% of TeX, below the floor {WITHIN_10_PERCENT_FLOOR}"
    );
}
