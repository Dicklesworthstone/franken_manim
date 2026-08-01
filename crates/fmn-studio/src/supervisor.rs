//! Supervisor-owned build/restart/checkpoint/replay orchestration (§13.3).
//!
//! The supervisor is the stable process.  It owns the UI connection, build
//! driver, durable cache namespace, replay journal, and crash history.  A scene
//! worker is deliberately disposable: every rebuild or crash replaces the
//! whole executable, performs an exact-version handshake, restores the newest
//! reusable post-command checkpoint, and replays only the verified suffix.
//!
//! `fmn-platform::ProcessRunner` remains the ffmpeg-only engine capability.
//! The launcher here is narrower and belongs to the development Studio host:
//! it can launch only the exact absolute worker artifact returned by the
//! injected build driver, with argv-only invocation and a cleared environment.
//! It is not exposed to scene code and cannot be used as an external-tool
//! escape hatch.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::io::Read as _;
use std::path::PathBuf;
use std::process::Child;
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use fmn_cache::Namespace;
use fmn_hash::{Digest, SerialError, sha256};
use fmn_platform::clock::Clock;
use fmn_scene::{AssetRead, CommandRecord, Journal, ReplayAudit, ReplayPlan, plan_replay};

use crate::protocol::{
    CURRENT_VERSION, Checkpoint, CrashReport, FramingError, JournalReplay, ProtocolError,
    ProtocolLimits, RequestEnvelope, ResponseEnvelope, SupervisorRequest, TransportCapabilities,
    WorkerErrorCode, WorkerResponse, read_response, write_document,
};

fn lock_poisoned<T>(error: PoisonError<T>) -> T {
    error.into_inner()
}

/// One freshly built scene-worker executable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkerArtifact {
    /// Absolute executable path; never resolved through `PATH`.
    pub executable: PathBuf,
    /// Worker-mode argv, passed verbatim.
    pub argv: Vec<String>,
    /// Complete child environment after `env_clear`.
    pub env: Vec<(String, String)>,
    /// Absolute working directory, or no working directory override.
    pub cwd: Option<PathBuf>,
    /// Content/build identity returned by the worker handshake.
    pub build_id: Digest,
}

impl WorkerArtifact {
    fn validate(&self) -> Result<(), LaunchError> {
        if !self.executable.is_absolute() {
            return Err(LaunchError::InvalidArtifact(
                "worker executable path must be absolute",
            ));
        }
        if self.cwd.as_ref().is_some_and(|path| !path.is_absolute()) {
            return Err(LaunchError::InvalidArtifact(
                "worker working directory must be absolute",
            ));
        }
        let mut keys = BTreeSet::new();
        for (key, _) in &self.env {
            if key.is_empty() {
                return Err(LaunchError::InvalidArtifact(
                    "worker environment key cannot be empty",
                ));
            }
            if !keys.insert(key.to_ascii_uppercase()) {
                return Err(LaunchError::InvalidArtifact(
                    "worker environment keys must be unique ignoring ASCII case",
                ));
            }
        }
        Ok(())
    }
}

/// An incremental build failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildError {
    /// Human diagnostic from the build host.
    pub message: String,
}

impl BuildError {
    /// Construct a build failure.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for BuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "incremental worker build failed: {}", self.message)
    }
}

impl std::error::Error for BuildError {}

/// Host-provided incremental build operation.
pub trait RebuildDriver {
    /// Produce the exact artifact to launch.
    fn rebuild(&mut self) -> Result<WorkerArtifact, BuildError>;
}

/// Launching the exact worker artifact failed.
#[derive(Debug)]
pub enum LaunchError {
    /// Artifact construction violated the narrow launcher contract.
    InvalidArtifact(&'static str),
    /// `std::process::Command` could not launch the artifact.
    Spawn {
        /// Program.
        program: PathBuf,
        /// I/O error.
        error: std::io::Error,
    },
    /// A requested standard pipe was absent.
    MissingPipe(&'static str),
}

impl fmt::Display for LaunchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidArtifact(message) => write!(f, "invalid worker artifact: {message}"),
            Self::Spawn { program, error } => {
                write!(
                    f,
                    "cannot launch scene worker {}: {error}",
                    program.display()
                )
            }
            Self::MissingPipe(pipe) => write!(f, "scene worker launched without its {pipe} pipe"),
        }
    }
}

impl std::error::Error for LaunchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn { error, .. } => Some(error),
            Self::InvalidArtifact(_) | Self::MissingPipe(_) => None,
        }
    }
}

/// Channel failure class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelFailureKind {
    /// Peer closed or exited.
    Closed,
    /// Request deadline expired.
    Timeout,
    /// Pipe I/O failed.
    Io,
    /// Canonical protocol failed.
    Protocol,
    /// Response correlation id did not match.
    Correlation,
}

/// One failed worker exchange, with a bounded stderr tail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelError {
    /// Stable class.
    pub kind: ChannelFailureKind,
    /// Human diagnostic.
    pub detail: String,
    /// Last bounded worker stderr bytes.
    pub stderr_tail: Vec<u8>,
}

impl ChannelError {
    fn new(kind: ChannelFailureKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
            stderr_tail: Vec::new(),
        }
    }

    /// Whether replacing the disposable worker is a safe response.
    #[must_use]
    pub const fn recoverable(&self) -> bool {
        matches!(
            self.kind,
            ChannelFailureKind::Closed | ChannelFailureKind::Timeout | ChannelFailureKind::Io
        )
    }
}

impl fmt::Display for ChannelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "worker channel {:?}: {}", self.kind, self.detail)
    }
}

impl std::error::Error for ChannelError {}

/// A running worker control channel.
pub trait WorkerChannel: Send {
    /// Exchange exactly one correlated request/response pair.
    fn exchange(
        &mut self,
        request: &RequestEnvelope,
        timeout: Duration,
    ) -> Result<ResponseEnvelope, ChannelError>;

