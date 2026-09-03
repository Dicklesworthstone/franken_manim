#![forbid(unsafe_code)]
#![allow(clippy::expect_used, clippy::panic)]

#[cfg(feature = "batch")]
use std::ffi::OsString;
#[cfg(feature = "batch")]
use std::fs;
#[cfg(feature = "batch")]
use std::path::{Path, PathBuf};
#[cfg(feature = "batch")]
use std::process::{Command, Output, Stdio};
#[cfg(feature = "batch")]
use std::sync::atomic::{AtomicU64, Ordering};

#[test]
fn parser_smoke_remains_available_without_a_partial_shipping_binary() {
    let invocation = fmn_cli::parse_args(["doctor", "--robot"])
        .expect("the schema-generated doctor command parses");
    assert!(matches!(
        invocation,
        fmn_cli::Invocation::Doctor(fmn_cli::DoctorCommand {
            common: fmn_cli::CommonOptions { robot: true, .. },
            ..
        })
    ));
}

#[cfg(feature = "batch")]
static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

#[cfg(feature = "batch")]
struct Fixture {
    root: PathBuf,
}

#[cfg(feature = "batch")]
impl Fixture {
    fn new() -> Self {
        for _ in 0..1_024 {
            let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir()
                .join(format!("fmn-cli-smoke-{}-{sequence}", std::process::id()));
            match fs::create_dir(&root) {
                Ok(()) => return Self { root },
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => panic!("could not create CLI smoke fixture {root:?}: {error}"),
            }
        }
        panic!("could not allocate a unique CLI smoke fixture");
    }

    fn cache(&self) -> PathBuf {
        self.root.join("cache")
    }

    fn missing_ffmpeg(&self) -> PathBuf {
        self.root.join("definitely-missing-ffmpeg")
    }
}

#[cfg(feature = "batch")]
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[cfg(feature = "batch")]
fn run(fixture: &Fixture, args: Vec<OsString>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fmn"));
    command
        .args(args)
        .current_dir(&fixture.root)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("HOME")
        .env_remove("APPDATA")
        .env_remove("USERPROFILE")
        .stdin(Stdio::null());
    command.output().expect("the shipped fmn binary runs")
}

#[cfg(feature = "batch")]
fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(|value| OsString::from(*value)).collect()
}

#[cfg(feature = "batch")]
fn doctor_args(fixture: &Fixture, extra: &[&str]) -> Vec<OsString> {
    let mut values = args(&["doctor"]);
    values.extend(extra.iter().map(|value| OsString::from(*value)));
    values.push(OsString::from("--ffmpeg"));
    values.push(fixture.missing_ffmpeg().into_os_string());
    values.push(OsString::from("--cache-dir"));
    values.push(fixture.cache().into_os_string());
    values
}

#[cfg(feature = "batch")]
fn write_synthetic_timeline(path: &Path) {
    let mut stage = fmn_scene::studio_bridge::Stage::new();
    let mut timeline = fmn_scene::studio_bridge::Timeline::new(4).expect("valid fixture FPS");
    timeline.wait(0.25).expect("one-frame wait segment");
    let bytes = fmn_scene::export_timeline_bundle(
        timeline,
        &mut stage,
        &fmn_core::rng::RngRoot::from_seed(0),
    )
    .expect("synthetic FMTL export");
    fs::write(path, bytes).expect("write synthetic FMTL scene");
}

#[cfg(feature = "batch")]
struct SyntheticBatch {
    argv: Vec<OsString>,
    artifact: PathBuf,
    manifest_binary: PathBuf,
    manifest_text: PathBuf,
}

