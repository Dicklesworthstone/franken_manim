//! Native mathematical-document acceptance. Oracles use the existing Text,
//! Tex and table owners, not a second parser/typesetter or regenerated golden.
use fmn_library::markdown::{MAX_MARKDOWN_BYTES, Markdown, MarkdownMathOptions};
use fmn_library::tex::Tex;
use fmn_library::text::Text;
use fmn_library::vmobject::VMobject;
use fmn_tex::TexEngine;
use fmn_text::FontBook;

fn book() -> FontBook {
    FontBook::bundled().unwrap()
}
fn engine() -> TexEngine {
    TexEngine::new("fmd-math/pack/default", None).unwrap()
}
fn canonical(v: &VMobject) -> Vec<[f64; 3]> {
    fn append(v: &VMobject, points: &mut Vec<[f64; 3]>) {
        points.extend_from_slice(v.points());
        for c in v.children() {
            append(c, points);
        }
    }
    let mut points = Vec::new();
    append(v, &mut points);
    if let Some(origin) = points.first().copied() {
        for p in &mut points {
            for i in 0..3 {
                p[i] -= origin[i];
            }
        }
    }
    points
}
fn same_shape(a: &VMobject, b: &VMobject) {
    let (a, b) = (canonical(a), canonical(b));
    assert_eq!(a.len(), b.len());
    for (a, b) in a.iter().zip(&b) {
        for i in 0..3 {
            assert!((a[i] - b[i]).abs() < 1e-10, "{a:?} != {b:?}");
        }
    }
}

#[test]
fn inline_math_preserves_tex_underscores_and_uses_native_layout() {
    let (book, engine) = (book(), engine());
    let doc = Markdown::new(r"before $x_i+y_j$ after")
        .build_with_math(&book, &engine)
        .unwrap();
    let children = doc.blocks[0].vmob.children();
    assert_eq!(children.len(), 3);
    let reference = Tex::new("x_i+y_j").build(&engine).unwrap();
    same_shape(&children[1], &reference.vmob);
    assert!(children[0].extent().unwrap().1[0] < children[1].extent().unwrap().0[0]);
    assert!(children[1].extent().unwrap().1[0] < children[2].extent().unwrap().0[0]);
}

#[test]
fn display_and_fenced_math_are_exact_native_formulas() {
    let (book, engine) = (book(), engine());
    for source in [
        "$$\\frac{1}{x^2}$$",
        "```math\n\\frac{1}{x^2}\n```",
        "```tex\n\\frac{1}{x^2}\n```",
    ] {
        let doc = Markdown::new(source)
            .build_with_math(&book, &engine)
            .unwrap();
        same_shape(
            &doc.vmob,
            &Tex::new(r"\frac{1}{x^2}")
                .display()
                .build(&engine)
                .unwrap()
                .vmob,
        );
    }
}

#[test]
fn code_spans_and_escaped_dollars_do_not_become_math_or_markup() {
    let book = book();
    let doc = Markdown::new(r"`x_i $y_j$ a < b && c > d`")
        .build_with_math(&book, &engine())
        .unwrap();
    let reference = Text::new("x_i $y_j$ a < b && c > d")
        .font(fmn_text::font::MONO_FAMILY)
        .build(&book)
        .unwrap();
    same_shape(&doc.vmob, &reference.vmob);
    Markdown::new(r"A \$literal and $unpaired")
        .build(&book)
        .unwrap();
    Markdown::new("```rust\nlet price = \"$x_i$\";\n```")
        .build(&book)
        .unwrap();
}

#[test]
fn nested_blocks_keep_all_content_and_do_not_overlap() {
    let source = "# Title\n\n- outer\n  - nested\n\n> ## Quote\n>\n> body\n\n| a | b |\n|---|---|\n| 1 | 2 |\n";
    let doc = Markdown::new(source)
        .block_gap(0.3)
        .build_with_math(&book(), &engine())
        .unwrap();
    assert_eq!(
        doc.blocks.iter().map(|b| b.kind).collect::<Vec<_>>(),
        ["heading", "list", "quote", "table"]
    );
    for pair in doc.blocks.windows(2) {
        let bottom = pair[0].vmob.extent().unwrap().0[1];
        let top = pair[1].vmob.extent().unwrap().1[1];
        assert!((bottom - top - 0.3).abs() < 1e-10);
    }
    assert!(
        canonical(&doc.blocks[1].vmob).len()
            > canonical(&Markdown::new("- outer").build(&book()).unwrap().vmob).len()
    );
    assert!(doc.blocks[3].vmob.children().len() >= 7);
}

