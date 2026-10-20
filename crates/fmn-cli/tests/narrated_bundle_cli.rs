//! Real shipping executable, canonical WAV, and optional ffmpeg round trips.
#![forbid(unsafe_code)]
#![cfg(all(feature = "cli", feature = "batch"))]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use fmn::animation::Timeline;
use fmn::mobject::{Mobject, RecordBuffer, RecordSchema, Stage};
use fmn_codec::{WavLimits, decode_wav};
use fmn_core::rng::RngRoot;
use fmn_scene::timeline_bundle::{BundleSoundtrack, attach_bundle_soundtrack};
use fmn_scene::{DEFAULT_MAX_BUNDLE_BYTES, export_timeline_bundle};

static NEXT: AtomicU64 = AtomicU64::new(0);

fn root() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "fmn-narrated-cli-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::create_dir(&path).unwrap();
    path
}

fn recording(root: &Path) -> (PathBuf, Vec<f32>) {
    let mut records = RecordBuffer::new(RecordSchema::vmobject(), 3).unwrap();
    records.write_range(
        "point", 0, &[-2.0, -1.0, 0.0, -0.5, 1.0, 0.0, 1.0, -1.0, 0.0],
    );
    records.write_range("fill_rgba", 0, &[0.2, 0.6, 1.0, 1.0].repeat(3));
    let mut stage = Stage::new();
    let mob = stage.add(Mobject::from_buffer(records));
    stage.add_to_scene(mob).unwrap();
    stage.add_dt_updater(mob, |stage, mob, dt| {
        stage.shift(mob, [dt, 0.0, 0.0]);
    }, false).unwrap();
    let mut timeline = Timeline::new(8).unwrap();
    timeline.wait(0.375).unwrap();
    timeline.wait(0.625).unwrap();
    let pictures = export_timeline_bundle(timeline, &mut stage, &RngRoot::from_seed(19)).unwrap();
    // Distinct periodic channels expose swapped channels, lost time offsets,
    // or silent output. One second of pictures has a quarter-second audio tail.
    let triangle = |frame: u32, period: u16| {
        let phase = u16::try_from(frame % u32::from(period)).unwrap();
        let rising = phase.min(period - phase);
        f32::from(rising) / f32::from(period / 2) - 0.5
    };
    let pcm: Vec<_> = (0..60_000)
        .flat_map(|frame| [0.5 * triangle(frame, 480), 0.25 * triangle(frame, 600)])
        .collect();
    let soundtrack = BundleSoundtrack::new(48_000, 2, pcm.clone()).unwrap();
    let bytes = attach_bundle_soundtrack(&pictures, &soundtrack, DEFAULT_MAX_BUNDLE_BYTES).unwrap();
    let source = root.join("self-contained.fmtl");
    std::fs::write(&source, bytes).unwrap();
    (source, pcm)
}

fn invoke(source: &Path, directory: &Path, format: &str, extra: &[&str], empty_path: bool) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fmn"));
    command.current_dir(source.parent().unwrap())
        .args(["--robot", "--format", format, "--resolution", "32x24", "--video_dir"])
        .arg(directory)
        .args(extra)
        .arg(source)
        .arg("Narrated")
        .env_remove("PYTHONPATH")
        .env_remove("PYTHONHOME");
    if empty_path {
        command.env("PATH", "");
    }
    command.output().unwrap()
}

fn success(output: Output) -> String {
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(output.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&output.stderr));
    assert!(stdout.contains("\"kind\":\"bundle-audio-render\""), "{stdout}");
    assert!(stdout.contains("\"certified\":false"), "{stdout}");
    assert!(stdout.contains("\"sample_rate\":48000,\"channels\":2"), "{stdout}");
    stdout
}

fn assert_wav(path: &Path, expected: &[f32]) {
    let bytes = std::fs::read(path).unwrap();
    let audio = decode_wav(&bytes, &WavLimits::default()).unwrap();
    assert_eq!((audio.sample_rate, audio.channels), (48_000, 2));
    assert_eq!(audio.samples.len(), expected.len());
    for (&actual, &original) in audio.samples.iter().zip(expected) {
        assert!((actual - original).abs() <= 1.0 / 32768.0, "canonical S16 quantization drift");
    }
}

