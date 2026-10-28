//! Corpus-wide TeX box oracle for fmd-math layout (fm-tex-layout-oracle-bkbc).
//!
//! `fixtures/tex_corpus_boxes_census.v1.tsv` holds real TeX's box (width,
//! height and depth in ems, display style, under the pinned Reference's
//! preamble) for every math-mode string of the private TeX corpus that TeX can
//! typeset alone (fm-box-oracle-full-corpus-r11-lykz).
//! `fixtures/tex_corpus_boxes.v1.tsv` is its stratified sample, row for row.
//! `fixtures/tex_text_boxes_census.v1.tsv` holds TeX's box for every
//! text-mode string the Reference sets on one line, laid out natively through
//! the TexText contract (fm-texttext-box-oracle-r11-hkdv).
//! `fixtures/tex_text_paragraphs_census.v1.tsv` holds TeX's paragraph for every
//! other text-mode string, multi-line or wider than the Reference's 345 pt
//! line: its line count, widest line and box, laid out natively as TexText's
//! default `\centering` paragraph (fm-texttext-line-breaking-r11-bfdx).
//! Each string is named only by its public digest (plan §15.3); one run of
//! `scripts/capture_tex_corpus_boxes.py` captures them all. The evaluation lives in
//! `fmn_conformance::ratchet`, which the coverage ratchet's dashboard shares.
//!
//! Where the corpus is present (`FMN_TEX_CORPUS`, or `corpus/tex_corpus.jsonl`),
//! every census string is laid out natively. A string agrees when its width,
//! and its height and depth relative to the total, are within the tolerance.
//! The census counts and the sample's within 5% and 10% may only rise, and
//! `docs/ratchet/dashboard.md` must publish the census counts. The run logs
//! bounded NDJSON (schema `fmn.tex-corpus-boxes.v1`): a record per census row
//! with TeX's and the native box, a record per construct class with the worst
//! first, and a summary with the decision. Without the corpus, only the
//! fixtures' shape and the sample's agreement with the census are checked.
#![forbid(unsafe_code)]
#![allow(clippy::expect_used, clippy::panic, clippy::print_stderr)]

use fmn_conformance::ratchet::{
    CENSUS_FIXTURE, ORACLE_FIXTURE, OracleRow, PARAGRAPH_CENSUS_FIXTURE, TEXT_CENSUS_FIXTURE,
    corpus_digest, oracle_dashboard_line, oracle_error, oracle_ndjson,
    paragraph_oracle_dashboard_line, parse_corpus_entry, parse_oracle_fixture,
    parse_paragraph_fixture, run_layout_oracle, run_paragraph_oracle, run_text_layout_oracle,
    summarize_oracle, text_oracle_dashboard_line,
};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// Sampled strings within 5% of TeX's box: 188 of 563 when this oracle was
/// first measured (franken_markdown a8aab0d), 189 once the Tex surface's rows
/// opened up by \jot (d664c13), 197 of 585 once the capture set strings as
/// the Reference's align* does (stripped, braced, multi-line rows on their
/// first baseline), 233 once fractions carried TeX's null delimiters and
/// \vdots, \ddots and trailing `\\` rows were TeX's (e4e8e2e), 235 once the
/// Tex surface's rows were align* rows (798de11), 252 with TeX's accent
/// geometry, \big sizes and array struts (bd00a24), 279 with TeX's
/// \scriptspace, cmsy's prime character and LaTeX's brace fills (3ed1d33),
/// 281 with delimiters at the sizes TeX's variant search sets (e0dd66e),
/// 299 with their cmex10 box widths (29385b3), 303 with big operators'
/// cmex10 widths and italic corrections (80cbadc), 489 in TeX's own CM math
/// faces (23c4d0c, ADR-0028), 540 laid out from TeX's TFM metrics with clean
/// boxes that keep a lone character's italic correction (216c7c3), 543 with
/// rule 18a's drops read at the script size (4db6f43, fm-4ptw), 552 with
/// authored AMS symbols and the native default-pack minus (9b457a38, fm-0byq),
/// 556 with amsmath's \mod kern (4b705d14, fm-eo8l), captured with
/// \if@display set as the Reference's align* sets it, 559 with amsmath's
/// \dddot as an operator under a period row (1ffcadc9, fm-ew7o), 565 with
/// the long arrows from CM pieces and amsmath's dots look-ahead (d99fee96,
/// fm-fm-long-arrows-joinrel-r11-b2c5, fm-fm-amsmath-dots-context-r11-ymdd),
/// 579 when the capture dropped tipa, which breaks math-mode `\!`, `\;` and
/// `\:` in the Reference's preamble (Appendix C-41,
/// fm-reference-tipa-math-spacing-r12-9ujn).
/// Raise it when layout improves; never lower it to land a change.
const WITHIN_5_PERCENT_FLOOR: usize = 579;
/// The same at 10%: 315, 316, 330, 365, 367, 385, 421, 423, 437, 444, 543,
/// 564, 565, 570 with the AMS/default-pack correction (9b457a38), 573 with
/// amsmath's \mod (4b705d14), 574 with its \dddot (1ffcadc9), then 578 with
/// the long arrows and the dots (d99fee96), 581 without tipa (C-41).
const WITHIN_10_PERCENT_FLOOR: usize = 581;

