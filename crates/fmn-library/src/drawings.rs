//! The drawings shelf, geometry-native tranche (fm-3kr, §12.7): `Clock`,
//! `DieFace`, and `Dartboard` — pure Marionette + Chisel compositions, no
//! new mechanisms (the shelf's quality bar). The asset-backed census
//! VectorizedEarth) load Reference-repo SVG files and are governed by
//! ADR-0020: those assets never ship — classes resolve user-supplied files,
//! refusing by name otherwise. They are deliberately absent here until
//! fm-3kr tranche 3 lands the resolver families.
//!
//! Self-goldens: one bit-locked point digest per class in
//! `tests/drawings.rs`, the fm-6l6 pattern.

use fmn_core::color::Srgb;
use fmn_core::constants::{
    BLACK, DL, DR, GREEN_E, GREY_B, GREY_E, LEFT, ORIGIN, OUT, RED_E, RIGHT, TAU, UL, UP, UR, WHITE,
};
use fmn_core::types::Vec3;
use fmn_mobject::{Mob, Stage};

use crate::arc::{AnnularSector, Circle, Dot};
use crate::boolean_ops::BooleanMobjectError;
use crate::line::Line;
use crate::poly::Rectangle;
use crate::style::Style;
use crate::vmobject::{VMobject, v_group};

/// drawings.py:309's tick direction: `cos(angle)·UP + sin(angle)·RIGHT`.
fn tick_direction(angle: f64) -> Vec3 {
    [
        angle.cos() * UP[0] + angle.sin() * RIGHT[0],
        angle.cos() * UP[1] + angle.sin() * RIGHT[1],
        0.0,
    ]
}

/// `Clock` (drawings.py:296): a circle, twelve tick marks (the cardinals
/// double-length), and hour/minute hands pointing up from the center.
///
/// Child order is the Reference's — `[circle, hour_hand, minute_hand,
/// ticks]` — so `ClockPassesTime` wiring can find the hands positionally.
pub struct Clock {
    vmob: VMobject,
}

impl Clock {
    /// Reference defaults: `stroke_color = WHITE`, `stroke_width = 3.0`,
    /// `hour_hand_height = 0.3`, `minute_hand_height = 0.6`,
    /// `tick_length = 0.1`.
    #[must_use]
    pub fn new() -> Self {
        Self::with_style(WHITE, 3.0, 0.3, 0.6, 0.1)
    }

    /// The full constructor surface.
    #[must_use]
    pub fn with_style(
        stroke_color: Srgb,
        stroke_width: f64,
        hour_hand_height: f64,
        minute_hand_height: f64,
        tick_length: f64,
    ) -> Self {
        let circle = Circle::new().build();
        let mut ticks: Vec<VMobject> = Vec::new();
        for (index, step) in (0..12).enumerate() {
            let angle = f64::from(step as u32) * TAU / 12.0;
            let direction = tick_direction(angle);
            let length = if index % 3 == 0 {
                2.0 * tick_length
            } else {
                tick_length
            };
            let inner = [
                (1.0 - length) * direction[0],
                (1.0 - length) * direction[1],
                0.0,
            ];
            ticks.push(
                Line::new(direction, inner)
                    .build()
                    .expect("a straight tick never fails"),
            );
        }
        let hour_hand = Line::new(ORIGIN, [0.0, hour_hand_height, 0.0])
            .build()
            .expect("a straight hand never fails");
        let minute_hand = Line::new(ORIGIN, [0.0, minute_hand_height, 0.0])
            .build()
            .expect("a straight hand never fails");
        let family = v_group([circle, hour_hand, minute_hand, v_group(ticks)]).map_style_deep(
            move |style| {
                style
                    .stroke(stroke_color, stroke_width, 1.0)
                    .fill(stroke_color, 0.0)
            },
        );
        Self { vmob: family }
    }

    /// The built clock family.
    #[must_use]
    pub fn build(self) -> VMobject {
        self.vmob
    }
}

