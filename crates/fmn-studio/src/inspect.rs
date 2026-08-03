//! Bounded mobject inspection, source-span adapters, and render-debug overlays.
//!
//! Inspector ids are ephemeral and deterministic for one capture: roots in
//! scene order, then depth-first children in insertion order. They never claim
//! to be durable [`Mob`] identities. Every traversal and copied record column
//! has an explicit ceiling, so a hostile or simply enormous scene cannot turn
//! Studio inspection into an unbounded allocation.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::Arc;

use fmn_render::bin::{CLASS_INTERIOR, CLASS_PARTIAL};
use fmn_render::{Binning, Viewport};
use fmn_scene::studio_bridge::{Mob, Stage, Uniforms};

use crate::protocol::DebugLayerSet;

/// Inspector and overlay resource ceilings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InspectorLimits {
    /// Maximum visible mobjects.
    pub max_nodes: usize,
    /// Maximum traversal depth.
    pub max_depth: usize,
    /// Maximum child edges examined while finding visible mobjects.
    pub max_traversal_edges: usize,
    /// Maximum visible parent/child links copied for one mobject.
    pub max_links_per_node: usize,
    /// Maximum visible parent/child links copied across one inspection.
    pub max_total_links: usize,
    /// Maximum record fields copied from one mobject.
    pub max_fields_per_node: usize,
    /// Maximum f32 values copied from one record field.
    pub max_values_per_field: usize,
    /// Maximum f32 values copied across the complete inspection.
    pub max_total_values: usize,
    /// Maximum control points copied for one overlay node.
    pub max_points_per_node: usize,
    /// Maximum control points copied across the complete overlay.
    pub max_total_points: usize,
    /// Maximum fine-tile diagnostics copied for one overlay.
    pub max_tiles: usize,
    /// Maximum source excerpt bytes attached to one node.
    pub max_source_excerpt_bytes: usize,
    /// Maximum source excerpt bytes copied across one inspection.
    pub max_total_source_excerpt_bytes: usize,
    /// Maximum encoded JSON document.
    pub max_json_bytes: usize,
}

impl Default for InspectorLimits {
    fn default() -> Self {
        Self {
            max_nodes: 50_000,
            max_depth: 512,
            max_traversal_edges: 250_000,
            max_links_per_node: 4096,
            max_total_links: 250_000,
            max_fields_per_node: 128,
            max_values_per_field: 4096,
            max_total_values: 1_000_000,
            max_points_per_node: 4096,
            max_total_points: 250_000,
            max_tiles: 1_000_000,
            max_source_excerpt_bytes: 4096,
            max_total_source_excerpt_bytes: 8 * 1024 * 1024,
            max_json_bytes: 8 * 1024 * 1024,
        }
    }
}

impl InspectorLimits {
    fn validate(self) -> Result<Self, InspectError> {
        if self.max_nodes == 0
            || self.max_depth == 0
            || self.max_traversal_edges == 0
            || self.max_links_per_node == 0
            || self.max_total_links == 0
            || self.max_fields_per_node == 0
            || self.max_values_per_field == 0
            || self.max_total_values == 0
            || self.max_points_per_node == 0
            || self.max_total_points == 0
            || self.max_tiles == 0
            || self.max_source_excerpt_bytes == 0
            || self.max_total_source_excerpt_bytes == 0
            || self.max_json_bytes == 0
        {
            Err(InspectError::InvalidLimits)
        } else {
            Ok(self)
        }
    }
}

/// Kind of source construct represented by one mobject.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpanKind {
    /// Plain-text glyph.
    TextGlyph,
    /// Math glyph.
    MathGlyph,
    /// Math rule.
    MathRule,
    /// Math outline/path.
    MathPath,
}

impl SpanKind {
    const fn name(self) -> &'static str {
        match self {
            Self::TextGlyph => "text_glyph",
            Self::MathGlyph => "math_glyph",
            Self::MathRule => "math_rule",
            Self::MathPath => "math_path",
        }
    }
}

/// One native Scribe span mapped to its emitted submobject ordinal.
///
/// Both `TextLayout` glyphs and `Typeset` primitives already carry these
/// four values. The composition root translates those native entries into
/// this lower-DAG seam, preserving source identity without making Studio
/// depend directly on either Scribe crate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeSpanBinding {
    /// Ordinal in the caller's live submobject slice.
    pub submobject_index: usize,
    /// Inclusive source byte offset.
    pub start: usize,
    /// Exclusive source byte offset.
    pub end: usize,
    /// Kind of source construct.
    pub kind: SpanKind,
}

#[derive(Clone, Debug)]
struct RegisteredSpan {
    source: Arc<str>,
    start: usize,
    end: usize,
    kind: SpanKind,
}

