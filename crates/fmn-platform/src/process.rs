//! The process capability: **the one sanctioned subprocess mechanism** (D2).
//!
//! ffmpeg is the only program the engine will ever spawn, and every rule of
//! the D2 security protocol that belongs to the *mechanism* lives here:
//!
//! - **argv-only.** [`ProcessSpec`] is a program path plus an argument
//!   vector. There is no shell, no string splitting, no interpolation —
//!   a shell cannot be reached through this API at all.
//! - **Environment allowlist.** The child's environment is cleared and
//!   rebuilt from [`ProcessSpec::env`] alone; nothing ambient leaks in.
//! - **Timeout.** [`ProcessSpec::timeout`] bounds wall-clock runtime; on
//!   expiry the child is killed and the outcome says so.
//! - **Output-size limits.** stdout and stderr are each capped at
//!   [`ProcessSpec::max_output_bytes`]; exceeding a cap kills the child with
//!   [`ProcessTermination::OutputLimitExceeded`] — a runaway encoder cannot
//!   fill the disk or the heap.
//! - **Bounded streaming stdin.** [`ProcessRunner::start`] yields a supervised
//!   session with explicit per-chunk and cumulative limits. The OS pipe
//!   supplies backpressure, cooperative cancellation interrupts a blocked
//!   write, and every terminal path reaps the child.
//!
//! - **Process-tree cancellation.** On supported Unix targets, every child
//!   leads a fresh process group and every terminal path kills that complete
//!   group through the pinned nightly's safe standard-library API. Targets
//!   without an equivalent safe mechanism are refused before spawn rather than
//!   silently weakening D2.
//!   Higher layers (job-scoped temp dirs and their `fm-yw7h` hardening,
//!   atomic publication, provenance fingerprinting) belong to the W8
//!   boundary, not the mechanism.

use std::collections::BTreeMap;
use std::fmt;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

/// A complete, self-contained description of one subprocess invocation.
#[derive(Clone, Debug)]
pub struct ProcessSpec {
    /// The program to execute (a path, never a shell string).
    pub program: PathBuf,
    /// The argument vector, passed verbatim.
    pub argv: Vec<String>,
    /// The child's entire environment: cleared, then exactly these pairs.
    pub env: Vec<(String, String)>,
    /// Working directory, or inherit.
    pub cwd: Option<PathBuf>,
    /// Bytes written to the child's stdin by [`ProcessRunner::run`] (then
    /// closed); `None` for a null stdin. [`ProcessRunner::start`] requires
    /// this field to be `None` and accepts ordered chunks through
    /// [`RunningProcess::write_stdin`].
    pub stdin: Option<Vec<u8>>,
    /// Wall-clock bound; on expiry the child is killed.
    pub timeout: Duration,
    /// Per-stream cap on captured stdout/stderr bytes; on overflow the child
    /// is killed with [`ProcessTermination::OutputLimitExceeded`].
    pub max_output_bytes: u64,
}

/// Why process supervision stopped.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProcessTermination {
    /// The child exited; an OS exit code may be unavailable.
    Exited(Option<i32>),
    /// The wall-clock bound expired.
    TimedOut,
    /// stdout or stderr exceeded its capture cap.
    OutputLimitExceeded,
    /// Cooperative cancellation won before exit was observed.
    Cancelled,
}

/// What happened when a spawned process finished (or was stopped).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ProcessOutcome {
    /// The authoritative terminal reason.
    pub termination: ProcessTermination,
    /// Captured stdout (up to the cap).
    pub stdout: Vec<u8>,
    /// Captured stderr (up to the cap).
    pub stderr: Vec<u8>,
}

impl ProcessOutcome {
    /// Whether the process ran to completion with exit code 0.
    #[must_use]
    pub fn success(&self) -> bool {
        self.termination == ProcessTermination::Exited(Some(0))
    }
}

/// Cooperative cancellation shared by a process session and its owner.
///
/// The std runner polls this token while the child is alive, including while
/// another thread is blocked feeding stdin. Cancelling therefore closes the
/// process side of a full pipe and unblocks bounded sink backpressure.
#[derive(Clone, Debug, Default)]
pub struct ProcessCancellation {
    cancelled: Arc<AtomicBool>,
}

impl ProcessCancellation {
    /// A fresh, live cancellation token.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Request cancellation. Repeated requests are idempotent.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    /// Whether cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

/// Bounds enforced by the process mechanism for incremental stdin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessStdinLimits {
    /// Largest accepted call to [`RunningProcess::write_stdin`].
    pub max_chunk_bytes: u64,
    /// Largest cumulative stdin payload.
    pub max_total_bytes: u64,
}

