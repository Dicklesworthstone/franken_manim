//! Owned affine frames for bounded, multi-frame CPU execution.
//!
//! Marionette and its callbacks stay on their scene owner. Only the existing
//! compiled Lumen IR crosses threads. Workers use the ordinary `FrameJob` and
//! content-keyed tile cache, so scheduling changes neither pixels nor semantics.

use std::sync::{Arc, Mutex, OnceLock};

use fmn_frame::{FrameBuffer, FrameError, FrameLayout};
use fmn_mobject::{ProgramKind, Stage};

use crate::plan::{GeometryIdentity, PlanIdentity};
use crate::{
    Binning, BinningError, CachedRenderStats, FrameArena, FrameJob, MonoTable, MonoTableError,
    PixelTileCache, RenderPlan, RetainedFrameRendererConfig, RetainedFrameRendererError, ScreenMap,
    SyncStats,
};

/// A single successful preparation shared by concurrent readers. Failed builds
/// leave the cell empty and retryable; initialized readers never take the lock.
#[derive(Debug)]
struct LazyTable<T> {
    value: OnceLock<T>,
    building: Mutex<()>,
}

impl<T> LazyTable<T> {
    fn new() -> Self {
        Self {
            value: OnceLock::new(),
            building: Mutex::new(()),
        }
    }

    fn get(&self) -> Option<&T> {
        self.value.get()
    }

    fn get_or_try_init<E>(&self, build: impl FnOnce() -> Result<T, E>) -> Result<&T, E> {
        if let Some(table) = self.get() {
            return Ok(table);
        }
        // Only initialization is guarded. A panic cannot corrupt a partially
        // published table: the complete value is moved into the cell below.
        let _building = self
            .building
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(table) = self.get() {
            return Ok(table);
        }
        let table = build()?;
        Ok(self.value.get_or_init(|| table))
    }
}

/// One lazily prepared fill table shared by frames with identical geometry.
///
/// Placement, colour and painter order belong to each frame, not this table.
/// Keeping initialization on the first raster worker avoids moving preparation
/// back onto the capture thread. Only successful builds are published: an
/// allocation failure may be retried instead of poisoning the generation.
#[derive(Debug)]
struct SharedMonoTable {
    geometry: GeometryIdentity,
    map: ScreenMap,
    table: LazyTable<MonoTable>,
}

impl SharedMonoTable {
    fn new(plan: &RenderPlan, map: ScreenMap) -> Self {
        Self {
            geometry: plan.geometry_identity(),
            map,
            table: LazyTable::new(),
        }
    }

    fn matches(&self, plan: &RenderPlan, map: ScreenMap) -> bool {
        self.geometry == plan.geometry_identity() && self.map == map
    }

    fn get(&self, plan: &RenderPlan) -> Result<&MonoTable, MonoTableError> {
        self.table
            .get_or_try_init(|| MonoTable::build(plan, self.map))
    }
}

/// Painter-ordered tile commands depend on the full plan, unlike fill geometry.
/// In particular, changing opacity must invalidate prior occlusion pruning.
#[derive(Debug)]
struct SharedBinning {
    plan: PlanIdentity,
    config: RetainedFrameRendererConfig,
    table: LazyTable<Binning>,
}

impl SharedBinning {
    fn new(plan: &RenderPlan, config: RetainedFrameRendererConfig) -> Self {
        Self {
            plan: plan.identity(),
            config,
            table: LazyTable::new(),
        }
    }

    fn matches(&self, plan: &RenderPlan, config: RetainedFrameRendererConfig) -> bool {
        self.plan == plan.identity() && self.config == config
    }

    fn get(&self, plan: &RenderPlan) -> Result<&Binning, BinningError> {
        self.table.get_or_try_init(|| {
            let mut binning = Binning::build(
                plan,
                self.config.frame.viewport,
                self.config.tiling,
                self.config.frame.map,
            )?;
            binning.prune_occluded(plan)?;
            Ok(binning)
        })
    }
}

/// Serial retained compiler whose captured frames own immutable IR generations.
///
/// An unshared plan is synchronized in place. With in-flight readers, static
/// captures retain one mutable working plan and reuse the immutable generation
/// instead of cloning its geometry and compiler scratch on every capture.
/// Changed captures publish that working plan by moving it, not copying it
/// again. Every capture still synchronizes live views and revision metadata;
/// no live arena or callback is made `Send`.
#[derive(Debug)]
pub struct VectorFrameCompiler {
    plan: Arc<RenderPlan>,
    working: Option<RenderPlan>,
    mono: Option<Arc<SharedMonoTable>>,
    binning: Option<Arc<SharedBinning>>,
    config: RetainedFrameRendererConfig,
}

