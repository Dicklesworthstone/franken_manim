//! Real host authorization, bounded PCM routing and atomic worker generation
//! changes. The injected channel uses the production binary codec and PCM
//! chunker; Python capture/mixing is exercised separately by the portal suite.
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::Duration;

use fmn_hash::sha256;
use fmn_platform::{
    clock::{Clock, FakeClock},
    fs::VirtualFs,
};
use fmn_scene::{
    Journal, RenderBackendRecord, RenderBackendRole, timeline_bundle::BundleSoundtrack,
};
use fmn_studio::*;

struct Builder {
    build: usize,
    fail: Arc<AtomicBool>,
}
impl RebuildDriver for Builder {
    fn rebuild(&mut self) -> Result<WorkerArtifact, BuildError> {
        if self.fail.load(Ordering::Acquire) {
            return Err(BuildError::new("replacement refused"));
        }
        self.build += 1;
        Ok(WorkerArtifact {
            executable: std::env::current_exe().unwrap(),
            argv: Vec::new(),
            env: Vec::new(),
            cwd: None,
            build_id: sha256(format!("sound build {}", self.build).as_bytes()),
            prepare_scene: Some("Sound".into()),
        })
    }
}

struct Launcher {
    calls: Arc<AtomicUsize>,
}
impl WorkerLauncher for Launcher {
    fn launch(
        &mut self,
        artifact: &WorkerArtifact,
        limits: ProtocolLimits,
    ) -> Result<Box<dyn WorkerChannel>, LaunchError> {
        Ok(Box::new(Channel {
            track: StudioAudioTrack::new(
                artifact.build_id,
                BundleSoundtrack::new(48_000, 2, [0.25, -0.5].repeat(4)).unwrap(),
            ),
            calls: Arc::clone(&self.calls),
            limits,
        }))
    }
}
struct Channel {
    track: StudioAudioTrack,
    calls: Arc<AtomicUsize>,
    limits: ProtocolLimits,
}
impl WorkerChannel for Channel {
    fn exchange(
        &mut self,
        request: &RequestEnvelope,
        _: Duration,
    ) -> Result<ResponseEnvelope, ChannelError> {
        let response = match &request.request {
            SupervisorRequest::Hello { .. } => WorkerResponse::Hello {
                version: CURRENT_VERSION,
                worker_build: self.track.info().source,
                transports: TransportCapabilities {
                    pipe: true,
                    shared_memory: false,
                },
            },
            SupervisorRequest::Scrub { scene, .. } => {
                let bytes = fmn_codec::encode_rgba8(
                    1,
                    1,
                    &[32, 64, 96, 255],
                    fmn_codec::CompressionLevel::Fast,
                );
                WorkerResponse::Frame(FrameStream {
                    scene: scene.clone(),
                    frame_index: 0,
                    width: 1,
                    height: 1,
                    stride: 0,
                    encoding: FrameEncoding::Png,
                    payload: FramePayload::Pipe {
                        digest: sha256(&bytes),
                        bytes,
                    },
                    render_backends: vec![
                        RenderBackendRecord::new(
                            RenderBackendRole::FrameStream,
                            b"audio-host-fixture".to_vec(),
                        )
                        .unwrap(),
                    ],
                })
            }
            SupervisorRequest::AudioInfo { scene } => {
                self.calls.fetch_add(1, Ordering::AcqRel);
                WorkerResponse::AudioInfo {
                    scene: scene.clone(),
                    info: Some(self.track.info()),
                    fps: 30,
                    picture_frames: 1,
                }
            }
            SupervisorRequest::AudioChunk {
                scene,
                source,
                start,
                frames,
            } => {
                self.calls.fetch_add(1, Ordering::AcqRel);
                match self.track.chunk(*source, *start, *frames) {
                    Ok(chunk) => WorkerResponse::AudioChunk {
                        scene: scene.clone(),
                        chunk,
                    },
                    Err(error) => WorkerResponse::Error {
                        code: WorkerErrorCode::InvalidRequest,
                        message: error.to_string(),
                    },
                }
            }
            SupervisorRequest::Shutdown => WorkerResponse::Bye,
            _ => WorkerResponse::Error {
                code: WorkerErrorCode::InvalidRequest,
                message: "unsupported fixture operation".into(),
            },
        };
        let response = ResponseEnvelope {
            request_id: request.request_id,
            response,
        };
        let bytes = response.to_bytes(self.limits).unwrap();
        Ok(ResponseEnvelope::from_bytes(&bytes, self.limits).unwrap())
    }
    fn terminate(&mut self) {}
}