impl ProcessStdinLimits {
    /// Construct explicit stdin bounds.
    #[must_use]
    pub const fn new(max_chunk_bytes: u64, max_total_bytes: u64) -> Self {
        Self {
            max_chunk_bytes,
            max_total_bytes,
        }
    }
}

/// A process-mechanism failure (distinct from a process that ran and
/// failed, which is a [`ProcessOutcome`] with a nonzero code).
#[derive(Debug)]
pub enum ProcessError {
    /// The program could not be spawned at all.
    Spawn {
        /// The program that failed to spawn.
        program: PathBuf,
        /// The underlying error.
        err: std::io::Error,
    },
    /// I/O plumbing to the child failed mid-run.
    Plumbing {
        /// The program being run.
        program: PathBuf,
        /// What broke.
        detail: String,
    },
    /// A [`ScriptedRunner`] was asked for a program it has no script for.
    NotScripted {
        /// The unscripted program.
        program: PathBuf,
    },
    /// The spec's program path is not absolute. The mechanism refuses PATH
    /// resolution outright: the D2 boundary resolves its one tool to an
    /// absolute path (and content-hashes it into provenance) before any
    /// spawn, so an ambient `PATH` can never choose the executable.
    ProgramNotAbsolute {
        /// The offending program path.
        program: PathBuf,
    },
    /// The host target cannot provide process-tree cancellation through the
    /// pinned safe standard-library surface. D2 requires a refusal, not a
    /// direct-child-only downgrade.
    ProcessTreeCancellationUnsupported {
        /// The program that was not spawned.
        program: PathBuf,
    },
    /// A streaming start was supplied preloaded stdin bytes.
    ///
    /// Long-lived sessions receive input only through
    /// [`RunningProcess::write_stdin`], so accepting both channels would make
    /// byte order ambiguous.
    StreamingInputPreloaded {
        /// The program whose invalid specification was refused.
        program: PathBuf,
    },
    /// One stdin chunk exceeded the mechanism's declared bound.
    StdinChunkLimit {
        /// The program receiving input.
        program: PathBuf,
        /// Attempted bytes.
        attempted: u64,
        /// Configured maximum.
        max: u64,
    },
    /// Cumulative stdin exceeded the mechanism's declared bound.
    StdinTotalLimit {
        /// The program receiving input.
        program: PathBuf,
        /// Attempted bytes.
        attempted: u64,
        /// Configured maximum.
        max: u64,
    },
}

impl fmt::Display for ProcessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn { program, err } => {
                write!(f, "cannot spawn {}: {err}", program.display())
            }
            Self::Plumbing { program, detail } => {
                write!(f, "I/O plumbing to {} failed: {detail}", program.display())
            }
            Self::NotScripted { program } => {
                write!(f, "no scripted outcome for {}", program.display())
            }
            Self::ProgramNotAbsolute { program } => {
                write!(
                    f,
                    "program path {} is not absolute; the process capability \
                     refuses PATH resolution (D2: resolve and fingerprint the \
                     tool first)",
                    program.display()
                )
            }
            Self::ProcessTreeCancellationUnsupported { program } => write!(
                f,
                "cannot spawn {}: this target has no safe process-tree \
                 cancellation mechanism required by D2",
                program.display()
            ),
            Self::StreamingInputPreloaded { program } => write!(
                f,
                "streaming process {} must start without preloaded stdin bytes",
                program.display()
            ),
            Self::StdinChunkLimit {
                program,
                attempted,
                max,
            } => write!(
                f,
                "stdin chunk of {attempted} bytes for {} exceeds limit {max}",
                program.display()
            ),
            Self::StdinTotalLimit {
                program,
                attempted,
                max,
            } => write!(
                f,
                "stdin total of {attempted} bytes for {} exceeds limit {max}",
                program.display()
            ),
        }
    }
}

impl std::error::Error for ProcessError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn { err, .. } => Some(err),
            _ => None,
        }
    }
}

/// A started child process with bounded, backpressured stdin.
///
/// Implementations must enforce the start specification's timeout and output
/// caps for the whole session, not only while [`Self::finish`] is waiting.
/// Dropping a live session must cancel and reap it.
pub trait RunningProcess: Send {
    /// Write the next ordered stdin chunk, blocking when the child applies
    /// backpressure.
    ///
    /// # Errors
    /// [`ProcessError`] when the pipe or supervision mechanism fails.
    fn write_stdin(&mut self, bytes: &[u8]) -> Result<(), ProcessError>;

