//! fm-s1o.2 acceptance: production FrameSink adapters, bounded buffering,
//! cancellation-safe publication, native determinism, and fake-ffmpeg
//! provenance.

use fmn_codec::{
    CompressionLevel, PngLimits, SampleFormat, WavAudio, WavLimits, Y4mColorspace, Y4mWriter,
    decode_png, decode_wav, decode_y4m,
};
use fmn_frame::{FrameBuffer, FrameLayout, PixelFormat};
use fmn_hash::{Sha256, sha256};
#[cfg(unix)]
use fmn_output::{
    ColorDescription, Container, EncoderCapabilities, EncoderChoice, FfmpegSink, FfmpegSinkConfig,
    FfmpegTool, JobLimits, VideoJob, WireFormat,
};
use fmn_output::{
    DitherPolicy, EmitterConfig, FrameSink, GifSink, GifSinkConfig, MixKernel, MixReport,
    NativeArtifactKind, OrderedEmitter, OutputProfile, PngSink, PngSinkConfig, PngTarget,
    ReceiptError, SinkAdapterError, SinkLimits, SinkMode, SinkWrite, WavPublicationConfig, Y4mSink,
    Y4mSinkConfig, publish_wav,
};
use fmn_platform::clock::FakeClock;
use fmn_platform::fs::{
    ATOMIC_DIRECTORY_COMPLETE_LEAF, AtomicDirectoryWriter, AtomicFileWriter, FileSystem, FsError,
    FsNodeKind, PreparedAtomicDirectory, PreparedAtomicFile, VirtualFs,
};
#[cfg(unix)]
use fmn_platform::process::{
    ProcessCancellation, ProcessError, ProcessMechanism, ProcessOutcome, ProcessRunner,
    ProcessSpec, ProcessStdinLimits, ProcessTermination, RunningProcess,
};
use fmn_platform::profile::{ProfilePath, ProfileRecorder};
#[cfg(unix)]
use std::io::Write as _;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
#[cfg(unix)]
use std::time::Duration;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn exact_limits(frames: u64) -> SinkLimits {
    SinkLimits::new(16, 1 << 20, 1 << 20, 1 << 20)
        .expect("valid limits")
        .requiring_exact_frames(frames)
        .expect("valid exact count")
}

fn padded_frame(format: PixelFormat, seed: u8) -> (FrameBuffer, Vec<u8>) {
    let width = 4;
    let height = 2;
    let layout = FrameLayout::with_row_alignment(format, width, height, 32).expect("padded layout");
    let tight = FrameLayout::tight(format, width, height).expect("tight layout");
    let mut frame = FrameBuffer::new(layout.clone());
    frame.as_bytes_mut().fill(0xee);
    let mut expected = Vec::with_capacity(tight.total_bytes());
    let mut value = seed;
    for plane in 0..format.plane_count() {
        let row_bytes = tight.stride(plane);
        let rows = tight.plane_bytes(plane) / row_bytes;
        let stride = layout.stride(plane);
        let storage = frame.plane_mut(plane);
        for row in 0..rows {
            let payload = &mut storage[row * stride..row * stride + row_bytes];
            for byte in payload {
                *byte = value;
                expected.push(value);
                value = value.wrapping_add(1);
            }
        }
    }
    (frame, expected)
}

fn png_config(
    target: PngTarget,
    limits: SinkLimits,
    compression: CompressionLevel,
    threads: usize,
) -> PngSinkConfig {
    PngSinkConfig {
        target,
        width: 4,
        height: 2,
        first_sequence: 7,
        compression,
        threads,
        limits,
        profile: None,
    }
}

fn write_direct(sink: &mut dyn FrameSink, sequence: u64, frame: &FrameBuffer) {
    assert_eq!(
        sink.write_frame(sequence, frame).expect("sink write"),
        SinkWrite::Consumed
    );
}

#[test]
fn sink_limits_reject_invalid_and_truncated_stream_contracts() {
    assert!(matches!(
        SinkLimits::new(0, 1, 1, 1),
        Err(SinkAdapterError::InvalidConfig(_))
    ));
    assert!(matches!(
        SinkLimits::new(1, 0, 1, 1),
        Err(SinkAdapterError::InvalidConfig(_))
    ));
    assert!(matches!(
        SinkLimits::new(1, 1, 0, 1),
        Err(SinkAdapterError::InvalidConfig(_))
    ));
    assert!(matches!(
        SinkLimits::new(1, 1, 1, 0),
        Err(SinkAdapterError::InvalidConfig(_))
    ));
    let limits = SinkLimits::new(2, 32, 64, 128).expect("limits");
    assert_eq!(limits.exact_frames(), None);
    assert!(matches!(
        limits.requiring_exact_frames(0),
        Err(SinkAdapterError::InvalidConfig(_))
    ));
    assert!(matches!(
        limits.requiring_exact_frames(3),
        Err(SinkAdapterError::InvalidConfig(_))
    ));

    let fs = Arc::new(VirtualFs::new());
    assert!(matches!(
        PngSink::new(
            fs.clone(),
            png_config(
                PngTarget::Single(PathBuf::from("/out/impossible.png")),
                exact_limits(2),
                CompressionLevel::Default,
                1,
            ),
        ),
        Err(SinkAdapterError::InvalidConfig(
            "single PNG exact frame count must be one"
        ))
    ));
    assert!(matches!(
        GifSink::new(
            fs.clone(),
            GifSinkConfig {
                destination: PathBuf::from("/out/impossible.gif"),
                width: u32::from(u16::MAX) + 1,
                height: 1,
                fps: (24, 1),
                loop_forever: false,
                first_sequence: 7,
                limits: exact_limits(1),
                profile: None,
            },
        ),
        Err(SinkAdapterError::InvalidConfig(
            "GIF dimensions exceed the 16-bit format limit"
        ))
    ));
    assert!(matches!(
        Y4mSink::new(
            fs.clone(),
            Y4mSinkConfig {
                destination: PathBuf::from("/out/odd.y4m"),
                width: 3,
                height: 2,
                fps: (24, 1),
                colorspace: Y4mColorspace::C420Mpeg2,
                first_sequence: 7,
                limits: exact_limits(1),
                profile: None,
            },
        ),
        Err(SinkAdapterError::InvalidGeometry { .. })
    ));

    let destination = PathBuf::from("/out/truncated_7.png");
    let mut sink = PngSink::new(
        fs.clone(),
        png_config(
            PngTarget::Sequence {
                directory: PathBuf::from("/out"),
                stem: "truncated".to_string(),
                digits: 1,
            },
            exact_limits(2),
            CompressionLevel::Default,
            1,
        ),
    )
    .expect("sink");
    let receipt = sink.receipt();
    let (frame, _) = padded_frame(PixelFormat::Rgba8, 1);
    write_direct(&mut sink, 7, &frame);
    let error = sink.finish().expect_err("truncated stream refused");
    assert!(error.message().contains("expected exactly 2 frames"));
    assert!(matches!(receipt.take(), Err(ReceiptError::Failed(_))));
    assert!(!fs.exists(&destination));
}

#[test]
fn canonical_png_sink_strips_padding_profiles_and_publishes_on_finish() {
    let fs = Arc::new(VirtualFs::new());
    let destination = PathBuf::from("/render/frame.png");
    let clock = Arc::new(FakeClock::new());
    let recorder = ProfileRecorder::enabled();
    let profile = OutputProfile::new(clock, recorder.clone(), ProfilePath::scene(3).with_play(5));
    let mut config = png_config(
        PngTarget::Single(destination.clone()),
        exact_limits(1),
        CompressionLevel::Best,
        4,
    );
    config.profile = Some(profile);
    let sink = PngSink::new(fs.clone(), config).expect("PNG sink");
    let (binding, receipt) = sink.into_binding("canonical-png");
    assert_eq!(binding.mode(), SinkMode::Reliable);
    assert_eq!(binding.name(), "canonical-png");
    let (frame, tight) = padded_frame(PixelFormat::Rgba8, 9);
    let emitter = OrderedEmitter::new(
        EmitterConfig::new(frame.layout().clone(), 1, 7).expect("emitter config"),
        vec![binding],
    )
    .expect("emitter");
    let mut reservation = emitter.reserve(7).expect("reservation");
    reservation
        .frame_mut()
        .as_bytes_mut()
        .copy_from_slice(frame.as_bytes());
    reservation.publish().expect("publish");
    assert!(!fs.exists(&destination));
    emitter.finish().expect("finish");

    let bytes = fs.read(&destination).expect("published PNG");
    let decoded = decode_png(&bytes, &PngLimits::default()).expect("decode PNG");
    assert_eq!((decoded.width, decoded.height), (4, 2));
    assert_eq!(decoded.rgba, tight);
    let report = receipt.take().expect("completion report");
    assert_eq!(report.kind, NativeArtifactKind::Png);
    assert_eq!(report.path, destination);
    assert_eq!(report.frame_count, 1);
    assert_eq!(report.bytes, bytes.len() as u64);
    assert_eq!(report.digest, sha256(&bytes));

    let ndjson = recorder.snapshot().to_ndjson();
    assert!(ndjson.contains("\"phase\":\"emit\""), "{ndjson}");
    assert!(ndjson.contains("\"phase\":\"encode\""), "{ndjson}");
    assert!(!ndjson.contains("\"phase\":\"ffmpeg_feed\""), "{ndjson}");
    assert_eq!(ndjson.lines().count(), 2);
}