impl VectorFrameCompiler {
    /// Construct without allocating a raw frame or spawning any workers.
    ///
    /// # Errors
    /// Refuses an empty team or an invalid raw-frame layout.
    pub fn new(config: RetainedFrameRendererConfig) -> Result<Self, RetainedFrameRendererError> {
        if config.threads == 0 {
            return Err(RetainedFrameRendererError::InvalidThreads);
        }
        config.frame.layout()?;
        Ok(Self {
            plan: Arc::new(RenderPlan::new()),
            working: None,
            mono: None,
            binning: None,
            config,
        })
    }

    /// Freeze one affine frame after the caller has acquired pipeline capacity.
    ///
    /// Camera-only primitives are refused rather than flattened or omitted.
    /// Geometry/binning errors remain the existing typed Lumen errors.
    ///
    /// # Errors
    /// Refuses invalid records or content requiring the camera route.
    pub fn capture(
        &mut self,
        stage: &Stage,
        camera_revision: u64,
    ) -> Result<OwnedVectorFrame, RetainedFrameRendererError> {
        if let Some(item) = stage
            .draw_plan()
            .items()
            .iter()
            .find(|item| item.key.program != ProgramKind::Vector)
        {
            return Err(RetainedFrameRendererError::CameraRequired {
                program: item.key.program,
            });
        }
        if let Some(plan) = Arc::get_mut(&mut self.plan) {
            // Once the last worker releases this generation, reclaim the
            // authoritative compiler state without copying either plan.
            if let Some(working) = self.working.take() {
                *plan = working;
            }
            plan.sync(stage, camera_revision)?;
        } else {
            let working = self
                .working
                .get_or_insert_with(|| self.plan.as_ref().clone());
            working.sync(stage, camera_revision)?;
            // Content identity alone omits cache volatility, revision folds
            // and inspector handles. Preserve all instance metadata too.
            if working.identity() != self.plan.identity()
                || working.epoch() != self.plan.epoch()
                || working.shapes().instances() != self.plan.shapes().instances()
            {
                self.plan = Arc::new(self.working.take().expect("synchronized working plan"));
            }
        }
        let map = self.config.frame.map;
        let mono = self
            .mono
            .get_or_insert_with(|| Arc::new(SharedMonoTable::new(&self.plan, map)));
        if !mono.matches(&self.plan, map) {
            *mono = Arc::new(SharedMonoTable::new(&self.plan, map));
        }
        let binning = self
            .binning
            .get_or_insert_with(|| Arc::new(SharedBinning::new(&self.plan, self.config)));
        if !binning.matches(&self.plan, self.config) {
            *binning = Arc::new(SharedBinning::new(&self.plan, self.config));
        }
        Ok(OwnedVectorFrame {
            plan: Arc::clone(&self.plan),
            mono: Arc::clone(mono),
            binning: Arc::clone(binning),
            config: self.config,
            camera_revision,
        })
    }

    /// Work counters from the authoritative retained-plan synchronization.
    #[must_use]
    pub fn stats(&self) -> SyncStats {
        match &self.working {
            Some(working) => working.stats(),
            None => self.plan.stats(),
        }
    }
}

/// Immutable compiled affine frame, independent of the source stage's lifetime.
///
/// The scheduler owns the number of these in flight. Geometry is governed by
/// `RenderPlan`'s existing limits, separately from pixel-storage admission.
/// Frames with unchanged geometry share a lazily built monotone fill table;
/// frames with unchanged draw content also share pruned tile-command lists.
/// Each cache retains only its latest generation, plus any in-flight owners.
#[derive(Debug, Clone)]
pub struct OwnedVectorFrame {
    plan: Arc<RenderPlan>,
    mono: Arc<SharedMonoTable>,
    binning: Arc<SharedBinning>,
    config: RetainedFrameRendererConfig,
    camera_revision: u64,
}

impl OwnedVectorFrame {
    /// The RGBA16F surface layout [`Self::render_cached_into`] requires.
    ///
    /// # Errors
    /// Refuses a viewport too large for one frame allocation.
    pub fn raster_layout(&self) -> Result<FrameLayout, FrameError> {
        self.config.frame.layout()
    }

