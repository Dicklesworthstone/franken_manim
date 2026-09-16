//! Native executable scene programs for the isolated Studio worker.
//!
//! These owners use Proscenium's stepped animation/wait drivers. They retain
//! executable callbacks while paused rather than pretending a durable snapshot
//! can recreate them.

mod program;

pub use program::{MAX_NATIVE_SEGMENTS, NativeSceneProgram, NativeSegment};
