//! Whole-record geometry must survive retained placement, live views and maps.
use fmn_mobject::{Mob, Mobject, Placement, RecordBuffer, RecordSchema, Stage};

fn surface(stage: &mut Stage) -> Mob {
    let schema = RecordSchema::new(
        &[
            ("point", 3),
            ("d_normal_point", 3),
            ("anchor", 3),
            ("rgba", 4),
            ("uv", 2),
        ],
        &["point"],
        &["point", "d_normal_point", "anchor", "d_normal_point"],
    )
    .unwrap();
    let mut buffer = RecordBuffer::new(schema, 2).unwrap();
    for (row, x) in [0.0_f32, 2.0].into_iter().enumerate() {
        assert!(buffer.write(row, "point", &[x, 1.0, 0.0]));
        assert!(buffer.write(row, "d_normal_point", &[x, 1.0, 0.25]));
        assert!(buffer.write(row, "anchor", &[x, 2.0, 0.0]));
        assert!(buffer.write(row, "rgba", &[0.2, 0.4, 0.6, 0.8]));
        assert!(buffer.write(row, "uv", &[0.1, 0.9]));
    }
    stage.add(Mobject::from_buffer(buffer))
}

fn column(stage: &Stage, mob: Mob, key: &str) -> Vec<f32> {
    stage.get(mob).unwrap().buffer.read_column(key).unwrap()
}

#[test]
fn retained_placement_bakes_all_pointlikes_once_and_no_style_fields() {
    let mut stage = Stage::new();
    let mob = surface(&mut stage);
    let rgba = column(&stage, mob, "rgba");
    let uv = column(&stage, mob, "uv");
    let revision = stage.get(mob).unwrap().buffer.revision();
    stage.shift(mob, [3.0, -2.0, 5.0]);
    // The no-view fast path is still retained, not eagerly rewritten.
    assert_eq!(stage.get(mob).unwrap().buffer.revision(), revision);
    assert!(stage.bake_placement(mob).unwrap());
    assert_eq!(
        column(&stage, mob, "point"),
        [3.0, -1.0, 5.0, 5.0, -1.0, 5.0]
    );
    assert_eq!(
        column(&stage, mob, "d_normal_point"),
        [3.0, -1.0, 5.25, 5.0, -1.0, 5.25]
    );
    assert_eq!(
        column(&stage, mob, "anchor"),
        [3.0, 0.0, 5.0, 5.0, 0.0, 5.0]
    );
    assert_eq!(column(&stage, mob, "rgba"), rgba);
    assert_eq!(column(&stage, mob, "uv"), uv);
    let revision = stage.get(mob).unwrap().buffer.revision();
    assert!(!stage.bake_placement(mob).unwrap());
    assert_eq!(stage.get(mob).unwrap().buffer.revision(), revision);
}

#[test]
fn pinned_field_views_observe_every_affine_geometry_channel() {
    let mut stage = Stage::new();
    let mob = surface(&mut stage);
    stage.shift(mob, [1.0, 0.0, 0.0]);
    let normal_view = stage
        .get_mut(mob)
        .unwrap()
        .buffer
        .export_field_view("d_normal_point", true)
        .unwrap();
    let affine = Placement::about(
        [[2.0, 0.0, 0.0], [0.0, 3.0, 0.0], [0.0, 0.0, 4.0]],
        [1.0, 1.0, 1.0],
    );
    stage.apply_affine(mob, affine);
    assert_eq!(stage.placement(mob), Some(Placement::IDENTITY));
    assert!(normal_view.is_attached_to(&stage.get(mob).unwrap().buffer));
    assert_eq!(
        normal_view.read(0, "d_normal_point").unwrap(),
        [1.0, 1.0, -2.0]
    );
    assert_eq!(
        column(&stage, mob, "point"),
        [1.0, 1.0, -3.0, 5.0, 1.0, -3.0]
    );
    assert_eq!(
        column(&stage, mob, "anchor"),
        [1.0, 4.0, -3.0, 5.0, 4.0, -3.0]
    );
}

