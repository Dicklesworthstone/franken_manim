//! Shipping CLI adapter to the same bounded CPU pipeline as native `render()`.
//!
//! The CLI retains ownership of sink finalization and artifact publication.
//! No scene callback is moved to a worker and no sampling loop is replaced.

mod pure;
pub(super) use pure::DispatchStats;

use fmn::rendering::{NativeFramePipeline, RenderError};
use fmn_output::EmitterHandle;
use fmn_render::RetainedFrameRendererConfig;
use fmn_runtime::{ExecutionPlan, PipelineStats};
use fmn_scene::{IntegrationError, timeline_bundle::TimelineFrameJob};

use super::CliError;

pub(super) struct CpuRenderer {
    pipeline: Option<NativeFramePipeline>,
}

impl CpuRenderer {
    pub(super) fn new_with_camera(
        plan: ExecutionPlan,
        config: RetainedFrameRendererConfig,
        output: EmitterHandle,
        camera: Option<fmn_render::Camera>,
        color_grade: fmn_frame::VideoColorGrade,
        hdr_pq: Option<fmn::HdrPqConfig>,
    ) -> Result<Self, CliError> {
        NativeFramePipeline::with_output_color_policy(
            plan,
            config,
            camera,
            output,
            fmn_render::PixelTileCache::DEFAULT_PREFIX_BUDGET_BYTES,
            0,
            0,
            color_grade,
            fmn::rendering::YuvConversionPolicy::Scoped,
            hdr_pq,
        )
        .map(|pipeline| Self {
            pipeline: Some(pipeline),
        })
        .map_err(|error| CliError::new("render", error.to_string()))
    }

    pub(super) fn capture(
        &mut self,
        stage: &fmn_scene::studio_bridge::Stage,
        sequence: u64,
    ) -> Result<(), IntegrationError> {
        let pipeline = self.pipeline.as_mut().ok_or_else(|| {
            IntegrationError::new("lumen", "CPU frame pipeline was already finalized")
        })?;
        let result = pipeline.capture(stage, sequence);
        self.capture_result(result)
    }

    pub(super) fn capture_compiled(
        &mut self,
        job: TimelineFrameJob,
        sequence: u64,
    ) -> Result<(), IntegrationError> {
        let pipeline = self.pipeline.as_mut().ok_or_else(|| {
            IntegrationError::new("lumen", "CPU frame pipeline was already finalized")
        })?;
        let result = pipeline.capture_compiled(job, sequence);
        self.capture_result(result)
    }

    pub(super) fn capture_packet(
        &mut self,
        packet: fmn::animation::FramePacket,
        sequence: u64,
    ) -> Result<(), IntegrationError> {
        let pipeline = self.pipeline.as_mut().ok_or_else(|| {
            IntegrationError::new("lumen", "CPU frame pipeline was already finalized")
        })?;
        let result = pipeline.capture_packet(packet, sequence);
        self.capture_result(result)
    }

    fn capture_result(&mut self, result: Result<(), RenderError>) -> Result<(), IntegrationError> {
        let Err(mut error) = result else {
            return Ok(());
        };
        // Source admission may only see Closed. Recover the actual failing
        // raster/conversion/emit stage before the outer scene hides it behind
        // IntegrationError. A geometric preparation error stays geometric.
        if matches!(&error, RenderError::Pipeline(_))
            && let Some(pipeline) = self.pipeline.take()
            && let Err(root) = pipeline.finish()
        {
            error = root;
        }
        Err(IntegrationError::new("lumen", error.to_string()))
    }

    pub(super) fn finish(&mut self) -> Result<PipelineStats, CliError> {
        self.pipeline
            .take()
            .ok_or_else(|| CliError::new("render", "CPU frame pipeline was already finalized"))?
            .finish()
            .map_err(|error| CliError::new("render", error.to_string()))
    }

    pub(super) fn abort(&mut self) {
        // NativeFramePipeline wakes the output ring before joining workers.
        drop(self.pipeline.take());
    }
}

