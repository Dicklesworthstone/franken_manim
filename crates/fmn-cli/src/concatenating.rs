//! The command-line front door for bounded, create-only encoded clip assembly.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use fmn::{FfmpegCapability, VideoConcatError, VideoConcatOptions, VideoConcatReport};
use fmn_config::config::DeterminismMode;
use fmn_output::{BoundaryError, JobLimits};
use fmn_platform::fs::{FileSystem, FsError, StdFs};
use fmn_platform::process::{FfmpegExecutable, FfmpegLocator, FfmpegLocatorError, ProcessRunner};

use super::{CliError, Collected, CommonOptions, RunOutput};

/// Assemble compatible, self-contained MP4/MOV clips in the supplied order.
///
/// Repeated paths retain their positions and consume the input allowance again.
/// The command copies encoded streams; it does not convert codecs, resize,
/// resample, trim, or claim a decoded duration or frame count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConcatCommand {
    /// Shared configuration and response flags.
    pub common: CommonOptions,
    /// Ordered source paths, including repeated positions.
    pub inputs: Vec<PathBuf>,
    /// Exact MP4/MOV destination. Existing nodes are never overwritten.
    pub output: PathBuf,
    /// Maximum input positions (at most 4093).
    pub max_clips: usize,
    /// Maximum bytes retained for any one input position.
    pub max_clip_bytes: u64,
    /// Maximum combined bytes retained for all inputs.
    pub max_input_bytes: u64,
    /// Positive wall-clock seconds for the stream-copy job.
    pub timeout_seconds: u64,
    /// Maximum captured bytes per stdout/stderr stream.
    pub max_log_bytes: u64,
    /// Maximum output artifact bytes and additional process scratch.
    pub max_output_bytes: u64,
}

pub(super) fn parse(values: &Collected, common: CommonOptions) -> Result<ConcatCommand, CliError> {
    let output = values
        .explicit_value("concat_output")
        .ok_or_else(|| super::usage("fmn concat requires --output MOVIE.mp4 or MOVIE.mov"))?;
    let command = ConcatCommand {
        common,
        inputs: values
            .many("concat_inputs")
            .into_iter()
            .map(PathBuf::from)
            .collect(),
        output: PathBuf::from(output),
        max_clips: super::parse_value(values, "concat_max_clips", "1024")?,
        max_clip_bytes: super::parse_value(values, "concat_max_clip_bytes", "67108864")?,
        max_input_bytes: super::parse_value(values, "concat_max_input_bytes", "268435456")?,
        timeout_seconds: super::parse_value(values, "concat_timeout", "600")?,
        max_log_bytes: super::parse_value(values, "concat_max_log_bytes", "1048576")?,
        max_output_bytes: super::parse_value(values, "concat_max_output_bytes", "8589934592")?,
    };
    command.admit()?;
    Ok(command)
}

impl ConcatCommand {
    // This pure admission is also repeated by the facade. Keeping it ahead of
    // configuration and capability selection gives the CLI stable usage errors
    // and ensures a forged typed command cannot bypass preflight.
    fn admit(&self) -> Result<JobLimits, CliError> {
        if self.inputs.is_empty() {
            return Err(super::usage("fmn concat requires at least one input clip"));
        }
        if self.max_clips == 0 || self.max_clips > fmn_output::ffmpeg::MAX_CONCAT_INPUTS {
            return Err(super::usage(format!(
                "--max-clips must be in 1..={}",
                fmn_output::ffmpeg::MAX_CONCAT_INPUTS,
            )));
        }
        if self.inputs.len() > self.max_clips {
            return Err(super::usage("input count exceeds --max-clips"));
        }
        for (name, value) in [
            ("--max-clip-bytes", self.max_clip_bytes),
            ("--max-input-bytes", self.max_input_bytes),
        ] {
            if value == 0
                || usize::try_from(value)
                    .ok()
                    .is_none_or(|limit| limit > isize::MAX as usize)
            {
                return Err(super::usage(format!(
                    "{name} must be positive and fit the addressable memory range"
                )));
            }
        }
        if !self
            .output
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| {
                value.eq_ignore_ascii_case("mp4") || value.eq_ignore_ascii_case("mov")
            })
        {
            return Err(super::usage("--output must have an MP4 or MOV extension"));
        }
        let requested = JobLimits {
            timeout: Duration::from_secs(self.timeout_seconds),
            max_log_bytes: self.max_log_bytes,
            max_artifact_bytes: self.max_output_bytes,
            keep_workdir: false,
        };
        fmn_output::admit_render_ffmpeg_limits(&requested, u64::MAX).map_err(super::usage)
    }
}