/// Live source-span bindings supplied by Scribe adapters.
#[derive(Clone, Debug, Default)]
pub struct SpanRegistry {
    spans: HashMap<Mob, RegisteredSpan>,
}

impl SpanRegistry {
    /// Empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register one explicit source range.
    pub fn register(
        &mut self,
        mob: Mob,
        source: Arc<str>,
        start: usize,
        end: usize,
        kind: SpanKind,
    ) -> Result<(), InspectError> {
        validate_span(&source, start, end)?;
        self.spans.insert(
            mob,
            RegisteredSpan {
                source,
                start,
                end,
                kind,
            },
        );
        Ok(())
    }

    /// Bind native Scribe span-map entries to their live submobjects.
    ///
    /// Validation is atomic: an invalid ordinal or byte range leaves the
    /// registry unchanged.
    pub fn bind_native(
        &mut self,
        source: Arc<str>,
        submobjects: &[Mob],
        bindings: &[NativeSpanBinding],
    ) -> Result<(), InspectError> {
        for binding in bindings {
            submobjects
                .get(binding.submobject_index)
                .ok_or(InspectError::SpanMapMismatch(
                    "native span submobject index is out of range",
                ))?;
            validate_span(&source, binding.start, binding.end)?;
        }
        for binding in bindings {
            let mob = submobjects[binding.submobject_index];
            self.spans.insert(
                mob,
                RegisteredSpan {
                    source: Arc::clone(&source),
                    start: binding.start,
                    end: binding.end,
                    kind: binding.kind,
                },
            );
        }
        Ok(())
    }

    /// Remove a binding.
    pub fn remove(&mut self, mob: Mob) {
        self.spans.remove(&mob);
    }
}

/// One copied record field.
#[derive(Clone, Debug, PartialEq)]
pub struct RecordFieldSnapshot {
    /// Field name.
    pub name: String,
    /// Lanes per record.
    pub width: usize,
    /// Field revision.
    pub revision: u64,
    /// Record-major copied values.
    pub values: Vec<f32>,
    /// Total values before truncation.
    pub total_values: usize,
}

/// Source excerpt attached to an inspector node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceSpanSnapshot {
    /// Construct kind.
    pub kind: SpanKind,
    /// Half-open source byte range.
    pub start: usize,
    /// Half-open source byte range.
    pub end: usize,
    /// Complete source length.
    pub source_bytes: usize,
    /// Bounded exact excerpt beginning at `start`.
    pub excerpt: String,
    /// Whether the construct continued beyond the excerpt.
    pub excerpt_truncated: bool,
}

/// Typed copy of one mobject's uniform inventory.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UniformSnapshot {
    /// Fixed-in-frame mix.
    pub is_fixed_in_frame: f64,
    /// Reflectiveness, gloss, shadow.
    pub shading: [f64; 3],
    /// Clip planes.
    pub clip_planes: [[f64; 4]; 4],
    /// AA width.
    pub anti_alias_width: f64,
    /// Joint code.
    pub joint_type: f64,
    /// Flat-stroke flag.
    pub flat_stroke: bool,
    /// Zoom-scaled-stroke flag.
    pub scale_stroke_with_zoom: bool,
    /// Stroke-behind-fill flag.
    pub stroke_behind: bool,
    /// Depth-test flag.
    pub depth_test: bool,
    /// Compatibility no-op flag.
    pub use_winding_fill: bool,
}

impl From<&Uniforms> for UniformSnapshot {
    fn from(uniforms: &Uniforms) -> Self {
        Self {
            is_fixed_in_frame: uniforms.is_fixed_in_frame,
            shading: uniforms.shading,
            clip_planes: uniforms.clip_planes,
            anti_alias_width: uniforms.anti_alias_width,
            joint_type: uniforms.joint_type.to_code(),
            flat_stroke: uniforms.flat_stroke,
            scale_stroke_with_zoom: uniforms.scale_stroke_with_zoom,
            stroke_behind: uniforms.stroke_behind,
            depth_test: uniforms.depth_test,
            use_winding_fill: uniforms.use_winding_fill,
        }
    }
}

/// One deterministic ephemeral inspector node.
#[derive(Clone, Debug, PartialEq)]
pub struct InspectorNode {
    /// Capture-local id.
    pub id: usize,
    /// Whether this mobject is a scene root.
    pub root: bool,
    /// Capture-local parent ids.
    pub parents: Vec<usize>,
    /// Capture-local child ids.
    pub children: Vec<usize>,
    /// Outstanding binding proxy pins.
    pub pins: usize,
    /// Draw-order key.
    pub z_index: i32,
    /// Whether the object is currently animating.
    pub animating: bool,
    /// Whether object state may change between captures.
    pub changing: bool,
    /// Number of records.
    pub record_count: usize,
    /// Whole-buffer revision.
    pub record_revision: u64,
    /// Bounded record fields.
    pub fields: Vec<RecordFieldSnapshot>,
    /// Uniform inventory.
    pub uniforms: UniformSnapshot,
    /// Row-major placement matrix.
    pub placement_linear: [[f64; 3]; 3],
    /// Placement translation.
    pub placement_translation: [f64; 3],
    /// World-space family bounds, `[min, mid, max]`.
    pub bounds: [[f64; 3]; 3],
    /// Optional Scribe source identity.
    pub source_span: Option<SourceSpanSnapshot>,
}