impl super::RenderSink {
    /// Consume a saved capture after pipeline admission, without reconstructing
    /// a full owner-side arena for a final PNG or a full-packet observer.
    pub(super) fn render_packet(
        &mut self,
        packet: fmn::animation::FramePacket,
    ) -> Result<(), IntegrationError> {
        let _revision = u64::try_from(packet.frame_index())
            .map_err(|_| IntegrationError::new("lumen", "negative frame index"))?;
        match &mut self.renderer {
            super::OfflineFrameRenderer::Cpu(renderer) => {
                renderer.capture_packet(packet, self.next_sequence)?;
                self.backend.record_cpu_frame()?;
                self.next_sequence = self
                    .next_sequence
                    .checked_add(1)
                    .ok_or_else(|| IntegrationError::new("reel", "frame sequence exhausted"))?;
                Ok(())
            }
            #[cfg(feature = "metal")]
            super::OfflineFrameRenderer::Metal { .. } => {
                self.render_stage(&packet.materialize_stage(), _revision)
            }
        }
    }

    /// Submit immutable FMTL input; CPU reconstruction happens after admission
    /// on the assigned render team. Keep the annex's serial path explicit.
    pub(super) fn render_compiled(
        &mut self,
        job: TimelineFrameJob,
    ) -> Result<(), IntegrationError> {
        match &mut self.renderer {
            super::OfflineFrameRenderer::Cpu(renderer) => {
                renderer.capture_compiled(job, self.next_sequence)?;
                self.backend.record_cpu_frame()?;
                self.backend.dispatch.compiled_frame()?;
                self.backend.route =
                    if matches!(self.backend.route, "camera-cpu" | "compiled-camera-cpu") {
                        "compiled-camera-cpu"
                    } else {
                        "compiled-cpu"
                    };
                self.next_sequence = self
                    .next_sequence
                    .checked_add(1)
                    .ok_or_else(|| IntegrationError::new("reel", "frame sequence exhausted"))?;
                Ok(())
            }
            #[cfg(feature = "metal")]
            super::OfflineFrameRenderer::Metal { .. } => {
                let revision = u64::from(job.index()) + 1;
                let stage = job
                    .materialize()
                    .map_err(|error| IntegrationError::new("compiled", error.to_string()))?;
                self.render_stage(&stage, revision)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use fmn::mobject::{Mobject, RecordBuffer, RecordSchema, Stage};
    use fmn_codec::{PngLimits, decode_png};
    use fmn_platform::fs::VirtualFs;
    use fmn_platform::topology::HardwareTopology;
    use fmn_runtime::{ExecutionPlan, OutputPixelFormat, PlanRequest, RenderIntent, SurfaceSpec};

    fn config() -> fmn_config::Config {
        let mut config = fmn_config::Config::resolve(&[], None).unwrap().config;
        config.camera.resolution = (32, 24);
        config.camera.fps = 8;
        config.sizes.frame_height = 6.0;
        config.determinism.mode = fmn_config::config::DeterminismMode::Certified;
        config
    }

    fn plan(format: OutputPixelFormat, window: usize) -> ExecutionPlan {
        let mut surface = SurfaceSpec::lumen(32, 24);
        surface.working_bytes_per_pixel = if format == OutputPixelFormat::Rgba8 {
            16
        } else {
            20
        };
        ExecutionPlan::derive(
            PlanRequest::certified(RenderIntent::Offline, surface, format)
                .with_max_frames_in_flight(window),
            &HardwareTopology::from_group_sizes(&[2, 2]).unwrap(),
            None,
        )
        .unwrap()
    }

    fn shape() -> Mobject {
        let mut records = RecordBuffer::new(RecordSchema::vmobject(), 3).unwrap();
        records.write_range(
            "point",
            0,
            &[-1.5, -1.0, 0.0, 0.0, 1.25, 0.0, 1.5, -1.0, 0.0],
        );
        records.write_range("fill_rgba", 0, &[0.2, 0.6, 1.0, 1.0].repeat(3));
        Mobject::from_buffer(records)
    }

    fn observed_scene() -> (fmn_scene::Scene, fmn::mobject::Mob, impl Fn() -> usize) {
        let mut scene =
            fmn_scene::Scene::new(fmn_scene::RuntimeConfig::from_config(&config()), 17).unwrap();
        let visible = scene.stage_mut().add(shape());
        scene.stage_mut().add_to_scene(visible).unwrap();
        // This exercises the per-frame owner path. A pure segment correctly
        // retains its own begin-state checkpoint, independently of capture.
        scene
            .stage_mut()
            .add_dt_updater(visible, |_, _, _| {}, false)
            .unwrap();
        let image = fmn::mobject::ImageResource::rgba8(
            1,
            1,
            vec![1, 2, 3, 255],
            fmn::mobject::ImageColorSpace::Srgb,
            fmn::mobject::ImageSampler::default(),
        )
        .unwrap();
        let pixels = image.shared_pixels();
        let mut detached = shape();
        detached.image = Some(image);
        scene.stage_mut().add(detached);
        // A full Stage snapshot retains an additional image owner in the
        // detached entry's snapshot cache. Rooted capture never visits it.
        (scene, visible, move || Arc::strong_count(&pixels))
    }

    /// A custom observer keeps the public full-packet default, independently
    /// of the native sink's optimized capture entry point.
    struct PacketObserver<'a>(&'a mut dyn SceneSink);

    impl SceneSink for PacketObserver<'_> {
        fn event(&mut self, event: LifecycleEvent) -> Result<(), IntegrationError> {
            self.0.event(event)
        }

        fn capture(
            &mut self,
            reason: CaptureReason,
            packet: fmn::animation::FramePacket,
        ) -> Result<(), IntegrationError> {
            self.0.capture(reason, packet)
        }
    }

    struct CaptureAudit<'a> {
        inner: &'a mut dyn SceneSink,
        metadata: Vec<i64>,
        frames: Vec<fmn::animation::FramePacket>,
        #[cfg(feature = "batch")]
        cancel_after_metadata: Option<(usize, &'a RenderCancellation)>,
    }

    impl<'a> CaptureAudit<'a> {
        fn new(inner: &'a mut dyn SceneSink) -> Self {
            Self {
                inner,
                metadata: Vec::new(),
                frames: Vec::new(),
                #[cfg(feature = "batch")]
                cancel_after_metadata: None,
            }
        }
    }

    impl SceneSink for CaptureAudit<'_> {
        fn capture_state_required(&self) -> bool {
            self.inner.capture_state_required()
        }

        fn capture_final_state_required(&self) -> bool {
            self.inner.capture_final_state_required()
        }

        fn event(&mut self, event: LifecycleEvent) -> Result<(), IntegrationError> {
            self.inner.event(event)
        }

        fn capture_metadata(
            &mut self,
            reason: CaptureReason,
            metadata: fmn::animation::FrameMetadata,
        ) -> Result<(), IntegrationError> {
            self.metadata.push(metadata.frame_index());
            self.inner.capture_metadata(reason, metadata)?;
            #[cfg(feature = "batch")]
            if let Some((count, cancellation)) = self.cancel_after_metadata
                && self.metadata.len() == count
            {
                cancellation.request(RENDER_CANCEL_FAIL_FAST);
            }
            Ok(())
        }

        fn capture_stage(
            &mut self,
            reason: CaptureReason,
            stage: &Stage,
            metadata: fmn::animation::FrameMetadata,
        ) -> Result<(), IntegrationError> {
            self.frames
                .push(fmn::animation::FramePacket::freeze_metadata(
                    stage, metadata,
                ));
            self.inner.capture_stage(reason, stage, metadata)
        }

        fn capture(
            &mut self,
            _: CaptureReason,
            _: fmn::animation::FramePacket,
        ) -> Result<(), IntegrationError> {
            panic!("the driver must preserve the requested stage capture boundary")
        }
    }

