//! Durable provenance for the invisible families created by native alignment.

use fmn_hash::{Limits, Reader, Schema, SerialError, UnknownPolicy, Writer, sha256};
use fmn_mobject::{Mob, Mobject, PersistError, RenderPrimitive, Snapshot, SnapshotLimits, Stage};

fn minor(bytes: &[u8]) -> u16 {
    u16::from_le_bytes(bytes[10..12].try_into().unwrap())
}

fn frame(minor: u16, payload: &[u8]) -> Vec<u8> {
    let mut writer = Writer::new(Schema::new(*b"FMNA", 1, 1, minor));
    for &byte in payload {
        writer.put_u8(byte);
    }
    writer.finish().unwrap()
}

fn rebound(source: &Stage, restored: &Stage, mob: Mob) -> Mob {
    let (index, generation) = source.snapshot_handle(mob).unwrap();
    restored.resolve_snapshot_handle(index, generation).unwrap()
}

/// Two live slots and one hole exercise every liveness/option combination:
/// the source has no provenance, its pad has a source, the last slot is dead.
fn padding_fixture(primitive: RenderPrimitive, glow: f64) -> (Stage, Mob, Mob) {
    let mut stage = Stage::new();
    let root =
        stage.add(Mobject::from_points(&[[1.0, -2.0, 0.5]]).with_render_primitive(primitive));
    stage.get_mut(root).unwrap().uniforms_mut().glow_factor = glow;
    stage.add_to_scene(root).unwrap();
    stage.add_n_more_submobjects(root, 1).unwrap();
    let pad = stage.get(root).unwrap().submobjects()[0];
    let deleted = stage.add(Mobject::new());
    stage.delete(deleted).unwrap();
    (stage, root, pad)
}

// One absent source (2 bytes), one present source (10), and one dead slot (1).
const PROVENANCE_TABLE_BYTES: usize = 13;
// The same slots' v1.9 glow table: two live flags + f64, one dead flag.
const GLOW_TABLE_BYTES: usize = 19;

#[test]
fn durable_padding_round_trip_preserves_source_provenance() {
    let mut source = Stage::new();
    let root = source.add(Mobject::from_points(&[[0.0, 0.0, 0.0]]));
    source.add_to_scene(root).unwrap();
    source.add_n_more_submobjects(root, 1).unwrap();
    let pad = source.get(root).unwrap().submobjects()[0];
    assert_eq!(source.padding_source(pad), Some(root));

    let bytes = source.snapshot_bytes().unwrap();
    let mut restored = Stage::new();
    let decoded = Snapshot::from_bytes(&bytes, &restored).unwrap();
    restored.restore(&decoded.snapshot);
    let restored_root = restored.roots()[0];
    let restored_pad = restored.get(restored_root).unwrap().submobjects()[0];
    assert_ne!(restored_root, root, "decode must use the destination mint");
    assert_eq!(
        restored.padding_source(restored_pad),
        Some(restored_root),
        "durable restore must retain the source used to publish a padding copy"
    );
    assert_eq!(restored.snapshot_bytes().unwrap(), bytes);
}

