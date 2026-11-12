//! Dot material state has one scalar authority, independent of record dtypes.

use fmn_hash::{Schema, Writer};
use fmn_mobject::record::{RecordBuffer, RecordSchema};
use fmn_mobject::{Mobject, PersistError, RenderPrimitive, Snapshot, Stage};

fn cloud(glow: f64, custom_glow: Option<&[f32]>) -> Mobject {
    let mut fields = vec![("point", 3), ("radius", 1), ("rgba", 4)];
    if custom_glow.is_some() {
        fields.push(("glow_factor", 1));
    }
    let schema = RecordSchema::new(&fields, &["point"], &["point"]).unwrap();
    let len = custom_glow.map_or(2, <[f32]>::len);
    let mut buffer = RecordBuffer::new(schema, len).unwrap();
    buffer.write_range("radius", 0, &vec![0.5; len]);
    buffer.write_range("rgba", 0, &[1.0; 4].repeat(len));
    if let Some(values) = custom_glow {
        buffer.write_range("glow_factor", 0, values);
    }
    let mut object = Mobject::from_buffer(buffer).with_render_primitive(RenderPrimitive::DotCloud);
    object.uniforms.glow_factor = glow;
    object
}

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

// One live slot appends a presence byte and an f64 in 1.9. The unchanged
// prefix is a genuine 1.8 document once framed under its original schema.
fn legacy_one_cloud(stage: &Stage) -> Vec<u8> {
    let bytes = stage.snapshot_bytes().unwrap();
    assert_eq!(minor(&bytes), 9);
    frame(8, &bytes[24..bytes.len() - 32 - 9])
}

#[test]
fn glow_survives_family_copies_saved_state_become_and_durable_snapshots() {
    let mut stage = Stage::new();
    let mob = stage.add(cloud(3.125, None));
    let copied = stage.copy_family(mob).unwrap();
    let target = stage.generate_target(mob).unwrap();
    let saved = stage.save_state(mob).unwrap();
    for handle in [mob, copied, target, saved] {
        let entry = stage.get(handle).unwrap();
        assert_eq!(entry.uniforms().glow_factor, 3.125);
        assert_eq!(entry.buffer.schema().stride(), 8);
        assert!(entry.buffer.schema().field_width("glow_factor").is_none());
    }
    stage.get_mut(mob).unwrap().uniforms_mut().glow_factor = 9.0;
    stage.restore_mobject(mob).unwrap();
    assert_eq!(stage.get(mob).unwrap().uniforms().glow_factor, 3.125);
    stage.get_mut(copied).unwrap().uniforms_mut().glow_factor = 6.5;
    stage.become_mobject(mob, copied, false).unwrap();
    assert_eq!(stage.get(mob).unwrap().uniforms().glow_factor, 6.5);

    let bytes = stage.snapshot_bytes().unwrap();
    assert_eq!(minor(&bytes), 9);
    stage.get_mut(mob).unwrap().uniforms_mut().glow_factor = 0.0;
    let decoded = Snapshot::from_bytes(&bytes, &stage).unwrap();
    stage.restore(&decoded.snapshot);
    assert_eq!(stage.get(mob).unwrap().uniforms().glow_factor, 6.5);
    assert_eq!(stage.snapshot_bytes().unwrap(), bytes);
}

#[test]
fn snapshots_preserve_custom_glow_data_without_promoting_it_to_material() {
    for glow in [0.0, 1.0e40] {
        let mut stage = Stage::new();
        let mob = stage.add(cloud(glow, Some(&[-7.0, 31.0])));
        let bytes = stage.snapshot_bytes().unwrap();
        assert_eq!(minor(&bytes), 9, "custom data must disambiguate old lanes");
        let decoded = Snapshot::from_bytes(&bytes, &stage).unwrap();
        stage.restore(&decoded.snapshot);
        let entry = stage.get(mob).unwrap();
        assert_eq!(entry.uniforms().glow_factor, glow);
        assert_eq!(
            entry.buffer.read_column("glow_factor").unwrap(),
            [-7.0, 31.0]
        );
        assert_eq!(stage.snapshot_bytes().unwrap(), bytes);
    }
    let mut unchanged = Stage::new();
    unchanged.add(Mobject::new());
    assert_eq!(minor(&unchanged.snapshot_bytes().unwrap()), 8);
    unchanged.add(cloud(0.0, None));
    assert_eq!(minor(&unchanged.snapshot_bytes().unwrap()), 9);
}

#[test]
fn legacy_constant_glow_promotes_exactly_and_preserves_the_old_dtype() {
    for value in [0.0_f32, 2.25, 0.123_456_79] {
        let mut stage = Stage::new();
        let mob = stage.add(cloud(0.0, Some(&[value, value])));
        let old = legacy_one_cloud(&stage);
        let decoded = Snapshot::from_bytes(&old, &stage).unwrap();
        stage.restore(&decoded.snapshot);
        let entry = stage.get(mob).unwrap();
        assert_eq!(entry.uniforms().glow_factor, f64::from(value));
        assert_eq!(entry.buffer.schema().stride(), 9);
        assert_eq!(
            entry.buffer.read_column("glow_factor").unwrap(),
            [value, value]
        );
        // Future material edits, including zero, have one authoritative path.
        stage.set_glow_factor(mob, 0.0, false);
        let current = stage.snapshot_bytes().unwrap();
        assert_eq!(minor(&current), 9);
        let decoded = Snapshot::from_bytes(&current, &stage).unwrap();
        stage.restore(&decoded.snapshot);
        assert_eq!(stage.get(mob).unwrap().uniforms().glow_factor, 0.0);
    }
}

#[test]
fn legacy_empty_cloud_preserves_phantom_glow_and_refuses_lossy_migration() {
    let mut stage = Stage::new();
    let mob = stage.add(cloud(0.0, Some(&[])));
    stage
        .get_mut(mob)
        .unwrap()
        .buffer
        .write_default_record("glow_factor", &[2.5]);
    let decoded = Snapshot::from_bytes(&legacy_one_cloud(&stage), &stage).unwrap();
    stage.restore(&decoded.snapshot);
    assert_eq!(stage.get(mob).unwrap().uniforms().glow_factor, 2.5);

    let mut heterogeneous = Stage::new();
    heterogeneous.add(cloud(0.0, Some(&[1.0, 2.0])));
    let error = Snapshot::from_bytes(&legacy_one_cloud(&heterogeneous), &heterogeneous)
        .map(|_| ())
        .unwrap_err();
    assert_eq!(
        error,
        PersistError::Malformed(
            "legacy DotCloud per-point glow cannot be represented by one finite glow_factor uniform"
        )
    );
}

#[test]
fn malformed_glow_tables_cannot_drop_live_material() {
    let mut stage = Stage::new();
    stage.add(cloud(2.0, None));
    let bytes = stage.snapshot_bytes().unwrap();
    let prefix = &bytes[24..bytes.len() - 32 - 9];
    assert!(Snapshot::from_bytes(&frame(9, prefix), &stage).is_err());
    let mut absent = prefix.to_vec();
    absent.push(0);
    assert_eq!(
        Snapshot::from_bytes(&frame(9, &absent), &stage)
            .map(|_| ())
            .unwrap_err(),
        PersistError::Malformed("glow-uniform table does not match arena liveness")
    );
}
