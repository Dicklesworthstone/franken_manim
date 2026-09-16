//! Concrete native WorkerService, with cold, hash-verified recovery.

use fmn_render::RetainedFrameRendererConfig;
use fmn_scene::studio_bridge::{SceneState, Stage};
use fmn_scene::{AssetRead, CommandRecord, EffectClass, Entry, EventPayload, ImpureEffectTag, Journal};

use crate::protocol::studio_seek_frame;
use crate::{
    Checkpoint, JournalReplay, ProtocolDigest, ProtocolLimits, ResponseEnvelope,
    ServiceError, StudioDataKind, SupervisorRequest, WorkerErrorCode, WorkerResponse, WorkerService,
    protocol_digest,
};

use super::program::{execution_error, invalid};
use super::raster::NativeRaster;
use super::NativeSceneProgram;

/// Whether a factory may be run again during supervisor recovery.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum NativeReplayPolicy {
    /// Safe default for arbitrary native callbacks: recovery refuses, and
    /// committed commands are opaque replay barriers.
    #[default]
    Disabled,
    /// The author attests that every invocation creates independent callback
    /// captures and depends only on the content-hashed source/input closure.
    /// No unjournaled I/O or externally shared mutable state may affect it.
    /// Recovery still executes from zero and verifies each actual state hash;
    /// it never imports executable behavior from serialized SceneState bytes.
    ColdVerified,
}

/// Policy and content identities for a single registered native scene.
#[derive(Debug, Clone)]
pub struct NativeWorkerConfig {
    /// Stable scene name, also used by canonical Studio command identities.
    pub scene: String,
    /// Identity of the actual worker executable/build.
    pub build_id: ProtocolDigest,
    /// Digest covering scene code, assets and factory inputs (including seed).
    pub source_digest: ProtocolDigest,
    /// Number of addressable states, including the constructed frame zero.
    pub frame_count: u64,
    /// Exact factory Scene frame rate.
    pub fps: u32,
    /// The existing retained CPU renderer's complete policy.
    pub renderer: RetainedFrameRendererConfig,
    /// Canonical IPC and document limits; the handshake can only reduce them.
    pub limits: ProtocolLimits,
    /// Frame-distance cadence for checkpoint attachment to committed seeks.
    pub checkpoint_frames: u64,
    /// Explicit callback replay contract; disabled by default.
    pub replay: NativeReplayPolicy,
    /// Maximum total frame steps admitted by one seek or entire replay request.
    /// All replay targets are charged before any factory invocation.
    pub max_replay_frames: u64,
    /// Maximum viewport pixels, checked before allocating render buffers.
    pub max_pixels: u64,
}

impl NativeWorkerConfig {
    /// Configure a bounded CPU worker; arbitrary callbacks are not assumed
    /// replayable until the caller explicitly selects `ColdVerified`.
    #[must_use]
    pub fn new(
        scene: impl Into<String>,
        build_id: ProtocolDigest,
        source_digest: ProtocolDigest,
        frame_count: u64,
        fps: u32,
        renderer: RetainedFrameRendererConfig,
    ) -> Self {
        Self {
            scene: scene.into(),
            build_id,
            source_digest,
            frame_count,
            fps,
            renderer,
            limits: ProtocolLimits::default(),
            checkpoint_frames: 30,
            replay: NativeReplayPolicy::Disabled,
            max_replay_frames: 1_000_000,
            max_pixels: 16_777_216,
        }
    }
}

/// A production native implementation of the existing isolated worker protocol.
///
/// Feed this value to [`crate::serve_worker`]. Play records canonical committed
/// seeks. Clean forward previews resume the retained native program; backward
/// seeks and seeks after edits reconstruct from a fresh factory. No clock jumps
/// substitute for execution. At a capture,
/// Event edits the current paused owner and returns newly rendered PNG pixels.
/// Edits are deliberately transient: the next seek/recovery reconstructs from
/// source. Inspection and overlays read that same live arena, including edits.
///
/// The factory must create independent callback state on *every* invocation.
/// It is invoked inside the disposable worker, not inside the UI supervisor.
/// Native callbacks may panic; `serve_worker` supplies the crash boundary.
/// This implementation does not claim constant-time or warm callback recovery.
pub struct NativeSceneWorker {
    config: NativeWorkerConfig,
    factory: Box<dyn Fn() -> Result<NativeSceneProgram, ServiceError>>,
    program: Option<NativeSceneProgram>,
    preview_clean: bool,
    raster: NativeRaster,
    reads: Vec<AssetRead>,
    position: u64,
    last_hash: Option<ProtocolDigest>,
    last_checkpoint: Option<u64>,
    tail: Vec<u8>,
}

