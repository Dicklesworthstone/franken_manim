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
const TIMEOUT_READY_LEAF: &str = ".fmn-native-ffmpeg-timeout-ready";
const OUTPUT_READY_LEAF: &str = ".fmn-native-ffmpeg-output-ready";

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
    if argv.len() == 3 && argv[0] == "--fmn-grow-file" {
        grow_file(Path::new(&argv[1]), Path::new(&argv[2]))?;
        return Ok(0);
    }
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
    let mode = fixture_mode(&private_dir)?;
    if argv.len() == 3 && argv[0] == "-hide_banner" && argv[1] == "-h" {
        match mode.as_ref().map(|(mode, _)| mode.as_str()) {
            Some("help-exit") => {
                std::io::stdout().write_all(b"partial help\0")?;
                std::io::stderr().write_all(b"driver diagnostic\xff\n")?;
                return Ok(23);
            }
            Some("help-timeout") => {
                std::io::stderr().write_all(b"help is blocked\n")?;
                std::thread::sleep(Duration::from_secs(5));
                return Ok(0);
            }
            Some("help-output-overflow") => {
                std::io::stderr().write_all(&[b'x'; 65_537])?;
                return Ok(0);
            }
            _ => {}
        }
        let encoder = argv[2]
            .to_str()
            .and_then(|help| help.strip_prefix("encoder="))
            .ok_or("expected a named encoder help query")?;
        let formats = match encoder {
            "libx264" => "yuv420p nv12 yuv420p10le",
            "h264_nvenc" => "yuv420p nv12 p010le",
            _ => return Err("fixture help queried an unlisted encoder".into()),
        };
        println!("Encoder {encoder} [Fixture encoder]:\n    Supported pixel formats: {formats}");
        return Ok(0);
    }

    if argv
        .windows(2)
        .any(|pair| pair[0] == "-f" && pair[1] == "null")
    {
        if mode
            .as_ref()
            .is_some_and(|(mode, _)| mode == "hardware-unavailable")
        {
            // Fail before reading stdin, as FFmpeg does when driver loading
            // fails. The real runner must retain this diagnostic even when
            // its larger-than-pipe-capacity input encounters a broken pipe.
            eprintln!("Cannot load libcuda.so.1");
            return Ok(1);
        }
        validate_hardware_probe(&argv)?;
        return Ok(0);
    }
    let Some((mode, fixture_root)) = mode else {
        drain_stdin()?;
        let artifact = artifact_path(&argv)?;
        append_argv(&private_dir, &argv)?;
        write_environment(&artifact)?;
        std::fs::write(artifact, b"FAKEVIDEO")?;
        return Ok(0);
    };
    match mode.as_str() {
        "mjpeg-preview"
        | "mjpeg-preview-corrupt"
        | "mjpeg-preview-truncated"
        | "mjpeg-preview-swap"
        | "mjpeg-preview-extra" => {
            for (flag, value) in [
                ("-protocol_whitelist", "file"),
                ("-f", "rawvideo"),
                ("-pixel_format", "rgba"),
                ("-framerate", "1"),
                ("-map", "0:v:0"),
                ("-frames:v", "1"),
                ("-c:v", "mjpeg"),
                ("-q:v", "3"),
                ("-pix_fmt", "yuvj444p"),
                ("-f", "mpjpeg"),
                ("-boundary_tag", "fmn-preview"),
            ] {
                if !argv
                    .windows(2)
                    .any(|pair| pair[0] == flag && pair[1] == value)
                {
                    return Err(format!("MJPEG preview omitted {flag} {value}").into());
                }
            }
            let value = |flag: &str| -> Result<&OsString, Box<dyn std::error::Error>> {
                let at = argv
                    .iter()
                    .position(|arg| arg == flag)
                    .ok_or("missing preview argument")?;
                argv.get(at + 1)
                    .ok_or_else(|| "missing preview value".into())
            };
            let source = Path::new(value("-i")?);
            if source != private_dir.join("preview.rgba") {
                return Err("MJPEG preview reopened an authored input path".into());
            }
            let size = value("-video_size")?
                .to_str()
                .ok_or("non-UTF8 preview dimensions")?;
            let (width, height) = size.split_once('x').ok_or("invalid preview dimensions")?;
            let width: usize = width.parse()?;
            let height: usize = height.parse()?;
            let expected = width
                .checked_mul(height)
                .and_then(|pixels| pixels.checked_mul(4))
                .filter(|&bytes| bytes > 0 && bytes <= 64 * 48 * 4)
                .ok_or("preview fixture pixel budget")?;
            let source_bytes = std::fs::read(source)?;
            if source_bytes.len() != expected
                || source_bytes
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|pixel| pixel[3] != 255)
            {
                return Err("MJPEG preview input is not the opaque RGBA snapshot".into());
            }
            if value("-fs")?
                .to_str()
                .ok_or("non-UTF8 preview budget")?
                .parse::<u64>()?
                == 0
            {
                return Err("zero MJPEG artifact ceiling".into());
            }
            if mode == "mjpeg-preview-swap" {
                let mut changed = source_bytes;
                changed[0] ^= 1;
                std::fs::write(source, changed)?;
            }
            let jpeg: &[u8] = if mode == "mjpeg-preview-corrupt" {
                b"not JPEG"
            } else {
                include_bytes!("../../../fmn-codec/tests/fixtures/jpeg/baseline_444.jpg")
            };
            let mut multipart = format!(
                "--fmn-preview\r\nContent-type: image/jpeg\r\nContent-length: {}\r\n\r\n",
                jpeg.len()
            )
            .into_bytes();
            multipart.extend_from_slice(jpeg);
            multipart.extend_from_slice(b"\r\n--fmn-preview\r\n");
            if mode == "mjpeg-preview-truncated" {
                let _ = multipart.pop();
            } else if mode == "mjpeg-preview-extra" {
                multipart.extend_from_within(..);
            }
            let artifact = artifact_path(&argv)?;
            if artifact != private_dir.join("preview.mjpeg") {
                return Err("MJPEG preview output escaped its private job".into());
            }
            std::fs::write(artifact, multipart)?;
            Ok(0)
        }
        "image-decode" | "image-decode-swap" | "image-decode-corrupt" => {
            let source_index = argv
                .iter()
                .position(|arg| arg == "-i")
                .ok_or("image decode has no source")?;
            let source = Path::new(&argv[source_index + 1]);
            if source.file_name().is_none_or(|name| name != "source.image") {
                return Err("image decode reopened an authored path".into());
            }
            let expected = [
                ("-protocol_whitelist", "file"),
                ("-f", "bmp_pipe"),
                ("-frames:v", "1"),
                ("-pix_fmt", "rgba"),
                ("-map", "0:v:0"),
            ];
            for (name, value) in expected {
                if !argv
                    .windows(2)
                    .any(|pair| pair[0] == name && pair[1] == value)
                {
                    return Err(format!("image decode omitted {name} {value}").into());
                }
            }
            let bytes = std::fs::read(source)?;
            if !bytes.starts_with(b"BM") {
                return Err("image decode did not stage the actual source bytes".into());
            }
            if mode == "image-decode-swap" {
                std::fs::write(source, b"BM substituted")?;
            }
            let artifact = artifact_path(&argv)?;
            let png = if mode == "image-decode-corrupt" {
                b"not PNG".to_vec()
            } else {
                fmn_codec::encode_rgba8(
                    2,
                    2,
                    &[
                        255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 64, 255, 255, 0, 0,
                    ],
                    fmn_codec::CompressionLevel::Fast,
                )
            };
            std::fs::write(artifact, png)?;
            Ok(0)
        }
        "timeout" => {
            let mut child = Command::new(std::env::current_exe()?)
                .arg("--fmn-sleep-child")
                .spawn()?;
            std::fs::write(
                fixture_root.join(TIMEOUT_READY_LEAF),
                child.id().to_string(),
            )?;
            let _status = child.wait()?;
            Ok(0)
        }
        "oversize" | "bounded-output" => {
            drain_stdin()?;
            let artifact = artifact_path(&argv)?;
            std::fs::write(&artifact, vec![0_u8; 4_096])?;
            Ok(0)
        }
        "runaway-output" | "runaway-scratch" | "runaway-mux" => {
            let artifact = artifact_path(&argv)?;
            if mode == "runaway-mux" && artifact.file_name().is_none_or(|name| name != "muxed.mp4")
            {
                drain_stdin()?;
                std::fs::write(&artifact, b"FAKEVIDEO")?;
                return Ok(0);
            }
            let growing = if mode == "runaway-scratch" {
                std::fs::write(&artifact, b"FAKEVIDEO")?;
                let scratch = private_dir.join("scratch");
                std::fs::create_dir(&scratch)?;
                scratch.join("growing.tmp")
            } else {
                artifact
            };
            // This parent deliberately keeps stdin unread. The descendant
            // inherits its process group and pipes, exercising cancellation
            // of both a blocked writer and the complete external process tree.
            let mut child = Command::new(std::env::current_exe()?)
                .arg("--fmn-grow-file")
                .arg(&growing)
                .arg(fixture_root.join(OUTPUT_READY_LEAF))
                .spawn()?;
            let _status = child.wait()?;
            Ok(0)
        }
        "fail7" => {
            drain_stdin()?;
            eprintln!("boom");
            Ok(7)
        }
        mode => Err(format!("unknown fixture mode {mode:?}").into()),
    }
}

