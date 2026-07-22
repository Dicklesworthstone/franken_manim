//! fm-cus acceptance: the data plane's three layers.
//!
//! - Layout locks: schema byte offsets equal NumPy's structured-dtype
//!   packing for the Reference dtypes (the zero-copy export contract).
//!   The live fnp-backed round-trip lands when SUITE.lock makes the suite
//!   consumable (follow-up bead, blocked on fm-g2c).
//! - View protocol: resize under a live view, field-scoped views and
//!   dirty, ranged edits (precise-dirty opt-in).
//! - Mirrors: coherence (mirror ≡ buffer at every observation point),
//!   laziness (untouched fields never rematerialize), conservative refresh
//!   under writable views.
//! - Custom dtypes end-to-end; locking as copy-elision state;
//!   resize-with-interpolation semantics ported from the Reference.

use fmn_mobject::{MirrorSet, RecordBuffer, RecordSchema};

// ------------------------------------------------------------------ layout

#[test]
fn schema_offsets_match_numpy_structured_packing() {
    // numpy: itemsize 28, point@0, rgba@12 (byte offsets).
    let mobject = RecordSchema::mobject();
    assert_eq!(mobject.stride() * 4, 28);
    assert_eq!(mobject.offset("point").unwrap() * 4, 0);
    assert_eq!(mobject.offset("rgba").unwrap() * 4, 12);
    assert_eq!(mobject.aligned_keys(), ["point"]);
    assert_eq!(mobject.pointlike_keys(), ["point"]);

    // numpy: itemsize 68; offsets point@0, stroke_rgba@12, stroke_width@28,
    // joint_angle@32, fill_rgba@36, base_normal@52, fill_border_width@64.
    let vmobject = RecordSchema::vmobject();
    assert_eq!(vmobject.stride() * 4, 68);
    for (field, byte_offset) in [
        ("point", 0),
        ("stroke_rgba", 12),
        ("stroke_width", 28),
        ("joint_angle", 32),
        ("fill_rgba", 36),
        ("base_normal", 52),
        ("fill_border_width", 64),
    ] {
        assert_eq!(
            vmobject.offset(field).unwrap() * 4,
            byte_offset,
            "byte offset of {field}"
        );
    }
}

// ------------------------------------------------------------ view protocol

#[test]
fn field_scoped_views_and_dirty() {
    let mut buffer = RecordBuffer::new(RecordSchema::vmobject(), 4);
    let point_view = buffer.export_field_view("point", true).unwrap();

    // Scoped views only touch their field.
    assert!(point_view.write(0, "point", &[1.0, 2.0, 3.0]));
    assert!(!point_view.write(0, "fill_rgba", &[1.0; 4]));
    assert!(point_view.read(0, "fill_rgba").is_none());
    assert_eq!(buffer.read(0, "point").unwrap(), vec![1.0, 2.0, 3.0]);

    // Writable-view bookkeeping is per-field.
    assert!(buffer.field_has_writable_view("point"));
    assert!(!buffer.field_has_writable_view("fill_rgba"));
    assert!(!buffer.has_writable_whole_view());
    drop(point_view);
    assert!(!buffer.field_has_writable_view("point"));

    // Field revisions move independently.
    let point_rev = buffer.field_revision("point").unwrap();
    let fill_rev = buffer.field_revision("fill_rgba").unwrap();
    buffer.write(1, "fill_rgba", &[0.5; 4]);
    assert_eq!(buffer.field_revision("point").unwrap(), point_rev);
    assert!(buffer.field_revision("fill_rgba").unwrap() > fill_rev);
}

#[test]
fn ranged_edits_accumulate_precise_dirty_spans() {
    let mut buffer = RecordBuffer::new(RecordSchema::mobject(), 10);
    assert_eq!(buffer.take_dirty_span("point"), None);

    // A bulk write covering records 2..=4.
    assert!(buffer.write_range("point", 2, &[1.0; 9]));
    // A single write at record 7 widens the span.
    assert!(buffer.write(7, "point", &[2.0, 2.0, 2.0]));
    assert_eq!(buffer.take_dirty_span("point"), Some((2, 7)));
    // Taking clears.
    assert_eq!(buffer.take_dirty_span("point"), None);
    // Other fields untouched.
    assert_eq!(buffer.take_dirty_span("rgba"), None);

    // Bounds and width are checked.
    assert!(!buffer.write_range("point", 9, &[0.0; 6]));
    assert!(!buffer.write_range("point", 0, &[0.0; 4]));
    // The rejected writes left no dirty span behind.
    assert_eq!(buffer.take_dirty_span("point"), None);
}

