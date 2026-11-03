//! Persistent binary16-to-YUV conversion over owned source frames.
//!
//! A frame's source allocation moves into one reused shared payload. Workers
//! own disjoint tight row-pair bands; the caller publishes those bands only
//! after all workers return successfully, then regains the same source frame.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;

use super::linear_yuv::{convert_owned_band, worker_bands};
use super::{check_dims, check_rgba16f_source, rgba16f_to_nv12, rgba16f_to_p010};
use crate::{ChromaSiting, ColorRange, FrameBuffer, FrameError, FrameLayout, PixelFormat};

/// Lifetime observations for one conversion team.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct YuvConversionTeamStats {
    /// Successful OS worker starts, including workers joined after a refused
    /// partial startup. Fixed geometry starts workers only on its first frame.
    pub workers_spawned: usize,
    /// Frames converted and published successfully, including inline frames.
    pub frames_completed: u64,
    /// Retained capacity of the tight YUV bands; never a source-frame copy.
    pub band_buffer_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BandLayout {
    format: PixelFormat,
    stride: usize,
    row_pairs: usize,
    pairs_per_band: usize,
}

#[derive(Debug)]
struct Input {
    source: FrameBuffer,
    range: ColorRange,
    siting: ChromaSiting,
}

#[derive(Debug, Default)]
struct Payload {
    input: Option<Input>,
    layout: Option<BandLayout>,
    bands: Vec<Mutex<Vec<u8>>>,
    next: AtomicUsize,
    #[cfg(test)]
    panic_next: std::sync::atomic::AtomicBool,
}

impl Payload {
    fn prepare(&mut self, destination: &FrameLayout, bands: usize) -> Result<(), FrameError> {
        let row_pairs = destination.height() as usize / 2;
        let layout = BandLayout {
            format: destination.format(),
            stride: destination
                .format()
                .min_row_bytes(destination.width(), 0)
                .ok_or(FrameError::TooLarge)?,
            row_pairs,
            pairs_per_band: row_pairs.div_ceil(bands),
        };
        if self.layout != Some(layout) {
            // A resize/format change retires the old storage before creating
            // the new shape, so admission never needs two band surfaces.
            self.bands = Vec::new();
            self.layout = Some(layout);
        }
        let count = row_pairs.div_ceil(layout.pairs_per_band);
        if self.bands.len() < count {
            self.bands
                .try_reserve_exact(count - self.bands.len())
                .map_err(|_| FrameError::TooLarge)?;
            self.bands.resize_with(count, || Mutex::new(Vec::new()));
        }
        for (index, band) in self.bands.iter_mut().enumerate() {
            let pairs = layout
                .pairs_per_band
                .min(row_pairs - index * layout.pairs_per_band);
            let bytes = pairs
                .checked_mul(3)
                .and_then(|rows| rows.checked_mul(layout.stride))
                .ok_or(FrameError::TooLarge)?;
            let buffer = band.get_mut().unwrap_or_else(PoisonError::into_inner);
            if buffer.len() < bytes {
                buffer
                    .try_reserve_exact(bytes - buffer.len())
                    .map_err(|_| FrameError::TooLarge)?;
            }
            buffer.resize(bytes, 0);
        }
        self.next.store(0, Ordering::Relaxed);
        Ok(())
    }

    fn convert(&self) {
        #[cfg(test)]
        assert!(
            !self.panic_next.swap(false, Ordering::Relaxed),
            "injected persistent conversion worker panic"
        );
        let input = self
            .input
            .as_ref()
            .expect("dispatched conversion owns its source");
        let layout = self
            .layout
            .expect("dispatched conversion has prepared bands");
        loop {
            let index = self.next.fetch_add(1, Ordering::Relaxed);
            let Some(band) = self.bands.get(index) else {
                break;
            };
            let first = index * layout.pairs_per_band;
            let pairs = layout.pairs_per_band.min(layout.row_pairs - first);
            let mut buffer = band.lock().unwrap_or_else(PoisonError::into_inner);
            let (luma, chroma) = buffer.split_at_mut(pairs * 2 * layout.stride);
            if layout.format == PixelFormat::P010 {
                convert_owned_band::<true>(
                    &input.source,
                    first,
                    luma,
                    chroma,
                    layout.stride,
                    input.range,
                    input.siting,
                );
            } else {
                convert_owned_band::<false>(
                    &input.source,
                    first,
                    luma,
                    chroma,
                    layout.stride,
                    input.range,
                    input.siting,
                );
            }
        }
    }

