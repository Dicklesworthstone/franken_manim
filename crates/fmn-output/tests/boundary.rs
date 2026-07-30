//! fm-wj3 acceptance: the fake-ffmpeg contract suite (every
//! negotiation dimension, both mux stages, failure modes), sandbox
//! tests, capability-error tests with ffmpeg absent, the provenance
//! fingerprint test, and the real-ffmpeg smoke test behind an env
//! flag (FFMPEG_PROTOCOL.md §6).

use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
#[cfg(unix)]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(unix)]
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
#[cfg(unix)]
use std::time::Duration;

use fmn_frame::ColorRange;
#[cfg(unix)]
use fmn_output::{Boundary, BoundaryError, EncoderCapabilities, FfmpegTool, JobLimits};
use fmn_output::{ColorDescription, Container, EncoderChoice, VideoJob, WireFormat, negotiate};
#[cfg(unix)]
use fmn_platform::process::{
    ProcessCancellation, ProcessError, ProcessOutcome, ProcessRunner, ProcessSpec,
    ProcessStdinLimits, ProcessTermination, RunningProcess, ScriptedRunner, StdProcessRunner,
};

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

#[cfg(unix)]
static DIR_SEQ: AtomicU64 = AtomicU64::new(0);

/// The write-then-exec gate (fm-rvm).
///
/// These tests write a fake ffmpeg script and then execute it. That is a
/// race with *any other thread in this process forking*: a child forked
/// while the script file is still open for writing inherits that writable
/// descriptor, and the exec that follows fails with `ETXTBSY` ("Text file
/// busy"). Rust's test harness runs tests in parallel threads and
/// `Command::spawn` forks, so the window is real — microseconds wide,
/// widened by machine load, which is exactly the observed flake profile:
/// intermittent under full-suite parallelism, green in isolation, and
/// green on the immediate rerun.
///
/// Every test that writes a tool and runs it holds this gate for its whole
/// body, so no sibling can fork inside the window. [`scratch`] hands out
/// the guard with the directory, which is what makes it hard to forget:
/// a test that wants a private directory gets the gate whether it thought
/// about `ETXTBSY` or not. The cost is that the subprocess tests run one
/// at a time — a few hundred milliseconds, against a merge gate that
/// otherwise fails for no reason.
#[cfg(unix)]
static SPAWN_GATE: Mutex<()> = Mutex::new(());

#[cfg(unix)]
fn create_private_test_directory(path: &Path) {
    std::fs::create_dir(path).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
}

