//! Reusable retained CPU-frame composition for native front doors.
//!
//! Lumen owns the synchronization, binning, frame-job, arena, and tile-cache
//! sequence. CLI, Studio, WASM, and Python adapters decide where the returned
//! raw frame goes; none of them should copy this orchestration or grow a second
//! semantic renderer.

use std::fmt;
use std::sync::{Arc, Mutex, PoisonError, Weak};

use fmn_core::color::LinearRgba;
use fmn_core::types::Vec3;
use fmn_frame::{FrameBuffer, FrameError};
use fmn_mobject::record::StorageIdentity;
use fmn_mobject::{
    DrawItem, DrawPlan, Entry, ImageResource, Mob, Placement, ProgramKind, RenderPrimitive, Stage,
    Uniforms,
};

use crate::three_d::{PreparedThreeDProjection, same_projection_camera};
use crate::{
    Binning, BinningError, CachedRenderError, CachedRenderStats, Camera, EngineIdentity,
    FrameArena, FrameConfig, FrameJob, FrameJobError, MonoTable, MonoTableError, PixelTileCache,
    RenderPlan, SurfaceDraw, SurfaceMaterial, SurfaceMesh, SurfaceVertex, SyncError, ThreeDDraw,
    ThreeDError, ThreeDJob, Tiling, TrueDotDraw, VectorDraw,
};

/// Immutable policy for one retained CPU renderer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RetainedFrameRendererConfig {
    /// Viewport, mapping, background, and AA policy.
    pub frame: FrameConfig,
    /// Two-level tile geometry.
    pub tiling: Tiling,
    /// Certified-CPU or fast-CPU identity.
    pub engine: EngineIdentity,
    /// Fixed worker-team width selected by the composition root.
    pub threads: usize,
}

/// Construction or frame-preparation failure at the shared Lumen boundary.
#[derive(Debug)]
pub enum RetainedFrameRendererError {
    /// A renderer cannot execute with an empty worker team.
    InvalidThreads,
    /// The configured raw-frame layout is invalid.
    Layout(FrameError),
    /// Retained-plan synchronization failed.
    Sync(SyncError),
    /// Monotone fill-table preparation failed.
    Mono(MonoTableError),
    /// Tile binning or painter-safe pruning failed.
    Binning(BinningError),
    /// The selected CPU frame job could not be prepared.
    Prepare(FrameJobError),
    /// Rasterization or retained-cache publication failed.
    Render(CachedRenderError),
    /// An affine-only capture encountered camera-bound content.
    CameraRequired {
        /// First renderer program requiring the camera route.
        program: ProgramKind,
    },
    /// Arena record data did not match its durable renderer metadata.
    InvalidPrimitive {
        /// Offending arena handle.
        mob: Mob,
        /// Stable refusal reason.
        reason: &'static str,
    },
    /// Camera-route staging storage could not be reserved atomically.
    AllocationFailed {
        /// Named staging table.
        resource: &'static str,
        /// Exact row count requested.
        requested: usize,
    },
    /// The mixed painter sequence disagreed with the retained vector table.
    VectorPlanMismatch,
    /// Camera-bound preparation rejected a surface/vector/dot command.
    ThreeD(ThreeDError),
    /// Camera-bound rasterization rejected the destination or worker team.
    ThreeDFrame(FrameError),
}

impl fmt::Display for RetainedFrameRendererError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidThreads => {
                f.write_str("retained CPU rendering requires at least one thread")
            }
            Self::Layout(error) => error.fmt(f),
            Self::Sync(error) => error.fmt(f),
            Self::Mono(error) => error.fmt(f),
            Self::Binning(error) => error.fmt(f),
            Self::Prepare(error) => error.fmt(f),
            Self::Render(error) => error.fmt(f),
            Self::CameraRequired { program } => write!(
                f,
                "{program:?} content requires RetainedFrameRenderer::render_with_camera"
            ),
            Self::InvalidPrimitive { mob, reason } => {
                write!(f, "invalid retained primitive for {mob:?}: {reason}")
            }
            Self::AllocationFailed {
                resource,
                requested,
            } => write!(
                f,
                "retained camera route could not reserve {requested} {resource} rows"
            ),
            Self::VectorPlanMismatch => {
                f.write_str("mixed painter sequence does not match retained vector instances")
            }
            Self::ThreeD(error) => error.fmt(f),
            Self::ThreeDFrame(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for RetainedFrameRendererError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidThreads => None,
            Self::Layout(error) => Some(error),
            Self::Sync(error) => Some(error),
            Self::Mono(error) => Some(error),
            Self::Binning(error) => Some(error),
            Self::Prepare(error) => Some(error),
            Self::Render(error) => Some(error),
            Self::CameraRequired { .. }
            | Self::InvalidPrimitive { .. }
            | Self::AllocationFailed { .. }
            | Self::VectorPlanMismatch => None,
            Self::ThreeD(error) => Some(error),
            Self::ThreeDFrame(error) => Some(error),
        }
    }
}

impl From<FrameError> for RetainedFrameRendererError {
    fn from(error: FrameError) -> Self {
        Self::Layout(error)
    }
}
impl From<SyncError> for RetainedFrameRendererError {
    fn from(error: SyncError) -> Self {
        Self::Sync(error)
    }
}
impl From<MonoTableError> for RetainedFrameRendererError {
    fn from(error: MonoTableError) -> Self {
        Self::Mono(error)
    }
}
impl From<BinningError> for RetainedFrameRendererError {
    fn from(error: BinningError) -> Self {
        Self::Binning(error)
    }
}
impl From<FrameJobError> for RetainedFrameRendererError {
    fn from(error: FrameJobError) -> Self {
        Self::Prepare(error)
    }
}
impl From<CachedRenderError> for RetainedFrameRendererError {
    fn from(error: CachedRenderError) -> Self {
        Self::Render(error)
    }
}

/// Lumen state retained across immutable scene captures.
///
/// The first frame sizes the compiled plan, bump arena, and pixel cache. Later
/// frames synchronize only changed Marionette revisions and restore unchanged
/// tiles byte-for-byte before rasterizing dirty work.
#[derive(Debug)]
pub struct RetainedFrameRenderer {
    config: RetainedFrameRendererConfig,
    plan: RenderPlan,
    arena: FrameArena,
    cache: PixelTileCache,
    frame: FrameBuffer,
    camera_scene: Option<CachedCameraScene>,
    camera_plan: Option<Arc<RenderPlan>>,
    camera_projection: Option<PreparedCameraFrame>,
    // Identity of the successful camera raster currently in `frame`, not the
    // last prepared job. A weak key adds no framebuffer or geometry ownership
    // and cannot alias a subsequently recycled projection allocation.
    camera_pixels: Weak<CameraProjection>,
    last_camera_frame_reused: bool,
}

impl RetainedFrameRenderer {
    /// Construct an empty retained renderer for the declared CPU policy.
    ///
    /// # Errors
    /// Refuses zero threads, invalid frame geometry, or an unrepresentable raw
    /// frame layout. Annex identities are rejected by the first render through
    /// the ordinary typed [`FrameJobError::UnsupportedEngine`] boundary.
    pub fn new(config: RetainedFrameRendererConfig) -> Result<Self, RetainedFrameRendererError> {
        if config.threads == 0 {
            return Err(RetainedFrameRendererError::InvalidThreads);
        }
        let frame = FrameBuffer::new(config.frame.layout()?);
        Ok(Self {
            config,
            plan: RenderPlan::new(),
            arena: FrameArena::new(),
            cache: PixelTileCache::new(),
            frame,
            camera_scene: None,
            camera_plan: None,
            camera_projection: None,
            camera_pixels: Weak::new(),
            last_camera_frame_reused: false,
        })
    }

    /// Replace the planar route's background. Cached tiles hold composited
    /// background pixels, so a change discards them; an unchanged background
    /// keeps every cache hit.
    pub fn set_background(&mut self, background: LinearRgba) {
        if self.config.frame.background != background {
            self.config.frame.background = background;
            self.cache = PixelTileCache::new();
        }
    }

    /// Synchronize one immutable stage and rasterize its retained raw frame.
    ///
    /// `camera_revision` is independent of frame sequence. A fixed 2D camera
    /// therefore passes zero on every call and preserves cache hits.
    pub fn render(
        &mut self,
        stage: &Stage,
        camera_revision: u64,
    ) -> Result<CachedRenderStats, RetainedFrameRendererError> {
        self.last_camera_frame_reused = false;
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
        self.camera_pixels = Weak::new();
        self.camera_scene = None;
        self.camera_plan = None;
        self.camera_projection = None;
        self.plan.sync(stage, camera_revision)?;
        let mono = MonoTable::build(&self.plan, self.config.frame.map)?;
        let mut binning = Binning::build(
            &self.plan,
            self.config.frame.viewport,
            self.config.tiling,
            self.config.frame.map,
        )?;
        binning.prune_occluded(&self.plan)?;
        Ok(FrameJob::with_identity_in(
            &mut self.arena,
            &self.plan,
            &mono,
            &binning,
            self.config.frame,
            self.config.engine,
        )?
        .render_into_cached(
            self.config.threads,
            &mut self.frame,
            camera_revision,
            &mut self.cache,
        )?)
    }

    /// Synchronize vector records, retain camera-bound surface/dot commands,
    /// and rasterize their one shared painter sequence through [`ThreeDJob`].
    ///
    /// Surface topology comes only from [`RenderPrimitive`]; record counts are
    /// never factorized or guessed. The 2D retained plan remains the sole owner
    /// of vector geometry, so this route does not create a second path compiler.
    /// Identical consecutive captures reuse the completed pixels in place.
    /// Synchronization still runs, including conservative writable-view checks.
    pub fn render_with_camera(
        &mut self,
        stage: &Stage,
        camera: &Camera,
    ) -> Result<(), RetainedFrameRendererError> {
        self.last_camera_frame_reused = false;
        let prepared = self.prepare_with_camera(stage, camera)?;
        if self
            .camera_pixels
            .upgrade()
            .is_some_and(|previous| Arc::ptr_eq(&previous, &prepared.projection))
        {
            self.last_camera_frame_reused = true;
            return Ok(());
        }
        // A failed raster may have partially written the destination. Never
        // leave a success key installed across a fallible write.
        self.camera_pixels = Weak::new();
        prepared.render_into(self.config.threads, &mut self.frame)?;
        self.camera_pixels = Arc::downgrade(&prepared.projection);
        Ok(())
    }

