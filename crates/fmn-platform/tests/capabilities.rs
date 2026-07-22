//! Capability trait-suite tests (fm-x68 acceptance): the std implementations
//! against the real host, and the test doubles' contracts.
//!
//! The process tests exercise the D2 mechanism substrate for real: argv-only
//! spawning, the cleared-environment allowlist, timeout kill, output-cap
//! kill, and stdin plumbing — all against coreutils, Unix-only (`cfg`).

use fmn_platform::fs::{FileSystem, StdFs};
use fmn_platform::process::{ProcessOutcome, ProcessRunner, ProcessSpec, ScriptedRunner};
use std::path::PathBuf;
use std::time::Duration;

fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("caps_{name}"));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

#[test]
fn std_fs_atomic_write_round_trips_and_lists_sorted() {
    let dir = scratch("stdfs");
    let fs = StdFs;
    let deep = dir.join("a/b/file.bin");
    fs.write_atomic(&deep, b"payload")
        .expect("atomic write creates parents");
    assert_eq!(fs.read(&deep).expect("read"), b"payload");
    fs.write_atomic(&deep, b"replaced").expect("atomic replace");
    assert_eq!(fs.read_to_string(&deep).expect("read"), "replaced");
    fs.write_atomic(&dir.join("a/z.txt"), b"z").expect("write");
    fs.write_atomic(&dir.join("a/a.txt"), b"a").expect("write");
    let listed = fs.list_dir(&dir.join("a")).expect("list");
    let names: Vec<String> = listed
        .iter()
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect();
    assert_eq!(names, vec!["a.txt", "b", "z.txt"], "sorted listing");
    assert!(fs.exists(&deep));
    assert!(!fs.exists(&dir.join("missing")));
}

fn spec(program: &str, argv: &[&str]) -> ProcessSpec {
    ProcessSpec {
        program: PathBuf::from(program),
        argv: argv.iter().map(ToString::to_string).collect(),
        env: Vec::new(),
        cwd: None,
        stdin: None,
        timeout: Duration::from_secs(10),
        max_output_bytes: 1 << 20,
    }
}

#[cfg(unix)]
mod std_runner {
    use super::*;
    use fmn_platform::process::StdProcessRunner;

    #[test]
    fn argv_only_echo_succeeds() {
        let out = StdProcessRunner
            .run(&spec("/usr/bin/echo", &["hello", "argv world"]))
            .expect("run");
        assert!(out.success());
        // The two argv entries arrive as two arguments — no shell splitting.
        assert_eq!(out.stdout, b"hello argv world\n");
        assert!(!out.timed_out && !out.truncated);
    }

    #[test]
    fn nonzero_exit_is_an_outcome_not_an_error() {
        let out = StdProcessRunner
            .run(&spec("/usr/bin/false", &[]))
            .expect("run");
        assert_eq!(out.code, Some(1));
        assert!(!out.success());
    }

    #[test]
    fn environment_is_cleared_then_allowlisted() {
        let mut s = spec("/usr/bin/env", &[]);
        s.env = vec![("FMN_ALLOWED".to_string(), "yes".to_string())];
        let out = StdProcessRunner.run(&s).expect("run");
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("FMN_ALLOWED=yes"), "{text}");
        // Nothing ambient leaks: PATH/HOME are gone.
        assert!(!text.contains("PATH="), "ambient PATH leaked: {text}");
        assert!(!text.contains("HOME="), "ambient HOME leaked: {text}");
    }

    #[test]
    fn stdin_bytes_flow_through() {
        let mut s = spec("/usr/bin/cat", &[]);
        s.stdin = Some(b"through the capability".to_vec());
        let out = StdProcessRunner.run(&s).expect("run");
        assert!(out.success());
        assert_eq!(out.stdout, b"through the capability");
    }

    #[test]
    fn timeout_kills_the_child() {
        let mut s = spec("/usr/bin/sleep", &["30"]);
        s.timeout = Duration::from_millis(200);
        let started = std::time::Instant::now();
        let out = StdProcessRunner.run(&s).expect("run");
        assert!(out.timed_out);
        assert!(!out.success());
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "kill was not prompt: {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn output_cap_kills_and_truncates() {
        // `yes` produces unbounded output; the cap must stop it long before
        // the timeout.
        let mut s = spec("/usr/bin/yes", &[]);
        s.timeout = Duration::from_secs(30);
        s.max_output_bytes = 64 * 1024;
        let started = std::time::Instant::now();
        let out = StdProcessRunner.run(&s).expect("run");
        assert!(out.truncated);
        assert!(!out.timed_out);
        assert!(out.stdout.len() as u64 <= s.max_output_bytes);
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "cap kill was not prompt: {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn spawn_failure_is_a_named_mechanism_error() {
        let err = StdProcessRunner
            .run(&spec("/nonexistent/fmn-no-such-program", &[]))
            .unwrap_err();
        assert!(err.to_string().contains("fmn-no-such-program"));
    }
}

#[test]
fn relative_program_paths_are_refused_by_contract() {
    // Both runners enforce it — PATH resolution is unreachable through the
    // capability (D2: the boundary resolves and fingerprints its one tool).
    let s = spec("echo", &["hi"]);
    let err = ScriptedRunner::new().run(&s).unwrap_err();
    assert!(err.to_string().contains("not absolute"), "{err}");
    #[cfg(unix)]
    {
        use fmn_platform::process::StdProcessRunner;
        assert!(StdProcessRunner.run(&s).is_err());
    }
}

#[test]
fn scripted_runner_replays_and_logs() {
    let mut r = ScriptedRunner::new();
    r.script(
        "/fake/ffmpeg",
        ProcessOutcome {
            code: Some(0),
            stdout: b"frame=  1".to_vec(),
            stderr: Vec::new(),
            timed_out: false,
            truncated: false,
        },
    );
    let s = spec("/fake/ffmpeg", &["-i", "-", "out.mp4"]);
    let out = r.run(&s).expect("scripted");
    assert!(out.success());
    assert_eq!(out.stdout, b"frame=  1");
    assert!(r.run(&spec("/fake/other", &[])).is_err());
    let runs = r.runs();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0].argv, vec!["-i", "-", "out.mp4"]);
}
