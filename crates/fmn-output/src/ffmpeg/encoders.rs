//! Functional observations and input formats of the installed encoders.
//!
//! An encoder listed by FFmpeg may still lack its driver or a usable device.
//! Trials use the same bound executable and containment as production jobs.
//! They establish that a small encode worked, not support for every job profile.

use super::*;

const ENCODER_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const ENCODER_PROBE_LOG_CAP: u64 = 16 * 1024;
const ENCODER_PROBE_DISK_CAP: u64 = 1024 * 1024;
const ENCODER_FORMAT_LOG_CAP: u64 = 64 * 1024;
const MAX_ENCODER_PIXEL_FORMATS: usize = 256;
const PROBE_SIDE: usize = 256;
const PROBE_LUMA_BYTES: usize = PROBE_SIDE * PROBE_SIDE;
const PROBE_FRAME_BYTES: usize = PROBE_LUMA_BYTES * 3 / 2;

/// One bounded functional observation of a compiled-in hardware encoder.
///
/// Results are shared by clones of one resolved, fingerprinted tool. Resolving
/// the tool again refreshes the observation, including after driver/device
/// changes. A successful trial is not a promise that every resolution, pixel
/// format, or later encode will succeed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncoderProbe {
    /// The exact FFmpeg encoder name.
    pub encoder: String,
    /// Whether the two-frame trial exited successfully.
    pub available: bool,
    /// First bounded, control-free stderr line or a named process refusal.
    /// Successful trials have no diagnostic.
    pub detail: Option<String>,
}

/// Input pixel formats advertised by one encoder in this FFmpeg executable.
///
/// This is a capability observation, not a hardware initialization trial.
/// The boundary checks both when a hardware encoder is explicitly selected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncoderPixelFormats {
    /// The exact encoder whose help was queried.
    pub encoder: String,
    /// Sorted, unique FFmpeg format names. Empty means the matching encoder
    /// help did not advertise a list; it never establishes 10-bit support.
    pub formats: Vec<String>,
}

impl EncoderCapabilities {
    /// Discover the selected encoder's supported input formats once per tool.
    ///
    /// The bounded help query uses the same verified private executable and
    /// process containment as an encode. Clones share the result, but every
    /// cache hit still checks the tool's executable and private-session identity.
    ///
    /// # Errors
    /// Unknown encoders, failed/malformed help, and boundary integrity failures.
    pub fn probe_pixel_formats(
        &self,
        tool: &FfmpegTool,
        runner: &dyn ProcessRunner,
        encoder: &str,
    ) -> Result<EncoderPixelFormats, BoundaryError> {
        if !self.offers(encoder) {
            return Err(BoundaryError::UnknownEncoder {
                requested: encoder.to_owned(),
                hardware_available: self.hardware(),
            });
        }
        tool.probe_encoder_pixel_formats(runner, encoder)
    }

    /// Admit an entire video job before scene execution or encoder startup.
    ///
    /// Hardware must pass its functional trial. Opaque NV12/P010 jobs also
    /// discover encoder input formats so their native planes can pass through.
    /// P010 refuses an unverified 10-bit path instead of letting FFmpeg choose
    /// an 8-bit substitute. Alpha and GIF jobs retain their existing profiles.
    ///
    /// # Errors
    /// Invalid jobs, unknown/unusable encoders, incompatible input precision,
    /// malformed capability output, and boundary integrity failures.
    pub fn require_video_job(
        &self,
        tool: &FfmpegTool,
        runner: &dyn ProcessRunner,
        job: &VideoJob,
    ) -> Result<Option<EncoderPixelFormats>, BoundaryError> {
        let Some(encoder) = job.resolved_encoder()? else {
            return Ok(None);
        };
        self.require_encoder(tool, runner, &encoder)?;
        if !matches!(job.container, Container::Mp4 | Container::Mov)
            || !matches!(job.wire, WireFormat::Nv12 | WireFormat::P010)
        {
            return Ok(None);
        }
        let formats = self.probe_pixel_formats(tool, runner, &encoder)?;
        encoder_pixel_format(job, Some(&formats.formats)).map_err(|_| {
            BoundaryError::UnsupportedEncoderPixelFormat {
                requested: encoder,
                wire: job.wire,
                advertised: formats.formats.clone(),
            }
        })?;
        Ok(Some(formats))
    }

