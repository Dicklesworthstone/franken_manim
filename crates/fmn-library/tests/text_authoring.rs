use fmn_core::color::Srgb;
use fmn_library::text::{SourceSelector, SourceStyle};
use fmn_library::{FontBook, Text};

#[test]
fn base_font_face_and_source_spans_stay_native() {
    let book = FontBook::bundled().unwrap();
    let text = Text::new("éA")
        .font("IBM Plex Sans")
        .bold(true)
        .italic(true)
        .build(&book)
        .unwrap();
    assert_eq!(&*text.source, "éA");
    assert_eq!(
        text.layout
            .glyphs
            .iter()
            .map(|g| g.span)
            .collect::<Vec<_>>(),
        [(0, 2), (2, 3)]
    );
    for glyph in &text.layout.glyphs {
        assert_eq!(glyph.face.family, "IBM Plex Sans");
        assert!(glyph.face.key.bold && glyph.face.key.italic);
    }
}

#[test]
fn markup_overrides_and_restores_inherited_style_without_prefixes() {
    let book = FontBook::bundled().unwrap();
    let source = "A<span weight=\"normal\" font_family=\"Computer Modern\">B</span>C";
    let text = Text::markup(source)
        .font("IBM Plex Sans")
        .bold(true)
        .build(&book)
        .unwrap();
    let glyphs = &text.layout.glyphs;
    assert_eq!(glyphs.len(), 3);
    assert!(glyphs[0].face.key.bold && glyphs[2].face.key.bold);
    assert!(!glyphs[1].face.key.bold);
    assert_eq!(glyphs[0].face.family, "IBM Plex Sans");
    assert_eq!(glyphs[1].face.family, "Computer Modern");
    assert_eq!(glyphs[2].face.family, "IBM Plex Sans");
    assert_eq!(&source[glyphs[1].span.0..glyphs[1].span.1], "B");
}

#[test]
fn local_maps_win_over_base_and_markup_in_source_order() {
    let book = FontBook::bundled().unwrap();
    let text = Text::markup("<b>ABA</b>")
        .font("IBM Plex Sans")
        .t2f(&[("B", "Computer Modern")])
        .t2w(&[("A", false)])
        .t2s(&[("B", true)])
        .build(&book)
        .unwrap();
    assert!(!text.layout.glyphs[0].face.key.bold);
    assert!(text.layout.glyphs[1].face.key.bold && text.layout.glyphs[1].face.key.italic);
    assert_eq!(text.layout.glyphs[1].face.family, "Computer Modern");
    assert!(!text.layout.glyphs[2].face.key.bold);
}

#[test]
fn empty_text_does_not_hide_unknown_base_font() {
    let book = FontBook::bundled().unwrap();
    assert!(
        Text::new("")
            .font("definitely-not-installed")
            .build(&book)
            .is_err()
    );
}

#[test]
fn explicit_default_preserves_existing_geometry() {
    let book = FontBook::bundled().unwrap();
    let old = Text::new("Text A").build(&book).unwrap();
    let new = Text::new("Text A")
        .font("")
        .bold(false)
        .italic(false)
        .build(&book)
        .unwrap();
    assert_eq!(old.layout.glyphs, new.layout.glyphs);
}

#[test]
fn inherited_family_does_not_mask_an_inner_monospace_tag() {
    let book = FontBook::bundled().unwrap();
    let source = "<tt>A</tt>B";
    let built = Text::markup(source)
        .font("IBM Plex Sans")
        .build(&book)
        .unwrap();
    assert_eq!(built.layout.glyphs[0].face.family, "CM Typewriter");
    assert_eq!(built.layout.glyphs[0].span, (4, 5));
    assert_eq!(built.layout.glyphs[1].face.family, "IBM Plex Sans");
}

#[test]
fn native_highlighter_exposes_positional_styles_without_source_rewriting() {
    use fmn_core::color::Srgb;
    use fmn_library::{Code, CodeTheme};
    let styles = Code::new("fn fnord")
        .language("rust")
        .theme(CodeTheme::dark())
        .character_overrides();
    assert_eq!(styles.len(), 8);
    assert_eq!(styles[0].color, Some(Srgb::from_rgb8(104, 194, 70)));
    assert!(
        styles[3..]
            .iter()
            .all(|style| style.color != styles[0].color)
    );
    assert!(
        styles
            .iter()
            .all(|style| style.family == Some("CM Typewriter"))
    );
}