#[test]
fn nested_padding_chains_and_detached_families_keep_immediate_sources() {
    let mut source = Stage::new();
    let root = source.add(Mobject::new());
    let glyph = source.add(Mobject::from_points(&[[1.0, 0.0, 0.0]]));
    let dot = source.add(Mobject::from_points(&[[1.5, 0.0, 0.0]]));
    source.attach(root, glyph).unwrap();
    source.attach(glyph, dot).unwrap();
    source.add_to_scene(root).unwrap();
    source.add_n_more_submobjects(root, 1).unwrap();
    let first_pad = source.get(root).unwrap().submobjects()[1];
    let first_dot_pad = source.get(first_pad).unwrap().submobjects()[0];

    // Padding an existing padding family must retain its immediate source,
    // not flatten the chain to the original glyph. The binding tier uses the
    // immediate source's class/state when it publishes a missing proxy.
    let holder = source.add(Mobject::new());
    source.attach(holder, first_pad).unwrap();
    source.add_n_more_submobjects(holder, 1).unwrap();
    let second_pad = source.get(holder).unwrap().submobjects()[1];
    let second_dot_pad = source.get(second_pad).unwrap().submobjects()[0];
    source.detach(holder, second_pad);
    assert!(source.get(second_pad).unwrap().parents().is_empty());
    assert!(!source.roots().contains(&second_pad));

    let ordinary_copy = source.copy_family(first_pad).unwrap();
    let pairs = [
        (first_pad, glyph),
        (first_dot_pad, dot),
        (second_pad, first_pad),
        (second_dot_pad, first_dot_pad),
    ];
    let bytes = source.snapshot_bytes().unwrap();
    assert_eq!(minor(&bytes), 11);
    let mut restored = Stage::new();
    let decoded = Snapshot::from_bytes(&bytes, &restored).unwrap();
    restored.restore(&decoded.snapshot);
    for (pad, original) in pairs {
        assert_eq!(source.padding_source(pad), Some(original));
        assert_eq!(
            restored.padding_source(rebound(&source, &restored, pad)),
            Some(rebound(&source, &restored, original))
        );
    }
    for member in restored.family(rebound(&source, &restored, ordinary_copy)) {
        assert_eq!(
            restored.padding_source(member),
            None,
            "ordinary copies are not pads"
        );
    }
    assert_eq!(restored.snapshot_bytes().unwrap(), bytes);
}

#[test]
fn deleted_and_recycled_sources_remain_stale_without_owning_the_source() {
    for recycle in [false, true] {
        let mut source = Stage::new();
        let root = source.add(Mobject::new());
        let original = source.add(Mobject::from_points(&[[1.0, 0.0, 0.0]]));
        source.attach(root, original).unwrap();
        source.add_to_scene(root).unwrap();
        source.add_n_more_submobjects(root, 1).unwrap();
        let pad = source.get(root).unwrap().submobjects()[1];
        let (source_index, source_generation) = source.snapshot_handle(original).unwrap();

        // Remember the exact identity in the destination mint while the
        // source is still live. A later restore must keep that same identity
        // as a stale plain link, even if its slot now holds another mobject.
        let mut restored = Stage::new();
        let before_delete = source.snapshot_bytes().unwrap();
        let decoded = Snapshot::from_bytes(&before_delete, &restored).unwrap();
        restored.restore(&decoded.snapshot);
        let stale_source = restored
            .resolve_snapshot_handle(source_index, source_generation)
            .unwrap();
        source.detach(root, pad);
        source.delete(original).unwrap();
        assert!(
            !source.contains(original),
            "padding provenance must not pin its source"
        );
        let replacement = recycle.then(|| source.add(Mobject::new()));
        if let Some(replacement) = replacement {
            let (index, generation) = source.snapshot_handle(replacement).unwrap();
            assert_eq!(index, source_index);
            assert_ne!(generation, source_generation);
        }
        assert_eq!(source.padding_source(pad), Some(original));

        let bytes = source.snapshot_bytes().unwrap();
        let decoded = Snapshot::from_bytes(&bytes, &restored).unwrap();
        assert!(
            restored.contains(stale_source),
            "decode itself must not mutate the destination"
        );
        restored.restore(&decoded.snapshot);
        let restored_pad = rebound(&source, &restored, pad);
        assert_eq!(restored.padding_source(restored_pad), Some(stale_source));
        assert!(!restored.contains(stale_source));
        if let Some(replacement) = replacement {
            assert_ne!(stale_source, rebound(&source, &restored, replacement));
        }
        assert!(restored.has_padding_sources());
        assert_eq!(restored.snapshot_bytes().unwrap(), bytes);
    }
}