/// One bounded family-tree snapshot.
#[derive(Clone, Debug, PartialEq)]
pub struct InspectorSnapshot {
    /// Format version.
    pub version: u16,
    /// Scene time mirror.
    pub scene_time: f64,
    /// Captured nodes.
    pub nodes: Vec<InspectorNode>,
    /// Traversal or field data hit a declared ceiling.
    pub truncated: bool,
}

impl InspectorSnapshot {
    /// Capture the visible Stage family graph.
    pub fn capture(
        stage: &Stage,
        spans: &SpanRegistry,
        limits: InspectorLimits,
    ) -> Result<Self, InspectError> {
        let limits = limits.validate()?;
        let (handles, ids, traversal_truncated) = visible_handles(stage, limits);
        let roots = stage.roots();
        let root_set: HashSet<Mob> = roots
            .iter()
            .copied()
            .filter(|mob| ids.contains_key(mob))
            .collect();
        let mut total_links = 0usize;
        let mut total_values = 0usize;
        let mut total_source_excerpt_bytes = 0usize;
        let mut truncated = traversal_truncated;
        let mut nodes = Vec::with_capacity(handles.len());
        for (id, mob) in handles.iter().copied().enumerate() {
            let Some(entry) = stage.get(mob) else {
                continue;
            };
            let mut node_links = 0usize;
            let mut parents = Vec::new();
            for parent in entry.parents() {
                let Some(parent) = ids.get(parent).copied() else {
                    continue;
                };
                if node_links >= limits.max_links_per_node || total_links >= limits.max_total_links
                {
                    truncated = true;
                    break;
                }
                parents.push(parent);
                node_links += 1;
                total_links += 1;
            }
            let mut children = Vec::new();
            for child in entry.submobjects() {
                let Some(child) = ids.get(child).copied() else {
                    continue;
                };
                if node_links >= limits.max_links_per_node || total_links >= limits.max_total_links
                {
                    truncated = true;
                    break;
                }
                children.push(child);
                node_links += 1;
                total_links += 1;
            }
            let mut fields = Vec::new();
            for field in entry
                .buffer
                .schema()
                .fields()
                .iter()
                .take(limits.max_fields_per_node)
            {
                let total = entry.buffer.len().saturating_mul(field.width);
                let available = limits.max_total_values.saturating_sub(total_values);
                let keep = total.min(limits.max_values_per_field).min(available);
                if keep < total {
                    truncated = true;
                }
                let mut values = Vec::with_capacity(keep);
                for record in 0..entry.buffer.len() {
                    if values.len() >= keep {
                        break;
                    }
                    let Some(record_values) = entry.buffer.read(record, &field.name) else {
                        break;
                    };
                    let remaining = keep - values.len();
                    values.extend_from_slice(&record_values[..record_values.len().min(remaining)]);
                }
                total_values += keep;
                fields.push(RecordFieldSnapshot {
                    name: field.name.clone(),
                    width: field.width,
                    revision: entry.buffer.field_revision(&field.name).unwrap_or(0),
                    total_values: total,
                    values,
                });
            }
            if entry.buffer.schema().fields().len() > limits.max_fields_per_node {
                truncated = true;
            }
            let placement = entry.placement();
            let bounds = stage.get_bounding_box(mob);
            let source_excerpt_bytes = limits
                .max_total_source_excerpt_bytes
                .saturating_sub(total_source_excerpt_bytes)
                .min(limits.max_source_excerpt_bytes);
            let source_span = span_snapshot(spans.spans.get(&mob), source_excerpt_bytes);
            if let Some(source_span) = &source_span {
                total_source_excerpt_bytes += source_span.excerpt.len();
                if source_span.excerpt_truncated {
                    truncated = true;
                }
            }
            nodes.push(InspectorNode {
                id,
                root: root_set.contains(&mob),
                parents,
                children,
                pins: entry.pins(),
                z_index: stage.z_index(mob),
                animating: stage.is_animating(mob),
                changing: stage.is_changing(mob),
                record_count: entry.buffer.len(),
                record_revision: entry.buffer.revision(),
                fields,
                uniforms: UniformSnapshot::from(entry.uniforms()),
                placement_linear: placement.linear(),
                placement_translation: placement.translation(),
                bounds: [bounds.min, bounds.mid, bounds.max],
                source_span,
            });
        }
        Ok(Self {
            version: 1,
            scene_time: stage.time(),
            nodes,
            truncated,
        })
    }

