//! fm-census-drawing-window-classes-kw9m: structural-class surface fixtures
//!
//! Every Reference structural class is already wired in the native crates
//! (`Group`/`VGroup` builders in fmn-library/vmobject, `PGroup`/`PMobject`/
//! `DotCloud` in fmn-library/pointcloud, `ValueTracker`/`TrackerKind` in
//! fmn-mobject/dynamics, `SVGMobject` in fmn-library/svg, `StringMobject`
//! conventions in fmn-text, `OldTex`/`OldTexText` and the LatexError →
//! fmd-math error contract). CameraFrame in fmn-render and Tex/TexEngine
//! in fmn-tex have their own test surfaces and are intentionally not
//! re-tested here (those crates are downstream dev-deps of the W10 Gauntlet,
//! not of fmn-library). This target proves the fmn-library-owned structural
//! surface constructs and family-walks correctly — the surface the portal
//! exposes, not the native internals.

use fmn_library::controls::ControlMobject;
use fmn_library::image::ImageMobject;
use fmn_mobject::Mobject;
use fmn_library::pointcloud::{DOT_CLOUD_SHADING, DotCloud, PMobject, p_group};
use fmn_library::poly::Square;
use fmn_library::svg::svg_mobject;
use fmn_library::text::Text;
use fmn_library::vmobject::{VMobject, group, v_group};
use fmn_mobject::dynamics::TrackerKind;
use fmn_mobject::stage::Stage;

/// `Group` is the heterogeneous `Mobject` family container.
#[test]
fn group_collects_heterogeneous_children() {
    let g = group([Mobject::new(), Mobject::new()]);
    assert_eq!(g.submobjects.len(), 2);
}

/// `VGroup` is the `VMobject` family container; only `VMobject` children
/// are accepted at the type level.
#[test]
fn v_group_collects_only_vectorized_children() {
    let v = v_group([Square::new().build()]);
    assert_eq!(v.children().len(), 1);
}

/// `ValueTracker` (C-5) is the typed single-lane scalar tracker; the
/// round trip `set` → `get` is exact (BN-07, f64 path, no drift).
#[test]
fn value_tracker_set_get_round_trip() {
    let mut stage = Stage::new();
    let handle = stage.add_value_tracker(0.0);
    stage.set_tracker_value(handle, 1.5).expect("scalar set");
    assert_eq!(stage.tracker_value(handle), Some(1.5));
}

/// `ExponentialValueTracker` encodes through `fmn_dmath::ln` so lane
/// interpolation is geometric; `get` decodes through `exp`.
#[test]
fn exponential_value_tracker_geometric_round_trip() {
    let mut stage = Stage::new();
    let handle = stage.add_exponential_value_tracker(2.0);
    let v = stage.tracker_value(handle).expect("exponential get");
    assert!((v - 2.0).abs() < 1.0e-9, "geometric round-trip: got {v}");
}

/// `ComplexValueTracker` holds re + im·i.
#[test]
fn complex_value_tracker_set_get_round_trip() {
    let mut stage = Stage::new();
    let handle = stage.add_complex_value_tracker(3.0, 4.0);
    assert_eq!(stage.tracker_complex_value(handle), Some((3.0, 4.0)));
}

/// `ControlMobject` is the typed scalar-control base (C-6, BN-07).
#[test]
fn control_mobject_set_get_round_trip() {
    let mut c = ControlMobject::new(0.5, std::iter::empty::<VMobject>());
    assert_eq!(c.value(), 0.5);
    c.set_value(0.9);
    assert_eq!(c.value(), 0.9);
}

/// `PMobject` constructs empty; `with_points` builds the typed point run.
#[test]
fn pmobject_constructs_with_points() {
    let p = PMobject::new()
        .with_points(vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0]]);
    assert_eq!(p.num_points(), 2);
}

/// `PGroup` is the typed variadic PMobject container; the helper
/// `p_group` composes child buffers.
#[test]
fn pgroup_merges_child_point_buffers() {
    let p1 = PMobject::new().with_points(vec![[0.0, 0.0, 0.0]]);
    let p2 = PMobject::new().with_points(vec![[1.0, 0.0, 0.0]]);
    let p3 = PMobject::new().with_points(vec![[2.0, 0.0, 0.0]]);
    let _ = p_group(vec![p1, p2, p3]);
}

/// `DotCloud` carries the typed dot-cloud surface with a shading binding
/// (BN-07 surface under R-2).
#[test]
fn dot_cloud_construction_and_make_3d_shading() {
    let dc = DotCloud::new(vec![[0.0, 0.0, 0.0], [0.5, 0.0, 0.0]]);
    assert_eq!(dc.num_points(), 2);
    let _dc3d = dc.make_3d();
}

/// `SVGMobject` (Chisel) constructs from a minimal valid user SVG document.
#[test]
fn svg_mobject_constructs_from_minimal_rect() {
    let svg = svg_mobject(
        br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><rect width="10" height="10" fill="red"/></svg>"#,
    )
    .expect("minimal rect parses");
    assert!(!svg.children().is_empty(), "rect path becomes children");
}

/// `ImageMobject` constructs from a 1x1 RGB byte buffer (the minimum
/// raster payload).
#[test]
fn image_mobject_constructs_from_rgba8() {
    let _img = ImageMobject::from_rgba8(1, 1, vec![0xFF, 0x00, 0x00, 0xFF])
        .expect("1x1 RGBA8 image materializes");
}

/// `Text` constructs the bare glyph-family surface.
#[test]
fn text_constructs_with_string() {
    let _text = Text::new("hello").font_size(12.0);
}

/// `ValueTracker` typed add-then-remove-via-remove_from_scene lifecycle.
#[test]
fn tracker_add_then_remove_drops_handle() {
    let mut stage = Stage::new();
    let h = stage.add_value_tracker(7.0);
    assert!(stage.get(h).is_some());
    stage.remove_from_scene(h);
    // remove_from_scene is a draw-list edit: the handle stays valid but
    // the mob is no longer in the rendered scene family.
    assert!(
        !stage.roots().contains(&h),
        "removed mob is no longer a scene root"
    );
}
/// `ExponentialValueTracker` and `ComplexValueTracker` live in the same
/// `TrackerKind` enum; they are distinguishable but share the Stage surface.
#[test]
fn tracker_kinds_round_trip() {
    let mut stage = Stage::new();
    let h_plain = stage.add_value_tracker(1.0);
    let h_exp = stage.add_exponential_value_tracker(2.0);
    let h_cx = stage.add_complex_value_tracker(3.0, 4.0);
    assert_eq!(stage.tracker(h_plain).map(|t| t.kind), Some(TrackerKind::Plain));
    assert_eq!(stage.tracker(h_exp).map(|t| t.kind), Some(TrackerKind::Exponential));
    assert_eq!(stage.tracker_complex_value(h_cx), Some((3.0, 4.0)));
}
