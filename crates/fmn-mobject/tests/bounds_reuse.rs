//! Translation reuse is checked against the original full point-scan law.

use fmn_core::constants::ORIGIN;
use fmn_core::types::Vec3;
use fmn_mobject::bbox::{BoxAccum, MAX_LINEAR_BOUND_MEMBERS};
use fmn_mobject::stage::{SURFACE_VALIDITY_FIELD, surface_sample_is_present};
use fmn_mobject::{BoundingBox, Mob, Mobject, Placement, RecordBuffer, RecordSchema, Stage};

fn records(points: &[[f32; 3]]) -> RecordBuffer {
    let mut buffer = RecordBuffer::new(RecordSchema::vmobject(), points.len()).unwrap();
    for (index, point) in points.iter().enumerate() {
        assert!(buffer.write(index, "point", point));
    }
    buffer
}

fn object(points: &[[f32; 3]]) -> Mobject {
    Mobject::from_buffer(records(points))
}

// Deliberately retain the original raw-point computation as the oracle.
// A transformed object-space AABB is not equivalent under shear.
fn raw_bounds(stage: &Stage, mob: Mob) -> BoundingBox {
    let mut acc = BoxAccum::new();
    let mut has_points = false;
    for member in stage.family(mob) {
        if let Some(entry) = stage.get(member)
            && let Some(points) = entry.buffer.read_column("point")
        {
            let validity = entry.buffer.read_column(SURFACE_VALIDITY_FIELD);
            let (points, _) = points.as_chunks::<3>();
            for (index, point) in points.iter().enumerate() {
                if !surface_sample_is_present(validity.as_deref(), index) {
                    continue;
                }
                has_points = true;
                acc.push(entry.placement().apply_point(point.map(f64::from)));
            }
        }
    }
    if !has_points && let Some(entry) = stage.get(mob) {
        acc.push(entry.placement().apply_point(ORIGIN));
    }
    acc.finish()
}

fn box_bits(value: BoundingBox) -> [[u64; 3]; 3] {
    [value.min, value.mid, value.max].map(|point| point.map(f64::to_bits))
}

fn placement_bits(stage: &Stage, mob: Mob) -> [[u64; 3]; 4] {
    let placement = stage.get(mob).unwrap().placement();
    let linear = placement.linear();
    [linear[0], linear[1], linear[2], placement.translation()]
        .map(|row| row.map(f64::to_bits))
}

fn assert_raw(stage: &Stage, mob: Mob) {
    assert_eq!(
        box_bits(stage.get_bounding_box(mob)),
        box_bits(raw_bounds(stage, mob)),
    );
}