    /// Execute with caller-owned, worker-local scratch and retained pixels.
    ///
    /// Workers may receive nonconsecutive frames: cache keys name content, not
    /// completion order. Each worker must own its arena/cache exclusively.
    /// The returned frame is owned by the next pipeline stage, never by a live
    /// scene. No second rasterizer, sampling policy or color conversion exists.
    ///
    /// # Errors
    /// Preserves all ordinary preparation, binning and cached-render errors.
    pub fn render_cached(
        &self,
        threads: usize,
        arena: &mut FrameArena,
        cache: &mut PixelTileCache,
    ) -> Result<(FrameBuffer, CachedRenderStats), RetainedFrameRendererError> {
        let mut frame = FrameBuffer::new(self.config.frame.layout()?);
        let stats = self.render_cached_into(threads, arena, cache, &mut frame, None)?;
        Ok((frame, stats))
    }

    /// [`Self::render_cached`] into a caller-recycled RGBA16F surface.
    ///
    /// Every pixel is overwritten: reused tiles are restored and every other
    /// tile writes its background before any coverage, so a recycled surface
    /// yields the same bytes as a fresh one. Recycling skips the frame-sized
    /// zeroed allocation (66 MB at 4K) and its page faults on every frame.
    /// Row padding is never written. The surface must have this frame's
    /// RGBA16F layout.
    ///
    /// `resume` is the cache's [`PixelTileCache::serial`] from the render that
    /// last filled `frame`, when nothing has written it since. Reused tiles
    /// are then left in place instead of copied back from the cache. See
    /// [`crate::engine::FrameJob::render_into_cached_resuming`].
    ///
    /// # Errors
    /// As [`Self::render_cached`], plus a layout mismatch for `frame`.
    pub fn render_cached_into(
        &self,
        threads: usize,
        arena: &mut FrameArena,
        cache: &mut PixelTileCache,
        frame: &mut FrameBuffer,
        resume: Option<u64>,
    ) -> Result<CachedRenderStats, RetainedFrameRendererError> {
        if threads == 0 {
            return Err(RetainedFrameRendererError::InvalidThreads);
        }
        let mono = self.mono.get(&self.plan)?;
        let binning = self.binning.get(&self.plan)?;
        let job = FrameJob::with_identity_in(
            arena,
            &self.plan,
            mono,
            binning,
            self.config.frame,
            self.config.engine,
        )?;
        Ok(job.render_into_cached_resuming(threads, frame, self.camera_revision, cache, resume)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BinningLimits, EngineIdentity, FrameConfig, RetainedFrameRenderer, ScreenMap, Tiling,
        Viewport,
    };
    use fmn_core::color::LinearRgba;
    use fmn_mobject::{Mobject, RecordBuffer, RecordSchema, RenderPrimitive, ShapeTag};

    fn config() -> RetainedFrameRendererConfig {
        RetainedFrameRendererConfig {
            frame: FrameConfig::new(
                Viewport {
                    width: 32,
                    height: 24,
                },
                ScreenMap {
                    scale: 4.0,
                    origin: [16.0, 12.0],
                    y_up: true,
                },
                LinearRgba {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
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
        let mut buffer = RecordBuffer::new(RecordSchema::vmobject(), 3).unwrap();
        buffer.write_range(
            "point",
            0,
            &[-1.5, -1.0, 0.0, 0.0, 1.25, 0.0, 1.5, -1.0, 0.0],
        );
        buffer.write_range("fill_rgba", 0, &[0.1, 0.3, 1.0, 0.8].repeat(3));
        Mobject::from_buffer(buffer)
    }

    #[test]
    fn immutable_ir_is_send_sync_and_outlives_stage() {
        fn send_sync<T: Send + Sync>() {}
        send_sync::<OwnedVectorFrame>();
        let mut compiler = VectorFrameCompiler::new(config()).unwrap();
        let frame = {
            let mut stage = Stage::new();
            let mob = stage.add(vector());
            stage.add_to_scene(mob).unwrap();
            compiler.capture(&stage, 0).unwrap()
        };
        drop(compiler);
        std::thread::spawn(move || {
            let (pixels, _) = frame
                .render_cached(2, &mut FrameArena::new(), &mut PixelTileCache::new())
                .unwrap();
            assert_eq!(pixels.layout().width(), 32);
        })
        .join()
        .unwrap();
    }

    #[test]
    fn frozen_frames_match_retained_renderer_after_later_mutation_and_out_of_order_execution() {
        let mut stage = Stage::new();
        let mob = stage.add(vector());
        stage.add_to_scene(mob).unwrap();
        let mut compiler = VectorFrameCompiler::new(config()).unwrap();
        let mut serial = RetainedFrameRenderer::new(config()).unwrap();
        let mut frames = Vec::new();
        let mut expected = Vec::new();
        for _ in 0..5 {
            frames.push(compiler.capture(&stage, 0).unwrap());
            serial.render(&stage, 0).unwrap();
            expected.push(serial.frame().as_bytes().to_vec());
            stage.shift(mob, [0.25, 0.1, 0.0]);
        }
        stage.remove_from_scene(mob);
        assert_ne!(expected[0], expected[4]);
        for threads in [1, 4, 16] {
            let mut arena = FrameArena::new();
            let mut cache = PixelTileCache::new();
            for index in [4, 1, 3, 0, 2, 2] {
                let (pixels, _) = frames[index]
                    .render_cached(threads, &mut arena, &mut cache)
                    .unwrap();
                assert_eq!(pixels.as_bytes(), expected[index]);
            }
        }
    }

    #[test]
    fn static_captures_keep_shape_reuse_and_worker_tile_hits() {
        let mut stage = Stage::new();
        let mob = stage.add(vector());
        stage.add_to_scene(mob).unwrap();
        let mut compiler = VectorFrameCompiler::new(config()).unwrap();
        let mut arena = FrameArena::new();
        let mut cache = PixelTileCache::new();
        let first = compiler.capture(&stage, 0).unwrap();
        assert!(first.mono.table.get().is_none(), "capture must stay lazy");
        assert!(first.binning.table.get().is_none());
        let (before, _) = first.render_cached(1, &mut arena, &mut cache).unwrap();
        let second = compiler.capture(&stage, 0).unwrap();
        assert!(Arc::ptr_eq(&first.mono, &second.mono));
        assert!(Arc::ptr_eq(&first.binning, &second.binning));
        assert_eq!(compiler.stats().shapes_compiled, 0);
        assert_eq!(compiler.stats().shapes_reused, 1);
        let (after, stats) = second.render_cached(4, &mut arena, &mut cache).unwrap();
        assert_eq!(before.as_bytes(), after.as_bytes());
        assert!(stats.cache.hits > 0);
    }

    #[test]
    fn static_captures_share_ir_and_move_working_storage_on_the_next_edit() {
        let mut stage = Stage::new();
        let mob = stage.add(vector());
        stage.add_to_scene(mob).unwrap();
        let mut compiler = VectorFrameCompiler::new(config()).unwrap();
        let first = compiler.capture(&stage, 0).unwrap();
        let second = compiler.capture(&stage, 0).unwrap();
        assert!(Arc::ptr_eq(&first.plan, &second.plan));
        let storage = compiler.working.as_ref().unwrap().segments().as_ptr();
        for _ in 0..4 {
            let frame = compiler.capture(&stage, 0).unwrap();
            assert!(Arc::ptr_eq(&first.plan, &frame.plan));
            assert_eq!(compiler.stats().shapes_compiled, 0);
            assert_eq!(compiler.stats().shapes_reused, 1);
            assert_eq!(
                compiler.working.as_ref().unwrap().segments().as_ptr(),
                storage
            );
        }
        stage.shift(mob, [1.0, 0.0, 0.0]);
        let changed = compiler.capture(&stage, 0).unwrap();
        assert!(!Arc::ptr_eq(&first.plan, &changed.plan));
        assert!(compiler.working.is_none());
        assert_eq!(
            changed.plan.segments().as_ptr(),
            storage,
            "publish must move"
        );
        let mut serial = RetainedFrameRenderer::new(config()).unwrap();
        serial.render(&stage, 0).unwrap();
        let (actual, _) = changed
            .render_cached(4, &mut FrameArena::new(), &mut PixelTileCache::new())
            .unwrap();
        assert_eq!(actual.as_bytes(), serial.frame().as_bytes());
    }

    #[test]
    fn unshared_captures_reclaim_working_state_without_an_ir_copy() {
        let mut stage = Stage::new();
        let mob = stage.add(vector());
        stage.add_to_scene(mob).unwrap();
        let mut compiler = VectorFrameCompiler::new(config()).unwrap();
        let first = compiler.capture(&stage, 0).unwrap();
        let generation = Arc::as_ptr(&first.plan);
        drop(first);
        stage.shift(mob, [0.5, 0.0, 0.0]);
        let second = compiler.capture(&stage, 0).unwrap();
        assert_eq!(Arc::as_ptr(&second.plan), generation);
        assert!(compiler.working.is_none());
        let third = compiler.capture(&stage, 0).unwrap();
        let storage = compiler.working.as_ref().unwrap().segments().as_ptr();
        drop(second);
        drop(third);
        let reclaimed = compiler.capture(&stage, 0).unwrap();
        assert_eq!(Arc::as_ptr(&reclaimed.plan), generation);
        assert_eq!(reclaimed.plan.segments().as_ptr(), storage);
        assert!(compiler.working.is_none());
    }

    #[test]
    fn writable_style_views_preserve_cache_volatility_and_observe_every_edit() {
        let mut stage = Stage::new();
        let mob = stage.add(vector());
        stage.add_to_scene(mob).unwrap();
        let mut compiler = VectorFrameCompiler::new(config()).unwrap();
        let first = compiler.capture(&stage, 0).unwrap();
        let view = stage
            .get_mut(mob)
            .unwrap()
            .buffer
            .export_field_view("fill_rgba", true)
            .unwrap();
        let volatile = compiler.capture(&stage, 0).unwrap();
        assert!(!Arc::ptr_eq(&first.plan, &volatile.plan));
        assert!(!first.plan.shapes().instances()[0].volatile);
        assert!(volatile.plan.shapes().instances()[0].volatile);
        let repeated = compiler.capture(&stage, 0).unwrap();
        assert!(Arc::ptr_eq(&volatile.plan, &repeated.plan));
        assert_eq!(compiler.stats().styles_rebuilt, 1);

        for index in 0..3 {
            assert!(view.write(index, "fill_rgba", &[1.0, 0.1, 0.2, 0.9]));
        }
        let edited = compiler.capture(&stage, 0).unwrap();
        assert!(!Arc::ptr_eq(&volatile.plan, &edited.plan));
        assert!(edited.plan.shapes().instances()[0].volatile);
        let mut serial = RetainedFrameRenderer::new(config()).unwrap();
        serial.render(&stage, 0).unwrap();
        let mut arena = FrameArena::new();
        let mut cache = PixelTileCache::new();
        let (old, _) = first.render_cached(1, &mut arena, &mut cache).unwrap();
        let (actual, _) = edited.render_cached(4, &mut arena, &mut cache).unwrap();
        assert_eq!(actual.as_bytes(), serial.frame().as_bytes());
        assert_ne!(actual.as_bytes(), old.as_bytes());

        drop(view);
        let detached = compiler.capture(&stage, 0).unwrap();
        assert!(!Arc::ptr_eq(&edited.plan, &detached.plan));
        assert!(!detached.plan.shapes().instances()[0].volatile);
        let (after, _) = detached.render_cached(4, &mut arena, &mut cache).unwrap();
        assert_eq!(after.as_bytes(), actual.as_bytes());
    }

    #[test]
    fn identical_replacement_geometry_keeps_the_new_instance_handle() {
        let mut stage = Stage::new();
        let old = stage.add(vector());
        stage.add_to_scene(old).unwrap();
        let mut compiler = VectorFrameCompiler::new(config()).unwrap();
        let first = compiler.capture(&stage, 0).unwrap();
        stage.remove_from_scene(old);
        let new = stage.add(vector());
        stage.add_to_scene(new).unwrap();
        let replacement = compiler.capture(&stage, 0).unwrap();
        assert!(!Arc::ptr_eq(&first.plan, &replacement.plan));
        assert_eq!(first.plan.shapes().instances()[0].mob, old);
        assert_eq!(replacement.plan.shapes().instances()[0].mob, new);
        assert!(Arc::ptr_eq(&first.mono, &replacement.mono));
    }

    #[test]
    fn monotone_geometry_is_shared_across_style_placement_and_order_changes() {
        let mut stage = Stage::new();
        let front = stage.add(vector());
        let back = stage.add(vector());
        stage.add_to_scene(front).unwrap();
        stage.add_to_scene(back).unwrap();
        stage.shift(back, [0.5, 0.0, 0.0]);
        let mut compiler = VectorFrameCompiler::new(config()).unwrap();
        let mut serial = RetainedFrameRenderer::new(config()).unwrap();
        let mut frames = Vec::new();
        let mut expected = Vec::new();
        for step in 0..4 {
            match step {
                1 => assert!(stage.get_mut(front).unwrap().buffer.write_range(
                    "fill_rgba",
                    0,
                    &[0.1, 0.9, 0.2, 0.7].repeat(3),
                )),
                2 => {
                    stage.shift(front, [1.0, 0.0, 0.0]);
                }
                3 => {
                    // z_index takes effect on the next scene add, not at
                    // assignment. Exercise a real painter reorder here.
                    assert_eq!(stage.draw_plan().items()[0].mob, front);
                    stage.set_z_index(front, 1, false);
                    assert_eq!(stage.draw_plan().items()[0].mob, front);
                    stage.add_to_scene(front).unwrap();
                    assert_eq!(stage.draw_plan().items()[0].mob, back);
                }
                _ => {}
            }
            frames.push(compiler.capture(&stage, 0).unwrap());
            serial.render(&stage, 0).unwrap();
            expected.push(serial.frame().as_bytes().to_vec());
        }
        for frame in &frames {
            assert!(Arc::ptr_eq(&frames[0].mono, &frame.mono));
            assert!(frame.mono.table.get().is_none());
            assert!(frame.binning.table.get().is_none());
        }
        for pair in frames.windows(2) {
            assert!(!Arc::ptr_eq(&pair[0].binning, &pair[1].binning));
        }
        assert_ne!(expected[0], expected[1]);
        assert_ne!(expected[1], expected[2]);
        assert_ne!(expected[2], expected[3]);
        for threads in [1, 4, 16] {
            let mut arena = FrameArena::new();
            let mut cache = PixelTileCache::new();
            for index in [3, 0, 2, 1, 3] {
                let (pixels, _) = frames[index]
                    .render_cached(threads, &mut arena, &mut cache)
                    .unwrap();
                assert_eq!(pixels.as_bytes(), expected[index]);
            }
        }
    }

    #[test]
    fn changing_frames_share_interned_outlines_and_detach_only_written_tables() {
        let mut stage = Stage::new();
        let mob = stage.add(vector());
        stage.add_to_scene(mob).unwrap();
        let mut compiler = VectorFrameCompiler::new(config()).unwrap();
        let first = compiler.capture(&stage, 0).unwrap();
        let shape_rows = first.plan.shapes().shapes().as_ptr();
        let style_rows = first.plan.styles().rows().as_ptr();
        let lengths = first.plan.shapes().shapes()[0]
            .arc_length
            .curve_lengths()
            .as_ptr();
        let mut frames = vec![first];
        let mut expected = Vec::new();
        let mut serial = RetainedFrameRenderer::new(config()).unwrap();
        serial.render(&stage, 0).unwrap();
        expected.push(serial.frame().as_bytes().to_vec());

        for step in 0..6 {
            stage.shift(mob, [0.125, 0.0, 0.0]);
            if step == 3 {
                assert!(stage.get_mut(mob).unwrap().buffer.write_range(
                    "fill_rgba",
                    0,
                    &[0.1, 0.8, 0.2, 0.7].repeat(3),
                ));
            }
            let frame = compiler.capture(&stage, 0).unwrap();
            assert!(!Arc::ptr_eq(&frames.last().unwrap().plan, &frame.plan));
            assert_eq!(frame.plan.shapes().shapes().as_ptr(), shape_rows);
            assert_eq!(
                frame.plan.shapes().shapes()[0]
                    .arc_length
                    .curve_lengths()
                    .as_ptr(),
                lengths
            );
            if step < 3 {
                assert_eq!(frame.plan.styles().rows().as_ptr(), style_rows);
            } else {
                assert_ne!(frame.plan.styles().rows().as_ptr(), style_rows);
            }
            assert_eq!(compiler.stats().shapes_compiled, 0);
            frames.push(frame);
            serial.render(&stage, 0).unwrap();
            expected.push(serial.frame().as_bytes().to_vec());
        }
        // A geometry miss detaches the shape table without rewriting any
        // earlier frame's rows or their arc-length storage.
        assert!(
            stage
                .get_mut(mob)
                .unwrap()
                .buffer
                .write_range("point", 1, &[0.0, 2.5, 0.0])
        );
        let changed = compiler.capture(&stage, 0).unwrap();
        assert_ne!(changed.plan.shapes().shapes().as_ptr(), shape_rows);
        assert_eq!(compiler.stats().shapes_compiled, 1);
        frames.push(changed);
        serial.render(&stage, 0).unwrap();
        expected.push(serial.frame().as_bytes().to_vec());
        assert_ne!(expected[0], *expected.last().unwrap());
        for threads in [1, 4, 16] {
            let mut arena = FrameArena::new();
            let mut cache = PixelTileCache::new();
            for index in (0..frames.len()).rev() {
                let (actual, _) = frames[index]
                    .render_cached(threads, &mut arena, &mut cache)
                    .unwrap();
                assert_eq!(actual.as_bytes(), expected[index]);
            }
        }
    }

    #[test]
    fn point_views_and_screen_maps_cannot_reuse_stale_monotone_geometry() {
        let mut stage = Stage::new();
        let mob = stage.add(vector());
        stage.add_to_scene(mob).unwrap();
        let mut compiler = VectorFrameCompiler::new(config()).unwrap();
        let mut serial = RetainedFrameRenderer::new(config()).unwrap();
        let first = compiler.capture(&stage, 0).unwrap();
        serial.render(&stage, 0).unwrap();
        let before = serial.frame().as_bytes().to_vec();

        let view = stage
            .get_mut(mob)
            .unwrap()
            .buffer
            .export_field_view("point", true)
            .unwrap();
        assert!(view.write(1, "point", &[0.0, 2.0, 0.0]));
        let second = compiler.capture(&stage, 0).unwrap();
        serial.render(&stage, 0).unwrap();
        let after = serial.frame().as_bytes().to_vec();
        assert_ne!(before, after);
        assert!(!Arc::ptr_eq(&first.mono, &second.mono));

        let mut moved_config = config();
        moved_config.frame.map.origin[0] += 4.0;
        assert!(!second.mono.matches(&second.plan, moved_config.frame.map));
        let moved = VectorFrameCompiler::new(moved_config)
            .unwrap()
            .capture(&stage, 0)
            .unwrap();
        let mut moved_serial = RetainedFrameRenderer::new(moved_config).unwrap();
        moved_serial.render(&stage, 0).unwrap();
        let moved_expected = moved_serial.frame().as_bytes().to_vec();
        assert_ne!(after, moved_expected);
        drop(view);
        drop(stage);
        drop(compiler);

        let mut arena = FrameArena::new();
        let mut cache = PixelTileCache::new();
        for (frame, expected) in [
            (&moved, &moved_expected),
            (&second, &after),
            (&first, &before),
        ] {
            let (pixels, _) = frame.render_cached(4, &mut arena, &mut cache).unwrap();
            assert_eq!(pixels.as_bytes(), expected);
        }
    }

    #[test]
    fn concurrent_frames_initialize_shared_preparation() {
        let mut stage = Stage::new();
        let mob = stage.add(vector());
        stage.add_to_scene(mob).unwrap();
        let mut compiler = VectorFrameCompiler::new(config()).unwrap();
        let frames: Vec<_> = (0..4)
            .map(|_| compiler.capture(&stage, 0).unwrap())
            .collect();
        let mut serial = RetainedFrameRenderer::new(config()).unwrap();
        serial.render(&stage, 0).unwrap();
        let expected = serial.frame().as_bytes().to_vec();
        assert!(frames[0].mono.table.get().is_none());
        assert!(frames[0].binning.table.get().is_none());
        let barrier = std::sync::Barrier::new(frames.len());
        std::thread::scope(|scope| {
            let workers: Vec<_> = frames
                .iter()
                .map(|frame| {
                    let barrier = &barrier;
                    scope.spawn(move || {
                        barrier.wait();
                        let table = frame.mono.get(&frame.plan).unwrap();
                        let (pixels, _) = frame
                            .render_cached(1, &mut FrameArena::new(), &mut PixelTileCache::new())
                            .unwrap();
                        (table, frame.binning.table.get().unwrap(), pixels)
                    })
                })
                .collect();
            for worker in workers {
                let (table, binning, pixels) = worker.join().unwrap();
                assert!(std::ptr::eq(table, frames[0].mono.table.get().unwrap()));
                assert!(std::ptr::eq(
                    binning,
                    frames[0].binning.table.get().unwrap()
                ));
                assert_eq!(pixels.as_bytes(), expected);
            }
        });
    }

    #[test]
    fn failed_preparation_is_retryable_and_success_is_published_once() {
        let table = LazyTable::new();
        let plan = RenderPlan::new();
        let config = config();
        let rejected = table.get_or_try_init(|| {
            Binning::build_with_limits(
                &plan,
                config.frame.viewport,
                config.tiling,
                config.frame.map,
                BinningLimits {
                    max_tiles: 0,
                    ..BinningLimits::default()
                },
            )
        });
        assert!(matches!(rejected, Err(BinningError::LimitExceeded { .. })));
        assert!(table.get().is_none());
        let prepared = table
            .get_or_try_init(|| {
                Binning::build(
                    &plan,
                    config.frame.viewport,
                    config.tiling,
                    config.frame.map,
                )
            })
            .unwrap();
        let reused = table
            .get_or_try_init::<BinningError>(|| panic!("published tables must not be rebuilt"))
            .unwrap();
        assert!(std::ptr::eq(prepared, reused));
    }

    #[test]
    fn opacity_changes_restore_commands_pruned_by_an_opaque_foreground() {
        let mut stage = Stage::new();
        let back = stage.add(vector());
        stage.add_to_scene(back).unwrap();
        let points = [
            [-10.0, -10.0, 0.0],
            [0.0, -10.0, 0.0],
            [10.0, -10.0, 0.0],
            [10.0, 0.0, 0.0],
            [10.0, 10.0, 0.0],
            [0.0, 10.0, 0.0],
            [-10.0, 10.0, 0.0],
            [-10.0, 0.0, 0.0],
            [-10.0, -10.0, 0.0],
        ];
        let mut buffer = RecordBuffer::new(RecordSchema::vmobject(), points.len()).unwrap();
        for (index, point) in points.iter().enumerate() {
            assert!(buffer.write(index, "point", point));
            assert!(buffer.write(index, "fill_rgba", &[1.0, 0.1, 0.1, 1.0]));
        }
        let front = stage.add(Mobject::from_buffer(buffer));
        stage.set_shape(
            front,
            ShapeTag::Rect {
                center: [0.0, 0.0, 0.0],
                width: 20.0,
                height: 20.0,
            },
        );
        stage.add_to_scene(front).unwrap();
        let mut compiler = VectorFrameCompiler::new(config()).unwrap();
        let mut serial = RetainedFrameRenderer::new(config()).unwrap();
        let opaque = compiler.capture(&stage, 0).unwrap();
        serial.render(&stage, 0).unwrap();
        let before = serial.frame().as_bytes().to_vec();
        let unpruned = Binning::build(
            &opaque.plan,
            config().frame.viewport,
            config().tiling,
            config().frame.map,
        )
        .unwrap();
        let pruned = opaque.binning.get(&opaque.plan).unwrap();
        assert!(pruned.draws().len() < unpruned.draws().len());

        assert!(stage.get_mut(front).unwrap().buffer.write_range(
            "fill_rgba",
            0,
            &[1.0, 0.1, 0.1, 0.25].repeat(points.len()),
        ));
        let translucent = compiler.capture(&stage, 0).unwrap();
        assert!(Arc::ptr_eq(&opaque.mono, &translucent.mono));
        assert!(!Arc::ptr_eq(&opaque.binning, &translucent.binning));
        let restored = translucent.binning.get(&translucent.plan).unwrap();
        assert!(restored.draws().len() > pruned.draws().len());
        serial.render(&stage, 0).unwrap();
        let after = serial.frame().as_bytes().to_vec();
        assert_ne!(before, after);
        let mut arena = FrameArena::new();
        let mut cache = PixelTileCache::new();
        for (frame, expected) in [(&translucent, &after), (&opaque, &before)] {
            let (pixels, _) = frame.render_cached(4, &mut arena, &mut cache).unwrap();
            assert_eq!(pixels.as_bytes(), expected);
        }
    }

    #[test]
    fn callback_free_production_handoffs_reuse_static_preparation_on_a_worker() {
        let mut stage = Stage::new();
        let mob = stage.add(vector());
        stage.add_to_scene(mob).unwrap();
        let first = stage.freeze_render_state();
        let second = stage.freeze_render_state();
        let mut serial = RetainedFrameRenderer::new(config()).unwrap();
        serial.render(&stage, 0).unwrap();
        let before = serial.frame().as_bytes().to_vec();
        stage.shift(mob, [1.0, 0.0, 0.0]);
        let third = stage.freeze_render_state();
        serial.render(&stage, 0).unwrap();
        let after = serial.frame().as_bytes().to_vec();
        drop(stage);
        let frames: Vec<_> = std::thread::spawn(move || {
            let mut compiler = VectorFrameCompiler::new(config()).unwrap();
            [first, second, third]
                .into_iter()
                .map(|state| compiler.capture(&state.materialize(), 0).unwrap())
                .collect::<Vec<_>>()
        })
        .join()
        .unwrap();
        assert!(Arc::ptr_eq(&frames[0].plan, &frames[1].plan));
        assert!(Arc::ptr_eq(&frames[0].binning, &frames[1].binning));
        assert!(!Arc::ptr_eq(&frames[1].binning, &frames[2].binning));
        assert!(Arc::ptr_eq(&frames[0].mono, &frames[2].mono));
        assert_ne!(before, after);
        let mut arena = FrameArena::new();
        let mut cache = PixelTileCache::new();
        for (index, expected) in [(2, &after), (0, &before), (1, &before)] {
            let (pixels, _) = frames[index]
                .render_cached(4, &mut arena, &mut cache)
                .unwrap();
            assert_eq!(pixels.as_bytes(), expected);
        }
    }

    #[test]
    fn camera_content_and_zero_teams_are_not_silently_accepted() {
        let mut invalid = config();
        invalid.threads = 0;
        assert!(matches!(
            VectorFrameCompiler::new(invalid),
            Err(RetainedFrameRendererError::InvalidThreads)
        ));
        let mut stage = Stage::new();
        let mob = stage.add(vector().with_render_primitive(RenderPrimitive::DotCloud));
        stage.add_to_scene(mob).unwrap();
        let mut compiler = VectorFrameCompiler::new(config()).unwrap();
        assert!(matches!(
            compiler.capture(&stage, 0),
            Err(RetainedFrameRendererError::CameraRequired { .. })
        ));
    }
}
