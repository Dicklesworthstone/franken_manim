//! Certification must compare complete, unambiguous declarations, not a lossy
//! projection of them. All fixtures use the public FMNP construction/decoding API.
use fmn_hash::sha256;
use fmn_output::{ManifestError, ManifestMode, ProvenanceManifest};

#[path = "support/manifest_integrity_fixture.rs"]
mod fixture;
use fixture::{manifest, unchecked_bytes};

fn rebuild(value: &ProvenanceManifest) -> Result<ProvenanceManifest, ManifestError> {
    ProvenanceManifest::new(
        value.mode,
        value.items.clone(),
        value.identity.clone(),
        value.outputs.clone(),
        value.journal_ref,
    )
}

#[test]
fn duplicate_output_paths_refuse_in_every_order_even_with_matching_digests() {
    let original = manifest();
    for same_digest in [false, true] {
        for certified in [false, true] {
            for reverse in [false, true] {
                let mut value = original.clone();
                let mut duplicate = value.outputs[0].clone();
                if !same_digest {
                    duplicate.digest = sha256(b"different bytes hidden by the later entry");
                }
                duplicate.certified = certified;
                value.outputs.push(duplicate);
                if reverse {
                    value.outputs.reverse();
                }
                let error = ManifestError::Invalid("duplicate manifest output path");
                assert_eq!(rebuild(&value), Err(error.clone()));
                assert_eq!(value.validate(), Err(error.clone()));
                assert_eq!(value.to_bytes(), Err(error.clone()));
                assert_eq!(value.semantic_digest(), Err(error.clone()));
                assert_eq!(value.compare(&original), Err(error.clone()));
                assert_eq!(original.compare(&value), Err(error.clone()));
                assert_eq!(ProvenanceManifest::from_bytes(&unchecked_bytes(&value)), Err(error));
            }
        }
    }
}

#[test]
fn artifact_kind_is_part_of_certified_output_identity() {
    let left = manifest();
    let mut right = left.clone();
    right.outputs[0].kind = "raw_frames".to_owned();
    // Identical hashes cannot turn a declaration about one artifact kind into
    // a proof about another kind. Output evidence stays outside the input seal.
    assert_eq!(left.semantic_digest(), right.semantic_digest());
    for comparison in [left.compare(&right).unwrap(), right.compare(&left).unwrap()] {
        assert!(comparison.semantic_equal && comparison.certified_both);
        assert!(!comparison.certified_bits_agree());
        assert_eq!(comparison.differing_outputs, ["scene.wav"]);
    }
}

#[test]
fn encoded_only_certified_documents_are_valid_but_cannot_prove_bit_agreement() {
    let mut value = manifest();
    value.outputs[0].kind = "encoded_mp4".to_owned();
    value.outputs[0].certified = false;
    let decoded = ProvenanceManifest::from_bytes(&value.to_bytes().unwrap()).unwrap();
    assert_eq!(value, decoded);
    assert_eq!(value.compare(&decoded), Err(ManifestError::NoCertifiedOutputs));
    assert!(ManifestError::NoCertifiedOutputs.to_string().contains("no certified output"));
    // Standard provenance remains useful, but never claims certified agreement.
    let mut standard = value.clone();
    standard.mode = ManifestMode::Standard;
    assert!(!standard.compare(&standard).unwrap().certified_bits_agree());
    assert!(!value.compare(&standard).unwrap().certified_bits_agree());
}

#[test]
fn loss_of_certification_on_only_one_side_is_a_named_difference() {
    let left = manifest();
    let mut right = left.clone();
    right.outputs[0].certified = false;
    for comparison in [left.compare(&right).unwrap(), right.compare(&left).unwrap()] {
        assert!(comparison.certified_both && comparison.semantic_equal);
        assert_eq!(comparison.differing_outputs, ["scene.wav"]);
        assert!(!comparison.certified_bits_agree());
    }
}

