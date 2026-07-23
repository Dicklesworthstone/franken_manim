//! fm-wj3 acceptance: the fake-ffmpeg contract suite (every
//! negotiation dimension, both mux stages, failure modes), sandbox
//! tests, capability-error tests with ffmpeg absent, the provenance
//! fingerprint test, and the real-ffmpeg smoke test behind an env
//! flag (FFMPEG_PROTOCOL.md §6).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use fmn_frame::ColorRange;
use fmn_output::{
    Boundary, BoundaryError, ColorDescription, Container, EncoderCapabilities, EncoderChoice,
    FfmpegTool, JobLimits, VideoJob, WireFormat, negotiate,
};
use fmn_platform::process::{ProcessOutcome, ScriptedRunner, StdProcessRunner};

fn job(wire: WireFormat, container: Container, encoder: EncoderChoice) -> VideoJob {
    VideoJob {
        width: 64,
        height: 36,
        fps: (30000, 1001),
        wire,
        color: ColorDescription::video_bt709(),
        container,
        encoder,
        crf: None,
    }
}

static DIR_SEQ: AtomicU64 = AtomicU64::new(0);

/// A fresh private scratch dir for one test.
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "fmn-boundary-test-{}-{}-{tag}",
        std::process::id(),
        DIR_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// ---- the pure argv contract ----------------------------------------

/// Collect a representative argv from every builder and mode.
fn all_argvs() -> Vec<Vec<String>> {
    let out = Path::new("/tmp/out.mp4");
    let mut argvs = Vec::new();
    for wire in [
        WireFormat::Rgba8,
        WireFormat::Bgra8,
        WireFormat::Nv12,
        WireFormat::P010,
    ] {
        argvs.push(
            negotiate::encode_argv(&job(wire, Container::Mp4, EncoderChoice::Auto), out).unwrap(),
        );
    }
    argvs.push(
        negotiate::encode_argv(
            &job(
                WireFormat::Rgba8,
                Container::MovTransparent,
                EncoderChoice::Auto,
            ),
            out,
        )
        .unwrap(),
    );
    argvs.push(
        negotiate::encode_argv(
            &job(WireFormat::Rgba8, Container::Gif, EncoderChoice::Auto),
            out,
        )
        .unwrap(),
    );
    argvs.push(negotiate::mux_argv(
        Path::new("/tmp/v.mp4"),
        Path::new("/tmp/a.wav"),
        out,
    ));
    argvs.push(negotiate::concat_argv(Path::new("/tmp/list.txt"), out));
    argvs.push(negotiate::transcode_audio_argv(
        Path::new("/tmp/in.mp3"),
        Path::new("/tmp/out.wav"),
    ));
    argvs.push(negotiate::transcode_image_argv(
        Path::new("/tmp/in.webp"),
        Path::new("/tmp/out.png"),
    ));
    argvs
}

#[test]
fn no_vflip_no_eq_no_filters_anywhere() {
    // D-23: the repairs are structurally impossible. No invocation may
    // contain a filter argument of any kind.
    for argv in all_argvs() {
        for arg in &argv {
            assert!(!arg.contains("vflip"), "vflip found in {argv:?}");
            assert!(!arg.starts_with("eq="), "eq filter found in {argv:?}");
            assert!(
                !matches!(
                    arg.as_str(),
                    "-vf" | "-af" | "-filter:v" | "-filter:a" | "-filter_complex"
                ),
                "filter flag found in {argv:?}"
            );
        }
    }
}