    fn publish(&mut self, destination: &mut FrameBuffer) {
        let layout = self
            .layout
            .expect("completed conversion has prepared bands");
        let y_stride = destination.layout().stride(0);
        let c_stride = destination.layout().stride(1);
        let c_offset = destination.layout().plane_offset(1);
        let (luma, chroma) = destination.as_bytes_mut().split_at_mut(c_offset);
        for (index, band) in self.bands.iter_mut().enumerate() {
            let first = index * layout.pairs_per_band;
            let pairs = layout.pairs_per_band.min(layout.row_pairs - first);
            let buffer = band.get_mut().unwrap_or_else(PoisonError::into_inner);
            let (band_luma, band_chroma) = buffer.split_at(pairs * 2 * layout.stride);
            for (row, bytes) in band_luma.chunks_exact(layout.stride).enumerate() {
                let start = (first * 2 + row) * y_stride;
                luma[start..start + layout.stride].copy_from_slice(bytes);
            }
            for (row, bytes) in band_chroma.chunks_exact(layout.stride).enumerate() {
                let start = (first + row) * c_stride;
                chroma[start..start + layout.stride].copy_from_slice(bytes);
            }
        }
    }
}

#[derive(Debug, Default)]
struct Dispatch {
    generation: u64,
    pending: usize,
    shutdown: bool,
    panicked: bool,
    frame: Option<Arc<Payload>>,
}

#[derive(Debug, Default)]
struct Shared {
    dispatch: Mutex<Dispatch>,
    ready: Condvar,
    completed: Condvar,
}

/// Persistent workers for direct RGBA16F → NV12/P010 conversion.
///
/// This is an explicit opt-in for stable worker lifetimes and predictable
/// steady-state allocation behavior. Parallel conversion publishes private
/// bands with an extra full-frame copy compared with the borrowed threaded
/// converters ([`super::rgba16f_to_nv12_threaded`] and
/// [`super::rgba16f_to_p010_threaded`]). Avoiding thread startup does not imply
/// higher throughput; this API is not assumed to be faster for a given workload.
///
/// Arithmetic, transfer, range, siting, orientation and padding match the
/// borrowed converters exactly. Workers start lazily only when the existing
/// row-pair threshold admits useful parallelism. Fixed-size streams
/// reuse those workers, their shared payload, and their output bands without
/// per-frame thread creation or frame-sized allocation. A larger later frame
/// may grow the team up to its requested width; workers otherwise remain
/// parked until the next parallel frame or the team's destruction.
///
/// The caller's source frame is moved, never cloned or copied. Parallel
/// conversion needs one additional tight output surface in disjoint bands;
/// include [`Self::scratch_bytes`] in memory admission. Small-frame, one-thread
/// and wasm32 conversion stay inline with no extra pixel storage.
#[derive(Debug)]
pub struct YuvConversionTeam {
    threads: usize,
    shared: Arc<Shared>,
    workers: Vec<JoinHandle<()>>,
    payload: Arc<Payload>,
    workers_spawned: usize,
    frames_completed: u64,
}

impl YuvConversionTeam {
    /// Set the maximum conversion width without starting OS workers yet.
    ///
    /// # Errors
    /// Zero threads is a typed refusal. On wasm32 the declared width is one.
    pub fn new(threads: usize) -> Result<Self, FrameError> {
        if threads == 0 {
            return Err(FrameError::InvalidConversionThreads);
        }
        Ok(Self {
            threads: if cfg!(target_arch = "wasm32") {
                1
            } else {
                threads
            },
            shared: Arc::new(Shared::default()),
            workers: Vec::new(),
            payload: Arc::new(Payload::default()),
            workers_spawned: 0,
            frames_completed: 0,
        })
    }