    /// Whether the most recent render call reused a complete camera frame
    /// without rasterization. Preparation alone does not change this flag.
    #[must_use]
    pub const fn last_camera_frame_reused(&self) -> bool {
        self.last_camera_frame_reused
    }

    /// Freeze a camera-bound frame for worker-owned rasterization.
    ///
    /// Call on the live Stage's owner after its capture barrier. Geometry,
    /// painter order, uniforms, image resources, and camera state are captured
    /// now; subsequent edits and scene teardown cannot affect the job. No
    /// arena, callback, or Python proxy is transferred. Rasterization shares
    /// the implementation used by [`Self::render_with_camera`].
    ///
    /// Acquire a runtime source permit BEFORE this call so frozen geometry is
    /// included in the bounded frame count.
    ///
    /// # Errors
    /// Returns the same record/topology validation errors as the synchronous
    /// route. Projection is prepared on the first raster worker so capture can
    /// overlap earlier frames; subsequent captures and replays share it.
    pub fn prepare_with_camera(
        &mut self,
        stage: &Stage,
        camera: &Camera,
    ) -> Result<PreparedCameraFrame, RetainedFrameRendererError> {
        let scene = self.prepare_camera_scene(stage, camera)?;
        if !self
            .camera_plan
            .as_ref()
            .is_some_and(|plan| same_camera_plan(plan, &self.plan))
        {
            self.camera_plan = Some(Arc::new(self.plan.clone()));
        }
        let plan = Arc::clone(self.camera_plan.as_ref().expect("published camera plan"));
        if let Some(previous) = &self.camera_projection
            && Arc::ptr_eq(&previous.scene, &scene)
            && Arc::ptr_eq(&previous.plan, &plan)
            && same_projection_camera(&previous.projection.camera, camera)
        {
            return Ok(previous.clone());
        }
        self.camera_projection = None;
        let prepared = PreparedCameraFrame {
            plan,
            scene,
            projection: Arc::new(CameraProjection {
                camera: camera.clone(),
                tiling: self.config.tiling,
                prepared: Mutex::new(None),
            }),
        };
        // Retain only the most recent projection. Older captures remain alive
        // solely while callers hold their bounded in-flight frame permits.
        self.camera_projection = Some(prepared.clone());
        Ok(prepared)
    }

    fn prepare_camera_scene(
        &mut self,
        stage: &Stage,
        camera: &Camera,
    ) -> Result<Arc<PreparedCameraScene>, RetainedFrameRendererError> {
        // Always synchronize the authoritative plan, including its latest
        // revision diagnostics. Camera motion does not change world geometry.
        self.plan.sync(stage, camera.revision())?;
        let draw_plan = stage.draw_plan();
        if let Some(cached) = &self.camera_scene
            && cached.matches(stage, &draw_plan)
        {
            return Ok(Arc::clone(&cached.scene));
        }
        let scene = Arc::new(prepare_camera_scene(&mut self.plan, stage, &draw_plan)?);
        let entries = camera_scene_entries(stage, &draw_plan)?;
        self.plan.commit_image_frame(scene.images.clone());
        self.camera_scene = Some(CachedCameraScene {
            entries,
            scene: Arc::clone(&scene),
        });
        Ok(scene)
    }

    /// Current raw linear-light RGBA16F frame.
    #[must_use]
    pub const fn frame(&self) -> &FrameBuffer {
        &self.frame
    }
    /// Declared renderer policy, including the journaled engine identity.
    #[must_use]
    pub const fn config(&self) -> RetainedFrameRendererConfig {
        self.config
    }
    /// Retained compiled plan for diagnostics and overlays.
    #[must_use]
    pub const fn plan(&self) -> &RenderPlan {
        &self.plan
    }
    /// Retained pixel cache for diagnostics and profiling.
    #[must_use]
    pub const fn cache(&self) -> &PixelTileCache {
        &self.cache
    }
}

/// Owned immutable Lumen input suitable for native frame-stage overlap.
///
/// Contains no `Stage`, record views, or frame-clock callbacks. It can be moved
/// to a worker or replayed with different worker counts using identical
/// camera projection, texture sampling, lighting, and painter order.
#[derive(Debug, Clone)]
pub struct PreparedCameraFrame {
    plan: Arc<RenderPlan>,
    scene: Arc<PreparedCameraScene>,
    projection: Arc<CameraProjection>,
}

impl PreparedCameraFrame {
    /// Whether two captures share the complete immutable raster input.
    ///
    /// A true result permits reusing a successful raw frame, regardless of
    /// worker count. A false result is conservative, not proof of different
    /// pixels. The identity is process-local and must not be serialized or
    /// substituted for a provenance hash. It includes scene resources and
    /// every camera value checked by the ordinary preparation cache.
    #[must_use]
    pub fn same_capture(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.projection, &other.projection)
    }

    /// Frozen raw linear-light RGBA16F layout.
    ///
    /// # Errors
    /// Refuses an unrepresentable framebuffer layout.
    pub fn layout(&self) -> Result<fmn_frame::FrameLayout, FrameError> {
        fmn_frame::FrameLayout::tight(
            fmn_frame::PixelFormat::Rgba16F,
            self.projection.camera.pixel_width(),
            self.projection.camera.pixel_height(),
        )
    }

    /// Rasterize on the receiving worker into an existing raw frame.
    ///
    /// # Errors
    /// Refuses invalid destination geometry, empty teams, and native renderer
    /// preparation errors. Does not publish an output artifact.
    pub fn render_into(
        &self,
        threads: usize,
        frame: &mut FrameBuffer,
    ) -> Result<(), RetainedFrameRendererError> {
        if threads == 0 {
            return Err(RetainedFrameRendererError::InvalidThreads);
        }
        let draws = camera_draws(&self.plan, &self.scene)?;
        self.projection
            .get_or_prepare(&draws)
            .and_then(|projection| projection.bind(&self.projection.camera, &draws))
            .map_err(RetainedFrameRendererError::ThreeD)?
            .render_into(threads, frame)
            .map_err(RetainedFrameRendererError::ThreeDFrame)
    }

    /// Rasterize into a new worker-owned raw frame.
    ///
    /// # Errors
    /// Refuses empty teams before allocation and propagates layout/raster errors.
    pub fn render(&self, threads: usize) -> Result<FrameBuffer, RetainedFrameRendererError> {
        if threads == 0 {
            return Err(RetainedFrameRendererError::InvalidThreads);
        }
        let mut frame = FrameBuffer::new(self.layout()?);
        self.render_into(threads, &mut frame)?;
        Ok(frame)
    }
}

/// One lazily prepared immutable projection, shared by identical captures.
/// Initialization runs on the first raster worker, retaining native stage
/// overlap. Serialize only that first preparation and publish only success so
/// transient allocation failures remain retryable. The returned job owns its
/// shared rows; it never holds this initialization lock while rasterizing.
#[derive(Debug)]
struct CameraProjection {
    camera: Camera,
    tiling: Tiling,
    prepared: Mutex<Option<Arc<PreparedThreeDProjection>>>,
}

