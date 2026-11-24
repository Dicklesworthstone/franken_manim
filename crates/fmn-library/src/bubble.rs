//! Content-bearing native bubbles over the existing drawings shelf.
//!
//! The detached drawing constructors own the body geometry. This module owns
//! its live relationship to content: stable arena handles, fitting, pinning,
//! replacement and clearing. Content is adopted by handle, never cloned or
//! deleted; other parents, live views and updaters keep their usual semantics.

use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;

use fmn_core::constants::{BLACK, LEFT, MED_LARGE_BUFF, MED_SMALL_BUFF, RIGHT, SMALL_BUFF, WHITE};
use fmn_core::rng::Pcg64Dxsm;
use fmn_core::types::Vec3;
use fmn_mobject::bbox::BoxAccum;
use fmn_mobject::stage::{SURFACE_VALIDITY_FIELD, surface_sample_is_present};
use fmn_mobject::{BoundingBox, IdBuildHasher, Mob, Placement, RenderPrimitive, Stage, StageError};

use crate::boolean_ops::BooleanMobjectError;
use crate::drawings::{
    DrawingsAssetError, ThoughtBubbleShape, bubble_with, speech_bubble, thought_bubble,
};
use crate::style::Style;
use crate::svg::SvgDocument;
use crate::vmobject::{VMobject, v_group};

/// Bound body construction before entering the pairwise boolean kernel.
pub const MAX_BUBBLE_BULGES: usize = 512;
/// Maximum trailing circles in a thought bubble.
pub const MAX_BUBBLE_TRAIL_CIRCLES: usize = 64;
/// Maximum body records admitted by construction or a live resize.
pub const MAX_BUBBLE_POINTS: usize = 1_048_576;

/// The horizontal direction of the bubble's stem.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum BubbleDirection {
    /// Stem at the lower-left, the Reference default.
    #[default]
    Left,
    /// Stem at the lower-right.
    Right,
}

impl BubbleDirection {
    fn vector(self) -> Vec3 {
        match self {
            Self::Left => LEFT,
            Self::Right => RIGHT,
        }
    }

    fn opposite(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
        }
    }
}

/// Shared construction controls. Speech/thought constructors specialize the
/// Reference's buffer and invisible filler dimensions in their defaults.
#[derive(Debug, Clone, PartialEq)]
pub struct BubbleConfig {
    /// Horizontal stem direction.
    pub direction: BubbleDirection,
    /// Clearance between the original content and body.
    pub buff: f64,
    /// Invisible content dimensions when no handle is supplied.
    pub filler_shape: (f64, f64),
    /// Initial body style; content paint is untouched.
    pub style: Style,
}

impl Default for BubbleConfig {
    fn default() -> Self {
        Self {
            direction: BubbleDirection::Left,
            buff: 1.0,
            filler_shape: (3.0, 2.0),
            style: Style::default().fill(BLACK, 0.8).stroke(WHITE, 3.0, 1.0),
        }
    }
}

/// A bubble could not be constructed or edited. Invalid inputs and changed
/// component topology are checked before any live geometry is modified.
#[derive(Debug)]
pub enum BubbleError {
    /// A constructor or positioning value cannot describe finite geometry.
    InvalidParameter {
        /// Setting or geometry being rejected.
        name: &'static str,
        /// Required condition.
        reason: &'static str,
    },
    /// A handle is stale, foreign, or violates Marionette's graph rules.
    Stage(StageError),
    /// The existing geometry constructor rejected its operation.
    Geometry(BooleanMobjectError),
    /// The explicit SVG body could not be constructed.
    Asset(DrawingsAssetError),
    /// The retained body/content relationship was changed outside this binding.
    ChangedTopology,
    /// Content or a pin target overlaps the bubble body or contains its root.
    OverlappingFamily,
    /// A cleared bubble has no content to resize around.
    NoContent,
    /// Bounded geometry construction refused an excessive request.
    Limit {
        /// Resource whose count exceeds the declared limit.
        resource: &'static str,
        /// Requested count, saturated when the arithmetic overflows.
        requested: usize,
        /// Declared maximum.
        max: usize,
    },
}