#[test]
fn shipping_wav_and_subdivision_need_no_tools_or_source_assets() {
    let root = root();
    let (source, pcm) = recording(&root);
    let full = root.join("wav");
    let stdout = success(invoke(&source, &full, "wav", &[], true));
    assert!(stdout.contains("\"sample_frames\":60000"), "{stdout}");
    let artifact = full.join("Narrated.wav");
    assert_wav(&artifact, &pcm);
    let original = std::fs::read(&artifact).unwrap();
    let refusal = invoke(&source, &full, "wav", &[], true);
    assert!(!refusal.status.success());
    assert_eq!(std::fs::read(&artifact).unwrap(), original);
    let clips = root.join("wav-clips");
    let stdout = success(invoke(&source, &clips, "wav", &["--subdivide"], true));
    assert_eq!(stdout.lines().count(), 2);
    assert_wav(&clips.join("Narrated/00000.wav"), &pcm[..36_000]);
    assert_wav(&clips.join("Narrated/00001.wav"), &pcm[36_000..96_000]);
}

fn ffmpeg() -> Option<PathBuf> {
    let found = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|root| root.join(format!("ffmpeg{}", std::env::consts::EXE_SUFFIX)))
            .find(|path| path.is_file())
    });
    if found.is_none() {
        assert_ne!(std::env::var("FMN_REQUIRE_FULL_INPUTS").as_deref(), Ok("1"),
            "real narrated video acceptance requires ffmpeg");
        eprintln!("real narrated video acceptance skipped: ffmpeg is not installed");
    }
    found.map(|path| path.canonicalize().unwrap())
}

fn decoded(ffmpeg: &Path, artifact: &Path, audio: bool) -> Vec<u8> {
    let mut command = Command::new(ffmpeg);
    command.args(["-v", "error", "-nostdin", "-i"]).arg(artifact);
    if audio {
        command.args(["-map", "0:a:0", "-c:a", "pcm_f32le", "-f", "f32le", "pipe:1"]);
    } else {
        command.args(["-map", "0:v:0", "-fps_mode", "passthrough", "-pix_fmt", "rgba", "-f", "rawvideo", "pipe:1"]);
    }
    let output = command.output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    output.stdout
}

fn assert_encoded_audio(ffmpeg: &Path, artifact: &Path, expected: &[f32]) {
    let bytes = decoded(ffmpeg, artifact, true);
    assert_eq!(bytes.len() % 4, 0);
    let samples: Vec<_> = bytes.chunks_exact(4)
        .map(|chunk| f32::from_le_bytes(chunk.try_into().unwrap())).collect();
    assert!(samples.len() >= expected.len(), "encoded soundtrack was truncated");
    assert!(samples.len() <= expected.len() + 4096, "unexpected encoded duration");
    let start = 2048;
    let end = expected.len() - 2048;
    let squared_error: f64 = samples[start..end].iter().zip(&expected[start..end])
        .map(|(&a, &b)| (f64::from(a) - f64::from(b)).powi(2)).sum();
    let mse = squared_error / f64::from(u32::try_from(end - start).unwrap());
    assert!(mse < 0.001, "wrong, silent, shifted or channel-swapped soundtrack: MSE={mse}");
}

#[test]
fn shipping_mp4_and_alpha_mov_preserve_pictures_channels_and_audio_tail() {
    let Some(ffmpeg) = ffmpeg() else { return; };
    let root = root();
    let (source, pcm) = recording(&root);
    for (extension, extra) in [("mp4", vec!["--threads", "1"]),
                               ("mov", vec!["--threads", "4", "--transparent"])] {
        let directory = root.join(extension);
        let stdout = success(invoke(&source, &directory, "video", &extra, false));
        assert!(stdout.contains("\"frames\":8"), "{stdout}");
        assert!(stdout.contains("\"sample_frames\":60000"), "{stdout}");
        assert!(stdout.contains("\"outstanding_slots\":0"), "{stdout}");
        assert!(stdout.contains("\"ffmpeg_invocations\":[{"), "{stdout}");
        let artifact = directory.join(format!("Narrated.{extension}"));
        let pictures = decoded(&ffmpeg, &artifact, false);
        assert_eq!(pictures.len(), 8 * 32 * 24 * 4);
        assert_ne!(&pictures[..32 * 24 * 4], &pictures[7 * 32 * 24 * 4..]);
        if extension == "mov" {
            assert!(pictures.chunks_exact(4).any(|pixel| pixel[3] == 0));
            assert!(pictures.chunks_exact(4).any(|pixel| pixel[3] > 0));
        }
        assert_encoded_audio(&ffmpeg, &artifact, &pcm);
    }
}

