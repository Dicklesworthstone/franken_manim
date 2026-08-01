//! fm-39s supervisor acceptance: warm-cache checkpoint fidelity,
//! trailing-edit PG-4 recovery, audited replay fallback, structured panic
//! conversion, and a real subprocess crash that leaves the parent supervisor
//! alive and automatically restores a replacement worker.

use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use fmn_cache::{Namespace, NamespacePolicy, Store, StoreConfig};
use fmn_hash::{Digest, sha256};
use fmn_platform::clock::{Clock, FakeClock};
use fmn_platform::fs::{FileSystem, VirtualFs};
use fmn_scene::{AssetRead, CommandKind, CommandRecord, EffectClass, Entry, Journal, Scene};
use fmn_studio::{
    BuildError, ChannelError, ChannelFailureKind, Checkpoint, CheckpointSource, CrashReport,
    FramingError, JournalReplay, LaunchError, ProtocolLimits, ProtocolVersion, RebuildDriver,
    RequestEnvelope, ResponseEnvelope, ServiceError, StdWorkerLauncher, Supervisor,
    SupervisorConfig, SupervisorReply, SupervisorRequest, TransportCapabilities, WorkerArtifact,
    WorkerChannel, WorkerErrorCode, WorkerLauncher, WorkerResponse, WorkerServeOutcome,
    WorkerService, read_response, serve_worker, write_request,
};

fn lock_poisoned<T>(error: PoisonError<T>) -> T {
    error.into_inner()
}

fn command(index: usize) -> CommandRecord {
    let label = format!("play command {index}");
    CommandRecord {
        kind: CommandKind::Play,
        identity: sha256(label.as_bytes()),
        label,
    }
}

fn scene_state_bytes() -> Vec<u8> {
    let mut scene = Scene::default();
    scene.state_bytes().expect("default SceneState encodes")
}

fn make_journal(count: usize, checkpoint_index: Option<usize>) -> Journal {
    let checkpoint = scene_state_bytes();
    let mut journal = Journal::new();
    for index in 0..count {
        let state_hash = if checkpoint_index == Some(index) {
            sha256(&checkpoint)
        } else {
            sha256(format!("state {index}").as_bytes())
        };
        journal.record(Entry {
            command: command(index),
            effect: EffectClass::Pure,
            reads: Vec::new(),
            subprocesses: Vec::new(),
            checkpoint: (checkpoint_index == Some(index)).then(|| checkpoint.clone()),
            state_hash,
        });
    }
    journal
}

fn commands(journal: &Journal) -> Vec<CommandRecord> {
    journal
        .entries()
        .iter()
        .map(|entry| entry.command.clone())
        .collect()
}

fn cache(clock: Arc<FakeClock>) -> Namespace {
    let fs: Arc<dyn FileSystem> = Arc::new(VirtualFs::new());
    let clock_cap: Arc<dyn Clock> = clock;
    Store::open(fs, clock_cap, "/studio-cache", StoreConfig::default())
        .expect("store opens")
        .namespace(
            "studio-replay",
            1,
            NamespacePolicy {
                ceiling_bytes: None,
            },
        )
        .expect("namespace opens")
}

#[derive(Default)]
struct FakeState {
    launches: usize,
    terminated: usize,
    crash_first_play: bool,
    crash_every_play: bool,
    channel_error_first_play: bool,
    diverge_next_replay: bool,
    skew_handshake: bool,
    wrong_build_handshake: bool,
    journal_segment: Option<(u64, Vec<u8>)>,
    checkpoint_response: Option<Checkpoint>,
    restored: Vec<Vec<u8>>,
    replayed: Vec<(u64, u64)>,
}

struct FakeLauncher {
    state: Arc<Mutex<FakeState>>,
    clock: Arc<FakeClock>,
    exchange_cost: Duration,
}

impl WorkerLauncher for FakeLauncher {
    fn launch(
        &mut self,
        artifact: &WorkerArtifact,
        _limits: ProtocolLimits,
    ) -> Result<Box<dyn WorkerChannel>, LaunchError> {
        let mut state = self.state.lock().unwrap_or_else(lock_poisoned);
        state.launches += 1;
        let launch = state.launches;
        let crash_every_play = state.crash_every_play;
        let crash_on_play = crash_every_play || (state.crash_first_play && launch == 1);
        let channel_error_on_play = state.channel_error_first_play && launch == 1;
        drop(state);
        Ok(Box::new(FakeChannel {
            state: Arc::clone(&self.state),
            clock: Arc::clone(&self.clock),
            exchange_cost: self.exchange_cost,
            build_id: artifact.build_id,
            crash_on_play,
            crash_message: if crash_every_play {
                format!("scripted scene panic {launch}")
            } else {
                "scripted scene panic".to_owned()
            },
            channel_error_on_play,
            terminated: false,
        }))
    }
}

struct FakeChannel {
    state: Arc<Mutex<FakeState>>,
    clock: Arc<FakeClock>,
    exchange_cost: Duration,
    build_id: Digest,
    crash_on_play: bool,
    crash_message: String,
    channel_error_on_play: bool,
    terminated: bool,
}

