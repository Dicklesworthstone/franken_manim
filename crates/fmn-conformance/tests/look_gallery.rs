//! The Look Gallery plane (§16.3 plane 3, fm-t1v): metric sanity, manifest
//! mechanics, verdict transitions, and the smoke-alarm demo over the real
//! captured pairs.
//!
//! The metrics are smoke alarms, never gates: the demo test reports numbers
//! and fails only on manifest corruption or a missing committed render. The
//! Reference captures are private §15.3 fixtures (gitignored); a checkout
//! without them simply skips the measurement and says so.

use fmn_conformance::gallery::{
    GalleryError, GalleryManifest, PairMetrics, RgbaView, Verdict, compare_pair, render_pairs,
};
use std::path::{Path, PathBuf};

// ------------------------------------------------------------ image helpers

/// Build a tight RGBA8 image from a per-pixel function.
fn image(width: u32, height: u32, f: impl Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
    let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            pixels.extend_from_slice(&f(x, y));
        }
    }
    pixels
}

/// A 96×96 dark field with a filled bright square at (`ox`, `oy`) — a shape
/// with real Sobel edges, so the edge metric has something to measure.
fn square_image(ox: u32, oy: u32) -> Vec<u8> {
    image(96, 96, |x, y| {
        let inside = (ox..ox + 40).contains(&x) && (oy..oy + 40).contains(&y);
        if inside {
            [200, 180, 160, 255]
        } else {
            [20, 24, 32, 255]
        }
    })
}

fn view(width: u32, height: u32, pixels: &[u8]) -> RgbaView<'_> {
    RgbaView::new(width, height, pixels).expect("the synthetic image is valid")
}

fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("look_gallery_{name}"));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the workspace root exists")
}

fn committed_manifest_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/look_gallery.tsv")
}

// ----------------------------------------------------------- metric sanity

#[test]
fn identical_images_have_perfect_metrics() {
    let pixels = image(64, 64, |x, y| {
        [
            ((x * 3 + y) % 256) as u8,
            ((x + y * 5) % 256) as u8,
            ((x * 7 + y * 11) % 256) as u8,
            255,
        ]
    });
    let a = view(64, 64, &pixels);
    let b = view(64, 64, &pixels);
    let m: PairMetrics = compare_pair(&a, &b).expect("same dimensions");
    assert!(
        (m.ssim - 1.0).abs() < 1e-12,
        "identical images must score SSIM 1.0, got {}",
        m.ssim
    );
    assert!(
        m.edge.symmetric.abs() < 1e-12,
        "identical images must have zero edge distance, got {}",
        m.edge.symmetric
    );
    for value in [m.error.p50, m.error.p95, m.error.p99, m.error.max] {
        assert!(
            value.abs() < 1e-12,
            "identical images must have zero error at every percentile, got {value}"
        );
    }
}

#[test]
fn shifted_edges_move_the_metrics_in_the_right_direction() {
    let base = square_image(10, 10);
    let near_pixels = square_image(14, 10);
    let far_pixels = square_image(30, 10);
    let reference = view(96, 96, &base);
    let near = view(96, 96, &near_pixels);
    let far = view(96, 96, &far_pixels);

    let near_metrics = compare_pair(&reference, &near).expect("same dimensions");
    let far_metrics = compare_pair(&reference, &far).expect("same dimensions");

    assert!(
        near_metrics.edge.symmetric > 0.0,
        "a shifted edge must have positive edge distance"
    );
    // A pure 4 px horizontal translation moves the square's vertical edges
    // by 4 px, but the horizontal edges partially overlap the original (36
    // of 40 px at distance 0), so the symmetric mean lands strictly below 4.
    assert!(
        near_metrics.edge.symmetric > 1.0 && near_metrics.edge.symmetric < 4.0,
        "a 4 px shift should read as single-digit edge distance, got {}",
        near_metrics.edge.symmetric
    );
    assert!(
        far_metrics.edge.symmetric > 2.0 * near_metrics.edge.symmetric,
        "a 20 px shift must outdistance a 4 px shift by a wide margin: {} vs {}",
        far_metrics.edge.symmetric,
        near_metrics.edge.symmetric
    );
    assert!(
        near_metrics.ssim < 1.0 && far_metrics.ssim < near_metrics.ssim,
        "SSIM must fall as the shift grows: near {}, far {}",
        near_metrics.ssim,
        far_metrics.ssim
    );
    // A 4 px shift touches ~3.5% of pixels (p95 still zero); a 20 px shift
    // touches ~17%, pushing the p95 off zero.
    assert!(
        far_metrics.error.p95 > near_metrics.error.p95,
        "the bigger shift touches more pixels, so the p95 error must rise: {} vs {}",
        far_metrics.error.p95,
        near_metrics.error.p95
    );
    // The red channel jumps 180 codes (200 vs 20): 180/255 ≈ 0.706.
    assert!(
        near_metrics.error.max > 0.7,
        "a 180-code channel jump must register near full scale, got {}",
        near_metrics.error.max
    );
}

