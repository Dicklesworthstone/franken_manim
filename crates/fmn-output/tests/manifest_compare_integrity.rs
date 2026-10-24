//! Execute the shipping manifest verifier, including checksum-valid hostile
//! declarations. No renderer, external encoder, shell, or mock verifier is used.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use fmn_hash::sha256;
use fmn_output::{ManifestMode, ProvenanceManifest};

#[path = "support/manifest_integrity_fixture.rs"]
mod fixture;
use fixture::{manifest, unchecked_bytes};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        for _ in 0..1024 {
            let index = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "fmn-manifest-integrity-{}-{index}",
                std::process::id(),
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => panic!("create owned fixture directory: {error}"),
            }
        }
        panic!("could not allocate a unique fixture directory")
    }

    fn put(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        // Only this test's successfully created directory is ever removed.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn compare_paths(left: &Path, right: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fmn-manifest-compare"))
        .arg(left)
        .arg(right)
        .output()
        .expect("execute the real manifest comparison binary")
}

fn compare_bytes(left: &[u8], right: &[u8]) -> Output {
    let directory = Directory::new();
    compare_paths(
        &directory.put("left.fmnp", left),
        &directory.put("right.fmnp", right),
    )
}

fn record(output: &Output, code: i32, verdict: &str) -> String {
    assert_eq!(output.status.code(), Some(code), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    let text = String::from_utf8(output.stdout.clone()).unwrap();
    assert_eq!(text.lines().count(), 1, "one terminal robot record");
    assert!(text.contains("\"schema\":\"fmn.manifest-compare\",\"version\":1"));
    assert!(
        text.contains(&format!("\"verdict\":\"{verdict}\"")),
        "{text}"
    );
    text
}

fn malformed(output: &Output, message: &str) {
    assert_eq!(output.status.code(), Some(64), "{output:?}");
    assert!(
        output.stdout.is_empty(),
        "invalid input must not emit a comparison verdict"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(message),
        "{output:?}"
    );
}

#[test]
fn matching_certified_artifacts_still_produce_the_original_success_record() {
    let value = manifest();
    let bytes = value.to_bytes().unwrap();
    let text = record(&compare_bytes(&bytes, &bytes), 0, "agree");
    let digest = value.semantic_digest().unwrap();
    assert!(text.contains(&format!("\"left_semantic_digest\":\"{digest}\"")));
    assert!(text.contains(&format!("\"right_semantic_digest\":\"{digest}\"")));
    assert!(text.contains("\"certified_both\":true"));
    assert!(text.contains("\"differing_outputs\":[]"));
    assert!(!text.contains("\"reason\""));
}

#[test]
fn kind_digest_and_certification_changes_are_real_cli_determinism_failures() {
    let original = manifest();
    for mutation in 0..3 {
        let mut changed = original.clone();
        match mutation {
            0 => changed.outputs[0].kind = "raw_frames".to_owned(),
            1 => changed.outputs[0].digest = sha256(b"different waveform"),
            _ => changed.outputs[0].certified = false,
        }
        let a = original.to_bytes().unwrap();
        let b = changed.to_bytes().unwrap();
        for (left, right) in [(&a, &b), (&b, &a)] {
            let text = record(&compare_bytes(left, right), 1, "determinism-failure");
            assert!(text.contains("\"differing_outputs\":[\"scene.wav\"]"));
        }
    }
}

#[test]
fn encoded_only_documents_report_noncomparability_not_success_or_corruption() {
    let mut value = manifest();
    value.outputs[0].certified = false;
    value.outputs[0].kind = "encoded_mp4".to_owned();
    let bytes = value.to_bytes().unwrap();
    let text = record(&compare_bytes(&bytes, &bytes), 2, "not-comparable");
    assert!(text.contains("\"reason\":\"no-certified-outputs\""));
    assert!(text.contains("\"certified_both\":true"));
    assert!(text.contains("\"differing_outputs\":[]"));
}

#[test]
fn standard_mode_and_changed_inputs_remain_noncomparable() {
    let original = manifest();
    let mut standard = original.clone();
    standard.mode = ManifestMode::Standard;
    let a = original.to_bytes().unwrap();
    let b = standard.to_bytes().unwrap();
    let text = record(&compare_bytes(&a, &b), 2, "not-comparable");
    assert!(text.contains("\"certified_both\":false"));
    let mut changed = original.clone();
    changed.items[0].digest = sha256(b"other scene program");
    let changed = ProvenanceManifest::new(
        changed.mode,
        changed.items,
        changed.identity,
        changed.outputs,
        changed.journal_ref,
    )
    .unwrap();
    let text = record(
        &compare_bytes(&a, &changed.to_bytes().unwrap()),
        2,
        "not-comparable",
    );
    assert!(text.contains("\"semantic_equal\":false"));
}

#[test]
fn checksummed_duplicate_outputs_cannot_hide_different_artifacts_from_the_cli() {
    let original = manifest();
    let mut duplicate = original.clone();
    let mut hidden = duplicate.outputs[0].clone();
    hidden.digest = sha256(b"hidden mismatch");
    duplicate.outputs.insert(0, hidden);
    let valid = original.to_bytes().unwrap();
    for reverse in [false, true] {
        if reverse {
            duplicate.outputs.reverse();
        }
        let forged = unchecked_bytes(&duplicate);
        malformed(
            &compare_bytes(&valid, &forged),
            "duplicate manifest output path",
        );
        malformed(
            &compare_bytes(&forged, &valid),
            "duplicate manifest output path",
        );
    }
}

#[test]
fn checksummed_unsorted_and_mutated_seals_refuse_without_a_verdict() {
    let original = manifest();
    let valid = original.to_bytes().unwrap();
    let mut noncanonical = original.clone();
    noncanonical.items.swap(0, 1);
    malformed(
        &compare_bytes(&valid, &unchecked_bytes(&noncanonical)),
        "canonical order",
    );
    let mut unsealed = original;
    unsealed.items[0].digest = sha256(b"changed after sealing");
    malformed(
        &compare_bytes(&valid, &unchecked_bytes(&unsealed)),
        "closure digest does not match",
    );
}

#[test]
fn missing_nonregular_and_oversized_inputs_refuse_at_the_real_read_boundary() {
    let directory = Directory::new();
    let valid = directory.put("valid.fmnp", &manifest().to_bytes().unwrap());
    let missing = directory.0.join("missing.fmnp");
    malformed(&compare_paths(&valid, &missing), "missing.fmnp");
    malformed(
        &compare_paths(&directory.0, &valid),
        "fmn-manifest-compare:",
    );
    let large = directory.0.join("oversized.fmnp");
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&large)
        .unwrap();
    file.set_len(64 * 1024 * 1024 + 1).unwrap();
    drop(file);
    malformed(&compare_paths(&large, &valid), "oversized.fmnp");
}

#[test]
fn output_names_remain_escaped_in_single_line_diagnostics() {
    let mut left = manifest();
    left.outputs[0].virtual_path = "α/\"chapter\"\\take\n.wav".to_owned();
    let mut right = left.clone();
    right.outputs[0].digest = sha256(b"drift");
    let text = record(
        &compare_bytes(&left.to_bytes().unwrap(), &right.to_bytes().unwrap()),
        1,
        "determinism-failure",
    );
    assert!(text.contains(r#""differing_outputs":["α/\"chapter\"\\take\u000a.wav"]"#));
}
