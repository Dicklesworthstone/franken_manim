//! Live disk budgets for the one external-tool boundary.
//!
//! The watcher stays active while a producer is blocked on stdin and while
//! ffmpeg finishes an encode/mux/decode. Cancellation uses the process
//! capability's existing process-tree supervisor; this module never kills a
//! PID or reads child-created file contents. Final verification owns publication.

use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread::JoinHandle;

use super::*;

/// Maximum idle delay between disk-budget inspections, excluding scan time,
/// the process mechanism's cancellation polling, and host scheduling delay.
const POLL_INTERVAL: Duration = Duration::from_millis(25);
const MAX_ENTRIES: usize = 4096;
const MAX_DEPTH: usize = 32;

fn failure(detail: impl Into<String>) -> BoundaryError {
    BoundaryError::Workdir {
        detail: detail.into(),
    }
}

/// Count regular-file bytes and refuse observed symlinks or special files.
/// Entry and depth limits bound a scan even when a child creates empty files.
///
/// These metadata/path checks share the enclosing boundary's same-identity
/// rename-race limitation: safe std has no handle-relative directory traversal.
/// A path replaced after its check can race the subsequent directory open.
fn workdir_bytes(workdir: &OwnedWorkdir, maximum: u64) -> Result<u64, BoundaryError> {
    workdir.verify_current("inspect live ffmpeg disk budget")?;
    let mut pending = vec![(workdir.path().to_path_buf(), 0_usize)];
    let mut entries = 0_usize;
    let mut bytes = 0_u64;
    while let Some((directory, depth)) = pending.pop() {
        // Recheck queued directories before opening them: an encoder may have
        // removed or replaced one since its parent was inspected. This rejects
        // observed substitutions, not races after this final path check.
        if depth > 0 {
            match std::fs::symlink_metadata(&directory) {
                Ok(metadata) if metadata.is_dir() => {}
                Ok(_) => return Err(failure("ffmpeg scratch directory changed its file type")),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(failure(format!(
                        "inspect ffmpeg scratch directory: {error}"
                    )));
                }
            }
        }
        let children = match std::fs::read_dir(&directory) {
            Ok(children) => children,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && depth > 0 => continue,
            Err(error) => {
                return Err(failure(format!("inspect ffmpeg workdir: {error}")));
            }
        };
        for child in children {
            let child =
                child.map_err(|error| failure(format!("inspect ffmpeg workdir entry: {error}")))?;
            entries += 1;
            if entries > MAX_ENTRIES {
                return Err(failure("ffmpeg workdir exceeds its 4096-entry budget"));
            }
            let path = child.path();
            let metadata = match std::fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                // Encoders may rename or unlink their own temporary files.
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(failure(format!("inspect ffmpeg workdir file: {error}")));
                }
            };
            if metadata.is_file() {
                bytes =
                    bytes
                        .checked_add(metadata.len())
                        .ok_or(BoundaryError::WorkdirOversized {
                            bytes: u64::MAX,
                            max: maximum,
                        })?;
                if bytes > maximum {
                    return Err(BoundaryError::WorkdirOversized {
                        bytes,
                        max: maximum,
                    });
                }
            } else if metadata.is_dir() {
                if depth == MAX_DEPTH {
                    return Err(failure(
                        "ffmpeg workdir exceeds its 32-level nesting budget",
                    ));
                }
                pending.push((path, depth + 1));
            } else {
                return Err(failure(
                    "ffmpeg workdir contains a symlink or non-regular file",
                ));
            }
        }
    }
    workdir.verify_current("finish live ffmpeg disk-budget inspection")?;
    Ok(bytes)
}

fn check_budget(
    workdir: &OwnedWorkdir,
    artifact: &Path,
    artifact_limit: u64,
    workdir_limit: u64,
) -> Result<(), BoundaryError> {
    workdir.verify_current("inspect live ffmpeg artifact")?;
    match std::fs::symlink_metadata(artifact) {
        Ok(metadata) if !metadata.is_file() => {
            return Err(failure("ffmpeg output is not a regular private file"));
        }
        Ok(metadata) if metadata.len() > artifact_limit => {
            return Err(BoundaryError::ArtifactOversized {
                bytes: metadata.len(),
                max: artifact_limit,
            });
        }
        Ok(_) => {}
        // The output need not exist until ffmpeg has received its first frame.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(failure(format!("inspect live ffmpeg artifact: {error}")));
        }
    }
    match workdir_bytes(workdir, workdir_limit) {
        Err(error @ BoundaryError::WorkdirOversized { .. }) => {
            // The encoder can grow the output between the direct inspection
            // and the directory scan. Preserve the precise artifact refusal
            // when its own bound caused that aggregate crossing.
            if let Ok(metadata) = std::fs::symlink_metadata(artifact)
                && metadata.is_file()
                && metadata.len() > artifact_limit
            {
                Err(BoundaryError::ArtifactOversized {
                    bytes: metadata.len(),
                    max: artifact_limit,
                })
            } else {
                Err(error)
            }
        }
        result => result.map(|_| ()),
    }
}

