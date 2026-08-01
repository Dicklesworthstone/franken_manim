//! Reproduction probe for fm-qjy: the transversal route must admit.

use fmn_geom::{BooleanOperation, BooleanOptions, BooleanRoute, QuadPath, path_boolean};

fn circle(radius: f64, center: [f64; 2]) -> QuadPath {
    QuadPath::arc(
        0.0,
        fmn_core::constants::TAU,
        radius,
        [center[0], center[1], 0.0],
        None,
    )
}

#[test]
fn overlapping_circles_admit_transversal_route() {
    let subject = circle(2.0, [0.0, 0.0]);
    let clip = circle(1.5, [1.0, 0.0]);
    for operation in [
        BooleanOperation::Union,
        BooleanOperation::Intersection,
        BooleanOperation::Difference,
        BooleanOperation::Exclusion,
    ] {
        let result = path_boolean(&subject, &clip, operation, BooleanOptions::default())
            .expect("boolean must succeed");
        eprintln!("{operation:?}: route = {:?}", result.route);
        assert_eq!(
            result.route,
            BooleanRoute::CurveAwareTransversal,
            "{operation:?} declined"
        );
    }
}
