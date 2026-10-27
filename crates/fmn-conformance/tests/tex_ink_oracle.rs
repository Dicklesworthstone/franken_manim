//! Ink-fidelity oracle for native TeX (fm-tex-ink-oracle-5ehf).
//!
//! The box oracles check where TeX puts the ink; this one checks the ink.
//! `fixtures/tex_ink_reference.v1.txt` holds the pinned Reference's own
//! `Tex`/`TexText` outlines for the authored formulas of the Reference box
//! oracle plus authored `TexText` strings (`scripts/capture_tex_ink.py`). Each
//! row's native ink is registered onto the Reference's and both are drawn by
//! the certified engine; [`fmn_conformance::tex_ink`] defines the metrics.
//!
//! The floors may only improve: the count of matching formulas may only
//! rise, the Hausdorff and W1 quantiles only fall, the IoU quantiles only
//! rise. A change that moves one the other way needs a Behavior Note that
//! re-blesses it. Three planted defects must each break a floor: θ drawn as
//! ϑ, every stem 10% thicker, and a missing superscript.
//!
//! The private corpus set (the TeX box oracle's sample plus the corpus's most
//! frequent `TexText` strings, by digest) runs where its capture exists
//! (`FMN_TEX_INK_CORPUS`, or `corpus/tex_ink_reference.v1.txt` with
//! `FMN_TEX_CORPUS`), and records a skip otherwise.
//!
//! Every run logs NDJSON (schema `fmn.tex-ink.v1`): a record per formula,
//! worst IoU first, then the summary.
#![forbid(unsafe_code)]
#![allow(clippy::expect_used, clippy::panic, clippy::print_stderr)]

use fmn_conformance::ratchet::{
    ORACLE_FIXTURE, corpus_digest, parse_corpus_entry, parse_oracle_fixture,
};
use fmn_conformance::tex_ink::{
    INK_FIXTURE, InkFloors, InkKind, InkMetrics, InkRow, Members, compare_ink, floor_failures,
    ink_class_ndjson, ink_matches, ink_ndjson, members_of, parse_ink_fixture, registered_masks,
    side_by_side_png, summarize_ink,
};
use fmn_core::types::Vec3;
use fmn_library::{Tex, TexEngine, TexText};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The public set's floors. 71 of 123 formulas matched when the oracle
/// landed (2026-10-21, franken_markdown 216c7c3); most misses were CM
/// Unicode's `+` and `=` at 0.57 of cmr10's ink. 96 match since math-mode
/// family 0 is cmr10 itself (49466bc, fm-8zy2), 98 since rule 18a reads its
/// drops at the script size (4db6f43, fm-4ptw): a display integral's upper
/// limit is in place. 101 since TeX's own small delimiters and brace pieces
/// (2f1e00c, fm-u5sb): \underbrace, \overbrace and \left[ x \right] match.
/// 102 since `\vec` is cmmi's own accent (439a910, fm-zxxy): \vec{v} goes
/// from IoU 0.86 to 0.986.
/// 104 with authored AMS symbols and blackboard capitals (9b457a38, fm-0byq).
/// 105 since operator names keep their last italic correction (cc470d1a,
/// fm-fm-physics-trig-parens-r11-nou8): `\log_2 n` goes from IoU 0.864 to
/// 0.989.
/// Improve the floors when the ink improves; never loosen one to land a change.
const PUBLIC_FLOORS: InkFloors = InkFloors {
    matches: 105,
    median_hausdorff_px: 1.0,
    p95_hausdorff_px: 4.124,
    median_sliced_w1_px: 0.057,
    p95_sliced_w1_px: 2.461,
    median_iou: 0.9906,
    p05_iou: 0.3991,
};