impl WorkerChannel for FakeChannel {
    fn exchange(
        &mut self,
        request: &RequestEnvelope,
        _timeout: Duration,
    ) -> Result<ResponseEnvelope, ChannelError> {
        self.clock.advance(self.exchange_cost);
        if self.channel_error_on_play && matches!(&request.request, SupervisorRequest::Play { .. })
        {
            self.channel_error_on_play = false;
            return Err(ChannelError {
                kind: ChannelFailureKind::Closed,
                detail: "short diagnostic".to_owned(),
                stderr_tail: vec![0xff; 4096],
            });
        }
        let response = match &request.request {
            SupervisorRequest::Hello { .. } => {
                let skew = self
                    .state
                    .lock()
                    .unwrap_or_else(lock_poisoned)
                    .skew_handshake;
                let worker_build = if self
                    .state
                    .lock()
                    .unwrap_or_else(lock_poisoned)
                    .wrong_build_handshake
                {
                    sha256(b"wrong worker build")
                } else {
                    self.build_id
                };
                WorkerResponse::Hello {
                    version: if skew {
                        ProtocolVersion { major: 2, minor: 0 }
                    } else {
                        fmn_studio::CURRENT_VERSION
                    },
                    worker_build,
                    transports: TransportCapabilities {
                        pipe: true,
                        shared_memory: false,
                    },
                }
            }
            SupervisorRequest::EnumerateScenes => {
                let mut state = self.state.lock().unwrap_or_else(lock_poisoned);
                if let Some((start_entry, journal)) = state.journal_segment.take() {
                    WorkerResponse::JournalSegment {
                        scene: "Demo".to_owned(),
                        start_entry,
                        journal,
                    }
                } else if let Some(checkpoint) = state.checkpoint_response.take() {
                    WorkerResponse::Checkpoint(checkpoint)
                } else {
                    WorkerResponse::Scenes(vec!["Demo".to_owned()])
                }
            }
            SupervisorRequest::Play { .. } if self.crash_on_play => {
                self.crash_on_play = false;
                WorkerResponse::Crash(CrashReport {
                    scene: Some("Demo".to_owned()),
                    message: self.crash_message.clone(),
                    journal_tail: b"scripted journal tail".to_vec(),
                    state_hash: None,
                })
            }
            SupervisorRequest::Play { .. }
            | SupervisorRequest::Seek { .. }
            | SupervisorRequest::Scrub { .. }
            | SupervisorRequest::Event { .. }
            | SupervisorRequest::Inspect { .. }
            | SupervisorRequest::Overlay { .. } => WorkerResponse::Ack {
                state_hash: None,
                journal_len: 0,
            },
            SupervisorRequest::RestoreCheckpoint(checkpoint) => {
                self.state
                    .lock()
                    .unwrap_or_else(lock_poisoned)
                    .restored
                    .push(checkpoint.state.clone());
                WorkerResponse::Ack {
                    state_hash: Some(checkpoint.state_hash),
                    journal_len: checkpoint.after_entry + 1,
                }
            }
            SupervisorRequest::ReplayJournal(replay) => {
                let journal = Journal::from_bytes(&replay.journal).expect("valid replay journal");
                let start = usize::try_from(replay.from_entry).expect("test range fits");
                let end = usize::try_from(replay.through_entry).expect("test range fits");
                let mut hashes: Vec<Digest> = journal.entries()[start..end]
                    .iter()
                    .map(|entry| entry.state_hash)
                    .collect();
                let mut state = self.state.lock().unwrap_or_else(lock_poisoned);
                state
                    .replayed
                    .push((replay.from_entry, replay.through_entry));
                if state.diverge_next_replay && !hashes.is_empty() {
                    state.diverge_next_replay = false;
                    hashes[0] = sha256(b"deliberate divergence");
                }
                WorkerResponse::ReplayComplete {
                    from_entry: replay.from_entry,
                    state_hashes: hashes,
                }
            }
            SupervisorRequest::Shutdown => WorkerResponse::Bye,
        };
        Ok(ResponseEnvelope {
            request_id: request.request_id,
            response,
        })
    }

    fn terminate(&mut self) {
        if !self.terminated {
            self.terminated = true;
            self.state.lock().unwrap_or_else(lock_poisoned).terminated += 1;
        }
    }
}

struct ScriptedBuilder {
    clock: Arc<FakeClock>,
    build_cost: Duration,
    build: usize,
    executable: PathBuf,
}

impl ScriptedBuilder {
    fn fake(clock: Arc<FakeClock>, build_cost: Duration) -> Self {
        Self {
            clock,
            build_cost,
            build: 0,
            executable: PathBuf::from("/worker/fmn"),
        }
    }
}

impl RebuildDriver for ScriptedBuilder {
    fn rebuild(&mut self) -> Result<WorkerArtifact, BuildError> {
        self.clock.advance(self.build_cost);
        self.build += 1;
        Ok(WorkerArtifact {
            executable: self.executable.clone(),
            argv: vec!["--studio-worker".to_owned()],
            env: Vec::new(),
            cwd: Some(PathBuf::from("/workspace")),
            build_id: sha256(format!("worker build {}", self.build).as_bytes()),
        })
    }
}

fn fake_supervisor(
    state: Arc<Mutex<FakeState>>,
    clock: Arc<FakeClock>,
    exchange_cost: Duration,
) -> Supervisor {
    let config = SupervisorConfig {
        request_timeout: Duration::from_secs(1),
        edit_to_frame_budget: Duration::from_secs(1),
        ..SupervisorConfig::default()
    };
    Supervisor::new(
        Box::new(FakeLauncher {
            state,
            clock: Arc::clone(&clock),
            exchange_cost,
        }),
        clock.clone(),
        cache(clock),
        config,
    )
}

