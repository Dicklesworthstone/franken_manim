//! fm-6nm acceptance: a fixture corpus of real-world SVG shapes with golden
//! path/style output, plus the adversarial fixtures (entity bombs, deep
//! nesting, huge counts, billion-laughs-style entity refs) rejected within
//! budget by named error.
//!
//! The goldens live in `fixtures/svg_goldens.txt`. Regenerate for review
//! with `FMN_SVG_BLESS=1 cargo test -p fmn-geom --test svg` (the test never
//! rewrites the fixture without the flag).

use fmn_core::color::Srgb;
use fmn_geom::{Paint, SvgDocument, SvgError, SvgLimits};

fn fmt(v: f64) -> String {
    // Normalize -0.0 so the golden text is stable.
    let v = if v == 0.0 { 0.0 } else { v };
    format!("{v:.4}")
}

/// Fail with a message. ubs bans `panic!`/`unreachable!` and clippy bans
/// `assert!(false, …)`; a non-constant failing assertion is the compliant
/// form.
#[track_caller]
fn fail(message: String) {
    assert!(message.is_empty(), "{message}");
}

fn fmt_paint(paint: &Option<Paint>) -> String {
    match paint {
        None => "none".to_owned(),
        Some(Paint::Color(Srgb { r, g, b })) => format!(
            "#{:02x}{:02x}{:02x}",
            (r * 255.0).round() as u8,
            (g * 255.0).round() as u8,
            (b * 255.0).round() as u8
        ),
    }
}

/// A deterministic dump of the resolved document: viewport, then per shape
/// the curve/point counts, the full point list, and the style record.
fn dump(doc: &SvgDocument) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "viewport {}x{}\n",
        fmt(doc.width),
        fmt(doc.height)
    ));
    match doc.view_box {
        Some(vb) => out.push_str(&format!(
            "viewBox {} {} {} {}\n",
            fmt(vb[0]),
            fmt(vb[1]),
            fmt(vb[2]),
            fmt(vb[3])
        )),
        None => out.push_str("viewBox none\n"),
    }
    for (i, shape) in doc.shapes.iter().enumerate() {
        out.push_str(&format!(
            "shape {i}: curves={} points={} closed={}\n",
            shape.path.num_curves(),
            shape.path.num_points(),
            shape.path.is_closed()
        ));
        out.push_str("  points:");
        for p in shape.path.points() {
            out.push_str(&format!(" ({},{})", fmt(p[0]), fmt(p[1])));
        }
        out.push('\n');
        let s = &shape.style;
        out.push_str(&format!(
            "  style: fill={} fill-opacity={} fill-rule={:?} stroke={} stroke-width={} \
             stroke-opacity={} cap={} join={} miter={} dash={:?} dash-offset={} opacity={}\n",
            fmt_paint(&s.fill),
            fmt(s.fill_opacity),
            s.fill_rule,
            fmt_paint(&s.stroke),
            fmt(s.stroke_width),
            fmt(s.stroke_opacity),
            s.line_cap,
            s.line_join,
            fmt(s.miter_limit),
            s.stroke_dasharray,
            fmt(s.stroke_dashoffset),
            fmt(s.opacity),
        ));
    }
    out
}