impl fmt::Display for BubbleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidParameter { name, reason } => {
                write!(f, "invalid bubble {name}: {reason}")
            }
            Self::Stage(error) => error.fmt(f),
            Self::Geometry(error) => error.fmt(f),
            Self::Asset(error) => error.fmt(f),
            Self::ChangedTopology => {
                f.write_str("bubble component topology changed outside its binding")
            }
            Self::OverlappingFamily => {
                f.write_str("bubble content or pin target overlaps its body or root")
            }
            Self::NoContent => f.write_str("a cleared bubble has no content to resize around"),
            Self::Limit {
                resource,
                requested,
                max,
            } => write!(
                f,
                "bubble {resource} needs {requested} entries, above the {max} limit"
            ),
        }
    }
}

impl std::error::Error for BubbleError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Stage(error) => Some(error),
            Self::Geometry(error) => Some(error),
            Self::Asset(error) => Some(error),
            _ => None,
        }
    }
}

impl From<StageError> for BubbleError {
    fn from(error: StageError) -> Self {
        Self::Stage(error)
    }
}

impl From<BooleanMobjectError> for BubbleError {
    fn from(error: BooleanMobjectError) -> Self {
        Self::Geometry(error)
    }
}

impl From<DrawingsAssetError> for BubbleError {
    fn from(error: DrawingsAssetError) -> Self {
        Self::Asset(error)
    }
}

fn invalid(name: &'static str, reason: &'static str) -> BubbleError {
    BubbleError::InvalidParameter { name, reason }
}

fn nonnegative(value: f64, name: &'static str) -> Result<(), BubbleError> {
    if value.is_finite() && value >= 0.0 && value <= f64::from(f32::MAX) {
        Ok(())
    } else {
        Err(invalid(
            name,
            "must be finite, nonnegative and representable in record space",
        ))
    }
}

fn finite_point(point: Vec3) -> bool {
    point
        .iter()
        .all(|x| x.is_finite() && x.abs() <= f64::from(f32::MAX))
}

fn check_bounds(bounds: BoundingBox) -> Result<BoundingBox, BubbleError> {
    if !finite_point(bounds.min) || !finite_point(bounds.mid) || !finite_point(bounds.max) {
        return Err(invalid(
            "content bounds",
            "must fit finite record-space coordinates",
        ));
    }
    Ok(bounds)
}

fn bounds_of(stage: &Stage, mob: Mob) -> Result<BoundingBox, BubbleError> {
    stage.try_get(mob)?;
    check_bounds(stage.get_bounding_box(mob))
}

// Observe only this family under the proposed scale. This is the same point
// placement used by Marionette's positional operations, including publication
// into f32 records when a live view is attached. Surface holes contribute no
// bounds, and a point-free family retains its positional anchor.
fn bounds_after_scale(
    stage: &Stage,
    root: Mob,
    scaled_family: &HashSet<Mob, IdBuildHasher>,
    affine: Placement,
) -> Result<BoundingBox, BubbleError> {
    let family = stage.family(root);
    if !family.iter().any(|member| scaled_family.contains(member)) {
        return bounds_of(stage, root);
    }
    let mut bounds = BoxAccum::new();
    let mut has_points = false;
    for member in family {
        let entry = stage.try_get(member)?;
        let Some(points) = entry.buffer.read_column("point") else {
            continue;
        };
        let validity = entry.buffer.read_column(SURFACE_VALIDITY_FIELD);
        let original = entry.placement();
        let moving = scaled_family.contains(&member);
        let projected = if moving {
            affine.compose(original)
        } else {
            original
        };
        let publish = moving && !entry.buffer.is_empty() && entry.buffer.live_view_count() > 0;
        for (index, point) in points.as_chunks::<3>().0.iter().enumerate() {
            if !surface_sample_is_present(validity.as_deref(), index) {
                continue;
            }
            let local = point.map(f64::from);
            let world = if publish {
                affine.apply_point(original.apply_point(local))
            } else {
                projected.apply_point(local)
            };
            if !finite_point(world) {
                return Err(invalid(
                    "fitted content",
                    "must fit finite record-space coordinates",
                ));
            }
            #[allow(clippy::cast_possible_truncation)]
            let world = if publish {
                world.map(|coordinate| f64::from(coordinate as f32))
            } else {
                world
            };
            bounds.push(world);
            has_points = true;
        }
    }
    if !has_points {
        let entry = stage.try_get(root)?;
        let projected = if scaled_family.contains(&root) {
            affine.compose(entry.placement())
        } else if entry.buffer.is_empty() {
            // Scaling a descendant refreshes its point-free ancestors.
            Placement::IDENTITY
        } else {
            entry.placement()
        };
        bounds.push(projected.apply_point([0.0; 3]));
    }
    check_bounds(bounds.finish())
}

