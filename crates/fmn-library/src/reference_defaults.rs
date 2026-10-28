//! The Reference's geometry defaults (§15.1, §16.2).
//!
//! GENERATED from API_SCHEMA.tsv + API_OVERLAY.tsv by
//! fmn_conformance::schema — regenerate, never hand-edit.
//! Reference pin: 6199a00d4c1b1127ebe45cb629c3f22538b10e13
//!
//! Regenerate:  UPDATE_API_ARTIFACTS=1 cargo test -p fmn-conformance \
//!                  --test api_schema
//!
//! One module per `manimlib.mobject.geometry` constructor, one constant per
//! parameter default, spelled as the schema records it. The builders read
//! these, and fmn-python's signature check holds the portal's defaults to
//! the same schema rows, so the two front doors cannot drift apart.

/// `DEFAULT_ARROW_TIP_LENGTH = 0.35`
pub const DEFAULT_ARROW_TIP_LENGTH: f64 = 0.35;
/// `DEFAULT_ARROW_TIP_WIDTH = 0.35`
pub const DEFAULT_ARROW_TIP_WIDTH: f64 = 0.35;
/// `DEFAULT_DASH_LENGTH = 0.05`
pub const DEFAULT_DASH_LENGTH: f64 = 0.05;
/// `DEFAULT_DOT_RADIUS = 0.08`
pub const DEFAULT_DOT_RADIUS: f64 = 0.08;
/// `DEFAULT_SMALL_DOT_RADIUS = 0.04`
pub const DEFAULT_SMALL_DOT_RADIUS: f64 = 0.04;

/// `AnnularSector.__init__`.
pub mod annular_sector {
    /// `angle=TAU / 4`
    pub const ANGLE: f64 = fmn_core::constants::TAU / 4.0;
    /// `start_angle=0.0`
    pub const START_ANGLE: f64 = 0.0;
    /// `inner_radius=1.0`
    pub const INNER_RADIUS: f64 = 1.0;
    /// `outer_radius=2.0`
    pub const OUTER_RADIUS: f64 = 2.0;
    /// `arc_center=ORIGIN`
    pub const ARC_CENTER: fmn_core::types::Vec3 = fmn_core::constants::ORIGIN;
    /// `fill_color=DEFAULT_LIGHT_COLOR`
    pub const FILL_COLOR: fmn_core::color::Srgb = fmn_core::constants::DEFAULT_LIGHT_COLOR;
    /// `fill_opacity=1.0`
    pub const FILL_OPACITY: f64 = 1.0;
    /// `stroke_width=0.0`
    pub const STROKE_WIDTH: f64 = 0.0;
}

/// `Annulus.__init__`.
pub mod annulus {
    /// `inner_radius=1.0`
    pub const INNER_RADIUS: f64 = 1.0;
    /// `outer_radius=2.0`
    pub const OUTER_RADIUS: f64 = 2.0;
    /// `fill_opacity=1.0`
    pub const FILL_OPACITY: f64 = 1.0;
    /// `stroke_width=0.0`
    pub const STROKE_WIDTH: f64 = 0.0;
    /// `fill_color=DEFAULT_LIGHT_COLOR`
    pub const FILL_COLOR: fmn_core::color::Srgb = fmn_core::constants::DEFAULT_LIGHT_COLOR;
    /// `center=ORIGIN`
    pub const CENTER: fmn_core::types::Vec3 = fmn_core::constants::ORIGIN;
}

/// `Arc.__init__`.
pub mod arc {
    /// `start_angle=0`
    pub const START_ANGLE: f64 = 0.0;
    /// `angle=TAU / 4`
    pub const ANGLE: f64 = fmn_core::constants::TAU / 4.0;
    /// `radius=1.0`
    pub const RADIUS: f64 = 1.0;
    /// `n_components=None`
    pub const N_COMPONENTS: Option<usize> = None;
    /// `arc_center=ORIGIN`
    pub const ARC_CENTER: fmn_core::types::Vec3 = fmn_core::constants::ORIGIN;
}

/// `ArcBetweenPoints.__init__`.
pub mod arc_between_points {
    /// `angle=TAU / 4`
    pub const ANGLE: f64 = fmn_core::constants::TAU / 4.0;
}