#[test]
fn worker_crash_auto_restarts_restores_warm_checkpoint_and_parent_survives() {
    let state = Arc::new(Mutex::new(FakeState {
        crash_first_play: true,
        ..FakeState::default()
    }));
    let clock = Arc::new(FakeClock::new());
    let mut supervisor = fake_supervisor(
        Arc::clone(&state),
        Arc::clone(&clock),
        Duration::from_millis(10),
    );
    let journal = make_journal(2, Some(0));
    let checkpoint = journal.entries()[0]
        .checkpoint
        .as_ref()
        .expect("checkpoint")
        .clone();
    supervisor
        .install_session("Demo", journal.clone())
        .expect("session");
    let mut builder = ScriptedBuilder::fake(Arc::clone(&clock), Duration::from_millis(20));
    supervisor.build_and_start(&mut builder).expect("start");

    let reply = supervisor
        .request(
            SupervisorRequest::Play {
                scene: "Demo".to_owned(),
                command: command(1),
            },
            &|_: &AssetRead| true,
        )
        .expect("supervisor recovers");
    let SupervisorReply::Recovered { crash, recovery } = reply else {
        // ubs:ignore - deliberate assertion failure in a recovery test.
        std::panic::panic_any("expected automatic recovery");
    };
    assert_eq!(crash.message, "scripted scene panic");
    assert_eq!(recovery.restored_checkpoint, Some(0));
    assert_eq!(recovery.checkpoint_source, Some(CheckpointSource::Cache));
    assert_eq!(recovery.replayed_entries, 1);
    assert!(!recovery.cold_fallback);
    assert_eq!(supervisor.generation(), 2);
    assert_eq!(supervisor.crashes(), &[crash]);

    let state = state.lock().unwrap_or_else(lock_poisoned);
    assert_eq!(state.launches, 2);
    assert_eq!(state.restored.last(), Some(&checkpoint));
    drop(state);

    assert_eq!(
        supervisor
            .request(SupervisorRequest::EnumerateScenes, &|_| true)
            .expect("replacement answers"),
        SupervisorReply::Worker(WorkerResponse::Scenes(vec!["Demo".to_owned()]))
    );
}

fn repeating_crash_supervisor(max_crash_reports: usize) -> Supervisor {
    let state = Arc::new(Mutex::new(FakeState {
        crash_every_play: true,
        ..FakeState::default()
    }));
    let clock = Arc::new(FakeClock::new());
    let mut supervisor = Supervisor::new(
        Box::new(FakeLauncher {
            state,
            clock: Arc::clone(&clock),
            exchange_cost: Duration::from_millis(1),
        }),
        clock.clone(),
        cache(clock.clone()),
        SupervisorConfig {
            max_crash_reports,
            ..SupervisorConfig::default()
        },
    );
    supervisor
        .install_session("Demo", Journal::new())
        .expect("session");
    let mut builder = ScriptedBuilder::fake(clock, Duration::ZERO);
    supervisor.build_and_start(&mut builder).expect("start");
    supervisor
}

#[test]
fn repeated_crash_history_evicts_oldest_and_zero_disables_retention() {
    let mut bounded = repeating_crash_supervisor(2);
    for launch in 1..=3 {
        let reply = bounded
            .request(
                SupervisorRequest::Play {
                    scene: "Demo".to_owned(),
                    command: command(launch),
                },
                &|_| true,
            )
            .expect("supervisor recovers");
        let SupervisorReply::Recovered { crash, .. } = reply else {
            std::panic::panic_any("expected automatic recovery");
        };
        assert_eq!(crash.message, format!("scripted scene panic {launch}"));
    }
    assert_eq!(
        bounded
            .crashes()
            .iter()
            .map(|report| report.message.as_str())
            .collect::<Vec<_>>(),
        ["scripted scene panic 2", "scripted scene panic 3"]
    );

    let mut disabled = repeating_crash_supervisor(0);
    let reply = disabled
        .request(
            SupervisorRequest::Play {
                scene: "Demo".to_owned(),
                command: command(1),
            },
            &|_| true,
        )
        .expect("supervisor recovers without retaining history");
    assert!(matches!(reply, SupervisorReply::Recovered { .. }));
    assert!(disabled.crashes().is_empty());
}

#[test]
fn locally_synthesized_crash_message_is_bounded_before_lossy_stderr_conversion() {
    let state = Arc::new(Mutex::new(FakeState {
        channel_error_first_play: true,
        ..FakeState::default()
    }));
    let clock = Arc::new(FakeClock::new());
    let mut supervisor = Supervisor::new(
        Box::new(FakeLauncher {
            state,
            clock: Arc::clone(&clock),
            exchange_cost: Duration::from_millis(1),
        }),
        clock.clone(),
        cache(clock.clone()),
        SupervisorConfig {
            protocol_limits: ProtocolLimits {
                max_crash_message_bytes: 64,
                ..ProtocolLimits::default()
            },
            ..SupervisorConfig::default()
        },
    );
    supervisor
        .install_session("Demo", Journal::new())
        .expect("session");
    let mut builder = ScriptedBuilder::fake(clock, Duration::ZERO);
    supervisor.build_and_start(&mut builder).expect("start");
    let reply = supervisor
        .request(
            SupervisorRequest::Play {
                scene: "Demo".to_owned(),
                command: command(1),
            },
            &|_| true,
        )
        .expect("supervisor recovers");
    let SupervisorReply::Recovered { crash, .. } = reply else {
        std::panic::panic_any("expected automatic recovery");
    };
    assert!(crash.message.len() <= 64);
    assert!(
        crash
            .message
            .starts_with("worker channel Closed: short diagnostic")
    );
    assert!(crash.message.contains("stderr tail"));
    assert!(crash.message.contains('\u{fffd}'));
    assert_eq!(supervisor.crashes(), &[crash]);
}