#[test]
fn shipping_video_subdivision_preserves_original_audio_offsets() {
    let Some(ffmpeg) = ffmpeg() else { return; };
    let root = root();
    let (source, pcm) = recording(&root);
    let clips = root.join("video-clips");
    let stdout = success(invoke(&source, &clips, "video", &["--subdivide", "--threads", "4"], false));
    assert_eq!(stdout.lines().count(), 2);
    for (index, start, end, frame_count) in [(0, 0, 36_000, 3), (1, 36_000, 96_000, 5)] {
        let artifact = clips.join(format!("Narrated/{index:05}.mp4"));
        assert_eq!(decoded(&ffmpeg, &artifact, false).len(), frame_count * 32 * 24 * 4);
        assert_encoded_audio(&ffmpeg, &artifact, &pcm[start..end]);
    }
}

#[test]
fn shipping_range_selection_extracts_exact_wav_without_a_source_runtime() {
    let root = root();
    let (source, pcm) = recording(&root);
    for (range, name) in [("1,2", "Narrated_1_2"), ("1", "Narrated_1")] {
        let directory = root.join(format!("range-{name}"));
        let stdout = success(invoke(&source, &directory, "wav", &["-n", range], true));
        assert!(stdout.contains("\"source_frames\":[3,8]"), "{stdout}");
        assert!(stdout.contains("\"source_segments\":[1,2]"), "{stdout}");
        assert!(stdout.contains("\"sample_frames\":30000"), "{stdout}");
        assert_wav(&directory.join(format!("{name}.wav")), &pcm[36_000..96_000]);
    }
    let clips = root.join("selected-clips");
    let stdout = success(invoke(&source, &clips, "wav", &["-n", "1,2", "--subdivide"], true));
    assert_eq!(stdout.lines().count(), 1);
    assert!(stdout.contains("\"subdivision\":1"), "{stdout}");
    assert!(!clips.join("Narrated_1_2/00000.wav").exists());
    assert_wav(&clips.join("Narrated_1_2/00001.wav"), &pcm[36_000..96_000]);
    let invalid = root.join("outside");
    assert!(!invoke(&source, &invalid, "wav", &["-n", "1,3"], true).status.success());
    assert!(!invalid.exists());
}

#[test]
fn shipping_selected_alpha_video_matches_full_frame_slice_and_original_sound() {
    let Some(ffmpeg) = ffmpeg() else { return; };
    let root = root();
    let (source, pcm) = recording(&root);
    let full = root.join("full");
    success(invoke(&source, &full, "video", &["--transparent", "--threads", "1"], false));
    let reference = decoded(&ffmpeg, &full.join("Narrated.mov"), false);
    assert_eq!(reference.len(), 8 * 32 * 24 * 4);
    for subdivide in [false, true] {
        let directory = root.join(if subdivide { "selected-clips" } else { "selected" });
        let mut arguments = vec!["--transparent", "--threads", "4", "-n", "1,2"];
        if subdivide {
            arguments.push("--subdivide");
        }
        let stdout = success(invoke(&source, &directory, "video", &arguments, false));
        assert!(stdout.contains("\"frames\":5"), "{stdout}");
        assert!(stdout.contains("\"source_frames\":[3,8]"), "{stdout}");
        assert!(stdout.contains("\"sample_frames\":30000"), "{stdout}");
        assert!(stdout.contains("\"outstanding_slots\":0"), "{stdout}");
        let path = directory.join(if subdivide { "Narrated_1_2/00001.mov" } else { "Narrated_1_2.mov" });
        // qtrle's lossless alpha path permits an exact decoded-pixel comparison.
        assert_eq!(decoded(&ffmpeg, &path, false), reference[3 * 32 * 24 * 4..]);
        assert_encoded_audio(&ffmpeg, &path, &pcm[36_000..96_000]);
    }
}