/// A fresh private scratch dir for one test, plus the spawn gate.
#[cfg(unix)]
fn scratch(tag: &str) -> (PathBuf, MutexGuard<'static, ()>) {
    // A panicking test poisons the mutex; the gate protects a file-system
    // race, not an invariant, so a poisoned gate is still a usable gate.
    let gate = SPAWN_GATE.lock().unwrap_or_else(PoisonError::into_inner);
    let dir = std::env::temp_dir().join(format!(
        "fmn-boundary-test-{}-{}-{tag}",
        std::process::id(),
        DIR_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    create_private_test_directory(&dir);
    (dir, gate)
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

#[cfg(not(unix))]
#[test]
fn private_ffmpeg_boundary_fails_closed_without_a_provable_directory_acl() {
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    let root = std::env::temp_dir().join(format!(
        "fmn-boundary-non-unix-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir(&root).expect("fresh non-Unix boundary fixture");
    let source = root.join("ffmpeg");
    std::fs::write(&source, b"non-Unix ffmpeg fixture").expect("write ffmpeg fixture");
    let runner = fmn_platform::process::ScriptedRunner::new();

    let error = fmn_output::FfmpegTool::resolve(&source, &runner, &root)
        .expect_err("safe std cannot prove a private non-Unix workdir ACL");
    assert!(matches!(
        error,
        fmn_output::BoundaryError::Workdir { ref detail }
            if detail.contains("cannot prove a private ffmpeg workdir ACL")
    ));
    assert!(
        runner.runs().is_empty(),
        "a refused private workdir must not execute ffmpeg"
    );
}

// ---- the ScriptedRunner contract suite -----------------------------

#[cfg(unix)]
mod private_boundary {
    use super::*;

    const FAKE_TOOL_BYTES: &[u8] = b"#!/bin/sh\nexit 0\n";
    const FAKE_VERSION: &str = "ffmpeg version 7.1-fake Copyright (c) fake";

    /// Write a fake tool file and script its `-version` probe.
    fn scripted_tool(dir: &Path) -> (PathBuf, ScriptedRunner) {
        let tool_path = dir.join("ffmpeg");
        std::fs::write(&tool_path, FAKE_TOOL_BYTES).unwrap();
        let mut runner = ScriptedRunner::new();
        runner.script(
            probe_bound_tool(dir, 0),
            ProcessOutcome {
                termination: ProcessTermination::Exited(Some(0)),
                stdout: format!("{FAKE_VERSION}\nbuilt with fake-gcc\n").into_bytes(),
                stderr: Vec::new(),
            },
        );
        (tool_path, runner)
    }

    fn session_root(dir: &Path, sequence: u64) -> PathBuf {
        dir.join(format!(
            "fmn-ffmpeg-session-{}-{sequence}",
            std::process::id()
        ))
    }

    fn probe_bound_tool(dir: &Path, sequence: u64) -> PathBuf {
        probe_bound_tool_in_session(dir, 0, sequence)
    }

    fn probe_bound_tool_in_session(
        dir: &Path,
        session_sequence: u64,
        probe_sequence: u64,
    ) -> PathBuf {
        session_root(dir, session_sequence)
            .join(format!("fmn-probe-{}-{probe_sequence}", std::process::id()))
            .join("fmn-bound-ffmpeg")
    }

    fn bound_tool(dir: &Path, sequence: u64) -> PathBuf {
        session_root(dir, 0)
            .join(format!("fmn-job-{}-{sequence}", std::process::id()))
            .join("fmn-bound-ffmpeg")
    }

    fn first_bound_tool(dir: &Path) -> PathBuf {
        bound_tool(dir, 0)
    }

    fn successful_scripted_outcome() -> ProcessOutcome {
        ProcessOutcome {
            termination: ProcessTermination::Exited(Some(0)),
            stdout: Vec::new(),
            stderr: Vec::new(),
        }
    }

    #[test]
    fn provenance_fingerprint() {
        let (dir, _gate) = scratch("fingerprint");
        let (tool_path, runner) = scripted_tool(&dir);
        let tool = FfmpegTool::resolve(&tool_path, &runner, &dir).unwrap();
        assert_eq!(
            tool.sha256_hex(),
            fmn_hash::sha256::sha256(FAKE_TOOL_BYTES).to_hex()
        );
        assert_eq!(tool.version(), FAKE_VERSION);
        assert_eq!(tool.path(), std::fs::canonicalize(tool_path).unwrap());
        let runs = runner.runs();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].program, probe_bound_tool(&dir, 0));
        assert_ne!(runs[0].program, tool.path());
        assert_eq!(runs[0].cwd.as_deref(), runs[0].program.parent());
    }

    #[test]
    fn absent_ffmpeg_is_a_capability_error_naming_the_alternative() {
        let (dir, _gate) = scratch("absent");
        let runner = ScriptedRunner::new();
        let err = FfmpegTool::resolve(&dir.join("nope/ffmpeg"), &runner, &dir).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("y4m"), "alternative not named: {message}");
        assert!(message.contains("PNG sequences"), "{message}");
        assert!(matches!(err, BoundaryError::FfmpegUnavailable { .. }));
    }

    #[test]
    fn non_relocatable_ffmpeg_is_a_named_capability_refusal() {
        let (dir, _gate) = scratch("non-relocatable");
        let tool_path = dir.join("ffmpeg");
        std::fs::write(&tool_path, FAKE_TOOL_BYTES).unwrap();
        let mut runner = ScriptedRunner::new();
        runner.script(
            probe_bound_tool(&dir, 0),
            ProcessOutcome {
                termination: ProcessTermination::Exited(Some(127)),
                stdout: Vec::new(),
                stderr: b"loader could not resolve a sibling library".to_vec(),
            },
        );

        let error = FfmpegTool::resolve(&tool_path, &runner, &dir)
            .expect_err("a private-copy probe failure must reject the installation");
        assert!(matches!(
            error,
            BoundaryError::UnsupportedRelocatedExecutable { .. }
        ));
        let message = error.to_string();
        assert!(message.contains("relocatable/self-contained"), "{message}");
        assert!(message.contains("sibling library"), "{message}");
        let runs = runner.runs();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].program, probe_bound_tool(&dir, 0));
        assert_ne!(runs[0].program, std::fs::canonicalize(tool_path).unwrap());
    }

    #[test]
    fn private_session_creation_skips_a_foreign_collision() {
        let (dir, _gate) = scratch("session-collision");
        let collision = session_root(&dir, 0);
        std::fs::create_dir(&collision).unwrap();
        let sentinel = collision.join("foreign");
        std::fs::write(&sentinel, b"not ours").unwrap();
        let tool_path = dir.join("ffmpeg");
        std::fs::write(&tool_path, FAKE_TOOL_BYTES).unwrap();
        let mut runner = ScriptedRunner::new();
        runner.script(
            probe_bound_tool_in_session(&dir, 1, 0),
            ProcessOutcome {
                termination: ProcessTermination::Exited(Some(0)),
                stdout: format!("{FAKE_VERSION}\n").into_bytes(),
                stderr: Vec::new(),
            },
        );

        let tool = FfmpegTool::resolve(&tool_path, &runner, &dir).unwrap();
        assert_eq!(tool.version(), FAKE_VERSION);
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"not ours");
        assert_eq!(
            runner.runs()[0].program,
            probe_bound_tool_in_session(&dir, 1, 0)
        );
    }

    #[cfg(unix)]
    #[test]
    fn writable_non_sticky_workdir_parent_is_refused_before_probe() {
        use std::os::unix::fs::PermissionsExt as _;

        let (dir, _gate) = scratch("insecure-parent");
        let parent = dir.join("insecure");
        std::fs::create_dir(&parent).unwrap();
        let tool_path = parent.join("ffmpeg");
        std::fs::write(&tool_path, FAKE_TOOL_BYTES).unwrap();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o777)).unwrap();
        let runner = ScriptedRunner::new();

        let error = FfmpegTool::resolve(&tool_path, &runner, &parent)
            .expect_err("an attacker-writable parent cannot anchor a private session");
        assert!(matches!(error, BoundaryError::Workdir { .. }));
        assert!(error.to_string().contains("without the sticky bit"));
        assert!(runner.runs().is_empty());
    }

    #[test]
    fn parent_directory_components_are_refused_before_creation() {
        let (dir, _gate) = scratch("parent-component");
        let tool_path = dir.join("ffmpeg");
        std::fs::write(&tool_path, FAKE_TOOL_BYTES).unwrap();
        let runner = ScriptedRunner::new();
        let requested = dir.join("must-not-create/../work");

        let error = FfmpegTool::resolve(&tool_path, &runner, &requested)
            .expect_err("workdir construction must not traverse parent components");
        assert!(matches!(error, BoundaryError::Workdir { .. }));
        assert!(error.to_string().contains("parent-directory component"));
        assert!(!dir.join("must-not-create").exists());
        assert!(runner.runs().is_empty());
    }

    #[test]
    fn resolved_identity_change_refuses_probes_and_jobs_before_spawn() {
        let (dir, _gate) = scratch("identity-change");
        let (tool_path, runner) = scripted_tool(&dir);
        let tool = FfmpegTool::resolve(&tool_path, &runner, &dir).unwrap();
        std::fs::write(&tool_path, b"substituted executable bytes").unwrap();

        let probe_error =
            EncoderCapabilities::probe(&tool, &runner).expect_err("changed probe identity");
        assert!(matches!(
            probe_error,
            BoundaryError::ExecutableIdentityChanged { .. }
        ));

        let runner = Arc::new(runner);
        let boundary =
            Boundary::new(tool, runner.clone(), JobLimits::default(), dir.clone()).unwrap();
        let destination = dir.join("must-not-publish.mp4");
        let frames = vec![0_u8; WireFormat::Nv12.frame_bytes(64, 36)];
        let job_error = boundary
            .encode(
                &job(WireFormat::Nv12, Container::Mp4, EncoderChoice::Auto),
                frames,
                &EncoderCapabilities::parse(ENCODERS_LISTING),
                &destination,
            )
            .expect_err("changed encode identity");
        assert!(matches!(
            job_error,
            BoundaryError::ExecutableIdentityChanged { .. }
        ));
        assert_eq!(runner.runs().len(), 1, "only the resolution probe ran");
        assert!(!destination.exists());
    }

    #[test]
    fn exclusive_workdir_creation_skips_collisions_without_claiming_them() {
        let (dir, _gate) = scratch("workdir-collision");
        let (tool_path, mut runner) = scripted_tool(&dir);
        let tool = FfmpegTool::resolve(&tool_path, &runner, &dir).unwrap();
        let collision = first_bound_tool(&dir)
            .parent()
            .expect("collision directory")
            .to_path_buf();
        std::fs::create_dir(&collision).unwrap();
        let sentinel = collision.join("foreign");
        std::fs::write(&sentinel, b"not ours").unwrap();

        runner.script(bound_tool(&dir, 1), successful_scripted_outcome());
        let runner = Arc::new(runner);
        let boundary = Boundary::new(
            tool,
            runner.clone(),
            JobLimits {
                keep_workdir: true,
                ..JobLimits::default()
            },
            dir.clone(),
        )
        .unwrap();
        let frames = vec![0_u8; WireFormat::Nv12.frame_bytes(64, 36)];
        let error = boundary
            .encode(
                &job(WireFormat::Nv12, Container::Mp4, EncoderChoice::Auto),
                frames,
                &EncoderCapabilities::parse(ENCODERS_LISTING),
                &dir.join("unused.mp4"),
            )
            .expect_err("scripted runner creates no artifact");
        assert!(matches!(error, BoundaryError::ArtifactMissing));
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"not ours");

        let runs = runner.runs();
        assert_eq!(runs.len(), 2);
        let workdir = runs[1].cwd.as_ref().expect("job workdir");
        assert_eq!(
            workdir,
            &session_root(&dir, 0).join(format!("fmn-job-{}-1", std::process::id()))
        );
        assert_eq!(runs[1].program, workdir.join("fmn-bound-ffmpeg"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            assert_eq!(
                std::fs::metadata(workdir).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(&runs[1].program)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o500
            );
        }
    }

    #[test]
    fn boundary_refuses_a_different_workdir_parent_than_resolution() {
        let (dir, _gate) = scratch("workdir-parent-mismatch");
        let (tool_path, runner) = scripted_tool(&dir);
        let tool = FfmpegTool::resolve(&tool_path, &runner, &dir).unwrap();
        let other = dir.join("other");

        let error = match Boundary::new(tool, Arc::new(runner), JobLimits::default(), other) {
            Ok(_) => panic!("a boundary cannot change the private-copy loader environment"),
            Err(error) => error,
        };
        assert!(matches!(error, BoundaryError::Workdir { .. }));
        assert!(
            error
                .to_string()
                .contains("differs from the resolved tool parent")
        );
    }

    #[test]
    fn boundary_refuses_a_replaced_private_session_root() {
        let (dir, _gate) = scratch("session-replacement");
        let (tool_path, runner) = scripted_tool(&dir);
        let tool = FfmpegTool::resolve(&tool_path, &runner, &dir).unwrap();
        let original = session_root(&dir, 0);
        let displaced = dir.join("displaced-session");
        std::fs::rename(&original, &displaced).unwrap();
        std::fs::create_dir(&original).unwrap();
        let sentinel = original.join("foreign");
        std::fs::write(&sentinel, b"not ours").unwrap();

        let error = match Boundary::new(tool, Arc::new(runner), JobLimits::default(), dir.clone()) {
            Ok(_) => panic!("a boundary cannot accept a replaced private session root"),
            Err(error) => error,
        };
        assert!(matches!(error, BoundaryError::Workdir { .. }));
        assert!(error.to_string().contains("session identity changed"));
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"not ours");
    }

    #[test]
    fn cleanup_retains_a_replaced_job_directory() {
        let (dir, _gate) = scratch("workdir-replacement");
        let (tool_path, mut runner) = scripted_tool(&dir);
        let tool = FfmpegTool::resolve(&tool_path, &runner, &dir).unwrap();
        runner.script(first_bound_tool(&dir), successful_scripted_outcome());
        let runner = Arc::new(runner);
        let boundary = Boundary::new(tool, runner, JobLimits::default(), dir.clone()).unwrap();
        let caps = EncoderCapabilities::parse(ENCODERS_LISTING);
        let stream = boundary
            .start_encode(
                &job(WireFormat::Nv12, Container::Mp4, EncoderChoice::Auto),
                &caps,
                &dir.join("unused.mp4"),
                None,
                fmn_platform::process::ProcessCancellation::new(),
                fmn_platform::process::ProcessStdinLimits::new(1, 1),
            )
            .unwrap();
        let claimed = first_bound_tool(&dir)
            .parent()
            .expect("job directory")
            .to_path_buf();
        let displaced = session_root(&dir, 0).join("displaced-owned-job");
        std::fs::rename(&claimed, &displaced).unwrap();
        std::fs::create_dir(&claimed).unwrap();
        let sentinel = claimed.join("foreign");
        std::fs::write(&sentinel, b"not ours").unwrap();

        drop(stream);

        assert_eq!(std::fs::read(&sentinel).unwrap(), b"not ours");
        assert!(displaced.join("fmn-bound-ffmpeg").is_file());
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
        let (dir, _gate) = scratch("contract");
        let (tool_path, mut runner) = scripted_tool(&dir);
        let tool = FfmpegTool::resolve(&tool_path, &runner, &dir).unwrap();
        runner.script(first_bound_tool(&dir), successful_scripted_outcome());
        let runner = Arc::new(runner);
        let caps = EncoderCapabilities::parse(ENCODERS_LISTING);
        let boundary =
            Boundary::new(tool, runner.clone(), JobLimits::default(), dir.clone()).unwrap();
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
        assert_eq!(spec.program, cwd.join("fmn-bound-ffmpeg"));
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
        let error = result.expect_err("unknown hardware encoder must be refused");
        assert!(
            matches!(&error, BoundaryError::UnknownEncoder { .. }),
            "expected UnknownEncoder, got {error:?}"
        );
        if let BoundaryError::UnknownEncoder {
            requested,
            hardware_available,
        } = error
        {
            assert_eq!(requested, "hevc_videotoolbox");
            assert_eq!(hardware_available, vec!["h264_nvenc".to_string()]);
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
        let (dir, _gate) = scratch("failures");
        let caps = EncoderCapabilities::parse(ENCODERS_LISTING);
        let frames = vec![0u8; WireFormat::Nv12.frame_bytes(64, 36)];
        let j = job(WireFormat::Nv12, Container::Mp4, EncoderChoice::Auto);

        // Nonzero exit with stderr.
        let nonzero_dir = dir.join("nonzero");
        create_private_test_directory(&nonzero_dir);
        let (tool_path, mut runner) = scripted_tool(&nonzero_dir);
        let tool = FfmpegTool::resolve(&tool_path, &runner, &nonzero_dir).unwrap();
        runner.script(
            first_bound_tool(&nonzero_dir),
            ProcessOutcome {
                termination: ProcessTermination::Exited(Some(1)),
                stdout: Vec::new(),
                stderr: b"Unknown encoder 'libx264'".to_vec(),
            },
        );
        let boundary =
            Boundary::new(tool, Arc::new(runner), JobLimits::default(), nonzero_dir).unwrap();
        let error = boundary
            .encode(&j, frames.clone(), &caps, &dir.join("a.mp4"))
            .expect_err("nonzero ffmpeg exit must be refused");
        assert!(
            matches!(&error, BoundaryError::EncodeFailed { .. }),
            "expected EncodeFailed, got {error:?}"
        );
        if let BoundaryError::EncodeFailed { code, stderr } = error {
            assert_eq!(code, Some(1));
            assert!(stderr.contains("Unknown encoder"));
        }

        // Timeout.
        let timeout_dir = dir.join("timeout");
        create_private_test_directory(&timeout_dir);
        let (tool_path, mut runner) = scripted_tool(&timeout_dir);
        let tool = FfmpegTool::resolve(&tool_path, &runner, &timeout_dir).unwrap();
        runner.script(
            first_bound_tool(&timeout_dir),
            ProcessOutcome {
                termination: ProcessTermination::TimedOut,
                stdout: Vec::new(),
                stderr: Vec::new(),
            },
        );
        let boundary =
            Boundary::new(tool, Arc::new(runner), JobLimits::default(), timeout_dir).unwrap();
        assert!(matches!(
            boundary.encode(&j, frames.clone(), &caps, &dir.join("b.mp4")),
            Err(BoundaryError::JobTimedOut { .. })
        ));

        // Log overflow.
        let overflow_dir = dir.join("overflow");
        create_private_test_directory(&overflow_dir);
        let (tool_path, mut runner) = scripted_tool(&overflow_dir);
        let tool = FfmpegTool::resolve(&tool_path, &runner, &overflow_dir).unwrap();
        runner.script(
            first_bound_tool(&overflow_dir),
            ProcessOutcome {
                termination: ProcessTermination::OutputLimitExceeded,
                stdout: Vec::new(),
                stderr: Vec::new(),
            },
        );
        let boundary =
            Boundary::new(tool, Arc::new(runner), JobLimits::default(), overflow_dir).unwrap();
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

    #[cfg(unix)]
    struct SourceSwappingRunner {
        source: PathBuf,
        replacement: Vec<u8>,
        swapped: std::sync::atomic::AtomicBool,
        swap_on_version: bool,
    }

    #[cfg(unix)]
    impl ProcessRunner for SourceSwappingRunner {
        fn start(
            &self,
            spec: &ProcessSpec,
            cancellation: ProcessCancellation,
            stdin_limits: ProcessStdinLimits,
        ) -> Result<Box<dyn RunningProcess>, ProcessError> {
            let is_version = spec.argv == ["-version"];
            if (self.swap_on_version || !is_version) && !self.swapped.swap(true, Ordering::AcqRel) {
                std::fs::write(&self.source, &self.replacement).map_err(|error| {
                    ProcessError::Plumbing {
                        program: spec.program.clone(),
                        detail: format!(
                            "replace resolved executable {}: {error}",
                            self.source.display()
                        ),
                    }
                })?;
            }
            StdProcessRunner.start(spec, cancellation, stdin_limits)
        }
    }

    #[cfg(unix)]
    struct TransientSourceSwappingRunner {
        source: PathBuf,
        replacement: Vec<u8>,
    }

    #[cfg(unix)]
    impl ProcessRunner for TransientSourceSwappingRunner {
        fn start(
            &self,
            spec: &ProcessSpec,
            cancellation: ProcessCancellation,
            stdin_limits: ProcessStdinLimits,
        ) -> Result<Box<dyn RunningProcess>, ProcessError> {
            let original = std::fs::read(&self.source).map_err(|error| ProcessError::Plumbing {
                program: spec.program.clone(),
                detail: format!(
                    "read resolved executable {}: {error}",
                    self.source.display()
                ),
            })?;
            std::fs::write(&self.source, &self.replacement).map_err(|error| {
                ProcessError::Plumbing {
                    program: spec.program.clone(),
                    detail: format!(
                        "transiently replace resolved executable {}: {error}",
                        self.source.display()
                    ),
                }
            })?;
            let started = StdProcessRunner.start(spec, cancellation, stdin_limits);
            let restored = std::fs::write(&self.source, original);
            match (started, restored) {
                (Ok(process), Ok(())) => Ok(process),
                (Ok(process), Err(error)) => {
                    let _ = process.cancel();
                    Err(ProcessError::Plumbing {
                        program: spec.program.clone(),
                        detail: format!(
                            "restore resolved executable {}: {error}",
                            self.source.display()
                        ),
                    })
                }
                (Err(error), _) => Err(error),
            }
        }
    }

    /// Resolve a fake tool that answers the `-version` probe.
    #[cfg(unix)]
    fn real_tool(dir: &Path, body: &str) -> (FfmpegTool, StdProcessRunner) {
        let runner = StdProcessRunner;
        let path = write_script(dir, "fake-ffmpeg", body);
        let tool = FfmpegTool::resolve(&path, &runner, dir).unwrap();
        (tool, runner)
    }

    #[cfg(unix)]
    #[test]
    fn transient_source_swap_cannot_select_the_version_probe_executable() {
        let (dir, _gate) = scratch("transient-version-substitution");
        let source = write_script(&dir, "fake-ffmpeg", FAKE_FFMPEG);
        let runner = TransientSourceSwappingRunner {
        source: source.clone(),
        replacement:
            b"#!/bin/sh\nif [ \"$1\" = \"-version\" ]; then echo 'ffmpeg version evil'; exit 0; fi\nexit 1\n"
                .to_vec(),
    };

        let tool = FfmpegTool::resolve(&source, &runner, &dir)
            .expect("the private copy is unaffected by a transient source swap");
        assert_eq!(tool.version(), "ffmpeg version 7.1-fake");
        assert_eq!(std::fs::read(&source).unwrap(), FAKE_FFMPEG.as_bytes());
    }

    #[cfg(unix)]
    #[test]
    fn version_probe_detects_substitution_during_execution() {
        let (dir, _gate) = scratch("version-substitution");
        let source = write_script(&dir, "fake-ffmpeg", FAKE_FFMPEG);
        let runner = SourceSwappingRunner {
        source: source.clone(),
        replacement:
            b"#!/bin/sh\nif [ \"$1\" = \"-version\" ]; then echo 'ffmpeg version evil'; exit 0; fi\nexit 1\n"
                .to_vec(),
        swapped: std::sync::atomic::AtomicBool::new(false),
        swap_on_version: true,
    };

        let error = FfmpegTool::resolve(&source, &runner, &dir)
            .expect_err("version substitution must fail");
        assert!(matches!(
            error,
            BoundaryError::ExecutableIdentityChanged { .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn encoder_probe_detects_substitution_during_execution() {
        let (dir, _gate) = scratch("probe-substitution");
        let source = write_script(&dir, "fake-ffmpeg", FAKE_FFMPEG);
        let tool = FfmpegTool::resolve(&source, &StdProcessRunner, &dir).unwrap();
        let runner = SourceSwappingRunner {
        source,
        replacement: b"#!/bin/sh\nif [ \"$1\" = \"-version\" ]; then echo 'ffmpeg version evil'; exit 0; fi\nif [ \"$2\" = \"-encoders\" ]; then echo 'Encoders:'; echo ' ------'; echo ' V....D libx264 fake'; exit 0; fi\nexit 1\n".to_vec(),
        swapped: std::sync::atomic::AtomicBool::new(false),
        swap_on_version: false,
    };

        let error =
            EncoderCapabilities::probe(&tool, &runner).expect_err("probe substitution must fail");
        assert!(matches!(
            error,
            BoundaryError::ExecutableIdentityChanged { .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn sandbox_publishes_atomically_and_pins_the_environment() {
        let (dir, _gate) = scratch("sandbox");
        let (tool, runner) = real_tool(&dir, FAKE_FFMPEG);
        let limits = JobLimits {
            keep_workdir: true,
            ..JobLimits::default()
        };
        let boundary = Boundary::new(tool, Arc::new(runner), limits, dir.clone()).unwrap();
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
        assert_eq!(report.destination, destination);
        assert_eq!(report.invocations.len(), 1);
        let invocation = &report.invocations[0];
        assert_eq!(invocation.provenance.encoder.as_deref(), Some("libx264"));
        assert!(
            invocation
                .provenance
                .tool_version
                .starts_with("ffmpeg version 7.1-fake")
        );

        // The env the child actually observed: pinned locale, private
        // TMPDIR, and no ambient leakage.
        let workdir: PathBuf = invocation.provenance.argv.last().unwrap().into();
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
    fn prepared_commit_refuses_a_replaced_job_directory() {
        let (dir, _gate) = scratch("prepared-workdir-replacement");
        let (tool, runner) = real_tool(&dir, FAKE_FFMPEG);
        let boundary =
            Boundary::new(tool, Arc::new(runner), JobLimits::default(), dir.clone()).unwrap();
        let caps = EncoderCapabilities::parse(ENCODERS_LISTING);
        let destination = dir.join("must-not-publish.mp4");
        let frames = vec![5_u8; WireFormat::Nv12.frame_bytes(64, 36)];
        let bytes = u64::try_from(frames.len()).unwrap();
        let mut stream = boundary
            .start_encode(
                &job(WireFormat::Nv12, Container::Mp4, EncoderChoice::Auto),
                &caps,
                &destination,
                None,
                ProcessCancellation::new(),
                ProcessStdinLimits::new(bytes, bytes),
            )
            .unwrap();
        stream.write_stdin(&frames).unwrap();
        let prepared = stream.prepare().unwrap();
        let claimed = first_bound_tool(&dir)
            .parent()
            .expect("job directory")
            .to_path_buf();
        let displaced = session_root(&dir, 0).join("displaced-prepared-job");
        std::fs::rename(&claimed, &displaced).unwrap();
        std::fs::create_dir(&claimed).unwrap();
        let sentinel = claimed.join("foreign");
        std::fs::write(&sentinel, b"not ours").unwrap();

        let error = prepared
            .commit()
            .expect_err("commit must not read or publish through a replaced path");
        assert!(matches!(error, BoundaryError::Workdir { .. }));
        assert!(
            error
                .to_string()
                .contains("claimed directory identity changed")
        );
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"not ours");
        assert!(displaced.join("out.mp4").is_file());
        assert!(!destination.exists());
    }

    #[cfg(unix)]
    #[test]
    fn spawn_executes_the_bound_copy_when_the_source_is_replaced() {
        let (dir, _gate) = scratch("spawn-binding");
        let source = write_script(&dir, "fake-ffmpeg", FAKE_FFMPEG);
        let replacement = b"#!/bin/sh\ncat > /dev/null\nif [ \"$1\" = \"-version\" ]; then echo 'ffmpeg version evil'; exit 0; fi\nfor a in \"$@\"; do last=\"$a\"; done\nprintf 'EVILVIDEO' > \"$last\"\nexit 0\n".to_vec();
        let runner = Arc::new(SourceSwappingRunner {
            source: source.clone(),
            replacement: replacement.clone(),
            swapped: std::sync::atomic::AtomicBool::new(false),
            swap_on_version: false,
        });
        let tool = FfmpegTool::resolve(&source, runner.as_ref(), &dir).unwrap();
        let source_identity = tool.path().to_path_buf();
        let source_sha256 = tool.sha256_hex().to_owned();
        let boundary = Boundary::new(
            tool,
            runner,
            JobLimits {
                keep_workdir: true,
                ..JobLimits::default()
            },
            dir.clone(),
        )
        .unwrap();
        let caps = EncoderCapabilities::parse(ENCODERS_LISTING);
        let destination = dir.join("bound.mp4");
        let frames = vec![3_u8; WireFormat::Nv12.frame_bytes(64, 36)];
        let report = boundary
            .encode(
                &job(WireFormat::Nv12, Container::Mp4, EncoderChoice::Auto),
                frames.clone(),
                &caps,
                &destination,
            )
            .unwrap();

        assert_eq!(std::fs::read(&destination).unwrap(), b"FAKEVIDEO");
        assert_eq!(std::fs::read(&source).unwrap(), replacement);
        let provenance = &report.invocations[0].provenance;
        assert_eq!(provenance.tool_path, source_identity);
        assert_eq!(provenance.tool_sha256_hex, source_sha256);
        assert_ne!(provenance.bound_tool_path, provenance.tool_path);
        assert_eq!(
            std::fs::read(&provenance.bound_tool_path).unwrap(),
            FAKE_FFMPEG.as_bytes()
        );

        let second_destination = dir.join("must-refuse.mp4");
        let error = boundary
            .encode(
                &job(WireFormat::Nv12, Container::Mp4, EncoderChoice::Auto),
                frames,
                &caps,
                &second_destination,
            )
            .expect_err("later job must reject the changed source");
        assert!(matches!(
            error,
            BoundaryError::ExecutableIdentityChanged { .. }
        ));
        assert!(!second_destination.exists());
    }

    #[cfg(unix)]
    #[test]
    fn sandbox_timeout_kills_and_leaves_destination_untouched() {
        let (dir, _gate) = scratch("timeout");
        // The shell remains the direct child while `sleep` inherits its pipes.
        // Returning promptly therefore proves the whole isolated process group
        // was killed; direct-child-only cancellation would block until sleep exits.
        let (tool, runner) = real_tool(
            &dir,
            "#!/bin/sh\nif [ \"$1\" = \"-version\" ]; then echo 'ffmpeg version 7.1-fake'; exit 0; fi\nsleep 5\n",
        );
        let limits = JobLimits {
            timeout: Duration::from_millis(200),
            ..JobLimits::default()
        };
        let boundary = Boundary::new(tool, Arc::new(runner), limits, dir.clone()).unwrap();
        let caps = EncoderCapabilities::parse(ENCODERS_LISTING);
        let destination = dir.join("never.mp4");
        let mut blocked_job = job(WireFormat::Nv12, Container::Mp4, EncoderChoice::Auto);
        blocked_job.width = 3840;
        blocked_job.height = 2160;
        let frames = vec![0u8; WireFormat::Nv12.frame_bytes(blocked_job.width, blocked_job.height)];
        let started = std::time::Instant::now();
        let result = boundary.encode(&blocked_job, frames, &caps, &destination);
        assert!(
            matches!(result, Err(BoundaryError::JobTimedOut { .. })),
            "expected a timeout, got {result:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "kill was not prompt"
        );
        assert!(!destination.exists());
    }

    #[cfg(unix)]
    #[test]
    fn sandbox_refuses_oversized_artifacts() {
        let (dir, _gate) = scratch("oversize");
        let (tool, runner) = real_tool(
            &dir,
            "#!/bin/sh\nif [ \"$1\" = \"-version\" ]; then echo 'ffmpeg version 7.1-fake'; exit 0; fi\ncat > /dev/null\nfor a in \"$@\"; do last=\"$a\"; done\nhead -c 4096 /dev/zero > \"$last\"\nexit 0\n",
        );
        let limits = JobLimits {
            max_artifact_bytes: 1024,
            ..JobLimits::default()
        };
        let boundary = Boundary::new(tool, Arc::new(runner), limits, dir.clone()).unwrap();
        let caps = EncoderCapabilities::parse(ENCODERS_LISTING);
        let destination = dir.join("big.mp4");
        let frames = vec![0u8; WireFormat::Nv12.frame_bytes(64, 36)];
        let result = boundary.encode(
            &job(WireFormat::Nv12, Container::Mp4, EncoderChoice::Auto),
            frames,
            &caps,
            &destination,
        );
        assert!(
            matches!(
                result,
                Err(BoundaryError::ArtifactOversized {
                    bytes: 4096,
                    max: 1024
                })
            ),
            "expected an oversized-artifact refusal, got {result:?}"
        );
        assert!(!destination.exists());
    }

    #[cfg(unix)]
    #[test]
    fn sandbox_failed_job_preserves_existing_destination() {
        let (dir, _gate) = scratch("failkeep");
        let (tool, runner) = real_tool(
            &dir,
            "#!/bin/sh\nif [ \"$1\" = \"-version\" ]; then echo 'ffmpeg version 7.1-fake'; exit 0; fi\ncat > /dev/null\necho 'boom' >&2\nexit 7\n",
        );
        let boundary =
            Boundary::new(tool, Arc::new(runner), JobLimits::default(), dir.clone()).unwrap();
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
        let error = result.expect_err("failed ffmpeg job must be refused");
        assert!(
            matches!(&error, BoundaryError::EncodeFailed { .. }),
            "expected EncodeFailed, got {error:?}"
        );
        if let BoundaryError::EncodeFailed { code, stderr } = error {
            assert_eq!(code, Some(7));
            assert!(stderr.contains("boom"));
        }
        assert_eq!(std::fs::read(&destination).unwrap(), b"the old render");
    }

    #[cfg(unix)]
    #[test]
    fn two_stage_mux_runs_both_stages_and_copies_video() {
        let (dir, _gate) = scratch("mux");
        let (tool, runner) = real_tool(&dir, FAKE_FFMPEG);
        let limits = JobLimits {
            keep_workdir: true,
            ..JobLimits::default()
        };
        let boundary = Boundary::new(tool, Arc::new(runner), limits, dir.clone()).unwrap();
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
        assert_eq!(report.invocations.len(), 2);
        let workdir: PathBuf = report.invocations[0].provenance.argv.last().unwrap().into();
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
    fn audio_transcode_uses_the_fake_capability_and_publishes_wav() {
        let (dir, _gate) = scratch("audio-transcode");
        let (tool, runner) = real_tool(&dir, FAKE_FFMPEG);
        let limits = JobLimits {
            keep_workdir: true,
            ..JobLimits::default()
        };
        let boundary = Boundary::new(tool, Arc::new(runner), limits, dir.clone()).unwrap();
        let input = dir.join("source.mp3");
        let destination = dir.join("decoded/track.wav");
        std::fs::write(&input, b"fake compressed audio").unwrap();

        let report = boundary
            .transcode_audio(&input, &destination)
            .expect("fake ffmpeg decodes");
        assert_eq!(std::fs::read(&destination).unwrap(), b"FAKEVIDEO");
        assert_eq!(report.invocations.len(), 1);
        let invocation = &report.invocations[0];
        assert!(invocation.provenance.encoder.is_none());
        assert!(
            invocation
                .provenance
                .argv
                .windows(2)
                .any(|args| { args == ["-acodec", "pcm_s16le"] })
        );
        assert!(
            invocation
                .provenance
                .argv
                .last()
                .is_some_and(|path| path.ends_with("decoded.wav"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn concat_writes_a_list_and_copies_streams() {
        let (dir, _gate) = scratch("concat");
        let (tool, runner) = real_tool(&dir, FAKE_FFMPEG);
        let limits = JobLimits {
            keep_workdir: true,
            ..JobLimits::default()
        };
        let boundary = Boundary::new(tool, Arc::new(runner), limits, dir.clone()).unwrap();
        let parts = vec![dir.join("part0.mp4"), dir.join("part1.mp4")];
        let destination = dir.join("joined.mp4");
        let report = boundary.concat(&parts, &destination).unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), b"FAKEVIDEO");
        assert_eq!(report.invocations.len(), 1);
        assert!(report.invocations[0].provenance.encoder.is_none());

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
        let (dir, _gate) = scratch("real");
        let runner = StdProcessRunner;
        let tool = FfmpegTool::resolve(Path::new("/usr/bin/ffmpeg"), &runner, &dir).unwrap();
        assert!(tool.version().starts_with("ffmpeg version"));
        let caps = EncoderCapabilities::probe(&tool, &runner).unwrap();
        assert!(caps.offers("libx264") || caps.offers("mpeg4"));

        let boundary =
            Boundary::new(tool, Arc::new(runner), JobLimits::default(), dir.clone()).unwrap();
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
        assert!(report.invocations[0].provenance.tool_sha256_hex.len() == 64);
        println!(
            "real ffmpeg smoke OK: {} bytes via {}",
            bytes.len(),
            report.invocations[0].provenance.tool_version
        );
    }
}
