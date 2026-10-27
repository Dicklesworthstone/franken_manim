//! Live graph bindings and deterministic graph animations (§12.5).
//!
//! The binding retains the builder's edge-first/vertex-last identity table.
//! Layout changes preserve those handles and their styles; traversal consumes
//! fnx's ordered adjacency, and flow pulses use a separate overlay family.
//! All interpolation is composed from Choreo's existing native mechanisms.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use fmn_anim::{
    AnimConfig, AnimError, AnimState, Animation, AnimationGroup, AnimationSignature, RateFunc,
    Transform, fade_to_color, show_passing_flash, v_fade_in,
};
use fmn_core::color::Srgb;
use fmn_core::constants::YELLOW;
use fmn_core::rate::CatalogRate;
use fmn_core::types::Vec3;
use fmn_mobject::{Mob, Stage, StageError};

use super::{GraphLayout, NetworkGraph, NetworkGraphError};
use crate::VMobject;

/// A graph animation could not be constructed from the requested inputs.
#[derive(Clone, Debug, PartialEq)]
pub enum GraphAnimationError {
    /// The requested layout or graph geometry was refused.
    Graph(NetworkGraphError),
    /// A Choreo or arena operation failed.
    Animation(AnimError),
    /// A numeric control is non-finite or outside its documented domain.
    InvalidControl(&'static str),
    /// A flow step names an edge the undirected graph does not contain.
    MissingEdge {
        /// Requested source label.
        from: String,
        /// Requested destination label.
        to: String,
    },
    /// Flow requires at least one edge to animate.
    EmptyFlow,
}

impl std::fmt::Display for GraphAnimationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Graph(error) => error.fmt(f),
            Self::Animation(error) => error.fmt(f),
            Self::InvalidControl(name) => write!(f, "invalid graph animation control: {name}"),
            Self::MissingEdge { from, to } => {
                write!(f, "graph flow names missing edge {from:?} -> {to:?}")
            }
            Self::EmptyFlow => write!(f, "graph flow needs at least one edge"),
        }
    }
}

impl std::error::Error for GraphAnimationError {}

impl From<NetworkGraphError> for GraphAnimationError {
    fn from(error: NetworkGraphError) -> Self {
        Self::Graph(error)
    }
}

impl From<AnimError> for GraphAnimationError {
    fn from(error: AnimError) -> Self {
        Self::Animation(error)
    }
}

impl From<StageError> for GraphAnimationError {
    fn from(error: StageError) -> Self {
        Self::Animation(AnimError::Stage(error))
    }
}

/// An arena-backed graph with stable handles for its labeled vertices and edges.
///
/// Construct with [`NetworkGraph::add_to`], then add [`Self::mobject`] to the
/// scene. Moving, scaling, or restyling the family preserves the binding.
/// Reordering, replacing, or attaching children invalidates it and is refused
/// before an animation begins. Removing it from the scene does not delete it.
///
/// These graph types are also reexported by `fmn::prelude`. In a native scene,
/// build with `network.add_to(stage.arena_mut())`, root the family with
/// `stage.scene_mut().add(&[graph.mobject()])`, and pass each animation to
/// `stage.play(animation)`. The same animations work directly with Choreo:
///
/// ```
/// use fmn_anim::{Animation, RationalFrameClock, play_segment};
/// use fmn_core::rng::RngRoot;
/// use fmn_library::{
///     FlowAlongEdges, GraphLayout, GraphTraversal, LayoutTransition,
///     NetworkGraph, TraverseGraph,
/// };
/// use fmn_mobject::Stage;
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let mut stage = Stage::new();
/// let network = NetworkGraph::from_edge_list(
///     &["a", "b", "c"], &[("a", "b"), ("b", "c")],
/// ).laid_out(&GraphLayout::Circular)?;
/// let graph = network.add_to(&mut stage)?;
/// stage.add_to_scene(graph.mobject())?;
/// let animations: Vec<Box<dyn Animation>> = vec![
///     Box::new(LayoutTransition::new(&stage, &graph,
///         &GraphLayout::BreadthFirst { root: "a".into() })?),
///     Box::new(TraverseGraph::new(&mut stage, &graph, "a",
///         GraphTraversal::BreadthFirst)?),
///     Box::new(FlowAlongEdges::new(&mut stage, &graph,
///         &[("a", "b", 1.0), ("b", "c", 3.0)])?),
/// ];
/// let mut clock = RationalFrameClock::new(30)?;
/// let rng = RngRoot::from_seed(42);
/// for animation in animations {
///     stage.add_to_scene(animation.state().mobject())?;
///     play_segment(&mut stage, &mut clock, &rng,
///         &mut [animation], false, &mut |_frame| {})?;
/// }
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug)]
pub struct NetworkGraphMobject {
    network: NetworkGraph,
    root: Mob,
    children: Vec<Mob>,
    nodes: BTreeMap<String, Mob>,
    edges: Vec<(String, String, Mob)>,
}

