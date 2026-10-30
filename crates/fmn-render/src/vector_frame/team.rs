//! Persistent native raster workers over owned compiled frame generations.
//!
//! The scene and output buffer never cross threads. The team temporarily owns
//! the caller's prepared arena and tile cache, plus either an immutable frame
//! generation or the synchronous renderer's compiler tables, and rasterizes
//! into reusable, disjoint row bands. Only completed dirty tiles are copied to
//! the output. No thread-lifetime coercion or unsafe code is used.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;

use fmn_frame::{FrameBuffer, FrameError};

use super::OwnedVectorFrame;
use crate::engine::PreparedBands;
use crate::{
    AaStats, Binning, CachedRenderError, CachedRenderStats, FrameArena, FrameJob, MonoTable,
    PixelTileCache, RenderPlan, RetainedFrameRendererConfig, RetainedFrameRendererError, TileWork,
};

/// Mutable compiler tables lent to a synchronous raster call by ownership
/// exchange. None of these references are sent to a worker.
pub(crate) struct RetainedVectorFrame<'a> {
    pub(crate) plan: &'a mut RenderPlan,
    pub(crate) mono: &'a mut MonoTable,
    pub(crate) binning: &'a mut Binning,
    pub(crate) config: RetainedFrameRendererConfig,
    pub(crate) camera_revision: u64,
}

#[derive(Debug)]
struct RetainedInput {
    plan: RenderPlan,
    mono: MonoTable,
    binning: Binning,
    config: RetainedFrameRendererConfig,
    camera_revision: u64,
}

impl RetainedInput {
    fn new(config: RetainedFrameRendererConfig) -> Self {
        Self {
            plan: RenderPlan::new(),
            mono: MonoTable::default(),
            binning: Binning::default(),
            config,
            camera_revision: 0,
        }
    }

    fn exchange(&mut self, frame: &mut RetainedVectorFrame<'_>) {
        std::mem::swap(&mut self.plan, frame.plan);
        std::mem::swap(&mut self.mono, frame.mono);
        std::mem::swap(&mut self.binning, frame.binning);
        self.config = frame.config;
        self.camera_revision = frame.camera_revision;
    }
}

#[derive(Debug, Default)]
enum RasterInput {
    #[default]
    Empty,
    Frozen(OwnedVectorFrame),
    // Keep the empty exchange tables between calls. Box allocation and empty
    // compiler initialization happen once, never on each changing frame.
    Retained(Box<RetainedInput>),
}

struct RasterTables<'a> {
    plan: &'a RenderPlan,
    mono: &'a MonoTable,
    binning: &'a Binning,
    config: RetainedFrameRendererConfig,
    camera_revision: u64,
}

impl RasterInput {
    fn tables(&self) -> Result<RasterTables<'_>, RetainedFrameRendererError> {
        match self {
            Self::Empty => unreachable!("a dispatched raster owns its input"),
            Self::Frozen(frame) => Ok(RasterTables {
                plan: &frame.plan,
                mono: frame.mono.get(&frame.plan)?,
                binning: frame.binning.get(&frame.plan)?,
                config: frame.config,
                camera_revision: frame.camera_revision,
            }),
            Self::Retained(frame) => Ok(RasterTables {
                plan: &frame.plan,
                mono: &frame.mono,
                binning: &frame.binning,
                config: frame.config,
                camera_revision: frame.camera_revision,
            }),
        }
    }
}

/// Lifetime counters for one persistent raster team.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RasterTeamStats {
    /// OS workers started when the team was constructed; zero for inline mode.
    pub workers_spawned: usize,
    /// Frames successfully rasterized by this team, including inline frames.
    pub frames_completed: u64,
    /// Retained capacity of the owned tight RGBA16F band buffers, in bytes.
    pub band_buffer_bytes: usize,
}

#[derive(Debug, Default)]
struct Dispatch {
    generation: u64,
    pending: usize,
    shutdown: bool,
    panicked: bool,
    stats: AaStats,
    frame: Option<Arc<Payload>>,
}

#[derive(Debug, Default)]
struct Shared {
    dispatch: Mutex<Dispatch>,
    ready: Condvar,
    completed: Condvar,
}

#[derive(Debug, Default)]
struct Payload {
    input: RasterInput,
    prepared: Option<PreparedBands>,
    arena: FrameArena,
    cache: PixelTileCache,
    bands: Vec<Mutex<Vec<u8>>>,
    band_layout: Option<(usize, usize, usize)>,
    next: AtomicUsize,
    resumed: bool,
    #[cfg(test)]
    panic_next: std::sync::atomic::AtomicBool,
}

