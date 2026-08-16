//! Reusable retained CPU-frame composition for native front doors.
//!
//! Lumen owns the synchronization, binning, frame-job, arena, and tile-cache
//! sequence.  CLI, Studio, WASM, and Python adapters decide where the returned
//! raw frame goes; none of them should copy this orchestration or grow a second
//! semantic renderer.

use std::fmt;

use fmn_frame::{FrameBuffer, FrameError};
use fmn_mobject::Stage;

use crate::{
    Binning, BinningError, CachedRenderError, CachedRenderStats, EngineIdentity, FrameArena,
    FrameConfig, FrameJob, FrameJobError, MonoTable, MonoTableError, PixelTileCache, RenderPlan,
    SyncError, Tiling,
};

/// Immutable policy for one retained CPU renderer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RetainedFrameRendererConfig {
    /// Viewport, mapping, background, and AA policy.
    pub frame: FrameConfig,
    /// Two-level tile geometry.
    pub tiling: Tiling,
    /// Certified-CPU or fast-CPU identity.
    pub engine: EngineIdentity,
    /// Fixed worker-team width selected by the composition root.
    pub threads: usize,
}

/// Construction or frame-preparation failure at the shared Lumen boundary.
#[derive(Debug)]
pub enum RetainedFrameRendererError {
    /// A renderer cannot execute with an empty worker team.
    InvalidThreads,
    /// The configured raw-frame layout is invalid.
    Layout(FrameError),
    /// Retained-plan synchronization failed.
    Sync(SyncError),
    /// Monotone fill-table preparation failed.
    Mono(MonoTableError),
    /// Tile binning or painter-safe pruning failed.
    Binning(BinningError),
    /// The selected CPU frame job could not be prepared.
    Prepare(FrameJobError),
    /// Rasterization or retained-cache publication failed.
    Render(CachedRenderError),
}

impl fmt::Display for RetainedFrameRendererError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidThreads => {
                f.write_str("retained CPU rendering requires at least one thread")
            }
            Self::Layout(error) => error.fmt(f),
            Self::Sync(error) => error.fmt(f),
            Self::Mono(error) => error.fmt(f),
            Self::Binning(error) => error.fmt(f),
            Self::Prepare(error) => error.fmt(f),
            Self::Render(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for RetainedFrameRendererError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidThreads => None,
            Self::Layout(error) => Some(error),
            Self::Sync(error) => Some(error),
            Self::Mono(error) => Some(error),
            Self::Binning(error) => Some(error),
            Self::Prepare(error) => Some(error),
            Self::Render(error) => Some(error),
        }
    }
}

impl From<FrameError> for RetainedFrameRendererError {
    fn from(error: FrameError) -> Self {
        Self::Layout(error)
    }
}

impl From<SyncError> for RetainedFrameRendererError {
    fn from(error: SyncError) -> Self {
        Self::Sync(error)
    }
}

impl From<MonoTableError> for RetainedFrameRendererError {
    fn from(error: MonoTableError) -> Self {
        Self::Mono(error)
    }
}

impl From<BinningError> for RetainedFrameRendererError {
    fn from(error: BinningError) -> Self {
        Self::Binning(error)
    }
}

impl From<FrameJobError> for RetainedFrameRendererError {
    fn from(error: FrameJobError) -> Self {
        Self::Prepare(error)
    }
}

impl From<CachedRenderError> for RetainedFrameRendererError {
    fn from(error: CachedRenderError) -> Self {
        Self::Render(error)
    }
}

/// Lumen state retained across immutable scene captures.
///
/// The first frame sizes the compiled plan, bump arena, and pixel cache.  Later
/// frames synchronize only changed Marionette revisions and restore unchanged
/// tiles byte-for-byte before rasterizing dirty work.
#[derive(Debug)]
pub struct RetainedFrameRenderer {
    config: RetainedFrameRendererConfig,
    plan: RenderPlan,
    arena: FrameArena,
    cache: PixelTileCache,
    frame: FrameBuffer,
}