fn host(fail: Arc<AtomicBool>, calls: Arc<AtomicUsize>) -> (StudioHost, Arc<StudioWorkerSession>) {
    let clock: Arc<dyn Clock> = Arc::new(FakeClock::new());
    let cache = fmn_cache::Store::open(
        Arc::new(VirtualFs::new()),
        Arc::clone(&clock),
        std::env::temp_dir().join("worker-audio-http"),
        fmn_cache::StoreConfig::default(),
    )
    .unwrap()
    .namespace(
        "audio",
        1,
        fmn_cache::NamespacePolicy {
            ceiling_bytes: None,
        },
    )
    .unwrap();
    let mut supervisor = Supervisor::new(
        Box::new(Launcher { calls }),
        Arc::clone(&clock),
        cache,
        SupervisorConfig {
            auto_restart: false,
            ..SupervisorConfig::default()
        },
    );
    supervisor.install_session("Sound", Journal::new()).unwrap();
    let mut builder = Box::new(Builder { build: 0, fail });
    supervisor.build_and_start(&mut *builder).unwrap();
    let session = Arc::new(
        StudioWorkerSession::new("Sound", supervisor, builder, Arc::new(|_| false))
            .unwrap()
            .with_fresh_capture_reload(),
    );
    let host = StudioHost::bind(
        Arc::clone(&session),
        FrameHub::new(2, 1024 * 1024).unwrap(),
        CapabilityToken::new([7; 32]).unwrap(),
        clock,
        StudioHostConfig::default(),
    )
    .unwrap();
    (host, session)
}

fn request(host: &StudioHost, path: &str, authenticated: bool, post: bool) -> Vec<u8> {
    let address = host.local_addr().unwrap();
    std::thread::scope(|scope| {
        let mut client = TcpStream::connect(address).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let server = scope.spawn(|| host.serve_once());
        write!(
            client,
            "{} {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n",
            if post { "POST" } else { "GET" }
        )
        .unwrap();
        if authenticated {
            write!(
                client,
                "X-FMN-Capability: {}\r\n",
                CapabilityToken::new([7; 32])
                    .unwrap()
                    .try_expose_hex()
                    .unwrap()
            )
            .unwrap();
        }
        if post {
            write!(client, "Origin: http://{address}\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: 0\r\n").unwrap();
        }
        client.write_all(b"\r\n").unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).unwrap();
        server.join().unwrap().unwrap();
        response
    })
}

fn body(response: &[u8]) -> &[u8] {
    let offset = response
        .windows(4)
        .position(|bytes| bytes == b"\r\n\r\n")
        .unwrap()
        + 4;
    &response[offset..]
}
fn chunk(generation: u64, start: u64, frames: u32) -> String {
    let source = sha256(format!("sound build {generation}").as_bytes());
    format!(
        "/api/audio/chunk?source={source}&worker_generation={generation}&start={start}&frames={frames}"
    )
}

#[test]
fn worker_audio_uses_authenticated_routes_and_bounds_requests_before_exchange() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (host, session) = host(Arc::new(AtomicBool::new(false)), Arc::clone(&calls));
    for path in ["/api/audio".to_owned(), chunk(1, 0, 4)] {
        assert!(request(&host, &path, false, false).starts_with(b"HTTP/1.1 403 "));
    }
    assert_eq!(calls.load(Ordering::Acquire), 0);
    let metadata = request(&host, "/api/audio", true, false);
    assert!(metadata.starts_with(b"HTTP/1.1 200 "));
    assert!(
        std::str::from_utf8(body(&metadata))
            .unwrap()
            .contains("\"sample_frames\":4,\"fps\":30,\"picture_frames\":1")
    );
    let response = request(&host, &chunk(1, 2, 65536), true, false);
    assert!(response.starts_with(b"HTTP/1.1 200 "));
    let expected: Vec<_> = [0.25_f32, -0.5]
        .repeat(2)
        .into_iter()
        .flat_map(f32::to_le_bytes)
        .collect();
    assert_eq!(body(&response), expected);
    assert!(String::from_utf8_lossy(&response).contains("X-FMN-Audio-End: true\r\n"));
    let before = calls.load(Ordering::Acquire);
    for path in [chunk(1, 0, 0), chunk(1, 0, 65537), chunk(2, 0, 1)] {
        assert!(request(&host, &path, true, false).starts_with(b"HTTP/1.1 400 "));
    }
    assert_eq!(calls.load(Ordering::Acquire), before);
    assert!(request(&host, &chunk(1, 5, 1), true, false).starts_with(b"HTTP/1.1 400 "));
    session.shutdown_worker();
}

#[test]
fn reload_replaces_worker_audio_atomically_and_failed_replacement_keeps_old_pcm() {
    let fail = Arc::new(AtomicBool::new(false));
    let (host, session) = host(Arc::clone(&fail), Arc::new(AtomicUsize::new(0)));
    let first = request(&host, "/api/audio", true, false);
    let restarted = request(&host, "/api/restart", true, true);
    assert!(restarted.starts_with(b"HTTP/1.1 200 "));
    let second = request(&host, "/api/audio", true, false);
    assert_ne!(body(&first), body(&second));
    assert!(
        std::str::from_utf8(body(&second))
            .unwrap()
            .contains("\"worker_generation\":2")
    );
    assert!(request(&host, &chunk(1, 0, 1), true, false).starts_with(b"HTTP/1.1 400 "));
    let pcm = request(&host, &chunk(2, 0, 4), true, false);
    assert!(pcm.starts_with(b"HTTP/1.1 200 "));
    fail.store(true, Ordering::Release);
    assert!(!request(&host, "/api/restart", true, true).starts_with(b"HTTP/1.1 200 "));
    assert_eq!(
        body(&request(&host, "/api/audio", true, false)),
        body(&second)
    );
    assert_eq!(
        body(&request(&host, &chunk(2, 0, 4), true, false)),
        body(&pcm)
    );
    session.shutdown_worker();
}
