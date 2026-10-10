//! Registration of executable native Studio scenes. The factory runs only in
//! the disposable worker; the supervisor owns content identities, not callbacks.
use super::*;
use fmn_studio::native::{
    NativeReplayPolicy, NativeSceneProgram, NativeSceneWorker, NativeSegment, NativeWorkerConfig,
};

pub(super) fn selected(command: &RenderCommand) -> bool {
    command.file.as_deref() == Some(Path::new(BUILTIN_SCENE_SOURCE))
        && command.scene_names.iter().any(|name| {
            name == fmn::builtins::INTERACTIVE_SCENE_NAME
                || camera_route::studio_builtin(name).is_some()
        })
}

pub(super) fn validate_selection(command: &RenderCommand) -> Result<(), CliError> {
    if command.scene_names.len() != 1 || command.write_all {
        return Err(CliError::new(
            "scene",
            "live Studio requires exactly one registered scene",
        ));
    }
    if command.skip_animations || command.animation_range.is_some() || command.presenter_mode {
        return Err(CliError::new(
            "config",
            "live Studio uses its timeline controls, not offline skip/range/presenter flags",
        ));
    }
    Ok(())
}

fn configuration(
    fs: &dyn FileSystem,
    command: &StudioCommand,
) -> Result<(NativeWorkerConfig, fmn_scene::RuntimeConfig, u64), CliError> {
    validate_selection(&command.render)?;
    let config = resolve_render_config(fs, &command.render)?;
    let mut runtime = command.render.runtime_config(&config);
    runtime.windowed = true;
    // Use Proscenium's effective windowed FPS rather than a second clock policy.
    let fps = runtime.effective_fps();
    let name = &command.render.scene_names[0];
    let camera = camera_route::studio_builtin(name).is_some();
    let frame_count = if camera {
        let segment = fmn::animation::RationalFrameClock::new(fps)
            .and_then(|clock| clock.segment(camera_route::CAMERA_SCENE_RUN_TIME))
            .map_err(|error| CliError::new("config", error.to_string()))?;
        u64::try_from(segment.n_frames())
            .map_err(|_| internal("native camera scene has an invalid frame count"))?
            + 1
    } else {
        2 * u64::from(fps) + 1
    };
    if camera && config.render.aa != fmn_core::AaPolicy::Adaptive {
        return Err(CliError::new(
            "capability",
            "native camera Studio requires adaptive camera coverage",
        ));
    }
    let (plan, _, _) = derive_execution_plan_with_annex(
        fs,
        &config,
        fmn_runtime::RenderIntent::Preview,
        fmn_runtime::OutputPixelFormat::Rgba8,
        MetalSelection::Studio,
        camera,
    )?;
    let engine = match plan.engine {
        fmn_runtime::ExecutionEngine::CertifiedCpu => EngineIdentity::certified(),
        fmn_runtime::ExecutionEngine::FastCpu => EngineIdentity::fast(),
        _ => {
            return Err(CliError::new(
                "capability",
                "live native Studio currently requires a CPU renderer; no annex fallback is substituted",
            ));
        }
    };
    let renderer = RetainedFrameRendererConfig {
        frame: resolved_frame_config(&config)?,
        tiling: Tiling {
            macro_tile: plan.macro_tile,
            fine_tile: plan.fine_tile,
        },
        engine,
        threads: plan
            .render_teams
            .first()
            .map_or(1, fmn_runtime::TeamPlan::threads),
    };
    let build = fmn_studio::protocol_digest(BUILD_ID.as_bytes());
    // Inputs are all embedded or resolved before worker creation. No I/O, shared
    // mutable captures or arbitrary closure deserialization enters this factory.
    let mut closure = include_bytes!("studio_live.rs").to_vec();
    closure.extend_from_slice(include_bytes!("camera_route.rs"));
    closure.extend_from_slice(include_bytes!("../../fmn/src/lib.rs"));
    closure.extend_from_slice(SUITE_LOCK_BYTES);
    closure.extend_from_slice(build.as_bytes());
    closure.extend_from_slice(
        &config
            .canonical_bytes()
            .map_err(|error| internal(error.to_string()))?,
    );
    let mut native = NativeWorkerConfig::new(
        name,
        build,
        fmn_studio::protocol_digest(&closure),
        frame_count,
        fps,
        renderer,
    );
    native.replay = NativeReplayPolicy::ColdVerified;
    native.checkpoint_frames = command.checkpoint_frames;
    if camera {
        let mut base = camera_route::camera_config(&config)?;
        base.fps = fps;
        native.camera = Some(base);
    }
    Ok((native, runtime, config.determinism.seed))
}