impl RetainedFrameRenderer {
    /// Construct an empty retained renderer for the declared CPU policy.
    ///
    /// # Errors
    ///
    /// Refuses zero threads, invalid frame geometry, or an unrepresentable raw
    /// frame layout.  Annex identities are rejected by the first render through
    /// the ordinary typed [`FrameJobError::UnsupportedEngine`] boundary.
    pub fn new(config: RetainedFrameRendererConfig) -> Result<Self, RetainedFrameRendererError> {
        if config.threads == 0 {
            return Err(RetainedFrameRendererError::InvalidThreads);
        }
        let frame = FrameBuffer::new(config.frame.layout()?);
        Ok(Self {
            config,
            plan: RenderPlan::new(),
            arena: FrameArena::new(),
            cache: PixelTileCache::new(),
            frame,
        })
    }

    /// Synchronize one immutable stage and rasterize its retained raw frame.
    ///
    /// `camera_revision` is independent of frame sequence. A fixed 2D camera
    /// therefore passes zero on every call and preserves cache hits.
    pub fn render(
        &mut self,
        stage: &Stage,
        camera_revision: u64,
    ) -> Result<CachedRenderStats, RetainedFrameRendererError> {
        self.plan.sync(stage, camera_revision)?;
        let mono = MonoTable::build(&self.plan, self.config.frame.map)?;
        let mut binning = Binning::build(
            &self.plan,
            self.config.frame.viewport,
            self.config.tiling,
            self.config.frame.map,
        )?;
        binning.prune_occluded(&self.plan)?;
        Ok(FrameJob::with_identity_in(
            &mut self.arena,
            &self.plan,
            &mono,
            &binning,
            self.config.frame,
            self.config.engine,
        )?
        .render_into_cached(
            self.config.threads,
            &mut self.frame,
            camera_revision,
            &mut self.cache,
        )?)
    }

    /// Current raw linear-light RGBA16F frame.
    #[must_use]
    pub const fn frame(&self) -> &FrameBuffer {
        &self.frame
    }

    /// Declared renderer policy, including the journaled engine identity.
    #[must_use]
    pub const fn config(&self) -> RetainedFrameRendererConfig {
        self.config
    }

    /// Retained compiled plan for diagnostics and overlays.
    #[must_use]
    pub const fn plan(&self) -> &RenderPlan {
        &self.plan
    }

    /// Retained pixel cache for diagnostics and profiling.
    #[must_use]
    pub const fn cache(&self) -> &PixelTileCache {
        &self.cache
    }
}

#[cfg(test)]
mod tests {
    use fmn_core::color::LinearRgba;

    use super::*;
    use crate::{ScreenMap, Viewport};

    fn config(threads: usize) -> RetainedFrameRendererConfig {
        RetainedFrameRendererConfig {
            frame: FrameConfig::new(
                Viewport {
                    width: 32,
                    height: 18,
                },
                ScreenMap {
                    scale: 2.25,
                    origin: [16.0, 9.0],
                },
                LinearRgba {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 1.0,
                },
            ),
            tiling: Tiling {
                macro_tile: 16,
                fine_tile: 8,
            },
            engine: EngineIdentity::certified(),
            threads,
        }
    }

    #[test]
    fn zero_threads_fail_before_frame_allocation() {
        assert!(matches!(
            RetainedFrameRenderer::new(config(0)),
            Err(RetainedFrameRendererError::InvalidThreads)
        ));
    }

    #[test]
    fn fixed_camera_reuses_the_same_retained_frame_owner() {
        let stage = Stage::new();
        let mut renderer = RetainedFrameRenderer::new(config(1)).expect("valid renderer");
        renderer.render(&stage, 0).expect("first frame");
        let first = renderer.frame().as_bytes().to_vec();
        let second = renderer.render(&stage, 0).expect("retained frame");
        assert_eq!(renderer.frame().as_bytes(), first);
        assert!(second.cache.hits > 0 || second.cache.misses == 0);
    }
}