    /// Stop only this exact owned worker and reap it.  Implementations must be
    /// idempotent.
    fn terminate(&mut self);
}

/// Factory for disposable worker channels.
pub trait WorkerLauncher: Send {
    /// Launch `artifact` under the negotiated budgets.
    fn launch(
        &mut self,
        artifact: &WorkerArtifact,
        limits: ProtocolLimits,
    ) -> Result<Box<dyn WorkerChannel>, LaunchError>;
}

/// Narrow production launcher for the Studio's own worker artifact.
#[derive(Clone, Copy, Debug)]
pub struct StdWorkerLauncher {
    /// Maximum stderr bytes retained for crash diagnostics.
    pub stderr_tail_bytes: usize,
}

impl Default for StdWorkerLauncher {
    fn default() -> Self {
        Self {
            stderr_tail_bytes: 1024 * 1024,
        }
    }
}

enum PipeEvent {
    Response(Result<ResponseEnvelope, FramingError>),
    WriteFailed(FramingError),
}

impl WorkerLauncher for StdWorkerLauncher {
    fn launch(
        &mut self,
        artifact: &WorkerArtifact,
        limits: ProtocolLimits,
    ) -> Result<Box<dyn WorkerChannel>, LaunchError> {
        artifact.validate()?;
        let mut command = std::process::Command::new(&artifact.executable);
        command
            .args(&artifact.argv)
            .env_clear()
            .envs(artifact.env.iter().map(|(key, value)| (key, value)))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        if let Some(cwd) = &artifact.cwd {
            command.current_dir(cwd);
        }
        let mut child = command.spawn().map_err(|error| LaunchError::Spawn {
            program: artifact.executable.clone(),
            error,
        })?;
        let mut stdin = match child.stdin.take() {
            Some(stdin) => stdin,
            None => {
                terminate_child(&mut child);
                return Err(LaunchError::MissingPipe("stdin"));
            }
        };
        let mut stdout = match child.stdout.take() {
            Some(stdout) => stdout,
            None => {
                terminate_child(&mut child);
                return Err(LaunchError::MissingPipe("stdout"));
            }
        };
        let mut stderr = match child.stderr.take() {
            Some(stderr) => stderr,
            None => {
                terminate_child(&mut child);
                return Err(LaunchError::MissingPipe("stderr"));
            }
        };

        // Both directions run off the supervisor thread. In particular, a
        // worker that stops reading a checkpoint-sized request must still be
        // cancellable by the request deadline instead of blocking the Studio
        // inside `write_all`.
        let (event_tx, event_rx) = mpsc::sync_channel(1);
        let response_tx = event_tx.clone();
        let response_thread = std::thread::spawn(move || {
            loop {
                let response = read_response(&mut stdout, limits);
                let terminal = response.is_err();
                if response_tx.send(PipeEvent::Response(response)).is_err() || terminal {
                    break;
                }
            }
        });
        let (request_tx, request_rx) = mpsc::sync_channel::<Vec<u8>>(1);
        let writer_thread = std::thread::spawn(move || {
            while let Ok(request) = request_rx.recv() {
                if let Err(error) = write_document(&mut stdin, &request, limits) {
                    let _ = event_tx.send(PipeEvent::WriteFailed(error));
                    break;
                }
            }
        });

        let tail = Arc::new(Mutex::new(VecDeque::new()));
        let tail_writer = Arc::clone(&tail);
        let tail_cap = self.stderr_tail_bytes;
        let stderr_thread = std::thread::spawn(move || {
            let mut buffer = [0u8; 8192];
            loop {
                match stderr.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(count) => {
                        let mut tail = tail_writer.lock().unwrap_or_else(lock_poisoned);
                        tail.extend(&buffer[..count]);
                        while tail.len() > tail_cap {
                            tail.pop_front();
                        }
                    }
                }
            }
        });

        Ok(Box::new(ChildPipeChannel {
            child: Some(child),
            request_tx: Some(request_tx),
            event_rx: Some(event_rx),
            writer_thread: Some(writer_thread),
            response_thread: Some(response_thread),
            stderr_thread: Some(stderr_thread),
            stderr_tail: tail,
            limits,
        }))
    }
}

struct ChildPipeChannel {
    child: Option<Child>,
    request_tx: Option<mpsc::SyncSender<Vec<u8>>>,
    event_rx: Option<mpsc::Receiver<PipeEvent>>,
    writer_thread: Option<JoinHandle<()>>,
    response_thread: Option<JoinHandle<()>>,
    stderr_thread: Option<JoinHandle<()>>,
    stderr_tail: Arc<Mutex<VecDeque<u8>>>,
    limits: ProtocolLimits,
}

impl ChildPipeChannel {
    fn tail(&self) -> Vec<u8> {
        self.stderr_tail
            .lock()
            .unwrap_or_else(lock_poisoned)
            .iter()
            .copied()
            .collect()
    }

    fn terminal_framing_error(&mut self, error: FramingError) -> ChannelError {
        let kind = match &error {
            FramingError::Closed => ChannelFailureKind::Closed,
            FramingError::Io(_) => ChannelFailureKind::Io,
            FramingError::FrameTooLarge { .. } | FramingError::Protocol(_) => {
                ChannelFailureKind::Protocol
            }
        };
        let detail = error.to_string();
        self.stop();
        ChannelError {
            kind,
            detail,
            stderr_tail: self.tail(),
        }
    }

