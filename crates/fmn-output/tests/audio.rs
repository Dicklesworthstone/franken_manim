//! Audio ingestion tests: native-only contracts plus real compressed fixtures.

#[cfg(all(not(target_arch = "wasm32"), feature = "exact-process"))]
use fmn_codec::WavLimits;
use fmn_codec::{SampleFormat, encode_wav};
use fmn_output::audio::{AudioDecodeError, AudioDecodeLimits, AudioDecoder, AudioInputFormat};

#[test]
fn audio_native_pcm_never_needs_the_configured_executable() {
    let mut decoder =
        AudioDecoder::new(AudioDecodeLimits::default(), Some("/missing/ffmpeg".into())).unwrap();
    let samples = [0.25_f32, -0.125, 0.0, 0.75];
    let first = decoder
        .decode(&encode_wav(2, 32_000, SampleFormat::S16, &samples))
        .unwrap();
    let second = decoder
        .decode(&encode_wav(2, 32_000, SampleFormat::F32, &samples))
        .unwrap();
    assert_eq!(first.audio.samples, samples);
    assert_eq!(first.audio.sample_rate, 32_000);
    assert_eq!(first.audio.channels, 2);
    assert!(first.report.invocation.is_none());
    assert!(second.report.invocation.is_none());
    assert_ne!(first.report.source_digest, second.report.source_digest);
    assert_eq!(first.report.pcm_digest, second.report.pcm_digest);
}

#[test]
fn audio_job_policy_is_admitted_before_lazy_tool_discovery() {
    use fmn_output::JobLimits;
    use std::time::Duration;
    for limits in [
        JobLimits {
            timeout: Duration::ZERO,
            ..JobLimits::default()
        },
        JobLimits {
            timeout: Duration::MAX,
            ..JobLimits::default()
        },
        JobLimits {
            max_log_bytes: 0,
            ..JobLimits::default()
        },
        JobLimits {
            max_artifact_bytes: 0,
            ..JobLimits::default()
        },
    ] {
        let decoder =
            AudioDecoder::new(AudioDecodeLimits::default(), Some("/missing/ffmpeg".into()))
                .unwrap();
        assert!(matches!(
            decoder.with_job_limits(limits),
            Err(AudioDecodeError::InvalidJobLimits(_))
        ));
    }
    let mut decoder =
        AudioDecoder::new(AudioDecodeLimits::default(), Some("/missing/ffmpeg".into()))
            .unwrap()
            .with_job_limits(JobLimits {
                timeout: Duration::from_secs(37),
                max_artifact_bytes: 1,
                ..JobLimits::default()
            })
            .unwrap();
    let decoded = decoder
        .decode(&encode_wav(1, 48000, SampleFormat::F32, &[0.25]))
        .unwrap();
    assert_eq!(decoded.audio.samples, [0.25]);
    assert!(decoded.report.invocation.is_none());
}

#[test]
fn audio_native_only_refuses_transcode_without_substitution() {
    let mut decoder = AudioDecoder::new(AudioDecodeLimits::default(), None).unwrap();
    assert!(matches!(
        decoder.decode(b"fLaC12345678"),
        Err(AudioDecodeError::TranscoderRequired {
            format: AudioInputFormat::Flac
        })
    ));
    for text in [
        b"#EXTM3U\nhttps://example.invalid/song.mp3".as_slice(),
        b"ffconcat version 1.0\nfile '/private/voice.wav'",
        b"https://example.invalid/music",
    ] {
        assert!(matches!(
            decoder.decode(text),
            Err(AudioDecodeError::UnsupportedInput)
        ));
    }
}

