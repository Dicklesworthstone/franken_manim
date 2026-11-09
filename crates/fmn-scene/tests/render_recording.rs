//! Render-only recording is compared with full capture in the same invocation.
//! The real Scene, snapshot reader, retained renderer and shared player run;
//! neither a callback clock nor a geometry/serialization oracle is substituted.
use std::cell::Cell;
use std::rc::Rc;

use fmn_anim::FramePacket;
use fmn_core::color::Srgb;
use fmn_hash::sha256;
use fmn_mobject::{Mob, Mobject, RecordBuffer, RecordSchema, Stage};
use fmn_render::{
    EngineIdentity, FrameConfig, RetainedFrameRenderer, RetainedFrameRendererConfig, ScreenMap,
    Tiling, Viewport,
};
use fmn_scene::recording::{
    CompactRecordingLimits, RecordedSceneBundle, RecordingError, SceneBundleRecorder,
};
use fmn_scene::timeline_bundle::{SharedTimelineBundle, TimelineFrameCache};
use fmn_scene::{
    BundleError, BundleExportLimits, BundleSegmentKind, CaptureReason, IntegrationError,
    LifecycleEvent, RuntimeConfig, Scene, SceneSink, TimelineBundle,
};

fn rect(stage: &mut Stage, x: f64, rgba: [f32; 4]) -> Mob {
    let mob = stage.add(Mobject::new());
    let entry = stage.get_mut(mob).unwrap();
    entry.buffer = RecordBuffer::new(RecordSchema::vmobject(), 9).unwrap();
    entry.buffer.write_range(
        "point",
        0,
        &[
            -0.5, -0.5, 0.0, 0.0, -0.5, 0.0, 0.5, -0.5, 0.0, 0.5, 0.0, 0.0, 0.5, 0.5, 0.0, 0.0,
            0.5, 0.0, -0.5, 0.5, 0.0, -0.5, 0.0, 0.0, -0.5, -0.5, 0.0,
        ],
    );
    entry.buffer.write_range("fill_rgba", 0, &rgba.repeat(9));
    entry.buffer.write_range("stroke_width", 0, &[0.0; 9]);
    stage.shift(mob, [x, 0.0, 0.0]);
    mob
}

fn renderer(threads: usize) -> RetainedFrameRenderer {
    RetainedFrameRenderer::new(RetainedFrameRendererConfig {
        frame: FrameConfig::new(
            Viewport {
                width: 96,
                height: 64,
            },
            ScreenMap::y_up(16.0, [48.0, 32.0]),
            Srgb::from_rgb8(0, 0, 0).to_linear(1.0),
        ),
        tiling: Tiling {
            macro_tile: 64,
            fine_tile: 8,
        },
        engine: EngineIdentity::certified(),
        threads,
    })
    .unwrap()
}

fn pixels(renderer: &mut RetainedFrameRenderer, stage: &Stage) -> Vec<u8> {
    renderer.render(stage, 0).unwrap();
    renderer.frame().plane(0).to_vec()
}

fn scene() -> Scene {
    Scene::new(
        RuntimeConfig {
            fps: 8,
            ..RuntimeConfig::default()
        },
        19,
    )
    .unwrap()
}

fn picture(history: usize) -> Stage {
    let mut stage = Stage::new();
    for _ in 0..history {
        rect(&mut stage, 100.0, [1.0, 0.0, 0.0, 1.0]);
    }
    let root = rect(&mut stage, -1.0, [0.0, 0.5, 1.0, 0.75]);
    stage.add_to_scene(root).unwrap();
    stage
}

fn terminal(stage: &Stage, compact: bool, limits: BundleExportLimits) -> SceneBundleRecorder {
    let mut recorder = if compact {
        SceneBundleRecorder::new_render_only(8, limits).unwrap()
    } else {
        SceneBundleRecorder::new(8, limits).unwrap()
    };
    recorder.capture_terminal_still(stage).unwrap();
    recorder
}

