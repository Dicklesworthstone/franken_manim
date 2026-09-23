use fmn_library::svg::{SvgPaintOverrides, svg_mobject_with_paints};
use fmn_library::VMobject;

fn build(source: &str) -> VMobject {
    svg_mobject_with_paints(source.as_bytes(), SvgPaintOverrides::default()).unwrap()
}
fn area(mob: &VMobject) -> f64 {
    mob.path().unwrap().subpaths().iter().map(|points| {
        points.iter().step_by(2).zip(points.iter().skip(2).step_by(2))
            .map(|(a,b)| (a[0]*b[1]-b[0]*a[1])/2.0).sum::<f64>()
    }).sum()
}
#[test]
fn even_odd_is_a_native_filled_set_not_the_original_double_winding() {
    let source = r#"<svg><path fill-rule="evenodd" d="M0 0H10V10H0Z M2 2H8V8H2Z"/></svg>"#;
    let svg = build(source);
    assert_eq!(svg.children().len(), 1);
    let fill = &svg.children()[0].children()[0];
    assert!((area(fill).abs()-64.0).abs()<1e-8, "{}", area(fill));
    assert_eq!(fill.style().stroke_opacity, 0.0);
}
#[test]
fn coincident_even_odd_fill_cancels_without_erasing_the_stroke() {
    let svg=build(r#"<svg><path fill-rule="evenodd" stroke="red" d="M0 0H10V10H0Z M0 0H10V10H0Z"/></svg>"#);
    let layers=svg.children()[0].children();
    assert_eq!(layers.len(),1);
    assert_eq!(layers[0].style().fill_opacity,0.0);
    assert_eq!(layers[0].points().len(),19);
}
#[test]
fn unequal_dashes_odd_lists_offsets_and_moves_use_svg_distance_units() {
    for (pattern,offset,expected) in [
        ("2 1 4 1",0,vec![(0.,2.),(3.,7.),(8.,10.)]),
        ("2 1 3",0,vec![(0.,2.),(3.,6.),(8.,9.)]),
        ("2 2",1,vec![(0.,1.),(3.,5.),(7.,9.)]),
        ("2 2",-1,vec![(1.,3.),(5.,7.),(9.,10.)]),
    ] {
        let svg=build(&format!(r#"<svg><path fill="none" stroke="red" stroke-dasharray="{pattern}" stroke-dashoffset="{offset}" d="M0 0H10 M0 5H10"/></svg>"#));
        let layers=svg.children()[0].children();
        assert_eq!(layers.len(),2*expected.len());
        for (row,group) in layers.chunks(expected.len()).enumerate() {
            for (dash,(a,b)) in group.iter().zip(&expected) {
                assert!((dash.points()[0][0]-a).abs()<1e-6);
                assert!((dash.points().last().unwrap()[0]-b).abs()<1e-6);
                assert_eq!(dash.points()[0][1],row as f64*5.);
            }
        }
    }
}
#[test]
fn dashed_fill_keeps_closed_outline_and_zero_gaps_merge() {
    let svg=build(r#"<svg><path fill="blue" stroke="red" stroke-dasharray="3 0 2 4" d="M0 0H10V10H0Z"/></svg>"#);
    let layers=svg.children()[0].children();
    assert!((area(&layers[0]).abs()-100.).abs()<1e-8);
    assert_eq!(layers[0].style().stroke_width,0.);
    assert!(layers[1..].iter().all(|m|m.style().fill_opacity==0.));
    let line=build(r#"<svg><path fill="none" stroke="red" stroke-dasharray="3 0 2 4" d="M0 0H10"/></svg>"#);
    let dashes=line.children()[0].children();
    assert_eq!(dashes.len(),2);
    assert!((dashes[0].points().last().unwrap()[0]-5.).abs()<1e-6);
}
#[test]
fn caps_are_not_inserted_at_a_closed_contour_seam() {
    let svg=build(r#"<svg><path fill="none" stroke="red" stroke-dasharray="8 4" stroke-dashoffset="2" d="M0 0H10V10H0Z"/></svg>"#);
    let dashes=svg.children()[0].children();
    assert_eq!(dashes.len(),3);
    assert!(dashes.iter().any(|dash| dash.points().iter().skip(1).take(dash.points().len()-2).any(|p| *p==[0.,0.,0.])));
}
#[test]
fn curved_dashes_measure_arc_length_not_control_polygon_or_curve_index() {
    let svg=build(r#"<svg><path fill="none" stroke="red" stroke-dasharray="1 1" d="M0 0Q0 10 10 10"/></svg>"#);
    let dashes=svg.children()[0].children();
    for dash in &dashes[..dashes.len()-1] {
        assert!((dash.path().unwrap().get_arc_length()-1.).abs()<1e-7);
    }
}
#[test]
fn caller_overrides_apply_before_paint_layer_split() {
    let svg=svg_mobject_with_paints(br#"<svg><path fill="blue" stroke="red" stroke-dasharray="2 2" d="M0 0H10V10H0Z"/></svg>"#,
        SvgPaintOverrides{fill_opacity:Some(0.3),stroke_opacity:Some(0.6),..Default::default()}).unwrap();
    let layers=svg.children()[0].children();
    assert_eq!(layers[0].style().fill_opacity,0.3);
    for stroke in &layers[1..] {assert_eq!(stroke.style().fill_opacity,0.);assert_eq!(stroke.style().stroke_opacity,0.6);}
}
#[test]
fn hostile_patterns_fail_before_unbounded_output_and_defaults_stay_unchanged() {
    for pattern in ["0 1","1e-300 1e-300"] {
        assert!(svg_mobject_with_paints(format!(r#"<svg><path fill="none" stroke="red" stroke-dasharray="{pattern}" d="M0 0H10"/></svg>"#).as_bytes(),Default::default()).is_err());
    }
    let bytes=br#"<svg><rect width="10" height="8" fill="blue" stroke="red"/></svg>"#;
    assert_eq!(svg_mobject_with_paints(bytes,Default::default()).unwrap(),fmn_library::svg_mobject(bytes).unwrap());
}
