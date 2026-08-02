//! The retained render plan: lazy synchronization from Marionette (§10.8, §8.2).
//!
//! > … synchronized *lazily* from the RecordBuffer under §8.2's mirror rule …
//!
//! ## What "lazily" has to mean to be worth anything
//!
//! Not "recompute a bit less". The plan holds compiled outlines across frames
//! and, for a renderable whose declared axes have not moved, **never reads its
//! points at all**. That is the difference between a cache and a retained plan:
//! a cache still has to look at the data to discover it is unchanged, and
//! looking at the data is most of the cost — reading a glyph's points, hashing
//! them, splitting curves, measuring arc length.
//!
//! So the sync loop's inner question is a comparison of seven integers
//! ([`crate::revision`]), and everything expensive sits behind it. The cheap
//! things that genuinely must happen every frame — the painter-order instance
//! list — are rebuilt every frame, because an instance is an index, an affine
//! placement and two more indices, and pretending otherwise would buy nothing
//! and cost correctness.
//!
//! ## The observable
//!
//! [`SyncStats`] exists because "untouched resources never recompile" is a claim
//! about work *not* done, which is untestable unless the work counts itself.
//! `MirrorSet::materializations` and `CachedArcLength::rebuilds` are the same
//! idea one layer down, and the same reason.

use crate::hint::Hint;
use crate::revision::{Axis, Dependency, Revisions};
use crate::table::{
    Instance, Segment, Shape, ShapeTable, Style, StyleTable, TableError, check_row_count,
    compile_shape, shape_digest,
};
use fmn_core::types::Vec3;
use fmn_geom::{GeomError, quadpath::QuadPath};
use fmn_hash::{Digest, Sha256};
use fmn_mobject::{Mob, Placement, RecordBuffer, Stage};
use std::collections::{HashMap, HashSet};

/// The axes a compiled outline depends on.
///
/// Geometry because it *is* the geometry, topology because a point count or a
/// family change reshapes it. Deliberately **not** style, order or camera: a
/// recolour, a `z_index` bump and a pan must all leave a compiled outline alone,
/// which is the §10.8 promise this constant makes checkable.
pub const SHAPE_AXES: [Axis; 2] = [Axis::Topology, Axis::Geometry];

/// The axes a style row depends on.
pub const STYLE_AXES: [Axis; 1] = [Axis::Style];

/// Record fields the compiled outline actually observes today.
///
/// Keep this consumption list exact: a field-scoped view of user metadata must
/// not turn into object-wide recompilation merely because both share a record.
const SHAPE_VIEW_FIELDS: [&str; 1] = ["point"];

/// Record fields [`read_style`] observes.
const STYLE_VIEW_FIELDS: [&str; 4] = [
    "stroke_rgba",
    "stroke_width",
    "fill_rgba",
    "fill_border_width",
];

/// Admission limits for one retained-plan synchronization.
///
/// The instance default matches [`crate::bin::BinningLimits`]. The retained
/// geometry ceiling admits sixteen maximum-size Chisel paths while bounding
/// append-only outline history to a few hundred MiB rather than letting a
/// continuously changing object grow until allocator failure. Callers with a
/// deliberately larger, provisioned scene can pass explicit limits through
/// [`RenderPlan::sync_with_limits`]; the `u32` table width remains absolute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderPlanLimits {
    /// Maximum painter-ordered instances produced by one sync.
    pub max_instances: u64,
    /// Maximum input curves materialized for geometry rebuilds in one sync.
    pub max_input_curves: u64,
    /// Maximum segments retained across every interned outline.
    pub max_retained_segments: u64,
    /// Maximum distinct retained outlines.
    pub max_retained_shapes: u64,
    /// Maximum distinct retained style rows.
    pub max_retained_styles: u64,
}

impl Default for RenderPlanLimits {
    fn default() -> Self {
        Self {
            max_instances: 1 << 20,
            max_input_curves: 1 << 20,
            max_retained_segments: 1 << 20,
            max_retained_shapes: 1 << 20,
            max_retained_styles: 1 << 20,
        }
    }
}

/// A retained-plan synchronization could not represent scene geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncError {
    /// One renderable's point run violated Chisel's shared-anchor layout.
    InvalidGeometry {
        /// The renderable whose geometry was rejected.
        mob: Mob,
        /// Chisel's precise representation error.
        source: GeomError,
    },
    /// A declared retained-plan count exceeded its caller-selected ceiling.
    LimitExceeded {
        /// The table or work axis being bounded.
        resource: &'static str,
        /// Rows or input curves requested.
        requested: u64,
        /// Configured maximum.
        limit: u64,
    },
    /// A temporary synchronization table could not reserve its bounded input.
    AllocationFailed {
        /// The temporary table being reserved.
        resource: &'static str,
        /// Elements requested.
        requested: u64,
    },
    /// A retained table could not preserve its index or allocation contract.
    Table(TableError),
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidGeometry { mob, source } => {
                write!(f, "cannot compile geometry for {mob:?}: {source}")
            }
            Self::LimitExceeded {
                resource,
                requested,
                limit,
            } => write!(
                f,
                "retained plan {resource} needs {requested}, exceeding the limit {limit}"
            ),
            Self::AllocationFailed {
                resource,
                requested,
            } => write!(
                f,
                "could not reserve {requested} elements for retained plan {resource}"
            ),
            Self::Table(source) => write!(f, "could not build retained plan: {source}"),
        }
    }
}

impl std::error::Error for SyncError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidGeometry { source, .. } => Some(source),
            Self::Table(source) => Some(source),
            Self::LimitExceeded { .. } | Self::AllocationFailed { .. } => None,
        }
    }
}

impl From<TableError> for SyncError {
    fn from(source: TableError) -> Self {
        Self::Table(source)
    }
}

fn check_limit(resource: &'static str, requested: u64, limit: u64) -> Result<(), SyncError> {
    if requested > limit {
        return Err(SyncError::LimitExceeded {
            resource,
            requested,
            limit,
        });
    }
    Ok(())
}

fn count_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn checked_add_count(
    resource: &'static str,
    left: usize,
    right: usize,
) -> Result<usize, SyncError> {
    left.checked_add(right).ok_or({
        SyncError::Table(TableError::IndexCapacityExceeded {
            resource,
            requested: u64::MAX,
        })
    })
}

fn prepared_index(
    resource: &'static str,
    current: usize,
    pending: usize,
) -> Result<u32, SyncError> {
    let index = checked_add_count(resource, current, pending)?;
    let rows = index.checked_add(1).ok_or({
        SyncError::Table(TableError::IndexCapacityExceeded {
            resource,
            requested: u64::MAX,
        })
    })?;
    check_row_count(resource, rows)?;
    u32::try_from(index).map_err(|_| {
        SyncError::Table(TableError::IndexCapacityExceeded {
            resource,
            requested: count_u64(rows),
        })
    })
}