struct Paired {
    full: SceneBundleRecorder,
    compact: SceneBundleRecorder,
    packets: Vec<FramePacket>,
}

impl Paired {
    fn new() -> Self {
        Self {
            full: SceneBundleRecorder::new(8, BundleExportLimits::default()).unwrap(),
            compact: SceneBundleRecorder::new_render_only(8, BundleExportLimits::default())
                .unwrap(),
            packets: Vec::new(),
        }
    }

    fn finish(self) -> (RecordedSceneBundle, RecordedSceneBundle, Vec<FramePacket>) {
        (
            self.full.finish().unwrap(),
            self.compact.finish().unwrap(),
            self.packets,
        )
    }
}

impl SceneSink for Paired {
    fn event(&mut self, event: LifecycleEvent) -> Result<(), IntegrationError> {
        self.full.event(event)?;
        self.compact.event(event)
    }

    fn capture(
        &mut self,
        reason: CaptureReason,
        packet: FramePacket,
    ) -> Result<(), IntegrationError> {
        self.full.capture(reason, packet.clone())?;
        self.compact.capture(reason, packet.clone())?;
        self.packets.push(packet);
        Ok(())
    }
}

#[test]
fn terminal_export_ignores_history_and_accepts_the_same_exact_output_budget() {
    let clean = picture(0);
    let history = picture(256);
    let export = |stage: &Stage| {
        terminal(stage, true, BundleExportLimits::default())
            .finish()
            .unwrap()
    };
    let first = export(&clean);
    for _ in 0..3 {
        let next = terminal(&history, true, BundleExportLimits::default())
            .finish_with_max_bytes(first.bytes.len())
            .unwrap();
        assert_eq!(next.bytes, first.bytes);
        assert_eq!(next.digest, first.digest);
        assert_eq!((next.frame_count, next.segment_count), (1, 1));
    }
    assert!(matches!(
        terminal(&history, true, BundleExportLimits::default())
            .finish_with_max_bytes(first.bytes.len() - 1),
        Err(RecordingError::OutputLimit { .. })
    ));
    let full = terminal(&history, false, BundleExportLimits::default())
        .finish()
        .unwrap();
    assert!(full.bytes.len() > 20 * first.bytes.len());
    println!(
        "{{\"scenario\":\"render-only-terminal\",\"history\":256,\"full_bytes\":{},\"render_bytes\":{},\"sha256\":\"{}\"}}",
        full.bytes.len(),
        first.bytes.len(),
        first.digest.to_hex(),
    );
}

