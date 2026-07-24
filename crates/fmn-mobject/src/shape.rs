//! Semantic shape tags: what a constructor built, retained as data (§10.8).
//!
//! Lumen's compiled render plan routes **primitive hints** off the general
//! quadratic solver — line/polyline to a capsule-distance kernel, circle
//! and arc to a direct arc kernel, rectangles to specialized coverage, a
//! dot to a radial kernel — and §10.8 requires that "mutation through
//! `set_points` or a writable live view invalidates the hint back to the
//! general path". A tag is therefore two things at once, and this module
//! keeps them apart:
//!
//! * **The class identity and its constructor configuration**, which is
//!   durable. `Line` keeps its `path_arc` for the rest of its life, because
//!   the Reference's `Line.get_arc_length` and `put_start_and_end_on` both
//!   consult it after arbitrary transforms.
//! * **The geometric payload** — a circle's centre and radius — which is
//!   true only while nothing has written the points since construction.
//!
//! [`Stage::shape`] returns the first; [`Stage::primitive_hint`] returns
//! the tag only when the second still holds. Validity is decided by
//! comparing the `point` field's revision against the one recorded when the
//! tag was set, so **every** channel that can write points invalidates the
//! hint automatically — a positional transform, a raw `RecordBuffer` write,
//! a writable live view, a resize, an alignment pass — with no dirty-flag
//! bookkeeping to get wrong. It is the same D-20 lazy-revisioned pattern
//! the bounding box uses.
//!
//! A transform that *preserves* the shape (a shift, a uniform scale) still
//! demotes the hint here. That is conservative, never wrong, and cheap to
//! improve later; carrying tags through shape-preserving transforms is a
//! W5 optimization (fm-o70), not a correctness matter.

use fmn_core::types::Vec3;

use crate::stage::{Mob, Stage};

/// The semantic shape a library constructor built.
///
/// Scribe and Reel add their own members when they land (§10.8 also lists
/// glyph instances and images); this enum is the geometry tier's share of
/// that vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum ShapeTag {
    /// No semantic shape: the general quadratic path.
    #[default]
    General,
    /// A `Line`-family segment. `path_arc` is the Reference's, and is
    /// durable configuration rather than geometry: it survives transforms
    /// and decides how the path is rebuilt when the ends move.
    Line {
        /// The start the path was built from.
        start: Vec3,
        /// The end the path was built from.
        end: Vec3,
        /// Subtended angle; `0` for a straight segment.
        path_arc: f64,
        /// Buffer trimmed from each end at construction.
        buff: f64,
    },
    /// A corner-joined run of straight segments (`Polyline`, `Polygon`).
    Polyline {
        /// Number of corner vertices in the run.
        vertices: usize,
        /// Whether the last vertex repeats the first (`Polygon` does).
        closed: bool,
    },
    /// An `Arc`-family partial arc.
    Arc {
        /// Centre of curvature.
        center: Vec3,
        /// Radius.
        radius: f64,
        /// Angle of the first anchor, measured from `+x`.
        start_angle: f64,
        /// Subtended angle, signed.
        angle: f64,
    },
    /// A full circle — an [`ShapeTag::Arc`] of `TAU`, tagged distinctly
    /// because it gets its own kernel.
    Circle {
        /// Centre.
        center: Vec3,
        /// Radius.
        radius: f64,
    },
    /// A filled dot: a circle whose kernel is radial rather than stroked.
    Dot {
        /// Centre.
        center: Vec3,
        /// Radius.
        radius: f64,
    },
    /// An axis-aligned rectangle.
    Rect {
        /// Centre.
        center: Vec3,
        /// Full width.
        width: f64,
        /// Full height.
        height: f64,
    },
    /// An axis-aligned rectangle with rounded corners.
    RoundedRect {
        /// Centre.
        center: Vec3,
        /// Full width.
        width: f64,
        /// Full height.
        height: f64,
        /// Corner radius.
        corner_radius: f64,
    },
}