impl NetworkGraph {
    /// Build the graph in an arena and retain label/edge identity for animation.
    /// This allocates the family without adding it to the scene's draw list.
    ///
    /// # Errors
    /// As [`Self::build`].
    pub fn add_to(&self, stage: &mut Stage) -> Result<NetworkGraphMobject, NetworkGraphError> {
        let root = stage.add(self.build()?);
        let children = stage
            .get(root)
            .expect("a freshly built graph is live")
            .submobjects()
            .to_vec();
        let edges: Vec<_> = self
            .graph
            .edges_ordered_borrowed()
            .into_iter()
            .zip(&children)
            .map(|((from, to, _), &mob)| (from.to_owned(), to.to_owned(), mob))
            .collect();
        let nodes = self
            .labels
            .iter()
            .cloned()
            .zip(children[edges.len()..].iter().copied())
            .collect();
        Ok(NetworkGraphMobject {
            network: self.clone(),
            root,
            children,
            nodes,
            edges,
        })
    }
}

impl NetworkGraphMobject {
    /// The family to add to a scene or transform as a whole.
    #[must_use]
    pub fn mobject(&self) -> Mob {
        self.root
    }

    /// The live vertex handle for a label.
    #[must_use]
    pub fn node(&self, label: &str) -> Option<Mob> {
        self.nodes.get(label).copied()
    }

    /// The live edge handle; either orientation names the same undirected edge.
    #[must_use]
    pub fn edge(&self, from: &str, to: &str) -> Option<Mob> {
        self.oriented_edge(from, to).map(|(mob, _)| mob)
    }

    /// Vertex labels in fnx's insertion order.
    #[must_use]
    pub fn nodes(&self) -> &[String] {
        self.network.nodes()
    }

    fn oriented_edge(&self, from: &str, to: &str) -> Option<(Mob, bool)> {
        self.edges.iter().find_map(|(a, b, mob)| {
            if a == from && b == to {
                Some((*mob, false))
            } else if a == to && b == from {
                Some((*mob, true))
            } else {
                None
            }
        })
    }

    fn validate(&self, stage: &Stage) -> Result<(), AnimError> {
        if let Some(error) = stage.updater_error() {
            return Err(AnimError::Updater(error.clone()));
        }
        let root = stage
            .get(self.root)
            .ok_or(AnimError::StaleHandle(self.root))?;
        if !root.buffer.is_empty() || root.submobjects() != self.children.as_slice() {
            return Err(AnimError::Stage(StageError::FamilyShapeMismatch));
        }
        for &mob in &self.children {
            let entry = stage.get(mob).ok_or(AnimError::StaleHandle(mob))?;
            if !entry.submobjects().is_empty() {
                return Err(AnimError::Stage(StageError::FamilyShapeMismatch));
            }
            if entry.buffer.schema().offset("joint_angle").is_none() {
                return Err(AnimError::Stage(StageError::SchemaMismatch));
            }
            if entry.buffer.is_empty() {
                return Err(AnimError::EmptyMobject);
            }
            let points = stage.get_points(mob).ok_or(AnimError::EmptyMobject)?;
            if points.iter().flatten().any(|value| !value.is_finite()) {
                return Err(AnimError::InvalidFramePhase(
                    "graph coordinates must be finite",
                ));
            }
            fmn_geom::QuadPath::from_points(points).map_err(StageError::Geometry)?;
        }
        Ok(())
    }
}

