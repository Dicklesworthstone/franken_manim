//! fm-fc53: point-free layout anchors are positions, not phantom geometry.
use fmn_core::constants::{DOWN, ORIGIN, OUT};
use fmn_mobject::{Mobject, Placement, Snapshot, Stage};

#[test]
fn empty_layout_anchor_supports_shift_move_to_and_next_to() {
    let mut stage = Stage::new();
    let anchor = stage.add(Mobject::new());
    let square = stage.add(Mobject::from_points(&[
        [-1.0, -1.0, 0.0],
        [1.0, 1.0, 0.0],
    ]));
    let revision = stage.get(anchor).unwrap().buffer.revision();
    assert_eq!(stage.get_center(anchor), ORIGIN);

    stage.shift(anchor, [2.0, 0.0, 0.0]);
    assert_eq!(stage.get_center(anchor), [2.0, 0.0, 0.0]);
    stage.move_to(anchor, [0.0, 3.0, 0.0], ORIGIN);
    assert_eq!(stage.get_center(anchor), [0.0, 3.0, 0.0]);
    stage.next_to(anchor, square, DOWN, 0.25, ORIGIN);
    assert_eq!(stage.get_center(anchor), [0.0, -1.25, 0.0]);

    let bbox = stage.get_bounding_box(anchor);
    assert_eq!(bbox.min, bbox.mid);
    assert_eq!(bbox.mid, bbox.max);
    assert_eq!(stage.get_points(anchor).unwrap(), Vec::<[f64; 3]>::new());
    assert_eq!(stage.get(anchor).unwrap().buffer.revision(), revision);
}

#[test]
fn empty_anchor_affine_operations_use_the_requested_pivot() {
    let mut stage = Stage::new();
    let anchor = stage.add(Mobject::new());
    stage.shift(anchor, [2.0, 3.0, 0.0]);
    stage.scale(anchor, 7.0); // Scaling about its own center must not move it.
    assert_eq!(stage.get_center(anchor), [2.0, 3.0, 0.0]);
    stage.scale_about(anchor, 2.0, Some(ORIGIN), None);
    assert_eq!(stage.get_center(anchor), [4.0, 6.0, 0.0]);
    stage.rotate(
        anchor,
        std::f64::consts::FRAC_PI_2,
        OUT,
        Some(ORIGIN),
        None,
    );
    for (actual, expected) in stage.get_center(anchor).into_iter().zip([-6.0, 4.0, 0.0]) {
        assert!((actual - expected).abs() < 1e-12);
    }
}

#[test]
fn empty_points_readback_and_live_views_do_not_erase_the_anchor() {
    let mut stage = Stage::new();
    let anchor = stage.add(Mobject::new());
    stage.shift(anchor, [2.0, 3.0, 4.0]);
    assert!(!stage.bake_placement(anchor).unwrap());
    let view = stage
        .get_mut(anchor)
        .unwrap()
        .buffer
        .export_field_view("point", true)
        .unwrap();
    assert_eq!(stage.get_center(anchor), [2.0, 3.0, 4.0]);
    stage.shift(anchor, [1.0, -2.0, 3.0]);
    stage.bake_family_placements(anchor).unwrap();
    assert_eq!(stage.get_center(anchor), [3.0, 1.0, 7.0]);
    assert!(view.is_attached_to(&stage.get(anchor).unwrap().buffer));
    assert_eq!(stage.get(anchor).unwrap().buffer.len(), 0);
    drop(view);
    assert_eq!(stage.get_center(anchor), [3.0, 1.0, 7.0]);
}

#[test]
fn empty_parents_and_siblings_do_not_expand_real_geometry_bounds() {
    let mut stage = Stage::new();
    let parent = stage.add(Mobject::new());
    let empty = stage.add(Mobject::new());
    let child = stage.add(Mobject::from_points(&[
        [10.0, 12.0, 0.0],
        [20.0, 14.0, 0.0],
    ]));
    stage.shift(parent, [-100.0, -100.0, 0.0]);
    stage.shift(empty, [100.0, 100.0, 0.0]);
    stage.attach(parent, empty).unwrap();
    stage.attach(parent, child).unwrap();
    assert_eq!(stage.get_bounding_box(parent), stage.get_bounding_box(child));
    assert_eq!(stage.get_center(parent), [15.0, 13.0, 0.0]);
    stage.shift(parent, [1.0, 2.0, 3.0]);
    assert_eq!(stage.get_center(parent), [16.0, 15.0, 3.0]);
    assert_eq!(stage.get_bounding_box(parent), stage.get_bounding_box(child));
    assert_eq!(stage.get_center(empty), [101.0, 102.0, 3.0]);
}

#[test]
fn empty_anchor_cache_invalidates_on_placement_and_geometry_changes() {
    let mut stage = Stage::new();
    let anchor = stage.add(Mobject::new());
    assert_eq!(stage.get_center(anchor), ORIGIN);
    let first = stage.bbox_materializations(anchor);
    assert_eq!(stage.get_center(anchor), ORIGIN);
    assert_eq!(stage.bbox_materializations(anchor), first);
    stage.shift(anchor, [8.0, 4.0, 0.0]);
    assert_eq!(stage.get_center(anchor), [8.0, 4.0, 0.0]);
    assert_eq!(stage.bbox_materializations(anchor), first + 1);
    stage.set_points(anchor, &[[1.0, 2.0, 3.0]]).unwrap();
    assert_eq!(stage.placement(anchor), Some(Placement::IDENTITY));
    assert_eq!(stage.get_center(anchor), [1.0, 2.0, 3.0]);
    stage.set_points(anchor, &[]).unwrap();
    assert_eq!(stage.get_center(anchor), ORIGIN);
}

#[test]
fn empty_anchor_survives_copy_snapshot_restore_and_durable_decode() {
    let mut stage = Stage::new();
    let anchor = stage.add(Mobject::new());
    stage.shift(anchor, [2.0, -3.0, 4.0]);
    stage.add_to_scene(anchor).unwrap();
    let copy = stage.copy_family(anchor).unwrap();
    assert_eq!(stage.get_center(copy), [2.0, -3.0, 4.0]);
    let snapshot = stage.snapshot();
    let bytes = snapshot.to_bytes().unwrap();
    stage.shift(anchor, [10.0, 20.0, 30.0]);
    assert_eq!(snapshot.materialize().get_center(anchor), [2.0, -3.0, 4.0]);
    stage.restore(&snapshot);
    assert_eq!(stage.get_center(anchor), [2.0, -3.0, 4.0]);
    let decoded = Snapshot::from_bytes(&bytes, &stage).unwrap();
    let replay = decoded.snapshot.materialize();
    assert_eq!(replay.get_center(anchor), [2.0, -3.0, 4.0]);
    assert_eq!(replay.get_center(copy), [2.0, -3.0, 4.0]);
    assert_eq!(replay.snapshot_bytes().unwrap(), bytes);
}
