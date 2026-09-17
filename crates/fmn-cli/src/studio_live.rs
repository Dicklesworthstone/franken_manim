//! Registration of executable native Studio scenes. The factory runs only in
//! the disposable worker; the supervisor owns content identities, not callbacks.
use super::*;
use fmn_studio::native::{
    NativeReplayPolicy, NativeSceneProgram, NativeSceneWorker, NativeSegment, NativeWorkerConfig,
};

pub(super) fn selected(command: &RenderCommand) -> bool {
    command.file.as_deref() == Some(Path::new(BUILTIN_SCENE_SOURCE))
        && command
            .scene_names
            .iter()
            .any(|name| name == fmn::builtins::INTERACTIVE_SCENE_NAME)
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
    let frame_count = 2 * u64::from(fps) + 1;
    let (plan, _, _) =
        derive_studio_execution_plan(fs, &config, fmn_runtime::OutputPixelFormat::Rgba8)?;
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
    closure.extend_from_slice(include_bytes!("../../fmn/src/lib.rs"));
    closure.extend_from_slice(SUITE_LOCK_BYTES);
    closure.extend_from_slice(build.as_bytes());
    closure.extend_from_slice(format!("{config:?}").as_bytes());
    let mut native = NativeWorkerConfig::new(
        fmn::builtins::INTERACTIVE_SCENE_NAME,
        build,
        fmn_studio::protocol_digest(&closure),
        frame_count,
        fps,
        renderer,
    );
    native.replay = NativeReplayPolicy::ColdVerified;
    native.checkpoint_frames = command.checkpoint_frames;
    Ok((native, runtime, config.determinism.seed))
}

pub(super) fn source_reads(
    fs: &dyn FileSystem,
    command: &StudioCommand,
) -> Result<Vec<AssetRead>, CliError> {
    if selected(&command.render) {
        Ok(configuration(fs, command)?.0.base_input_reads())
    } else {
        Ok(Vec::new())
    }
}

pub(super) fn worker(
    fs: &dyn FileSystem,
    command: &StudioCommand,
) -> Result<Box<dyn fmn_studio::WorkerService>, CliError> {
    if !selected(&command.render) {
        return Ok(Box::new(NativeStudioWorker::from_command(fs, command)?));
    }
    let (config, runtime, seed) = configuration(fs, command)?;
    let limit = config.frame_count - 1;
    let worker = NativeSceneWorker::new(config, move || {
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
}