fn bubble_center(bounds: BoundingBox) -> Result<Vec3, BubbleError> {
    let center = [
        bounds.mid[0],
        bounds.mid[1] + 0.125 * bounds.height(),
        bounds.mid[2],
    ];
    if !finite_point(center) {
        return Err(invalid(
            "center",
            "must fit finite record-space coordinates",
        ));
    }
    Ok(center)
}

fn validate_config(config: &BubbleConfig) -> Result<(), BubbleError> {
    nonnegative(config.buff, "buff")?;
    for side in [config.filler_shape.0, config.filler_shape.1] {
        nonnegative(side, "filler_shape")?;
        if side == 0.0 {
            return Err(invalid("filler_shape", "both dimensions must be positive"));
        }
    }
    let style = config.style;
    for value in [
        style.fill_color.r,
        style.fill_color.g,
        style.fill_color.b,
        style.stroke_color.r,
        style.stroke_color.g,
        style.stroke_color.b,
        style.fill_opacity,
        style.stroke_opacity,
    ] {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(invalid(
                "style",
                "colors and opacities must be finite in 0..=1",
            ));
        }
    }
    nonnegative(style.stroke_width, "stroke_width")?;
    nonnegative(style.fill_border_width, "fill_border_width")
}

/// Native rounded speech body with a triangular stem.
#[derive(Debug, Clone, PartialEq)]
pub struct SpeechBubble {
    /// Shared body settings.
    pub config: BubbleConfig,
    /// Stem height divided by the surrounding rectangle's height.
    pub stem_height_to_bubble_height: f64,
    /// Ordered fractions of the bottom edge joined to the stem.
    pub stem_top_x_props: (f64, f64),
}

impl Default for SpeechBubble {
    fn default() -> Self {
        Self {
            config: BubbleConfig {
                buff: MED_SMALL_BUFF,
                filler_shape: (2.0, 1.0),
                ..BubbleConfig::default()
            },
            stem_height_to_bubble_height: 0.5,
            stem_top_x_props: (0.2, 0.3),
        }
    }
}

impl SpeechBubble {
    /// The Reference's speech-bubble defaults.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Install the body and adopt the original content handle. `None` creates
    /// an invisible filler. Add [`Bubble::mobject`] to the scene separately.
    ///
    /// # Errors
    /// Invalid controls, a stale content handle, or refused body geometry.
    pub fn add_to(&self, stage: &mut Stage, content: Option<Mob>) -> Result<Bubble, BubbleError> {
        nonnegative(self.stem_height_to_bubble_height, "stem height")?;
        let (left, right) = self.stem_top_x_props;
        if !left.is_finite()
            || !right.is_finite()
            || !(0.0..=1.0).contains(&left)
            || !(0.0..=1.0).contains(&right)
            || left >= right
        {
            return Err(invalid(
                "stem_top_x_props",
                "must be ordered distinct fractions in 0..=1",
            ));
        }
        Bubble::install(
            stage,
            content,
            self.config.clone(),
            Body::Speech {
                stem_height: self.stem_height_to_bubble_height,
                stem_top: self.stem_top_x_props,
            },
        )
    }
}

/// Native cloud body and thought-circle trail. Its one RNG is supplied by the
/// scene; rebuilding retains the original random shape instead of consuming
/// unrelated scene randomness on every resize.
#[derive(Debug, Clone, PartialEq)]
pub struct ThoughtBubble {
    /// Shared body settings.
    pub config: BubbleConfig,
    /// Cloud and trailing-circle geometry.
    pub shape: ThoughtBubbleShape,
}

impl Default for ThoughtBubble {
    fn default() -> Self {
        Self {
            config: BubbleConfig {
                buff: SMALL_BUFF,
                filler_shape: (2.0, 1.0),
                ..BubbleConfig::default()
            },
            shape: ThoughtBubbleShape::default(),
        }
    }
}

