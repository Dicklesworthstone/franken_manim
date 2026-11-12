//! The shipping compiled-scene path must reconstruct on its render teams.
#![forbid(unsafe_code)]
#![cfg(all(feature = "cli", feature = "batch"))]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use fmn::animation::{Timeline, prepare_animation};
use fmn::mobject::animate::AnimateArgs;
use fmn::mobject::{Mobject, RecordBuffer, RecordSchema, Stage};
use fmn_codec::{PngLimits, decode_png};
use fmn_core::{rate, rng::RngRoot};
use fmn_frame::convert::rgba16f_to_rgba8;
use fmn_frame::{FrameBuffer, FrameLayout, PixelFormat};
use fmn_render::{
    EngineIdentity, FrameConfig, RetainedFrameRenderer, RetainedFrameRendererConfig, ScreenMap,
    Tiling, Viewport,
};
use fmn_scene::timeline_bundle::{BundleSegmentKind, TimelineBundle, export_timeline_bundle};

static NEXT: AtomicU64 = AtomicU64::new(0);

fn root() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "fmn-compiled-pipeline-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&path).unwrap();
    path
}

fn bundle(stateful: bool, zero_segment: bool) -> Vec<u8> {
    let mut records = RecordBuffer::new(RecordSchema::vmobject(), 3).unwrap();
    records.write_range(
        "point",
        0,
        &[-2.0, -1.0, 0.0, -0.5, 1.0, 0.0, 1.0, -1.0, 0.0],
    );
    records.write_range("fill_rgba", 0, &[0.2, 0.6, 1.0, 1.0].repeat(3));
    let mut stage = Stage::new();
    let mob = stage.add(Mobject::from_buffer(records));
    stage.add_to_scene(mob).unwrap();
    let mut timeline = Timeline::new(8).unwrap();
    if stateful {
        // Captures a non-Send value. Compiled playback must consume the stored
        // snapshots, not rerun this callback or guess a pure endpoint law.
        let calls = std::rc::Rc::new(std::cell::Cell::new(0_u32));
        stage
            .add_dt_updater(
                mob,
                move |stage, mob, dt| {
                    calls.set(calls.get() + 1);
                    stage.shift(mob, [dt, dt * 0.5, 0.0]);
                },
                false,
            )
            .unwrap();
        timeline.wait(0.5).unwrap();
    } else {
        let builder = mob
            .animate()
            .set_anim_args(AnimateArgs {
                run_time: Some(0.5),
                rate_func: Some(rate::linear),
                ..AnimateArgs::default()
            })
            .unwrap()
            .shift([1.0, 0.0, 0.0])
            .unwrap();
        timeline
            .play(vec![prepare_animation(builder, &mut stage).unwrap()])
            .unwrap();
    }
    timeline
        .wait(if zero_segment { 0.0 } else { 0.25 })
        .unwrap();
    export_timeline_bundle(timeline, &mut stage, &RngRoot::from_seed(17)).unwrap()
}

fn invoke(source: &Path, output: &Path, format: &str, threads: &str, extra: &[&str]) -> Output {
    invoke_mode(source, output, format, threads, extra, false)
}

fn invoke_mode(
    source: &Path,
    output: &Path,
    format: &str,
    threads: &str,
    extra: &[&str],
    batch: bool,
) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fmn"));
    command.current_dir(source.parent().unwrap());
    if batch {
        command.arg("batch");
    }
    if format != "gif" {
        command.arg("--reproducible");
    }
    command
        .args([
            "--robot",
            "--format",
            format,
            "--resolution",
            "32x24",
            "--fps",
            "8",
            "--threads",
            threads,
            "--video_dir",
        ])
        .arg(output)
        .args(extra)
        .arg(source)
        .arg("Compiled")
        .env("PATH", "")
        .env_remove("PYTHONPATH")
        .env_remove("PYTHONHOME");
    command.output().unwrap()
}