#[cfg(feature = "batch")]
fn synthetic_batch(fixture: &Fixture) -> SyntheticBatch {
    const SCENE: &str = "synthetic_batch";

    let source = fixture.root.join(format!("{SCENE}.fmtl"));
    let output_root = fixture.root.join("batch-output");
    let manifest_root = fixture.root.join("batch-manifests");
    fs::create_dir(&output_root).expect("create batch output root");
    fs::create_dir(&manifest_root).expect("create batch manifest root");
    write_synthetic_timeline(&source);

    let mut argv = args(&[
        "batch",
        "--robot",
        "--format",
        "png",
        "--resolution",
        "16x16",
        "--fps",
        "4",
        "--threads",
        "1",
        "--max-scenes",
        "1",
        "--manifest-dir",
    ]);
    argv.push(manifest_root.clone().into_os_string());
    argv.push(OsString::from("--video_dir"));
    argv.push(output_root.clone().into_os_string());
    argv.push(source.into_os_string());

    SyntheticBatch {
        argv,
        artifact: output_root.join(format!("{SCENE}.png")),
        manifest_binary: manifest_root.join(SCENE).join("manifest.fmnp"),
        manifest_text: manifest_root.join(SCENE).join("manifest.txt"),
    }
}

#[cfg(feature = "batch")]
fn stdout(output: &Output) -> &str {
    std::str::from_utf8(&output.stdout).expect("stdout is UTF-8")
}

#[cfg(feature = "batch")]
fn stderr(output: &Output) -> &str {
    std::str::from_utf8(&output.stderr).expect("stderr is UTF-8")
}

#[cfg(feature = "batch")]
fn record_kind(line: &str) -> &str {
    let (_, tail) = line
        .split_once("\"kind\":\"")
        .expect("robot record carries a kind");
    tail.split_once('"')
        .expect("robot kind has a closing quote")
        .0
}

#[cfg(feature = "batch")]
fn assert_code(output: &Output, expected: i32) {
    assert_eq!(
        output.status.code(),
        Some(expected),
        "unexpected status; stdout={} stderr={}",
        stdout(output),
        stderr(output)
    );
}

#[cfg(feature = "batch")]
#[test]
fn shipped_binary_reports_version_and_generated_help() {
    let fixture = Fixture::new();

    let version = run(&fixture, args(&["--version"]));
    assert_code(&version, 0);
    assert_eq!(
        stdout(&version),
        format!("fmn {}\n", env!("CARGO_PKG_VERSION"))
    );
    assert!(stderr(&version).is_empty());

    let help = run(&fixture, args(&["--help"]));
    assert_code(&help, 0);
    assert!(stderr(&help).is_empty());
    assert!(stdout(&help).contains("Usage"));
    assert!(stdout(&help).contains("fmn"));
    for command in ["render", "doctor", "batch", "studio"] {
        assert!(
            stdout(&help).contains(command),
            "generated help omitted {command:?}: {}",
            stdout(&help)
        );
    }
}

#[cfg(feature = "batch")]
#[test]
fn shipped_doctor_robot_stream_has_the_complete_versioned_schema() {
    let fixture = Fixture::new();
    let output = run(&fixture, doctor_args(&fixture, &["--robot"]));
    assert_code(&output, 0);
    assert!(stderr(&output).is_empty());
    assert!(stdout(&output).ends_with('\n'));

    let lines = stdout(&output).lines().collect::<Vec<_>>();
    assert_eq!(
        lines
            .iter()
            .map(|line| record_kind(line))
            .collect::<Vec<_>>(),
        [
            "topology",
            "execution_plan",
            "ffmpeg",
            "cache",
            "fonts",
            "math_packs",
            "certification",
        ]
    );
    for line in lines {
        assert!(
            line.starts_with("{\"schema\":\"fmn.doctor\",\"version\":1,"),
            "noncanonical doctor record: {line}"
        );
        assert!(line.ends_with('}'), "unterminated doctor record: {line}");
    }
}