fn png_sequence_bytes(level: CompressionLevel, threads: usize) -> Vec<Vec<u8>> {
    let fs = Arc::new(VirtualFs::new());
    let mut sink = PngSink::new(
        fs.clone(),
        png_config(
            PngTarget::Sequence {
                directory: PathBuf::from("/sequence"),
                stem: "shot".to_string(),
                digits: 4,
            },
            exact_limits(3),
            level,
            threads,
        ),
    )
    .expect("sequence sink");
    let receipt = sink.receipt();
    for offset in 0..3 {
        let (frame, _) = padded_frame(PixelFormat::Rgba8, 20 + offset);
        write_direct(&mut sink, 7 + u64::from(offset), &frame);
    }
    sink.finish().expect("sequence finish");
    let report = receipt.take().expect("sequence report");
    assert_eq!(report.kind, NativeArtifactKind::PngSequence);
    assert_eq!(report.frame_count, 3);
    let members = (0..report.frame_count)
        .map(|offset| {
            let path = report.path.join(format!("shot_{:04}.png", 7 + offset));
            fs.read(&path).expect("sequence member")
        })
        .collect::<Vec<_>>();
    let mut tree = Sha256::new();
    tree.update(b"fmn-png-sequence-tree/v1\0");
    for (offset, bytes) in members.iter().enumerate() {
        let leaf = format!("shot_{:04}.png", 7 + offset);
        tree.update(&u64::try_from(leaf.len()).unwrap_or(u64::MAX).to_le_bytes());
        tree.update(leaf.as_bytes());
        tree.update(&u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_le_bytes());
        tree.update(bytes);
    }
    tree.update(&report.frame_count.to_le_bytes());
    assert_eq!(report.digest, tree.finalize());
    members
}

#[test]
fn png_sequence_is_bit_identical_at_one_four_and_sixteen_threads_for_every_quality() {
    for level in [
        CompressionLevel::Fast,
        CompressionLevel::Default,
        CompressionLevel::Best,
    ] {
        let serial = png_sequence_bytes(level, 1);
        assert_eq!(png_sequence_bytes(level, 4), serial);
        assert_eq!(png_sequence_bytes(level, 16), serial);
        for bytes in serial {
            decode_png(&bytes, &PngLimits::default()).expect("canonical member");
        }
    }
}

#[test]
fn large_png_frame_is_bit_identical_when_fixed_segments_run_in_parallel() {
    fn render(threads: usize) -> Vec<u8> {
        let (width, height) = (400, 180);
        let layout =
            FrameLayout::tight(PixelFormat::Rgba8, width, height).expect("large PNG layout");
        let mut frame = FrameBuffer::new(layout);
        let mut state = 99u32;
        for byte in frame.as_bytes_mut() {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            *byte = (state >> 24) as u8;
        }
        let frame_bytes = u64::try_from(frame.as_bytes().len()).expect("frame byte count");
        assert!(frame_bytes > 1 << 18, "fixture must span DEFLATE segments");
        let limits = SinkLimits::new(1, 2 << 20, frame_bytes, 1 << 20)
            .expect("large PNG limits")
            .requiring_exact_frames(1)
            .expect("one frame");
        let fs = Arc::new(VirtualFs::new());
        let destination = PathBuf::from(format!("/large/frame-{threads}.png"));
        let mut sink = PngSink::new(
            fs.clone(),
            PngSinkConfig {
                target: PngTarget::Single(destination.clone()),
                width,
                height,
                first_sequence: 0,
                compression: CompressionLevel::Default,
                threads,
                limits,
                profile: None,
            },
        )
        .expect("large PNG sink");
        write_direct(&mut sink, 0, &frame);
        sink.finish().expect("large PNG finish");
        fs.read(&destination).expect("large PNG bytes")
    }

    let serial = render(1);
    assert_eq!(render(4), serial);
    assert_eq!(render(16), serial);
}

struct FailNthWriteFs {
    inner: Arc<VirtualFs>,
    writes: Arc<AtomicUsize>,
    fail_at: usize,
}

impl FailNthWriteFs {
    fn new(fail_at: usize) -> Self {
        Self {
            inner: Arc::new(VirtualFs::new()),
            writes: Arc::new(AtomicUsize::new(0)),
            fail_at,
        }
    }
}

struct FailNthDirectoryWriter {
    inner: Box<dyn AtomicDirectoryWriter>,
    writes: Arc<AtomicUsize>,
    fail_at: usize,
}

impl AtomicDirectoryWriter for FailNthDirectoryWriter {
    fn write_file(&mut self, leaf: &Path, bytes: &[u8]) -> Result<(), FsError> {
        let write = self.writes.fetch_add(1, Ordering::Relaxed) + 1;
        if write == self.fail_at {
            return Err(FsError::Io {
                path: leaf.to_path_buf(),
                err: std::io::Error::other("injected private-generation failure"),
            });
        }
        self.inner.write_file(leaf, bytes)
    }

    fn prepare(self: Box<Self>) -> Result<Box<dyn PreparedAtomicDirectory>, FsError> {
        self.inner.prepare()
    }
}

impl FileSystem for FailNthWriteFs {
    fn node_kind_no_follow(&self, path: &Path) -> Result<Option<FsNodeKind>, FsError> {
        self.inner.node_kind_no_follow(path)
    }

    fn create_dir(&self, path: &Path) -> Result<bool, FsError> {
        self.inner.create_dir(path)
    }

    fn read(&self, path: &Path) -> Result<Vec<u8>, FsError> {
        self.inner.read(path)
    }

    fn read_bounded(&self, path: &Path, max_bytes: usize) -> Result<Vec<u8>, FsError> {
        self.inner.read_bounded(path, max_bytes)
    }

    fn write_atomic(&self, path: &Path, bytes: &[u8]) -> Result<(), FsError> {
        self.inner.write_atomic(path, bytes)
    }

    fn begin_atomic_directory(
        self: Arc<Self>,
        path: &Path,
    ) -> Result<Box<dyn AtomicDirectoryWriter>, FsError> {
        let inner = self.inner.clone().begin_atomic_directory(path)?;
        Ok(Box::new(FailNthDirectoryWriter {
            inner,
            writes: Arc::clone(&self.writes),
            fail_at: self.fail_at,
        }))
    }

    fn create_new(&self, path: &Path, bytes: &[u8]) -> Result<bool, FsError> {
        self.inner.create_new(path, bytes)
    }

    fn remove_file(&self, path: &Path) -> Result<(), FsError> {
        self.inner.remove_file(path)
    }

    fn remove_dir_all(&self, path: &Path) -> Result<(), FsError> {
        self.inner.remove_dir_all(path)
    }

    fn exists(&self, path: &Path) -> bool {
        self.inner.exists(path)
    }

    fn list_dir(&self, path: &Path) -> Result<Vec<PathBuf>, FsError> {
        self.inner.list_dir(path)
    }

