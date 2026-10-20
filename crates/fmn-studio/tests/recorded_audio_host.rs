//! Real authenticated HTTP transport of native recorded PCM; no audio device,
//! source-media decoder or scene worker is involved in these transport tests.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use fmn_cache::{NamespacePolicy, Store, StoreConfig};
use fmn_hash::sha256;
use fmn_platform::clock::{Clock, FakeClock};
use fmn_platform::fs::VirtualFs;
use fmn_scene::timeline_bundle::{BundleSoundtrack, attach_bundle_soundtrack};
use fmn_studio::{
    BuildError, CapabilityToken, FrameHub, RebuildDriver, StdWorkerLauncher, StudioHost,
    StudioHostConfig, StudioRecordedAudio, StudioWorkerSession, Supervisor, SupervisorConfig,
    WorkerArtifact,
};

struct NoBuild;
impl RebuildDriver for NoBuild {
    fn rebuild(&mut self) -> Result<WorkerArtifact, BuildError> {
        Err(BuildError::new("audio transport must not request a rebuild"))
    }
}

fn recording() -> StudioRecordedAudio {
    let mut timeline = fmn_anim::Timeline::new(3).unwrap();
    timeline.wait(1.0).unwrap();
    let pictures = fmn_scene::export_timeline_bundle(
        timeline,
        &mut fmn_mobject::Stage::new(),
        &fmn_core::rng::RngRoot::from_seed(0),
    )
    .unwrap();
    let pcm = BundleSoundtrack::new(
        10,
        2,
        (0_u8..14)
            .flat_map(|n| [f32::from(n) / 32.0, -f32::from(n) / 32.0])
            .collect(),
    )
    .unwrap();
    let bytes = attach_bundle_soundtrack(&pictures, &pcm, fmn_scene::DEFAULT_MAX_BUNDLE_BYTES).unwrap();
    StudioRecordedAudio::decode("compiled/lecture.fmtl", &bytes).unwrap().1.unwrap()
}

fn host(audio: &StudioRecordedAudio, clock: Arc<FakeClock>, allowed: Arc<AtomicBool>, config: StudioHostConfig) -> StudioHost {
    let clock: Arc<dyn Clock> = clock;
    let cache = Store::open(
        Arc::new(VirtualFs::new()), Arc::clone(&clock),
        std::env::temp_dir().join("fmn-recorded-audio-http"), StoreConfig::default(),
    ).unwrap().namespace("studio-replay", 1, NamespacePolicy { ceiling_bytes: None }).unwrap();
    let supervisor = Supervisor::new(Box::new(StdWorkerLauncher::default()), Arc::clone(&clock), cache, SupervisorConfig::default());
    let source = audio.source().clone();
    let session = Arc::new(StudioWorkerSession::new(
        "Lecture", supervisor, Box::new(NoBuild),
        Arc::new(move |read| allowed.load(Ordering::Acquire) && read == &source),
    ).unwrap());
    StudioHost::bind(
        session, FrameHub::new(1, 1024).unwrap(), CapabilityToken::new([7; 32]).unwrap(), clock, config,
    ).unwrap()
}

fn request(host: &StudioHost, audio: &StudioRecordedAudio, path: &str, token: Option<&str>, origin: Option<&str>) -> Vec<u8> {
    let address = host.local_addr().unwrap();
    std::thread::scope(|scope| {
        let mut client = TcpStream::connect(address).unwrap();
        client.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        client.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
        let server = scope.spawn(|| host.serve_recorded_once(audio));
        write!(client, "GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n").unwrap();
        if let Some(token) = token { write!(client, "X-FMN-Capability: {token}\r\n").unwrap(); }
        if let Some(origin) = origin { write!(client, "Origin: {origin}\r\n").unwrap(); }
        client.write_all(b"\r\n").unwrap();
        let mut bytes = Vec::new(); client.read_to_end(&mut bytes).unwrap();
        server.join().unwrap().unwrap(); bytes
    })
}

fn body(response: &[u8]) -> &[u8] {
    let end = response.windows(4).position(|bytes| bytes == b"\r\n\r\n").unwrap() + 4;
    &response[end..]
}
fn token() -> String { CapabilityToken::new([7; 32]).unwrap().try_expose_hex().unwrap() }
fn chunk(audio: &StudioRecordedAudio, start: u64, frames: u32) -> String {
    format!("/api/audio/chunk?source={}&worker_generation=0&start={start}&frames={frames}", audio.info().source.to_hex())
}

