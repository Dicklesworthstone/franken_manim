//! Cooperative batch cancellation at existing filesystem/process boundaries.
//!
//! A deadline may reject a new operation, never rewrite its successful bytes.
//! Prepared publication tokens are guarded too. An operation already admitted
//! may finish; cleanup always remains available. There is no thread detachment
//! or attempt to preempt arbitrary Rust/filesystem code.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fmn_platform::fs::{
    AtomicDirectoryWriter, AtomicFileWriter, FileSystem, FsError, FsNodeKind,
    PreparedAtomicDirectory, PreparedAtomicFile,
};
use fmn_platform::process::{
    ProcessCancellation, ProcessError, ProcessMechanism, ProcessOutcome, ProcessRunner,
    ProcessSpec, ProcessStdinLimits, RunningProcess,
};

use super::Failure;

pub(super) struct Control {
    deadline: Option<Instant>,
    cancelled: AtomicBool,
    processes: Mutex<Vec<Option<ProcessCancellation>>>,
}

impl Control {
    pub fn new(budget_ms: Option<u64>) -> Result<Arc<Self>, Failure> {
        let deadline = budget_ms
            .map(|milliseconds| {
                Instant::now()
                    .checked_add(Duration::from_millis(milliseconds))
                    .ok_or_else(|| Failure::new("config", "--budget-ms exceeds the host clock"))
            })
            .transpose()?;
        Ok(Arc::new(Self {
            deadline,
            cancelled: AtomicBool::new(false),
            processes: Mutex::new(Vec::new()),
        }))
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        for token in self.processes.lock().unwrap_or_else(|e| e.into_inner()).iter().flatten() {
            token.cancel();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        if self.deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            self.cancel();
        }
        self.cancelled.load(Ordering::Acquire)
    }

    pub fn remaining(&self) -> Option<Duration> {
        self.deadline.map(|deadline| deadline.saturating_duration_since(Instant::now()))
    }

    pub fn checkpoint(&self) -> Result<(), Failure> {
        if self.is_cancelled() {
            Err(Failure::new("budget", "narrated batch job cancelled at its wall-clock budget"))
        } else {
            Ok(())
        }
    }

    fn check_fs(&self, path: &Path) -> Result<(), FsError> {
        if self.is_cancelled() {
            Err(FsError::Io {
                path: path.to_owned(),
                err: std::io::Error::new(std::io::ErrorKind::TimedOut, "narrated batch budget exhausted"),
            })
        } else {
            Ok(())
        }
    }

    fn register(self: &Arc<Self>, token: ProcessCancellation) -> Registration {
        let mut processes = self.processes.lock().unwrap_or_else(|e| e.into_inner());
        if self.cancelled.load(Ordering::Acquire) {
            token.cancel();
        }
        let index = if let Some(index) = processes.iter().position(Option::is_none) {
            processes[index] = Some(token);
            index
        } else {
            processes.push(Some(token));
            processes.len() - 1
        };
        Registration { control: Arc::clone(self), index }
    }
}

struct Registration {
    control: Arc<Control>,
    index: usize,
}

impl Drop for Registration {
    fn drop(&mut self) {
        self.control.processes.lock().unwrap_or_else(|e| e.into_inner())[self.index] = None;
    }
}

pub(super) struct GuardedFs {
    pub inner: Arc<dyn FileSystem>,
    pub control: Arc<Control>,
}

macro_rules! guarded_fs {
    ($($method:ident($($arg:ident: $ty:ty),*) -> $output:ty;)*) => {$ (
        fn $method(&self, path: &Path, $($arg: $ty),*) -> Result<$output, FsError> {
            self.control.check_fs(path)?;
            self.inner.$method(path, $($arg),*)
        }
    )*};
}