impl ThoughtBubble {
    /// The Reference's thought-bubble defaults.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Install one thought bubble. Validation failures leave the Stage and
    /// caller's RNG unchanged; successful construction advances that stream
    /// exactly as the existing detached thought-body constructor does.
    ///
    /// # Errors
    /// Invalid controls, excessive cloud/trail geometry, a stale content
    /// handle, or a body geometry failure.
    pub fn add_to(
        &self,
        stage: &mut Stage,
        content: Option<Mob>,
        rng: &mut Pcg64Dxsm,
    ) -> Result<Bubble, BubbleError> {
        let bubble = Bubble::install(
            stage,
            content,
            self.config.clone(),
            Body::Thought {
                shape: self.shape.clone(),
                origin: rng.clone(),
            },
        )?;
        if let Some(next) = &bubble.next_rng {
            *rng = next.clone();
        }
        Ok(bubble)
    }
}

#[derive(Debug, Clone)]
enum Body {
    Speech {
        stem_height: f64,
        stem_top: (f64, f64),
    },
    Thought {
        shape: ThoughtBubbleShape,
        origin: Pcg64Dxsm,
    },
    Svg(Arc<SvgDocument>),
}

#[derive(Debug, Clone)]
struct BodyMember {
    mob: Mob,
    children: Vec<Mob>,
}

/// A content-bearing bubble in one Stage.
///
/// The root and every body handle remain stable through resizing. Arbitrary
/// content (including Text, Tex, images and shared groups) stays in its original
/// arena entry. Clearing or replacing it removes only the bubble's parent edge;
/// it never destroys caller-owned content or changes its other parents/roots.
/// External deletion or body-topology changes produce a typed error. Use
/// [`Self::copy`] for an independently editable copy with rebound handles.
#[derive(Debug)]
pub struct Bubble {
    root: Mob,
    body: Mob,
    content: Option<Mob>,
    members: Vec<BodyMember>,
    config: BubbleConfig,
    source: Body,
    next_rng: Option<Pcg64Dxsm>,
}

impl Bubble {
    /// Adopt content under an explicit user-supplied SVG body (ADR-0020).
    /// No asset lookup, network access, or bundled third-party drawing occurs.
    ///
    /// # Errors
    /// Invalid controls, a stale content handle, or refused SVG geometry.
    pub fn from_document(
        stage: &mut Stage,
        document: &SvgDocument,
        content: Option<Mob>,
        config: BubbleConfig,
    ) -> Result<Self, BubbleError> {
        Self::install(
            stage,
            content,
            config,
            Body::Svg(Arc::new(document.clone())),
        )
    }

    fn install(
        stage: &mut Stage,
        content: Option<Mob>,
        config: BubbleConfig,
        source: Body,
    ) -> Result<Self, BubbleError> {
        validate_config(&config)?;
        let bounds = match content {
            Some(mob) => bounds_of(stage, mob)?,
            None => {
                let (width, height) = config.filler_shape;
                BoundingBox {
                    min: [-width / 2.0, -height / 2.0, 0.0],
                    mid: [0.0; 3],
                    max: [width / 2.0, height / 2.0, 0.0],
                }
            }
        };
        let (body, next_rng) = build_body(&source, bounds, &config)?;
        let filler = if content.is_none() {
            Some(
                crate::Rectangle::new()
                    .width(config.filler_shape.0)
                    .height(config.filler_shape.1)
                    .build()
                    .map_err(BooleanMobjectError::from)?
                    .map_style_deep(|style| style.fill(BLACK, 0.0).stroke(BLACK, 0.0, 0.0)),
            )
        } else {
            None
        };
        // Everything that may reject authored input is prepared before Stage::add.
        let root = stage.add(v_group([body]));
        let body = stage.try_get(root)?.submobjects()[0];
        let content = match content {
            Some(mob) => mob,
            None => stage.add(filler.expect("missing content has an admitted filler")),
        };
        stage.attach(root, content)?;
        let members = stage
            .family(body)
            .into_iter()
            .map(|mob| BodyMember {
                mob,
                children: stage
                    .get(mob)
                    .expect("new body member")
                    .submobjects()
                    .to_vec(),
            })
            .collect();
        Ok(Self {
            root,
            body,
            content: Some(content),
            members,
            config,
            source,
            next_rng,
        })
    }

    /// The stable family root to add to a scene or animate.
    #[must_use]
    pub const fn mobject(&self) -> Mob {
        self.root
    }

    /// The stable body root; its native style and updaters survive resizing.
    #[must_use]
    pub const fn body(&self) -> Mob {
        self.body
    }