impl Payload {
    fn raster(&self) -> AaStats {
        #[cfg(test)]
        assert!(
            !self.panic_next.swap(false, Ordering::Relaxed),
            "injected persistent raster worker panic"
        );
        let prepared = self.prepared.expect("published frame is prepared");
        let frame = self.input.tables().expect("raster tables were prepared");
        FrameJob::from_prepared_bands(frame.plan, frame.mono, frame.binning, &self.arena, prepared)
            .render_owned_bands(&self.bands, &self.next, prepared.stride, &self.cache)
    }

    fn reserve_bands(&mut self, prepared: PreparedBands, height: usize) -> Result<(), FrameError> {
        let layout = (prepared.stride, prepared.band_rows, height);
        if self.band_layout != Some(layout) {
            // A differently shaped surface must not retain the old bands'
            // larger capacities as well as allocating its new shape. This
            // keeps the admitted one-surface budget true across resize; only
            // the fixed-shape steady state promises allocation-free reuse.
            self.bands = Vec::new();
            self.band_layout = Some(layout);
        }
        let count = height.div_ceil(prepared.band_rows);
        if self.bands.len() < count {
            self.bands
                .try_reserve_exact(count - self.bands.len())
                .map_err(|_| FrameError::TooLarge)?;
            self.bands.resize_with(count, || Mutex::new(Vec::new()));
        } else {
            self.bands.truncate(count);
        }
        for (index, band) in self.bands.iter_mut().enumerate() {
            let rows = prepared.band_rows.min(height - index * prepared.band_rows);
            let bytes = rows
                .checked_mul(prepared.stride)
                .ok_or(FrameError::TooLarge)?;
            let buffer = band.get_mut().unwrap_or_else(PoisonError::into_inner);
            if buffer.len() < bytes {
                buffer
                    .try_reserve_exact(bytes - buffer.len())
                    .map_err(|_| FrameError::TooLarge)?;
            }
            buffer.resize(bytes, 0);
        }
        Ok(())
    }

    fn prepare(
        &mut self,
        threads: usize,
        destination: &FrameBuffer,
        resume: Option<u64>,
    ) -> Result<(), RetainedFrameRendererError> {
        let frame = self.input.tables()?;
        let prepared = FrameJob::with_identity_in(
            &mut self.arena,
            frame.plan,
            frame.mono,
            frame.binning,
            frame.config.frame,
            frame.config.engine,
        )?
        .prepare_bands(threads, destination)?;
        self.reserve_bands(prepared, destination.layout().height() as usize)?;
        self.prepared = Some(prepared);
        self.next.store(0, Ordering::Relaxed);
        // No destination pixels are changed until every worker succeeds.
        // begin_render retires a failed or interrupted cache serial as usual.
        let frame = self.input.tables()?;
        self.resumed = self.cache.begin_render(resume);
        self.cache
            .prepare_frame_with_inputs(
                frame.binning,
                frame.plan,
                frame.camera_revision,
                frame.config.engine,
                frame.config.frame,
            )
            .map_err(CachedRenderError::from)?;
        Ok(())
    }

    fn publish(
        &mut self,
        destination: &mut FrameBuffer,
        raster: AaStats,
    ) -> Result<CachedRenderStats, RetainedFrameRendererError> {
        let frame = self.input.tables()?;
        let prepared = self.prepared.expect("completed frame is prepared");
        let binning = frame.binning;
        // Restoring reused tiles is safe after worker completion. An exact
        // resume token proves they already occupy this destination; a different
        // recycled surface restores from cache before copying its dirty tiles.
        if !self.resumed {
            self.cache
                .restore_reused_tiles(destination, binning)
                .map_err(CachedRenderError::from)?;
        }
        let stride = destination.layout().stride(0);
        let width = destination.layout().width() as usize;
        let tile = binning.tiling().fine_tile.max(1) as usize;
        let cols = width.div_ceil(tile);
        let output = destination.plane_mut(0);
        for (band, buffer) in self.bands.iter_mut().enumerate() {
            let buffer = buffer.get_mut().unwrap_or_else(PoisonError::into_inner);
            for (row, bytes) in buffer.chunks_exact(prepared.stride).enumerate() {
                let y = band * prepared.band_rows + row;
                for column in 0..cols {
                    if self.cache.work(band * cols + column) == Some(&TileWork::Reuse) {
                        continue;
                    }
                    let x = column * tile;
                    let begin = x * 8;
                    let end = (x + tile).min(width) * 8;
                    output[y * stride + begin..y * stride + end]
                        .copy_from_slice(&bytes[begin..end]);
                }
            }
        }
        self.cache
            .retain_rasterized_tiles(destination, binning)
            .map_err(CachedRenderError::from)?;
        self.cache.complete_render();
        Ok(CachedRenderStats {
            cache: self.cache.stats(),
            raster,
        })
    }
}

