//! Canonical supervisor ↔ scene-worker IPC (§13.3, D-14).
//!
//! Control messages are fmn-hash documents carried in a bounded
//! little-endian length frame.  The canonical document supplies versioning,
//! integrity, and deterministic encoding; the outer length lets a pipe reader
//! reject an oversized message before allocating it.  Frame bytes may travel
//! inline through the same pipe or out-of-band through an opaque shared-memory
//! token.  No filesystem path crosses this boundary.

use std::fmt;
use std::io::{Read, Write};

use fmn_hash::{Digest, Limits, Reader, Schema, SerialError, UnknownPolicy, Writer, sha256};
use fmn_scene::{CommandKind, CommandRecord, Journal};

/// Canonical request envelope schema.
pub const REQUEST_SCHEMA: Schema = Schema::new(*b"FMNI", 1, 1, 0);
/// Canonical response envelope schema.
pub const RESPONSE_SCHEMA: Schema = Schema::new(*b"FMNI", 2, 1, 0);

/// The live protocol version advertised during the mandatory handshake.
pub const CURRENT_VERSION: ProtocolVersion = ProtocolVersion { major: 1, minor: 0 };

/// A live-protocol version.
///
/// fmn-hash already rejects incompatible document schemas.  The explicit
/// version in `Hello` additionally prevents two processes that happen to
/// understand the envelope from silently disagreeing about session semantics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProtocolVersion {
    /// Breaking version.
    pub major: u16,
    /// Additive version.
    pub minor: u16,
}

impl ProtocolVersion {
    /// Require the exact live protocol used by this build.
    ///
    /// Unlike a durable file reader, a supervisor can always restart a worker
    /// from the same build.  Exact matching is therefore safer than carrying
    /// compatibility branches in a security boundary.
    pub fn require_current(self) -> Result<(), ProtocolError> {
        if self == CURRENT_VERSION {
            Ok(())
        } else {
            Err(ProtocolError::VersionSkew {
                local: CURRENT_VERSION,
                peer: self,
            })
        }
    }
}

/// Resource budgets for one IPC peer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProtocolLimits {
    /// Maximum canonical document, including its fmn-hash envelope.
    pub max_message_bytes: usize,
    /// Maximum individual bytes/string field.
    pub max_field_bytes: usize,
    /// Maximum inline or shared frame payload.
    pub max_frame_bytes: usize,
    /// Maximum serialized checkpoint.
    pub max_checkpoint_bytes: usize,
    /// Maximum serialized journal or journal segment.
    pub max_journal_bytes: usize,
    /// Maximum crash-report journal tail.
    pub max_crash_tail_bytes: usize,
    /// Maximum scene names in one enumeration response.
    pub max_scenes: usize,
    /// Maximum state hashes in one replay response.
    pub max_replay_hashes: usize,
}

impl ProtocolLimits {
    fn serial(self) -> Limits {
        Limits {
            max_total: self.max_message_bytes,
            max_field: self.max_field_bytes,
        }
    }
}

impl Default for ProtocolLimits {
    fn default() -> Self {
        Self {
            max_message_bytes: 128 * 1024 * 1024,
            max_field_bytes: 64 * 1024 * 1024,
            max_frame_bytes: 32 * 1024 * 1024,
            max_checkpoint_bytes: 64 * 1024 * 1024,
            max_journal_bytes: 64 * 1024 * 1024,
            max_crash_tail_bytes: 1024 * 1024,
            max_scenes: 4096,
            max_replay_hashes: 1_000_000,
        }
    }
}

/// A protocol document was invalid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProtocolError {
    /// Canonical framing/version/integrity failure.
    Serial(SerialError),
    /// The live peers do not run the exact same protocol.
    VersionSkew {
        /// This build.
        local: ProtocolVersion,
        /// The peer.
        peer: ProtocolVersion,
    },
    /// A bounded collection exceeded its protocol-specific ceiling.
    CountLimit {
        /// Which collection.
        field: &'static str,
        /// Ceiling.
        limit: usize,
        /// Supplied size.
        needed: usize,
    },
    /// A byte payload exceeded its protocol-specific ceiling.
    PayloadLimit {
        /// Which payload.
        field: &'static str,
        /// Ceiling.
        limit: usize,
        /// Supplied size.
        needed: usize,
    },
    /// A decoded payload violated a semantic invariant.
    Malformed(&'static str),
    /// A journal field was not a valid canonical replay journal.
    InvalidJournal(String),
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Serial(error) => write!(f, "IPC container: {error}"),
            Self::VersionSkew { local, peer } => write!(
                f,
                "worker protocol version skew: local {}.{}, peer {}.{}",
                local.major, local.minor, peer.major, peer.minor
            ),
            Self::CountLimit {
                field,
                limit,
                needed,
            } => write!(
                f,
                "IPC {field} count {needed} exceeds the configured limit {limit}"
            ),
            Self::PayloadLimit {
                field,
                limit,
                needed,
            } => write!(
                f,
                "IPC {field} payload {needed} bytes exceeds the configured limit {limit}"
            ),
            Self::Malformed(message) => write!(f, "malformed IPC payload: {message}"),
            Self::InvalidJournal(message) => write!(f, "invalid replay journal: {message}"),
        }
    }
}