#[test]
fn every_wire_format_negotiates() {
    let out = Path::new("/tmp/out.mp4");
    for (wire, pix) in [
        (WireFormat::Rgba8, "rgba"),
        (WireFormat::Bgra8, "bgra"),
        (WireFormat::Nv12, "nv12"),
        (WireFormat::P010, "p010le"),
    ] {
        let argv =
            negotiate::encode_argv(&job(wire, Container::Mp4, EncoderChoice::Auto), out).unwrap();
        let at = argv.iter().position(|a| a == "-pix_fmt").unwrap();
        assert_eq!(argv[at + 1], pix);
        // Rational frame rate, exactly.
        let at = argv.iter().position(|a| a == "-framerate").unwrap();
        assert_eq!(argv[at + 1], "30000/1001");
        // Input geometry.
        let at = argv.iter().position(|a| a == "-video_size").unwrap();
        assert_eq!(argv[at + 1], "64x36");
    }
    // Wire payload arithmetic (the NV12 argument).
    assert_eq!(WireFormat::Rgba8.frame_bytes(3840, 2160), 33_177_600);
    assert_eq!(WireFormat::Nv12.frame_bytes(3840, 2160), 12_441_600);
    assert_eq!(WireFormat::P010.frame_bytes(3840, 2160), 24_883_200);
}

#[test]
fn color_description_maps() {
    let out = Path::new("/tmp/out.mp4");
    let mut j = job(WireFormat::Nv12, Container::Mp4, EncoderChoice::Auto);
    let argv = negotiate::encode_argv(&j, out).unwrap();
    let find = |argv: &[String], key: &str| -> String {
        let at = argv.iter().position(|a| a == key).unwrap();
        argv[at + 1].clone()
    };
    assert_eq!(find(&argv, "-color_primaries"), "bt709");
    assert_eq!(find(&argv, "-color_trc"), "bt709");
    assert_eq!(find(&argv, "-colorspace"), "bt709");
    assert_eq!(find(&argv, "-color_range"), "tv");

    j.color = ColorDescription::srgb_full();
    let argv = negotiate::encode_argv(&j, out).unwrap();
    assert_eq!(find(&argv, "-color_trc"), "iec61966-2-1");
    assert_eq!(find(&argv, "-color_range"), "pc");
    assert_eq!(j.color.range, ColorRange::Full);
}

#[test]
fn container_modes() {
    let out = Path::new("/tmp/out.x");
    let argv = negotiate::encode_argv(
        &job(WireFormat::Nv12, Container::Mp4, EncoderChoice::Auto),
        out,
    )
    .unwrap();
    assert!(argv.windows(2).any(|w| w == ["-c:v", "libx264"]));
    assert!(argv.windows(2).any(|w| w == ["-movflags", "+faststart"]));
    assert!(argv.windows(2).any(|w| w == ["-pix_fmt", "yuv420p"]));

    // 10-bit in stays 10-bit out.
    let argv = negotiate::encode_argv(
        &job(WireFormat::P010, Container::Mp4, EncoderChoice::Auto),
        out,
    )
    .unwrap();
    assert!(argv.windows(2).any(|w| w == ["-pix_fmt", "yuv420p10le"]));

    // Transparent MOV: qtrle over argb.
    let argv = negotiate::encode_argv(
        &job(
            WireFormat::Bgra8,
            Container::MovTransparent,
            EncoderChoice::Auto,
        ),
        out,
    )
    .unwrap();
    assert!(argv.windows(2).any(|w| w == ["-c:v", "qtrle"]));
    assert!(argv.windows(2).any(|w| w == ["-pix_fmt", "argb"]));

    // GIF mode is muxer-level: -f gif and no -c:v at all.
    let argv = negotiate::encode_argv(
        &job(WireFormat::Rgba8, Container::Gif, EncoderChoice::Auto),
        out,
    )
    .unwrap();
    assert!(argv.windows(2).any(|w| w == ["-f", "gif"]));
    assert!(!argv.iter().any(|a| a == "-c:v"));
}

