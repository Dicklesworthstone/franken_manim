//! The checked-in README FMTL must work in the shipping optimized binary.
#![forbid(unsafe_code)]
#![cfg(all(feature = "cli", feature = "batch"))]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use fmn_codec::{PngLimits, decode_png};

static NEXT: AtomicU64 = AtomicU64::new(0);

fn pngs(directory: &Path) -> Vec<Vec<u8>> {
    let mut paths: Vec<_> = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "png"))
        .collect();
    paths.sort();
    paths
        .iter()
        .map(|path| std::fs::read(path).unwrap())
        .collect()
}

#[test]
fn readme_bundle_renders_all_frames_without_external_tools_at_1_4_16_workers() {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../demo/wasm/bundle.fmtl")
        .canonicalize()
        .unwrap();
    let input = std::fs::read(&source).unwrap();
    let root: PathBuf = std::env::temp_dir().join(format!(
        "fmn-readme-catalog-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::create_dir(&root).unwrap();
    let mut first = None;
    for workers in ["1", "4", "16"] {
        let output = root.join(workers);
        let result = Command::new(env!("CARGO_BIN_EXE_fmn"))
            .args([
                "--reproducible",
                "--format",
                "png_sequence",
                "--resolution",
                "64x36",
            ])
            .args(["--threads", workers, "--video_dir"])
            .arg(&output)
            .arg(&source)
            .arg("DemoTimeline")
            .env("PATH", "")
            .env_remove("PYTHONPATH")
            .env_remove("PYTHONHOME")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr),
        );
        let frames = pngs(&output.join("DemoTimeline"));
        assert_eq!(frames.len(), 45);
        for bytes in &frames {
            let image = decode_png(bytes, &PngLimits::default()).unwrap();
            assert_eq!((image.width, image.height), (64, 36));
        }
        assert!(frames.windows(2).any(|pair| pair[0] != pair[1]));
        if let Some(expected) = &first {
            assert_eq!(&frames, expected, "worker count must not change PNG bytes");
        } else {
            first = Some(frames);
        }
    }
    assert_eq!(
        std::fs::read(source).unwrap(),
        input,
        "never regenerate the fixture"
    );
}