impl ShapeTag {
    /// Whether this tag names a semantic shape at all.
    #[must_use]
    pub fn is_general(&self) -> bool {
        matches!(self, Self::General)
    }

    /// A short, stable name for logs, traces, and test messages.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Line { .. } => "line",
            Self::Polyline { .. } => "polyline",
            Self::Arc { .. } => "arc",
            Self::Circle { .. } => "circle",
            Self::Dot { .. } => "dot",
            Self::Rect { .. } => "rect",
            Self::RoundedRect { .. } => "rounded_rect",
        }
    }
}

/// A tag plus the point-field revision at which its geometry was true.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(crate) struct ShapeSlot {
    pub(crate) tag: ShapeTag,
    /// `None` for [`ShapeTag::General`], which has no geometry to stale.
    pub(crate) point_revision: Option<u64>,
}

impl Stage {
    /// The durable shape tag: the class identity and constructor
    /// configuration, readable for the entry's whole life.
    ///
    /// Geometric members of the payload (a centre, a radius) describe the
    /// points **only** while [`Stage::primitive_hint`] returns `Some`;
    /// non-geometric configuration (`Line`'s `path_arc`) is always valid.
    #[must_use]
    pub fn shape(&self, mob: Mob) -> ShapeTag {
        self.get(mob).map_or(ShapeTag::General, |e| e.shape().tag)
    }

    /// The render-side primitive hint (§10.8): the tag if and only if the
    /// points still are what the tag says they are, otherwise `None`,
    /// meaning the general path.
    #[must_use]
    pub fn primitive_hint(&self, mob: Mob) -> Option<ShapeTag> {
        let entry = self.get(mob)?;
        let slot = entry.shape();
        if slot.tag.is_general() {
            return None;
        }
        let current = entry.buffer.field_revision("point")?;
        (slot.point_revision == Some(current)).then_some(slot.tag)
    }

    /// Record the semantic shape of `mob`'s **current** points.
    ///
    /// Call this after the points are written; the current point revision
    /// is captured, so any later write demotes the hint.
    pub fn set_shape(&mut self, mob: Mob, tag: ShapeTag) -> &mut Self {
        if let Some(entry) = self.get_mut(mob) {
            let revision = entry.buffer.field_revision("point");
            entry.set_shape(ShapeSlot {
                tag,
                point_revision: revision,
            });
        }
        self
    }