#[test]
fn impulse_noise_moves_the_tail_not_the_median() {
    let base = square_image(10, 10);
    let mut noisy = base.clone();
    // One percent of pixels, deterministic positions, flipped to white.
    let total = 96 * 96;
    for i in 0..total {
        if i % 100 == 0 {
            noisy[i * 4] = 255;
            noisy[i * 4 + 1] = 255;
            noisy[i * 4 + 2] = 255;
        }
    }
    let reference = view(96, 96, &base);
    let candidate = view(96, 96, &noisy);
    let m = compare_pair(&reference, &candidate).expect("same dimensions");
    assert!(
        m.error.p50.abs() < 1e-12 && m.error.p95.abs() < 1e-12,
        "1% impulse noise must not move the median or the p95, got p50 {} p95 {}",
        m.error.p50,
        m.error.p95
    );
    // Nearest-rank p99 catches the smallest of the 93 nonzero errors: a flip
    // inside the square (a 95-code jump), well above the untouched p95.
    assert!(
        m.error.p99 > 0.3,
        "1% impulse noise must light up the p99 tail, got {}",
        m.error.p99
    );
    assert!(
        m.error.max > 0.9,
        "the darkest flipped pixel is 32 codes off white, got {}",
        m.error.max
    );
    assert!(
        m.ssim < 1.0,
        "impulse noise must register on SSIM, got {}",
        m.ssim
    );
}

#[test]
fn flat_images_have_no_edges_and_exact_error() {
    let dark = image(32, 32, |_, _| [0, 0, 0, 255]);
    let grey = image(32, 32, |_, _| [51, 51, 51, 255]);
    let m = compare_pair(&view(32, 32, &dark), &view(32, 32, &grey)).expect("same dimensions");
    assert_eq!(m.edge.reference_edges, 0);
    assert_eq!(m.edge.candidate_edges, 0);
    assert!(
        m.edge.symmetric.abs() < 1e-12,
        "two edge-free images have zero edge distance"
    );
    assert!(
        (m.error.max - 0.2).abs() < 1e-9,
        "a uniform 51-code jump is exactly 0.2 normalized, got {}",
        m.error.max
    );
}

#[test]
fn one_sided_edges_report_the_frame_diagonal() {
    let flat = image(96, 96, |_, _| [20, 24, 32, 255]);
    let shaped = square_image(10, 10);
    let m = compare_pair(&view(96, 96, &flat), &view(96, 96, &shaped)).expect("same dimensions");
    let diagonal = (96.0_f64 * 96.0 + 96.0 * 96.0).sqrt();
    assert!(
        m.edge.reference_to_candidate.abs() < 1e-12,
        "the edge-free reference contributes zero in its direction"
    );
    assert!(
        (m.edge.candidate_to_reference - diagonal).abs() < 1e-9,
        "edges with nothing to match read as the frame diagonal: {} vs {diagonal}",
        m.edge.candidate_to_reference
    );
}

#[test]
fn mismatched_dimensions_are_a_named_error() {
    let small = image(32, 32, |_, _| [0, 0, 0, 255]);
    let large = image(64, 64, |_, _| [0, 0, 0, 255]);
    let err = compare_pair(&view(32, 32, &small), &view(64, 64, &large))
        .expect_err("different sizes must refuse");
    assert!(
        err.to_string().contains("dimension mismatch"),
        "unexpected error: {err}"
    );
}