#[cfg(feature = "batch")]
#[test]
fn shipped_batch_renders_a_synthetic_scene_and_publishes_its_manifest() {
    let fixture = Fixture::new();
    let batch = synthetic_batch(&fixture);

    let output = run(&fixture, batch.argv.clone());
    assert_code(&output, 0);
    assert!(stderr(&output).is_empty());
    let lines = stdout(&output).lines().collect::<Vec<_>>();
    assert_eq!(
        lines
            .iter()
            .map(|line| record_kind(line))
            .collect::<Vec<_>>(),
        ["render", "batch"]
    );
    assert!(lines[0].contains("\"source\":\"compiled\""));
    assert!(lines[0].contains("\"scene\":\"synthetic_batch\""));
    assert!(lines[0].contains("\"format\":\"png\""));
    assert!(lines[0].contains("\"frames\":1"));
    assert!(lines[0].contains("\"manifest\":"));
    assert!(lines[1].contains("\"status\":\"ok\""));
    assert!(lines[1].contains("\"jobs\":1"));
    assert!(lines[1].contains("\"succeeded\":1"));
    assert!(lines[1].contains("\"failed\":0"));
    assert!(lines[1].contains("\"cancelled\":0"));
    assert!(lines[1].contains("\"max_scenes\":1"));

    let png = fs::read(&batch.artifact).expect("batch publishes its PNG");
    assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert!(png.len() > 8);
    let manifest_binary = fs::read(&batch.manifest_binary).expect("batch publishes manifest.fmnp");
    let manifest_text =
        fs::read_to_string(&batch.manifest_text).expect("batch publishes UTF-8 manifest.txt");
    assert!(!manifest_binary.is_empty());
    assert!(!manifest_text.is_empty());

    let retry = run(&fixture, batch.argv);
    assert_ne!(retry.status.code(), Some(0));
    assert!(stderr(&retry).is_empty());
    assert_eq!(stdout(&retry).lines().count(), 1);
    assert_eq!(record_kind(stdout(&retry).trim_end()), "error");
    assert!(stdout(&retry).contains("\"exit_name\":\"render\""));
    assert!(stdout(&retry).contains("already exists"));
    assert_eq!(
        fs::read(&batch.artifact).expect("first artifact remains intact"),
        png
    );
    assert_eq!(
        fs::read(&batch.manifest_binary).expect("first manifest remains intact"),
        manifest_binary
    );
}

#[cfg(feature = "batch")]
#[test]
fn shipped_binary_preserves_typed_usage_and_capability_exit_codes() {
    let fixture = Fixture::new();

    let usage = run(&fixture, args(&["--robot", "-l", "-m"]));
    assert_code(&usage, 2);
    assert!(stderr(&usage).is_empty());
    assert_eq!(stdout(&usage).lines().count(), 1);
    assert!(stdout(&usage).contains("\"kind\":\"error\""));
    assert!(stdout(&usage).contains("\"exit_name\":\"usage\""));
    assert!(stdout(&usage).contains("\"rule\":\"quality-exclusive\""));

    let capability = run(
        &fixture,
        doctor_args(&fixture, &["--robot", "--require-ffmpeg"]),
    );
    assert_code(&capability, 4);
    assert!(stderr(&capability).is_empty());
    let lines = stdout(&capability).lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 8);
    assert_eq!(record_kind(lines[7]), "error");
    assert!(lines[7].contains("\"exit_name\":\"capability\""));
    assert!(lines[7].contains("native PNG-sequence, GIF, and y4m outputs remain available"));
}

#[cfg(feature = "batch")]
#[test]
fn shipped_doctor_quiet_mode_suppresses_only_non_error_output() {
    let fixture = Fixture::new();

    let success = run(&fixture, doctor_args(&fixture, &["--quiet"]));
    assert_code(&success, 0);
    assert!(stdout(&success).is_empty());
    assert!(stderr(&success).is_empty());

    let failure = run(
        &fixture,
        doctor_args(&fixture, &["--quiet", "--require-ffmpeg"]),
    );
    assert_code(&failure, 4);
    assert!(stdout(&failure).is_empty());
    assert!(stderr(&failure).contains("ffmpeg was required but is unavailable"));
}

#[cfg(feature = "batch")]
#[test]
fn smoke_fixture_paths_are_absolute_and_do_not_depend_on_repo_state() {
    let fixture = Fixture::new();
    assert!(fixture.root.is_absolute());
    assert!(!fixture.root.join("custom_config.yml").exists());
}