/// Census strings within 5% of TeX's box, of the 4175 math-mode corpus strings
/// TeX typesets alone: 4132 when the census was first measured
/// (franken_markdown 8667819a), covering 11107 of its 11183 occurrences. The
/// stratified sample's 559 of 585 understates it, since the sample takes four
/// strings of every construct class however rare. 4139 (11130 occurrences)
/// with the long arrows from CM pieces and amsmath's dots look-ahead
/// (d99fee96): five arrow and three `(\dots)` rows enter, and `\sin(\dots)`
/// leaves, since physics' `\sin(...)` hides the `)` from `\dots`
/// (fm-fm-physics-trig-parens-r11-nou8). 4146 (11139 occurrences) with
/// physics' function arguments in the default pack (cc470d1a): the six
/// auto-sized `\cos(...)` rows and `\sin(\dots)` enter, and none leaves.
/// 4149 (11142 occurrences) with TeX's delimiter size chain, an unscaled
/// \nulldelimiterspace and the radical bar at its sign's height (04d811db,
/// fm-fm-delim-size-chain-r11-khsh): the three `\left(…\right)` superscripts
/// enter, fourteen script-fraction rows tighten to TeX's box, and none leaves.
/// 4166 (11171 occurrences) when the capture dropped tipa (Appendix C-41,
/// fm-reference-tipa-math-spacing-r12-9ujn): the recapture changed exactly
/// the 22 rows whose strings use `\!`, `\;` or `\:`, 17 of them enter, and
/// none leaves.
/// Raise it when layout improves; never lower it to land a change.
const CENSUS_WITHIN_5_PERCENT_FLOOR: usize = 4166;
/// The same at 10%: 4157 at 8667819a, 4161 at d99fee96, 4166 at cc470d1a,
/// 4167 at 04d811db, 4171 without tipa (C-41).
const CENSUS_WITHIN_10_PERCENT_FLOOR: usize = 4171;

/// Text-mode census strings within 5% of TeX's box, of the 3747 text-mode
/// corpus strings the Reference sets on one line
/// (fm-texttext-box-oracle-r11-hkdv): 3733 when first measured (franken_markdown
/// cc470d1a), covering 4666 of their 4689 occurrences. Of the 14 misses, six
/// are C-27's tipa `\;` and `\!` (deliberately not replicated), two are the
/// EC text fonts' kerning (fm-q364), and two are text-mode `\dots`
/// (`\textellipsis`). Raise it when layout improves; never lower it to land
/// a change.
const TEXT_CENSUS_WITHIN_5_PERCENT_FLOOR: usize = 3733;
/// The same at 10%: 3743 at cc470d1a.
const TEXT_CENSUS_WITHIN_10_PERCENT_FLOOR: usize = 3743;

