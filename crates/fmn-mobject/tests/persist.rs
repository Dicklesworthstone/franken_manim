//! The persistence layer's acceptance surface (§8.7, fm-879): round-trip
//! across a representative scene (geometry + uniforms + family topology +
//! trackers + §8.3 links + RNG), byte determinism (twice, and across a
//! re-open), the updater honesty clause, versioning and corruption
//! refusals, and cross-stage decode (handles re-bound to a fresh mint).

use fmn_core::rng::Pcg64Dxsm;
use fmn_hash::{SerialError, sha256};
use fmn_mobject::record::{RecordBuffer, RecordSchema};
use fmn_mobject::{JointType, Mob, Mobject, PersistError, SceneState, Snapshot, Stage};

fn vmob(stage: &mut Stage, points: &[[f64; 3]], fill: [f32; 4]) -> Mob {
    let mob = stage.add(Mobject::new());
    let entry = stage.get_mut(mob).unwrap();
    entry.buffer = RecordBuffer::new(RecordSchema::vmobject(), points.len());
    #[allow(clippy::cast_possible_truncation)]
    let flat: Vec<f32> = points
        .iter()
        .flat_map(|p| p.iter().map(|v| *v as f32))
        .collect();
    entry.buffer.write_range("point", 0, &flat);
    entry
        .buffer
        .write_range("fill_rgba", 0, &fill.repeat(points.len()));
    mob
}

/// A representative scene: a rooted family with styled children, tweaked
/// uniforms, a value tracker, §8.3 links, and a pin.
fn build_scene(stage: &mut Stage) -> (Mob, Mob, Mob, Mob) {
    let root = stage.add(Mobject::new());
    let c1 = vmob(
        stage,
        &[[0.0; 3], [1.0, 0.0, 0.0], [2.0, 0.0, 0.0]],
        [0.2, 0.4, 0.6, 1.0],
    );
    let c2 = vmob(
        stage,
        &[[0.0, 3.0, 0.0], [1.0, 3.0, 0.0], [2.0, 3.0, 0.0]],
        [0.9, 0.1, 0.1, 0.5],
    );
    stage.attach(root, c1).unwrap();
    stage.attach(root, c2).unwrap();
    stage.add_to_scene(root).unwrap();
    {
        let u = stage.get_mut(c1).unwrap().uniforms_mut();
        u.anti_alias_width = 2.5;
        u.flat_stroke = true;
        u.joint_type = JointType::Miter;
        u.shading = [0.1, 0.2, 0.3];
    }
    let tracker = stage.add_value_tracker(42.5);
    stage.generate_target(c1).unwrap();
    stage.save_state(c2).unwrap();
    stage.pin(c1).unwrap();
    (root, c1, c2, tracker)
}

fn column(stage: &Stage, mob: Mob, field: &str) -> Vec<f32> {
    stage.get(mob).unwrap().buffer.read_column(field).unwrap()
}

#[test]
fn round_trip_representative_scene() {
    let mut stage = Stage::new();
    let (root, c1, c2, tracker) = build_scene(&mut stage);
    let points_before = column(&stage, c1, "point");
    let fill_before = column(&stage, c2, "fill_rgba");
    let bytes = stage.snapshot_bytes().unwrap();

    // Mutate everything the snapshot should undo.
    stage.shift(c1, [5.0, 5.0, 0.0]);
    stage.set_family_opacity_zero(c2);
    stage.detach(root, c2);
    stage.set_tracker_value(tracker, -1.0).unwrap();

    let decoded = Snapshot::from_bytes(&bytes, &stage).unwrap();
    stage.restore(&decoded.snapshot);

    assert_eq!(column(&stage, c1, "point"), points_before);
    assert_eq!(column(&stage, c2, "fill_rgba"), fill_before);
    assert_eq!(stage.family(root), vec![root, c1, c2]);
    assert_eq!(stage.roots(), &[root]);
    let u = *stage.get(c1).unwrap().uniforms();
    assert_eq!(u.anti_alias_width, 2.5);
    assert!(u.flat_stroke);
    assert_eq!(u.joint_type, JointType::Miter);
    assert_eq!(u.shading, [0.1, 0.2, 0.3]);
    assert_eq!(stage.tracker_value(tracker), Some(42.5));
    assert!(stage.target(c1).is_some());
    assert!(stage.saved_state(c2).is_some());
    assert_eq!(stage.get(c1).unwrap().pins(), 1);
}