    fn count_dir_entries_bounded(&self, path: &Path, max_entries: usize) -> Result<usize, FsError> {
        self.inner.count_dir_entries_bounded(path, max_entries)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct AtomicWriteStats {
    calls: u64,
    max_chunk: usize,
    total: u64,
    committed: bool,
}

struct ChunkProbeFs {
    inner: Arc<VirtualFs>,
    stats: Arc<Mutex<AtomicWriteStats>>,
}

impl ChunkProbeFs {
    fn new() -> Self {
        Self {
            inner: Arc::new(VirtualFs::new()),
            stats: Arc::new(Mutex::new(AtomicWriteStats::default())),
        }
    }

    fn stats(&self) -> AtomicWriteStats {
        *lock(&self.stats)
    }
}

struct ChunkProbeWriter {
    inner: Box<dyn AtomicFileWriter>,
    stats: Arc<Mutex<AtomicWriteStats>>,
}

impl AtomicFileWriter for ChunkProbeWriter {
    fn write(&mut self, bytes: &[u8]) -> Result<(), FsError> {
        let mut stats = lock(&self.stats);
        stats.calls += 1;
        stats.max_chunk = stats.max_chunk.max(bytes.len());
        stats.total = stats
            .total
            .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        drop(stats);
        self.inner.write(bytes)
    }

    fn prepare(self: Box<Self>) -> Result<Box<dyn PreparedAtomicFile>, FsError> {
        let Self { inner, stats } = *self;
        Ok(Box::new(ChunkProbePrepared {
            inner: inner.prepare()?,
            stats,
        }))
    }
}

struct ChunkProbePrepared {
    inner: Box<dyn PreparedAtomicFile>,
    stats: Arc<Mutex<AtomicWriteStats>>,
}

impl PreparedAtomicFile for ChunkProbePrepared {
    fn commit(self: Box<Self>) -> Result<(), FsError> {
        let Self { inner, stats } = *self;
        inner.commit()?;
        lock(&stats).committed = true;
        Ok(())
    }
}

impl FileSystem for ChunkProbeFs {
    fn node_kind_no_follow(&self, path: &Path) -> Result<Option<FsNodeKind>, FsError> {
        self.inner.node_kind_no_follow(path)
    }

    fn create_dir(&self, path: &Path) -> Result<bool, FsError> {
        self.inner.create_dir(path)
    }

    fn read(&self, path: &Path) -> Result<Vec<u8>, FsError> {
        self.inner.read(path)
    }

    fn read_bounded(&self, path: &Path, max_bytes: usize) -> Result<Vec<u8>, FsError> {
        self.inner.read_bounded(path, max_bytes)
    }

    fn write_atomic(&self, path: &Path, bytes: &[u8]) -> Result<(), FsError> {
        self.inner.write_atomic(path, bytes)
    }

    fn begin_atomic_file(
        self: Arc<Self>,
        path: &Path,
    ) -> Result<Box<dyn AtomicFileWriter>, FsError> {
        Ok(Box::new(ChunkProbeWriter {
            inner: self.inner.clone().begin_atomic_file(path)?,
            stats: Arc::clone(&self.stats),
        }))
    }

    fn create_new(&self, path: &Path, bytes: &[u8]) -> Result<bool, FsError> {
        self.inner.create_new(path, bytes)
    }

    fn remove_file(&self, path: &Path) -> Result<(), FsError> {
        self.inner.remove_file(path)
    }

    fn remove_dir_all(&self, path: &Path) -> Result<(), FsError> {
        self.inner.remove_dir_all(path)
    }

    fn exists(&self, path: &Path) -> bool {
        self.inner.exists(path)
    }

    fn list_dir(&self, path: &Path) -> Result<Vec<PathBuf>, FsError> {
        self.inner.list_dir(path)
    }

    fn count_dir_entries_bounded(&self, path: &Path, max_entries: usize) -> Result<usize, FsError> {
        self.inner.count_dir_entries_bounded(path, max_entries)
    }
}

#[test]
fn png_sequence_private_failure_never_exposes_a_partial_generation() -> Result<(), &'static str> {
    let fs = Arc::new(FailNthWriteFs::new(2));
    let mut sink = PngSink::new(
        fs.clone(),
        png_config(
            PngTarget::Sequence {
                directory: PathBuf::from("/partial"),
                stem: "frame".to_string(),
                digits: 2,
            },
            exact_limits(3),
            CompressionLevel::Default,
            2,
        ),
    )
    .expect("sequence");
    let receipt = sink.receipt();
    let (first, _) = padded_frame(PixelFormat::Rgba8, 0);
    write_direct(&mut sink, 7, &first);
    let (second, _) = padded_frame(PixelFormat::Rgba8, 1);
    let error = sink
        .write_frame(8, &second)
        .expect_err("second private child write fails");
    assert_eq!(error.code(), "sink.publish");
    let retry = sink
        .write_frame(8, &second)
        .expect_err("failed lifecycle cannot be retried");
    assert_eq!(retry.code(), "sink.already_finalized");
    assert!(!fs.exists(Path::new("/partial")));
    let ReceiptError::Failed(failure) = receipt.take().expect_err("failed receipt") else {
        return Err("receipt must retain the root failure");
    };
    assert_eq!(failure.code(), "sink.publish");
    Ok(())
}

#[test]
fn png_sequence_generations_are_no_clobber_and_cannot_mix_stale_tails() {
    let fs = Arc::new(VirtualFs::new());
    let directory = PathBuf::from("/generation");
    let target = || PngTarget::Sequence {
        directory: directory.clone(),
        stem: "shot".to_string(),
        digits: 2,
    };

    let mut first = PngSink::new(
        fs.clone(),
        png_config(target(), exact_limits(5), CompressionLevel::Default, 1),
    )
    .expect("first generation");
    for offset in 0..5 {
        let (frame, _) = padded_frame(PixelFormat::Rgba8, offset);
        write_direct(&mut first, 7 + u64::from(offset), &frame);
    }
    first.finish().expect("publish first generation");
    let published = fs.list_dir(&directory).expect("first generation inventory");
    assert!(published.contains(&directory.join(ATOMIC_DIRECTORY_COMPLETE_LEAF)));
    let original = published
        .iter()
        .filter(|path| {
            path.file_name()
                .is_some_and(|leaf| leaf != ATOMIC_DIRECTORY_COMPLETE_LEAF)
        })
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(original.len(), 5);
    let original_bytes = original
        .iter()
        .map(|path| fs.read(path).expect("first generation child"))
        .collect::<Vec<_>>();

    let mut contender = PngSink::new(
        fs.clone(),
        png_config(target(), exact_limits(3), CompressionLevel::Best, 4),
    )
    .expect("competing generation");
    let receipt = contender.receipt();
    for offset in 0..3 {
        let (frame, _) = padded_frame(PixelFormat::Rgba8, 100 + offset);
        write_direct(&mut contender, 7 + u64::from(offset), &frame);
    }
    let error = contender
        .finish()
        .expect_err("existing generation must win");
    assert_eq!(error.code(), "sink.publish");
    assert!(matches!(receipt.take(), Err(ReceiptError::Failed(_))));
    assert_eq!(
        fs.list_dir(&directory).expect("stable inventory"),
        published
    );
    for (path, bytes) in original.iter().zip(original_bytes) {
        assert_eq!(
            fs.read(path).expect("stable generation child"),
            bytes,
            "contender changed {}",
            path.display()
        );
    }
}

#[test]
fn png_sequence_stems_are_cross_platform_safe_leaf_names() {
    let fs = Arc::new(VirtualFs::new());
    for stem in [
        "",
        "C:",
        "bad:name",
        "bad/name",
        "bad\\name",
        "trailing.",
        "trailing ",
        "_leading",
        "CON",
        "con",
        "COM1",
        "LPT9",
    ] {
        let result = PngSink::new(
            fs.clone(),
            png_config(
                PngTarget::Sequence {
                    directory: PathBuf::from("/safe"),
                    stem: stem.to_string(),
                    digits: 4,
                },
                exact_limits(1),
                CompressionLevel::Default,
                1,
            ),
        );
        assert!(
            matches!(result, Err(SinkAdapterError::InvalidConfig(_))),
            "unsafe stem was accepted: {stem:?}"
        );
    }
    assert!(
        PngSink::new(
            fs,
            png_config(
                PngTarget::Sequence {
                    directory: PathBuf::from("/safe"),
                    stem: "Scene-01_frame".to_string(),
                    digits: 4,
                },
                exact_limits(1),
                CompressionLevel::Default,
                1,
            ),
        )
        .is_ok()
    );
}

#[test]
fn gif_sink_is_deterministic_bounded_and_reports_its_artifact() {
    fn render() -> (Vec<u8>, u64) {
        let fs = Arc::new(VirtualFs::new());
        let destination = PathBuf::from("/render/clip.gif");
        let mut sink = GifSink::new(
            fs.clone(),
            GifSinkConfig {
                destination: destination.clone(),
                width: 4,
                height: 2,
                fps: (30_000, 1_001),
                loop_forever: true,
                first_sequence: 7,
                limits: exact_limits(2),
                profile: None,
            },
        )
        .expect("GIF sink");
        let receipt = sink.receipt();
        for offset in 0..2 {
            let (frame, _) = padded_frame(PixelFormat::Rgba8, 70 + offset);
            write_direct(&mut sink, 7 + u64::from(offset), &frame);
        }
        assert!(!fs.exists(&destination));
        sink.finish().expect("GIF finish");
        assert!(
            sink.finish()
                .expect_err("double finish")
                .message()
                .contains("already finalized")
        );
        let bytes = fs.read(&destination).expect("GIF bytes");
        let report = receipt.take().expect("GIF report");
        assert_eq!(report.kind, NativeArtifactKind::Gif);
        assert_eq!(report.frame_count, 2);
        assert_eq!(report.bytes, bytes.len() as u64);
        assert_eq!(report.digest, sha256(&bytes));
        (bytes, report.bytes)
    }

    let (first, first_len) = render();
    let (second, second_len) = render();
    assert_eq!(first, second);
    assert_eq!(first_len, second_len);
    assert!(first.starts_with(b"GIF89a"));
    assert_eq!(first.last(), Some(&0x3b));
}

#[test]
fn y4m_sink_strips_nv12_padding_and_preserves_exact_timeline() {
    let fs = Arc::new(VirtualFs::new());
    let destination = PathBuf::from("/render/clip.y4m");
    let mut sink = Y4mSink::new(
        fs.clone(),
        Y4mSinkConfig {
            destination: destination.clone(),
            width: 4,
            height: 2,
            fps: (24, 1),
            colorspace: Y4mColorspace::C420Mpeg2,
            first_sequence: 7,
            limits: exact_limits(1),
            profile: None,
        },
    )
    .expect("y4m sink");
    let receipt = sink.receipt();
    let layout = FrameLayout::with_strides(PixelFormat::Nv12, 4, 2, &[8, 8]).expect("padded NV12");
    let mut frame = FrameBuffer::new(layout);
    frame.as_bytes_mut().fill(0xee);
    frame.plane_mut(0)[..4].copy_from_slice(&[1, 2, 3, 4]);
    frame.plane_mut(0)[8..12].copy_from_slice(&[5, 6, 7, 8]);
    frame.plane_mut(1)[..4].copy_from_slice(&[10, 20, 11, 21]);
    write_direct(&mut sink, 7, &frame);
    sink.finish().expect("y4m finish");

    let bytes = fs.read(&destination).expect("y4m bytes");
    let mut expected = Y4mWriter::new(4, 2, (24, 1), Y4mColorspace::C420Mpeg2);
    expected
        .write_frame_nv12(&frame)
        .expect("reference y4m writer");
    assert_eq!(bytes, expected.finish());
    let decoded = decode_y4m(&bytes).expect("decode y4m");
    assert_eq!((decoded.width, decoded.height), (4, 2));
    assert_eq!(decoded.fps, (24, 1));
    assert_eq!(decoded.colorspace, "C420mpeg2");
    assert_eq!(
        decoded.frames,
        vec![vec![1, 2, 3, 4, 5, 6, 7, 8, 10, 11, 20, 21]]
    );
    let report = receipt.take().expect("y4m report");
    assert_eq!(report.kind, NativeArtifactKind::Y4m);
    assert_eq!(report.frame_count, 1);
    assert_eq!(report.bytes, bytes.len() as u64);
    assert_eq!(report.digest, sha256(&bytes));
}

#[test]
fn gif_and_y4m_stream_many_frames_as_bounded_chunks() {
    const FRAMES: u64 = 256;

    let gif_fs = Arc::new(ChunkProbeFs::new());
    let gif_limits = SinkLimits::new(FRAMES, 1 << 20, FRAMES * 32, 1 << 20)
        .expect("GIF limits")
        .requiring_exact_frames(FRAMES)
        .expect("GIF exact frames");
    let mut gif = GifSink::new(
        gif_fs.clone(),
        GifSinkConfig {
            destination: PathBuf::from("/stream/many.gif"),
            width: 4,
            height: 2,
            fps: (24, 1),
            loop_forever: true,
            first_sequence: 0,
            limits: gif_limits,
            profile: None,
        },
    )
    .expect("GIF stream");
    let gif_receipt = gif.receipt();
    let (rgba, _) = padded_frame(PixelFormat::Rgba8, 5);
    for sequence in 0..FRAMES {
        write_direct(&mut gif, sequence, &rgba);
    }
    gif.finish().expect("GIF finish");
    let gif_report = gif_receipt.take().expect("GIF report");
    let gif_stats = gif_fs.stats();
    assert!(gif_stats.committed);
    assert_eq!(gif_stats.total, gif_report.bytes);
    assert!(gif_stats.calls >= FRAMES + 2);
    assert!(
        u64::try_from(gif_stats.max_chunk).unwrap_or(u64::MAX) < gif_stats.total,
        "GIF collapsed the render into one whole-output write: {gif_stats:?}"
    );

    let y4m_fs = Arc::new(ChunkProbeFs::new());
    let y4m_limits = SinkLimits::new(FRAMES, 1 << 20, FRAMES * 12, 1 << 20)
        .expect("y4m limits")
        .requiring_exact_frames(FRAMES)
        .expect("y4m exact frames");
    let mut y4m = Y4mSink::new(
        y4m_fs.clone(),
        Y4mSinkConfig {
            destination: PathBuf::from("/stream/many.y4m"),
            width: 4,
            height: 2,
            fps: (24, 1),
            colorspace: Y4mColorspace::C420Mpeg2,
            first_sequence: 0,
            limits: y4m_limits,
            profile: None,
        },
    )
    .expect("y4m stream");
    let y4m_receipt = y4m.receipt();
    let (nv12, _) = padded_frame(PixelFormat::Nv12, 9);
    for sequence in 0..FRAMES {
        write_direct(&mut y4m, sequence, &nv12);
    }
    y4m.finish().expect("y4m finish");
    let y4m_report = y4m_receipt.take().expect("y4m report");
    let y4m_stats = y4m_fs.stats();
    assert!(y4m_stats.committed);
    assert_eq!(y4m_stats.total, y4m_report.bytes);
    assert!(y4m_stats.calls > FRAMES);
    assert!(
        u64::try_from(y4m_stats.max_chunk).unwrap_or(u64::MAX) < y4m_stats.total,
        "y4m collapsed the render into one whole-output write: {y4m_stats:?}"
    );
}

#[test]
fn native_sink_abort_never_replaces_existing_destinations() {
    let (rgba, _) = padded_frame(PixelFormat::Rgba8, 4);

    let png_fs = Arc::new(VirtualFs::new());
    let png_path = PathBuf::from("/cancel/frame.png");
    png_fs.insert(&png_path, b"old-png".to_vec());
    let mut png = PngSink::new(
        png_fs.clone(),
        png_config(
            PngTarget::Single(png_path.clone()),
            exact_limits(1),
            CompressionLevel::Default,
            1,
        ),
    )
    .expect("PNG");
    let png_receipt = png.receipt();
    write_direct(&mut png, 7, &rgba);
    png.abort();
    assert_eq!(png_fs.read(&png_path).expect("old PNG"), b"old-png");
    assert!(matches!(png_receipt.take(), Err(ReceiptError::Aborted)));

    let gif_fs = Arc::new(VirtualFs::new());
    let gif_path = PathBuf::from("/cancel/clip.gif");
    gif_fs.insert(&gif_path, b"old-gif".to_vec());
    let mut gif = GifSink::new(
        gif_fs.clone(),
        GifSinkConfig {
            destination: gif_path.clone(),
            width: 4,
            height: 2,
            fps: (24, 1),
            loop_forever: false,
            first_sequence: 7,
            limits: exact_limits(1),
            profile: None,
        },
    )
    .expect("GIF");
    let gif_receipt = gif.receipt();
    write_direct(&mut gif, 7, &rgba);
    gif.abort();
    assert_eq!(gif_fs.read(&gif_path).expect("old GIF"), b"old-gif");
    assert!(matches!(gif_receipt.take(), Err(ReceiptError::Aborted)));

    let y4m_fs = Arc::new(VirtualFs::new());
    let y4m_path = PathBuf::from("/cancel/clip.y4m");
    y4m_fs.insert(&y4m_path, b"old-y4m".to_vec());
    let mut y4m = Y4mSink::new(
        y4m_fs.clone(),
        Y4mSinkConfig {
            destination: y4m_path.clone(),
            width: 4,
            height: 2,
            fps: (24, 1),
            colorspace: Y4mColorspace::C420Jpeg,
            first_sequence: 7,
            limits: exact_limits(1),
            profile: None,
        },
    )
    .expect("y4m");
    let y4m_receipt = y4m.receipt();
    let (nv12, _) = padded_frame(PixelFormat::Nv12, 6);
    write_direct(&mut y4m, 7, &nv12);
    y4m.abort();
    assert_eq!(y4m_fs.read(&y4m_path).expect("old y4m"), b"old-y4m");
    assert!(matches!(y4m_receipt.take(), Err(ReceiptError::Aborted)));
}

#[test]
fn dropping_an_unfinished_sink_terminalizes_its_receipt_without_publication() {
    let fs = Arc::new(VirtualFs::new());
    let destination = PathBuf::from("/drop/frame.png");
    fs.insert(&destination, b"old-png".to_vec());
    let receipt = {
        let mut sink = PngSink::new(
            fs.clone(),
            png_config(
                PngTarget::Single(destination.clone()),
                exact_limits(1),
                CompressionLevel::Default,
                1,
            ),
        )
        .expect("PNG sink");
        let receipt = sink.receipt();
        let (frame, _) = padded_frame(PixelFormat::Rgba8, 9);
        write_direct(&mut sink, 7, &frame);
        receipt
    };
    assert_eq!(fs.read(&destination).expect("old destination"), b"old-png");
    assert!(matches!(receipt.take(), Err(ReceiptError::Aborted)));
}

#[test]
fn frame_validation_and_resource_refusals_precede_publication() {
    let fs = Arc::new(VirtualFs::new());
    let destination = PathBuf::from("/limits/frame.png");
    fs.insert(&destination, b"old".to_vec());
    let (rgba, _) = padded_frame(PixelFormat::Rgba8, 1);

    let mut wrong_sequence = PngSink::new(
        fs.clone(),
        png_config(
            PngTarget::Single(destination.clone()),
            exact_limits(1),
            CompressionLevel::Default,
            1,
        ),
    )
    .expect("sink");
    let error = wrong_sequence
        .write_frame(8, &rgba)
        .expect_err("sequence gap");
    assert!(error.message().contains("expected frame 7"));

    let (nv12, _) = padded_frame(PixelFormat::Nv12, 1);
    let mut wrong_format = PngSink::new(
        fs.clone(),
        png_config(
            PngTarget::Single(destination.clone()),
            exact_limits(1),
            CompressionLevel::Default,
            1,
        ),
    )
    .expect("sink");
    let error = wrong_format
        .write_frame(7, &nv12)
        .expect_err("format mismatch");
    assert!(error.message().contains("needs Rgba8 4x2"));

    let wrong_size =
        FrameBuffer::new(FrameLayout::tight(PixelFormat::Rgba8, 2, 2).expect("smaller RGBA frame"));
    let mut wrong_dimensions = PngSink::new(
        fs.clone(),
        png_config(
            PngTarget::Single(destination.clone()),
            exact_limits(1),
            CompressionLevel::Default,
            1,
        ),
    )
    .expect("sink");
    let error = wrong_dimensions
        .write_frame(7, &wrong_size)
        .expect_err("dimension mismatch");
    assert!(error.message().contains("got Rgba8 2x2"));

    let mut repeated = PngSink::new(
        fs.clone(),
        png_config(
            PngTarget::Sequence {
                directory: PathBuf::from("/limits"),
                stem: "repeat".to_string(),
                digits: 1,
            },
            exact_limits(2),
            CompressionLevel::Default,
            1,
        ),
    )
    .expect("sequence sink");
    write_direct(&mut repeated, 7, &rgba);
    let error = repeated
        .write_frame(7, &rgba)
        .expect_err("repeated sequence");
    assert!(error.message().contains("expected frame 8"));
    repeated.abort();

    let mut overrun = PngSink::new(
        fs.clone(),
        png_config(
            PngTarget::Single(destination.clone()),
            exact_limits(1),
            CompressionLevel::Default,
            1,
        ),
    )
    .expect("single sink");
    write_direct(&mut overrun, 7, &rgba);
    let error = overrun
        .write_frame(8, &rgba)
        .expect_err("single-frame overrun");
    assert!(error.message().contains("frame count 2 exceeds limit 1"));
    overrun.abort();

    let limits = SinkLimits::new(1, 31, 1 << 20, 1 << 20).expect("small resident budget");
    let error = PngSink::new(
        fs.clone(),
        png_config(
            PngTarget::Single(destination.clone()),
            limits,
            CompressionLevel::Default,
            1,
        ),
    )
    .err()
    .expect("resident budget is refused at construction");
    assert_eq!(
        error,
        SinkAdapterError::ResidentBytesExceeded {
            attempted: 32,
            max: 31,
        }
    );

    let limits = SinkLimits::new(1, 1 << 20, 32, 1)
        .expect("small artifact budget")
        .requiring_exact_frames(1)
        .expect("exact count");
    let mut artifact_bounded = PngSink::new(
        fs.clone(),
        png_config(
            PngTarget::Single(destination.clone()),
            limits,
            CompressionLevel::Default,
            1,
        ),
    )
    .expect("sink");
    let error = artifact_bounded
        .write_frame(7, &rgba)
        .expect_err("artifact budget");
    assert!(error.message().contains("artifact set"));
    assert_eq!(fs.read(&destination).expect("old destination"), b"old");

    let y4m_path = PathBuf::from("/limits/bounded.y4m");
    fs.insert(&y4m_path, b"old-y4m".to_vec());
    let mut y4m = Y4mSink::new(
        fs.clone(),
        Y4mSinkConfig {
            destination: y4m_path.clone(),
            width: 4,
            height: 2,
            fps: (24, 1),
            colorspace: Y4mColorspace::C420Mpeg2,
            first_sequence: 7,
            limits: SinkLimits::new(1, 1 << 20, 1 << 20, 50).expect("y4m artifact budget"),
            profile: None,
        },
    )
    .expect("header fits artifact budget");
    let y4m_receipt = y4m.receipt();
    let error = y4m
        .write_frame(7, &nv12)
        .expect_err("y4m frame exceeds artifact budget");
    assert!(error.message().contains("artifact set 58 bytes"));
    assert_eq!(fs.read(&y4m_path).expect("old y4m"), b"old-y4m");
    assert!(matches!(y4m_receipt.take(), Err(ReceiptError::Failed(_))));
}

fn mix_report() -> MixReport {
    MixReport {
        audio: WavAudio {
            channels: 2,
            sample_rate: 48_000,
            format: SampleFormat::F32,
            samples: vec![0.0, 0.5, -0.5, 1.0],
        },
        clipped_samples: 2,
        cues_mixed: 3,
        workers_used: 1,
        kernel: MixKernel::Scalar,
    }
}

#[test]
fn wav_publication_is_native_atomic_and_preflight_bounded() {
    let fs = VirtualFs::new();
    let destination = PathBuf::from("/render/audio.wav");
    let config = WavPublicationConfig {
        destination: destination.clone(),
        format: SampleFormat::S16,
        dither: DitherPolicy::None,
        max_artifact_bytes: 52,
        profile: None,
    };
    let report = publish_wav(&fs, &config, &mix_report()).expect("WAV publish");
    assert_eq!(report.path, destination);
    assert_eq!(report.bytes, 52);
    assert_eq!(report.sample_frames, 2);
    assert_eq!(report.clipped_samples, 2);
    assert_eq!(report.cues_mixed, 3);
    let bytes = fs.read(&destination).expect("WAV bytes");
    assert_eq!(report.digest, sha256(&bytes));
    let decoded = decode_wav(&bytes, &WavLimits::default()).expect("decode WAV");
    assert_eq!(decoded.channels, 2);
    assert_eq!(decoded.sample_rate, 48_000);
    assert_eq!(decoded.samples.len(), 4);

    fs.insert(&destination, b"old-wav".to_vec());
    let too_small = WavPublicationConfig {
        max_artifact_bytes: 51,
        ..config
    };
    assert!(matches!(
        publish_wav(&fs, &too_small, &mix_report()),
        Err(SinkAdapterError::ArtifactBytesExceeded {
            attempted: 52,
            max: 51
        })
    ));
    assert_eq!(fs.read(&destination).expect("old WAV"), b"old-wav");
}

#[cfg(unix)]
mod ffmpeg_boundary {
    use super::*;