#[test]
fn image_views_refuse_dimension_products_that_overflow_rgba8_length() {
    let error = RgbaView::new(1 << 31, 1 << 31, &[])
        .expect_err("overflowing dimensions must fail without inspecting a wrapped length");
    assert!(
        error
            .to_string()
            .contains("overflow the addressable RGBA8 byte length"),
        "unexpected error: {error}"
    );

    let error = RgbaView::new(2, 3, &[0; 23])
        .expect_err("ordinary wrong lengths must retain their named refusal");
    assert!(error.to_string().contains("expected 24"));

    assert!(RgbaView::new(2, 3, &[0; 24]).is_ok());
}

// ---------------------------------------------------------- manifest rules

#[test]
fn committed_manifest_parses_and_round_trips_byte_for_byte() {
    let path = committed_manifest_path();
    let text = std::fs::read_to_string(&path).expect("committed manifest");
    let manifest = GalleryManifest::parse(&text).expect("the committed manifest parses");
    assert_eq!(
        manifest.revision, 1,
        "the seeded manifest starts at revision 1"
    );
    assert_eq!(manifest.rows.len(), 5, "five panels have committed renders");
    assert_eq!(
        manifest.to_text(),
        text,
        "the committed manifest must be in canonical form"
    );
    // The seeded verdicts are the G1 verdict sheet, not fresh judgments.
    let verdict = |panel: &str| {
        manifest
            .row(panel)
            .expect("the seeded manifest covers every G1 panel")
            .verdict
    };
    assert_eq!(verdict("self_intersections"), Verdict::AtLeastAsGood);
    assert_eq!(verdict("joints_and_caps"), Verdict::DifferentButFine);
    assert_eq!(verdict("glow"), Verdict::AtLeastAsGood);
    assert_eq!(verdict("gradient_fills"), Verdict::DifferentButFine);
    assert_eq!(verdict("lighting_3d"), Verdict::AtLeastAsGood);
}

#[test]
fn corrupt_manifests_are_named_errors() {
    let cases: [(&str, &str); 8] = [
        ("", "empty manifest"),
        ("# fmn-look-gallery v1\n", "revision"),
        (
            "# fmn-look-gallery v9\n# revision: 1\n",
            "first line must be",
        ),
        (
            "# fmn-look-gallery v1\n# revision: soon\n",
            "not a non-negative integer",
        ),
        (
            "# fmn-look-gallery v1\n# revision: 1\npanel\tonly-two\n",
            "expected 5 tab-separated fields",
        ),
        (
            "# fmn-look-gallery v1\n# revision: 1\nPanel\ta/b.png\tc/d.png\tregression\tnote\n",
            "invalid panel id",
        ),
        (
            "# fmn-look-gallery v1\n# revision: 1\npanel\t../escape.png\tc/d.png\tregression\tnote\n",
            "invalid reference path",
        ),
        (
            "# fmn-look-gallery v1\n# revision: 1\npanel\ta/b.png\tc/d.png\tlooks-good\tnote\n",
            "unknown verdict",
        ),
    ];
    for (text, needle) in cases {
        let err = GalleryManifest::parse(text).expect_err("corrupt input must refuse");
        assert!(
            err.to_string().contains(needle),
            "expected {needle:?} in: {err}"
        );
    }
    let duplicate = "# fmn-look-gallery v1\n# revision: 1\n\
        panel\ta/b.png\tc/d.png\tregression\tnote\n\
        panel\ta/b.png\tc/d.png\tregression\tnote\n";
    let err = GalleryManifest::parse(duplicate).expect_err("duplicate panel must refuse");
    assert!(err.to_string().contains("duplicate panel"), "{err}");
    let empty_note =
        "# fmn-look-gallery v1\n# revision: 1\npanel\ta/b.png\tc/d.png\tregression\t\n";
    let err = GalleryManifest::parse(empty_note).expect_err("empty change note must refuse");
    assert!(err.to_string().contains("empty change note"), "{err}");
}