impl FileSystem for GuardedFs {
    // This decorator adds cancellation, not another source of successful
    // bytes. Scheduling and wall time remain outside semantic identity.
    fn identity(&self) -> &'static str { self.inner.identity() }
    guarded_fs! {
        node_kind_no_follow() -> Option<FsNodeKind>;
        create_dir() -> bool;
        read() -> Vec<u8>;
        read_bounded(max_bytes: usize) -> Vec<u8>;
        write_atomic(bytes: &[u8]) -> ();
        create_new(bytes: &[u8]) -> bool;
        list_dir() -> Vec<PathBuf>;
        count_dir_entries_bounded(max_entries: usize) -> usize;
    }
    fn exists(&self, path: &Path) -> bool { self.inner.exists(path) }
    fn remove_file(&self, path: &Path) -> Result<(), FsError> { self.inner.remove_file(path) }
    fn remove_dir_all(&self, path: &Path) -> Result<(), FsError> { self.inner.remove_dir_all(path) }
    fn begin_atomic_file(self: Arc<Self>, path: &Path) -> Result<Box<dyn AtomicFileWriter>, FsError> {
        self.control.check_fs(path)?;
        Ok(Box::new(FileWriter {
            inner: Arc::clone(&self.inner).begin_atomic_file(path)?,
            control: Arc::clone(&self.control), path: path.to_owned(),
        }))
    }
    fn begin_atomic_directory(self: Arc<Self>, path: &Path) -> Result<Box<dyn AtomicDirectoryWriter>, FsError> {
        self.control.check_fs(path)?;
        Ok(Box::new(DirectoryWriter {
            inner: Arc::clone(&self.inner).begin_atomic_directory(path)?,
            control: Arc::clone(&self.control), path: path.to_owned(),
        }))
    }
}

struct FileWriter {
    inner: Box<dyn AtomicFileWriter>,
    control: Arc<Control>,
    path: PathBuf,
}
impl AtomicFileWriter for FileWriter {
    fn write(&mut self, bytes: &[u8]) -> Result<(), FsError> {
        self.control.check_fs(&self.path)?;
        self.inner.write(bytes)
    }
    fn prepare(self: Box<Self>) -> Result<Box<dyn PreparedAtomicFile>, FsError> {
        self.control.check_fs(&self.path)?;
        Ok(Box::new(PreparedFile { inner: self.inner.prepare()?, control: self.control, path: self.path }))
    }
}
struct PreparedFile {
    inner: Box<dyn PreparedAtomicFile>,
    control: Arc<Control>,
    path: PathBuf,
}
impl PreparedAtomicFile for PreparedFile {
    fn commit(self: Box<Self>) -> Result<(), FsError> {
        self.control.check_fs(&self.path)?;
        self.inner.commit()
    }
    fn commit_new(self: Box<Self>) -> Result<(), FsError> {
        self.control.check_fs(&self.path)?;
        self.inner.commit_new()
    }
}
struct DirectoryWriter {
    inner: Box<dyn AtomicDirectoryWriter>,
    control: Arc<Control>,
    path: PathBuf,
}
impl AtomicDirectoryWriter for DirectoryWriter {
    fn write_file(&mut self, leaf: &Path, bytes: &[u8]) -> Result<(), FsError> {
        self.control.check_fs(&self.path)?;
        self.inner.write_file(leaf, bytes)
    }
    fn prepare(self: Box<Self>) -> Result<Box<dyn PreparedAtomicDirectory>, FsError> {
        self.control.check_fs(&self.path)?;
        Ok(Box::new(PreparedDirectory { inner: self.inner.prepare()?, control: self.control, path: self.path }))
    }
}
struct PreparedDirectory {
    inner: Box<dyn PreparedAtomicDirectory>,
    control: Arc<Control>,
    path: PathBuf,
}
impl PreparedAtomicDirectory for PreparedDirectory {
    fn commit(self: Box<Self>) -> Result<(), FsError> {
        self.control.check_fs(&self.path)?;
        self.inner.commit()
    }
}