    /// Encode stable line-free JSON under the configured byte ceiling.
    pub fn to_json(&self, limits: InspectorLimits) -> Result<Vec<u8>, InspectError> {
        let limits = limits.validate()?;
        let mut out = String::new();
        out.push_str("{\"version\":");
        push_usize(&mut out, usize::from(self.version));
        out.push_str(",\"scene_time\":");
        push_f64(&mut out, self.scene_time);
        out.push_str(",\"truncated\":");
        push_bool(&mut out, self.truncated);
        out.push_str(",\"nodes\":[");
        for (index, node) in self.nodes.iter().enumerate() {
            if index != 0 {
                out.push(',');
            }
            push_inspector_node(&mut out, node, limits.max_json_bytes)?;
        }
        out.push_str("]}");
        if out.len() > limits.max_json_bytes {
            return Err(InspectError::JsonLimit {
                limit: limits.max_json_bytes,
                needed: out.len(),
            });
        }
        Ok(out.into_bytes())
    }
}

/// Winding diagnosis for one point run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindingDirection {
    /// Positive signed xy area.
    CounterClockwise,
    /// Negative signed xy area.
    Clockwise,
    /// Fewer than three points or zero signed area.
    Degenerate,
}

impl WindingDirection {
    const fn name(self) -> &'static str {
        match self {
            Self::CounterClockwise => "counter_clockwise",
            Self::Clockwise => "clockwise",
            Self::Degenerate => "degenerate",
        }
    }
}

/// Debug geometry for one visible mobject.
#[derive(Clone, Debug, PartialEq)]
pub struct NodeOverlay {
    /// Inspector-compatible ephemeral id.
    pub id: usize,
    /// World-space control points.
    pub control_points: Vec<[f64; 3]>,
    /// Total points before truncation.
    pub total_points: usize,
    /// Optional `[min, mid, max]` world-space box.
    pub bounds: Option<[[f64; 3]; 3]>,
    /// Optional path winding.
    pub winding: Option<WindingDirection>,
    /// Optional world-space center z.
    pub center_z: Option<f64>,
    /// Optional z-index.
    pub z_index: Option<i32>,
    /// Optional depth-test flag.
    pub depth_test: Option<bool>,
}

/// One fine-tile diagnostic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TileOverlay {
    /// Fine-tile index.
    pub index: usize,
    /// Pixel rectangle `[x0, y0, x1, y1)`.
    pub rect: [u32; 4],
    /// Commands touching the tile.
    pub draws: usize,
    /// Partial-coverage commands.
    pub partial: usize,
    /// Interior commands.
    pub interior: usize,
}

/// Bounded render-debug snapshot.
#[derive(Clone, Debug, PartialEq)]
pub struct DebugOverlaySnapshot {
    /// Format version.
    pub version: u16,
    /// Layer mask actually captured.
    pub layers: DebugLayerSet,
    /// Fine-tile diagnostics.
    pub tiles: Vec<TileOverlay>,
    /// Mobject diagnostics.
    pub nodes: Vec<NodeOverlay>,
    /// Traversal or point data hit a declared ceiling.
    pub truncated: bool,
}

