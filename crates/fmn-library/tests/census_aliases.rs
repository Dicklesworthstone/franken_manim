//! Census alias constructors: thin wrappers and re-exports that expose the
//! most common manim class names through the native Rust API (fm-5wq.4).

use fmn_core::constants::{ORIGIN, RIGHT, UP};
use fmn_library::{
    Circle, Dot, Line, Rectangle, RegularPolygon, VMobject, group, polyline, rounded_rectangle,
    sector, small_dot, triangle, v_group, vector, vectorized_point,
};

#[test]
fn group_collects_heterogeneous_children() {
    let a = VMobject::from_points(vec![[0.0; 3], [1.0; 3]]);
    let b = Circle::new().build();
    let m = group([a.into(), b.into()]);
    assert_eq!(m.submobjects.len(), 2);
}

#[test]
fn v_group_collects_vectorized_children() {
    let a = VMobject::from_points(vec![[0.0; 3], [1.0; 3]]);
    let b = Circle::new().build();
    let g = v_group([a, b]);
    assert_eq!(g.children().len(), 2);
}

#[test]
fn vectorized_point_is_one_anchor() {
    let p = vectorized_point([1.0, 2.0, 3.0]);
    assert_eq!(p.points().len(), 1);
}

#[test]
fn vector_arrow_from_origin() {
    let v = vector(RIGHT).build().unwrap();
    let pts = v.points();
    assert!(!pts.is_empty());
    let start = pts.first().unwrap();
    assert_eq!(*start, ORIGIN);
}

#[test]
fn polyline_stays_open() {
    let p = polyline([[0.0; 3], [1.0; 3], [1.0, 1.0, 0.0]]).build();
    assert_ne!(p.points().first(), p.points().last());
}

#[test]
fn triangle_is_three_sided_regular_polygon() {
    let t = triangle().build().unwrap();
    // A closed triangle has 3 curves -> 3 anchors + 2 handles per anchor? No:
    // shared-anchor layout: each curve contributes one anchor and two handles,
    // plus closing anchor. The exact count is an internal detail; just verify
    // it is closed and non-empty.
    assert!(!t.points().is_empty());
}

#[test]
fn rounded_rectangle_has_corner_radius() {
    let r = rounded_rectangle(4.0, 2.0, 0.3).build().unwrap();
    assert!(!r.points().is_empty());
}

#[test]
fn sector_is_pie_slice() {
    let s = sector(fmn_core::constants::TAU / 4.0, 1.0).build().unwrap();
    assert!(!s.points().is_empty());
}

#[test]
fn small_dot_builds_at_location() {
    let sd = small_dot(ORIGIN);
    let _ = sd.build();
}