/// The corpus: name → source. Real-world shapes, nested transforms,
/// defs/use, arc paths, and cascade cases.
const CASES: &[(&str, &str)] = &[
    (
        "shapes_basic",
        "<svg width=\"200\" height=\"100\">\
           <rect x=\"10\" y=\"10\" width=\"50\" height=\"30\"/>\
           <rect x=\"70\" y=\"10\" width=\"40\" height=\"30\" rx=\"5\" ry=\"8\"/>\
           <circle cx=\"30\" cy=\"70\" r=\"15\"/>\
           <ellipse cx=\"90\" cy=\"70\" rx=\"20\" ry=\"10\"/>\
           <line x1=\"130\" y1=\"10\" x2=\"180\" y2=\"40\"/>\
           <polyline points=\"130,50 150,70 170,55 190,80\"/>\
           <polygon points=\"130,90 160,85 185,95\"/>\
         </svg>",
    ),
    (
        "nested_transforms",
        "<svg width=\"100\" height=\"100\">\
           <g transform=\"translate(10 20)\">\
             <g transform=\"scale(2) rotate(90 5 5)\">\
               <rect x=\"1\" y=\"2\" width=\"3\" height=\"4\"/>\
             </g>\
             <g transform=\"matrix(0 -1 1 0 50 60) skewX(0)\">\
               <circle cx=\"5\" cy=\"5\" r=\"2\"/>\
             </g>\
           </g>\
           <svg x=\"50\" y=\"50\" width=\"40\" height=\"40\" viewBox=\"0 0 20 20\">\
             <rect x=\"5\" y=\"5\" width=\"10\" height=\"10\"/>\
           </svg>\
         </svg>",
    ),
    (
        "defs_use",
        "<svg width=\"100\" height=\"100\">\
           <defs>\
             <g id=\"badge\">\
               <circle cx=\"0\" cy=\"0\" r=\"5\"/>\
               <rect x=\"-2\" y=\"-2\" width=\"4\" height=\"4\"/>\
             </g>\
           </defs>\
           <use href=\"#badge\" x=\"10\" y=\"20\"/>\
           <use xlink:href=\"#badge\" x=\"50\" y=\"60\" transform=\"scale(2)\"/>\
         </svg>",
    ),
    (
        "path_arcs_and_curves",
        "<svg width=\"100\" height=\"100\">\
           <path d=\"M10 10 A30 15 0 0 1 70 10 Z\"/>\
           <path d=\"M10 50 a20 20 45 1 0 30 30\"/>\
           <path d=\"m10 90 c5 -5 10 -5 15 0 s10 5 15 0 q5 5 10 0 t10 0 h5 v-5 z\"/>\
           <path d=\"M60 90 A1 1 0 0 1 61 90\"/>\
         </svg>",
    ),
    (
        "cascade",
        "<svg width=\"100\" height=\"100\" fill=\"green\">\
           <g fill=\"#ff0000\" fill-opacity=\"0.5\" opacity=\"0.8\" stroke=\"rgb(0,0,255)\" \
              stroke-width=\"2\" stroke-dasharray=\"4 2\" stroke-linecap=\"round\" \
              stroke-linejoin=\"bevel\" fill-rule=\"evenodd\">\
             <rect x=\"5\" y=\"5\" width=\"20\" height=\"20\"/>\
             <g style=\"fill: rgb(0 128 0); opacity: 0.5\" color=\"orange\">\
               <rect x=\"30\" y=\"30\" width=\"20\" height=\"20\" fill=\"currentColor\"/>\
               <rect x=\"60\" y=\"60\" width=\"10\" height=\"10\" fill=\"none\" stroke=\"rgba(255,0,0,0.25)\"/>\
             </g>\
           </g>\
           <rect x=\"80\" y=\"5\" width=\"10\" height=\"10\" display=\"none\"/>\
           <rect x=\"80\" y=\"20\" width=\"10\" height=\"10\" visibility=\"hidden\"/>\
         </svg>",
    ),
    (
        "viewbox_default_meet",
        "<svg width=\"200\" height=\"100\" viewBox=\"0 0 100 100\">\
           <rect x=\"0\" y=\"0\" width=\"100\" height=\"100\"/>\
         </svg>",
    ),
    (
        "viewbox_none_stretch",
        "<svg width=\"200\" height=\"100\" viewBox=\"10 10 50 50\" preserveAspectRatio=\"none\">\
           <rect x=\"10\" y=\"10\" width=\"50\" height=\"50\"/>\
         </svg>",
    ),
    (
        "viewbox_slice_max",
        "<svg width=\"200\" height=\"100\" viewBox=\"0 0 100 100\" preserveAspectRatio=\"xMaxYMax slice\">\
           <circle cx=\"50\" cy=\"50\" r=\"25\"/>\
         </svg>",
    ),
    (
        "units_and_percent",
        "<svg width=\"96\" height=\"96\">\
           <rect x=\"1in\" y=\"72pt\" width=\"2.54cm\" height=\"25.4mm\"/>\
           <line x1=\"0\" y1=\"0\" x2=\"50%\" y2=\"25%\" stroke-width=\"6pc\"/>\
         </svg>",
    ),
];

fn golden_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/svg_goldens.txt")
}

fn render_all_cases() -> String {
    let mut out = String::new();
    for (name, source) in CASES {
        let doc = match SvgDocument::parse(source.as_bytes()) {
            Ok(doc) => doc,
            Err(e) => {
                fail(format!("golden case {name} must parse: {e}"));
                return String::new();
            }
        };
        out.push_str(&format!("== {name} ==\n"));
        out.push_str(&dump(&doc));
    }
    out
}