#[test]
fn legacy_snapshots_without_provenance_keep_their_exact_payloads() {
    for (version, primitive, glow) in [
        (8, RenderPrimitive::Vector, 0.0),
        (9, RenderPrimitive::Vector, 0.75),
        (9, RenderPrimitive::DotCloud, 0.0),
        (
            10,
            RenderPrimitive::CameraSortedSurfaceGrid { resolution: (1, 1) },
            0.0,
        ),
    ] {
        let (source, _, _) = padding_fixture(primitive, glow);
        let bytes = source.snapshot_bytes().unwrap();
        assert_eq!(minor(&bytes), 11);
        let mut prefix_end = bytes.len() - 32 - PROVENANCE_TABLE_BYTES;
        if version == 8 {
            prefix_end -= GLOW_TABLE_BYTES;
        }
        // Appended tables leave the entire legacy payload intact. A genuine
        // legacy frame must decode without inventing padding provenance, and
        // re-encoding it must not upgrade or alter any byte unnecessarily.
        let legacy = frame(version, &bytes[24..prefix_end]);
        let mut restored = Stage::new();
        let decoded = Snapshot::from_bytes(&legacy, &restored).unwrap();
        restored.restore(&decoded.snapshot);
        assert!(!restored.has_padding_sources());
        for member in restored.family(restored.roots()[0]) {
            assert_eq!(restored.padding_source(member), None);
        }
        assert_eq!(restored.snapshot_bytes().unwrap(), legacy);
    }
}

#[test]
fn nonpadding_snapshot_bytes_match_the_pre_extension_encoder() {
    // These hashes were captured from unchanged production at 7cc6af7, before
    // adding the padding table. They lock all three existing emission paths.
    for (version, primitive, glow, length, digest) in [
        (
            8,
            RenderPrimitive::Vector,
            0.0,
            514,
            "d7784a2a40c2e5fcb7f20b21e356b0fcbc12cf0a67436acd0441afe535674b10",
        ),
        (
            9,
            RenderPrimitive::Vector,
            0.75,
            524,
            "628e37e646f3536215b1ecca5082fdb9edcad9864a440d6400733e77dc8291b5",
        ),
        (
            10,
            RenderPrimitive::CameraSortedSurfaceGrid { resolution: (1, 1) },
            0.0,
            540,
            "f5cb9d615a4e1d6e74b9c42a33a830c1c96b1ed60bc59b84bea8404a86fdb5e4",
        ),
    ] {
        let mut stage = Stage::new();
        let mob =
            stage.add(Mobject::from_points(&[[1.0, -2.0, 0.5]]).with_render_primitive(primitive));
        stage.get_mut(mob).unwrap().uniforms_mut().glow_factor = glow;
        stage.add_to_scene(mob).unwrap();
        let deleted = stage.add(Mobject::new());
        stage.delete(deleted).unwrap();
        let bytes = stage.snapshot_bytes().unwrap();
        assert_eq!(minor(&bytes), version);
        assert_eq!(bytes.len(), length);
        assert_eq!(sha256(&bytes).to_hex(), digest);
    }
}

