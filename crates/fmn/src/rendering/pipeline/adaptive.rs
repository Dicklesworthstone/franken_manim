//! One adaptive capture policy shared by serial and native-offloaded portals.

use std::sync::{Arc, Mutex};

use fmn_mobject::{Mob, Stage};
use fmn_render::{Camera, PlanarCapturePolicy, Tiling};

use super::NativeFrameError;

/// Completed raster routes, excluding queued or failed captures.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NativeAdaptiveRoutes {
    /// Captures rasterized on the retained planar route.
    pub planar: u64,
    /// Captures rasterized through the camera.
    pub camera: u64,
    /// Refusal for the first camera capture in output sequence order.
    pub first_camera_reason: Option<&'static str>,
}

#[derive(Default)]
struct Counts {
    routes: NativeAdaptiveRoutes,
    first_camera_sequence: Option<u64>,
}

/// Frozen capture policy for an independently sampled native pure segment.
///
/// Clone this into jobs, retain one copy, then inspect `routes` after joining
/// the pipeline. No live arena, Python object, updater or camera callback is
/// captured. Route counters are diagnostics, outside certified identities.
#[derive(Clone)]
pub struct NativeAdaptiveCamera {
    camera: Camera,
    camera_tiling: Option<Tiling>,
    planar_policy: PlanarCapturePolicy,
    light_mob: Option<Mob>,
    counts: Arc<Mutex<Counts>>,
}

impl NativeAdaptiveCamera {
    /// Snapshot the camera and the output's frozen planar admission policy.
    /// `light_mob` must refer to the segment's Stage if provided.
    #[must_use]
    pub fn new(camera: Camera, planar_policy: PlanarCapturePolicy, light_mob: Option<Mob>) -> Self {
        Self {
            camera,
            camera_tiling: None,
            planar_policy,
            light_mob,
            counts: Arc::new(Mutex::new(Counts::default())),
        }
    }

    /// Keep camera captures on this tile geometry while planar captures use
    /// the pipeline's tiles. Without an override both routes retain the
    /// constructor's existing tile policy.
    #[must_use]
    pub fn with_camera_tiling(mut self, tiling: Tiling) -> Self {
        self.camera_tiling = Some(tiling);
        self
    }

    pub(super) const fn camera_tiling(&self) -> Option<Tiling> {
        self.camera_tiling
    }

    /// Observe joined route counts. Earlier reads can see incomplete work.
    ///
    /// # Errors
    /// A worker panic poisoned the diagnostic state.
    pub fn routes(&self) -> Result<NativeAdaptiveRoutes, NativeFrameError> {
        self.counts
            .lock()
            .map(|counts| counts.routes)
            .map_err(|_| NativeFrameError::WorkerState("adaptive route counters were poisoned"))
    }

    pub(super) const fn pixel_shape(&self) -> (u32, u32) {
        self.camera.pixel_shape()
    }

    pub(super) const fn background(&self) -> fmn_core::color::LinearRgba {
        self.camera.background()
    }

    pub(super) fn for_stage(
        &self,
        stage: &Stage,
    ) -> Result<(Option<Camera>, Option<&'static str>), NativeFrameError> {
        let mut camera = self.camera.clone();
        if let Some(light) = self.light_mob {
            if stage.get(light).is_none() {
                return Err(NativeFrameError::WorkerState(
                    "camera light handle is stale",
                ));
            }
            camera
                .set_light_source_position(stage.get_center(light))
                .map_err(NativeFrameError::Camera)?;
        }
        let refusal = self.planar_policy.refusal(stage, &camera);
        Ok((refusal.map(|_| camera), refusal))
    }

    pub(super) fn record(
        &self,
        sequence: u64,
        refusal: Option<&'static str>,
    ) -> Result<(), NativeFrameError> {
        let mut counts = self
            .counts
            .lock()
            .map_err(|_| NativeFrameError::WorkerState("adaptive route counters were poisoned"))?;
        if let Some(reason) = refusal {
            counts.routes.camera += 1;
            if counts
                .first_camera_sequence
                .is_none_or(|first| sequence < first)
            {
                counts.first_camera_sequence = Some(sequence);
                counts.routes.first_camera_reason = Some(reason);
            }
        } else {
            counts.routes.planar += 1;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