/// The private corpus set's floors (the TeX box oracle's sample and the 60
/// most frequent TexText strings): 398 of 643 matched when the oracle landed
/// (franken_markdown 216c7c3), 486 since math-mode family 0 is cmr10
/// (49466bc, fm-8zy2), 487 with rule 18a's drops (4db6f43, fm-4ptw), 488
/// with Appendix G's parameters read at the current size (abbb957, fm-giir):
/// e^{-x^2} matches. 489 since `\cong` is cmsy's ∼ over cmr's = (83d363c,
/// fm-shqq): its one corpus row went from IoU 0.190 to 0.984, and the p95
/// Hausdorff fell from 11 to 10.05 px. 493 with TeX's small delimiters and
/// brace pieces (2f1e00c, fm-u5sb). 500 with cmmi's `\vec`, cmex's tall
/// radical and amsmath's `\iint` (439a910, fm-zxxy): 14 rows change, every
/// one's IoU rises, 8 of them now match, and p05 IoU rises 0.1431 -> 0.1443.
/// 501 since a reversal keeps a contour whole when a handle sits on its end
/// anchor (fm-7lzd): fm-hc78's orientation (47d2d8e0) had split 32 CM glyphs
/// into open runs, moving six rows and the p95 sliced W1 to 6.0091 px. The
/// fix moves those six rows back, the \mathcal row matches again, and the
/// p95 is 5.9700 px.
/// 505 with AMS optical cuts and the default pack's native short minus
/// (9b457a38, fm-0byq); all 643 captures remain, including the original
/// 26-row non-CM cohort, whose 4 matches still miss its 65% acceptance bar.
/// 515 since LaTeX's fractions are Ord atoms and amssymb sets its authored
/// AMS glyphs (683e4ccc, fm-0byq): \frac no longer takes thin spaces against
/// an Ord. The non-CM cohort rises to 9 of 26, and the p95 Hausdorff falls
/// 8.0 -> 7.071 px and the p95 sliced W1 4.4397 -> 4.0399 px.
/// 518 with amsmath's `\mod` as its display kern (4b705d14, fm-eo8l): the
/// five `\mod` rows' IoU rises from 0.09-0.64 to 0.92-0.99 and three match.
/// The p95 Hausdorff falls to 6.083 px, the p95 sliced W1 to 3.3195 px, and
/// the p05 IoU rises to 0.2368.
/// 521 with amsmath's `\dddot` as an operator under a row of text periods
/// (1ffcadc9, fm-ew7o): its three rows' IoU rises from 0.07-0.12 to
/// 0.98-0.99 and all three match. The p95 Hausdorff falls to 6.000 px, the
/// p95 sliced W1 to 2.9750 px, and the p05 IoU rises to 0.2815.
/// 528 with the long arrows from CM pieces and amsmath's dots look-ahead
/// (d99fee96, fm-fm-long-arrows-joinrel-r11-b2c5,
/// fm-fm-amsmath-dots-context-r11-ymdd): seven long-arrow rows' IoU rises
/// from 0.14-0.55 to 0.93-0.99 and six match, a `(\dots)` row matches
/// (0.11 -> 0.99), and no row's IoU falls. The p95 Hausdorff falls to
/// 5.099 px, the p95 sliced W1 to 2.4729 px, and the p05 IoU rises to
/// 0.2915.
/// 532 with physics' function arguments and operator names keeping their
/// last italic correction (cc470d1a, fm-fm-physics-trig-parens-r11-nou8):
/// four `\log` rows match (IoU 0.84-0.87 -> 0.99), one long `\log p` row's
/// IoU falls 0.9025 -> 0.9022 (no match before or after), and the p95
/// sliced W1 falls to 2.4523 px.
const CORPUS_FLOORS: InkFloors = InkFloors {
    matches: 532,
    median_hausdorff_px: 1.0,
    p95_hausdorff_px: 5.100,
    median_sliced_w1_px: 0.0688,
    p95_sliced_w1_px: 2.453,
    median_iou: 0.9895,
    p05_iou: 0.2914,
};

/// cmr10's stem (`stem# = 25/36 pt`) in scene units at font size 48: an em
/// is (48/144) / 0.688 units, the "0" calibration over Computer Modern's
/// 0.688 em digit.
const STEM_UNITS: f64 = 0.0694 * (48.0 / 144.0) / 0.688;