    /// Close stdin, wait for the bounded child, and return its outcome.
    ///
    /// # Errors
    /// [`ProcessError`] when process supervision fails.
    fn finish(self: Box<Self>) -> Result<ProcessOutcome, ProcessError>;

    /// Cancel, close stdin, and reap the child.
    ///
    /// # Errors
    /// [`ProcessError`] when process supervision fails.
    fn cancel(self: Box<Self>) -> Result<(), ProcessError>;
}

/// The process capability.
pub trait ProcessRunner: Send + Sync {
    /// Start a long-lived process whose stdin is supplied incrementally.
    ///
    /// `spec.stdin` must be `None`; the returned session is the only stdin
    /// channel. `cancellation` is polled for the complete child lifetime.
    ///
    /// # Errors
    /// [`ProcessError`] when the process cannot be started.
    fn start(
        &self,
        spec: &ProcessSpec,
        cancellation: ProcessCancellation,
        stdin_limits: ProcessStdinLimits,
    ) -> Result<Box<dyn RunningProcess>, ProcessError>;

    /// Run the process to completion under the spec's bounds.
    ///
    /// # Errors
    /// [`ProcessError`] when the mechanism itself fails; a process that
    /// runs and exits nonzero is an `Ok` outcome.
    fn run(&self, spec: &ProcessSpec) -> Result<ProcessOutcome, ProcessError> {
        let mut start_spec = spec.clone();
        let stdin = start_spec.stdin.take();
        let stdin_bytes = stdin.as_ref().map_or(0, Vec::len);
        let stdin_bytes = u64::try_from(stdin_bytes).unwrap_or(u64::MAX);
        let mut process = self.start(
            &start_spec,
            ProcessCancellation::new(),
            ProcessStdinLimits::new(stdin_bytes.max(1), stdin_bytes),
        )?;
        if let Some(bytes) = stdin
            && let Err(error) = process.write_stdin(&bytes)
        {
            let _ = process.cancel();
            return Err(error);
        }
        process.finish()
    }
}

/// How often the std runner polls the child while enforcing the timeout.
const POLL_INTERVAL: Duration = Duration::from_millis(5);

/// Every runner (std and scripted alike) refuses relative program paths:
/// the trait contract, not an implementation detail.
fn require_absolute(spec: &ProcessSpec) -> Result<(), ProcessError> {
    if spec.program.is_absolute() {
        Ok(())
    } else {
        Err(ProcessError::ProgramNotAbsolute {
            program: spec.program.clone(),
        })
    }
}

/// The host implementation over `std::process::Command`.
#[derive(Clone, Copy, Debug, Default)]
pub struct StdProcessRunner;

#[cfg(all(unix, not(target_os = "espidf")))]
fn configure_process_tree(
    command: &mut std::process::Command,
    _program: &std::path::Path,
) -> Result<(), ProcessError> {
    use std::os::unix::process::CommandExt as _;

    command.process_group(0);
    Ok(())
}

#[cfg(not(all(unix, not(target_os = "espidf"))))]
fn configure_process_tree(
    _command: &mut std::process::Command,
    program: &std::path::Path,
) -> Result<(), ProcessError> {
    Err(ProcessError::ProcessTreeCancellationUnsupported {
        program: program.to_path_buf(),
    })
}

#[cfg(all(unix, not(target_os = "espidf")))]
fn kill_process_tree(child: &mut std::process::Child) -> std::io::Result<()> {
    use std::os::unix::process::ChildExt as _;

    child.kill_process_group()
}

#[cfg(not(all(unix, not(target_os = "espidf"))))]
fn kill_process_tree(_child: &mut std::process::Child) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "safe process-tree cancellation is unavailable",
    ))
}

/// Drain one output pipe on its own thread, capturing up to `cap` bytes and
/// discarding the rest (so the child is never back-pressured into a pipe
/// deadlock). Sets `overflow` the moment the cap is exceeded — the poll loop
/// watches it and kills the child promptly. Returns the captured bytes.
fn drain(
    mut pipe: impl std::io::Read + Send + 'static,
    cap: u64,
    overflow: Arc<AtomicBool>,
) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let cap = usize::try_from(cap).unwrap_or(usize::MAX);
        let mut captured = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            match pipe.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let room = cap.saturating_sub(captured.len());
                    captured.extend_from_slice(&buf[..n.min(room)]);
                    if n > room {
                        overflow.store(true, Ordering::Relaxed);
                        // Keep reading (and discarding) until the kill
                        // closes the pipe.
                    }
                }
            }
        }
        captured
    })
}