impl Default for Clock {
    fn default() -> Self {
        Self::new()
    }
}

/// `DieFace` (drawings.py:687): a rounded square with `value` pips at the
/// Reference's face positions, coalesced toward the center by
/// `dot_coalesce_factor` (the Reference's `space_out_submobjects`).
///
/// Returns `None` outside 1..=6 — the Reference's "DieFace only accepts
/// integer inputs between 1 and 6".
#[must_use]
pub fn die_face(value: u8, side_length: f64, dot_coalesce_factor: f64) -> Option<VMobject> {
    if !(1..=6).contains(&value) {
        return None;
    }
    let square = Rectangle::new()
        .width(side_length)
        .height(side_length)
        .corner_radius(0.15)
        .style(Style::default().stroke(WHITE, 2.0, 1.0).fill(GREY_E, 1.0))
        .build()
        .expect("a die face builds: pure geometry with requested corners");
    let center = square.center_point();
    let face_positions: [&[Vec3]; 6] = [
        &[ORIGIN],
        &[UL, DR],
        &[UL, ORIGIN, DR],
        &[UL, UR, DL, DR],
        &[UL, UR, ORIGIN, DL, DR],
        &[UL, UR, LEFT, RIGHT, DL, DR],
    ];
    let pips: Vec<VMobject> = face_positions[value as usize - 1]
        .iter()
        .map(|direction| {
            let anchor = square.bbox_point(*direction).unwrap_or(center);
            // move_to the face anchor, then the coalesce factor scales each
            // pip's offset from the group center (space_out_submobjects).
            Dot::new()
                .radius(0.08)
                .build()
                .moved_to(anchor)
                .scaled_about(dot_coalesce_factor, center)
        })
        .collect();
    Some(v_group([square, v_group(pips)]))
}

/// `Dartboard` (drawings.py:731): the twenty-sector ring stack — bull ring
/// in `[GREY_B, GREY_E]`, the 25 / triple / double / single rings in
/// alternating `[GREEN_E, RED_E]` — rotated half a sector, two bullseyes,
/// scaled to the class radius 3.
///
/// # Errors
/// [`fmn_geom::GeomError`] from sector construction.
pub fn dartboard() -> Result<VMobject, fmn_geom::GeomError> {
    dartboard_with_palettes(&[GREY_B, GREY_E], &[GREEN_E, RED_E])
}

/// [`dartboard`] with explicit ring palettes.
///
/// # Errors
/// [`fmn_geom::GeomError`] from sector construction.
pub fn dartboard_with_palettes(
    bull_colors: &[Srgb],
    scoring_colors: &[Srgb],
) -> Result<VMobject, fmn_geom::GeomError> {
    const N_SECTORS: usize = 20;
    let angle = TAU / f64::from(N_SECTORS as u32);
    let rings: [(f64, f64); 4] = [(0.0, 1.0), (0.5, 0.55), (0.55, 0.95), (0.95, 1.0)];
    let mut segments: Vec<VMobject> = Vec::new();
    for (ring_index, (inner, outer)) in rings.iter().enumerate() {
        let palette = if ring_index == 0 {
            bull_colors
        } else {
            scoring_colors
        };
        for n in 0..N_SECTORS {
            let color = palette[n % palette.len()];
            segments.push(
                AnnularSector::sector(angle, *outer)
                    .inner_radius(*inner)
                    .start_angle(f64::from(n as u32) * angle)
                    .color(color)
                    .build()?,
            );
        }
    }
    let mut board = v_group(segments).rotated_about(-angle / 2.0, OUT, ORIGIN);
    let bullseyes = v_group([
        Circle::new()
            .radius(0.07)
            .build()
            .map_style(|style| style.fill(GREEN_E, 1.0).stroke(GREEN_E, 0.0, 1.0)),
        Circle::new()
            .radius(0.035)
            .build()
            .map_style(|style| style.fill(RED_E, 1.0).stroke(RED_E, 0.0, 1.0)),
    ]);
    board = v_group([board, bullseyes]);
    Ok(board.scaled_about(3.0, ORIGIN))
}

