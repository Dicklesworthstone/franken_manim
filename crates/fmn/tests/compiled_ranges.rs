//! Compiled excerpts retain global samples while emitting a new local sequence.
use std::cell::Cell;
use std::ops::Range;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use fmn::rendering::{RenderError, RenderFormat, RenderOptions, render_bundle_with_fs};
use fmn_anim::Timeline;
use fmn_config::config::{DeterminismMode, ThreadPolicy};
use fmn_core::rng::RngRoot;
use fmn_mobject::{Mobject, RecordBuffer, RecordSchema, Stage};
use fmn_platform::fs::{FileSystem, VirtualFs};
use fmn_scene::timeline_bundle::{BundleSoundtrack, attach_bundle_soundtrack};
use fmn_scene::{DEFAULT_MAX_BUNDLE_BYTES, TimelineBundle, export_timeline_bundle};

fn options(path: &str, format: RenderFormat, workers: u32, window: usize) -> RenderOptions {
    let mut options = RenderOptions::with_format(path, format).unwrap();
    options.config.camera.resolution = (32, 24);
    options.config.camera.fps = 8;
    options.config.sizes.frame_height = 6.0;
    options.config.render.threads = ThreadPolicy::Fixed(workers);
    options.config.determinism.mode = DeterminismMode::Certified;
    options.frames_in_flight = window;
    options.typeset_cache = false;
    options
}

fn recording() -> (Vec<u8>, Rc<Cell<usize>>) {
    let mut records = RecordBuffer::new(RecordSchema::vmobject(), 3).unwrap();
    records.write_range(
        "point",
        0,
        &[-1.5, -1.0, 0.0, 0.0, 1.25, 0.0, 1.5, -1.0, 0.0],
    );
    records.write_range("fill_rgba", 0, &[0.2, 0.6, 1.0, 1.0].repeat(3));
    let mut stage = Stage::new();
    let mob = stage.add(Mobject::from_buffer(records));
    stage.add_to_scene(mob).unwrap();
    let calls = Rc::new(Cell::new(0));
    let observed = Rc::clone(&calls);
    stage
        .add_dt_updater(
            mob,
            move |stage, mob, dt| {
                observed.set(observed.get() + 1);
                stage.shift(mob, [dt, 0.0, 0.0]);
            },
            false,
        )
        .unwrap();
    let mut timeline = Timeline::new(8).unwrap();
    for _ in 0..4 {
        timeline.wait(0.5).unwrap();
    }
    let bytes = export_timeline_bundle(timeline, &mut stage, &RngRoot::from_seed(7)).unwrap();
    assert!(
        calls.get() > 0,
        "the recording must actually execute the live updater"
    );
    (bytes, calls)
}

fn frame(fs: &VirtualFs, root: &str, index: u32) -> Vec<u8> {
    fs.read(Path::new(&format!("{root}/frame_{index:06}.png")))
        .unwrap()
}

#[test]
fn selected_frames_equal_the_full_recording_without_replaying_callbacks_or_prefix() {
    let (bytes, calls) = recording();
    let callbacks_after_export = calls.get();
    let fs = Arc::new(VirtualFs::new());
    let full = render_bundle_with_fs(
        &bytes,
        options("/full", RenderFormat::PngSequence, 1, 1),
        fs.clone(),
    )
    .unwrap();
    assert_eq!(full.artifact.frame_count, 16);
    assert_ne!(frame(&fs, "/full", 0), frame(&fs, "/full", 15));
    for (workers, window, preloaded) in [(1, 1, false), (4, 4, false), (4, 2, true)] {
        let path = format!("/clip-{workers}-{window}");
        let mut options = options(&path, RenderFormat::PngSequence, workers, window);
        // The unrendered 16-frame recording exceeds this selected-work bound.
        options.max_frames = 4;
        let report = if preloaded {
            options.render_loaded_bundle_with_fs(
                TimelineBundle::from_bytes(&bytes).unwrap(),
                Some(5..9),
                fs.clone(),
            )
        } else {
            options.render_bundle_range_with_fs(&bytes, 5..9, fs.clone())
        }
        .unwrap();
        assert_eq!(report.artifact.frame_count, 4);
        assert_eq!(report.scene.time.frames(), 9);
        assert_eq!(report.scene.play_count, 2);
        assert!(report.scene.ended_early);
        let stats = report.frame_pipeline.unwrap();
        assert_eq!(
            (stats.submitted, stats.emitted, stats.outstanding_slots),
            (4, 4, 0)
        );
        for local in 0..4 {
            assert_eq!(frame(&fs, &path, local), frame(&fs, "/full", local + 5));
        }
        assert!(!fs.exists(Path::new(&format!("{path}/frame_000004.png"))));
        assert_eq!(
            calls.get(),
            callbacks_after_export,
            "compiled replay invoked scene code"
        );
    }
}