fn positive(value: f64, name: &'static str) -> Result<(), GraphAnimationError> {
    if !value.is_finite() || value <= 0.0 {
        return Err(GraphAnimationError::InvalidControl(name));
    }
    Ok(())
}

fn color_components(color: Srgb) -> Result<[f32; 3], GraphAnimationError> {
    let rgb = [color.r, color.g, color.b];
    if rgb
        .iter()
        .any(|value| !value.is_finite() || !(0.0..=1.0).contains(value))
    {
        return Err(GraphAnimationError::InvalidControl(
            "color components must be in [0, 1]",
        ));
    }
    Ok(rgb.map(|value| value as f32))
}

// The wrappers retain Choreo's lifecycle, updater accounting, error channel,
// and pure-worker lowering instead of reimplementing any interpolation law.
macro_rules! delegate_animation {
    () => {
        fn state(&self) -> &AnimState {
            self.inner.state()
        }
        fn state_mut(&mut self) -> &mut AnimState {
            self.inner.state_mut()
        }
        fn all_mobjects(&self) -> Vec<Mob> {
            self.inner.all_mobjects()
        }
        fn interpolate_submobject(&mut self, stage: &mut Stage, mobs: &[Mob], alpha: f64) {
            self.inner.interpolate_submobject(stage, mobs, alpha);
        }
        fn interpolate(&mut self, stage: &mut Stage, alpha: f64) {
            self.inner.interpolate(stage, alpha);
        }
        fn update_mobjects(&mut self, stage: &mut Stage, dt: f64) {
            self.inner.update_mobjects(stage, dt);
        }
        fn finish(&mut self, stage: &mut Stage) {
            self.inner.finish(stage);
        }
        fn abort(&mut self, stage: &mut Stage) {
            self.inner.abort(stage);
        }
        fn deferred_error(&self) -> Option<AnimError> {
            self.inner.deferred_error()
        }
        fn collect_resumed_updater_mobjects(&self, out: &mut Vec<Mob>) {
            self.inner.collect_resumed_updater_mobjects(out);
        }
        fn effect_signature(&self) -> AnimationSignature {
            self.inner.effect_signature()
        }
        fn freeze_interpolation(&self) -> Option<fmn_anim::pure_frame::FrozenAnimation> {
            self.inner.freeze_interpolation()
        }
        fn can_replay_interpolation(&self) -> bool {
            self.inner.can_replay_interpolation()
        }
        fn prepare_interpolation_replay(&mut self) {
            self.inner.prepare_interpolation_replay();
        }
    };
}

/// Move a graph to another deterministic layout while keeping its handles,
/// vertex shapes, and styles. Target coordinates are in scene space.
///
/// The target is prepared from the live graph at **begin**, so a transition in
/// a succession preserves the geometry and styles its predecessor produced.
/// Edge endpoints and vertex centers share one straight interpolation law.
/// Per-member lag is refused because it would detach edges from their nodes.
pub struct LayoutTransition {
    binding: NetworkGraphMobject,
    positions: BTreeMap<String, Vec3>,
    inner: Transform,
}