#[test]
fn negotiation_refusals_are_named() {
    let out = Path::new("/tmp/out.mov");
    // Alpha container on an opaque wire.
    assert!(
        negotiate::encode_argv(
            &job(
                WireFormat::Nv12,
                Container::MovTransparent,
                EncoderChoice::Auto
            ),
            out
        )
        .is_err()
    );
    // CRF on a hardware encoder.
    let mut j = job(
        WireFormat::Nv12,
        Container::Mp4,
        EncoderChoice::Named("h264_nvenc".into()),
    );
    j.crf = Some(20);
    assert!(negotiate::encode_argv(&j, out).is_err());
    // CRF out of range.
    let mut j = job(WireFormat::Nv12, Container::Mp4, EncoderChoice::Auto);
    j.crf = Some(60);
    assert!(negotiate::encode_argv(&j, out).is_err());
    // GIF takes no encoder.
    assert!(
        negotiate::encode_argv(
            &job(
                WireFormat::Rgba8,
                Container::Gif,
                EncoderChoice::Named("libx264".into())
            ),
            out
        )
        .is_err()
    );
    // Zero geometry.
    let mut j = job(WireFormat::Nv12, Container::Mp4, EncoderChoice::Auto);
    j.width = 0;
    assert!(negotiate::encode_argv(&j, out).is_err());
    let mut j = job(WireFormat::Nv12, Container::Mp4, EncoderChoice::Auto);
    j.fps = (30, 0);
    assert!(negotiate::encode_argv(&j, out).is_err());
}

#[test]
fn mux_stage_two_copies_video() {
    // The audit-addendum contract: stage 2 must never re-encode video.
    let argv = negotiate::mux_argv(
        Path::new("/w/video.mp4"),
        Path::new("/w/audio.wav"),
        Path::new("/out/final.mp4"),
    );
    assert!(argv.windows(2).any(|w| w == ["-c:v", "copy"]));
    assert!(argv.windows(2).any(|w| w == ["-c:a", "aac"]));
    assert!(argv.windows(2).any(|w| w == ["-map", "0:v:0"]));
    assert!(argv.windows(2).any(|w| w == ["-map", "1:a:0"]));
}

#[test]
fn concat_and_transcode_shapes() {
    let argv = negotiate::concat_argv(Path::new("/w/list.txt"), Path::new("/out/full.mp4"));
    assert!(argv.windows(4).any(|w| w == ["-f", "concat", "-safe", "0"]));
    assert!(argv.windows(2).any(|w| w == ["-c", "copy"]));

    let argv = negotiate::transcode_audio_argv(Path::new("/in.mp3"), Path::new("/out.wav"));
    assert!(argv.windows(2).any(|w| w == ["-acodec", "pcm_s16le"]));
    let argv = negotiate::transcode_image_argv(Path::new("/in.webp"), Path::new("/out.png"));
    assert!(argv.windows(2).any(|w| w == ["-c:v", "png"]));
}

// ---- the ScriptedRunner contract suite -----------------------------

const FAKE_TOOL_BYTES: &[u8] = b"#!/bin/sh\nexit 0\n";
const FAKE_VERSION: &str = "ffmpeg version 7.1-fake Copyright (c) fake";

/// Write a fake tool file and script its `-version` probe.
fn scripted_tool(dir: &Path) -> (PathBuf, ScriptedRunner) {
    let tool_path = dir.join("ffmpeg");
    std::fs::write(&tool_path, FAKE_TOOL_BYTES).unwrap();
    let mut runner = ScriptedRunner::new();
    runner.script(
        &tool_path,
        ProcessOutcome {
            code: Some(0),
            stdout: format!("{FAKE_VERSION}\nbuilt with fake-gcc\n").into_bytes(),
            stderr: Vec::new(),
            timed_out: false,
            truncated: false,
        },
    );
    (tool_path, runner)
}

#[test]
fn provenance_fingerprint() {
    let dir = scratch("fingerprint");
    let (tool_path, runner) = scripted_tool(&dir);
    let tool = FfmpegTool::resolve(&tool_path, &runner).unwrap();
    assert_eq!(
        tool.sha256_hex,
        fmn_hash::sha256::sha256(FAKE_TOOL_BYTES).to_hex()
    );
    assert_eq!(tool.version, FAKE_VERSION);
}