#[test]
fn source_ranges_compose_with_literal_styles_without_losing_markup_or_unicode() {
    let book = FontBook::bundled().unwrap();
    let source = "é<b>ABA</b>é";
    let last_a = source.rfind('A').unwrap();
    let last_range = [(last_a, last_a + 1)];
    let styles = [
        SourceStyle {
            bold: Some(false),
            ..SourceStyle::new(SourceSelector::Literal("A"))
        },
        SourceStyle {
            family: Some("IBM Plex Sans"),
            bold: Some(true),
            italic: Some(true),
            ..SourceStyle::new(SourceSelector::Ranges(&last_range))
        },
    ];
    let text = Text::markup(source)
        .source_styles(&styles)
        .build(&book)
        .unwrap();
    assert_eq!(&*text.source, source);
    let glyphs = &text.layout.glyphs;
    assert_eq!(glyphs.len(), 5);
    assert_eq!(glyphs[0].span, (0, 2));
    assert!(!glyphs[1].face.key.bold);
    assert!(glyphs[2].face.key.bold);
    assert_eq!(glyphs[3].span, last_range[0]);
    assert_eq!(glyphs[3].face.family, "IBM Plex Sans");
    assert!(glyphs[3].face.key.bold && glyphs[3].face.key.italic);
    assert!(!glyphs[4].face.key.bold);
    assert_eq!(text.select(last_range[0]), [3]);
}

#[test]
fn resolved_styles_preserve_the_existing_native_map_layout_and_paint() {
    let book = FontBook::bundled().unwrap();
    let stops = [Srgb::from_rgb8(255, 0, 0), Srgb::from_rgb8(0, 0, 255)];
    let old = Text::new("éABA")
        .t2f(&[("B", "IBM Plex Sans")])
        .t2s(&[("B", true)])
        .t2w(&[("A", true)])
        .t2g(&[("AB", &stops)])
        .build(&book)
        .unwrap();
    let b = [(3, 4)];
    let a = [(2, 3), (4, 5)];
    let ab = [(2, 4)];
    let styles = [
        SourceStyle {
            family: Some("IBM Plex Sans"),
            italic: Some(true),
            ..SourceStyle::new(SourceSelector::Ranges(&b))
        },
        SourceStyle {
            bold: Some(true),
            ..SourceStyle::new(SourceSelector::Ranges(&a))
        },
        SourceStyle {
            gradient: &stops,
            ..SourceStyle::new(SourceSelector::Ranges(&ab))
        },
    ];
    let new = Text::new("éABA")
        .source_styles(&styles)
        .build(&book)
        .unwrap();
    assert_eq!(old.layout, new.layout);
    assert_eq!(old.span_map().entries, new.span_map().entries);
}

#[test]
fn range_gradients_restart_per_occurrence_and_respect_whole_entities() {
    let book = FontBook::bundled().unwrap();
    let source = "<b>AB</b>&amp;<b>AB</b>";
    let first = source.find("AB").unwrap();
    let second = source.rfind("AB").unwrap();
    let entity = source.find("&amp;").unwrap();
    let ranges = [(first, first + 2), (second, second + 2)];
    let partial_entity = [(entity + 1, entity + 4)];
    let stops = [Srgb::from_rgb8(255, 0, 0), Srgb::from_rgb8(0, 0, 255)];
    let styles = [
        SourceStyle {
            gradient: &stops,
            ..SourceStyle::new(SourceSelector::Ranges(&ranges))
        },
        SourceStyle {
            color: Some(Srgb::from_rgb8(0, 255, 0)),
            family: Some("IBM Plex Sans"),
            ..SourceStyle::new(SourceSelector::Ranges(&partial_entity))
        },
    ];
    let text = Text::markup(source)
        .source_styles(&styles)
        .build(&book)
        .unwrap();
    let glyphs = &text.layout.glyphs;
    assert_eq!(glyphs.len(), 5);
    for (index, color) in [(0, stops[0]), (1, stops[1]), (3, stops[0]), (4, stops[1])] {
        assert_eq!(glyphs[index].fill, Some(color));
    }
    assert_eq!(glyphs[2].span, (entity, entity + 5));
    assert_eq!(glyphs[2].face.family, "Computer Modern");
    assert_eq!(glyphs[2].fill, None);
}
