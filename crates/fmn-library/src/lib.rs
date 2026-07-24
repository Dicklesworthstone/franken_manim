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
//! Still to land here: coordinate systems and plotting (fm-v4l), the
//! de-TeX'd natives (fm-y69), 3D solids and fields (fm-2u6), the
//! enhanced graph and data mobjects (fm-n64), and the drawings shelf
//! (fm-3kr). The boolean-op mobjects (`Union`/`Difference`/`Intersection`/
//! `Exclusion`) wait on Chisel's boolean kernel (fm-8dx) and are tracked
//! by fm-6l6.
#![forbid(unsafe_code)]

pub mod arc;
pub mod line;
pub mod poly;
pub mod style;
pub mod tip;
pub mod vmobject;

pub use arc::{AnnularSector, Annulus, Arc, ArcBetweenPoints, Circle, Dot, Ellipse};
pub use line::{Arrow, DashedLine, Elbow, Line, StrokeArrow};
pub use poly::{ArrowTip, CubicBezier, Polygon, Rectangle, RegularPolygon, TipStyle};
pub use style::{Style, VStyle};
pub use tip::TipEnd;
pub use vmobject::VMobject;