#[derive(Clone)]
struct SelectedFfmpeg {
    executable: FfmpegExecutable,
}

impl FfmpegLocator for SelectedFfmpeg {
    fn locate_ffmpeg(&self, _configured: &Path) -> Result<FfmpegExecutable, FfmpegLocatorError> {
        Ok(self.executable.clone())
    }
}

pub(super) fn execute(
    fs: Arc<dyn FileSystem>,
    runner: Arc<dyn ProcessRunner>,
    locator: &dyn FfmpegLocator,
    command: &ConcatCommand,
) -> Result<VideoConcatReport, CliError> {
    let limits = command.admit()?;
    let config = super::resolve_common_config(fs.as_ref(), &command.common)?;
    if config.determinism.mode == DeterminismMode::Certified {
        return Err(concat_error(VideoConcatError::Capability(
            "encoded video is outside the certified artifact set",
        )));
    }
    let output = std::path::absolute(&command.output).map_err(|err| {
        concat_error(VideoConcatError::Output(FsError::Io {
            path: command.output.clone(),
            err,
        }))
    })?;
    match StdFs.node_kind_no_follow(&output) {
        Ok(Some(_)) => return Err(concat_error(VideoConcatError::DestinationExists(output))),
        Ok(None) | Err(FsError::NotFound { .. }) => {}
        Err(error) => return Err(concat_error(VideoConcatError::Output(error))),
    }

    // Preserve the injected CLI locator and opaque selected token. Capability
    // selection reads the configured executable before the source snapshots;
    // the native facade still freezes every bounded clip before any probe or
    // process starts. There is no second host PATH lookup.
    let configured = PathBuf::from(&config.file_writer.ffmpeg_bin);
    let executable = locator.locate_ffmpeg(&configured).map_err(|error| {
        CliError::new(
            "capability",
            format!(
                "ffmpeg is unavailable at {}: {error}; {}",
                configured.display(),
                fmn_output::NATIVE_ALTERNATIVE,
            ),
        )
    })?;
    let mut options = VideoConcatOptions::new(output);
    options.determinism = config.determinism.mode;
    options.ffmpeg_bin = configured;
    options.ffmpeg = Some(FfmpegCapability {
        runner,
        locator: Arc::new(SelectedFfmpeg { executable }),
        workdir_root: std::env::temp_dir(),
    });
    options.ffmpeg_limits = limits;
    options.max_inputs = command.max_clips;
    options.max_input_bytes = command.max_clip_bytes;
    options.max_total_input_bytes = command.max_input_bytes;
    fmn::concatenate_video_with_fs(&command.inputs, options, fs).map_err(concat_error)
}

fn concat_error(error: VideoConcatError) -> CliError {
    let exit_name = match &error {
        VideoConcatError::InvalidOptions(_) => "usage",
        VideoConcatError::Capability(_) | VideoConcatError::FfmpegUnavailable(_) => "capability",
        VideoConcatError::Allocation(_)
        | VideoConcatError::Input {
            error: FsError::TooLarge { .. } | FsError::TooManyEntries { .. },
            ..
        }
        | VideoConcatError::Boundary(
            BoundaryError::JobTimedOut { .. }
            | BoundaryError::LogOverflow
            | BoundaryError::ArtifactOversized { .. }
            | BoundaryError::WorkdirOversized { .. },
        ) => "budget",
        VideoConcatError::Boundary(BoundaryError::Cancelled) => "cancelled",
        VideoConcatError::Boundary(
            BoundaryError::FfmpegUnavailable { .. }
            | BoundaryError::ExecutableIdentityChanged { .. }
            | BoundaryError::ExecutableImageRejected { .. }
            | BoundaryError::UnsupportedRelocatedExecutable { .. }
            | BoundaryError::ProbeFailed(_)
            | BoundaryError::ProbeProcessFailed(_),
        ) => "capability",
        _ => "render",
    };
    CliError::new(exit_name, error.to_string())
}

