//! Reference width oracle for native `Tex` (fm-tex-metrics-glyphs-ru72).
//!
//! `fixtures/tex_reference_widths.v1.tsv` holds the pinned Reference's own
//! `Tex(source)` width and height in scene units at font_size 48, captured by
//! `scripts/capture_tex_reference_widths.py`. The rows are:
//! - the ten formulas the bead first measured, named by source;
//! - the 200 most frequent corpus math strings, named only by their public
//!   digest (plan §15.3).
//!
//! A row's class comes from the fonts its DVI loads:
//! - `plain`: TeX set it in Computer Modern.
//! - `fallback`: it also loaded another font (msam, msbm, dsrom, ...), whose
//!   glyphs fmd-math draws from a fallback face.
//! - `traced`: only EC text fonts. The capture host has no outlines for them,
//!   so its dvisvgm traced them from METAFONT bitmaps (BN-05).
//!
//! Native `Tex` must hold every row's width within 3% (plain) or 6%
//! (fallback). The exceptions are named:
//! - Rows where the Reference itself is wrong are excluded. Appendix C-27:
//!   tipa breaks `\;`, `\:` and `\!`. A traced row's ink is a bitmap trace:
//!   its `\vdots` dots measure 0.120 em against Computer Modern's 0.106.
//! - A row an open bead owns must still be out of tolerance, so the exception
//!   goes the day the bead lands.
//!
//! Corpus rows need the private corpus (`FMN_TEX_CORPUS`, or
//! `corpus/tex_corpus.jsonl`); without it only the authored rows run. A
//! failing row logs its source, both widths and fmd-math's per-glyph layout.
#![forbid(unsafe_code)]
#![allow(clippy::expect_used, clippy::panic, clippy::print_stderr)]

use fmn_conformance::ratchet::{corpus_digest, parse_corpus_entry};
use fmn_library::Tex;
use fmn_tex::TexEngine;
use std::collections::HashMap;
use std::path::PathBuf;

const FIXTURE: &str = include_str!("../fixtures/tex_reference_widths.v1.tsv");
const PLAIN_TOLERANCE: f64 = 0.03;
const FALLBACK_TOLERANCE: f64 = 0.06;

/// Rows an open bead owns, by source (authored) or digest prefix (corpus):
/// each must still miss its tolerance. Empty since `\Longrightarrow` is
/// LaTeX's \Relbar\joinrel\Rightarrow (fm-fm-long-arrows-joinrel-r11-b2c5,
/// franken_markdown 094d4f6).
const OWNED: &[(&str, &str)] = &[];

struct Row<'a> {
    kind: &'a str,
    key: &'a str,
    count: u64,
    width: f64,
    class: &'a str,
}

fn rows() -> Vec<Row<'static>> {
    FIXTURE
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            assert!(fields.len() == 7, "seven fields: {line}");
            Row {
                kind: fields[0],
                key: fields[1],
                count: fields[2].parse().expect("count"),
                width: fields[3].parse().expect("reference width"),
                class: match fields[5] {
                    class @ ("plain" | "fallback" | "traced") => class,
                    other => panic!("unknown class {other}"),
                },
            }
        })
        .collect()
}

/// Appendix C-27: under the Reference preamble tipa redefines `\;`, `\:` and
/// `\!` as text commands, so the Reference's box for such a string is wrong.
fn reference_defect(source: &str) -> bool {
    [r"\;", r"\:", r"\!"]
        .iter()
        .any(|space| source.contains(space))
}

fn owner(row: &Row<'_>) -> Option<&'static str> {
    OWNED
        .iter()
        .find(|(key, _)| {
            if row.kind == "authored" {
                row.key == *key
            } else {
                row.key.starts_with(key)
            }
        })
        .map(|&(_, bead)| bead)
}

#[test]
fn fixture_names_corpus_strings_only_by_digest() {
    let rows = rows();
    assert!(rows.iter().filter(|row| row.kind == "authored").count() == 10);
    assert!(rows.iter().filter(|row| row.kind == "corpus").count() >= 150);
    for row in rows.iter().filter(|row| row.kind == "corpus") {
        assert!(
            row.key.len() == 64 && row.key.bytes().all(|b| b.is_ascii_hexdigit()),
            "a corpus row names only a digest: {}",
            row.key
        );
        assert!(row.count > 0);
    }
}

#[test]
fn native_tex_widths_hold_the_reference() {
    let path = std::env::var("FMN_TEX_CORPUS").map_or_else(
        |_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus/tex_corpus.jsonl"),
        PathBuf::from,
    );
    let texts: HashMap<String, String> = std::fs::read_to_string(&path)
        .map(|corpus| {
            corpus
                .lines()
                .filter_map(parse_corpus_entry)
                .filter(|entry| entry.mode == "math")
                .map(|entry| (corpus_digest("math", &entry.text), entry.text))
                .collect()
        })
        .unwrap_or_default();
    let engine = TexEngine::new("fmd-math/pack/default", None).expect("engine");
    let (mut checked, mut excluded, mut skipped) = (0_usize, 0_usize, 0_usize);
    let mut failures = Vec::new();
    for row in rows() {
        let source = if row.kind == "authored" {
            row.key.to_owned()
        } else if let Some(text) = texts.get(row.key) {
            text.clone()
        } else {
            skipped += 1;
            continue;
        };
        if reference_defect(&source) || row.class == "traced" {
            excluded += 1;
            continue;
        }
        let tolerance = if row.class == "fallback" {
            FALLBACK_TOLERANCE
        } else {
            PLAIN_TOLERANCE
        };
        let (error, trace) = match Tex::new(&source).build(&engine) {
            Ok(tex) => {
                let (min, max) = tex.vmob.extent().expect("a typeset formula has extent");
                (
                    (max[0] - min[0]) / row.width - 1.0,
                    fmd_math::paths::layout_dump(&tex.typeset.layout),
                )
            }
            Err(error) => (f64::INFINITY, format!("refused: {error}")),
        };
        checked += 1;
        let within = error.abs() <= tolerance;
        match (owner(&row), within) {
            (None, true) | (Some(_), false) => {}
            (None, false) => failures.push(format!(
                "{} {:?} (x{}): Reference width {:.6}, native {:+.2}% (tolerance {:.0}%)\n{trace}",
                row.kind,
                source,
                row.count,
                row.width,
                error * 100.0,
                tolerance * 100.0
            )),
            (Some(bead), true) => failures.push(format!(
                "{:?} is within tolerance now ({:+.2}%): drop its {bead} exception",
                source,
                error * 100.0
            )),
        }
    }
    eprintln!(
        "tex reference widths: {checked} rows checked, {excluded} Reference defects excluded, \
         {skipped} corpus rows skipped without the corpus, {} out of tolerance",
        failures.len()
    );
    assert!(
        texts.is_empty() || skipped == 0,
        "the corpus is present but {skipped} fixture digests are not in it"
    );
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