fn excerpt_bundle(recorded: bool) -> Vec<u8> {
    let mut stage = Stage::new();
    let mut records = RecordBuffer::new(RecordSchema::vmobject(), 3).unwrap();
    records.write_range(
        "point",
        0,
        &[-2.0, -1.0, 0.0, -0.5, 1.0, 0.0, 1.0, -1.0, 0.0],
    );
    records.write_range("fill_rgba", 0, &[0.2, 0.6, 1.0, 1.0].repeat(3));
    let mob = stage.add(Mobject::from_buffer(records));
    stage.add_to_scene(mob).unwrap();
    if recorded {
        let calls = std::rc::Rc::new(std::cell::Cell::new(0_u32));
        stage
            .add_dt_updater(
                mob,
                move |stage, mob, dt| {
                    calls.set(calls.get() + 1);
                    stage.shift(mob, [dt, 0.5 * dt, 0.0]);
                },
                false,
            )
            .unwrap();
    }
    let mut timeline = Timeline::new(8).unwrap();
    for (duration, shift) in [
        (0.5, [1.0, 0.0, 0.0]),
        (0.25, [0.0, 1.0, 0.0]),
        (0.25, [-1.0, 0.0, 0.0]),
    ] {
        if recorded {
            timeline.wait(duration).unwrap();
        } else {
            let animation = mob
                .animate()
                .set_anim_args(AnimateArgs {
                    run_time: Some(duration),
                    rate_func: Some(rate::linear),
                    ..AnimateArgs::default()
                })
                .unwrap()
                .shift(shift)
                .unwrap();
            timeline
                .play(vec![prepare_animation(animation, &mut stage).unwrap()])
                .unwrap();
        }
    }
    export_timeline_bundle(timeline, &mut stage, &RngRoot::from_seed(17)).unwrap()
}

fn selected_naming(output: &Path, end: Option<u64>) -> fmn_scene::OutputNaming {
    fmn_scene::OutputNaming {
        output_directory: output.to_owned(),
        start_at_play: Some(1),
        end_at_play: end,
        ..Default::default()
    }
}

fn success(output: Output) -> String {
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("\"route\":\"compiled-cpu\""), "{stdout}");
    assert!(stdout.contains("\"outstanding_slots\":0"), "{stdout}");
    stdout
}

fn pngs(directory: &Path) -> Vec<Vec<u8>> {
    let mut paths: Vec<_> = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "png"))
        .collect();
    paths.sort();
    paths
        .iter()
        .map(|path| std::fs::read(path).unwrap())
        .collect()
}

fn serial_pixels(bundle: &TimelineBundle) -> Vec<Vec<u8>> {
    let config = fmn_config::Config::resolve(&[], None).unwrap().config;
    let frame = FrameConfig::new(
        Viewport {
            width: 32,
            height: 24,
        },
        ScreenMap {
            scale: 24.0 / config.sizes.frame_height,
            origin: [16.0, 12.0],
            y_up: true,
        },
        fmn_core::color::Srgb::from_hex(&config.camera.background_color)
            .unwrap()
            .to_linear(config.camera.background_opacity),
    )
    .with_aa_policy(config.render.aa);
    (0..bundle.frame_count())
        .map(|index| {
            // Original serial reader and a fresh renderer are the oracle. Neither
            // SharedTimelineBundle nor NativeFramePipeline contributes expected bits.
            let stage = bundle.stage_at(index).unwrap();
            let mut renderer = RetainedFrameRenderer::new(RetainedFrameRendererConfig {
                frame,
                tiling: Tiling::default(),
                engine: EngineIdentity::certified(),
                threads: 1,
            })
            .unwrap();
            renderer.render(&stage, 0).unwrap();
            let mut pixels =
                FrameBuffer::new(FrameLayout::tight(PixelFormat::Rgba8, 32, 24).unwrap());
            rgba16f_to_rgba8(renderer.frame(), &mut pixels).unwrap();
            pixels.as_bytes().to_vec()
        })
        .collect()
}