impl LayoutTransition {
    /// Prepare a layout change without mutating the arena.
    ///
    /// # Errors
    /// A stale binding, invalid layout, or non-finite target position.
    pub fn new(
        stage: &Stage,
        graph: &NetworkGraphMobject,
        layout: &GraphLayout,
    ) -> Result<Self, GraphAnimationError> {
        graph.validate(stage)?;
        if let GraphLayout::Spring { ideal_edge, .. } = layout {
            positive(*ideal_edge, "spring ideal_edge")?;
        }
        let target = graph.network.clone().laid_out(layout)?;
        if target
            .index
            .values()
            .flatten()
            .any(|value| !value.is_finite() || value.abs() > f64::from(f32::MAX))
        {
            return Err(GraphAnimationError::InvalidControl(
                "layout coordinates must fit finite records",
            ));
        }
        let inner = Transform::new(graph.root, graph.root).with_config(AnimConfig {
            name: "LayoutTransition".into(),
            suspend_mobject_updating: true,
            ..AnimConfig::default()
        });
        Ok(Self {
            binding: graph.clone(),
            positions: target.index,
            inner,
        })
    }

    /// Set the common timing/rate curve. `lag_ratio` must remain zero.
    #[must_use]
    pub fn with_config(mut self, config: AnimConfig) -> Self {
        self.inner = self.inner.with_config(config);
        self
    }
}

impl Animation for LayoutTransition {
    delegate_animation!();

    fn preflight_mobjects(&self) -> Vec<Mob> {
        vec![self.binding.root]
    }

    fn validate_begin(&self, stage: &Stage) -> Result<(), AnimError> {
        self.binding.validate(stage)?;
        if self.state().config.lag_ratio != 0.0 {
            return Err(AnimError::InvalidFramePhase(
                "graph layout requires zero per-member lag",
            ));
        }
        // Previous begin-time targets may be absent after a timeline restore.
        Transform::new(self.binding.root, self.binding.root)
            .with_config(self.state().config.clone())
            .validate_begin(stage)
    }

    fn begin(&mut self, stage: &mut Stage) -> Result<(), AnimError> {
        self.validate_begin(stage)?;
        let target = stage.copy_family(self.binding.root)?;
        let children = stage
            .get(target)
            .ok_or(AnimError::StaleHandle(target))?
            .submobjects()
            .to_vec();
        let offset = self.binding.edges.len();
        let mut centers = BTreeMap::new();
        for (label, &node) in self.binding.network.labels.iter().zip(&children[offset..]) {
            let position = self.positions[label];
            stage.move_to(node, position, [0.0; 3]);
            centers.insert(label.as_str(), stage.get_center(node));
        }
        for ((from, to, _), &edge) in self.binding.edges.iter().zip(&children[..offset]) {
            let start = centers[from.as_str()];
            let end = centers[to.as_str()];
            // Keep the live edge's record count, including author-created
            // subdivisions, so per-record colors and width profiles survive.
            let count = stage
                .get(edge)
                .ok_or(AnimError::StaleHandle(edge))?
                .buffer
                .len()
                .max(3);
            let points: Vec<_> = (0..count)
                .map(|index| {
                    let alpha = index as f64 / (count - 1) as f64;
                    std::array::from_fn(|axis| (1.0 - alpha) * start[axis] + alpha * end[axis])
                })
                .collect();
            stage.set_points(edge, &points)?;
        }
        self.inner =
            Transform::new(self.binding.root, target).with_config(self.state().config.clone());
        self.inner.begin(stage)
    }

    fn clean_up_from_scene(&mut self, stage: &mut Stage) {
        self.inner.clean_up_from_scene(stage);
    }
}

/// The visit order of a graph traversal animation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphTraversal {
    /// Queue order, as fnx's `bfs_layers` flattened from the named root.
    BreadthFirst,
    /// Preorder, as fnx's `dfs_preorder_nodes` from the named root.
    DepthFirst,
}

/// Visual and timing options for [`TraverseGraph`].
#[derive(Clone, Copy, Debug)]
pub struct GraphTraversalConfig {
    /// Total traversal duration, in seconds.
    pub run_time: f64,
    /// The color left on each visited vertex.
    pub color: Srgb,
}

