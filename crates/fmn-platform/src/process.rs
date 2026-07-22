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
//!   [`ProcessSpec::max_output_bytes`]; exceeding a cap kills the child and
//!   marks the outcome truncated — a runaway encoder cannot fill the disk
//!   or the heap.
//!
//! **Process-tree cancellation, honestly:** with `#![forbid(unsafe_code)]`
//! and no libc, the std runner kills the direct child only. That satisfies
//! the boundary's threat model because ffmpeg does not daemonize or spawn
//! grandchildren under the boundary's invocation contract
//! (FFMPEG_PROTOCOL.md, W8); if a future audit finds otherwise, the revisit
//! path is a platform-specific kill of the child's process group behind
//! this same trait. Higher layers (private temp dirs, atomic publication,
//! provenance fingerprinting) belong to the W8 boundary, not the mechanism.

use std::collections::BTreeMap;
use std::fmt;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
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
    /// Bytes written to the child's stdin (then closed); `None` for a null
    /// stdin. Streaming stdin is the W8 boundary's concern.
    pub stdin: Option<Vec<u8>>,
    /// Wall-clock bound; on expiry the child is killed.
    pub timeout: Duration,
    /// Per-stream cap on captured stdout/stderr bytes; on overflow the
    /// child is killed and the outcome is marked truncated.
    pub max_output_bytes: u64,
}

/// What happened when a spawned process finished (or was stopped).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ProcessOutcome {
    /// The exit code, if the process exited normally with one.
    pub code: Option<i32>,
    /// Captured stdout (up to the cap).
    pub stdout: Vec<u8>,
    /// Captured stderr (up to the cap).
    pub stderr: Vec<u8>,
    /// The timeout expired and the child was killed.
    pub timed_out: bool,
    /// An output cap was hit and the child was killed.
    pub truncated: bool,
}

impl ProcessOutcome {
    /// Whether the process ran to completion with exit code 0.
    #[must_use]
    pub fn success(&self) -> bool {
        self.code == Some(0) && !self.timed_out && !self.truncated
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

/// The process capability.
pub trait ProcessRunner: Send + Sync {
    /// Run the process to completion under the spec's bounds.
    ///
    /// # Errors
    /// [`ProcessError`] when the mechanism itself fails; a process that
    /// runs and exits nonzero is an `Ok` outcome.
    fn run(&self, spec: &ProcessSpec) -> Result<ProcessOutcome, ProcessError>;
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
    fn run(&self, spec: &ProcessSpec) -> Result<ProcessOutcome, ProcessError> {
        require_absolute(spec)?;
        // The program is an absolute, caller-resolved path (checked above),
        // never a PATH lookup or user-composed string.
        let mut cmd = std::process::Command::new(&spec.program);
        cmd.args(&spec.argv)
            .env_clear()
            .envs(spec.env.iter().map(|(k, v)| (k, v)))
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .stdin(if spec.stdin.is_some() {
                std::process::Stdio::piped()
            } else {
                std::process::Stdio::null()
            });
        if let Some(cwd) = &spec.cwd {
            cmd.current_dir(cwd);
        }
        let mut child = cmd.spawn().map_err(|err| ProcessError::Spawn {
            program: spec.program.clone(),
            err,
        })?;

        // Feed stdin from its own thread, then close it. A write error just
        // means the child stopped reading (e.g. it exited) — not a failure
        // of the mechanism.
        let stdin_thread = match (child.stdin.take(), spec.stdin.clone()) {
            (Some(mut pipe), Some(bytes)) => Some(std::thread::spawn(move || {
                let _ = pipe.write_all(&bytes);
            })),
            _ => None,
        };
        let plumbing = |detail: &str| ProcessError::Plumbing {
            program: spec.program.clone(),
            detail: detail.to_string(),
        };
        let overflow = Arc::new(AtomicBool::new(false));
        let stdout_thread = drain(
            child
                .stdout
                .take()
                .ok_or_else(|| plumbing("no stdout pipe"))?,
            spec.max_output_bytes,
            Arc::clone(&overflow),
        );
        let stderr_thread = drain(
            child
                .stderr
                .take()
                .ok_or_else(|| plumbing("no stderr pipe"))?,
            spec.max_output_bytes,
            Arc::clone(&overflow),
        );

        // Poll for exit, timeout expiry, or output-cap overflow; the latter
        // two kill the child (the D2 bounds are enforcement, not advice).
        let start = Instant::now();
        let mut timed_out = false;
        let mut truncated = false;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) => {
                    if overflow.load(Ordering::Relaxed) {
                        truncated = true;
                        let _ = child.kill();
                        break None;
                    }
                    if start.elapsed() >= spec.timeout {
                        timed_out = true;
                        let _ = child.kill();
                        break None;
                    }
                    std::thread::sleep(POLL_INTERVAL);
                }
                Err(err) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(ProcessError::Plumbing {
                        program: spec.program.clone(),
                        detail: format!("try_wait failed: {err}"),
                    });
                }
            }
        };
        // Reap after a kill so no zombie outlives the call.
        let status = match status {
            Some(s) => Some(s),
            None => child.wait().ok(),
        };
        if let Some(t) = stdin_thread {
            let _ = t.join();
        }
        let stdout = stdout_thread
            .join()
            .map_err(|_| plumbing("stdout drain panicked"))?;
        let stderr = stderr_thread
            .join()
            .map_err(|_| plumbing("stderr drain panicked"))?;
        // A cap can also trip between the child's exit and the final drain.
        let truncated = truncated || overflow.load(Ordering::Relaxed);
        Ok(ProcessOutcome {
            code: status.and_then(|s| s.code()),
            stdout,
            stderr,
            timed_out,
            truncated,
        })
    }
}

/// The test double: canned outcomes per program path, with a full log of
/// every spec it was asked to run.
#[derive(Debug, Default)]
pub struct ScriptedRunner {
    scripts: BTreeMap<PathBuf, ProcessOutcome>,
    runs: Mutex<Vec<ProcessSpec>>,
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
    fn run(&self, spec: &ProcessSpec) -> Result<ProcessOutcome, ProcessError> {
        require_absolute(spec)?;
        self.runs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(spec.clone());
        self.scripts
            .get(&spec.program)
            .cloned()
            .ok_or_else(|| ProcessError::NotScripted {
                program: spec.program.clone(),
            })
    }
}
