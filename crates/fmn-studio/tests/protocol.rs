//! fm-39s canonical IPC acceptance: every message class round-trips, the
//! request/response bytes are self-goldened, incompatible versions and
//! malformed ranges fail closed, and the outer pipe rejects oversized lengths
//! before allocating.

use std::io::Cursor;

use fmn_hash::{Schema, Writer, sha256};
use fmn_scene::{
    CommandKind, CommandRecord, EffectClass, Entry, EventPayload, Journal, Key, Modifiers,
};
use fmn_studio::{
    CURRENT_VERSION, Checkpoint, CrashReport, DebugLayerSet, FrameEncoding, FramePayload,
    FrameStream, FramingError, JournalReplay, ProtocolError, ProtocolLimits, ProtocolVersion,
    REQUEST_SCHEMA, RequestEnvelope, ResponseEnvelope, StudioDataKind, SupervisorRequest,
    TransportCapabilities, WorkerErrorCode, WorkerResponse, read_request, read_response,
    write_request, write_response,
};

fn command(label: &str) -> CommandRecord {
    CommandRecord {
        kind: CommandKind::Play,
        identity: sha256(label.as_bytes()),
        label: label.to_owned(),
    }
}

fn journal() -> Journal {
    let state = b"canonical checkpoint".to_vec();
    let mut journal = Journal::new();
    journal.record(Entry {
        command: command("play FadeIn(dot)"),
        effect: EffectClass::Pure,
        reads: Vec::new(),
        subprocesses: Vec::new(),
        checkpoint: Some(state.clone()),
        state_hash: sha256(&state),
    });
    journal
}

fn checkpoint() -> Checkpoint {
    let state = b"canonical checkpoint".to_vec();
    Checkpoint {
        scene: "Demo".to_owned(),
        after_entry: 0,
        state_hash: sha256(&state),
        state,
    }
}

fn request(request_id: u64, request: SupervisorRequest) -> RequestEnvelope {
    RequestEnvelope {
        request_id,
        request,
    }
}

fn response(request_id: u64, response: WorkerResponse) -> ResponseEnvelope {
    ResponseEnvelope {
        request_id,
        response,
    }
}

#[test]
fn every_request_variant_round_trips_canonically() {
    let limits = ProtocolLimits::default();
    let replay = JournalReplay {
        scene: "Demo".to_owned(),
        from_entry: 0,
        through_entry: 1,
        journal: journal().to_bytes().unwrap(),
    };
    let variants = vec![
        SupervisorRequest::Hello {
            version: CURRENT_VERSION,
            supervisor_build: sha256(b"supervisor"),
            max_frame_bytes: 1024,
        },
        SupervisorRequest::EnumerateScenes,
        SupervisorRequest::Play {
            scene: "Demo".to_owned(),
            command: command("play FadeIn(dot)"),
        },
        SupervisorRequest::Seek {
            scene: "Demo".to_owned(),
            frame: 42,
        },
        SupervisorRequest::Scrub {
            scene: "Demo".to_owned(),
            frame: 17,
        },
        SupervisorRequest::RestoreCheckpoint(checkpoint()),
        SupervisorRequest::ReplayJournal(replay),
        SupervisorRequest::Event {
            scene: "Demo".to_owned(),
            event: EventPayload::KeyPress {
                key: Key::Character('k'),
                modifiers: Modifiers::CONTROL | Modifiers::SHIFT,
            },
        },
        SupervisorRequest::Inspect {
            scene: "Demo".to_owned(),
        },
        SupervisorRequest::Overlay {
            scene: "Demo".to_owned(),
            layers: DebugLayerSet::TILES | DebugLayerSet::DEPTH,
        },
        SupervisorRequest::Shutdown,
    ];
    for (index, variant) in variants.into_iter().enumerate() {
        let envelope = request(index as u64 + 1, variant);
        let bytes = envelope.to_bytes(limits).unwrap();
        let decoded = RequestEnvelope::from_bytes(&bytes, limits).unwrap();
        assert_eq!(decoded, envelope);
        assert_eq!(decoded.to_bytes(limits).unwrap(), bytes);
    }
}