#[test]
fn trailing_edit_restores_entry_28_and_meets_pg4_scripted_budget() {
    let state = Arc::new(Mutex::new(FakeState::default()));
    let clock = Arc::new(FakeClock::new());
    let mut supervisor = fake_supervisor(
        Arc::clone(&state),
        Arc::clone(&clock),
        Duration::from_millis(50),
    );
    let journal = make_journal(30, Some(28));
    supervisor
        .install_session("Demo", journal.clone())
        .expect("session");
    let mut builder = ScriptedBuilder::fake(Arc::clone(&clock), Duration::from_millis(400));
    supervisor
        .build_and_start(&mut builder)
        .expect("initial start");

    let mut edited = commands(&journal);
    edited[29] = CommandRecord {
        kind: CommandKind::Play,
        identity: sha256(b"edited trailing command"),
        label: "play edited trailing command".to_owned(),
    };
    let report = supervisor
        .rebuild_and_restart(&mut builder, &edited, &|_| true)
        .expect("autoreload");
    assert_eq!(report.plan.reuse, 29);
    assert_eq!(report.restored_checkpoint, Some(28));
    assert_eq!(report.checkpoint_source, Some(CheckpointSource::Cache));
    assert_eq!(report.replayed_entries, 0);
    assert_eq!(report.elapsed, Duration::from_millis(550));
    assert!(report.within_edit_to_frame_budget);
    assert_eq!(report.generation, 2);
}

#[test]
fn replay_hash_divergence_discards_reuse_and_cold_reexecutes() {
    let state = Arc::new(Mutex::new(FakeState {
        diverge_next_replay: true,
        ..FakeState::default()
    }));
    let clock = Arc::new(FakeClock::new());
    let mut supervisor = fake_supervisor(
        Arc::clone(&state),
        Arc::clone(&clock),
        Duration::from_millis(1),
    );
    let journal = make_journal(3, Some(0));
    let incoming = commands(&journal);
    supervisor
        .install_session("Demo", journal)
        .expect("session");
    let mut builder = ScriptedBuilder::fake(Arc::clone(&clock), Duration::from_millis(1));
    supervisor.build_and_start(&mut builder).expect("start");
    let report = supervisor
        .rebuild_and_restart(&mut builder, &incoming, &|_| true)
        .expect("fallback succeeds");
    assert_eq!(report.diverged_at, Some(1));
    assert!(report.cold_fallback);
    assert_eq!(report.restored_checkpoint, None);
    assert_eq!(report.replayed_entries, 3);
    assert_eq!(report.generation, 3);
    assert_eq!(
        state.lock().unwrap_or_else(lock_poisoned).replayed,
        vec![(1, 3), (0, 3)]
    );
}

#[test]
fn handshake_version_skew_fails_closed_before_scene_commands() {
    let state = Arc::new(Mutex::new(FakeState {
        skew_handshake: true,
        ..FakeState::default()
    }));
    let clock = Arc::new(FakeClock::new());
    let mut supervisor = fake_supervisor(Arc::clone(&state), Arc::clone(&clock), Duration::ZERO);
    let mut builder = ScriptedBuilder::fake(clock, Duration::ZERO);
    assert!(matches!(
        supervisor.build_and_start(&mut builder),
        Err(fmn_studio::SupervisorError::Protocol(
            fmn_studio::ProtocolError::VersionSkew { .. }
        ))
    ));
    assert_eq!(supervisor.generation(), 0);
    assert_eq!(state.lock().unwrap_or_else(lock_poisoned).launches, 1);
}

#[test]
fn bad_replacement_keeps_the_previous_healthy_worker_live() {
    let state = Arc::new(Mutex::new(FakeState::default()));
    let clock = Arc::new(FakeClock::new());
    let mut supervisor = fake_supervisor(Arc::clone(&state), Arc::clone(&clock), Duration::ZERO);
    let journal = make_journal(1, Some(0));
    let incoming = commands(&journal);
    supervisor
        .install_session("Demo", journal)
        .expect("session");
    let mut builder = ScriptedBuilder::fake(clock, Duration::ZERO);
    supervisor
        .build_and_start(&mut builder)
        .expect("initial worker");

    state
        .lock()
        .unwrap_or_else(lock_poisoned)
        .wrong_build_handshake = true;
    assert!(matches!(
        supervisor.rebuild_and_restart(&mut builder, &incoming, &|_| true),
        Err(fmn_studio::SupervisorError::BuildIdentityMismatch { .. })
    ));
    assert_eq!(supervisor.generation(), 1);
    assert_eq!(
        supervisor
            .request(SupervisorRequest::EnumerateScenes, &|_| true)
            .expect("old worker remains live"),
        SupervisorReply::Worker(WorkerResponse::Scenes(vec!["Demo".to_owned()]))
    );
    let state = state.lock().unwrap_or_else(lock_poisoned);
    assert_eq!(state.launches, 2);
    assert_eq!(state.terminated, 1, "only the bad candidate was retired");
}