    fn stop(&mut self) {
        self.request_tx.take();
        // Bounded event senders must never remain blocked while this method
        // joins their threads. Dropping the receiver makes in-flight sends
        // fail promptly before the child is reaped.
        self.event_rx.take();
        if let Some(child) = self.child.as_mut() {
            let deadline = Instant::now() + Duration::from_millis(100);
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    Ok(None) | Err(_) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        break;
                    }
                }
            }
        }
        self.child.take();
        if let Some(thread) = self.writer_thread.take() {
            let _ = thread.join();
        }
        if let Some(thread) = self.response_thread.take() {
            let _ = thread.join();
        }
        if let Some(thread) = self.stderr_thread.take() {
            let _ = thread.join();
        }
    }
}

impl WorkerChannel for ChildPipeChannel {
    fn exchange(
        &mut self,
        request: &RequestEnvelope,
        timeout: Duration,
    ) -> Result<ResponseEnvelope, ChannelError> {
        let request_id = request.request_id;
        let request = match request.to_bytes(self.limits) {
            Ok(request) => request,
            Err(error) => {
                return Err(self.terminal_framing_error(FramingError::Protocol(error)));
            }
        };
        let Some(sender) = self.request_tx.as_ref() else {
            return Err(ChannelError {
                kind: ChannelFailureKind::Closed,
                detail: "worker request writer is already closed".to_owned(),
                stderr_tail: self.tail(),
            });
        };
        if sender.send(request).is_err() {
            self.stop();
            return Err(ChannelError {
                kind: ChannelFailureKind::Closed,
                detail: "worker request writer disconnected".to_owned(),
                stderr_tail: self.tail(),
            });
        }
        let Some(receiver) = self.event_rx.as_ref() else {
            return Err(ChannelError {
                kind: ChannelFailureKind::Closed,
                detail: "worker response reader is already closed".to_owned(),
                stderr_tail: self.tail(),
            });
        };
        let response = match receiver.recv_timeout(timeout) {
            Ok(PipeEvent::Response(Ok(response))) => response,
            Ok(PipeEvent::Response(Err(error)) | PipeEvent::WriteFailed(error)) => {
                return Err(self.terminal_framing_error(error));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let detail = format!("worker did not answer within {timeout:?}");
                self.stop();
                return Err(ChannelError {
                    kind: ChannelFailureKind::Timeout,
                    detail,
                    stderr_tail: self.tail(),
                });
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                self.stop();
                return Err(ChannelError {
                    kind: ChannelFailureKind::Closed,
                    detail: "worker response reader disconnected".to_owned(),
                    stderr_tail: self.tail(),
                });
            }
        };
        // ubs:ignore - request IDs are public correlation integers, not secrets.
        if response.request_id != request_id {
            let error = ChannelError {
                kind: ChannelFailureKind::Correlation,
                detail: format!(
                    "request {} received response {}",
                    request_id, response.request_id
                ),
                stderr_tail: self.tail(),
            };
            self.stop();
            return Err(error);
        }
        Ok(response)
    }

    fn terminate(&mut self) {
        self.stop();
    }
}

fn terminate_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

impl Drop for ChildPipeChannel {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Supervisor recovery policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SupervisorConfig {
    /// Deadline for one worker request.
    pub request_timeout: Duration,
    /// PG-4 edit-to-frame budget.
    pub edit_to_frame_budget: Duration,
    /// IPC resource budgets.
    pub protocol_limits: ProtocolLimits,
    /// Content/build identity advertised to the worker and journaled by a
    /// service that implements `WorkerService::begin_session`.
    pub supervisor_build_id: Digest,
    /// Replace a crashed/closed worker automatically.
    pub auto_restart: bool,
    /// Most recent crash reports retained in memory. Zero disables history;
    /// the current crash is still returned to the caller.
    pub max_crash_reports: usize,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(10),
            edit_to_frame_budget: Duration::from_secs(1),
            protocol_limits: ProtocolLimits::default(),
            supervisor_build_id: sha256(
                concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION")).as_bytes(),
            ),
            auto_restart: true,
            max_crash_reports: 16,
        }
    }
}

/// Where restored checkpoint bytes came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckpointSource {
    /// Verified hit in the persistent fmn-cache namespace.
    Cache,
    /// Supervisor's in-memory safety copy.
    SupervisorMemory,
    /// Original bytes embedded in the replay journal.
    Journal,
}

/// Result of one rebuild/restart/restore/replay cycle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryReport {
    /// Conservative reuse plan.
    pub plan: ReplayPlan,
    /// Restored post-command checkpoint index.
    pub restored_checkpoint: Option<usize>,
    /// Storage tier that supplied it.
    pub checkpoint_source: Option<CheckpointSource>,
    /// Entries actually re-executed during recovery.
    pub replayed_entries: usize,
    /// First unexpected state hash, if the audited fast path diverged.
    pub diverged_at: Option<usize>,
    /// Whether the supervisor discarded reuse and re-executed from the top.
    pub cold_fallback: bool,
    /// Build-through-restored-state elapsed time.
    pub elapsed: Duration,
    /// Whether elapsed time met PG-4's `<1s` contract.
    pub within_edit_to_frame_budget: bool,
    /// Worker generation after recovery.
    pub generation: u64,
}

/// Result of a user request that may have triggered crash recovery.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SupervisorReply {
    /// Worker answered normally.
    Worker(WorkerResponse),
    /// Worker failed; the supervisor stayed alive and restored a replacement.
    Recovered {
        /// Structured crash evidence.
        crash: CrashReport,
        /// Replacement recovery.
        recovery: RecoveryReport,
    },
}