#[test]
fn golden_document_dumps_match_the_fixture() {
    let rendered = render_all_cases();
    let path = golden_path();
    if std::env::var_os("FMN_SVG_BLESS").is_some() {
        std::fs::write(&path, &rendered).expect("fixture writable");
        eprintln!("blessed {}", path.display());
        return;
    }
    let committed = std::fs::read_to_string(&path).expect("fixtures/svg_goldens.txt is committed");
    if committed != rendered {
        // Report the first differing case for a readable failure.
        let committed_lines: Vec<&str> = committed.lines().collect();
        let rendered_lines: Vec<&str> = rendered.lines().collect();
        for (i, (a, b)) in committed_lines
            .iter()
            .zip(rendered_lines.iter())
            .enumerate()
        {
            assert_eq!(
                a,
                b,
                "golden drift at line {} (re-bless with FMN_SVG_BLESS=1)",
                i + 1
            );
        }
        assert_eq!(
            committed_lines.len(),
            rendered_lines.len(),
            "golden line-count drift (re-bless with FMN_SVG_BLESS=1)"
        );
    }
}

// ------------------------------------------------ adversarial fixtures

/// The adversarial corpus: each fixture must refuse within budget with the
/// named error class — never hang, never panic (the budgets bound the work
/// by construction).
#[test]
fn adversarial_fixtures_refuse_with_named_errors() {
    // The classic billion laughs: refused at the DOCTYPE, before any entity
    // machinery — expansion is impossible by construction.
    let billion_laughs = "\
        <?xml version=\"1.0\"?>\n\
        <!DOCTYPE lolz [\n\
          <!ENTITY lol \"lollollollollollollollollollol\">\n\
          <!ENTITY lol2 \"&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;\">\n\
          <!ENTITY lol3 \"&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;\">\n\
          <!ENTITY lol4 \"&lol3;&lol3;&lol3;&lol3;&lol3;&lol3;&lol3;&lol3;&lol3;&lol3;\">\n\
        ]>\n\
        <svg><desc>&lol4;</desc></svg>";
    assert!(matches!(
        SvgDocument::parse(billion_laughs.as_bytes()),
        Err(SvgError::DoctypeRefused { .. })
    ));

    // An entity-bomb-shaped document without a DOCTYPE: the undefined
    // entity is named, again without expansion.
    let undefined = "<svg><rect width=\"&megabomb;\" height=\"1\"/></svg>";
    assert!(matches!(
        SvgDocument::parse(undefined.as_bytes()),
        Err(SvgError::UnknownEntity { .. })
    ));

    // Deep nesting: 10_000 levels must refuse at the depth budget, in
    // bounded time and memory.
    let mut deep = String::with_capacity(70_000);
    deep.push_str("<svg>");
    for _ in 0..10_000 {
        deep.push_str("<g>");
    }
    match SvgDocument::parse(deep.as_bytes()) {
        Err(SvgError::TooDeep { depth, limit }) => {
            assert_eq!(limit, 32);
            assert_eq!(depth, 33);
        }
        other => fail(format!("expected TooDeep, got {other:?}")),
    }

    // Huge element counts refuse at the element budget.
    let mut many = String::with_capacity(200_000);
    many.push_str("<svg>");
    for _ in 0..200_000 {
        many.push_str("<g/>");
    }
    many.push_str("</svg>");
    assert!(matches!(
        SvgDocument::parse(many.as_bytes()),
        Err(SvgError::TooManyElements { .. } | SvgError::TooLarge { .. })
    ));

    // Huge path-command counts refuse at the command budget. With the
    // default budgets the byte budget (1 MiB) binds first — commands cost
    // ≥4 bytes each — so this fixture raises the byte budget to isolate the
    // command budget.
    let mut d = String::with_capacity(600_000);
    d.push_str("<svg><path d=\"M0 0");
    for i in 0..100_000 {
        d.push_str(&format!("L{} {}", i % 97, i % 89));
    }
    d.push_str("\"/></svg>");
    let limits = SvgLimits {
        max_bytes: 1 << 22,
        max_path_commands: 50_000,
        ..SvgLimits::default()
    };
    assert!(matches!(
        SvgDocument::parse_with_limits(d.as_bytes(), &limits),
        Err(SvgError::TooManyCommands { .. })
    ));

    // A use-expansion pyramid refuses at the expansion budget: each level
    // doubles the expansions.
    let mut pyramid = String::from("<svg><defs>");
    pyramid.push_str("<g id=\"u0\"><rect width=\"1\" height=\"1\"/></g>");
    for level in 1..12 {
        pyramid.push_str(&format!(
            "<g id=\"u{level}\"><use href=\"#u{}\"/><use href=\"#u{}\"/></g>",
            level - 1,
            level - 1
        ));
    }
    pyramid.push_str("</defs><use href=\"#u11\"/></svg>");
    match SvgDocument::parse(pyramid.as_bytes()) {
        Err(SvgError::TooManyUseExpansions { limit }) => assert_eq!(limit, 1024),
        other => fail(format!("expected TooManyUseExpansions, got {other:?}")),
    }

    // Oversized input refuses before any allocation.
    let big = vec![b'<'; (1 << 20) + 1];
    match SvgDocument::parse(&big) {
        Err(SvgError::TooLarge { bytes, limit }) => {
            assert_eq!(bytes, (1 << 20) + 1);
            assert_eq!(limit, 1 << 20);
        }
        other => fail(format!("expected TooLarge, got {other:?}")),
    }

    // Non-UTF-8 input is named with its offset.
    assert!(matches!(
        SvgDocument::parse(b"<svg>\xff</svg>"),
        Err(SvgError::NotUtf8 { offset: 5 })
    ));
}