impl Default for GraphTraversalConfig {
    fn default() -> Self {
        Self {
            run_time: 2.0,
            color: YELLOW,
        }
    }
}

/// Visit and highlight reachable vertices in fnx's deterministic adjacency
/// order. Disconnected components remain unchanged; an isolated root is visited
/// once. Neighbor insertion order matters, as it does in fnx (no label sort).
pub struct TraverseGraph {
    binding: NetworkGraphMobject,
    order: Vec<String>,
    inner: AnimationGroup,
}

impl TraverseGraph {
    /// Animate a two-second, yellow traversal from `root`.
    ///
    /// # Errors
    /// A stale binding or an unknown root.
    pub fn new(
        stage: &mut Stage,
        graph: &NetworkGraphMobject,
        root: &str,
        traversal: GraphTraversal,
    ) -> Result<Self, GraphAnimationError> {
        Self::with_config(
            stage,
            graph,
            root,
            traversal,
            GraphTraversalConfig::default(),
        )
    }

    /// Construct with a total duration and highlight color.
    ///
    /// # Errors
    /// As [`Self::new`], plus non-positive/non-finite duration or invalid color.
    pub fn with_config(
        stage: &mut Stage,
        graph: &NetworkGraphMobject,
        root: &str,
        traversal: GraphTraversal,
        config: GraphTraversalConfig,
    ) -> Result<Self, GraphAnimationError> {
        graph.validate(stage)?;
        positive(config.run_time, "traversal run_time")?;
        let color = color_components(config.color)?;
        let order = traversal_order(&graph.network, root, traversal)?;
        let mut animations: Vec<Box<dyn Animation>> = Vec::with_capacity(order.len());
        for label in &order {
            let mut animation = fade_to_color(stage, graph.nodes[label], color)?;
            animation.state_mut().config.suspend_mobject_updating = true;
            animations.push(Box::new(animation));
        }
        let inner = AnimationGroup::in_group(stage, animations, 1.0, Some(graph.root))?
            .with_run_time(config.run_time)
            .with_name("TraverseGraph");
        Ok(Self {
            binding: graph.clone(),
            order,
            inner,
        })
    }

    /// The exact visit sequence, including the root.
    #[must_use]
    pub fn order(&self) -> &[String] {
        &self.order
    }
}

fn traversal_order(
    graph: &NetworkGraph,
    root: &str,
    traversal: GraphTraversal,
) -> Result<Vec<String>, NetworkGraphError> {
    if !graph.graph.has_node(root) {
        return Err(NetworkGraphError::UnknownNode(root.to_owned()));
    }
    // This is the animation's schedule over the already-governed Graph
    // adjacency, not a second graph kernel dependency. fnx-algorithms at the
    // pin cannot enter D1's closure (the module's OQ-5 audit explains why).
    // Its bfs_layers / dfs_preorder_nodes use exactly this adjacency order.
    let mut order = Vec::new();
    let mut seen = BTreeSet::new();
    let mut queued = BTreeSet::from([root]);
    let mut pending = VecDeque::from([root]);
    while let Some(node) = match traversal {
        GraphTraversal::BreadthFirst => pending.pop_front(),
        GraphTraversal::DepthFirst => pending.pop_back(),
    } {
        if !seen.insert(node) {
            continue;
        }
        order.push(node.to_owned());
        let neighbors = graph.graph.neighbors(node).unwrap_or_default();
        match traversal {
            GraphTraversal::BreadthFirst => pending.extend(
                neighbors
                    .into_iter()
                    .filter(|neighbor| queued.insert(*neighbor)),
            ),
            GraphTraversal::DepthFirst => pending.extend(neighbors.into_iter().rev()),
        }
    }
    Ok(order)
}

impl Animation for TraverseGraph {
    delegate_animation!();