#[test]
fn compiled_pure_and_recorded_cli_frames_match_serial_pixels_at_1_4_16_threads() {
    for stateful in [false, true] {
        let root = root();
        let source = root.join("scene.fmtl");
        let bytes = bundle(stateful, false);
        std::fs::write(&source, &bytes).unwrap();
        let reader = TimelineBundle::from_bytes(&bytes).unwrap();
        assert_eq!(reader.frame_count(), 6);
        assert_eq!(
            reader.segment_kind(0),
            Some(if stateful {
                BundleSegmentKind::Stateful
            } else {
                BundleSegmentKind::Pure
            })
        );
        let expected = serial_pixels(&reader);
        assert_ne!(expected[0], expected[3], "fixture must visibly animate");
        let mut first = None;
        for threads in ["1", "4", "16"] {
            let output = root.join(threads);
            let stdout = success(invoke(&source, &output, "png_sequence", threads, &[]));
            assert!(stdout.contains("\"submitted\":6,\"emitted\":6"), "{stdout}");
            let pngs = pngs(&output.join("Compiled"));
            assert_eq!(pngs.len(), expected.len());
            for (png, pixels) in pngs.iter().zip(&expected) {
                assert_eq!(
                    &decode_png(png, &PngLimits::default()).unwrap().rgba,
                    pixels
                );
            }
            if let Some(first) = &first {
                assert_eq!(&pngs, first);
            } else {
                first = Some(pngs);
            }
        }
    }
}

#[test]
fn compiled_stills_subdivisions_and_native_streams_keep_original_frame_selection() {
    let root = root();
    let source = root.join("scene.fmtl");
    std::fs::write(&source, bundle(false, false)).unwrap();
    let all = root.join("all");
    success(invoke(&source, &all, "png_sequence", "1", &[]));
    let expected = pngs(&all.join("Compiled"));
    let still = root.join("still");
    let stdout = success(invoke(&source, &still, "png", "4", &[]));
    assert!(stdout.contains("\"submitted\":1,\"emitted\":1"), "{stdout}");
    assert_eq!(
        std::fs::read(still.join("Compiled.png")).unwrap(),
        expected[5]
    );
    let partial = root.join("partial");
    let stdout = success(invoke(
        &source,
        &partial,
        "png_sequence",
        "4",
        &["--subdivide", "--prerun"],
    ));
    assert!(
        stdout.contains("\"subdivision\":0") && stdout.contains("\"subdivision\":1"),
        "{stdout}"
    );
    // Derive the authored naming rule rather than duplicating it in a test.
    let naming = fmn_scene::OutputNaming {
        output_directory: partial,
        ..Default::default()
    };
    let directory = naming.partial_directory("Compiled");
    assert_eq!(pngs(&directory.join("00000")), expected[..4]);
    assert_eq!(pngs(&directory.join("00001")), expected[4..]);
    for format in ["gif", "y4m"] {
        let mut first = None;
        for threads in ["1", "4", "16"] {
            let output = root.join(format!("{format}-{threads}"));
            success(invoke(&source, &output, format, threads, &[]));
            let bytes = std::fs::read(output.join(format!("Compiled.{format}"))).unwrap();
            if let Some(first) = &first {
                assert_eq!(&bytes, first);
            } else {
                first = Some(bytes);
            }
        }
    }
}