    /// Maximum execution width after the browser tier's explicit clamp.
    #[must_use]
    pub const fn threads(&self) -> usize {
        self.threads
    }

    /// Conservative extra pixel storage for this layout and thread request.
    ///
    /// Inline or non-YUV output needs no bands. Parallel YUV requires at most
    /// one negotiated output frame (the actual bands omit row padding). Source
    /// storage is transferred by ownership and does not count a second time.
    #[must_use]
    pub fn scratch_bytes(layout: &FrameLayout, threads: usize) -> usize {
        if matches!(layout.format(), PixelFormat::Nv12 | PixelFormat::P010)
            && worker_bands(layout.height() as usize, threads) > 1
        {
            layout.total_bytes()
        } else {
            0
        }
    }

    /// Successful starts, completed frames, and currently retained band bytes.
    #[must_use]
    pub fn stats(&self) -> YuvConversionTeamStats {
        YuvConversionTeamStats {
            workers_spawned: self.workers_spawned,
            frames_completed: self.frames_completed,
            band_buffer_bytes: self
                .payload
                .bands
                .iter()
                .map(|band| {
                    band.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .capacity()
                })
                .sum(),
        }
    }

    /// Convert an owned source and return its exact allocation for recycling.
    ///
    /// The destination must be NV12 or P010. The source must be RGBA16F of the
    /// same shape; P010 supports limited range only. Destination padding is
    /// unchanged. A failure consumes the source but never publishes a partial
    /// output. The team remains usable after a conversion-worker panic.
    ///
    /// # Errors
    /// Invalid layouts/range, band allocation refusal, incomplete worker
    /// startup, or a worker panic. Startup failures join all started workers
    /// before returning; worker panics join the frame before reporting failure.
    pub fn convert(
        &mut self,
        source: FrameBuffer,
        destination: &mut FrameBuffer,
        range: ColorRange,
        siting: ChromaSiting,
    ) -> Result<FrameBuffer, FrameError> {
        self.convert_with_spawner(
            source,
            destination,
            range,
            siting,
            |index, shared, generation| {
                std::thread::Builder::new()
                    .name(format!("fmn-convert-{index}"))
                    .spawn(move || worker_loop(&shared, generation))
            },
        )
    }

    fn convert_with_spawner(
        &mut self,
        source: FrameBuffer,
        destination: &mut FrameBuffer,
        range: ColorRange,
        siting: ChromaSiting,
        spawn: impl FnMut(usize, Arc<Shared>, u64) -> std::io::Result<JoinHandle<()>>,
    ) -> Result<FrameBuffer, FrameError> {
        check_rgba16f_source(&source)?;
        let format = destination.layout().format();
        if !matches!(format, PixelFormat::Nv12 | PixelFormat::P010) {
            return Err(FrameError::FormatMismatch {
                expected: "Nv12 or P010 destination",
                got: format,
            });
        }
        check_dims(&source, destination)?;
        if format == PixelFormat::P010 && range != ColorRange::Limited {
            return Err(FrameError::UnsupportedConversion(
                "P010 output is limited-range only",
            ));
        }
        let bands = worker_bands(source.layout().height() as usize, self.threads);
        if bands == 1 {
            let payload = Arc::get_mut(&mut self.payload).expect("previous conversion was joined");
            // Resizing to an inline frame must retire a previous large band
            // surface; the current layout's zero-scratch admission stays true.
            payload.bands.clear();
            payload.layout = None;
            if format == PixelFormat::P010 {
                rgba16f_to_p010(&source, destination, range, siting)?;
            } else {
                rgba16f_to_nv12(&source, destination, range, siting)?;
            }
            self.frames_completed = self.frames_completed.saturating_add(1);
            return Ok(source);
        }
        self.start_workers(bands, spawn)?;
        let payload = Arc::get_mut(&mut self.payload).expect("previous conversion was joined");
        payload.prepare(destination.layout(), bands)?;
        payload.input = Some(Input {
            source,
            range,
            siting,
        });
        let result = self.dispatch();
        let payload = Arc::get_mut(&mut self.payload).expect("all source readers released");
        let source = payload
            .input
            .take()
            .expect("dispatched source is returned")
            .source;
        result?;
        payload.publish(destination);
        self.frames_completed = self.frames_completed.saturating_add(1);
        Ok(source)
    }

