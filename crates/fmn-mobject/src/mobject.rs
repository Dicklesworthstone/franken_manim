//! Detached mobject values: plain data before any stage exists.
//!
//! The §15.1 boundary ratified by G0-1: mobjects are constructed and
//! composed as ordinary values (builders in higher crates convert via
//! `Into<Mobject>`), and `Stage::add` moves them — with their detached
//! children — into the arena.
//!
//! A detached mobject carries everything `Stage::add` needs to reconstruct
//! the entry: the record data, the per-object [`Uniforms`], and the
//! semantic [`ShapeTag`] its constructor built (§10.8). Those last two
//! matter because a builder decides them — `Dot` is a dot, `Circle` sets
//! its own stroke colour — and a value that could not carry them would
//! force every library class to be constructed in two halves, one before
//! `add` and one after.

use fmn_core::types::Vec3;

use crate::record::{RecordBuffer, RecordSchema};
use crate::shape::ShapeTag;
use crate::uniforms::Uniforms;

/// A detached mobject: record data, per-object uniforms, the semantic
/// shape tag, and detached children.
pub struct Mobject {
    /// The per-object record data.
    pub buffer: RecordBuffer,
    /// The per-object uniform inventory (§8.4) this mobject will enter the
    /// arena with.
    pub uniforms: Uniforms,
    /// The semantic shape (§10.8) the constructor built, stamped against
    /// these points when the mobject enters the arena.
    pub shape: ShapeTag,
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
        Self::from_buffer(RecordBuffer::new(RecordSchema::mobject(), 0))
    }

    /// A mobject over an already-built record buffer, with default
    /// uniforms, no shape, and no children.
    #[must_use]
    pub fn from_buffer(buffer: RecordBuffer) -> Self {
        Self {
            buffer,
            uniforms: Uniforms::default(),
            shape: ShapeTag::General,
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
        Self::from_buffer(buffer)
    }

    /// Group composition while detached.
    #[must_use]
    pub fn group(children: Vec<Mobject>) -> Self {
        let mut out = Self::new();
        out.submobjects = children;
        out
    }

    /// Attach the semantic shape tag (builder style).
    #[must_use]
    pub fn with_shape(mut self, shape: ShapeTag) -> Self {
        self.shape = shape;
        self
    }

    /// Attach the uniform inventory (builder style).
    #[must_use]
    pub fn with_uniforms(mut self, uniforms: Uniforms) -> Self {
        self.uniforms = uniforms;
        self
    }
}
