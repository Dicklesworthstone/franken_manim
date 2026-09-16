//! Live worker output through Lumen's sole retained CPU renderer.

use fmn_frame::convert::rgba16f_to_rgba8;
use fmn_frame::{FrameBuffer, FrameLayout, PixelFormat};
use fmn_render::engine::journal as render_engine_journal;
use fmn_render::{Binning, RetainedFrameRenderer, RetainedFrameRendererConfig};
use fmn_scene::{RenderBackendRecord, RenderBackendRole};

use crate::{
    DebugLayerSet, DebugOverlaySnapshot, FrameEncoding, FramePayload, FrameStream, InspectorLimits,
    InspectorSnapshot, InspectorView, ServiceError, WorkerResponse, protocol_digest,
};

use super::program::execution_error;
use super::NativeSceneProgram;

pub(super) struct NativeRaster {
    renderer: RetainedFrameRenderer,
    rgba: FrameBuffer,
}

impl NativeRaster {
    pub(super) fn new(config: RetainedFrameRendererConfig) -> Result<Self, ServiceError> {
        let layout = FrameLayout::tight(PixelFormat::Rgba8, config.frame.viewport.width, config.frame.viewport.height)
            .map_err(execution_error)?;
        Ok(Self {
            renderer: RetainedFrameRenderer::new(config).map_err(execution_error)?,
            rgba: FrameBuffer::new(layout),
        })
    }

    pub(super) fn frame(&mut self, scene: &str, program: &NativeSceneProgram) -> Result<WorkerResponse, ServiceError> {
        self.renderer.render(program.preview().stage(), 0).map_err(execution_error)?;
        rgba16f_to_rgba8(self.renderer.frame(), &mut self.rgba).map_err(execution_error)?;
        let config = self.renderer.config();
        let width = config.frame.viewport.width;
        let height = config.frame.viewport.height;
        let bytes = fmn_codec::encode_rgba8(width, height, self.rgba.as_bytes(), fmn_codec::CompressionLevel::Fast);
        let digest = protocol_digest(&bytes);
        let backend = self.backend()?;
        Ok(WorkerResponse::Frame(FrameStream {
            scene: scene.to_owned(), frame_index: program.frame_index(), width, height, stride: 0,
            encoding: FrameEncoding::Png, payload: FramePayload::Pipe { bytes, digest },
            render_backends: vec![backend],
        }))
    }

    pub(super) fn backend(&self) -> Result<RenderBackendRecord, ServiceError> {
        let config = self.renderer.config();
        RenderBackendRecord::new(RenderBackendRole::FrameStream,
            render_engine_journal(config.engine, &config.frame, config.tiling)).map_err(execution_error)
    }

    pub(super) fn inspect(&self, program: &NativeSceneProgram, frame_count: u64, max_bytes: usize) -> Result<Vec<u8>, ServiceError> {
        let limits = InspectorLimits { max_json_bytes: max_bytes, ..InspectorLimits::default() };
        let mut snapshot = InspectorSnapshot::capture(program.preview().stage(), program.spans(), limits)
            .map_err(execution_error)?;
        let config = self.renderer.config();
        snapshot.view = Some(InspectorView::new(program.frame_index(), frame_count,
            program.preview().scene().fps(), config.frame.viewport, config.frame.map, true)
            .map_err(execution_error)?);
        snapshot.to_json(limits).map_err(execution_error)
    }

    pub(super) fn overlay(&mut self, program: &NativeSceneProgram, layers: DebugLayerSet, max_bytes: usize) -> Result<Vec<u8>, ServiceError> {
        self.renderer.render(program.preview().stage(), 0).map_err(execution_error)?;
        let config = self.renderer.config();
        let mut binning = Binning::build(self.renderer.plan(), config.frame.viewport, config.tiling, config.frame.map)
            .map_err(execution_error)?;
        binning.prune_occluded(self.renderer.plan()).map_err(execution_error)?;
        let limits = InspectorLimits { max_json_bytes: max_bytes, ..InspectorLimits::default() };
        DebugOverlaySnapshot::capture(program.preview().stage(), Some((&binning, config.frame.viewport)), layers, limits)
            .map_err(execution_error)?.to_json(limits).map_err(execution_error)
    }
}