#[test]
fn pointwise_maps_transform_control_points_in_world_space_about_one_pivot() {
    let mut stage = Stage::new();
    let mob = surface(&mut stage);
    stage.shift(mob, [1.0, 2.0, 0.0]);
    stage.apply_points_function(
        mob,
        |p| [p[0], p[1], p[2] + p[0] * p[1]],
        Some([1.0, 1.0, 0.0]),
        None,
    );
    assert_eq!(column(&stage, mob, "point"), [1.0, 3.0, 0.0, 3.0, 3.0, 4.0]);
    assert_eq!(
        column(&stage, mob, "d_normal_point"),
        [1.0, 3.0, 0.25, 3.0, 3.0, 4.25]
    );
    assert_eq!(
        column(&stage, mob, "anchor"),
        [1.0, 4.0, 0.0, 3.0, 4.0, 6.0]
    );
}

#[test]
fn point_replacement_preserves_world_auxiliary_fields_and_resize_order() {
    let mut stage = Stage::new();
    let mob = surface(&mut stage);
    stage.shift(mob, [3.0, -2.0, 5.0]);
    stage.set_points(mob, &[[10.0, 20.0, 30.0]]).unwrap();
    assert_eq!(column(&stage, mob, "point"), [10.0, 20.0, 30.0]);
    assert_eq!(column(&stage, mob, "d_normal_point"), [3.0, -1.0, 5.25]);
    assert_eq!(column(&stage, mob, "anchor"), [3.0, 0.0, 5.0]);
    assert_eq!(column(&stage, mob, "uv"), [0.1, 0.9]);
}

#[test]
fn multiple_roots_and_diamond_families_do_not_double_transform_normals() {
    let mut stage = Stage::new();
    let a = stage.add(Mobject::new());
    let b = stage.add(Mobject::new());
    let child = surface(&mut stage);
    stage.attach(a, child).unwrap();
    stage.attach(b, child).unwrap();
    let view = stage.get_mut(child).unwrap().buffer.export_view(false);
    stage.shift_many(&[a, b, child], [2.0, 3.0, 4.0]);
    assert_eq!(view.read(0, "point").unwrap(), [2.0, 4.0, 4.0]);
    assert_eq!(view.read(0, "d_normal_point").unwrap(), [2.0, 4.0, 4.25]);
    assert_eq!(view.read(0, "anchor").unwrap(), [2.0, 5.0, 4.0]);
}

#[test]
fn snapshot_copy_on_write_preserves_original_geometry_and_placement() {
    let mut stage = Stage::new();
    let mob = surface(&mut stage);
    stage.shift(mob, [3.0, 0.0, 0.0]);
    let snapshot = stage.snapshot();
    stage.bake_placement(mob).unwrap();
    stage.shift(mob, [4.0, 0.0, 0.0]);
    stage.bake_placement(mob).unwrap();
    assert_eq!(column(&stage, mob, "d_normal_point")[0], 7.0);
    stage.restore(&snapshot);
    assert_eq!(column(&stage, mob, "d_normal_point")[0], 0.0);
    assert_eq!(stage.get_points(mob).unwrap()[0], [3.0, 1.0, 0.0]);
    stage.bake_placement(mob).unwrap();
    assert_eq!(column(&stage, mob, "d_normal_point")[0], 3.0);
}

#[test]
fn auxiliary_only_custom_records_are_not_stranded_by_readback_or_live_views() {
    let mut stage = Stage::new();
    let schema = RecordSchema::new(&[("anchor", 3), ("rgba", 4)], &[], &["anchor"]).unwrap();
    let mut buffer = RecordBuffer::new(schema, 1).unwrap();
    buffer.write(0, "anchor", &[1.0, 2.0, 3.0]);
    let mob = stage.add(Mobject::from_buffer(buffer));
    stage.shift(mob, [1.0, 2.0, 3.0]);
    stage.bake_placement(mob).unwrap();
    assert_eq!(column(&stage, mob, "anchor"), [2.0, 4.0, 6.0]);
    let view = stage.get_mut(mob).unwrap().buffer.export_view(true);
    stage.shift(mob, [2.0, 0.0, 0.0]);
    assert_eq!(view.read(0, "anchor").unwrap(), [4.0, 4.0, 6.0]);
    stage.apply_points_function(mob, |p| [p[0], p[1], p[2] * 2.0], None, None);
    assert_eq!(view.read(0, "anchor").unwrap(), [4.0, 4.0, 12.0]);
}