impl std::error::Error for ProtocolError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Serial(error) => Some(error),
            _ => None,
        }
    }
}

impl From<SerialError> for ProtocolError {
    fn from(error: SerialError) -> Self {
        Self::Serial(error)
    }
}

/// Outer pipe framing failure.
#[derive(Debug)]
pub enum FramingError {
    /// The peer closed cleanly before another frame began.
    Closed,
    /// The pipe failed.
    Io(std::io::Error),
    /// The length prefix exceeded the negotiated message budget.
    FrameTooLarge {
        /// Ceiling.
        limit: usize,
        /// Declared frame length.
        needed: u64,
    },
    /// The canonical payload was invalid.
    Protocol(ProtocolError),
}

impl fmt::Display for FramingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => f.write_str("worker IPC pipe closed"),
            Self::Io(error) => write!(f, "worker IPC pipe failed: {error}"),
            Self::FrameTooLarge { limit, needed } => write!(
                f,
                "worker IPC frame declares {needed} bytes, over the {limit}-byte limit"
            ),
            Self::Protocol(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for FramingError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Protocol(error) => Some(error),
            Self::Closed | Self::FrameTooLarge { .. } => None,
        }
    }
}

impl From<std::io::Error> for FramingError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<ProtocolError> for FramingError {
    fn from(error: ProtocolError) -> Self {
        Self::Protocol(error)
    }
}

/// Which frame-transfer mechanisms a worker can serve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransportCapabilities {
    /// Canonical bytes inline on the control pipe.
    pub pipe: bool,
    /// An out-of-band region identified by an opaque token.
    pub shared_memory: bool,
}

impl TransportCapabilities {
    const PIPE_BIT: u8 = 1;
    const SHARED_MEMORY_BIT: u8 = 2;

    fn bits(self) -> u8 {
        (u8::from(self.pipe) * Self::PIPE_BIT)
            | (u8::from(self.shared_memory) * Self::SHARED_MEMORY_BIT)
    }

    fn from_bits(bits: u8) -> Result<Self, ProtocolError> {
        if bits & !(Self::PIPE_BIT | Self::SHARED_MEMORY_BIT) != 0 {
            return Err(ProtocolError::Malformed("frame transport capability bits"));
        }
        Ok(Self {
            pipe: bits & Self::PIPE_BIT != 0,
            shared_memory: bits & Self::SHARED_MEMORY_BIT != 0,
        })
    }
}

/// Encoding of a streamed preview frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameEncoding {
    /// Canonical PNG bytes.
    Png,
    /// Tight or explicitly-strided top-row-first RGBA8.
    Rgba8,
}

impl FrameEncoding {
    const fn code(self) -> u8 {
        match self {
            Self::Png => 0,
            Self::Rgba8 => 1,
        }
    }

    fn from_code(code: u8) -> Result<Self, ProtocolError> {
        match code {
            0 => Ok(Self::Png),
            1 => Ok(Self::Rgba8),
            _ => Err(ProtocolError::Malformed("frame encoding")),
        }
    }
}

/// Where a frame's bytes live.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FramePayload {
    /// Bytes carried inline on the bounded pipe.
    Pipe {
        /// Encoded payload.
        bytes: Vec<u8>,
        /// Content digest, checked on receipt.
        digest: Digest,
    },
    /// Bytes held in a host-mapped shared region.
    SharedMemory {
        /// Opaque, unforgeable region token.  It is never interpreted as a
        /// filesystem path.
        token: Digest,
        /// Mapped payload length.
        len: u64,
        /// Digest the mapper verifies after reading the region.
        digest: Digest,
    },
}

impl FramePayload {
    fn len(&self) -> Result<usize, ProtocolError> {
        match self {
            Self::Pipe { bytes, .. } => Ok(bytes.len()),
            Self::SharedMemory { len, .. } => usize::try_from(*len)
                .map_err(|_| ProtocolError::Malformed("shared frame length overflows usize")),
        }
    }
}

/// One frame-stream event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameStream {
    /// Scene whose renderer produced the frame.
    pub scene: String,
    /// Global, zero-based preview frame index.
    pub frame_index: u64,
    /// Pixel width.
    pub width: u32,
    /// Pixel height.
    pub height: u32,
    /// Bytes per row for raw RGBA8; zero for PNG.
    pub stride: u32,
    /// Payload encoding.
    pub encoding: FrameEncoding,
    /// Pipe or shared-memory transport.
    pub payload: FramePayload,
}