#[test]
fn observed_updaters_replay_the_same_pixels_without_retaining_their_copies() {
    let mut source = scene();
    let moving = rect(source.stage_mut(), -1.0, [0.0, 0.5, 1.0, 0.75]);
    source.stage_mut().add_to_scene(moving).unwrap();
    let calls = Rc::new(Cell::new(0));
    let observed = calls.clone();
    source
        .stage_mut()
        .add_dt_updater(
            moving,
            move |stage, mob, dt| {
                if dt > 0.0 {
                    observed.set(observed.get() + 1);
                    stage.shift(mob, [dt, 0.0, 0.0]);
                    // The old producer carries these invisible copies in later frames.
                    // Keep the live scene unchanged: no garbage-collection shortcut.
                    for _ in 0..8 {
                        stage.copy_family(mob).unwrap();
                    }
                }
            },
            false,
        )
        .unwrap();
    let mut paired = Paired::new();
    source.show(&mut paired).unwrap();
    source.wait(Some(0.5), &mut paired).unwrap();
    source.show(&mut paired).unwrap();
    let before_replay = calls.get();
    assert_eq!(before_replay, 4);
    let (full, compact, packets) = paired.finish();
    assert_eq!(full.frame_count, 6);
    assert_eq!(full.frame_count, compact.frame_count);
    assert_eq!(full.segment_count, compact.segment_count);
    assert!(full.bytes.len() > compact.bytes.len());
    let legacy = TimelineBundle::from_bytes(&full.bytes).unwrap();
    let replay = TimelineBundle::from_bytes(&compact.bytes).unwrap();
    for i in 0..compact.segment_count {
        assert_eq!(replay.segment_kind(i), Some(BundleSegmentKind::Stateful));
    }
    let mut reference_renderer = renderer(1);
    let expected: Vec<Vec<u8>> = packets
        .iter()
        .map(|packet| pixels(&mut reference_renderer, &packet.state().materialize()))
        .collect();
    let blank = pixels(&mut reference_renderer, &Stage::new());
    assert!(expected.iter().all(|frame| *frame != blank));
    assert!(
        expected
            .windows(2)
            .filter(|pair| pair[0] != pair[1])
            .count()
            >= 3
    );
    for threads in [1, 4, 16] {
        let mut output = renderer(threads);
        for index in [5, 0, 4, 1, 3, 2, 5] {
            assert_eq!(
                pixels(&mut output, &replay.stage_at(index).unwrap()),
                expected[index as usize]
            );
            assert_eq!(
                pixels(&mut output, &legacy.stage_at(index).unwrap()),
                expected[index as usize]
            );
        }
    }
    let shared = SharedTimelineBundle::from_bytes(&compact.bytes).unwrap();
    let jobs: Vec<_> = (0..compact.frame_count)
        .rev()
        .map(|index| shared.frame_job(index).unwrap())
        .collect();
    drop(shared);
    let workers: Vec<_> = jobs
        .into_iter()
        .map(|job| {
            std::thread::spawn(move || {
                let mut cache = TimelineFrameCache::default();
                let mut output = renderer(1);
                let frame = pixels(&mut output, &cache.materialize(&job).unwrap());
                (job.index(), frame)
            })
        })
        .collect();
    for worker in workers {
        let (index, frame) = worker.join().unwrap();
        assert_eq!(frame, expected[index as usize]);
    }
    assert_eq!(calls.get(), before_replay);
    assert!(source.stage().contains(moving));
    println!(
        "{{\"scenario\":\"render-only-updater\",\"frames\":6,\"updater_calls\":4,\"full_bytes\":{},\"render_bytes\":{},\"sha256\":\"{}\"}}",
        full.bytes.len(),
        compact.bytes.len(),
        compact.digest.to_hex(),
    );
}

#[test]
fn remapped_frame_identities_do_not_reuse_stale_retained_pixels() {
    let mut source = scene();
    // Allocate in the opposite order to first appearance, then swap the root.
    let blue = rect(source.stage_mut(), 1.0, [0.0, 0.0, 1.0, 1.0]);
    let red = rect(source.stage_mut(), -1.0, [1.0, 0.0, 0.0, 1.0]);
    source.stage_mut().add_to_scene(red).unwrap();
    let mut paired = Paired::new();
    source.show(&mut paired).unwrap();
    source.stage_mut().remove_from_scene(red);
    source.stage_mut().add_to_scene(blue).unwrap();
    source.show(&mut paired).unwrap();
    source.stage_mut().remove_from_scene(blue);
    source.show(&mut paired).unwrap();
    source.stage_mut().add_to_scene(red).unwrap();
    source.show(&mut paired).unwrap();
    let (_, compact, packets) = paired.finish();
    let replay = TimelineBundle::from_bytes(&compact.bytes).unwrap();
    let mut control = renderer(1);
    let expected: Vec<_> = packets
        .iter()
        .map(|packet| pixels(&mut control, &packet.state().materialize()))
        .collect();
    assert_eq!(expected[0], expected[3]);
    assert_ne!(expected[0], expected[1]);
    assert_ne!(expected[1], expected[2]);
    for threads in [1, 4, 16] {
        let mut output = renderer(threads);
        for index in [0, 1, 2, 3, 2, 1, 0] {
            assert_eq!(
                pixels(&mut output, &replay.stage_at(index).unwrap()),
                expected[index as usize]
            );
        }
    }
    assert!(source.stage().contains(red) && source.stage().contains(blue));
}

