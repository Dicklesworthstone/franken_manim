//! Narrated compiled jobs on the same Asupersync blocking-pool mechanism as
//! the native batch front door. One FMTL contains one scene, even when it is
//! subdivided. Clip count never masquerades as parallel scene count.

use std::sync::mpsc;

use fmn_cli::BatchCommand;

use super::control::{Control, GuardedFs, GuardedRunner};
use super::*;

/// Freeze the job's outcome before sending its terminal report and joining the
/// worker. A deadline observed during later cleanup cannot replace that outcome.
struct CompletedJob {
    result: Result<(), Failure>,
    stdout: String,
    cancelled: bool,
}

impl CompletedJob {
    fn capture(result: Result<(), Failure>, stdout: String, control: &Control) -> Self {
        let cancelled = result.is_err() && control.is_cancelled();
        Self {
            result,
            stdout,
            cancelled,
        }
    }

    fn into_output(self, command: &BatchCommand, scene: &str) -> RunOutput {
        terminal(command, scene, self.result, self.stdout, self.cancelled)
    }
}

pub(super) fn run(
    command: BatchCommand,
    bytes: &[u8],
    bundle: TimelineBundle,
    fs: Arc<dyn FileSystem>,
    capability: &dyn Fn() -> Option<FfmpegCapability>,
) -> RunOutput {
    // Admission and startup failures are terminal outcomes of the selected
    // scene too. Robot consumers must not wait for a batch record forever just
    // because no blocking task was started.
    let initial_failure = |error| {
        terminal(
            &command,
            fallback_name(&command.render),
            Err(error),
            String::new(),
            false,
        )
    };
    if command.max_scenes == Some(0) {
        return initial_failure(Failure::new(
            "config",
            "--max-scenes must be greater than zero",
        ));
    }
    let control = match Control::new(command.budget_ms) {
        Ok(control) => control,
        Err(error) => return initial_failure(error),
    };
    let fs: Arc<dyn FileSystem> = Arc::new(GuardedFs {
        inner: fs,
        control: Arc::clone(&control),
    });
    if let Err(error) = control.checkpoint() {
        return terminal(
            &command,
            fallback_name(&command.render),
            Err(error),
            String::new(),
            true,
        );
    }
    let prepared = match replay::prepare_replay(
        &command.render,
        bytes,
        bundle,
        fs.as_ref(),
        command.manifest_dir.as_deref(),
    ) {
        Ok(prepared) => prepared,
        Err(error) => {
            return CompletedJob::capture(Err(error), String::new(), &control)
                .into_output(&command, fallback_name(&command.render));
        }
    };
    let scene = prepared.request.scene.clone();
    if let Err(error) = control.checkpoint() {
        return terminal(&command, &scene, Err(error), String::new(), true);
    }
    // Native WAV never asks for a locator/runner, even in a batch. Snapshot
    // the optional host capability once; don't discover tools on each clip.
    let ffmpeg = if matches!(prepared.request.target, Target::Video(_)) {
        capability().map(|mut capability| {
            capability.runner = Arc::new(GuardedRunner {
                inner: capability.runner,
                control: Arc::clone(&control),
            });
            capability
        })
    } else {
        None
    };
    let runtime = match asupersync::runtime::RuntimeBuilder::current_thread()
        .blocking_threads(1, 1)
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            return initial_failure(Failure::new(
                "internal",
                format!("batch runtime initialization failed: {error}"),
            ));
        }
    };
    let (sender, receiver) = mpsc::sync_channel(1);
    let task_control = Arc::clone(&control);
    let task_command = command.render.clone();
    let Some(handle) = runtime.spawn_blocking(move || {
        let mut stdout = String::new();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            task_control.checkpoint()?;
            replay::execute_prepared(
                &task_command,
                prepared,
                fs,
                &|| ffmpeg.clone(),
                &mut stdout,
                &|| task_control.checkpoint(),
            )
        }))
        .unwrap_or_else(|_| {
            Err(Failure::new(
                "internal",
                "narrated batch scene job panicked",
            ))
        });
        let _ = sender.send(CompletedJob::capture(result, stdout, &task_control));
    }) else {
        return initial_failure(Failure::new(
            "internal",
            "Asupersync blocking pool is unavailable",
        ));
    };
    let received = match control.remaining() {
        Some(remaining) => receiver.recv_timeout(remaining),
        None => receiver
            .recv()
            .map_err(|_| mpsc::RecvTimeoutError::Disconnected),
    };
    let received = match received {
        Ok(result) => Some(result),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            // Cancel the exact tokens held by live probe/encode/mux sessions.
            // Prepared FS tokens reject new publication and remain droppable.
            control.cancel();
            None
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => None,
    };
    // Always join. Never detach a worker or let a background writer outlive a
    // failed command. Queued work also gets to observe the cancellation flag.
    handle.wait();
    received
        .or_else(|| receiver.try_recv().ok())
        .unwrap_or_else(|| {
            CompletedJob::capture(
                Err(Failure::new(
                    "internal",
                    "batch job ended without a terminal report",
                )),
                String::new(),
                &control,
            )
        })
        .into_output(&command, &scene)
}

