//! The VMobject style surface: colour, opacity, and width as record data.
//!
//! The Reference keeps style in the per-record columns `stroke_rgba`,
//! `stroke_width`, `fill_rgba`, and `fill_border_width`
//! (`VMobject.data_dtype`), which is why a gradient is just a column of
//! different rgba values and why `set_fill` is a write across every record
//! of every family member. That is ported here verbatim: [`Style`] is the
//! constructor-time configuration, and the [`Stage`] setters are the
//! runtime surface.
//!
//! Two behaviours worth stating, both the Reference's:
//!
//! * **`color` overrides both.** `VMobject.__init__` resolves
//!   `fill_color or color or DEFAULT`, so passing `color` sets stroke and
//!   fill together while an explicit `stroke_color`/`fill_color` wins over
//!   it.
//! * **Colour and opacity are independent writes.** `set_fill(color=None,
//!   opacity=0.5)` changes only the alpha lane, leaving the rgb alone —
//!   which is what every fade animation depends on.
//!
//! Styling a **point-less** mobject is remembered until it gains points,
//! as the Reference's one-record `_data_defaults` does (fm-sjl): the write
//! lands in the buffer's phantom default record, which growth from zero
//! adopts. Getters observe that same record without creating geometry or
//! inheriting a child's paint. The column writes themselves are Marionette's
//! (`RecordBuffer::write_color_lanes` / `write_scalar_lanes`), shared with
//! the `.animate` builder's style commands.

use fmn_core::color::Srgb;
use fmn_core::constants::{
    DEFAULT_STROKE_WIDTH, DEFAULT_VMOBJECT_FILL_COLOR, DEFAULT_VMOBJECT_STROKE_COLOR,
};
use fmn_mobject::{Mob, RecordBuffer, Stage};

/// Constructor-time style, matching `VMobject.__init__`'s colour surface.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Style {
    /// Fill colour (`fill_color`).
    pub fill_color: Srgb,
    /// Fill alpha (`fill_opacity`) — `0.0` by default: a bare VMobject is
    /// an outline.
    pub fill_opacity: f64,
    /// Stroke colour (`stroke_color`).
    pub stroke_color: Srgb,
    /// Stroke alpha (`stroke_opacity`).
    pub stroke_opacity: f64,
    /// Stroke width in the Reference's units (`stroke_width`).
    pub stroke_width: f64,
    /// Inner fill border width (`fill_border_width`).
    pub fill_border_width: f64,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            fill_color: DEFAULT_VMOBJECT_FILL_COLOR,
            fill_opacity: 0.0,
            stroke_color: DEFAULT_VMOBJECT_STROKE_COLOR,
            stroke_opacity: 1.0,
            stroke_width: DEFAULT_STROKE_WIDTH,
            fill_border_width: 0.0,
        }
    }
}

impl Style {
    /// The Reference's `color=` argument: set stroke and fill together.
    #[must_use]
    pub fn color(mut self, color: Srgb) -> Self {
        self.fill_color = color;
        self.stroke_color = color;
        self
    }

    /// Set the fill colour and alpha.
    #[must_use]
    pub fn fill(mut self, color: Srgb, opacity: f64) -> Self {
        self.fill_color = color;
        self.fill_opacity = opacity;
        self
    }

    /// Set the stroke colour, width, and alpha.
    #[must_use]
    pub fn stroke(mut self, color: Srgb, width: f64, opacity: f64) -> Self {
        self.stroke_color = color;
        self.stroke_width = width;
        self.stroke_opacity = opacity;
        self
    }

    /// Set the stroke width alone.
    #[must_use]
    pub fn stroke_width(mut self, width: f64) -> Self {
        self.stroke_width = width;
        self
    }

    /// Set the fill alpha alone.
    #[must_use]
    pub fn fill_opacity(mut self, opacity: f64) -> Self {
        self.fill_opacity = opacity;
        self
    }

    /// Set the inner fill border width.
    #[must_use]
    pub fn fill_border_width(mut self, width: f64) -> Self {
        self.fill_border_width = width;
        self
    }