fn try_reserve_exact<T>(
    values: &mut Vec<T>,
    resource: &'static str,
    additional: usize,
) -> Result<(), SyncError> {
    values
        .try_reserve_exact(additional)
        .map_err(|_| SyncError::AllocationFailed {
            resource,
            requested: count_u64(additional),
        })
}

fn writable_view_affects_any(buffer: &RecordBuffer, fields: &[&str]) -> bool {
    fields
        .iter()
        .any(|field| buffer.writable_view_affects(field))
}

/// What one [`RenderPlan::sync`] did, and — more usefully — did not do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SyncStats {
    /// Renderables the draw plan presented.
    pub visited: usize,
    /// Outlines compiled from points this sync.
    pub shapes_compiled: usize,
    /// Renderables whose outline was reused without their points being read.
    pub shapes_reused: usize,
    /// Outlines that interned onto an existing compiled shape — the instancing
    /// win, counted separately from a reuse because it is a different mechanism:
    /// this one is two *different* mobjects sharing one outline.
    pub shapes_shared: usize,
    /// Style rows recomputed this sync.
    pub styles_rebuilt: usize,
    /// Retained entries dropped because their mobject left the draw plan.
    pub dropped: usize,
}

/// One renderable's retained compilation.
#[derive(Debug, Clone)]
struct Retained {
    shape_dep: Dependency,
    style_dep: Dependency,
    shape: u32,
    style: u32,
    /// The object-space first-anchor normalization, kept so a reuse needs no
    /// point read. The entry placement composes with it per frame.
    local_origin: Vec3,
}

/// The compiled, backend-neutral render IR for a scene, retained across frames.
#[derive(Debug, Clone)]
pub struct RenderPlan {
    segments: Vec<Segment>,
    styles: StyleTable,
    shapes: ShapeTable,
    retained: HashMap<Mob, Retained>,
    scratch_instances: Vec<Instance>,
    scratch_retained: HashMap<Mob, Retained>,
    scratch_seen: HashSet<Mob>,
    stats: SyncStats,
    geometry_key: GeometryIdentity,
    plan_key: PlanIdentity,
}

/// Collision-resistant identity of the compiled geometry tables.
///
/// This is deliberately separate from [`PlanIdentity`]: a recolour or painter
/// reorder does not invalidate geometry-only artifacts such as
/// [`crate::fill::MonoTable`]. The zero value is reserved for an unbuilt
/// artifact and is never emitted by the SHA-256 construction below.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct GeometryIdentity(Digest);

impl Default for GeometryIdentity {
    fn default() -> Self {
        Self(Digest::from_bytes([0; 32]))
    }
}

/// Collision-resistant identity of every pixel-deciding row in a render plan.
///
/// Unlike an allocation address or a local generation counter, a content
/// identity lets independently compiled but identical plans share derived
/// artifacts safely, while reordered or stale instance lists cannot alias.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct PlanIdentity(Digest);

impl Default for PlanIdentity {
    fn default() -> Self {
        Self(Digest::from_bytes([0; 32]))
    }
}

impl Default for RenderPlan {
    fn default() -> Self {
        let mut plan = Self {
            segments: Vec::new(),
            styles: StyleTable::default(),
            shapes: ShapeTable::default(),
            retained: HashMap::new(),
            scratch_instances: Vec::new(),
            scratch_retained: HashMap::new(),
            scratch_seen: HashSet::new(),
            stats: SyncStats::default(),
            geometry_key: GeometryIdentity::default(),
            plan_key: PlanIdentity::default(),
        };
        plan.refresh_identities();
        plan
    }
}

/// Allocation-free canonical input to the in-tree SHA-256 implementation.
///
/// These identities are process-local guards rather than durable documents, so
/// they do not ride the size-limited serialization envelope. They preserve
/// exact float bits (matching `StyleTable`'s equality) and domain-separate every
/// sequence, giving the same collision resistance as the content-addressed
/// cache without making large scenes fail merely because an integrity check was
/// requested.
struct IdentityHasher(Sha256);

impl IdentityHasher {
    fn new(domain: &[u8]) -> Self {
        let mut hash = Sha256::new();
        hash.update(domain);
        Self(hash)
    }

    fn bytes(&mut self, value: &[u8]) {
        self.0.update(value);
    }

    fn bool(&mut self, value: bool) {
        self.bytes(&[u8::from(value)]);
    }

    fn u32(&mut self, value: u32) {
        self.bytes(&value.to_le_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.bytes(&value.to_le_bytes());
    }

    fn f32(&mut self, value: f32) {
        self.u32(value.to_bits());
    }

    fn f64(&mut self, value: f64) {
        self.u64(value.to_bits());
    }

    fn digest(&mut self, value: Digest) {
        self.bytes(value.as_bytes());
    }

    fn finish(self) -> Digest {
        self.0.finalize()
    }
}

impl RenderPlan {
    /// An empty plan.
    #[must_use]
    pub fn new() -> RenderPlan {
        RenderPlan::default()
    }

    /// Synchronize against `stage`, at camera revision `camera`.
    ///
    /// Uses [`RenderPlanLimits::default`]. Call [`RenderPlan::sync_with_limits`]
    /// for an explicitly provisioned larger or smaller workload.
    ///
    /// # Errors
    /// Returns [`SyncError`] before mutating the retained plan when geometry is
    /// malformed or the synchronization cannot satisfy its resource contract.
    pub fn sync(&mut self, stage: &Stage, camera: u64) -> Result<SyncStats, SyncError> {
        self.sync_with_limits(stage, camera, RenderPlanLimits::default())
    }