#[test]
fn compiled_ranges_replay_original_pure_and_recorded_samples_without_the_prefix() {
    for recorded in [false, true] {
        let root = root();
        let source = root.join("excerpt.fmtl");
        let bytes = excerpt_bundle(recorded);
        std::fs::write(&source, &bytes).unwrap();
        let reader = TimelineBundle::from_bytes(&bytes).unwrap();
        assert_eq!(reader.frame_count(), 8);
        assert_eq!(
            reader.segment_kind(1),
            Some(if recorded {
                BundleSegmentKind::Stateful
            } else {
                BundleSegmentKind::Pure
            })
        );
        let expected = serial_pixels(&reader);
        assert_ne!(expected[3], expected[5]);
        assert_ne!(expected[5], expected[7]);
        let mut baseline = None;
        for threads in ["1", "4", "16"] {
            let output = root.join(threads);
            let stdout = success(invoke(
                &source,
                &output,
                "png_sequence",
                threads,
                &["-n", "1,2", "--prerun"],
            ));
            assert!(stdout.contains("\"frames\":2,\"segments\":1"), "{stdout}");
            assert!(stdout.contains("\"source_frames\":[4,6]"), "{stdout}");
            assert!(stdout.contains("\"source_segments\":[1,2]"), "{stdout}");
            assert!(stdout.contains("\"submitted\":2,\"emitted\":2"), "{stdout}");
            let selected = selected_naming(&output, Some(2)).root("Compiled");
            let frames = pngs(&selected);
            assert_eq!(frames.len(), 2);
            assert!(selected.join("frame_000000.png").is_file());
            assert!(!selected.join("frame_000002.png").exists());
            for (png, pixels) in frames.iter().zip(&expected[4..6]) {
                assert_eq!(
                    &decode_png(png, &PngLimits::default()).unwrap().rgba,
                    pixels
                );
            }
            if let Some(baseline) = &baseline {
                assert_eq!(&frames, baseline);
            } else {
                baseline = Some(frames);
            }
        }
        let output = root.join("tail");
        success(invoke(&source, &output, "png_sequence", "4", &["-n", "1"]));
        let frames = pngs(&selected_naming(&output, None).root("Compiled"));
        assert_eq!(frames.len(), 4);
        for (png, pixels) in frames.iter().zip(&expected[4..]) {
            assert_eq!(
                &decode_png(png, &PngLimits::default()).unwrap().rgba,
                pixels
            );
        }
        let output = root.join("still");
        let stdout = success(invoke(&source, &output, "png", "4", &["-n", "1,2"]));
        assert!(stdout.contains("\"source_frames\":[5,6]"), "{stdout}");
        let path = selected_naming(&output, Some(2)).artifact("Compiled", "png");
        assert_eq!(
            decode_png(&std::fs::read(path).unwrap(), &PngLimits::default())
                .unwrap()
                .rgba,
            expected[5],
        );
    }
}

#[test]
fn compiled_range_subdivision_keeps_authored_ordinals_in_render_and_batch() {
    let root = root();
    let source = root.join("excerpt.fmtl");
    let bytes = excerpt_bundle(false);
    std::fs::write(&source, &bytes).unwrap();
    let expected = serial_pixels(&TimelineBundle::from_bytes(&bytes).unwrap());
    for batch in [false, true] {
        let output = root.join(if batch { "batch" } else { "render" });
        let stdout = success(invoke_mode(
            &source,
            &output,
            "png_sequence",
            "4",
            &["-n", "1,3", "--subdivide", "--prerun"],
            batch,
        ));
        assert!(stdout.contains("\"frames\":4,\"segments\":2"), "{stdout}");
        assert!(stdout.contains("\"subdivision\":1"), "{stdout}");
        assert!(stdout.contains("\"subdivision\":2"), "{stdout}");
        assert!(!stdout.contains("\"subdivision\":0"), "{stdout}");
        let partial = selected_naming(&output, Some(3)).partial_directory("Compiled");
        assert!(!partial.join("00000").exists());
        for (ordinal, pixels) in [(1, &expected[4..6]), (2, &expected[6..8])] {
            let frames = pngs(&partial.join(format!("{ordinal:05}")));
            assert_eq!(frames.len(), pixels.len());
            for (png, expected) in frames.iter().zip(pixels) {
                assert_eq!(
                    &decode_png(png, &PngLimits::default()).unwrap().rgba,
                    expected
                );
            }
        }
    }
    let output = root.join("y4m");
    let stdout = success(invoke(&source, &output, "y4m", "4", &["-n", "1,2"]));
    assert!(stdout.contains("\"source_frames\":[4,6]"), "{stdout}");
    let bytes =
        std::fs::read(selected_naming(&output, Some(2)).artifact("Compiled", "y4m")).unwrap();
    assert!(bytes.starts_with(b"YUV4MPEG2 W32 H24 F8:1"));
    assert_eq!(
        bytes
            .windows(6)
            .filter(|window| *window == b"FRAME\n")
            .count(),
        2
    );
}

