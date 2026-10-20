//! Shipping binary: certified narrated WAV is a self-contained replay product.
#![forbid(unsafe_code)]
#![cfg(all(feature = "cli", feature = "batch"))]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use fmn_output::{ManifestMode, ProvenanceManifest};
use fmn_scene::timeline_bundle::{BundleSoundtrack, attach_bundle_soundtrack};
use fmn_scene::{DEFAULT_MAX_BUNDLE_BYTES, export_timeline_bundle};

static NEXT: AtomicU64 = AtomicU64::new(0);

fn fixture() -> (PathBuf, PathBuf, Vec<f32>) {
    let root = std::env::temp_dir().join(format!(
        "fmn-narrated-manifest-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::create_dir(&root).unwrap();
    let mut stage = fmn::mobject::Stage::new();
    let mut timeline = fmn::animation::Timeline::new(3).unwrap();
    for _ in 0..3 {
        timeline.wait(1.0 / 3.0).unwrap();
    }
    let pictures = export_timeline_bundle(timeline, &mut stage,
        &fmn_core::rng::RngRoot::from_seed(11)).unwrap();
    let samples: Vec<_> = (0_u8..16)
        .flat_map(|n| [f32::from(n) / 32.0, -f32::from(n) / 64.0]).collect();
    let pcm = BundleSoundtrack::new(11, 2, samples.clone()).unwrap();
    let bytes = attach_bundle_soundtrack(&pictures, &pcm, DEFAULT_MAX_BUNDLE_BYTES).unwrap();
    let source = root.join("recording.fmtl");
    std::fs::write(&source, bytes).unwrap();
    (root, source, samples)
}

fn invoke(source: &Path, directory: &Path, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fmn"))
        .current_dir(source.parent().unwrap())
        .args(["--robot", "--format", "wav", "--file_name", "audio", "--video_dir"])
        .arg(directory).args(extra).arg(source)
        .env("PATH", "").env_remove("PYTHONPATH").env_remove("PYTHONHOME")
        .output().unwrap()
}

fn success(output: Output) -> String {
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(output.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&output.stderr));
    assert!(stdout.contains("\"kind\":\"bundle-audio-render\""), "{stdout}");
    assert!(stdout.contains("\"manifest\":{\"path\":"), "{stdout}");
    stdout
}

fn manifest(artifact: &Path) -> ProvenanceManifest {
    let mut name = artifact.file_name().unwrap().to_os_string();
    name.push(".manifest");
    let directory = artifact.with_file_name(name);
    assert!(directory.join(fmn_platform::fs::ATOMIC_DIRECTORY_COMPLETE_LEAF).is_file());
    let bytes = std::fs::read(directory.join("manifest.fmnp")).unwrap();
    let m = ProvenanceManifest::from_bytes(&bytes).unwrap();
    assert_eq!(bytes, m.to_bytes().unwrap());
    assert_eq!(std::fs::read_to_string(directory.join("manifest.txt")).unwrap(), m.to_text());
    assert_eq!(m.outputs[0].digest, fmn_hash::sha256(&std::fs::read(artifact).unwrap()));
    m
}

#[cfg(any(
    all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")),
    all(target_os = "macos", target_arch = "aarch64"),
))]
#[test]
fn certified_shipping_wav_full_excerpts_and_subdivisions_compare_without_external_tools() {
    let (root, source, expected) = fixture();
    let source_digest = fmn_hash::sha256(&std::fs::read(&source).unwrap());
    for (case, selection, subdivide, expected_range) in [
        ("full", None, false, 0..32),
        ("cut", Some("1,3"), false, 8..22),
        ("clips", Some("1,3"), true, 8..22),
        ("explicit-full", Some("0"), false, 0..22),
    ] {
        let mut reference: Option<Vec<ProvenanceManifest>> = None;
        for threads in ["1", "4", "16"] {
            let directory = root.join(format!("{case}-{threads}"));
            let mut extra = vec!["--reproducible", "--threads", threads];
            if let Some(selection) = selection {
                extra.extend(["-n", selection]);
            }
            if subdivide {
                extra.push("--subdivide");
            }
            let stdout = success(invoke(&source, &directory, &extra));
            assert!(stdout.contains("\"certified\":true"));
            let artifacts = if subdivide {
                vec![directory.join("audio/00001.wav"), directory.join("audio/00002.wav")]
            } else {
                vec![directory.join("audio.wav")]
            };
            assert_eq!(stdout.lines().count(), artifacts.len());
            let mut observed = Vec::new();
            let mut pcm = Vec::new();
            for artifact in artifacts {
                let m = manifest(&artifact);
                assert_eq!(m.mode, ManifestMode::Certified);
                assert!(m.outputs[0].certified);
                assert_eq!(m.items.iter().find(|item| item.item_id == 1).unwrap().digest, source_digest);
                let wav = fmn_codec::decode_wav(&std::fs::read(&artifact).unwrap(),
                    &fmn_codec::WavLimits::default()).unwrap();
                assert_eq!((wav.sample_rate, wav.channels), (11, 2));
                pcm.extend(wav.samples);
                observed.push(m);
            }
            assert_eq!(pcm, expected[expected_range.clone()]);
            if let Some(reference) = &reference {
                assert_eq!(reference.len(), observed.len());
                for (a, b) in reference.iter().zip(&observed) {
                    assert!(a.compare(b).unwrap().certified_bits_agree());
                    assert_eq!(a.closure_digest, b.closure_digest);
                }
            } else {
                reference = Some(observed);
            }
        }
    }
}

#[test]
fn shipping_sidecar_collision_does_not_publish_media_or_replace_the_winner() {
    let (root, source, _) = fixture();
    let directory = root.join("out");
    std::fs::create_dir(&directory).unwrap();
    let sidecar = directory.join("audio.wav.manifest");
    std::fs::write(&sidecar, b"existing provenance generation").unwrap();
    let output = invoke(&source, &directory, &[]);
    assert!(!output.status.success());
    assert!(!directory.join("audio.wav").exists());
    assert_eq!(std::fs::read(sidecar).unwrap(), b"existing provenance generation");
    assert!(!String::from_utf8(output.stdout).unwrap().contains("\"kind\":\"bundle-audio-render\""));
}

#[test]
fn shipping_standard_wav_manifest_is_sealed_but_not_certified() {
    let (root, source, expected) = fixture();
    let directory = root.join("standard");
    let stdout = success(invoke(&source, &directory, &[]));
    assert!(stdout.contains("\"certified\":false"));
    let artifact = directory.join("audio.wav");
    let m = manifest(&artifact);
    assert_eq!(m.mode, ManifestMode::Standard);
    assert!(!m.outputs[0].certified);
    let wav = fmn_codec::decode_wav(&std::fs::read(artifact).unwrap(),
        &fmn_codec::WavLimits::default()).unwrap();
    assert_eq!(wav.samples, expected);
}