    /// Write this style across every record of `buffer` — the
    /// constructor path, where the points are already in place.
    pub fn write(&self, buffer: &mut RecordBuffer) {
        write_rgba(
            buffer,
            "stroke_rgba",
            Some(self.stroke_color),
            Some(self.stroke_opacity),
        );
        write_rgba(
            buffer,
            "fill_rgba",
            Some(self.fill_color),
            Some(self.fill_opacity),
        );
        write_scalar(buffer, "stroke_width", self.stroke_width);
        write_scalar(buffer, "fill_border_width", self.fill_border_width);
    }
}

/// One colour column's rgb and/or alpha lanes, through Marionette's record
/// primitive (which also carries the fm-sjl point-less default-record path).
fn write_rgba(buffer: &mut RecordBuffer, field: &str, color: Option<Srgb>, opacity: Option<f64>) {
    buffer.write_color_lanes(field, color.map(|c| [c.r, c.g, c.b]), opacity);
}

fn write_scalar(buffer: &mut RecordBuffer, field: &str, value: f64) {
    buffer.write_scalar_lanes(field, value);
}

/// The runtime style surface, as an extension trait because [`Stage`]
/// belongs to Marionette and style belongs to the library tier (§19).
///
/// Every setter takes `recurse`, exactly as the Reference's do, and
/// recursing means "the whole family", not "the direct children".
/// Getters read the first actual record, or this object's own phantom
/// default record when point-free. Missing or incompatible fields return None.
pub trait VStyle {
    /// Reference `set_fill`: colour and/or opacity, plus the inner border
    /// width. `None` leaves that aspect untouched.
    fn set_fill(
        &mut self,
        mob: Mob,
        color: Option<Srgb>,
        opacity: Option<f64>,
        border_width: Option<f64>,
        recurse: bool,
    ) -> &mut Self;

    /// Reference `set_stroke`: colour, width, and/or opacity, plus the
    /// `behind` uniform. `None` leaves that aspect untouched.
    fn set_stroke(
        &mut self,
        mob: Mob,
        color: Option<Srgb>,
        width: Option<f64>,
        opacity: Option<f64>,
        behind: Option<bool>,
        recurse: bool,
    ) -> &mut Self;

    /// Reference `set_color`: stroke and fill colour together, opacity
    /// optional.
    fn set_color(
        &mut self,
        mob: Mob,
        color: Srgb,
        opacity: Option<f64>,
        recurse: bool,
    ) -> &mut Self;

    /// Reference `set_opacity`: both alphas.
    fn set_opacity(&mut self, mob: Mob, opacity: f64, recurse: bool) -> &mut Self;

    /// Reference `set_backstroke`: a stroke drawn behind the fill.
    fn set_backstroke(&mut self, mob: Mob, color: Srgb, width: f64) -> &mut Self;

    /// Reference `get_fill_color`: the first record's fill rgb.
    fn get_fill_color(&self, mob: Mob) -> Option<Srgb>;
    /// Reference `get_fill_opacity`: the first record's fill alpha.
    fn get_fill_opacity(&self, mob: Mob) -> Option<f64>;
    /// Reference `get_stroke_color`: the first record's stroke rgb.
    fn get_stroke_color(&self, mob: Mob) -> Option<Srgb>;
    /// Reference `get_stroke_opacity`: the first record's stroke alpha.
    fn get_stroke_opacity(&self, mob: Mob) -> Option<f64>;
    /// Reference `get_stroke_width`: the first record's stroke width.
    fn get_stroke_width(&self, mob: Mob) -> Option<f64>;
    /// Reference `get_color`: the fill colour if there is fill, else the
    /// stroke colour.
    fn get_color(&self, mob: Mob) -> Option<Srgb>;
    /// Reference `has_fill`: any record with a nonzero fill alpha.
    /// A point-free object observes its own default record.
    fn has_fill(&self, mob: Mob) -> bool;
    /// Reference `has_stroke`: nonzero width and alpha occur in the columns.
    /// This is conservative because continuous interpolation can overlap them
    /// between records. A point-free object observes its own default record.
    fn has_stroke(&self, mob: Mob) -> bool;
}