    fn assert_subdivided_png_matches_packet(
        fs: &Arc<VirtualFs>,
        config: &fmn_config::Config,
        plan: &ExecutionPlan,
        packet: fmn::animation::FramePacket,
    ) {
        let mut reference = RenderSink::new(
            fs.clone(),
            config,
            plan,
            &RenderTarget::Native(NativeFrameFormat::Png),
            "/expected.png".into(),
        )
        .unwrap();
        reference.capture(CaptureReason::Segment, packet).unwrap();
        assert_eq!(reference.finish(None).unwrap().backend.frames, 1);
        assert_eq!(
            fs.read(Path::new("/clips/observed/00000.png")).unwrap(),
            fs.read(Path::new("/expected.png")).unwrap()
        );
    }

    #[test]
    fn cli_and_batch_capture_rooted_frames_without_retaining_detached_images() {
        for camera in [false, true] {
            let mut expected = None;
            for route in ["observer", "native", "batch"] {
                let fs = Arc::new(VirtualFs::new());
                let mut sink = RenderSink::new_with_camera(
                    fs.clone(),
                    &config(),
                    &plan(OutputPixelFormat::Rgba8, 4),
                    &RenderTarget::Native(NativeFrameFormat::PngSequence),
                    "/frames".into(),
                    camera.then(|| camera_route::camera(&config()).unwrap()),
                )
                .unwrap();
                let cancellation = RenderCancellation::default();
                cancellation.register_emitter(sink.emitter_handle().unwrap());
                let (mut scene, visible, snapshots) = observed_scene();
                for _ in 0..3 {
                    match route {
                        "observer" => scene.show(&mut PacketObserver(&mut sink)),
                        "batch" => scene.show(&mut CancellableSceneSink {
                            inner: &mut sink,
                            cancellation: &cancellation,
                        }),
                        _ => scene.show(&mut sink),
                    }
                    .unwrap();
                    // The admitted picture must remain immutable while the
                    // owner changes the next frame and workers are running.
                    scene.stage_mut().shift(visible, [0.5, 0.0, 0.0]);
                }
                if route == "observer" {
                    assert!(
                        snapshots() > 2,
                        "the full-packet control must retain detached image storage"
                    );
                } else {
                    assert_eq!(snapshots(), 2, "{route}, camera={camera}");
                }
                drop(scene);
                let finished = sink.finish(None).unwrap();
                assert_eq!(finished.backend.frames, 3);
                assert_eq!(finished.backend.pipeline.unwrap().outstanding_slots, 0);
                let frames: Vec<_> = (0..3)
                    .map(|index| {
                        fs.read(Path::new(&format!("/frames/frame_{index:06}.png")))
                            .unwrap()
                    })
                    .collect();
                assert_ne!(frames[0], frames[2]);
                match &expected {
                    Some(expected) => assert_eq!(&frames, expected, "{route}, camera={camera}"),
                    None => expected = Some(frames),
                }
            }
        }
    }