    fn start_workers(
        &mut self,
        requested: usize,
        mut spawn: impl FnMut(usize, Arc<Shared>, u64) -> std::io::Result<JoinHandle<()>>,
    ) -> Result<(), FrameError> {
        if requested <= self.workers.len() {
            return Ok(());
        }
        self.workers
            .try_reserve_exact(requested - self.workers.len())
            .map_err(|_| FrameError::TooLarge)?;
        // New workers join between frames. Pass their starting generation
        // explicitly so neither a past dispatch nor a fast next dispatch can
        // be mistaken for their first job.
        let generation = self
            .shared
            .dispatch
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .generation;
        for index in self.workers.len()..requested {
            match spawn(index, Arc::clone(&self.shared), generation) {
                Ok(worker) => {
                    self.workers.push(worker);
                    self.workers_spawned = self.workers_spawned.saturating_add(1);
                }
                Err(_) => {
                    let spawned = self.workers.len();
                    self.stop_workers();
                    self.shared = Arc::new(Shared::default());
                    return Err(FrameError::ConversionWorkerSpawnFailed { requested, spawned });
                }
            }
        }
        Ok(())
    }

    fn dispatch(&self) -> Result<(), FrameError> {
        let mut dispatch = self
            .shared
            .dispatch
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        dispatch.generation = dispatch.generation.wrapping_add(1);
        dispatch.pending = self.workers.len();
        dispatch.panicked = false;
        dispatch.frame = Some(Arc::clone(&self.payload));
        self.shared.ready.notify_all();
        while dispatch.pending != 0 {
            dispatch = self
                .shared
                .completed
                .wait(dispatch)
                .unwrap_or_else(PoisonError::into_inner);
        }
        dispatch.frame = None;
        if dispatch.panicked {
            Err(FrameError::ConversionWorkerPanicked)
        } else {
            Ok(())
        }
    }

