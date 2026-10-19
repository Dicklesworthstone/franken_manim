//! Functional observations of the installed hardware encoders.
//!
//! An encoder listed by FFmpeg may still lack its driver or a usable device.
//! Trials use the same bound executable and containment as production jobs.
//! They establish that a small encode worked, not support for every job profile.

use super::*;

const ENCODER_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const ENCODER_PROBE_LOG_CAP: u64 = 16 * 1024;
const ENCODER_PROBE_DISK_CAP: u64 = 1024 * 1024;
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

impl EncoderCapabilities {
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