    #[test]
    fn batch_adapter_preserves_final_only_capture_and_cancellation_demand() {
        #[derive(Default)]
        struct FinalOnly {
            metadata: Vec<i64>,
            frames: Vec<(CaptureReason, i64)>,
        }
        impl SceneSink for FinalOnly {
            fn capture_state_required(&self) -> bool {
                false
            }
            fn capture_final_state_required(&self) -> bool {
                true
            }
            fn capture_metadata(
                &mut self,
                _: CaptureReason,
                metadata: fmn::animation::FrameMetadata,
            ) -> Result<(), IntegrationError> {
                self.metadata.push(metadata.frame_index());
                Ok(())
            }
            fn capture_stage(
                &mut self,
                reason: CaptureReason,
                _: &Stage,
                metadata: fmn::animation::FrameMetadata,
            ) -> Result<(), IntegrationError> {
                self.frames.push((reason, metadata.frame_index()));
                Ok(())
            }
            fn capture(
                &mut self,
                _: CaptureReason,
                _: fmn::animation::FramePacket,
            ) -> Result<(), IntegrationError> {
                panic!("batch forwarding must preserve the sink's capture boundary")
            }
        }
        let cancellation = RenderCancellation::default();
        let mut sink = FinalOnly::default();
        let (mut scene, _, snapshots) = observed_scene();
        let mut adapter = CancellableSceneSink {
            inner: &mut sink,
            cancellation: &cancellation,
        };
        scene.wait_frames(3, &mut adapter).unwrap();
        scene.show(&mut adapter).unwrap();
        assert_eq!(snapshots(), 2);
        #[cfg(feature = "batch")]
        {
            cancellation.request(RENDER_CANCEL_FAIL_FAST);
            assert!(!adapter.capture_state_required());
            assert!(!adapter.capture_final_state_required());
            assert!(scene.show(&mut adapter).is_err());
        }
        assert_eq!(sink.metadata, [1, 2]);
        assert_eq!(
            sink.frames,
            [(CaptureReason::Segment, 3), (CaptureReason::Show, 3)]
        );
        assert_eq!(snapshots(), 2);
    }