#[test]
fn audio_deferred_boundary_runs_only_after_native_and_byte_admission() {
    use std::sync::{Arc, Mutex};

    // Adding an injected resolver must preserve the decoder's thread traits.
    fn send_sync<T: Send + Sync>() {}
    send_sync::<AudioDecoder>();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::clone(&requests);
    let mut decoder = AudioDecoder::with_boundary_resolver(
        AudioDecodeLimits {
            max_input_bytes: 1024,
            max_samples: 16,
        },
        move |format| {
            observed.lock().unwrap().push(format);
            Err(AudioDecodeError::Capability("injected refusal".into()))
        },
    )
    .unwrap();
    let wav = encode_wav(1, 8, SampleFormat::F32, &[0.25, -0.125]);
    assert_eq!(decoder.decode(&wav).unwrap().audio.samples, [0.25, -0.125]);
    assert!(matches!(
        decoder.decode(b"#EXTM3U\nhttps://example.invalid/music.mp3"),
        Err(AudioDecodeError::UnsupportedInput),
    ));
    assert!(matches!(
        decoder.decode(&wav[..wav.len() - 1]),
        Err(AudioDecodeError::Native(_)),
    ));
    assert!(matches!(
        decoder.decode(&encode_wav(1, 8, SampleFormat::F32, &[0.0; 17])),
        Err(AudioDecodeError::Native(
            fmn_codec::WavError::TooLarge { .. }
        )),
    ));
    assert!(matches!(
        decoder.decode(&[0; 1025]),
        Err(AudioDecodeError::InputOversized { .. }),
    ));
    assert!(requests.lock().unwrap().is_empty());
    for (source, format) in [
        (b"fLaCsource".as_slice(), AudioInputFormat::Flac),
        (b"OggSsource".as_slice(), AudioInputFormat::Ogg),
    ] {
        assert!(matches!(
            decoder.decode(source),
            Err(AudioDecodeError::Capability(ref message)) if message == "injected refusal",
        ));
        assert_eq!(requests.lock().unwrap().last(), Some(&format));
        // A refused external capability does not poison the native route.
        assert!(decoder.decode(&wav).unwrap().report.invocation.is_none());
    }
    assert_eq!(requests.lock().unwrap().len(), 2);
}

#[test]
fn audio_deferred_boundary_validates_limits_and_owns_its_process_policy() {
    use fmn_output::JobLimits;
    for limits in [
        AudioDecodeLimits {
            max_input_bytes: 0,
            max_samples: 1,
        },
        AudioDecodeLimits {
            max_input_bytes: 1,
            max_samples: u64::MAX,
        },
    ] {
        assert!(matches!(
            AudioDecoder::with_boundary_resolver(limits, |_| {
                panic!("invalid admission resolved a boundary")
            }),
            Err(AudioDecodeError::InvalidLimits),
        ));
    }
    let decoder = AudioDecoder::with_boundary_resolver(AudioDecodeLimits::default(), |_| {
        panic!("configuration must not resolve a boundary")
    })
    .unwrap();
    assert!(matches!(
        decoder.with_job_limits(JobLimits::default()),
        Err(AudioDecodeError::InvalidJobLimits(_)),
    ));
}

#[test]
fn audio_budgets_and_damaged_pcm_do_not_fall_through_to_ffmpeg() {
    let wav = encode_wav(1, 48_000, SampleFormat::F32, &[0.0; 64]);
    let mut decoder = AudioDecoder::new(
        AudioDecodeLimits {
            max_input_bytes: 32,
            max_samples: 16,
        },
        Some("/missing/ffmpeg".into()),
    )
    .unwrap();
    assert!(matches!(
        decoder.decode(&wav),
        Err(AudioDecodeError::InputOversized { .. })
    ));
    let mut decoder = AudioDecoder::new(
        AudioDecodeLimits {
            max_input_bytes: 4096,
            max_samples: 16,
        },
        Some("/missing/ffmpeg".into()),
    )
    .unwrap();
    assert!(matches!(
        decoder.decode(&wav),
        Err(AudioDecodeError::Native(
            fmn_codec::WavError::TooLarge { .. }
        ))
    ));
    assert!(matches!(
        decoder.decode(&wav[..50]),
        Err(AudioDecodeError::Native(fmn_codec::WavError::Truncated))
    ));
    assert!(
        AudioDecoder::new(
            AudioDecodeLimits {
                max_input_bytes: 0,
                max_samples: 1
            },
            None
        )
        .is_err()
    );
    assert!(
        AudioDecoder::new(
            AudioDecodeLimits {
                max_input_bytes: 1,
                max_samples: u64::MAX
            },
            None
        )
        .is_err()
    );
}

