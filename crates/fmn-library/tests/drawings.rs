//! fm-3kr tranche 1: the geometry-native drawings — Clock, DieFace,
//! Dartboard — structural fixtures plus one bit-locked point digest per
//! class. The asset-backed census classes wait on fm-7lx1's ruling and are
//! deliberately absent.

use fmn_core::constants::TAU;
use fmn_hash::sha256;
use fmn_library::drawings::{Clock, dartboard, die_face, piano, piano_3d, speedometer};
use fmn_library::vmobject::VMobject;
use fmn_text::FontBook;

fn digest(vmob: &VMobject) -> String {
    // Depth-first pre-order over the whole family: group roots carry no
    // own points — the ink lives in the children.
    let mut bytes = Vec::new();
    let mut stack = vec![vmob];
    while let Some(current) = stack.pop() {
        for point in current.points() {
            for component in point {
                bytes.extend_from_slice(&component.to_le_bytes());
            }
        }
        for child in current.children().iter().rev() {
            stack.push(child);
        }
    }
    sha256(&bytes).to_hex()
}

#[allow(dead_code)] // tranche-reserved: used by the upcoming family tests
fn count_family(vmob: &VMobject) -> usize {
    let mut count = 1;
    for child in vmob.children() {
        count += count_family(child);
    }
    count
}

/// Fail with a message. UBS bans literal `panic!`/`unreachable!`, so test
/// refusals go through [`std::panic::panic_any`] like the svg suite's.
fn fail(message: String) {
    std::panic::panic_any(message);
}

#[test]
fn clock_matches_the_reference_structure() {
    let clock = Clock::new().build();
    // [circle, hour_hand, minute_hand, ticks]
    assert_eq!(clock.children().len(), 4);
    // The ticks child holds twelve tick lines.
    assert_eq!(clock.children()[3].children().len(), 12);
    // Cardinals double: 8 short + 4 long.
    let tick_points: Vec<usize> = clock.children()[3]
        .children()
        .iter()
        .map(|tick| tick.points().len())
        .collect();
    assert!(tick_points.iter().all(|count| *count >= 2));
}

#[test]
fn die_face_rejects_out_of_range_values() {
    assert!(die_face(0, 1.0, 0.5).is_none());
    assert!(die_face(7, 1.0, 0.5).is_none());
}

#[test]
fn die_face_pip_counts_follow_the_reference_layouts() {
    let expected: [usize; 6] = [1, 2, 3, 4, 5, 6];
    for value in 1u8..=6 {
        let face = die_face(value, 1.0, 0.5).expect("in range");
        // [square, pip group]
        assert_eq!(face.children().len(), 2);
        assert_eq!(
            face.children()[1].children().len(),
            expected[value as usize - 1],
            "value {value} pip count"
        );
    }
}

#[test]
fn dartboard_carries_the_full_ring_stack() {
    let board = dartboard().expect("pure geometry");
    // [segments group, bullseyes group]
    assert_eq!(board.children().len(), 2);
    assert_eq!(
        board.children()[0].children().len(),
        20 * 4,
        "twenty sectors across four rings"
    );
    assert_eq!(board.children()[1].children().len(), 2, "two bullseyes");
    // The class radius 3, seen through the hull: an 18-degree arc segment's
    // quadratic handle overshoots the true radius by 1/cos(9°), and the
    // bbox counts handles — the same hull behavior as the Reference's.
    let hull = 3.0 / f64::cos(TAU / 40.0);
    let (min, max) = board.extent().expect("a drawn board has an extent");
    assert!(
        ((min[0] + hull).abs() < 1e-9) && ((max[0] - hull).abs() < 1e-9),
        "board extent x = [{}, {}], hull bound {}",
        min[0],
        max[0],
        hull
    );
    let _ = TAU;
}