fn family_of(stage: &Stage, mob: Mob, recurse: bool) -> Vec<Mob> {
    if recurse {
        stage.family(mob)
    } else {
        vec![mob]
    }
}

fn first_style_record<const N: usize>(stage: &Stage, mob: Mob, field: &str) -> Option<[f32; N]> {
    let buffer = &stage.get(mob)?.buffer;
    if buffer.schema().field_width(field) != Some(N) {
        return None;
    }
    let values = if buffer.is_empty() {
        buffer.default_record(field)?
    } else {
        buffer.read(0, field)?
    };
    values.try_into().ok()
}

fn first_rgba(stage: &Stage, mob: Mob, field: &str) -> Option<[f32; 4]> {
    first_style_record(stage, mob, field)
}

impl VStyle for Stage {
    fn set_fill(
        &mut self,
        mob: Mob,
        color: Option<Srgb>,
        opacity: Option<f64>,
        border_width: Option<f64>,
        recurse: bool,
    ) -> &mut Self {
        for m in family_of(self, mob, recurse) {
            let Some(entry) = self.get_mut(m) else {
                continue;
            };
            write_rgba(&mut entry.buffer, "fill_rgba", color, opacity);
            if let Some(w) = border_width {
                write_scalar(&mut entry.buffer, "fill_border_width", w);
            }
        }
        self
    }

    fn set_stroke(
        &mut self,
        mob: Mob,
        color: Option<Srgb>,
        width: Option<f64>,
        opacity: Option<f64>,
        behind: Option<bool>,
        recurse: bool,
    ) -> &mut Self {
        for m in family_of(self, mob, recurse) {
            let Some(entry) = self.get_mut(m) else {
                continue;
            };
            write_rgba(&mut entry.buffer, "stroke_rgba", color, opacity);
            if let Some(w) = width {
                write_scalar(&mut entry.buffer, "stroke_width", w);
            }
            if let Some(b) = behind {
                entry.uniforms_mut().stroke_behind = b;
            }
        }
        self
    }

    fn set_color(
        &mut self,
        mob: Mob,
        color: Srgb,
        opacity: Option<f64>,
        recurse: bool,
    ) -> &mut Self {
        self.set_fill(mob, Some(color), opacity, None, recurse);
        self.set_stroke(mob, Some(color), None, opacity, None, recurse)
    }

    fn set_opacity(&mut self, mob: Mob, opacity: f64, recurse: bool) -> &mut Self {
        self.set_fill(mob, None, Some(opacity), None, recurse);
        self.set_stroke(mob, None, None, Some(opacity), None, recurse)
    }

    fn set_backstroke(&mut self, mob: Mob, color: Srgb, width: f64) -> &mut Self {
        self.set_stroke(mob, Some(color), Some(width), None, Some(true), true)
    }

    fn get_fill_color(&self, mob: Mob) -> Option<Srgb> {
        let rgba = first_rgba(self, mob, "fill_rgba")?;
        Some(Srgb {
            r: f64::from(rgba[0]),
            g: f64::from(rgba[1]),
            b: f64::from(rgba[2]),
        })
    }

    fn get_fill_opacity(&self, mob: Mob) -> Option<f64> {
        Some(f64::from(first_rgba(self, mob, "fill_rgba")?[3]))
    }

    fn get_stroke_color(&self, mob: Mob) -> Option<Srgb> {
        let rgba = first_rgba(self, mob, "stroke_rgba")?;
        Some(Srgb {
            r: f64::from(rgba[0]),
            g: f64::from(rgba[1]),
            b: f64::from(rgba[2]),
        })
    }

    fn get_stroke_opacity(&self, mob: Mob) -> Option<f64> {
        Some(f64::from(first_rgba(self, mob, "stroke_rgba")?[3]))
    }

    fn get_stroke_width(&self, mob: Mob) -> Option<f64> {
        Some(f64::from(first_style_record::<1>(self, mob, "stroke_width")?[0]))
    }

    fn get_color(&self, mob: Mob) -> Option<Srgb> {
        if self.has_fill(mob) {
            self.get_fill_color(mob)
        } else {
            self.get_stroke_color(mob)
        }
    }