    fn preflight_mobjects(&self) -> Vec<Mob> {
        self.inner.preflight_mobjects()
    }
    fn validate_begin(&self, stage: &Stage) -> Result<(), AnimError> {
        self.binding.validate(stage)?;
        self.inner.validate_begin(stage)
    }
    fn begin(&mut self, stage: &mut Stage) -> Result<(), AnimError> {
        self.validate_begin(stage)?;
        self.inner.begin(stage)
    }
    fn clean_up_from_scene(&mut self, stage: &mut Stage) {
        self.inner.clean_up_from_scene(stage);
    }
}

/// Appearance and total duration of [`FlowAlongEdges`].
#[derive(Clone, Copy, Debug)]
pub struct GraphFlowConfig {
    /// Total duration; each edge receives its weight's share of this time.
    pub run_time: f64,
    /// Pulse color.
    pub color: Srgb,
    /// Pulse stroke width in the ordinary VMobject stroke units.
    pub stroke_width: f64,
    /// Length of the moving window as a fraction of its edge.
    pub time_width: f64,
}

impl Default for GraphFlowConfig {
    fn default() -> Self {
        Self {
            run_time: 2.0,
            color: YELLOW,
            stroke_width: 5.0,
            time_width: 0.25,
        }
    }
}

/// Directed pulses along an authored sequence of graph edges. Positive weights
/// allocate proportional time: weights 1 and 3 occupy one and three quarters
/// of the total duration. An undirected edge can be visited in either direction
/// or more than once. The underlying graph is never recolored or shortened.
///
/// Paths are frozen from the live edges at begin, as with `MoveAlongPath`.
/// Pulse opacity is zero outside each edge's interval, including the two ends;
/// cleanup removes only the temporary overlay, leaving the graph rooted.
pub struct FlowAlongEdges {
    binding: NetworkGraphMobject,
    pulses: Vec<(Mob, Mob, bool)>,
    color: [f32; 3],
    stroke_width: f64,
    inner: AnimationGroup,
}

impl FlowAlongEdges {
    /// Construct a two-second yellow flow from `(from, to, positive_weight)`.
    ///
    /// # Errors
    /// A stale graph, missing edge, empty sequence, or invalid weight.
    pub fn new(
        stage: &mut Stage,
        graph: &NetworkGraphMobject,
        edges: &[(&str, &str, f64)],
    ) -> Result<Self, GraphAnimationError> {
        Self::with_config(stage, graph, edges, GraphFlowConfig::default())
    }

