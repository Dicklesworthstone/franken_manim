//! Native clip-assembly admission and source-snapshot contracts.
//!
//! Successful governed process execution is also covered by the conformance
//! scenario `lifecycle.native_video_concat.v1`, whose native fake ffmpeg checks the
//! ordered private input bytes and stream-copy invocation.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fmn::prelude::*;
use fmn_platform::fs::{FileSystem, FsError, FsNodeKind, VirtualFs};
use fmn_platform::process::{FfmpegExecutable, FfmpegLocator, FfmpegLocatorError, NoProcessRunner};

static NEXT_DESTINATION: AtomicUsize = AtomicUsize::new(0);

fn destination(label: &str) -> PathBuf {
    let id = NEXT_DESTINATION.fetch_add(1, Ordering::SeqCst);
    let timestamp_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "fmn-concat-{label}-{}-{timestamp_nanos}-{id}.mp4",
        std::process::id(),
    ))
}

struct CountingLocator(Arc<AtomicUsize>);

impl FfmpegLocator for CountingLocator {
    fn locate_ffmpeg(&self, _: &Path) -> Result<FfmpegExecutable, FfmpegLocatorError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(FfmpegLocatorError::NotFound)
    }
}

fn options(label: &str) -> (VideoConcatOptions, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut request = VideoConcatOptions::new(destination(label));
    request.ffmpeg = Some(FfmpegCapability {
        runner: Arc::new(NoProcessRunner),
        locator: Arc::new(CountingLocator(calls.clone())),
        workdir_root: PathBuf::from("/unused-concat-workdir"),
    });
    (request, calls)
}

/// Proves that the source capability only receives bounded reads. Metadata,
/// unbounded reads and output writes through this capability are forbidden.
#[derive(Default)]
struct SourceFs {
    inner: VirtualFs,
    reads: Mutex<Vec<(PathBuf, usize)>>,
    violate_bound: bool,
}

impl FileSystem for SourceFs {
    fn read_bounded(&self, path: &Path, max_bytes: usize) -> Result<Vec<u8>, FsError> {
        self.reads.lock().unwrap().push((path.into(), max_bytes));
        if self.violate_bound {
            self.inner.read(path)
        } else {
            self.inner.read_bounded(path, max_bytes)
        }
    }

    fn read(&self, _: &Path) -> Result<Vec<u8>, FsError> {
        panic!("unbounded source read")
    }

    fn node_kind_no_follow(&self, _: &Path) -> Result<Option<FsNodeKind>, FsError> {
        panic!("source metadata read")
    }

    fn create_dir(&self, _: &Path) -> Result<bool, FsError> {
        panic!("source directory write")
    }

    fn write_atomic(&self, _: &Path, _: &[u8]) -> Result<(), FsError> {
        panic!("source output write")
    }

    fn create_new(&self, _: &Path, _: &[u8]) -> Result<bool, FsError> {
        panic!("source output creation")
    }

    fn remove_file(&self, _: &Path) -> Result<(), FsError> {
        panic!("source removal")
    }

    fn remove_dir_all(&self, _: &Path) -> Result<(), FsError> {
        panic!("source directory removal")
    }

    fn exists(&self, _: &Path) -> bool {
        panic!("source existence read")
    }

    fn list_dir(&self, _: &Path) -> Result<Vec<PathBuf>, FsError> {
        panic!("source directory read")
    }

    fn count_dir_entries_bounded(&self, _: &Path, _: usize) -> Result<usize, FsError> {
        panic!("source directory count")
    }
}

