//! The frame buffer: one contiguous allocation described by a
//! [`FrameLayout`] (§14.1).
//!
//! Orientation is D-23's rule made structural: a `FrameBuffer` is
//! ALWAYS in output orientation — row 0 is the top row of the delivered
//! image. There is no flipped variant, no orientation flag, and no
//! vflip anywhere in the system.

use crate::format::FrameLayout;

/// A single frame's pixel storage.
///
/// The allocation happens once, in [`FrameBuffer::new`]; everything
/// afterwards is slicing. Buffers are meant to be pooled
/// ([`crate::FramePool`]) so the render hot path never allocates,
/// resizes, or frees frame-sized memory (PG-6).
#[derive(Debug, Clone)]
pub struct FrameBuffer {
    layout: FrameLayout,
    data: Vec<u8>,
}

impl FrameBuffer {
    /// Allocate a zero-filled buffer for `layout`.
    #[must_use]
    pub fn new(layout: FrameLayout) -> Self {
        let data = vec![0u8; layout.total_bytes()];
        Self { layout, data }
    }

    /// The negotiated geometry.
    #[must_use]
    pub const fn layout(&self) -> &FrameLayout {
        &self.layout
    }

    /// The bytes of `plane` (stride-padded rows included).
    #[must_use]
    pub fn plane(&self, plane: usize) -> &[u8] {
        let start = self.layout.plane_offset(plane);
        &self.data[start..start + self.layout.plane_bytes(plane)]
    }

    /// Mutable bytes of `plane`.
    pub fn plane_mut(&mut self, plane: usize) -> &mut [u8] {
        let start = self.layout.plane_offset(plane);
        let len = self.layout.plane_bytes(plane);
        &mut self.data[start..start + len]
    }

    /// The whole allocation, all planes, padding included.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.data
    }

    /// The whole allocation, mutable.
    pub fn as_bytes_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }
}