impl NativeSceneWorker {
    /// Construct one native scene service without playing or prerendering it.
    pub fn new(
        config: NativeWorkerConfig,
        factory: impl Fn() -> Result<NativeSceneProgram, ServiceError> + 'static,
    ) -> Result<Self, ServiceError> {
        crate::protocol::studio_seek_command(&config.scene, 0).map_err(execution_error)?;
        let viewport = config.renderer.frame.viewport;
        let pixels = u64::from(viewport.width) * u64::from(viewport.height);
        if config.frame_count == 0
            || config.frame_count > i64::MAX.cast_unsigned()
            || config.fps == 0
            || config.checkpoint_frames == 0
            || pixels == 0
            || pixels > config.max_pixels
        {
            return Err(invalid("invalid native worker frame, clock, checkpoint or pixel budget"));
        }
        let mut contract = b"fmn-native-studio-v1\0".to_vec();
        contract.extend_from_slice(&config.fps.to_le_bytes());
        contract.extend_from_slice(&config.frame_count.to_le_bytes());
        contract.extend_from_slice(config.scene.as_bytes());
        let reads = vec![
            AssetRead { path: "native/source-closure".into(), digest: config.source_digest },
            AssetRead { path: "native/worker-build".into(), digest: config.build_id },
            AssetRead { path: "native/capture-contract".into(), digest: protocol_digest(&contract) },
        ];
        crate::InspectorView::new(0, config.frame_count, config.fps,
            viewport, config.renderer.frame.map, true).map_err(execution_error)?;
        let raster = NativeRaster::new(config.renderer)?;
        let program = factory()?;
        Self::validate_program(&config, &program)?;
        Ok(Self {
            config, factory: Box::new(factory), program: Some(program), preview_clean: true, raster, reads,
            position: 0, last_hash: None, last_checkpoint: None, tail: Vec::new(),
        })
    }

    /// The current paused owner, when a failed input/render has not invalidated
    /// it. A successful seek reconstructs it without changing the replay journal.
    #[must_use]
    pub fn program(&self) -> Option<&NativeSceneProgram> {
        self.program.as_ref()
    }

    fn validate_program(config: &NativeWorkerConfig, program: &NativeSceneProgram) -> Result<(), ServiceError> {
        if program.frame_index() != 0
            || program.frame_limit() != config.frame_count - 1
            || program.preview().scene().fps() != config.fps
        {
            return Err(invalid("native factory disagrees with its initial-frame, length or fps contract"));
        }
        Ok(())
    }

    fn target(&self, frame: i64) -> Result<u64, ServiceError> {
        let frame = u64::try_from(frame).map_err(|_| invalid("negative native frame"))?;
        if frame >= self.config.frame_count || frame > self.config.max_replay_frames {
            return Err(invalid("native target exceeds the scene or execution budget"));
        }
        Ok(frame)
    }

    fn fresh_at(&self, frame: u64) -> Result<NativeSceneProgram, ServiceError> {
        let mut program = (self.factory)()?;
        Self::validate_program(&self.config, &program)?;
        program.advance_to(frame)?;
        Ok(program)
    }

    fn checked(&self, response: WorkerResponse) -> Result<WorkerResponse, ServiceError> {
        // Validate the *entire encoded envelope*, not just inner payload lengths,
        // before making a state transition observable to the supervisor.
        let envelope = ResponseEnvelope { request_id: 1, response };
        let _ = envelope.to_bytes(self.config.limits).map_err(execution_error)?;
        Ok(envelope.response)
    }

    fn require_scene(&self, scene: &str) -> Result<(), ServiceError> {
        if scene != self.config.scene {
            return Err(ServiceError::new(WorkerErrorCode::SceneNotFound, "native scene is not registered"));
        }
        Ok(())
    }