    #[derive(Default)]
    struct PublishingRunner {
        runs: Arc<Mutex<Vec<ProcessSpec>>>,
        stdin_chunks: Arc<Mutex<Vec<usize>>>,
        fail_encode: AtomicBool,
    }

    impl PublishingRunner {
        fn runs(&self) -> Vec<ProcessSpec> {
            lock(&self.runs).clone()
        }

        fn fail_encode(&self) {
            self.fail_encode.store(true, Ordering::Relaxed);
        }

        fn stdin_chunks(&self) -> Vec<usize> {
            lock(&self.stdin_chunks).clone()
        }
    }

    impl ProcessRunner for PublishingRunner {
        fn mechanism(&self) -> ProcessMechanism {
            ProcessMechanism::Scripted
        }

        fn start(
            &self,
            spec: &ProcessSpec,
            cancellation: ProcessCancellation,
            stdin_limits: ProcessStdinLimits,
        ) -> Result<Box<dyn RunningProcess>, ProcessError> {
            Ok(Box::new(PublishingProcess {
                spec: spec.clone(),
                input: Vec::new(),
                runs: Arc::clone(&self.runs),
                stdin_chunks: Arc::clone(&self.stdin_chunks),
                fail_encode: self.fail_encode.load(Ordering::Relaxed),
                cancellation,
                stdin_limits,
                stdin_bytes: 0,
                recorded: false,
            }))
        }
    }