// ------------------------------------------------ tranche 2 (fm-3kr):
// Speedometer, Piano, Piano3D

/// The needle pivot of a [`speedometer`] build: the arc's center — the
/// origin for the Reference defaults.
#[must_use]
pub fn speedometer_pivot() -> Vec3 {
    ORIGIN
}

/// `Speedometer` (drawings.py:111): the gauge arc, `num_ticks` tick lines
/// with `Integer(10·index)` labels, and the yellow triangular needle
/// pre-rotated to the zero-velocity position. Child order:
/// `[arc, (tick, label)×num_ticks, needle]` — the needle is LAST, so
/// needle wiring can find it positionally.
///
/// The Reference overrides `get_center` to subtract the as-constructed
/// box center (callers read positions relative to the pivot); here the
/// pivot is simply [`speedometer_pivot`] and the geometry is faithful.
///
/// # Errors
/// [`TextMobjectError`] from the tick labels or the arc.
pub fn speedometer(book: &fmn_text::FontBook) -> Result<VMobject, crate::text::TextMobjectError> {
    speedometer_with(
        book,
        4.0 * std::f64::consts::PI / 3.0,
        8,
        0.2,
        0.1,
        0.8,
        fmn_core::color::Srgb::from_rgb8(0xFF, 0xFF, 0x00),
    )
}

/// [`speedometer`] with the full constructor surface.
///
/// # Errors
/// As [`speedometer`].
pub fn speedometer_with(
    book: &fmn_text::FontBook,
    arc_angle: f64,
    num_ticks: usize,
    tick_length: f64,
    needle_width: f64,
    needle_height: f64,
    needle_color: Srgb,
) -> Result<VMobject, crate::text::TextMobjectError> {
    let start_angle = std::f64::consts::FRAC_PI_2 + arc_angle / 2.0;
    let end_angle = std::f64::consts::FRAC_PI_2 - arc_angle / 2.0;
    let arc = crate::arc::Arc::new()
        .start_angle(start_angle)
        .angle(-arc_angle)
        .build()
        .map_err(|e| crate::text::TextMobjectError::Geometry {
            what: format!("speedometer arc: {e}"),
        })?;
    let mut members: Vec<VMobject> = Vec::with_capacity(1 + 2 * num_ticks + 1);
    members.push(arc);
    for index in 0..num_ticks {
        let angle = start_angle
            + (end_angle - start_angle) * f64::from(index as u32)
                / f64::from(num_ticks.saturating_sub(1) as u32).max(1.0);
        let vect = [angle.cos(), angle.sin(), 0.0];
        let inner = [
            (1.0 - tick_length) * vect[0],
            (1.0 - tick_length) * vect[1],
            0.0,
        ];
        members.push(
            Line::new(vect, inner)
                .build()
                .expect("a straight tick never fails"),
        );
        let label = crate::numbers::Integer::new(f64::from(index as u32) * 10.0)
            .build(book)?
            .into_vmob()
            .with_height(tick_length, true)
            .moved_to([
                (1.0 + tick_length) * vect[0],
                (1.0 + tick_length) * vect[1],
                0.0,
            ]);
        members.push(label);
    }
    // The needle: the LEFT/UP/RIGHT triangle fitted to the needle box,
    // rotated so it points along the zero-velocity start angle.
    let needle = crate::poly::Polygon::new([LEFT, UP, RIGHT])
        .style(
            Style::default()
                .fill(needle_color, 1.0)
                .stroke(needle_color, 0.0, 1.0),
        )
        .build();
    let (w, h) = needle.extent().map_or((1.0, 1.0), |(min, max)| {
        ((max[0] - min[0]).max(1e-9), (max[1] - min[1]).max(1e-9))
    });
    let needle = needle
        .scaled_about(needle_width / w, ORIGIN)
        .scaled_about(needle_height / h, ORIGIN)
        .rotated_about(start_angle - std::f64::consts::FRAC_PI_2, OUT, ORIGIN);
    members.push(needle);
    Ok(v_group(members))
}