/// What one row's native side is: the row's source, possibly altered, and
/// an outward offset of every outline (scene units; 0 for none).
type Plant = fn(&InkRow) -> Option<(String, f64)>;

/// Twice the signed area of a point run's control polygon: its winding.
fn winding(points: &[Vec3]) -> f64 {
    points
        .windows(2)
        .map(|p| p[0][0] * p[1][1] - p[1][0] * p[0][1])
        .sum()
}

/// Every outline moved outward along its normal by `offset`: stems thicken
/// by twice `offset` and counters shrink by it. A point's normal is the
/// perpendicular of the chord between its neighbours in its subpath (the
/// Reference's subpath rule), turned outward by the member's winding.
fn dilated(members: &[Vec<Vec3>], offset: f64) -> Members {
    members
        .iter()
        .map(|points| {
            let side = if winding(points) >= 0.0 { 1.0 } else { -1.0 };
            let mut out = points.clone();
            let mut start = 0;
            let mut ends = Vec::new();
            let mut k = 0;
            while k + 2 < points.len() {
                let (a0, h, a1) = (points[k], points[k + 1], points[k + 2]);
                if a0 == h && (0..3).any(|i| (h[i] - a1[i]).abs() > 1e-4) {
                    ends.push((start, k));
                    start = k + 2;
                }
                k += 2;
            }
            ends.push((start, points.len() - 1));
            for (first, last) in ends {
                let n = last - first + 1;
                if n < 3 {
                    continue;
                }
                for k in 0..n {
                    // A closed run repeats its first point last; skip it.
                    let prev = if k == 0 {
                        points[first + n - 2]
                    } else {
                        points[first + k - 1]
                    };
                    let next = if k == n - 1 {
                        points[first + 1]
                    } else {
                        points[first + k + 1]
                    };
                    let (tx, ty) = (next[0] - prev[0], next[1] - prev[1]);
                    let length = (tx * tx + ty * ty).sqrt();
                    if length > 0.0 {
                        out[first + k][0] += side * offset * ty / length;
                        out[first + k][1] -= side * offset * tx / length;
                    }
                }
            }
            out
        })
        .collect()
}

/// One row's measurement, or `None` for a row its plant leaves out.
type Measured = Option<Result<InkMetrics, String>>;

fn honest(row: &InkRow) -> Option<(String, f64)> {
    Some((row.key.clone(), 0.0))
}

fn native(engine: &TexEngine, kind: InkKind, source: &str) -> Result<Members, String> {
    let built = match kind {
        InkKind::Math => Tex::new(source).build(engine),
        InkKind::Text => TexText::new(source).build(engine),
    }
    .map_err(|error| error.to_string())?;
    Ok(members_of(&built.vmob))
}

/// Measure every row whose `sources` entry is set, from that native source
/// and outline offset, on a few threads. Rows are claimed one at a time from
/// a shared counter, and each result lands at its row's index, so the output
/// is the same at any thread count. A row with no source is `None`.
fn measure(rows: &[InkRow], sources: &[Option<(String, f64)>]) -> Vec<Measured> {
    let threads = std::thread::available_parallelism()
        .map_or(1, usize::from)
        .clamp(1, 8);
    let next = AtomicUsize::new(0);
    let done: Vec<Vec<(usize, Measured)>> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                scope.spawn(|| {
                    let engine = TexEngine::new("fmd-math/pack/default", None).expect("engine");
                    let mut out = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let (Some(row), Some(source)) = (rows.get(i), sources.get(i)) else {
                            break;
                        };
                        out.push((
                            i,
                            source.as_ref().map(|(source, offset)| {
                                native(&engine, row.kind, source).and_then(|members| {
                                    let members = if *offset == 0.0 {
                                        members
                                    } else {
                                        dilated(&members, *offset)
                                    };
                                    compare_ink(&row.members, &members)
                                })
                            }),
                        ));
                    }
                    out
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|worker| worker.join().expect("a measuring thread"))
            .collect()
    });
    let mut results: Vec<Measured> = vec![None; rows.len()];
    for (i, result) in done.into_iter().flatten() {
        results[i] = result;
    }
    results
}

