//! Secure loopback Studio host and multipart-PNG preview hub (§13.3).
//!
//! This is intentionally a small HTTP/1.1 server rather than a general web
//! framework. It serves only embedded assets and exact routes, validates both
//! sides of the loopback connection, rejects ambiguous request syntax, and
//! authenticates every request with a caller-supplied 256-bit capability.
//! Scene operations always cross the [`Supervisor`] boundary into the
//! disposable worker.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::str::FromStr;
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::thread::JoinHandle;
use std::time::Duration;

use fmn_codec::{CompressionLevel, PngError, PngLimits, decode_png, encode_rgba8};
use fmn_hash::{Digest, sha256};
use fmn_platform::clock::Clock;
use fmn_scene::{AssetRead, EventPayload, Key, Modifiers, MouseButton};

use crate::protocol::{
    DebugLayerSet, FrameEncoding, FramePayload, FrameStream, ProtocolError, ProtocolLimits,
    StudioDataKind, SupervisorRequest, WorkerResponse,
};
use crate::supervisor::{Supervisor, SupervisorError, SupervisorReply};
use crate::ui;

/// Multipart boundary used by the permanent browser preview floor.
pub const MULTIPART_BOUNDARY: &str = "fmn-frame";

const TOKEN_BYTES: usize = 32;
const TOKEN_HEX_BYTES: usize = TOKEN_BYTES * 2;
const MAX_REQUEST_LINE_BYTES: usize = 4096;

/// A 256-bit bearer capability.
#[derive(Clone)]
pub struct CapabilityToken([u8; TOKEN_BYTES]);

impl CapabilityToken {
    /// Construct a capability from caller-provided random bytes.
    ///
    /// The host intentionally has no ambient entropy fallback. The caller must
    /// obtain these bytes from an audited host entropy capability.
    pub fn new(bytes: [u8; TOKEN_BYTES]) -> Result<Self, TokenError> {
        if bytes.iter().all(|byte| *byte == 0) {
            Err(TokenError::AllZero)
        } else {
            Ok(Self(bytes))
        }
    }

    /// Decode the exact 64-character lowercase or uppercase hexadecimal form.
    pub fn from_hex(hex: &str) -> Result<Self, TokenError> {
        if hex.len() != TOKEN_HEX_BYTES {
            return Err(TokenError::WrongLength);
        }
        let mut bytes = [0u8; TOKEN_BYTES];
        let (pairs, remainder) = hex.as_bytes().as_chunks::<2>();
        debug_assert!(remainder.is_empty());
        for (index, pair) in pairs.iter().enumerate() {
            let high = hex_nibble(pair[0]).ok_or(TokenError::InvalidHex)?;
            let low = hex_nibble(pair[1]).ok_or(TokenError::InvalidHex)?;
            bytes[index] = (high << 4) | low;
        }
        Self::new(bytes)
    }

    /// Explicitly reveal the launch-URL representation.
    #[must_use]
    pub fn expose_hex(&self) -> String {
        let mut out = String::with_capacity(TOKEN_HEX_BYTES);
        for byte in self.0 {
            out.push(hex_digit(byte >> 4));
            out.push(hex_digit(byte & 0x0f));
        }
        out
    }

    fn matches_hex(&self, presented: &str) -> bool {
        let Ok(other) = Self::from_hex(presented) else {
            return false;
        };
        let mut difference = 0u8;
        for (expected, found) in self.0.iter().zip(other.0) {
            difference |= *expected ^ found;
        }
        difference == 0
    }
}

impl fmt::Debug for CapabilityToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CapabilityToken([REDACTED])")
    }
}

/// Capability-token construction failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenError {
    /// Hex form was not exactly 64 characters.
    WrongLength,
    /// Hex form contained a non-hexadecimal byte.
    InvalidHex,
    /// The all-zero value is not accepted as a capability.
    AllZero,
}

impl fmt::Display for TokenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongLength => f.write_str("capability token must contain exactly 64 hex digits"),
            Self::InvalidHex => f.write_str("capability token contains a non-hex digit"),
            Self::AllZero => f.write_str("the all-zero capability token is forbidden"),
        }
    }
}

impl std::error::Error for TokenError {}

/// Resource and security policy for one Studio host.
#[derive(Clone, Debug)]
pub struct StudioHostConfig {
    /// Requested loopback address. Port zero asks the OS for a private port.
    pub bind_addr: SocketAddr,
    /// Capability lifetime from host construction.
    pub session_ttl: Duration,
    /// Per-connection socket read/write timeout.
    pub request_timeout: Duration,
    /// How long a multipart stream waits for a new frame before closing.
    pub stream_idle_timeout: Duration,
    /// Sliding request-rate window.
    pub rate_window: Duration,
    /// Maximum requests in one rate window.
    pub max_requests_per_window: usize,
    /// Maximum request-header bytes.
    pub max_header_bytes: usize,
    /// Maximum request-body bytes.
    pub max_body_bytes: usize,
    /// Maximum header fields.
    pub max_headers: usize,
    /// Maximum concurrently handled TCP clients.
    pub max_clients: usize,
    /// Maximum PNG frames retained by the preview hub.
    pub max_frame_history: usize,
    /// Maximum one preview PNG.
    pub max_png_bytes: usize,
    /// Maximum frames written to one multipart connection.
    pub max_stream_frames: usize,
}

impl Default for StudioHostConfig {
    fn default() -> Self {
        Self {
            bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            session_ttl: Duration::from_secs(8 * 60 * 60),
            request_timeout: Duration::from_secs(5),
            stream_idle_timeout: Duration::from_secs(30),
            rate_window: Duration::from_secs(1),
            max_requests_per_window: 120,
            max_header_bytes: 16 * 1024,
            max_body_bytes: 64 * 1024,
            max_headers: 64,
            max_clients: 16,
            max_frame_history: 4,
            max_png_bytes: 32 * 1024 * 1024,
            max_stream_frames: 4096,
        }
    }
}

impl StudioHostConfig {
    fn validate(&self) -> Result<(), HostError> {
        if !self.bind_addr.ip().is_loopback() {
            return Err(HostError::Configuration(
                "Studio bind address must be loopback",
            ));
        }
        if self.session_ttl.is_zero()
            || self.request_timeout.is_zero()
            || self.stream_idle_timeout.is_zero()
            || self.rate_window.is_zero()
        {
            return Err(HostError::Configuration(
                "Studio time budgets must be nonzero",
            ));
        }
        if self.max_requests_per_window == 0
            || self.max_header_bytes < 128
            || self.max_body_bytes == 0
            || self.max_headers == 0
            || self.max_clients == 0
            || self.max_frame_history == 0
            || self.max_png_bytes < 8
            || self.max_stream_frames == 0
        {
            return Err(HostError::Configuration(
                "Studio resource budgets must be nonzero and usable",
            ));
        }
        Ok(())
    }
}

/// A validated canonical PNG frame owned by the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PngFrame {
    /// Monotone host publication sequence, independent of timeline position.
    ///
    /// Scrubbing can publish the same or an earlier `frame_index`; consumers
    /// use this sequence to observe every newly published preview.
    pub publication_sequence: u64,
    /// Scene identity.
    pub scene: String,
    /// Global frame index.
    pub frame_index: u64,
    /// Pixel width.
    pub width: u32,
    /// Pixel height.
    pub height: u32,
    /// Canonical PNG bytes.
    pub png: Vec<u8>,
    /// Digest of `png`.
    pub digest: Digest,
}

#[derive(Debug)]
struct FrameHubState {
    frames: VecDeque<Arc<PngFrame>>,
    next_publication_sequence: u64,
    closed: bool,
}

#[derive(Debug)]
struct FrameHubInner {
    state: Mutex<FrameHubState>,
    changed: Condvar,
}

/// Bounded, thread-safe preview-frame fanout.
#[derive(Clone, Debug)]
pub struct FrameHub {
    inner: Arc<FrameHubInner>,
    max_history: usize,
    max_png_bytes: usize,
}

impl FrameHub {
    /// Create an empty bounded hub.
    pub fn new(max_history: usize, max_png_bytes: usize) -> Result<Self, HostError> {
        if max_history == 0 || max_png_bytes < 8 {
            return Err(HostError::Configuration("invalid frame-hub budget"));
        }
        Ok(Self {
            inner: Arc::new(FrameHubInner {
                state: Mutex::new(FrameHubState {
                    frames: VecDeque::new(),
                    next_publication_sequence: 0,
                    closed: false,
                }),
                changed: Condvar::new(),
            }),
            max_history,
            max_png_bytes,
        })
    }