    /// Drop any semantic shape, back to the general path.
    pub fn clear_shape(&mut self, mob: Mob) -> &mut Self {
        self.set_shape(mob, ShapeTag::General)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mobject::Mobject;

    fn square_points() -> Vec<Vec3> {
        vec![
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
            [-1.0, 1.0, 0.0],
            [-1.0, 0.0, 0.0],
            [-1.0, -1.0, 0.0],
        ]
    }

    #[test]
    fn an_untagged_mobject_has_no_hint() {
        let mut stage = Stage::new();
        let mob = stage.add(Mobject::from_points(&square_points()));
        assert_eq!(stage.shape(mob), ShapeTag::General);
        assert_eq!(stage.primitive_hint(mob), None);
    }

    #[test]
    fn a_tag_survives_reads_and_style_writes() {
        let mut stage = Stage::new();
        let mob = stage.add(Mobject::from_points(&square_points()));
        let tag = ShapeTag::Circle {
            center: [0.0; 3],
            radius: 1.0,
        };
        stage.set_shape(mob, tag);
        assert_eq!(stage.primitive_hint(mob), Some(tag));
        // A write to a *different* field leaves the hint alone: the point
        // revision is per-field, so recolouring a circle keeps its kernel.
        stage
            .get_mut(mob)
            .unwrap()
            .buffer
            .write(0, "rgba", &[1.0; 4]);
        assert_eq!(stage.primitive_hint(mob), Some(tag));
        // Reading the box does not disturb it either.
        let _ = stage.get_bounding_box(mob);
        assert_eq!(stage.primitive_hint(mob), Some(tag));
    }

    #[test]
    fn every_point_write_channel_demotes_the_hint() {
        let tag = ShapeTag::Dot {
            center: [0.0; 3],
            radius: 0.08,
        };

        // Channel 1: a positional transform.
        let mut stage = Stage::new();
        let mob = stage.add(Mobject::from_points(&square_points()));
        stage.set_shape(mob, tag);
        stage.shift(mob, [1.0, 0.0, 0.0]);
        assert_eq!(stage.primitive_hint(mob), None);
        // …but the durable tag is still readable.
        assert_eq!(stage.shape(mob), tag);

        // Channel 2: a raw record write.
        let mut stage = Stage::new();
        let mob = stage.add(Mobject::from_points(&square_points()));
        stage.set_shape(mob, tag);
        stage
            .get_mut(mob)
            .unwrap()
            .buffer
            .write(0, "point", &[9.0, 9.0, 9.0]);
        assert_eq!(stage.primitive_hint(mob), None);

        // Channel 3: set_points.
        let mut stage = Stage::new();
        let mob = stage.add(Mobject::from_points(&square_points()));
        stage.set_shape(mob, tag);
        stage.set_points(mob, &square_points()).unwrap();
        assert_eq!(stage.primitive_hint(mob), None);

        // Channel 4: a writable live view.
        let mut stage = Stage::new();
        let mob = stage.add(Mobject::from_points(&square_points()));
        stage.set_shape(mob, tag);
        let view = stage.get_mut(mob).unwrap().buffer.export_view(true);
        view.write(0, "point", &[3.0, 3.0, 3.0]);
        drop(view);
        assert_eq!(stage.primitive_hint(mob), None);
    }

    #[test]
    fn a_copy_carries_the_tag_and_its_validity() {
        let mut stage = Stage::new();
        let mob = stage.add(Mobject::from_points(&square_points()));
        let tag = ShapeTag::Rect {
            center: [0.0; 3],
            width: 2.0,
            height: 2.0,
        };
        stage.set_shape(mob, tag);
        let copy = stage.copy_family(mob).unwrap();
        assert_eq!(stage.shape(copy), tag);
        assert_eq!(
            stage.primitive_hint(copy),
            Some(tag),
            "a copy's points are the same points"
        );
        // Mutating the copy leaves the original's hint intact.
        stage.shift(copy, [0.0, 1.0, 0.0]);
        assert_eq!(stage.primitive_hint(copy), None);
        assert_eq!(stage.primitive_hint(mob), Some(tag));
    }

    #[test]
    fn snapshot_restore_round_trips_the_tag() {
        let mut stage = Stage::new();
        let mob = stage.add(Mobject::from_points(&square_points()));
        let tag = ShapeTag::Line {
            start: [-1.0, 0.0, 0.0],
            end: [1.0, 0.0, 0.0],
            path_arc: 0.5,
            buff: 0.0,
        };
        stage.set_shape(mob, tag);
        let snap = stage.snapshot();
        stage.clear_shape(mob);
        assert_eq!(stage.shape(mob), ShapeTag::General);
        stage.restore(&snap);
        assert_eq!(stage.shape(mob), tag);
        assert_eq!(stage.primitive_hint(mob), Some(tag));
    }

    #[test]
    fn clearing_is_explicit_too() {
        let mut stage = Stage::new();
        let mob = stage.add(Mobject::from_points(&square_points()));
        stage.set_shape(
            mob,
            ShapeTag::Circle {
                center: [0.0; 3],
                radius: 1.0,
            },
        );
        stage.clear_shape(mob);
        assert_eq!(stage.shape(mob), ShapeTag::General);
        assert_eq!(stage.primitive_hint(mob), None);
    }

    #[test]
    fn names_are_stable() {
        assert_eq!(ShapeTag::General.name(), "general");
        assert_eq!(
            ShapeTag::Arc {
                center: [0.0; 3],
                radius: 1.0,
                start_angle: 0.0,
                angle: 1.0
            }
            .name(),
            "arc"
        );
    }
}