    fn effect(&self) -> EffectClass {
        match self.config.replay {
            NativeReplayPolicy::Disabled => EffectClass::Opaque,
            // Even an attested factory is never advertised as frame-parallel
            // pure. Its native callback/animation front end remains serial.
            NativeReplayPolicy::ColdVerified => EffectClass::Stateful(vec![ImpureEffectTag::UnclassifiedAnimation]),
        }
    }

    fn require_replay(&self, code: WorkerErrorCode) -> Result<(), ServiceError> {
        if self.config.replay == NativeReplayPolicy::Disabled {
            return Err(ServiceError::new(code, "native callback replay requires an explicit ColdVerified factory contract"));
        }
        Ok(())
    }

    fn seek(&mut self, frame: i64) -> Result<WorkerResponse, ServiceError> {
        let frame = self.target(frame)?;
        if self.preview_clean
            && self.program.as_ref().is_some_and(|program| program.frame_index() <= frame)
        {
            let mut program = self.program.take().ok_or_else(|| invalid("native cursor disappeared"))?;
            program.advance_to(frame)?;
            let response = self.raster.frame(&self.config.scene, &program)?;
            let response = self.checked(response)?;
            self.program = Some(program);
            return Ok(response);
        }
        let program = self.fresh_at(frame)?;
        let mut raster = NativeRaster::new(self.config.renderer)?;
        let response = raster.frame(&self.config.scene, &program)?;
        let response = self.checked(response)?;
        self.program = Some(program);
        self.preview_clean = true;
        self.raster = raster;
        Ok(response)
    }

    fn event(&mut self, event: EventPayload) -> Result<WorkerResponse, ServiceError> {
        event.validate().map_err(|error| invalid(error.to_string()))?;
        let mut program = self.program.take().ok_or_else(|| invalid("seek before sending input to a failed native preview"))?;
        // Callback captures cannot be rolled back by restoring record bytes.
        // A failed dispatch/render therefore leaves no reusable preview owner.
        self.preview_clean = false;
        program.dispatch(event)?;
        let response = self.raster.frame(&self.config.scene, &program)?;
        let response = self.checked(response)?;
        self.program = Some(program);
        Ok(response)
    }

    fn record_seek(&mut self, command: CommandRecord) -> Result<WorkerResponse, ServiceError> {
        let frame = studio_seek_frame(&self.config.scene, &command).map_err(|error| invalid(error.to_string()))?;
        let frame = self.target(frame)?;
        let next = self.position.checked_add(1).ok_or_else(|| invalid("native journal position exhausted"))?;
        let mut program = self.fresh_at(frame)?;
        let state = program.state_bytes()?;
        if state.len() > self.config.limits.max_checkpoint_bytes {
            return Err(execution_error("native SceneState exceeds the checkpoint budget"));
        }
        let state_hash = protocol_digest(&state);
        let checkpoint = self.last_checkpoint.is_none_or(|last| last.abs_diff(frame) >= self.config.checkpoint_frames);
        let entry = Entry {
            command, effect: self.effect(), reads: self.reads.clone(), subprocesses: Vec::new(),
            checkpoint: checkpoint.then_some(state), state_hash,
        };
        let mut journal = Journal::new();
        journal.record(entry).map_err(execution_error)?;
        let journal = journal.to_bytes().map_err(execution_error)?;
        let response = self.checked(WorkerResponse::JournalSegment {
            scene: self.config.scene.clone(), start_entry: self.position, journal: journal.clone(),
        })?;
        let raster = NativeRaster::new(self.config.renderer)?;
        self.program = Some(program);
        self.preview_clean = true;
        self.raster = raster;
        self.position = next;
        self.last_hash = Some(state_hash);
        self.tail = journal;
        if checkpoint { self.last_checkpoint = Some(frame); }
        Ok(response)
    }