// ------------------------------------------------------- verdict workflow

#[test]
fn verdict_transitions_are_recorded_and_regressions_found() {
    let text = std::fs::read_to_string(committed_manifest_path()).expect("committed manifest");
    let baseline = GalleryManifest::parse(&text).expect("parses");
    let mut current = baseline.clone();

    // A deliberate worsening, with its reason — the human review act.
    let change = current
        .record_verdict(
            "glow",
            Verdict::Regression,
            "fm-t1v demo: glow falloff visibly clipped after radius rework",
        )
        .expect("known panel");
    assert_eq!(change.from, Some(Verdict::AtLeastAsGood));
    assert_eq!(change.to, Verdict::Regression);
    assert_eq!(current.revision, baseline.revision + 1);

    // An improvement in the same revision span must not read as a
    // regression: a manifest carrying only the improvement reports none.
    let mut improved = baseline.clone();
    improved
        .record_verdict(
            "gradient_fills",
            Verdict::AtLeastAsGood,
            "fm-t1v demo: owner signed off; field is at-least-as-good",
        )
        .expect("known panel");
    assert!(
        improved.regressions_since(&baseline).is_empty(),
        "moving from worse to better is never a regression"
    );

    // Both movements in one span: only the worsening is reported.
    current
        .record_verdict(
            "gradient_fills",
            Verdict::AtLeastAsGood,
            "fm-t1v demo: owner signed off; field is at-least-as-good",
        )
        .expect("known panel");

    let regressions = current.regressions_since(&baseline);
    assert_eq!(
        regressions.len(),
        1,
        "only the worsened panel is a regression: {regressions:?}"
    );
    assert_eq!(regressions[0].panel, "glow");
    assert_eq!(regressions[0].from, Some(Verdict::AtLeastAsGood));
    assert_eq!(regressions[0].to, Verdict::Regression);

    // Diffed backwards, the improvement reads as the (correct) worsening of
    // gradient_fills relative to the improved manifest.
    let backwards = baseline.regressions_since(&current);
    assert_eq!(backwards.len(), 1);
    assert_eq!(backwards[0].panel, "gradient_fills");

    // Persistence: save → load is lossless and the TSV round-trips.
    let dir = scratch("verdicts");
    let path = dir.join("look_gallery.tsv");
    current.save(&path).expect("save");
    let reloaded = GalleryManifest::load(&path).expect("reload");
    assert_eq!(reloaded, current);
    assert_eq!(
        std::fs::read_to_string(&path).expect("file bytes"),
        current.to_text(),
        "the file holds exactly the canonical text"
    );

    // Refusals stay named errors.
    let err = current
        .record_verdict("no_such_panel", Verdict::Regression, "note")
        .expect_err("unknown panel must refuse");
    assert!(err.to_string().contains("unknown gallery panel"), "{err}");
    let err = current
        .record_verdict("glow", Verdict::Regression, "tab\tin note")
        .expect_err("a tab would corrupt the TSV");
    assert!(err.to_string().contains("tabs or newlines"), "{err}");
}

#[test]
fn verdict_updates_refuse_revision_rollover_without_partial_mutation() {
    let text = "# fmn-look-gallery v1\n# revision: 18446744073709551615\n\
        panel\ta/b.png\tc/d.png\tat-least-as-good\toriginal note\n";
    let mut manifest = GalleryManifest::parse(text).expect("maximum u64 revision is valid v1");
    let before = manifest.clone();

    let error = manifest
        .record_verdict("panel", Verdict::Regression, "replacement note")
        .expect_err("the monotone revision must not roll over");
    assert!(matches!(error, GalleryError::RevisionOverflow));
    assert_eq!(
        manifest, before,
        "a refused revision advance must not mutate the verdict or note"
    );

    let error = manifest
        .record_verdict("missing", Verdict::Regression, "replacement note")
        .expect_err("unknown-panel refusal retains precedence");
    assert!(matches!(error, GalleryError::UnknownPanel(panel) if panel == "missing"));
    assert_eq!(manifest, before);
}