#[test]
fn capture_budget_charges_the_selected_representation_and_remains_sticky() {
    let stage = picture(256);
    let limits = BundleExportLimits {
        max_frames: 10,
        max_capture_bytes: 16 * 1024,
    };
    let compact = terminal(&stage, true, limits).finish().unwrap();
    assert!(compact.bytes.len() < 16 * 1024);
    let mut full = SceneBundleRecorder::new(8, limits).unwrap();
    assert!(full.capture_terminal_still(&stage).is_err());
    assert!(full.capture_terminal_still(&Stage::new()).is_err());
    assert_eq!(full.frame_count(), 0);
    assert!(matches!(
        full.finish(),
        Err(RecordingError::Bundle(
            BundleError::CaptureLimitExceeded { .. }
        ))
    ));
    let bytes = stage.snapshot().to_render_bytes().unwrap();
    let limits = BundleExportLimits {
        max_frames: 10,
        max_capture_bytes: bytes.len(),
    };
    let mut compact = SceneBundleRecorder::new_render_only(8, limits).unwrap();
    assert!(
        compact.capture_terminal_still(&stage).is_err(),
        "destination tables also cost bytes"
    );
    assert!(compact.finish().is_err());
}

#[test]
fn full_state_constructor_keeps_its_complete_snapshot_contract() {
    let stage = picture(32);
    let full = terminal(&stage, false, BundleExportLimits::default())
        .finish()
        .unwrap();
    let restored = TimelineBundle::from_bytes(&full.bytes)
        .unwrap()
        .stage_at(0)
        .unwrap();
    assert_eq!(
        stage.snapshot().to_bytes().unwrap(),
        restored.snapshot().to_bytes().unwrap()
    );
    assert_ne!(
        stage.snapshot().to_bytes().unwrap(),
        stage.snapshot().to_render_bytes().unwrap()
    );
}

#[test]
fn render_only_frame_refusal_cannot_publish_a_partial_artifact() {
    let mut source = scene();
    let limits = BundleExportLimits {
        max_frames: 1,
        ..BundleExportLimits::default()
    };
    let mut recorder = SceneBundleRecorder::new_render_only(8, limits).unwrap();
    source.show(&mut recorder).unwrap();
    assert!(source.show(&mut recorder).is_err());
    assert_eq!(recorder.frame_count(), 1);
    assert!(matches!(
        recorder.finish(),
        Err(RecordingError::Bundle(BundleError::FrameLimitExceeded {
            frames: 2,
            max_frames: 1
        }))
    ));
    assert!(SceneBundleRecorder::new_render_only(0, limits).is_err());
}

#[test]
fn equal_empty_frames_do_not_inherit_offscene_durable_resources() {
    let stage = picture(256);
    let mut source = stage.snapshot().materialize();
    let root = source.roots()[0];
    source.remove_from_scene(root);
    let a = terminal(&source, true, BundleExportLimits::default())
        .finish()
        .unwrap();
    let b = terminal(&Stage::new(), true, BundleExportLimits::default())
        .finish()
        .unwrap();
    assert_eq!(a.bytes, b.bytes);
    assert_eq!(a.digest, sha256(&a.bytes));
    assert!(source.contains(root));
}

struct ObservedCompact {
    recorder: SceneBundleRecorder,
    frames: Vec<Vec<u8>>,
}

impl SceneSink for ObservedCompact {
    fn event(&mut self, event: LifecycleEvent) -> Result<(), IntegrationError> {
        self.recorder.event(event)
    }

    fn capture(
        &mut self,
        reason: CaptureReason,
        packet: FramePacket,
    ) -> Result<(), IntegrationError> {
        self.frames.push(packet.state().to_render_bytes().unwrap());
        self.recorder.capture(reason, packet)
    }
}