/// Tight custom budgets refuse fast on inputs the defaults would accept —
/// the untrusted-input seam consumers use.
#[test]
fn custom_budgets_bite() {
    let limits = SvgLimits {
        max_bytes: 1 << 16,
        max_depth: 4,
        max_path_commands: 16,
        max_output_points: 64,
        max_use_expansions: 2,
        max_elements: 16,
    };
    assert!(matches!(
        SvgDocument::parse_with_limits(b"<svg><g><g><g><g><g/></g></g></g></g></svg>", &limits),
        Err(SvgError::TooDeep { .. })
    ));
    assert!(matches!(
        SvgDocument::parse_with_limits(
            b"<svg><path d=\"M0 0L1 1L2 2L3 3L4 4L5 5L6 6L7 7L8 8L9 9L10 10L11 11L12 12L13 13L14 14L15 15L16 16L17 17\"/></svg>",
            &limits
        ),
        Err(SvgError::TooManyCommands { .. })
    ));
}

// ---------------------------------------------------- export round-trip

/// fm-ek1 acceptance: `emit_svg_document` is the importer's inverse for the
/// whole accepted corpus — parse → emit → parse recovers the same resolved
/// SHAPES (structural equality via the deterministic dump), and the
/// emitter is idempotent (emit ∘ parse ∘ emit == emit, byte for byte).
///
/// The comparison clears `view_box` on both sides: resolved shapes are
/// already in post-viewBox user space, and the emitter deliberately does
/// not re-emit the record (re-import would apply the mapping twice), so
/// the round-tripped document carries `view_box: None` by design.
#[test]
fn every_golden_case_round_trips_through_the_emitter() {
    for (name, source) in CASES {
        let doc = match SvgDocument::parse(source.as_bytes()) {
            Ok(doc) => doc,
            Err(e) => {
                fail(format!("golden case {name} must parse: {e}"));
                continue;
            }
        };
        let emitted = fmn_geom::emit_svg_document(&doc);
        let reparsed = match SvgDocument::parse(emitted.as_bytes()) {
            Ok(doc) => doc,
            Err(e) => {
                fail(format!(
                    "case {name}: emitted bytes must re-parse: {e}\n{emitted}"
                ));
                continue;
            }
        };
        let original = SvgDocument {
            view_box: None,
            ..doc.clone()
        };
        assert_eq!(
            dump(&original),
            dump(&reparsed),
            "case {name}: round-trip drifted from the resolved document"
        );
        // Idempotence: emitting the re-parsed document reproduces the same
        // bytes, so export is a fixed point after one cycle.
        let re_emitted = fmn_geom::emit_svg_document(&reparsed);
        assert_eq!(
            emitted, re_emitted,
            "case {name}: emitter is not idempotent after one round-trip"
        );
    }
}
/// An open subpath stays open and a closed subpath stays closed across the
/// round-trip — closure is stroke-visible, so the emitter must not flip it.
#[test]
fn closure_state_survives_the_round_trip() {
    let open_source = b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"10\" height=\"10\"><path d=\"M1 1 Q 2 0 3 1\" fill=\"none\" stroke=\"#ff0000\"/></svg>";
    let closed_source = b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"10\" height=\"10\"><path d=\"M1 1 Q 2 0 3 1 Q 2 2 1 1 Z\" fill=\"#00ff00\"/></svg>";
    for (label, source, expect_closed) in [
        ("open", open_source.as_slice(), false),
        ("closed", closed_source.as_slice(), true),
    ] {
        let doc = SvgDocument::parse(source).expect("fixture parses");
        assert_eq!(doc.shapes.len(), 1);
        let emitted = fmn_geom::emit_svg_document(&doc);
        let round = SvgDocument::parse(emitted.as_bytes()).expect("emitted re-parses");
        assert_eq!(
            doc.shapes[0].path.subpaths().len(),
            round.shapes[0].path.subpaths().len()
        );
        let original_points = doc.shapes[0].path.points();
        let round_points = round.shapes[0].path.points();
        assert_eq!(
            original_points.len(),
            round_points.len(),
            "{label}: point-count drift across the round-trip"
        );
        for (a, b) in original_points.iter().zip(round_points.iter()) {
            assert!((a[0] - b[0]).abs() < 1e-12 && (a[1] - b[1]).abs() < 1e-12);
        }
        // The closure state shows in the re-emitted `d`: a closed subpath
        // ends with Z, an open one does not.
        let d_is_closed = emitted.contains("Z</svg") || emitted.contains("Z\"");
        assert_eq!(d_is_closed, expect_closed, "{label}: closure flag drifted");
    }
}