impl CameraProjection {
    fn get_or_prepare(
        &self,
        draws: &[ThreeDDraw<'_>],
    ) -> Result<Arc<PreparedThreeDProjection>, ThreeDError> {
        let mut prepared = self.prepared.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(projection) = &*prepared {
            return Ok(Arc::clone(projection));
        }
        let projection =
            Arc::new(ThreeDJob::new(&self.camera, draws, self.tiling)?.into_projection()?);
        *prepared = Some(Arc::clone(&projection));
        Ok(projection)
    }
}

#[derive(Debug)]
struct PreparedCameraScene {
    images: crate::ImageTable,
    meshes: Vec<SurfaceMesh>,
    commands: Vec<PreparedCommand>,
}

// The camera rasterizer reads geometry, styles and instance metadata, but not
// the tile-cache revision fold. Each packet owns its complete camera state.
// Ignore only that fold so camera motion can share immutable vector resources
// without copying the retained map and instance array for every in-flight frame.
fn same_camera_plan(previous: &RenderPlan, current: &RenderPlan) -> bool {
    previous.identity() == current.identity()
        && previous.epoch() == current.epoch()
        && previous.shapes().instances().len() == current.shapes().instances().len()
        && previous
            .shapes()
            .instances()
            .iter()
            .zip(current.shapes().instances())
            .all(|(before, after)| {
                let mut before = *before;
                before.revisions = after.revisions;
                before == *after
            })
        && previous.images().len() == current.images().len()
        && previous
            .images()
            .rows()
            .iter()
            .zip(current.images().rows())
            .all(|(before, after)| before.descriptor() == after.descriptor())
}

#[derive(Debug)]
struct CachedCameraScene {
    entries: Vec<CameraSceneEntry>,
    scene: Arc<PreparedCameraScene>,
}

impl CachedCameraScene {
    fn matches(&self, stage: &Stage, draw_plan: &DrawPlan) -> bool {
        self.entries.len() == draw_plan.items().len()
            && self
                .entries
                .iter()
                .zip(draw_plan.items())
                .all(|(cached, item)| {
                    cached.item == *item
                        && stage
                            .get(item.mob)
                            .is_some_and(|entry| cached.matches(entry))
                })
    }
}

#[derive(Debug)]
struct CameraSceneEntry {
    item: DrawItem,
    storage: StorageIdentity,
    record_revision: u64,
    records: usize,
    primitive: RenderPrimitive,
    placement_revision: u64,
    placement: Placement,
    uniforms: Uniforms,
    image: Option<ImageResource>,
}

impl CameraSceneEntry {
    fn matches(&self, entry: &Entry) -> bool {
        self.storage.matches(&entry.buffer)
            && self.record_revision == entry.buffer.revision()
            && self.records == entry.buffer.len()
            && self.primitive == entry.render_primitive()
            && self.placement_revision == entry.placement_revision()
            && self.placement == entry.placement()
            && same_camera_uniforms(&self.uniforms, entry.uniforms())
            && self.image.as_ref() == entry.image_resource()
            // Foreign writes can bypass revision increments. Snapshot buffers
            // preserve these flags too; all camera record fields participate.
            && !entry
                .buffer
                .schema()
                .fields()
                .iter()
                .any(|field| entry.buffer.writable_view_affects(&field.name))
    }
}

fn same_camera_uniforms(before: &Uniforms, after: &Uniforms) -> bool {
    before == after
        && before.is_fixed_in_frame.to_bits() == after.is_fixed_in_frame.to_bits()
        && before.shading.map(f64::to_bits) == after.shading.map(f64::to_bits)
        && before.clip_planes.map(|plane| plane.map(f64::to_bits))
            == after.clip_planes.map(|plane| plane.map(f64::to_bits))
        && before.anti_alias_width.to_bits() == after.anti_alias_width.to_bits()
}

fn camera_scene_entries(
    stage: &Stage,
    draw_plan: &DrawPlan,
) -> Result<Vec<CameraSceneEntry>, RetainedFrameRendererError> {
    let mut entries = Vec::new();
    entries
        .try_reserve_exact(draw_plan.items().len())
        .map_err(|_| RetainedFrameRendererError::AllocationFailed {
            resource: "camera-scene-entry",
            requested: draw_plan.items().len(),
        })?;
    for item in draw_plan.items() {
        let entry = stage
            .get(item.mob)
            .ok_or(RetainedFrameRendererError::InvalidPrimitive {
                mob: item.mob,
                reason: "draw-plan handle is stale",
            })?;
        entries.push(CameraSceneEntry {
            item: *item,
            storage: entry.buffer.storage_identity(),
            record_revision: entry.buffer.revision(),
            records: entry.buffer.len(),
            primitive: entry.render_primitive(),
            placement_revision: entry.placement_revision(),
            placement: entry.placement(),
            uniforms: *entry.uniforms(),
            image: entry.image_resource().cloned(),
        });
    }
    Ok(entries)
}

fn prepare_camera_scene(
    plan: &mut RenderPlan,
    stage: &Stage,
    draw_plan: &DrawPlan,
) -> Result<PreparedCameraScene, RetainedFrameRendererError> {
    let mut image_frame = plan.prepare_image_frame()?;
    let mut meshes = Vec::new();
    let mut commands = Vec::new();
    let mut command_count = 0usize;
    let mut mesh_count = 0usize;
    for item in draw_plan.items() {
        let entry = stage
            .get(item.mob)
            .ok_or(RetainedFrameRendererError::InvalidPrimitive {
                mob: item.mob,
                reason: "draw-plan handle is stale",
            })?;
        let added_commands = match entry.render_primitive() {
            RenderPrimitive::DotCloud => entry.buffer.len(),
            _ => 1,
        };
        command_count = command_count.checked_add(added_commands).ok_or(
            RetainedFrameRendererError::InvalidPrimitive {
                mob: item.mob,
                reason: "camera draw count overflows usize",
            },
        )?;
        if matches!(
            entry.render_primitive(),
            RenderPrimitive::SurfaceGrid { .. }
                | RenderPrimitive::TriangleMesh
                | RenderPrimitive::ImageQuad
        ) {
            mesh_count =
                mesh_count
                    .checked_add(1)
                    .ok_or(RetainedFrameRendererError::InvalidPrimitive {
                        mob: item.mob,
                        reason: "surface mesh count overflows usize",
                    })?;
        }
    }
    meshes.try_reserve_exact(mesh_count).map_err(|_| {
        RetainedFrameRendererError::AllocationFailed {
            resource: "surface-mesh",
            requested: mesh_count,
        }
    })?;
    commands.try_reserve_exact(command_count).map_err(|_| {
        RetainedFrameRendererError::AllocationFailed {
            resource: "prepared-command",
            requested: command_count,
        }
    })?;
    let mut vector_instance = 0u32;
    for item in draw_plan.items() {
        let entry = stage
            .get(item.mob)
            .ok_or(RetainedFrameRendererError::InvalidPrimitive {
                mob: item.mob,
                reason: "draw-plan handle is stale",
            })?;
        match entry.render_primitive() {
            RenderPrimitive::Vector => {
                commands.push(PreparedCommand::Vector(vector_instance));
                vector_instance = vector_instance.checked_add(1).ok_or(
                    RetainedFrameRendererError::InvalidPrimitive {
                        mob: item.mob,
                        reason: "vector instance index exceeds u32",
                    },
                )?;
            }
            RenderPrimitive::SurfaceGrid { .. } | RenderPrimitive::TriangleMesh => {
                let resolution = match entry.render_primitive() {
                    RenderPrimitive::SurfaceGrid { resolution } => Some(resolution),
                    _ => None,
                };
                // Raster resources do not change a surface's topology.
                let image = entry
                    .image_resource()
                    .map(|resource| image_frame.intern(resource).map_err(SyncError::from))
                    .transpose()?;
                let dark_image = entry
                    .image_resource()
                    .and_then(fmn_mobject::ImageResource::dark_image)
                    .map(|resource| image_frame.intern(resource).map_err(SyncError::from))
                    .transpose()?;
                let mesh = surface_mesh(stage, item.mob, resolution)?;
                let mesh_index = meshes.len();
                meshes.push(mesh);
                commands.push(PreparedCommand::Surface {
                    mesh: mesh_index,
                    image,
                    dark_image,
                    uniforms: *entry.uniforms(),
                });
            }
            RenderPrimitive::DotCloud => {
                commands.extend(
                    dot_draws(stage, item.mob)?
                        .into_iter()
                        .map(PreparedCommand::Dot),
                );
            }
            RenderPrimitive::ImageQuad => {
                let resource =
                    entry
                        .image_resource()
                        .ok_or(RetainedFrameRendererError::InvalidPrimitive {
                            mob: item.mob,
                            reason: "image primitive has no image resource",
                        })?;
                if resource.dark_image().is_some() {
                    return Err(RetainedFrameRendererError::InvalidPrimitive {
                        mob: item.mob,
                        reason: "light/dark textures require a surface or triangle mesh",
                    });
                }
                let image = image_frame.intern(resource).map_err(SyncError::from)?;
                let mesh = image_mesh(stage, item.mob)?;
                let mesh_index = meshes.len();
                meshes.push(mesh);
                commands.push(PreparedCommand::Image {
                    mesh: mesh_index,
                    image,
                    uniforms: *entry.uniforms(),
                });
            }
        }
    }
    if usize::try_from(vector_instance).ok() != Some(plan.shapes().instances().len()) {
        return Err(RetainedFrameRendererError::VectorPlanMismatch);
    }
    Ok(PreparedCameraScene {
        images: image_frame,
        meshes,
        commands,
    })
}

fn camera_draws<'a>(
    plan: &'a RenderPlan,
    scene: &'a PreparedCameraScene,
) -> Result<Vec<ThreeDDraw<'a>>, RetainedFrameRendererError> {
    let PreparedCameraScene {
        images: image_frame,
        meshes,
        commands,
    } = scene;
    let mut draws = Vec::new();
    draws.try_reserve_exact(commands.len()).map_err(|_| {
        RetainedFrameRendererError::AllocationFailed {
            resource: "draw-reference",
            requested: commands.len(),
        }
    })?;
    for command in commands {
        draws.push(match *command {
            PreparedCommand::Vector(instance) => {
                ThreeDDraw::Vector(VectorDraw::new(plan, instance))
            }
            PreparedCommand::Surface {
                mesh,
                image,
                dark_image,
                uniforms,
            } => {
                let mesh = meshes
                    .get(mesh)
                    .ok_or(RetainedFrameRendererError::VectorPlanMismatch)?;
                let mut draw = SurfaceDraw::new(mesh);
                if let Some(image) = image {
                    let image = image_frame
                        .get(image)
                        .ok_or(RetainedFrameRendererError::VectorPlanMismatch)?;
                    let dark = dark_image
                        .map(|index| {
                            image_frame
                                .get(index)
                                .ok_or(RetainedFrameRendererError::VectorPlanMismatch)
                        })
                        .transpose()?;
                    let mut material = crate::TextureMaterial::surface(
                        image.texture(),
                        dark.map(|image| image.texture()),
                    );
                    material.sampler = image.sampler();
                    draw.material = SurfaceMaterial::Texture(material);
                }
                draw.shading = uniforms.shading;
                draw.is_fixed_in_frame = uniforms.is_fixed_in_frame;
                draw.clip_planes = uniforms.clip_planes;
                draw.depth_test = uniforms.depth_test;
                ThreeDDraw::Surface(draw)
            }
            PreparedCommand::Dot(dot) => ThreeDDraw::TrueDot(dot),
            PreparedCommand::Image {
                mesh,
                image,
                uniforms,
            } => {
                let mesh = meshes
                    .get(mesh)
                    .ok_or(RetainedFrameRendererError::VectorPlanMismatch)?;
                let image = image_frame
                    .get(image)
                    .ok_or(RetainedFrameRendererError::VectorPlanMismatch)?;
                let mut draw = SurfaceDraw::image(mesh, image.texture());
                if let SurfaceMaterial::Texture(material) = &mut draw.material {
                    material.sampler = image.sampler();
                }
                draw.is_fixed_in_frame = uniforms.is_fixed_in_frame;
                draw.clip_planes = uniforms.clip_planes;
                draw.depth_test = uniforms.depth_test;
                ThreeDDraw::Surface(draw)
            }
        });
    }
    Ok(draws)
}

#[derive(Debug, Clone, Copy)]
enum PreparedCommand {
    Vector(u32),
    Surface {
        mesh: usize,
        image: Option<u32>,
        dark_image: Option<u32>,
        uniforms: fmn_mobject::Uniforms,
    },
    Dot(TrueDotDraw),
    Image {
        mesh: usize,
        image: u32,
        uniforms: fmn_mobject::Uniforms,
    },
}