impl DebugOverlaySnapshot {
    /// Derive overlay primitives without altering rendered output.
    pub fn capture(
        stage: &Stage,
        binning: Option<(&Binning, Viewport)>,
        layers: DebugLayerSet,
        limits: InspectorLimits,
    ) -> Result<Self, InspectError> {
        let limits = limits.validate()?;
        DebugLayerSet::from_bits(layers.bits()).map_err(|_| InspectError::InvalidLayers)?;
        let mut tiles = Vec::new();
        let mut tiles_truncated = false;
        if layers.contains(DebugLayerSet::TILES) {
            let (binning, viewport) = binning.ok_or(InspectError::MissingBinningForTileOverlay)?;
            let tiling = binning.tiling();
            let tile = tiling.fine_tile.max(1);
            let cols = viewport.width.div_ceil(tile);
            let rows = viewport.height.div_ceil(tile);
            let expected = usize::try_from(cols)
                .ok()
                .and_then(|cols| {
                    usize::try_from(rows)
                        .ok()
                        .and_then(|rows| cols.checked_mul(rows))
                })
                .ok_or(InspectError::OverlayGeometryOverflow)?;
            if expected != binning.tile_count() {
                return Err(InspectError::BinningViewportMismatch);
            }
            let keep = expected.min(limits.max_tiles);
            tiles_truncated = keep < expected;
            tiles.reserve(keep);
            for index in 0..keep {
                let x = u32::try_from(index % cols as usize)
                    .map_err(|_| InspectError::OverlayGeometryOverflow)?;
                let y = u32::try_from(index / cols as usize)
                    .map_err(|_| InspectError::OverlayGeometryOverflow)?;
                let x0 = x
                    .checked_mul(tile)
                    .ok_or(InspectError::OverlayGeometryOverflow)?;
                let y0 = y
                    .checked_mul(tile)
                    .ok_or(InspectError::OverlayGeometryOverflow)?;
                let flags = binning
                    .tile_flags(index)
                    .ok_or(InspectError::BinningViewportMismatch)?;
                let draws = binning
                    .tile(index)
                    .ok_or(InspectError::BinningViewportMismatch)?;
                tiles.push(TileOverlay {
                    index,
                    rect: [
                        x0,
                        y0,
                        x0.saturating_add(tile).min(viewport.width),
                        y0.saturating_add(tile).min(viewport.height),
                    ],
                    draws: draws.len(),
                    partial: flags.iter().filter(|flag| **flag == CLASS_PARTIAL).count(),
                    interior: flags.iter().filter(|flag| **flag == CLASS_INTERIOR).count(),
                });
            }
        }

        let (handles, _, traversal_truncated) = visible_handles(stage, limits);
        let mut total_points = 0usize;
        let mut truncated = traversal_truncated || tiles_truncated;
        let mut nodes = Vec::with_capacity(handles.len());
        for (id, mob) in handles.iter().copied().enumerate() {
            let Some(entry) = stage.get(mob) else {
                continue;
            };
            let placement = entry.placement();
            let total = if entry.buffer.schema().field_width("point") == Some(3) {
                entry.buffer.len()
            } else {
                0
            };
            let available = limits.max_total_points.saturating_sub(total_points);
            let keep = total.min(limits.max_points_per_node).min(available);
            if keep < total {
                truncated = true;
            }
            let mut control_points = Vec::new();
            if layers.contains(DebugLayerSet::CONTROL_POINTS)
                || layers.contains(DebugLayerSet::WINDING)
            {
                control_points.reserve(keep);
                for record in 0..keep {
                    if let Some(point) = entry.buffer.read(record, "point") {
                        control_points.push(placement.apply_point([
                            f64::from(point[0]),
                            f64::from(point[1]),
                            f64::from(point[2]),
                        ]));
                    }
                }
                total_points += keep;
            }
            let bounds = if layers.contains(DebugLayerSet::BOUNDING_BOXES)
                || layers.contains(DebugLayerSet::DEPTH)
            {
                let bounds = stage.get_bounding_box(mob);
                Some([bounds.min, bounds.mid, bounds.max])
            } else {
                None
            };
            let winding = (layers.contains(DebugLayerSet::WINDING) && keep == total)
                .then(|| winding(&control_points));
            let center_z = layers
                .contains(DebugLayerSet::DEPTH)
                .then(|| bounds.map_or(0.0, |bounds| bounds[1][2]));
            nodes.push(NodeOverlay {
                id,
                control_points: if layers.contains(DebugLayerSet::CONTROL_POINTS) {
                    control_points
                } else {
                    Vec::new()
                },
                total_points: total,
                bounds: layers
                    .contains(DebugLayerSet::BOUNDING_BOXES)
                    .then_some(bounds)
                    .flatten(),
                winding,
                center_z,
                z_index: layers
                    .contains(DebugLayerSet::DEPTH)
                    .then(|| stage.z_index(mob)),
                depth_test: layers
                    .contains(DebugLayerSet::DEPTH)
                    .then_some(entry.uniforms().depth_test),
            });
        }
        Ok(Self {
            version: 1,
            layers,
            tiles,
            nodes,
            truncated,
        })
    }

    /// Encode stable JSON under the configured byte ceiling.
    pub fn to_json(&self, limits: InspectorLimits) -> Result<Vec<u8>, InspectError> {
        let limits = limits.validate()?;
        let mut out = String::new();
        out.push_str("{\"version\":");
        push_usize(&mut out, usize::from(self.version));
        out.push_str(",\"layers\":");
        push_usize(&mut out, usize::from(self.layers.bits()));
        out.push_str(",\"truncated\":");
        push_bool(&mut out, self.truncated);
        out.push_str(",\"tiles\":[");
        for (index, tile) in self.tiles.iter().enumerate() {
            if index != 0 {
                out.push(',');
            }
            out.push_str("{\"id\":");
            push_usize(&mut out, tile.index);
            out.push_str(",\"rect\":");
            push_u32_array(&mut out, &tile.rect);
            out.push_str(",\"draws\":");
            push_usize(&mut out, tile.draws);
            out.push_str(",\"partial\":");
            push_usize(&mut out, tile.partial);
            out.push_str(",\"interior\":");
            push_usize(&mut out, tile.interior);
            out.push('}');
            if out.len() > limits.max_json_bytes {
                return Err(InspectError::JsonLimit {
                    limit: limits.max_json_bytes,
                    needed: out.len(),
                });
            }
        }
        out.push_str("],\"nodes\":[");
        for (index, node) in self.nodes.iter().enumerate() {
            if index != 0 {
                out.push(',');
            }
            push_overlay_node(&mut out, node, limits.max_json_bytes)?;
        }
        out.push_str("]}");
        if out.len() > limits.max_json_bytes {
            return Err(InspectError::JsonLimit {
                limit: limits.max_json_bytes,
                needed: out.len(),
            });
        }
        Ok(out.into_bytes())
    }
}