    /// Current content, or `None` after clearing.
    #[must_use]
    pub const fn content(&self) -> Option<Mob> {
        self.content
    }

    /// The direction maintained by this binding's explicit flip operation.
    #[must_use]
    pub const fn direction(&self) -> BubbleDirection {
        self.config.direction
    }

    fn validate(&self, stage: &Stage) -> Result<(), BubbleError> {
        let root = stage.try_get(self.root)?;
        if root.submobjects().first() != Some(&self.body) {
            return Err(BubbleError::ChangedTopology);
        }
        if let Some(content) = self.content {
            stage.try_get(content)?;
            if !root.submobjects().contains(&content) {
                return Err(BubbleError::ChangedTopology);
            }
            self.check_content(stage, content)?;
        }
        for member in &self.members {
            let entry = stage.try_get(member.mob)?;
            if entry.submobjects() != member.children
                || entry.render_primitive() != RenderPrimitive::Vector
                || entry.buffer.schema().field_width("point") != Some(3)
                || entry.buffer.schema().field_width("joint_angle") != Some(1)
            {
                return Err(BubbleError::ChangedTopology);
            }
        }
        Ok(())
    }

    fn check_content(&self, stage: &Stage, mob: Mob) -> Result<BoundingBox, BubbleError> {
        let bounds = bounds_of(stage, mob)?;
        let family: HashSet<_, IdBuildHasher> = stage.family(mob).into_iter().collect();
        if family.contains(&self.root) || self.members.iter().any(|part| family.contains(&part.mob))
        {
            return Err(BubbleError::OverlappingFamily);
        }
        Ok(bounds)
    }

    /// The Reference's lower corner in the current stem direction.
    ///
    /// # Errors
    /// A stale/changed binding or nonfinite live geometry.
    pub fn get_tip(&self, stage: &Stage) -> Result<Vec3, BubbleError> {
        self.validate(stage)?;
        let direction = self.config.direction.vector();
        Ok(bounds_of(stage, self.root)?.point([direction[0], -1.0, 0.0]))
    }

    /// Current family center, shifted upward by one eighth of its height.
    ///
    /// # Errors
    /// A stale/changed binding or nonfinite live geometry.
    pub fn get_bubble_center(&self, stage: &Stage) -> Result<Vec3, BubbleError> {
        self.validate(stage)?;
        bubble_center(bounds_of(stage, self.root)?)
    }

    /// Move the complete bubble so its tip reaches `point`.
    ///
    /// # Errors
    /// A stale/changed binding or a translation outside finite record space.
    pub fn move_tip_to(
        &mut self,
        stage: &mut Stage,
        point: Vec3,
    ) -> Result<&mut Self, BubbleError> {
        let tip = self.get_tip(stage)?;
        if !finite_point(point) {
            return Err(invalid(
                "pin point",
                "must fit finite record-space coordinates",
            ));
        }
        let shift = std::array::from_fn(|axis| point[axis] - tip[axis]);
        let bounds = bounds_of(stage, self.root)?;
        for corner in [bounds.min, bounds.max] {
            if !finite_point(std::array::from_fn(|axis| corner[axis] + shift[axis])) {
                return Err(invalid(
                    "pinned bounds",
                    "must fit finite record-space coordinates",
                ));
            }
        }
        stage.shift(self.root, shift);
        Ok(self)
    }

    /// Reflect horizontally about the live bubble center. With `only_body`,
    /// content changes position with the bubble but remains readable.
    ///
    /// # Errors
    /// A stale/changed binding or nonfinite live geometry.
    pub fn flip(&mut self, stage: &mut Stage, only_body: bool) -> Result<&mut Self, BubbleError> {
        self.validate(stage)?;
        let center = bounds_of(stage, self.root)?.mid;
        stage.stretch_about(self.root, -1.0, 0, Some(center), None);
        if only_body && let Some(content) = self.content {
            stage.stretch(content, -1.0, 0);
        }
        self.config.direction = self.config.direction.opposite();
        Ok(self)
    }