#[test]
fn compact_recording_preserves_every_callback_frame_across_block_boundaries() {
    let mut source = scene();
    let moving = rect(source.stage_mut(), -1.0, [0.0, 0.5, 1.0, 0.75]);
    source.stage_mut().add_to_scene(moving).unwrap();
    let calls = Rc::new(Cell::new(0_u64));
    let observed = Rc::clone(&calls);
    source
        .stage_mut()
        .add_dt_updater(
            moving,
            move |stage, mob, dt| {
                if dt > 0.0 {
                    observed.set(observed.get() + 1);
                    stage.shift(mob, [dt / 64.0, 0.0, 0.0]);
                    if observed.get() == 65 {
                        stage.get_mut(mob).unwrap().buffer.write_range(
                            "fill_rgba",
                            0,
                            &[0.75, 0.25, 0.0, 0.75].repeat(9),
                        );
                    }
                }
            },
            false,
        )
        .unwrap();
    let mut sink = ObservedCompact {
        recorder: SceneBundleRecorder::new_compact_render_only(
            8,
            CompactRecordingLimits::default(),
        )
        .unwrap(),
        frames: Vec::new(),
    };
    source.wait_frames(130, &mut sink).unwrap();
    source.wait_frames(0, &mut sink).unwrap();
    source.show(&mut sink).unwrap();
    assert_eq!(calls.get(), 130);
    let artifact = sink.recorder.finish().unwrap();
    assert_eq!((artifact.frame_count, artifact.segment_count), (131, 3));
    assert_eq!(artifact.digest, sha256(&artifact.bytes));
    let raw_bytes: usize = sink.frames.iter().map(Vec::len).sum();
    assert!(
        artifact.bytes.len() < raw_bytes / 3,
        "compact recording retained {} of {raw_bytes} observed snapshot bytes",
        artifact.bytes.len(),
    );
    let bundle = TimelineBundle::from_bytes(&artifact.bytes).unwrap();
    for segment in 0..3 {
        assert_eq!(
            bundle.segment_kind(segment),
            Some(BundleSegmentKind::CompactRecorded),
        );
    }
    assert_eq!(bundle.segment_frame_range(0), Some(0..130));
    assert_eq!(bundle.segment_frame_range(1), Some(130..130));
    assert_eq!(bundle.segment_frame_range(2), Some(130..131));
    for index in (0..artifact.frame_count).rev() {
        let replay = bundle.stage_at(index).unwrap();
        assert_eq!(
            replay.snapshot().to_render_bytes().unwrap(),
            sink.frames[index as usize],
            "frame {index}",
        );
    }
    let mut expected_renderer = renderer(1);
    let mut actual_renderer = renderer(4);
    for index in [0, 63, 64, 65, 127, 128, 129, 130] {
        let binding = Stage::new();
        let expected = fmn_mobject::Snapshot::from_bytes(&sink.frames[index], &binding)
            .unwrap()
            .snapshot
            .materialize();
        assert_eq!(
            pixels(
                &mut actual_renderer,
                &bundle.stage_at(index as u32).unwrap()
            ),
            pixels(&mut expected_renderer, &expected),
        );
    }
    assert_eq!(calls.get(), 130, "replay must never invoke the updater");
}

#[test]
fn compact_storage_budget_does_not_redefine_legacy_snapshot_work() {
    let mut source = scene();
    let root = rect(source.stage_mut(), -1.0, [0.0, 0.5, 1.0, 0.75]);
    source.stage_mut().add_to_scene(root).unwrap();
    let frame_bytes = source.stage().snapshot().to_render_bytes().unwrap().len();
    let retained = 12 * frame_bytes + 16 * 1024;
    assert!(retained < 130 * frame_bytes);
    let mut compact = SceneBundleRecorder::new_compact_render_only(
        8,
        CompactRecordingLimits {
            max_retained_bytes: retained,
            max_snapshot_work_bytes: (130 * frame_bytes) as u64,
            ..CompactRecordingLimits::default()
        },
    )
    .unwrap();
    source.wait_frames(130, &mut compact).unwrap();
    let compact = compact.finish().unwrap();
    assert_eq!(compact.frame_count, 130);
    assert!(compact.bytes.len() < retained);

    let mut control = scene();
    let root = rect(control.stage_mut(), -1.0, [0.0, 0.5, 1.0, 0.75]);
    control.stage_mut().add_to_scene(root).unwrap();
    let mut legacy = SceneBundleRecorder::new_render_only(
        8,
        BundleExportLimits {
            max_frames: 130,
            max_capture_bytes: retained,
        },
    )
    .unwrap();
    assert!(control.wait_frames(130, &mut legacy).is_err());
    assert!(matches!(
        legacy.finish(),
        Err(RecordingError::Bundle(
            BundleError::CaptureLimitExceeded { .. }
        )),
    ));
}