#[test]
fn checked_family_map_keeps_world_normals_views_and_unique_descendants() {
    let mut stage = Stage::new();
    let root = stage.add(Mobject::new());
    let left = stage.add(Mobject::new());
    let right = stage.add(Mobject::new());
    let shared = surface(&mut stage);
    stage.attach(root, left).unwrap();
    stage.attach(root, right).unwrap();
    stage.attach(left, shared).unwrap();
    stage.attach(right, shared).unwrap();
    stage.shift(root, [1.0, 2.0, 0.0]);
    let rgba = column(&stage, shared, "rgba");
    let view = stage.get_mut(shared).unwrap().buffer.export_view(false);
    let calls = std::cell::Cell::new(0);
    stage
        .try_apply_points_function(
            root,
            |p| {
                calls.set(calls.get() + 1);
                [p[0], p[1], p[2] + p[0] * p[1]]
            },
            Some([1.0, 1.0, 0.0]),
            None,
        )
        .unwrap();
    assert_eq!(
        calls.get(),
        6,
        "two rows times three unique pointlike fields"
    );
    assert_eq!(
        column(&stage, shared, "point"),
        [1.0, 3.0, 0.0, 3.0, 3.0, 4.0]
    );
    assert_eq!(
        column(&stage, shared, "d_normal_point"),
        [1.0, 3.0, 0.25, 3.0, 3.0, 4.25]
    );
    assert_eq!(
        column(&stage, shared, "anchor"),
        [1.0, 4.0, 0.0, 3.0, 4.0, 6.0]
    );
    assert_eq!(column(&stage, shared, "rgba"), rgba);
    assert_eq!(stage.placement(shared), Some(Placement::IDENTITY));
    assert!(view.is_attached_to(&stage.get(shared).unwrap().buffer));
    assert_eq!(view.read(1, "d_normal_point").unwrap(), [3.0, 3.0, 4.25]);
}

#[test]
fn checked_family_map_refuses_late_points_and_auxiliary_overflow_atomically() {
    for (bad, only_normals) in [(f64::NAN, false), (f64::MAX, false), (f64::MAX, true)] {
        let mut stage = Stage::new();
        let root = stage.add(Mobject::new());
        let first = surface(&mut stage);
        let second = surface(&mut stage);
        stage.attach(root, first).unwrap();
        stage.attach(root, second).unwrap();
        stage.shift(second, [8.0, 0.0, 0.0]);
        let view = stage.get_mut(first).unwrap().buffer.export_view(false);
        let before: Vec<_> = [first, second]
            .into_iter()
            .map(|mob| {
                (
                    stage.get_points(mob).unwrap(),
                    column(&stage, mob, "d_normal_point"),
                    stage.placement(mob),
                    stage.get(mob).unwrap().buffer.revision(),
                )
            })
            .collect();
        let error = stage
            .try_apply_points_function(
                root,
                |p| {
                    if p[0] > 7.0 && (!only_normals || p[2] > 0.0) {
                        [bad, p[1], p[2]]
                    } else {
                        [p[0] + 1.0, p[1], p[2]]
                    }
                },
                None,
                None,
            )
            .err()
            .expect("non-finite mapped record");
        assert!(matches!(error, fmn_mobject::StageError::InvalidPointMap(_)));
        for (mob, before) in [first, second].into_iter().zip(before) {
            assert_eq!(stage.get_points(mob).unwrap(), before.0);
            assert_eq!(column(&stage, mob, "d_normal_point"), before.1);
            assert_eq!(stage.placement(mob), before.2);
            assert_eq!(stage.get(mob).unwrap().buffer.revision(), before.3);
        }
        assert!(view.is_attached_to(&stage.get(first).unwrap().buffer));
        assert_eq!(view.read(0, "point").unwrap(), [0.0, 1.0, 0.0]);
        assert_eq!(view.read(0, "d_normal_point").unwrap(), [0.0, 1.0, 0.25]);
    }
}