#[test]
fn authenticated_metadata_and_original_pcm_are_exact_and_do_not_touch_the_worker() {
    let audio = recording();
    let host = host(&audio, Arc::new(FakeClock::new()), Arc::new(AtomicBool::new(true)), StudioHostConfig::default());
    let metadata = request(&host, &audio, "/api/audio", Some(&token()), None);
    assert!(metadata.starts_with(b"HTTP/1.1 200 "));
    let text = std::str::from_utf8(body(&metadata)).unwrap();
    assert!(text.contains("\"sample_frames\":14,\"fps\":3,\"picture_frames\":3"));
    assert!(text.contains(&audio.info().source.to_hex()));
    let response = request(&host, &audio, &chunk(&audio, 11, 7), Some(&token()), None);
    assert!(response.starts_with(b"HTTP/1.1 200 "));
    let expected: Vec<_> = (11_u8..14).flat_map(|n| [f32::from(n) / 32.0, -f32::from(n) / 32.0]).flat_map(f32::to_le_bytes).collect();
    assert_eq!(body(&response), expected);
    let header = String::from_utf8_lossy(&response[..response.len() - expected.len()]);
    assert!(header.contains("application/vnd.frankenmanim.pcm-f32le"));
    assert!(header.contains("X-FMN-Audio-Start: 11\r\n"));
    assert!(header.contains("X-FMN-Audio-Frames: 3\r\n"));
    assert!(header.contains("X-FMN-Audio-Next: 14\r\n"));
    assert!(header.contains("X-FMN-Audio-End: true\r\n"));
    assert!(header.contains(&sha256(&expected).to_hex()));
    assert!(header.contains("Cache-Control: no-store"));
    let eof = request(&host, &audio, &chunk(&audio, 14, 1), Some(&token()), None);
    assert!(eof.starts_with(b"HTTP/1.1 200 "));
    assert!(body(&eof).is_empty());
    let backwards = request(&host, &audio, &chunk(&audio, 0, 1), Some(&token()), None);
    assert_eq!(body(&backwards), [0; 8]);
    let index = request(&host, &audio, "/", Some(&token()), None);
    assert!(index.starts_with(b"HTTP/1.1 200 "));
    assert!(String::from_utf8_lossy(body(&index)).contains("FrankenManim Studio"));
}

#[test]
fn authorization_origin_rotation_and_expiry_also_guard_audio() {
    let audio = recording(); let clock = Arc::new(FakeClock::new());
    let host = host(&audio, clock.clone(), Arc::new(AtomicBool::new(true)), StudioHostConfig {
        session_ttl: Duration::from_secs(1), ..StudioHostConfig::default()
    });
    for path in ["/api/audio".to_owned(), chunk(&audio, 0, 1)] {
        assert!(request(&host, &audio, &path, None, None).starts_with(b"HTTP/1.1 403 "));
        assert!(request(&host, &audio, &path, Some("wrong"), None).starts_with(b"HTTP/1.1 403 "));
        assert!(request(&host, &audio, &path, Some(&token()), Some("https://other.example")).starts_with(b"HTTP/1.1 403 "));
    }
    let next = CapabilityToken::new([8; 32]).unwrap(); let next_text = next.try_expose_hex().unwrap();
    host.rotate_capability(next);
    assert!(request(&host, &audio, "/api/audio", Some(&token()), None).starts_with(b"HTTP/1.1 403 "));
    assert!(request(&host, &audio, "/api/audio", Some(&next_text), None).starts_with(b"HTTP/1.1 200 "));
    clock.advance(Duration::from_secs(2));
    assert!(request(&host, &audio, "/api/audio", Some(&next_text), None).starts_with(b"HTTP/1.1 401 "));
}

#[test]
fn stale_generations_sources_and_malformed_windows_fail_closed() {
    let audio = recording(); let allowed = Arc::new(AtomicBool::new(true));
    let host = host(&audio, Arc::new(FakeClock::new()), allowed.clone(), StudioHostConfig::default());
    let valid = chunk(&audio, 0, 1);
    for path in [
        valid.replace("worker_generation=0", "worker_generation=1"),
        valid.replace(&audio.info().source.to_hex(), &"0".repeat(64)),
        format!("{valid}&frames=2"), format!("{valid}&file=/etc/passwd"),
        chunk(&audio, 0, 0), chunk(&audio, 0, 65_537), chunk(&audio, u64::MAX, 1),
        "/api/audio/chunk".to_owned(),
    ] {
        assert!(request(&host, &audio, &path, Some(&token()), None).starts_with(b"HTTP/1.1 400 "), "{path}");
    }
    allowed.store(false, Ordering::Release);
    assert!(request(&host, &audio, &valid, Some(&token()), None).starts_with(b"HTTP/1.1 400 "));
    allowed.store(true, Ordering::Release);
    assert!(request(&host, &audio, &valid, Some(&token()), None).starts_with(b"HTTP/1.1 200 "));
}

#[test]
fn audio_uses_the_existing_rate_limit_and_bounded_shutdown_path() {
    let audio = recording();
    let host = host(&audio, Arc::new(FakeClock::new()), Arc::new(AtomicBool::new(true)), StudioHostConfig {
        max_requests_per_window: 1, ..StudioHostConfig::default()
    });
    assert!(request(&host, &audio, "/api/audio", Some(&token()), None).starts_with(b"HTTP/1.1 200 "));
    assert!(request(&host, &audio, &chunk(&audio, 0, 1), Some(&token()), None).starts_with(b"HTTP/1.1 429 "));
    host.serve_recorded_until(audio, &AtomicBool::new(true)).unwrap();
}
