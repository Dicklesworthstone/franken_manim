//! Menagerie + Atlas: the 161-class mobject library, coordinate systems, fields, 3D solids (§12).
//!
//! The library tier is thin compositions over Marionette (the arena and
//! its records), Chisel (paths, true arc length, space ops), and Scribe
//! (text and mathematics). Every class here is a **value**: chained
//! by-value setters producing a builder that `Stage::add` moves into the
//! arena, the §15.1 surface G0-1 ratified.
//!
//! Landed (fm-oab, §12.1): the vectorized base and its variants
//! ([`vmobject`]), the style surface ([`style`]), the Arc lineage
//! ([`arc`]), the Line lineage with the tip-attachment algebra
//! ([`line`], [`tip`]), and polygons, rectangles, arrow tips, and the
//! frame rectangles ([`poly`]).
//!
//! Three properties hold across the whole tier and are tested as such:
//!
//! * **One arc-density rule** (BN-09). Every arc, wherever it is built,
//!   uses `max(1, ceil(16·|θ|/TAU))` components; the Reference's three
//!   inconsistent conventions are gone.
//! * **True arc length everywhere** (BN-03). Buffers, dashes, tips, and
//!   tangent proportions all measure along the actual curve, not along a
//!   chord or a curve index.
//! * **Semantic shape tags** (§10.8). Constructors record what they built
//!   ([`fmn_mobject::ShapeTag`]) so Lumen can route a circle to the arc
//!   kernel; any write to the points demotes the hint automatically.
//!
//! * **De-TeX'd natives** (BN-08, §11.6/§12.3). Classes the Reference routes
//!   through LaTeX for want of anything better are built natively here:
//!   [`brace`] is a parametric path family that is correct at any width
//!   rather than one glyph stretched, [`matchers`] carries the shape
//!   matchers plus the two `pifont` marks as drawn paths, [`numbers`] is
//!   `DecimalNumber`/`Integer` with glyph-recycling updates, [`matrix`]
//!   gets its brackets from fmd-math's extensible-delimiter engine,
//!   [`special_tex`] is `BulletedList`/`Title` composed on Scribe, and
//!   [`controls`] carries the `interactive.py` control compositions (event
//!   wiring is Proscenium's, W9).
//!
//! * **The Scribe bridge** (fm-p5d, §11.2–11.5). [`text`] turns a
//!   `fmn_text::TextLayout` into a `VMobject` family — one child per glyph,
//!   the `Text[a:b]` / `isolate=` submobject contract intact — and [`tex`]
//!   turns a `fmn_tex::Typeset` into one child per `Sub` with the span map
//!   intact. `Text`, `MarkupText`, `Tex`, and `TexText` are the first
//!   text-bearing mobjects; scale is calibrated the Reference's way (a "0"
//!   stands `font_size / font_size_for_unit_height` units tall).
//!
//! * **Atlas** (fm-v4l, §12.2). [`coords`] owns `CoordinateSystem`, axes,
//!   number lines, Riemann rectangles, and area helpers; [`planes`] owns the
//!   2D/3D plane families; [`graphs`] owns parametric, explicit, and implicit
//!   curves over Chisel's bounded isoline extractor.
//!
//! Still to land here: 3D solids and fields (fm-2u6), the enhanced graph and
//! data mobjects (fm-n64), and the drawings shelf (fm-3kr). The boolean-op mobjects
//! (`Union`/`Difference`/`Intersection`/`Exclusion`) wait on Chisel's
//! boolean kernel (fm-8dx) and are tracked by fm-6l6.
#![forbid(unsafe_code)]

pub mod arc;
pub mod brace;
pub mod controls;
pub mod coords;
pub mod graphs;
pub mod line;
pub mod matchers;
pub mod matrix;
pub mod numbers;
pub mod planes;
pub mod poly;
pub mod special_tex;
pub mod style;
pub mod tex;
pub mod text;
pub mod tip;
pub mod vmobject;

pub use arc::{AnnularSector, Annulus, Arc, ArcBetweenPoints, Circle, Dot, Ellipse};
pub use brace::{Brace, BraceLabel, line_brace};
pub use controls::{
    Button, Checkbox, ColorSliders, ControlMob, ControlMobject, ControlPanel, ControlPanelMobject,
    EnableDisableButton, LinearNumberSlider, MotionMobject, ScalarControl, SliderError, Textbox,
    add_scalar_control,
};
pub use coords::{
    Axes, AxisConfig, CoordinateSystem, CoordsError, NumberLine, RiemannConfig, Slider,
    UnitInterval, create_axis,
};
pub use graphs::{
    DEFAULT_MAX_SAMPLES, FunctionGraph, GraphError, ImplicitFunction, ParametricCurve,
    SamplingBudget, SamplingError,
};
pub use line::{Arrow, DashedLine, Elbow, Line, StrokeArrow};
pub use matchers::{
    SurroundingRectangle, background_rectangle, checkmark, cross, exmark, underline,
};
pub use matrix::{
    DEFAULT_MAX_MATRIX_ENTRIES, DecimalMatrix, IntegerMatrix, Matrix, MatrixError, MatrixMobject,
    MobjectMatrix, TexMatrix,
};
pub use numbers::{DEFAULT_MAX_NUMBER_CHARACTERS, DecimalNumber, Integer};
pub use planes::{ComplexPlane, NumberPlane, ThreeDAxes};
pub use poly::{ArrowTip, CubicBezier, Polygon, Rectangle, RegularPolygon, TipStyle};
pub use special_tex::{BulletedList, BulletedListMobject, Title, TitleMobject};
pub use style::{Style, VStyle};
pub use tex::{Tex, TexMobject, TexMobjectError, TexText};
pub use text::{MarkupText, Text, TextMobject, TextMobjectError};
pub use tip::TipEnd;
pub use vmobject::{DashError, MAX_DASHES, VMobject};