fn grow_file(path: &Path, ready: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut output = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)?;
    // Signal before crossing the limit, so an immediate watcher cancellation
    // cannot race away the test's post-spawn timing marker.
    std::fs::write(ready, format!("{}\n{}", std::process::id(), path.display()))?;
    loop {
        output.write_all(&[0_u8; 4_096])?;
        output.flush()?;
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn fixture_mode(
    private_dir: &Path,
) -> Result<Option<(String, PathBuf)>, Box<dyn std::error::Error>> {
    for ancestor in private_dir.ancestors() {
        let marker = ancestor.join(MODE_LEAF);
        match std::fs::read_to_string(&marker) {
            Ok(mode) => return Ok(Some((mode.trim().to_owned(), ancestor.to_path_buf()))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(None)
}

fn validate_hardware_probe(argv: &[OsString]) -> Result<(), Box<dyn std::error::Error>> {
    for (flag, value) in [
        ("-f", "rawvideo"),
        ("-pix_fmt", "yuv420p"),
        ("-video_size", "256x256"),
        ("-framerate", "1"),
        ("-i", "-"),
        ("-frames:v", "2"),
        ("-c:v", "h264_nvenc"),
    ] {
        if !argv
            .windows(2)
            .any(|pair| pair[0] == flag && pair[1] == value)
        {
            return Err(format!("hardware probe is missing {flag} {value}").into());
        }
    }
    if argv.last().is_none_or(|value| value != "-") {
        return Err("hardware probe must use stdout as its null output".into());
    }

    const LUMA_BYTES: usize = 256 * 256;
    const FRAME_BYTES: usize = LUMA_BYTES * 3 / 2;
    const INPUT_BYTES: usize = 2 * FRAME_BYTES;
    // One extra byte detects excess input while keeping the fixture's own
    // allocation and read bounded, including if the trial contract regresses.
    let mut payload = Vec::new();
    std::io::stdin()
        .take((INPUT_BYTES + 1) as u64)
        .read_to_end(&mut payload)?;
    if payload.len() != INPUT_BYTES {
        return Err(format!(
            "hardware probe expected {INPUT_BYTES} bytes, received {}",
            payload.len()
        )
        .into());
    }
    for frame in payload.as_chunks::<FRAME_BYTES>().0 {
        if !frame[..LUMA_BYTES].iter().all(|byte| *byte == 16)
            || !frame[LUMA_BYTES..].iter().all(|byte| *byte == 128)
        {
            return Err("hardware probe input is not limited-range black YUV420P".into());
        }
    }
    // A null-muxer trial never creates an artifact or an environment dump.
    Ok(())
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