#[test]
fn resize_under_live_view_detaches_naturally() {
    let mut buffer = RecordBuffer::new(RecordSchema::mobject(), 2);
    buffer.write(0, "point", &[9.0, 9.0, 9.0]);
    let view = buffer.export_view(false);

    buffer.resize(5);
    assert!(!view.is_attached_to(&buffer));
    // The view still reads the old generation; growth is null-padded.
    assert_eq!(view.read(0, "point").unwrap(), vec![9.0, 9.0, 9.0]);
    assert_eq!(buffer.read(4, "point").unwrap(), vec![0.0, 0.0, 0.0]);
    assert_eq!(buffer.read(0, "point").unwrap(), vec![9.0, 9.0, 9.0]);
}

// ---------------------------------------------------------------- mirrors

#[test]
fn mirror_coherence_at_every_observation_point() {
    let mut buffer = RecordBuffer::new(RecordSchema::mobject(), 3);
    let mut mirrors = MirrorSet::new();

    let observe = |mirrors: &mut MirrorSet, buffer: &RecordBuffer| {
        for field in ["point", "rgba"] {
            let width = buffer.schema().field_width(field).unwrap();
            let len = buffer.len();
            let lanes = mirrors.sync(buffer, field).unwrap().to_vec();
            let column = buffer.read_column(field).unwrap();
            for record in 0..len {
                for lane in 0..width {
                    assert_eq!(
                        lanes[lane * len + record],
                        column[record * width + lane],
                        "{field} record {record} lane {lane}"
                    );
                }
            }
        }
    };

    observe(&mut mirrors, &buffer);
    buffer.write(1, "point", &[1.0, 2.0, 3.0]);
    observe(&mut mirrors, &buffer);
    buffer.write_range("rgba", 0, &[0.25; 12]);
    observe(&mut mirrors, &buffer);
    buffer.resize_with_interpolation(7);
    observe(&mut mirrors, &buffer);
    // Writes through a view are observed too (conservative refresh).
    let view = buffer.export_view(true);
    view.write(2, "point", &[5.0, 5.0, 5.0]);
    observe(&mut mirrors, &buffer);
}

#[test]
fn mirror_laziness_untouched_fields_never_rematerialize() {
    let mut buffer = RecordBuffer::new(RecordSchema::vmobject(), 8);
    let mut mirrors = MirrorSet::new();

    mirrors.sync(&buffer, "point").unwrap();
    mirrors.sync(&buffer, "fill_rgba").unwrap();
    assert_eq!(mirrors.materializations(), 2);

    // Repeated observation with no writes: nothing rematerializes.
    for _ in 0..5 {
        mirrors.sync(&buffer, "point").unwrap();
        mirrors.sync(&buffer, "fill_rgba").unwrap();
    }
    assert_eq!(mirrors.materializations(), 2);

    // Touch one field: only it rematerializes.
    buffer.write(3, "point", &[1.0, 1.0, 1.0]);
    mirrors.sync(&buffer, "point").unwrap();
    mirrors.sync(&buffer, "fill_rgba").unwrap();
    assert_eq!(mirrors.materializations(), 3);

    // A writable whole-buffer view forces conservative refresh every
    // observation — a live view never gets weaker semantics.
    let view = buffer.export_view(true);
    mirrors.sync(&buffer, "fill_rgba").unwrap();
    mirrors.sync(&buffer, "fill_rgba").unwrap();
    assert_eq!(mirrors.materializations(), 5);
    drop(view);
    mirrors.sync(&buffer, "fill_rgba").unwrap();
    let settled = mirrors.materializations();
    mirrors.sync(&buffer, "fill_rgba").unwrap();
    assert_eq!(mirrors.materializations(), settled);
}

// ------------------------------------------------------------ custom dtype