    /// Functionally probe each compiled-in recognized hardware encoder.
    ///
    /// Stable inventory order is preserved. Existing observations from this
    /// resolved tool are reused; software encoders are not trialled.
    ///
    /// # Errors
    /// Executable identity, private-directory, or process-mechanism failures
    /// remain boundary errors and are never cached as hardware observations.
    pub fn probe_hardware(
        &self,
        tool: &FfmpegTool,
        runner: &dyn ProcessRunner,
    ) -> Result<Vec<EncoderProbe>, BoundaryError> {
        self.hardware()
            .iter()
            .map(|encoder| tool.probe_hardware_encoder(runner, encoder))
            .collect()
    }

    /// Admit an encoder before rendering or starting its production process.
    ///
    /// Every encoder must be compiled in. Recognized hardware encoders must
    /// also pass the bounded functional trial; software selections need no
    /// extra process. The returned observation can be retained in provenance.
    ///
    /// # Errors
    /// Unknown or unusable encoders yield named capability refusals. Boundary
    /// integrity and process-mechanism errors are propagated unchanged.
    pub fn require_encoder(
        &self,
        tool: &FfmpegTool,
        runner: &dyn ProcessRunner,
        encoder: &str,
    ) -> Result<Option<EncoderProbe>, BoundaryError> {
        if !self.offers(encoder) {
            return Err(BoundaryError::UnknownEncoder {
                requested: encoder.to_owned(),
                hardware_available: self.hardware(),
            });
        }
        if !HARDWARE_ENCODERS.contains(&encoder) {
            return Ok(None);
        }
        let probe = tool.probe_hardware_encoder(runner, encoder)?;
        if !probe.available {
            return Err(BoundaryError::UnusableEncoder {
                requested: probe.encoder,
                detail: probe
                    .detail
                    .unwrap_or_else(|| "functional probe failed".to_owned()),
            });
        }
        Ok(Some(probe))
    }
}

impl FfmpegTool {
    fn probe_encoder_pixel_formats(
        &self,
        runner: &dyn ProcessRunner,
        encoder: &str,
    ) -> Result<EncoderPixelFormats, BoundaryError> {
        self.session.verify_root()?;
        self.verify_source_identity()?;
        if let Some(formats) = self
            .encoder_formats
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(encoder)
            .cloned()
        {
            return Ok(formats);
        }
        // No cache lock is held while invoking the host-supplied process runner.
        let outcome = self.with_bound_probe(
            |_, _| Ok(()),
            |path, workdir| {
                let help = format!("encoder={encoder}");
                let mut spec = probe_spec(path, &["-hide_banner", "-h", &help], workdir.path());
                spec.timeout = ENCODER_PROBE_TIMEOUT;
                spec.max_output_bytes = ENCODER_FORMAT_LOG_CAP;
                let cancellation = ProcessCancellation::new();
                let watch = ArtifactWatch::start(
                    workdir,
                    &workdir.join("probe-output"),
                    ENCODER_PROBE_DISK_CAP,
                    cancellation.clone(),
                )?;
                let result = runner
                    .start(&spec, cancellation, ProcessStdinLimits::new(1, 0))
                    .and_then(|process| process.finish())
                    .map_err(BoundaryError::from);
                watch.finish()?;
                let outcome = result?;
                if outcome.success() {
                    Ok(outcome)
                } else {
                    Err(BoundaryError::ProbeProcessFailed(Box::new(ProbeFailure {
                        source_path: self.path.clone(),
                        executable_sha256: self.sha256_hex.clone(),
                        version: self.version.clone(),
                        spec,
                        outcome,
                    })))
                }
            },
        )?;
        let listing = std::str::from_utf8(&outcome.stdout).map_err(|_| {
            BoundaryError::ProbeFailed("encoder pixel-format help is not valid UTF-8")
        })?;
        let formats = parse_pixel_formats(encoder, listing)?;
        let mut cache = self
            .encoder_formats
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(cache.entry(encoder.to_owned()).or_insert(formats).clone())
    }

    pub(super) fn cached_hardware_probe(&self, encoder: &str) -> Option<EncoderProbe> {
        self.encoder_probes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(encoder)
            .cloned()
    }

