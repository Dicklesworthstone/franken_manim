//! Alignment-plane semantics (fm-cye slice A, §9.4): `align_family` /
//! `align_data` / `align_points` against the Reference's exact rules
//! (mobject.py:1729–1806, vectorized_mobject.py:964). The full 10³-pair
//! align corpus lands with the Transform mechanism; these are the
//! rule-for-rule unit tests.

use fmn_mobject::record::{RecordBuffer, RecordSchema};
use fmn_mobject::{Mob, Mobject, Stage, StageError};

fn base(stage: &mut Stage, points: &[[f64; 3]]) -> Mob {
    stage.add(Mobject::from_points(points))
}

/// A vmobject-schema entry with the given shared-anchor point run.
fn vmob(stage: &mut Stage, points: &[[f64; 3]]) -> Mob {
    let mob = stage.add(Mobject::new());
    let entry = stage.get_mut(mob).unwrap();
    entry.buffer = RecordBuffer::new(RecordSchema::vmobject(), points.len());
    #[allow(clippy::cast_possible_truncation)]
    let flat: Vec<f32> = points
        .iter()
        .flat_map(|p| p.iter().map(|v| *v as f32))
        .collect();
    entry.buffer.write_range("point", 0, &flat);
    mob
}

fn points_of(stage: &Stage, mob: Mob) -> Vec<f32> {
    stage.get(mob).unwrap().buffer.read_column("point").unwrap()
}

// -------------------------------------------------------------- base align

#[test]
fn base_align_points_resizes_preserving_order() {
    let mut stage = Stage::new();
    let a = base(&mut stage, &[[1.0, 0.0, 0.0], [2.0, 0.0, 0.0]]);
    let b = base(
        &mut stage,
        &[[0.0; 3], [1.0; 3], [2.0; 3], [3.0; 3], [4.0; 3]],
    );
    stage.align_points(a, b).unwrap();
    assert_eq!(stage.get(a).unwrap().buffer.len(), 5);
    assert_eq!(stage.get(b).unwrap().buffer.len(), 5);
    // indices = arange(5) * 2 // 5 = [0, 0, 0, 1, 1]
    let pts = points_of(&stage, a);
    let xs: Vec<f32> = pts.as_chunks::<3>().0.iter().map(|c| c[0]).collect();
    assert_eq!(xs, vec![1.0, 1.0, 1.0, 2.0, 2.0]);
}

// ------------------------------------------------------------ align_family

#[test]
fn childless_side_pads_with_center_point_copies() {
    let mut stage = Stage::new();
    let a = base(&mut stage, &[[0.0; 3]]);
    let a1 = base(&mut stage, &[[1.0, 0.0, 0.0]]);
    let a2 = base(&mut stage, &[[2.0, 0.0, 0.0]]);
    stage.attach(a, a1).unwrap();
    stage.attach(a, a2).unwrap();
    // b is a leaf spanning x ∈ [0, 4] → center (2, 0, 0).
    let b = base(&mut stage, &[[0.0; 3], [4.0, 0.0, 0.0]]);

    stage.align_family(a, b).unwrap();
    let b_children = stage.get(b).unwrap().submobjects().to_vec();
    assert_eq!(b_children.len(), 2, "childless side padded to match");
    for child in b_children {
        let entry = stage.get(child).unwrap();
        assert_eq!(entry.buffer.len(), 1, "single-point null mob");
        assert_eq!(
            entry.buffer.read(0, "point"),
            Some(vec![2.0, 0.0, 0.0]),
            "placed at the parent's center"
        );
    }
    // The original sides are structurally aligned now.
    assert_eq!(stage.get(a).unwrap().submobjects().len(), 2);
}

#[test]
fn ghost_distribution_follows_repeat_indices() {
    let mut stage = Stage::new();
    // a has 3 children, b has 1 → b pads by splitting its child into
    // 1 kept + 2 invisible copies (repeat_indices = arange(3)*1//3 = [0,0,0]).
    let a = base(&mut stage, &[[0.0; 3]]);
    for x in 0..3 {
        let c = base(&mut stage, &[[f64::from(x), 0.0, 0.0]]);
        stage.attach(a, c).unwrap();
    }
    let b = base(&mut stage, &[[0.0; 3]]);
    let b_child = base(&mut stage, &[[7.0, 0.0, 0.0]]);
    stage.attach(b, b_child).unwrap();

    stage.align_family(a, b).unwrap();
    let b_children = stage.get(b).unwrap().submobjects().to_vec();
    assert_eq!(b_children.len(), 3);
    assert_eq!(b_children[0], b_child, "original child kept first");
    for &ghost in &b_children[1..] {
        assert_ne!(ghost, b_child, "ghosts are fresh copies");
        let entry = stage.get(ghost).unwrap();
        assert_eq!(
            entry.buffer.read(0, "point"),
            Some(vec![7.0, 0.0, 0.0]),
            "ghost copies the child's data"
        );
        let rgba = entry.buffer.read(0, "rgba").unwrap();
        assert_eq!(rgba[3], 0.0, "ghosts are invisible (alpha 0)");
    }
}