fn record_vec3(
    stage: &Stage,
    mob: Mob,
    record: usize,
    field: &'static str,
) -> Result<Vec3, RetainedFrameRendererError> {
    let value = stage
        .get(mob)
        .and_then(|entry| entry.buffer.read(record, field))
        .and_then(|value| <[f32; 3]>::try_from(value.as_slice()).ok())
        .ok_or(RetainedFrameRendererError::InvalidPrimitive {
            mob,
            reason: "required vec3 record field is absent",
        })?;
    Ok(value.map(f64::from))
}

fn record_rgba(
    stage: &Stage,
    mob: Mob,
    record: usize,
) -> Result<LinearRgba, RetainedFrameRendererError> {
    let value = stage
        .get(mob)
        .and_then(|entry| entry.buffer.read(record, "rgba"))
        .and_then(|value| <[f32; 4]>::try_from(value.as_slice()).ok())
        .ok_or(RetainedFrameRendererError::InvalidPrimitive {
            mob,
            reason: "required rgba record field is absent",
        })?;
    let linearize_srgb = fmn_frame::transfer::srgb_decode;
    Ok(LinearRgba {
        r: linearize_srgb(f64::from(value[0])),
        g: linearize_srgb(f64::from(value[1])),
        b: linearize_srgb(f64::from(value[2])),
        a: f64::from(value[3]),
    })
}

fn normalized_or_zero(value: Vec3) -> Vec3 {
    let length = (value[0] * value[0] + value[1] * value[1] + value[2] * value[2]).sqrt();
    if length > 0.0 && length.is_finite() {
        [value[0] / length, value[1] / length, value[2] / length]
    } else {
        [0.0; 3]
    }
}

fn surface_mesh(
    stage: &Stage,
    mob: Mob,
    resolution: Option<(usize, usize)>,
) -> Result<SurfaceMesh, RetainedFrameRendererError> {
    let entry = stage
        .get(mob)
        .ok_or(RetainedFrameRendererError::InvalidPrimitive {
            mob,
            reason: "surface handle is stale",
        })?;
    let records = entry.buffer.len();
    let placement = entry.placement();
    let mut vertices = Vec::new();
    vertices.try_reserve_exact(records).map_err(|_| {
        RetainedFrameRendererError::InvalidPrimitive {
            mob,
            reason: "surface vertex allocation failed",
        }
    })?;
    for record in 0..records {
        let point = record_vec3(stage, mob, record, "point")?;
        let d_normal = record_vec3(stage, mob, record, "d_normal_point")?;
        let normal = normalized_or_zero(placement.apply_vector([
            d_normal[0] - point[0],
            d_normal[1] - point[1],
            d_normal[2] - point[2],
        ]));
        let vertex = if entry.image_resource().is_some() {
            let uv = entry
                .buffer
                .read(record, "im_coords")
                .and_then(|value| <[f32; 2]>::try_from(value.as_slice()).ok())
                .ok_or(RetainedFrameRendererError::InvalidPrimitive {
                    mob,
                    reason: "textured surface im_coords field is absent",
                })?;
            let opacity = entry
                .buffer
                .read(record, "opacity")
                .and_then(|value| value.first().copied())
                .ok_or(RetainedFrameRendererError::InvalidPrimitive {
                    mob,
                    reason: "textured surface opacity field is absent",
                })?;
            if !uv.iter().all(|value| value.is_finite()) || !opacity.is_finite() {
                return Err(RetainedFrameRendererError::InvalidPrimitive {
                    mob,
                    reason: "textured surface UV and opacity must be finite",
                });
            }
            SurfaceVertex::textured(
                placement.apply_point(point),
                normal,
                uv.map(f64::from),
                f64::from(opacity),
            )
        } else {
            SurfaceVertex::colored(
                placement.apply_point(point),
                normal,
                record_rgba(stage, mob, record)?,
            )
        };
        vertices.push(vertex);
    }
    match resolution {
        Some((nu, nv)) => {
            if nu.checked_mul(nv) != Some(records) {
                return Err(RetainedFrameRendererError::InvalidPrimitive {
                    mob,
                    reason: "surface resolution does not match record count",
                });
            }
            let nu =
                u32::try_from(nu).map_err(|_| RetainedFrameRendererError::InvalidPrimitive {
                    mob,
                    reason: "surface u resolution exceeds u32",
                })?;
            let nv =
                u32::try_from(nv).map_err(|_| RetainedFrameRendererError::InvalidPrimitive {
                    mob,
                    reason: "surface v resolution exceeds u32",
                })?;
            SurfaceMesh::from_uv_grid(vertices, (nu, nv))
                .map_err(RetainedFrameRendererError::ThreeD)
        }
        None => {
            if !records.is_multiple_of(3) {
                return Err(RetainedFrameRendererError::InvalidPrimitive {
                    mob,
                    reason: "triangle mesh record count is not divisible by three",
                });
            }
            let count = u32::try_from(records).map_err(|_| {
                RetainedFrameRendererError::InvalidPrimitive {
                    mob,
                    reason: "triangle mesh vertex count exceeds u32",
                }
            })?;
            SurfaceMesh::new(vertices, (0..count).collect())
                .map_err(RetainedFrameRendererError::ThreeD)
        }
    }
}

fn image_mesh(stage: &Stage, mob: Mob) -> Result<SurfaceMesh, RetainedFrameRendererError> {
    let entry = stage
        .get(mob)
        .ok_or(RetainedFrameRendererError::InvalidPrimitive {
            mob,
            reason: "image handle is stale",
        })?;
    if entry.buffer.len() != 6 {
        return Err(RetainedFrameRendererError::InvalidPrimitive {
            mob,
            reason: "image quad must contain exactly six records",
        });
    }
    let placement = entry.placement();
    let mut vertices = Vec::new();
    vertices
        .try_reserve_exact(6)
        .map_err(|_| RetainedFrameRendererError::AllocationFailed {
            resource: "image-mesh vertex",
            requested: 6,
        })?;
    for record in 0..6 {
        let point = placement.apply_point(record_vec3(stage, mob, record, "point")?);
        let uv = entry
            .buffer
            .read(record, "im_coords")
            .and_then(|value| <[f32; 2]>::try_from(value.as_slice()).ok())
            .ok_or(RetainedFrameRendererError::InvalidPrimitive {
                mob,
                reason: "image im_coords field is absent",
            })?;
        let opacity = entry
            .buffer
            .read(record, "opacity")
            .and_then(|value| value.first().copied())
            .map(f64::from)
            .ok_or(RetainedFrameRendererError::InvalidPrimitive {
                mob,
                reason: "image opacity field is absent",
            })?;
        vertices.push(SurfaceVertex::textured(
            point,
            [0.0, 0.0, 1.0],
            uv.map(f64::from),
            opacity,
        ));
    }
    SurfaceMesh::new(vertices, (0..6).collect()).map_err(RetainedFrameRendererError::ThreeD)
}

fn uniform_scale(placement: Placement, mob: Mob) -> Result<f64, RetainedFrameRendererError> {
    let axes = [
        placement.apply_vector([1.0, 0.0, 0.0]),
        placement.apply_vector([0.0, 1.0, 0.0]),
        placement.apply_vector([0.0, 0.0, 1.0]),
    ];
    let lengths =
        axes.map(|axis| (axis[0] * axis[0] + axis[1] * axis[1] + axis[2] * axis[2]).sqrt());
    let scale = lengths[0];
    let tolerance = scale.abs().max(1.0) * 1e-12;
    let dot =
        |left: Vec3, right: Vec3| left[0] * right[0] + left[1] * right[1] + left[2] * right[2];
    if lengths
        .iter()
        .any(|length| (*length - scale).abs() > tolerance)
        || dot(axes[0], axes[1]).abs() > tolerance
        || dot(axes[0], axes[2]).abs() > tolerance
        || dot(axes[1], axes[2]).abs() > tolerance
    {
        return Err(RetainedFrameRendererError::InvalidPrimitive {
            mob,
            reason: "dot cloud placement is not a uniform orthogonal scale",
        });
    }
    Ok(scale)
}