pub(super) fn success(command: &ConcatCommand, report: &VideoConcatReport) -> RunOutput {
    if command.common.robot {
        let inputs = report
            .inputs
            .iter()
            .enumerate()
            .map(|(index, input)| {
                format!(
                    "{{\"index\":{index},\"path\":{},\"bytes\":{},\"sha256\":{}}}",
                    super::json_string(&input.source_path.to_string_lossy()),
                    input.bytes,
                    super::json_string(&input.digest.to_hex()),
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        return RunOutput::success(format!(
            "{{\"schema\":\"fmn.cli\",\"version\":{},\"kind\":\"concat\",\
             \"output\":{},\"artifact_bytes\":{},\"sha256\":{},\
             \"input_bytes\":{},\"inputs\":[{inputs}],\
             \"max_clips\":{},\"max_clip_bytes\":{},\
             \"limits\":{},\"ffmpeg\":{}}}\n",
            super::ROBOT_SCHEMA_VERSION,
            super::json_string(&report.boundary.destination.to_string_lossy()),
            report.boundary.artifact_bytes,
            super::json_string(&report.boundary.artifact_digest.to_hex()),
            report.total_input_bytes,
            command.max_clips,
            command.max_clip_bytes,
            report.limits.to_json(),
            super::ffmpeg_invocations_json(&report.boundary.invocations),
        ));
    }
    if command.common.quiet {
        return RunOutput::success(String::new());
    }
    RunOutput::success(format!(
        "concatenated {} clips to {} ({} input bytes, {} output bytes; SHA-256 {})\n",
        report.inputs.len(),
        report.boundary.destination.display(),
        report.total_input_bytes,
        report.boundary.artifact_bytes,
        report.boundary.artifact_digest.to_hex(),
    ))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]

    use super::*;
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    use crate::run_with_capabilities;
    use crate::{CommandScope, Invocation, parse_args};
    use fmn_platform::fs::VirtualFs;
    use fmn_platform::process::NoProcessRunner;
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    use fmn_platform::process::{
        ProcessCancellation, ProcessError, ProcessMechanism, ProcessOutcome, ProcessSpec,
        ProcessStdinLimits, ProcessTermination, RunningProcess, ScriptedRunner,
    };
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

    fn fixture(label: &str) -> PathBuf {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "fmn-cli-concat-{label}-{}-{timestamp}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::SeqCst),
        ));
        std::fs::create_dir(&root).unwrap();
        root
    }

    fn command(args: impl IntoIterator<Item = impl Into<String>>) -> ConcatCommand {
        let Invocation::Concat(command) = parse_args(args).unwrap() else {
            panic!("expected concat command"); // ubs:ignore - cfg(test) parser contract assertion.
        };
        command
    }

    #[test]
    fn concat_parser_preserves_positions_and_accepts_dash_paths_after_separator() {
        let command = command([
            "concat",
            "--output=complete.MOV",
            "--max-clips=3",
            "--max-clip-bytes=20",
            "--max-input-bytes=60",
            "--timeout=7",
            "--max-log-bytes=32",
            "--max-output-bytes=128",
            "--",
            "-opening.mp4",
            "a quoted'\nclip.mp4",
            "-opening.mp4",
        ]);
        assert_eq!(
            command.inputs,
            ["-opening.mp4", "a quoted'\nclip.mp4", "-opening.mp4"].map(PathBuf::from),
        );
        assert_eq!(command.output, Path::new("complete.MOV"));
        assert_eq!(command.max_clips, 3);
        assert_eq!(command.max_clip_bytes, 20);
        assert_eq!(command.max_input_bytes, 60);
        assert_eq!(command.timeout_seconds, 7);
        assert_eq!(command.max_log_bytes, 32);
        assert_eq!(command.max_output_bytes, 128);
    }

    #[test]
    fn concat_requires_output_and_inputs_and_rejects_unusable_or_render_only_flags() {
        for argv in [
            vec!["concat", "clip.mp4"],
            vec!["concat", "--output", "complete.mp4"],
            vec!["concat", "--output", "complete.gif", "clip.mp4"],
            vec![
                "concat",
                "--output",
                "complete.mp4",
                "--fps",
                "60",
                "clip.mp4",
            ],
            vec![
                "concat",
                "--output",
                "complete.mp4",
                "--ffmpeg-timeout",
                "9",
                "clip.mp4",
            ],
        ] {
            assert_eq!(parse_args(argv).unwrap_err().exit_name(), "usage");
        }
        for (flag, value) in [
            ("--max-clips", "0"),
            ("--max-clips", "4094"),
            ("--max-clip-bytes", "0"),
            ("--max-clip-bytes", "18446744073709551615"),
            ("--max-input-bytes", "0"),
            ("--max-input-bytes", "18446744073709551615"),
            ("--timeout", "0"),
            ("--timeout", "18446744073709551615"),
            ("--max-log-bytes", "0"),
            ("--max-output-bytes", "0"),
        ] {
            let error = parse_args([
                "concat",
                "--output",
                "complete.mp4",
                flag,
                value,
                "clip.mp4",
            ])
            .unwrap_err();
            assert_eq!(error.exit_name(), "usage", "{flag} {value}: {error}");
        }
        assert!(matches!(
            parse_args(["concat", "--help", "--robot"]).unwrap(),
            Invocation::Help {
                command: CommandScope::Concat,
                robot: true
            },
        ));
    }

    struct CountingLocator {
        selected: Option<FfmpegExecutable>,
        requests: Mutex<Vec<PathBuf>>,
    }

    impl FfmpegLocator for CountingLocator {
        fn locate_ffmpeg(&self, configured: &Path) -> Result<FfmpegExecutable, FfmpegLocatorError> {
            self.requests.lock().unwrap().push(configured.to_path_buf());
            self.selected.clone().ok_or(FfmpegLocatorError::NotFound)
        }
    }

    #[test]
    fn certified_and_existing_destinations_refuse_before_executable_selection() {
        let root = fixture("preflight");
        let output = root.join("joined.mp4");
        let locator = CountingLocator {
            selected: None,
            requests: Mutex::new(Vec::new()),
        };
        let source = Arc::new(VirtualFs::new());
        let request = command([
            "concat".to_owned(),
            "--output".to_owned(),
            output.to_string_lossy().into_owned(),
            "--reproducible".to_owned(),
            "/clip.mp4".to_owned(),
        ]);
        assert_eq!(
            execute(
                source.clone(),
                Arc::new(NoProcessRunner),
                &locator,
                &request
            )
            .unwrap_err()
            .exit_name(),
            "capability",
        );
        let mut request = request;
        request.common.reproducible = false;
        std::fs::write(&output, b"already complete").unwrap();
        assert_eq!(
            execute(source, Arc::new(NoProcessRunner), &locator, &request)
                .unwrap_err()
                .exit_name(),
            "render",
        );
        assert!(locator.requests.lock().unwrap().is_empty());
        assert_eq!(std::fs::read(output).unwrap(), b"already complete");
    }

    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    fn selected_locator(root: &Path) -> CountingLocator {
        let executable = root.join("selected-ffmpeg");
        crate::tests::write_native_ffmpeg_fixture(&executable);
        let selected = fmn_platform::process::StdFfmpegLocator::from_search_path(None)
            .locate_ffmpeg(&executable)
            .unwrap();
        CountingLocator {
            selected: Some(selected),
            requests: Mutex::new(Vec::new()),
        }
    }

    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    struct OrderedConcatRunner {
        source: Arc<VirtualFs>,
        calls: Mutex<Vec<ProcessSpec>>,
    }

    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    impl ProcessRunner for OrderedConcatRunner {
        fn mechanism(&self) -> ProcessMechanism {
            ProcessMechanism::Scripted
        }

        fn run(&self, spec: &ProcessSpec) -> Result<ProcessOutcome, ProcessError> {
            self.calls.lock().unwrap().push(spec.clone());
            assert_eq!(spec.argv, ["-version"]);
            // Changing authored sources at the first probe cannot alter the
            // snapshots passed to the process, or the ordered source receipts.
            self.source
                .insert("/clips/it's-A.mp4", b"changed after snapshot".to_vec());
            self.source
                .insert("/clips/line\nbreak.mp4", b"also changed".to_vec());
            Ok(ProcessOutcome {
                termination: ProcessTermination::Exited(Some(0)),
                stdout: b"ffmpeg version cli-concat-test\n".to_vec(),
                stderr: Vec::new(),
            })
        }

        fn start(
            &self,
            spec: &ProcessSpec,
            cancellation: ProcessCancellation,
            stdin_limits: ProcessStdinLimits,
        ) -> Result<Box<dyn RunningProcess>, ProcessError> {
            self.calls.lock().unwrap().push(spec.clone());
            for pair in [["-f", "concat"], ["-map", "0"], ["-c", "copy"]] {
                assert!(spec.argv.windows(2).any(|args| args == pair));
            }
            let at = spec.argv.iter().position(|arg| arg == "-i").unwrap();
            let list = Path::new(&spec.argv[at + 1]);
            let text = std::fs::read_to_string(list).unwrap();
            let parent = list.parent().unwrap();
            for (index, expected) in [b"AAA".as_slice(), b"BBBBB", b"AAA"]
                .into_iter()
                .enumerate()
            {
                let name = format!("clip-{index:06}.media");
                assert!(text.contains(&format!("file '{name}'\n")));
                assert_eq!(std::fs::read(parent.join(name)).unwrap(), expected);
            }
            assert!(!text.contains("it's-A"));
            assert!(!text.contains("line\nbreak"));
            assert_eq!(text.matches("option format_whitelist mov\n").count(), 3);
            assert_eq!(text.matches("option protocol_whitelist file\n").count(), 3);
            assert_eq!(text.matches("option enable_drefs 0\n").count(), 3);
            std::fs::write(spec.argv.last().unwrap(), b"ordered:A,B,A").unwrap();
            let mut runner = ScriptedRunner::new();
            runner.script(
                &spec.program,
                ProcessOutcome {
                    termination: ProcessTermination::Exited(Some(0)),
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                },
            );
            runner.start(spec, cancellation, stdin_limits)
        }
    }

    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    fn source_and_runner() -> (Arc<VirtualFs>, Arc<OrderedConcatRunner>) {
        let source = Arc::new(VirtualFs::new());
        source.insert("/clips/it's-A.mp4", b"AAA".to_vec());
        source.insert("/clips/line\nbreak.mp4", b"BBBBB".to_vec());
        let runner = Arc::new(OrderedConcatRunner {
            source: source.clone(),
            calls: Mutex::new(Vec::new()),
        });
        (source, runner)
    }

    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    #[test]
    fn source_failure_and_repeated_input_budget_refusal_never_start_a_process() {
        let root = fixture("source-budgets");
        let locator = selected_locator(&root);
        let (source, runner) = source_and_runner();
        let output = root.join("joined.mp4");
        for (clips, extra, expected) in [
            (vec!["/missing.mp4"], vec![], "render"),
            (
                vec!["/clips/it's-A.mp4"],
                vec!["--max-clip-bytes", "2"],
                "budget",
            ),
            (
                vec![
                    "/clips/it's-A.mp4",
                    "/clips/line\nbreak.mp4",
                    "/clips/it's-A.mp4",
                ],
                vec!["--max-input-bytes", "10"],
                "budget",
            ),
        ] {
            let mut args = vec![
                "concat".to_owned(),
                "--output".to_owned(),
                output.to_string_lossy().into_owned(),
            ];
            args.extend(extra.into_iter().chain(clips).map(str::to_owned));
            let request = command(args);
            let error = execute(source.clone(), runner.clone(), &locator, &request).unwrap_err();
            assert_eq!(error.exit_name(), expected, "{error}");
            assert!(!output.exists());
        }
        assert!(runner.calls.lock().unwrap().is_empty());
        assert_eq!(locator.requests.lock().unwrap().len(), 3);
    }

    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    #[test]
    fn dispatch_uses_injected_token_frozen_order_and_reports_create_only_publication() {
        let root = fixture("ordered");
        let locator = selected_locator(&root);
        let (source, runner) = source_and_runner();
        source.insert(
            "/concat.yml",
            b"file_writer:\n  ffmpeg_bin: config-selected-ffmpeg\n".to_vec(),
        );
        let output = root.join("joined.mp4");
        let args = vec![
            "concat".to_owned(),
            "--robot".to_owned(),
            "--config_file".to_owned(),
            "/concat.yml".to_owned(),
            "--output".to_owned(),
            output.to_string_lossy().into_owned(),
            "--max-input-bytes".to_owned(),
            "11".to_owned(),
            "--max-clip-bytes".to_owned(),
            "5".to_owned(),
            "--max-log-bytes".to_owned(),
            "1024".to_owned(),
            "--max-output-bytes".to_owned(),
            "16384".to_owned(),
            "--timeout".to_owned(),
            "7".to_owned(),
            "/clips/it's-A.mp4".to_owned(),
            "/clips/line\nbreak.mp4".to_owned(),
            "/clips/it's-A.mp4".to_owned(),
        ];
        let response =
            run_with_capabilities(args.clone(), source.clone(), runner.clone(), &locator);
        assert_eq!(response.code, 0, "{}{}", response.stdout, response.stderr);
        assert_eq!(std::fs::read(&output).unwrap(), b"ordered:A,B,A");
        assert_eq!(
            *locator.requests.lock().unwrap(),
            [PathBuf::from("config-selected-ffmpeg")]
        );
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1].max_output_bytes, 1024);
        assert!(calls[1].timeout <= Duration::from_secs(7));
        assert!(
            calls
                .iter()
                .all(|call| call.program != Path::new("config-selected-ffmpeg"))
        );
        drop(calls);
        let expected_inputs = format!(
            "\"inputs\":[{{\"index\":0,\"path\":\"/clips/it's-A.mp4\",\"bytes\":3,\"sha256\":\"{}\"}},\
             {{\"index\":1,\"path\":\"/clips/line\\nbreak.mp4\",\"bytes\":5,\"sha256\":\"{}\"}},\
             {{\"index\":2,\"path\":\"/clips/it's-A.mp4\",\"bytes\":3,\"sha256\":\"{}\"}}]",
            fmn_hash::sha256(b"AAA"),
            fmn_hash::sha256(b"BBBBB"),
            fmn_hash::sha256(b"AAA"),
        );
        assert!(
            response.stdout.contains(&expected_inputs),
            "{}",
            response.stdout
        );
        assert!(response.stdout.contains("\"kind\":\"concat\""));
        assert!(response.stdout.contains("\"input_bytes\":11"));
        assert!(response.stdout.contains("\"artifact_bytes\":13"));
        assert!(response.stdout.contains("\"max_input_bytes\":11"));
        assert!(response.stdout.contains("\"max_artifact_bytes\":16384"));
        assert!(
            response
                .stdout
                .contains("\"process_mechanism\":\"scripted.process_runner\"")
        );
        assert!(!response.stdout.contains("frame_count"));
        let second = run_with_capabilities(args, source, runner.clone(), &locator);
        assert_eq!(second.code, 6);
        assert!(second.stdout.contains("destination already exists"));
        assert_eq!(std::fs::read(output).unwrap(), b"ordered:A,B,A");
        assert_eq!(locator.requests.lock().unwrap().len(), 1);
        assert_eq!(runner.calls.lock().unwrap().len(), 2);
    }
}