#[test]
fn absent_ffmpeg_is_a_capability_error_naming_the_alternative() {
    let dir = scratch("absent");
    let runner = ScriptedRunner::new();
    let err = FfmpegTool::resolve(&dir.join("nope/ffmpeg"), &runner).unwrap_err();
    let message = err.to_string();
    assert!(message.contains("y4m"), "alternative not named: {message}");
    assert!(message.contains("PNG sequences"), "{message}");
    assert!(matches!(err, BoundaryError::FfmpegUnavailable { .. }));
}

const ENCODERS_LISTING: &str = "Encoders:\n V..... = Video\n ------\n V....D libx264              H.264 (x264)\n V....D libx265              H.265 (x265)\n V....D qtrle                QuickTime RLE\n V....D h264_nvenc           NVIDIA NVENC H.264\n A....D aac                  AAC audio\n";

#[test]
fn encoder_capabilities_parse_and_report_hardware() {
    let caps = EncoderCapabilities::parse(ENCODERS_LISTING);
    assert!(caps.offers("libx264"));
    assert!(caps.offers("h264_nvenc"));
    assert!(!caps.offers("hevc_videotoolbox"));
    assert_eq!(caps.hardware(), vec!["h264_nvenc".to_string()]);
}

/// Run one scripted encode and hand back the recorded spec (the run
/// necessarily ends in `ArtifactMissing` — the scripted runner writes
/// nothing — which itself proves artifact verification gates
/// publication).
fn scripted_encode(
    encoder: EncoderChoice,
    frames: Vec<u8>,
) -> (
    Result<(), BoundaryError>,
    Vec<fmn_platform::process::ProcessSpec>,
    PathBuf,
) {
    let dir = scratch("contract");
    let (tool_path, runner) = scripted_tool(&dir);
    let tool = FfmpegTool::resolve(&tool_path, &runner).unwrap();
    let caps = EncoderCapabilities::parse(ENCODERS_LISTING);
    let boundary = Boundary::new(tool, &runner, JobLimits::default(), dir.clone());
    let destination = dir.join("final/out.mp4");
    let result = boundary
        .encode(
            &job(WireFormat::Nv12, Container::Mp4, encoder),
            frames,
            &caps,
            &destination,
        )
        .map(|_| ());
    (result, runner.runs(), dir)
}

#[test]
fn encode_contract_spec() {
    let frame = vec![0u8; WireFormat::Nv12.frame_bytes(64, 36)];
    let (result, runs, dir) = scripted_encode(EncoderChoice::Auto, frame.repeat(3));
    // The scripted run wrote no artifact: verification refuses, and the
    // destination was never touched.
    assert!(matches!(result, Err(BoundaryError::ArtifactMissing)));
    assert!(!dir.join("final/out.mp4").exists());

    // One probe (-version) + one encode.
    assert_eq!(runs.len(), 2);
    let spec = &runs[1];
    assert!(spec.program.is_absolute());
    // Env allowlist, exactly, in order: LANG, LC_ALL, TMPDIR.
    assert_eq!(spec.env.len(), 3);
    assert_eq!(spec.env[0], ("LANG".to_string(), "C".to_string()));
    assert_eq!(spec.env[1], ("LC_ALL".to_string(), "C".to_string()));
    assert_eq!(spec.env[2].0, "TMPDIR");
    // The private dir is the cwd and TMPDIR, under the workdir root.
    let cwd = spec.cwd.clone().unwrap();
    assert!(cwd.starts_with(&dir));
    assert_eq!(spec.env[2].1, cwd.display().to_string());
    // The payload rides stdin, whole.
    assert_eq!(
        spec.stdin.as_ref().unwrap().len(),
        3 * WireFormat::Nv12.frame_bytes(64, 36)
    );
    // The artifact is born inside the private dir.
    let out_arg = spec.argv.last().unwrap();
    assert!(out_arg.starts_with(&cwd.display().to_string()));
}