    /// Place the tip at the target's upper corner opposite the stem direction.
    /// Auto-flipping preserves content orientation and follows the Reference's
    /// sign-of-target-center rule. Pinning is one positioning operation, not an
    /// implicit updater; call it again after moving the target.
    ///
    /// # Errors
    /// Stale/overlapping families or a result outside finite record space.
    pub fn pin_to(
        &mut self,
        stage: &mut Stage,
        target: Mob,
        auto_flip: bool,
    ) -> Result<&mut Self, BubbleError> {
        self.validate(stage)?;
        let target_bounds = self.check_content(stage, target)?;
        let target_family: HashSet<_, IdBuildHasher> = stage.family(target).into_iter().collect();
        if stage
            .family(self.root)
            .iter()
            .any(|mob| target_family.contains(mob))
        {
            return Err(BubbleError::OverlappingFamily);
        }
        let current = self.config.direction.vector()[0];
        let x = target_bounds.mid[0];
        let flip =
            auto_flip && ((x > 0.0 && current < 0.0) || (x < 0.0 && current > 0.0) || x == 0.0);
        let direction = if flip { -current } else { current };
        let point = target_bounds.point([-direction, 1.0, 0.0]);
        // Preflight the complete final reflected/translated box before flip.
        let bounds = bounds_of(stage, self.root)?;
        let tip = bounds.point([direction, -1.0, 0.0]);
        let shift: Vec3 = std::array::from_fn(|axis| point[axis] - tip[axis]);
        for corner in [bounds.min, bounds.max] {
            if !finite_point(std::array::from_fn(|axis| corner[axis] + shift[axis])) {
                return Err(invalid(
                    "pinned bounds",
                    "must fit finite record-space coordinates",
                ));
            }
        }
        if flip {
            self.flip(stage, true)?;
        }
        self.move_tip_to(stage, point)
    }

    /// Fit without enlarging or distorting content, then center it inside the
    /// body using the Reference's width/height clearances. The object need not
    /// already be the bubble's content; this method changes no parent edges.
    ///
    /// # Errors
    /// Invalid buffers, stale/overlapping families, an interior too small to
    /// contain content, or a scale below Marionette's supported minimum.
    pub fn position_mobject_inside(
        &self,
        stage: &mut Stage,
        mob: Mob,
        buff: f64,
    ) -> Result<Mob, BubbleError> {
        self.validate(stage)?;
        let content = self.check_content(stage, mob)?;
        nonnegative(buff, "content buff")?;
        let body = bounds_of(stage, self.body)?;
        let width = body.width() - 2.0 * buff;
        let height = body.height() / 1.5 - 2.0 * buff;
        if width <= 0.0 || height <= 0.0 {
            return Err(invalid(
                "content buff",
                "must leave positive interior width and height",
            ));
        }
        let mut scale = 1.0_f64;
        if content.width() > width {
            scale = scale.min(width / content.width());
        }
        if content.height() > height {
            scale = scale.min(height / content.height());
        }
        // Stage::scale clamps smaller factors to 1e-8. Refuse instead of
        // claiming that a clamped result fits the requested interior.
        if !scale.is_finite() || scale < 1e-8 {
            return Err(invalid(
                "content scale",
                "falls below the supported minimum scale",
            ));
        }
        // Fitting existing or shared content changes the bubble's own bounds.
        // Observe those post-scale bounds before publishing any live edit.
        let (fitted, center) = if scale < 1.0 {
            let scaled_family: HashSet<_, IdBuildHasher> = stage.family(mob).into_iter().collect();
            let affine = Placement::about(
                [[scale, 0.0, 0.0], [0.0, scale, 0.0], [0.0, 0.0, scale]],
                content.mid,
            );
            let fitted = bounds_after_scale(stage, mob, &scaled_family, affine)?;
            let center = bubble_center(bounds_after_scale(
                stage,
                self.root,
                &scaled_family,
                affine,
            )?)?;
            (fitted, center)
        } else {
            (content, self.get_bubble_center(stage)?)
        };
        let shift: Vec3 = std::array::from_fn(|axis| center[axis] - fitted.mid[axis]);
        for corner in [fitted.min, fitted.max] {
            if !finite_point(std::array::from_fn(|axis| corner[axis] + shift[axis])) {
                return Err(invalid(
                    "fitted content",
                    "must fit finite record-space coordinates",
                ));
            }
        }
        if scale < 1.0 {
            stage.scale_about(mob, scale, Some(content.mid), None);
            stage.scale_dot_radii(mob, scale);
        }
        stage.shift(mob, shift);
        Ok(mob)
    }