fn assert_cubic_viewport_bound(document: &SvgDocument, source: [[f64; 3]; 4], tolerance: f64) {
    let path = &document.shapes[0].path;
    let count = path.num_curves();
    assert!(count > 1, "witness must exercise adaptive subdivision");
    let derivative = |t: f64| {
        let mut result = [0.0; 3];
        for axis in 0..3 {
            result[axis] = 3.0
                * ((1.0 - t) * (1.0 - t) * (source[1][axis] - source[0][axis])
                    + 2.0 * (1.0 - t) * t * (source[2][axis] - source[1][axis])
                    + t * t * (source[3][axis] - source[2][axis]));
        }
        result
    };
    let evaluate = |t| fmn_geom::bezier::cubic_point(source[0], source[1], source[2], source[3], t);
    for index in 0..count {
        let start = index as f64 / count as f64;
        let end = (index + 1) as f64 / count as f64;
        let a = evaluate(start);
        let b = evaluate(end);
        let da = derivative(start);
        let db = derivative(end);
        let quad = path.nth_curve_points(index).unwrap();
        // The difference is a cubic Bernstein polynomial. Every control
        // difference within the tolerance proves the whole interval's bound.
        for control in 0..4 {
            let mut squared = 0.0;
            for axis in 0..3 {
                let original = match control {
                    0 => a[axis],
                    1 => a[axis] + da[axis] / (3.0 * count as f64),
                    2 => b[axis] - db[axis] / (3.0 * count as f64),
                    _ => b[axis],
                };
                let elevated = match control {
                    0 => quad[0][axis],
                    1 => (quad[0][axis] + 2.0 * quad[1][axis]) / 3.0,
                    2 => (2.0 * quad[1][axis] + quad[2][axis]) / 3.0,
                    _ => quad[2][axis],
                };
                squared += (original - elevated) * (original - elevated);
            }
            assert!(
                squared.sqrt() <= tolerance + 1e-9,
                "piece {index}/{count}, control {control}: deviation {} > {tolerance}",
                squared.sqrt()
            );
        }
    }
}

#[test]
fn cubic_error_is_bounded_after_magnification_skew_and_viewbox() {
    for (source, controls) in [
        (
            r#"<svg transform="scale(1000)"><path d="M0 0C0 1 1 -1 1 0"/></svg>"#,
            [
                [0., 0., 0.],
                [0., 1000., 0.],
                [1000., -1000., 0.],
                [1000., 0., 0.],
            ],
        ),
        (
            r#"<svg><path transform="scale(1000 1)" d="M0 0C0 1 1 -1 1 0"/></svg>"#,
            [
                [0., 0., 0.],
                [0., 1., 0.],
                [1000., -1., 0.],
                [1000., 0., 0.],
            ],
        ),
        (
            r#"<svg><path transform="matrix(1 .25 20 1 0 0)" d="M0 0C0 1 1 -1 1 0"/></svg>"#,
            [
                [0., 0., 0.],
                [20., 1., 0.],
                [-19., -0.75, 0.],
                [1., 0.25, 0.],
            ],
        ),
        (
            r#"<svg width="1024" height="768" viewBox="0 0 16 16" preserveAspectRatio="none"><path d="M0 0C0 16 16 -16 16 0"/></svg>"#,
            [
                [0., 0., 0.],
                [0., 768., 0.],
                [1024., -768., 0.],
                [1024., 0., 0.],
            ],
        ),
        (
            r##"<svg width="800" height="600" viewBox="0 0 16 12"><defs><g id="curve" transform="matrix(1 0 2 1 0 0)"><path d="M0 0C0 1 1 -1 1 0"/></g></defs><g transform="scale(4 2)"><use href="#curve" x="3" y="5"/></g></svg>"##,
            [
                [600., 500., 0.],
                [1000., 600., 0.],
                [400., 400., 0.],
                [800., 500., 0.],
            ],
        ),
    ] {
        let document = SvgDocument::parse(source.as_bytes()).unwrap();
        assert_cubic_viewport_bound(&document, controls, fmn_geom::svg::DEFAULT_SVG_TOLERANCE);
        assert_eq!(
            dump(&document),
            dump(&SvgDocument::parse(source.as_bytes()).unwrap())
        );
    }
}