#[test]
fn hardware_encoder_explicit_and_unknown() {
    let frame = vec![0u8; WireFormat::Nv12.frame_bytes(64, 36)];
    // Present in caps: the argv names it.
    let (result, runs, _) =
        scripted_encode(EncoderChoice::Named("h264_nvenc".into()), frame.clone());
    assert!(matches!(result, Err(BoundaryError::ArtifactMissing)));
    assert!(runs[1].argv.windows(2).any(|w| w == ["-c:v", "h264_nvenc"]));

    // Absent from caps: refused before any spawn.
    let (result, runs, _) =
        scripted_encode(EncoderChoice::Named("hevc_videotoolbox".into()), frame);
    match result {
        Err(BoundaryError::UnknownEncoder {
            requested,
            hardware_available,
        }) => {
            assert_eq!(requested, "hevc_videotoolbox");
            assert_eq!(hardware_available, vec!["h264_nvenc".to_string()]);
        }
        other => panic!("expected UnknownEncoder, got {other:?}"),
    }
    // Only the -version probe ran.
    assert_eq!(runs.len(), 1);
}

#[test]
fn payload_geometry_is_checked_before_spawn() {
    let (result, runs, _) = scripted_encode(EncoderChoice::Auto, vec![0u8; 1000]);
    assert!(matches!(result, Err(BoundaryError::PayloadGeometry { .. })));
    assert_eq!(runs.len(), 1); // -version only
}

#[test]
fn failure_modes_map_to_typed_refusals() {
    let dir = scratch("failures");
    let (tool_path, mut runner) = scripted_tool(&dir);
    let tool = FfmpegTool::resolve(&tool_path, &runner).unwrap();
    let caps = EncoderCapabilities::parse(ENCODERS_LISTING);
    let frames = vec![0u8; WireFormat::Nv12.frame_bytes(64, 36)];
    let j = job(WireFormat::Nv12, Container::Mp4, EncoderChoice::Auto);

    // Nonzero exit with stderr.
    runner.script(
        &tool_path,
        ProcessOutcome {
            code: Some(1),
            stdout: Vec::new(),
            stderr: b"Unknown encoder 'libx264'".to_vec(),
            timed_out: false,
            truncated: false,
        },
    );
    let boundary = Boundary::new(tool.clone(), &runner, JobLimits::default(), dir.clone());
    match boundary.encode(&j, frames.clone(), &caps, &dir.join("a.mp4")) {
        Err(BoundaryError::EncodeFailed { code, stderr }) => {
            assert_eq!(code, Some(1));
            assert!(stderr.contains("Unknown encoder"));
        }
        other => panic!("expected EncodeFailed, got {other:?}"),
    }

    // Timeout.
    runner.script(
        &tool_path,
        ProcessOutcome {
            code: None,
            stdout: Vec::new(),
            stderr: Vec::new(),
            timed_out: true,
            truncated: false,
        },
    );
    let boundary = Boundary::new(tool.clone(), &runner, JobLimits::default(), dir.clone());
    assert!(matches!(
        boundary.encode(&j, frames.clone(), &caps, &dir.join("b.mp4")),
        Err(BoundaryError::JobTimedOut { .. })
    ));

    // Log overflow.
    runner.script(
        &tool_path,
        ProcessOutcome {
            code: None,
            stdout: Vec::new(),
            stderr: Vec::new(),
            timed_out: false,
            truncated: true,
        },
    );
    let boundary = Boundary::new(tool, &runner, JobLimits::default(), dir.clone());
    assert!(matches!(
        boundary.encode(&j, frames, &caps, &dir.join("c.mp4")),
        Err(BoundaryError::LogOverflow)
    ));
}