#[test]
fn custom_dtype_end_to_end() {
    // A user-declared record type through the same schema machinery.
    let schema = RecordSchema::new(
        &[("position", 3), ("velocity", 3), ("charge", 1)],
        &["position"],
        &["position", "velocity"],
    );
    assert_eq!(schema.stride(), 7);
    let mut buffer = RecordBuffer::new(schema, 2);
    assert!(buffer.write(0, "charge", &[-1.0]));
    assert!(buffer.write(1, "velocity", &[0.0, 9.8, 0.0]));

    let view = buffer.export_field_view("velocity", true).unwrap();
    assert!(view.write(0, "velocity", &[1.0, 0.0, 0.0]));
    assert_eq!(buffer.read(0, "velocity").unwrap(), vec![1.0, 0.0, 0.0]);

    let mut mirrors = MirrorSet::new();
    let len = buffer.len();
    let lanes = mirrors.sync(&buffer, "charge").unwrap();
    assert_eq!(lanes.len(), len);
    assert_eq!(lanes[0], -1.0);

    // Unknown fields are precise failures, never silence.
    assert!(!buffer.write(0, "spin", &[1.0]));
    assert!(buffer.export_field_view("spin", true).is_none());
    assert_eq!(buffer.field_revision("spin"), None);
}

// -------------------------------------------------------------- lock state

#[test]
fn data_locking_is_copy_elision_state() {
    let mut buffer = RecordBuffer::new(RecordSchema::vmobject(), 4);
    buffer.lock_data(["point", "base_normal", "nonexistent"]);
    assert!(buffer.is_locked("point"));
    assert!(buffer.is_locked("base_normal"));
    assert!(!buffer.is_locked("fill_rgba"));
    assert_eq!(buffer.locked_keys(), vec!["point", "base_normal"]);

    // Locking never gates access (it is an animation-engine contract).
    assert!(buffer.write(0, "point", &[1.0, 1.0, 1.0]));

    // Lock state survives snapshot/deep clones (it is animation state).
    assert!(buffer.snapshot_clone().is_locked("point"));
    assert!(buffer.deep_clone().is_locked("base_normal"));

    buffer.unlock_data();
    assert!(buffer.locked_keys().is_empty());
}

// -------------------------------------------- resize-with-interpolation

#[test]
fn resize_with_interpolation_matches_reference_semantics() {
    // Linear ramp over 3 records → 5 records keeps the ramp.
    let mut buffer = RecordBuffer::new(RecordSchema::mobject(), 3);
    for (i, x) in [0.0f32, 1.0, 2.0].iter().enumerate() {
        buffer.write(i, "point", &[*x, 0.0, 0.0]);
    }
    buffer.resize_with_interpolation(5);
    assert_eq!(buffer.len(), 5);
    let xs: Vec<f32> = (0..5)
        .map(|i| buffer.read(i, "point").unwrap()[0])
        .collect();
    assert_eq!(xs, vec![0.0, 0.5, 1.0, 1.5, 2.0]);

    // A single record repeats.
    let mut single = RecordBuffer::new(RecordSchema::mobject(), 1);
    single.write(0, "point", &[7.0, 0.0, 0.0]);
    single.resize_with_interpolation(4);
    for i in 0..4 {
        assert_eq!(single.read(i, "point").unwrap()[0], 7.0);
    }

    // An all-equal buffer repeats rather than interpolating.
    let mut constant = RecordBuffer::new(RecordSchema::mobject(), 3);
    for i in 0..3 {
        constant.write(i, "point", &[4.0, 4.0, 4.0]);
        constant.write(i, "rgba", &[1.0, 0.0, 0.0, 1.0]);
    }
    constant.resize_with_interpolation(6);
    for i in 0..6 {
        assert_eq!(constant.read(i, "point").unwrap(), vec![4.0, 4.0, 4.0]);
    }

    // Zero target empties; same length is a no-op that keeps the storage.
    let mut empty_target = RecordBuffer::new(RecordSchema::mobject(), 3);
    empty_target.resize_with_interpolation(0);
    assert!(empty_target.is_empty());
    let mut same = RecordBuffer::new(RecordSchema::mobject(), 3);
    let id_before = same.storage_id();
    same.resize_with_interpolation(3);
    assert_eq!(same.storage_id(), id_before);
}