    fn probe_hardware_encoder(
        &self,
        runner: &dyn ProcessRunner,
        encoder: &str,
    ) -> Result<EncoderProbe, BoundaryError> {
        // A cached success must not conceal executable or session replacement.
        self.session.verify_root()?;
        self.verify_source_identity()?;
        if let Some(probe) = self.cached_hardware_probe(encoder) {
            return Ok(probe);
        }

        // Do not hold the cache lock across user-supplied process callbacks.
        // Concurrent first callers may perform the same bounded trial; the
        // first completed observation wins for this resolved tool.
        let outcome = self.with_bound_probe(
            |_, _| Ok(()),
            |path, workdir| run_encoder_probe(runner, path, workdir, encoder),
        )?;
        let probe = classify_probe(encoder, &outcome);
        let mut probes = self
            .encoder_probes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(probes.entry(encoder.to_owned()).or_insert(probe).clone())
    }
}

fn parse_pixel_formats(encoder: &str, listing: &str) -> Result<EncoderPixelFormats, BoundaryError> {
    let expected_banner = format!("Encoder {encoder} [");
    let first_line = listing.lines().next().unwrap_or_default().trim();
    if !first_line.starts_with(&expected_banner) || !first_line.ends_with("]:") {
        return Err(BoundaryError::ProbeFailed(
            "encoder help did not describe the requested encoder",
        ));
    }
    let mut formats = None;
    for line in listing.lines().skip(1) {
        let Some(values) = line.trim().strip_prefix("Supported pixel formats:") else {
            continue;
        };
        if formats.is_some() {
            return Err(BoundaryError::ProbeFailed(
                "encoder help repeats the supported pixel-format list",
            ));
        }
        let mut parsed = std::collections::BTreeSet::new();
        for (index, value) in values.split_whitespace().enumerate() {
            if index >= MAX_ENCODER_PIXEL_FORMATS
                || value.len() > 64
                || !value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            {
                return Err(BoundaryError::ProbeFailed(
                    "encoder help contains an invalid or oversized pixel-format list",
                ));
            }
            parsed.insert(value.to_owned());
        }
        formats = Some(parsed.into_iter().collect());
    }
    Ok(EncoderPixelFormats {
        encoder: encoder.to_owned(),
        formats: formats.unwrap_or_default(),
    })
}

fn run_encoder_probe(
    runner: &dyn ProcessRunner,
    path: &Path,
    workdir: &OwnedWorkdir,
    encoder: &str,
) -> Result<ProcessOutcome, BoundaryError> {
    // Some NVENC devices reject very small dimensions. 256x256 avoids
    // classifying a working device as absent because of a 64x64 test frame.
    let argv = [
        "-hide_banner",
        "-loglevel",
        "error",
        "-f",
        "rawvideo",
        "-pix_fmt",
        "yuv420p",
        "-video_size",
        "256x256",
        "-framerate",
        "1",
        "-i",
        "-",
        "-frames:v",
        "2",
        "-an",
        "-c:v",
        encoder,
        "-f",
        "null",
        "-",
    ];
    let mut spec = probe_spec(path, &argv, workdir.path());
    spec.timeout = ENCODER_PROBE_TIMEOUT;
    spec.max_output_bytes = ENCODER_PROBE_LOG_CAP;
    let cancellation = ProcessCancellation::new();
    // The null muxer creates no artifact, but the child still has a scratch
    // budget. The watched leaf is deliberately absent during a normal probe.
    let watch = ArtifactWatch::start(
        workdir,
        &workdir.join("probe-output"),
        ENCODER_PROBE_DISK_CAP,
        cancellation.clone(),
    )?;
    let payload = probe_frames();
    let input_bytes = payload.len() as u64;
    let result = (|| {
        let process = runner.start(
            &spec,
            cancellation,
            ProcessStdinLimits::new(input_bytes, input_bytes),
        )?;
        finish_probe_input(process, &payload)
    })();
    watch.finish()?;
    result
}

fn finish_probe_input(
    mut process: Box<dyn RunningProcess>,
    payload: &[u8],
) -> Result<ProcessOutcome, BoundaryError> {
    let written = process.write_stdin(payload);
    let outcome = process.finish()?;
    // Hardware initialization can fail before FFmpeg consumes stdin.
    // Preserve its exit status and useful stderr instead of replacing
    // "Cannot load libcuda.so.1" with the resulting broken-pipe error.
    match written {
        Ok(()) => Ok(outcome),
        Err(ProcessError::Plumbing { .. }) if !outcome.success() => Ok(outcome),
        Err(error) => Err(BoundaryError::Mechanism(error)),
    }
}