/// Supervisor orchestration failure.
#[derive(Debug)]
pub enum SupervisorError {
    /// Build failed.
    Build(BuildError),
    /// Launch failed.
    Launch(LaunchError),
    /// Worker channel failed.
    Channel(ChannelError),
    /// Canonical IPC failed.
    Protocol(ProtocolError),
    /// Journal serialization failed.
    Serial(SerialError),
    /// No worker is running.
    NoWorker,
    /// No replay session has been installed.
    NoSession,
    /// Request correlation id exhausted.
    RequestIdExhausted,
    /// Handshake succeeded syntactically but not semantically.
    Handshake(&'static str),
    /// Worker executable identity did not match the built artifact.
    BuildIdentityMismatch {
        /// Expected artifact.
        expected: Digest,
        /// Running worker.
        found: Digest,
    },
    /// Worker returned a typed refusal during recovery.
    WorkerRefusal {
        /// Stable class.
        code: WorkerErrorCode,
        /// Diagnostic.
        message: String,
    },
    /// Worker returned the wrong response variant.
    UnexpectedResponse(&'static str),
    /// Session/checkpoint invariants failed.
    InvalidSession(&'static str),
    /// A worker journal segment was not a valid canonical journal.
    InvalidJournal(String),
}

impl fmt::Display for SupervisorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Build(error) => error.fmt(f),
            Self::Launch(error) => error.fmt(f),
            Self::Channel(error) => error.fmt(f),
            Self::Protocol(error) => error.fmt(f),
            Self::Serial(error) => write!(f, "replay journal serialization failed: {error}"),
            Self::NoWorker => f.write_str("no scene worker is running"),
            Self::NoSession => f.write_str("no Studio replay session is installed"),
            Self::RequestIdExhausted => f.write_str("worker request id exhausted"),
            Self::Handshake(message) => write!(f, "worker handshake failed: {message}"),
            Self::BuildIdentityMismatch { expected, found } => write!(
                f,
                "worker build identity mismatch: expected {}, found {}",
                expected.to_hex(),
                found.to_hex()
            ),
            Self::WorkerRefusal { code, message } => {
                write!(f, "worker refused recovery ({code:?}): {message}")
            }
            Self::UnexpectedResponse(message) => {
                write!(f, "unexpected worker response: {message}")
            }
            Self::InvalidSession(message) => write!(f, "invalid Studio session: {message}"),
            Self::InvalidJournal(message) => {
                write!(f, "invalid worker journal segment: {message}")
            }
        }
    }
}

impl std::error::Error for SupervisorError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Build(error) => Some(error),
            Self::Launch(error) => Some(error),
            Self::Channel(error) => Some(error),
            Self::Protocol(error) => Some(error),
            Self::Serial(error) => Some(error),
            Self::NoWorker
            | Self::NoSession
            | Self::RequestIdExhausted
            | Self::Handshake(_)
            | Self::BuildIdentityMismatch { .. }
            | Self::WorkerRefusal { .. }
            | Self::UnexpectedResponse(_)
            | Self::InvalidSession(_)
            | Self::InvalidJournal(_) => None,
        }
    }
}

impl From<BuildError> for SupervisorError {
    fn from(error: BuildError) -> Self {
        Self::Build(error)
    }
}

impl From<LaunchError> for SupervisorError {
    fn from(error: LaunchError) -> Self {
        Self::Launch(error)
    }
}

impl From<ChannelError> for SupervisorError {
    fn from(error: ChannelError) -> Self {
        Self::Channel(error)
    }
}

impl From<ProtocolError> for SupervisorError {
    fn from(error: ProtocolError) -> Self {
        Self::Protocol(error)
    }
}

impl From<SerialError> for SupervisorError {
    fn from(error: SerialError) -> Self {
        Self::Serial(error)
    }
}

#[derive(Clone, Debug)]
struct StoredCheckpoint {
    state_hash: Digest,
    blob_digest: Option<Digest>,
    fallback: Vec<u8>,
}

/// Stable Studio supervisor.
pub struct Supervisor {
    launcher: Box<dyn WorkerLauncher>,
    clock: Arc<dyn Clock>,
    checkpoint_cache: Namespace,
    config: SupervisorConfig,
    worker: Option<Box<dyn WorkerChannel>>,
    artifact: Option<WorkerArtifact>,
    generation: u64,
    next_request_id: u64,
    scene: Option<String>,
    journal: Journal,
    checkpoints: BTreeMap<usize, StoredCheckpoint>,
    journal_cache_digest: Option<Digest>,
    crashes: Vec<CrashReport>,
    transports: Option<TransportCapabilities>,
}

impl fmt::Debug for Supervisor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Supervisor")
            .field("config", &self.config)
            .field("has_worker", &self.worker.is_some())
            .field("generation", &self.generation)
            .field("scene", &self.scene)
            .field("journal_entries", &self.journal.entries().len())
            .field("checkpoints", &self.checkpoints.len())
            .field("crashes", &self.crashes.len())
            .field("transports", &self.transports)
            .finish_non_exhaustive()
    }
}

impl Supervisor {
    /// Construct a stable supervisor around an injected launcher, clock, and
    /// manually-managed fmn-cache namespace.
    #[must_use]
    pub fn new(
        launcher: Box<dyn WorkerLauncher>,
        clock: Arc<dyn Clock>,
        checkpoint_cache: Namespace,
        config: SupervisorConfig,
    ) -> Self {
        Self {
            launcher,
            clock,
            checkpoint_cache,
            config,
            worker: None,
            artifact: None,
            generation: 0,
            next_request_id: 1,
            scene: None,
            journal: Journal::new(),
            checkpoints: BTreeMap::new(),
            journal_cache_digest: None,
            crashes: Vec::new(),
            transports: None,
        }
    }

    /// Current disposable-worker generation.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Immutable IPC resource budgets enforced for this supervisor session.
    #[must_use]
    pub const fn protocol_limits(&self) -> ProtocolLimits {
        self.config.protocol_limits
    }

    /// Crash reports retained by the stable supervisor.
    #[must_use]
    pub fn crashes(&self) -> &[CrashReport] {
        &self.crashes
    }

    fn retain_crash(&mut self, report: CrashReport) {
        let limit = self.config.max_crash_reports;
        if limit == 0 {
            return;
        }
        while self.crashes.len() >= limit {
            self.crashes.remove(0);
        }
        self.crashes.push(report);
    }