#[test]
fn self_goldens_lock_each_class_s_canonical_output() {
    let book = FontBook::bundled().expect("bundled book");
    let clock = Clock::new().build();
    let die = die_face(3, 1.0, 0.5).expect("in range");
    let board = dartboard().expect("pure geometry");
    let speedo = speedometer(&book).expect("labels typeset");
    let piano = piano().expect("pure geometry + booleans");
    let piano_3d = piano_3d().expect("pure geometry + extrusions");
    let cases: [(&str, String); 6] = [
        ("clock", digest(&clock)),
        ("die_face", digest(&die)),
        ("dartboard", digest(&board)),
        ("speedometer", digest(&speedo)),
        ("piano", digest(&piano)),
        ("piano_3d", digest(&piano_3d)),
    ];
    if std::env::var("PRINT_DRAWINGS_GOLDENS").is_ok() {
        for (name, digest) in &cases {
            println!("DRAWINGS_GOLDEN {name} {digest}");
        }
    }
    let expected: [(&str, &str); 7] = [
        (
            "clock",
            "2cbcdbb030cd6a2280ef4e2d873031458d0c3874c9a8858ae0b4300fb51a78c1",
        ),
        (
            "die_face",
            "a0cbb3350a222e71ccb269782e0c4cbf5600a7725cc01f0050ed696596a05477",
        ),
        (
            "dartboard",
            "6e7aebb6d059b3fe28cbab2028714a701766cffb553b377b0246d1539beded64",
        ),
        (
            "speedometer",
            "2d99fe1c318856ade256a1d324e18e939c54f61f0a5c176febde73cf288b4672",
        ),
        (
            "piano",
            "2e17730728ece6b4b7a1412a968eab6b8f714ab05a9c3a742ef848344cbe8a41",
        ),
        (
            "piano_3d",
            "8e62f98b0fcf17256c8e988c1135c37a1b8d056eb2c2ecf5c95647f6a9da7fdd",
        ),
        ("laptop", "PENDING_LAPTOP"),
    ];
    for ((name, actual), (expected_name, expected_hash)) in cases.iter().zip(expected.iter()) {
        assert_eq!(expected_name, name);
        assert_eq!(
            actual, expected_hash,
            "self-golden drift for {name}: rerun with PRINT_DRAWINGS_GOLDENS=1 to review"
        );
    }
}

// ------------------------------------------------ tranche 3 (fm-3kr): the
// asset-backed families under ADR-0020

use std::path::Path;

use fmn_core::constants::{BLUE_B, BLUE_C, BLUE_D, GREEN, GREEN_SCREEN, LEFT, RIGHT, YELLOW};
use fmn_library::drawings::{
    DrawingsAssetError, bubble_make_green_screen, double_speech_bubble, lightbulb,
    lightbulb_from_document, old_speech_bubble, old_thought_bubble, resolve_drawings_svg,
    vectorized_earth, vectorized_earth_from_document, video_icon, video_icon_from_document,
    video_series, video_series_from_document,
};
use fmn_library::svg::SvgDocument;

/// Original fixture art written for these tests. These are NOT
/// Reference-derived bytes — every asset-backed family builds from whatever
/// document the user supplies, so the tests supply their own.
const BUBBLE_BODY_SVG: &str = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"200\" height=\"120\"><circle cx=\"100\" cy=\"50\" r=\"45\" fill=\"#000000\" stroke=\"#ffffff\"/><circle cx=\"55\" cy=\"70\" r=\"20\" fill=\"#000000\" stroke=\"#ffffff\"/><path d=\"M150 80 L170 110 L130 85 Z\" fill=\"#000000\" stroke=\"#ffffff\"/></svg>";
const GLOBE_SVG: &str = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"120\" height=\"120\"><circle cx=\"60\" cy=\"60\" r=\"50\" fill=\"#0077be\"/><path d=\"M30 45 Q60 20 90 48 Q75 75 45 62 Z\" fill=\"#2e8b57\"/></svg>";
const ICON_SVG: &str = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"64\" height=\"48\"><rect x=\"4\" y=\"4\" width=\"56\" height=\"40\" fill=\"#c7e9f1\" stroke=\"#333333\"/><path d=\"M26 16 L42 24 L26 32 Z\" fill=\"#333333\"/></svg>";