#[test]
fn audio_per_source_budgets_tighten_without_replacing_decoder_policy() {
    use std::sync::{Arc, Mutex};

    let calls = Arc::new(Mutex::new(0));
    let observed = Arc::clone(&calls);
    let mut decoder = AudioDecoder::with_boundary_resolver(
        AudioDecodeLimits {
            max_input_bytes: 256,
            max_samples: 4,
        },
        move |_| {
            *observed.lock().unwrap() += 1;
            Err(AudioDecodeError::Capability("one retained resolver".into()))
        },
    )
    .unwrap();
    let wav = encode_wav(1, 8, SampleFormat::F32, &[0.25, -0.125]);
    assert!(matches!(
        decoder.decode_with_limits(
            &wav,
            &AudioDecodeLimits {
                max_input_bytes: wav.len() as u64 - 1,
                max_samples: 4,
            },
        ),
        Err(AudioDecodeError::InputOversized { .. }),
    ));
    assert!(matches!(
        decoder.decode_with_limits(
            &wav,
            &AudioDecodeLimits {
                max_input_bytes: 256,
                max_samples: 1,
            },
        ),
        Err(AudioDecodeError::Native(
            fmn_codec::WavError::TooLarge { .. }
        )),
    ));
    assert!(matches!(
        decoder.decode_with_limits(
            b"fLaCsource",
            &AudioDecodeLimits {
                max_input_bytes: 256,
                max_samples: 0,
            },
        ),
        Err(AudioDecodeError::InvalidLimits),
    ));
    assert_eq!(*calls.lock().unwrap(), 0);
    // Larger per-call allowances cannot relax the original policy.
    assert!(matches!(
        decoder.decode_with_limits(
            &encode_wav(1, 8, SampleFormat::F32, &[0.25; 5]),
            &AudioDecodeLimits::default(),
        ),
        Err(AudioDecodeError::Native(
            fmn_codec::WavError::TooLarge { .. }
        )),
    ));
    let later = decoder.decode(&wav).unwrap();
    assert_eq!(later.audio.samples, [0.25, -0.125]);
    assert!(later.report.invocation.is_none());
    assert_eq!(*calls.lock().unwrap(), 0);
    assert!(matches!(
        decoder.decode_with_limits(b"fLaCsource", &AudioDecodeLimits::default()),
        Err(AudioDecodeError::Capability(ref message)) if message == "one retained resolver",
    ));
    assert_eq!(*calls.lock().unwrap(), 1);
}

#[test]
fn audio_rejects_unknown_channel_layouts_and_nonfinite_pcm() {
    let mut decoder = AudioDecoder::new(AudioDecodeLimits::default(), None).unwrap();
    for wav in [
        encode_wav(3, 48_000, SampleFormat::F32, &[0.0; 3]),
        encode_wav(1, 48_000, SampleFormat::F32, &[f32::NAN]),
        encode_wav(2, 48_000, SampleFormat::F32, &[0.25]),
    ] {
        assert!(matches!(
            decoder.decode(&wav),
            Err(AudioDecodeError::InvalidPcm(_))
        ));
    }
    let plus = decoder
        .decode(&encode_wav(1, 48_000, SampleFormat::F32, &[0.0]))
        .unwrap();
    let minus = decoder
        .decode(&encode_wav(1, 48_000, SampleFormat::F32, &[-0.0]))
        .unwrap();
    assert_eq!(plus.report.pcm_digest, minus.report.pcm_digest);
}