/// A fixed, persistent CPU raster team.
///
/// Workers park between frames and retain the same operating-system identity
/// until the team is dropped. A frame's immutable compiled IR is shared, its
/// arena and cache are moved without cloning, and all worker references are
/// released before the caller regains either. The shared payload itself is
/// reused without allocating a fresh `Arc` per frame.
///
/// Multithreaded teams retain one additional tight RGBA16F surface in owned
/// bands. Include `width * height * 8` bytes per team in memory admission.
/// One-thread and browser tiers execute inline and allocate no band surface.
#[derive(Debug)]
pub struct RasterTeam {
    threads: usize,
    shared: Arc<Shared>,
    workers: Vec<JoinHandle<()>>,
    payload: Arc<Payload>,
    frames_completed: u64,
}

impl RasterTeam {
    /// Start the complete team before accepting any frames.
    ///
    /// # Errors
    /// Refuses zero threads. Partial thread-start failure shuts down and joins
    /// every successfully started worker before returning its typed error.
    pub fn new(threads: usize) -> Result<Self, RetainedFrameRendererError> {
        Self::with_spawner(threads, |index, shared| {
            std::thread::Builder::new()
                .name(format!("fmn-raster-{index}"))
                .spawn(move || worker_loop(&shared))
        })
    }

    fn with_spawner(
        threads: usize,
        mut spawn: impl FnMut(usize, Arc<Shared>) -> std::io::Result<JoinHandle<()>>,
    ) -> Result<Self, RetainedFrameRendererError> {
        if threads == 0 {
            return Err(RetainedFrameRendererError::InvalidThreads);
        }
        let threads = crate::effective_threads(threads);
        let mut team = Self {
            threads,
            shared: Arc::new(Shared::default()),
            workers: Vec::new(),
            payload: Arc::new(Payload::default()),
            frames_completed: 0,
        };
        if threads > 1 {
            team.workers
                .try_reserve_exact(threads)
                .map_err(|_| FrameError::TooLarge)?;
            for index in 0..threads {
                match spawn(index, Arc::clone(&team.shared)) {
                    Ok(worker) => team.workers.push(worker),
                    Err(_) => {
                        return Err(FrameError::WorkerSpawnFailed {
                            requested: threads,
                            spawned: index,
                        }
                        .into());
                    }
                }
            }
        }
        Ok(team)
    }

    /// Fixed team width, after the browser tier's thread clamp.
    #[must_use]
    pub fn threads(&self) -> usize {
        self.threads
    }