    struct PublishingProcess {
        spec: ProcessSpec,
        input: Vec<u8>,
        runs: Arc<Mutex<Vec<ProcessSpec>>>,
        stdin_chunks: Arc<Mutex<Vec<usize>>>,
        fail_encode: bool,
        cancellation: ProcessCancellation,
        stdin_limits: ProcessStdinLimits,
        stdin_bytes: u64,
        recorded: bool,
    }

    impl PublishingProcess {
        fn record(&mut self) {
            if self.recorded {
                return;
            }
            if !self.input.is_empty() {
                self.spec.stdin = Some(std::mem::take(&mut self.input));
            }
            lock(&self.runs).push(self.spec.clone());
            self.recorded = true;
        }

        fn outcome(&mut self) -> Result<ProcessOutcome, ProcessError> {
            let is_version = self.spec.argv == ["-version"];
            let is_encode = !self.input.is_empty();
            if self.cancellation.is_cancelled() {
                self.record();
                return Ok(ProcessOutcome {
                    termination: ProcessTermination::Cancelled,
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                });
            }
            if is_version {
                self.record();
                return Ok(ProcessOutcome {
                    termination: ProcessTermination::Exited(Some(0)),
                    stdout: b"ffmpeg version fm-s1o.2-fake\n".to_vec(),
                    stderr: Vec::new(),
                });
            }
            if self.fail_encode && is_encode {
                self.record();
                return Ok(ProcessOutcome {
                    termination: ProcessTermination::Exited(Some(9)),
                    stdout: Vec::new(),
                    stderr: b"deliberate fake encode failure".to_vec(),
                });
            }
            let artifact =
                self.spec
                    .argv
                    .last()
                    .map(PathBuf::from)
                    .ok_or_else(|| ProcessError::Plumbing {
                        program: self.spec.program.clone(),
                        detail: "fake ffmpeg received no output argument".to_string(),
                    })?;
            let bytes: &[u8] = if is_encode {
                b"video-artifact"
            } else {
                b"muxed-artifact"
            };
            std::fs::write(&artifact, bytes).map_err(|error| ProcessError::Plumbing {
                program: self.spec.program.clone(),
                detail: error.to_string(),
            })?;
            self.record();
            Ok(ProcessOutcome {
                termination: ProcessTermination::Exited(Some(0)),
                stdout: Vec::new(),
                stderr: b"fake-ffmpeg-log".to_vec(),
            })
        }
    }