    #[test]
    fn subdivided_streams_forward_rooted_capture_and_ignore_outside_segment_barriers() {
        let mut expected = None;
        for full_packets in [true, false] {
            let config = config();
            let plan = plan(OutputPixelFormat::Rgba8, 4);
            let target = RenderTarget::Native(NativeFrameFormat::PngSequence);
            let naming = OutputNaming {
                output_directory: "/clips".into(),
                ..OutputNaming::default()
            };
            let fs = Arc::new(VirtualFs::new());
            let mut sink = SubdividedRenderSink::new(
                fs.clone(),
                SubdivisionContext {
                    config: &config,
                    plan: &plan,
                    target: &target,
                    naming: &naming,
                    cancellation: None,
                    camera: None,
                    soundtrack: None,
                },
                "observed",
                2,
            )
            .unwrap();
            let (mut scene, visible, snapshots) = observed_scene();
            assert!(!sink.capture_state_required());
            scene.show(&mut sink).unwrap();
            assert_eq!(snapshots(), 2);
            for frames in [2, 1] {
                if full_packets {
                    scene
                        .wait_frames(frames, &mut PacketObserver(&mut sink))
                        .unwrap();
                } else {
                    scene.wait_frames(frames, &mut sink).unwrap();
                }
                scene.stage_mut().shift(visible, [0.5, 0.0, 0.0]);
                scene.show(&mut sink).unwrap();
            }
            if full_packets {
                assert!(snapshots() > 2);
            } else {
                assert_eq!(snapshots(), 2);
            }
            let artifacts = sink.into_artifacts().unwrap();
            assert_eq!(artifacts.len(), 2);
            assert_eq!(artifacts[0].1.backend.frames, 2);
            assert_eq!(artifacts[1].1.backend.frames, 1);
            let frames: Vec<_> = [(0, 0), (0, 1), (1, 0)]
                .into_iter()
                .map(|(clip, frame)| {
                    fs.read(Path::new(&format!(
                        "/clips/observed/{clip:05}/frame_{frame:06}.png"
                    )))
                    .unwrap()
                })
                .collect();
            assert_ne!(frames[0], frames[2]);
            match &expected {
                Some(expected) => assert_eq!(&frames, expected),
                None => expected = Some(frames),
            }
        }
    }