    /// Construct with explicit total time and pulse appearance.
    ///
    /// # Errors
    /// As [`Self::new`], plus non-finite/non-positive timing or stroke controls.
    /// Validation completes before any pulse family is allocated.
    pub fn with_config(
        stage: &mut Stage,
        graph: &NetworkGraphMobject,
        edges: &[(&str, &str, f64)],
        config: GraphFlowConfig,
    ) -> Result<Self, GraphAnimationError> {
        graph.validate(stage)?;
        if edges.is_empty() {
            return Err(GraphAnimationError::EmptyFlow);
        }
        positive(config.run_time, "flow run_time")?;
        positive(config.stroke_width, "flow stroke_width")?;
        positive(config.time_width, "flow time_width")?;
        if config.stroke_width > f64::from(f32::MAX) {
            return Err(GraphAnimationError::InvalidControl(
                "flow stroke_width exceeds f32",
            ));
        }
        let color = color_components(config.color)?;
        let mut largest_weight = 0.0_f64;
        let mut sources = Vec::with_capacity(edges.len());
        for &(from, to, weight) in edges {
            positive(weight, "edge weight")?;
            largest_weight = largest_weight.max(weight);
            let source =
                graph
                    .oriented_edge(from, to)
                    .ok_or_else(|| GraphAnimationError::MissingEdge {
                        from: from.to_owned(),
                        to: to.to_owned(),
                    })?;
            sources.push((source, weight));
        }
        // Normalize before summing: proportionally equivalent large weights
        // remain usable even when their unscaled sum would overflow.
        let total: f64 = sources
            .iter()
            .map(|(_, weight)| weight / largest_weight)
            .sum();
        let mut end = 0.0;
        for (_, weight) in &mut sources {
            *weight = (*weight / largest_weight) / total;
            let next = end + *weight;
            if !next.is_finite() || next <= end {
                return Err(GraphAnimationError::InvalidControl(
                    "edge weight cannot occupy a timeline interval",
                ));
            }
            end = next;
        }
        let overlay = stage.add(VMobject::new());
        let mut pulses = Vec::with_capacity(edges.len());
        let mut animations: Vec<Box<dyn Animation>> = Vec::with_capacity(edges.len());
        for ((source, reverse), duration) in sources {
            let pulse = stage.add(VMobject::new());
            stage.attach(overlay, pulse)?;
            pulses.push((source, pulse, reverse));
            let mut flash = show_passing_flash(pulse, config.time_width);
            flash.state_mut().config.rate_func = RateFunc::linear();
            flash.state_mut().config.remover = false;
            let opacity = v_fade_in(pulse).with_config(AnimConfig {
                rate_func: RateFunc::Catalog(CatalogRate::ThereAndBack),
                ..AnimConfig::default()
            });
            // The two mechanisms write disjoint fields of the same pulse.
            // They begin eagerly; the fade snapshots its opacity before zeroing it.
            let member = AnimationGroup::in_group(
                stage,
                vec![Box::new(flash), Box::new(opacity)],
                0.0,
                Some(pulse),
            )?
            .with_run_time(duration);
            animations.push(Box::new(member));
        }
        let inner = AnimationGroup::in_group(stage, animations, 1.0, Some(overlay))?
            .with_run_time(config.run_time)
            .with_name("FlowAlongEdges");
        Ok(Self {
            binding: graph.clone(),
            pulses,
            color,
            stroke_width: config.stroke_width,
            inner,
        })
    }

    /// Pulse intervals on the normalized internal timeline, in authored order.
    /// Their durations are proportional to the supplied weights.
    #[must_use]
    pub fn timings(&self) -> &[fmn_anim::composition::Interval] {
        self.inner.timings()
    }

    /// The temporary pulse handles, in authored order, for inspection.
    pub fn pulses(&self) -> impl Iterator<Item = Mob> + '_ {
        self.pulses.iter().map(|(_, pulse, _)| *pulse)
    }
}

impl Animation for FlowAlongEdges {
    delegate_animation!();

    fn preflight_mobjects(&self) -> Vec<Mob> {
        let mut mobs = self.inner.preflight_mobjects();
        mobs.push(self.binding.root);
        mobs
    }
    fn validate_begin(&self, stage: &Stage) -> Result<(), AnimError> {
        self.binding.validate(stage)?;
        self.inner.validate_begin(stage)
    }
    fn begin(&mut self, stage: &mut Stage) -> Result<(), AnimError> {
        self.validate_begin(stage)?;
        for &(source, pulse, reverse) in &self.pulses {
            let mut points = stage.get_points(source).ok_or(AnimError::EmptyMobject)?;
            if reverse {
                points.reverse();
            }
            stage.set_points(pulse, &points)?;
            stage.set_color_lanes(pulse, "fill_rgba", None, Some(0.0), false);
            stage.set_color_lanes(
                pulse,
                "stroke_rgba",
                Some(self.color.map(f64::from)),
                Some(1.0),
                false,
            );
            stage.set_scalar_lanes(pulse, "stroke_width", self.stroke_width, false);
        }
        self.inner.begin(stage)
    }
    fn clean_up_from_scene(&mut self, stage: &mut Stage) {
        self.inner.clean_up_from_scene(stage);
        if self.inner.deferred_error().is_none() && stage.updater_error().is_none() {
            stage.remove_from_scene(self.state().mobject());
        }
    }
}