fn probe_frames() -> Vec<u8> {
    let mut frames = vec![128_u8; 2 * PROBE_FRAME_BYTES];
    for frame in frames.as_chunks_mut::<PROBE_FRAME_BYTES>().0 {
        frame[..PROBE_LUMA_BYTES].fill(16);
    }
    frames
}

fn classify_probe(encoder: &str, outcome: &ProcessOutcome) -> EncoderProbe {
    let detail = match outcome.termination {
        ProcessTermination::Exited(Some(0)) => None,
        ProcessTermination::TimedOut => Some(format!(
            "functional probe exceeded its {} ms timeout",
            ENCODER_PROBE_TIMEOUT.as_millis()
        )),
        ProcessTermination::OutputLimitExceeded => Some(format!(
            "functional probe exceeded its {ENCODER_PROBE_LOG_CAP}-byte log limit"
        )),
        ProcessTermination::Cancelled => Some("functional probe was cancelled".to_owned()),
        ProcessTermination::Exited(code) => {
            let stderr = String::from_utf8_lossy(&outcome.stderr);
            let first_line = stderr.lines().find_map(|line| {
                let clean: String = line
                    .chars()
                    .filter(|character| !character.is_control())
                    .take(512)
                    .collect();
                let clean = clean.trim();
                (!clean.is_empty()).then(|| clean.to_owned())
            });
            Some(first_line.unwrap_or_else(|| {
                format!("functional probe exited unsuccessfully (code {code:?})")
            }))
        }
    };
    EncoderProbe {
        encoder: encoder.to_owned(),
        available: outcome.success(),
        detail,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::negotiate::{ColorDescription, EncoderChoice};

    #[cfg(unix)]
    struct HelpOutcomeRunner {
        outcome: ProcessOutcome,
        specs: std::sync::Mutex<Vec<ProcessSpec>>,
    }

    #[cfg(unix)]
    impl ProcessRunner for HelpOutcomeRunner {
        fn mechanism(&self) -> ProcessMechanism {
            ProcessMechanism::Scripted
        }

        fn start(
            &self,
            spec: &ProcessSpec,
            cancellation: ProcessCancellation,
            limits: ProcessStdinLimits,
        ) -> Result<Box<dyn RunningProcess>, ProcessError> {
            self.specs.lock().unwrap().push(spec.clone());
            let mut runner = fmn_platform::process::ScriptedRunner::new();
            runner.script(spec.program.clone(), self.outcome.clone());
            runner.start(spec, cancellation, limits)
        }
    }

    #[cfg(unix)]
    fn help_probe_tool() -> FfmpegTool {
        use fmn_platform::process::{FfmpegLocator as _, StdFfmpegLocator};

        let source_path = std::env::current_exe().unwrap();
        let executable = StdFfmpegLocator::default()
            .locate_ffmpeg(&source_path)
            .unwrap();
        let path = executable.canonical_path().to_path_buf();
        let (mut source, native_image) = executable.open_current().unwrap();
        let sha256_hex = sha256_open_file(&mut source, &path, native_image.file_bytes).unwrap();
        FfmpegTool {
            executable,
            path,
            sha256_hex,
            native_image,
            version: "ffmpeg version controlled-outcome".to_owned(),
            session: ToolSession::create(&std::env::temp_dir().canonicalize().unwrap()).unwrap(),
            encoder_probes: Arc::default(),
            encoder_formats: Arc::default(),
        }
    }

    #[cfg(unix)]
    #[test]
    fn failed_help_retains_the_actual_terminal_reason_and_unchanged_limits() {
        let tool = help_probe_tool();
        let mut missing = Vec::new();
        for termination in [
            ProcessTermination::Exited(Some(23)),
            ProcessTermination::Exited(None),
            ProcessTermination::TimedOut,
            ProcessTermination::OutputLimitExceeded,
            ProcessTermination::Cancelled,
        ] {
            let runner = HelpOutcomeRunner {
                outcome: ProcessOutcome {
                    termination,
                    stdout: b"partial help\0".to_vec(),
                    stderr: b"driver diagnostic\xff\n".to_vec(),
                },
                specs: std::sync::Mutex::default(),
            };
            let error = tool
                .probe_encoder_pixel_formats(&runner, "h264_nvenc")
                .unwrap_err();
            let detail = error.to_string();
            if !detail.contains(&format!("{termination:?}"))
                || !detail.contains(tool.sha256_hex())
                || !detail.contains("2000")
                || !detail.contains("65536")
            {
                missing.push(format!("{termination:?}: {detail}"));
            }
            let BoundaryError::ProbeProcessFailed(failure) = &error else {
                panic!("expected the actual failed process outcome: {error:?}");
            };
            assert_eq!(failure.outcome, runner.outcome);
            assert_eq!(failure.source_path, tool.path);
            assert_eq!(failure.executable_sha256, tool.sha256_hex);
            assert_eq!(failure.version, tool.version);
            let specs = runner.specs.lock().unwrap();
            assert_eq!(specs.len(), 1, "failed discovery must never be cached");
            assert_eq!(specs[0].timeout, Duration::from_secs(2));
            assert_eq!(specs[0].max_output_bytes, 64 * 1024);
            assert_eq!(failure.spec.argv, specs[0].argv);
            assert_eq!(failure.spec.program, specs[0].program);
            assert_eq!(failure.spec.env, specs[0].env);
            assert_eq!(failure.spec.timeout, specs[0].timeout);
            assert_eq!(failure.spec.max_output_bytes, specs[0].max_output_bytes);
            assert_eq!(
                specs[0].argv,
                vec![
                    "-hide_banner".to_owned(),
                    "-h".to_owned(),
                    "encoder=h264_nvenc".to_owned(),
                ]
            );
        }
        assert!(missing.is_empty(), "lost probe evidence: {missing:?}");
    }

    #[cfg(unix)]
    #[test]
    fn help_failure_display_is_escaped_and_bounded_without_losing_raw_streams() {
        let tool = help_probe_tool();
        let mut stderr = vec![0xff; 64 * 1024];
        stderr[..7].copy_from_slice(b"\x1b[31m\r\n");
        let runner = HelpOutcomeRunner {
            outcome: ProcessOutcome {
                termination: ProcessTermination::OutputLimitExceeded,
                stdout: vec![b'x'; 64 * 1024],
                stderr,
            },
            specs: std::sync::Mutex::default(),
        };
        let error = tool
            .probe_encoder_pixel_formats(&runner, "h264_nvenc")
            .unwrap_err();
        let text = error.to_string();
        assert!(text.contains("OutputLimitExceeded"));
        assert!(text.contains("\\x1b[31m\\r\\n"));
        assert!(text.contains("excerpt truncated"));
        assert!(text.len() < 4096);
        assert!(!text.chars().any(char::is_control));
        let BoundaryError::ProbeProcessFailed(failure) = error else {
            panic!("expected structured failed probe");
        };
        assert_eq!(failure.outcome, runner.outcome);
    }

    #[cfg(unix)]
    #[test]
    fn successful_help_still_parses_and_caches_only_the_verified_formats() {
        let tool = help_probe_tool();
        let runner = HelpOutcomeRunner {
            outcome: ProcessOutcome {
                termination: ProcessTermination::Exited(Some(0)),
                stdout: b"Encoder h264_nvenc [NVENC]:\n    Supported pixel formats: p010le nv12\n"
                    .to_vec(),
                stderr: Vec::new(),
            },
            specs: std::sync::Mutex::default(),
        };
        let formats = tool
            .probe_encoder_pixel_formats(&runner, "h264_nvenc")
            .unwrap();
        assert_eq!(formats.formats, advertised("nv12 p010le"));
        assert_eq!(
            tool.probe_encoder_pixel_formats(&runner, "h264_nvenc")
                .unwrap(),
            formats
        );
        assert_eq!(runner.specs.lock().unwrap().len(), 1);
    }

    fn format_job(wire: WireFormat) -> VideoJob {
        VideoJob {
            width: 64,
            height: 36,
            fps: (30000, 1001),
            wire,
            color: ColorDescription::for_native_wire(wire),
            container: Container::Mp4,
            encoder: EncoderChoice::Auto,
            crf: Some(18),
        }
    }

    fn advertised(formats: &str) -> Vec<String> {
        formats.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn encoder_help_is_bound_to_the_selected_encoder_and_keeps_exact_format_names() {
        let help = "Encoder libx264 [libx264 H.264 / AVC]:\r\n    General capabilities: dr1 delay\r\n    Supported pixel formats: yuv420p nv12 yuv420p10le nv12\r\nlibx264 AVOptions:\r\n";
        let formats = parse_pixel_formats("libx264", help).unwrap();
        assert_eq!(formats.encoder, "libx264");
        assert_eq!(formats.formats, advertised("nv12 yuv420p yuv420p10le"));
        assert!(parse_pixel_formats("h264_nvenc", help).is_err());
        assert!(parse_pixel_formats("libx264", "Codec libx264 is not recognized.\n").is_err());
        let unknown = parse_pixel_formats(
            "dynamic_encoder",
            "Encoder dynamic_encoder [Dynamic formats]:\n    General capabilities: none\n",
        )
        .unwrap();
        assert!(unknown.formats.is_empty());
    }

    #[test]
    fn malformed_and_unbounded_encoder_format_lists_are_refused() {
        for values in ["nv12 -pix_fmt", "nv12 yuv420p;", "nv12 \u{1b}[31m"] {
            let help =
                format!("Encoder example [Example]:\n    Supported pixel formats: {values}\n");
            assert!(parse_pixel_formats("example", &help).is_err());
        }
        let duplicated = "Encoder example [Example]:\n    Supported pixel formats: nv12\n    Supported pixel formats: yuv420p\n";
        assert!(parse_pixel_formats("example", duplicated).is_err());
        let repeated = "nv12 ".repeat(MAX_ENCODER_PIXEL_FORMATS + 1);
        let help = format!("Encoder example [Example]:\n    Supported pixel formats: {repeated}\n");
        assert!(parse_pixel_formats("example", &help).is_err());
        let long_name = "x".repeat(65);
        let help =
            format!("Encoder example [Example]:\n    Supported pixel formats: {long_name}\n");
        assert!(parse_pixel_formats("example", &help).is_err());
    }

    #[test]
    fn nv12_negotiation_uses_the_advertised_format_without_changing_quality_or_color() {
        let job = format_job(WireFormat::Nv12);
        let out = Path::new("/out.mp4");
        let original = encode_argv(&job, out).unwrap();
        let direct = encode_argv_with_formats(&job, out, &advertised("yuv420p nv12")).unwrap();
        let fallback = encode_argv_with_formats(&job, out, &advertised("yuv420p")).unwrap();
        let unknown = encode_argv_with_formats(&job, out, &[]).unwrap();
        assert_eq!(fallback, original);
        assert_eq!(unknown, original);
        let differences: Vec<_> = original
            .iter()
            .zip(&direct)
            .enumerate()
            .filter(|(_, (before, after))| before != after)
            .collect();
        assert_eq!(original.len(), direct.len());
        assert_eq!(differences.len(), 1);
        let (position, (before, after)) = differences[0];
        assert_eq!(original[position - 1], "-pix_fmt");
        assert_eq!(before, "yuv420p");
        assert_eq!(after, "nv12");
        assert!(direct.windows(2).any(|pair| pair == ["-crf", "18"]));
        assert_eq!(
            direct.iter().filter(|argument| *argument == "nv12").count(),
            2
        );
    }

    #[test]
    fn p010_negotiation_preserves_ten_bits_or_refuses_before_encoding() {
        let job = format_job(WireFormat::P010);
        let out = Path::new("/out.mp4");
        for (formats, expected) in [
            ("yuv420p p010le yuv420p10le", "p010le"),
            ("yuv420p yuv420p10le", "yuv420p10le"),
            ("yuv422p10le yuv444p10le", "yuv422p10le"),
            ("yuv444p10le", "yuv444p10le"),
        ] {
            let argv = encode_argv_with_formats(&job, out, &advertised(formats)).unwrap();
            let pixel_formats: Vec<_> = argv
                .windows(2)
                .filter(|pair| pair[0] == "-pix_fmt")
                .map(|pair| pair[1].as_str())
                .collect();
            assert_eq!(pixel_formats, ["p010le", expected]);
        }
        for formats in ["", "nv12 yuv420p", "yuv420p10be"] {
            let error = encode_argv_with_formats(&job, out, &advertised(formats)).unwrap_err();
            assert!(error.to_string().contains("10-bit"));
        }
    }

    #[test]
    fn format_discovery_does_not_change_alpha_rgb_or_gif_profiles() {
        for (wire, container) in [
            (WireFormat::Rgba8, Container::Mp4),
            (WireFormat::Bgra8, Container::Mov),
            (WireFormat::Rgba8, Container::MovTransparent),
            (WireFormat::Rgba8, Container::Gif),
        ] {
            let mut job = format_job(wire);
            job.container = container;
            job.crf = None;
            assert_eq!(
                encode_argv_with_formats(&job, Path::new("/out"), &[]).unwrap(),
                encode_argv(&job, Path::new("/out")).unwrap(),
            );
        }
    }

    fn failed(termination: ProcessTermination, stderr: Vec<u8>) -> EncoderProbe {
        classify_probe(
            "h264_nvenc",
            &ProcessOutcome {
                termination,
                stdout: Vec::new(),
                stderr,
            },
        )
    }

    #[test]
    fn unavailable_driver_preserves_only_a_bounded_control_free_diagnostic() {
        let probe = failed(
            ProcessTermination::Exited(Some(1)),
            b"\nCannot load libcuda.so.1\r\nencoder initialization failed\n".to_vec(),
        );
        assert!(!probe.available);
        assert_eq!(probe.detail.as_deref(), Some("Cannot load libcuda.so.1"));
        let mut noisy = vec![b'x'; 4096];
        noisy.insert(0, 0x1b);
        let probe = failed(ProcessTermination::Exited(Some(1)), noisy);
        let detail = probe.detail.expect("nonzero trial carries a reason");
        assert_eq!(detail.len(), 512);
        assert!(!detail.chars().any(char::is_control));
    }

    #[test]
    fn timeout_and_log_overflow_are_named_even_when_stderr_is_empty() {
        for (termination, expected) in [
            (ProcessTermination::TimedOut, "2000 ms timeout"),
            (
                ProcessTermination::OutputLimitExceeded,
                "16384-byte log limit",
            ),
            (ProcessTermination::Cancelled, "cancelled"),
            (ProcessTermination::Exited(None), "unsuccessfully"),
        ] {
            let probe = failed(termination, Vec::new());
            assert!(!probe.available);
            assert!(probe.detail.expect("named refusal").contains(expected));
        }
    }

    #[test]
    fn early_stdin_failure_keeps_hardware_stderr_but_not_hidden_budget_errors() {
        struct EarlyExit {
            program: PathBuf,
            budget_error: bool,
        }
        impl RunningProcess for EarlyExit {
            fn write_stdin(&mut self, bytes: &[u8]) -> Result<(), ProcessError> {
                if self.budget_error {
                    Err(ProcessError::StdinTotalLimit {
                        program: self.program.clone(),
                        attempted: bytes.len() as u64,
                        max: 0,
                    })
                } else {
                    Err(ProcessError::Plumbing {
                        program: self.program.clone(),
                        detail: "stdin write failed: broken pipe".to_owned(),
                    })
                }
            }

            fn finish(self: Box<Self>) -> Result<ProcessOutcome, ProcessError> {
                Ok(ProcessOutcome {
                    termination: ProcessTermination::Exited(Some(1)),
                    stdout: Vec::new(),
                    stderr: b"Cannot load libcuda.so.1\n".to_vec(),
                })
            }

            fn cancel(self: Box<Self>) -> Result<(), ProcessError> {
                Ok(())
            }
        }

        // Exercise the same stdin/finish policy independently of executable
        // discovery; the boundary contract tests cover its bound private copy.
        for budget_error in [false, true] {
            let process: Box<dyn RunningProcess> = Box::new(EarlyExit {
                program: PathBuf::from("/fmn-bound-ffmpeg"),
                budget_error,
            });
            let outcome = finish_probe_input(process, &probe_frames());
            if budget_error {
                assert!(matches!(
                    outcome,
                    Err(BoundaryError::Mechanism(
                        ProcessError::StdinTotalLimit { .. }
                    ))
                ));
            } else {
                let probe = classify_probe("h264_nvenc", &outcome.unwrap());
                assert_eq!(probe.detail.as_deref(), Some("Cannot load libcuda.so.1"));
            }
        }
    }

    #[test]
    fn a_successful_trial_does_not_turn_stderr_into_a_failure() {
        let probe = failed(
            ProcessTermination::Exited(Some(0)),
            b"driver notice".to_vec(),
        );
        assert!(probe.available);
        assert!(probe.detail.is_none());
    }
}