#[test]
fn invalid_session_install_is_atomic_and_preserves_prior_recovery_state() {
    let state = Arc::new(Mutex::new(FakeState::default()));
    let clock = Arc::new(FakeClock::new());
    let mut supervisor = fake_supervisor(Arc::clone(&state), Arc::clone(&clock), Duration::ZERO);
    let valid = make_journal(2, Some(0));
    let incoming = commands(&valid);
    supervisor
        .install_session("Demo", valid)
        .expect("valid session");
    let old_journal_digest = supervisor.journal_cache_digest();

    let mut malformed = Journal::new();
    malformed.record(Entry {
        command: command(99),
        effect: EffectClass::Pure,
        reads: Vec::new(),
        subprocesses: Vec::new(),
        checkpoint: Some(b"malformed checkpoint".to_vec()),
        state_hash: sha256(b"different bytes"),
    });
    assert!(matches!(
        supervisor.install_session("Broken", malformed),
        Err(fmn_studio::SupervisorError::InvalidSession(
            "checkpoint bytes do not match their state hash"
        ))
    ));
    assert_eq!(supervisor.journal_cache_digest(), old_journal_digest);

    let mut builder = ScriptedBuilder::fake(clock, Duration::ZERO);
    supervisor.build_and_start(&mut builder).expect("start");
    let report = supervisor
        .rebuild_and_restart(&mut builder, &incoming, &|_| true)
        .expect("old session still recovers");
    assert_eq!(report.plan.reuse, 2);
    assert_eq!(report.restored_checkpoint, Some(0));
}

#[test]
fn worker_checkpoint_must_match_the_installed_journal_authority() {
    let journal = make_journal(2, Some(0));
    let incoming = commands(&journal);
    let original_checkpoint = journal.entries()[0]
        .checkpoint
        .clone()
        .expect("journal checkpoint");
    let divergent_state = b"internally valid but not the journal state".to_vec();
    let state = Arc::new(Mutex::new(FakeState {
        checkpoint_response: Some(Checkpoint {
            scene: "Demo".to_owned(),
            after_entry: 0,
            state_hash: sha256(&divergent_state),
            state: divergent_state,
        }),
        ..FakeState::default()
    }));
    let clock = Arc::new(FakeClock::new());
    let mut supervisor = fake_supervisor(Arc::clone(&state), Arc::clone(&clock), Duration::ZERO);
    supervisor
        .install_session("Demo", journal)
        .expect("session");
    let mut builder = ScriptedBuilder::fake(clock, Duration::ZERO);
    supervisor.build_and_start(&mut builder).expect("start");

    assert!(matches!(
        supervisor.request(SupervisorRequest::EnumerateScenes, &|_| true),
        Err(fmn_studio::SupervisorError::InvalidSession(
            "worker checkpoint does not match the journal entry state hash"
        ))
    ));

    let report = supervisor
        .rebuild_and_restart(&mut builder, &incoming, &|_| true)
        .expect("known-good checkpoint remains authoritative");
    assert_eq!(report.restored_checkpoint, Some(0));
    assert_eq!(
        state.lock().unwrap_or_else(lock_poisoned).restored.last(),
        Some(&original_checkpoint)
    );
}

#[cfg(unix)]
#[test]
fn stdio_worker_deadline_cancels_a_blocked_checkpoint_write() {
    let sleep = Path::new("/bin/sleep");
    if !sleep.is_file() {
        return;
    }
    let mut launcher = StdWorkerLauncher::default();
    let limits = ProtocolLimits {
        max_checkpoint_bytes: 2 * 1024 * 1024,
        max_message_bytes: 3 * 1024 * 1024,
        ..ProtocolLimits::default()
    };
    let artifact = WorkerArtifact {
        executable: sleep.to_path_buf(),
        argv: vec!["10".to_owned()],
        env: Vec::new(),
        cwd: None,
        build_id: sha256(b"unresponsive test worker"),
    };
    let mut channel = launcher
        .launch(&artifact, limits)
        .expect("sleep fixture launches");
    let state = vec![0x5a; 1024 * 1024];
    let request = RequestEnvelope {
        request_id: 1,
        request: SupervisorRequest::RestoreCheckpoint(Checkpoint {
            scene: "Demo".to_owned(),
            after_entry: 0,
            state_hash: sha256(&state),
            state,
        }),
    };

    let started = Instant::now();
    let error = channel
        .exchange(&request, Duration::from_millis(100))
        .expect_err("unresponsive worker must hit its deadline");
    assert_eq!(error.kind, ChannelFailureKind::Timeout);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "blocked pipe write outlived cancellation budget"
    );
}