pub(super) fn source_reads(
    fs: &dyn FileSystem,
    command: &StudioCommand,
) -> Result<Vec<AssetRead>, CliError> {
    if selected(&command.render) {
        let native = configuration(fs, command)?.0;
        let mut reads = native.base_input_reads();
        if let Some(base) = native.camera {
            let camera = fmn_render::Camera::new(base)
                .map_err(|error| CliError::new("config", error.to_string()))?;
            let backend = fmn_studio::native::camera_capture_backend(&camera, native.renderer)
                .map_err(|error| CliError::new("capability", error.to_string()))?;
            reads.push(AssetRead {
                path: "native/camera-capture-policy".into(),
                digest: backend.digest(),
            });
        }
        Ok(reads)
    } else if let Some(input) = camera_bundle_input(fs, command)? {
        let NativeRenderInput::Compiled {
            bundle,
            source_item,
            ..
        } = input
        else {
            return Err(internal("compiled camera selection changed kind"));
        };
        let view = camera_bundle_config(fs, command, &bundle)?;
        fmn_studio::camera_bundle::CameraBundleWorker::input_reads(
            fmn_studio::protocol_digest(BUILD_ID.as_bytes()),
            &compiled_source_read(source_item)?,
            &bundle,
            &view,
        )
        .map_err(|error| CliError::new("scene", error.to_string()))
    } else {
        Ok(Vec::new())
    }
}

pub(super) fn worker(
    fs: &dyn FileSystem,
    command: &StudioCommand,
) -> Result<Box<dyn fmn_studio::WorkerService>, CliError> {
    if !selected(&command.render) {
        if let Some(input) = camera_bundle_input(fs, command)? {
            let NativeRenderInput::Compiled {
                name,
                bundle,
                source_item,
                ..
            } = input
            else {
                return Err(internal("compiled camera selection changed kind"));
            };
            let view = camera_bundle_config(fs, command, &bundle)?;
            return Ok(Box::new(
                fmn_studio::camera_bundle::CameraBundleWorker::new(
                    name,
                    fmn_studio::protocol_digest(BUILD_ID.as_bytes()),
                    compiled_source_read(source_item)?,
                    *bundle,
                    view,
                )
                .map_err(|error| CliError::new("scene", error.to_string()))?,
            ));
        }
        return Ok(Box::new(NativeStudioWorker::from_command(fs, command)?));
    }
    let (config, runtime, seed) = configuration(fs, command)?;
    let limit = config.frame_count - 1;
    let camera_scene = camera_route::studio_builtin(&config.scene);
    let worker = NativeSceneWorker::new(config, move || {
        if let Some(program) = &camera_scene {
            return program.native_program(runtime.clone(), seed, limit);
        }
        let error = |error: String| {
            fmn_studio::ServiceError::new(fmn_studio::WorkerErrorCode::ExecutionFailed, error)
        };
        let mut scene =
            fmn_scene::Scene::new(runtime.clone(), seed).map_err(|e| error(e.to_string()))?;
        fmn::builtins::populate_interactive_canvas(scene.stage_mut())
            .map_err(|e| error(e.to_string()))?;
        NativeSceneProgram::new(
            scene,
            vec![NativeSegment::Wait {
                duration: Some(2.0),
            }],
            limit,
        )
    })
    .map_err(|error| CliError::new("scene", error.to_string()))?;
    Ok(Box::new(worker))
}