    /// Fit and replace the visible content using the Reference's default
    /// interior buffer. The previous content remains alive in its Stage.
    ///
    /// # Errors
    /// As [`Self::add_content_with_buff`].
    pub fn add_content(&mut self, stage: &mut Stage, content: Mob) -> Result<Mob, BubbleError> {
        self.add_content_with_buff(stage, content, MED_LARGE_BUFF)
    }

    /// [`Self::add_content`] with an explicit interior buffer. Additional root
    /// children (such as annotations) retain their order after the content.
    ///
    /// # Errors
    /// Invalid buffers, a stale/changed binding, or overlapping content.
    pub fn add_content_with_buff(
        &mut self,
        stage: &mut Stage,
        content: Mob,
        buff: f64,
    ) -> Result<Mob, BubbleError> {
        self.validate(stage)?;
        self.check_content(stage, content)?;
        let mut children = stage.try_get(self.root)?.submobjects().to_vec();
        children.retain(|&child| Some(child) != self.content && child != content);
        children.insert(1, content);
        self.position_mobject_inside(stage, content, buff)?;
        stage.replace_children(self.root, &children)?;
        self.content = Some(content);
        Ok(content)
    }

    /// Rebuild the body around the live content without replacing any body
    /// handle, style, uniform, updater or external parent. Content is not moved.
    /// The same original thought-cloud RNG state supplies repeatable geometry.
    ///
    /// # Errors
    /// A stale/changed binding, no content, invalid buffer, or refused geometry.
    pub fn resize_to_content(
        &mut self,
        stage: &mut Stage,
        buff: f64,
    ) -> Result<&mut Self, BubbleError> {
        self.validate(stage)?;
        let content = self.content.ok_or(BubbleError::NoContent)?;
        let bounds = self.check_content(stage, content)?;
        let mut config = self.config.clone();
        config.buff = buff;
        validate_config(&config)?;
        let (body, _) = build_body(&self.source, bounds, &config)?;
        let mut pending = vec![&body];
        let mut geometry = Vec::new();
        while let Some(member) = pending.pop() {
            geometry.push(member);
            pending.extend(member.children().iter().rev());
        }
        if geometry.len() != self.members.len()
            || geometry
                .iter()
                .zip(&self.members)
                .any(|(new, old)| new.children().len() != old.children.len())
        {
            return Err(BubbleError::ChangedTopology);
        }
        // All paths and counts have been admitted by build_body. The existing
        // point setter owns resize/view and paint-preservation semantics.
        for (new, old) in geometry.into_iter().zip(&self.members) {
            stage.set_points(old.mob, new.points())?;
        }
        self.config.buff = buff;
        Ok(self)
    }

    /// Detach content and return its still-live handle. The body and other
    /// children remain in place; repeated clearing is a harmless no-op.
    ///
    /// # Errors
    /// A stale or externally changed binding.
    pub fn clear(&mut self, stage: &mut Stage) -> Result<Option<Mob>, BubbleError> {
        self.validate(stage)?;
        let previous = self.content.take();
        if let Some(content) = previous {
            stage.detach(self.root, content);
        }
        Ok(previous)
    }

    /// Copy the native family once and rebind all semantic component handles.
    /// Scene membership is not copied, following [`Stage::copy_family_mapped`].
    ///
    /// # Errors
    /// A stale or externally changed binding.
    pub fn copy(&self, stage: &mut Stage) -> Result<Self, BubbleError> {
        self.validate(stage)?;
        let mapping = stage.copy_family_mapped(self.root)?;
        let remap = |mob| {
            mapping
                .get(mob)
                .expect("validated bubble component belongs to copied family")
        };
        Ok(Self {
            root: mapping.root(),
            body: remap(self.body),
            content: self.content.map(remap),
            members: self
                .members
                .iter()
                .map(|member| BodyMember {
                    mob: remap(member.mob),
                    children: member.children.iter().copied().map(remap).collect(),
                })
                .collect(),
            config: self.config.clone(),
            source: self.source.clone(),
            next_rng: self.next_rng.clone(),
        })
    }
}