    fn stop_workers(&mut self) {
        self.shared
            .dispatch
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .shutdown = true;
        self.shared.ready.notify_all();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

impl Drop for YuvConversionTeam {
    fn drop(&mut self) {
        self.stop_workers();
    }
}

fn worker_loop(shared: &Shared, mut generation: u64) {
    loop {
        let payload = {
            let mut dispatch = shared
                .dispatch
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            while !dispatch.shutdown && dispatch.generation == generation {
                dispatch = shared
                    .ready
                    .wait(dispatch)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            if dispatch.shutdown {
                return;
            }
            generation = dispatch.generation;
            Arc::clone(
                dispatch
                    .frame
                    .as_ref()
                    .expect("active conversion is installed"),
            )
        };
        let result = catch_unwind(AssertUnwindSafe(|| payload.convert()));
        drop(payload);
        let mut dispatch = shared
            .dispatch
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        dispatch.panicked |= result.is_err();
        dispatch.pending -= 1;
        if dispatch.pending == 0 {
            shared.completed.notify_one();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(width: u32, height: u32) -> FrameBuffer {
        let mut source = FrameBuffer::new(
            FrameLayout::with_row_alignment(PixelFormat::Rgba16F, width, height, 128).unwrap(),
        );
        source.as_bytes_mut().fill(0xA5);
        let stride = source.layout().stride(0);
        for y in 0..height as usize {
            for x in 0..width as usize {
                let bits = ((y * width as usize + x) as u16).wrapping_mul(491);
                let values = [bits, bits.rotate_left(5), !bits, bits.rotate_right(3)];
                let start = y * stride + x * 8;
                for (channel, value) in values.into_iter().enumerate() {
                    source.as_bytes_mut()[start + channel * 2..start + channel * 2 + 2]
                        .copy_from_slice(&value.to_le_bytes());
                }
            }
        }
        // Both infinities, a NaN, and a negative finite channel occur even in
        // tiny fixtures; their handling must agree with the borrowed kernel.
        source.as_bytes_mut()[..8].copy_from_slice(&[0, 0x7C, 0, 0xFC, 1, 0x7E, 0, 0xBC]);
        source
    }

    fn output(format: PixelFormat, width: u32, height: u32) -> FrameBuffer {
        let stride = format.min_row_bytes(width, 0).unwrap();
        let mut output = FrameBuffer::new(
            FrameLayout::with_strides(format, width, height, &[stride + 14, stride + 30]).unwrap(),
        );
        output.as_bytes_mut().fill(0xA5);
        output
    }

    fn serial(
        source: &FrameBuffer,
        destination: &mut FrameBuffer,
        range: ColorRange,
        siting: ChromaSiting,
    ) {
        if destination.layout().format() == PixelFormat::P010 {
            rgba16f_to_p010(source, destination, range, siting).unwrap();
        } else {
            rgba16f_to_nv12(source, destination, range, siting).unwrap();
        }
    }

    fn worker_ids(team: &YuvConversionTeam) -> Vec<std::thread::ThreadId> {
        team.workers
            .iter()
            .map(|worker| worker.thread().id())
            .collect()
    }

    #[test]
    fn persistent_yuv_matches_serial_at_every_width_and_reuses_workers_and_storage() {
        let (width, height) = (18, 1058); // All 16 workers, including a short final band.
        for threads in [1, 4, 16] {
            let mut team = YuvConversionTeam::new(threads).unwrap();
            let mut source = source(width, height);
            let source_pointer = source.as_bytes().as_ptr();
            let mut ids = None;
            let mut completed = 0;
            for format in [PixelFormat::Nv12, PixelFormat::P010] {
                for range in [ColorRange::Limited, ColorRange::Full] {
                    if format == PixelFormat::P010 && range == ColorRange::Full {
                        continue;
                    }
                    for siting in [ChromaSiting::Left, ChromaSiting::Center] {
                        let mut actual = output(format, width, height);
                        let mut expected = output(format, width, height);
                        let tight_bytes = FrameLayout::tight(format, width, height)
                            .unwrap()
                            .total_bytes();
                        let mut buffers = None;
                        for frame in 0..3 {
                            source.as_bytes_mut()[8..10]
                                .copy_from_slice(&(0x3800_u16 + frame).to_le_bytes());
                            let source_before = source.as_bytes().to_vec();
                            expected.as_bytes_mut().fill(0xA5);
                            actual.as_bytes_mut().fill(0xA5);
                            serial(&source, &mut expected, range, siting);
                            source = team.convert(source, &mut actual, range, siting).unwrap();
                            assert_eq!(
                                actual.as_bytes(),
                                expected.as_bytes(),
                                "{format:?}, {range:?}, {siting:?}, {threads} threads"
                            );
                            assert_eq!(source.as_bytes(), source_before);
                            assert_eq!(
                                source.as_bytes().as_ptr(),
                                source_pointer,
                                "the raster is moved, never cloned"
                            );
                            completed += 1;
                            let stats = team.stats();
                            let parallel = team.threads() > 1;
                            assert_eq!(stats.workers_spawned, if parallel { threads } else { 0 });
                            assert_eq!(stats.frames_completed, completed);
                            assert_eq!(
                                stats.band_buffer_bytes,
                                if parallel { tight_bytes } else { 0 }
                            );
                            let current = worker_ids(&team);
                            assert_eq!(ids.get_or_insert_with(|| current.clone()), &current);
                            let current_buffers: Vec<_> = team
                                .payload
                                .bands
                                .iter()
                                .map(|band| band.lock().unwrap().as_ptr())
                                .collect();
                            assert_eq!(
                                buffers.get_or_insert_with(|| current_buffers.clone()),
                                &current_buffers
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn tight_and_independently_padded_planes_match_serial() {
        let (width, height) = (18, 1058);
        for format in [PixelFormat::Nv12, PixelFormat::P010] {
            let stride = format.min_row_bytes(width, 0).unwrap();
            for threads in [4, 16] {
                let mut team = YuvConversionTeam::new(threads).unwrap();
                let mut source = source(width, height);
                // Exercise each tight/padded plane independently, then return
                // to tight output with the same retained conversion bands.
                for padding in [[0, 0], [0, 30], [14, 0], [0, 0]] {
                    let layout = FrameLayout::with_strides(
                        format,
                        width,
                        height,
                        &[stride + padding[0], stride + padding[1]],
                    )
                    .unwrap();
                    let mut actual = FrameBuffer::new(layout.clone());
                    let mut expected = FrameBuffer::new(layout);
                    actual.as_bytes_mut().fill(0xA5);
                    expected.as_bytes_mut().fill(0xA5);
                    serial(
                        &source,
                        &mut expected,
                        ColorRange::Limited,
                        ChromaSiting::Center,
                    );
                    source = team
                        .convert(
                            source,
                            &mut actual,
                            ColorRange::Limited,
                            ChromaSiting::Center,
                        )
                        .unwrap();
                    assert_eq!(
                        actual.as_bytes(),
                        expected.as_bytes(),
                        "{format:?}, padding {padding:?}, {threads} threads"
                    );
                }
            }
        }
    }

    #[test]
    fn small_frames_stay_inline_and_resizes_only_grow_workers_when_useful() {
        let mut team = YuvConversionTeam::new(16).unwrap();
        let mut previous_ids = Vec::new();
        let mut maximum_workers = 0;
        for height in [2, 128, 258, 1058, 2, 128, 1058] {
            let source = source(18, height);
            let mut actual = output(PixelFormat::Nv12, 18, height);
            let mut expected = output(PixelFormat::Nv12, 18, height);
            serial(
                &source,
                &mut expected,
                ColorRange::Limited,
                ChromaSiting::Left,
            );
            let _source = team
                .convert(source, &mut actual, ColorRange::Limited, ChromaSiting::Left)
                .unwrap();
            assert_eq!(actual.as_bytes(), expected.as_bytes());
            let bands = worker_bands(height as usize, 16);
            if bands > 1 {
                maximum_workers = maximum_workers.max(bands);
            }
            assert_eq!(team.stats().workers_spawned, maximum_workers);
            let ids = worker_ids(&team);
            assert!(
                ids.starts_with(&previous_ids),
                "a larger frame extends the existing team"
            );
            previous_ids = ids;
            if bands == 1 {
                assert_eq!(team.stats().band_buffer_bytes, 0);
                assert_eq!(YuvConversionTeam::scratch_bytes(actual.layout(), 16), 0);
            } else {
                assert!(
                    team.stats().band_buffer_bytes
                        <= YuvConversionTeam::scratch_bytes(actual.layout(), 16)
                );
                assert_eq!(team.stats().band_buffer_bytes, 18 * height as usize * 3 / 2);
            }
        }
    }

    #[test]
    fn conversion_refusals_precede_worker_start_and_leave_output_untouched() {
        assert_eq!(
            YuvConversionTeam::new(0).unwrap_err(),
            FrameError::InvalidConversionThreads
        );
        let mut team = YuvConversionTeam::new(16).unwrap();
        for (format, width, range) in [
            (PixelFormat::Nv12, 20, ColorRange::Limited),
            (PixelFormat::P010, 18, ColorRange::Full),
        ] {
            let mut destination = output(format, width, 256);
            let before = destination.as_bytes().to_vec();
            let result = team.convert(source(18, 256), &mut destination, range, ChromaSiting::Left);
            assert!(result.is_err());
            assert_eq!(destination.as_bytes(), before);
        }
        let mut destination =
            FrameBuffer::new(FrameLayout::tight(PixelFormat::Rgba8, 18, 256).unwrap());
        destination.as_bytes_mut().fill(0xA5);
        let before = destination.as_bytes().to_vec();
        assert!(matches!(
            team.convert(
                source(18, 256),
                &mut destination,
                ColorRange::Limited,
                ChromaSiting::Left
            ),
            Err(FrameError::FormatMismatch { .. })
        ));
        assert_eq!(destination.as_bytes(), before);
        let invalid_source =
            FrameBuffer::new(FrameLayout::tight(PixelFormat::Rgba8, 18, 256).unwrap());
        let mut destination = output(PixelFormat::Nv12, 18, 256);
        let before = destination.as_bytes().to_vec();
        assert!(matches!(
            team.convert(
                invalid_source,
                &mut destination,
                ColorRange::Limited,
                ChromaSiting::Left
            ),
            Err(FrameError::FormatMismatch { .. })
        ));
        assert_eq!(destination.as_bytes(), before);
        assert_eq!(team.stats(), YuvConversionTeamStats::default());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn worker_panic_does_not_publish_and_the_same_team_can_convert_again() {
        for format in [PixelFormat::Nv12, PixelFormat::P010] {
            let mut team = YuvConversionTeam::new(4).unwrap();
            let mut actual = output(format, 18, 256);
            let mut expected = output(format, 18, 256);
            let source = source(18, 256);
            serial(
                &source,
                &mut expected,
                ColorRange::Limited,
                ChromaSiting::Left,
            );
            let source = team
                .convert(source, &mut actual, ColorRange::Limited, ChromaSiting::Left)
                .unwrap();
            let retry = source.clone();
            let ids = worker_ids(&team);
            actual.as_bytes_mut().fill(0xA5);
            let before = actual.as_bytes().to_vec();
            team.payload.panic_next.store(true, Ordering::Relaxed);
            assert_eq!(
                team.convert(source, &mut actual, ColorRange::Limited, ChromaSiting::Left)
                    .unwrap_err(),
                FrameError::ConversionWorkerPanicked
            );
            assert_eq!(actual.as_bytes(), before);
            assert_eq!(team.stats().frames_completed, 1);
            assert!(
                team.payload.input.is_none(),
                "failed frames release the source allocation"
            );
            let _source = team
                .convert(retry, &mut actual, ColorRange::Limited, ChromaSiting::Left)
                .unwrap();
            assert_eq!(actual.as_bytes(), expected.as_bytes());
            assert_eq!(worker_ids(&team), ids);
            assert_eq!(team.stats().workers_spawned, 4);
            assert_eq!(team.stats().frames_completed, 2);
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn partial_worker_start_is_joined_and_refused_before_any_output() {
        let mut team = YuvConversionTeam::new(4).unwrap();
        let exited = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&exited);
        let mut destination = output(PixelFormat::Nv12, 18, 256);
        let before = destination.as_bytes().to_vec();
        let result = team.convert_with_spawner(
            source(18, 256),
            &mut destination,
            ColorRange::Limited,
            ChromaSiting::Left,
            move |index, shared, generation| {
                if index == 1 {
                    return Err(std::io::Error::other(
                        "injected conversion worker spawn refusal",
                    ));
                }
                let exited = Arc::clone(&observed);
                std::thread::Builder::new().spawn(move || {
                    worker_loop(&shared, generation);
                    exited.fetch_add(1, Ordering::Relaxed);
                })
            },
        );
        assert_eq!(
            result.unwrap_err(),
            FrameError::ConversionWorkerSpawnFailed {
                requested: 4,
                spawned: 1
            }
        );
        assert_eq!(destination.as_bytes(), before);
        assert_eq!(
            exited.load(Ordering::Relaxed),
            1,
            "partial team is joined before refusal"
        );
        assert!(team.workers.is_empty());
        assert_eq!(team.stats().frames_completed, 0);
        assert_eq!(team.stats().band_buffer_bytes, 0);
        let source = source(18, 256);
        let mut expected = output(PixelFormat::Nv12, 18, 256);
        serial(
            &source,
            &mut expected,
            ColorRange::Limited,
            ChromaSiting::Left,
        );
        let _source = team
            .convert(
                source,
                &mut destination,
                ColorRange::Limited,
                ChromaSiting::Left,
            )
            .unwrap();
        assert_eq!(destination.as_bytes(), expected.as_bytes());
        assert_eq!(
            team.stats().workers_spawned,
            5,
            "successful retry plus the joined partial start"
        );
    }
}