#[test]
fn every_response_variant_round_trips_canonically() {
    let limits = ProtocolLimits::default();
    let rgba = vec![0, 1, 2, 3, 4, 5, 6, 7];
    let png = b"\x89PNG\r\n\x1a\nfixture".to_vec();
    let journal = journal().to_bytes().unwrap();
    let inspection = br#"{"scene":"Demo","nodes":[]}"#.to_vec();
    let variants = vec![
        WorkerResponse::Hello {
            version: CURRENT_VERSION,
            worker_build: sha256(b"worker"),
            transports: TransportCapabilities {
                pipe: true,
                shared_memory: true,
            },
        },
        WorkerResponse::Scenes(vec!["Demo".to_owned(), "Other".to_owned()]),
        WorkerResponse::Ack {
            state_hash: Some(sha256(b"state")),
            journal_len: 4,
        },
        WorkerResponse::Frame(FrameStream {
            scene: "Demo".to_owned(),
            frame_index: 7,
            width: 2,
            height: 1,
            stride: 8,
            encoding: FrameEncoding::Rgba8,
            payload: FramePayload::Pipe {
                digest: sha256(&rgba),
                bytes: rgba,
            },
        }),
        WorkerResponse::Frame(FrameStream {
            scene: "Demo".to_owned(),
            frame_index: 8,
            width: 1920,
            height: 1080,
            stride: 0,
            encoding: FrameEncoding::Png,
            payload: FramePayload::Pipe {
                digest: sha256(&png),
                bytes: png,
            },
        }),
        WorkerResponse::Frame(FrameStream {
            scene: "Demo".to_owned(),
            frame_index: 9,
            width: 2,
            height: 1,
            stride: 8,
            encoding: FrameEncoding::Rgba8,
            payload: FramePayload::SharedMemory {
                token: sha256(b"opaque region token"),
                len: 8,
                digest: sha256(b"region contents"),
            },
        }),
        WorkerResponse::Checkpoint(checkpoint()),
        WorkerResponse::JournalSegment {
            scene: "Demo".to_owned(),
            start_entry: 3,
            journal,
        },
        WorkerResponse::ReplayComplete {
            from_entry: 2,
            state_hashes: vec![sha256(b"s2"), sha256(b"s3")],
        },
        WorkerResponse::Crash(CrashReport {
            scene: Some("Demo".to_owned()),
            message: "scene panic".to_owned(),
            journal_tail: b"tail".to_vec(),
            state_hash: Some(sha256(b"last state")),
        }),
        WorkerResponse::Error {
            code: WorkerErrorCode::ReplayFailed,
            message: "replay refused".to_owned(),
        },
        WorkerResponse::StudioData {
            scene: "Demo".to_owned(),
            kind: StudioDataKind::Inspection,
            digest: sha256(&inspection),
            bytes: inspection,
        },
        WorkerResponse::Bye,
    ];
    for (index, variant) in variants.into_iter().enumerate() {
        let envelope = response(index as u64 + 1, variant);
        let bytes = envelope.to_bytes(limits).unwrap();
        let decoded = ResponseEnvelope::from_bytes(&bytes, limits).unwrap();
        assert_eq!(decoded, envelope);
        assert_eq!(decoded.to_bytes(limits).unwrap(), bytes);
    }
}

#[test]
fn request_and_response_bytes_are_self_goldened() {
    let limits = ProtocolLimits::default();
    let request = request(
        0x0102_0304_0506_0708,
        SupervisorRequest::Play {
            scene: "GoldenScene".to_owned(),
            command: command("play golden"),
        },
    );
    let response = response(
        0x0102_0304_0506_0708,
        WorkerResponse::Crash(CrashReport {
            scene: Some("GoldenScene".to_owned()),
            message: "golden crash".to_owned(),
            journal_tail: b"tail".to_vec(),
            state_hash: Some(sha256(b"state")),
        }),
    );
    let actual = format!(
        "request={}\nresponse={}\n",
        hex(&request.to_bytes(limits).unwrap()),
        hex(&response.to_bytes(limits).unwrap())
    );
    assert_eq!(
        actual,
        include_str!("../fixtures/protocol_v1.hex"),
        "protocol bytes moved; review the schema and deliberately re-bless the fixture"
    );
}

#[test]
fn live_and_container_version_skew_are_rejected() {
    let peer = ProtocolVersion { major: 2, minor: 0 };
    assert!(matches!(
        peer.require_current(),
        Err(ProtocolError::VersionSkew {
            local: CURRENT_VERSION,
            peer: found,
        }) if found == peer
    ));

    let limits = ProtocolLimits::default();
    let mut writer = Writer::new(Schema::new(
        REQUEST_SCHEMA.magic,
        REQUEST_SCHEMA.id,
        REQUEST_SCHEMA.major + 1,
        0,
    ));
    writer.put_u64(1);
    writer.put_u8(1);
    let foreign = writer.finish().unwrap();
    assert!(matches!(
        RequestEnvelope::from_bytes(&foreign, limits),
        Err(ProtocolError::Serial(
            fmn_hash::SerialError::MajorMismatch { reader: 1, doc: 2 }
        ))
    ));
}

