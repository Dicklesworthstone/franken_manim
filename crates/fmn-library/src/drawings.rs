//! The drawings shelf, geometry-native tranche (fm-3kr, §12.7): `Clock`,
//! `DieFace`, and `Dartboard` — pure Marionette + Chisel compositions, no
//! new mechanisms (the shelf's quality bar). The asset-backed census
//! classes (Lightbulb, VideoIcon/VideoSeries, the Bubble family,
//! VectorizedEarth) load Reference-repo SVG files and wait on fm-7lx1's
//! bundled-asset ruling; they are deliberately absent here.
//!
//! Self-goldens: one bit-locked point digest per class in
//! `tests/drawings.rs`, the fm-6l6 pattern.

use fmn_core::color::Srgb;
use fmn_core::constants::{
    DL, DR, GREEN_E, GREY_B, GREY_E, LEFT, ORIGIN, OUT, RED_E, RIGHT, TAU, UL, UP, UR, WHITE,
};
use fmn_core::types::Vec3;

use crate::arc::{AnnularSector, Circle, Dot};
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