    /// Negotiated frame transports.
    #[must_use]
    pub const fn transports(&self) -> Option<TransportCapabilities> {
        self.transports
    }

    /// Cached journal content address, when the best-effort cache write
    /// succeeded.
    #[must_use]
    pub const fn journal_cache_digest(&self) -> Option<Digest> {
        self.journal_cache_digest
    }

    /// Install the current scene and replay record.  Every checkpoint is
    /// integrity-checked before it becomes recovery authority and is written
    /// best-effort into the warm cache.
    pub fn install_session(
        &mut self,
        scene: impl Into<String>,
        journal: Journal,
    ) -> Result<(), SupervisorError> {
        let scene = scene.into();
        if scene.is_empty() {
            return Err(SupervisorError::InvalidSession("empty scene name"));
        }
        if scene.len() > self.config.protocol_limits.max_field_bytes {
            return Err(SupervisorError::InvalidSession(
                "scene name exceeds protocol field budget",
            ));
        }
        let journal_bytes = journal.to_bytes()?;
        let journal_limit = self
            .config
            .protocol_limits
            .max_journal_bytes
            .min(self.config.protocol_limits.max_field_bytes);
        if journal_bytes.len() > journal_limit {
            return Err(SupervisorError::InvalidSession(
                "journal exceeds protocol payload budget",
            ));
        }
        let checkpoint_limit = self
            .config
            .protocol_limits
            .max_checkpoint_bytes
            .min(self.config.protocol_limits.max_field_bytes);
        let mut validated = Vec::new();
        for (index, entry) in journal.entries().iter().enumerate() {
            if let Some(state) = &entry.checkpoint {
                if state.len() > checkpoint_limit {
                    return Err(SupervisorError::InvalidSession(
                        "checkpoint exceeds protocol payload budget",
                    ));
                }
                if sha256(state) != entry.state_hash {
                    return Err(SupervisorError::InvalidSession(
                        "checkpoint bytes do not match their state hash",
                    ));
                }
                validated.push((index, entry.state_hash, state.clone()));
            }
        }
        let mut checkpoints = BTreeMap::new();
        for (index, state_hash, state) in validated {
            checkpoints.insert(index, self.stored_checkpoint(state_hash, state));
        }
        let journal_cache_digest = self.checkpoint_cache.put_blob(&journal_bytes).ok();

        // Commit only after the complete incoming session has validated, so a
        // malformed replacement cannot erase a previously recoverable one.
        self.scene = Some(scene);
        self.journal = journal;
        self.checkpoints = checkpoints;
        self.journal_cache_digest = journal_cache_digest;
        Ok(())
    }

    /// Start the first worker after an incremental build.
    pub fn build_and_start(
        &mut self,
        builder: &mut dyn RebuildDriver,
    ) -> Result<(), SupervisorError> {
        let artifact = builder.rebuild()?;
        self.launch_and_handshake(artifact)
    }

    /// Incrementally rebuild, replace the worker, restore the nearest reusable
    /// checkpoint, and replay the verified suffix.
    pub fn rebuild_and_restart(
        &mut self,
        builder: &mut dyn RebuildDriver,
        incoming: &[CommandRecord],
        asset_ok: &dyn Fn(&AssetRead) -> bool,
    ) -> Result<RecoveryReport, SupervisorError> {
        let started = self.clock.monotonic();
        let artifact = builder.rebuild()?;
        self.launch_and_handshake(artifact)?;
        self.recover(incoming, asset_ok, started)
    }

    /// Send one user operation.  A reported panic or recoverable pipe failure
    /// automatically replaces and restores the worker when configured.
    pub fn request(
        &mut self,
        request: SupervisorRequest,
        asset_ok: &dyn Fn(&AssetRead) -> bool,
    ) -> Result<SupervisorReply, SupervisorError> {
        let response = self.exchange(request);
        match response {
            Ok(WorkerResponse::Crash(report)) if self.config.auto_restart => {
                self.recover_from_crash(report, asset_ok)
            }
            Ok(WorkerResponse::Checkpoint(checkpoint)) => {
                self.accept_checkpoint(&checkpoint)?;
                Ok(SupervisorReply::Worker(WorkerResponse::Checkpoint(
                    checkpoint,
                )))
            }
            Ok(response @ WorkerResponse::JournalSegment { .. }) => {
                if let WorkerResponse::JournalSegment {
                    scene,
                    start_entry,
                    journal,
                } = &response
                {
                    self.merge_journal_segment(scene, *start_entry, journal)?;
                }
                Ok(SupervisorReply::Worker(response))
            }
            Ok(response) => Ok(SupervisorReply::Worker(response)),
            Err(SupervisorError::Channel(error))
                if self.config.auto_restart && error.recoverable() =>
            {
                let report = self.channel_crash_report(&error);
                self.recover_from_crash(report, asset_ok)
            }
            Err(error) => Err(error),
        }
    }

    /// Gracefully shut down the current worker.  The stable supervisor,
    /// journals, and cache remain available for a later start.
    pub fn shutdown_worker(&mut self) {
        self.stop_worker(true);
    }

    fn recover_from_crash(
        &mut self,
        report: CrashReport,
        asset_ok: &dyn Fn(&AssetRead) -> bool,
    ) -> Result<SupervisorReply, SupervisorError> {
        let started = self.clock.monotonic();
        self.retain_crash(report.clone());
        let artifact = self.artifact.clone().ok_or(SupervisorError::NoWorker)?;
        let incoming: Vec<CommandRecord> = self
            .journal
            .entries()
            .iter()
            .map(|entry| entry.command.clone())
            .collect();
        self.launch_and_handshake(artifact)?;
        let recovery = self.recover(&incoming, asset_ok, started)?;
        Ok(SupervisorReply::Recovered {
            crash: report,
            recovery,
        })
    }