/// `Arrow.__init__`.
pub mod arrow {
    /// `start=LEFT`
    pub const START: fmn_core::types::Vec3 = fmn_core::constants::LEFT;
    /// `end=LEFT`
    pub const END: fmn_core::types::Vec3 = fmn_core::constants::LEFT;
    /// `buff=MED_SMALL_BUFF`
    pub const BUFF: f64 = fmn_core::constants::MED_SMALL_BUFF;
    /// `path_arc=0`
    pub const PATH_ARC: f64 = 0.0;
    /// `fill_color=DEFAULT_LIGHT_COLOR`
    pub const FILL_COLOR: fmn_core::color::Srgb = fmn_core::constants::DEFAULT_LIGHT_COLOR;
    /// `fill_opacity=1.0`
    pub const FILL_OPACITY: f64 = 1.0;
    /// `stroke_width=0.0`
    pub const STROKE_WIDTH: f64 = 0.0;
    /// `thickness=3.0`
    pub const THICKNESS: f64 = 3.0;
    /// `tip_width_ratio=5`
    pub const TIP_WIDTH_RATIO: f64 = 5.0;
    /// `tip_angle=PI / 3`
    pub const TIP_ANGLE: f64 = fmn_core::constants::PI / 3.0;
    /// `max_tip_length_to_length_ratio=0.5`
    pub const MAX_TIP_LENGTH_TO_LENGTH_RATIO: f64 = 0.5;
    /// `max_width_to_length_ratio=0.1`
    pub const MAX_WIDTH_TO_LENGTH_RATIO: f64 = 0.1;
}

/// `ArrowTip.__init__`.
pub mod arrow_tip {
    /// `angle=0`
    pub const ANGLE: f64 = 0.0;
    /// `width=DEFAULT_ARROW_TIP_WIDTH`
    pub const WIDTH: f64 = super::DEFAULT_ARROW_TIP_WIDTH;
    /// `length=DEFAULT_ARROW_TIP_LENGTH`
    pub const LENGTH: f64 = super::DEFAULT_ARROW_TIP_LENGTH;
    /// `fill_opacity=1.0`
    pub const FILL_OPACITY: f64 = 1.0;
    /// `fill_color=DEFAULT_MOBJECT_COLOR`
    pub const FILL_COLOR: fmn_core::color::Srgb = fmn_core::constants::DEFAULT_MOBJECT_COLOR;
    /// `stroke_width=0.0`
    pub const STROKE_WIDTH: f64 = 0.0;
    /// `tip_style=0`
    pub const TIP_STYLE: usize = 0;
}

/// `Circle.__init__`.
pub mod circle {
    /// `start_angle=0`
    pub const START_ANGLE: f64 = 0.0;
    /// `stroke_color=RED`
    pub const STROKE_COLOR: fmn_core::color::Srgb = fmn_core::constants::RED;
}

/// `DashedLine.__init__`.
pub mod dashed_line {
    /// `start=LEFT`
    pub const START: fmn_core::types::Vec3 = fmn_core::constants::LEFT;
    /// `end=RIGHT`
    pub const END: fmn_core::types::Vec3 = fmn_core::constants::RIGHT;
    /// `dash_length=DEFAULT_DASH_LENGTH`
    pub const DASH_LENGTH: f64 = super::DEFAULT_DASH_LENGTH;
    /// `positive_space_ratio=0.5`
    pub const POSITIVE_SPACE_RATIO: f64 = 0.5;
}

/// `Dot.__init__`.
pub mod dot {
    /// `point=ORIGIN`
    pub const POINT: fmn_core::types::Vec3 = fmn_core::constants::ORIGIN;
    /// `radius=DEFAULT_DOT_RADIUS`
    pub const RADIUS: f64 = super::DEFAULT_DOT_RADIUS;
    /// `stroke_color=BLACK`
    pub const STROKE_COLOR: fmn_core::color::Srgb = fmn_core::constants::BLACK;
    /// `stroke_width=0.0`
    pub const STROKE_WIDTH: f64 = 0.0;
    /// `fill_opacity=1.0`
    pub const FILL_OPACITY: f64 = 1.0;
    /// `fill_color=DEFAULT_MOBJECT_COLOR`
    pub const FILL_COLOR: fmn_core::color::Srgb = fmn_core::constants::DEFAULT_MOBJECT_COLOR;
}