    /// Synchronize with explicit retained-table admission limits.
    ///
    /// Preparation is transactional: changed geometry is compiled and every
    /// prospective table index is checked in temporary storage. Only after all
    /// checks and persistent reservations succeed are the append-only tables,
    /// painter instances, retained entries, statistics, and identities replaced.
    ///
    /// # Errors
    /// Returns [`SyncError`] without changing any observable plan state.
    pub fn sync_with_limits(
        &mut self,
        stage: &Stage,
        camera: u64,
        limits: RenderPlanLimits,
    ) -> Result<SyncStats, SyncError> {
        let plan = stage.draw_plan();
        let draw_items = count_u64(plan.items().len());
        check_limit("instances", draw_items, limits.max_instances)?;
        check_row_count("instance rows", plan.items().len())?;
        check_limit(
            "retained segments",
            count_u64(self.segments.len()),
            limits.max_retained_segments,
        )?;
        check_limit(
            "retained shapes",
            count_u64(self.shapes.shapes().len()),
            limits.max_retained_shapes,
        )?;
        check_limit(
            "retained styles",
            count_u64(self.styles.len()),
            limits.max_retained_styles,
        )?;

        // Chisel's whole-run invariant is available from record metadata. Check
        // it before materializing any changed point column so one malformed
        // renderable cannot make earlier valid outlines pay compilation work
        // before the atomic refusal.
        for item in plan.items() {
            let mob = item.mob;
            let Some(entry) = stage.get(mob) else {
                continue;
            };
            if entry.buffer.schema().field_width("point") != Some(3) {
                continue;
            }
            let len = entry.buffer.len();
            if len != 0 && len.is_multiple_of(2) {
                return Err(SyncError::InvalidGeometry {
                    mob,
                    source: GeomError::EvenPointCount { len },
                });
            }
        }

        let mut instances = std::mem::take(&mut self.scratch_instances);
        instances.clear();
        try_reserve_exact(&mut instances, "prepared instances", plan.items().len())?;
        let mut seen = std::mem::take(&mut self.scratch_seen);
        seen.clear();
        seen.try_reserve(plan.items().len())
            .map_err(|_| SyncError::AllocationFailed {
                resource: "live mobjects",
                requested: draw_items,
            })?;
        let mut next_retained = std::mem::take(&mut self.scratch_retained);
        next_retained.clear();
        next_retained
            .try_reserve(plan.items().len())
            .map_err(|_| SyncError::AllocationFailed {
                resource: "retained entries",
                requested: draw_items,
            })?;

        let mut pending_shapes: Vec<(u32, Shape, Vec<Segment>)> = Vec::new();
        let mut pending_shape_indices: HashMap<Digest, u32> = HashMap::new();
        let mut pending_styles: Vec<(u32, Style)> = Vec::new();
        let mut pending_style_indices: HashMap<[u64; 45], u32> = HashMap::new();
        let mut pending_segments = 0usize;
        let mut input_curves = 0u64;
        let mut stats = SyncStats::default();

        for (order, item) in plan.items().iter().enumerate() {
            let mob = item.mob;
            let Some(now) = Revisions::read(stage, mob).map(|r| r.with_camera(camera)) else {
                continue;
            };
            stats.visited += 1;
            seen.insert(mob);

            let previous = next_retained
                .get(&mob)
                .cloned()
                .or_else(|| self.retained.get(&mob).cloned());
            if let Some(retained) = previous.clone() {
                next_retained.entry(mob).or_insert(retained);
            }

            let Some(entry_placement) = stage.placement(mob) else {
                continue;
            };
            let entry = stage.get(mob);
            let (hint_unsafe, style_unsafe) = entry.map_or((false, false), |entry| {
                (
                    writable_view_affects_any(&entry.buffer, &SHAPE_VIEW_FIELDS),
                    writable_view_affects_any(&entry.buffer, &STYLE_VIEW_FIELDS),
                )
            });

            let (shape, local_origin) = match &previous {
                Some(retained) if !hint_unsafe && !retained.shape_dep.is_stale(&now) => {
                    stats.shapes_reused += 1;
                    (retained.shape, retained.local_origin)
                }
                _ => {
                    let counted_from_schema = entry.and_then(|entry| {
                        (entry.buffer.schema().field_width("point") == Some(3))
                            .then_some(entry.buffer.len())
                    });
                    if let Some(point_count) = counted_from_schema {
                        let curves = count_u64(point_count.saturating_sub(1) / 2);
                        input_curves =
                            input_curves
                                .checked_add(curves)
                                .ok_or(SyncError::LimitExceeded {
                                    resource: "input curves",
                                    requested: u64::MAX,
                                    limit: limits.max_input_curves,
                                })?;
                        check_limit("input curves", input_curves, limits.max_input_curves)?;
                    }
                    let Some(object_points) = stage.get_object_points(mob) else {
                        continue;
                    };
                    if object_points.is_empty() {
                        continue;
                    }
                    let local_origin = object_points[0];
                    let mut local = Vec::new();
                    try_reserve_exact(&mut local, "shape-local points", object_points.len())?;
                    local.extend(object_points.iter().map(|point| {
                        [
                            point[0] - local_origin[0],
                            point[1] - local_origin[1],
                            point[2] - local_origin[2],
                        ]
                    }));
                    let path = QuadPath::from_points(local)
                        .map_err(|source| SyncError::InvalidGeometry { mob, source })?;
                    if counted_from_schema.is_none() {
                        input_curves = input_curves
                            .checked_add(count_u64(path.num_curves()))
                            .ok_or(SyncError::LimitExceeded {
                                resource: "input curves",
                                requested: u64::MAX,
                                limit: limits.max_input_curves,
                            })?;
                        check_limit("input curves", input_curves, limits.max_input_curves)?;
                    }

                    let digest = shape_digest(&object_points);
                    let shape = if let Some(index) = self.shapes.shape_index(digest) {
                        stats.shapes_shared += 1;
                        index
                    } else if let Some(&index) = pending_shape_indices.get(&digest) {
                        stats.shapes_shared += 1;
                        index
                    } else {
                        let pending_shape_count =
                            checked_add_count("shape rows", pending_shapes.len(), 1)?;
                        let retained_shape_count = checked_add_count(
                            "shape rows",
                            self.shapes.shapes().len(),
                            pending_shape_count,
                        )?;
                        check_limit(
                            "retained shapes",
                            count_u64(retained_shape_count),
                            limits.max_retained_shapes,
                        )?;
                        let index = prepared_index(
                            "shape rows",
                            self.shapes.shapes().len(),
                            pending_shapes.len(),
                        )?;
                        let first_segment = checked_add_count(
                            "segment rows",
                            self.segments.len(),
                            pending_segments,
                        )?;
                        let first_segment = u32::try_from(first_segment).map_err(|_| {
                            SyncError::Table(TableError::IndexCapacityExceeded {
                                resource: "segment rows",
                                requested: count_u64(first_segment).saturating_add(1),
                            })
                        })?;
                        let hint = Hint::of(stage, mob).translated([
                            -local_origin[0],
                            -local_origin[1],
                            -local_origin[2],
                        ]);
                        let (shape, segments) = compile_shape(digest, &path, hint, first_segment)?;
                        let retained_segment_count = checked_add_count(
                            "segment rows",
                            self.segments.len(),
                            checked_add_count("segment rows", pending_segments, segments.len())?,
                        )?;
                        check_limit(
                            "retained segments",
                            count_u64(retained_segment_count),
                            limits.max_retained_segments,
                        )?;
                        check_row_count("segment rows", retained_segment_count)?;

                        pending_shapes
                            .try_reserve(1)
                            .map_err(|_| SyncError::AllocationFailed {
                                resource: "pending shapes",
                                requested: count_u64(pending_shapes.len()).saturating_add(1),
                            })?;
                        pending_shape_indices.try_reserve(1).map_err(|_| {
                            SyncError::AllocationFailed {
                                resource: "pending shape index",
                                requested: count_u64(pending_shape_indices.len()).saturating_add(1),
                            }
                        })?;
                        pending_segments = pending_segments.checked_add(segments.len()).ok_or(
                            SyncError::Table(TableError::IndexCapacityExceeded {
                                resource: "segment rows",
                                requested: u64::MAX,
                            }),
                        )?;
                        pending_shape_indices.insert(digest, index);
                        pending_shapes.push((index, shape, segments));
                        stats.shapes_compiled += 1;
                        index
                    };
                    (shape, local_origin)
                }
            };

            let style = match &previous {
                Some(retained) if !style_unsafe && !retained.style_dep.is_stale(&now) => {
                    retained.style
                }
                _ => {
                    stats.styles_rebuilt += 1;
                    let row = read_style(stage, mob);
                    let key = row.bits();
                    if let Some(index) = self.styles.index_of(&row) {
                        index
                    } else if let Some(&index) = pending_style_indices.get(&key) {
                        index
                    } else {
                        let pending_style_count =
                            checked_add_count("style rows", pending_styles.len(), 1)?;
                        let retained_style_count = checked_add_count(
                            "style rows",
                            self.styles.len(),
                            pending_style_count,
                        )?;
                        check_limit(
                            "retained styles",
                            count_u64(retained_style_count),
                            limits.max_retained_styles,
                        )?;
                        let index =
                            prepared_index("style rows", self.styles.len(), pending_styles.len())?;
                        pending_styles
                            .try_reserve(1)
                            .map_err(|_| SyncError::AllocationFailed {
                                resource: "pending styles",
                                requested: count_u64(pending_styles.len()).saturating_add(1),
                            })?;
                        pending_style_indices.try_reserve(1).map_err(|_| {
                            SyncError::AllocationFailed {
                                resource: "pending style index",
                                requested: count_u64(pending_style_indices.len()).saturating_add(1),
                            }
                        })?;
                        pending_style_indices.insert(key, index);
                        pending_styles.push((index, row));
                        index
                    }
                }
            };

            let order = u32::try_from(order).map_err(|_| {
                SyncError::Table(TableError::IndexCapacityExceeded {
                    resource: "painter order",
                    requested: draw_items,
                })
            })?;
            let volatile = hint_unsafe || style_unsafe;
            let placement = entry_placement.compose(Placement::from_translation(local_origin));
            instances.push(Instance {
                shape,
                style,
                mob,
                placement,
                order,
                revisions: now.fold(),
                volatile,
                hint_unsafe,
            });
            next_retained.insert(
                mob,
                Retained {
                    shape_dep: Dependency::new(now, &SHAPE_AXES),
                    style_dep: Dependency::new(now, &STYLE_AXES),
                    shape,
                    style,
                    local_origin,
                },
            );
        }

        stats.dropped = self
            .retained
            .keys()
            .filter(|mob| !seen.contains(mob))
            .count();

        self.segments
            .try_reserve_exact(pending_segments)
            .map_err(|_| SyncError::AllocationFailed {
                resource: "segment rows",
                requested: count_u64(self.segments.len())
                    .saturating_add(count_u64(pending_segments)),
            })?;
        self.styles.reserve_additional(pending_styles.len())?;
        self.shapes.reserve_shapes(pending_shapes.len())?;

        for (index, style) in pending_styles {
            self.styles.insert_prepared(index, style);
        }
        for (index, shape, mut segments) in pending_shapes {
            debug_assert_eq!(
                usize::try_from(shape.first_segment).ok(),
                Some(self.segments.len())
            );
            self.segments.append(&mut segments);
            self.shapes.insert_prepared(index, shape);
        }
        self.shapes.swap_instances(&mut instances);
        instances.clear();
        self.scratch_instances = instances;
        std::mem::swap(&mut self.retained, &mut next_retained);
        next_retained.clear();
        self.scratch_retained = next_retained;
        seen.clear();
        self.scratch_seen = seen;
        self.stats = stats;

        if stats.shapes_compiled > 0 {
            self.geometry_key = self.compute_geometry_identity();
        }
        self.plan_key = self.compute_identity(self.geometry_key);
        Ok(stats)
    }