    fn launch_and_handshake(&mut self, artifact: WorkerArtifact) -> Result<(), SupervisorError> {
        artifact.validate()?;
        let next_generation =
            self.generation
                .checked_add(1)
                .ok_or(SupervisorError::InvalidSession(
                    "worker generation exhausted",
                ))?;
        let max_frame_bytes = u64::try_from(self.config.protocol_limits.max_frame_bytes)
            .map_err(|_| SupervisorError::InvalidSession("frame budget exceeds u64"))?;
        let request = RequestEnvelope {
            request_id: self.take_request_id()?,
            request: SupervisorRequest::Hello {
                version: CURRENT_VERSION,
                supervisor_build: self.config.supervisor_build_id,
                max_frame_bytes,
            },
        };
        let mut worker = self
            .launcher
            .launch(&artifact, self.config.protocol_limits)?;
        let response = match worker.exchange(&request, self.config.request_timeout) {
            Ok(response) => response,
            Err(error) => {
                worker.terminate();
                return Err(error.into());
            }
        };
        // ubs:ignore - request IDs are public correlation integers, not secrets.
        if response.request_id != request.request_id {
            worker.terminate();
            return Err(SupervisorError::Handshake(
                "response correlation id did not match",
            ));
        }
        let (version, worker_build, transports) = match response.response {
            WorkerResponse::Hello {
                version,
                worker_build,
                transports,
            } => (version, worker_build, transports),
            WorkerResponse::Error { code, message } => {
                worker.terminate();
                return Err(SupervisorError::WorkerRefusal { code, message });
            }
            _ => {
                worker.terminate();
                return Err(SupervisorError::Handshake(
                    "worker did not answer Hello with Hello",
                ));
            }
        };
        if let Err(error) = version.require_current() {
            worker.terminate();
            return Err(error.into());
        }
        if worker_build != artifact.build_id {
            worker.terminate();
            return Err(SupervisorError::BuildIdentityMismatch {
                expected: artifact.build_id,
                found: worker_build,
            });
        }
        if !transports.pipe && !transports.shared_memory {
            worker.terminate();
            return Err(SupervisorError::Handshake(
                "worker advertised no frame transport",
            ));
        }

        // The healthy old worker remains available until its replacement has
        // passed the exact-version/build handshake. A failed build or bad
        // candidate therefore cannot turn a working Studio session dark.
        self.stop_worker(true);
        self.worker = Some(worker);
        self.artifact = Some(artifact);
        self.generation = next_generation;
        self.transports = Some(transports);
        Ok(())
    }

    fn recover(
        &mut self,
        incoming: &[CommandRecord],
        asset_ok: &dyn Fn(&AssetRead) -> bool,
        started: Duration,
    ) -> Result<RecoveryReport, SupervisorError> {
        let scene = self.scene.clone().ok_or(SupervisorError::NoSession)?;
        let plan = plan_replay(&self.journal, incoming, asset_ok);
        let journal_bytes = self.journal.to_bytes()?;
        let mut restored_checkpoint = None;
        let mut checkpoint_source = None;
        let mut replay_start = 0usize;

        if let Some(index) = plan.resume_checkpoint {
            let (state, source, state_hash) = self.load_checkpoint(index)?;
            let after_entry = u64::try_from(index)
                .map_err(|_| SupervisorError::InvalidSession("checkpoint index exceeds u64"))?;
            let expected_journal_len =
                after_entry
                    .checked_add(1)
                    .ok_or(SupervisorError::InvalidSession(
                        "checkpoint journal length exceeds u64",
                    ))?;
            let response = self.exchange(SupervisorRequest::RestoreCheckpoint(Checkpoint {
                scene: scene.clone(),
                after_entry,
                state_hash,
                state,
            }))?;
            match response {
                WorkerResponse::Ack {
                    state_hash: Some(found),
                    journal_len,
                } if found == state_hash && journal_len == expected_journal_len => {}
                WorkerResponse::Error { code, message } => {
                    return Err(SupervisorError::WorkerRefusal { code, message });
                }
                WorkerResponse::Crash(report) => {
                    self.retain_crash(report);
                    return Err(SupervisorError::UnexpectedResponse(
                        "worker crashed while restoring a checkpoint",
                    ));
                }
                _ => {
                    return Err(SupervisorError::UnexpectedResponse(
                        "checkpoint restore did not acknowledge the exact state/hash position",
                    ));
                }
            }
            restored_checkpoint = Some(index);
            checkpoint_source = Some(source);
            replay_start = index + 1;
        }

        let mut replayed_entries = 0usize;
        let mut diverged_at = None;
        let mut cold_fallback = false;
        if replay_start < plan.reuse {
            let hashes = self.replay_range(&scene, &journal_bytes, replay_start, plan.reuse)?;
            replayed_entries = hashes.len();
            let mut audit = ReplayAudit::new();
            for (offset, hash) in hashes.iter().enumerate() {
                let index = replay_start + offset;
                if !audit.step(&self.journal, index, hash) {
                    diverged_at = Some(index);
                    break;
                }
            }
        }

        if diverged_at.is_some() {
            cold_fallback = true;
            restored_checkpoint = None;
            checkpoint_source = None;
            let artifact = self.artifact.clone().ok_or(SupervisorError::NoWorker)?;
            self.launch_and_handshake(artifact)?;
            let hashes = self.replay_range(&scene, &journal_bytes, 0, plan.reuse)?;
            if hashes.len() != plan.reuse {
                return Err(SupervisorError::UnexpectedResponse(
                    "cold replay returned the wrong state-hash count",
                ));
            }
            replayed_entries = hashes.len();
        }

        let elapsed = self.clock.monotonic().saturating_sub(started);
        Ok(RecoveryReport {
            plan,
            restored_checkpoint,
            checkpoint_source,
            replayed_entries,
            diverged_at,
            cold_fallback,
            elapsed,
            within_edit_to_frame_budget: elapsed < self.config.edit_to_frame_budget,
            generation: self.generation,
        })
    }