fn visible_handles(
    stage: &Stage,
    limits: InspectorLimits,
) -> (Vec<Mob>, HashMap<Mob, usize>, bool) {
    let mut handles = Vec::new();
    let mut ids = HashMap::new();
    let mut stack: Vec<(Mob, usize, usize)> = Vec::new();
    let mut traversal_edges = 0usize;
    let mut truncated = false;
    for root in stage.roots().iter().copied() {
        if ids.contains_key(&root) || stage.get(root).is_none() {
            continue;
        }
        if handles.len() >= limits.max_nodes {
            truncated = true;
            break;
        }
        let id = handles.len();
        handles.push(root);
        ids.insert(root, id);
        stack.push((root, 0, 0));
        while let Some((mob, depth, next_child)) = stack.last_mut() {
            let Some(entry) = stage.get(*mob) else {
                stack.pop();
                continue;
            };
            if *depth >= limits.max_depth {
                if *next_child < entry.submobjects().len() {
                    truncated = true;
                }
                stack.pop();
                continue;
            }
            if *next_child >= entry.submobjects().len() {
                stack.pop();
                continue;
            }
            if traversal_edges >= limits.max_traversal_edges {
                truncated = true;
                stack.clear();
                break;
            }
            let child = entry.submobjects()[*next_child];
            *next_child += 1;
            traversal_edges += 1;
            if ids.contains_key(&child) || stage.get(child).is_none() {
                continue;
            }
            if handles.len() >= limits.max_nodes {
                truncated = true;
                stack.clear();
                break;
            }
            let child_id = handles.len();
            let child_depth = *depth + 1;
            handles.push(child);
            ids.insert(child, child_id);
            stack.push((child, child_depth, 0));
        }
    }
    (handles, ids, truncated)
}

fn span_snapshot(
    span: Option<&RegisteredSpan>,
    max_excerpt_bytes: usize,
) -> Option<SourceSpanSnapshot> {
    let span = span?;
    let intended_end = span.start.saturating_add(max_excerpt_bytes).min(span.end);
    let mut excerpt_end = intended_end;
    while excerpt_end > span.start && !span.source.is_char_boundary(excerpt_end) {
        excerpt_end -= 1;
    }
    Some(SourceSpanSnapshot {
        kind: span.kind,
        start: span.start,
        end: span.end,
        source_bytes: span.source.len(),
        excerpt: span.source[span.start..excerpt_end].to_owned(),
        excerpt_truncated: excerpt_end < span.end,
    })
}

fn validate_span(source: &str, start: usize, end: usize) -> Result<(), InspectError> {
    if start > end
        || end > source.len()
        || !source.is_char_boundary(start)
        || !source.is_char_boundary(end)
    {
        Err(InspectError::InvalidSourceSpan)
    } else {
        Ok(())
    }
}

fn winding(points: &[[f64; 3]]) -> WindingDirection {
    if points.len() < 3 {
        return WindingDirection::Degenerate;
    }
    let mut twice_area = 0.0;
    for (point, next) in points
        .iter()
        .zip(points.iter().cycle().skip(1))
        .take(points.len())
    {
        twice_area += point[0] * next[1] - next[0] * point[1];
    }
    if twice_area > 0.0 {
        WindingDirection::CounterClockwise
    } else if twice_area < 0.0 {
        WindingDirection::Clockwise
    } else {
        WindingDirection::Degenerate
    }
}

