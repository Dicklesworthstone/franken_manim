use fmn_geom::{BooleanOperation, BooleanOptions, BooleanRoute, QuadPath, path_boolean};

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> QuadPath {
    let mut p = QuadPath::new();
    p.start_new_path([x0, y0, 0.0]);
    for q in [[x1, y0], [x1, y1], [x0, y1], [x0, y0]] {
        p.add_line_to([q[0], q[1], 0.0], true).unwrap();
    }
    p
}

fn main() {
    let circle = QuadPath::arc(0.0, fmn_core::constants::TAU, 2.0, [0.0, 0.0, 0.0], None);
    let clip = rect(-1.0, -3.0, 1.0, 3.0);
    for op in [
        BooleanOperation::Union,
        BooleanOperation::Intersection,
        BooleanOperation::Difference,
        BooleanOperation::Exclusion,
    ] {
        let r = path_boolean(&circle, &clip, op, BooleanOptions::default()).unwrap();
        println!(
            "{op:?}: route={:?} contours={} segments={} intersections={} area_z={:.6}",
            r.route,
            r.stats.output_contours,
            r.stats.output_segments,
            r.stats.intersections,
            r.path.area_vector()[2]
        );
    }
    let big = QuadPath::arc(0.0, fmn_core::constants::TAU, 3.0, [0.0, 0.0, 0.0], None);
    let small = QuadPath::arc(0.0, fmn_core::constants::TAU, 1.0, [0.0, 0.0, 0.0], None);
    for op in [
        BooleanOperation::Union,
        BooleanOperation::Intersection,
        BooleanOperation::Difference,
    ] {
        let r = path_boolean(&big, &small, op, BooleanOptions::default()).unwrap();
        println!(
            "nested {op:?}: route={:?} contours={} area_z={:.6}",
            r.route,
            r.stats.output_contours,
            r.path.area_vector()[2]
        );
    }
    assert_eq!(
        path_boolean(
            &circle,
            &clip,
            BooleanOperation::Union,
            BooleanOptions::default()
        )
        .unwrap()
        .route,
        BooleanRoute::CurveAwareTransversal
    );
}