pub(super) struct GuardedRunner {
    pub inner: Arc<dyn ProcessRunner>,
    pub control: Arc<Control>,
}
impl ProcessRunner for GuardedRunner {
    fn mechanism(&self) -> ProcessMechanism { self.inner.mechanism() }
    fn start(&self, spec: &ProcessSpec, cancellation: ProcessCancellation, limits: ProcessStdinLimits)
        -> Result<Box<dyn RunningProcess>, ProcessError>
    {
        let registration = self.control.register(cancellation.clone());
        if self.control.is_cancelled() {
            return Err(ProcessError::Plumbing {
                program: spec.program.clone(), detail: "narrated batch budget exhausted before process start".to_owned(),
            });
        }
        Ok(Box::new(GuardedProcess {
            inner: self.inner.start(spec, cancellation, limits)?,
            registration,
        }))
    }
    // The trait's run() goes through this start(), so probes and mux jobs are
    // supervised as well as streaming encodes. No incoming cancellation token,
    // timeout, argv, mechanism identity, or stdin bound is replaced.
}
struct GuardedProcess {
    inner: Box<dyn RunningProcess>,
    registration: Registration,
}
impl RunningProcess for GuardedProcess {
    fn write_stdin(&mut self, bytes: &[u8]) -> Result<(), ProcessError> { self.inner.write_stdin(bytes) }
    fn finish(self: Box<Self>) -> Result<ProcessOutcome, ProcessError> {
        let Self { inner, registration } = *self;
        let result = inner.finish();
        drop(registration);
        result
    }
    fn cancel(self: Box<Self>) -> Result<(), ProcessError> {
        let Self { inner, registration } = *self;
        let result = inner.cancel();
        drop(registration);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmn_platform::fs::VirtualFs;

    #[test]
    fn cancellation_invalidates_prepared_file_and_directory_tokens() {
        let inner = Arc::new(VirtualFs::new());
        let control = Control::new(None).unwrap();
        let fs = Arc::new(GuardedFs { inner: inner.clone(), control: Arc::clone(&control) });
        let mut new_file = fs.clone().begin_atomic_file(Path::new("/new.wav")).unwrap();
        new_file.write(b"private").unwrap();
        let file_token = new_file.prepare().unwrap();
        let mut replacing = fs.clone().begin_atomic_file(Path::new("/winner.wav")).unwrap();
        replacing.write(b"replacement").unwrap();
        let replacing_token = replacing.prepare().unwrap();
        inner.insert("/winner.wav", b"original".to_vec());
        let mut directory = fs.clone().begin_atomic_directory(Path::new("/manifest")).unwrap();
        directory.write_file(Path::new("manifest.fmnp"), b"private closure").unwrap();
        let directory_token = directory.prepare().unwrap();
        control.cancel();
        assert!(file_token.commit_new().is_err());
        assert!(replacing_token.commit().is_err());
        assert!(directory_token.commit().is_err());
        assert!(!inner.exists(Path::new("/new.wav")));
        assert!(!inner.exists(Path::new("/manifest")));
        assert_eq!(inner.read(Path::new("/winner.wav")).unwrap(), b"original");
        assert!(!fs.grants_host_destructive_lifecycle());
    }

    #[test]
    fn cancellation_stops_staging_but_never_blocks_cleanup() {
        let inner = Arc::new(VirtualFs::new());
        let control = Control::new(None).unwrap();
        let fs = Arc::new(GuardedFs { inner: inner.clone(), control: Arc::clone(&control) });
        let mut file = fs.clone().begin_atomic_file(Path::new("/private")).unwrap();
        let mut directory = fs.clone().begin_atomic_directory(Path::new("/private-dir")).unwrap();
        inner.insert("/cleanup/file", b"temporary".to_vec());
        inner.insert("/cleanup2/file", b"temporary".to_vec());
        control.cancel();
        assert!(file.write(b"late").is_err());
        assert!(directory.write_file(Path::new("late"), b"late").is_err());
        assert!(file.prepare().is_err());
        assert!(directory.prepare().is_err());
        assert!(fs.create_new(Path::new("/late"), b"late").is_err());
        fs.remove_file(Path::new("/cleanup/file")).unwrap();
        fs.remove_dir_all(Path::new("/cleanup2")).unwrap();
        assert!(!inner.exists(Path::new("/cleanup2")));
        assert!(!inner.exists(Path::new("/private")));
        assert!(!inner.exists(Path::new("/private-dir")));
    }

    #[test]
    fn completed_process_registrations_are_reclaimed_and_live_tokens_cancel() {
        let control = Control::new(None).unwrap();
        let retired = ProcessCancellation::new();
        let registration = control.register(retired.clone());
        drop(registration);
        let live = ProcessCancellation::new();
        let registration = control.register(live.clone());
        assert_eq!(control.processes.lock().unwrap().len(), 1);
        control.cancel();
        assert!(live.is_cancelled());
        assert!(!retired.is_cancelled());
        let late = ProcessCancellation::new();
        let late_registration = control.register(late.clone());
        assert!(late.is_cancelled());
        drop(registration);
        drop(late_registration);
        assert!(control.processes.lock().unwrap().iter().all(Option::is_none));
    }

    #[test]
    fn a_zero_deadline_is_observed_without_a_watchdog_thread() {
        let control = Control::new(Some(0)).unwrap();
        assert!(control.checkpoint().is_err());
        assert!(control.is_cancelled());
        assert_eq!(control.remaining(), Some(Duration::ZERO));
    }
}