#[test]
fn padding_extension_refuses_unknown_versions_and_malformed_tables_atomically() {
    let (source, _, _) = padding_fixture(RenderPrimitive::Vector, 0.0);
    let bytes = source.snapshot_bytes().unwrap();
    let payload = &bytes[24..bytes.len() - 32];
    let table_start = payload.len() - PROVENANCE_TABLE_BYTES;
    assert_eq!(
        &payload[table_start..],
        &[1, 0, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        "fixture must include a missing source, a source handle and a dead slot"
    );
    let mut destination = Stage::new();
    let sentinel = destination.add(Mobject::from_points(&[[9.0, 8.0, 7.0]]));
    destination.add_to_scene(sentinel).unwrap();
    let unchanged = destination.snapshot_bytes().unwrap();

    assert!(matches!(
        Reader::open(
            &bytes,
            Schema::new(*b"FMNA", 1, 1, 10),
            Limits::DEFAULT,
            UnknownPolicy::Strict,
        ),
        Err(SerialError::NewerMinor {
            reader: 10,
            doc: 11
        })
    ));
    let future = frame(12, payload);
    assert!(matches!(
        Snapshot::from_bytes(&future, &destination),
        Err(PersistError::Serial(SerialError::NewerMinor {
            reader: 11,
            doc: 12
        }))
    ));
    for kept in 0..PROVENANCE_TABLE_BYTES {
        let truncated = frame(11, &payload[..table_start + kept]);
        assert!(
            matches!(
                Snapshot::from_bytes(&truncated, &destination),
                Err(PersistError::Serial(SerialError::UnexpectedEof { .. }))
            ),
            "truncated provenance table with {kept} bytes must be refused"
        );
    }
    for (offset, replacement) in [(0, 0), (12, 1)] {
        let mut altered = payload.to_vec();
        altered[table_start + offset] = replacement;
        assert!(matches!(
            Snapshot::from_bytes(&frame(11, &altered), &destination),
            Err(PersistError::Malformed(
                "padding-source table does not match arena liveness"
            ))
        ));
    }
    for offset in [0, 1, 3] {
        let mut altered = payload.to_vec();
        altered[table_start + offset] = 2;
        assert!(matches!(
            Snapshot::from_bytes(&frame(11, &altered), &destination),
            Err(PersistError::Serial(SerialError::NonCanonicalBool { .. }))
        ));
    }
    let mut trailing = payload.to_vec();
    trailing.push(0);
    assert!(matches!(
        Snapshot::from_bytes(&frame(11, &trailing), &destination),
        Err(PersistError::Serial(SerialError::TrailingData {
            remaining: 1
        }))
    ));
    assert_eq!(destination.snapshot_bytes().unwrap(), unchanged);
}

#[test]
fn padding_table_obeys_document_limits_and_uses_already_budgeted_entry_storage() {
    let (stage, _, _) = padding_fixture(RenderPrimitive::Vector, 0.0);
    let snapshot = stage.snapshot();
    let bytes = snapshot.to_bytes().unwrap();
    let limits = Limits {
        max_total: bytes.len(),
        ..Limits::DEFAULT
    };
    assert_eq!(snapshot.to_bytes_with_limits(limits).unwrap(), bytes);
    assert!(matches!(
        snapshot.to_bytes_with_limits(Limits {
            max_total: bytes.len() - 1,
            ..limits
        }),
        Err(SerialError::SizeLimit { .. })
    ));

    let legacy = frame(10, &bytes[24..bytes.len() - 32 - PROVENANCE_TABLE_BYTES]);
    let mut low = 0;
    let mut high = SnapshotLimits::DEFAULT.max_total_decoded_bytes;
    while low + 1 < high {
        let mid = low + (high - low) / 2;
        match Snapshot::from_bytes_with_limits(
            &bytes,
            &stage,
            SnapshotLimits {
                max_total_decoded_bytes: mid,
            },
        ) {
            Ok(_) => high = mid,
            Err(PersistError::AllocationLimit { .. }) => low = mid,
            Err(error) => panic!("unexpected decode failure while finding the budget: {error}"),
        }
    }
    for document in [&bytes, &legacy] {
        let decoded = Snapshot::from_bytes_with_limits(
            document,
            &stage,
            SnapshotLimits {
                max_total_decoded_bytes: high,
            },
        )
        .unwrap();
        assert!(matches!(
            Snapshot::from_bytes_with_limits(
                document,
                &stage,
                SnapshotLimits {
                    max_total_decoded_bytes: high - 1
                },
            ),
            Err(PersistError::AllocationLimit { .. })
        ));
        if document == &bytes {
            assert_eq!(decoded.snapshot.to_bytes().unwrap(), bytes);
        }
    }
}