fn public_rows() -> &'static [InkRow] {
    static ROWS: OnceLock<Vec<InkRow>> = OnceLock::new();
    ROWS.get_or_init(|| parse_ink_fixture(INK_FIXTURE).expect("the ink fixture parses"))
}

/// The honest public measurement, shared by the floor test and the planted
/// negatives.
fn public_results() -> &'static [Result<InkMetrics, String>] {
    static RESULTS: OnceLock<Vec<Result<InkMetrics, String>>> = OnceLock::new();
    RESULTS.get_or_init(|| {
        let rows = public_rows();
        let sources: Vec<_> = rows.iter().map(honest).collect();
        measure(rows, &sources)
            .into_iter()
            .map(|r| r.expect("every row is measured"))
            .collect()
    })
}

#[test]
fn the_ink_fixture_covers_the_box_oracle_and_texttext() {
    let rows = public_rows();
    let boxes = include_str!("../../fmn-library/tests/fixtures/tex_reference_boxes.v1.tsv")
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .count();
    let math = rows.iter().filter(|r| r.kind == InkKind::Math).count();
    let text = rows.iter().filter(|r| r.kind == InkKind::Text).count();
    assert_eq!(math, boxes, "one ink row per Reference box row");
    assert!(text >= 10, "{text} TexText rows");
    for row in rows {
        assert!(!row.members.is_empty() && row.members.iter().all(|m| m.len() >= 3));
        assert!(matches!(
            row.class.as_str(),
            "plain" | "traced" | "fallback"
        ));
    }
}

#[test]
fn public_ink_tracks_the_reference_and_never_regresses() {
    let rows = public_rows();
    let results = public_results();
    let labels: Vec<String> = rows.iter().map(|r| r.key.clone()).collect();
    let summary = summarize_ink(results);
    eprint!("{}", ink_ndjson("public", rows, &labels, results, &summary));
    let classes: Vec<Vec<String>> = rows.iter().map(font_classes).collect();
    eprint!("{}", ink_class_ndjson("public", &classes, results));
    let sources: Vec<String> = rows.iter().map(|r| r.key.clone()).collect();
    write_gallery("public", rows, &sources, &labels, results);
    let failures = floor_failures(&summary, &PUBLIC_FLOORS);
    assert!(
        failures.is_empty(),
        "the public ink regressed:\n  {}",
        failures.join("\n  ")
    );
}