#[test]
fn wrapping_preserves_atoms_and_separates_tall_math_lines() {
    let (book, engine) = (book(), engine());
    let source = r"alpha $\frac{1}{x}$ beta $\sqrt{x}$ gamma";
    let wide = Markdown::new(source)
        .build_with_math(&book, &engine)
        .unwrap();
    let narrow = Markdown::new(source)
        .build_with_math_options(
            &book,
            &engine,
            MarkdownMathOptions {
                line_width: Some(1.5),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(narrow.vmob.length_over_dim(1) > wide.vmob.length_over_dim(1));
    assert_eq!(
        wide.blocks[0].vmob.children().len(),
        narrow.blocks[0].vmob.children().len()
    );
    for (a, b) in wide.blocks[0]
        .vmob
        .children()
        .iter()
        .zip(narrow.blocks[0].vmob.children())
    {
        same_shape(a, b);
    }
}

#[test]
fn original_unicode_source_ranges_survive_math_protection() {
    let source = "# Ω\n\nA $x_i+y_j$ result.\n\n```math\nx^2\n```\n";
    let doc = Markdown::new(source)
        .build_with_math(&book(), &engine())
        .unwrap();
    assert_eq!(doc.blocks.len(), 3);
    assert!(source[doc.blocks[0].byte_range.0..doc.blocks[0].byte_range.1].contains('Ω'));
    assert!(source[doc.blocks[1].byte_range.0..doc.blocks[1].byte_range.1].contains("$x_i+y_j$"));
    for pair in doc.blocks.windows(2) {
        assert!(pair[0].byte_range.1 <= pair[1].byte_range.0);
    }
}

#[test]
fn source_and_layout_limits_refuse_before_unbounded_expansion() {
    let book = book();
    assert!(
        Markdown::new(&"x".repeat(MAX_MARKDOWN_BYTES + 1))
            .build(&book)
            .is_err()
    );
    assert!(
        Markdown::new("x")
            .build_with_math_options(
                &book,
                &engine(),
                MarkdownMathOptions {
                    line_width: Some(f64::NAN),
                    ..Default::default()
                }
            )
            .is_err()
    );
    assert!(Markdown::new("x").font_size(-1.0).build(&book).is_err());
    assert!(
        Markdown::new(&"$x$ ".repeat(257))
            .build_with_math(&book, &engine())
            .is_err()
    );
    assert!(
        Markdown::new("![missing](https://example.invalid/image.png)")
            .build_with_math(&book, &engine())
            .is_err()
    );
    assert!(
        Markdown::new("$x$").build(&book).is_ok(),
        "the text-only path remains literal"
    );
}

/// Whether fmd's own parser reads a one-paragraph source as mathematics.
fn fmd_reads_math(source: &str) -> bool {
    use franken_markdown::ast::{Block, Inline};
    franken_markdown::parse::parse_document_spanned(source)
        .blocks
        .iter()
        .any(|block| match &block.node {
            Block::Paragraph(inlines) => inlines
                .iter()
                .any(|i| matches!(i, Inline::Math(_) | Inline::DisplayMath(_))),
            Block::MathBlock(_) => true,
            _ => false,
        })
}

#[test]
fn dollar_admission_is_fmds_so_prose_dollars_stay_prose() {
    let (book, engine) = (book(), engine());
    for (source, math) in [
        ("It costs $5 and $10 today.", false),
        ("price: $5", false),
        ("a $ x$ b", false),
        ("a $x $ b", false),
        ("a $$$ b", false),
        ("a $x$ b", true),
        ("a $5 and 10$ b", true),
        ("$$x$$", true),
    ] {
        assert_eq!(fmd_reads_math(source), math, "fmd's reading of {source:?}");
        let doc = Markdown::new(source)
            .build_with_math(&book, &engine)
            .unwrap();
        let literal = Markdown::new(&source.replace('$', r"\$"))
            .build_with_math(&book, &engine)
            .unwrap();
        assert_eq!(
            canonical(&doc.vmob) != canonical(&literal.vmob),
            math,
            "the document's reading of {source:?}"
        );
    }
}

#[test]
fn invalid_math_is_a_named_error_not_literal_garbage() {
    let error = Markdown::new(r"$\thiscommanddoesnotexist{x}$")
        .build_with_math(&book(), &engine())
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("thiscommanddoesnotexist"));
}

#[test]
fn display_islands_take_their_own_line_and_unmatched_code_is_literal() {
    let (book, engine) = (book(), engine());
    let doc = Markdown::new(r"before $$\frac{1}{x}$$ after")
        .build_with_math(&book, &engine)
        .unwrap();
    let children = doc.blocks[0].vmob.children();
    assert_eq!(children.len(), 3);
    assert!(children[0].extent().unwrap().0[1] > children[1].extent().unwrap().1[1]);
    assert!(children[1].extent().unwrap().0[1] > children[2].extent().unwrap().1[1]);
    same_shape(
        &children[1],
        &Tex::new(r"\frac{1}{x}")
            .display()
            .build(&engine)
            .unwrap()
            .vmob,
    );
    let doc = Markdown::new(r"` $x_i$")
        .build_with_math(&book, &engine)
        .unwrap();
    assert_eq!(doc.blocks[0].vmob.children().len(), 2);
    same_shape(
        &doc.blocks[0].vmob.children()[1],
        &Tex::new("x_i").build(&engine).unwrap().vmob,
    );
}

#[test]
fn mathematical_paragraphs_keep_document_reference_link_definitions() {
    let (book, engine) = (book(), engine());
    let source = "See [reference][result] for $x_i$.\n\n[result]: https://example.invalid/result\n";
    let referenced = Markdown::new(source)
        .build_with_math(&book, &engine)
        .unwrap();
    let inline = Markdown::new("See [reference](https://example.invalid/result) for $x_i$.")
        .build_with_math(&book, &engine)
        .unwrap();
    assert_eq!(referenced.blocks.len(), 1);
    same_shape(&referenced.vmob, &inline.vmob);
    assert!(
        source[referenced.blocks[0].byte_range.0..referenced.blocks[0].byte_range.1]
            .contains("[reference][result]")
    );
}

#[test]
fn ordinary_tables_keep_the_existing_string_table_geometry() {
    let book = book();
    let source = "| full name | value |\n| --- | --- |\n| alpha beta | 2 |\n|  | 3 |\n";
    let expected = fmn_library::TableMobject::from_grid(
        vec!["full name".into(), "value".into()],
        vec![
            vec!["alpha beta".into(), "2".into()],
            vec!["".into(), "3".into()],
        ],
    )
    .build(&book)
    .unwrap();
    for size in [24.0, 48.0, 72.0] {
        let doc = Markdown::new(source)
            .font_size(size)
            .build_with_math(&book, &engine())
            .unwrap();
        same_shape(
            &doc.blocks[0].vmob,
            &expected.clone().scaled_about(size / 48.0, [0.0; 3]),
        );
        assert_eq!(
            doc.blocks[0].vmob.children().len(),
            expected.children().len()
        );
    }
}

#[test]
fn table_cells_keep_native_formulas_styles_code_and_source_order() {
    let (book, engine) = (book(), engine());
    let source = "| formula | strong | code |\n| --- | --- | --- |\n| $x_i+y_j$ | **bold** | `x_i $y_j$` |\n| *italic* | [label][ref] | ~~gone~~ |\n\n[ref]: https://example.invalid/result\n";
    let doc = Markdown::new(source)
        .build_with_math(&book, &engine)
        .unwrap();
    assert_eq!(doc.blocks.len(), 1);
    let table = &doc.blocks[0].vmob;
    let cells = &table.children()[table.children().len() - 9..];
    same_shape(&cells[3], &Tex::new("x_i+y_j").build(&engine).unwrap().vmob);
    same_shape(
        &cells[4],
        &Text::new("bold").bold(true).build(&book).unwrap().vmob,
    );
    same_shape(
        &cells[5],
        &Text::new("x_i $y_j$")
            .font(fmn_text::font::MONO_FAMILY)
            .build(&book)
            .unwrap()
            .vmob,
    );
    same_shape(
        &cells[6],
        &Text::new("italic").italic(true).build(&book).unwrap().vmob,
    );
    same_shape(&cells[7], &Text::new("label").build(&book).unwrap().vmob);
    same_shape(
        &cells[8],
        &Text::markup("<s>gone</s>").build(&book).unwrap().vmob,
    );
    assert!(
        source[doc.blocks[0].byte_range.0..doc.blocks[0].byte_range.1]
            .contains("$x_i+y_j$ | **bold**")
    );
}

/// franken_markdown's `---.` (decimal alignment for financial columns) has no
/// native table equivalent: a named refusal, never a silent right alignment.
#[test]
fn decimal_aligned_table_columns_refuse_by_name() {
    let (book, engine) = (book(), engine());
    let source = "| item | amount |\n| :--- | ---. |\n| a | 1.50 |\n| b | 12.25 |\n";
    let error = Markdown::new(source)
        .build_with_math(&book, &engine)
        .err()
        .expect("decimal alignment is refused");
    assert!(
        error
            .to_string()
            .contains("decimal-aligned table columns (`---.`)"),
        "{error}"
    );
}

#[test]
fn math_tables_honor_column_alignment_and_make_room_for_tall_formulas() {
    let (book, engine) = (book(), engine());
    let source = "| left heading | centered heading | right heading |\n| :--- | :---: | ---: |\n| $x$ | **i** | `r` |\n| $\\frac{1}{1+\\frac{1}{x}}$ | $y_j$ | *end* |\n";
    let doc = Markdown::new(source)
        .build_with_math(&book, &engine)
        .unwrap();
    let table = &doc.blocks[0].vmob;
    let cells = &table.children()[table.children().len() - 9..];
    for row in 1..3 {
        assert!(
            (cells[row * 3].extent().unwrap().0[0] - cells[0].extent().unwrap().0[0]).abs() < 1e-10
        );
        assert!((cells[row * 3 + 1].center_point()[0] - cells[1].center_point()[0]).abs() < 1e-10);
        assert!(
            (cells[row * 3 + 2].extent().unwrap().1[0] - cells[2].extent().unwrap().1[0]).abs()
                < 1e-10
        );
    }
    for rows in cells.as_chunks::<3>().0.windows(2) {
        let bottom = rows[0]
            .iter()
            .map(|cell| cell.extent().unwrap().0[1])
            .fold(f64::INFINITY, f64::min);
        let top = rows[1]
            .iter()
            .map(|cell| cell.extent().unwrap().1[1])
            .fold(f64::NEG_INFINITY, f64::max);
        assert!(bottom > top, "adjacent table rows overlap");
    }
    for row in cells.as_chunks::<3>().0 {
        for pair in row.windows(2) {
            assert!(pair[0].extent().unwrap().1[0] < pair[1].extent().unwrap().0[0]);
        }
    }
}

#[test]
fn tables_preserve_empty_cells_and_color_without_losing_math_errors() {
    let (book, engine) = (book(), engine());
    let source = "|  | $x$ |\n| --- | --- |\n|  |  |\n| $y$ |  |\n";
    let color = fmn_core::constants::TEAL;
    let doc = Markdown::new(source)
        .build_with_math_options(
            &book,
            &engine,
            MarkdownMathOptions {
                color,
                ..Default::default()
            },
        )
        .unwrap();
    let table = &doc.blocks[0].vmob;
    let cells = &table.children()[table.children().len() - 6..];
    for index in [0, 2, 3, 5] {
        assert!(canonical(&cells[index]).is_empty());
    }
    same_shape(&cells[1], &Tex::new("x").build(&engine).unwrap().vmob);
    same_shape(&cells[4], &Tex::new("y").build(&engine).unwrap().vmob);
    fn check_color(v: &VMobject, color: fmn_core::color::Srgb) {
        assert_eq!(v.style().fill_color, color);
        assert_eq!(v.style().stroke_color, color);
        for child in v.children() {
            check_color(child, color);
        }
    }
    check_color(table, color);
    let error = Markdown::new("| math |\n| --- |\n| $\\thiscommanddoesnotexist{x}$ |\n")
        .build_with_math(&book, &engine)
        .err()
        .expect("invalid table formula must fail")
        .to_string();
    assert!(error.contains("thiscommanddoesnotexist"), "{error}");
    let error = Markdown::new("| asset |\n| --- |\n| ![missing](file.png) |\n")
        .build_with_math(&book, &engine)
        .err()
        .expect("implicit table image must fail")
        .to_string();
    assert!(error.contains("explicit scene asset"), "{error}");
}

#[test]
fn tables_admit_only_bounded_rectangular_grids() {
    let (book, engine) = (book(), engine());
    let source = format!("| math |\n| --- |\n{}", "| $x$ |\n".repeat(257));
    assert!(
        Markdown::new(&source)
            .build_with_math(&book, &engine)
            .is_err()
    );
    let source = format!(
        "|{}\n|{}\n|{}\n",
        " h |".repeat(65),
        " --- |".repeat(65),
        " b |".repeat(65)
    );
    let error = Markdown::new(&source)
        .build_with_math(&book, &engine)
        .err()
        .expect("a table over the column budget must fail")
        .to_string();
    assert!(error.contains("64 columns"), "{error}");
    assert!(
        fmn_library::TableMobject::from_grid(Vec::new(), Vec::new())
            .build(&book)
            .is_err()
    );
    assert!(
        fmn_library::TableMobject::from_grid(
            vec!["one".into()],
            vec![vec!["a".into(), "b".into()]]
        )
        .build(&book)
        .is_err()
    );
}
