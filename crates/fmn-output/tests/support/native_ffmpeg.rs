//! Native fake-ffmpeg executable for the real process-boundary tests.
//!
//! This target is absent from ordinary product builds. Cargo builds it only
//! under the explicit `ffmpeg-test-fixture` feature, allowing the sandbox
//! suite to exercise a real host-native image without a shell or runtime
//! compiler.
#![forbid(unsafe_code)]

use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;

const VERSION: &str = "ffmpeg version 7.1-fake";
const MODE_LEAF: &str = ".fmn-native-ffmpeg-mode";

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("native ffmpeg fixture: {error}");
            ExitCode::from(70)
        }
    }
}

fn run() -> Result<u8, Box<dyn std::error::Error>> {
    let argv: Vec<OsString> = std::env::args_os().skip(1).collect();
    if argv == [OsString::from("--fmn-sleep-child")] {
        std::thread::sleep(Duration::from_secs(5));
        return Ok(0);
    }
    if argv == [OsString::from("-version")] {
        println!("{VERSION}");
        return Ok(0);
    }
    if argv == [OsString::from("-hide_banner"), OsString::from("-encoders")] {
        println!(
            "Encoders:\n V..... = Video\n ------\n V....D libx264 fake\n V....D h264_nvenc fake"
        );
        return Ok(0);
    }

    let private_dir = std::env::var_os("TMPDIR")
        .map(PathBuf::from)
        .ok_or("ffmpeg fixture invocation has no TMPDIR")?;
    if !private_dir.is_absolute() {
        return Err("ffmpeg fixture TMPDIR is not absolute".into());
    }
    match fixture_mode(&private_dir)?.as_deref() {
        Some("timeout") => {
            let mut child = Command::new(std::env::current_exe()?)
                .arg("--fmn-sleep-child")
                .spawn()?;
            let _status = child.wait()?;
            Ok(0)
        }
        Some("oversize") => {
            drain_stdin()?;
            let artifact = artifact_path(&argv)?;
            std::fs::write(&artifact, vec![0_u8; 4_096])?;
            Ok(0)
        }
        Some("fail7") => {
            drain_stdin()?;
            eprintln!("boom");
            Ok(7)
        }
        Some(mode) => Err(format!("unknown fixture mode {mode:?}").into()),
        None => {
            drain_stdin()?;
            let artifact = artifact_path(&argv)?;
            append_argv(&private_dir, &argv)?;
            write_environment(&artifact)?;
            std::fs::write(artifact, b"FAKEVIDEO")?;
            Ok(0)
        }
    }
}

fn fixture_mode(private_dir: &Path) -> Result<Option<String>, Box<dyn std::error::Error>> {
    for ancestor in private_dir.ancestors() {
        let marker = ancestor.join(MODE_LEAF);
        match std::fs::read_to_string(&marker) {
            Ok(mode) => return Ok(Some(mode.trim().to_owned())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(None)
}

fn drain_stdin() -> Result<(), std::io::Error> {
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        if std::io::stdin().read(&mut buffer)? == 0 {
            return Ok(());
        }
    }
}

fn artifact_path(argv: &[OsString]) -> Result<PathBuf, Box<dyn std::error::Error>> {
    argv.last()
        .map(PathBuf::from)
        .ok_or_else(|| "ffmpeg fixture invocation has no artifact argument".into())
}

fn append_argv(private_dir: &Path, argv: &[OsString]) -> Result<(), Box<dyn std::error::Error>> {
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(private_dir.join("argv.log"))?;
    let line = argv
        .iter()
        .map(|value| value.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");
    writeln!(log, "{line}")?;
    Ok(())
}

fn write_environment(artifact: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut entries: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    entries.sort();
    let mut bytes = Vec::new();
    for (key, value) in entries {
        bytes.extend_from_slice(key.to_string_lossy().as_bytes());
        bytes.push(b'=');
        bytes.extend_from_slice(value.to_string_lossy().as_bytes());
        bytes.push(b'\n');
    }
    let mut path = artifact.as_os_str().to_os_string();
    path.push(".envdump");
    std::fs::write(PathBuf::from(path), bytes)?;
    Ok(())
}