/// Paragraph census strings within 5% of TeX (the widest line, the first
/// line's height and the depth to the last line), of the 925 text-mode corpus
/// strings that are multi-line or wider than the Reference's 345 pt line and
/// that TeX typesets alone (fm-texttext-line-breaking-r11-bfdx): 915 when
/// first measured (franken_markdown 450111a4), covering 946 of their 956
/// occurrences. Of the 10 misses, eight set display mathematics (`$$…$$`,
/// `align*`) inside the paragraph, which TeX sets between display skips and
/// fmd-math sets inline; one is `\Large`'s optical size and one the EC fonts'
/// kerning (fm-q364). Raise it when layout improves; never lower it to land
/// a change.
const PARAGRAPH_CENSUS_WITHIN_5_PERCENT_FLOOR: usize = 915;
/// The same at 10%: 916 at 450111a4.
const PARAGRAPH_CENSUS_WITHIN_10_PERCENT_FLOOR: usize = 916;

fn rows() -> Vec<OracleRow> {
    parse_oracle_fixture(ORACLE_FIXTURE).expect("the oracle fixture parses")
}

fn census_rows() -> Vec<OracleRow> {
    parse_oracle_fixture(CENSUS_FIXTURE).expect("the census fixture parses")
}

fn text_census_rows() -> Vec<OracleRow> {
    parse_oracle_fixture(TEXT_CENSUS_FIXTURE).expect("the text census fixture parses")
}

fn paragraph_rows() -> Vec<(OracleRow, usize)> {
    parse_paragraph_fixture(PARAGRAPH_CENSUS_FIXTURE).expect("the paragraph census fixture parses")
}

#[test]
fn fixture_names_corpus_strings_only_by_digest() {
    let rows = rows();
    let census = census_rows();
    let text_census = text_census_rows();
    assert!(
        rows.len() >= 500,
        "the sample thinned out: {} rows",
        rows.len()
    );
    assert!(
        census.len() >= 4000,
        "the census thinned out: {} rows",
        census.len()
    );
    assert!(
        text_census.len() >= 3500,
        "the text census thinned out: {} rows",
        text_census.len()
    );
    let paragraphs = paragraph_rows();
    assert!(
        paragraphs.len() >= 800,
        "the paragraph census thinned out: {} rows",
        paragraphs.len()
    );
    assert!(paragraphs.iter().all(|(_, lines)| *lines >= 1));
    // A paragraph census string is never also a one-line census string.
    let one_line: HashSet<&str> = text_census.iter().map(|row| row.digest.as_str()).collect();
    assert!(
        paragraphs
            .iter()
            .all(|(row, _)| !one_line.contains(row.digest.as_str()))
    );
    let paragraphs: Vec<OracleRow> = paragraphs.into_iter().map(|(row, _)| row).collect();
    for fixture in [&rows, &census, &text_census, &paragraphs] {
        assert_rows_are_digest_only(fixture);
    }
}