#[test]
fn stills_sample_the_selection_end_not_the_recording_end_and_remain_create_only() {
    let (bytes, _) = recording();
    let fs = Arc::new(VirtualFs::new());
    render_bundle_with_fs(
        &bytes,
        options("/full", RenderFormat::PngSequence, 1, 1),
        fs.clone(),
    )
    .unwrap();
    let mut selected = options("/still.png", RenderFormat::Png, 2, 2);
    selected.max_frames = 4;
    let report = selected
        .render_bundle_range_with_fs(&bytes, 5..9, fs.clone())
        .unwrap();
    assert_eq!(report.artifact.frame_count, 1);
    assert_eq!(report.scene.time.frames(), 9);
    assert_eq!(report.frame_pipeline.unwrap().emitted, 1);
    let kept = fs.read(Path::new("/still.png")).unwrap();
    let pixels = |png: &[u8]| {
        fmn_codec::decode_png(png, &fmn_codec::PngLimits::default())
            .unwrap()
            .rgba
    };
    assert_eq!(pixels(&kept), pixels(&frame(&fs, "/full", 8)));
    assert!(
        options("/still.png", RenderFormat::Png, 2, 2)
            .render_bundle_range_with_fs(&bytes, 9..12, fs.clone())
            .is_err()
    );
    assert_eq!(fs.read(Path::new("/still.png")).unwrap(), kept);
}

#[test]
fn invalid_ranges_clock_and_budget_fail_before_an_output_generation_exists() {
    let (bytes, _) = recording();
    for (start, end) in [(0, 0), (7, 3), (0, 17), (16, 17)] {
        let fs = Arc::new(VirtualFs::new());
        let result = options("/refused", RenderFormat::PngSequence, 1, 1)
            .render_bundle_range_with_fs(&bytes, Range { start, end }, fs.clone());
        assert!(matches!(result, Err(RenderError::InvalidOptions(_))));
        assert!(!fs.exists(Path::new("/refused")));
    }
    for wrong_clock in [false, true] {
        let fs = Arc::new(VirtualFs::new());
        let mut invalid = options("/refused", RenderFormat::PngSequence, 1, 1);
        if wrong_clock {
            invalid.config.camera.fps = 9;
        } else {
            invalid.max_frames = 3;
        }
        assert!(matches!(
            invalid.render_bundle_range_with_fs(&bytes, 5..9, fs.clone()),
            Err(RenderError::InvalidOptions(_)),
        ));
        assert!(!fs.exists(Path::new("/refused")));
    }
}

#[test]
fn a_range_never_authorizes_silent_picture_only_replay_of_attached_audio() {
    let (pictures, _) = recording();
    let pcm = BundleSoundtrack::new(48_000, 1, vec![0.25; 8]).unwrap();
    let bytes = attach_bundle_soundtrack(&pictures, &pcm, DEFAULT_MAX_BUNDLE_BYTES).unwrap();
    let fs = Arc::new(VirtualFs::new());
    let result = options("/refused", RenderFormat::PngSequence, 1, 1)
        .render_bundle_range_with_fs(&bytes, 5..9, fs.clone());
    assert!(matches!(result, Err(RenderError::InvalidOptions(_))));
    assert!(!fs.exists(Path::new("/refused")));
}