    fn replay_range(
        &mut self,
        scene: &str,
        journal: &[u8],
        from: usize,
        through: usize,
    ) -> Result<Vec<Digest>, SupervisorError> {
        let from_entry = u64::try_from(from)
            .map_err(|_| SupervisorError::InvalidSession("replay start exceeds u64"))?;
        let through_entry = u64::try_from(through)
            .map_err(|_| SupervisorError::InvalidSession("replay end exceeds u64"))?;
        let response = self.exchange(SupervisorRequest::ReplayJournal(JournalReplay {
            scene: scene.to_owned(),
            from_entry,
            through_entry,
            journal: journal.to_vec(),
        }))?;
        match response {
            WorkerResponse::ReplayComplete {
                from_entry: found_from,
                state_hashes,
            } if found_from == from_entry && state_hashes.len() == through - from => {
                Ok(state_hashes)
            }
            WorkerResponse::Error { code, message } => {
                Err(SupervisorError::WorkerRefusal { code, message })
            }
            WorkerResponse::Crash(report) => {
                self.retain_crash(report);
                Err(SupervisorError::UnexpectedResponse(
                    "worker crashed during journal replay",
                ))
            }
            _ => Err(SupervisorError::UnexpectedResponse(
                "journal replay response range/count did not match",
            )),
        }
    }

    fn exchange(&mut self, request: SupervisorRequest) -> Result<WorkerResponse, SupervisorError> {
        let envelope = RequestEnvelope {
            request_id: self.take_request_id()?,
            request,
        };
        let worker = self.worker.as_mut().ok_or(SupervisorError::NoWorker)?;
        let response = worker.exchange(&envelope, self.config.request_timeout)?;
        // ubs:ignore - request IDs are public correlation integers, not secrets.
        if response.request_id != envelope.request_id {
            return Err(ChannelError::new(
                ChannelFailureKind::Correlation,
                format!(
                    "request {} received response {}",
                    envelope.request_id, response.request_id
                ),
            )
            .into());
        }
        if let (Some(request_scene), Some(response_scene)) = (
            supervisor_request_scene(&envelope.request),
            worker_response_scene(&response.response),
        ) {
            // ubs:ignore - scene names are public routing identifiers, not secrets.
            if request_scene != response_scene {
                return Err(ChannelError::new(
                    ChannelFailureKind::Correlation,
                    "scene-bearing response did not match request scene",
                )
                .into());
            }
        }
        Ok(response.response)
    }

    fn accept_checkpoint(&mut self, checkpoint: &Checkpoint) -> Result<(), SupervisorError> {
        let scene = self.scene.as_deref().ok_or(SupervisorError::NoSession)?;
        // ubs:ignore - scene names are public routing identifiers, not secrets.
        if checkpoint.scene != scene {
            return Err(SupervisorError::InvalidSession(
                "worker pushed a checkpoint for a different scene",
            ));
        }
        if sha256(&checkpoint.state) != checkpoint.state_hash {
            return Err(SupervisorError::InvalidSession(
                "worker pushed a checkpoint with the wrong state hash",
            ));
        }
        let index = usize::try_from(checkpoint.after_entry)
            .map_err(|_| SupervisorError::InvalidSession("checkpoint index exceeds usize"))?;
        let expected = self
            .journal
            .entries()
            .get(index)
            .ok_or(SupervisorError::InvalidSession(
                "worker pushed a checkpoint beyond the journal",
            ))?;
        if expected.state_hash != checkpoint.state_hash {
            return Err(SupervisorError::InvalidSession(
                "worker checkpoint does not match the journal entry state hash",
            ));
        }
        self.store_checkpoint(index, checkpoint.state_hash, checkpoint.state.clone());
        Ok(())
    }

    fn merge_journal_segment(
        &mut self,
        scene: &str,
        start_entry: u64,
        bytes: &[u8],
    ) -> Result<(), SupervisorError> {
        let current_scene = self.scene.as_deref().ok_or(SupervisorError::NoSession)?;
        // ubs:ignore - scene names are public routing identifiers, not secrets.
        if scene != current_scene {
            return Err(SupervisorError::InvalidSession(
                "worker pushed a journal for a different scene",
            ));
        }
        let start = usize::try_from(start_entry)
            .map_err(|_| SupervisorError::InvalidSession("journal start exceeds usize"))?;
        if start > self.journal.entries().len() {
            return Err(SupervisorError::InvalidSession(
                "journal segment begins beyond the current journal",
            ));
        }
        let segment = Journal::from_bytes(bytes)
            .map_err(|error| SupervisorError::InvalidJournal(error.to_string()))?;
        let mut merged = Journal::new();
        for entry in &self.journal.entries()[..start] {
            merged.record(entry.clone());
        }
        for entry in segment.entries() {
            merged.record(entry.clone());
        }
        self.install_session(scene.to_owned(), merged)
    }

    fn store_checkpoint(&mut self, index: usize, state_hash: Digest, state: Vec<u8>) {
        let stored = self.stored_checkpoint(state_hash, state);
        self.checkpoints.insert(index, stored);
    }

    fn stored_checkpoint(&self, state_hash: Digest, state: Vec<u8>) -> StoredCheckpoint {
        let blob_digest = self
            .checkpoint_cache
            .put_blob(&state)
            .ok()
            // ubs:ignore - this digest is a public state integrity identifier.
            .filter(|digest| *digest == state_hash);
        StoredCheckpoint {
            state_hash,
            blob_digest,
            fallback: state,
        }
    }