fn push_inspector_node(
    out: &mut String,
    node: &InspectorNode,
    max_bytes: usize,
) -> Result<(), InspectError> {
    out.push_str("{\"id\":");
    push_usize(out, node.id);
    out.push_str(",\"root\":");
    push_bool(out, node.root);
    out.push_str(",\"parents\":");
    push_usize_array(out, &node.parents, max_bytes)?;
    out.push_str(",\"children\":");
    push_usize_array(out, &node.children, max_bytes)?;
    out.push_str(",\"pins\":");
    push_usize(out, node.pins);
    out.push_str(",\"z_index\":");
    out.push_str(&node.z_index.to_string());
    out.push_str(",\"animating\":");
    push_bool(out, node.animating);
    out.push_str(",\"changing\":");
    push_bool(out, node.changing);
    out.push_str(",\"records\":{\"count\":");
    push_usize(out, node.record_count);
    out.push_str(",\"revision\":");
    out.push_str(&node.record_revision.to_string());
    out.push_str(",\"fields\":[");
    for (index, field) in node.fields.iter().enumerate() {
        if index != 0 {
            out.push(',');
        }
        out.push_str("{\"name\":\"");
        push_json_escaped(out, &field.name, max_bytes)?;
        out.push_str("\",\"width\":");
        push_usize(out, field.width);
        out.push_str(",\"revision\":");
        out.push_str(&field.revision.to_string());
        out.push_str(",\"total_values\":");
        push_usize(out, field.total_values);
        out.push_str(",\"values\":[");
        for (value_index, value) in field.values.iter().copied().enumerate() {
            if value_index != 0 {
                out.push(',');
            }
            push_f32(out, value);
            require_json_budget(out, max_bytes)?;
        }
        out.push_str("]}");
        require_json_budget(out, max_bytes)?;
    }
    out.push_str("]},\"uniforms\":");
    push_uniforms(out, node.uniforms);
    out.push_str(",\"placement\":{\"linear\":");
    push_matrix3(out, &node.placement_linear);
    out.push_str(",\"translation\":");
    push_vec3(out, &node.placement_translation);
    out.push_str("},\"bounds\":");
    push_matrix3(out, &node.bounds);
    out.push_str(",\"source_span\":");
    match &node.source_span {
        Some(span) => {
            out.push_str("{\"kind\":\"");
            out.push_str(span.kind.name());
            out.push_str("\",\"start\":");
            push_usize(out, span.start);
            out.push_str(",\"end\":");
            push_usize(out, span.end);
            out.push_str(",\"source_bytes\":");
            push_usize(out, span.source_bytes);
            out.push_str(",\"excerpt\":\"");
            push_json_escaped(out, &span.excerpt, max_bytes)?;
            out.push_str("\",\"excerpt_truncated\":");
            push_bool(out, span.excerpt_truncated);
            out.push('}');
        }
        None => out.push_str("null"),
    }
    out.push('}');
    require_json_budget(out, max_bytes)
}

fn push_overlay_node(
    out: &mut String,
    node: &NodeOverlay,
    max_bytes: usize,
) -> Result<(), InspectError> {
    out.push_str("{\"id\":");
    push_usize(out, node.id);
    out.push_str(",\"total_points\":");
    push_usize(out, node.total_points);
    out.push_str(",\"control_points\":[");
    for (index, point) in node.control_points.iter().enumerate() {
        if index != 0 {
            out.push(',');
        }
        push_vec3(out, point);
        require_json_budget(out, max_bytes)?;
    }
    out.push_str("],\"bounds\":");
    match &node.bounds {
        Some(bounds) => push_matrix3(out, bounds),
        None => out.push_str("null"),
    }
    out.push_str(",\"winding\":");
    match node.winding {
        Some(winding) => {
            out.push('"');
            out.push_str(winding.name());
            out.push('"');
        }
        None => out.push_str("null"),
    }
    out.push_str(",\"center_z\":");
    push_optional_f64(out, node.center_z);
    out.push_str(",\"z_index\":");
    match node.z_index {
        Some(value) => out.push_str(&value.to_string()),
        None => out.push_str("null"),
    }
    out.push_str(",\"depth_test\":");
    match node.depth_test {
        Some(value) => push_bool(out, value),
        None => out.push_str("null"),
    }
    out.push('}');
    require_json_budget(out, max_bytes)
}

fn push_uniforms(out: &mut String, uniforms: UniformSnapshot) {
    out.push_str("{\"is_fixed_in_frame\":");
    push_f64(out, uniforms.is_fixed_in_frame);
    out.push_str(",\"shading\":");
    push_vec3(out, &uniforms.shading);
    out.push_str(",\"clip_planes\":[");
    for (index, plane) in uniforms.clip_planes.iter().enumerate() {
        if index != 0 {
            out.push(',');
        }
        push_f64_array(out, plane);
    }
    out.push_str("],\"anti_alias_width\":");
    push_f64(out, uniforms.anti_alias_width);
    out.push_str(",\"joint_type\":");
    push_f64(out, uniforms.joint_type);
    out.push_str(",\"flat_stroke\":");
    push_bool(out, uniforms.flat_stroke);
    out.push_str(",\"scale_stroke_with_zoom\":");
    push_bool(out, uniforms.scale_stroke_with_zoom);
    out.push_str(",\"stroke_behind\":");
    push_bool(out, uniforms.stroke_behind);
    out.push_str(",\"depth_test\":");
    push_bool(out, uniforms.depth_test);
    out.push_str(",\"use_winding_fill\":");
    push_bool(out, uniforms.use_winding_fill);
    out.push('}');
}

