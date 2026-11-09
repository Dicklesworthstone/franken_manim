//! Public revision dependencies across real record-storage lifetimes.

use fmn_mobject::{Mobject, RecordBuffer, RecordSchema, Stage};
use fmn_render::{Axis, Dependency, Revisions};

fn records(step: u16) -> RecordBuffer {
    let mut buffer = RecordBuffer::new(RecordSchema::vmobject(), 3).unwrap();
    let x = f32::from(step);
    assert!(buffer.write_range(
        "point",
        0,
        &[x, 0.0, 0.0, x + 0.5, 1.0, 0.0, x + 1.0, 0.0, 0.0],
    ));
    let color = x / 512.0;
    assert!(buffer.write_range(
        "fill_rgba",
        0,
        &[
            color, 0.25, 0.5, 1.0, color, 0.25, 0.5, 1.0, color, 0.25, 0.5, 1.0
        ],
    ));
    buffer
}

#[test]
fn dependencies_observe_rebuilt_buffers_after_clear_and_replace() {
    let mut stage = Stage::new();
    let mob = stage.add(Mobject::from_buffer(records(0)));
    let initial = Revisions::read(&stage, mob).unwrap();
    let mut geometry = Dependency::new(initial, &[Axis::Geometry]);
    let mut style = Dependency::new(initial, &[Axis::Style]);
    let mut previous = {
        let buffer = &stage.get(mob).unwrap().buffer;
        (buffer.storage_id(), buffer.storage_generation())
    };
    let mut address_reuses = 0;
    for step in 1_u16..=256 {
        // An updater can clear and rebuild a public point table before the
        // next observation. Clearing releases its old storage; the Mob and
        // retained Dependency values remain alive throughout this lifecycle.
        stage.get_mut(mob).unwrap().buffer.resize(0).unwrap();
        stage.get_mut(mob).unwrap().buffer = records(step);
        let buffer = &stage.get(mob).unwrap().buffer;
        let identity = (buffer.storage_id(), buffer.storage_generation());
        address_reuses += usize::from(identity.0 == previous.0);
        assert_ne!(identity.1, previous.1);
        assert_eq!(buffer.field_revision("point"), Some(1));
        assert_eq!(buffer.field_revision("fill_rgba"), Some(1));
        assert_eq!(
            buffer.read(0, "point").unwrap(),
            [f32::from(step), 0.0, 0.0]
        );

        let now = Revisions::read(&stage, mob).unwrap();
        assert!(
            geometry.is_stale(&now),
            "geometry stayed current after replacement {step}; allocator reuse observed {address_reuses} times",
        );
        assert!(
            style.is_stale(&now),
            "replacement colors must stale their dependency"
        );
        assert_eq!(now.image, initial.image);
        assert_eq!(now.camera, initial.camera);
        geometry.refresh(now);
        style.refresh(now);
        assert!(!geometry.is_stale(&now));
        assert!(!style.is_stale(&now));
        previous = identity;
    }
    // This records the actual allocator behavior without making address reuse
    // a portable test requirement or manufacturing any buffer identity.
    eprintln!("revision lifecycle observed {address_reuses} allocator address reuses");
}

#[test]
fn shared_snapshots_and_field_edits_keep_precise_revision_dependencies() {
    let mut stage = Stage::new();
    let mob = stage.add(Mobject::from_buffer(records(1)));
    let before = Revisions::read(&stage, mob).unwrap();
    let generation = stage.get(mob).unwrap().buffer.storage_generation();
    let snapshot = stage.snapshot();
    stage.restore(&snapshot);
    assert_eq!(
        stage.get(mob).unwrap().buffer.storage_generation(),
        generation
    );
    assert_eq!(Revisions::read(&stage, mob).unwrap(), before);

    // Independent copies preserve record revisions and bytes but acquire a
    // different allocation identity. Only buffer-derived axes must change.
    let independent = stage.get(mob).unwrap().buffer.deep_clone();
    for field in ["point", "fill_rgba"] {
        assert_eq!(
            independent.field_revision(field),
            stage.get(mob).unwrap().buffer.field_revision(field),
        );
    }
    stage.get_mut(mob).unwrap().buffer = independent;
    let copied = Revisions::read(&stage, mob).unwrap();
    assert_eq!(
        copied.diff(&before),
        [
            Axis::Topology,
            Axis::Geometry,
            Axis::Transform,
            Axis::Style,
            Axis::Order
        ],
    );
    let geometry = Dependency::new(copied, &[Axis::Geometry]);
    assert!(
        stage
            .get_mut(mob)
            .unwrap()
            .buffer
            .write(0, "fill_rgba", &[0.1, 0.2, 0.3, 1.0])
    );
    let recolored = Revisions::read(&stage, mob).unwrap();
    assert_eq!(recolored.diff(&copied), [Axis::Style]);
    assert!(!geometry.is_stale(&recolored));
    stage.shift(mob, [1.0, 2.0, 3.0]);
    let translated = Revisions::read(&stage, mob).unwrap();
    assert_eq!(translated.diff(&recolored), [Axis::Transform]);
    assert!(!geometry.is_stale(&translated));

    // Restoring the still-shared source legitimately restores its old key;
    // cached artifacts from that snapshot remain reusable, without blanket
    // invalidation merely because a restore took place.
    stage.restore(&snapshot);
    let restored = Revisions::read(&stage, mob).unwrap();
    assert_eq!(restored, before);
    assert!(!Dependency::new(before, &[Axis::Geometry, Axis::Style]).is_stale(&restored));
    assert!(geometry.is_stale(&restored));
}