    /// Worker creation, successful frame count, and retained pixel storage.
    #[must_use]
    pub fn stats(&self) -> RasterTeamStats {
        RasterTeamStats {
            workers_spawned: self.workers.len(),
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

    #[cfg(test)]
    pub(crate) fn worker_ids(&self) -> Vec<std::thread::ThreadId> {
        self.workers
            .iter()
            .map(|worker| worker.thread().id())
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn panic_next_frame(&self) {
        self.payload.panic_next.store(true, Ordering::Relaxed);
    }

    pub(super) fn render(
        &mut self,
        frame: &OwnedVectorFrame,
        arena: &mut FrameArena,
        cache: &mut PixelTileCache,
        destination: &mut FrameBuffer,
        resume: Option<u64>,
    ) -> Result<CachedRenderStats, RetainedFrameRendererError> {
        if self.threads == 1 {
            let stats = frame.render_cached_into(1, arena, cache, destination, resume)?;
            self.frames_completed = self.frames_completed.saturating_add(1);
            return Ok(stats);
        }
        let payload = Arc::get_mut(&mut self.payload).expect("previous frame was joined");
        std::mem::swap(&mut payload.arena, arena);
        std::mem::swap(&mut payload.cache, cache);
        payload.input = RasterInput::Frozen(frame.clone());
        let result = self.render_owned(destination, resume);
        let payload = Arc::get_mut(&mut self.payload).expect("all frame readers released");
        // Restore ownership on every typed failure, including preparation and
        // worker panics. An unsuccessful render never completes the cache.
        std::mem::swap(&mut payload.arena, arena);
        std::mem::swap(&mut payload.cache, cache);
        payload.input = RasterInput::Empty;
        payload.prepared = None;
        if result.is_ok() {
            self.frames_completed = self.frames_completed.saturating_add(1);
        }
        result
    }

    /// Synchronous retained rendering uses the same workers and raster kernels
    /// as an owned pipeline frame. Compiler tables are moved, not cloned, and
    /// restored before returning even when preparation or a worker fails.
    pub(crate) fn render_retained(
        &mut self,
        mut frame: RetainedVectorFrame<'_>,
        arena: &mut FrameArena,
        cache: &mut PixelTileCache,
        destination: &mut FrameBuffer,
        resume: Option<u64>,
    ) -> Result<CachedRenderStats, RetainedFrameRendererError> {
        if self.threads == 1 {
            let stats = FrameJob::with_identity_in(
                arena,
                frame.plan,
                frame.mono,
                frame.binning,
                frame.config.frame,
                frame.config.engine,
            )?
            .render_into_cached_resuming(
                1,
                destination,
                frame.camera_revision,
                cache,
                resume,
            )?;
            self.frames_completed = self.frames_completed.saturating_add(1);
            return Ok(stats);
        }
        let payload = Arc::get_mut(&mut self.payload).expect("previous frame was joined");
        if !matches!(payload.input, RasterInput::Retained(_)) {
            payload.input = RasterInput::Retained(Box::new(RetainedInput::new(frame.config)));
        }
        let RasterInput::Retained(input) = &mut payload.input else {
            unreachable!("retained input installed above")
        };
        input.exchange(&mut frame);
        std::mem::swap(&mut payload.arena, arena);
        std::mem::swap(&mut payload.cache, cache);
        let result = self.render_owned(destination, resume);
        let payload = Arc::get_mut(&mut self.payload).expect("all frame readers released");
        let RasterInput::Retained(input) = &mut payload.input else {
            unreachable!("workers cannot replace the retained input")
        };
        input.exchange(&mut frame);
        std::mem::swap(&mut payload.arena, arena);
        std::mem::swap(&mut payload.cache, cache);
        payload.prepared = None;
        if result.is_ok() {
            self.frames_completed = self.frames_completed.saturating_add(1);
        }
        result
    }

    fn render_owned(
        &mut self,
        destination: &mut FrameBuffer,
        resume: Option<u64>,
    ) -> Result<CachedRenderStats, RetainedFrameRendererError> {
        Arc::get_mut(&mut self.payload)
            .expect("idle payload has one owner")
            .prepare(self.threads, destination, resume)?;
        let mut dispatch = self
            .shared
            .dispatch
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        dispatch.generation = dispatch.generation.wrapping_add(1);
        dispatch.pending = self.threads;
        dispatch.panicked = false;
        dispatch.stats = AaStats::default();
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
        let panicked = dispatch.panicked;
        let stats = dispatch.stats;
        drop(dispatch);
        if panicked {
            return Err(RetainedFrameRendererError::RasterWorkerPanicked);
        }
        Arc::get_mut(&mut self.payload)
            .expect("workers release their payload before completing")
            .publish(destination, stats)
    }
}

impl Drop for RasterTeam {
    fn drop(&mut self) {
        self.shared
            .dispatch
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .shutdown = true;
        self.shared.ready.notify_all();
        for worker in self.workers.drain(..) {
            // Raster work is caught and reported at the frame boundary. Drop
            // still joins if a future non-raster worker bug unwinds the loop.
            let _ = worker.join();
        }
    }
}

fn worker_loop(shared: &Shared) {
    let mut generation = 0;
    loop {
        let payload = {
            let mut dispatch = shared
                .dispatch
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            while !dispatch.shutdown && generation == dispatch.generation {
                dispatch = shared
                    .ready
                    .wait(dispatch)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            if dispatch.shutdown {
                return;
            }
            generation = dispatch.generation;
            Arc::clone(dispatch.frame.as_ref().expect("active frame is installed"))
        };
        let result = catch_unwind(AssertUnwindSafe(|| payload.raster()));
        drop(payload);
        let mut dispatch = shared
            .dispatch
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        match result {
            Ok(stats) => dispatch.stats.merge(stats),
            Err(_) => dispatch.panicked = true,
        }
        dispatch.pending -= 1;
        if dispatch.pending == 0 {
            shared.completed.notify_one();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        EngineIdentity, FrameConfig, RetainedFrameRendererConfig, ScreenMap, Tiling,
        VectorFrameCompiler, Viewport,
    };
    use fmn_core::color::LinearRgba;
    use fmn_frame::{FrameLayout, PixelFormat};
    use fmn_mobject::{Mobject, RecordBuffer, RecordSchema, Stage};

    fn config() -> RetainedFrameRendererConfig {
        RetainedFrameRendererConfig {
            frame: FrameConfig::new(
                Viewport {
                    width: 31,
                    height: 23,
                },
                ScreenMap {
                    scale: 4.0,
                    origin: [15.0, 11.0],
                    y_up: true,
                },
                LinearRgba {
                    r: 0.03,
                    g: 0.05,
                    b: 0.08,
                    a: 1.0,
                },
            ),
            tiling: Tiling {
                macro_tile: 16,
                fine_tile: 8,
            },
            engine: EngineIdentity::certified(),
            threads: 1,
        }
    }

    fn vector() -> Mobject {
        let mut buffer = RecordBuffer::new(RecordSchema::vmobject(), 5).unwrap();
        buffer.write_range(
            "point",
            0,
            &[
                -1.5, -1.0, 0.0, -0.4, 1.8, 0.0, 1.5, -1.0, 0.0, 0.0, -1.0, 0.0, -1.5, -1.0, 0.0,
            ],
        );
        buffer.write_range("fill_rgba", 0, &[0.1, 0.3, 1.0, 0.8].repeat(5));
        buffer.write_range("stroke_rgba", 0, &[1.0, 0.3, 0.1, 0.9].repeat(5));
        buffer.write_range("stroke_width", 0, &[3.0; 5]);
        Mobject::from_buffer(buffer)
    }

    #[test]
    fn persistent_teams_match_scoped_pixels_statistics_and_recycled_surfaces() {
        for engine in [EngineIdentity::certified(), EngineIdentity::fast()] {
            let mut config = config();
            config.engine = engine;
            let mut stage = Stage::new();
            let moving = stage.add(vector());
            let fixed = stage.add(vector());
            stage.shift(fixed, [1.0, 0.5, 0.0]);
            stage.add_to_scene(moving).unwrap();
            stage.add_to_scene(fixed).unwrap();
            let mut compiler = VectorFrameCompiler::new(config).unwrap();
            let mut frames = Vec::new();
            for _ in 0..5 {
                frames.push(compiler.capture(&stage, 0).unwrap());
                stage.shift(moving, [0.3, 0.2, 0.0]);
            }
            stage.remove_from_scene(moving);
            frames.push(compiler.capture(&stage, 0).unwrap());
            stage.remove_from_scene(fixed);
            frames.push(compiler.capture(&stage, 0).unwrap());

            for threads in [1, 4, 16] {
                let mut team = RasterTeam::new(threads).unwrap();
                let (mut arena, mut cache) = (FrameArena::new(), PixelTileCache::new());
                let (mut oracle_arena, mut oracle_cache) =
                    (FrameArena::new(), PixelTileCache::new());
                let layout = config.frame.layout().unwrap();
                let mut actual = FrameBuffer::new(layout.clone());
                let mut oracle = FrameBuffer::new(layout);
                let (mut resume, mut oracle_resume) = (None, None);
                let mut hits = 0;
                for index in [0, 1, 2, 2, 4, 3, 5, 6, 0] {
                    let frame = &frames[index];
                    let expected = frame
                        .render_cached_into(
                            1,
                            &mut oracle_arena,
                            &mut oracle_cache,
                            &mut oracle,
                            oracle_resume,
                        )
                        .unwrap();
                    let observed = frame
                        .render_cached_into_with_team(
                            &mut team,
                            &mut arena,
                            &mut cache,
                            &mut actual,
                            resume,
                        )
                        .unwrap();
                    assert_eq!(actual.as_bytes(), oracle.as_bytes(), "threads={threads}");
                    assert_eq!(observed, expected, "threads={threads}, frame={index}");
                    hits += observed.cache.hits;
                    resume = Some(cache.serial());
                    oracle_resume = Some(oracle_cache.serial());
                    if index == 4 {
                        // A successful earlier surface may re-enter the pool.
                        // Its pixels cannot be trusted without an exact token.
                        actual.plane_mut(0).fill(0x91);
                        oracle.plane_mut(0).fill(0x91);
                        resume = None;
                        oracle_resume = None;
                    }
                }
                assert!(hits > 0, "exercise mixed dirty and cached bands");
                assert_eq!(team.stats().frames_completed, 9);
                assert_eq!(
                    team.stats().workers_spawned,
                    if threads == 1 { 0 } else { threads }
                );
            }
        }
    }

    #[test]
    fn thousand_frames_reuse_workers_payload_arena_and_band_allocations() {
        let mut config = config();
        config.frame.viewport = Viewport {
            width: 8,
            height: 8,
        };
        config.frame.map = ScreenMap {
            scale: 1.0,
            origin: [4.0, 4.0],
            y_up: true,
        };
        let mut stage = Stage::new();
        let mob = stage.add(vector());
        stage.add_to_scene(mob).unwrap();
        let mut compiler = VectorFrameCompiler::new(config).unwrap();
        let frame = compiler.capture(&stage, 0).unwrap();
        let mut team = RasterTeam::new(4).unwrap();
        let workers: Vec<_> = team
            .workers
            .iter()
            .map(|worker| worker.thread().id())
            .collect();
        let payload = Arc::as_ptr(&team.payload);
        let (mut arena, mut cache) = (FrameArena::new(), PixelTileCache::new());
        let mut pixels = FrameBuffer::new(config.frame.layout().unwrap());
        let first = frame
            .render_cached_into_with_team(&mut team, &mut arena, &mut cache, &mut pixels, None)
            .unwrap();
        assert!(first.cache.misses > 0);
        let buffers: Vec<_> = team
            .payload
            .bands
            .iter()
            .map(|band| band.lock().unwrap().as_ptr() as usize)
            .collect();
        let warm_bytes = team.stats().band_buffer_bytes;
        for _ in 1..1000 {
            let resume = Some(cache.serial());
            frame
                .render_cached_into_with_team(
                    &mut team,
                    &mut arena,
                    &mut cache,
                    &mut pixels,
                    resume,
                )
                .unwrap();
            assert_eq!(arena.stats().heap_allocs_this_frame, 0);
        }
        assert_eq!(team.stats().workers_spawned, 4);
        assert_eq!(team.stats().frames_completed, 1000);
        assert_eq!(team.stats().band_buffer_bytes, warm_bytes);
        assert_eq!(Arc::as_ptr(&team.payload), payload);
        assert_eq!(
            team.workers
                .iter()
                .map(|worker| worker.thread().id())
                .collect::<Vec<_>>(),
            workers
        );
        assert_eq!(
            team.payload
                .bands
                .iter()
                .map(|band| { band.lock().unwrap().as_ptr() as usize })
                .collect::<Vec<_>>(),
            buffers
        );
    }

    #[test]
    fn worker_panic_returns_ownership_and_does_not_publish_partial_pixels() {
        let mut stage = Stage::new();
        let mob = stage.add(vector());
        stage.add_to_scene(mob).unwrap();
        let mut compiler = VectorFrameCompiler::new(config()).unwrap();
        let frame = compiler.capture(&stage, 0).unwrap();
        let mut team = RasterTeam::new(4).unwrap();
        let (mut arena, mut cache) = (FrameArena::new(), PixelTileCache::new());
        let mut pixels = FrameBuffer::new(config().frame.layout().unwrap());
        pixels.plane_mut(0).fill(0x74);
        team.payload.panic_next.store(true, Ordering::Relaxed);
        let error = frame
            .render_cached_into_with_team(&mut team, &mut arena, &mut cache, &mut pixels, None)
            .unwrap_err();
        assert!(matches!(
            error,
            RetainedFrameRendererError::RasterWorkerPanicked
        ));
        assert!(pixels.as_bytes().iter().all(|byte| *byte == 0x74));
        assert_eq!(team.stats().frames_completed, 0);
        assert!(matches!(team.payload.input, RasterInput::Empty));
        assert!(arena.stats().pool_slots >= 4);
        let actual = frame
            .render_cached_into_with_team(&mut team, &mut arena, &mut cache, &mut pixels, None)
            .unwrap();
        let (oracle, expected) = frame
            .render_cached(1, &mut FrameArena::new(), &mut PixelTileCache::new())
            .unwrap();
        assert_eq!(pixels.as_bytes(), oracle.as_bytes());
        assert_eq!(actual.raster, expected.raster);
        assert_eq!(team.stats().workers_spawned, 4);
    }

    #[test]
    fn failed_dirty_frame_retires_resume_tokens_and_recovers_poisoned_band_storage() {
        let mut stage = Stage::new();
        let mob = stage.add(vector());
        stage.add_to_scene(mob).unwrap();
        let mut compiler = VectorFrameCompiler::new(config()).unwrap();
        let first = compiler.capture(&stage, 0).unwrap();
        let mut team = RasterTeam::new(4).unwrap();
        let (mut arena, mut cache) = (FrameArena::new(), PixelTileCache::new());
        let mut pixels = FrameBuffer::new(config().frame.layout().unwrap());
        first
            .render_cached_into_with_team(&mut team, &mut arena, &mut cache, &mut pixels, None)
            .unwrap();
        let before = pixels.as_bytes().to_vec();
        let previous_token = Some(cache.serial());

        // Model an unwind while a worker owns a band, after some of its
        // output has been changed. The next complete render must overwrite
        // the dirty band and recover its lock, not trust partial bytes.
        let poisoned = catch_unwind(AssertUnwindSafe(|| {
            let mut band = team.payload.bands[0].lock().unwrap();
            band.fill(0x9b);
            panic!("injected partial-band unwind");
        }));
        assert!(poisoned.is_err());
        stage.shift(mob, [0.4, 0.3, 0.0]);
        let changed = compiler.capture(&stage, 0).unwrap();
        team.payload.panic_next.store(true, Ordering::Relaxed);
        assert!(matches!(
            changed.render_cached_into_with_team(
                &mut team,
                &mut arena,
                &mut cache,
                &mut pixels,
                previous_token,
            ),
            Err(RetainedFrameRendererError::RasterWorkerPanicked)
        ));
        assert_eq!(pixels.as_bytes(), before);
        assert_ne!(Some(cache.serial()), previous_token);

        // A caller may supply the last successful token again. The failed
        // frame retired it, so hits must be restored to this recycled output.
        pixels.plane_mut(0).fill(0x67);
        changed
            .render_cached_into_with_team(
                &mut team,
                &mut arena,
                &mut cache,
                &mut pixels,
                previous_token,
            )
            .unwrap();
        let (oracle, _) = changed
            .render_cached(1, &mut FrameArena::new(), &mut PixelTileCache::new())
            .unwrap();
        assert_eq!(pixels.as_bytes(), oracle.as_bytes());
        assert_eq!(team.stats().frames_completed, 2);
        assert_eq!(team.stats().workers_spawned, 4);
    }

    #[test]
    fn invalid_destination_and_zero_threads_are_refused_before_publication() {
        assert!(matches!(
            RasterTeam::new(0),
            Err(RetainedFrameRendererError::InvalidThreads)
        ));
        let mut compiler = VectorFrameCompiler::new(config()).unwrap();
        let frame = compiler.capture(&Stage::new(), 0).unwrap();
        let mut team = RasterTeam::new(4).unwrap();
        let mut pixels = FrameBuffer::new(FrameLayout::tight(PixelFormat::Rgba8, 31, 23).unwrap());
        pixels.plane_mut(0).fill(0x62);
        let error = frame
            .render_cached_into_with_team(
                &mut team,
                &mut FrameArena::new(),
                &mut PixelTileCache::new(),
                &mut pixels,
                None,
            )
            .unwrap_err();
        assert!(matches!(
            error,
            RetainedFrameRendererError::Layout(FrameError::FormatMismatch { .. })
        ));
        assert!(pixels.as_bytes().iter().all(|byte| *byte == 0x62));
        assert_eq!(team.stats().frames_completed, 0);
        assert_eq!(team.stats().band_buffer_bytes, 0);
    }

    #[test]
    fn partially_started_team_is_joined_when_the_host_refuses_a_worker() {
        let alive = Arc::new(AtomicUsize::new(0));
        let error = RasterTeam::with_spawner(4, |index, shared| {
            if index == 2 {
                return Err(std::io::ErrorKind::WouldBlock.into());
            }
            let alive = Arc::clone(&alive);
            std::thread::Builder::new().spawn(move || {
                alive.fetch_add(1, Ordering::SeqCst);
                worker_loop(&shared);
                alive.fetch_sub(1, Ordering::SeqCst);
            })
        })
        .unwrap_err();
        assert!(matches!(
            error,
            RetainedFrameRendererError::Layout(FrameError::WorkerSpawnFailed {
                requested: 4,
                spawned: 2
            })
        ));
        assert_eq!(alive.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn resized_teams_release_the_previous_shape_and_preserve_row_padding() {
        let mut team = RasterTeam::new(4).unwrap();
        let (mut arena, mut cache) = (FrameArena::new(), PixelTileCache::new());
        for (width, height) in [(63, 17), (7, 41), (31, 23), (8, 8)] {
            let mut config = config();
            config.frame.viewport = Viewport { width, height };
            let mut stage = Stage::new();
            let mob = stage.add(vector());
            stage.add_to_scene(mob).unwrap();
            let mut compiler = VectorFrameCompiler::new(config).unwrap();
            let frame = compiler.capture(&stage, 0).unwrap();
            let row_bytes = width as usize * 8;
            let stride = row_bytes + 24;
            let layout =
                FrameLayout::with_strides(PixelFormat::Rgba16F, width, height, &[stride]).unwrap();
            let mut pixels = FrameBuffer::new(layout);
            pixels.plane_mut(0).fill(0x53);
            frame
                .render_cached_into_with_team(&mut team, &mut arena, &mut cache, &mut pixels, None)
                .unwrap();
            let (oracle, _) = frame
                .render_cached(1, &mut FrameArena::new(), &mut PixelTileCache::new())
                .unwrap();
            for (observed, expected) in pixels
                .plane(0)
                .chunks_exact(stride)
                .zip(oracle.plane(0).chunks_exact(row_bytes))
            {
                assert_eq!(&observed[..row_bytes], expected);
                assert!(observed[row_bytes..].iter().all(|byte| *byte == 0x53));
            }
            assert_eq!(team.stats().band_buffer_bytes, row_bytes * height as usize);
            assert_eq!(team.stats().workers_spawned, 4);
        }
    }

    #[test]
    fn retained_tables_return_after_resize_and_destination_refusal() {
        let mut team = RasterTeam::new(4).unwrap();
        let workers = team.worker_ids();
        let (mut arena, mut cache) = (FrameArena::new(), PixelTileCache::new());
        let mut stage = Stage::new();
        let mob = stage.add(vector());
        stage.add_to_scene(mob).unwrap();
        let mut plan = RenderPlan::new();
        plan.sync(&stage, 0).unwrap();
        let mut mono = MonoTable::default();
        let mut binning = Binning::default();
        for (width, height) in [(63, 17), (7, 41), (31, 23), (8, 8)] {
            let mut config = config();
            config.frame.viewport = Viewport { width, height };
            mono.rebuild_in_place(&plan, config.frame.map).unwrap();
            binning
                .rebuild_in_place(
                    &plan,
                    config.frame.viewport,
                    config.tiling,
                    config.frame.map,
                )
                .unwrap();
            binning.prune_occluded(&plan).unwrap();
            let identity = plan.identity();
            let pieces = mono.pieces().as_ptr();
            let oracle =
                FrameJob::with_identity(&plan, &mono, &binning, config.frame, config.engine)
                    .unwrap()
                    .render(1)
                    .unwrap();
            let row_bytes = width as usize * 8;
            let stride = row_bytes + 24;
            let mut pixels = FrameBuffer::new(
                FrameLayout::with_strides(PixelFormat::Rgba16F, width, height, &[stride]).unwrap(),
            );
            pixels.plane_mut(0).fill(0x53);
            team.render_retained(
                RetainedVectorFrame {
                    plan: &mut plan,
                    mono: &mut mono,
                    binning: &mut binning,
                    config,
                    camera_revision: 0,
                },
                &mut arena,
                &mut cache,
                &mut pixels,
                None,
            )
            .unwrap();
            for (actual, expected) in pixels
                .plane(0)
                .chunks_exact(stride)
                .zip(oracle.plane(0).chunks_exact(row_bytes))
            {
                assert_eq!(&actual[..row_bytes], expected);
                assert!(actual[row_bytes..].iter().all(|byte| *byte == 0x53));
            }
            assert_eq!(plan.identity(), identity);
            assert_eq!(mono.pieces().as_ptr(), pieces);
            assert_eq!(team.stats().band_buffer_bytes, row_bytes * height as usize);
            assert_eq!(team.worker_ids(), workers);

            let mut invalid =
                FrameBuffer::new(FrameLayout::tight(PixelFormat::Rgba8, width, height).unwrap());
            invalid.plane_mut(0).fill(0x74);
            let completed = team.stats().frames_completed;
            assert!(matches!(
                team.render_retained(
                    RetainedVectorFrame {
                        plan: &mut plan,
                        mono: &mut mono,
                        binning: &mut binning,
                        config,
                        camera_revision: 0,
                    },
                    &mut arena,
                    &mut cache,
                    &mut invalid,
                    None,
                ),
                Err(RetainedFrameRendererError::Layout(
                    FrameError::FormatMismatch { .. }
                ))
            ));
            assert_eq!(plan.identity(), identity);
            assert_eq!(mono.pieces().as_ptr(), pieces);
            assert_eq!(team.stats().frames_completed, completed);
            assert!(invalid.as_bytes().iter().all(|byte| *byte == 0x74));
        }
    }
}