    fn replay(&mut self, replay: JournalReplay) -> Result<WorkerResponse, ServiceError> {
        self.require_scene(&replay.scene)?;
        self.require_replay(WorkerErrorCode::ReplayFailed)?;
        let refuse = |message| ServiceError::new(WorkerErrorCode::ReplayFailed, message);
        if replay.from_entry != self.position
            || replay.from_entry > replay.through_entry
            || replay.journal.len() > self.config.limits.max_journal_bytes
        {
            return Err(refuse("native replay range or byte budget is invalid"));
        }
        let journal = Journal::from_bytes(&replay.journal).map_err(|error| refuse_owned(WorkerErrorCode::ReplayFailed, error))?;
        let from = usize::try_from(replay.from_entry).map_err(|_| refuse("native replay start exceeds usize"))?;
        let through = usize::try_from(replay.through_entry).map_err(|_| refuse("native replay end exceeds usize"))?;
        let entries = journal.entries().get(from..through).ok_or_else(|| refuse("native replay range exceeds the journal"))?;
        if !journal.events().is_empty() || entries.len() > self.config.limits.max_replay_hashes {
            return Err(refuse("native replay contains unowned input events or too many entries"));
        }
        let backend = self.raster.backend()?;
        if journal.render_backends().iter().any(|record| record != &backend) {
            return Err(refuse("native replay renderer identity differs from this worker"));
        }
        let mut total_frames = 0_u64;
        let mut targets = Vec::new();
        targets.try_reserve_exact(entries.len()).map_err(execution_error)?;
        for entry in entries {
            if entry.reads != self.reads || entry.effect != self.effect() || !entry.subprocesses.is_empty() {
                return Err(refuse("native replay closure or effect differs from this factory"));
            }
            let frame = studio_seek_frame(&self.config.scene, &entry.command)
                .map_err(|error| refuse_owned(WorkerErrorCode::ReplayFailed, error))?;
            let frame = self.target(frame).map_err(|error| refuse_owned(WorkerErrorCode::ReplayFailed, error))?;
            total_frames = total_frames.checked_add(frame).ok_or_else(|| refuse("native replay work overflows"))?;
            if total_frames > self.config.max_replay_frames {
                return Err(refuse("native replay exceeds its total frame-work budget"));
            }
            if let Some(state) = &entry.checkpoint {
                if state.len() > self.config.limits.max_checkpoint_bytes || protocol_digest(state) != entry.state_hash {
                    return Err(refuse("native replay checkpoint digest or budget is invalid"));
                }
            }
            targets.push(frame);
        }
        let mut hashes = Vec::new();
        hashes.try_reserve_exact(entries.len()).map_err(execution_error)?;
        let mut candidate = None;
        let mut last_checkpoint = self.last_checkpoint;
        for (entry, &frame) in entries.iter().zip(&targets) {
            let mut program = self.fresh_at(frame).map_err(|error| refuse_owned(WorkerErrorCode::ReplayFailed, error))?;
            let state = program.state_bytes()?;
            if state.len() > self.config.limits.max_checkpoint_bytes {
                return Err(refuse("native replay state exceeds the checkpoint budget"));
            }
            let hash = protocol_digest(&state);
            if hash != entry.state_hash {
                return Err(refuse("native replay state diverged; no partial replay was installed"));
            }
            hashes.push(hash);
            if entry.checkpoint.is_some() { last_checkpoint = Some(frame); }
            candidate = Some(program);
        }
        // Keep only an actually executed tail, never the unexecuted suffix
        // supplied by the caller. Empty replay preserves the existing tail.
        let mut tail = None;
        if let Some(entry) = entries.last() {
            let mut segment = Journal::new();
            segment.record(entry.try_clone().map_err(execution_error)?).map_err(execution_error)?;
            tail = Some(segment.to_bytes().map_err(execution_error)?);
        }
        let last_hash = hashes.last().copied().or(self.last_hash);
        let response = self.checked(WorkerResponse::ReplayComplete { from_entry: replay.from_entry, state_hashes: hashes })?;
        if let Some(program) = candidate {
            let raster = NativeRaster::new(self.config.renderer)?;
            self.program = Some(program);
            self.preview_clean = true;
            self.raster = raster;
        }
        self.position = replay.through_entry;
        self.last_checkpoint = last_checkpoint;
        self.last_hash = last_hash;
        if let Some(tail) = tail { self.tail = tail; }
        Ok(response)
    }