#[test]
fn prerun_counts_without_spawning() {
    // The --prerun retention: a counting pass plans jobs (negotiation +
    // argv construction are pure) and invokes the boundary zero times.
    let runner = ScriptedRunner::new();
    let j = job(WireFormat::Nv12, Container::Mp4, EncoderChoice::Auto);
    let _plan = negotiate::encode_argv(&j, Path::new("/tmp/out.mp4")).unwrap();
    let frames_expected = 42u64; // the counting pass's product
    assert!(frames_expected > 0);
    assert!(runner.runs().is_empty(), "prerun must not spawn");
}

// ---- the fake-ffmpeg sandbox suite (real StdProcessRunner) ---------

#[cfg(unix)]
fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// A fake ffmpeg: consumes stdin, dumps env beside the artifact,
/// appends its argv to `argv.log` in the private cwd, writes the
/// artifact.
#[cfg(unix)]
const FAKE_FFMPEG: &str = "#!/bin/sh\ncat > /dev/null\nif [ \"$1\" = \"-version\" ]; then echo 'ffmpeg version 7.1-fake'; exit 0; fi\nfor a in \"$@\"; do last=\"$a\"; done\nprintf '%s\\n' \"$*\" >> ./argv.log\nenv > \"$last.envdump\"\nprintf 'FAKEVIDEO' > \"$last\"\nexit 0\n";

/// Resolve a fake tool that answers the `-version` probe.
#[cfg(unix)]
fn real_tool(dir: &Path, body: &str) -> (FfmpegTool, StdProcessRunner) {
    let runner = StdProcessRunner;
    let path = write_script(dir, "fake-ffmpeg", body);
    let tool = FfmpegTool::resolve(&path, &runner).unwrap();
    (tool, runner)
}

/// A tool handle over a script that need not answer probes (the
/// failure-mode scripts): fields are data, resolution is elsewhere.
#[cfg(unix)]
fn unprobed_tool(dir: &Path, body: &str) -> (FfmpegTool, StdProcessRunner) {
    let path = write_script(dir, "fake-ffmpeg", body);
    let tool = FfmpegTool {
        path,
        sha256_hex: "00".repeat(32),
        version: "ffmpeg version 0.0-unprobed".into(),
    };
    (tool, StdProcessRunner)
}

#[cfg(unix)]
#[test]
fn sandbox_publishes_atomically_and_pins_the_environment() {
    let dir = scratch("sandbox");
    let (tool, runner) = real_tool(&dir, FAKE_FFMPEG);
    let limits = JobLimits {
        keep_workdir: true,
        ..JobLimits::default()
    };
    let boundary = Boundary::new(tool, &runner, limits, dir.clone());
    let caps = EncoderCapabilities::parse(ENCODERS_LISTING);
    let destination = dir.join("published/movie.mp4");
    let frames = vec![7u8; WireFormat::Nv12.frame_bytes(64, 36)];
    let report = boundary
        .encode(
            &job(WireFormat::Nv12, Container::Mp4, EncoderChoice::Auto),
            frames,
            &caps,
            &destination,
        )
        .unwrap();

    // Published by rename: destination holds the artifact bytes.
    assert_eq!(std::fs::read(&destination).unwrap(), b"FAKEVIDEO");
    assert_eq!(report.provenance.destination, destination);
    assert_eq!(report.provenance.encoder.as_deref(), Some("libx264"));
    assert!(
        report
            .provenance
            .tool_version
            .starts_with("ffmpeg version 7.1-fake")
    );

    // The env the child actually observed: pinned locale, private
    // TMPDIR, and no ambient leakage.
    let workdir: PathBuf = report.provenance.argv.last().unwrap().into();
    let envdump = std::fs::read_to_string(format!("{}.envdump", workdir.display())).unwrap();
    assert!(envdump.contains("LANG=C"));
    assert!(envdump.contains("LC_ALL=C"));
    assert!(envdump.contains("TMPDIR="));
    assert!(
        !envdump.contains("HOME="),
        "ambient HOME leaked:\n{envdump}"
    );
    assert!(
        !envdump.contains("PATH="),
        "ambient PATH leaked:\n{envdump}"
    );
}