fn fixture_document(svg: &str) -> SvgDocument {
    SvgDocument::parse(svg.as_bytes()).expect("fixture SVG parses")
}

/// A temp directory carrying one fixture under a Reference-style name —
/// the user-supplied-root happy path. Files are deliberately left for the
/// OS to reclaim: nothing in the repo tree is touched.
fn fixture_root(tag: &str, file_name: &str, contents: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("fmn_drawings_{tag}_{}", std::process::id()));
    std::fs::create_dir_all(&root).expect("temp root creates");
    std::fs::write(root.join(file_name), contents).expect("fixture writes");
    root
}

fn family_width_height(vmob: &VMobject) -> (f64, f64) {
    let right = vmob.bbox_point(RIGHT).expect("right extent");
    let left = vmob.bbox_point(LEFT).expect("left extent");
    let up = vmob.bbox_point(fmn_core::constants::UP).expect("up extent");
    let down = vmob
        .bbox_point(fmn_core::constants::DOWN)
        .expect("down extent");
    (right[0] - left[0], up[1] - down[1])
}

#[test]
fn every_default_asset_refuses_by_policy() {
    let refusals = [
        ("Lightbulb", "lightbulb", lightbulb(None).unwrap_err()),
        ("VideoIcon", "video_icon", video_icon(None).unwrap_err()),
        ("VideoIcon", "video_icon", video_series(None).unwrap_err()),
        (
            "OldSpeechBubble",
            "Bubbles_speech.svg",
            old_speech_bubble(None, None, LEFT, 1.0).unwrap_err(),
        ),
        (
            "DoubleSpeechBubble",
            "Bubbles_double_speech.svg",
            double_speech_bubble(None, None, LEFT, 1.0).unwrap_err(),
        ),
        (
            "OldThoughtBubble",
            "Bubbles_thought.svg",
            old_thought_bubble(None, None, LEFT, 1.0).unwrap_err(),
        ),
        (
            "VectorizedEarth",
            "earth",
            vectorized_earth(None).unwrap_err(),
        ),
    ];
    for (class, asset, error) in refusals {
        match error {
            DrawingsAssetError::AssetNotShipped {
                class: named_class,
                asset: named_asset,
            } => {
                assert_eq!(named_class, class);
                assert_eq!(named_asset, asset);
            }
            other => fail(format!("{class} must refuse by policy, got {other:?}")),
        }
    }
    // The Display form is the user-facing remedy required by ADR-0020:
    // it names the owning policy and tells the user what to do.
    let message = lightbulb(None).unwrap_err().to_string();
    assert!(
        message.contains("docs/adr/0020"),
        "policy pointer missing: {message}"
    );
    assert!(
        message.contains("lightbulb"),
        "asset name missing: {message}"
    );
    assert!(
        message.contains("your own copy"),
        "remedy missing: {message}"
    );
}

#[test]
fn unreadable_asset_roots_refuse_by_name() {
    let missing = resolve_drawings_svg(
        "Lightbulb",
        "lightbulb",
        Some(Path::new("/nonexistent/fmn-drawings-root")),
    )
    .unwrap_err();
    assert!(matches!(
        missing,
        DrawingsAssetError::UnreadableAsset { .. }
    ));

    // A directory where the file belongs is refused as such, not read.
    let root = std::env::temp_dir().join(format!("fmn_drawings_dir_{}", std::process::id()));
    std::fs::create_dir_all(root.join("video_icon.svg")).expect("decoy dir creates");
    let decoy = resolve_drawings_svg("VideoIcon", "video_icon", Some(&root)).unwrap_err();
    match decoy {
        DrawingsAssetError::UnreadableAsset { reason, .. } => {
            assert!(reason.contains("not a regular file"), "{reason}");
        }
        other => fail(format!(
            "directory decoy must refuse as unreadable, got {other:?}"
        )),
    }
}