    /// The segment table.
    #[must_use]
    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    /// The interned style table.
    #[must_use]
    pub fn styles(&self) -> &StyleTable {
        &self.styles
    }

    /// The interned shapes and their painter-ordered instances.
    #[must_use]
    pub fn shapes(&self) -> &ShapeTable {
        &self.shapes
    }

    /// What the last [`sync`] did.
    ///
    /// [`sync`]: RenderPlan::sync
    #[must_use]
    pub fn stats(&self) -> SyncStats {
        self.stats
    }

    /// Identity of the shape-indexed geometry consumed by derived fill tables.
    pub(crate) fn geometry_identity(&self) -> GeometryIdentity {
        self.geometry_key
    }

    /// Identity of geometry, styles, and the painter-ordered instance list.
    pub(crate) fn identity(&self) -> PlanIdentity {
        self.plan_key
    }

    /// Refresh both cached identities once after a synchronization.
    fn refresh_identities(&mut self) {
        let geometry = self.compute_geometry_identity();
        let plan = self.compute_identity(geometry);
        self.geometry_key = geometry;
        self.plan_key = plan;
    }

    fn compute_geometry_identity(&self) -> GeometryIdentity {
        let mut hash = IdentityHasher::new(b"fmn-render/geometry-identity/v1");

        hash.u64(self.segments.len() as u64);
        for segment in &self.segments {
            for point in [segment.p0, segment.p1, segment.p2] {
                for component in point {
                    hash.f64(component);
                }
            }
            hash.f64(segment.s0);
            hash.f64(segment.s1);
        }

        let shapes = self.shapes.shapes();
        hash.u64(shapes.len() as u64);
        for shape in shapes {
            hash.digest(shape.digest);
            hash.u32(shape.first_segment);
            hash.u32(shape.segment_count);
            for point in [shape.bounds.min, shape.bounds.mid, shape.bounds.max] {
                for component in point {
                    hash.f64(component);
                }
            }
            hash_hint(&mut hash, shape.hint);
            hash.u64(shape.arc_length.curve_lengths().len() as u64);
            for &length in shape.arc_length.curve_lengths() {
                hash.f64(length);
            }
            hash.u64(shape.subpath_starts.len() as u64);
            for &start in &shape.subpath_starts {
                hash.u32(start);
            }
        }

        GeometryIdentity(hash.finish())
    }