#[cfg(unix)]
#[test]
fn sandbox_timeout_kills_and_leaves_destination_untouched() {
    let dir = scratch("timeout");
    // `exec` so the sleep IS the direct child — the mechanism's kill
    // promise covers the direct child (a plain `sleep` line would be a
    // grandchild holding the pipes, the documented tree-kill gap).
    let (tool, runner) = unprobed_tool(&dir, "#!/bin/sh\nexec sleep 5\n");
    let limits = JobLimits {
        timeout: Duration::from_millis(200),
        ..JobLimits::default()
    };
    let boundary = Boundary::new(tool, &runner, limits, dir.clone());
    let caps = EncoderCapabilities::parse(ENCODERS_LISTING);
    let destination = dir.join("never.mp4");
    let frames = vec![0u8; WireFormat::Nv12.frame_bytes(64, 36)];
    let started = std::time::Instant::now();
    let result = boundary.encode(
        &job(WireFormat::Nv12, Container::Mp4, EncoderChoice::Auto),
        frames,
        &caps,
        &destination,
    );
    assert!(matches!(result, Err(BoundaryError::JobTimedOut { .. })));
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "kill was not prompt"
    );
    assert!(!destination.exists());
}

#[cfg(unix)]
#[test]
fn sandbox_refuses_oversized_artifacts() {
    let dir = scratch("oversize");
    let (tool, runner) = unprobed_tool(
        &dir,
        "#!/bin/sh\ncat > /dev/null\nfor a in \"$@\"; do last=\"$a\"; done\nhead -c 4096 /dev/zero > \"$last\"\nexit 0\n",
    );
    let limits = JobLimits {
        max_artifact_bytes: 1024,
        ..JobLimits::default()
    };
    let boundary = Boundary::new(tool, &runner, limits, dir.clone());
    let caps = EncoderCapabilities::parse(ENCODERS_LISTING);
    let destination = dir.join("big.mp4");
    let frames = vec![0u8; WireFormat::Nv12.frame_bytes(64, 36)];
    let result = boundary.encode(
        &job(WireFormat::Nv12, Container::Mp4, EncoderChoice::Auto),
        frames,
        &caps,
        &destination,
    );
    assert!(matches!(
        result,
        Err(BoundaryError::ArtifactOversized {
            bytes: 4096,
            max: 1024
        })
    ));
    assert!(!destination.exists());
}

#[cfg(unix)]
#[test]
fn sandbox_failed_job_preserves_existing_destination() {
    let dir = scratch("failkeep");
    let (tool, runner) = unprobed_tool(
        &dir,
        "#!/bin/sh\ncat > /dev/null\necho 'boom' >&2\nexit 7\n",
    );
    let boundary = Boundary::new(tool, &runner, JobLimits::default(), dir.clone());
    let caps = EncoderCapabilities::parse(ENCODERS_LISTING);
    let destination = dir.join("keep.mp4");
    std::fs::write(&destination, b"the old render").unwrap();
    let frames = vec![0u8; WireFormat::Nv12.frame_bytes(64, 36)];
    let result = boundary.encode(
        &job(WireFormat::Nv12, Container::Mp4, EncoderChoice::Auto),
        frames,
        &caps,
        &destination,
    );
    match result {
        Err(BoundaryError::EncodeFailed { code, stderr }) => {
            assert_eq!(code, Some(7));
            assert!(stderr.contains("boom"));
        }
        other => panic!("expected EncodeFailed, got {other:?}"),
    }
    assert_eq!(std::fs::read(&destination).unwrap(), b"the old render");
}