    /// Validate and publish one inline worker frame.
    pub fn publish(
        &self,
        frame: &FrameStream,
        limits: ProtocolLimits,
    ) -> Result<Arc<PngFrame>, HostError> {
        frame.validate(limits)?;
        let png = match (&frame.encoding, &frame.payload) {
            (FrameEncoding::Png, FramePayload::Pipe { bytes, .. }) => {
                if bytes.len() > self.max_png_bytes {
                    return Err(HostError::Frame("preview PNG exceeds the host budget"));
                }
                let declared_pixels = u64::from(frame.width) * u64::from(frame.height);
                let decoded_pixel_limit =
                    u64::try_from(limits.max_frame_bytes / 4).unwrap_or(u64::MAX);
                let decoded = decode_png(
                    bytes,
                    &PngLimits {
                        max_pixels: declared_pixels.min(decoded_pixel_limit),
                        ..PngLimits::default()
                    },
                )?;
                // ubs:ignore - public image dimensions are not secret comparisons.
                if decoded.width != frame.width || decoded.height != frame.height {
                    return Err(HostError::Frame(
                        "preview PNG dimensions do not match frame metadata",
                    ));
                }
                drop(decoded);
                bytes.clone()
            }
            (FrameEncoding::Rgba8, FramePayload::Pipe { bytes, .. }) => {
                let tight_stride = usize::try_from(frame.width)
                    .ok()
                    .and_then(|width| width.checked_mul(4))
                    .ok_or(HostError::Frame("RGBA row size overflow"))?;
                let stride = usize::try_from(frame.stride)
                    .map_err(|_| HostError::Frame("RGBA stride overflows usize"))?;
                let height = usize::try_from(frame.height)
                    .map_err(|_| HostError::Frame("RGBA height overflows usize"))?;
                let tight_len = tight_stride
                    .checked_mul(height)
                    .ok_or(HostError::Frame("RGBA tight buffer size overflow"))?;
                // ubs:ignore - stride equality is not a secret comparison.
                let rgba = if stride == tight_stride {
                    bytes.clone()
                } else {
                    let mut tight = Vec::with_capacity(tight_len);
                    for row in bytes.chunks_exact(stride) {
                        tight.extend_from_slice(&row[..tight_stride]);
                    }
                    tight
                };
                encode_rgba8(frame.width, frame.height, &rgba, CompressionLevel::Fast)
            }
            (_, FramePayload::SharedMemory { .. }) => {
                return Err(HostError::Frame(
                    "shared-memory previews require an explicit mapper capability",
                ));
            }
        };
        if png.len() > self.max_png_bytes {
            return Err(HostError::Frame("preview PNG exceeds the host budget"));
        }
        let mut state = lock(&self.inner.state);
        if state.closed {
            return Err(HostError::Frame("preview hub is closed"));
        }
        let publication_sequence = state.next_publication_sequence;
        state.next_publication_sequence = publication_sequence
            .checked_add(1)
            .ok_or(HostError::Frame("preview publication sequence exhausted"))?;
        let published = Arc::new(PngFrame {
            publication_sequence,
            scene: frame.scene.clone(),
            frame_index: frame.frame_index,
            width: frame.width,
            height: frame.height,
            digest: sha256(&png),
            png,
        });
        state.frames.push_back(Arc::clone(&published));
        while state.frames.len() > self.max_history {
            state.frames.pop_front();
        }
        self.inner.changed.notify_all();
        Ok(published)
    }

    /// Most recently published frame.
    #[must_use]
    pub fn latest(&self) -> Option<Arc<PngFrame>> {
        lock(&self.inner.state).frames.back().cloned()
    }

    /// Wait for a publication sequence newer than `after`.
    pub fn wait_after(&self, after: Option<u64>, timeout: Duration) -> Option<Arc<PngFrame>> {
        let state = lock(&self.inner.state);
        let (state, _) = self
            .inner
            .changed
            .wait_timeout_while(state, timeout, |state| {
                !state.closed
                    && state.frames.back().is_none_or(|frame| {
                        after.is_some_and(|sequence| frame.publication_sequence <= sequence)
                    })
            })
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state
            .frames
            .iter()
            .find(|frame| after.is_none_or(|sequence| frame.publication_sequence > sequence))
            .cloned()
    }

    /// Wake streams and reject future publications.
    pub fn close(&self) {
        let mut state = lock(&self.inner.state);
        state.closed = true;
        self.inner.changed.notify_all();
    }

    /// One multipart body part for `frame`.
    #[must_use]
    pub fn multipart_part(frame: &PngFrame) -> Vec<u8> {
        let header = format!(
            "--{MULTIPART_BOUNDARY}\r\nContent-Type: image/png\r\nContent-Length: {}\r\nX-FMN-Publication-Sequence: {}\r\nX-FMN-Frame-Index: {}\r\nX-FMN-SHA256: {}\r\n\r\n",
            frame.png.len(),
            frame.publication_sequence,
            frame.frame_index,
            digest_hex(&frame.digest)
        );
        let mut part = Vec::with_capacity(header.len() + frame.png.len() + 2);
        part.extend_from_slice(header.as_bytes());
        part.extend_from_slice(&frame.png);
        part.extend_from_slice(b"\r\n");
        part
    }

    /// Closing multipart delimiter.
    #[must_use]
    pub fn multipart_end() -> Vec<u8> {
        format!("--{MULTIPART_BOUNDARY}--\r\n").into_bytes()
    }
}

impl Drop for FrameHub {
    fn drop(&mut self) {
        if Arc::strong_count(&self.inner) == 1 {
            self.close();
        }
    }
}

/// Stable host-side owner of the disposable-worker supervisor.
///
/// The HTTP host accepts this concrete boundary, not an arbitrary scene
/// callback. The only injected callback verifies replayed asset digests.
pub struct StudioWorkerSession {
    scene: String,
    protocol_limits: ProtocolLimits,
    supervisor: Mutex<Supervisor>,
    asset_ok: Arc<dyn Fn(&AssetRead) -> bool + Send + Sync>,
}

impl fmt::Debug for StudioWorkerSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StudioWorkerSession")
            .field("scene", &self.scene)
            .finish_non_exhaustive()
    }
}

impl StudioWorkerSession {
    /// Bind a started supervisor to one scene and its asset verifier.
    pub fn new(
        scene: impl Into<String>,
        supervisor: Supervisor,
        asset_ok: Arc<dyn Fn(&AssetRead) -> bool + Send + Sync>,
    ) -> Result<Self, HostError> {
        let scene = scene.into();
        if scene.is_empty() {
            return Err(HostError::Configuration("empty Studio scene name"));
        }
        let protocol_limits = supervisor.protocol_limits();
        if scene.len() > protocol_limits.max_field_bytes {
            return Err(HostError::Configuration(
                "Studio scene name exceeds protocol field budget",
            ));
        }
        Ok(Self {
            scene,
            protocol_limits,
            supervisor: Mutex::new(supervisor),
            asset_ok,
        })
    }

    /// Active scene identity.
    #[must_use]
    pub fn scene(&self) -> &str {
        &self.scene
    }

    fn request(&self, request: SupervisorRequest) -> Result<WorkerResponse, HostError> {
        let reply = lock(&self.supervisor).request(request, &*self.asset_ok)?;
        match reply {
            SupervisorReply::Worker(response) => Ok(response),
            SupervisorReply::Recovered { crash, .. } => {
                Err(HostError::WorkerRecovered(crash.message))
            }
        }
    }
}

#[derive(Debug)]
struct AuthState {
    token: CapabilityToken,
    started: Duration,
    requests: VecDeque<Duration>,
}

#[derive(Clone)]
struct HostHandler {
    session: Arc<StudioWorkerSession>,
    frames: FrameHub,
    clock: Arc<dyn Clock>,
    config: StudioHostConfig,
    authority: String,
    localhost_authority: String,
    auth: Arc<Mutex<AuthState>>,
}

impl fmt::Debug for HostHandler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostHandler")
            .field("authority", &self.authority)
            .finish_non_exhaustive()
    }
}

/// Bound secure Studio service.
pub struct StudioHost {
    listener: TcpListener,
    handler: HostHandler,
    active_clients: Arc<AtomicUsize>,
}