    impl RunningProcess for PublishingProcess {
        fn write_stdin(&mut self, bytes: &[u8]) -> Result<(), ProcessError> {
            if self.cancellation.is_cancelled() {
                return Err(ProcessError::Plumbing {
                    program: self.spec.program.clone(),
                    detail: "fake ffmpeg was cancelled".to_string(),
                });
            }
            let chunk = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
            if chunk > self.stdin_limits.max_chunk_bytes {
                return Err(ProcessError::StdinChunkLimit {
                    program: self.spec.program.clone(),
                    attempted: chunk,
                    max: self.stdin_limits.max_chunk_bytes,
                });
            }
            let attempted =
                self.stdin_bytes
                    .checked_add(chunk)
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
            lock(&self.stdin_chunks).push(bytes.len());
            self.input.extend_from_slice(bytes);
            self.stdin_bytes = attempted;
            Ok(())
        }

        fn finish(mut self: Box<Self>) -> Result<ProcessOutcome, ProcessError> {
            self.outcome()
        }

        fn cancel(mut self: Box<Self>) -> Result<(), ProcessError> {
            self.cancellation.cancel();
            self.record();
            Ok(())
        }
    }

    impl Drop for PublishingProcess {
        fn drop(&mut self) {
            self.record();
        }
    }