    fn load_checkpoint(
        &self,
        index: usize,
    ) -> Result<(Vec<u8>, CheckpointSource, Digest), SupervisorError> {
        if let Some(stored) = self.checkpoints.get(&index) {
            if let Some(digest) = stored.blob_digest
                && let Ok(Some(bytes)) = self.checkpoint_cache.get_blob(&digest)
                && sha256(&bytes) == stored.state_hash
            {
                return Ok((bytes, CheckpointSource::Cache, stored.state_hash));
            }
            if sha256(&stored.fallback) == stored.state_hash {
                return Ok((
                    stored.fallback.clone(),
                    CheckpointSource::SupervisorMemory,
                    stored.state_hash,
                ));
            }
        }
        let entry = self
            .journal
            .entries()
            .get(index)
            .ok_or(SupervisorError::InvalidSession(
                "replay plan named an absent checkpoint entry",
            ))?;
        let bytes = entry
            .checkpoint
            .as_ref()
            .ok_or(SupervisorError::InvalidSession(
                "replay plan named an entry without a checkpoint",
            ))?;
        if sha256(bytes) != entry.state_hash {
            return Err(SupervisorError::InvalidSession(
                "journal checkpoint failed integrity verification",
            ));
        }
        Ok((bytes.clone(), CheckpointSource::Journal, entry.state_hash))
    }

    fn channel_crash_report(&self, error: &ChannelError) -> CrashReport {
        let journal_bytes = self.journal.to_bytes().unwrap_or_default();
        let tail_len = journal_bytes
            .len()
            .min(self.config.protocol_limits.max_crash_tail_bytes);
        let mut journal_tail = Vec::with_capacity(tail_len);
        journal_tail.extend_from_slice(&journal_bytes[journal_bytes.len() - tail_len..]);
        let message = bounded_channel_error_message(
            error,
            self.config
                .protocol_limits
                .max_crash_message_bytes
                .min(self.config.protocol_limits.max_field_bytes),
        );
        CrashReport {
            scene: self.scene.clone(),
            message,
            journal_tail,
            state_hash: self.journal.entries().last().map(|entry| entry.state_hash),
        }
    }

    fn take_request_id(&mut self) -> Result<u64, SupervisorError> {
        let request_id = self.next_request_id;
        self.next_request_id = self
            .next_request_id
            .checked_add(1)
            .ok_or(SupervisorError::RequestIdExhausted)?;
        Ok(request_id)
    }

    fn stop_worker(&mut self, graceful: bool) {
        let Some(mut worker) = self.worker.take() else {
            return;
        };
        if graceful && let Ok(request_id) = self.take_request_id() {
            let _ = worker.exchange(
                &RequestEnvelope {
                    request_id,
                    request: SupervisorRequest::Shutdown,
                },
                self.config.request_timeout.min(Duration::from_millis(250)),
            );
        }
        worker.terminate();
        self.transports = None;
    }
}

fn supervisor_request_scene(request: &SupervisorRequest) -> Option<&str> {
    match request {
        SupervisorRequest::Play { scene, .. }
        | SupervisorRequest::Seek { scene, .. }
        | SupervisorRequest::Scrub { scene, .. }
        | SupervisorRequest::Event { scene, .. }
        | SupervisorRequest::Inspect { scene }
        | SupervisorRequest::Overlay { scene, .. } => Some(scene),
        SupervisorRequest::RestoreCheckpoint(checkpoint) => Some(&checkpoint.scene),
        SupervisorRequest::ReplayJournal(replay) => Some(&replay.scene),
        SupervisorRequest::Hello { .. }
        | SupervisorRequest::EnumerateScenes
        | SupervisorRequest::Shutdown => None,
    }
}

fn worker_response_scene(response: &WorkerResponse) -> Option<&str> {
    match response {
        WorkerResponse::Frame(frame) => Some(&frame.scene),
        WorkerResponse::Checkpoint(checkpoint) => Some(&checkpoint.scene),
        WorkerResponse::JournalSegment { scene, .. } | WorkerResponse::StudioData { scene, .. } => {
            Some(scene)
        }
        WorkerResponse::Crash(report) => report.scene.as_deref(),
        WorkerResponse::Hello { .. }
        | WorkerResponse::Scenes(_)
        | WorkerResponse::Ack { .. }
        | WorkerResponse::ReplayComplete { .. }
        | WorkerResponse::Error { .. }
        | WorkerResponse::Bye => None,
    }
}

fn bounded_channel_error_message(error: &ChannelError, limit: usize) -> String {
    let limit = limit.max(1);
    let mut message = String::with_capacity(limit.min(256));
    push_str_bounded(&mut message, "worker channel ", limit);
    push_str_bounded(&mut message, channel_failure_name(error.kind), limit);
    push_str_bounded(&mut message, ": ", limit);
    push_str_bounded(&mut message, &error.detail, limit);
    if !error.stderr_tail.is_empty() {
        push_str_bounded(&mut message, "; stderr tail: ", limit);
        let remaining = limit.saturating_sub(message.len());
        let take = remaining.min(error.stderr_tail.len());
        let stderr = String::from_utf8_lossy(&error.stderr_tail[..take]);
        push_str_bounded(&mut message, &stderr, limit);
    }
    message
}

const fn channel_failure_name(kind: ChannelFailureKind) -> &'static str {
    match kind {
        ChannelFailureKind::Closed => "Closed",
        ChannelFailureKind::Timeout => "Timeout",
        ChannelFailureKind::Io => "Io",
        ChannelFailureKind::Protocol => "Protocol",
        ChannelFailureKind::Correlation => "Correlation",
    }
}

fn push_str_bounded(output: &mut String, value: &str, limit: usize) {
    let mut end = limit.saturating_sub(output.len()).min(value.len());
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    output.push_str(&value[..end]);
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        self.stop_worker(false);
    }
}