    fn restore(&mut self, checkpoint: Checkpoint) -> Result<WorkerResponse, ServiceError> {
        self.require_scene(&checkpoint.scene)?;
        self.require_replay(WorkerErrorCode::CheckpointRejected)?;
        let refuse = |message| ServiceError::new(WorkerErrorCode::CheckpointRejected, message);
        if checkpoint.state.len() > self.config.limits.max_checkpoint_bytes
            || protocol_digest(&checkpoint.state) != checkpoint.state_hash
        {
            return Err(refuse("native checkpoint byte budget or digest is invalid"));
        }
        let next = checkpoint.after_entry.checked_add(1).ok_or_else(|| refuse("native checkpoint journal position exhausted"))?;
        let decoded = SceneState::from_bytes(&checkpoint.state, &Stage::new())
            .map_err(|error| refuse_owned(WorkerErrorCode::CheckpointRejected, error))?;
        let frame = self.target(decoded.frames_elapsed).map_err(|error| refuse_owned(WorkerErrorCode::CheckpointRejected, error))?;
        if decoded.fps != self.config.fps {
            return Err(refuse("native checkpoint uses a different frame clock"));
        }
        // The decoded snapshot and updater manifest are NEVER installed.
        // Real execution reconstructs both the visible state and closure state.
        drop(decoded);
        let mut program = self.fresh_at(frame)?;
        let state = program.state_bytes()?;
        if state != checkpoint.state {
            return Err(refuse("native checkpoint does not match fresh callback execution"));
        }
        let response = self.checked(WorkerResponse::Ack { state_hash: Some(checkpoint.state_hash), journal_len: next })?;
        let raster = NativeRaster::new(self.config.renderer)?;
        self.program = Some(program);
        self.preview_clean = true;
        self.raster = raster;
        self.position = next;
        self.last_hash = Some(checkpoint.state_hash);
        self.last_checkpoint = Some(frame);
        self.tail.clear();
        Ok(response)
    }
}

fn refuse_owned(code: WorkerErrorCode, error: impl std::fmt::Display) -> ServiceError {
    ServiceError::new(code, error.to_string())
}

impl WorkerService for NativeSceneWorker {
    fn build_id(&self) -> ProtocolDigest { self.config.build_id }

    fn begin_session(&mut self, _supervisor_build: ProtocolDigest, max_frame_bytes: usize) -> Result<(), ServiceError> {
        if max_frame_bytes == 0 { return Err(invalid("zero native frame budget")); }
        self.config.limits.max_frame_bytes = self.config.limits.max_frame_bytes.min(max_frame_bytes);
        Ok(())
    }

    fn handle(&mut self, request: SupervisorRequest) -> Result<WorkerResponse, ServiceError> {
        match request {
            SupervisorRequest::EnumerateScenes => self.checked(WorkerResponse::Scenes(vec![self.config.scene.clone()])),
            SupervisorRequest::Play { scene, command } => { self.require_scene(&scene)?; self.record_seek(command) }
            SupervisorRequest::Seek { scene, frame } | SupervisorRequest::Scrub { scene, frame } => {
                self.require_scene(&scene)?; self.seek(frame)
            }
            SupervisorRequest::Event { scene, event } => { self.require_scene(&scene)?; self.event(event) }
            SupervisorRequest::Inspect { scene } => {
                self.require_scene(&scene)?;
                let program = self.program.as_ref().ok_or_else(|| invalid("native preview requires a successful seek"))?;
                let bytes = self.raster.inspect(program, self.config.frame_count, self.config.limits.max_studio_data_bytes)?;
                self.checked(WorkerResponse::StudioData {
                    scene, kind: StudioDataKind::Inspection, digest: protocol_digest(&bytes), bytes,
                })
            }
            SupervisorRequest::Overlay { scene, layers } => {
                self.require_scene(&scene)?;
                let program = self.program.as_ref().ok_or_else(|| invalid("native preview requires a successful seek"))?;
                let bytes = self.raster.overlay(program, layers, self.config.limits.max_studio_data_bytes)?;
                self.checked(WorkerResponse::StudioData {
                    scene, kind: StudioDataKind::Overlay, digest: protocol_digest(&bytes), bytes,
                })
            }
            SupervisorRequest::RestoreCheckpoint(checkpoint) => self.restore(checkpoint),
            SupervisorRequest::ReplayJournal(replay) => self.replay(replay),
            SupervisorRequest::Hello { .. } | SupervisorRequest::Shutdown => Err(invalid("the protocol driver owns handshake and shutdown")),
        }
    }

    fn active_scene(&self) -> Option<&str> { Some(&self.config.scene) }
    fn journal_tail(&self) -> &[u8] { &self.tail }
    fn last_state_hash(&self) -> Option<ProtocolDigest> { self.last_hash }
}