/// `move_needle_to_velocity` (drawings.py:180): rotate the needle child of
/// a [`speedometer`] build to the velocity proportion of the gauge
/// (`max_velocity = 10 × (num_ticks − 1)`), about the pivot.
///
/// # Errors
/// [`fmn_mobject::StageError::StaleHandle`] for a dead needle.
pub fn move_needle_to_velocity(
    stage: &mut Stage,
    needle: Mob,
    arc_angle: f64,
    num_ticks: usize,
    velocity: f64,
) -> Result<(), fmn_mobject::StageError> {
    let max_velocity = 10.0 * f64::from(num_ticks.saturating_sub(1) as u32).max(1.0);
    let proportion = (velocity / max_velocity).clamp(0.0, 1.0);
    let start_angle = std::f64::consts::FRAC_PI_2 + arc_angle / 2.0;
    let target = start_angle - arc_angle * proportion;
    let pivot = speedometer_pivot();
    let tip = stage.get_center(needle);
    let current = f64::atan2(tip[1] - pivot[1], tip[0] - pivot[0]);
    stage.rotate(needle, target - current, OUT, Some(pivot), None);
    Ok(())
}

/// A built [`piano`]: the keyboard plus which final child indices are the
/// black keys (the Piano3D elevation needs them).
#[derive(Debug, Clone)]
pub struct PianoBuild {
    /// The keyboard, fitted to the total width.
    pub vmob: VMobject,
    /// Child indices of the black keys, in child order.
    pub black_key_indices: Vec<usize>,
}

/// `Piano` (drawings.py:593): `n_white_keys` white keys, black keys at the
/// octave positions the pattern skips, each neighboring white key notched
/// by a boolean difference against the enlarged black key — the
/// Reference's `wk.become(Difference(wk, big_bk))` running on the
/// certified Chisel kernel. Keys sort by x, all but the last
/// point-reverse, and the keyboard fits to `total_width`.
///
/// # Errors
/// [`BooleanMobjectError`] from the boolean notching or the key geometry.
pub fn piano() -> Result<VMobject, BooleanMobjectError> {
    Ok(piano_with(
        52,
        &[0, 2, 3, 5, 6],
        7,
        (0.15, 1.0),
        (0.1, 0.66),
        0.02,
        WHITE,
        GREY_E,
        13.0,
    )?
    .vmob)
}