impl fmt::Debug for StudioHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StudioHost")
            .field("local_addr", &self.local_addr())
            .finish_non_exhaustive()
    }
}

impl StudioHost {
    /// Bind a loopback-only host.
    pub fn bind(
        session: Arc<StudioWorkerSession>,
        frames: FrameHub,
        token: CapabilityToken,
        clock: Arc<dyn Clock>,
        config: StudioHostConfig,
    ) -> Result<Self, HostError> {
        config.validate()?;
        if frames.max_history > config.max_frame_history
            || frames.max_png_bytes > config.max_png_bytes
        {
            return Err(HostError::Configuration(
                "frame hub exceeds Studio host resource budgets",
            ));
        }
        let listener = TcpListener::bind(config.bind_addr)?;
        let local_addr = listener.local_addr()?;
        if !local_addr.ip().is_loopback() {
            return Err(HostError::Configuration(
                "bound Studio address is not loopback",
            ));
        }
        let authority = socket_authority(local_addr);
        let localhost_authority = format!("localhost:{}", local_addr.port());
        let started = clock.monotonic();
        Ok(Self {
            listener,
            handler: HostHandler {
                session,
                frames,
                clock,
                config,
                authority,
                localhost_authority,
                auth: Arc::new(Mutex::new(AuthState {
                    token,
                    started,
                    requests: VecDeque::new(),
                })),
            },
            active_clients: Arc::new(AtomicUsize::new(0)),
        })
    }

    /// Actual bound address.
    pub fn local_addr(&self) -> Result<SocketAddr, HostError> {
        Ok(self.listener.local_addr()?)
    }

    /// Explicit capability-bearing browser launch URL.
    pub fn launch_url(&self) -> String {
        let auth = lock(&self.handler.auth);
        format!(
            "http://{}/?cap={}",
            self.handler.authority,
            auth.token.expose_hex()
        )
    }

    /// Rotate the bearer capability without extending the session deadline.
    pub fn rotate_capability(&self, token: CapabilityToken) {
        let mut auth = lock(&self.handler.auth);
        auth.token = token;
        auth.requests.clear();
    }

    /// Accept and handle one connection synchronously.
    pub fn serve_once(&self) -> Result<(), HostError> {
        let (stream, peer) = self.listener.accept()?;
        self.handler.handle_stream(stream, peer)
    }

    /// Serve concurrent bounded connections until `shutdown` is set.
    pub fn serve_until(&self, shutdown: &AtomicBool) -> Result<(), HostError> {
        self.listener.set_nonblocking(true)?;
        let mut clients = Vec::new();
        let serve_result = loop {
            if let Err(error) = reap_finished_clients(&mut clients) {
                break Err(error);
            }
            if shutdown.load(Ordering::Acquire) {
                break Ok(());
            }
            match self.listener.accept() {
                Ok((mut stream, peer)) => {
                    let previous = self.active_clients.fetch_add(1, Ordering::AcqRel);
                    if previous >= self.handler.config.max_clients {
                        self.active_clients.fetch_sub(1, Ordering::AcqRel);
                        write_connection_limit_response(&mut stream);
                        continue;
                    }
                    let handler = self.handler.clone();
                    let active = Arc::clone(&self.active_clients);
                    let active_guard = Arc::clone(&active);
                    let client = match std::thread::Builder::new()
                        .name("fmn-studio-client".to_owned())
                        .spawn(move || {
                            let _guard = ActiveClientGuard(active_guard);
                            let _ = handler.handle_stream(stream, peer);
                        }) {
                        Ok(client) => client,
                        Err(error) => {
                            active.fetch_sub(1, Ordering::AcqRel);
                            break Err(error.into());
                        }
                    };
                    clients.push(client);
                }
                // ubs:ignore - I/O error kinds are public enum values, not secrets.
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => break Err(error.into()),
            }
        };
        let blocking_result = self
            .listener
            .set_nonblocking(false)
            .map_err(HostError::from);
        self.handler.frames.close();
        let join_result = join_clients(&mut clients);
        serve_result.and(blocking_result).and(join_result)
    }
}

fn reap_finished_clients(clients: &mut Vec<JoinHandle<()>>) -> Result<(), HostError> {
    let mut index = 0;
    while index < clients.len() {
        if clients[index].is_finished() {
            let client = clients.swap_remove(index);
            client.join().map_err(|_| HostError::ClientThreadPanicked)?;
        } else {
            index += 1;
        }
    }
    Ok(())
}

fn join_clients(clients: &mut Vec<JoinHandle<()>>) -> Result<(), HostError> {
    let mut panicked = false;
    for client in clients.drain(..) {
        panicked |= client.join().is_err();
    }
    if panicked {
        Err(HostError::ClientThreadPanicked)
    } else {
        Ok(())
    }
}

struct ActiveClientGuard(Arc<AtomicUsize>);

impl Drop for ActiveClientGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl HostHandler {
    fn handle_stream(&self, mut stream: TcpStream, peer: SocketAddr) -> Result<(), HostError> {
        if !peer.ip().is_loopback() {
            return Err(HostError::Forbidden("peer is not loopback"));
        }
        // Some platforms propagate the listener's nonblocking mode to accepted
        // sockets. Client handling is deliberately blocking and bounded by the
        // read/write deadlines installed immediately below.
        stream.set_nonblocking(false)?;
        stream.set_read_timeout(Some(self.config.request_timeout))?;
        stream.set_write_timeout(Some(self.config.request_timeout))?;
        let request = match read_http_request(&mut stream, &self.config) {
            Ok(request) => request,
            Err(error) => {
                write_http_response(
                    &mut stream,
                    400,
                    "Bad Request",
                    "text/plain; charset=utf-8",
                    format!("{error}\n").as_bytes(),
                )?;
                return Ok(());
            }
        };
        let now = self.clock.monotonic();
        if let Err(error) = self.authorize(&request, now) {
            let (status, reason) = match error {
                HostError::Expired => (401, "Unauthorized"),
                HostError::RateLimited => (429, "Too Many Requests"),
                _ => (403, "Forbidden"),
            };
            write_http_response(
                &mut stream,
                status,
                reason,
                "text/plain; charset=utf-8",
                format!("{error}\n").as_bytes(),
            )?;
            return Ok(());
        }
        match self.route(&mut stream, request, now) {
            Ok(()) => Ok(()),
            Err(error) => {
                let Some((status, reason)) = error.http_status() else {
                    return Err(error);
                };
                write_http_response(
                    &mut stream,
                    status,
                    reason,
                    "text/plain; charset=utf-8",
                    format!("{error}\n").as_bytes(),
                )
            }
        }
    }

    fn authorize(&self, request: &HttpRequest, now: Duration) -> Result<(), HostError> {
        authorize_request(
            request,
            now,
            &self.config,
            &self.authority,
            &self.localhost_authority,
            &self.auth,
        )
    }

    fn route(
        &self,
        stream: &mut TcpStream,
        request: HttpRequest,
        now: Duration,
    ) -> Result<(), HostError> {
        match (request.method, request.path.as_str()) {
            (Method::Get, "/") => {
                let token = lock(&self.auth).token.expose_hex();
                write_html_response(stream, ui::studio_index_html(&token).as_bytes())
            }
            (Method::Get, "/studio.js") => {
                let asset = ui::ui_asset("/studio.js").ok_or(HostError::Configuration(
                    "the embedded studio.js asset is missing from the binary",
                ))?;
                write_static_response(stream, asset.content_type, asset.bytes)
            }
            (Method::Get, "/stream") => self.stream_frames(stream, now),
            (Method::Post, "/api/scrub") => self.scrub(stream, &request),
            (Method::Post, "/api/event") => self.event(stream, &request),
            (Method::Get, "/api/inspect") => self.inspect(stream),
            (Method::Get, "/api/overlays") => self.overlays(stream, &request),
            _ => write_http_response(
                stream,
                404,
                "Not Found",
                "text/plain; charset=utf-8",
                b"unknown Studio route\n",
            ),
        }
    }