#[test]
fn compiled_range_validation_bounds_selected_work_and_precedes_publication() {
    let root = root();
    let source = root.join("excerpt.fmtl");
    let bytes = excerpt_bundle(false);
    std::fs::write(&source, &bytes).unwrap();
    let reader = TimelineBundle::from_bytes(&bytes).unwrap();
    assert!(fmn_cli::select_compiled_segments(&reader, None, 2).is_err());
    let selected = fmn_cli::select_compiled_segments(
        &reader,
        Some(fmn_cli::AnimationRange {
            start: 1,
            end: Some(2),
        }),
        2,
    )
    .unwrap();
    assert_eq!(selected.frames, 4..6);
    assert_eq!(selected.segments, 1..2);
    let mut zero_edges = Timeline::new(8).unwrap();
    for duration in [0.0, 0.125, 0.0, 0.125, 0.0] {
        zero_edges.wait(duration).unwrap();
    }
    let zero_edges =
        export_timeline_bundle(zero_edges, &mut Stage::new(), &RngRoot::from_seed(0)).unwrap();
    let zero_edges = TimelineBundle::from_bytes(&zero_edges).unwrap();
    for (start, end, expected) in [(0, 3, 0..1), (2, 5, 1..2)] {
        let selected = fmn_cli::select_compiled_segments(
            &zero_edges,
            Some(fmn_cli::AnimationRange {
                start,
                end: Some(end),
            }),
            1,
        )
        .unwrap();
        assert_eq!(selected.frames, expected);
    }
    let output = root.join("refused");
    std::fs::create_dir(&output).unwrap();
    for selection in ["0,0", "1,1", "3", "1,4", "18446744073709551615"] {
        let result = invoke(&source, &output, "png", "1", &["-n", selection]);
        assert_eq!(
            result.status.code(),
            Some(2),
            "{}",
            String::from_utf8_lossy(&result.stdout)
        );
        assert_eq!(std::fs::read_dir(&output).unwrap().count(), 0);
    }
    let zero_source = root.join("zero.fmtl");
    std::fs::write(&zero_source, bundle(false, true)).unwrap();
    let result = invoke(&zero_source, &output, "png", "1", &["-n", "1,2"]);
    assert_eq!(result.status.code(), Some(5));
    assert!(
        String::from_utf8(result.stdout)
            .unwrap()
            .contains("no captured frames")
    );
    assert_eq!(std::fs::read_dir(&output).unwrap().count(), 0);
    // A zero-frame segment outside the requested interval does not block output.
    success(invoke(
        &zero_source,
        &output,
        "png",
        "1",
        &["-n", "0,1", "--subdivide"],
    ));
}

#[test]
fn compiled_range_subdivision_preflights_every_selected_generation() {
    let root = root();
    let source = root.join("excerpt.fmtl");
    std::fs::write(&source, excerpt_bundle(false)).unwrap();
    let output = root.join("output");
    let partial = selected_naming(&output, Some(3)).partial_directory("Compiled");
    std::fs::create_dir_all(&partial).unwrap();
    let unselected = partial.join("00000.png");
    let existing = partial.join("00002.png");
    std::fs::write(&unselected, b"unselected generation").unwrap();
    std::fs::write(&existing, b"previously published generation").unwrap();
    let result = invoke(&source, &output, "png", "4", &["-n", "1,3", "--subdivide"]);
    assert_eq!(
        result.status.code(),
        Some(6),
        "{}",
        String::from_utf8_lossy(&result.stdout)
    );
    assert!(!partial.join("00001.png").exists());
    assert_eq!(std::fs::read(unselected).unwrap(), b"unselected generation");
    assert_eq!(
        std::fs::read(existing).unwrap(),
        b"previously published generation"
    );
}