    fn compute_identity(&self, geometry: GeometryIdentity) -> PlanIdentity {
        let mut hash = IdentityHasher::new(b"fmn-render/plan-identity/v1");
        hash.digest(geometry.0);

        let instances = self.shapes.instances();
        hash.u64(instances.len() as u64);
        for instance in instances {
            hash.u32(instance.shape);
            hash.u32(instance.style);
            match self.styles.get(instance.style) {
                Some(style) => {
                    hash.bool(true);
                    for &component in style
                        .stroke_rgba
                        .iter()
                        .chain(style.stroke_rgba_end.iter())
                        .chain(style.fill_rgba.iter())
                        .chain(style.fill_rgba_end.iter())
                    {
                        hash.f32(component);
                    }
                    hash.f32(style.stroke_width);
                    hash.f32(style.stroke_width_end);
                    hash.f32(style.fill_border_width);
                    hash.f32(style.anti_alias_width);
                    hash.f64(style.joint_type.to_code());
                    hash.f64(style.is_fixed_in_frame);
                    for component in style.shading {
                        hash.f64(component);
                    }
                    for component in style.clip_planes.iter().flatten() {
                        hash.f64(*component);
                    }
                    hash.bool(style.flat_stroke);
                    hash.bool(style.scale_stroke_with_zoom);
                    hash.bool(style.stroke_behind);
                    hash.bool(style.depth_test);
                }
                None => hash.bool(false),
            }
            for component in instance.placement.coefficients() {
                hash.f64(component);
            }
            hash.u32(instance.order);
            // A point view disables hint-derived tile classification. Binning
            // built on the other side of this transition is therefore stale
            // even when the point bytes themselves have not moved.
            hash.bool(instance.hint_unsafe);
        }

        PlanIdentity(hash.finish())
    }
}

/// Feed a primitive hint's full payload, not merely its diagnostic name.
///
/// The parameters participate in specialized fill and containment kernels, so
/// hashing only the enum discriminant would let two differently placed circles
/// or rectangles share a binning that was built for the other one.
fn hash_hint(hash: &mut IdentityHasher, hint: Hint) {
    fn point(hash: &mut IdentityHasher, tag: u8, center: [f64; 3]) {
        hash.bytes(&[tag]);
        for component in center {
            hash.f64(component);
        }
    }

    match hint {
        Hint::General => hash.bytes(&[0]),
        Hint::Line => hash.bytes(&[1]),
        Hint::Polyline { closed } => {
            hash.bytes(&[2]);
            hash.bool(closed);
        }
        Hint::Arc {
            center,
            radius,
            start_angle,
            angle,
        } => {
            point(hash, 3, center);
            hash.f64(radius);
            hash.f64(start_angle);
            hash.f64(angle);
        }
        Hint::Circle { center, radius } => {
            point(hash, 4, center);
            hash.f64(radius);
        }
        Hint::Dot { center, radius } => {
            point(hash, 5, center);
            hash.f64(radius);
        }
        Hint::Rect {
            center,
            width,
            height,
        } => {
            point(hash, 6, center);
            hash.f64(width);
            hash.f64(height);
        }
        Hint::RoundedRect {
            center,
            width,
            height,
            corner_radius,
        } => {
            point(hash, 7, center);
            hash.f64(width);
            hash.f64(height);
            hash.f64(corner_radius);
        }
    }
}

/// Decode one sRGB-encoded colour record into the linear light the IR stores.
///
/// **This is BN-04's "render boundary", and it had no implementation.** The
/// record buffer holds what manim holds — sRGB-encoded components, because
/// `mobject.data` is API surface — while [`Style`] documents linear light and
/// every consumer already assumes it: `fill_rgba_at` and `stroke_rgba_at` both
/// say a ramp "happens where colour interpolation is defined, not in an encoded
/// space". Between the writer and those readers the decode simply did not exist,
/// so a mid-tone would have rendered at its encoded value — visibly wrong, and
/// wrong in the direction that looks like a lighting choice rather than a bug.
///
/// Alpha does not decode. It is a coverage fraction, not a light intensity, and
/// gamma-encoding it is the mistake `fmn_frame::transfer` names on the way out.
///
/// The decode routes through [`fmn_frame::transfer::srgb_decode`] rather than
/// `fmn_core::color::srgb_eotf`: the two compute the same function, but the
/// former rides `fmn_dmath::pow` and the latter rides `std::powf`, and ADR-0010's
/// first binding property is that fmn-dmath owns every transcendental on the
/// certified path. It is decoded **once per interned style row**, not per pixel.
fn decode_rgba(rgba: [f32; 4]) -> [f32; 4] {
    [
        fmn_frame::transfer::srgb_decode(f64::from(rgba[0])) as f32,
        fmn_frame::transfer::srgb_decode(f64::from(rgba[1])) as f32,
        fmn_frame::transfer::srgb_decode(f64::from(rgba[2])) as f32,
        rgba[3],
    ]
}

/// Read one renderable's style row out of its record buffer.
///
/// The Reference stores these per point; this reads the ramp's endpoints, which
/// is the subset [`Style`] documents. A point-less mobject yields the default
/// row rather than an error — a group with no geometry of its own is normal, and
/// it simply contributes no instance.
fn read_style(stage: &Stage, mob: Mob) -> Style {
    let mut row = Style::default();
    let Some(entry) = stage.get(mob) else {
        return row;
    };
    let buffer = &entry.buffer;
    let n = buffer.len();
    if n == 0 {
        return row.with_uniforms(entry.uniforms());
    }
    let last = n - 1;
    let rgba = |index: usize, field: &str| -> [f32; 4] {
        buffer
            .read(index, field)
            .and_then(|v| <[f32; 4]>::try_from(v.as_slice()).ok())
            .unwrap_or([0.0; 4])
    };
    let scalar = |index: usize, field: &str| -> f32 {
        buffer
            .read(index, field)
            .and_then(|v| v.first().copied())
            .unwrap_or(0.0)
    };
    row.stroke_rgba = decode_rgba(rgba(0, "stroke_rgba"));
    row.stroke_rgba_end = decode_rgba(rgba(last, "stroke_rgba"));
    row.fill_rgba = decode_rgba(rgba(0, "fill_rgba"));
    row.fill_rgba_end = decode_rgba(rgba(last, "fill_rgba"));
    row.stroke_width = scalar(0, "stroke_width");
    row.stroke_width_end = scalar(last, "stroke_width");
    row.fill_border_width = scalar(0, "fill_border_width");
    row.with_uniforms(entry.uniforms())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmn_mobject::{Mobject, RecordBuffer, RecordSchema};

    fn vmobject(points: &[[f64; 3]]) -> Mobject {
        let mut buffer = RecordBuffer::new(RecordSchema::vmobject(), points.len()).unwrap();
        for (i, p) in points.iter().enumerate() {
            buffer.write(i, "point", &[p[0] as f32, p[1] as f32, p[2] as f32]);
            buffer.write(i, "stroke_rgba", &[1.0, 1.0, 1.0, 1.0]);
            buffer.write(i, "stroke_width", &[2.0]);
        }
        Mobject::from_buffer(buffer)
    }

    fn tri_at(dx: f64, dy: f64) -> Mobject {
        vmobject(&[
            [dx, dy, 0.0],
            [dx + 1.0, dy, 0.0],
            [dx + 2.0, dy, 0.0],
            [dx + 2.0, dy + 0.5, 0.0],
            [dx + 2.0, dy + 1.0, 0.0],
        ])
    }

    fn staged(n: usize) -> (Stage, Vec<Mob>) {
        let mut stage = Stage::new();
        let mut mobs = Vec::new();
        for i in 0..n {
            let mob = stage.add(tri_at(i as f64 * 10.0, 0.0));
            stage.add_to_scene(mob).expect("live");
            mobs.push(mob);
        }
        (stage, mobs)
    }

    fn sync_valid(plan: &mut RenderPlan, stage: &Stage, camera: u64) -> SyncStats {
        plan.sync(stage, camera).expect("valid test scene")
    }

    #[test]
    fn a_first_sync_compiles_and_a_second_reads_no_points() {
        // The claim the whole module exists for. Not "recompiles less" —
        // *nothing* is compiled on the second pass, and the points are never
        // touched.
        let (stage, _) = staged(3);
        let mut plan = RenderPlan::new();

        let first = sync_valid(&mut plan, &stage, 0);
        assert_eq!(first.visited, 3);
        assert_eq!(first.shapes_compiled, 1, "three copies of one outline");
        assert_eq!(first.shapes_shared, 2);
        assert_eq!(first.shapes_reused, 0);

        let second = sync_valid(&mut plan, &stage, 0);
        assert_eq!(second.visited, 3);
        assert_eq!(second.shapes_compiled, 0);
        assert_eq!(second.shapes_shared, 0);
        assert_eq!(second.shapes_reused, 3);
        assert_eq!(second.styles_rebuilt, 0);
    }

    #[test]
    fn malformed_shared_anchor_geometry_is_named_and_atomic() {
        let (mut stage, _) = staged(1);
        let mut plan = RenderPlan::new();
        sync_valid(&mut plan, &stage, 0);

        let before_segments = plan.segments().to_vec();
        let before_styles = plan.styles().rows().to_vec();
        let before_shapes = plan.shapes().shapes().to_vec();
        let before_instances = plan.shapes().instances().to_vec();
        let before_stats = plan.stats();
        let before_geometry = plan.geometry_identity();
        let before_identity = plan.identity();
        let before_retained = plan.retained.len();

        let malformed = stage.add(vmobject(&[[20.0, 0.0, 0.0], [21.0, 0.0, 0.0]]));
        stage.add_to_scene(malformed).expect("live");
        assert_eq!(
            plan.sync(&stage, 0)
                .expect_err("even point run must be refused"),
            SyncError::InvalidGeometry {
                mob: malformed,
                source: fmn_geom::GeomError::EvenPointCount { len: 2 },
            }
        );

        assert_eq!(plan.segments(), before_segments);
        assert_eq!(plan.styles().rows(), before_styles);
        assert_eq!(plan.shapes().shapes(), before_shapes);
        assert_eq!(plan.shapes().instances(), before_instances);
        assert_eq!(plan.stats(), before_stats);
        assert_eq!(plan.geometry_identity(), before_geometry);
        assert_eq!(plan.identity(), before_identity);
        assert_eq!(plan.retained.len(), before_retained);
    }

    #[test]
    fn retained_limits_count_deduplicated_rows_not_draw_items() {
        let (stage, _) = staged(8);
        let mut plan = RenderPlan::new();
        let limits = RenderPlanLimits {
            max_instances: 8,
            max_input_curves: 16,
            max_retained_segments: 2,
            max_retained_shapes: 1,
            max_retained_styles: 1,
        };

        let stats = plan
            .sync_with_limits(&stage, 0, limits)
            .expect("eight copies fit one retained shape and style row");

        assert_eq!(stats.shapes_compiled, 1);
        assert_eq!(stats.shapes_shared, 7);
        assert_eq!(plan.segments().len(), 2);
        assert_eq!(plan.shapes().shapes().len(), 1);
        assert_eq!(plan.styles().len(), 1);
        assert_eq!(plan.shapes().instances().len(), 8);
    }

    #[test]
    fn retained_limit_refusal_is_named_and_atomic() {
        let (mut stage, mobs) = staged(1);
        let mut plan = RenderPlan::new();
        sync_valid(&mut plan, &stage, 0);

        let before_segments = plan.segments().to_vec();
        let before_styles = plan.styles().rows().to_vec();
        let before_shapes = plan.shapes().shapes().to_vec();
        let before_instances = plan.shapes().instances().to_vec();
        let before_stats = plan.stats();
        let before_geometry = plan.geometry_identity();
        let before_identity = plan.identity();
        let before_retained = plan.retained.len();

        stage
            .get_mut(mobs[0])
            .expect("live")
            .buffer
            .write(2, "point", &[500.0, 500.0, 0.0]);
        let limits = RenderPlanLimits {
            max_retained_shapes: 1,
            ..RenderPlanLimits::default()
        };

        assert_eq!(
            plan.sync_with_limits(&stage, 0, limits)
                .expect_err("a second retained shape exceeds the limit"),
            SyncError::LimitExceeded {
                resource: "retained shapes",
                requested: 2,
                limit: 1,
            }
        );

        assert_eq!(plan.segments(), before_segments);
        assert_eq!(plan.styles().rows(), before_styles);
        assert_eq!(plan.shapes().shapes(), before_shapes);
        assert_eq!(plan.shapes().instances(), before_instances);
        assert_eq!(plan.stats(), before_stats);
        assert_eq!(plan.geometry_identity(), before_geometry);
        assert_eq!(plan.identity(), before_identity);
        assert_eq!(plan.retained.len(), before_retained);
    }

    #[test]
    fn each_sync_work_axis_has_a_named_limit() {
        let (two, _) = staged(2);
        let mut plan = RenderPlan::new();
        assert_eq!(
            plan.sync_with_limits(
                &two,
                0,
                RenderPlanLimits {
                    max_instances: 1,
                    ..RenderPlanLimits::default()
                },
            )
            .expect_err("two draw items exceed one instance"),
            SyncError::LimitExceeded {
                resource: "instances",
                requested: 2,
                limit: 1,
            }
        );
        assert!(plan.shapes().instances().is_empty());
        assert!(plan.shapes().shapes().is_empty());
        assert!(plan.segments().is_empty());

        let (one, _) = staged(1);
        assert_eq!(
            plan.sync_with_limits(
                &one,
                0,
                RenderPlanLimits {
                    max_input_curves: 1,
                    ..RenderPlanLimits::default()
                },
            )
            .expect_err("the two-curve outline exceeds one input curve"),
            SyncError::LimitExceeded {
                resource: "input curves",
                requested: 2,
                limit: 1,
            }
        );
        assert!(plan.shapes().shapes().is_empty());
        assert!(plan.segments().is_empty());

        assert_eq!(
            plan.sync_with_limits(
                &one,
                0,
                RenderPlanLimits {
                    max_retained_segments: 1,
                    ..RenderPlanLimits::default()
                },
            )
            .expect_err("the two compiled segments exceed one retained row"),
            SyncError::LimitExceeded {
                resource: "retained segments",
                requested: 2,
                limit: 1,
            }
        );
        assert!(plan.shapes().shapes().is_empty());
        assert!(plan.segments().is_empty());
    }

    #[test]
    fn retained_style_limit_refusal_preserves_the_old_style_and_identity() {
        let (mut stage, mobs) = staged(1);
        let mut plan = RenderPlan::new();
        sync_valid(&mut plan, &stage, 0);
        let before_styles = plan.styles().rows().to_vec();
        let before_instances = plan.shapes().instances().to_vec();
        let before_stats = plan.stats();
        let before_identity = plan.identity();

        stage
            .get_mut(mobs[0])
            .expect("live")
            .buffer
            .write(0, "stroke_rgba", &[1.0, 0.0, 0.0, 1.0]);
        assert_eq!(
            plan.sync_with_limits(
                &stage,
                0,
                RenderPlanLimits {
                    max_retained_styles: 1,
                    ..RenderPlanLimits::default()
                },
            )
            .expect_err("a second retained style exceeds the limit"),
            SyncError::LimitExceeded {
                resource: "retained styles",
                requested: 2,
                limit: 1,
            }
        );

        assert_eq!(plan.styles().rows(), before_styles);
        assert_eq!(plan.shapes().instances(), before_instances);
        assert_eq!(plan.stats(), before_stats);
        assert_eq!(plan.identity(), before_identity);
    }

    #[test]
    fn a_lower_limit_does_not_grandfather_existing_retained_rows() {
        let (stage, _) = staged(1);
        let mut plan = RenderPlan::new();
        sync_valid(&mut plan, &stage, 0);
        let before_instances = plan.shapes().instances().to_vec();
        let before_stats = plan.stats();
        let before_identity = plan.identity();

        assert_eq!(
            plan.sync_with_limits(
                &stage,
                0,
                RenderPlanLimits {
                    max_retained_segments: 1,
                    ..RenderPlanLimits::default()
                },
            )
            .expect_err("the existing two-row segment table exceeds the new limit"),
            SyncError::LimitExceeded {
                resource: "retained segments",
                requested: 2,
                limit: 1,
            }
        );
        assert_eq!(plan.shapes().instances(), before_instances);
        assert_eq!(plan.stats(), before_stats);
        assert_eq!(plan.identity(), before_identity);
    }

    #[test]
    fn writable_views_refresh_exactly_the_render_fields_they_expose() {
        let (mut stage, mobs) = staged(1);
        let mob = mobs[0];
        let mut plan = RenderPlan::new();
        sync_valid(&mut plan, &stage, 0);

        let point_view = stage
            .get_mut(mob)
            .expect("live")
            .buffer
            .export_field_view("point", true)
            .expect("point field");
        for _ in 0..2 {
            let stats = sync_valid(&mut plan, &stage, 0);
            assert_eq!(stats.shapes_reused, 0);
            assert_eq!(
                stats.shapes_shared, 1,
                "unchanged bytes may re-intern, but must be observed"
            );
            assert_eq!(stats.styles_rebuilt, 0);
            let instance = plan.shapes().instances()[0];
            assert!(instance.volatile);
            assert!(instance.hint_unsafe);
        }
        drop(point_view);

        // Detaching the writable point view conservatively advances its field
        // revision, so the final state is observed once more before ordinary
        // reuse resumes.
        let detached = sync_valid(&mut plan, &stage, 0);
        assert_eq!(detached.shapes_reused, 0);
        assert_eq!(detached.shapes_shared, 1);
        assert!(!plan.shapes().instances()[0].hint_unsafe);
        assert_eq!(sync_valid(&mut plan, &stage, 0).shapes_reused, 1);

        let style_view = stage
            .get_mut(mob)
            .expect("live")
            .buffer
            .export_field_view("fill_rgba", true)
            .expect("fill field");
        for _ in 0..2 {
            let stats = sync_valid(&mut plan, &stage, 0);
            assert_eq!(stats.shapes_reused, 1);
            assert_eq!(stats.styles_rebuilt, 1);
            let instance = plan.shapes().instances()[0];
            assert!(instance.volatile);
            assert!(
                !instance.hint_unsafe,
                "a colour view cannot invalidate geometry"
            );
        }
        drop(style_view);

        let whole_view = stage.get_mut(mob).expect("live").buffer.export_view(true);
        let stats = sync_valid(&mut plan, &stage, 0);
        assert_eq!(stats.shapes_reused, 0);
        assert_eq!(stats.shapes_shared, 1);
        assert_eq!(stats.styles_rebuilt, 1);
        assert!(plan.shapes().instances()[0].hint_unsafe);
        drop(whole_view);
    }

    #[test]
    fn a_writable_custom_field_view_does_not_invalidate_render_state() {
        let schema = RecordSchema::new(
            &[
                ("point", 3),
                ("stroke_rgba", 4),
                ("stroke_width", 1),
                ("joint_angle", 1),
                ("fill_rgba", 4),
                ("base_normal", 3),
                ("fill_border_width", 1),
                ("user_metadata", 1),
            ],
            &["point"],
            &["point"],
        )
        .unwrap();
        let points = [
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [2.0, 0.5, 0.0],
            [2.0, 1.0, 0.0],
        ];
        let mut buffer = RecordBuffer::new(schema, points.len()).unwrap();
        for (i, point) in points.iter().enumerate() {
            buffer.write(i, "point", point);
            buffer.write(i, "stroke_rgba", &[1.0; 4]);
            buffer.write(i, "stroke_width", &[2.0]);
        }
        let mut stage = Stage::new();
        let mob = stage.add(Mobject::from_buffer(buffer));
        stage.add_to_scene(mob).expect("live");
        let mut plan = RenderPlan::new();
        sync_valid(&mut plan, &stage, 0);

        let metadata_view = stage
            .get_mut(mob)
            .expect("live")
            .buffer
            .export_field_view("user_metadata", true)
            .expect("custom field");
        let stats = sync_valid(&mut plan, &stage, 0);
        assert_eq!(stats.shapes_reused, 1);
        assert_eq!(stats.styles_rebuilt, 0);
        assert!(!plan.shapes().instances()[0].volatile);
        drop(metadata_view);
    }

    #[test]
    fn n_copies_of_one_outline_are_one_compiled_path_and_n_instances() {
        // §10.8's instancing dedup, stated as the acceptance criterion does.
        let (stage, _) = staged(8);
        let mut plan = RenderPlan::new();
        sync_valid(&mut plan, &stage, 0);
        assert_eq!(plan.shapes().shapes().len(), 1);
        assert_eq!(plan.shapes().instances().len(), 8);
        assert!((plan.shapes().instances_per_shape() - 8.0).abs() < 1e-12);
        // And the segment table holds one outline's worth, not eight.
        assert_eq!(plan.segments().len(), 2);
    }

    #[test]
    fn a_recolour_rebuilds_the_style_and_not_the_geometry() {
        // The §10.8 headline: "a color change does not regenerate curve
        // coefficients".
        let (mut stage, mobs) = staged(2);
        let mut plan = RenderPlan::new();
        sync_valid(&mut plan, &stage, 0);

        stage
            .get_mut(mobs[0])
            .expect("live")
            .buffer
            .write(0, "stroke_rgba", &[1.0, 0.0, 0.0, 1.0]);

        let after = sync_valid(&mut plan, &stage, 0);
        assert_eq!(after.shapes_compiled, 0, "no outline may recompile");
        assert_eq!(after.shapes_reused, 2);
        assert_eq!(after.styles_rebuilt, 1, "exactly the one that changed");
        assert_eq!(plan.styles().len(), 2, "and it interned as a new row");
    }

    #[test]
    fn a_camera_move_rebuilds_nothing() {
        // "a camera move does not re-decode font outlines" — object-space
        // geometry cannot depend on the camera, so the axis must not reach it.
        let (stage, _) = staged(4);
        let mut plan = RenderPlan::new();
        sync_valid(&mut plan, &stage, 0);
        let after = sync_valid(&mut plan, &stage, 99);
        assert_eq!(after.shapes_compiled, 0);
        assert_eq!(after.styles_rebuilt, 0);
        assert_eq!(after.shapes_reused, 4);
    }

    #[test]
    fn affine_placement_reuses_the_object_space_outline() {
        let (mut stage, mobs) = staged(1);
        let mob = mobs[0];
        let point_revision = stage.get(mob).unwrap().buffer.field_revision("point");
        let mut plan = RenderPlan::new();
        sync_valid(&mut plan, &stage, 0);
        let geometry = plan.geometry_identity();

        stage.shift(mob, [7.0, -3.0, 0.5]);
        let translated = sync_valid(&mut plan, &stage, 0);
        assert_eq!(translated.shapes_reused, 1);
        assert_eq!(translated.shapes_compiled, 0);
        assert_eq!(translated.shapes_shared, 0);
        assert_eq!(plan.geometry_identity(), geometry);
        assert_eq!(
            stage.get(mob).unwrap().buffer.field_revision("point"),
            point_revision
        );
        assert_eq!(
            plan.shapes().instances()[0].placement.translation(),
            [7.0, -3.0, 0.5]
        );

        stage.rotate(
            mob,
            std::f64::consts::FRAC_PI_2,
            [0.0, 0.0, 1.0],
            Some([7.0, -3.0, 0.5]),
            None,
        );
        let rotated = sync_valid(&mut plan, &stage, 0);
        assert_eq!(
            rotated.shapes_reused, 1,
            "linear placement is screen-derived state, not a reshape"
        );
        assert_eq!(rotated.shapes_compiled, 0);
        assert_eq!(plan.geometry_identity(), geometry);
    }

    #[test]
    fn moving_one_point_recompiles_exactly_one_outline() {
        let (mut stage, mobs) = staged(3);
        let mut plan = RenderPlan::new();
        sync_valid(&mut plan, &stage, 0);

        stage
            .get_mut(mobs[1])
            .expect("live")
            .buffer
            .write(2, "point", &[500.0, 500.0, 0.0]);

        let after = sync_valid(&mut plan, &stage, 0);
        assert_eq!(after.shapes_reused, 2, "the untouched two are reused");
        assert_eq!(
            after.shapes_compiled + after.shapes_shared,
            1,
            "and exactly one is recompiled"
        );
        assert_eq!(
            plan.shapes().shapes().len(),
            2,
            "a second outline exists now"
        );
    }

    #[test]
    fn a_z_index_change_reuses_every_outline_but_reorders_the_instances() {
        // Order is its own axis: the geometry cannot care, and the instance list
        // must.
        let (mut stage, mobs) = staged(3);
        let mut plan = RenderPlan::new();
        sync_valid(&mut plan, &stage, 0);
        let before: Vec<Mob> = plan.shapes().instances().iter().map(|i| i.mob).collect();

        stage.set_z_index(mobs[0], 10, false);
        stage.add_to_scene(mobs[0]).expect("live");

        let after = sync_valid(&mut plan, &stage, 0);
        assert_eq!(after.shapes_compiled, 0);
        assert_eq!(after.shapes_reused, 3);
        let now: Vec<Mob> = plan.shapes().instances().iter().map(|i| i.mob).collect();
        assert_ne!(before, now, "raising z_index must move it in painter order");
        assert_eq!(now.last(), Some(&mobs[0]), "and put it on top");
    }

    #[test]
    fn leaving_the_scene_drops_the_retained_entry() {
        let (mut stage, mobs) = staged(3);
        let mut plan = RenderPlan::new();
        sync_valid(&mut plan, &stage, 0);

        stage.remove_from_scene(mobs[2]);
        let after = sync_valid(&mut plan, &stage, 0);
        assert_eq!(after.visited, 2);
        assert_eq!(after.dropped, 1);
        assert_eq!(plan.shapes().instances().len(), 2);
    }

    #[test]
    fn instances_carry_painter_order_and_the_placement_that_positions_them() {
        let (stage, mobs) = staged(3);
        let mut plan = RenderPlan::new();
        sync_valid(&mut plan, &stage, 0);
        let instances = plan.shapes().instances();
        assert_eq!(instances.len(), 3);
        for (i, inst) in instances.iter().enumerate() {
            assert_eq!(inst.order, u32::try_from(i).expect("small fixture"));
            assert_eq!(inst.mob, mobs[i]);
            // The outline is shared, so the placement is what distinguishes the
            // three — which is the whole reason position is excluded from the
            // shape digest.
            assert!((inst.placement.translation()[0] - i as f64 * 10.0).abs() < 1e-9);
        }
        assert_eq!(instances[0].shape, instances[2].shape);
    }

    #[test]
    fn interned_indices_are_stable_across_syncs() {
        // The property the tile cache's key rests on (`crate::cache`): shape and
        // style indices come from append-only interning, so a retained plan
        // keeps them stable and adding an object cannot renumber the ones
        // already there. A plan rebuilt each frame reshuffles them, and every
        // tile key in the frame moves for a reason that has nothing to do with
        // the tile — which is exactly what the cache's first test harness did.
        let (mut stage, mobs) = staged(2);
        let mut plan = RenderPlan::new();
        sync_valid(&mut plan, &stage, 0);
        let shapes: Vec<u32> = plan.shapes().instances().iter().map(|i| i.shape).collect();
        let styles: Vec<u32> = plan.shapes().instances().iter().map(|i| i.style).collect();

        // A newcomer *behind* the others, so it lands first in the draw sequence
        // and shifts every instance index.
        let newcomer = stage.add(vmobject(&[
            [0.0, 0.0, 0.0],
            [3.0, 1.0, 0.0],
            [6.0, 0.0, 0.0],
        ]));
        stage.set_z_index(newcomer, -5, false);
        stage.add_to_scene(newcomer).expect("live");
        sync_valid(&mut plan, &stage, 0);

        let instances = plan.shapes().instances();
        assert_eq!(instances.len(), 3);
        assert_eq!(instances[0].mob, newcomer, "the newcomer draws first");
        for (k, mob) in mobs.iter().enumerate() {
            let inst = instances
                .iter()
                .find(|i| i.mob == *mob)
                .expect("still in the scene");
            assert_eq!(inst.shape, shapes[k], "shape index moved for {mob:?}");
            assert_eq!(inst.style, styles[k], "style index moved for {mob:?}");
        }
    }

    #[test]
    fn a_point_less_group_contributes_no_instance() {
        // A `Group` with children but no geometry of its own is normal, and it
        // must not appear as a degenerate compiled path.
        let mut stage = Stage::new();
        let group = stage.add(Mobject::new());
        stage.add_to_scene(group).expect("live");
        let mut plan = RenderPlan::new();
        let stats = sync_valid(&mut plan, &stage, 0);
        assert_eq!(stats.shapes_compiled, 0);
        assert_eq!(plan.shapes().instances().len(), 0);
    }
}