#[test]
fn explicit_svg_tolerance_is_validated_and_controls_refinement() {
    let source = br#"<svg><path d="M0 0C0 1 1 -1 1 0"/></svg>"#;
    let limits = SvgLimits::default();
    let ordinary = SvgDocument::parse(source).unwrap();
    let fine = SvgDocument::parse_with_tolerance(source, &limits, 1e-5).unwrap();
    assert!(fine.shapes[0].path.num_curves() > ordinary.shapes[0].path.num_curves());
    assert_cubic_viewport_bound(
        &fine,
        [[0., 0., 0.], [0., 1., 0.], [1., -1., 0.], [1., 0., 0.]],
        1e-5,
    );
    assert_eq!(
        dump(&ordinary),
        dump(
            &SvgDocument::parse_with_tolerance(
                source,
                &limits,
                fmn_geom::svg::DEFAULT_SVG_TOLERANCE
            )
            .unwrap()
        )
    );
    for tolerance in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        assert!(matches!(
            SvgDocument::parse_with_tolerance(source, &limits, tolerance),
            Err(SvgError::Geometry {
                source: fmn_geom::GeomError::InvalidTolerance
            })
        ));
    }
}

#[test]
fn svg_affine_rounding_cannot_spend_the_curve_error_budget_silently() {
    let source = br#"<svg><path transform="translate(1e16 0)" d="M0 0C0.6666666666666666 1.3333333333333333 1.3333333333333333 1.3333333333333333 2 0"/></svg>"#;
    assert!(matches!(
        SvgDocument::parse(source),
        Err(SvgError::Geometry {
            source: fmn_geom::GeomError::ToleranceUnreachable { .. }
        })
    ));
    // A moderate offset remains representable and keeps the viewport bound.
    let accepted = br#"<svg><path transform="translate(1000000 0)" d="M0 0C0 1 1 -1 1 0"/></svg>"#;
    let document = SvgDocument::parse(accepted).unwrap();
    assert_cubic_viewport_bound(
        &document,
        [
            [1e6, 0., 0.],
            [1e6, 1., 0.],
            [1e6 + 1., -1., 0.],
            [1e6 + 1., 0., 0.],
        ],
        fmn_geom::svg::DEFAULT_SVG_TOLERANCE,
    );
}

#[test]
fn svg_derived_geometry_has_a_document_wide_point_budget() {
    let limits = SvgLimits {
        max_output_points: 10,
        ..SvgLimits::default()
    };
    assert!(matches!(
        SvgDocument::parse_with_limits(
            br#"<svg><path transform="scale(10000)" d="M0 0C0 1 1 -1 1 0"/></svg>"#,
            &limits
        ),
        Err(SvgError::TooManyPoints { limit: 10 })
    ));
    let limits = SvgLimits {
        max_output_points: 20,
        ..SvgLimits::default()
    };
    assert!(matches!(SvgDocument::parse_with_limits(
        br##"<svg><defs><path id="p" d="M0 0L1 0L1 1L0 1Z"/></defs><use href="#p"/><use href="#p"/><use href="#p"/></svg>"##, &limits),
        Err(SvgError::TooManyPoints { limit: 20 })));
    assert!(matches!(
        SvgDocument::parse(br#"<svg><path transform="scale(1e300)" d="M0 0C0 1 1 -1 1 0"/></svg>"#),
        Err(SvgError::Geometry {
            source: fmn_geom::GeomError::ToleranceUnreachable { .. }
        })
    ));
}