#[test]
fn public_field_mutations_cannot_bypass_seal_validation_during_comparison() {
    let original = manifest();
    for mutation in 0..6 {
        let mut changed = original.clone();
        match mutation {
            0 => changed.closure_digest = sha256(b"forged seal"),
            1 => changed.items[0].digest = sha256(b"changed source"),
            2 => changed.identity.build_id = "another build".to_owned(),
            3 => changed.items.swap(0, 1),
            4 => {
                changed.items.pop();
            }
            _ => changed.items[0].virtual_path = Some(String::new()),
        }
        let error = changed.validate().unwrap_err();
        assert_eq!(changed.to_bytes(), Err(error.clone()));
        assert_eq!(changed.semantic_digest(), Err(error.clone()));
        assert_eq!(changed.compare(&original), Err(error.clone()));
        assert_eq!(original.compare(&changed), Err(error));
    }
}

#[test]
fn standard_to_certified_mutation_rechecks_build_identification() {
    let original = manifest();
    let mut changed = original.clone();
    changed.mode = ManifestMode::Standard;
    changed.identity.build_id = "unidentified:fixture".to_owned();
    let build = changed.items.iter_mut().find(|item| {
        item.item_id == 2 && item.virtual_path.as_deref() == Some("franken_manim.build")
    }).unwrap();
    build.digest = sha256(changed.identity.build_id.as_bytes());
    let mut changed = rebuild(&changed).unwrap();
    assert!(changed.validate().is_ok());
    changed.mode = ManifestMode::Certified;
    let error = ManifestError::Invalid("a certified manifest requires an identified build (ADR-0025)");
    assert_eq!(changed.validate(), Err(error.clone()));
    assert_eq!(changed.to_bytes(), Err(error.clone()));
    assert_eq!(changed.compare(&original), Err(error));
}

#[test]
fn a_checksummed_noncanonical_item_order_is_not_repaired_on_decode() {
    let mut value = manifest();
    value.items.swap(0, 1);
    // Authoring construction may sort; reading an existing sealed document
    // must reject its noncanonical ordering instead of silently changing it.
    assert!(rebuild(&value).is_ok());
    assert_eq!(
        ProvenanceManifest::from_bytes(&unchecked_bytes(&value)),
        Err(ManifestError::Invalid("closure items are not in canonical order")),
    );
}

#[test]
fn unchanged_manifests_retain_their_exact_wire_bytes_and_output_order() {
    let mut value = manifest();
    let mut other = value.outputs[0].clone();
    other.virtual_path = "α/frame.png".to_owned();
    other.kind = "canonical_png".to_owned();
    other.digest = sha256(b"png");
    value.outputs.push(other);
    value.outputs.reverse();
    let bytes = unchecked_bytes(&value);
    assert_eq!(value.to_bytes().unwrap(), bytes);
    let decoded = ProvenanceManifest::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, value);
    let mut reordered = value.clone();
    reordered.outputs.reverse();
    assert!(value.compare(&reordered).unwrap().certified_bits_agree());
    assert_eq!(value.to_bytes().unwrap(), bytes);
}

#[test]
fn mixed_output_comparison_is_symmetric_sorted_and_ignores_uncertified_products() {
    let mut left = manifest();
    for path in ["z.wav", "a.wav", "ignored.mp4"] {
        let mut output = left.outputs[0].clone();
        output.virtual_path = path.to_owned();
        output.certified = path != "ignored.mp4";
        left.outputs.push(output);
    }
    let mut right = left.clone();
    right.outputs[1].digest = sha256(b"different z");
    right.outputs[2].virtual_path = "b.wav".to_owned();
    right.outputs[3].digest = sha256(b"different uncertified product");
    right.outputs.reverse();
    let expected = ["a.wav", "b.wav", "z.wav"];
    let a = left.compare(&right).unwrap();
    let b = right.compare(&left).unwrap();
    assert_eq!(a, b);
    assert_eq!(a.differing_outputs, expected);
    assert!(!a.certified_bits_agree());
    let mut other_encoded = left.clone();
    other_encoded.outputs[3].digest = sha256(b"other encoded product");
    assert!(left.compare(&other_encoded).unwrap().certified_bits_agree());
}

#[test]
fn output_count_is_bounded_before_comparison_indexing() {
    let original = manifest();
    let mut changed = original.clone();
    changed.outputs = vec![changed.outputs[0].clone(); 4097];
    let error = ManifestError::CountLimit { field: "output", count: 4097, max: 4096 };
    assert_eq!(changed.validate(), Err(error.clone()));
    assert_eq!(changed.compare(&original), Err(error.clone()));
    assert_eq!(original.compare(&changed), Err(error));
}
