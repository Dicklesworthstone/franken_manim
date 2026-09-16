//! Camera capture policy for the native worker. Projection and rasterization
//! remain owned by Lumen; this module only validates and journals their inputs.

use fmn_hash::{Schema, Writer};
use fmn_render::{Camera, CameraConfig, EngineIdentity, RetainedFrameRendererConfig, ScreenMap};
use fmn_scene::{RenderBackendRecord, RenderBackendRole};

use super::program::{execution_error, invalid};
use crate::ServiceError;

const CAMERA_CAPTURE_SCHEMA: Schema = Schema::new(*b"FMSC", 1, 1, 0);

pub(super) struct CameraCapture {
    pub camera: Camera,
    pub backend: RenderBackendRecord,
}

impl CameraCapture {
    pub fn new(
        config: CameraConfig,
        renderer: RetainedFrameRendererConfig,
    ) -> Result<Self, ServiceError> {
        let viewport = renderer.frame.viewport;
        if config.resolution != (viewport.width, viewport.height) {
            return Err(invalid("native camera resolution must match the output viewport"));
        }
        if config.background != renderer.frame.background {
            return Err(invalid("native camera background must match the output background"));
        }
        // render_with_camera executes ThreeDJob, not the affine fast-CPU or
        // accelerator engine selected by FrameJob. Never mislabel this route.
        if renderer.engine != EngineIdentity::certified() {
            return Err(invalid("native camera capture requires the certified CPU engine"));
        }
        let camera = Camera::new(config).map_err(execution_error)?;
        let mut identity = Writer::new(CAMERA_CAPTURE_SCHEMA);
        identity
            .put_str("lumen-retained-camera-cpu-v1")
            .put_str(&EngineIdentity::certified().closure_string())
            .put_u32(camera.pixel_width())
            .put_u32(camera.pixel_height())
            .put_u32(camera.fps())
            .put_u32(renderer.tiling.macro_tile)
            .put_u32(renderer.tiling.fine_tile)
            .put_u8(camera.samples())
            .put_f64(camera.max_allowable_norm());
        let frame = camera.frame();
        for value in frame.center() {
            identity.put_f64(value);
        }
        for value in frame.shape() {
            identity.put_f64(value);
        }
        for value in frame.orientation() {
            identity.put_f64(value);
        }
        identity.put_f64(frame.field_of_view());
        for value in camera.light_source_position() {
            identity.put_f64(value);
        }
        let background = camera.background();
        for value in [background.r, background.g, background.b, background.a] {
            identity.put_f64(value);
        }
        // Record the normalized Camera actually consumed by Lumen. Unused
        // affine map/AA settings and scheduler thread counts do not belong here.
        let backend = RenderBackendRecord::new(
            RenderBackendRole::FrameStream,
            identity.finish().map_err(execution_error)?,
        )
        .map_err(execution_error)?;
        Ok(Self { camera, backend })
    }

    pub fn view_map(&self) -> ScreenMap {
        // Nominal camera-plane scale for view metadata only. It is not an
        // inverse world-space projection, so pointer editing is not advertised.
        ScreenMap {
            scale: 1.0 / self.camera.pixel_size(),
            origin: [
                f64::from(self.camera.pixel_width()) * 0.5,
                f64::from(self.camera.pixel_height()) * 0.5,
            ],
        }
    }
}