impl FrameStream {
    fn validate(&self, limits: ProtocolLimits) -> Result<(), ProtocolError> {
        require_scene(&self.scene)?;
        if self.width == 0 || self.height == 0 {
            return Err(ProtocolError::Malformed("zero-sized frame"));
        }
        let payload_len = self.payload.len()?;
        limit_payload("frame", payload_len, limits.max_frame_bytes)?;
        match &self.payload {
            FramePayload::Pipe { bytes, digest } => {
                if sha256(bytes) != *digest {
                    return Err(ProtocolError::Malformed("inline frame digest"));
                }
            }
            FramePayload::SharedMemory { .. } => {}
        }
        match self.encoding {
            FrameEncoding::Png => {
                if self.stride != 0 {
                    return Err(ProtocolError::Malformed("PNG frame has a stride"));
                }
                if let FramePayload::Pipe { bytes, .. } = &self.payload
                    && !bytes.starts_with(b"\x89PNG\r\n\x1a\n")
                {
                    return Err(ProtocolError::Malformed("inline PNG signature"));
                }
            }
            FrameEncoding::Rgba8 => {
                let row = self
                    .width
                    .checked_mul(4)
                    .ok_or(ProtocolError::Malformed("RGBA row overflow"))?;
                if self.stride < row {
                    return Err(ProtocolError::Malformed("RGBA stride too small"));
                }
                let expected = u64::from(self.stride)
                    .checked_mul(u64::from(self.height))
                    .ok_or(ProtocolError::Malformed("RGBA payload length overflow"))?;
                if u64::try_from(payload_len).ok() != Some(expected) {
                    return Err(ProtocolError::Malformed("RGBA payload length"));
                }
            }
        }
        Ok(())
    }
}

/// A durable worker checkpoint pushed to, or restored by, the supervisor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checkpoint {
    /// Scene identity.
    pub scene: String,
    /// Journal entry after which this state was captured.
    pub after_entry: u64,
    /// Digest of `state`.
    pub state_hash: Digest,
    /// Canonical `SceneState` bytes.
    pub state: Vec<u8>,
}

impl Checkpoint {
    fn validate(&self, limits: ProtocolLimits) -> Result<(), ProtocolError> {
        require_scene(&self.scene)?;
        limit_payload("checkpoint", self.state.len(), limits.max_checkpoint_bytes)?;
        if sha256(&self.state) != self.state_hash {
            return Err(ProtocolError::Malformed("checkpoint state hash"));
        }
        Ok(())
    }
}

/// A range of a canonical journal to re-execute.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JournalReplay {
    /// Scene identity.
    pub scene: String,
    /// First entry to execute, inclusive.
    pub from_entry: u64,
    /// End entry, exclusive.
    pub through_entry: u64,
    /// Canonical complete journal.  Ranges refer to this record.
    pub journal: Vec<u8>,
}

impl JournalReplay {
    fn validate(&self, limits: ProtocolLimits) -> Result<(), ProtocolError> {
        require_scene(&self.scene)?;
        if self.from_entry > self.through_entry {
            return Err(ProtocolError::Malformed("replay range is reversed"));
        }
        limit_payload("journal", self.journal.len(), limits.max_journal_bytes)?;
        let journal = Journal::from_bytes(&self.journal)
            .map_err(|error| ProtocolError::InvalidJournal(error.to_string()))?;
        let end = usize::try_from(self.through_entry)
            .map_err(|_| ProtocolError::Malformed("replay range overflows usize"))?;
        if end > journal.entries().len() {
            return Err(ProtocolError::Malformed("replay range exceeds journal"));
        }
        Ok(())
    }
}

/// A structured worker crash report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CrashReport {
    /// Active scene, when known.
    pub scene: Option<String>,
    /// Panic/termination diagnostic.
    pub message: String,
    /// Canonical journal tail or the best bounded diagnostic tail available.
    pub journal_tail: Vec<u8>,
    /// Last known state hash.
    pub state_hash: Option<Digest>,
}

impl CrashReport {
    fn validate(&self, limits: ProtocolLimits) -> Result<(), ProtocolError> {
        if let Some(scene) = &self.scene {
            require_scene(scene)?;
        }
        if self.message.is_empty() {
            return Err(ProtocolError::Malformed("empty crash message"));
        }
        limit_payload(
            "crash journal tail",
            self.journal_tail.len(),
            limits.max_crash_tail_bytes,
        )
    }
}

/// One supervisor request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SupervisorRequest {
    /// Mandatory first request.
    Hello {
        /// Explicit live-protocol version.
        version: ProtocolVersion,
        /// Supervisor build/content identity.
        supervisor_build: Digest,
        /// Largest frame the supervisor will accept.
        max_frame_bytes: u64,
    },
    /// Enumerate scene names in source order.
    EnumerateScenes,
    /// Advance one declared scene command.
    Play {
        /// Scene identity.
        scene: String,
        /// Expected command identity from the compiled scene.
        command: CommandRecord,
    },
    /// Seek deterministically to a global frame.
    Seek {
        /// Scene identity.
        scene: String,
        /// Global frame.
        frame: i64,
    },
    /// Interactive scrub request (kept distinct from a committed seek).
    Scrub {
        /// Scene identity.
        scene: String,
        /// Global frame.
        frame: i64,
    },
    /// Restore a supervisor-owned durable checkpoint.
    RestoreCheckpoint(Checkpoint),
    /// Re-execute a verified journal suffix.
    ReplayJournal(JournalReplay),
    /// Graceful worker shutdown.
    Shutdown,
}

