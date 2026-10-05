//! Reference box oracle for native `Tex` layout (fm-tex-display-style-nclg,
//! fm-tex-layout-oracle-bkbc). `fixtures/tex_reference_boxes.v1.tsv` holds
//! authored tier-1 formulas with the pinned Reference's measured
//! `Tex(source)` width and height at the default font size (see its header).
//! Native boxes are compared with them; the counts of formulas inside a
//! relative tolerance on both axes may only rise. Every row is printed,
//! worst first, so each run names what to fix next.
#![forbid(unsafe_code)]
#![allow(clippy::expect_used, clippy::panic, clippy::print_stderr)]

use fmn_library::{Tex, TexEngine};

/// Formulas within ±5% of the Reference on both axes: 24 when this oracle
/// landed (2026-10-04), 26 after the display-integral fix (franken_markdown
/// 68fe29b). Raise it when layout improves; never lower it to land a change.
const WITHIN_5_PERCENT_FLOOR: usize = 26;
/// The same at ±10%: 57, then 60.
const WITHIN_10_PERCENT_FLOOR: usize = 60;

struct Row {
    width: f64,
    height: f64,
    source: &'static str,
}

fn rows() -> Vec<Row> {
    include_str!("fixtures/tex_reference_boxes.v1.tsv")
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .map(|line| {
            let mut fields = line.splitn(3, '\t');
            let mut next = || fields.next().expect("three tab-separated fields");
            let width = next().parse().expect("reference width");
            let height = next().parse().expect("reference height");
            Row {
                width,
                height,
                source: next(),
            }
        })
        .collect()
}

#[test]
fn native_tex_boxes_track_the_reference_and_never_regress() {
    let engine = TexEngine::new("fmd-math/pack/default", None).expect("engine");
    let rows = rows();
    assert!(
        rows.len() >= 100,
        "the oracle thinned out: {} rows",
        rows.len()
    );
    let mut measured = Vec::with_capacity(rows.len());
    for row in &rows {
        let error = match Tex::new(row.source).build(&engine) {
            Ok(tex) => {
                let (min, max) = tex.vmob.extent().expect("a typeset formula has extent");
                let (width, height) = (max[0] - min[0], max[1] - min[1]);
                (width / row.width - 1.0, height / row.height - 1.0)
            }
            Err(_) => (f64::INFINITY, f64::INFINITY),
        };
        measured.push((error, row.source));
    }
    let within = |tolerance: f64| {
        measured
            .iter()
            .filter(|((dw, dh), _)| dw.abs() <= tolerance && dh.abs() <= tolerance)
            .count()
    };
    let (five, ten) = (within(0.05), within(0.10));
    measured.sort_by(|a, b| {
        let key = |((dw, dh), _): &((f64, f64), &str)| dw.abs().max(dh.abs());
        key(b).total_cmp(&key(a))
    });
    eprintln!(
        "tex reference boxes: {five}/{} within 5%, {ten}/{} within 10% (floors {WITHIN_5_PERCENT_FLOOR}, {WITHIN_10_PERCENT_FLOOR})",
        rows.len(),
        rows.len()
    );
    // Every row, worst first: formula, relative width and height error.
    for ((dw, dh), source) in &measured {
        eprintln!("  dw={dw:+.3} dh={dh:+.3}  {source}");
    }
    assert!(
        five >= WITHIN_5_PERCENT_FLOOR,
        "{five} formulas within 5% of the Reference, below the floor {WITHIN_5_PERCENT_FLOOR}"
    );
    assert!(
        ten >= WITHIN_10_PERCENT_FLOOR,
        "{ten} formulas within 10% of the Reference, below the floor {WITHIN_10_PERCENT_FLOOR}"
    );
}