    fn stream_frames(&self, stream: &mut TcpStream, now: Duration) -> Result<(), HostError> {
        if now.saturating_sub(lock(&self.auth).started) >= self.config.session_ttl {
            return Err(HostError::Expired);
        }
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: multipart/x-mixed-replace; boundary={MULTIPART_BOUNDARY}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(head.as_bytes())?;
        let mut last = None;
        for _ in 0..self.config.max_stream_frames {
            let Some(frame) = self
                .frames
                .wait_after(last, self.config.stream_idle_timeout)
            else {
                break;
            };
            stream.write_all(&FrameHub::multipart_part(&frame))?;
            stream.flush()?;
            last = Some(frame.publication_sequence);
            if self
                .clock
                .monotonic()
                .saturating_sub(lock(&self.auth).started)
                >= self.config.session_ttl
            {
                break;
            }
        }
        stream.write_all(&FrameHub::multipart_end())?;
        stream.flush()?;
        Ok(())
    }

    fn scrub(&self, stream: &mut TcpStream, request: &HttpRequest) -> Result<(), HostError> {
        require_form_content_type(request)?;
        let form = parse_form(&request.body)?;
        let frame = required_form(&form, "frame")?
            .parse::<i64>()
            .map_err(|_| HostError::BadRequest("invalid frame index"))?;
        if frame < 0 {
            return Err(HostError::BadRequest("negative frame index"));
        }
        let commit = match form.get("commit").map(String::as_str) {
            None | Some("false") => false,
            Some("true") => true,
            Some(_) => return Err(HostError::BadRequest("invalid commit flag")),
        };
        let request = if commit {
            SupervisorRequest::Seek {
                scene: self.session.scene().to_owned(),
                frame,
            }
        } else {
            SupervisorRequest::Scrub {
                scene: self.session.scene().to_owned(),
                frame,
            }
        };
        self.write_worker_response(stream, self.session.request(request)?, None)
    }

    fn event(&self, stream: &mut TcpStream, request: &HttpRequest) -> Result<(), HostError> {
        require_form_content_type(request)?;
        let form = parse_form(&request.body)?;
        let event = event_from_form(&form)?;
        let response = self.session.request(SupervisorRequest::Event {
            scene: self.session.scene().to_owned(),
            event,
        })?;
        self.write_worker_response(stream, response, None)
    }

    fn inspect(&self, stream: &mut TcpStream) -> Result<(), HostError> {
        let response = self.session.request(SupervisorRequest::Inspect {
            scene: self.session.scene().to_owned(),
        })?;
        self.write_worker_response(stream, response, Some(StudioDataKind::Inspection))
    }

    fn overlays(&self, stream: &mut TcpStream, request: &HttpRequest) -> Result<(), HostError> {
        let layers = match request.query_one("layers")? {
            Some(raw) => DebugLayerSet::from_bits(
                raw.parse::<u8>()
                    .map_err(|_| HostError::BadRequest("invalid overlay layer bits"))?,
            )?,
            None => DebugLayerSet::ALL,
        };
        let response = self.session.request(SupervisorRequest::Overlay {
            scene: self.session.scene().to_owned(),
            layers,
        })?;
        self.write_worker_response(stream, response, Some(StudioDataKind::Overlay))
    }

    fn write_worker_response(
        &self,
        stream: &mut TcpStream,
        response: WorkerResponse,
        expected_data: Option<StudioDataKind>,
    ) -> Result<(), HostError> {
        match response {
            // ubs:ignore - response-kind equality is not a secret comparison.
            WorkerResponse::StudioData { kind, bytes, .. } if expected_data == Some(kind) => {
                write_json_response(stream, &bytes)
            }
            WorkerResponse::Frame(frame) if expected_data.is_none() => {
                let published = self.frames.publish(&frame, self.session.protocol_limits)?;
                let body = format!(
                    "{{\"status\":\"frame\",\"frame_index\":{},\"sha256\":\"{}\"}}",
                    published.frame_index,
                    digest_hex(&published.digest)
                );
                write_json_response(stream, body.as_bytes())
            }
            WorkerResponse::Ack {
                state_hash,
                journal_len,
            } if expected_data.is_none() => {
                let hash = state_hash.as_ref().map_or_else(
                    || "null".to_owned(),
                    |digest| format!("\"{}\"", digest_hex(digest)),
                );
                let body = format!(
                    "{{\"status\":\"ok\",\"journal_len\":{journal_len},\"state_hash\":{hash}}}"
                );
                write_json_response(stream, body.as_bytes())
            }
            WorkerResponse::Error { code, message } => {
                let body = format!(
                    "{{\"status\":\"worker_error\",\"code\":\"{code:?}\",\"message\":\"{}\"}}",
                    json_escape(&message)
                );
                write_http_response(
                    stream,
                    422,
                    "Unprocessable Content",
                    "application/json; charset=utf-8",
                    body.as_bytes(),
                )
            }
            _ => Err(HostError::UnexpectedWorkerResponse),
        }
    }
}

fn authorize_request(
    request: &HttpRequest,
    now: Duration,
    config: &StudioHostConfig,
    authority: &str,
    localhost_authority: &str,
    auth: &Mutex<AuthState>,
) -> Result<(), HostError> {
    let host = request
        .headers
        .get("host")
        .ok_or(HostError::Forbidden("missing Host header"))?;
    if !is_allowed_host(host, authority, localhost_authority) {
        return Err(HostError::Forbidden("Host authority mismatch"));
    }
    if let Some(origin) = request.headers.get("origin") {
        if !is_allowed_origin(origin, authority, localhost_authority) {
            return Err(HostError::Forbidden("Origin authority mismatch"));
        }
    } else {
        // ubs:ignore - HTTP-method equality is not a secret comparison.
        if request.method == Method::Post {
            return Err(HostError::Forbidden(
                "state-changing requests require Origin",
            ));
        }
    }
    let header_token = request.headers.get("x-fmn-capability");
    let query_token = request.query_one("cap")?;
    if header_token.is_some() && query_token.is_some() {
        return Err(HostError::Forbidden(
            "capability supplied through multiple channels",
        ));
    }
    let presented = header_token
        .map(String::as_str)
        .or(query_token)
        .ok_or(HostError::Forbidden("missing capability"))?;

    let mut auth = lock(auth);
    if now.saturating_sub(auth.started) >= config.session_ttl {
        return Err(HostError::Expired);
    }
    while auth
        .requests
        .front()
        .is_some_and(|at| now.saturating_sub(*at) >= config.rate_window)
    {
        auth.requests.pop_front();
    }
    if auth.requests.len() >= config.max_requests_per_window {
        return Err(HostError::RateLimited);
    }
    auth.requests.push_back(now);
    if !auth.token.matches_hex(presented) {
        return Err(HostError::Forbidden("invalid capability"));
    }
    Ok(())
}

fn is_allowed_host(host: &str, authority: &str, localhost_authority: &str) -> bool {
    host.eq_ignore_ascii_case(authority) || host.eq_ignore_ascii_case(localhost_authority)
}