impl ProcessRunner for StdProcessRunner {
    fn start(
        &self,
        spec: &ProcessSpec,
        cancellation: ProcessCancellation,
        stdin_limits: ProcessStdinLimits,
    ) -> Result<Box<dyn RunningProcess>, ProcessError> {
        require_absolute(spec)?;
        if spec.stdin.is_some() {
            return Err(ProcessError::StreamingInputPreloaded {
                program: spec.program.clone(),
            });
        }
        // The program is an absolute, caller-resolved path (checked above),
        // never a PATH lookup or user-composed string.
        // The trusted absolute executable capability is resolved and fingerprinted
        // by the ffmpeg boundary before it reaches this runner.
        let mut cmd = std::process::Command::new(&spec.program); // ubs:ignore
        cmd.args(&spec.argv)
            .env_clear()
            .envs(spec.env.iter().map(|(k, v)| (k, v)))
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .stdin(std::process::Stdio::piped());
        configure_process_tree(&mut cmd, &spec.program)?;
        if let Some(cwd) = &spec.cwd {
            cmd.current_dir(cwd);
        }
        let mut child = cmd.spawn().map_err(|err| ProcessError::Spawn {
            program: spec.program.clone(),
            err,
        })?;

        let plumbing = |detail: &str| ProcessError::Plumbing {
            program: spec.program.clone(),
            detail: detail.to_string(),
        };
        let Some(stdin) = child.stdin.take() else {
            let _ = kill_process_tree(&mut child);
            let _ = child.wait();
            return Err(plumbing("no stdin pipe"));
        };
        let overflow = Arc::new(AtomicBool::new(false));
        let Some(stdout) = child.stdout.take() else {
            let _ = kill_process_tree(&mut child);
            let _ = child.wait();
            return Err(plumbing("no stdout pipe"));
        };
        let Some(stderr) = child.stderr.take() else {
            let _ = kill_process_tree(&mut child);
            let _ = child.wait();
            return Err(plumbing("no stderr pipe"));
        };
        let stdout_thread = drain(stdout, spec.max_output_bytes, Arc::clone(&overflow));
        let stderr_thread = drain(stderr, spec.max_output_bytes, Arc::clone(&overflow));
        let program = spec.program.clone();
        let timeout = spec.timeout;
        let supervisor_cancellation = cancellation.clone();
        let (outcome_tx, outcome_rx) = mpsc::sync_channel(1);
        let supervisor = std::thread::spawn(move || {
            let result = supervise_child(
                child,
                &program,
                timeout,
                supervisor_cancellation,
                overflow,
                stdout_thread,
                stderr_thread,
            );
            let _ = outcome_tx.send(result);
        });
        Ok(Box::new(StdRunningProcess {
            program: spec.program.clone(),
            stdin: Some(stdin),
            outcome_rx,
            supervisor: Some(supervisor),
            cancellation,
            stdin_limits,
            stdin_bytes: 0,
            finished: false,
        }))
    }
}