fn push_matrix3(out: &mut String, matrix: &[[f64; 3]; 3]) {
    out.push('[');
    for (index, row) in matrix.iter().enumerate() {
        if index != 0 {
            out.push(',');
        }
        push_vec3(out, row);
    }
    out.push(']');
}

fn push_vec3(out: &mut String, vector: &[f64; 3]) {
    push_f64_array(out, vector);
}

fn push_f64_array<const N: usize>(out: &mut String, values: &[f64; N]) {
    out.push('[');
    for (index, value) in values.iter().copied().enumerate() {
        if index != 0 {
            out.push(',');
        }
        push_f64(out, value);
    }
    out.push(']');
}

fn push_u32_array<const N: usize>(out: &mut String, values: &[u32; N]) {
    out.push('[');
    for (index, value) in values.iter().enumerate() {
        if index != 0 {
            out.push(',');
        }
        out.push_str(&value.to_string());
    }
    out.push(']');
}

fn push_usize_array(
    out: &mut String,
    values: &[usize],
    max_bytes: usize,
) -> Result<(), InspectError> {
    out.push('[');
    for (index, value) in values.iter().enumerate() {
        if index != 0 {
            out.push(',');
        }
        push_usize(out, *value);
        require_json_budget(out, max_bytes)?;
    }
    out.push(']');
    require_json_budget(out, max_bytes)
}

fn push_optional_f64(out: &mut String, value: Option<f64>) {
    match value {
        Some(value) => push_f64(out, value),
        None => out.push_str("null"),
    }
}

fn push_f64(out: &mut String, value: f64) {
    if value.is_finite() {
        out.push_str(&value.to_string());
    } else if value.is_nan() {
        out.push_str("\"NaN\"");
    } else if value.is_sign_positive() {
        out.push_str("\"Infinity\"");
    } else {
        out.push_str("\"-Infinity\"");
    }
}

fn push_f32(out: &mut String, value: f32) {
    push_f64(out, f64::from(value));
}

fn push_usize(out: &mut String, value: usize) {
    out.push_str(&value.to_string());
}

fn push_bool(out: &mut String, value: bool) {
    out.push_str(if value { "true" } else { "false" });
}

fn push_json_escaped(out: &mut String, raw: &str, max_bytes: usize) -> Result<(), InspectError> {
    for character in raw.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            character if character.is_control() => {
                out.push_str(&format!("\\u{:04x}", character as u32));
            }
            character => out.push(character),
        }
        require_json_budget(out, max_bytes)?;
    }
    Ok(())
}

fn require_json_budget(out: &str, max_bytes: usize) -> Result<(), InspectError> {
    if out.len() > max_bytes {
        Err(InspectError::JsonLimit {
            limit: max_bytes,
            needed: out.len(),
        })
    } else {
        Ok(())
    }
}

/// Inspector/overlay refusal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InspectError {
    /// At least one ceiling was zero.
    InvalidLimits,
    /// Source byte offsets were invalid or split UTF-8.
    InvalidSourceSpan,
    /// A Scribe map did not match the supplied live family.
    SpanMapMismatch(&'static str),
    /// Overlay mask contained undefined bits.
    InvalidLayers,
    /// Tile layer was requested without a binning result.
    MissingBinningForTileOverlay,
    /// Supplied viewport did not describe the supplied binning.
    BinningViewportMismatch,
    /// Tile-grid arithmetic overflowed.
    OverlayGeometryOverflow,
    /// JSON would exceed the worker response budget.
    JsonLimit {
        /// Ceiling.
        limit: usize,
        /// Bytes observed before refusal.
        needed: usize,
    },
}

impl fmt::Display for InspectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLimits => f.write_str("inspector limits must be nonzero"),
            Self::InvalidSourceSpan => f.write_str("invalid UTF-8 source span"),
            Self::SpanMapMismatch(message) => write!(f, "source span map mismatch: {message}"),
            Self::InvalidLayers => f.write_str("invalid debug-overlay layer mask"),
            Self::MissingBinningForTileOverlay => {
                f.write_str("tile overlay requires a Binning and Viewport")
            }
            Self::BinningViewportMismatch => {
                f.write_str("supplied Viewport does not describe the Binning grid")
            }
            Self::OverlayGeometryOverflow => f.write_str("debug-overlay grid size overflow"),
            Self::JsonLimit { limit, needed } => {
                write!(
                    f,
                    "Studio JSON used {needed} bytes, over the {limit}-byte limit"
                )
            }
        }
    }
}

impl std::error::Error for InspectError {}