    fn has_fill(&self, mob: Mob) -> bool {
        let Some(entry) = self.get(mob) else {
            return false;
        };
        if entry.buffer.schema().field_width("fill_rgba") != Some(4) {
            return false;
        }
        if entry.buffer.is_empty() {
            return first_rgba(self, mob, "fill_rgba").is_some_and(|rgba| rgba[3] > 0.0);
        }
        entry
            .buffer
            .read_column("fill_rgba")
            .is_some_and(|c| c.as_chunks::<4>().0.iter().any(|rgba| rgba[3] > 0.0))
    }

    fn has_stroke(&self, mob: Mob) -> bool {
        let Some(entry) = self.get(mob) else {
            return false;
        };
        if entry.buffer.schema().field_width("stroke_rgba") != Some(4)
            || entry.buffer.schema().field_width("stroke_width") != Some(1)
        {
            return false;
        }
        if entry.buffer.is_empty() {
            return first_rgba(self, mob, "stroke_rgba").is_some_and(|rgba| rgba[3] != 0.0)
                && self.get_stroke_width(mob).is_some_and(|width| width != 0.0);
        }
        let Some(rgba) = entry.buffer.read_column("stroke_rgba") else {
            return false;
        };
        let Some(widths) = entry.buffer.read_column("stroke_width") else {
            return false;
        };
        // Opposite endpoint profiles can overlap inside an interpolated curve.
        // Do not turn this conservative admission into a point-sample test.
        widths.iter().any(|width| *width != 0.0)
            && rgba.as_chunks::<4>().0.iter().any(|rgba| rgba[3] != 0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmn_core::constants::{BLUE, RED, WHITE};

    use crate::vmobject::VMobject;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    /// Colours round-trip through f32 records (§6.1), so they compare at
    /// f32 tolerance, never bit-for-bit.
    #[track_caller]
    fn assert_color(actual: Option<Srgb>, expected: Srgb) {
        let a = actual.expect("mobject has records");
        assert!(
            close(a.r, expected.r) && close(a.g, expected.g) && close(a.b, expected.b),
            "{a:?} vs {expected:?}"
        );
    }

    #[test]
    fn defaults_match_the_reference() {
        let s = Style::default();
        assert_eq!(s.fill_opacity, 0.0, "a bare VMobject is an outline");
        assert_eq!(s.stroke_opacity, 1.0);
        assert_eq!(s.stroke_width, DEFAULT_STROKE_WIDTH);
        assert_eq!(s.stroke_color, DEFAULT_VMOBJECT_STROKE_COLOR);
        assert_eq!(s.fill_color, DEFAULT_VMOBJECT_FILL_COLOR);
    }

    #[test]
    fn color_sets_both_stroke_and_fill() {
        let s = Style::default().color(RED);
        assert_eq!(s.fill_color, RED);
        assert_eq!(s.stroke_color, RED);
    }

    #[test]
    fn constructor_style_reaches_every_record() {
        let mut stage = Stage::new();
        let mob = stage.add(
            VMobject::from_points(vec![[0.0; 3], [1.0, 1.0, 0.0], [2.0, 0.0, 0.0]])
                .with_style(Style::default().fill(BLUE, 0.5).stroke(RED, 7.0, 1.0)),
        );
        assert_color(stage.get_fill_color(mob), BLUE);
        assert!(close(stage.get_fill_opacity(mob).unwrap(), 0.5));
        assert_color(stage.get_stroke_color(mob), RED);
        assert!(close(stage.get_stroke_width(mob).unwrap(), 7.0));
        // Every record, not just the first.
        let widths = stage
            .get(mob)
            .unwrap()
            .buffer
            .read_column("stroke_width")
            .unwrap();
        assert_eq!(widths.len(), 3);
        assert!(widths.iter().all(|w| close(f64::from(*w), 7.0)));
    }

    #[test]
    fn colour_and_opacity_are_independent_writes() {
        let mut stage = Stage::new();
        let mob = stage.add(
            VMobject::from_points(vec![[0.0; 3], [1.0, 0.0, 0.0], [2.0, 0.0, 0.0]])
                .with_style(Style::default().fill(BLUE, 1.0)),
        );
        stage.set_fill(mob, None, Some(0.25), None, true);
        assert_color(stage.get_fill_color(mob), BLUE);
        assert!(close(stage.get_fill_opacity(mob).unwrap(), 0.25));
        stage.set_fill(mob, Some(RED), None, None, true);
        assert_color(stage.get_fill_color(mob), RED);
        assert!(
            close(stage.get_fill_opacity(mob).unwrap(), 0.25),
            "alpha kept"
        );
    }

    #[test]
    fn setters_recurse_over_the_family_or_not() {
        let mut stage = Stage::new();
        let group = stage.add(
            VMobject::new()
                .with_child(VMobject::from_points(vec![
                    [0.0; 3],
                    [1.0, 0.0, 0.0],
                    [2.0, 0.0, 0.0],
                ]))
                .with_child(VMobject::from_points(vec![
                    [0.0; 3],
                    [0.0, 1.0, 0.0],
                    [0.0, 2.0, 0.0],
                ])),
        );
        let children = stage.get(group).unwrap().submobjects().to_vec();
        stage.set_color(group, RED, None, true);
        for child in &children {
            assert_color(stage.get_stroke_color(*child), RED);
        }
        // recurse = false touches only the named mobject, which here has no
        // records at all, so the children keep their colour.
        stage.set_color(group, BLUE, None, false);
        for child in &children {
            assert_color(stage.get_stroke_color(*child), RED);
        }
    }

    #[test]
    fn styling_a_pointless_entry_is_remembered_by_growth() {
        let mut stage = Stage::new();
        let mob = stage.add(VMobject::new());
        assert_eq!(stage.get(mob).unwrap().buffer.len(), 0);

        // The Reference's mobject.py:1329 path: the write lands in
        // `_data_defaults`, not in any row.
        stage.set_fill(mob, Some(BLUE), Some(0.5), None, false);
        stage.set_stroke(mob, Some(RED), Some(9.0), None, None, false);
        assert_eq!(stage.get(mob).unwrap().buffer.len(), 0);

        // `resize_points` growing from zero adopts the defaults wholesale.
        stage
            .get_mut(mob)
            .unwrap()
            .buffer
            .resize(4)
            .expect("growth within allocation limits");
        let fills = stage
            .get(mob)
            .unwrap()
            .buffer
            .read_column("fill_rgba")
            .unwrap();
        for rgba in fills.as_chunks::<4>().0 {
            assert!(close(f64::from(rgba[0]), BLUE.r));
            assert!(close(f64::from(rgba[3]), 0.5));
        }
        let widths = stage
            .get(mob)
            .unwrap()
            .buffer
            .read_column("stroke_width")
            .unwrap();
        assert!(widths.iter().all(|w| close(f64::from(*w), 9.0)));
    }

    #[test]
    fn copies_carry_the_defaults() {
        let mut stage = Stage::new();
        let mob = stage.add(VMobject::new());
        stage.set_color(mob, RED, Some(1.0), false);

        // `copy()` semantics: an independent buffer that still remembers.
        let copied = stage.get(mob).unwrap().buffer.deep_clone();
        let mut grown = copied;
        grown.resize(2).expect("growth within allocation limits");
        for rgba in grown.read_column("fill_rgba").unwrap().as_chunks::<4>().0 {
            assert!(close(f64::from(rgba[0]), RED.r));
        }

        // The saved-state snapshot clone carries them too (§8.3).
        let snapshotted = stage.get(mob).unwrap().buffer.snapshot_clone();
        let mut grown = snapshotted;
        grown.resize(3).expect("growth within allocation limits");
        let strokes = grown.read_column("stroke_rgba").unwrap();
        assert!(!strokes.as_chunks::<4>().0.is_empty());
        for rgba in strokes.as_chunks::<4>().0 {
            assert!(close(f64::from(rgba[3]), 1.0));
        }
    }

    #[test]
    fn vgroup_styled_before_children_gain_geometry() {
        let mut stage = Stage::new();
        let group = stage.add(
            VMobject::new()
                .with_child(VMobject::new())
                .with_child(VMobject::new()),
        );
        let children = stage.get(group).unwrap().submobjects().to_vec();
        stage.set_color(group, BLUE, None, true);

        for child in &children {
            stage
                .get_mut(*child)
                .unwrap()
                .buffer
                .resize(3)
                .expect("growth within allocation limits");
            assert_color(stage.get_stroke_color(*child), BLUE);
            assert_color(stage.get_fill_color(*child), BLUE);
        }
    }

    #[test]
    fn has_fill_and_has_stroke_read_the_records() {
        let mut stage = Stage::new();
        let outline = stage.add(VMobject::from_points(vec![
            [0.0; 3],
            [1.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
        ]));
        assert!(!stage.has_fill(outline), "default fill opacity is zero");
        assert!(stage.has_stroke(outline));
        assert_eq!(stage.get_color(outline), stage.get_stroke_color(outline));

        stage.set_fill(outline, Some(WHITE), Some(1.0), None, true);
        assert!(stage.has_fill(outline));
        assert_color(stage.get_color(outline), WHITE);

        stage.set_stroke(outline, None, Some(0.0), None, None, true);
        assert!(!stage.has_stroke(outline), "zero width is no stroke");
    }

    #[test]
    fn backstroke_sets_the_uniform() {
        let mut stage = Stage::new();
        let mob = stage.add(VMobject::from_points(vec![
            [0.0; 3],
            [1.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
        ]));
        assert!(!stage.get(mob).unwrap().uniforms().stroke_behind);
        stage.set_backstroke(mob, WHITE, 3.0);
        assert!(stage.get(mob).unwrap().uniforms().stroke_behind);
        assert!(close(stage.get_stroke_width(mob).unwrap(), 3.0));
    }

    #[test]
    fn point_free_getters_observe_own_paint_without_mutating_geometry() {
        let mut stage = Stage::new();
        let root = stage.add(
            VMobject::new().with_child(
                VMobject::from_points(vec![[0.0; 3], [1.0; 3], [2.0; 3]])
                    .with_style(Style::default().fill(WHITE, 1.0)),
            ),
        );
        stage.set_fill(root, Some(BLUE), Some(0.6), None, false);
        stage.set_stroke(root, Some(RED), Some(2.5), Some(0.4), None, false);
        let before = stage.snapshot_bytes().unwrap();
        assert_color(stage.get_fill_color(root), BLUE);
        assert_color(stage.get_stroke_color(root), RED);
        assert!(close(stage.get_fill_opacity(root).unwrap(), 0.6));
        assert!(close(stage.get_stroke_opacity(root).unwrap(), 0.4));
        assert_eq!(stage.get_stroke_width(root), Some(2.5));
        assert!(stage.has_fill(root));
        assert!(stage.has_stroke(root));
        assert_color(stage.get_color(root), BLUE);
        assert_eq!(stage.snapshot_bytes().unwrap(), before);
        assert!(stage.get(root).unwrap().buffer.is_empty());
        let child = stage.get(root).unwrap().submobjects()[0];
        assert_color(stage.get_fill_color(child), WHITE);
        stage.set_fill(root, None, Some(0.0), None, false);
        assert!(!stage.has_fill(root));
        assert_color(stage.get_color(root), RED);
        stage.set_stroke(root, None, Some(0.0), None, None, false);
        assert!(!stage.has_stroke(root));
    }

    #[test]
    fn copied_phantom_paint_reads_independently_before_growth() {
        let mut stage = Stage::new();
        let root = stage.add(VMobject::new());
        stage.set_fill(root, Some(BLUE), Some(0.5), None, false);
        let buffer = stage.get(root).unwrap().buffer.snapshot_clone();
        let copy = stage.add(fmn_mobject::Mobject::from_buffer(buffer));
        stage.set_fill(root, Some(RED), Some(0.25), None, false);
        assert_color(stage.get_fill_color(root), RED);
        assert_color(stage.get_fill_color(copy), BLUE);
        assert_eq!(stage.get_fill_opacity(copy), Some(0.5));
        stage.get_mut(copy).unwrap().buffer.resize(3).unwrap();
        assert_color(stage.get_fill_color(copy), BLUE);
        assert_eq!(stage.get_fill_opacity(copy), Some(0.5));
    }

    #[test]
    fn populated_records_override_defaults_without_culling_interpolated_strokes() {
        let mut stage = Stage::new();
        let root = stage.add(VMobject::new());
        stage.set_fill(root, Some(BLUE), Some(1.0), None, false);
        stage.set_stroke(root, Some(RED), Some(4.0), Some(1.0), None, false);
        stage.get_mut(root).unwrap().buffer.resize(3).unwrap();
        stage.set_fill(root, Some(WHITE), Some(0.0), None, false);
        assert_color(stage.get_fill_color(root), WHITE);
        assert_eq!(stage.get_fill_opacity(root), Some(0.0));
        assert!(!stage.has_fill(root));
        let buffer = &mut stage.get_mut(root).unwrap().buffer;
        assert!(buffer.write_range("stroke_width", 0, &[2.0, 0.0, 0.0]));
        assert!(buffer.write_range(
            "stroke_rgba",
            0,
            &[1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0],
        ));
        // Width falls to zero while alpha rises: both are positive in between.
        assert!(stage.has_stroke(root));
        stage.set_stroke(root, None, None, Some(0.0), None, false);
        assert!(!stage.has_stroke(root));
        assert_eq!(stage.get_stroke_opacity(root), Some(0.0));
    }

    #[test]
    fn absent_style_fields_and_foreign_handles_stay_absent() {
        let mut stage = Stage::new();
        let root = stage.add(fmn_mobject::Mobject::new());
        assert_eq!(stage.get_fill_color(root), None);
        assert_eq!(stage.get_stroke_width(root), None);
        assert!(!stage.has_fill(root));
        assert!(!stage.has_stroke(root));
        let foreign = Stage::new();
        assert_eq!(foreign.get_fill_color(root), None);
        assert_eq!(foreign.get_stroke_width(root), None);
        assert!(!foreign.has_fill(root));
        assert!(!foreign.has_stroke(root));
    }

    #[test]
    fn incompatible_custom_style_fields_refuse_on_empty_and_populated_records() {
        for records in [0, 3] {
            for color_width in [0, 1, 2, 3, 5] {
                let schema = fmn_mobject::RecordSchema::new(
                    &[
                        ("point", 3),
                        ("fill_rgba", color_width),
                        ("stroke_rgba", color_width),
                        ("stroke_width", 1),
                    ],
                    &["point"],
                    &["point"],
                )
                .unwrap();
                let mut buffer = RecordBuffer::new(schema, records).unwrap();
                buffer.write_scalar_lanes("stroke_width", 2.5);
                let mut stage = Stage::new();
                let mob = stage.add(fmn_mobject::Mobject::from_buffer(buffer));
                assert_eq!(stage.get_fill_color(mob), None);
                assert_eq!(stage.get_fill_opacity(mob), None);
                assert_eq!(stage.get_stroke_color(mob), None);
                assert_eq!(stage.get_stroke_opacity(mob), None);
                assert_eq!(stage.get_stroke_width(mob), Some(2.5));
                assert!(!stage.has_fill(mob));
                assert!(!stage.has_stroke(mob));
                assert_eq!(stage.get(mob).unwrap().buffer.len(), records);
            }
            for width_lanes in [0, 2, 3] {
                let schema = fmn_mobject::RecordSchema::new(
                    &[("point", 3), ("stroke_rgba", 4), ("stroke_width", width_lanes)],
                    &["point"],
                    &["point"],
                )
                .unwrap();
                let mut buffer = RecordBuffer::new(schema, records).unwrap();
                buffer.write_color_lanes("stroke_rgba", Some([1.0, 0.0, 0.0]), Some(1.0));
                let mut stage = Stage::new();
                let mob = stage.add(fmn_mobject::Mobject::from_buffer(buffer));
                assert_eq!(stage.get_stroke_width(mob), None);
                assert_color(stage.get_stroke_color(mob), RED);
                assert_eq!(stage.get_stroke_opacity(mob), Some(1.0));
                assert!(!stage.has_stroke(mob));
            }
        }
    }
}
