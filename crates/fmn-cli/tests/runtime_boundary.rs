#![forbid(unsafe_code)]

use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use fmn_cli::{BUILTIN_SCENE_SOURCE, PYTHON_SOURCE_PORTAL_MESSAGE};

static RUN_SUFFIX: AtomicU64 = AtomicU64::new(0);

fn run_clean(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fmn"))
        .args(args)
        .env_remove("PYTHONHOME")
        .env_remove("PYTHONPATH")
        .env("PATH", "")
        .output()
        .expect("launch the standalone fmn binary by absolute path")
}

fn output_root(label: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "fmn-cli-{label}-{}-{}",
        std::process::id(),
        RUN_SUFFIX.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&root).expect("create fresh CLI output root");
    root
}

#[test]
fn standalone_binary_starts_without_a_python_runtime_on_path() {
    let output = run_clean(&["--version"]);

    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    assert!(
        String::from_utf8(output.stdout)
            .expect("version output is UTF-8")
            .starts_with("fmn ")
    );
}

#[test]
fn python_source_is_refused_before_file_or_process_access() {
    for args in [
        &["--robot", "missing-scene.py", "Demo"][..],
        &["--robot", "studio", "missing-scene.PYW", "Demo"],
        &["--robot", "batch", "missing-scene.py", "Demo"],
    ] {
        let output = run_clean(args);
        let stdout = String::from_utf8(output.stdout).expect("robot output is UTF-8");

        assert_eq!(output.status.code(), Some(4), "{args:?}");
        assert!(output.stderr.is_empty(), "{args:?}");
        assert_eq!(stdout.lines().count(), 1, "{args:?}");
        assert!(stdout.contains("\"exit_name\":\"capability\""), "{args:?}");
        assert!(stdout.contains(PYTHON_SOURCE_PORTAL_MESSAGE), "{args:?}");
        assert!(!stdout.contains("composition is unavailable"), "{args:?}");
    }
}

#[test]
fn built_in_corpus_renders_through_the_real_png_and_y4m_sinks() {
    for format in ["png_sequence", "y4m"] {
        let root = output_root(format);
        let root_text = root.to_str().expect("output path is UTF-8");
        let output = run_clean(&[
            "--robot",
            "--write_all",
            "--format",
            format,
            "--resolution",
            "96x54",
            "--fps",
            "8",
            "--threads",
            "1",
            "--video_dir",
            root_text,
            BUILTIN_SCENE_SOURCE,
        ]);
        let stdout = String::from_utf8(output.stdout).expect("robot output is UTF-8");

        assert_eq!(output.status.code(), Some(0), "{format}: {stdout}");
        assert!(output.stderr.is_empty(), "{format}");
        assert_eq!(stdout.lines().count(), 25, "{format}");
        assert!(
            stdout.lines().all(|line| {
                line.contains("\"kind\":\"render\"")
                    && line.contains(&format!("\"format\":\"{format}\""))
                    && line.contains("\"frames\":3")
                    && line.contains("\"engine\":\"fast-cpu")
            }),
            "{format}: {stdout}"
        );

        let entries = std::fs::read_dir(&root)
            .expect("list published corpus root")
            .collect::<Result<Vec<_>, _>>()
            .expect("read published corpus entries");
        assert_eq!(entries.len(), 25, "{format}");
        if format == "png_sequence" {
            let sequence = root.join("circle_shift.v1");
            assert!(sequence.join("FMN_COMPLETE").is_file());
            let frame =
                std::fs::read(sequence.join("frame_000000.png")).expect("read first canonical PNG");
            assert_eq!(&frame[..8], b"\x89PNG\r\n\x1a\n");
        } else {
            let artifact =
                std::fs::read(root.join("circle_shift.y4m")).expect("read native y4m artifact");
            assert!(artifact.starts_with(b"YUV4MPEG2 W96 H54 F8:1"));
            assert!(artifact.windows(6).any(|window| window == b"FRAME\n"));
        }
    }
}

#[test]
fn reproducible_cli_render_is_byte_identical_across_fresh_publications() {
    let mut outputs = Vec::new();
    for label in ["certified-a", "certified-b"] {
        let root = output_root(label);
        let output = run_clean(&[
            "--robot",
            "--reproducible",
            "--format",
            "png_sequence",
            "--resolution",
            "96x54",
            "--fps",
            "8",
            "--threads",
            "1",
            "--video_dir",
            root.to_str().expect("output path is UTF-8"),
            BUILTIN_SCENE_SOURCE,
            "circle_shift.v1",
        ]);
        let stdout = String::from_utf8(output.stdout).expect("robot output is UTF-8");
        assert_eq!(output.status.code(), Some(0), "{stdout}");
        assert!(output.stderr.is_empty());
        assert!(stdout.contains("\"engine\":\"certified-cpu:scalar:"));
        let sequence = root.join("circle_shift.v1");
        outputs.push(
            (0..3)
                .map(|frame| {
                    std::fs::read(sequence.join(format!("frame_{frame:06}.png")))
                        .expect("read certified PNG")
                })
                .collect::<Vec<_>>(),
        );
    }
    assert_eq!(outputs[0], outputs[1]);
}

#[test]
fn unknown_built_in_scene_fails_before_publication() {
    let root = output_root("unknown-scene");
    let output = run_clean(&[
        "--robot",
        "--format",
        "png_sequence",
        "--video_dir",
        root.to_str().expect("output path is UTF-8"),
        BUILTIN_SCENE_SOURCE,
        "NotARealScene",
    ]);
    let stdout = String::from_utf8(output.stdout).expect("robot output is UTF-8");

    assert_eq!(output.status.code(), Some(5));
    assert!(output.stderr.is_empty());
    assert!(stdout.contains("\"exit_name\":\"scene\""));
    assert!(stdout.contains("unknown built-in scene"));
    assert!(!stdout.contains(PYTHON_SOURCE_PORTAL_MESSAGE));
    assert_eq!(
        std::fs::read_dir(root)
            .expect("list untouched output root")
            .count(),
        0
    );
}