fn is_allowed_origin(origin: &str, authority: &str, localhost_authority: &str) -> bool {
    let numeric = format!("http://{authority}");
    let named = format!("http://{localhost_authority}");
    origin.eq_ignore_ascii_case(&numeric) || origin.eq_ignore_ascii_case(&named)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Method {
    Get,
    Post,
}

#[derive(Debug)]
struct HttpRequest {
    method: Method,
    path: String,
    query: Vec<(String, String)>,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

impl HttpRequest {
    fn query_one(&self, key: &str) -> Result<Option<&str>, HostError> {
        let mut values = self
            .query
            .iter()
            // ubs:ignore - query-key equality is not a secret comparison.
            .filter(|(candidate, _)| candidate == key)
            .map(|(_, value)| value.as_str());
        let first = values.next();
        if values.next().is_some() {
            Err(HostError::BadRequest("duplicate query parameter"))
        } else {
            Ok(first)
        }
    }
}

fn read_http_request(
    reader: &mut impl Read,
    config: &StudioHostConfig,
) -> Result<HttpRequest, HostError> {
    let mut bytes = Vec::with_capacity(config.max_header_bytes.min(4096));
    let header_end = loop {
        if let Some(index) = find_bytes(&bytes, b"\r\n\r\n") {
            break index + 4;
        }
        if bytes.len() >= config.max_header_bytes {
            return Err(HostError::BadRequest("request headers exceed budget"));
        }
        let remaining = config.max_header_bytes - bytes.len();
        let mut chunk = [0u8; 2048];
        let chunk_limit = remaining.min(chunk.len());
        let count = reader.read(&mut chunk[..chunk_limit])?;
        if count == 0 {
            return Err(HostError::BadRequest("request ended before headers"));
        }
        bytes.extend_from_slice(&chunk[..count]);
    };
    let header_bytes = &bytes[..header_end - 4];
    validate_crlf(header_bytes)?;
    let header_text = std::str::from_utf8(header_bytes)
        .map_err(|_| HostError::BadRequest("headers are not UTF-8"))?;
    let mut lines = header_text.split("\r\n");
    let request_line = lines
        .next()
        .ok_or(HostError::BadRequest("missing request line"))?;
    if request_line.len() > MAX_REQUEST_LINE_BYTES {
        return Err(HostError::BadRequest("request line exceeds budget"));
    }
    let mut request_parts = request_line.split(' ');
    let method = match request_parts.next() {
        Some("GET") => Method::Get,
        Some("POST") => Method::Post,
        _ => return Err(HostError::BadRequest("unsupported HTTP method")),
    };
    let target = request_parts
        .next()
        .ok_or(HostError::BadRequest("missing request target"))?;
    // ubs:ignore - protocol-version equality is not a secret comparison.
    if request_parts.next() != Some("HTTP/1.1") || request_parts.next().is_some() {
        return Err(HostError::BadRequest(
            "request must use exact HTTP/1.1 syntax",
        ));
    }
    let (path, query) = parse_target(target)?;
    let mut headers = BTreeMap::new();
    for line in lines {
        if headers.len() >= config.max_headers {
            return Err(HostError::BadRequest("too many request headers"));
        }
        if line.starts_with([' ', '\t']) {
            return Err(HostError::BadRequest("obsolete folded header"));
        }
        let (name, value) = line
            .split_once(':')
            .ok_or(HostError::BadRequest("malformed header field"))?;
        if name.is_empty() || !name.bytes().all(is_header_name_byte) {
            return Err(HostError::BadRequest("invalid header name"));
        }
        let name = name.to_ascii_lowercase();
        let value = value.trim();
        // ubs:ignore - header-byte classification is not a secret comparison.
        if value.bytes().any(|byte| byte < 0x20 || byte == 0x7f) {
            return Err(HostError::BadRequest("invalid header value"));
        }
        if headers.insert(name, value.to_owned()).is_some() {
            return Err(HostError::BadRequest("duplicate header field"));
        }
    }
    if headers.contains_key("transfer-encoding") {
        return Err(HostError::BadRequest("Transfer-Encoding is forbidden"));
    }
    let content_length = match headers.get("content-length") {
        Some(value) => parse_decimal_usize(value)?,
        // ubs:ignore - HTTP-method equality is not a secret comparison.
        None if method == Method::Post => {
            return Err(HostError::BadRequest("POST requires Content-Length"));
        }
        None => 0,
    };
    if content_length > config.max_body_bytes {
        return Err(HostError::BadRequest("request body exceeds budget"));
    }
    // ubs:ignore - method/body-shape equality is not a secret comparison.
    if method == Method::Get && content_length != 0 {
        return Err(HostError::BadRequest("GET request carries a body"));
    }
    let total = header_end
        .checked_add(content_length)
        .ok_or(HostError::BadRequest("request size overflow"))?;
    if bytes.len() > total {
        return Err(HostError::BadRequest("HTTP pipelining is forbidden"));
    }
    let already_read = bytes.len();
    bytes.resize(total, 0);
    reader.read_exact(&mut bytes[already_read..])?;
    Ok(HttpRequest {
        method,
        path,
        query,
        headers,
        body: bytes[header_end..].to_vec(),
    })
}

fn parse_target(target: &str) -> Result<(String, Vec<(String, String)>), HostError> {
    if !target.starts_with('/') || target.starts_with("//") || target.contains('#') {
        return Err(HostError::BadRequest("request target must be origin-form"));
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path.contains(['\\', '%'])
        || path
            .split('/')
            .any(|component| component == "." || component == "..")
    {
        return Err(HostError::BadRequest("ambiguous request path"));
    }
    Ok((path.to_owned(), parse_urlencoded(query.as_bytes())?))
}

fn parse_form(body: &[u8]) -> Result<BTreeMap<String, String>, HostError> {
    let pairs = parse_urlencoded(body)?;
    let mut form = BTreeMap::new();
    for (key, value) in pairs {
        if form.insert(key, value).is_some() {
            return Err(HostError::BadRequest("duplicate form field"));
        }
    }
    Ok(form)
}

fn parse_urlencoded(bytes: &[u8]) -> Result<Vec<(String, String)>, HostError> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let mut pairs = Vec::new();
    // ubs:ignore - URL form delimiter equality is not a secret comparison.
    for pair in bytes.split(|byte| *byte == b'&') {
        if pair.is_empty() {
            return Err(HostError::BadRequest("empty URL-encoded field"));
        }
        // ubs:ignore - URL form delimiter equality is not a secret comparison.
        let (key, value) = match pair.iter().position(|byte| *byte == b'=') {
            Some(index) => (&pair[..index], &pair[index + 1..]),
            None => (pair, &[][..]),
        };
        let key = decode_form_component(key)?;
        let value = decode_form_component(value)?;
        pairs.push((key, value));
    }
    Ok(pairs)
}

fn decode_form_component(bytes: &[u8]) -> Result<String, HostError> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            b'%' => {
                let high = bytes
                    .get(index + 1)
                    .and_then(|byte| hex_nibble(*byte))
                    .ok_or(HostError::BadRequest("invalid percent escape"))?;
                let low = bytes
                    .get(index + 2)
                    .and_then(|byte| hex_nibble(*byte))
                    .ok_or(HostError::BadRequest("invalid percent escape"))?;
                out.push((high << 4) | low);
                index += 3;
            }
            // ubs:ignore - decoded-byte classification is not a secret comparison.
            byte if byte == 0 || byte < 0x20 || byte == 0x7f => {
                return Err(HostError::BadRequest("control byte in URL-encoded value"));
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(out).map_err(|_| HostError::BadRequest("URL value is not UTF-8"))
}

fn event_from_form(form: &BTreeMap<String, String>) -> Result<EventPayload, HostError> {
    let modifiers = Modifiers::from_bits(parse_optional(form, "modifiers", 0u8)?)
        .map_err(|_| HostError::BadRequest("invalid modifier bits"))?;
    let point = || -> Result<[f64; 3], HostError> {
        Ok([
            parse_required::<f64>(form, "x")?,
            parse_required::<f64>(form, "y")?,
            parse_optional(form, "z", 0.0)?,
        ])
    };
    let delta = || -> Result<[f64; 3], HostError> {
        Ok([
            parse_required::<f64>(form, "dx")?,
            parse_required::<f64>(form, "dy")?,
            parse_optional(form, "dz", 0.0)?,
        ])
    };
    let event = match required_form(form, "type")? {
        "mouse_motion" => EventPayload::MouseMotion {
            point: point()?,
            delta: delta()?,
            modifiers,
        },
        "mouse_press" => EventPayload::MousePress {
            point: point()?,
            button: parse_mouse_button(required_form(form, "button")?)?,
            modifiers,
        },
        "mouse_release" => EventPayload::MouseRelease {
            point: point()?,
            button: parse_mouse_button(required_form(form, "button")?)?,
            modifiers,
        },
        "mouse_drag" => EventPayload::MouseDrag {
            point: point()?,
            delta: delta()?,
            button: parse_mouse_button(required_form(form, "button")?)?,
            modifiers,
        },
        "mouse_scroll" => EventPayload::MouseScroll {
            point: point()?,
            offset: [
                parse_required::<f64>(form, "offset_x")?,
                parse_required::<f64>(form, "offset_y")?,
            ],
            modifiers,
        },
        "key_press" => EventPayload::KeyPress {
            key: parse_key(required_form(form, "key")?)?,
            modifiers,
        },
        "key_release" => EventPayload::KeyRelease {
            key: parse_key(required_form(form, "key")?)?,
            modifiers,
        },
        _ => return Err(HostError::BadRequest("unknown event type")),
    };
    event
        .validate()
        .map_err(|_| HostError::BadRequest("invalid event coordinates"))?;
    Ok(event)
}

fn parse_mouse_button(raw: &str) -> Result<MouseButton, HostError> {
    match raw {
        "left" => Ok(MouseButton::Left),
        "middle" => Ok(MouseButton::Middle),
        "right" => Ok(MouseButton::Right),
        _ => raw
            .strip_prefix("other:")
            .ok_or(HostError::BadRequest("invalid mouse button"))?
            .parse::<u16>()
            .map(MouseButton::Other)
            .map_err(|_| HostError::BadRequest("invalid mouse button")),
    }
}

fn parse_key(raw: &str) -> Result<Key, HostError> {
    match raw {
        "backspace" => Ok(Key::Backspace),
        "arrow_left" => Ok(Key::ArrowLeft),
        "arrow_up" => Ok(Key::ArrowUp),
        "arrow_right" => Ok(Key::ArrowRight),
        "arrow_down" => Ok(Key::ArrowDown),
        "escape" => Ok(Key::Escape),
        "enter" => Ok(Key::Enter),
        "tab" => Ok(Key::Tab),
        _ => {
            if let Some(raw) = raw.strip_prefix("other:") {
                return raw
                    .parse::<u32>()
                    .map(Key::Other)
                    .map_err(|_| HostError::BadRequest("invalid key"));
            }
            let mut characters = raw.chars();
            let character = characters
                .next()
                .ok_or(HostError::BadRequest("empty key"))?;
            if characters.next().is_some() {
                Err(HostError::BadRequest("character key is not one scalar"))
            } else {
                Ok(Key::Character(character))
            }
        }
    }
}

fn parse_required<T: FromStr>(
    form: &BTreeMap<String, String>,
    key: &'static str,
) -> Result<T, HostError> {
    required_form(form, key)?
        .parse()
        .map_err(|_| HostError::BadRequest("invalid numeric form field"))
}

fn parse_optional<T: FromStr>(
    form: &BTreeMap<String, String>,
    key: &'static str,
    default: T,
) -> Result<T, HostError> {
    match form.get(key) {
        Some(value) => value
            .parse()
            .map_err(|_| HostError::BadRequest("invalid numeric form field")),
        None => Ok(default),
    }
}

fn required_form<'a>(
    form: &'a BTreeMap<String, String>,
    key: &'static str,
) -> Result<&'a str, HostError> {
    form.get(key)
        .map(String::as_str)
        .ok_or(HostError::BadRequest("missing form field"))
}

