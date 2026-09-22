//! Native macro preambles preserve source identity, cache semantics and bounds.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use fmn_tex::{
    Mode, Span, Style, TEX_PREAMBLE_MAX_BYTES, TEX_PREAMBLE_SOURCE_MAX_BYTES, TexEngine, TexError,
};

const MATH: Mode = Mode::Math(Style::Display);

fn engine() -> TexEngine {
    TexEngine::new("fmd-math/pack/default", None).unwrap()
}

#[test]
fn empty_preamble_is_the_unchanged_typeset_path_in_every_mode() {
    let engine = engine();
    for mode in [MATH, Mode::Math(Style::Text), Mode::Math(Style::Script), Mode::Text] {
        let expected = engine.typeset(mode, "x").unwrap().to_bytes().unwrap();
        let actual = engine.typeset_with_preamble(mode, "x", "").unwrap();
        assert_eq!(actual.to_bytes().unwrap(), expected);
    }
}

#[test]
fn declarations_and_trailing_comments_do_not_change_plain_formula_geometry() {
    let engine = engine();
    for mode in [MATH, Mode::Text] {
        let plain = engine.typeset(mode, "x+y").unwrap().to_bytes().unwrap();
        for preamble in ["% a comment without a newline", r"\newcommand{\unused}{x}% end", " \n% comment\n"] {
            let actual = engine.typeset_with_preamble(mode, "x+y", preamble).unwrap();
            assert_eq!(actual.to_bytes().unwrap(), plain, "{mode:?}: {preamble}");
        }
    }
}

#[test]
fn parameters_call_sites_rules_and_drawn_paths_keep_original_formula_spans() {
    let engine = engine();
    let preamble = r"\newcommand{\half}[1]{\frac{#1}{2}}";
    let source = r"\half{x}+\left[\frac{y}{z}\right]";
    let actual = engine.typeset_with_preamble(MATH, source, preamble).unwrap();
    assert_eq!(actual.source, source);
    assert_eq!(actual.layout.rules.len(), 2);
    assert!(!actual.occurrences("x")[0].is_empty());
    assert!(!actual.occurrences(r"\half")[0].is_empty());
    assert!(actual.subs.iter().any(|sub| sub.span == Span::new(6, 7)));
    assert!(actual.subs.iter().any(|sub| sub.span == Span::new(0, 8)));
    for sub in &actual.subs {
        assert!(sub.span.end <= source.len());
        assert!(source.is_char_boundary(sub.span.start));
        assert!(source.is_char_boundary(sub.span.end));
        engine.resolve_prim(&actual, sub.prim).unwrap();
    }
    assert_eq!(fmn_tex::Typeset::from_bytes(&actual.to_bytes().unwrap()).unwrap().source, source);
}

#[test]
fn text_mainland_and_math_islands_keep_utf8_source_coordinates() {
    let engine = engine();
    let source = "é area $\\sq{x}$";
    let actual = engine.typeset_with_preamble(Mode::Text, source, r"\newcommand{\sq}[1]{#1^2}").unwrap();
    assert_eq!(actual.source, source);
    for token in ["é", "x", r"\sq"] {
        assert!(!actual.occurrences(token)[0].is_empty(), "{token}");
    }
    for sub in &actual.subs {
        assert!(source.is_char_boundary(sub.span.start));
        assert!(source.is_char_boundary(sub.span.end));
    }
}

#[test]
fn macros_are_request_local_and_pack_shadowing_rules_are_native() {
    let engine = engine();
    let preamble = r"\newcommand{\local}{x}";
    assert!(engine.typeset_with_preamble(MATH, r"\local", preamble).is_ok());
    assert!(engine.typeset(MATH, r"\local").is_err());
    assert!(engine.typeset_with_preamble(MATH, r"\minus", r"\newcommand{\minus}{+}").is_err());
    assert!(engine.typeset_with_preamble(MATH, r"\minus", r"\renewcommand{\minus}{+}").is_ok());
    let empty = TexEngine::new("fmd-math/pack/empty", None).unwrap();
    assert!(empty.typeset_with_preamble(MATH, r"\minus", r"\newcommand{\minus}{+}").is_ok());
    assert!(empty.typeset_with_preamble(MATH, "x", r"\renewcommand{\minus}{+}").is_err());
}

#[test]
fn different_definitions_cannot_reuse_another_cached_expansion() {
    let engine = engine();
    let source = r"\coefficient";
    let x = r"\newcommand{\coefficient}{x}";
    let y = r"\newcommand{\coefficient}{y^2}";
    let first = engine.typeset_with_preamble(MATH, source, x).unwrap().to_bytes().unwrap();
    let second = engine.typeset_with_preamble(MATH, source, y).unwrap().to_bytes().unwrap();
    assert_ne!(first, second);
    let mut edited = engine.typeset_with_preamble(MATH, source, x).unwrap();
    edited.source.clear();
    edited.layout.glyphs.clear();
    edited.subs.clear();
    assert_eq!(engine.typeset_with_preamble(MATH, source, x).unwrap().to_bytes().unwrap(), first);
    assert_eq!(engine.typeset_with_preamble(MATH, source, y).unwrap().to_bytes().unwrap(), second);
    assert!(engine.memory_cache_stats().hits >= 4);
}

#[test]
fn native_formula_diagnostics_are_rebased_and_declarations_are_named() {
    let engine = engine();
    let preamble = "% UTF-8 é\n\\newcommand{\\local}{x}";
    let source = r"x+\unknownnativecommand";
    let expected = engine.typeset(MATH, source).unwrap_err().to_string();
    assert_eq!(engine.typeset_with_preamble(MATH, source, preamble).unwrap_err().to_string(), expected);
    let error = engine.typeset_with_preamble(MATH, "x", r"\newcommand{\bad}[1]{#2}").unwrap_err();
    assert!(matches!(error, TexError::Preamble { .. }));
    assert!(error.to_string().starts_with("additional_preamble:"));
    let recursion = engine.typeset_with_preamble(MATH, r"\loop", r"\newcommand{\loop}{\loop}").unwrap_err();
    assert!(recursion.to_string().contains("loop"));
}

#[test]
fn visible_content_external_tools_and_over_budget_sources_are_refused() {
    let engine = engine();
    for preamble in ["x", r"\quad", r"\usepackage{amsmath}", r"\input{secret.tex}", r"\write18{command}"] {
        let error = engine.typeset_with_preamble(MATH, "x", preamble).unwrap_err();
        assert!(matches!(error, TexError::Preamble { .. }), "{preamble}: {error}");
    }
    let at_limit = format!("%{}", "a".repeat(TEX_PREAMBLE_MAX_BYTES - 1));
    assert!(engine.typeset_with_preamble(MATH, "x", &at_limit).is_ok());
    let over = format!("{at_limit}a");
    assert!(engine.typeset_with_preamble(MATH, "x", &over).is_err());
    let huge_formula = "x".repeat(TEX_PREAMBLE_SOURCE_MAX_BYTES);
    assert!(matches!(engine.typeset_with_preamble(MATH, &huge_formula, "% comment"), Err(TexError::Preamble { .. })));
}