fn supervise_child(
    mut child: std::process::Child,
    program: &std::path::Path,
    timeout: Duration,
    cancellation: ProcessCancellation,
    overflow: Arc<AtomicBool>,
    stdout_thread: std::thread::JoinHandle<Vec<u8>>,
    stderr_thread: std::thread::JoinHandle<Vec<u8>>,
) -> Result<ProcessOutcome, ProcessError> {
    let start = Instant::now();
    let termination = loop {
        // Reaping the group leader makes std's safe group-signal handle a
        // no-op. Once both inherited pipes close, kill the isolated group
        // before reaping. For an already-exited leader this preserves its
        // status while terminating redirected descendants; a leader that
        // closed both supervision pipes early is failed closed.
        if stdout_thread.is_finished() && stderr_thread.is_finished() {
            match kill_process_tree(&mut child) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    // No member remains in the isolated group.
                }
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(ProcessError::Plumbing {
                        program: program.to_path_buf(),
                        detail: format!("process-tree completion kill failed: {error}"),
                    });
                }
            }
            match child.try_wait() {
                Ok(Some(status)) => break ProcessTermination::Exited(status.code()),
                Ok(None) => {}
                Err(error) => {
                    let _ = kill_process_tree(&mut child);
                    let _ = child.wait();
                    return Err(ProcessError::Plumbing {
                        program: program.to_path_buf(),
                        detail: format!(
                            "try_wait after process-tree completion kill failed: {error}"
                        ),
                    });
                }
            }
        }
        let terminal = if overflow.load(Ordering::Acquire) {
            Some(ProcessTermination::OutputLimitExceeded)
        } else if cancellation.is_cancelled() {
            Some(ProcessTermination::Cancelled)
        } else if start.elapsed() >= timeout {
            Some(ProcessTermination::TimedOut)
        } else {
            None
        };
        if let Some(terminal) = terminal {
            if let Err(err) = kill_process_tree(&mut child) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ProcessError::Plumbing {
                    program: program.to_path_buf(),
                    detail: format!("process-tree kill failed: {err}"),
                });
            }
            break terminal;
        }
        std::thread::sleep(POLL_INTERVAL);
    };
    // Reap after a terminal kill so no zombie outlives the session. A
    // pipe-closed completion observed through `try_wait` was already reaped.
    if !matches!(termination, ProcessTermination::Exited(_)) {
        let _ = child.wait();
    }
    let plumbing = |detail: &str| ProcessError::Plumbing {
        program: program.to_path_buf(),
        detail: detail.to_string(),
    };
    let stdout = stdout_thread
        .join()
        .map_err(|_| plumbing("stdout drain panicked"))?;
    let stderr = stderr_thread
        .join()
        .map_err(|_| plumbing("stderr drain panicked"))?;
    // A cap can also trip between process exit and the final drain. Output
    // overflow is authoritative even when an exit was observed first.
    let termination = if overflow.load(Ordering::Acquire) {
        ProcessTermination::OutputLimitExceeded
    } else {
        termination
    };
    Ok(ProcessOutcome {
        termination,
        stdout,
        stderr,
    })
}

struct StdRunningProcess {
    program: PathBuf,
    stdin: Option<std::process::ChildStdin>,
    outcome_rx: mpsc::Receiver<Result<ProcessOutcome, ProcessError>>,
    supervisor: Option<std::thread::JoinHandle<()>>,
    cancellation: ProcessCancellation,
    stdin_limits: ProcessStdinLimits,
    stdin_bytes: u64,
    finished: bool,
}

impl StdRunningProcess {
    fn wait(&mut self) -> Result<ProcessOutcome, ProcessError> {
        self.stdin.take();
        let outcome = self.outcome_rx.recv().map_err(|_| ProcessError::Plumbing {
            program: self.program.clone(),
            detail: "process supervisor exited without an outcome".to_string(),
        })?;
        if let Some(supervisor) = self.supervisor.take() {
            supervisor.join().map_err(|_| ProcessError::Plumbing {
                program: self.program.clone(),
                detail: "process supervisor panicked".to_string(),
            })?;
        }
        self.finished = true;
        outcome
    }
}

impl RunningProcess for StdRunningProcess {
    fn write_stdin(&mut self, bytes: &[u8]) -> Result<(), ProcessError> {
        if self.cancellation.is_cancelled() {
            return Err(ProcessError::Plumbing {
                program: self.program.clone(),
                detail: "process was cancelled before stdin write".to_string(),
            });
        }
        let chunk_bytes = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if chunk_bytes > self.stdin_limits.max_chunk_bytes {
            return Err(ProcessError::StdinChunkLimit {
                program: self.program.clone(),
                attempted: chunk_bytes,
                max: self.stdin_limits.max_chunk_bytes,
            });
        }
        let attempted =
            self.stdin_bytes
                .checked_add(chunk_bytes)
                .ok_or(ProcessError::StdinTotalLimit {
                    program: self.program.clone(),
                    attempted: u64::MAX,
                    max: self.stdin_limits.max_total_bytes,
                })?;
        if attempted > self.stdin_limits.max_total_bytes {
            return Err(ProcessError::StdinTotalLimit {
                program: self.program.clone(),
                attempted,
                max: self.stdin_limits.max_total_bytes,
            });
        }
        self.stdin
            .as_mut()
            .ok_or_else(|| ProcessError::Plumbing {
                program: self.program.clone(),
                detail: "stdin is already closed".to_string(),
            })?
            .write_all(bytes)
            .map_err(|error| ProcessError::Plumbing {
                program: self.program.clone(),
                detail: format!("stdin write failed: {error}"),
            })?;
        self.stdin_bytes = attempted;
        Ok(())
    }

    fn finish(mut self: Box<Self>) -> Result<ProcessOutcome, ProcessError> {
        self.wait()
    }