fn require_form_content_type(request: &HttpRequest) -> Result<(), HostError> {
    if request.headers.get("content-type").map(String::as_str)
        == Some("application/x-www-form-urlencoded")
    {
        Ok(())
    } else {
        Err(HostError::BadRequest(
            "expected application/x-www-form-urlencoded",
        ))
    }
}

fn write_html_response(stream: &mut impl Write, body: &[u8]) -> Result<(), HostError> {
    write_response_with_headers(
        stream,
        200,
        "OK",
        "text/html; charset=utf-8",
        body,
        &[
            (
                "Content-Security-Policy",
                "default-src 'none'; img-src 'self'; script-src 'self'; connect-src 'self'",
            ),
            ("Referrer-Policy", "no-referrer"),
            (ui::STUDIO_UI_VERSION_HEADER, ui::STUDIO_UI_VERSION),
        ],
    )
}

fn write_static_response(
    stream: &mut impl Write,
    content_type: &str,
    body: &[u8],
) -> Result<(), HostError> {
    write_response_with_headers(
        stream,
        200,
        "OK",
        content_type,
        body,
        &[
            ("Referrer-Policy", "no-referrer"),
            (ui::STUDIO_UI_VERSION_HEADER, ui::STUDIO_UI_VERSION),
        ],
    )
}

fn write_json_response(stream: &mut impl Write, body: &[u8]) -> Result<(), HostError> {
    write_http_response(stream, 200, "OK", "application/json; charset=utf-8", body)
}

fn write_connection_limit_response(stream: &mut impl Write) {
    // The connection is already rejected and is not owned by a client
    // handler. Delivery is best-effort: a peer that disconnected before the
    // 503 must not bypass the server's common frame-close and thread-join
    // path by returning from inside the accept loop.
    let _ = write_http_response(
        stream,
        503,
        "Service Unavailable",
        "text/plain; charset=utf-8",
        b"Studio connection limit reached\n",
    );
}

fn write_http_response(
    stream: &mut impl Write,
    status: u16,
    reason: &str,
    content_type: &str,
    body: &[u8],
) -> Result<(), HostError> {
    write_response_with_headers(stream, status, reason, content_type, body, &[])
}

fn write_response_with_headers(
    stream: &mut impl Write,
    status: u16,
    reason: &str,
    content_type: &str,
    body: &[u8],
    extra: &[(&str, &str)],
) -> Result<(), HostError> {
    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n",
        body.len()
    );
    for (name, value) in extra {
        head.push_str(name);
        head.push_str(": ");
        head.push_str(value);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()?;
    Ok(())
}

fn validate_crlf(bytes: &[u8]) -> Result<(), HostError> {
    for (index, byte) in bytes.iter().copied().enumerate() {
        // ubs:ignore - CRLF grammar equality is not a secret comparison.
        if byte == b'\n' && (index == 0 || bytes[index - 1] != b'\r') {
            return Err(HostError::BadRequest("bare LF in headers"));
        }
        // ubs:ignore - CRLF grammar equality is not a secret comparison.
        if byte == b'\r' && bytes.get(index + 1) != Some(&b'\n') {
            return Err(HostError::BadRequest("bare CR in headers"));
        }
    }
    Ok(())
}

fn parse_decimal_usize(raw: &str) -> Result<usize, HostError> {
    if raw.is_empty() || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(HostError::BadRequest("invalid Content-Length"));
    }
    raw.parse()
        .map_err(|_| HostError::BadRequest("Content-Length overflow"))
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn is_header_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

fn socket_authority(addr: SocketAddr) -> String {
    match addr.ip() {
        IpAddr::V4(ip) => format!("{ip}:{}", addr.port()),
        IpAddr::V6(ip) => format!("[{ip}]:{}", addr.port()),
    }
}

fn digest_hex(digest: &Digest) -> String {
    let mut out = String::with_capacity(64);
    for byte in digest.as_bytes() {
        out.push(hex_digit(byte >> 4));
        out.push(hex_digit(byte & 0x0f));
    }
    out
}

const fn hex_digit(nibble: u8) -> char {
    match nibble {
        0..=9 => (b'0' + nibble) as char,
        _ => (b'a' + nibble - 10) as char,
    }
}

const fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn json_escape(raw: &str) -> String {
    let mut out = String::new();
    for character in raw.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            character if character.is_control() => {
                out.push_str(&format!("\\u{:04x}", character as u32));
            }
            character => out.push(character),
        }
    }
    out
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Loopback-host failure.
#[derive(Debug)]
pub enum HostError {
    /// I/O boundary failed.
    Io(std::io::Error),
    /// Host policy is internally invalid.
    Configuration(&'static str),
    /// Request syntax or payload was malformed.
    BadRequest(&'static str),
    /// Authentication/authority policy refused the request.
    Forbidden(&'static str),
    /// Capability session expired.
    Expired,
    /// Sliding-window request ceiling was reached.
    RateLimited,
    /// Worker protocol rejected a message.
    Protocol(ProtocolError),
    /// Stable supervisor failed.
    Supervisor(SupervisorError),
    /// Worker crashed and the supervisor recovered it.
    WorkerRecovered(String),
    /// Frame could not be admitted to the preview hub.
    Frame(&'static str),
    /// The owned codec rejected an inline preview PNG.
    FramePng(PngError),
    /// Worker answered a route with the wrong response class.
    UnexpectedWorkerResponse,
    /// A bounded client handler panicked before it could be joined.
    ClientThreadPanicked,
}

impl HostError {
    fn http_status(&self) -> Option<(u16, &'static str)> {
        match self {
            Self::BadRequest(_) | Self::Protocol(_) => Some((400, "Bad Request")),
            Self::Forbidden(_) => Some((403, "Forbidden")),
            Self::Expired => Some((401, "Unauthorized")),
            Self::RateLimited => Some((429, "Too Many Requests")),
            Self::Supervisor(_)
            | Self::WorkerRecovered(_)
            | Self::Frame(_)
            | Self::FramePng(_)
            | Self::UnexpectedWorkerResponse
            | Self::ClientThreadPanicked => Some((502, "Bad Gateway")),
            Self::Configuration(_) => Some((500, "Internal Server Error")),
            Self::Io(_) => None,
        }
    }
}

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "Studio I/O: {error}"),
            Self::Configuration(message) => write!(f, "Studio configuration: {message}"),
            Self::BadRequest(message) => write!(f, "bad Studio request: {message}"),
            Self::Forbidden(message) => write!(f, "Studio request forbidden: {message}"),
            Self::Expired => f.write_str("Studio capability session expired"),
            Self::RateLimited => f.write_str("Studio request rate limit reached"),
            Self::Protocol(error) => error.fmt(f),
            Self::Supervisor(error) => error.fmt(f),
            Self::WorkerRecovered(message) => {
                write!(f, "scene worker recovered after crash: {message}")
            }
            Self::Frame(message) => write!(f, "Studio preview frame: {message}"),
            Self::FramePng(error) => write!(f, "Studio preview PNG: {error}"),
            Self::UnexpectedWorkerResponse => {
                f.write_str("worker returned an unexpected Studio response")
            }
            Self::ClientThreadPanicked => f.write_str("Studio client handler panicked"),
        }
    }
}