impl SupervisorRequest {
    fn validate(&self, limits: ProtocolLimits) -> Result<(), ProtocolError> {
        match self {
            Self::Hello {
                max_frame_bytes, ..
            } => {
                if *max_frame_bytes == 0 {
                    return Err(ProtocolError::Malformed("zero frame budget"));
                }
            }
            Self::EnumerateScenes | Self::Shutdown => {}
            Self::Play { scene, command } => {
                require_scene(scene)?;
                if command.label.is_empty() {
                    return Err(ProtocolError::Malformed("empty command label"));
                }
            }
            Self::Seek { scene, frame } | Self::Scrub { scene, frame } => {
                require_scene(scene)?;
                if *frame < 0 {
                    return Err(ProtocolError::Malformed("negative frame index"));
                }
            }
            Self::RestoreCheckpoint(checkpoint) => checkpoint.validate(limits)?,
            Self::ReplayJournal(replay) => replay.validate(limits)?,
        }
        Ok(())
    }
}

/// Stable worker-side error classes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkerErrorCode {
    /// Request was illegal in the current session state.
    InvalidRequest,
    /// Named scene was not present.
    SceneNotFound,
    /// Checkpoint could not be restored.
    CheckpointRejected,
    /// Journal replay failed.
    ReplayFailed,
    /// Peer version mismatch.
    VersionSkew,
    /// Scene/renderer integration failed.
    ExecutionFailed,
}

impl WorkerErrorCode {
    const fn code(self) -> u16 {
        match self {
            Self::InvalidRequest => 1,
            Self::SceneNotFound => 2,
            Self::CheckpointRejected => 3,
            Self::ReplayFailed => 4,
            Self::VersionSkew => 5,
            Self::ExecutionFailed => 6,
        }
    }

    fn from_code(code: u16) -> Result<Self, ProtocolError> {
        match code {
            1 => Ok(Self::InvalidRequest),
            2 => Ok(Self::SceneNotFound),
            3 => Ok(Self::CheckpointRejected),
            4 => Ok(Self::ReplayFailed),
            5 => Ok(Self::VersionSkew),
            6 => Ok(Self::ExecutionFailed),
            _ => Err(ProtocolError::Malformed("worker error code")),
        }
    }
}

/// One worker response/event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkerResponse {
    /// Mandatory handshake response.
    Hello {
        /// Exact live-protocol version.
        version: ProtocolVersion,
        /// Worker executable/content identity.
        worker_build: Digest,
        /// Available frame transports.
        transports: TransportCapabilities,
    },
    /// Ordered scene names.
    Scenes(Vec<String>),
    /// Command completed.
    Ack {
        /// State after the command, when state-producing.
        state_hash: Option<Digest>,
        /// Journal length after the command.
        journal_len: u64,
    },
    /// Preview/export frame.
    Frame(FrameStream),
    /// Newly captured checkpoint for supervisor storage.
    Checkpoint(Checkpoint),
    /// Newly recorded journal segment.
    JournalSegment {
        /// Scene identity.
        scene: String,
        /// Global entry index represented by segment entry zero.
        start_entry: u64,
        /// Canonical standalone `Journal`.
        journal: Vec<u8>,
    },
    /// State hashes produced by one replay request, in entry order.
    ReplayComplete {
        /// First replayed entry.
        from_entry: u64,
        /// One hash per replayed entry.
        state_hashes: Vec<Digest>,
    },
    /// Scene code panicked or the worker is terminating abnormally.
    Crash(CrashReport),
    /// Typed worker refusal.
    Error {
        /// Stable class.
        code: WorkerErrorCode,
        /// Human diagnostic.
        message: String,
    },
    /// Graceful shutdown acknowledged.
    Bye,
}

impl WorkerResponse {
    fn validate(&self, limits: ProtocolLimits) -> Result<(), ProtocolError> {
        match self {
            Self::Hello { .. } | Self::Ack { .. } | Self::Bye => {}
            Self::Scenes(scenes) => {
                limit_count("scene", scenes.len(), limits.max_scenes)?;
                for scene in scenes {
                    require_scene(scene)?;
                }
            }
            Self::Frame(frame) => frame.validate(limits)?,
            Self::Checkpoint(checkpoint) => checkpoint.validate(limits)?,
            Self::JournalSegment { scene, journal, .. } => {
                require_scene(scene)?;
                limit_payload("journal", journal.len(), limits.max_journal_bytes)?;
                Journal::from_bytes(journal)
                    .map_err(|error| ProtocolError::InvalidJournal(error.to_string()))?;
            }
            Self::ReplayComplete { state_hashes, .. } => {
                limit_count(
                    "replay state hash",
                    state_hashes.len(),
                    limits.max_replay_hashes,
                )?;
            }
            Self::Crash(report) => report.validate(limits)?,
            Self::Error { message, .. } => {
                if message.is_empty() {
                    return Err(ProtocolError::Malformed("empty worker error"));
                }
            }
        }
        Ok(())
    }
}

