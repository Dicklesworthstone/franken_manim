//! The soundtrack shares the actual captured picture generation and byte cap.
use super::*;
use fmn_scene::timeline_bundle::BundleSoundtrack;

fn timeline(value: u8, max_bytes: usize) -> RecordedTimeline {
    let mut timeline = RecordedTimeline::new(
        "Sound".into(),
        sha256(b"build"),
        sha256(b"source"),
        4,
        max_bytes,
    )
    .unwrap();
    let (frame, inspection) = super::tests::capture("Sound", value);
    timeline.push(frame, inspection).unwrap();
    timeline
}

fn pcm(value: f32) -> BundleSoundtrack {
    BundleSoundtrack::new(48_000, 2, vec![value, -value, 0.125, -0.5]).unwrap()
}

fn info(timeline: &mut RecordedTimeline) -> Option<crate::StudioAudioInfo> {
    match timeline
        .handle(SupervisorRequest::AudioInfo {
            scene: "Sound".into(),
        })
        .unwrap()
    {
        WorkerResponse::AudioInfo {
            info,
            fps,
            picture_frames,
            ..
        } => {
            assert_eq!((fps, picture_frames), (30, 1));
            info
        }
        _ => panic!("expected audio metadata"),
    }
}

#[test]
fn immutable_soundtrack_binds_actual_pcm_and_pictures_and_does_not_advance() {
    let mut recorded = timeline(32, 1 << 20);
    let occupied = recorded.encoded_bytes();
    let original_state = recorded.last_state_hash();
    assert!(info(&mut recorded).is_none());
    recorded.attach_soundtrack(pcm(0.25)).unwrap();
    assert_eq!(recorded.encoded_bytes(), occupied + 16);
    let metadata = info(&mut recorded).unwrap();
    assert_eq!(
        (
            metadata.channels,
            metadata.sample_rate,
            metadata.sample_frames
        ),
        (2, 48_000, 2)
    );
    for start in [1, 0, 2, 1] {
        let request = SupervisorRequest::AudioChunk {
            scene: "Sound".into(),
            source: metadata.source,
            start,
            frames: 65_536,
        };
        let WorkerResponse::AudioChunk { chunk, .. } = recorded.handle(request).unwrap() else {
            panic!()
        };
        let expected: Vec<_> = pcm(0.25).samples()[start as usize * 2..]
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        assert_eq!(chunk.bytes(), expected);
        assert_eq!(chunk.next(), 2);
        assert_eq!(recorded.last_state_hash(), original_state);
        assert_eq!(
            recorded.commands, 0,
            "audio reads must not journal scene commands"
        );
    }
    for (source, start, frames) in [
        (sha256(b"stale"), 0, 1),
        (metadata.source, 3, 1),
        (metadata.source, 0, 0),
        (metadata.source, 0, 65_537),
    ] {
        assert!(
            recorded
                .handle(SupervisorRequest::AudioChunk {
                    scene: "Sound".into(),
                    source,
                    start,
                    frames,
                })
                .is_err()
        );
    }
    for (pixel, sample) in [(32, 0.25), (33, 0.25), (32, 0.5)] {
        let mut candidate = timeline(pixel, 1 << 20);
        candidate.attach_soundtrack(pcm(sample)).unwrap();
        assert_eq!(
            info(&mut candidate).unwrap().source == metadata.source,
            pixel == 32 && sample == 0.25
        );
    }
}

#[test]
fn soundtrack_refusal_preserves_picture_and_combined_budget() {
    let mut recorded = timeline(16, 1 << 20);
    let before = (recorded.encoded_bytes(), recorded.last_state_hash());
    recorded.max_bytes = recorded.encoded_bytes() + 15;
    assert!(recorded.attach_soundtrack(pcm(0.25)).is_err());
    assert!(info(&mut recorded).is_none());
    assert_eq!(
        (recorded.encoded_bytes(), recorded.last_state_hash()),
        before
    );
    recorded.max_bytes += 1;
    recorded.attach_soundtrack(pcm(0.25)).unwrap();
    assert_eq!(recorded.remaining_bytes(), 0);
    let source = info(&mut recorded).unwrap().source;
    assert!(recorded.attach_soundtrack(pcm(0.5)).is_err());
    assert_eq!(info(&mut recorded).unwrap().source, source);
    let (frame, inspection) = super::tests::capture("Sound", 64);
    assert!(recorded.push(frame, inspection).is_err());
    assert_eq!(recorded.frame_count(), 1);
}

#[test]
fn live_replacement_invalidates_audio_only_when_new_picture_is_admitted() {
    let mut recorded = timeline(16, 1 << 20);
    recorded.attach_soundtrack(pcm(0.25)).unwrap();
    let source = info(&mut recorded).unwrap().source;
    recorded.enable_live_input().unwrap();
    let (mut frame, inspection) = super::tests::capture("Sound", 64);
    frame.width += 1;
    assert!(recorded.replace_live_frame(frame, inspection).is_err());
    assert_eq!(info(&mut recorded).unwrap().source, source);
    let (frame, inspection) = super::tests::capture("Sound", 64);
    recorded.replace_live_frame(frame, inspection).unwrap();
    assert!(info(&mut recorded).is_none());
    assert_eq!(recorded.audio_bytes, 0);
    assert!(
        recorded
            .handle(SupervisorRequest::AudioChunk {
                scene: "Sound".into(),
                source,
                start: 0,
                frames: 1,
            })
            .is_err()
    );
}