/// [`piano`] with the full constructor surface, returning the black-key
/// child indices for the Piano3D elevation.
///
/// # Errors
/// [`BooleanMobjectError`] from the boolean notching or the key geometry.
#[allow(clippy::too_many_arguments)]
pub fn piano_with(
    n_white_keys: usize,
    black_pattern: &[usize],
    white_keys_per_octave: usize,
    white_key_dims: (f64, f64),
    black_key_dims: (f64, f64),
    key_buff: f64,
    white_key_color: Srgb,
    black_key_color: Srgb,
    total_width: f64,
) -> Result<PianoBuild, BooleanMobjectError> {
    let white_style = Style::default()
        .fill(white_key_color, 1.0)
        .stroke(white_key_color, 0.0, 1.0);
    let black_style = Style::default()
        .fill(black_key_color, 1.0)
        .stroke(black_key_color, 0.0, 1.0);
    let (ww, wh) = white_key_dims;
    let (bw, bh) = black_key_dims;
    let mut white_keys: Vec<VMobject> = Vec::with_capacity(n_white_keys);
    for i in 0..n_white_keys {
        let x = f64::from(i as u32) * (ww + key_buff);
        white_keys.push(
            crate::poly::Rectangle::new()
                .width(ww)
                .height(wh)
                .style(white_style)
                .build()
                .expect("a piano key builds: pure geometry")
                .moved_to([x + ww / 2.0, 0.0, 0.0]),
        );
    }
    let mut black_keys: Vec<VMobject> = Vec::new();
    for i in 0..n_white_keys.saturating_sub(1) {
        if black_pattern.contains(&(i % white_keys_per_octave)) {
            continue;
        }
        let top1 = white_keys[i]
            .bbox_point(UP)
            .expect("a white key has an extent");
        let top2 = white_keys[i + 1]
            .bbox_point(UP)
            .expect("a white key has an extent");
        let midpoint = [0.5 * (top1[0] + top2[0]), 0.5 * (top1[1] + top2[1]), 0.0];
        let black = crate::poly::Rectangle::new()
            .width(bw)
            .height(bh)
            .style(black_style)
            .build()
            .expect("a piano key builds: pure geometry")
            .moved_to_aligned(midpoint, UP);
        // The enlarged cutter, then the notch (Difference(wk, big_bk)).
        let (gw, gh) = black
            .extent()
            .map_or((bw, bh), |(min, max)| (max[0] - min[0], max[1] - min[1]));
        let big_black = black
            .clone()
            .scaled_about((gw + key_buff) / gw.max(1e-9), ORIGIN)
            .stretched_about((gh + key_buff) / gh.max(1e-9), 1, ORIGIN)
            .moved_to_aligned(midpoint, UP);
        white_keys[i] = crate::boolean_ops::difference(&white_keys[i], &big_black)?
            .into_mobject()
            .map_style(move |_| white_style);
        black_keys.push(black);
    }
    // sort_keys: one x-sorted child list, tracking which are black.
    let mut all: Vec<(VMobject, bool)> = white_keys
        .into_iter()
        .map(|key| (key, false))
        .chain(black_keys.into_iter().map(|key| (key, true)))
        .collect();
    all.sort_by(|a, b| {
        a.0.center_point()[0]
            .partial_cmp(&b.0.center_point()[0])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    // self[:-1].reverse_points(): every key except the LAST in x order.
    let last = all.len().saturating_sub(1);
    let mut keyboard: Vec<VMobject> = Vec::with_capacity(all.len());
    let mut black_key_indices: Vec<usize> = Vec::new();
    for (index, (key, is_black)) in all.into_iter().enumerate() {
        if is_black {
            black_key_indices.push(index);
        }
        keyboard.push(if index == last {
            key
        } else {
            key.reversed_points()
        });
    }
    Ok(PianoBuild {
        vmob: v_group(keyboard).with_width(total_width, true),
        black_key_indices,
    })
}

/// `Piano3D` (drawings.py:657): every 2D key extruded through
/// [`crate::solids::Prismify`], stroked black, shaded `(1.0, 0.2, 0.2)`,
/// depth-tested, with the black keys elevated along OUT and recolored.
///
/// # Errors
/// [`fmn_geom::GeomError`] from the 2D piano or the extrusions.
pub fn piano_3d() -> Result<VMobject, BooleanMobjectError> {
    let build = piano_with(
        52,
        &[0, 2, 3, 5, 6],
        7,
        (0.15, 1.0),
        (0.1, 0.66),
        0.001,
        fmn_core::constants::GREY_A,
        GREY_E,
        13.0,
    )?;
    let black: std::collections::HashSet<usize> = build.black_key_indices.iter().copied().collect();
    let keys: Vec<VMobject> = build
        .vmob
        .children()
        .iter()
        .enumerate()
        .map(|(index, key)| {
            let mut extruded = crate::solids::Prismify::new(key.clone()).depth(0.1).build();
            if black.contains(&index) {
                extruded = extruded
                    .shifted(fmn_core::constants::OUT.map(|c| c * 0.05))
                    .map_style_deep(|style| style.color(BLACK));
            }
            extruded
        })
        .collect();
    Ok(v_group(keys)
        .map_style_deep(|style| style.stroke(BLACK, 0.25, 1.0))
        .with_uniforms(fmn_mobject::Uniforms {
            shading: [1.0, 0.2, 0.2],
            depth_test: true,
            ..fmn_mobject::Uniforms::default()
        }))
}