pub(super) struct ArtifactWatch {
    stop: Sender<()>,
    worker: Option<JoinHandle<Result<(), BoundaryError>>>,
    workdir_limit: Arc<AtomicU64>,
}

impl ArtifactWatch {
    /// Start before spawning the child so even immediate output is accounted
    /// for. Existing verified executable/input bytes are the fixed baseline;
    /// the output and all newly created scratch files share one artifact budget.
    pub(super) fn start(
        workdir: &OwnedWorkdir,
        artifact: &Path,
        artifact_limit: u64,
        cancellation: ProcessCancellation,
    ) -> Result<Self, BoundaryError> {
        let baseline = workdir_bytes(workdir, u64::MAX)?;
        let maximum = baseline
            .checked_add(artifact_limit)
            .ok_or_else(|| failure("ffmpeg workdir baseline plus artifact budget overflows"))?;
        let workdir_limit = Arc::new(AtomicU64::new(maximum));
        let live_limit = Arc::clone(&workdir_limit);
        let workdir = workdir.clone();
        let artifact = artifact.to_path_buf();
        let (stop, stopped) = mpsc::channel();
        let worker = std::thread::Builder::new()
            .name("fmn-ffmpeg-disk-budget".into())
            .spawn(move || {
                let inspect = || {
                    let limit = live_limit.load(Ordering::Acquire);
                    let mut result = check_budget(&workdir, &artifact, artifact_limit, limit);
                    if matches!(&result, Err(BoundaryError::WorkdirOversized { .. })) {
                        let updated = live_limit.load(Ordering::Acquire);
                        if updated != limit {
                            // A WAV allowance can be published during a scan.
                            // Recheck before attributing that host-owned input
                            // to the child's output/scratch budget.
                            result = check_budget(&workdir, &artifact, artifact_limit, updated);
                        }
                    }
                    if result.is_err() {
                        cancellation.cancel();
                    }
                    result
                };
                loop {
                    inspect()?;
                    match stopped.recv_timeout(POLL_INTERVAL) {
                        Ok(()) | Err(RecvTimeoutError::Disconnected) => return inspect(),
                        Err(RecvTimeoutError::Timeout) => {}
                    }
                }
            })
            .map_err(|error| failure(format!("start ffmpeg disk-budget watcher: {error}")))?;
        Ok(Self {
            stop,
            worker: Some(worker),
            workdir_limit,
        })
    }

    /// A native WAV is the only input that can be staged while the video
    /// process is already running. Its separately validated exact byte count
    /// joins the baseline before the host writes it, never the output budget.
    pub(super) fn allow_staged_input(&self, bytes: u64) -> Result<(), BoundaryError> {
        self.workdir_limit
            .try_update(Ordering::AcqRel, Ordering::Acquire, |limit| {
                limit.checked_add(bytes)
            })
            .map(|_| ())
            .map_err(|_| failure("ffmpeg staged-input disk allowance overflows"))
    }

    pub(super) fn is_finished(&self) -> bool {
        self.worker.as_ref().is_none_or(JoinHandle::is_finished)
    }

    /// Reap after the process is finished/cancelled and before its directory
    /// can be cleaned up. An explicit wake avoids adding one poll of latency.
    pub(super) fn finish(mut self) -> Result<(), BoundaryError> {
        self.stop_and_join()
    }

    fn stop_and_join(&mut self) -> Result<(), BoundaryError> {
        let _ = self.stop.send(());
        self.worker.take().map_or(Ok(()), |worker| {
            worker
                .join()
                .map_err(|_| failure("ffmpeg disk-budget watcher panicked"))?
        })
    }
}

impl Drop for ArtifactWatch {
    fn drop(&mut self) {
        let _ = self.stop_and_join();
    }
}