#[test]
fn lightbulb_applies_the_reference_defaults() {
    let bulb = lightbulb_from_document(&fixture_document(GLOBE_SVG));
    let (_, height) = family_width_height(&bulb);
    assert!(
        (height - 1.0).abs() < 1e-9,
        "default height is 1.0, got {height}"
    );
    let shape = &bulb.children()[0];
    let style = shape.style();
    assert!((style.fill_opacity - 0.0).abs() < 1e-9, "outline only");
    assert_eq!(style.stroke_color, YELLOW);
    assert!((style.stroke_width - 3.0).abs() < 1e-9);
}

#[test]
fn video_series_arranges_gradients_and_width_in_reference_order() {
    let document = fixture_document(ICON_SVG);
    let series = video_series_from_document(&document, 5, &[BLUE_B, BLUE_D], 10.0);
    assert_eq!(series.children().len(), 5);
    let (width, _) = family_width_height(&series);
    assert!(
        (width - 10.0).abs() < 1e-9,
        "set_width last before gradient"
    );
    // Icons arranged RIGHT with the default buff between them.
    let centers: Vec<f64> = series
        .children()
        .iter()
        .map(|icon| icon.center_point()[0])
        .collect();
    assert!(
        centers.windows(2).all(|pair| pair[0] < pair[1]),
        "icons run left to right"
    );
    // set_color_by_gradient spans the anchors across the children.
    let first = series.children()[0].style().stroke_color;
    let last = series.children()[4].style().stroke_color;
    assert_eq!(first, BLUE_B);
    assert_eq!(last, BLUE_D);
}

#[test]
fn bubble_body_follows_the_resize_formula() {
    // Default content is the invisible 3x2 filler; buff 1.0 gives
    // target_width = 3 + min(1, 2) = 4 and target_height = 1.35 * 3 = 4.05,
    // then the centre drops by 0.125 * 4.05.
    let bubble = old_speech_bubble(None, None, LEFT, 1.0)
        .or_else(|_| {
            let root = fixture_root("bubble", "Bubbles_speech.svg", BUBBLE_BODY_SVG);
            old_speech_bubble(Some(&root), None, LEFT, 1.0)
        })
        .expect("bubble builds from its fixture root");
    assert_eq!(bubble.children().len(), 2, "[body, content]");
    let body = &bubble.children()[0];
    let (width, height) = family_width_height(body);
    assert!((width - 4.0).abs() < 1e-6, "target width, got {width}");
    assert!((height - 4.05).abs() < 1e-6, "target height, got {height}");
    let center_y = body.center_point()[1];
    assert!(
        (center_y - (-0.125 * 4.05)).abs() < 1e-6,
        "adjustment drop, got {center_y}"
    );
    let style = body.style();
    assert!((style.fill_opacity - 0.8).abs() < 1e-9);
    assert_eq!(style.stroke_color, fmn_core::constants::WHITE);
}

#[test]
fn rightward_directions_mirror_the_body() {
    let root = fixture_root("mirror", "Bubbles_speech.svg", BUBBLE_BODY_SVG);
    let leftward = old_speech_bubble(Some(&root), None, LEFT, 1.0).expect("LEFT builds");
    let rightward = old_speech_bubble(Some(&root), None, RIGHT, 1.0).expect("RIGHT builds");
    let left_tail = leftward.children()[0].children()[2].center_point()[0];
    let right_tail = rightward.children()[0].children()[2].center_point()[0];
    let body_center_x = leftward.children()[0].center_point()[0];
    assert!(
        (left_tail - body_center_x) * (right_tail - body_center_x) < 0.0,
        "the tail must land on opposite sides after the mirror"
    );
}