#[test]
fn byte_determinism_twice_and_across_reopen() {
    let mut stage = Stage::new();
    build_scene(&mut stage);
    let b1 = stage.snapshot_bytes().unwrap();
    let b2 = stage.snapshot_bytes().unwrap();
    assert_eq!(b1, b2, "same state ⇒ same bytes");

    let decoded = Snapshot::from_bytes(&b1, &stage).unwrap();
    let reencoded = decoded.snapshot.to_bytes().unwrap();
    assert_eq!(b1, reencoded, "re-open ⇒ identical bytes (no callables)");

    let snap = stage.snapshot();
    assert_eq!(
        snap.content_hash().unwrap(),
        sha256(&b1),
        "content hash is the canonical bytes' sha256"
    );
}

#[test]
fn updater_identities_survive_but_callables_do_not() {
    let mut stage = Stage::new();
    let mob = vmob(&mut stage, &[[0.0; 3]], [1.0; 4]);
    let id = stage.add_updater(mob, |_, _| {}, false).unwrap();
    let with_updater = stage.snapshot_bytes().unwrap();

    let decoded = Snapshot::from_bytes(&with_updater, &stage).unwrap();
    // The manifest carries (id, kind)…
    assert_eq!(decoded.updaters.entries.len(), 1);
    let (_, ids) = &decoded.updaters.entries[0];
    assert_eq!(ids.len(), 1);
    assert_eq!(
        ids[0].1,
        fmn_mobject::UpdaterKindTag::NonDt,
        "kind recorded"
    );
    // …the restored stage carries no callables…
    stage.restore(&decoded.snapshot);
    assert!(stage.updater_ids(mob).is_empty());
    let _ = id;
    // …and re-encoding therefore differs (the documented honesty clause).
    let reencoded = stage.snapshot_bytes().unwrap();
    assert_ne!(with_updater, reencoded);
}

#[test]
fn future_major_is_refused_by_name() {
    let mut stage = Stage::new();
    build_scene(&mut stage);
    let mut bytes = stage.snapshot_bytes().unwrap();
    // Header: magic[4] | schema u32 | major u16 LE at offset 8.
    bytes[8] = bytes[8].wrapping_add(1);
    // Re-seal the checksum so only the version differs.
    let body_len = bytes.len() - 32;
    let digest = sha256(&bytes[..body_len]);
    bytes[body_len..].copy_from_slice(digest.as_bytes());
    let err = Snapshot::from_bytes(&bytes, &stage)
        .map(|_| ())
        .expect_err("a future major must be refused");
    assert!(
        matches!(err, PersistError::Serial(SerialError::MajorMismatch { .. })),
        "expected MajorMismatch, got {err:?}"
    );
}

#[test]
fn corruption_is_detected_before_any_payload_is_read() {
    let mut stage = Stage::new();
    build_scene(&mut stage);
    let mut bytes = stage.snapshot_bytes().unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0x40;
    let err = Snapshot::from_bytes(&bytes, &stage)
        .map(|_| ())
        .expect_err("corruption must be refused");
    assert!(
        matches!(err, PersistError::Serial(SerialError::ChecksumMismatch)),
        "expected ChecksumMismatch, got {err:?}"
    );
}

#[test]
fn scene_state_round_trips_with_the_rng() {
    let mut stage = Stage::new();
    build_scene(&mut stage);
    let mut rng = Pcg64Dxsm::from_seed(7);
    for _ in 0..5 {
        rng.next_u64();
    }
    let state = SceneState::capture(&stage, 3, &rng);
    let bytes = state.to_bytes().unwrap();
    let decoded = SceneState::from_bytes(&bytes, &stage).unwrap();
    assert_eq!(decoded.time, stage.time());
    assert_eq!(decoded.play_count, 3);
    let mut restored = decoded.rng();
    assert_eq!(restored, rng, "generator state is bit-identical");
    assert_eq!(restored.next_u64(), rng.clone().next_u64());
}

#[test]
fn cross_stage_decode_rebinds_handles() {
    let mut source = Stage::new();
    let (_, c1, _, _) = build_scene(&mut source);
    let points = column(&source, c1, "point");
    let bytes = source.snapshot_bytes().unwrap();

    // A fresh arena with a different process-local mint.
    let mut target = Stage::new();
    let decoded = Snapshot::from_bytes(&bytes, &target).unwrap();
    target.restore(&decoded.snapshot);

    let roots = target.roots().to_vec();
    assert_eq!(roots.len(), 1);
    let family = target.family(roots[0]);
    assert_eq!(family.len(), 3, "root + two children");
    // The first child's geometry travelled intact.
    let restored_points = target
        .get(family[1])
        .unwrap()
        .buffer
        .read_column("point")
        .unwrap();
    assert_eq!(restored_points, points);
    // And the re-bound handles are live in the new stage.
    assert!(target.contains(family[1]));
}
