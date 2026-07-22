//! Detached mobject values: plain data before any stage exists.
//!
//! The §15.1 boundary ratified by G0-1: mobjects are constructed and
//! composed as ordinary values (builders in higher crates convert via
//! `Into<Mobject>`), and `Stage::add` moves them — with their detached
//! children — into the arena. The QuadPath wiring into record fields, the
//! full style surface, and the class library live upstream (fm-jru, W7);
//! this type is the ownership boundary only.

use fmn_core::types::Vec3;

use crate::record::{RecordBuffer, RecordSchema};

/// A detached mobject: record data plus detached children.
pub struct Mobject {
    /// The per-object record data.
    pub buffer: RecordBuffer,
    /// Children still outside any arena; `Stage::add` recurses over these.
    pub submobjects: Vec<Mobject>,
}

impl Default for Mobject {
    fn default() -> Self {
        Self::new()
    }
}

impl Mobject {
    /// An empty mobject (no records, no children).
    #[must_use]
    pub fn new() -> Self {
        Self {
            buffer: RecordBuffer::new(RecordSchema::mobject(), 0),
            submobjects: Vec::new(),
        }
    }

    /// A mobject whose `point` records are the given points (semantic f64
    /// in, record f32 stored, per §6.1).
    #[must_use]
    pub fn from_points(points: &[Vec3]) -> Self {
        let mut buffer = RecordBuffer::new(RecordSchema::mobject(), points.len());
        for (i, p) in points.iter().enumerate() {
            buffer.write(i, "point", &[p[0] as f32, p[1] as f32, p[2] as f32]);
        }
        Self {
            buffer,
            submobjects: Vec::new(),
        }
    }

    /// Group composition while detached.
    #[must_use]
    pub fn group(children: Vec<Mobject>) -> Self {
        Self {
            buffer: RecordBuffer::new(RecordSchema::mobject(), 0),
            submobjects: children,
        }
    }
}