/// A request plus its monotonically increasing correlation id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestEnvelope {
    /// Correlation id.
    pub request_id: u64,
    /// Request.
    pub request: SupervisorRequest,
}

impl RequestEnvelope {
    /// Encode canonically.
    pub fn to_bytes(&self, limits: ProtocolLimits) -> Result<Vec<u8>, ProtocolError> {
        self.request.validate(limits)?;
        let mut writer = Writer::with_limits(REQUEST_SCHEMA, limits.serial());
        writer.put_u64(self.request_id);
        put_request(&mut writer, &self.request);
        Ok(writer.finish()?)
    }

    /// Decode and validate canonically.
    pub fn from_bytes(bytes: &[u8], limits: ProtocolLimits) -> Result<Self, ProtocolError> {
        let mut reader = Reader::open(
            bytes,
            REQUEST_SCHEMA,
            limits.serial(),
            UnknownPolicy::Strict,
        )?;
        let request_id = reader.get_u64()?;
        let request = get_request(&mut reader, limits)?;
        reader.finish()?;
        request.validate(limits)?;
        Ok(Self {
            request_id,
            request,
        })
    }
}

/// A response correlated to exactly one request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResponseEnvelope {
    /// Correlation id copied from the request.
    pub request_id: u64,
    /// Response/event.
    pub response: WorkerResponse,
}

impl ResponseEnvelope {
    /// Encode canonically.
    pub fn to_bytes(&self, limits: ProtocolLimits) -> Result<Vec<u8>, ProtocolError> {
        self.response.validate(limits)?;
        let mut writer = Writer::with_limits(RESPONSE_SCHEMA, limits.serial());
        writer.put_u64(self.request_id);
        put_response(&mut writer, &self.response)?;
        Ok(writer.finish()?)
    }

    /// Decode and validate canonically.
    pub fn from_bytes(bytes: &[u8], limits: ProtocolLimits) -> Result<Self, ProtocolError> {
        let mut reader = Reader::open(
            bytes,
            RESPONSE_SCHEMA,
            limits.serial(),
            UnknownPolicy::Strict,
        )?;
        let request_id = reader.get_u64()?;
        let response = get_response(&mut reader, limits)?;
        reader.finish()?;
        response.validate(limits)?;
        Ok(Self {
            request_id,
            response,
        })
    }
}

/// Write one length-framed request.
pub fn write_request(
    writer: &mut impl Write,
    request: &RequestEnvelope,
    limits: ProtocolLimits,
) -> Result<(), FramingError> {
    write_document(writer, &request.to_bytes(limits)?, limits)
}

/// Read one length-framed request.
pub fn read_request(
    reader: &mut impl Read,
    limits: ProtocolLimits,
) -> Result<RequestEnvelope, FramingError> {
    let bytes = read_document(reader, limits)?;
    Ok(RequestEnvelope::from_bytes(&bytes, limits)?)
}

/// Write one length-framed response.
pub fn write_response(
    writer: &mut impl Write,
    response: &ResponseEnvelope,
    limits: ProtocolLimits,
) -> Result<(), FramingError> {
    write_document(writer, &response.to_bytes(limits)?, limits)
}

/// Read one length-framed response.
pub fn read_response(
    reader: &mut impl Read,
    limits: ProtocolLimits,
) -> Result<ResponseEnvelope, FramingError> {
    let bytes = read_document(reader, limits)?;
    Ok(ResponseEnvelope::from_bytes(&bytes, limits)?)
}

pub(crate) fn write_document(
    writer: &mut impl Write,
    bytes: &[u8],
    limits: ProtocolLimits,
) -> Result<(), FramingError> {
    if bytes.len() > limits.max_message_bytes {
        let needed = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        return Err(FramingError::FrameTooLarge {
            limit: limits.max_message_bytes,
            needed,
        });
    }
    let len = u64::try_from(bytes.len()).map_err(|_| FramingError::FrameTooLarge {
        limit: limits.max_message_bytes,
        needed: u64::MAX,
    })?;
    writer.write_all(&len.to_le_bytes())?;
    writer.write_all(bytes)?;
    writer.flush()?;
    Ok(())
}