#[test]
fn malformed_semantics_fail_before_crossing_the_pipe() {
    let limits = ProtocolLimits::default();
    let mut bad_checkpoint = checkpoint();
    bad_checkpoint.state.push(0);
    assert!(matches!(
        request(1, SupervisorRequest::RestoreCheckpoint(bad_checkpoint)).to_bytes(limits),
        Err(ProtocolError::Malformed("checkpoint state hash"))
    ));

    let replay = JournalReplay {
        scene: "Demo".to_owned(),
        from_entry: 0,
        through_entry: 2,
        journal: journal().to_bytes().unwrap(),
    };
    assert!(matches!(
        request(2, SupervisorRequest::ReplayJournal(replay)).to_bytes(limits),
        Err(ProtocolError::Malformed("replay range exceeds journal"))
    ));

    assert!(matches!(
        request(
            3,
            SupervisorRequest::Event {
                scene: "Demo".to_owned(),
                event: EventPayload::MouseMotion {
                    point: [f64::INFINITY, 0.0, 0.0],
                    delta: [0.0, 0.0, 0.0],
                    modifiers: Modifiers::NONE,
                },
            }
        )
        .to_bytes(limits),
        Err(ProtocolError::Malformed("invalid input event"))
    ));

    let bytes = br#"{"nodes":[]}"#.to_vec();
    assert!(matches!(
        response(
            4,
            WorkerResponse::StudioData {
                scene: "Demo".to_owned(),
                kind: StudioDataKind::Inspection,
                digest: sha256(b"different"),
                bytes,
            }
        )
        .to_bytes(limits),
        Err(ProtocolError::Malformed("Studio data digest"))
    ));

    let malformed_json = b"{]".to_vec();
    assert!(matches!(
        response(
            5,
            WorkerResponse::StudioData {
                scene: "Demo".to_owned(),
                kind: StudioDataKind::Inspection,
                digest: sha256(&malformed_json),
                bytes: malformed_json,
            }
        )
        .to_bytes(limits),
        Err(ProtocolError::Malformed("Studio data is not valid JSON"))
    ));

    let mut writer = Writer::new(REQUEST_SCHEMA);
    writer.put_u64(6);
    writer.put_u8(99);
    assert!(matches!(
        RequestEnvelope::from_bytes(&writer.finish().unwrap(), limits),
        Err(ProtocolError::Malformed("supervisor request tag"))
    ));
}

#[test]
fn length_framing_round_trips_and_rejects_oversize_before_allocation() {
    let limits = ProtocolLimits::default();
    let request = request(1, SupervisorRequest::EnumerateScenes);
    let mut pipe = Vec::new();
    write_request(&mut pipe, &request, limits).unwrap();
    assert_eq!(
        read_request(&mut Cursor::new(pipe), limits).unwrap(),
        request
    );

    let response = response(1, WorkerResponse::Scenes(vec!["Demo".to_owned()]));
    let mut pipe = Vec::new();
    write_response(&mut pipe, &response, limits).unwrap();
    assert_eq!(
        read_response(&mut Cursor::new(pipe), limits).unwrap(),
        response
    );

    let tight = ProtocolLimits {
        max_message_bytes: 64,
        ..limits
    };
    let prefix = 65u64.to_le_bytes();
    assert!(matches!(
        read_request(&mut Cursor::new(prefix), tight),
        Err(FramingError::FrameTooLarge {
            limit: 64,
            needed: 65,
        })
    ));
}

#[test]
fn response_collection_counts_are_preflighted_before_reserve() {
    let limits = ProtocolLimits::default();

    let mut scenes = Writer::new(fmn_studio::RESPONSE_SCHEMA);
    scenes.put_u64(41);
    scenes.put_u8(1);
    scenes.put_u32(u32::try_from(limits.max_scenes).unwrap());
    assert_eq!(
        ResponseEnvelope::from_bytes(&scenes.finish().unwrap(), limits),
        Err(ProtocolError::CollectionPayloadTooShort {
            field: "scene",
            count: limits.max_scenes,
            minimum_bytes: u64::try_from(limits.max_scenes).unwrap() * 8,
            remaining_bytes: 0,
        })
    );

    let mut replay = Writer::new(fmn_studio::RESPONSE_SCHEMA);
    replay.put_u64(42);
    replay.put_u8(6);
    replay.put_u64(0);
    replay.put_u32(u32::try_from(limits.max_replay_hashes).unwrap());
    assert_eq!(
        ResponseEnvelope::from_bytes(&replay.finish().unwrap(), limits),
        Err(ProtocolError::CollectionPayloadTooShort {
            field: "replay state hash",
            count: limits.max_replay_hashes,
            minimum_bytes: u64::try_from(limits.max_replay_hashes).unwrap() * 32,
            remaining_bytes: 0,
        })
    );
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}