#[test]
fn ghost_distribution_spreads_across_children() {
    let mut stage = Stage::new();
    // a: 5 children, b: 2 → target 5, repeat_indices = arange(5)*2//5 =
    // [0,0,0,1,1] → child0 + 2 ghosts, child1 + 1 ghost.
    let a = base(&mut stage, &[[0.0; 3]]);
    for _ in 0..5 {
        let c = base(&mut stage, &[[0.0; 3]]);
        stage.attach(a, c).unwrap();
    }
    let b = base(&mut stage, &[[0.0; 3]]);
    let b0 = base(&mut stage, &[[10.0, 0.0, 0.0]]);
    let b1 = base(&mut stage, &[[20.0, 0.0, 0.0]]);
    stage.attach(b, b0).unwrap();
    stage.attach(b, b1).unwrap();

    stage.align_family(a, b).unwrap();
    let children = stage.get(b).unwrap().submobjects().to_vec();
    assert_eq!(children.len(), 5);
    assert_eq!(children[0], b0);
    assert_eq!(children[3], b1, "second original after first's ghosts");
    let x_of = |m: Mob| stage.get(m).unwrap().buffer.read(0, "point").unwrap()[0];
    assert_eq!(x_of(children[1]), 10.0);
    assert_eq!(x_of(children[2]), 10.0);
    assert_eq!(x_of(children[4]), 20.0);
}

#[test]
fn is_aligned_after_align_data_and_family() {
    let mut stage = Stage::new();
    let a = base(&mut stage, &[[0.0; 3], [1.0; 3], [2.0; 3]]);
    let a1 = base(&mut stage, &[[1.0, 0.0, 0.0], [3.0, 0.0, 0.0]]);
    stage.attach(a, a1).unwrap();
    let b = base(&mut stage, &[[5.0, 0.0, 0.0]]);

    assert!(!stage.is_aligned_with(a, b));
    stage.align_data_and_family(a, b).unwrap();
    assert!(stage.is_aligned_with(a, b), "aligned after the full pass");
    // Zipped pairs share record counts.
    let fa = stage.family(a);
    let fb = stage.family(b);
    assert_eq!(fa.len(), fb.len());
    for (&ma, &mb) in fa.iter().zip(fb.iter()) {
        assert_eq!(
            stage.get(ma).unwrap().buffer.len(),
            stage.get(mb).unwrap().buffer.len()
        );
    }
}

// -------------------------------------------------------- vmobject align