#[test]
fn journal_segment_replaces_only_its_declared_tail() {
    let replacement_command = command(99);
    let mut replacement = Journal::new();
    replacement.record(Entry {
        command: replacement_command.clone(),
        effect: EffectClass::Pure,
        reads: Vec::new(),
        subprocesses: Vec::new(),
        checkpoint: None,
        state_hash: sha256(b"replacement tail state"),
    });
    let state = Arc::new(Mutex::new(FakeState {
        journal_segment: Some((1, replacement.to_bytes().expect("segment encodes"))),
        ..FakeState::default()
    }));
    let clock = Arc::new(FakeClock::new());
    let mut supervisor = fake_supervisor(Arc::clone(&state), Arc::clone(&clock), Duration::ZERO);
    let original = make_journal(2, Some(0));
    let first = original.entries()[0].command.clone();
    supervisor
        .install_session("Demo", original)
        .expect("session");
    let mut builder = ScriptedBuilder::fake(clock, Duration::ZERO);
    supervisor.build_and_start(&mut builder).expect("start");
    assert!(matches!(
        supervisor
            .request(SupervisorRequest::EnumerateScenes, &|_| true)
            .expect("segment accepted"),
        SupervisorReply::Worker(WorkerResponse::JournalSegment { start_entry: 1, .. })
    ));

    let report = supervisor
        .rebuild_and_restart(&mut builder, &[first, replacement_command], &|_| true)
        .expect("merged journal replays");
    assert_eq!(report.plan.reuse, 2);
    assert_eq!(report.restored_checkpoint, Some(0));
    assert_eq!(report.replayed_entries, 1);
}

struct PanicService {
    build_id: Digest,
    negotiated_frame_budget: Option<usize>,
}

impl WorkerService for PanicService {
    fn build_id(&self) -> Digest {
        self.build_id
    }

    fn begin_session(
        &mut self,
        _supervisor_build: Digest,
        max_frame_bytes: usize,
    ) -> Result<(), ServiceError> {
        self.negotiated_frame_budget = Some(max_frame_bytes);
        Ok(())
    }

    fn handle(&mut self, _request: SupervisorRequest) -> Result<WorkerResponse, ServiceError> {
        // ubs:ignore - deliberate fixture panic exercises worker isolation.
        std::panic::panic_any("fixture scene panic")
    }

    fn active_scene(&self) -> Option<String> {
        Some("Demo".to_owned())
    }

    fn journal_tail(&self) -> Vec<u8> {
        b"canonical journal tail".to_vec()
    }

    fn last_state_hash(&self) -> Option<Digest> {
        Some(sha256(b"last state"))
    }
}

#[test]
fn worker_loop_converts_scene_panic_to_structured_correlated_report() {
    let limits = ProtocolLimits::default();
    let build_id = sha256(b"panic worker");
    let mut input = Vec::new();
    write_request(
        &mut input,
        &RequestEnvelope {
            request_id: 1,
            request: SupervisorRequest::Hello {
                version: fmn_studio::CURRENT_VERSION,
                supervisor_build: sha256(b"supervisor"),
                max_frame_bytes: 1024,
            },
        },
        limits,
    )
    .expect("hello");
    write_request(
        &mut input,
        &RequestEnvelope {
            request_id: 2,
            request: SupervisorRequest::EnumerateScenes,
        },
        limits,
    )
    .expect("command");
    let mut output = Vec::new();
    let mut service = PanicService {
        build_id,
        negotiated_frame_budget: None,
    };
    let outcome = serve_worker(
        &mut service,
        &mut std::io::Cursor::new(input),
        &mut output,
        limits,
    )
    .expect("worker loop reports panic");
    let WorkerServeOutcome::Crashed(report) = outcome else {
        // ubs:ignore - deliberate assertion failure in a crash-report test.
        std::panic::panic_any("expected crash outcome");
    };
    assert_eq!(report.scene.as_deref(), Some("Demo"));
    assert_eq!(report.message, "fixture scene panic");
    assert_eq!(report.journal_tail, b"canonical journal tail");
    assert_eq!(service.negotiated_frame_budget, Some(1024));

    let mut output = std::io::Cursor::new(output);
    assert!(matches!(
        read_response(&mut output, limits)
            .expect("hello response")
            .response,
        WorkerResponse::Hello { worker_build, .. } if worker_build == build_id // ubs:ignore - public build identity.
    ));
    let crash = read_response(&mut output, limits).expect("crash response");
    assert_eq!(crash.request_id, 2);
    assert_eq!(crash.response, WorkerResponse::Crash(report));
}

#[test]
fn worker_loop_rejects_reserved_and_nonmonotonic_request_ids() {
    let limits = ProtocolLimits::default();
    let build_id = sha256(b"request-id worker");
    let hello = |request_id| RequestEnvelope {
        request_id,
        request: SupervisorRequest::Hello {
            version: fmn_studio::CURRENT_VERSION,
            supervisor_build: sha256(b"supervisor"),
            max_frame_bytes: 1024,
        },
    };

    let mut zero_input = Vec::new();
    write_request(&mut zero_input, &hello(0), limits).expect("zero-id hello encodes");
    let mut zero_output = Vec::new();
    let mut service = PanicService {
        build_id,
        negotiated_frame_budget: None,
    };
    assert_eq!(
        serve_worker(
            &mut service,
            &mut std::io::Cursor::new(zero_input),
            &mut zero_output,
            limits,
        )
        .expect("zero id is a typed refusal"),
        WorkerServeOutcome::HandshakeRejected
    );
    assert!(matches!(
        read_response(&mut std::io::Cursor::new(zero_output), limits)
            .expect("zero-id refusal")
            .response,
        WorkerResponse::Error {
            code: WorkerErrorCode::InvalidRequest,
            ..
        }
    ));

    let mut repeated_input = Vec::new();
    write_request(&mut repeated_input, &hello(1), limits).expect("hello");
    write_request(
        &mut repeated_input,
        &RequestEnvelope {
            request_id: 1,
            request: SupervisorRequest::EnumerateScenes,
        },
        limits,
    )
    .expect("repeated request id encodes");
    let mut repeated_output = Vec::new();
    assert_eq!(
        serve_worker(
            &mut service,
            &mut std::io::Cursor::new(repeated_input),
            &mut repeated_output,
            limits,
        )
        .expect("nonmonotonic id is a typed refusal"),
        WorkerServeOutcome::HandshakeRejected
    );
    let mut repeated_output = std::io::Cursor::new(repeated_output);
    assert!(matches!(
        read_response(&mut repeated_output, limits)
            .expect("hello response")
            .response,
        WorkerResponse::Hello { .. }
    ));
    assert!(matches!(
        read_response(&mut repeated_output, limits)
            .expect("nonmonotonic refusal")
            .response,
        WorkerResponse::Error {
            code: WorkerErrorCode::InvalidRequest,
            ..
        }
    ));
}