#[cfg(all(not(target_arch = "wasm32"), feature = "exact-process"))]
#[test]
fn audio_real_compressed_decode_keeps_pcm_and_records_exact_source() {
    let Some(root) = std::env::var_os("FMN_AUDIO_FIXTURE_DIR") else {
        assert!(
            std::env::var_os("FMN_REQUIRE_FFMPEG").is_none(),
            "required real audio fixtures are missing"
        );
        eprintln!("real audio decode: set FMN_AUDIO_FIXTURE_DIR to generated fixtures");
        return;
    };
    let root = std::path::PathBuf::from(root);
    let path = std::env::var_os("FMN_AUDIO_FFMPEG").map_or_else(|| "ffmpeg".into(), Into::into);
    let mut decoder = AudioDecoder::new(AudioDecodeLimits::default(), Some(path)).unwrap();
    let manifest = std::fs::read_to_string(root.join("manifest.tsv")).unwrap();
    let mut count = 0;
    for row in manifest.lines() {
        let fields: Vec<_> = row.split('\t').collect();
        let input = std::fs::read(root.join(fields[0])).unwrap();
        let expected = std::fs::read(root.join(fields[1])).unwrap();
        let channels: u16 = fields[2].parse().unwrap();
        let rate: u32 = fields[3].parse().unwrap();
        let result = decoder
            .decode(&input)
            .unwrap_or_else(|e| panic!("{}: {e}", fields[0]));
        assert_eq!(result.audio.channels, channels);
        assert_eq!(result.audio.sample_rate, rate);
        let actual: Vec<u8> = result
            .audio
            .samples
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        assert_eq!(
            actual, expected,
            "{} changed native sample values/order/count",
            fields[0]
        );
        assert_eq!(result.report.source_digest, fmn_hash::sha256(&input));
        let invocation = result
            .report
            .invocation
            .as_ref()
            .expect("external decode is not concealed");
        assert_eq!(invocation.provenance.encoder.as_deref(), Some("pcm_f32le"));
        assert_eq!(invocation.provenance.tool_sha256_hex.len(), 64);
        let i = invocation
            .provenance
            .argv
            .iter()
            .position(|a| a == "-i")
            .unwrap();
        assert!(invocation.provenance.argv[i + 1].ends_with("source.audio"));
        assert!(
            !invocation
                .provenance
                .argv
                .contains(&root.join(fields[0]).display().to_string())
        );
        assert!(
            !invocation.artifact.exists(),
            "decoded private file leaked outside its lifetime"
        );
        count += 1;
    }
    assert_eq!(count, 8, "real format coverage must not silently shrink");
    let mut limited = AudioDecoder::new(
        AudioDecodeLimits {
            max_input_bytes: 1 << 20,
            max_samples: 16,
        },
        Some("ffmpeg".into()),
    )
    .unwrap();
    assert!(matches!(
        limited.decode(&std::fs::read(root.join("flac.asset")).unwrap()),
        Err(AudioDecodeError::Native(
            fmn_codec::WavError::TooLarge { .. }
        ))
    ));
    // Independent source-precision check: FLAC's float intermediate must not
    // quantize the source to 16 bits before native gain and final WAV output.
    let flac = decoder
        .decode(&std::fs::read(root.join("flac.asset")).unwrap())
        .unwrap();
    let reference = fmn_codec::decode_wav(
        &std::fs::read(root.join("source.wav")).unwrap(),
        &WavLimits::default(),
    )
    .unwrap();
    assert_eq!(flac.audio.samples, reference.samples);
}

#[test]
fn audio_missing_decoder_does_not_disable_native_wav() {
    let mut decoder =
        AudioDecoder::new(AudioDecodeLimits::default(), Some("/missing/ffmpeg".into())).unwrap();
    assert!(matches!(
        decoder.decode(b"fLaC12345678"),
        Err(AudioDecodeError::Capability(_))
    ));
    let result = decoder
        .decode(&encode_wav(1, 48000, SampleFormat::F32, &[0.25]))
        .unwrap();
    assert!(result.report.invocation.is_none());
}

#[cfg(all(not(target_arch = "wasm32"), feature = "exact-process"))]
#[test]
fn audio_real_file_cap_is_not_a_successfully_truncated_soundtrack() {
    let Some(root) = std::env::var_os("FMN_AUDIO_FIXTURE_DIR") else {
        assert!(
            std::env::var_os("FMN_REQUIRE_FFMPEG").is_none(),
            "required real audio fixtures are missing"
        );
        return;
    };
    use fmn_output::{Boundary, BoundaryError, FfmpegTool, JobLimits};
    use fmn_platform::process::{FfmpegLocator, StdFfmpegLocator, StdProcessRunner};
    let runner = std::sync::Arc::new(StdProcessRunner);
    let executable = StdFfmpegLocator::from_host_path()
        .locate_ffmpeg(std::path::Path::new("ffmpeg"))
        .unwrap();
    let workdir = std::env::temp_dir();
    let tool = FfmpegTool::resolve(executable, runner.as_ref(), &workdir).unwrap();
    let boundary = Boundary::new(
        tool,
        runner,
        JobLimits {
            max_artifact_bytes: 1024,
            ..JobLimits::default()
        },
        workdir,
    )
    .unwrap();
    let mut decoder = AudioDecoder::with_boundary(AudioDecodeLimits::default(), boundary).unwrap();
    let input = std::fs::read(std::path::PathBuf::from(root).join("flac.asset")).unwrap();
    assert!(matches!(
        decoder.decode(&input),
        Err(AudioDecodeError::Boundary(
            BoundaryError::ArtifactOversized { .. }
        ))
    ));
}
