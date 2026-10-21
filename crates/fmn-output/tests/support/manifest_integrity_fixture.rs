use fmn_hash::{Writer, sha256};
use fmn_output::{
    ClosureItem, ManifestIdentity, ManifestMode, ManifestOutput, PROVENANCE_SCHEMA,
    ProvenanceManifest, StructuralField,
};

pub fn manifest() -> ProvenanceManifest {
    let build = "release:manifest-integrity-fixture";
    let mut items = vec![
        ClosureItem::byte_input(1, "scene.rs", b"scene", "source").unwrap(),
        ClosureItem::byte_input(2, "franken_manim.build", build.as_bytes(), "build").unwrap(),
        ClosureItem::byte_input(2, "SUITE.lock", b"suite", "suite").unwrap(),
    ];
    for id in 3..=10 {
        items.push(
            ClosureItem::structural(id, format!("C{id}"), &[StructuralField::U64(u64::from(id))])
                .unwrap(),
        );
    }
    let declared_config_digest = items.last().unwrap().digest;
    ProvenanceManifest::new(
        ManifestMode::Certified,
        items,
        ManifestIdentity {
            build_id: build.to_owned(),
            suite_lock_digest: sha256(b"suite"),
            toolchain: "nightly-test".to_owned(),
            target_triple: "x86_64-unknown-linux-gnu".to_owned(),
            target_features: "baseline".to_owned(),
            engine: "certified-cpu:scalar:1".to_owned(),
            simd_tier: "portable".to_owned(),
            declared_config_digest,
        },
        vec![ManifestOutput {
            virtual_path: "scene.wav".to_owned(),
            kind: "wav".to_owned(),
            digest: sha256(b"waveform"),
            certified: true,
        }],
        None,
    )
    .unwrap()
}

// An independent wire writer deliberately bypasses manifest admission. Invalid
// fixtures retain correct FMNP framing/checksums so decoding must reject their
// semantics, not merely catch accidental corruption of the outer envelope.
pub fn unchecked_bytes(manifest: &ProvenanceManifest) -> Vec<u8> {
    let mut writer = Writer::new(PROVENANCE_SCHEMA);
    writer.put_u8(match manifest.mode {
        ManifestMode::Standard => 0,
        ManifestMode::Certified => 1,
    });
    writer.put_digest(&manifest.closure_digest);
    let identity = &manifest.identity;
    writer.put_str(&identity.build_id);
    writer.put_digest(&identity.suite_lock_digest);
    writer.put_str(&identity.toolchain);
    writer.put_str(&identity.target_triple);
    writer.put_str(&identity.target_features);
    writer.put_str(&identity.engine);
    writer.put_str(&identity.simd_tier);
    writer.put_digest(&identity.declared_config_digest);
    writer.put_u32(u32::try_from(manifest.items.len()).unwrap());
    for item in &manifest.items {
        writer.put_u8(item.item_id);
        writer.put_bool(item.virtual_path.is_some());
        if let Some(path) = &item.virtual_path {
            writer.put_str(path);
        }
        writer.put_digest(&item.digest);
        writer.put_str(&item.detail);
    }
    writer.put_u32(u32::try_from(manifest.outputs.len()).unwrap());
    for output in &manifest.outputs {
        writer.put_str(&output.virtual_path);
        writer.put_str(&output.kind);
        writer.put_digest(&output.digest);
        writer.put_bool(output.certified);
    }
    writer.put_bool(manifest.journal_ref.is_some());
    if let Some(digest) = manifest.journal_ref {
        writer.put_digest(&digest);
    }
    writer.finish().unwrap()
}