impl std::error::Error for HostError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Protocol(error) => Some(error),
            Self::Supervisor(error) => Some(error),
            Self::FramePng(error) => Some(error),
            _ => None,
        }
    }
}

impl From<std::io::Error> for HostError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<ProtocolError> for HostError {
    fn from(error: ProtocolError) -> Self {
        Self::Protocol(error)
    }
}

impl From<PngError> for HostError {
    fn from(error: PngError) -> Self {
        Self::FramePng(error)
    }
}

impl From<SupervisorError> for HostError {
    fn from(error: SupervisorError) -> Self {
        Self::Supervisor(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, ErrorKind};

    use fmn_cache::{NamespacePolicy, Store, StoreConfig};
    use fmn_platform::clock::FakeClock;
    use fmn_platform::fs::{FileSystem, VirtualFs};

    use crate::supervisor::{StdWorkerLauncher, SupervisorConfig};

    fn test_supervisor_with_protocol_limits(
        limits: ProtocolLimits,
    ) -> (Supervisor, Arc<dyn Clock>) {
        let fs: Arc<dyn FileSystem> = Arc::new(VirtualFs::new());
        let clock: Arc<dyn Clock> = Arc::new(FakeClock::new());
        let cache = Store::open(
            fs,
            Arc::clone(&clock),
            "/host-protocol-limit-cache",
            StoreConfig::default(),
        )
        .unwrap()
        .namespace(
            "studio-replay",
            1,
            NamespacePolicy {
                ceiling_bytes: None,
            },
        )
        .unwrap();
        let supervisor = Supervisor::new(
            Box::new(StdWorkerLauncher::default()),
            Arc::clone(&clock),
            cache,
            SupervisorConfig {
                protocol_limits: limits,
                ..SupervisorConfig::default()
            },
        );
        (supervisor, clock)
    }

    fn test_session_with_protocol_limits(
        limits: ProtocolLimits,
    ) -> (Arc<StudioWorkerSession>, Arc<dyn Clock>) {
        let (supervisor, clock) = test_supervisor_with_protocol_limits(limits);
        let session =
            Arc::new(StudioWorkerSession::new("Demo", supervisor, Arc::new(|_| true)).unwrap());
        (session, clock)
    }

    fn bind_test_host(
        config: StudioHostConfig,
        max_history: usize,
        max_png_bytes: usize,
    ) -> Result<StudioHost, HostError> {
        let (session, clock) = test_session_with_protocol_limits(ProtocolLimits::default());
        StudioHost::bind(
            session,
            FrameHub::new(max_history, max_png_bytes).unwrap(),
            CapabilityToken::new([0x55; TOKEN_BYTES]).unwrap(),
            clock,
            config,
        )
    }

    #[derive(Default)]
    struct DisconnectedClient {
        write_attempts: usize,
    }

    impl Write for DisconnectedClient {
        fn write(&mut self, _bytes: &[u8]) -> std::io::Result<usize> {
            self.write_attempts += 1;
            Err(std::io::Error::from(ErrorKind::BrokenPipe))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::from(ErrorKind::BrokenPipe))
        }
    }

    #[test]
    fn connection_limit_reply_failure_is_best_effort() {
        let mut client = DisconnectedClient::default();
        write_connection_limit_response(&mut client);
        assert_eq!(client.write_attempts, 1);
    }

    #[test]
    fn serve_until_restores_blocking_mode_for_serve_once() {
        let host = bind_test_host(StudioHostConfig::default(), 1, 8).unwrap();
        let shutdown = AtomicBool::new(true);
        host.serve_until(&shutdown).unwrap();
        let address = host.local_addr().unwrap();

        std::thread::scope(|scope| {
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let server_barrier = Arc::clone(&barrier);
            let (result_tx, result_rx) = std::sync::mpsc::sync_channel(1);
            let server = scope.spawn(move || {
                server_barrier.wait();
                result_tx.send(host.serve_once()).unwrap();
            });

            barrier.wait();
            assert!(matches!(
                result_rx.recv_timeout(Duration::from_millis(250)),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            ));

            let mut client = TcpStream::connect(address).unwrap();
            client
                .write_all(b"BAD / HTTP/1.1\r\nHost: localhost\r\n\r\n")
                .unwrap();
            client.shutdown(std::net::Shutdown::Write).unwrap();
            let mut response = Vec::new();
            client.read_to_end(&mut response).unwrap();
            assert!(response.starts_with(b"HTTP/1.1 400 Bad Request\r\n"));
            assert!(
                result_rx
                    .recv_timeout(Duration::from_secs(2))
                    .unwrap()
                    .is_ok()
            );
            server.join().unwrap();
        });
    }

    #[test]
    fn client_handler_normalizes_inherited_nonblocking_mode() {
        let host = bind_test_host(StudioHostConfig::default(), 1, 8).unwrap();
        let address = host.local_addr().unwrap();
        let mut client = TcpStream::connect(address).unwrap();
        let (server, peer) = host.listener.accept().unwrap();
        server.set_nonblocking(true).unwrap();
        let handler = host.handler.clone();

        std::thread::scope(|scope| {
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let server_barrier = Arc::clone(&barrier);
            let (result_tx, result_rx) = std::sync::mpsc::sync_channel(1);
            let server = scope.spawn(move || {
                server_barrier.wait();
                result_tx.send(handler.handle_stream(server, peer)).unwrap();
            });

            barrier.wait();
            assert!(matches!(
                result_rx.recv_timeout(Duration::from_millis(250)),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            ));

            client
                .write_all(b"BAD / HTTP/1.1\r\nHost: localhost\r\n\r\n")
                .unwrap();
            client.shutdown(std::net::Shutdown::Write).unwrap();
            let mut response = Vec::new();
            client.read_to_end(&mut response).unwrap();
            assert!(response.starts_with(b"HTTP/1.1 400 Bad Request\r\n"));
            assert!(
                result_rx
                    .recv_timeout(Duration::from_secs(2))
                    .unwrap()
                    .is_ok()
            );
            server.join().unwrap();
        });
    }

    #[test]
    fn strict_parser_rejects_duplicate_host_and_transfer_encoding() {
        let config = StudioHostConfig::default();
        let duplicate = b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nHost: localhost\r\n\r\n".to_vec();
        assert!(matches!(
            read_http_request(&mut Cursor::new(duplicate), &config),
            Err(HostError::BadRequest("duplicate header field"))
        ));

        let chunked =
            b"POST /api/event HTTP/1.1\r\nHost: localhost\r\nTransfer-Encoding: chunked\r\nContent-Length: 0\r\n\r\n"
                .to_vec();
        assert!(matches!(
            read_http_request(&mut Cursor::new(chunked), &config),
            Err(HostError::BadRequest("Transfer-Encoding is forbidden"))
        ));
    }

    #[test]
    fn strict_parser_accepts_one_bounded_origin_form_request() {
        let config = StudioHostConfig::default();
        let bytes = b"POST /api/scrub HTTP/1.1\r\nHost: 127.0.0.1:42\r\nOrigin: http://127.0.0.1:42\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: 20\r\n\r\nframe=17&commit=true"
            .to_vec();
        let request = read_http_request(&mut Cursor::new(bytes), &config).unwrap();
        assert_eq!(request.method, Method::Post);
        assert_eq!(request.path, "/api/scrub");
        assert_eq!(request.body, b"frame=17&commit=true");
    }