/// The first superscript's argument wrapped in `\phantom`: the layout keeps
/// its box, the ink loses it.
fn without_first_superscript(source: &str) -> Option<String> {
    let at = source.find('^')?;
    let rest = &source[at + 1..];
    let end = if rest.starts_with('{') {
        let mut depth = 0_usize;
        let mut end = None;
        for (i, ch) in rest.char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(i + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        end?
    } else if let Some(name) = rest.strip_prefix('\\') {
        1 + name
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(name.len())
    } else {
        rest.chars().next()?.len_utf8()
    };
    Some(format!(
        "{}^{{\\phantom{{{}}}}}{}",
        &source[..at],
        &rest[..end],
        &rest[end..]
    ))
}

fn theta_as_vartheta(row: &InkRow) -> Option<(String, f64)> {
    (row.kind == InkKind::Math && row.key.contains(r"\theta"))
        .then(|| (row.key.replace(r"\theta", r"\vartheta"), 0.0))
}

fn heavier_stroke(row: &InkRow) -> Option<(String, f64)> {
    Some((row.key.clone(), 0.05 * STEM_UNITS))
}

fn missing_superscript(row: &InkRow) -> Option<(String, f64)> {
    if row.kind != InkKind::Math {
        return None;
    }
    without_first_superscript(&row.key).map(|source| (source, 0.0))
}

#[test]
fn the_superscript_plant_keeps_the_layout() {
    assert_eq!(
        without_first_superscript("x^2 + y^2").as_deref(),
        Some(r"x^{\phantom{2}} + y^2")
    );
    assert_eq!(
        without_first_superscript(r"e^{i\pi} + 1").as_deref(),
        Some(r"e^{\phantom{{i\pi}}} + 1")
    );
    assert_eq!(
        without_first_superscript(r"\sin^\theta x").as_deref(),
        Some(r"\sin^{\phantom{\theta}} x")
    );
    assert_eq!(without_first_superscript("x_i"), None);
}

#[test]
fn planted_defects_fail_the_floor() {
    let rows = public_rows();
    let honest_results = public_results();
    let plants: [(&str, Plant); 3] = [
        ("theta drawn as vartheta", theta_as_vartheta),
        ("every stem 10% thicker", heavier_stroke),
        ("a missing superscript", missing_superscript),
    ];
    let mut report = Vec::new();
    for (name, plant) in plants {
        let sources: Vec<_> = rows.iter().map(plant).collect();
        let planted = sources.iter().filter(|s| s.is_some()).count();
        assert!(planted > 0, "{name}: no row to plant in");
        let measured = measure(rows, &sources);
        let mixed: Vec<Result<InkMetrics, String>> = measured
            .into_iter()
            .zip(honest_results)
            .map(|(planted, honest)| planted.unwrap_or_else(|| honest.clone()))
            .collect();
        let lost = honest_results
            .iter()
            .zip(&mixed)
            .filter(|(h, p)| {
                h.as_ref().is_ok_and(ink_matches) && !p.as_ref().is_ok_and(ink_matches)
            })
            .count();
        let failures = floor_failures(&summarize_ink(&mixed), &PUBLIC_FLOORS);
        // The plant's own strength: the medians of IoU and ink ratio over the
        // planted rows that matched honestly.
        let strength: Vec<(f64, f64)> = sources
            .iter()
            .zip(honest_results)
            .zip(&mixed)
            .filter(|((source, honest), _)| {
                source.is_some() && honest.as_ref().is_ok_and(ink_matches)
            })
            .map(|(_, planted)| {
                planted
                    .as_ref()
                    .map_or((0.0, 0.0), |m| (m.iou, m.mass_ratio))
            })
            .collect();
        let median = |mut values: Vec<f64>| {
            values.sort_by(f64::total_cmp);
            values.get(values.len() / 2).copied().unwrap_or(f64::NAN)
        };
        let median_iou = median(strength.iter().map(|s| s.0).collect());
        let median_mass = median(strength.iter().map(|s| s.1).collect());
        eprintln!(
            "{{\"schema\":\"fmn.tex-ink.v1\",\"planted\":\"{name}\",\"rows\":{planted},\
             \"matches_lost\":{lost},\"planted_median_iou\":{median_iou:.4},\
             \"planted_median_mass_ratio\":{median_mass:.4},\"floor_failures\":{}}}",
            failures.len()
        );
        if failures.is_empty() {
            report.push(format!("{name}: {planted} planted rows broke no floor"));
        }
    }
    assert!(report.is_empty(), "{}", report.join("\n"));
}

#[test]
fn corpus_ink_tracks_the_reference() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let capture = std::env::var("FMN_TEX_INK_CORPUS").map_or_else(
        |_| root.join("corpus/tex_ink_reference.v1.txt"),
        PathBuf::from,
    );
    let corpus = std::env::var("FMN_TEX_CORPUS")
        .map_or_else(|_| root.join("corpus/tex_corpus.jsonl"), PathBuf::from);
    let (Ok(capture_text), Ok(corpus_text)) = (
        std::fs::read_to_string(&capture),
        std::fs::read_to_string(&corpus),
    ) else {
        fmn_core::test_inputs::skip_or_fail(
            "fmn-conformance::corpus_ink_tracks_the_reference",
            &format!(
                "private ink capture at {} and TeX corpus at {}",
                capture.display(),
                corpus.display()
            ),
        );
        return;
    };
    let texts: HashMap<String, String> = corpus_text
        .lines()
        .filter_map(parse_corpus_entry)
        .map(|entry| (corpus_digest(&entry.mode, &entry.text), entry.text))
        .collect();
    let rows = parse_ink_fixture(&capture_text).expect("the private capture parses");
    let sources: Vec<_> = rows
        .iter()
        .map(|row| {
            let text = texts
                .get(&row.key)
                .expect("every captured digest is in the corpus");
            Some((text.clone(), 0.0))
        })
        .collect();
    let results: Vec<Result<InkMetrics, String>> = measure(&rows, &sources)
        .into_iter()
        .map(|r| r.expect("every row is measured"))
        .collect();
    let labels: Vec<String> = rows.iter().map(|r| r.key[..16].to_owned()).collect();
    let summary = summarize_ink(&results);
    eprint!(
        "{}",
        ink_ndjson("corpus", &rows, &labels, &results, &summary)
    );
    // Construct classes from the TeX box oracle's sample, by digest.
    let constructs: HashMap<String, Vec<String>> = parse_oracle_fixture(ORACLE_FIXTURE)
        .expect("the box oracle fixture parses")
        .into_iter()
        .map(|row| (row.digest, row.classes))
        .collect();
    let classes: Vec<Vec<String>> = rows
        .iter()
        .map(|row| {
            let mut names = font_classes(row);
            names.extend(constructs.get(&row.key).cloned().unwrap_or_default());
            names
        })
        .collect();
    eprint!("{}", ink_class_ndjson("corpus", &classes, &results));
    let sources: Vec<String> = sources
        .into_iter()
        .map(|s| s.map(|(text, _)| text).unwrap_or_default())
        .collect();
    write_gallery("corpus", &rows, &sources, &labels, &results);
    let failures = floor_failures(&summary, &CORPUS_FLOORS);
    assert!(
        failures.is_empty(),
        "the corpus ink regressed:\n  {}",
        failures.join("\n  ")
    );
}

