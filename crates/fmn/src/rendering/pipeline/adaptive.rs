//! One adaptive capture policy shared by serial and native-offloaded portals.

use std::sync::{Arc, Mutex};

use fmn_mobject::{Mob, ProgramKind, Stage};
use fmn_render::{Camera, CameraConfig};

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
    planar_map_matches: bool,
    light_mob: Option<Mob>,
    counts: Arc<Mutex<Counts>>,
}

impl NativeAdaptiveCamera {
    /// Snapshot the camera and the portal's existing planar-map admission.
    /// `light_mob` must refer to the segment's Stage if provided.
    #[must_use]
    pub fn new(camera: Camera, planar_map_matches: bool, light_mob: Option<Mob>) -> Self {
        Self {
            camera,
            planar_map_matches,
            light_mob,
            counts: Arc::new(Mutex::new(Counts::default())),
        }
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
        let frame = camera.frame();
        let baseline = CameraConfig::default().frame;
        let refusal = if !self.planar_map_matches {
            Some("output aspect differs from the default frame")
        } else if frame.center() != baseline.center()
            || frame.shape() != baseline.shape()
            || frame.orientation() != baseline.orientation()
            || frame.field_of_view() != baseline.field_of_view()
        {
            Some("camera frame differs from the default frame")
        } else {
            planar_stage_refusal(stage)
        };
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

/// Explain why captured geometry needs Lumen's camera route. The same rule
/// guards planar FMTL export and ordinary portal captures, so pure offload
/// cannot change accepted geometry or guess that a whole animation stays 2D.
#[must_use]
pub fn planar_stage_refusal(stage: &Stage) -> Option<&'static str> {
    for item in stage.draw_plan().items() {
        let Some(entry) = stage.get(item.mob) else {
            return Some("stale drawable");
        };
        let uniforms = entry.uniforms();
        if item.key.program != ProgramKind::Vector
            || uniforms.depth_test
            || uniforms.shading != [0.0; 3]
            || uniforms.clip_planes != [[0.0; 4]; 4]
        {
            return Some(
                "depth, raster primitives, lighting or clip planes require a camera-bearing bundle",
            );
        }
        if uniforms.is_fixed_in_frame != 0.0 || uniforms.scale_stroke_with_zoom {
            return Some("fixed-in-frame or zoom-scaled strokes require a camera-bearing bundle");
        }
        let placement = entry.placement();
        if let Some(points) = entry.buffer.read_column("point")
            && points
                .as_chunks::<3>()
                .0
                .iter()
                .any(|point| placement.apply_point(point.map(f64::from))[2] != 0.0)
        {
            return Some("nonplanar geometry requires a camera-bearing bundle");
        }
    }
    None
}

#[cfg(test)]
mod tests;