    static SCRATCH_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn native_tool_bytes() -> Vec<u8> {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            const HEADER_BYTES: usize = 64;
            const PROGRAM_BYTES: usize = 56;

            let mut bytes = vec![0_u8; HEADER_BYTES + PROGRAM_BYTES];
            bytes[..4].copy_from_slice(b"\x7fELF");
            bytes[4] = 2;
            bytes[5] = 1;
            bytes[6] = 1;
            bytes[16..18].copy_from_slice(&3_u16.to_le_bytes());
            let machine = if cfg!(target_arch = "x86_64") {
                62_u16
            } else {
                183_u16
            };
            bytes[18..20].copy_from_slice(&machine.to_le_bytes());
            bytes[20..24].copy_from_slice(&1_u32.to_le_bytes());
            bytes[24..32].copy_from_slice(&0x1000_u64.to_le_bytes());
            bytes[32..40].copy_from_slice(&(HEADER_BYTES as u64).to_le_bytes());
            bytes[52..54].copy_from_slice(&(HEADER_BYTES as u16).to_le_bytes());
            bytes[54..56].copy_from_slice(&(PROGRAM_BYTES as u16).to_le_bytes());
            bytes[56..58].copy_from_slice(&1_u16.to_le_bytes());
            let image_bytes = bytes.len() as u64;
            let program = &mut bytes[HEADER_BYTES..];
            program[..4].copy_from_slice(&1_u32.to_le_bytes());
            program[4..8].copy_from_slice(&5_u32.to_le_bytes());
            program[16..24].copy_from_slice(&0x1000_u64.to_le_bytes());
            program[32..40].copy_from_slice(&image_bytes.to_le_bytes());
            program[40..48].copy_from_slice(&image_bytes.to_le_bytes());
            program[48..56].copy_from_slice(&0x1000_u64.to_le_bytes());
            return bytes;
        }
        #[cfg(target_os = "macos")]
        {
            const HEADER_BYTES: usize = 32;
            const SEGMENT_BYTES: usize = 72;
            const ENTRY_BYTES: usize = 24;

            let mut bytes = vec![0_u8; HEADER_BYTES + 2 * SEGMENT_BYTES + ENTRY_BYTES + 1];
            bytes[..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
            let cpu = if cfg!(target_arch = "x86_64") {
                0x0100_0007_u32
            } else {
                0x0100_000c_u32
            };
            bytes[4..8].copy_from_slice(&cpu.to_le_bytes());
            let subtype = if cfg!(target_arch = "x86_64") {
                3_u32
            } else {
                0_u32
            };
            bytes[8..12].copy_from_slice(&subtype.to_le_bytes());
            bytes[12..16].copy_from_slice(&2_u32.to_le_bytes());
            bytes[16..20].copy_from_slice(&3_u32.to_le_bytes());
            bytes[20..24]
                .copy_from_slice(&((2 * SEGMENT_BYTES + ENTRY_BYTES) as u32).to_le_bytes());
            let image_bytes = bytes.len() as u64;
            let pagezero = &mut bytes[HEADER_BYTES..HEADER_BYTES + SEGMENT_BYTES];
            pagezero[..4].copy_from_slice(&0x19_u32.to_le_bytes());
            pagezero[4..8].copy_from_slice(&(SEGMENT_BYTES as u32).to_le_bytes());
            pagezero[8..18].copy_from_slice(b"__PAGEZERO");
            pagezero[32..40].copy_from_slice(&0x1_0000_0000_u64.to_le_bytes());
            let text_offset = HEADER_BYTES + SEGMENT_BYTES;
            let text = &mut bytes[text_offset..text_offset + SEGMENT_BYTES];
            text[..4].copy_from_slice(&0x19_u32.to_le_bytes());
            text[4..8].copy_from_slice(&(SEGMENT_BYTES as u32).to_le_bytes());
            text[8..14].copy_from_slice(b"__TEXT");
            text[24..32].copy_from_slice(&0x1_0000_0000_u64.to_le_bytes());
            text[32..40].copy_from_slice(&image_bytes.to_le_bytes());
            text[48..56].copy_from_slice(&image_bytes.to_le_bytes());
            text[56..60].copy_from_slice(&7_u32.to_le_bytes());
            text[60..64].copy_from_slice(&5_u32.to_le_bytes());
            let entry_offset = HEADER_BYTES + 2 * SEGMENT_BYTES;
            let entry = &mut bytes[entry_offset..entry_offset + ENTRY_BYTES];
            entry[..4].copy_from_slice(&0x8000_0028_u32.to_le_bytes());
            entry[4..8].copy_from_slice(&(ENTRY_BYTES as u32).to_le_bytes());
            entry[8..16].copy_from_slice(
                &((HEADER_BYTES + 2 * SEGMENT_BYTES + ENTRY_BYTES) as u64).to_le_bytes(),
            );
            *bytes.last_mut().expect("entry byte") = 0xc3;
            return bytes;
        }
        #[allow(unreachable_code)]
        std::fs::read(std::env::current_exe().expect("current test executable"))
            .expect("read current native test executable")
    }