#[test]
fn vmobject_align_equalizes_point_counts_preserving_endpoints() {
    let mut stage = Stage::new();
    // 1 curve (3 points) vs 3 curves (7 points).
    let a = vmob(&mut stage, &[[0.0; 3], [1.0, 1.0, 0.0], [2.0, 0.0, 0.0]]);
    let b = vmob(
        &mut stage,
        &[
            [0.0; 3],
            [1.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [3.0, 0.0, 0.0],
            [4.0, 0.0, 0.0],
            [5.0, 0.0, 0.0],
            [6.0, 0.0, 0.0],
        ],
    );
    stage.align_points(a, b).unwrap();
    let pa = points_of(&stage, a);
    let pb = points_of(&stage, b);
    assert_eq!(pa.len(), pb.len(), "point counts equalized");
    assert_eq!(pa.len() / 3, 7, "smaller side gained (7-3)/2 = 2 curves");
    // Endpoints preserved on the subdivided side.
    assert_eq!(&pa[..3], &[0.0, 0.0, 0.0]);
    assert_eq!(&pa[pa.len() - 3..], &[2.0, 0.0, 0.0]);
    // Odd shared-anchor length maintained.
    assert_eq!((pa.len() / 3) % 2, 1);
}

#[test]
fn vmobject_align_folds_missing_subpath() {
    let mut stage = Stage::new();
    // a: two subpaths (3+3 points with a break marker anchor between:
    // shared-anchor break = repeated anchor with handle-on-anchor).
    let sub1 = [[0.0; 3], [1.0, 0.0, 0.0], [2.0, 0.0, 0.0]];
    let mut path = fmn_geom::QuadPath::new();
    path.set_points_as_corners(&[[0.0; 3], [2.0, 0.0, 0.0]])
        .unwrap();
    path.start_new_path([5.0, 0.0, 0.0]);
    path.add_line_to([6.0, 0.0, 0.0], false).unwrap();
    let a = vmob(&mut stage, path.points());
    let _ = sub1;
    // b: one subpath.
    let b = vmob(
        &mut stage,
        &[[0.0, 5.0, 0.0], [1.0, 5.0, 0.0], [2.0, 5.0, 0.0]],
    );

    stage.align_points(a, b).unwrap();
    let pa = points_of(&stage, a);
    let pb = points_of(&stage, b);
    assert_eq!(pa.len(), pb.len(), "folded subpath equalizes counts");
    let n = pb.len() / 3;
    assert_eq!(n % 2, 1, "odd point run after fold + break anchor");
    // b's first anchor is unchanged: the largest subpath comes first.
    assert_eq!(&pb[..3], &[0.0, 5.0, 0.0]);
}

#[test]
fn vmobject_equal_counts_short_circuits() {
    let mut stage = Stage::new();
    let pts = [[0.0; 3], [1.0, 1.0, 0.0], [2.0, 0.0, 0.0]];
    let a = vmob(&mut stage, &pts);
    let b = vmob(&mut stage, &pts);
    let before = points_of(&stage, a);
    stage.align_points(a, b).unwrap();
    assert_eq!(points_of(&stage, a), before, "points untouched");
    // Joint angles were refreshed (column exists and is finite).
    let angles = stage
        .get(a)
        .unwrap()
        .buffer
        .read_column("joint_angle")
        .unwrap();
    assert!(angles.iter().all(|a| a.is_finite()));
}

#[test]
fn empty_vmobject_gains_center_point() {
    let mut stage = Stage::new();
    let a = vmob(&mut stage, &[]);
    let b = vmob(&mut stage, &[[0.0; 3], [1.0, 0.0, 0.0], [2.0, 0.0, 0.0]]);
    stage.align_points(a, b).unwrap();
    let pa = points_of(&stage, a);
    let pb = points_of(&stage, b);
    assert_eq!(pa.len(), pb.len());
    // The empty side grew from a single point at its (zero-bbox) center.
    assert!(pa.as_chunks::<3>().0.iter().all(|c| *c == [0.0, 0.0, 0.0]));
}

#[test]
fn mixed_schema_pair_is_refused() {
    let mut stage = Stage::new();
    let a = base(&mut stage, &[[0.0; 3]]);
    let b = vmob(&mut stage, &[[0.0; 3]]);
    assert_eq!(stage.align_points(a, b), Err(StageError::SchemaMismatch));
}

// ------------------------------------------------- record-level primitive

#[test]
fn resize_preserving_order_exact_indices() {
    let mut buffer = RecordBuffer::new(RecordSchema::mobject(), 3);
    for (i, x) in [1.0f32, 2.0, 3.0].iter().enumerate() {
        buffer.write(i, "point", &[*x, 0.0, 0.0]);
    }
    buffer.resize_preserving_order(7);
    // indices = arange(7) * 3 // 7 = [0, 0, 0, 1, 1, 2, 2]
    let xs: Vec<f32> = buffer
        .read_column("point")
        .unwrap()
        .as_chunks::<3>()
        .0
        .iter()
        .map(|c| c[0])
        .collect();
    assert_eq!(xs, vec![1.0, 1.0, 1.0, 2.0, 2.0, 3.0, 3.0]);
    // Shrinking picks the proportional prefix representatives.
    buffer.resize_preserving_order(2);
    let xs: Vec<f32> = buffer
        .read_column("point")
        .unwrap()
        .as_chunks::<3>()
        .0
        .iter()
        .map(|c| c[0])
        .collect();
    // indices = arange(2) * 7 // 2 = [0, 3] over the 7-run [1,1,1,2,2,3,3]
    assert_eq!(xs, vec![1.0, 2.0]);
}

#[test]
fn resize_preserving_order_empty_zero_fills() {
    let mut buffer = RecordBuffer::new(RecordSchema::mobject(), 0);
    buffer.resize_preserving_order(3);
    assert_eq!(buffer.len(), 3);
    assert!(
        buffer
            .read_column("point")
            .unwrap()
            .iter()
            .all(|v| *v == 0.0)
    );
}