fn dot_draws(stage: &Stage, mob: Mob) -> Result<Vec<TrueDotDraw>, RetainedFrameRendererError> {
    let entry = stage
        .get(mob)
        .ok_or(RetainedFrameRendererError::InvalidPrimitive {
            mob,
            reason: "dot-cloud handle is stale",
        })?;
    let placement = entry.placement();
    let scale = uniform_scale(placement, mob)?;
    let mut draws = Vec::new();
    draws.try_reserve_exact(entry.buffer.len()).map_err(|_| {
        RetainedFrameRendererError::InvalidPrimitive {
            mob,
            reason: "dot-cloud draw allocation failed",
        }
    })?;
    for record in 0..entry.buffer.len() {
        let radius = entry
            .buffer
            .read(record, "radius")
            .and_then(|value| value.first().copied())
            .map(f64::from)
            .ok_or(RetainedFrameRendererError::InvalidPrimitive {
                mob,
                reason: "dot-cloud radius field is absent",
            })?;
        let glow_factor = entry
            .buffer
            .read(record, "glow_factor")
            .and_then(|value| value.first().copied())
            .map(f64::from)
            .ok_or(RetainedFrameRendererError::InvalidPrimitive {
                mob,
                reason: "dot-cloud glow_factor field is absent",
            })?;
        let mut draw = TrueDotDraw::new(
            placement.apply_point(record_vec3(stage, mob, record, "point")?),
            radius * scale,
            record_rgba(stage, mob, record)?,
        );
        draw.glow_factor = glow_factor;
        draw.anti_alias_width = entry.uniforms().anti_alias_width;
        draw.shading = entry.uniforms().shading;
        draw.is_fixed_in_frame = entry.uniforms().is_fixed_in_frame;
        draw.clip_planes = entry.uniforms().clip_planes;
        draw.depth_test = entry.uniforms().depth_test;
        draws.push(draw);
    }
    Ok(draws)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CameraConfig, ScreenMap, Viewport};
    use fmn_core::color::LinearRgba;
    use fmn_mobject::{
        ImageColorSpace, ImageResource, ImageSampler, Mobject, RecordBuffer, RecordSchema,
        RenderPrimitive,
    };

    fn config(threads: usize) -> RetainedFrameRendererConfig {
        RetainedFrameRendererConfig {
            frame: FrameConfig::new(
                Viewport {
                    width: 32,
                    height: 18,
                },
                ScreenMap {
                    scale: 2.25,
                    origin: [16.0, 9.0],
                    y_up: false,
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
            threads,
        }
    }
    fn camera() -> Camera {
        Camera::new(CameraConfig {
            resolution: (32, 18),
            background: config(1).frame.background,
            ..CameraConfig::default()
        })
        .expect("valid test camera")
    }
    fn vector() -> Mobject {
        let mut buffer = RecordBuffer::new(RecordSchema::vmobject(), 3).expect("small vector");
        buffer.write_range(
            "point",
            0,
            &[-1.5, -1.0, 0.0, 0.0, 1.25, 0.0, 1.5, -1.0, 0.0],
        );
        buffer.write_range("fill_rgba", 0, &[0.1, 0.3, 1.0, 0.8].repeat(3));
        Mobject::from_buffer(buffer)
    }
    fn surface(resolution: (usize, usize)) -> Mobject {
        let schema = RecordSchema::new(
            &[("point", 3), ("d_normal_point", 3), ("rgba", 4)],
            &["point"],
            &["point", "d_normal_point"],
        )
        .expect("surface schema");
        let mut buffer = RecordBuffer::new(schema, 4).expect("small surface");
        buffer.write_range(
            "point",
            0,
            &[
                -2.5, -1.5, 0.2, -2.5, 0.5, 0.2, -0.5, -1.5, 0.2, -0.5, 0.5, 0.2,
            ],
        );
        buffer.write_range(
            "d_normal_point",
            0,
            &[
                -2.5, -1.5, 1.2, -2.5, 0.5, 1.2, -0.5, -1.5, 1.2, -0.5, 0.5, 1.2,
            ],
        );
        buffer.write_range("rgba", 0, &[1.0, 0.2, 0.1, 0.9].repeat(4));
        Mobject::from_buffer(buffer)
            .with_render_primitive(RenderPrimitive::SurfaceGrid { resolution })
    }
    fn dot_cloud() -> Mobject {
        let schema = RecordSchema::new(
            &[("point", 3), ("radius", 1), ("rgba", 4), ("glow_factor", 1)],
            &["point"],
            &["point"],
        )
        .expect("dot schema");
        let mut buffer = RecordBuffer::new(schema, 1).expect("one dot");
        buffer.write(0, "point", &[2.0, 0.5, 0.5]);
        buffer.write(0, "radius", &[0.45]);
        buffer.write(0, "rgba", &[0.2, 1.0, 0.2, 1.0]);
        buffer.write(0, "glow_factor", &[0.0]);
        Mobject::from_buffer(buffer).with_render_primitive(RenderPrimitive::DotCloud)
    }
    fn triangle_mesh() -> Mobject {
        let schema = RecordSchema::new(
            &[("point", 3), ("d_normal_point", 3), ("rgba", 4)],
            &["point"],
            &["point", "d_normal_point"],
        )
        .expect("triangle schema");
        let mut buffer = RecordBuffer::new(schema, 3).expect("one triangle");
        buffer.write_range(
            "point",
            0,
            &[0.5, -1.5, 0.4, 1.5, 0.25, 0.4, 2.5, -1.5, 0.4],
        );
        buffer.write_range(
            "d_normal_point",
            0,
            &[0.5, -1.5, 1.4, 1.5, 0.25, 1.4, 2.5, -1.5, 1.4],
        );
        buffer.write_range("rgba", 0, &[0.9, 0.8, 0.1, 0.9].repeat(3));
        Mobject::from_buffer(buffer).with_render_primitive(RenderPrimitive::TriangleMesh)
    }
    fn image_quad(camera: &Camera, pixels: Vec<u8>) -> Mobject {
        let schema = RecordSchema::new(
            &[("point", 3), ("im_coords", 2), ("opacity", 1)],
            &["point"],
            &["point"],
        )
        .expect("image schema");
        let mut buffer = RecordBuffer::new(schema, 6).expect("image records");
        let scale = camera.frame().scale();
        let half_width = fmn_core::constants::FRAME_WIDTH * scale / 2.0;
        let half_height = fmn_core::constants::FRAME_HEIGHT * scale / 2.0;
        #[allow(clippy::cast_possible_truncation)]
        buffer.write_range(
            "point",
            0,
            &[
                -half_width as f32,
                half_height as f32,
                0.0,
                -half_width as f32,
                -half_height as f32,
                0.0,
                half_width as f32,
                half_height as f32,
                0.0,
                half_width as f32,
                -half_height as f32,
                0.0,
                half_width as f32,
                half_height as f32,
                0.0,
                -half_width as f32,
                -half_height as f32,
                0.0,
            ],
        );
        buffer.write_range(
            "im_coords",
            0,
            &[0.0, 0.0, 0.0, 1.0, 1.0, 0.0, 1.0, 1.0, 1.0, 0.0, 0.0, 1.0],
        );
        buffer.write_range("opacity", 0, &[1.0; 6]);
        let resource = ImageResource::rgba8(
            2,
            2,
            pixels,
            ImageColorSpace::Linear,
            ImageSampler::default(),
        )
        .expect("valid test image");
        Mobject::from_buffer(buffer).with_image_resource(resource)
    }
    fn pixel(frame: &FrameBuffer, x: usize, y: usize) -> [f64; 4] {
        let stride = frame.layout().stride(0);
        let offset = y * stride + x * 8;
        let bytes = &frame.plane(0)[offset..offset + 8];
        std::array::from_fn(|component| {
            let at = component * 2;
            fmn_frame::half::f16_to_f64(u16::from_le_bytes([bytes[at], bytes[at + 1]]))
        })
    }

    fn fresh_camera_bits(stage: &Stage, camera: &Camera) -> Vec<u8> {
        // Independent serial oracle: compile directly without retaining or
        // rebinding the owned projection under test.
        let mut plan = RenderPlan::new();
        plan.sync(stage, camera.revision()).unwrap();
        let scene = prepare_camera_scene(&mut plan, stage, &stage.draw_plan()).unwrap();
        plan.commit_image_frame(scene.images.clone());
        let draws = camera_draws(&plan, &scene).unwrap();
        ThreeDJob::new(camera, &draws, config(1).tiling)
            .unwrap()
            .render(1)
            .unwrap()
            .as_bytes()
            .to_vec()
    }

    #[test]
    fn unchanged_camera_projection_is_shared_and_replayed_concurrently() {
        let camera = camera();
        let mut stage = Stage::new();
        for object in [
            image_quad(&camera, [40, 70, 100, 255].repeat(4)),
            vector(),
            surface((2, 2)),
            dot_cloud(),
            triangle_mesh(),
        ] {
            let mob = stage.add(object);
            stage.add_to_scene(mob).unwrap();
        }
        let expected = fresh_camera_bits(&stage, &camera);
        let mut renderer = RetainedFrameRenderer::new(config(1)).unwrap();
        let first = renderer.prepare_with_camera(&stage, &camera).unwrap();
        let repeated = renderer.prepare_with_camera(&stage, &camera).unwrap();
        assert!(Arc::ptr_eq(&first.projection, &repeated.projection));
        assert!(first.projection.prepared.lock().unwrap().is_none());
        drop(renderer);
        drop(stage);
        drop(camera);
        // All workers race a cold projection after the capture owner and
        // live image resources are gone. They share its successful result.
        std::thread::scope(|scope| {
            let replays: Vec<_> = [1, 4, 16]
                .into_iter()
                .map(|threads| {
                    let frozen = &repeated;
                    scope.spawn(move || frozen.render(threads).unwrap())
                })
                .collect();
            for replay in replays {
                assert_eq!(replay.join().unwrap().as_bytes(), expected);
            }
        });
        let projection = Arc::clone(first.projection.prepared.lock().unwrap().as_ref().unwrap());
        for threads in [4, 1] {
            assert_eq!(first.render(threads).unwrap().as_bytes(), expected);
            assert!(Arc::ptr_eq(
                &projection,
                repeated
                    .projection
                    .prepared
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap(),
            ));
        }
        let mut destination = FrameBuffer::new(first.layout().unwrap());
        destination.plane_mut(0).fill(0xA5);
        let untouched = destination.as_bytes().to_vec();
        assert!(matches!(
            first.render_into(0, &mut destination),
            Err(RetainedFrameRendererError::InvalidThreads)
        ));
        assert_eq!(destination.as_bytes(), untouched);
    }

    #[test]
    fn projection_cache_keys_complete_camera_values_even_when_revisions_match() {
        let base = CameraConfig {
            resolution: (32, 18),
            ..CameraConfig::default()
        };
        let original_camera = Camera::new(base.clone()).unwrap();
        let mut stage = Stage::new();
        for object in [vector(), surface((2, 2)), dot_cloud()] {
            let mob = stage.add(object);
            stage.add_to_scene(mob).unwrap();
        }
        let mut renderer = RetainedFrameRenderer::new(config(1)).unwrap();
        let original = renderer
            .prepare_with_camera(&stage, &original_camera)
            .unwrap();
        for change in 0..11 {
            let mut changed = base.clone();
            match change {
                0 => changed.background.r = 0.25,
                1 => changed.light_source_position = [3.0, -2.0, 4.0],
                2 => changed.samples = 4,
                3 => changed.resolution = (24, 24),
                4 => changed.fps += 1,
                5 => changed.max_allowable_norm *= 0.75,
                6 => {
                    changed.frame.set_center([0.7, -0.2, 0.3]).unwrap();
                }
                7 => {
                    changed.frame.set_width(12.0).unwrap();
                }
                8 => {
                    changed
                        .frame
                        .set_field_of_view(base.frame.field_of_view() * 0.8)
                        .unwrap();
                }
                9 => {
                    changed
                        .frame
                        .set_orientation([0.0, 0.0, 0.2, 0.98])
                        .unwrap();
                }
                _ => changed.background.r = -0.0,
            }
            let changed = Camera::new(changed).unwrap();
            assert_eq!(original_camera.revision(), changed.revision());
            if change == 10 {
                assert_eq!(
                    original_camera, changed,
                    "float equality ignores signed zero"
                );
            }
            // Return to the original capture so every case independently
            // exercises its key rather than inheriting the previous change.
            let before = renderer
                .prepare_with_camera(&stage, &original_camera)
                .unwrap();
            let after = renderer.prepare_with_camera(&stage, &changed).unwrap();
            assert!(Arc::ptr_eq(&original.scene, &after.scene));
            assert!(Arc::ptr_eq(&original.plan, &after.plan));
            assert!(!Arc::ptr_eq(&before.projection, &after.projection));
            let expected = fresh_camera_bits(&stage, &changed);
            for threads in [1, 4, 16] {
                assert_eq!(after.render(threads).unwrap().as_bytes(), expected);
            }
        }
    }

    #[test]
    fn renderer_retains_only_the_most_recent_camera_projection() {
        let camera = camera();
        let mut stage = Stage::new();
        let mob = stage.add(vector());
        stage.add_to_scene(mob).unwrap();
        let mut renderer = RetainedFrameRenderer::new(config(1)).unwrap();
        let first = renderer.prepare_with_camera(&stage, &camera).unwrap();
        first.render(1).unwrap();
        let first_projection = Arc::downgrade(&first.projection);
        drop(first);
        assert!(first_projection.upgrade().is_some());
        stage.shift(mob, [0.5, 0.0, 0.0]);
        let next = renderer.prepare_with_camera(&stage, &camera).unwrap();
        next.render(1).unwrap();
        assert!(first_projection.upgrade().is_none());
        let next_projection = Arc::downgrade(&next.projection);
        drop(next);
        assert!(next_projection.upgrade().is_some());
        renderer.render(&stage, camera.revision()).unwrap();
        assert!(next_projection.upgrade().is_none());
    }

    #[test]
    fn projection_cache_observes_uniform_float_bits_without_a_revision_change() {
        let camera = camera();
        let mut stage = Stage::new();
        let mob = stage.add(surface((2, 2)));
        stage.add_to_scene(mob).unwrap();
        let mut renderer = RetainedFrameRenderer::new(config(1)).unwrap();
        let first = renderer.prepare_with_camera(&stage, &camera).unwrap();
        let before = *stage.uniforms(mob).unwrap();
        let revision = stage.get(mob).unwrap().buffer.revision();
        stage.uniforms_mut(mob).unwrap().shading[0] = -0.0;
        assert_eq!(before, *stage.uniforms(mob).unwrap());
        assert_eq!(revision, stage.get(mob).unwrap().buffer.revision());
        let changed = renderer.prepare_with_camera(&stage, &camera).unwrap();
        assert!(!Arc::ptr_eq(&first.scene, &changed.scene));
        assert!(!Arc::ptr_eq(&first.projection, &changed.projection));
        let expected = fresh_camera_bits(&stage, &camera);
        for threads in [1, 4, 16] {
            assert_eq!(changed.render(threads).unwrap().as_bytes(), expected);
        }
    }

    #[test]
    fn camera_motion_reuses_world_resources_and_frozen_vector_plan() {
        let mut camera = camera();
        let mut stage = Stage::new();
        for object in [vector(), surface((2, 2)), dot_cloud(), triangle_mesh()] {
            let mob = stage.add(object);
            stage.add_to_scene(mob).unwrap();
        }
        let mut renderer = RetainedFrameRenderer::new(config(1)).unwrap();
        let first = renderer.prepare_with_camera(&stage, &camera).unwrap();
        let mut frames = vec![(first, fresh_camera_bits(&stage, &camera))];
        for step in 1..=3 {
            camera
                .frame_mut()
                .set_center([f64::from(step) * 0.4, 0.0, 0.0])
                .unwrap();
            camera
                .set_light_source_position([f64::from(step), -3.0, 5.0])
                .unwrap();
            let frame = renderer.prepare_with_camera(&stage, &camera).unwrap();
            assert!(Arc::ptr_eq(&frames[0].0.scene, &frame.scene));
            assert!(Arc::ptr_eq(&frames[0].0.plan, &frame.plan));
            assert!(!Arc::ptr_eq(
                &frames.last().unwrap().0.projection,
                &frame.projection
            ));
            frames.push((frame, fresh_camera_bits(&stage, &camera)));
        }
        assert_ne!(frames[0].1, frames[3].1);
        drop(renderer);
        drop(stage);
        for (frame, expected) in frames.into_iter().rev() {
            for threads in [1, 4, 16] {
                assert_eq!(frame.render(threads).unwrap().as_bytes(), expected);
            }
        }
    }

    #[test]
    fn camera_resource_cache_observes_records_uniforms_placement_and_order() {
        let camera = camera();
        let mut stage = Stage::new();
        let vector = stage.add(vector());
        let surface = stage.add(surface((2, 2)));
        stage.add_to_scene(vector).unwrap();
        stage.add_to_scene(surface).unwrap();
        let mut renderer = RetainedFrameRenderer::new(config(1)).unwrap();
        let first = renderer.prepare_with_camera(&stage, &camera).unwrap();
        let mut frames = vec![(first, fresh_camera_bits(&stage, &camera))];
        for step in 0..6 {
            match step {
                0 => {
                    stage
                        .set_placement(surface, Placement::from_translation([1.0, 0.0, 0.0]))
                        .unwrap();
                }
                1 => {
                    stage.get_mut(surface).unwrap().buffer.write_range(
                        "rgba",
                        0,
                        &[0.1, 0.8, 0.3, 0.8].repeat(4),
                    );
                }
                2 => {
                    stage.set_shading(surface, [0.2, 0.4, 0.1], false);
                }
                3 => {
                    stage.get_mut(vector).unwrap().buffer.write_range(
                        "fill_rgba",
                        0,
                        &[0.9, 0.1, 0.4, 0.8].repeat(3),
                    );
                }
                4 => {
                    stage
                        .get_mut(vector)
                        .unwrap()
                        .buffer
                        .write(1, "point", &[0.5, 1.75, 0.0]);
                }
                _ => stage.bring_to_front(vector).unwrap(),
            }
            let frame = renderer.prepare_with_camera(&stage, &camera).unwrap();
            let previous = &frames.last().unwrap().0;
            assert!(!Arc::ptr_eq(&previous.scene, &frame.scene));
            assert!(!Arc::ptr_eq(&previous.projection, &frame.projection));
            if matches!(step, 3 | 4) {
                assert!(!Arc::ptr_eq(&previous.plan, &frame.plan));
            }
            frames.push((frame, fresh_camera_bits(&stage, &camera)));
        }
        for (frame, expected) in frames.into_iter().rev() {
            for threads in [1, 4] {
                assert_eq!(frame.render(threads).unwrap().as_bytes(), expected);
            }
        }
    }

    #[test]
    fn camera_resources_never_reuse_writable_view_observations() {
        let camera = camera();
        let mut stage = Stage::new();
        let mob = stage.add(surface((2, 2)));
        stage.add_to_scene(mob).unwrap();
        let mut renderer = RetainedFrameRenderer::new(config(1)).unwrap();
        let first = renderer.prepare_with_camera(&stage, &camera).unwrap();
        let first_bits = first.render(1).unwrap().as_bytes().to_vec();
        let view = stage
            .get_mut(mob)
            .unwrap()
            .buffer
            .export_field_view("rgba", true)
            .unwrap();
        let viewed = renderer.prepare_with_camera(&stage, &camera).unwrap();
        let repeated = renderer.prepare_with_camera(&stage, &camera).unwrap();
        assert!(!Arc::ptr_eq(&viewed.scene, &repeated.scene));
        assert!(!Arc::ptr_eq(&viewed.projection, &repeated.projection));
        let revision = stage.get(mob).unwrap().buffer.revision();
        for record in 0..4 {
            assert!(view.write_foreign(record, "rgba", &[0.0, 1.0, 0.0, 1.0]));
        }
        assert_eq!(revision, stage.get(mob).unwrap().buffer.revision());
        let changed = renderer.prepare_with_camera(&stage, &camera).unwrap();
        let expected = fresh_camera_bits(&stage, &camera);
        assert_ne!(expected, first_bits);
        assert!(!Arc::ptr_eq(&repeated.scene, &changed.scene));
        assert!(!Arc::ptr_eq(&repeated.projection, &changed.projection));
        let frozen_stage = stage.freeze_render_state().materialize();
        let snapshot = renderer
            .prepare_with_camera(&frozen_stage, &camera)
            .unwrap();
        let snapshot_repeat = renderer
            .prepare_with_camera(&frozen_stage, &camera)
            .unwrap();
        assert!(!Arc::ptr_eq(&snapshot.scene, &snapshot_repeat.scene));
        assert!(!Arc::ptr_eq(
            &snapshot.projection,
            &snapshot_repeat.projection
        ));
        drop(view);
        let clean = renderer.prepare_with_camera(&stage, &camera).unwrap();
        let clean_repeat = renderer.prepare_with_camera(&stage, &camera).unwrap();
        assert!(Arc::ptr_eq(&clean.scene, &clean_repeat.scene));
        assert!(Arc::ptr_eq(&clean.projection, &clean_repeat.projection));
        for threads in [1, 4] {
            assert_eq!(first.render(threads).unwrap().as_bytes(), first_bits);
            assert_eq!(changed.render(threads).unwrap().as_bytes(), expected);
            assert_eq!(snapshot.render(threads).unwrap().as_bytes(), expected);
            assert_eq!(clean_repeat.render(threads).unwrap().as_bytes(), expected);
        }
    }

    #[test]
    fn camera_resource_cache_distinguishes_replacement_storage_with_equal_revisions() {
        let camera = camera();
        let mut stage = Stage::new();
        let mob = stage.add(surface((2, 2)));
        stage.add_to_scene(mob).unwrap();
        let mut replacement = stage.get(mob).unwrap().buffer.deep_clone();
        let view = replacement.export_field_view("rgba", true).unwrap();
        for record in 0..4 {
            assert!(view.write_foreign(record, "rgba", &[0.0, 0.1, 1.0, 1.0]));
        }
        drop(view);
        // Match the view-detach bump without changing the original pixels.
        stage
            .get_mut(mob)
            .unwrap()
            .buffer
            .write(0, "rgba", &[1.0, 0.2, 0.1, 0.9]);
        assert_eq!(
            replacement.revision(),
            stage.get(mob).unwrap().buffer.revision()
        );
        let mut renderer = RetainedFrameRenderer::new(config(1)).unwrap();
        let first = renderer.prepare_with_camera(&stage, &camera).unwrap();
        let first_bits = first.render(1).unwrap().as_bytes().to_vec();
        stage.get_mut(mob).unwrap().buffer = replacement;
        let second = renderer.prepare_with_camera(&stage, &camera).unwrap();
        assert!(!Arc::ptr_eq(&first.scene, &second.scene));
        assert!(!Arc::ptr_eq(&first.projection, &second.projection));
        let second_bits = fresh_camera_bits(&stage, &camera);
        assert_ne!(first_bits, second_bits);
        for threads in [1, 4] {
            assert_eq!(first.render(threads).unwrap().as_bytes(), first_bits);
            assert_eq!(second.render(threads).unwrap().as_bytes(), second_bits);
        }
    }

    #[test]
    fn frozen_camera_frames_are_send_and_retain_mixed_scene_bits_after_edits() {
        fn send_sync<T: Send + Sync + 'static>() {}
        send_sync::<PreparedCameraFrame>();
        let mut camera = camera();
        let mut stage = Stage::new();
        let mut roots = Vec::new();
        for mobject in [
            vector(),
            surface((2, 2)),
            dot_cloud(),
            triangle_mesh(),
            image_quad(&camera, [230, 40, 90, 255].repeat(4)),
        ] {
            let mob = stage.add(mobject);
            stage.add_to_scene(mob).unwrap();
            roots.push(mob);
        }
        let mut renderer = RetainedFrameRenderer::new(config(1)).unwrap();
        renderer.render_with_camera(&stage, &camera).unwrap();
        let expected = renderer.frame().as_bytes().to_vec();
        let frozen = renderer.prepare_with_camera(&stage, &camera).unwrap();
        for root in roots {
            stage.shift(root, [3.0, 1.0, 0.0]);
        }
        camera.frame_mut().set_center([1.0, -2.0, 0.0]).unwrap();
        camera
            .set_background(LinearRgba {
                r: 0.4,
                g: 0.1,
                b: 0.2,
                a: 1.0,
            })
            .unwrap();
        renderer.render_with_camera(&stage, &camera).unwrap();
        assert_ne!(expected, renderer.frame().as_bytes());
        drop(renderer);
        drop(stage);
        drop(camera);
        std::thread::spawn(move || {
            for threads in [1, 2, 4] {
                assert_eq!(frozen.render(threads).unwrap().as_bytes(), expected);
            }
            assert!(frozen.render(0).is_err());
        })
        .join()
        .unwrap();
    }

    #[test]
    fn frozen_camera_frames_keep_successive_camera_and_texture_revisions_separate() {
        let mut camera = camera();
        let mut stage = Stage::new();
        let mob = stage.add(image_quad(&camera, [250, 0, 0, 255].repeat(4)));
        stage.add_to_scene(mob).unwrap();
        let mut renderer = RetainedFrameRenderer::new(config(1)).unwrap();
        let first = renderer.prepare_with_camera(&stage, &camera).unwrap();
        let first_bits = first.render(1).unwrap().as_bytes().to_vec();
        let resource = ImageResource::rgba8(
            2,
            2,
            [0, 250, 0, 255].repeat(4),
            ImageColorSpace::Linear,
            ImageSampler::default(),
        )
        .unwrap();
        stage.set_image_resource(mob, Some(resource)).unwrap();
        let second = renderer.prepare_with_camera(&stage, &camera).unwrap();
        assert!(!Arc::ptr_eq(&first.projection, &second.projection));
        let second_bits = fresh_camera_bits(&stage, &camera);
        camera.frame_mut().set_center([1.0, 0.0, 0.0]).unwrap();
        let third = renderer.prepare_with_camera(&stage, &camera).unwrap();
        assert!(!Arc::ptr_eq(&second.projection, &third.projection));
        renderer.render_with_camera(&stage, &camera).unwrap();
        let third_bits = renderer.frame().as_bytes().to_vec();
        assert_ne!(first_bits, second_bits);
        assert_ne!(second_bits, third_bits);
        drop(renderer);
        drop(stage);
        for threads in [1, 4, 16] {
            assert_eq!(third.render(threads).unwrap().as_bytes(), third_bits);
            assert_eq!(second.render(threads).unwrap().as_bytes(), second_bits);
            assert_eq!(first.render(threads).unwrap().as_bytes(), first_bits);
        }
    }

    #[test]
    fn zero_threads_fail_before_frame_allocation() {
        assert!(matches!(
            RetainedFrameRenderer::new(config(0)),
            Err(RetainedFrameRendererError::InvalidThreads)
        ));
    }
    #[test]
    fn fixed_camera_reuses_the_same_retained_frame_owner() {
        let stage = Stage::new();
        let mut renderer = RetainedFrameRenderer::new(config(1)).expect("valid renderer");
        renderer.render(&stage, 0).expect("first frame");
        let first = renderer.frame().as_bytes().to_vec();
        let second = renderer.render(&stage, 0).expect("retained frame");
        assert_eq!(renderer.frame().as_bytes(), first);
        assert!(second.cache.hits > 0 || second.cache.misses == 0);
    }
    #[test]
    fn camera_bound_vector_projection_is_not_silently_substituted_for_affine_output() {
        let camera = camera();
        let mut stage = Stage::new();
        let mob = stage.add(vector());
        stage.add_to_scene(mob).expect("live root");
        let mut affine = RetainedFrameRenderer::new(config(1)).expect("valid renderer");
        affine
            .render(&stage, camera.revision())
            .expect("affine vector frame");
        let mut camera_bound = RetainedFrameRenderer::new(config(1)).expect("valid renderer");
        camera_bound
            .render_with_camera(&stage, &camera)
            .expect("camera-bound vector frame");
        assert_ne!(affine.frame().as_bytes(), camera_bound.frame().as_bytes());
    }
    /// Blue-channel coverage of each row (the vector fill is blue on black).
    fn row_blue(frame: &FrameBuffer) -> Vec<f64> {
        (0..18)
            .map(|y| (0..32).map(|x| pixel(frame, x, y)[2]).sum())
            .collect()
    }
    /// Topmost and bottommost rows the shape covers.
    fn covered_extent(rows: &[f64]) -> (usize, usize) {
        let covered: Vec<usize> = (0..rows.len()).filter(|&y| rows[y] > 0.05).collect();
        (covered[0], *covered.last().expect("the shape is visible"))
    }
    #[test]
    fn object_plus_y_is_the_top_of_a_scene_frame_on_both_routes() {
        // fm-sq8.9: the triangle points +Y, so its narrow apex belongs at the top.
        let mut stage = Stage::new();
        let mob = stage.add(vector());
        stage.add_to_scene(mob).expect("live root");
        let mut oriented = config(1);
        oriented.frame.map.y_up = true;
        let mut retained = RetainedFrameRenderer::new(oriented).expect("valid renderer");
        retained.render(&stage, 0).expect("oriented 2D frame");
        let up = row_blue(retained.frame());
        let (top, bottom) = covered_extent(&up);
        assert!(
            up[top] < up[bottom],
            "apex must be at the top of the frame: rows {up:?}"
        );
        let mut camera_route = RetainedFrameRenderer::new(oriented).expect("valid renderer");
        camera_route
            .render_with_camera(&stage, &camera())
            .expect("camera frame");
        let projected = row_blue(camera_route.frame());
        let (top, bottom) = covered_extent(&projected);
        assert!(
            projected[top] < projected[bottom],
            "the camera route agrees on orientation: rows {projected:?}"
        );
        let mut raw = RetainedFrameRenderer::new(config(1)).expect("valid renderer");
        raw.render(&stage, 0).expect("raw frame");
        let mirrored: Vec<f64> = row_blue(raw.frame()).into_iter().rev().collect();
        for (row, (a, b)) in up.iter().zip(&mirrored).enumerate() {
            assert!((a - b).abs() < 1e-2, "row {row}: {a} vs mirrored {b}");
        }
    }
    #[test]
    fn camera_route_renders_one_mixed_vector_surface_dot_painter_sequence() {
        let camera = camera();
        let mut empty_renderer = RetainedFrameRenderer::new(config(1)).expect("valid renderer");
        empty_renderer
            .render_with_camera(&Stage::new(), &camera)
            .expect("empty background");
        let background = empty_renderer.frame().as_bytes().to_vec();
        let mut stage = Stage::new();
        for mobject in [vector(), surface((2, 2)), dot_cloud(), triangle_mesh()] {
            let mob = stage.add(mobject);
            stage.add_to_scene(mob).expect("live root");
        }
        let mut one = RetainedFrameRenderer::new(config(1)).expect("valid renderer");
        assert!(matches!(
            one.render(&stage, camera.revision()),
            Err(RetainedFrameRendererError::CameraRequired { .. })
        ));
        one.render_with_camera(&stage, &camera)
            .expect("mixed camera-bound frame");
        assert_ne!(one.frame().as_bytes(), background);
        let mut four = RetainedFrameRenderer::new(config(4)).expect("valid renderer");
        four.render_with_camera(&stage, &camera)
            .expect("same mixed frame at four threads");
        assert_eq!(one.frame().as_bytes(), four.frame().as_bytes());
    }
    #[test]
    fn camera_route_renders_image_pixels_and_interns_repeated_resources() {
        let camera = camera();
        let pixels = vec![
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
        ];
        let mut stage = Stage::new();
        let first = stage.add(image_quad(&camera, pixels.clone()));
        stage.add_to_scene(first).expect("first image root");
        let second = stage.add(image_quad(&camera, pixels));
        stage.add_to_scene(second).expect("second image root");
        let mut renderer = RetainedFrameRenderer::new(config(1)).expect("valid renderer");
        renderer
            .render_with_camera(&stage, &camera)
            .expect("production image frame");
        assert_eq!(
            renderer.plan().images().len(),
            1,
            "identical descriptors and bytes share one retained texture"
        );
        let red = pixel(renderer.frame(), 8, 4);
        let green = pixel(renderer.frame(), 24, 4);
        let blue = pixel(renderer.frame(), 8, 13);
        let white = pixel(renderer.frame(), 24, 13);
        assert!(red[0] > 0.8 && red[1] < 0.1 && red[2] < 0.1);
        assert!(green[0] < 0.1 && green[1] > 0.8 && green[2] < 0.1);
        assert!(blue[0] < 0.1 && blue[1] < 0.1 && blue[2] > 0.8);
        assert!(white[0] > 0.8 && white[1] > 0.8 && white[2] > 0.8);
        let changed = ImageResource::rgba8(
            2,
            2,
            [255, 255, 0, 255].repeat(4),
            ImageColorSpace::Linear,
            ImageSampler::default(),
        )
        .expect("replacement image");
        assert!(
            stage
                .set_image_resource(second, Some(changed.clone()))
                .unwrap()
        );
        renderer
            .render_with_camera(&stage, &camera)
            .expect("replacement image frame");
        assert_eq!(renderer.plan().images().len(), 2);
        let yellow = pixel(renderer.frame(), 16, 9);
        assert!(yellow[0] > 0.8 && yellow[1] > 0.8 && yellow[2] < 0.1);
        assert!(stage.set_image_resource(first, Some(changed)).unwrap());
        renderer
            .render_with_camera(&stage, &camera)
            .expect("deduplicated replacement image frame");
        assert_eq!(
            renderer.plan().images().len(),
            1,
            "textures absent from the current frame are not retained indefinitely"
        );
    }
    #[test]
    fn malformed_surface_resolution_refuses_before_publishing_a_frame() {
        let camera = camera();
        let mut stage = Stage::new();
        let mob = stage.add(surface((3, 2)));
        stage.add_to_scene(mob).expect("live root");
        let mut renderer = RetainedFrameRenderer::new(config(1)).expect("valid renderer");
        let before = renderer.frame().as_bytes().to_vec();
        assert!(matches!(
            renderer.render_with_camera(&stage, &camera),
            Err(RetainedFrameRendererError::InvalidPrimitive {
                reason: "surface resolution does not match record count",
                ..
            })
        ));
        assert_eq!(renderer.frame().as_bytes(), before);
    }

    #[test]
    fn static_camera_pixels_are_reused_without_an_extra_frame_owner() {
        let camera = camera();
        let mut stage = Stage::new();
        for object in [vector(), surface((2, 2)), dot_cloud(), triangle_mesh()] {
            let mob = stage.add(object);
            stage.add_to_scene(mob).unwrap();
        }
        let expected = fresh_camera_bits(&stage, &camera);
        for threads in [1, 4, 16] {
            let mut renderer = RetainedFrameRenderer::new(config(threads)).unwrap();
            let prepared = renderer.prepare_with_camera(&stage, &camera).unwrap();
            assert!(prepared.same_capture(&prepared.clone()));
            renderer.render_with_camera(&stage, &camera).unwrap();
            assert!(!renderer.last_camera_frame_reused());
            let storage = renderer.frame().as_bytes().as_ptr();
            let owners = Arc::strong_count(&prepared.projection);
            for _ in 0..4 {
                renderer.render_with_camera(&stage, &camera).unwrap();
                assert!(renderer.last_camera_frame_reused());
                assert_eq!(renderer.frame().as_bytes(), expected);
                assert_eq!(renderer.frame().as_bytes().as_ptr(), storage);
                assert_eq!(Arc::strong_count(&prepared.projection), owners);
            }
        }
    }

    #[test]
    fn camera_pixel_reuse_observes_live_scene_and_foreign_view_changes() {
        let camera = camera();
        let mut stage = Stage::new();
        let vector = stage.add(vector());
        let surface = stage.add(surface((2, 2)));
        let image = stage.add(image_quad(&camera, [80, 20, 10, 255].repeat(4)));
        for mob in [image, vector, surface] {
            stage.add_to_scene(mob).unwrap();
        }
        let mut renderer = RetainedFrameRenderer::new(config(4)).unwrap();
        renderer.render_with_camera(&stage, &camera).unwrap();
        for step in 0..6 {
            renderer.render_with_camera(&stage, &camera).unwrap();
            assert!(renderer.last_camera_frame_reused());
            match step {
                0 => {
                    stage.shift(vector, [0.5, 0.0, 0.0]);
                }
                1 => {
                    stage.set_shading(surface, [0.2, 0.4, 0.1], false);
                }
                2 => {
                    stage.get_mut(vector).unwrap().buffer.write_range(
                        "fill_rgba",
                        0,
                        &[0.8, 0.1, 0.4, 0.7].repeat(3),
                    );
                }
                3 => {
                    stage.bring_to_front(vector).unwrap();
                }
                4 => {
                    let resource = ImageResource::rgba8(
                        2,
                        2,
                        [10, 90, 20, 255].repeat(4),
                        ImageColorSpace::Linear,
                        ImageSampler::default(),
                    )
                    .unwrap();
                    stage.set_image_resource(image, Some(resource)).unwrap();
                }
                _ => {
                    stage
                        .set_placement(surface, Placement::from_translation([1.0, 0.0, 0.0]))
                        .unwrap();
                }
            }
            renderer.render_with_camera(&stage, &camera).unwrap();
            assert!(!renderer.last_camera_frame_reused());
            assert_eq!(
                renderer.frame().as_bytes(),
                fresh_camera_bits(&stage, &camera)
            );
        }
        let view = stage
            .get_mut(surface)
            .unwrap()
            .buffer
            .export_field_view("rgba", true)
            .unwrap();
        for _ in 0..2 {
            renderer.render_with_camera(&stage, &camera).unwrap();
            assert!(!renderer.last_camera_frame_reused());
        }
        for record in 0..4 {
            assert!(view.write_foreign(record, "rgba", &[0.0, 1.0, 0.0, 1.0]));
        }
        renderer.render_with_camera(&stage, &camera).unwrap();
        assert!(!renderer.last_camera_frame_reused());
        assert_eq!(
            renderer.frame().as_bytes(),
            fresh_camera_bits(&stage, &camera)
        );
        drop(view);
        renderer.render_with_camera(&stage, &camera).unwrap();
        renderer.render_with_camera(&stage, &camera).unwrap();
        assert!(renderer.last_camera_frame_reused());
    }

    #[test]
    fn camera_pixels_key_the_rendered_capture_not_the_last_prepared_capture() {
        let base = camera();
        let mut stage = Stage::new();
        let mob = stage.add(vector());
        stage.add_to_scene(mob).unwrap();
        let mut renderer = RetainedFrameRenderer::new(config(1)).unwrap();
        renderer.render_with_camera(&stage, &base).unwrap();
        let original = renderer.frame().as_bytes().to_vec();
        let changed = Camera::new(CameraConfig {
            resolution: (32, 18),
            background: LinearRgba {
                r: 0.3,
                g: 0.1,
                b: 0.2,
                a: 1.0,
            },
            ..CameraConfig::default()
        })
        .unwrap();
        assert_eq!(base.revision(), changed.revision());
        let a = renderer.prepare_with_camera(&stage, &base).unwrap();
        let b = renderer.prepare_with_camera(&stage, &changed).unwrap();
        assert!(!a.same_capture(&b));
        assert_eq!(renderer.frame().as_bytes(), original);
        renderer.render_with_camera(&stage, &changed).unwrap();
        assert!(!renderer.last_camera_frame_reused());
        assert_eq!(
            renderer.frame().as_bytes(),
            fresh_camera_bits(&stage, &changed)
        );
        assert_ne!(renderer.frame().as_bytes(), original);
        renderer.render_with_camera(&stage, &changed).unwrap();
        assert!(renderer.last_camera_frame_reused());

        renderer.render(&stage, changed.revision()).unwrap();
        assert!(!renderer.last_camera_frame_reused());
        renderer.render_with_camera(&stage, &changed).unwrap();
        assert!(!renderer.last_camera_frame_reused());
        assert_eq!(
            renderer.frame().as_bytes(),
            fresh_camera_bits(&stage, &changed)
        );

        let invalid_layout = Camera::new(CameraConfig {
            resolution: (24, 24),
            ..CameraConfig::default()
        })
        .unwrap();
        assert!(
            renderer
                .render_with_camera(&stage, &invalid_layout)
                .is_err()
        );
        assert!(!renderer.last_camera_frame_reused());
        renderer.render_with_camera(&stage, &changed).unwrap();
        assert!(!renderer.last_camera_frame_reused());
        assert_eq!(
            renderer.frame().as_bytes(),
            fresh_camera_bits(&stage, &changed)
        );
    }

    #[test]
    fn cached_camera_pixels_never_hide_invalid_live_records() {
        let camera = camera();
        let mut stage = Stage::new();
        let mob = stage.add(vector());
        stage.add_to_scene(mob).unwrap();
        let mut renderer = RetainedFrameRenderer::new(config(1)).unwrap();
        renderer.render_with_camera(&stage, &camera).unwrap();
        renderer.render_with_camera(&stage, &camera).unwrap();
        assert!(renderer.last_camera_frame_reused());
        let before = renderer.frame().as_bytes().to_vec();
        stage
            .get_mut(mob)
            .unwrap()
            .buffer
            .write(1, "point", &[f32::NAN, 1.25, 0.0]);
        assert!(renderer.render_with_camera(&stage, &camera).is_err());
        assert!(!renderer.last_camera_frame_reused());
        assert_eq!(renderer.frame().as_bytes(), before);
        stage
            .get_mut(mob)
            .unwrap()
            .buffer
            .write(1, "point", &[0.5, 1.75, 0.0]);
        renderer.render_with_camera(&stage, &camera).unwrap();
        assert!(!renderer.last_camera_frame_reused());
        assert_eq!(
            renderer.frame().as_bytes(),
            fresh_camera_bits(&stage, &camera)
        );
        renderer.render_with_camera(&stage, &camera).unwrap();
        assert!(renderer.last_camera_frame_reused());
    }
}

#[cfg(test)]
#[path = "retained_texture_tests.rs"]
mod texture_tests;