#[test]
fn translated_families_keep_sequential_motion_bits_without_rescanning_points() {
    let mut stage = Stage::new();
    let root = stage.add(Mobject::new());
    let left = stage.add(Mobject::new());
    let right = stage.add(Mobject::new());
    let fraction = stage.add(object(&[[0.1, 0.0, 0.0], [0.3, 0.0, 0.0]]));
    let diagonal = stage.add(object(&[[0.0, 0.0, 0.0], [1.0, 1.0, 0.0]]));
    let points: Vec<Vec3> = (0..257_u16)
        .map(|index| {
            let x = f64::from(index) / 31.0;
            [x, (x - 2.0) * (x - 2.0), x / 7.0]
        })
        .collect();
    let dense = stage.add(Mobject::from_points(&points));
    stage.attach(root, left).unwrap();
    stage.attach(root, right).unwrap();
    stage.attach(left, fraction).unwrap();
    stage.attach(left, diagonal).unwrap();
    stage.attach(right, diagonal).unwrap();
    stage.attach(right, dense).unwrap();
    stage
        .set_placement(
            diagonal,
            Placement::new(
                [[1.0, -1.0, 0.0], [0.0, 2.0, 0.0], [0.0, 0.0, 1.0]],
                [0.0, 0.0, 0.0],
            ),
        )
        .unwrap();
    stage
        .set_placement(
            dense,
            Placement::new(
                [[1.25, 0.25, 0.0], [0.0, 0.5, 0.125], [0.0, 0.0, 2.0]],
                [-3.0, 1.0, 0.25],
            ),
        )
        .unwrap();

    let mut oracle = stage.snapshot().materialize();
    let family = stage.family(root);
    let mut warm_materializations = 0;
    for frame in 0..65_u16 {
        let alpha = f64::from(frame) / 31.0;
        let target = [alpha, alpha / 3.0, -alpha / 7.0];
        let center = raw_bounds(&oracle, root).mid;
        let delta = std::array::from_fn(|axis| target[axis] - center[axis]);
        oracle.shift(root, delta);
        stage.move_to(root, target, ORIGIN);
        assert_raw(&stage, root);
        assert_eq!(
            box_bits(stage.get_bounding_box(root)),
            box_bits(raw_bounds(&oracle, root)),
            "sequential frame {frame}",
        );
        for &member in &family {
            assert_eq!(
                placement_bits(&stage, member),
                placement_bits(&oracle, member),
                "member {member:?} at frame {frame}",
            );
        }
        let stats = stage.linear_bounds_stats();
        assert_eq!(stats.fallback_queries, 0);
        if frame == 1 {
            warm_materializations = stats.geometry_materializations;
        } else if frame > 1 {
            assert_eq!(stats.geometry_materializations, warm_materializations);
            assert!(stats.cache_hits > 0);
        }
    }
}