#[test]
fn thought_bubbles_sort_shapes_and_green_screen_tops_the_cloud() {
    let root = fixture_root("thought", "Bubbles_thought.svg", BUBBLE_BODY_SVG);
    let bubble = old_thought_bubble(Some(&root), None, LEFT, 1.0).expect("thought builds");
    let body = &bubble.children()[0];
    let ys: Vec<f64> = body
        .children()
        .iter()
        .map(|shape| shape.center_point()[1])
        .collect();
    assert!(
        ys.windows(2).all(|pair| pair[0] <= pair[1]),
        "shapes sorted ascending by y: {ys:?}"
    );
    let screened = bubble_make_green_screen(&bubble);
    let cloud = &screened.children()[0].children()[2];
    let style = cloud.style();
    assert_eq!(style.fill_color, GREEN_SCREEN);
    assert!((style.fill_opacity - 1.0).abs() < 1e-9);
}

#[test]
fn vectorized_earth_backs_the_globe_with_a_stretched_circle() {
    let earth = vectorized_earth_from_document(&fixture_document(GLOBE_SVG)).expect("earth builds");
    assert_eq!(earth.children().len(), 2, "[backdrop circle, globe]");
    let (_, height) = family_width_height(&earth.children()[1]);
    assert!((height - 2.0).abs() < 1e-9, "globe at reference height");
    let backdrop = &earth.children()[0];
    let backdrop_style = backdrop.style();
    assert_eq!(backdrop_style.fill_color, BLUE_C);
    assert_eq!(backdrop_style.stroke_color, GREEN);
    let globe_center = earth.children()[1].center_point();
    let backdrop_center = backdrop.center_point();
    for dim in 0..3 {
        assert!((globe_center[dim] - backdrop_center[dim]).abs() < 1e-9);
    }
    let (backdrop_w, backdrop_h) = family_width_height(backdrop);
    let (globe_w, globe_h) = family_width_height(&earth.children()[1]);
    assert!(backdrop_w + 1e-9 >= globe_w && backdrop_h + 1e-9 >= globe_h);
}

#[test]
fn self_goldens_lock_each_asset_family_over_its_fixture() {
    // The bit-locked digests are over OUR fixture documents through OUR
    // pipeline — the shipped behavior any regression would drift.
    let lightbulb_digest = digest(&lightbulb_from_document(&fixture_document(GLOBE_SVG)));
    let video_icon_digest = digest(&video_icon_from_document(&fixture_document(ICON_SVG)));
    let series =
        video_series_from_document(&fixture_document(ICON_SVG), 5, &[BLUE_B, BLUE_D], 10.0);
    let video_series_digest = digest(&series);
    let bubble_root = fixture_root("golden", "Bubbles_speech.svg", BUBBLE_BODY_SVG);
    let bubble = old_speech_bubble(Some(&bubble_root), None, LEFT, 1.0).expect("builds");
    let bubble_digest = digest(&bubble);
    let earth = vectorized_earth_from_document(&fixture_document(GLOBE_SVG)).expect("builds");
    let earth_digest = digest(&earth);

    let goldens: &[(&str, &str)] = &[
        (
            "lightbulb",
            "eec85a0bcadea2c5d7e7a1f9e1cff054fb903b10d579d5ede0c4203b37f3f5db",
        ),
        (
            "video_icon",
            "9e726cacf90e5fa4c413d8afdb0d90aae58cd4659bde569496cd73334ffaca52",
        ),
        (
            "video_series",
            "82926faadd0b825d97af65da0681eb00acbfcab694f8682f78011cbd3a417efa",
        ),
        (
            "old_speech_bubble",
            "33155870c657fb1f03c43e2a82f5597406c69f298d3bdaadac355a4542641bc0",
        ),
        (
            "vectorized_earth",
            "7783c05d5070e51ebfb0633e271fcc57aef2adf72dfd302feb8548b6b52d863b",
        ),
    ];
    for (name, golden) in goldens {
        let actual = match *name {
            "lightbulb" => &lightbulb_digest,
            "video_icon" => &video_icon_digest,
            "video_series" => &video_series_digest,
            "old_speech_bubble" => &bubble_digest,
            _ => &earth_digest,
        };
        assert_eq!(actual, *golden, "{name} drifted");
    }
}