    fn scratch(tag: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "fmn-sinks-test-{}-{}-{tag}",
            std::process::id(),
            SCRATCH_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).expect("fresh scratch directory");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;

            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
                .expect("private scratch permissions");
        }
        path
    }

    fn fake_tool(tag: &str) -> (PathBuf, FfmpegTool, Arc<PublishingRunner>) {
        let root = scratch(tag);
        let path = root.join("ffmpeg");
        let mut fixture = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .expect("fresh fake tool");
        fixture
            .write_all(&native_tool_bytes())
            .expect("write fake tool");
        fixture.sync_all().expect("sync fake tool");
        drop(fixture);
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("mark fake tool executable");
        let runner = Arc::new(PublishingRunner::default());
        use fmn_platform::process::{FfmpegLocator as _, StdFfmpegLocator};
        let executable = StdFfmpegLocator::default()
            .locate_ffmpeg(&path)
            .expect("locate native fake tool");
        let tool = FfmpegTool::resolve(executable, runner.as_ref(), &root.join("work"))
            .expect("resolved fake tool");
        (root, tool, runner)
    }

    fn video_job(wire: WireFormat) -> VideoJob {
        VideoJob {
            width: 4,
            height: 2,
            fps: (30_000, 1_001),
            wire,
            color: if wire.has_alpha() {
                ColorDescription::srgb_full()
            } else {
                ColorDescription::video_bt709()
            },
            container: Container::Mp4,
            encoder: EncoderChoice::Auto,
            crf: Some(18),
        }
    }

    fn ffmpeg_config(
        tool: FfmpegTool,
        root: &Path,
        wire: WireFormat,
        destination: PathBuf,
        audio: Option<PathBuf>,
        profile: Option<OutputProfile>,
    ) -> FfmpegSinkConfig {
        let job_limits = JobLimits {
            keep_workdir: true,
            ..JobLimits::default()
        };
        FfmpegSinkConfig {
            tool,
            capabilities: EncoderCapabilities::parse(
                "Encoders:\n V..... = Video\n ------\n V....D libx264 fake\n",
            ),
            job: video_job(wire),
            audio,
            destination,
            workdir_root: root.join("work"),
            job_limits,
            first_sequence: 7,
            limits: exact_limits(1),
            profile,
        }
    }

    #[test]
    fn ffmpeg_sink_packs_every_wire_format_and_returns_exact_provenance() {
        for (index, wire) in [
            WireFormat::Rgba8,
            WireFormat::Bgra8,
            WireFormat::Nv12,
            WireFormat::P010,
        ]
        .into_iter()
        .enumerate()
        {
            let (root, tool, runner) = fake_tool(&format!("wire-{index}"));
            let destination = root.join(format!("final-{index}.mp4"));
            let recorder = ProfileRecorder::enabled();
            let profile = OutputProfile::new(
                Arc::new(FakeClock::new()),
                recorder.clone(),
                ProfilePath::scene(8).with_play(13),
            );
            let sink = FfmpegSink::new(
                runner.clone(),
                ffmpeg_config(
                    tool.clone(),
                    &root,
                    wire,
                    destination.clone(),
                    None,
                    Some(profile),
                ),
            )
            .expect("ffmpeg sink");
            let (binding, receipt) = sink.into_binding(format!("ffmpeg-{index}"));
            let (frame, tight) = padded_frame(wire.frame_format(), 30 + index as u8);
            let emitter = OrderedEmitter::new(
                EmitterConfig::new(frame.layout().clone(), 1, 7).expect("emitter config"),
                vec![binding],
            )
            .expect("emitter");
            let mut reservation = emitter.reserve(7).expect("reservation");
            reservation
                .frame_mut()
                .as_bytes_mut()
                .copy_from_slice(frame.as_bytes());
            reservation.publish().expect("publish");
            emitter.finish().expect("ffmpeg finish");

            let runs = runner.runs();
            assert_eq!(runs.len(), 2);
            assert_eq!(runs[1].stdin.as_deref(), Some(tight.as_slice()));
            let chunks = runner.stdin_chunks();
            assert_eq!(chunks.iter().sum::<usize>(), tight.len());
            assert!(chunks.len() > 1, "frame feed must be row/plane chunked");
            assert!(
                chunks.iter().copied().max().unwrap_or(0) < tight.len(),
                "ffmpeg feed collapsed a frame into one retained payload"
            );
            assert_eq!(
                std::fs::read(&destination).expect("video"),
                b"video-artifact"
            );
            let report = receipt.take().expect("ffmpeg report");
            assert_eq!(report.frame_count, 1);
            assert_eq!(report.input_bytes, tight.len() as u64);
            assert_eq!(report.boundary.destination, destination);
            assert_eq!(report.boundary.artifact_bytes, 14);
            assert_eq!(report.boundary.artifact_digest, sha256(b"video-artifact"));
            assert_eq!(report.boundary.invocations.len(), 1);
            let invocation = &report.boundary.invocations[0];
            assert_eq!(invocation.provenance.tool_path, tool.path());
            assert_eq!(invocation.provenance.tool_sha256_hex, tool.sha256_hex());
            assert_eq!(invocation.provenance.native_image, tool.native_image());
            assert_eq!(invocation.provenance.tool_version, tool.version());
            assert_eq!(invocation.provenance.bound_tool_path, runs[1].program);
            assert_eq!(
                invocation.provenance.process_mechanism,
                "scripted.process_runner"
            );
            assert_eq!(invocation.provenance.process_policy_version, 1);
            assert_ne!(
                invocation.provenance.bound_tool_path,
                invocation.provenance.tool_path
            );
            assert_eq!(invocation.provenance.encoder.as_deref(), Some("libx264"));
            assert_eq!(invocation.stderr, b"fake-ffmpeg-log");

            let ndjson = recorder.snapshot().to_ndjson();
            assert!(ndjson.contains("\"phase\":\"emit\""), "{ndjson}");
            assert!(ndjson.contains("\"phase\":\"encode\""), "{ndjson}");
            assert!(ndjson.contains("\"phase\":\"ffmpeg_feed\""), "{ndjson}");
        }
    }

    #[test]
    fn ffmpeg_audio_mux_is_two_stage_and_requires_an_absolute_audio_path() {
        let (root, tool, runner) = fake_tool("audio");
        let destination = root.join("final.mp4");
        let audio = root.join("audio.wav");
        std::fs::write(&audio, b"native wav fixture").expect("audio fixture");
        let mut sink = FfmpegSink::new(
            runner.clone(),
            ffmpeg_config(
                tool.clone(),
                &root,
                WireFormat::Rgba8,
                destination.clone(),
                Some(audio.clone()),
                None,
            ),
        )
        .expect("audio mux sink");
        let receipt = sink.receipt();
        let (frame, tight) = padded_frame(PixelFormat::Rgba8, 10);
        write_direct(&mut sink, 7, &frame);
        sink.finish().expect("two-stage mux");
        assert_eq!(
            std::fs::read(&destination).expect("muxed"),
            b"muxed-artifact"
        );
        let runs = runner.runs();
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[1].stdin.as_deref(), Some(tight.as_slice()));
        assert!(runs[2].stdin.is_none());
        assert_eq!(runs[1].program, runs[2].program);
        assert!(
            runs[2]
                .argv
                .iter()
                .any(|argument| argument == &audio.display().to_string())
        );
        assert!(runs[2].argv.windows(2).any(|pair| pair == ["-c:v", "copy"]));
        let report = receipt.take().expect("mux report");
        assert_eq!(report.boundary.destination, destination);
        assert_eq!(report.boundary.invocations.len(), 2);
        assert!(
            report.boundary.invocations[0]
                .provenance
                .argv
                .windows(2)
                .any(|pair| pair == ["-f", "rawvideo"])
        );
        assert!(
            report.boundary.invocations[1]
                .provenance
                .argv
                .windows(2)
                .any(|pair| pair == ["-c:v", "copy"])
        );

        let error = FfmpegSink::new(
            runner,
            ffmpeg_config(
                tool,
                &root,
                WireFormat::Rgba8,
                root.join("relative-refused.mp4"),
                Some(PathBuf::from("audio.wav")),
                None,
            ),
        )
        .err()
        .expect("relative audio path refused");
        assert_eq!(
            error,
            SinkAdapterError::InvalidConfig("ffmpeg audio path must be absolute")
        );
    }

    #[test]
    fn ffmpeg_abort_cancels_the_live_process_without_replacing_the_destination() {
        let (root, tool, runner) = fake_tool("abort");
        let destination = root.join("existing.mp4");
        std::fs::write(&destination, b"old-video").expect("old video");
        let mut sink = FfmpegSink::new(
            runner.clone(),
            ffmpeg_config(
                tool,
                &root,
                WireFormat::P010,
                destination.clone(),
                None,
                None,
            ),
        )
        .expect("ffmpeg sink");
        let receipt = sink.receipt();
        let (frame, tight) = padded_frame(PixelFormat::P010, 8);
        write_direct(&mut sink, 7, &frame);
        sink.abort();
        let runs = runner.runs();
        assert_eq!(runs.len(), 2, "version probe and cancelled encode");
        assert_eq!(runs[1].stdin.as_deref(), Some(tight.as_slice()));
        assert_eq!(
            std::fs::read(&destination).expect("existing destination"),
            b"old-video"
        );
        assert!(matches!(receipt.take(), Err(ReceiptError::Aborted)));
    }

    #[test]
    fn ffmpeg_failure_is_receipted_and_never_replaces_the_destination() {
        let (root, tool, runner) = fake_tool("failure");
        let destination = root.join("existing.mp4");
        std::fs::write(&destination, b"old-video").expect("old video");
        runner.fail_encode();
        let mut sink = FfmpegSink::new(
            runner.clone(),
            ffmpeg_config(
                tool,
                &root,
                WireFormat::Rgba8,
                destination.clone(),
                None,
                None,
            ),
        )
        .expect("ffmpeg sink");
        let receipt = sink.receipt();
        let (frame, _) = padded_frame(PixelFormat::Rgba8, 8);
        write_direct(&mut sink, 7, &frame);
        let error = sink.finish().expect_err("fake encode failure");
        assert!(error.message().contains("deliberate fake encode failure"));
        assert_eq!(runner.runs().len(), 2);
        assert_eq!(
            std::fs::read(&destination).expect("existing destination"),
            b"old-video"
        );
        assert!(matches!(receipt.take(), Err(ReceiptError::Failed(_))));
    }

    #[test]
    fn ffmpeg_sink_refuses_empty_gap_overrun_and_payload_budgets_before_spawn() {
        let (root, tool, runner) = fake_tool("negative");
        let destination = root.join("negative.mp4");
        let config = ffmpeg_config(
            tool.clone(),
            &root,
            WireFormat::Rgba8,
            destination.clone(),
            None,
            None,
        );
        let mut empty = FfmpegSink::new(runner.clone(), config).expect("empty sink");
        assert!(
            empty
                .finish()
                .expect_err("empty")
                .message()
                .contains("empty")
        );
        assert_eq!(runner.runs().len(), 1);

        let (frame, _) = padded_frame(PixelFormat::Rgba8, 5);
        let mut gap = FfmpegSink::new(
            runner.clone(),
            ffmpeg_config(
                tool.clone(),
                &root,
                WireFormat::Rgba8,
                destination.clone(),
                None,
                None,
            ),
        )
        .expect("gap sink");
        assert!(
            gap.write_frame(8, &frame)
                .expect_err("gap")
                .message()
                .contains("expected frame 7")
        );

        let mut unavailable = ffmpeg_config(
            tool.clone(),
            &root,
            WireFormat::Rgba8,
            destination.clone(),
            None,
            None,
        );
        unavailable.job.encoder = EncoderChoice::Named("h264_nvenc".to_string());
        unavailable.job.crf = None;
        let error = FfmpegSink::new(runner.clone(), unavailable)
            .err()
            .expect("unavailable encoder");
        assert_eq!(error.code(), "sink.ffmpeg");
        assert!(error.to_string().contains("h264_nvenc"));
        assert_eq!(runner.runs().len(), 1, "encoder refusal must not spawn");

        let mut config = ffmpeg_config(tool, &root, WireFormat::Rgba8, destination, None, None);
        config.limits = SinkLimits::new(1, 31, 1 << 20, 1 << 20).expect("small resident budget");
        assert_eq!(
            FfmpegSink::new(runner.clone(), config)
                .err()
                .expect("resident budget"),
            SinkAdapterError::ResidentBytesExceeded {
                attempted: 32,
                max: 31,
            }
        );
        assert_eq!(runner.runs().len(), 1, "no encode process ran");
    }

    #[test]
    fn fake_runner_contract_keeps_time_and_log_limits_explicit() {
        let (root, tool, runner) = fake_tool("limits");
        let mut config = ffmpeg_config(
            tool,
            &root,
            WireFormat::Nv12,
            root.join("limits.mp4"),
            None,
            None,
        );
        config.job_limits.timeout = Duration::from_secs(17);
        config.job_limits.max_log_bytes = 4_096;
        let mut sink = FfmpegSink::new(runner.clone(), config).expect("sink");
        let (frame, _) = padded_frame(PixelFormat::Nv12, 4);
        write_direct(&mut sink, 7, &frame);
        sink.finish().expect("finish");
        let runs = runner.runs();
        assert_eq!(runs[1].timeout, Duration::from_secs(17));
        assert_eq!(runs[1].max_output_bytes, 4_096);
        assert_eq!(
            runs[1].env,
            vec![
                ("LANG".to_string(), "C".to_string()),
                ("LC_ALL".to_string(), "C".to_string()),
                (
                    "TMPDIR".to_string(),
                    runs[1]
                        .program
                        .parent()
                        .expect("private tool parent")
                        .display()
                        .to_string()
                ),
            ]
        );
        assert!(runs[1].cwd.is_none());
    }
}
