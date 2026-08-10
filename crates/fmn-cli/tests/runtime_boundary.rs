#![forbid(unsafe_code)]

use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use fmn_cli::{BUILTIN_SCENE_SOURCE, PYTHON_SOURCE_PORTAL_MESSAGE};
use fmn_core::rng::RngRoot;
use fmn_scene::export_timeline_bundle;
use fmn_scene::studio_bridge::{Stage, Timeline};

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

fn compiled_wait_bundle(root: &std::path::Path) -> std::path::PathBuf {
    let path = root.join("compiled_wait.fmtl");
    let mut timeline = Timeline::new(8).expect("valid bundle fps");
    timeline.wait(0.25).expect("valid bundle wait");
    let bytes = export_timeline_bundle(timeline, &mut Stage::new(), &RngRoot::from_seed(0))
        .expect("compile wait artifact");
    std::fs::write(&path, bytes).expect("write compiled artifact");
    path
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
fn compiled_fmtl_renders_through_the_standalone_binary() {
    let root = output_root("compiled-fmtl");
    let source = compiled_wait_bundle(&root);
    let output_dir = root.join("output");
    std::fs::create_dir(&output_dir).expect("create compiled output root");
    let output = run_clean(&[
        "--robot",
        "--format",
        "png_sequence",
        "--resolution",
        "96x54",
        "--threads",
        "1",
        "--video_dir",
        output_dir.to_str().expect("output path is UTF-8"),
        source.to_str().expect("source path is UTF-8"),
        "CompiledWait",
    ]);
    let stdout = String::from_utf8(output.stdout).expect("robot output is UTF-8");

    assert_eq!(output.status.code(), Some(0), "{stdout}");
    assert!(output.stderr.is_empty());
    assert_eq!(stdout.lines().count(), 1);
    assert!(stdout.contains("\"source\":\"compiled\""));
    assert!(stdout.contains("\"source_artifact\":"));
    assert!(stdout.contains("\"scene\":\"CompiledWait\""));
    assert!(stdout.contains("\"frames\":2"));
    let sequence = output_dir.join("CompiledWait");
    assert!(sequence.join("FMN_COMPLETE").is_file());
    let pngs = std::fs::read_dir(&sequence)
        .expect("list compiled output")
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "png"))
        .count();
    assert_eq!(pngs, 2);
    let frame = std::fs::read(sequence.join("frame_000000.png")).expect("read compiled PNG");
    assert!(frame.starts_with(b"\x89PNG\r\n\x1a\n"));
}

#[test]
fn compiled_fmtl_refuses_schedule_changes_and_malformed_input_before_publication() {
    let root = output_root("compiled-refusals");
    let source = compiled_wait_bundle(&root);
    let output_dir = root.join("output");
    std::fs::create_dir(&output_dir).expect("create refusal output root");

    let fps = run_clean(&[
        "--robot",
        "--format",
        "png_sequence",
        "--fps",
        "9",
        "--video_dir",
        output_dir.to_str().expect("output path is UTF-8"),
        source.to_str().expect("source path is UTF-8"),
    ]);
    let fps_stdout = String::from_utf8(fps.stdout).expect("robot output is UTF-8");
    assert_eq!(fps.status.code(), Some(3), "{fps_stdout}");
    assert!(fps.stderr.is_empty());
    assert!(fps_stdout.contains("fixed 8 fps schedule"));
    assert_eq!(
        std::fs::read_dir(&output_dir).expect("list output").count(),
        0
    );

    let malformed = root.join("malformed.fmtl");
    std::fs::write(&malformed, b"not an FMTL document").expect("write malformed artifact");
    let bad = run_clean(&[
        "--robot",
        "--format",
        "png_sequence",
        "--video_dir",
        output_dir.to_str().expect("output path is UTF-8"),
        malformed.to_str().expect("source path is UTF-8"),
    ]);
    let bad_stdout = String::from_utf8(bad.stdout).expect("robot output is UTF-8");
    assert_eq!(bad.status.code(), Some(5), "{bad_stdout}");
    assert!(bad.stderr.is_empty());
    assert!(bad_stdout.contains("malformed FMTL/1 container"));
    assert_eq!(
        std::fs::read_dir(output_dir).expect("list output").count(),
        0
    );
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

#[test]
fn video_render_names_the_missing_optional_tool_and_native_alternative() {
    let root = output_root("missing-ffmpeg");
    let output = run_clean(&[
        "--robot",
        "--format",
        "video",
        "--resolution",
        "96x54",
        "--fps",
        "8",
        "--video_dir",
        root.to_str().expect("output path is UTF-8"),
        BUILTIN_SCENE_SOURCE,
        "circle_shift.v1",
    ]);
    let stdout = String::from_utf8(output.stdout).expect("robot output is UTF-8");

    assert_eq!(output.status.code(), Some(4), "{stdout}");
    assert!(output.stderr.is_empty());
    assert!(stdout.contains("\"exit_name\":\"capability\""));
    assert!(stdout.contains("ffmpeg is unavailable"));
    assert!(stdout.contains("native outputs need no ffmpeg"));
    assert!(stdout.contains("y4m, PNG sequences, and GIF"));
    assert_eq!(
        std::fs::read_dir(root)
            .expect("list untouched output root")
            .count(),
        0
    );
}
