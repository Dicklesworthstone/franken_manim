//! Exercise the shipping binary, its actual dispatch counters and FMNP output.
#![forbid(unsafe_code)]
#![cfg(all(feature = "cli", feature = "batch"))]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use fmn_output::ProvenanceManifest;

fn root() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!("fmn-cli-pure-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    std::fs::create_dir(&path).unwrap();
    path
}

fn invoke(root: &Path, threads: &str, extra: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_fmn"))
        .current_dir(root)
        .args(["--robot", "--reproducible", "--format", "png_sequence", "--resolution", "32x24", "--fps", "8", "--threads", threads, "--video_dir"])
        .arg(root).args(extra).args(["@builtin", "circle_shift.v1"])
        .env("PATH", "").env_remove("HOME").env_remove("XDG_CONFIG_HOME")
        .env_remove("APPDATA").env_remove("USERPROFILE")
        .output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(output.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&output.stderr));
    stdout
}

fn pngs(root: &Path) -> Vec<Vec<u8>> {
    let mut paths: Vec<_> = std::fs::read_dir(root).unwrap().map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "png")).collect();
    paths.sort();
    paths.iter().map(|path| std::fs::read(path).unwrap()).collect()
}

#[test]
fn shipping_native_pure_segments_report_real_work_and_identical_certified_frames() {
    let mut expected = None;
    for threads in ["1", "4", "16"] {
        let root = root();
        let output = invoke(&root, threads, &[]);
        assert!(output.contains("\"execution_dispatch\":{\"schema\":\"fmn.cli.dispatch\",\"version\":1,\"pure_segments\":2,\"serial_segments\":0,\"pure_frames\":3,\"compiled_frames\":0}"), "{output}");
        assert!(output.contains("\"outstanding_slots\":0"), "{output}");
        let frames = pngs(&root.join("circle_shift.v1"));
        assert_eq!(frames.len(), 3);
        assert_ne!(frames[0], frames[1]);
        assert_eq!(frames[1], frames[2]);
        if let Some(previous) = &expected { assert_eq!(&frames, previous); }
        else { expected = Some(frames); }
        let bytes = std::fs::read(root.join("circle_shift.v1.manifest/manifest.fmnp")).unwrap();
        let mut manifest = ProvenanceManifest::from_bytes(&bytes).unwrap();
        let c10 = manifest.items.iter_mut().find(|item| item.item_id == 10).unwrap();
        assert!(c10.detail.contains("execution_dispatch="));
        assert!(c10.detail.contains("\"pure_frames\":3"));
        let digest = manifest.closure_digest;
        let identity = manifest.identity.clone();
        // Receipts remain descriptive: changing them cannot change C1--C10.
        c10.detail = "different execution diagnostics".into();
        let reread = ProvenanceManifest::from_bytes(&manifest.to_bytes().unwrap()).unwrap();
        assert_eq!(reread.closure_digest, digest);
        assert_eq!(reread.identity, identity);
    }
}

#[test]
fn animation_ranges_rebase_output_without_losing_the_pure_segment() {
    let root = root();
    let output = invoke(&root, "4", &["-n", "1"]);
    assert!(output.contains("\"pure_segments\":1"), "{output}");
    assert!(output.contains("\"pure_frames\":1"), "{output}");
    assert!(output.contains("\"serial_segments\":0"), "{output}");
    assert!(output.contains("\"outstanding_slots\":0"), "{output}");
}