#[test]
fn transformed_extrema_do_not_use_object_box_corners() {
    let mut stage = Stage::new();
    let mob = stage.add(object(&[[0.0, 0.0, 0.0], [1.0, 1.0, 0.0]]));
    stage
        .set_placement(
            mob,
            Placement::new(
                [[1.0, -1.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
                [2.0, 3.0, 0.0],
            ),
        )
        .unwrap();
    assert_raw(&stage, mob);
    assert_eq!(stage.get_bounding_box(mob).width(), 0.0);
    stage.shift(mob, [0.1, 0.2, 0.3]);
    assert_raw(&stage, mob);
    assert_eq!(stage.get_bounding_box(mob).width(), 0.0);
}

fn masked_object() -> Mobject {
    let schema = RecordSchema::new(
        &[("point", 3), (SURFACE_VALIDITY_FIELD, 1)],
        &["point"],
        &["point"],
    )
    .unwrap();
    let mut buffer = RecordBuffer::new(schema, 3).unwrap();
    for (index, point) in [
        [1.0, 2.0, 3.0],
        [4.0, 5.0, 6.0],
        [f32::INFINITY, f32::NAN, f32::NEG_INFINITY],
    ]
    .iter()
    .enumerate()
    {
        assert!(buffer.write(index, "point", point));
        let present = if index < 2 { 1.0 } else { 0.0 };
        assert!(buffer.write(index, SURFACE_VALIDITY_FIELD, &[present]));
    }
    Mobject::from_buffer(buffer)
}

#[test]
fn point_mask_and_linear_edits_live_views_and_restore_invalidate_observations() {
    let mut stage = Stage::new();
    let mob = stage.add(masked_object());
    assert_raw(&stage, mob);
    let saved = stage.snapshot();

    assert!(
        stage
            .get_mut(mob)
            .unwrap()
            .buffer
            .write(1, "point", &[8.0, 9.0, 10.0])
    );
    assert_raw(&stage, mob);
    assert_eq!(stage.get_bounding_box(mob).max, [8.0, 9.0, 10.0]);
    assert!(
        stage
            .get_mut(mob)
            .unwrap()
            .buffer
            .write(1, SURFACE_VALIDITY_FIELD, &[0.0])
    );
    assert_raw(&stage, mob);
    assert_eq!(stage.get_bounding_box(mob).max, [1.0, 2.0, 3.0]);

    let point_view = stage
        .get_mut(mob)
        .unwrap()
        .buffer
        .export_field_view("point", true)
        .unwrap();
    let revision = stage.get(mob).unwrap().buffer.field_revision("point");
    assert!(point_view.write_foreign(0, "point", &[-2.0, -3.0, -4.0]));
    assert_eq!(
        stage.get(mob).unwrap().buffer.field_revision("point"),
        revision,
    );
    assert_raw(&stage, mob);
    assert_eq!(stage.get_bounding_box(mob).min, [-2.0, -3.0, -4.0]);
    drop(point_view);

    let mask_view = stage
        .get_mut(mob)
        .unwrap()
        .buffer
        .export_field_view(SURFACE_VALIDITY_FIELD, true)
        .unwrap();
    let revision = stage
        .get(mob)
        .unwrap()
        .buffer
        .field_revision(SURFACE_VALIDITY_FIELD);
    assert!(mask_view.write_foreign(1, SURFACE_VALIDITY_FIELD, &[1.0]));
    assert_eq!(
        stage
            .get(mob)
            .unwrap()
            .buffer
            .field_revision(SURFACE_VALIDITY_FIELD),
        revision,
    );
    assert_raw(&stage, mob);
    assert_eq!(stage.get_bounding_box(mob).max, [8.0, 9.0, 10.0]);
    drop(mask_view);

    stage
        .set_placement(
            mob,
            Placement::new(
                [[1.0, 0.5, 0.0], [0.0, 2.0, 0.0], [0.0, 0.0, 3.0]],
                [0.25, -0.5, 1.0],
            ),
        )
        .unwrap();
    assert_raw(&stage, mob);
    stage.restore(&saved);
    assert_eq!(stage.linear_bounds_stats().cached_members, 0);
    assert_eq!(stage.linear_bounds_stats().geometry_materializations, 0);
    assert_raw(&stage, mob);
    assert_eq!(stage.get_bounding_box(mob).min, [1.0, 2.0, 3.0]);
    assert_eq!(stage.get_bounding_box(mob).max, [4.0, 5.0, 6.0]);
}

#[test]
fn replacement_storage_with_equal_revisions_cannot_reuse_an_old_box() {
    let mut stage = Stage::new();
    let mob = stage.add(object(&[[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]]));
    assert_raw(&stage, mob);
    let replacement = records(&[[-9.0, -8.0, -7.0], [-6.0, -5.0, -4.0]]);
    assert_eq!(
        stage.get(mob).unwrap().buffer.field_revision("point"),
        replacement.field_revision("point"),
    );
    stage.get_mut(mob).unwrap().buffer = replacement;
    assert_raw(&stage, mob);
    assert_eq!(stage.get_bounding_box(mob).min, [-9.0, -8.0, -7.0]);
}

#[test]
fn unusual_floating_point_inputs_keep_the_original_whole_family_fold() {
    for (points, placement) in [
        (
            vec![[-0.0; 3], [0.0; 3]],
            Placement::from_translation([-0.0; 3]),
        ),
        (
            vec![[f32::INFINITY, 1.0, 2.0], [3.0, 4.0, 5.0]],
            Placement::IDENTITY,
        ),
        (
            vec![[1.0, 2.0, 3.0]],
            Placement::from_translation([f64::NAN, f64::INFINITY, 0.0]),
        ),
        (
            vec![[2.0, 0.0, 0.0]],
            Placement::new(
                [[f64::MAX / 2.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
                [f64::MAX, 0.0, 0.0],
            ),
        ),
    ] {
        let mut stage = Stage::new();
        let root = stage.add(object(&[[0.0; 3]]));
        let child = stage.add(object(&points));
        stage.attach(root, child).unwrap();
        stage.set_placement(child, placement).unwrap();
        assert_raw(&stage, root);
        assert!(stage.linear_bounds_stats().fallback_queries > 0);
    }

    // Midpoint overflow remains part of the existing arithmetic even when
    // all world-space extrema qualify for reuse.
    let mut stage = Stage::new();
    let mob = stage.add(object(&[[2.0, 0.0, 0.0], [3.0, 0.0, 0.0]]));
    stage
        .set_placement(
            mob,
            Placement::new(
                [[f64::MAX / 4.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
                [0.0; 3],
            ),
        )
        .unwrap();
    assert_raw(&stage, mob);
    assert!(stage.get_bounding_box(mob).mid[0].is_infinite());
}

#[test]
fn empty_anchors_and_caller_installed_boxes_keep_their_existing_lifetimes() {
    let mut stage = Stage::new();
    let parent = stage.add(Mobject::new());
    let child = stage.add(Mobject::new());
    stage.attach(parent, child).unwrap();
    assert!(stage.set_point_free_anchor(parent, [4.0, 5.0, 6.0]).unwrap());
    assert!(stage.set_point_free_anchor(child, [7.0, 8.0, 9.0]).unwrap());
    assert_raw(&stage, parent);
    assert_eq!(stage.get_bounding_box(parent).mid, [4.0, 5.0, 6.0]);
    stage.shift(child, [1.0, 2.0, 3.0]);
    assert_raw(&stage, parent);
    assert_raw(&stage, child);
    assert_eq!(stage.get_bounding_box(parent).mid, ORIGIN);
    assert_eq!(stage.get_bounding_box(child).mid, [8.0, 10.0, 12.0]);

    let geometry = stage.add(object(&[[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]]));
    stage.attach(parent, geometry).unwrap();
    let custom = BoundingBox {
        min: [-100.0; 3],
        mid: [15.0; 3],
        max: [200.0; 3],
    };
    stage.install_bounding_box_cache(parent, custom);
    assert_eq!(box_bits(stage.get_bounding_box(parent)), box_bits(custom));
    assert_eq!(stage.installed_bounding_box_cache(parent), Some(custom));
    stage.shift(geometry, [0.125, 0.25, 0.5]);
    assert_eq!(stage.installed_bounding_box_cache(parent), None);
    assert_raw(&stage, parent);
}

#[test]
fn retained_bounds_are_bounded_and_do_not_pin_record_storage() {
    let mut stage = Stage::new();
    let mut mobs = Vec::new();
    for index in 0..160_u16 {
        let x = f32::from(index);
        let mob = stage.add(object(&[[x, 0.0, 0.0], [x + 1.0, 1.0, 0.0]]));
        assert_raw(&stage, mob);
        mobs.push(mob);
    }
    assert_eq!(
        stage.linear_bounds_stats().cached_members,
        MAX_LINEAR_BOUND_MEMBERS,
    );
    let mob = *mobs.last().unwrap();
    let storage = stage.get(mob).unwrap().buffer.storage_id();
    assert!(
        stage
            .get_mut(mob)
            .unwrap()
            .buffer
            .write(0, "point", &[99.0, 98.0, 97.0])
    );
    assert_eq!(stage.get(mob).unwrap().buffer.storage_id(), storage);
    assert_raw(&stage, mob);
    stage.shift(mobs[0], [0.25, 0.5, 0.0]);
    assert_raw(&stage, mobs[0]);
}

#[test]
fn storage_generation_tracks_allocation_not_record_revision() {
    let mut buffer = records(&[[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]]);
    let generation = buffer.storage_generation();
    assert!(buffer.write(0, "point", &[0.5, 1.5, 2.5]));
    assert_eq!(buffer.storage_generation(), generation);
    let shared = buffer.snapshot_clone();
    assert_eq!(shared.storage_generation(), generation);
    let deep = buffer.deep_clone();
    assert_ne!(deep.storage_generation(), generation);
    assert_eq!(deep.field_revision("point"), buffer.field_revision("point"));
    assert_eq!(deep.read_column("point"), buffer.read_column("point"));

    assert!(buffer.write(1, "point", &[7.0, 8.0, 9.0]));
    assert_ne!(buffer.storage_generation(), generation);
    assert_eq!(shared.storage_generation(), generation);
    let unshared = buffer.storage_generation();
    buffer.resize_preserving_order(3).unwrap();
    assert_ne!(buffer.storage_generation(), unshared);
}