    #[test]
    fn subdivided_png_captures_only_the_final_sample_before_removal_and_cleanup() {
        use fmn::animation::{AnimConfig, RateFunc, Transform};

        let config = config();
        let plan = plan(OutputPixelFormat::Rgba8, 4);
        let target = RenderTarget::Native(NativeFrameFormat::Png);
        let naming = OutputNaming {
            output_directory: "/clips".into(),
            ..OutputNaming::default()
        };
        let fs = Arc::new(VirtualFs::new());
        let mut sink = SubdividedRenderSink::new(
            fs.clone(),
            SubdivisionContext {
                config: &config,
                plan: &plan,
                target: &target,
                naming: &naming,
                cancellation: None,
                camera: None,
                soundtrack: None,
            },
            "observed",
            1,
        )
        .unwrap();
        let (mut scene, visible, _) = observed_scene();
        let marker = scene.stage_mut().add(shape());
        scene.stage_mut().add_to_scene(marker).unwrap();
        scene
            .stage_mut()
            .add_dt_updater(
                marker,
                |stage, marker, dt| {
                    if dt == 0.0 {
                        stage.shift(marker, [0.0, 0.5, 0.0]);
                    }
                },
                false,
            )
            .unwrap();
        let end = scene.stage_mut().copy_family(visible).unwrap();
        scene.stage_mut().shift(end, [1.5, 0.0, 0.0]);
        let animation = Transform::new(visible, end).with_config(AnimConfig {
            run_time: 8.0,
            rate_func: RateFunc::linear(),
            remover: true,
            ..AnimConfig::default()
        });
        let mut audit = CaptureAudit::new(&mut sink);
        scene
            .play(
                vec![Box::new(animation)],
                fmn_scene::PlayOverrides::default(),
                &mut audit,
            )
            .unwrap();
        assert_eq!(audit.metadata, (1..64).collect::<Vec<_>>());
        assert_eq!(audit.frames.len(), 1);
        let packet = audit.frames.pop().unwrap();
        assert_eq!(packet.frame_index(), 64);
        let frozen = packet.materialize_stage();
        assert!(frozen.roots().contains(&visible));
        assert!(!scene.stage().roots().contains(&visible));
        assert_eq!(frozen.get_center(visible)[0], 1.5);
        assert_eq!(
            scene.stage().get_center(marker)[1] - frozen.get_center(marker)[1],
            0.5
        );
        drop(audit);
        assert!(!sink.capture_state_required());
        assert!(!sink.capture_final_state_required());
        let artifacts = sink.into_artifacts().unwrap();
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].1.backend.frames, 1);
        assert_eq!(
            artifacts[0]
                .1
                .backend
                .pipeline
                .as_ref()
                .unwrap()
                .outstanding_slots,
            0
        );
        assert_subdivided_png_matches_packet(&fs, &config, &plan, packet);

        // Ordinary PNG uses show after scene completion. Its post-cleanup
        // picture must remain distinct from subdivision's final played sample.
        let mut post_cleanup = RenderSink::new(
            fs.clone(),
            &config,
            &plan,
            &target,
            "/post-cleanup.png".into(),
        )
        .unwrap();
        scene.show(&mut post_cleanup).unwrap();
        post_cleanup.finish(None).unwrap();
        assert_ne!(
            fs.read(Path::new("/clips/observed/00000.png")).unwrap(),
            fs.read(Path::new("/post-cleanup.png")).unwrap()
        );
    }

    #[test]
    fn subdivided_png_keeps_the_early_stop_sample_and_rejects_empty_segments() {
        let config = config();
        let plan = plan(OutputPixelFormat::Rgba8, 4);
        let target = RenderTarget::Native(NativeFrameFormat::Png);
        let naming = OutputNaming {
            output_directory: "/clips".into(),
            ..OutputNaming::default()
        };
        let fs = Arc::new(VirtualFs::new());
        let mut sink = SubdividedRenderSink::new(
            fs.clone(),
            SubdivisionContext {
                config: &config,
                plan: &plan,
                target: &target,
                naming: &naming,
                cancellation: None,
                camera: None,
                soundtrack: None,
            },
            "observed",
            1,
        )
        .unwrap();
        let (mut scene, visible, _) = observed_scene();
        scene
            .stage_mut()
            .add_dt_updater(
                visible,
                |stage, visible, dt| {
                    stage.shift(visible, [dt, 0.0, 0.0]);
                },
                false,
            )
            .unwrap();
        let mut audit = CaptureAudit::new(&mut sink);
        scene
            .wait_until_frames(64, &mut |stage| stage.time() >= 0.375, &mut audit)
            .unwrap();
        assert!(audit.metadata.is_empty());
        assert_eq!(
            audit
                .frames
                .iter()
                .map(|packet| packet.frame_index())
                .collect::<Vec<_>>(),
            [1, 2, 3]
        );
        let packet = audit.frames.pop().unwrap();
        assert_eq!(packet.materialize_stage().get_center(visible)[0], 0.375);
        drop(audit);
        let Err(empty) = scene.wait_frames(0, &mut sink) else {
            panic!("an empty segment cannot produce a final-state PNG");
        };
        assert!(empty.to_string().contains("zero-frame segment"), "{empty}");
        assert!(!fs.exists(Path::new("/clips/observed/00001.png")));
        let artifacts = sink.into_artifacts().unwrap();
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].1.backend.frames, 1);
        assert_subdivided_png_matches_packet(&fs, &config, &plan, packet);
    }

    #[cfg(feature = "batch")]
    #[test]
    fn cancelled_subdivided_png_stops_during_metadata_without_freezing_or_publishing() {
        let config = config();
        let plan = plan(OutputPixelFormat::Rgba8, 4);
        let target = RenderTarget::Native(NativeFrameFormat::Png);
        let naming = OutputNaming {
            output_directory: "/clips".into(),
            ..OutputNaming::default()
        };
        let fs = Arc::new(VirtualFs::new());
        let cancellation = RenderCancellation::default();
        let mut sink = SubdividedRenderSink::new(
            fs.clone(),
            SubdivisionContext {
                config: &config,
                plan: &plan,
                target: &target,
                naming: &naming,
                cancellation: Some(&cancellation),
                camera: None,
                soundtrack: None,
            },
            "observed",
            1,
        )
        .unwrap();
        let (mut scene, _, detached_owners) = observed_scene();
        let mut audit = CaptureAudit::new(&mut sink);
        audit.cancel_after_metadata = Some((3, &cancellation));
        let mut adapter = CancellableSceneSink {
            inner: &mut audit,
            cancellation: &cancellation,
        };
        assert!(scene.wait_frames(64, &mut adapter).is_err());
        assert!(!adapter.capture_state_required());
        assert!(!adapter.capture_final_state_required());
        assert_eq!(audit.metadata, [1, 2, 3]);
        assert!(audit.frames.is_empty());
        assert_eq!(detached_owners(), 2);
        drop(audit);
        drop(sink);
        assert!(!fs.exists(Path::new("/clips/observed/00000.png")));
    }

    #[test]
    fn compiled_jobs_reconstruct_on_all_cli_render_teams() {
        let mut stage = Stage::new();
        let mob = stage.add(shape());
        stage.add_to_scene(mob).unwrap();
        let mut timeline = fmn::animation::Timeline::new(8).unwrap();
        timeline.wait(1.0).unwrap();
        let bytes = fmn_scene::export_timeline_bundle(timeline, &mut stage, &RngRoot::from_seed(0))
            .unwrap();
        let shared = fmn_scene::timeline_bundle::SharedTimelineBundle::from_bytes(&bytes).unwrap();
        let fs = Arc::new(VirtualFs::new());
        let plan = plan(OutputPixelFormat::Rgba8, 4);
        assert_eq!(plan.render_teams.len(), 2);
        let mut sink = RenderSink::new(
            fs,
            &config(),
            &plan,
            &RenderTarget::Native(NativeFrameFormat::PngSequence),
            PathBuf::from("/compiled"),
        )
        .unwrap();
        for index in 0..shared.frame_count() {
            sink.render_compiled(shared.frame_job(index).unwrap())
                .unwrap();
        }
        let finished = sink.finish(None).unwrap();
        assert_eq!(finished.backend.route, "compiled-cpu");
        let stats = finished.backend.pipeline.unwrap();
        assert_eq!(
            (stats.submitted, stats.emitted, stats.outstanding_slots),
            (8, 8, 0)
        );
        assert_eq!(stats.render_team_frames.len(), 2);
        assert!(stats.render_team_frames.iter().all(|n| *n > 0));
    }

    #[test]
    fn compiled_camera_refusal_occurs_on_worker_not_serial_capture() {
        let mut stage = Stage::new();
        let mob = stage.add(Mobject::from(fmn::library::Cube::new(1.0)));
        stage.add_to_scene(mob).unwrap();
        let mut timeline = fmn::animation::Timeline::new(8).unwrap();
        timeline.wait(0.125).unwrap();
        let bytes = fmn_scene::export_timeline_bundle(timeline, &mut stage, &RngRoot::from_seed(0))
            .unwrap();
        let shared = fmn_scene::timeline_bundle::SharedTimelineBundle::from_bytes(&bytes).unwrap();
        let fs = Arc::new(VirtualFs::new());
        let mut sink = RenderSink::new(
            fs.clone(),
            &config(),
            &plan(OutputPixelFormat::Rgba8, 1),
            &RenderTarget::Native(NativeFrameFormat::PngSequence),
            PathBuf::from("/refused"),
        )
        .unwrap();
        // Ordinary affine capture refuses Cube before queueing. The compiled
        // route must admit its input and let the worker discover that refusal.
        sink.render_compiled(shared.frame_job(0).unwrap()).unwrap();
        assert!(sink.finish(None).is_err());
        assert!(!fs.exists(Path::new("/refused")));
    }

    #[test]
    fn cli_sink_uses_all_teams_and_matches_live_serial_pixels() {
        let config = config();
        for window in [1, 2, 4] {
            let plan = plan(OutputPixelFormat::Rgba8, window);
            let fs = Arc::new(VirtualFs::new());
            let mut sink = RenderSink::new(
                fs.clone(),
                &config,
                &plan,
                &RenderTarget::Native(NativeFrameFormat::PngSequence),
                PathBuf::from("/frames"),
            )
            .unwrap();
            let mut serial = RetainedFrameRenderer::new(RetainedFrameRendererConfig {
                frame: resolved_frame_config(&config).unwrap(),
                tiling: Tiling {
                    macro_tile: plan.macro_tile,
                    fine_tile: plan.fine_tile,
                },
                engine: EngineIdentity::certified(),
                threads: 1,
            })
            .unwrap();
            let mut stage = Stage::new();
            let mob = stage.add(shape());
            stage.add_to_scene(mob).unwrap();
            let mut expected = Vec::new();
            for index in 0..12 {
                serial.render(&stage, 0).unwrap();
                let mut pixels =
                    FrameBuffer::new(FrameLayout::tight(PixelFormat::Rgba8, 32, 24).unwrap());
                rgba16f_to_rgba8(serial.frame(), &mut pixels).unwrap();
                expected.push(pixels.as_bytes().to_vec());
                sink.render_stage(&stage, index).unwrap();
                stage.shift(mob, [0.1, 0.0, 0.0]);
            }
            drop(stage);
            let finished = sink.finish(None).unwrap();
            let stats = finished.backend.pipeline.unwrap();
            assert_eq!((stats.submitted, stats.emitted), (12, 12));
            assert_eq!(stats.outstanding_slots, 0);
            assert!(stats.max_in_flight <= plan.frames_in_flight);
            assert_eq!(stats.render_team_frames.len(), plan.render_teams.len());
            assert!(stats.render_team_frames.iter().all(|n| *n > 0));
            for (index, expected) in expected.iter().enumerate() {
                let bytes = fs
                    .read(Path::new(&format!("/frames/frame_{index:06}.png")))
                    .unwrap();
                assert_eq!(
                    &decode_png(&bytes, &PngLimits::default()).unwrap().rgba,
                    expected
                );
            }
        }
    }

    #[test]
    fn gif_and_y4m_bytes_are_identical_across_cli_windows() {
        for (format, output) in [
            (NativeFrameFormat::Gif, OutputPixelFormat::Rgba8),
            (NativeFrameFormat::Y4m, OutputPixelFormat::Nv12),
        ] {
            let mut expected = None;
            for window in [1, 2, 4] {
                let fs = Arc::new(VirtualFs::new());
                let mut sink = RenderSink::new(
                    fs.clone(),
                    &config(),
                    &plan(output, window),
                    &RenderTarget::Native(format),
                    PathBuf::from("/movie"),
                )
                .unwrap();
                let mut stage = Stage::new();
                let mob = stage.add(shape());
                stage.add_to_scene(mob).unwrap();
                for index in 0..8 {
                    sink.render_stage(&stage, index).unwrap();
                    stage.shift(mob, [0.1, 0.0, 0.0]);
                }
                let finished = sink.finish(None).unwrap();
                assert_eq!(finished.backend.pipeline.unwrap().emitted, 8);
                let bytes = fs.read(Path::new("/movie")).unwrap();
                if let Some(expected) = &expected {
                    assert_eq!(&bytes, expected);
                } else {
                    expected = Some(bytes);
                }
            }
        }
    }

    #[test]
    fn occupied_destinations_and_competing_writers_are_preserved_at_commit() {
        for format in [
            NativeFrameFormat::Png,
            NativeFrameFormat::Gif,
            NativeFrameFormat::Y4m,
        ] {
            for competing in [false, true] {
                let fs = Arc::new(VirtualFs::new());
                let output = if matches!(format, NativeFrameFormat::Y4m) {
                    OutputPixelFormat::Nv12
                } else {
                    OutputPixelFormat::Rgba8
                };
                if !competing {
                    fs.insert("/movie", b"foreign generation".to_vec());
                }
                let mut sink = RenderSink::new(
                    fs.clone(),
                    &config(),
                    &plan(output, 2),
                    &RenderTarget::Native(format),
                    PathBuf::from("/movie"),
                )
                .unwrap();
                sink.render_stage(&Stage::new(), 0).unwrap();
                if competing {
                    fs.insert("/movie", b"foreign generation".to_vec());
                }
                assert!(sink.finish(None).is_err());
                assert_eq!(fs.read(Path::new("/movie")).unwrap(), b"foreign generation");
            }
        }
    }

    #[test]
    fn dropping_or_unwinding_cli_capture_joins_before_same_destination_retry() {
        for panic in [false, true] {
            let fs = Arc::new(VirtualFs::new());
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut sink = RenderSink::new(
                    fs.clone(),
                    &config(),
                    &plan(OutputPixelFormat::Rgba8, 4),
                    &RenderTarget::Native(NativeFrameFormat::PngSequence),
                    PathBuf::from("/retry"),
                )
                .unwrap();
                for index in 0..3 {
                    sink.render_stage(&Stage::new(), index).unwrap();
                }
                if panic {
                    panic!("scene failure after queued captures");
                }
                drop(sink);
            }));
            assert_eq!(result.is_err(), panic);
            assert!(!fs.exists(Path::new("/retry")));
            let mut retry = RenderSink::new(
                fs.clone(),
                &config(),
                &plan(OutputPixelFormat::Rgba8, 4),
                &RenderTarget::Native(NativeFrameFormat::PngSequence),
                PathBuf::from("/retry"),
            )
            .unwrap();
            retry.render_stage(&Stage::new(), 0).unwrap();
            assert_eq!(retry.finish(None).unwrap().backend.frames, 1);
        }
    }
}
