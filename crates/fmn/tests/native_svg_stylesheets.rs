//! Public native import, animation and rendering of embedded SVG stylesheets.

#[path = "support/svg_stylesheets.rs"]
mod svg_stylesheets;

#[test]
fn stylesheet_imports_match_authored_paints_at_every_worker_count() {
    let witness = svg_stylesheets::verify().expect("embedded SVG stylesheet render witness");
    assert_eq!(witness.frames, 12);
    assert_ne!(witness.first, witness.last);
}