/// The text-mode census against fmd-math's TexText layout under the default
/// pack, with the private corpus where it is present (as the math census).
#[test]
fn corpus_text_boxes_track_tex_and_never_regress() {
    let path = std::env::var("FMN_TEX_CORPUS").map_or_else(
        |_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus/tex_corpus.jsonl"),
        PathBuf::from,
    );
    let Ok(corpus) = std::fs::read_to_string(&path) else {
        fmn_core::test_inputs::skip_or_fail(
            "fmn-conformance::corpus_text_boxes_track_tex_and_never_regress",
            &format!("TeX corpus at {}", path.display()),
        );
        return;
    };
    let texts: HashMap<String, String> = corpus
        .lines()
        .filter_map(parse_corpus_entry)
        .filter(|entry| entry.mode == "text")
        .map(|entry| (corpus_digest("text", &entry.text), entry.text))
        .collect();
    let census = text_census_rows();
    let laid =
        run_text_layout_oracle(&census, &texts).expect("every text census string is in the corpus");
    let errors: Vec<f64> = census
        .iter()
        .zip(&laid)
        .map(|(row, &laid)| oracle_error(row, laid))
        .collect();
    eprint!(
        "{}",
        oracle_ndjson(
            &census,
            &laid,
            &errors,
            (
                TEXT_CENSUS_WITHIN_5_PERCENT_FLOOR,
                TEXT_CENSUS_WITHIN_10_PERCENT_FLOOR
            )
        )
    );
    let summary = summarize_oracle(&census, &errors);
    eprintln!(
        "{{\"schema\":\"fmn.tex-corpus-boxes.v1\",\"kind\":\"text_summary\",\"rows\":{},\"within_5\":{},\"within_10\":{},\"refused\":{},\"occurrences\":{},\"occurrences_within_5\":{}}}",
        summary.rows,
        summary.within_5,
        summary.within_10,
        summary.refused,
        summary.occurrences,
        summary.occurrences_within_5
    );
    assert!(
        summary.within_5 >= TEXT_CENSUS_WITHIN_5_PERCENT_FLOOR,
        "{} text strings within 5% of TeX, below the floor {TEXT_CENSUS_WITHIN_5_PERCENT_FLOOR}",
        summary.within_5
    );
    assert!(
        summary.within_10 >= TEXT_CENSUS_WITHIN_10_PERCENT_FLOOR,
        "{} text strings within 10% of TeX, below the floor {TEXT_CENSUS_WITHIN_10_PERCENT_FLOOR}",
        summary.within_10
    );
    // The paragraph census: the widest line, the first line's height and the
    // depth to the last line of each multi-line or wrapping string's centred
    // paragraph.
    let paragraphs: Vec<OracleRow> = paragraph_rows().into_iter().map(|(row, _)| row).collect();
    let laid = run_paragraph_oracle(&paragraphs, &texts)
        .expect("every paragraph census string is in the corpus");
    let errors: Vec<f64> = paragraphs
        .iter()
        .zip(&laid)
        .map(|(row, &laid)| oracle_error(row, laid))
        .collect();
    eprint!(
        "{}",
        oracle_ndjson(
            &paragraphs,
            &laid,
            &errors,
            (
                PARAGRAPH_CENSUS_WITHIN_5_PERCENT_FLOOR,
                PARAGRAPH_CENSUS_WITHIN_10_PERCENT_FLOOR
            )
        )
    );
    let paragraph_summary = summarize_oracle(&paragraphs, &errors);
    eprintln!(
        "{{\"schema\":\"fmn.tex-corpus-boxes.v1\",\"kind\":\"paragraph_summary\",\"rows\":{},\"within_5\":{},\"within_10\":{},\"refused\":{},\"occurrences\":{},\"occurrences_within_5\":{}}}",
        paragraph_summary.rows,
        paragraph_summary.within_5,
        paragraph_summary.within_10,
        paragraph_summary.refused,
        paragraph_summary.occurrences,
        paragraph_summary.occurrences_within_5
    );
    assert!(
        paragraph_summary.within_5 >= PARAGRAPH_CENSUS_WITHIN_5_PERCENT_FLOOR,
        "{} paragraphs within 5% of TeX, below the floor {PARAGRAPH_CENSUS_WITHIN_5_PERCENT_FLOOR}",
        paragraph_summary.within_5
    );
    assert!(
        paragraph_summary.within_10 >= PARAGRAPH_CENSUS_WITHIN_10_PERCENT_FLOOR,
        "{} paragraphs within 10% of TeX, below the floor {PARAGRAPH_CENSUS_WITHIN_10_PERCENT_FLOOR}",
        paragraph_summary.within_10
    );
    let dashboard = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/ratchet/dashboard.md"),
    )
    .expect("the ratchet dashboard");
    assert!(
        dashboard.contains(&text_oracle_dashboard_line(&summary))
            && dashboard.contains(&paragraph_oracle_dashboard_line(&paragraph_summary)),
        "docs/ratchet/dashboard.md does not publish the text censuses ({summary:?}, \
         {paragraph_summary:?}); re-bless it with RATCHET_UPDATE=1 cargo test -p \
         fmn-conformance --test coverage_ratchet"
    );
}