// ---------------------------------------------------------------------------
// Real process-isolation acceptance.  The test harness's stdout is irrelevant:
// control messages use a private loopback socket solely because stable Rust
// does not expose arbitrary inherited file descriptors portably.  Production
// `StdWorkerLauncher` uses the bounded stdin/stdout pipe implementation.
// ---------------------------------------------------------------------------

struct TcpLauncher {
    launches: Arc<AtomicUsize>,
    crash_first: bool,
}

impl WorkerLauncher for TcpLauncher {
    fn launch(
        &mut self,
        artifact: &WorkerArtifact,
        _limits: ProtocolLimits,
    ) -> Result<Box<dyn WorkerChannel>, LaunchError> {
        let launch = self.launches.fetch_add(1, Ordering::SeqCst) + 1;
        let listener = TcpListener::bind("127.0.0.1:0").map_err(|error| LaunchError::Spawn {
            program: artifact.executable.clone(),
            error,
        })?;
        listener
            .set_nonblocking(true)
            .map_err(|error| LaunchError::Spawn {
                program: artifact.executable.clone(),
                error,
            })?;
        let address = listener.local_addr().map_err(|error| LaunchError::Spawn {
            program: artifact.executable.clone(),
            error,
        })?;
        let mut command = std::process::Command::new(&artifact.executable);
        command
            .arg("--exact")
            .arg("subprocess_worker_entry")
            .arg("--nocapture")
            .env("FMN_STUDIO_CHILD", "1")
            .env("FMN_STUDIO_ADDR", address.to_string())
            .env("FMN_STUDIO_BUILD_ID", artifact.build_id.to_hex())
            .env(
                "FMN_STUDIO_CRASH",
                if self.crash_first && launch == 1 {
                    "1"
                } else {
                    "0"
                },
            )
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let mut child = command.spawn().map_err(|error| LaunchError::Spawn {
            program: artifact.executable.clone(),
            error,
        })?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    // Darwin carries the listener's nonblocking mode onto the
                    // accepted socket. The channel below is intentionally
                    // blocking and enforces its own read/write deadlines.
                    if let Err(error) = stream.set_nonblocking(false) {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(LaunchError::Spawn {
                            program: artifact.executable.clone(),
                            error,
                        });
                    }
                    return Ok(Box::new(TcpChannel {
                        stream,
                        child: Some(child),
                    }));
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if let Ok(Some(_)) = child.try_wait() {
                        return Err(LaunchError::InvalidArtifact(
                            "test worker exited before connecting",
                        ));
                    }
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(LaunchError::InvalidArtifact(
                            "test worker did not connect before deadline",
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(LaunchError::Spawn {
                        program: artifact.executable.clone(),
                        error,
                    });
                }
            }
        }
    }
}

struct TcpChannel {
    stream: TcpStream,
    child: Option<Child>,
}

impl TcpChannel {
    fn map(error: FramingError) -> ChannelError {
        let kind = match &error {
            FramingError::Closed => ChannelFailureKind::Closed,
            FramingError::Io(_) => ChannelFailureKind::Io,
            FramingError::FrameTooLarge { .. } | FramingError::Protocol(_) => {
                ChannelFailureKind::Protocol
            }
        };
        ChannelError {
            kind,
            detail: error.to_string(),
            stderr_tail: Vec::new(),
        }
    }
}

impl WorkerChannel for TcpChannel {
    fn exchange(
        &mut self,
        request: &RequestEnvelope,
        timeout: Duration,
    ) -> Result<ResponseEnvelope, ChannelError> {
        self.stream
            .set_read_timeout(Some(timeout))
            .map_err(|error| ChannelError {
                kind: ChannelFailureKind::Io,
                detail: error.to_string(),
                stderr_tail: Vec::new(),
            })?;
        self.stream
            .set_write_timeout(Some(timeout))
            .map_err(|error| ChannelError {
                kind: ChannelFailureKind::Io,
                detail: error.to_string(),
                stderr_tail: Vec::new(),
            })?;
        write_request(&mut self.stream, request, ProtocolLimits::default()).map_err(Self::map)?;
        read_response(&mut self.stream, ProtocolLimits::default()).map_err(Self::map)
    }

    fn terminate(&mut self) {
        let _ = self.stream.shutdown(Shutdown::Both);
        if let Some(mut child) = self.child.take() {
            match child.try_wait() {
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
        }
    }
}

impl Drop for TcpChannel {
    fn drop(&mut self) {
        self.terminate();
    }
}

struct ChildService {
    build_id: Digest,
    crash: bool,
}

impl WorkerService for ChildService {
    fn build_id(&self) -> Digest {
        self.build_id
    }