fn fallback_name(command: &RenderCommand) -> &str {
    command
        .scene_names
        .first()
        .map(String::as_str)
        .or_else(|| command.file.as_deref()?.file_stem()?.to_str())
        .unwrap_or("scene")
}

fn terminal(
    command: &BatchCommand,
    scene: &str,
    result: Result<(), Failure>,
    stdout: String,
    cancelled: bool,
) -> RunOutput {
    let succeeded = result.is_ok();
    // A completed publication is not undone by a cancellation observed later.
    let cancelled = cancelled && !succeeded;
    let failure = result
        .as_ref()
        .err()
        .map(|error| (error.identity, error.message.clone()));
    let result = if cancelled {
        let detail = failure.as_ref().map_or("", |(_, detail)| detail.as_str());
        Err(Failure::new(
            "budget",
            format!("narrated batch wall-clock budget exhausted; {detail}"),
        ))
    } else {
        result
    };
    let mut output = completed_output(&command.render, result, stdout);
    if command.render.common.robot {
        let status = if succeeded {
            "succeeded"
        } else if cancelled {
            "cancelled"
        } else {
            "failed"
        };
        let detail = if cancelled {
            ",\"reason\":\"budget\"".to_owned()
        } else if let Some((identity, message)) = failure {
            format!(
                ",\"exit_code\":{},\"exit_name\":{},\"message\":{}",
                code(identity),
                quote(identity),
                quote(&message),
            )
        } else {
            String::new()
        };
        let _ = writeln!(
            output.stdout,
            "{{\"schema\":\"fmn.cli\",\"version\":{},\"kind\":\"batch_job\",\"scene\":{},\"status\":{}{detail}}}",
            fmn_cli::ROBOT_SCHEMA_VERSION,
            quote(scene),
            quote(status),
        );
        let budget = command
            .budget_ms
            .map_or_else(|| "null".to_owned(), |budget| budget.to_string());
        let _ = writeln!(
            output.stdout,
            "{{\"schema\":\"fmn.cli\",\"version\":{},\"kind\":\"batch\",\"status\":{},\"jobs\":1,\"succeeded\":{},\"failed\":{},\"cancelled\":{},\"max_scenes\":1,\"budget_ms\":{budget}}}",
            fmn_cli::ROBOT_SCHEMA_VERSION,
            quote(if succeeded { "ok" } else { "failed" }),
            u8::from(succeeded),
            u8::from(!succeeded && !cancelled),
            u8::from(cancelled),
        );
    } else if !command.render.common.quiet {
        let _ = writeln!(
            output.stdout,
            "batch finished: {} succeeded, {} failed, {} cancelled (max 1 concurrent)",
            u8::from(succeeded),
            u8::from(!succeeded && !cancelled),
            u8::from(cancelled),
        );
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmn_platform::fs::VirtualFs;

    fn command(extra: &[&str]) -> BatchCommand {
        let mut args = vec![
            "batch",
            "/scene.fmtl",
            "--format",
            "wav",
            "--video_dir",
            "/out",
            "--robot",
        ];
        args.extend_from_slice(extra);
        let Invocation::Batch(command) = fmn_cli::parse_args(args).unwrap() else {
            panic!("batch invocation");
        };
        command
    }

    fn recording() -> Vec<u8> {
        let mut stage = fmn::mobject::Stage::new();
        let mut timeline = fmn::animation::Timeline::new(3).unwrap();
        for _ in 0..3 {
            timeline.wait(1.0 / 3.0).unwrap();
        }
        let pictures = fmn_scene::export_timeline_bundle(
            timeline,
            &mut stage,
            &fmn_core::rng::RngRoot::from_seed(0),
        )
        .unwrap();
        let pcm = BundleSoundtrack::new(
            10,
            2,
            (0_u8..14)
                .flat_map(|n| [f32::from(n) / 32.0, -f32::from(n) / 32.0])
                .collect(),
        )
        .unwrap();
        fmn_scene::timeline_bundle::attach_bundle_soundtrack(
            &pictures,
            &pcm,
            DEFAULT_MAX_BUNDLE_BYTES,
        )
        .unwrap()
    }

    fn assert_terminal_failure(output: &RunOutput, identity: &str, scene: &str) {
        assert_eq!(output.code, code(identity), "{}", output.stdout);
        let lines: Vec<_> = output.stdout.lines().collect();
        assert_eq!(lines.len(), 3, "{}", output.stdout);
        assert!(lines[0].contains("\"kind\":\"error\""));
        assert!(lines[1].contains("\"kind\":\"batch_job\""));
        assert!(lines[1].contains(&format!("\"scene\":{}", quote(scene))));
        assert!(lines[1].contains("\"status\":\"failed\""));
        assert!(lines[1].contains(&format!("\"exit_name\":{}", quote(identity))));
        assert!(lines[2].contains("\"kind\":\"batch\""));
        assert!(lines[2].contains("\"jobs\":1,\"succeeded\":0,\"failed\":1,\"cancelled\":0"));
        assert!(output.stderr.is_empty());
    }

    #[test]
    fn batch_and_single_share_pcm_manifest_and_original_segment_semantics() {
        let bytes = recording();
        for extra in [
            vec![],
            vec!["-n", "1,3", "--subdivide"],
            vec!["--reproducible", "-n", "1,2"],
        ] {
            let cmd = command(&extra);
            let fs = Arc::new(VirtualFs::new());
            let direct = Arc::new(VirtualFs::new());
            let bundle = TimelineBundle::from_bytes(&bytes).unwrap();
            let planned = prepare(&cmd.render, &bundle, fs.as_ref()).unwrap();
            let result = run(cmd.clone(), &bytes, bundle, fs.clone(), &|| {
                panic!("WAV requested process capability")
            });
            assert_eq!(result.code, 0, "{}", result.stdout);
            assert!(result.stdout.contains(
                "\"jobs\":1,\"succeeded\":1,\"failed\":0,\"cancelled\":0,\"max_scenes\":1"
            ));
            let single = super::super::run(
                cmd.render,
                &bytes,
                TimelineBundle::from_bytes(&bytes).unwrap(),
                direct.clone(),
                &|| None,
            );
            assert_eq!(single.code, 0, "{}", single.stdout);
            for (artifact, _) in planned.outputs {
                assert_eq!(fs.read(&artifact).unwrap(), direct.read(&artifact).unwrap());
                let manifest = provenance::destination(&artifact)
                    .unwrap()
                    .join("manifest.fmnp");
                assert_eq!(fs.read(&manifest).unwrap(), direct.read(&manifest).unwrap());
            }
        }
    }

    #[test]
    fn relocated_subdivision_manifests_keep_original_ordinals() {
        let bytes = recording();
        let fs = Arc::new(VirtualFs::new());
        let result = run(
            command(&[
                "-n",
                "1,3",
                "--subdivide",
                "--manifest-dir",
                "/manifests",
                "--max-scenes",
                "16",
            ]),
            &bytes,
            TimelineBundle::from_bytes(&bytes).unwrap(),
            fs.clone(),
            &|| None,
        );
        assert_eq!(result.code, 0, "{}", result.stdout);
        for ordinal in [1, 2] {
            assert!(fs.exists(&PathBuf::from(format!(
                "/manifests/scene/{ordinal:05}/manifest.fmnp"
            ))));
            assert!(
                result
                    .stdout
                    .contains(&format!("\"subdivision\":{ordinal}"))
            );
        }
        assert!(!fs.exists(Path::new("/manifests/scene/00000")));
        assert!(!fs.exists(Path::new("/out/scene_1_3/00001.wav.manifest")));
        assert!(result.stdout.contains("\"max_scenes\":1"));
    }

    #[test]
    fn zero_budget_has_a_terminal_cancelled_job_and_no_artifacts() {
        let bytes = recording();
        let fs = Arc::new(VirtualFs::new());
        let result = run(
            command(&["--budget-ms", "0"]),
            &bytes,
            TimelineBundle::from_bytes(&bytes).unwrap(),
            fs.clone(),
            &|| panic!("cancelled job acquired capability"),
        );
        assert_eq!(result.code, code("budget"));
        assert!(
            result
                .stdout
                .contains("\"status\":\"cancelled\",\"reason\":\"budget\"")
        );
        assert!(
            result
                .stdout
                .contains("\"succeeded\":0,\"failed\":0,\"cancelled\":1")
        );
        assert_eq!(result.stdout.lines().count(), 3);
        assert!(!fs.exists(Path::new("/out")));
    }

    #[test]
    fn all_requested_sidecars_are_preflighted_before_any_media() {
        let bytes = recording();
        let fs = Arc::new(VirtualFs::new());
        fs.insert("/manifests/scene/00002/foreign", b"winner".to_vec());
        let result = run(
            command(&["--subdivide", "--manifest-dir", "/manifests"]),
            &bytes,
            TimelineBundle::from_bytes(&bytes).unwrap(),
            fs.clone(),
            &|| None,
        );
        assert_terminal_failure(&result, "render", "scene");
        assert!(!fs.exists(Path::new("/out")));
        assert_eq!(
            fs.read(Path::new("/manifests/scene/00002/foreign"))
                .unwrap(),
            b"winner"
        );
    }

    #[test]
    fn manifest_media_aliases_refuse_before_generation() {
        let bytes = recording();
        let fs = Arc::new(VirtualFs::new());
        for root in ["/out/scene.wav", "/out"] {
            let mut command = command(&["--manifest-dir", root]);
            if root == "/out" {
                command.render.file_name = Some("scene/file".to_owned());
            }
            let result = run(
                command,
                &bytes,
                TimelineBundle::from_bytes(&bytes).unwrap(),
                fs.clone(),
                &|| None,
            );
            assert_terminal_failure(&result, "config", "scene");
            assert!(!fs.exists(Path::new("/out")));
        }
    }

    #[test]
    fn zero_scene_limit_refuses_without_publication() {
        let bytes = recording();
        let fs = Arc::new(VirtualFs::new());
        let mut cmd = command(&[]);
        cmd.max_scenes = Some(0);
        let result = run(
            cmd,
            &bytes,
            TimelineBundle::from_bytes(&bytes).unwrap(),
            fs.clone(),
            &|| None,
        );
        assert_terminal_failure(&result, "config", "scene");
        assert!(!fs.exists(Path::new("/out")));
    }

    #[test]
    fn range_admission_failure_keeps_selected_scene_and_terminal_summary() {
        let bytes = recording();
        let fs = Arc::new(VirtualFs::new());
        let mut cmd = command(&["-n", "8,9"]);
        cmd.render.scene_names = vec!["NamedScene".to_owned()];
        let result = run(
            cmd,
            &bytes,
            TimelineBundle::from_bytes(&bytes).unwrap(),
            fs.clone(),
            &|| panic!("invalid range acquired process capability"),
        );
        assert_terminal_failure(&result, "usage", "NamedScene");
        assert!(!fs.exists(Path::new("/out")));
    }

    #[test]
    fn a_completed_failure_cannot_be_relabelled_by_late_cancellation() {
        let control = Control::new(None).unwrap();
        let completed = CompletedJob::capture(
            Err(Failure::new("render", "original sink failure")),
            String::new(),
            &control,
        );
        // Model the budget firing while the owner joins an already-reported
        // worker. No sleeps or scheduler assumptions participate in this test.
        control.cancel();
        let result = completed.into_output(&command(&[]), "scene");
        assert_terminal_failure(&result, "render", "scene");
        assert!(result.stdout.contains("original sink failure"));
        assert!(!result.stdout.contains("wall-clock budget exhausted"));
    }

    #[test]
    fn completed_publication_stays_successful_after_late_cancellation() {
        let control = Control::new(None).unwrap();
        let receipt = "{\"kind\":\"bundle-audio-render\",\"artifact\":\"kept.wav\"}\n";
        let completed = CompletedJob::capture(Ok(()), receipt.to_owned(), &control);
        control.cancel();
        let result = completed.into_output(&command(&[]), "scene");
        assert_eq!(result.code, 0, "{}", result.stdout);
        assert!(result.stdout.starts_with(receipt));
        assert!(result.stdout.contains("\"status\":\"succeeded\""));
        assert!(
            result
                .stdout
                .contains("\"succeeded\":1,\"failed\":0,\"cancelled\":0")
        );
        assert!(!result.stdout.contains("\"kind\":\"error\""));
        assert_eq!(result.stdout.lines().count(), 3);
    }

    #[test]
    fn cancellation_keeps_prior_clip_receipts_and_one_terminal_job() {
        let control = Control::new(None).unwrap();
        let receipt = "{\"kind\":\"bundle-audio-render\",\"subdivision\":2}\n";
        control.cancel();
        let completed = CompletedJob::capture(
            Err(Failure::new(
                "render",
                "next clip publication was cancelled",
            )),
            receipt.to_owned(),
            &control,
        );
        let result = completed.into_output(&command(&[]), "scene");
        assert_eq!(result.code, code("budget"));
        assert!(result.stdout.starts_with(receipt));
        assert!(
            result
                .stdout
                .contains("next clip publication was cancelled")
        );
        assert!(
            result
                .stdout
                .contains("\"status\":\"cancelled\",\"reason\":\"budget\"")
        );
        assert!(
            result
                .stdout
                .contains("\"succeeded\":0,\"failed\":0,\"cancelled\":1")
        );
        assert_eq!(result.stdout.matches("\"kind\":\"batch_job\"").count(), 1);
        assert_eq!(result.stdout.matches("\"kind\":\"batch\"").count(), 1);
        assert_eq!(result.stdout.lines().count(), 4);
    }
}