fn build_body(
    source: &Body,
    bounds: BoundingBox,
    config: &BubbleConfig,
) -> Result<(VMobject, Option<Pcg64Dxsm>), BubbleError> {
    check_bounds(bounds)?;
    let width = bounds.width() + 2.0 * config.buff;
    let height = bounds.height() + 2.0 * config.buff;
    if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
        return Err(invalid(
            "body extent",
            "buffered content must have finite positive width and height",
        ));
    }
    // The drawing builders inspect bounds only. A valid diagonal path supplies
    // those exact live world bounds without copying caller content or styles.
    let proxy = VMobject::from_points(vec![bounds.min, bounds.mid, bounds.max]);
    let direction = config.direction.vector();
    let (group, next_rng) = match source {
        Body::Speech {
            stem_height,
            stem_top,
        } => {
            if !finite_point([
                bounds.min[0] - config.buff,
                bounds.min[1] - config.buff - stem_height * height,
                bounds.mid[2],
            ]) || !finite_point([
                bounds.max[0] + config.buff,
                bounds.max[1] + config.buff,
                bounds.mid[2],
            ]) {
                return Err(invalid(
                    "body extent",
                    "must fit finite record-space coordinates",
                ));
            }
            (
                speech_bubble(
                    Some(&proxy),
                    direction,
                    config.buff,
                    config.filler_shape,
                    *stem_height,
                    *stem_top,
                )?,
                None,
            )
        }
        Body::Thought { shape, origin } => {
            validate_thought(shape, width, height)?;
            let mut rng = origin.clone();
            let group = thought_bubble(
                Some(&proxy),
                direction,
                config.buff,
                config.filler_shape,
                shape,
                &mut rng,
            )?;
            (group, Some(rng))
        }
        Body::Svg(document) => (
            bubble_with(document, Some(&proxy), direction, config.buff)?,
            None,
        ),
    };
    let (_, mut children) = group.into_own_and_children();
    if children.len() != 2 {
        return Err(BubbleError::ChangedTopology);
    }
    let body = children.remove(0).map_style_deep(|_| config.style);
    let Some((min, max)) = body.extent() else {
        return Err(invalid("body geometry", "must contain a visible outline"));
    };
    if max[0] <= min[0] || max[1] <= min[1] {
        return Err(invalid(
            "body geometry",
            "must have positive width and height",
        ));
    }
    let mut pending = vec![&body];
    let mut count = 0usize;
    while let Some(member) = pending.pop() {
        count = count.saturating_add(member.points().len());
        if count > MAX_BUBBLE_POINTS {
            return Err(BubbleError::Limit {
                resource: "body points",
                requested: count,
                max: MAX_BUBBLE_POINTS,
            });
        }
        if member.points().iter().any(|point| !finite_point(*point)) {
            return Err(invalid(
                "body points",
                "must fit finite record-space coordinates",
            ));
        }
        member.path().map_err(BooleanMobjectError::from)?;
        pending.extend(member.children());
    }
    Ok((body, next_rng))
}

fn validate_thought(
    shape: &ThoughtBubbleShape,
    width: f64,
    height: f64,
) -> Result<(), BubbleError> {
    nonnegative(shape.bulge_radius, "bulge_radius")?;
    if shape.bulge_radius == 0.0 {
        return Err(invalid("bulge_radius", "must be positive"));
    }
    if !shape.bulge_overlap.is_finite() || !(0.0..1.0).contains(&shape.bulge_overlap) {
        return Err(invalid("bulge_overlap", "must be finite in 0..1"));
    }
    if !shape.noise_factor.is_finite() || !(0.0..=1.0).contains(&shape.noise_factor) {
        return Err(invalid("noise_factor", "must be finite in 0..=1"));
    }
    if shape.circle_radii.len() > MAX_BUBBLE_TRAIL_CIRCLES {
        return Err(BubbleError::Limit {
            resource: "thought trail circles",
            requested: shape.circle_radii.len(),
            max: MAX_BUBBLE_TRAIL_CIRCLES,
        });
    }
    for &radius in &shape.circle_radii {
        nonnegative(radius, "thought circle radius")?;
        if radius == 0.0 {
            return Err(invalid("thought circle radius", "must be positive"));
        }
    }
    let step = (1.0 - shape.bulge_overlap) * 2.0 * shape.bulge_radius;
    let count = 2.0 * ((width / step).floor() + 1.0) + 2.0 * ((height / step).floor() + 1.0);
    if !count.is_finite() || count > MAX_BUBBLE_BULGES as f64 {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        return Err(BubbleError::Limit {
            resource: "cloud bulges",
            requested: count as usize,
            max: MAX_BUBBLE_BULGES,
        });
    }
    Ok(())
}