    fn cancel(mut self: Box<Self>) -> Result<(), ProcessError> {
        self.cancellation.cancel();
        self.wait().map(|_| ())
    }
}

impl Drop for StdRunningProcess {
    fn drop(&mut self) {
        if !self.finished {
            self.cancellation.cancel();
            let _ = self.wait();
        }
    }
}

/// The test double: canned outcomes per program path, with a full log of
/// every spec it was asked to run.
#[derive(Debug, Default)]
pub struct ScriptedRunner {
    scripts: BTreeMap<PathBuf, ProcessOutcome>,
    runs: Arc<Mutex<Vec<ProcessSpec>>>,
}

impl ScriptedRunner {
    /// An empty scripted runner (every run is [`ProcessError::NotScripted`]).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Script the outcome for a program path.
    pub fn script(&mut self, program: impl Into<PathBuf>, outcome: ProcessOutcome) {
        self.scripts.insert(program.into(), outcome);
    }

    /// Every spec run so far, in order.
    #[must_use]
    pub fn runs(&self) -> Vec<ProcessSpec> {
        self.runs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl ProcessRunner for ScriptedRunner {
    fn start(
        &self,
        spec: &ProcessSpec,
        cancellation: ProcessCancellation,
        stdin_limits: ProcessStdinLimits,
    ) -> Result<Box<dyn RunningProcess>, ProcessError> {
        require_absolute(spec)?;
        if spec.stdin.is_some() {
            return Err(ProcessError::StreamingInputPreloaded {
                program: spec.program.clone(),
            });
        }
        let outcome =
            self.scripts
                .get(&spec.program)
                .cloned()
                .ok_or_else(|| ProcessError::NotScripted {
                    program: spec.program.clone(),
                })?;
        Ok(Box::new(ScriptedProcess {
            spec: spec.clone(),
            input: Vec::new(),
            outcome,
            runs: Arc::clone(&self.runs),
            cancellation,
            stdin_limits,
            stdin_bytes: 0,
            finished: false,
        }))
    }
}

struct ScriptedProcess {
    spec: ProcessSpec,
    input: Vec<u8>,
    outcome: ProcessOutcome,
    runs: Arc<Mutex<Vec<ProcessSpec>>>,
    cancellation: ProcessCancellation,
    stdin_limits: ProcessStdinLimits,
    stdin_bytes: u64,
    finished: bool,
}

impl ScriptedProcess {
    fn record(&mut self) {
        if self.finished {
            return;
        }
        if !self.input.is_empty() {
            self.spec.stdin = Some(std::mem::take(&mut self.input));
        }
        self.runs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(self.spec.clone());
        self.finished = true;
    }
}

impl RunningProcess for ScriptedProcess {
    fn write_stdin(&mut self, bytes: &[u8]) -> Result<(), ProcessError> {
        if self.cancellation.is_cancelled() {
            return Err(ProcessError::Plumbing {
                program: self.spec.program.clone(),
                detail: "scripted process was cancelled before stdin write".to_string(),
            });
        }
        let chunk_bytes = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if chunk_bytes > self.stdin_limits.max_chunk_bytes {
            return Err(ProcessError::StdinChunkLimit {
                program: self.spec.program.clone(),
                attempted: chunk_bytes,
                max: self.stdin_limits.max_chunk_bytes,
            });
        }
        let attempted =
            self.stdin_bytes
                .checked_add(chunk_bytes)
                .ok_or(ProcessError::StdinTotalLimit {
                    program: self.spec.program.clone(),
                    attempted: u64::MAX,
                    max: self.stdin_limits.max_total_bytes,
                })?;
        if attempted > self.stdin_limits.max_total_bytes {
            return Err(ProcessError::StdinTotalLimit {
                program: self.spec.program.clone(),
                attempted,
                max: self.stdin_limits.max_total_bytes,
            });
        }
        self.input.extend_from_slice(bytes);
        self.stdin_bytes = attempted;
        Ok(())
    }

    fn finish(mut self: Box<Self>) -> Result<ProcessOutcome, ProcessError> {
        self.record();
        let mut outcome = self.outcome.clone();
        if self.cancellation.is_cancelled() {
            outcome.termination = ProcessTermination::Cancelled;
        }
        Ok(outcome)
    }

    fn cancel(mut self: Box<Self>) -> Result<(), ProcessError> {
        self.cancellation.cancel();
        self.record();
        Ok(())
    }
}

impl Drop for ScriptedProcess {
    fn drop(&mut self) {
        self.record();
    }
}