#[test]
fn compact_snapshot_work_retained_and_output_limits_are_independent_and_sticky() {
    let stage = picture(0);
    let frame_bytes = stage.snapshot().to_render_bytes().unwrap().len();
    let limits = CompactRecordingLimits {
        max_snapshot_bytes: frame_bytes,
        max_snapshot_work_bytes: frame_bytes as u64,
        ..CompactRecordingLimits::default()
    };
    let mut exact = SceneBundleRecorder::new_compact_render_only(8, limits).unwrap();
    exact.capture_terminal_still(&stage).unwrap();
    let artifact = exact.finish().unwrap();
    let mut exact_output = SceneBundleRecorder::new_compact_render_only(8, limits).unwrap();
    exact_output.capture_terminal_still(&stage).unwrap();
    assert_eq!(
        exact_output
            .finish_with_max_bytes(artifact.bytes.len())
            .unwrap()
            .bytes,
        artifact.bytes,
    );
    let mut short_output = SceneBundleRecorder::new_compact_render_only(8, limits).unwrap();
    short_output.capture_terminal_still(&stage).unwrap();
    assert!(matches!(
        short_output.finish_with_max_bytes(artifact.bytes.len() - 1),
        Err(RecordingError::OutputLimit { limit, .. }) if limit == artifact.bytes.len() - 1,
    ));
    let mut short_snapshot = SceneBundleRecorder::new_compact_render_only(
        8,
        CompactRecordingLimits {
            max_snapshot_bytes: frame_bytes - 1,
            ..limits
        },
    )
    .unwrap();
    assert!(short_snapshot.capture_terminal_still(&stage).is_err());
    assert!(
        short_snapshot
            .capture_terminal_still(&Stage::new())
            .is_err()
    );
    assert!(matches!(
        short_snapshot.finish(),
        Err(RecordingError::SnapshotLimit { limit, .. }) if limit == frame_bytes - 1,
    ));
    let mut short_retained = SceneBundleRecorder::new_compact_render_only(
        8,
        CompactRecordingLimits {
            max_retained_bytes: frame_bytes,
            ..limits
        },
    )
    .unwrap();
    assert!(short_retained.capture_terminal_still(&stage).is_err());
    assert!(matches!(
        short_retained.finish(),
        Err(RecordingError::Bundle(
            BundleError::CaptureLimitExceeded { .. }
        )),
    ));

    let mut source = scene();
    let root = rect(source.stage_mut(), -1.0, [0.0, 0.5, 1.0, 0.75]);
    source.stage_mut().add_to_scene(root).unwrap();
    let bytes = source.stage().snapshot().to_render_bytes().unwrap().len();
    let mut work = SceneBundleRecorder::new_compact_render_only(
        8,
        CompactRecordingLimits {
            max_snapshot_work_bytes: (2 * bytes - 1) as u64,
            ..CompactRecordingLimits::default()
        },
    )
    .unwrap();
    assert!(source.wait_frames(2, &mut work).is_err());
    assert_eq!(work.frame_count(), 1);
    assert!(source.show(&mut work).is_err());
    assert!(matches!(
        work.finish(),
        Err(RecordingError::SnapshotWorkLimit { limit, .. }) if limit == (2 * bytes - 1) as u64,
    ));
}