    #[test]
    fn event_form_is_typed_and_finite() {
        let form =
            parse_form(b"type=mouse_drag&x=1&y=2&dx=3&dy=4&button=left&modifiers=3").unwrap();
        assert!(matches!(
            event_from_form(&form).unwrap(),
            EventPayload::MouseDrag {
                point: [1.0, 2.0, 0.0],
                delta: [3.0, 4.0, 0.0],
                button: MouseButton::Left,
                modifiers,
            } if modifiers == (Modifiers::SHIFT | Modifiers::CONTROL) // ubs:ignore - ordinary test equality.
        ));
    }

    #[test]
    fn security_policy_refuses_non_loopback_missing_stale_and_expired_tokens() {
        let non_loopback = StudioHostConfig {
            bind_addr: SocketAddr::from(([0, 0, 0, 0], 9000)),
            ..StudioHostConfig::default()
        };
        assert!(matches!(
            non_loopback.validate(),
            Err(HostError::Configuration(
                "Studio bind address must be loopback"
            ))
        ));

        let config = StudioHostConfig {
            session_ttl: Duration::from_secs(10),
            ..StudioHostConfig::default()
        };
        let current = CapabilityToken::new([0x11; 32]).unwrap();
        let stale = CapabilityToken::new([0x22; 32]).unwrap();
        let auth = Mutex::new(AuthState {
            token: current.clone(),
            started: Duration::ZERO,
            requests: VecDeque::new(),
        });
        let mut request = authorized_get(&current);
        request.query.clear();
        assert!(matches!(
            authorize_request(
                &request,
                Duration::ZERO,
                &config,
                "127.0.0.1:42",
                "localhost:42",
                &auth
            ),
            Err(HostError::Forbidden("missing capability"))
        ));

        let stale_request = authorized_get(&stale);
        assert!(matches!(
            authorize_request(
                &stale_request,
                Duration::ZERO,
                &config,
                "127.0.0.1:42",
                "localhost:42",
                &auth
            ),
            Err(HostError::Forbidden("invalid capability"))
        ));
        assert!(matches!(
            authorize_request(
                &authorized_get(&current),
                Duration::from_secs(10),
                &config,
                "127.0.0.1:42",
                "localhost:42",
                &auth
            ),
            Err(HostError::Expired)
        ));
    }

    #[test]
    fn security_policy_bounds_bodies_rates_and_paths() {
        let config = StudioHostConfig {
            max_body_bytes: 4,
            max_requests_per_window: 1,
            ..StudioHostConfig::default()
        };
        let oversized =
            b"POST /api/event HTTP/1.1\r\nHost: localhost\r\nContent-Length: 5\r\n\r\n12345"
                .to_vec();
        assert!(matches!(
            read_http_request(&mut Cursor::new(oversized), &config),
            Err(HostError::BadRequest("request body exceeds budget"))
        ));
        assert!(matches!(
            parse_target("/../secret"),
            Err(HostError::BadRequest("ambiguous request path"))
        ));
        assert!(matches!(
            parse_target("/%2e%2e/secret"),
            Err(HostError::BadRequest("ambiguous request path"))
        ));

        let token = CapabilityToken::new([0x33; 32]).unwrap();
        let auth = Mutex::new(AuthState {
            token: token.clone(),
            started: Duration::ZERO,
            requests: VecDeque::new(),
        });
        let request = authorized_event(&token);
        authorize_request(
            &request,
            Duration::ZERO,
            &config,
            "127.0.0.1:42",
            "localhost:42",
            &auth,
        )
        .unwrap();
        assert!(matches!(
            authorize_request(
                &request,
                Duration::ZERO,
                &config,
                "127.0.0.1:42",
                "localhost:42",
                &auth
            ),
            Err(HostError::RateLimited)
        ));
    }

    #[test]
    fn worker_frame_publication_uses_the_supervisor_protocol_budget() {
        let limits = ProtocolLimits {
            max_frame_bytes: 3,
            ..ProtocolLimits::default()
        };
        let (session, clock) = test_session_with_protocol_limits(limits);
        let token = CapabilityToken::new([0x44; TOKEN_BYTES]).unwrap();
        let handler = HostHandler {
            session,
            frames: FrameHub::new(1, 1024).unwrap(),
            clock,
            config: StudioHostConfig::default(),
            authority: "127.0.0.1:42".to_owned(),
            localhost_authority: "localhost:42".to_owned(),
            auth: Arc::new(Mutex::new(AuthState {
                token,
                started: Duration::ZERO,
                requests: VecDeque::new(),
            })),
        };
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut server, _) = listener.accept().unwrap();
        let bytes = vec![0x11; 4];
        let response = WorkerResponse::Frame(FrameStream {
            scene: "Demo".to_owned(),
            frame_index: 0,
            width: 1,
            height: 1,
            stride: 4,
            encoding: FrameEncoding::Rgba8,
            payload: FramePayload::Pipe {
                digest: sha256(&bytes),
                bytes,
            },
        });

        let error = handler
            .write_worker_response(&mut server, response, None)
            .expect_err("the configured three-byte frame budget must reject four bytes");
        assert!(matches!(
            error,
            HostError::Protocol(ProtocolError::PayloadLimit {
                field: "frame",
                limit: 3,
                needed: 4,
            })
        ));
        assert!(handler.frames.latest().is_none());
        client.shutdown(std::net::Shutdown::Both).unwrap();
        server.shutdown(std::net::Shutdown::Both).unwrap();
        drop(client);
    }

    #[test]
    fn worker_session_scene_name_obeys_the_protocol_field_budget() {
        let limits = ProtocolLimits {
            max_field_bytes: 4,
            ..ProtocolLimits::default()
        };
        let (exact_supervisor, _) = test_supervisor_with_protocol_limits(limits);
        assert!(StudioWorkerSession::new("Demo", exact_supervisor, Arc::new(|_| true)).is_ok());

        let (oversized_supervisor, _) = test_supervisor_with_protocol_limits(limits);
        assert!(matches!(
            StudioWorkerSession::new("Demos", oversized_supervisor, Arc::new(|_| true)),
            Err(HostError::Configuration(
                "Studio scene name exceeds protocol field budget"
            ))
        ));
    }

    #[test]
    fn host_frame_hub_budgets_refuse_looser_history() {
        let config = StudioHostConfig {
            max_frame_history: 1,
            max_png_bytes: 8,
            ..StudioHostConfig::default()
        };
        assert!(matches!(
            bind_test_host(config, 2, 8),
            Err(HostError::Configuration(
                "frame hub exceeds Studio host resource budgets"
            ))
        ));
    }

    #[test]
    fn host_frame_hub_budgets_refuse_looser_png_limit() {
        let config = StudioHostConfig {
            max_frame_history: 1,
            max_png_bytes: 8,
            ..StudioHostConfig::default()
        };
        assert!(matches!(
            bind_test_host(config, 1, 9),
            Err(HostError::Configuration(
                "frame hub exceeds Studio host resource budgets"
            ))
        ));
    }

    #[test]
    fn host_frame_hub_budgets_admit_equal_and_tighter_hubs() {
        let equal = StudioHostConfig {
            max_frame_history: 1,
            max_png_bytes: 8,
            ..StudioHostConfig::default()
        };
        assert!(bind_test_host(equal, 1, 8).is_ok());

        let tighter = StudioHostConfig {
            max_frame_history: 2,
            max_png_bytes: 9,
            ..StudioHostConfig::default()
        };
        assert!(bind_test_host(tighter, 1, 8).is_ok());
    }

    fn authorized_get(token: &CapabilityToken) -> HttpRequest {
        HttpRequest {
            method: Method::Get,
            path: "/".to_owned(),
            query: vec![("cap".to_owned(), token.expose_hex())],
            headers: BTreeMap::from([("host".to_owned(), "127.0.0.1:42".to_owned())]),
            body: Vec::new(),
        }
    }

    fn authorized_event(token: &CapabilityToken) -> HttpRequest {
        HttpRequest {
            method: Method::Post,
            path: "/api/event".to_owned(),
            query: Vec::new(),
            headers: BTreeMap::from([
                ("host".to_owned(), "127.0.0.1:42".to_owned()),
                ("origin".to_owned(), "http://127.0.0.1:42".to_owned()),
                ("x-fmn-capability".to_owned(), token.expose_hex()),
            ]),
            body: b"type=key_press&key=a".to_vec(),
        }
    }
}
