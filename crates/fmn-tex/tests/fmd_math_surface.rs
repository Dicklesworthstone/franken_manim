//! First consumption of fmd-math (fm-wgl): hold the pinned crate to the
//! contracts this repo defines.
//!
//! Two planes:
//!
//! 1. **The tier table is law.** Every construct row of the committed G0-4
//!    table (`docs/g0/g0-4-corpus/construct_table.tsv`) must agree with the
//!    crate's `construct_status`: T1 ⇒ parse-supported; T2 command/
//!    environment vocabulary ⇒ the named, tier-tagged unsupported error
//!    (T2 `char:` rows are parse-transparent — their tier is a layout
//!    concern). This runs in every CI checkout.
//! 2. **API smoke over the frozen G0-3 surface** — parse/parse_text, span
//!    provenance, the atom/spacing engine, and the style walk — so a pin
//!    bump that drifts the frozen shape fails here, in the consumer.
//!
//! The full 9,269-string corpus goldens live upstream (fmd-math's
//! env-gated `corpus_goldens` suite) because the corpus text is private;
//! run them locally with
//! `FMD_MATH_CORPUS=$PWD/corpus/tex_corpus.jsonl cargo test -p fmd-math`
//! from a franken_markdown checkout.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use fmd_math::atom::{AtomClass, Spacing, classify_list, spacing_in_style};
use fmd_math::{ConstructStatus, MathError, NodeKind, Style, StyleCtx, construct_status};

fn repo_file(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

#[test]
fn the_committed_tier_table_agrees_with_the_crate() {
    let table = repo_file("docs/g0/g0-4-corpus/construct_table.tsv");
    let mut checked = 0_usize;
    for line in table.lines() {
        if line.starts_with('#') || line.trim().is_empty() || line.starts_with("rank\t") {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        assert!(fields.len() >= 9, "short construct row: {line}");
        let construct = fields[1];
        let tier = fields[7];
        let status = construct_status(construct);
        match tier {
            "T1" => assert_eq!(
                status,
                ConstructStatus::Supported,
                "tier-1 construct `{construct}` must be parse-supported"
            ),
            "T2" => {
                if construct.starts_with("char:") {
                    assert_eq!(
                        status,
                        ConstructStatus::Supported,
                        "`{construct}`: character coverage is a layout tier, parse-transparent"
                    );
                } else {
                    assert_eq!(
                        status,
                        ConstructStatus::UnsupportedT2,
                        "tier-2 construct `{construct}` must fail as known-T2 vocabulary"
                    );
                }
            }
            other => panic!("unknown tier `{other}` in row: {line}"),
        }
        checked += 1;
    }
    assert_eq!(checked, 206, "the G0-4 table carries 206 constructs");
}

#[test]
fn parse_covers_the_flagship_shapes() {
    for src in [
        r"\int_0^\infty e^{-x^2}\,dx = \frac{\sqrt{\pi}}{2}",
        r"\sum_{n=1}^{\infty} \frac{1}{n^2} = \frac{\pi^2}{6}",
        r"{a+b \over c}",
        r"\left[ \begin{array}{c} x \\ y \end{array} \right]",
        r"e^{i\pi} + 1 = 0",
    ] {
        let root = fmd_math::parse(src).unwrap_or_else(|e| panic!("`{src}`: {e}"));
        assert_eq!(root.span.start, 0);
        assert_eq!(root.span.end, src.len());
    }
    let root = fmd_math::parse_text(r"area $\pi r^2$ of a circle").unwrap();
    let NodeKind::List(items) = &root.kind else {
        panic!("text root is a list");
    };
    assert!(
        items
            .iter()
            .any(|n| matches!(&n.kind, NodeKind::MathIsland { .. }))
    );
}

#[test]
fn the_error_contract_names_constructs_for_the_ratchet() {
    let err = fmd_math::parse(r"\substack{a \\ b}").unwrap_err();
    assert_eq!(err.unsupported_construct(), Some(r"\substack"));
    assert!(err.to_string().contains("tier T2"));
    assert!(matches!(err, MathError::UnsupportedCommand { .. }));
}

#[test]
fn the_atom_engine_answers_spacing_queries() {
    // "a = -b": degradation turns the minus unary; the Rel spacing stays.
    let root = fmd_math::parse(r"a=-b").unwrap();
    let NodeKind::List(items) = &root.kind else {
        panic!("list");
    };
    let classes: Vec<AtomClass> = classify_list(items).into_iter().flatten().collect();
    assert_eq!(
        classes,
        vec![
            AtomClass::Ord,
            AtomClass::Rel,
            AtomClass::Ord,
            AtomClass::Ord
        ]
    );
    assert_eq!(
        spacing_in_style(AtomClass::Ord, AtomClass::Rel, Style::Text),
        Spacing::Thick
    );
    assert_eq!(
        spacing_in_style(AtomClass::Ord, AtomClass::Rel, Style::Script),
        Spacing::None
    );
}

#[test]
fn typeset_produces_placed_output_through_the_pin() {
    let engine = fmd_math::Engine::bundled().unwrap();
    let src = r"\int_0^\infty e^{-x^2}\,dx = \frac{\sqrt{\pi}}{2}";
    let layout = engine.typeset(src, Style::Display).unwrap();
    assert!(!layout.glyphs.is_empty());
    assert!(layout.rules.len() >= 2, "fraction bar + radical overbar");
    assert!(fmd_math::paths::spans_cover(&layout, src.len()));
    // Placement is the published mathematics: the fraction bar is θ thick,
    // centered on the axis.
    let c = engine.constants();
    let bar = layout.rules.iter().find(|r| {
        (r.height - c.rule_thickness).abs() < 1e-9
            && (r.y - (c.axis_height - c.rule_thickness / 2.0)).abs() < 1e-9
    });
    assert!(bar.is_some(), "axis-centered fraction bar");
    // Paths resolve deterministically to identical bytes.
    let a =
        fmd_math::paths::canonical_dump(&fmd_math::paths::resolve_paths(&engine, &layout).unwrap());
    let layout2 = engine.typeset(src, Style::Display).unwrap();
    let b = fmd_math::paths::canonical_dump(
        &fmd_math::paths::resolve_paths(&engine, &layout2).unwrap(),
    );
    assert_eq!(a, b);
}

#[test]
fn typeset_text_handles_the_textext_contract() {
    let engine = fmd_math::Engine::bundled().unwrap();
    let layout = engine
        .typeset_text(r"the area $\pi r^2$ of a \textbf{circle}")
        .unwrap();
    assert!(!layout.glyphs.is_empty());
}

#[test]
fn layout_pending_constructs_stay_named_through_the_pin() {
    let engine = fmd_math::Engine::bundled().unwrap();
    let err = engine
        .typeset(r"\begin{matrix} a \end{matrix}", Style::Display)
        .unwrap_err();
    assert_eq!(err.unsupported_construct(), Some("env:matrix"));
    assert!(err.to_string().contains("fm-kg9"));
}

#[test]
fn the_style_walk_propagates_like_tex() {
    let root = fmd_math::parse(r"\frac{n}{d}").unwrap();
    let mut seen = Vec::new();
    fmd_math::style_walk(&root, StyleCtx::display(), &mut |node, ctx| {
        if let NodeKind::Symbol { ch, .. } = &node.kind {
            seen.push((*ch, ctx.style, ctx.cramped));
        }
    });
    assert_eq!(
        seen,
        vec![('n', Style::Text, false), ('d', Style::Text, true),]
    );
}