#[test]
fn regressions_since_flags_new_regression_panels_only() {
    let parse = |rows: &str| {
        GalleryManifest::parse(&format!("# fmn-look-gallery v1\n# revision: 1\n{rows}"))
            .expect("well-formed")
    };
    let earlier = parse("alpha\ta/b.png\tc/d.png\tat-least-as-good\tnote\n");
    // A panel entering at different-but-fine is a review item, not a regression.
    let with_review_item = parse(
        "alpha\ta/b.png\tc/d.png\tat-least-as-good\tnote\n\
         beta\ta/b.png\tc/d.png\tdifferent-but-fine\tnote\n",
    );
    assert!(with_review_item.regressions_since(&earlier).is_empty());
    // A panel entering at regression is one.
    let with_regression = parse(
        "alpha\ta/b.png\tc/d.png\tat-least-as-good\tnote\n\
         beta\ta/b.png\tc/d.png\tregression\tnote\n",
    );
    let found = with_regression.regressions_since(&earlier);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].panel, "beta");
    assert_eq!(found[0].from, None);
}

// ------------------------------------------------------------ the demo run

/// Decode a PNG via the dev-dependency codec into an RGBA8 buffer.
fn load_rgba(path: &Path) -> (u32, u32, Vec<u8>) {
    let bytes = std::fs::read(path).expect("read image");
    let decoded = fmn_codec::decode_png(&bytes, &fmn_codec::PngLimits::default())
        .expect("the gallery images are valid PNGs");
    (decoded.width, decoded.height, decoded.rgba)
}

/// The smoke-alarm demo over the real captured pairs: every committed render
/// named by the manifest must exist (a missing pair fails), and where the
/// private Reference captures are present the three metrics are computed and
/// reported — never asserted.
#[test]
fn smoke_alarm_over_the_real_pairs() {
    let manifest = GalleryManifest::load(&committed_manifest_path()).expect("manifest parses");
    let pairs = render_pairs(&manifest, &repo_root()).expect("no missing committed renders");
    assert_eq!(
        pairs.len(),
        5,
        "the manifest covers exactly the five committed panels"
    );

    let measurable: Vec<_> = pairs.iter().filter(|p| p.reference_present).collect();
    if measurable.is_empty() {
        eprintln!(
            "look gallery: private Reference captures not present in this checkout \
             (gallery/reference_captures/ is gitignored per §15.3); smoke alarm skipped, \
             {} committed renders verified present",
            pairs.len()
        );
        return;
    }

    eprintln!("look gallery smoke alarm (informational; thresholds are verdict inputs):");
    eprintln!(
        "{panel:<20} {ssim:>12} {edge:>10} {p50:>8} {p95:>8} {p99:>8} {max:>8}",
        panel = "panel",
        ssim = "ssim",
        edge = "edge-px",
        p50 = "p50",
        p95 = "p95",
        p99 = "p99",
        max = "max"
    );
    for pair in &measurable {
        let (rw, rh, reference_pixels) = load_rgba(&pair.reference);
        let (cw, ch, candidate_pixels) = load_rgba(&pair.render);
        let reference = RgbaView::new(rw, rh, &reference_pixels).expect("reference view");
        let candidate = RgbaView::new(cw, ch, &candidate_pixels).expect("candidate view");
        let m = compare_pair(&reference, &candidate)
            .expect("captures and renders share the capture resolution");
        eprintln!(
            "{panel:<20} {ssim:>12.9} {edge:>10.3} {p50:>8.5} {p95:>8.5} {p99:>8.5} {max:>8.5}",
            panel = pair.panel,
            ssim = m.ssim,
            edge = m.edge.symmetric,
            p50 = m.error.p50,
            p95 = m.error.p95,
            p99 = m.error.p99,
            max = m.error.max
        );
    }
    let skipped: Vec<_> = pairs
        .iter()
        .filter(|p| !p.reference_present)
        .map(|p| p.panel.as_str())
        .collect();
    if !skipped.is_empty() {
        eprintln!("look gallery: skipped pairs without a capture in this checkout: {skipped:?}");
    }
}