/// A row's kind and font class, as classes of the per-class records.
fn font_classes(row: &InkRow) -> Vec<String> {
    vec![row.kind.as_str().to_owned(), format!("font:{}", row.class)]
}

/// With `FMN_TEX_INK_GALLERY` set to a directory (outside the repository:
/// corpus formulas are CC BY-NC-SA, plan §15.3), write the side-by-side crops
/// of the 50 lowest-IoU rows and every originally captured fallback row as
/// `<set>/NN-<label>.png`. Improved font rows must remain visible for review.
fn write_gallery(
    set: &str,
    rows: &[InkRow],
    sources: &[String],
    labels: &[String],
    results: &[Result<InkMetrics, String>],
) {
    let Ok(dir) = std::env::var("FMN_TEX_INK_GALLERY") else {
        return;
    };
    let dir = PathBuf::from(dir).join(set);
    std::fs::create_dir_all(&dir).expect("the gallery directory");
    let mut order: Vec<usize> = (0..rows.len()).filter(|&i| results[i].is_ok()).collect();
    let iou = |i: usize| results[i].as_ref().map_or(0.0, |m| m.iou);
    order.sort_by(|&x, &y| iou(x).total_cmp(&iou(y)).then(x.cmp(&y)));
    let engine = TexEngine::new("fmd-math/pack/default", None).expect("engine");
    for (rank, &i) in order.iter().enumerate() {
        if rank >= 50 && rows[i].class != "fallback" {
            continue;
        }
        let Ok(members) = native(&engine, rows[i].kind, &sources[i]) else {
            continue;
        };
        let Ok((_, reference, native)) = registered_masks(&rows[i].members, &members) else {
            continue;
        };
        let name: String = labels[i]
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .take(40)
            .collect();
        std::fs::write(
            dir.join(format!("{rank:02}-{name}.png")),
            side_by_side_png(&reference, &native),
        )
        .expect("a gallery crop is written");
    }
}