// Keep executable scene registration and immutable bundle playback separate.
// A recorded Python updater is already a captured picture, never live code in
// this worker. The ordinary resolver validates the actual source bytes once.
fn camera_bundle_input(
    fs: &dyn FileSystem,
    command: &StudioCommand,
) -> Result<Option<NativeRenderInput>, CliError> {
    if command.render.file.as_deref() == Some(Path::new(BUILTIN_SCENE_SOURCE)) {
        return Ok(None);
    }
    let input = resolve_native_render_input(fs, &command.render)?;
    if !matches!(&input, NativeRenderInput::Compiled { bundle, .. } if bundle.requires_camera()) {
        return Ok(None);
    }
    if command.render.skip_animations
        || command.render.animation_range.is_some()
        || command.render.presenter_mode
        || command.render.write_all
    {
        return Err(CliError::new(
            "config",
            "camera-bundle Studio uses recorded frames, not offline skip/range/presenter/batch flags",
        ));
    }
    Ok(Some(input))
}

fn compiled_source_read(source: ClosureItem) -> Result<AssetRead, CliError> {
    Ok(AssetRead {
        path: source
            .virtual_path
            .ok_or_else(|| internal("compiled Studio source omitted its closure path"))?,
        digest: source.digest,
    })
}

fn camera_bundle_config(
    fs: &dyn FileSystem,
    command: &StudioCommand,
    bundle: &TimelineBundle,
) -> Result<fmn_studio::camera_bundle::CameraBundleView, CliError> {
    let mut render = command.render.clone();
    if render.fps.is_some_and(|fps| fps != bundle.fps()) {
        return Err(CliError::new(
            "config",
            "requested FPS disagrees with the camera artifact's fixed clock",
        ));
    }
    render.fps = Some(bundle.fps());
    let config = resolve_render_config(fs, &render)?;
    if config.render.aa != fmn_core::AaPolicy::Adaptive {
        return Err(CliError::new(
            "capability",
            "camera-bundle Studio requires adaptive camera coverage",
        ));
    }
    let (plan, _, _) = derive_execution_plan_with_annex(
        fs,
        &config,
        fmn_runtime::RenderIntent::Preview,
        fmn_runtime::OutputPixelFormat::Rgba8,
        MetalSelection::Studio,
        true,
    )?;
    Ok(fmn_studio::camera_bundle::CameraBundleView {
        renderer: RetainedFrameRendererConfig {
            frame: resolved_frame_config(&config)?,
            tiling: Tiling {
                macro_tile: plan.macro_tile,
                fine_tile: plan.fine_tile,
            },
            engine: EngineIdentity::certified(),
            threads: plan
                .render_teams
                .first()
                .map_or(1, fmn_runtime::TeamPlan::threads),
        },
        seed: config.determinism.seed,
        fixed_camera: if bundle.has_camera_track() {
            None
        } else {
            Some(camera_route::camera(&config)?)
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn command() -> StudioCommand {
        let Invocation::Studio(command) = parse_args([
            "studio",
            "--resolution",
            "96x54",
            "--fps",
            "8",
            "--threads",
            "1",
            "@builtin",
            "interactive.v1",
        ])
        .unwrap() else {
            panic!("Studio");
        };
        command
    }
    #[test]
    fn live_registration_exposes_constructed_zero_and_native_windowed_clock() {
        let fs = fmn_platform::fs::VirtualFs::new();
        let command = command();
        let mut service = worker(&fs, &command).unwrap();
        let scene = fmn::builtins::INTERACTIVE_SCENE_NAME.to_owned();
        let fmn_studio::WorkerResponse::StudioData { bytes, .. } = service
            .handle(fmn_studio::SupervisorRequest::Inspect {
                scene: scene.clone(),
            })
            .unwrap()
        else {
            panic!("inspector");
        };
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("\"scene_time\":0"));
        assert!(text.contains("\"fps\":30"));
        assert!(text.contains("\"frame_count\":61"));
        assert!(text.contains("\"input_events\":true,\"input_revision\":0"));
        assert!(matches!(
            service
                .handle(fmn_studio::SupervisorRequest::Scrub {
                    scene: scene.clone(),
                    frame: 60
                })
                .unwrap(),
            fmn_studio::WorkerResponse::Frame(_)
        ));
        assert!(
            service
                .handle(fmn_studio::SupervisorRequest::Scrub { scene, frame: 61 })
                .is_err()
        );
        assert_eq!(source_reads(&fs, &command).unwrap().len(), 4);
    }
    #[test]
    fn live_factory_rejects_offline_skip_before_execution() {
        let mut command = command();
        command.render.skip_animations = true;
        assert!(worker(&fmn_platform::fs::VirtualFs::new(), &command).is_err());
    }

    fn camera_command(name: &str) -> StudioCommand {
        let mut result = command();
        result.render.scene_names = vec![name.to_owned()];
        result.render.resolution = Some(ResolutionChoice::Exact(64, 48));
        result
    }

    fn scrub_camera(
        service: &mut dyn fmn_studio::WorkerService,
        name: &str,
        frame: i64,
    ) -> fmn_studio::FrameStream {
        let fmn_studio::WorkerResponse::Frame(frame) = service
            .handle(fmn_studio::SupervisorRequest::Scrub {
                scene: name.into(),
                frame,
            })
            .unwrap()
        else {
            panic!("camera frame")
        };
        assert_eq!(frame.render_backends.len(), 1);
        assert!(
            frame.render_backends[0]
                .identity()
                .windows(b"lumen-retained-camera-cpu-v1".len())
                .any(|bytes| bytes == b"lumen-retained-camera-cpu-v1")
        );
        frame
    }

    fn rgba(frame: &fmn_studio::FrameStream) -> Vec<u8> {
        let fmn_studio::FramePayload::Pipe { bytes, digest } = &frame.payload else {
            panic!("inline camera PNG")
        };
        assert_eq!(fmn_studio::protocol_digest(bytes), *digest);
        fmn_codec::decode_png(bytes, &fmn_codec::PngLimits::default())
            .unwrap()
            .rgba
    }

    fn serial_camera(
        stage: &fmn::mobject::Stage,
        camera: &fmn_render::Camera,
        renderer: RetainedFrameRendererConfig,
    ) -> Vec<u8> {
        let mut renderer = RetainedFrameRenderer::new(renderer).unwrap();
        renderer.render_with_camera(stage, camera).unwrap();
        let (width, height) = camera.pixel_shape();
        let mut frame =
            FrameBuffer::new(FrameLayout::tight(PixelFormat::Rgba8, width, height).unwrap());
        rgba16f_to_rgba8(renderer.frame(), &mut frame).unwrap();
        frame.as_bytes().to_vec()
    }

    #[test]
    fn camera_builtins_reach_live_studio_with_real_clock_pixels_and_replay_inputs() {
        let fs = fmn_platform::fs::VirtualFs::new();
        for name in [
            "surface_cube.v1",
            "dot_cloud_depth.v1",
            "image_quad.v1",
            "mixed_camera.v1",
        ] {
            let command = camera_command(name);
            assert_eq!(studio_scene_name(&fs, &command.render).unwrap(), name);
            let (native, runtime, seed) = configuration(&fs, &command).unwrap();
            assert_eq!((native.fps, native.frame_count), (30, 9));
            let camera = fmn_render::Camera::new(native.camera.clone().unwrap()).unwrap();
            let mut capture = StudioPacketCapture::default();
            let mut program = camera_route::builtin(name).unwrap();
            let _ = fmn::run_scene(&mut program, runtime, seed, &mut capture).unwrap();
            assert_eq!(capture.packets.len(), 8);
            let expected: Vec<_> = capture
                .packets
                .iter()
                .map(|packet| serial_camera(&packet.materialize_stage(), &camera, native.renderer))
                .collect();
            assert_ne!(expected[0], expected[7]);
            assert_ne!(
                expected[0],
                serial_camera(&fmn::mobject::Stage::new(), &camera, native.renderer)
            );
            for threads in [1, 4] {
                let mut selected = command.clone();
                selected.render.common.threads = Some(threads);
                let expected_reads = source_reads(&fs, &selected).unwrap();
                assert_eq!(expected_reads.len(), 5);
                let mut service = worker(&fs, &selected).unwrap();
                for frame in [8, 1, 4, 8, 1] {
                    assert_eq!(
                        rgba(&scrub_camera(&mut *service, name, frame)),
                        expected[frame as usize - 1]
                    );
                }
                let initial = rgba(&scrub_camera(&mut *service, name, 0));
                assert_ne!(initial, expected[7]);
                assert!(service.journal_tail().is_empty());
                let fmn_studio::WorkerResponse::JournalSegment { journal, .. } = service
                    .handle(fmn_studio::SupervisorRequest::Play {
                        scene: name.into(),
                        command: fmn_studio::protocol::studio_seek_command(name, 4).unwrap(),
                    })
                    .unwrap()
                else {
                    panic!("committed camera seek")
                };
                let journal = fmn_scene::Journal::from_bytes(&journal).unwrap();
                assert_eq!(journal.entries().len(), 1);
                assert_eq!(journal.entries()[0].reads, expected_reads);
            }
        }
    }

    #[test]
    fn legacy_camera_bundles_reach_studio_without_a_camera_track_or_clock_retiming() {
        let fs = fmn_platform::fs::VirtualFs::new();
        for name in [
            "surface_cube.v1",
            "dot_cloud_depth.v1",
            "image_quad.v1",
            "mixed_camera.v1",
        ] {
            let mut recorder = fmn_scene::recording::SceneBundleRecorder::new_render_only(
                8,
                fmn_scene::BundleExportLimits::default(),
            )
            .unwrap();
            let mut program = camera_route::builtin(name).unwrap();
            let _ = fmn::run_scene(
                &mut program,
                fmn_scene::RuntimeConfig {
                    fps: 8,
                    ..fmn_scene::RuntimeConfig::default()
                },
                0,
                &mut recorder,
            )
            .unwrap();
            let source = recorder.finish().unwrap();
            let bundle = TimelineBundle::from_bytes(&source.bytes).unwrap();
            assert!(!bundle.has_camera_track());
            assert!(bundle.requires_camera());
            assert_eq!(bundle.frame_count(), 2);
            fs.insert("/camera.fmtl", source.bytes);
            let mut command = camera_command(name);
            command.render.file = Some(PathBuf::from("/camera.fmtl"));
            let view = camera_bundle_config(&fs, &command, &bundle).unwrap();
            let camera = view.fixed_camera.as_ref().unwrap();
            let expected: Vec<_> = (0..2)
                .map(|frame| serial_camera(&bundle.stage_at(frame).unwrap(), camera, view.renderer))
                .collect();
            assert_ne!(expected[0], expected[1]);
            let expected_reads = source_reads(&fs, &command).unwrap();
            assert_eq!(expected_reads.len(), 2);
            let mut service = worker(&fs, &command).unwrap();
            for frame in [1, 0, 1] {
                assert_eq!(
                    rgba(&scrub_camera(&mut *service, name, frame)),
                    expected[frame as usize]
                );
            }
            let fmn_studio::WorkerResponse::StudioData { bytes, .. } = service
                .handle(fmn_studio::SupervisorRequest::Inspect { scene: name.into() })
                .unwrap()
            else {
                panic!("camera inspection")
            };
            let info = String::from_utf8(bytes).unwrap();
            assert!(info.contains("\"fps\":8"));
            assert!(info.contains("\"frame_count\":2"));
            let fmn_studio::WorkerResponse::JournalSegment { journal, .. } = service
                .handle(fmn_studio::SupervisorRequest::Play {
                    scene: name.into(),
                    command: fmn_studio::protocol::studio_seek_command(name, 1).unwrap(),
                })
                .unwrap()
            else {
                panic!("compiled camera journal")
            };
            assert_eq!(
                fmn_scene::Journal::from_bytes(&journal).unwrap().entries()[0].reads,
                expected_reads
            );
        }
    }

    #[test]
    fn camera_studio_refuses_incompatible_engines_and_offline_flags_before_startup() {
        let fs = fmn_platform::fs::VirtualFs::new();
        let mut command = camera_command("mixed_camera.v1");
        command.render.engine = Some(EngineChoice::Metal);
        assert_eq!(
            source_reads(&fs, &command).unwrap_err().exit_name(),
            "capability"
        );
        assert!(worker(&fs, &command).is_err());
        command.render.engine = None;
        command.render.skip_animations = true;
        assert_eq!(
            source_reads(&fs, &command).unwrap_err().exit_name(),
            "config"
        );
        assert!(worker(&fs, &command).is_err());
    }
}
