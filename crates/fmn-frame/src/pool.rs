//! The preallocated frame pool (§14.1, §14.3, PG-6).
//!
//! The in-flight frame budget is real memory — a 4K RGBA frame is
//! ~33 MB — so the pool preallocates its whole capacity up front and
//! never grows. An exhausted pool returns `None` from
//! [`FramePool::try_acquire`]; that refusal IS the backpressure signal
//! the ordered emitter (fm-hv4) and the pipeline (§17.4) propagate.
//! Zero frame-sized allocations happen after construction, which is
//! exactly what the steady-state allocation gate instruments.

use crate::FrameError;
use crate::buffer::FrameBuffer;
use crate::format::FrameLayout;

/// A fixed-capacity pool of interchangeable [`FrameBuffer`]s sharing
/// one [`FrameLayout`].
#[derive(Debug)]
pub struct FramePool {
    layout: FrameLayout,
    free: Vec<FrameBuffer>,
    capacity: usize,
}

impl FramePool {
    /// Preallocate `capacity` buffers of `layout`. This is the only
    /// place the pool ever allocates frame memory.
    #[must_use]
    pub fn new(layout: FrameLayout, capacity: usize) -> Self {
        let free = (0..capacity)
            .map(|_| FrameBuffer::new(layout.clone()))
            .collect();
        Self {
            layout,
            free,
            capacity,
        }
    }

    /// Take a buffer, or `None` if the pool is exhausted (backpressure —
    /// the pool never allocates to satisfy demand).
    ///
    /// The returned buffer's contents are stale (whatever the previous
    /// user wrote); the hot path overwrites, it does not re-zero.
    pub fn try_acquire(&mut self) -> Option<FrameBuffer> {
        self.free.pop()
    }

    /// Return a buffer to the pool.
    ///
    /// Refuses buffers of a foreign layout (they would silently corrupt
    /// the pool's geometry contract) and refuses to grow past capacity.
    pub fn release(&mut self, buffer: FrameBuffer) -> Result<(), FrameError> {
        if *buffer.layout() != self.layout {
            return Err(FrameError::ForeignBuffer);
        }
        if self.free.len() >= self.capacity {
            return Err(FrameError::PoolOverflow);
        }
        self.free.push(buffer);
        Ok(())
    }

    /// The layout every pooled buffer shares.
    #[must_use]
    pub const fn layout(&self) -> &FrameLayout {
        &self.layout
    }

    /// Total buffers the pool owns (free + outstanding).
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Buffers currently available to acquire.
    #[must_use]
    pub fn available(&self) -> usize {
        self.free.len()
    }
}