    fn handle(&mut self, request: SupervisorRequest) -> Result<WorkerResponse, ServiceError> {
        match request {
            SupervisorRequest::EnumerateScenes => {
                Ok(WorkerResponse::Scenes(vec!["Demo".to_owned()]))
            }
            SupervisorRequest::Play { .. } if self.crash => {
                self.crash = false;
                // ubs:ignore - deliberate subprocess crash fixture.
                std::panic::panic_any("subprocess scene panic")
            }
            SupervisorRequest::Play { .. }
            | SupervisorRequest::Seek { .. }
            | SupervisorRequest::Scrub { .. }
            | SupervisorRequest::Event { .. }
            | SupervisorRequest::Inspect { .. }
            | SupervisorRequest::Overlay { .. } => Ok(WorkerResponse::Ack {
                state_hash: None,
                journal_len: 0,
            }),
            SupervisorRequest::RestoreCheckpoint(checkpoint) => Ok(WorkerResponse::Ack {
                state_hash: Some(checkpoint.state_hash),
                journal_len: checkpoint.after_entry + 1,
            }),
            SupervisorRequest::ReplayJournal(JournalReplay {
                from_entry,
                through_entry,
                journal,
                ..
            }) => {
                let journal = Journal::from_bytes(&journal).map_err(|error| {
                    ServiceError::new(WorkerErrorCode::ReplayFailed, error.to_string())
                })?;
                let start = usize::try_from(from_entry).map_err(|_| {
                    ServiceError::new(WorkerErrorCode::ReplayFailed, "range overflow")
                })?;
                let end = usize::try_from(through_entry).map_err(|_| {
                    ServiceError::new(WorkerErrorCode::ReplayFailed, "range overflow")
                })?;
                Ok(WorkerResponse::ReplayComplete {
                    from_entry,
                    state_hashes: journal.entries()[start..end]
                        .iter()
                        .map(|entry| entry.state_hash)
                        .collect(),
                })
            }
            SupervisorRequest::Hello { .. } | SupervisorRequest::Shutdown => {
                Err(ServiceError::new(
                    WorkerErrorCode::InvalidRequest,
                    "protocol driver request reached service",
                ))
            }
        }
    }

    fn active_scene(&self) -> Option<String> {
        Some("Demo".to_owned())
    }

    fn journal_tail(&self) -> Vec<u8> {
        b"subprocess journal tail".to_vec()
    }
}

#[test]
fn subprocess_worker_entry() {
    if std::env::var("FMN_STUDIO_CHILD").as_deref() != Ok("1") {
        return;
    }
    let address = std::env::var("FMN_STUDIO_ADDR").expect("child address");
    let build_id = Digest::from_hex(&std::env::var("FMN_STUDIO_BUILD_ID").expect("child build id"))
        .expect("valid build id");
    let crash = std::env::var("FMN_STUDIO_CRASH").as_deref() == Ok("1");
    let stream = TcpStream::connect(address).expect("connect supervisor");
    let mut reader = stream.try_clone().expect("clone stream");
    let mut writer = stream;
    let outcome = serve_worker(
        &mut ChildService { build_id, crash },
        &mut reader,
        &mut writer,
        ProtocolLimits::default(),
    )
    .expect("serve worker");
    if matches!(outcome, WorkerServeOutcome::Crashed(_)) {
        // ubs:ignore - child exits after its deliberate structured crash.
        std::panic::panic_any("worker process exits after its structured crash report");
    }
}

#[test]
fn real_worker_process_panic_is_isolated_and_auto_restarted() {
    if std::env::var("FMN_STUDIO_CHILD").as_deref() == Ok("1") {
        return;
    }
    let clock = Arc::new(FakeClock::new());
    let launches = Arc::new(AtomicUsize::new(0));
    let mut supervisor = Supervisor::new(
        Box::new(TcpLauncher {
            launches: Arc::clone(&launches),
            crash_first: true,
        }),
        clock.clone(),
        cache(clock.clone()),
        SupervisorConfig {
            request_timeout: Duration::from_secs(3),
            ..SupervisorConfig::default()
        },
    );
    let journal = make_journal(2, Some(0));
    supervisor
        .install_session("Demo", journal)
        .expect("session");
    let mut builder = ScriptedBuilder {
        clock,
        build_cost: Duration::ZERO,
        build: 0,
        executable: std::env::current_exe().expect("current test executable"),
    };
    supervisor
        .build_and_start(&mut builder)
        .expect("start child");
    let reply = supervisor
        .request(
            SupervisorRequest::Play {
                scene: "Demo".to_owned(),
                command: command(1),
            },
            &|_| true,
        )
        .expect("parent survives and recovers");
    let SupervisorReply::Recovered { crash, recovery } = reply else {
        // ubs:ignore - deliberate assertion failure in a process-recovery test.
        std::panic::panic_any("expected process crash recovery");
    };
    assert_eq!(crash.message, "subprocess scene panic");
    assert_eq!(crash.journal_tail, b"subprocess journal tail");
    assert_eq!(recovery.restored_checkpoint, Some(0));
    assert_eq!(supervisor.generation(), 2);
    assert_eq!(launches.load(Ordering::SeqCst), 2);
    assert_eq!(
        supervisor
            .request(SupervisorRequest::EnumerateScenes, &|_| true)
            .expect("replacement child answers"),
        SupervisorReply::Worker(WorkerResponse::Scenes(vec!["Demo".to_owned()]))
    );
}
