#![forbid(unsafe_code)]

use std::process::{Command, Output};

use fmn_cli::PYTHON_SOURCE_PORTAL_MESSAGE;

fn run_clean(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fmn"))
        .args(args)
        .env_remove("PYTHONHOME")
        .env_remove("PYTHONPATH")
        .env("PATH", "")
        .output()
        .expect("launch the standalone fmn binary by absolute path")
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
fn native_source_reaches_the_real_composition_gap() {
    let output = run_clean(&["--robot", "missing-scene.fmn", "Demo"]);
    let stdout = String::from_utf8(output.stdout).expect("robot output is UTF-8");

    assert_eq!(output.status.code(), Some(4));
    assert!(output.stderr.is_empty());
    assert!(stdout.contains("render composition is unavailable"));
    assert!(!stdout.contains(PYTHON_SOURCE_PORTAL_MESSAGE));
}
