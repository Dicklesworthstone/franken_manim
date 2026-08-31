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
            let root = std::env::temp_dir().join(format!(
                "fmn-cli-smoke-{}-{sequence}",
                std::process::id()
            ));
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
    values.iter().map(OsString::from).collect()
}

#[cfg(feature = "batch")]
fn doctor_args(fixture: &Fixture, extra: &[&str]) -> Vec<OsString> {
    let mut values = args(&["doctor"]);
    values.extend(extra.iter().map(OsString::from));
    values.push(OsString::from("--ffmpeg"));
    values.push(fixture.missing_ffmpeg().into_os_string());
    values.push(OsString::from("--cache-dir"));
    values.push(fixture.cache().into_os_string());
    values
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
    assert_eq!(stdout(&version), format!("fmn {}\n", env!("CARGO_PKG_VERSION")));
    assert!(stderr(&version).is_empty());

    let help = run(&fixture, args(&["--help"]));
    assert_code(&help, 0);
    assert!(stderr(&help).is_empty());
    assert!(stdout(&help).starts_with("Usage: fmn [OPTIONS]"));
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
        lines.iter().map(|line| record_kind(line)).collect::<Vec<_>>(),
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
    assert!(Path::new(&fixture.root).is_absolute());
    assert!(!fixture.root.join("custom_config.yml").exists());
}