#[test]
fn compiled_range_provenance_binds_the_selected_segment_interval() {
    let root = root();
    let source = root.join("excerpt.fmtl");
    std::fs::write(&source, excerpt_bundle(false)).unwrap();
    let mut manifests = Vec::new();
    for (range, start, end) in [("0,1", 0, 1), ("1,2", 1, 2)] {
        let output = root.join(range);
        success(invoke(&source, &output, "png", "1", &["-n", range]));
        let naming = fmn_scene::OutputNaming {
            output_directory: output,
            start_at_play: Some(start),
            end_at_play: Some(end),
            ..Default::default()
        };
        let artifact = naming.artifact("Compiled", "png");
        let mut leaf = artifact.file_name().unwrap().to_os_string();
        leaf.push(".manifest");
        let bytes = std::fs::read(artifact.with_file_name(leaf).join("manifest.fmnp")).unwrap();
        manifests.push(fmn_output::ProvenanceManifest::from_bytes(&bytes).unwrap());
    }
    // Identical input bytes and renderer configuration still represent
    // different declared renders when the authored interval changes.
    let sources = |manifest: &fmn_output::ProvenanceManifest| {
        manifest
            .items
            .iter()
            .filter(|item| item.item_id == 1)
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(sources(&manifests[0]), sources(&manifests[1]));
    assert_ne!(
        manifests[0].identity.declared_config_digest,
        manifests[1].identity.declared_config_digest,
    );
    assert_ne!(manifests[0].closure_digest, manifests[1].closure_digest);
}

#[test]
fn zero_frame_subdivision_refuses_before_publishing_any_generation() {
    let root = root();
    let source = root.join("scene.fmtl");
    std::fs::write(&source, bundle(false, true)).unwrap();
    let reader = TimelineBundle::from_bytes(&std::fs::read(&source).unwrap()).unwrap();
    assert_eq!(reader.segment_frame_range(1), Some(4..4));
    let output = root.join("output");
    std::fs::create_dir(&output).unwrap();
    let result = invoke(&source, &output, "png", "4", &["--subdivide"]);
    assert!(!result.status.success());
    assert!(
        String::from_utf8(result.stdout)
            .unwrap()
            .contains("zero-frame segment")
    );
    assert_eq!(std::fs::read_dir(output).unwrap().count(), 0);
}

#[test]
fn readme_fmtl_fixture_renders_without_retagging_decoded_functions() {
    // Run this test with --release as well as the dev profile. The regression
    // lives at a cross-crate/codegen boundary; an exporter-created fixture can
    // silently fall back to recorded data and cannot exercise this failure.
    // Do not regenerate the repository's shipped README artifact here.
    let bytes = include_bytes!("../../../demo/wasm/bundle.fmtl");
    let reader = TimelineBundle::from_bytes(bytes).unwrap();
    assert_eq!(reader.frame_count(), 45);
    assert_eq!(reader.segment_kind(0), Some(BundleSegmentKind::Pure));
    assert_eq!(reader.segment_kind(1), Some(BundleSegmentKind::Pure));
    let expected = serial_pixels(&reader);
    assert_ne!(
        expected[0], expected[29],
        "README scene must visibly animate"
    );
    let root = root();
    let source = root.join("bundle.fmtl");
    std::fs::write(&source, bytes).unwrap();
    let mut first = None;
    for threads in ["1", "4", "16"] {
        let output = root.join(threads);
        let result = Command::new(env!("CARGO_BIN_EXE_fmn"))
            .current_dir(&root)
            .args([
                "--reproducible",
                "--robot",
                "--format",
                "png_sequence",
                "--resolution",
                "32x24",
                "--threads",
                threads,
                "--video_dir",
            ])
            .arg(&output)
            .arg(&source)
            .arg("DemoTimeline")
            .env("PATH", "")
            .env_remove("PYTHONPATH")
            .env_remove("PYTHONHOME")
            .output()
            .unwrap();
        // In particular, do not set --fps: the README artifact owns its clock.
        let stdout = success(result);
        assert!(
            stdout.contains("\"submitted\":45,\"emitted\":45"),
            "{stdout}"
        );
        let actual = pngs(&output.join("DemoTimeline"));
        assert_eq!(actual.len(), expected.len());
        for (png, pixels) in actual.iter().zip(&expected) {
            assert_eq!(
                &decode_png(png, &PngLimits::default()).unwrap().rgba,
                pixels
            );
        }
        if let Some(first) = &first {
            assert_eq!(&actual, first);
        } else {
            first = Some(actual);
        }
    }
}