fn read_document(reader: &mut impl Read, limits: ProtocolLimits) -> Result<Vec<u8>, FramingError> {
    let mut prefix = [0u8; 8];
    loop {
        match reader.read(&mut prefix[..1]) {
            Ok(0) => return Err(FramingError::Closed),
            Ok(1) => break,
            Ok(count) => {
                return Err(std::io::Error::other(format!(
                    "IPC reader violated Read by returning {count} for a one-byte buffer"
                ))
                .into());
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
    }
    reader.read_exact(&mut prefix[1..])?;
    let declared = u64::from_le_bytes(prefix);
    let message_limit = u64::try_from(limits.max_message_bytes).unwrap_or(u64::MAX);
    if declared > message_limit {
        return Err(FramingError::FrameTooLarge {
            limit: limits.max_message_bytes,
            needed: declared,
        });
    }
    let len = usize::try_from(declared).map_err(|_| FramingError::FrameTooLarge {
        limit: limits.max_message_bytes,
        needed: declared,
    })?;
    let mut bytes = vec![0u8; len];
    reader.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn put_request(writer: &mut Writer, request: &SupervisorRequest) {
    match request {
        SupervisorRequest::Hello {
            version,
            supervisor_build,
            max_frame_bytes,
        } => {
            writer.put_u8(0);
            put_version(writer, *version);
            writer.put_digest(supervisor_build);
            writer.put_u64(*max_frame_bytes);
        }
        SupervisorRequest::EnumerateScenes => {
            writer.put_u8(1);
        }
        SupervisorRequest::Play { scene, command } => {
            writer.put_u8(2);
            writer.put_str(scene);
            put_command(writer, command);
        }
        SupervisorRequest::Seek { scene, frame } => {
            writer.put_u8(3);
            writer.put_str(scene);
            writer.put_i64(*frame);
        }
        SupervisorRequest::Scrub { scene, frame } => {
            writer.put_u8(4);
            writer.put_str(scene);
            writer.put_i64(*frame);
        }
        SupervisorRequest::RestoreCheckpoint(checkpoint) => {
            writer.put_u8(5);
            put_checkpoint(writer, checkpoint);
        }
        SupervisorRequest::ReplayJournal(replay) => {
            writer.put_u8(6);
            put_replay(writer, replay);
        }
        SupervisorRequest::Shutdown => {
            writer.put_u8(7);
        }
    }
}

fn get_request(
    reader: &mut Reader<'_>,
    limits: ProtocolLimits,
) -> Result<SupervisorRequest, ProtocolError> {
    match reader.get_u8()? {
        0 => Ok(SupervisorRequest::Hello {
            version: get_version(reader)?,
            supervisor_build: reader.get_digest()?,
            max_frame_bytes: reader.get_u64()?,
        }),
        1 => Ok(SupervisorRequest::EnumerateScenes),
        2 => Ok(SupervisorRequest::Play {
            scene: reader.get_str()?.to_owned(),
            command: get_command(reader)?,
        }),
        3 => Ok(SupervisorRequest::Seek {
            scene: reader.get_str()?.to_owned(),
            frame: reader.get_i64()?,
        }),
        4 => Ok(SupervisorRequest::Scrub {
            scene: reader.get_str()?.to_owned(),
            frame: reader.get_i64()?,
        }),
        5 => Ok(SupervisorRequest::RestoreCheckpoint(get_checkpoint(
            reader, limits,
        )?)),
        6 => Ok(SupervisorRequest::ReplayJournal(get_replay(
            reader, limits,
        )?)),
        7 => Ok(SupervisorRequest::Shutdown),
        _ => Err(ProtocolError::Malformed("supervisor request tag")),
    }
}

fn put_response(writer: &mut Writer, response: &WorkerResponse) -> Result<(), ProtocolError> {
    match response {
        WorkerResponse::Hello {
            version,
            worker_build,
            transports,
        } => {
            writer.put_u8(0);
            put_version(writer, *version);
            writer.put_digest(worker_build);
            writer.put_u8(transports.bits());
        }
        WorkerResponse::Scenes(scenes) => {
            writer.put_u8(1);
            writer.put_u32(wire_count("scene", scenes.len())?);
            for scene in scenes {
                writer.put_str(scene);
            }
        }
        WorkerResponse::Ack {
            state_hash,
            journal_len,
        } => {
            writer.put_u8(2);
            put_optional_digest(writer, state_hash);
            writer.put_u64(*journal_len);
        }
        WorkerResponse::Frame(frame) => {
            writer.put_u8(3);
            put_frame(writer, frame);
        }
        WorkerResponse::Checkpoint(checkpoint) => {
            writer.put_u8(4);
            put_checkpoint(writer, checkpoint);
        }
        WorkerResponse::JournalSegment {
            scene,
            start_entry,
            journal,
        } => {
            writer.put_u8(5);
            writer.put_str(scene);
            writer.put_u64(*start_entry);
            writer.put_bytes(journal);
        }
        WorkerResponse::ReplayComplete {
            from_entry,
            state_hashes,
        } => {
            writer.put_u8(6);
            writer.put_u64(*from_entry);
            writer.put_u32(wire_count("replay state hash", state_hashes.len())?);
            for hash in state_hashes {
                writer.put_digest(hash);
            }
        }
        WorkerResponse::Crash(report) => {
            writer.put_u8(7);
            put_crash(writer, report);
        }
        WorkerResponse::Error { code, message } => {
            writer.put_u8(8);
            writer.put_u16(code.code());
            writer.put_str(message);
        }
        WorkerResponse::Bye => {
            writer.put_u8(9);
        }
    }
    Ok(())
}

fn get_response(
    reader: &mut Reader<'_>,
    limits: ProtocolLimits,
) -> Result<WorkerResponse, ProtocolError> {
    match reader.get_u8()? {
        0 => Ok(WorkerResponse::Hello {
            version: get_version(reader)?,
            worker_build: reader.get_digest()?,
            transports: TransportCapabilities::from_bits(reader.get_u8()?)?,
        }),
        1 => {
            let count = count(reader.get_u32()?, "scene", limits.max_scenes)?;
            let mut scenes = Vec::with_capacity(count);
            for _ in 0..count {
                scenes.push(reader.get_str()?.to_owned());
            }
            Ok(WorkerResponse::Scenes(scenes))
        }
        2 => Ok(WorkerResponse::Ack {
            state_hash: get_optional_digest(reader)?,
            journal_len: reader.get_u64()?,
        }),
        3 => Ok(WorkerResponse::Frame(get_frame(reader, limits)?)),
        4 => Ok(WorkerResponse::Checkpoint(get_checkpoint(reader, limits)?)),
        5 => Ok(WorkerResponse::JournalSegment {
            scene: reader.get_str()?.to_owned(),
            start_entry: reader.get_u64()?,
            journal: bounded_bytes(reader, "journal", limits.max_journal_bytes)?,
        }),
        6 => {
            let from_entry = reader.get_u64()?;
            let count = count(
                reader.get_u32()?,
                "replay state hash",
                limits.max_replay_hashes,
            )?;
            let mut state_hashes = Vec::with_capacity(count);
            for _ in 0..count {
                state_hashes.push(reader.get_digest()?);
            }
            Ok(WorkerResponse::ReplayComplete {
                from_entry,
                state_hashes,
            })
        }
        7 => Ok(WorkerResponse::Crash(get_crash(reader, limits)?)),
        8 => Ok(WorkerResponse::Error {
            code: WorkerErrorCode::from_code(reader.get_u16()?)?,
            message: reader.get_str()?.to_owned(),
        }),
        9 => Ok(WorkerResponse::Bye),
        _ => Err(ProtocolError::Malformed("worker response tag")),
    }
}

fn put_version(writer: &mut Writer, version: ProtocolVersion) {
    writer.put_u16(version.major);
    writer.put_u16(version.minor);
}

fn get_version(reader: &mut Reader<'_>) -> Result<ProtocolVersion, ProtocolError> {
    Ok(ProtocolVersion {
        major: reader.get_u16()?,
        minor: reader.get_u16()?,
    })
}

fn put_command(writer: &mut Writer, command: &CommandRecord) {
    writer.put_u8(command_kind_code(command.kind));
    writer.put_digest(&command.identity);
    writer.put_str(&command.label);
}

fn get_command(reader: &mut Reader<'_>) -> Result<CommandRecord, ProtocolError> {
    Ok(CommandRecord {
        kind: command_kind_from_code(reader.get_u8()?)?,
        identity: reader.get_digest()?,
        label: reader.get_str()?.to_owned(),
    })
}

const fn command_kind_code(kind: CommandKind) -> u8 {
    match kind {
        CommandKind::Play => 0,
        CommandKind::Wait => 1,
        CommandKind::Add => 2,
        CommandKind::Remove => 3,
        CommandKind::CameraChange => 4,
        CommandKind::Sound => 5,
        CommandKind::Custom => 6,
    }
}

fn command_kind_from_code(code: u8) -> Result<CommandKind, ProtocolError> {
    match code {
        0 => Ok(CommandKind::Play),
        1 => Ok(CommandKind::Wait),
        2 => Ok(CommandKind::Add),
        3 => Ok(CommandKind::Remove),
        4 => Ok(CommandKind::CameraChange),
        5 => Ok(CommandKind::Sound),
        6 => Ok(CommandKind::Custom),
        _ => Err(ProtocolError::Malformed("command kind")),
    }
}

fn put_checkpoint(writer: &mut Writer, checkpoint: &Checkpoint) {
    writer.put_str(&checkpoint.scene);
    writer.put_u64(checkpoint.after_entry);
    writer.put_digest(&checkpoint.state_hash);
    writer.put_bytes(&checkpoint.state);
}

fn get_checkpoint(
    reader: &mut Reader<'_>,
    limits: ProtocolLimits,
) -> Result<Checkpoint, ProtocolError> {
    Ok(Checkpoint {
        scene: reader.get_str()?.to_owned(),
        after_entry: reader.get_u64()?,
        state_hash: reader.get_digest()?,
        state: bounded_bytes(reader, "checkpoint", limits.max_checkpoint_bytes)?,
    })
}

fn put_replay(writer: &mut Writer, replay: &JournalReplay) {
    writer.put_str(&replay.scene);
    writer.put_u64(replay.from_entry);
    writer.put_u64(replay.through_entry);
    writer.put_bytes(&replay.journal);
}

fn get_replay(
    reader: &mut Reader<'_>,
    limits: ProtocolLimits,
) -> Result<JournalReplay, ProtocolError> {
    Ok(JournalReplay {
        scene: reader.get_str()?.to_owned(),
        from_entry: reader.get_u64()?,
        through_entry: reader.get_u64()?,
        journal: bounded_bytes(reader, "journal", limits.max_journal_bytes)?,
    })
}

fn put_frame(writer: &mut Writer, frame: &FrameStream) {
    writer.put_str(&frame.scene);
    writer.put_u64(frame.frame_index);
    writer.put_u32(frame.width);
    writer.put_u32(frame.height);
    writer.put_u32(frame.stride);
    writer.put_u8(frame.encoding.code());
    match &frame.payload {
        FramePayload::Pipe { bytes, digest } => {
            writer.put_u8(0);
            writer.put_digest(digest);
            writer.put_bytes(bytes);
        }
        FramePayload::SharedMemory { token, len, digest } => {
            writer.put_u8(1);
            writer.put_digest(token);
            writer.put_u64(*len);
            writer.put_digest(digest);
        }
    }
}

fn get_frame(
    reader: &mut Reader<'_>,
    limits: ProtocolLimits,
) -> Result<FrameStream, ProtocolError> {
    let scene = reader.get_str()?.to_owned();
    let frame_index = reader.get_u64()?;
    let width = reader.get_u32()?;
    let height = reader.get_u32()?;
    let stride = reader.get_u32()?;
    let encoding = FrameEncoding::from_code(reader.get_u8()?)?;
    let payload = match reader.get_u8()? {
        0 => {
            let digest = reader.get_digest()?;
            let bytes = bounded_bytes(reader, "frame", limits.max_frame_bytes)?;
            FramePayload::Pipe { bytes, digest }
        }
        1 => FramePayload::SharedMemory {
            token: reader.get_digest()?,
            len: reader.get_u64()?,
            digest: reader.get_digest()?,
        },
        _ => return Err(ProtocolError::Malformed("frame transport")),
    };
    Ok(FrameStream {
        scene,
        frame_index,
        width,
        height,
        stride,
        encoding,
        payload,
    })
}

fn put_crash(writer: &mut Writer, report: &CrashReport) {
    match &report.scene {
        Some(scene) => {
            writer.put_bool(true);
            writer.put_str(scene);
        }
        None => {
            writer.put_bool(false);
        }
    }
    writer.put_str(&report.message);
    writer.put_bytes(&report.journal_tail);
    put_optional_digest(writer, &report.state_hash);
}

fn get_crash(
    reader: &mut Reader<'_>,
    limits: ProtocolLimits,
) -> Result<CrashReport, ProtocolError> {
    let scene = if reader.get_bool()? {
        Some(reader.get_str()?.to_owned())
    } else {
        None
    };
    Ok(CrashReport {
        scene,
        message: reader.get_str()?.to_owned(),
        journal_tail: bounded_bytes(reader, "crash journal tail", limits.max_crash_tail_bytes)?,
        state_hash: get_optional_digest(reader)?,
    })
}

fn put_optional_digest(writer: &mut Writer, digest: &Option<Digest>) {
    match digest {
        Some(digest) => {
            writer.put_bool(true);
            writer.put_digest(digest);
        }
        None => {
            writer.put_bool(false);
        }
    }
}

fn get_optional_digest(reader: &mut Reader<'_>) -> Result<Option<Digest>, ProtocolError> {
    if reader.get_bool()? {
        Ok(Some(reader.get_digest()?))
    } else {
        Ok(None)
    }
}

fn bounded_bytes(
    reader: &mut Reader<'_>,
    field: &'static str,
    limit: usize,
) -> Result<Vec<u8>, ProtocolError> {
    let bytes = reader.get_bytes()?;
    limit_payload(field, bytes.len(), limit)?;
    Ok(bytes.to_vec())
}

fn count(raw: u32, field: &'static str, limit: usize) -> Result<usize, ProtocolError> {
    let needed =
        usize::try_from(raw).map_err(|_| ProtocolError::Malformed("wire count overflows usize"))?;
    limit_count(field, needed, limit)?;
    Ok(needed)
}

fn wire_count(field: &'static str, needed: usize) -> Result<u32, ProtocolError> {
    u32::try_from(needed).map_err(|_| ProtocolError::CountLimit {
        field,
        limit: usize::try_from(u32::MAX).unwrap_or(usize::MAX),
        needed,
    })
}

fn limit_count(field: &'static str, needed: usize, limit: usize) -> Result<(), ProtocolError> {
    if needed > limit {
        Err(ProtocolError::CountLimit {
            field,
            limit,
            needed,
        })
    } else {
        Ok(())
    }
}

fn limit_payload(field: &'static str, needed: usize, limit: usize) -> Result<(), ProtocolError> {
    if needed > limit {
        Err(ProtocolError::PayloadLimit {
            field,
            limit,
            needed,
        })
    } else {
        Ok(())
    }
}

fn require_scene(scene: &str) -> Result<(), ProtocolError> {
    if scene.is_empty() {
        Err(ProtocolError::Malformed("empty scene name"))
    } else {
        Ok(())
    }
}