#[test]
fn invalid_policy_and_certification_fail_before_source_reads_or_tool_discovery() {
    let (valid, calls) = options("policy");
    let source = Arc::new(SourceFs::default());
    let inputs = [PathBuf::from("/scene.mp4")];
    let mut cases = Vec::new();

    let mut request = valid.clone();
    request.max_inputs = 0;
    cases.push(request);
    let mut request = valid.clone();
    request.max_inputs = fmn_output::ffmpeg::MAX_CONCAT_INPUTS + 1;
    cases.push(request);
    let mut request = valid.clone();
    request.max_input_bytes = 0;
    cases.push(request);
    let mut request = valid.clone();
    request.max_total_input_bytes = 0;
    cases.push(request);
    let mut request = valid.clone();
    request.max_input_bytes = u64::MAX;
    cases.push(request);
    let mut request = valid.clone();
    request.max_total_input_bytes = u64::MAX;
    cases.push(request);
    let mut request = valid.clone();
    request.ffmpeg_limits.timeout = Duration::ZERO;
    cases.push(request);
    let mut request = valid.clone();
    request.ffmpeg_limits.timeout = Duration::MAX;
    cases.push(request);
    let mut request = valid.clone();
    request.ffmpeg_limits.max_log_bytes = 0;
    cases.push(request);
    let mut request = valid.clone();
    request.ffmpeg_limits.max_artifact_bytes = 0;
    cases.push(request);
    let mut request = valid.clone();
    request.output.set_extension("gif");
    cases.push(request);

    for request in cases {
        let error = concatenate_video_with_fs(&inputs, request, source.clone()).unwrap_err();
        assert!(
            matches!(error, VideoConcatError::InvalidOptions(_)),
            "{error}"
        );
    }
    let error = concatenate_video_with_fs(&[], valid.clone(), source.clone()).unwrap_err();
    assert!(matches!(error, VideoConcatError::InvalidOptions(_)));

    let mut request = valid.clone();
    request.max_inputs = 1;
    let error = concatenate_video_with_fs(
        &[inputs[0].clone(), inputs[0].clone()],
        request,
        source.clone(),
    )
    .unwrap_err();
    assert!(matches!(error, VideoConcatError::InvalidOptions(_)));

    let mut request = valid;
    request.determinism = DeterminismMode::Certified;
    let error = concatenate_video_with_fs(&inputs, request, source.clone()).unwrap_err();
    assert!(matches!(error, VideoConcatError::Capability(_)));
    assert!(
        error
            .to_string()
            .contains("outside the certified artifact set")
    );
    assert!(source.reads.lock().unwrap().is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn explicit_source_entry_point_never_infers_host_process_authority() {
    let source = Arc::new(SourceFs::default());
    let request = VideoConcatOptions::new(destination("no-capability"));
    let error = concatenate_video_with_fs(&[PathBuf::from("/scene.mp4")], request, source.clone())
        .unwrap_err();
    assert!(matches!(error, VideoConcatError::Capability(_)));
    assert!(error.to_string().contains("FfmpegCapability"));
    assert!(source.reads.lock().unwrap().is_empty());
}

#[test]
fn every_position_is_read_once_with_the_remaining_inclusive_aggregate_bound() {
    let source = Arc::new(SourceFs::default());
    source.inner.insert("/A.mp4", b"AAA".to_vec());
    source.inner.insert("/B.mp4", b"BBBBB".to_vec());
    let inputs = ["/A.mp4", "/B.mp4", "/A.mp4"].map(PathBuf::from);
    let (mut request, calls) = options("inclusive");
    request.max_input_bytes = 8;
    request.max_total_input_bytes = 11;
    let output = request.output.clone();

    // The locator is deliberately the first failing operation. Reaching it
    // proves all bounded source reads admitted the inclusive byte budgets.
    let error = concatenate_video_with_fs(&inputs, request, source.clone()).unwrap_err();
    assert!(matches!(
        error,
        VideoConcatError::FfmpegUnavailable(FfmpegLocatorError::NotFound),
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        *source.reads.lock().unwrap(),
        [
            (PathBuf::from("/A.mp4"), 8),
            (PathBuf::from("/B.mp4"), 8),
            (PathBuf::from("/A.mp4"), 3),
        ],
    );
    assert!(!output.exists());
}

#[test]
fn repeated_clips_consume_the_aggregate_budget_and_failure_never_discovers_a_tool() {
    let source = Arc::new(SourceFs::default());
    source.inner.insert("/A.mp4", b"AAA".to_vec());
    source.inner.insert("/B.mp4", b"BBBBB".to_vec());
    let inputs = ["/A.mp4", "/B.mp4", "/A.mp4"].map(PathBuf::from);
    let (mut request, calls) = options("aggregate");
    request.max_input_bytes = 8;
    request.max_total_input_bytes = 10;
    let error = concatenate_video_with_fs(&inputs, request, source.clone()).unwrap_err();
    assert!(matches!(
        error,
        VideoConcatError::Input {
            index: 2,
            error: FsError::TooLarge { limit: 2, .. },
        },
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(source.reads.lock().unwrap().len(), 3);

    // Exhaustion at an exact boundary still refuses a following nonempty
    // clip, without relaxing its read to an unlimited sentinel.
    let (mut request, calls) = options("exhausted");
    request.max_input_bytes = 3;
    request.max_total_input_bytes = 3;
    let error = concatenate_video_with_fs(
        &[inputs[0].clone(), inputs[0].clone()],
        request,
        source.clone(),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        VideoConcatError::Input {
            index: 1,
            error: FsError::TooLarge { limit: 0, .. },
        },
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(source.reads.lock().unwrap().last().unwrap().1, 0);
}

#[test]
fn missing_empty_and_oversized_sources_are_typed_before_tool_discovery() {
    let source = Arc::new(SourceFs::default());
    source.inner.insert("/empty.mp4", Vec::new());
    source.inner.insert("/large.mp4", b"1234".to_vec());
    for (path, kind) in [("/missing.mp4", 0), ("/empty.mp4", 1), ("/large.mp4", 2)] {
        let (mut request, calls) = options("source-refusal");
        request.max_input_bytes = 3;
        let error =
            concatenate_video_with_fs(&[PathBuf::from(path)], request, source.clone()).unwrap_err();
        match kind {
            0 => assert!(matches!(
                error,
                VideoConcatError::Input {
                    index: 0,
                    error: FsError::NotFound { .. }
                },
            )),
            1 => assert!(matches!(
                error,
                VideoConcatError::EmptyInput { index: 0, .. }
            )),
            _ => assert!(matches!(
                error,
                VideoConcatError::Input {
                    index: 0,
                    error: FsError::TooLarge { limit: 3, .. }
                },
            )),
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    let source = Arc::new(SourceFs {
        violate_bound: true,
        ..SourceFs::default()
    });
    source.inner.insert("/untrusted.mp4", b"1234".to_vec());
    let (mut request, calls) = options("broken-fs-bound");
    request.max_input_bytes = 3;
    let error =
        concatenate_video_with_fs(&[PathBuf::from("/untrusted.mp4")], request, source).unwrap_err();
    assert!(matches!(
        error,
        VideoConcatError::Input {
            index: 0,
            error: FsError::TooLarge { limit: 3, .. }
        },
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn existing_output_is_preserved_before_source_reads_or_tool_discovery() {
    let source = Arc::new(SourceFs::default());
    for directory in [false, true] {
        let (request, calls) = options("existing");
        if directory {
            std::fs::create_dir(&request.output).unwrap();
        } else {
            std::fs::write(&request.output, b"existing movie").unwrap();
        }
        let output = request.output.clone();
        let error =
            concatenate_video_with_fs(&[PathBuf::from("/scene.mp4")], request, source.clone())
                .unwrap_err();
        assert!(matches!(error, VideoConcatError::DestinationExists(path) if path == output));
        if directory {
            assert!(output.is_dir());
        } else {
            assert_eq!(std::fs::read(output).unwrap(), b"existing movie");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
    assert!(source.reads.lock().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn dangling_output_link_is_a_conflict_even_when_its_target_is_absent() {
    let source = Arc::new(SourceFs::default());
    let (request, calls) = options("dangling-link");
    let target = destination("absent-target");
    std::os::unix::fs::symlink(&target, &request.output).unwrap();
    let output = request.output.clone();
    let error = concatenate_video_with_fs(&[PathBuf::from("/scene.mp4")], request, source.clone())
        .unwrap_err();
    assert!(matches!(error, VideoConcatError::DestinationExists(path) if path == output));
    assert_eq!(std::fs::read_link(output).unwrap(), target);
    assert!(!target.exists());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(source.reads.lock().unwrap().is_empty());
}