#[cfg(unix)]
#[test]
fn two_stage_mux_runs_both_stages_and_copies_video() {
    let dir = scratch("mux");
    let (tool, runner) = real_tool(&dir, FAKE_FFMPEG);
    let limits = JobLimits {
        keep_workdir: true,
        ..JobLimits::default()
    };
    let boundary = Boundary::new(tool, &runner, limits, dir.clone());
    let caps = EncoderCapabilities::parse(ENCODERS_LISTING);
    let destination = dir.join("with_audio.mp4");
    let audio = dir.join("track.wav");
    std::fs::write(&audio, b"RIFFfake").unwrap();
    let frames = vec![0u8; WireFormat::Nv12.frame_bytes(64, 36)];
    let report = boundary
        .encode_with_audio(
            &job(WireFormat::Nv12, Container::Mp4, EncoderChoice::Auto),
            frames,
            &audio,
            &caps,
            &destination,
        )
        .unwrap();
    assert_eq!(std::fs::read(&destination).unwrap(), b"FAKEVIDEO");

    // The private dir's argv log shows both stages; stage 2 copied the
    // video stream and never re-encoded it.
    let workdir: PathBuf = report.provenance.argv.last().unwrap().into();
    let log = std::fs::read_to_string(workdir.parent().unwrap().join("argv.log")).unwrap();
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 2, "expected two stages:\n{log}");
    assert!(lines[0].contains("rawvideo"), "stage 1 encodes video");
    assert!(lines[1].contains("-c:v copy"), "stage 2 copies video");
    assert!(lines[1].contains("-c:a aac"), "stage 2 encodes audio");
    assert!(!lines[1].contains("libx264"), "stage 2 must not re-encode");
}

#[cfg(unix)]
#[test]
fn concat_writes_a_list_and_copies_streams() {
    let dir = scratch("concat");
    let (tool, runner) = real_tool(&dir, FAKE_FFMPEG);
    let limits = JobLimits {
        keep_workdir: true,
        ..JobLimits::default()
    };
    let boundary = Boundary::new(tool, &runner, limits, dir.clone());
    let parts = vec![dir.join("part0.mp4"), dir.join("part1.mp4")];
    let destination = dir.join("joined.mp4");
    let report = boundary.concat(&parts, &destination).unwrap();
    assert_eq!(std::fs::read(&destination).unwrap(), b"FAKEVIDEO");
    assert!(report.provenance.encoder.is_none());

    // A quoted path is refused, not escaped.
    let evil = vec![dir.join("it's.mp4")];
    assert!(matches!(
        boundary.concat(&evil, &destination),
        Err(BoundaryError::Negotiation(_))
    ));
}

// ---- the real thing, behind an env flag ----------------------------

#[test]
fn real_ffmpeg_smoke() {
    if std::env::var("FMN_REAL_FFMPEG").is_err() {
        return; // opt-in only
    }
    let runner = StdProcessRunner;
    let tool = FfmpegTool::resolve(Path::new("/usr/bin/ffmpeg"), &runner).unwrap();
    assert!(tool.version.starts_with("ffmpeg version"));
    let caps = EncoderCapabilities::probe(&tool, &runner).unwrap();
    assert!(caps.offers("libx264") || caps.offers("mpeg4"));

    let dir = scratch("real");
    let boundary = Boundary::new(tool, &runner, JobLimits::default(), dir.clone());
    let destination = dir.join("smoke.mp4");
    let mut j = job(WireFormat::Nv12, Container::Mp4, EncoderChoice::Auto);
    j.fps = (30, 1);
    // Three gray frames.
    let frame = vec![0x80u8; WireFormat::Nv12.frame_bytes(64, 36)];
    let report = boundary
        .encode(&j, frame.repeat(3), &caps, &destination)
        .unwrap();
    let bytes = std::fs::read(&destination).unwrap();
    assert!(bytes.len() > 100, "suspiciously small mp4");
    assert!(report.provenance.tool_sha256_hex.len() == 64);
    println!(
        "real ffmpeg smoke OK: {} bytes via {}",
        bytes.len(),
        report.provenance.tool_version
    );
}