/// `Elbow.__init__`.
pub mod elbow {
    /// `width=0.2`
    pub const WIDTH: f64 = 0.2;
    /// `angle=0`
    pub const ANGLE: f64 = 0.0;
}

/// `Ellipse.__init__`.
pub mod ellipse {
    /// `width=2.0`
    pub const WIDTH: f64 = 2.0;
    /// `height=1.0`
    pub const HEIGHT: f64 = 1.0;
}

/// `Line.__init__`.
pub mod line {
    /// `start=LEFT`
    pub const START: fmn_core::types::Vec3 = fmn_core::constants::LEFT;
    /// `end=RIGHT`
    pub const END: fmn_core::types::Vec3 = fmn_core::constants::RIGHT;
    /// `buff=0.0`
    pub const BUFF: f64 = 0.0;
    /// `path_arc=0.0`
    pub const PATH_ARC: f64 = 0.0;
}

/// `Rectangle.__init__`.
pub mod rectangle {
    /// `width=4.0`
    pub const WIDTH: f64 = 4.0;
    /// `height=2.0`
    pub const HEIGHT: f64 = 2.0;
}

/// `RegularPolygon.__init__`.
pub mod regular_polygon {
    /// `n=6`
    pub const N: usize = 6;
    /// `radius=1.0`
    pub const RADIUS: f64 = 1.0;
    /// `start_angle=None`
    pub const START_ANGLE: Option<f64> = None;
}

/// `RoundedRectangle.__init__`.
pub mod rounded_rectangle {
    /// `width=4.0`
    pub const WIDTH: f64 = 4.0;
    /// `height=2.0`
    pub const HEIGHT: f64 = 2.0;
    /// `corner_radius=0.5`
    pub const CORNER_RADIUS: f64 = 0.5;
}

/// `Sector.__init__`.
pub mod sector {
    /// `angle=TAU / 4`
    pub const ANGLE: f64 = fmn_core::constants::TAU / 4.0;
    /// `radius=1.0`
    pub const RADIUS: f64 = 1.0;
}

/// `SmallDot.__init__`.
pub mod small_dot {
    /// `point=ORIGIN`
    pub const POINT: fmn_core::types::Vec3 = fmn_core::constants::ORIGIN;
    /// `radius=DEFAULT_SMALL_DOT_RADIUS`
    pub const RADIUS: f64 = super::DEFAULT_SMALL_DOT_RADIUS;
}

/// `Square.__init__`.
pub mod square {
    /// `side_length=2.0`
    pub const SIDE_LENGTH: f64 = 2.0;
}

/// `StrokeArrow.__init__`.
pub mod stroke_arrow {
    /// `stroke_color=DEFAULT_LIGHT_COLOR`
    pub const STROKE_COLOR: fmn_core::color::Srgb = fmn_core::constants::DEFAULT_LIGHT_COLOR;
    /// `stroke_width=5`
    pub const STROKE_WIDTH: f64 = 5.0;
    /// `buff=0.25`
    pub const BUFF: f64 = 0.25;
    /// `tip_width_ratio=5`
    pub const TIP_WIDTH_RATIO: f64 = 5.0;
    /// `tip_len_to_width=0.0075`
    pub const TIP_LEN_TO_WIDTH: f64 = 0.0075;
    /// `max_tip_length_to_length_ratio=0.3`
    pub const MAX_TIP_LENGTH_TO_LENGTH_RATIO: f64 = 0.3;
    /// `max_width_to_length_ratio=8.0`
    pub const MAX_WIDTH_TO_LENGTH_RATIO: f64 = 8.0;
}

/// `TangentLine.__init__`.
pub mod tangent_line {
    /// `length=2`
    pub const LENGTH: f64 = 2.0;
    /// `d_alpha=1e-06`
    pub const D_ALPHA: f64 = 1e-06;
}

/// `Vector.__init__`.
pub mod vector {
    /// `direction=RIGHT`
    pub const DIRECTION: fmn_core::types::Vec3 = fmn_core::constants::RIGHT;
    /// `buff=0.0`
    pub const BUFF: f64 = 0.0;
}