#[test]
fn checked_maps_preserve_surface_holes_without_sampling_their_inert_rows() {
    let mut stage = Stage::new();
    let schema = RecordSchema::new(
        &[("point", 3), ("d_normal_point", 3), ("surface_validity", 1)],
        &["point"],
        &["point", "d_normal_point"],
    )
    .unwrap();
    let mut buffer = RecordBuffer::new(schema, 2).unwrap();
    assert!(buffer.write(0, "point", &[2.0, 0.0, 0.0]));
    assert!(buffer.write(0, "d_normal_point", &[2.0, 0.0, 0.25]));
    assert!(buffer.write(0, "surface_validity", &[1.0]));
    let mob = stage.add(Mobject::from_buffer(buffer));
    stage.shift(mob, [1.0, 2.0, 3.0]);
    let calls = std::cell::Cell::new(0);
    stage
        .try_apply_points_function(
            mob,
            |p| {
                calls.set(calls.get() + 1);
                [p[0] + 4.0, p[1], p[2]]
            },
            None,
            None,
        )
        .unwrap();
    assert_eq!(calls.get(), 2, "only the present point and its normal");
    assert_eq!(column(&stage, mob, "point"), [7.0, 2.0, 3.0, 0.0, 0.0, 0.0]);
    assert_eq!(
        column(&stage, mob, "d_normal_point"),
        [7.0, 2.0, 3.25, 0.0, 0.0, 0.0]
    );
    assert_eq!(column(&stage, mob, "surface_validity"), [1.0, 0.0]);
}

#[test]
fn checked_shift_retains_placement_and_refuses_overflow_without_moving_views() {
    let mut stage = Stage::new();
    let mob = surface(&mut stage);
    let revision = stage.get(mob).unwrap().buffer.revision();
    stage.try_shift(mob, [2.0, 3.0, 4.0]).unwrap();
    assert_eq!(stage.get(mob).unwrap().buffer.revision(), revision);
    assert_eq!(stage.get_points(mob).unwrap()[0], [2.0, 4.0, 4.0]);
    let view = stage.get_mut(mob).unwrap().buffer.export_view(false);
    stage.try_shift(mob, [1.0, -2.0, 0.5]).unwrap();
    assert_eq!(view.read(0, "point").unwrap(), [3.0, 2.0, 4.5]);
    assert_eq!(view.read(0, "d_normal_point").unwrap(), [3.0, 2.0, 4.75]);
    let revision = stage.get(mob).unwrap().buffer.revision();
    for bad in [f64::NAN, 2.0 * f64::from(f32::MAX)] {
        let error = stage
            .try_shift(mob, [bad, 0.0, 0.0])
            .err()
            .expect("invalid translation");
        assert!(matches!(error, fmn_mobject::StageError::InvalidPointMap(_)));
        assert_eq!(stage.get(mob).unwrap().buffer.revision(), revision);
        assert_eq!(view.read(0, "point").unwrap(), [3.0, 2.0, 4.5]);
        assert_eq!(view.read(0, "d_normal_point").unwrap(), [3.0, 2.0, 4.75]);
    }
    let foreign = surface(&mut Stage::new());
    assert!(matches!(
        stage.try_shift(foreign, [0.0; 3]),
        Err(fmn_mobject::StageError::StaleHandle)
    ));
}

#[test]
fn checked_map_preserves_earlier_members_on_a_later_native_geometry_error() {
    let mut stage = Stage::new();
    let root = stage.add(Mobject::new());
    let good = surface(&mut stage);
    let malformed = RecordBuffer::new(RecordSchema::vmobject(), 2).unwrap();
    let bad = stage.add(Mobject::from_buffer(malformed));
    stage.attach(root, good).unwrap();
    stage.attach(root, bad).unwrap();
    let points = stage.get_points(good).unwrap();
    let revision = stage.get(good).unwrap().buffer.revision();
    let error = stage
        .try_apply_points_function(root, |p| [p[0] + 1.0, p[1], p[2]], None, None)
        .err()
        .expect("malformed vector geometry");
    assert!(matches!(
        error,
        fmn_mobject::StageError::Geometry(fmn_geom::GeomError::EvenPointCount { len: 2 })
    ));
    assert_eq!(stage.get_points(good).unwrap(), points);
    assert_eq!(stage.get(good).unwrap().buffer.revision(), revision);
}