/// One capture run writes both fixtures, so every sample row must be its
/// census row exactly: a hand edit or a partial re-capture shows up here.
#[test]
fn the_sample_is_a_subset_of_the_census() {
    let census: HashMap<String, OracleRow> = census_rows()
        .into_iter()
        .map(|row| (row.digest.clone(), row))
        .collect();
    for row in rows() {
        assert_eq!(
            census.get(&row.digest),
            Some(&row),
            "sample row {} differs from its census row",
            row.digest
        );
    }
}

fn assert_rows_are_digest_only(rows: &[OracleRow]) {
    let mut seen = HashSet::new();
    for row in rows {
        assert!(
            row.digest.len() == 64
                && row
                    .digest
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "not a lowercase sha256: {}",
            row.digest
        );
        assert!(seen.insert(&row.digest), "duplicate row {}", row.digest);
        assert!(row.width > 0.0 && row.height + row.depth > 0.0 && row.count > 0);
        assert!(row.height >= 0.0 && row.depth >= 0.0);
        assert!(!row.classes.is_empty() && row.classes.iter().all(|c| !c.is_empty()));
    }
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
    let census = census_rows();
    let laid = run_layout_oracle(&census, &texts).expect("every census string is in the corpus");
    let errors: Vec<f64> = census
        .iter()
        .zip(&laid)
        .map(|(row, &laid)| oracle_error(row, laid))
        .collect();
    eprint!(
        "{}",
        oracle_ndjson(
            &census,
            &laid,
            &errors,
            (
                CENSUS_WITHIN_5_PERCENT_FLOOR,
                CENSUS_WITHIN_10_PERCENT_FLOOR
            )
        )
    );
    let summary = summarize_oracle(&census, &errors);
    assert!(
        summary.within_5 >= CENSUS_WITHIN_5_PERCENT_FLOOR,
        "{} census strings within 5% of TeX, below the floor {CENSUS_WITHIN_5_PERCENT_FLOOR}",
        summary.within_5
    );
    assert!(
        summary.within_10 >= CENSUS_WITHIN_10_PERCENT_FLOOR,
        "{} census strings within 10% of TeX, below the floor {CENSUS_WITHIN_10_PERCENT_FLOOR}",
        summary.within_10
    );
    // The sample's long-running series, from the same layouts.
    let sampled: HashSet<String> = rows().into_iter().map(|row| row.digest).collect();
    let (sample, sample_errors): (Vec<OracleRow>, Vec<f64>) = census
        .iter()
        .zip(&errors)
        .filter(|(row, _)| sampled.contains(&row.digest))
        .map(|(row, &error)| (row.clone(), error))
        .unzip();
    let sample_summary = summarize_oracle(&sample, &sample_errors);
    eprintln!(
        "{{\"schema\":\"fmn.tex-corpus-boxes.v1\",\"kind\":\"sample_summary\",\"rows\":{},\"within_5\":{},\"within_10\":{},\"floor_5\":{WITHIN_5_PERCENT_FLOOR},\"floor_10\":{WITHIN_10_PERCENT_FLOOR}}}",
        sample_summary.rows, sample_summary.within_5, sample_summary.within_10
    );
    assert!(
        sample_summary.within_5 >= WITHIN_5_PERCENT_FLOOR,
        "{} sampled strings within 5% of TeX, below the floor {WITHIN_5_PERCENT_FLOOR}",
        sample_summary.within_5
    );
    assert!(
        sample_summary.within_10 >= WITHIN_10_PERCENT_FLOOR,
        "{} sampled strings within 10% of TeX, below the floor {WITHIN_10_PERCENT_FLOOR}",
        sample_summary.within_10
    );
    let dashboard = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/ratchet/dashboard.md"),
    )
    .expect("the ratchet dashboard");
    assert!(
        dashboard.contains(&oracle_dashboard_line(&summary)),
        "docs/ratchet/dashboard.md does not publish the oracle's counts \
         ({summary:?}); re-bless it with RATCHET_UPDATE=1 cargo test -p \
         fmn-conformance --test coverage_ratchet"
    );
}
