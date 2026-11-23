//! Audio ingestion tests: native-only contracts plus real compressed fixtures.

#[cfg(all(not(target_arch = "wasm32"), feature = "exact-process"))]
use fmn_codec::WavLimits;
use fmn_codec::{
    ChannelLayout, SampleFormat, WavAudio, WavError, encode_wav, encode_wav_with_layout,
};
use fmn_output::audio::{AudioDecodeError, AudioDecodeLimits, AudioDecoder, AudioInputFormat};
use fmn_output::{DitherPolicy, MixKernel, MixerConfig, SoundCue, SoundError, SoundMixer};

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
    assert!(matches!(
        decoder.decode(&encode_wav(3, 48_000, SampleFormat::F32, &[0.0; 3])),
        Err(AudioDecodeError::Native(
            WavError::UnsupportedChannelLayout {
                channels: 3,
                channel_mask: None,
            }
        ))
    ));
    assert!(matches!(
        decoder.decode(&encode_wav(1, 48_000, SampleFormat::F32, &[f32::NAN])),
        Err(AudioDecodeError::InvalidPcm(_))
    ));
    assert!(matches!(
        decoder.decode(&encode_wav(2, 48_000, SampleFormat::F32, &[0.25])),
        Err(AudioDecodeError::Native(WavError::BadFmt(
            "incomplete interleaved sample frame"
        )))
    ));
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

fn layout_audio(layout: ChannelLayout, samples: Vec<f32>) -> WavAudio {
    WavAudio {
        channels: layout.channels(),
        channel_mask: Some(layout.mask()),
        sample_rate: 8_000,
        format: SampleFormat::F32,
        samples,
    }
}

fn layout_mixer(channels: u16) -> SoundMixer {
    SoundMixer::new(MixerConfig {
        channels,
        sample_rate: 8_000,
        max_output_frames: 128,
    })
    .unwrap()
}

#[test]
fn audio_native_speaker_impulses_reach_the_declared_mono_stereo_matrix() {
    let k = std::f64::consts::FRAC_1_SQRT_2;
    // Independent per-speaker analytic expectations, in file channel order.
    // The codec's layout enum supplies admission only, not expected weights.
    let layouts: &[(ChannelLayout, &[[f64; 2]])] = &[
        (ChannelLayout::Mono, &[[1.0, 1.0]]),
        (ChannelLayout::Stereo, &[[1.0, 0.0], [0.0, 1.0]]),
        (
            ChannelLayout::StereoWithLfe,
            &[[1.0, 0.0], [0.0, 1.0], [0.0, 0.0]],
        ),
        (ChannelLayout::Surround3, &[[1.0, 0.0], [0.0, 1.0], [k, k]]),
        (
            ChannelLayout::Quad,
            &[[1.0, 0.0], [0.0, 1.0], [k, 0.0], [0.0, k]],
        ),
        (
            ChannelLayout::Surround4,
            &[[1.0, 0.0], [0.0, 1.0], [k, k], [0.5, 0.5]],
        ),
        (
            ChannelLayout::Surround5,
            &[[1.0, 0.0], [0.0, 1.0], [k, k], [k, 0.0], [0.0, k]],
        ),
        (
            ChannelLayout::Surround5Side,
            &[[1.0, 0.0], [0.0, 1.0], [k, k], [k, 0.0], [0.0, k]],
        ),
        (
            ChannelLayout::Surround5Point1,
            &[
                [1.0, 0.0],
                [0.0, 1.0],
                [k, k],
                [0.0, 0.0],
                [k, 0.0],
                [0.0, k],
            ],
        ),
        (
            ChannelLayout::Surround5Point1Side,
            &[
                [1.0, 0.0],
                [0.0, 1.0],
                [k, k],
                [0.0, 0.0],
                [k, 0.0],
                [0.0, k],
            ],
        ),
        (
            ChannelLayout::Surround6Point1,
            &[
                [1.0, 0.0],
                [0.0, 1.0],
                [k, k],
                [0.0, 0.0],
                [0.5, 0.5],
                [k, 0.0],
                [0.0, k],
            ],
        ),
        (
            ChannelLayout::Surround7Point1,
            &[
                [1.0, 0.0],
                [0.0, 1.0],
                [k, k],
                [0.0, 0.0],
                [k, 0.0],
                [0.0, k],
                [k, 0.0],
                [0.0, k],
            ],
        ),
    ];
    let mut decoder = AudioDecoder::with_boundary_resolver(AudioDecodeLimits::default(), |_| {
        panic!("native speaker layouts must never resolve an external decoder")
    })
    .unwrap();
    for &(layout, weights) in layouts {
        let channels = usize::from(layout.channels());
        let mut samples = vec![0.0; channels * channels];
        for channel in 0..channels {
            samples[channel * channels + channel] = 0.5;
        }
        let input = layout_audio(layout, samples);
        let decoded = decoder
            .decode(&encode_wav_with_layout(&input, SampleFormat::F32).unwrap())
            .unwrap();
        assert_eq!(decoded.audio, input);
        assert!(decoded.report.invocation.is_none());
        for output_channels in [1, 2] {
            let mut mixer = layout_mixer(output_channels);
            mixer
                .add(SoundCue::new(decoded.audio.clone(), 1, 8_000))
                .unwrap();
            let mut expected = vec![0.0_f32; usize::from(output_channels)];
            for &[left, right] in weights {
                if output_channels == 1 {
                    expected.push(((left + right) * 0.25) as f32);
                } else {
                    expected.extend_from_slice(&[(left * 0.5) as f32, (right * 0.5) as f32]);
                }
            }
            let expected_bits: Vec<_> = expected.iter().map(|sample| sample.to_bits()).collect();
            for &kernel in MixKernel::ALL {
                for threads in [1, 4, 16] {
                    let report = mixer.mix_with_kernel(threads, kernel).unwrap();
                    let actual: Vec<_> = report
                        .audio
                        .samples
                        .iter()
                        .map(|sample| sample.to_bits())
                        .collect();
                    assert_eq!(
                        actual, expected_bits,
                        "{layout:?}, output={output_channels}, threads={threads}"
                    );
                    assert_eq!(report.clipped_samples, 0);
                    assert_eq!(report.audio.channel_mask, None);
                    let wav = report
                        .wav_bytes(SampleFormat::F32, DitherPolicy::None)
                        .unwrap();
                    assert_eq!(decoder.decode(&wav).unwrap().audio.samples, expected);
                }
            }
        }
    }
}

#[test]
fn audio_surround_mapping_keeps_headroom_until_final_gain_ducking_and_clamp() {
    let foreground = layout_audio(ChannelLayout::Surround5Point1, vec![1.0; 6]);
    let mut clipped = layout_mixer(2);
    clipped
        .add(SoundCue::new(foreground.clone(), 0, 8_000))
        .unwrap();
    let report = clipped.mix(4).unwrap();
    assert_eq!(report.audio.samples, [1.0, 1.0]);
    assert_eq!(report.clipped_samples, 2);

    let mut gained = layout_mixer(2);
    gained
        .add(SoundCue::new(
            layout_audio(ChannelLayout::Stereo, vec![0.4, 0.4]),
            0,
            8_000,
        ))
        .unwrap();
    let mut cue = SoundCue::new(foreground, 0, 8_000);
    cue.gain = Some(-20.0);
    cue.gain_to_background = Some(-20.0);
    gained.add(cue).unwrap();
    let expected = (f64::from(0.4_f32) + 1.0 + 2.0 * std::f64::consts::FRAC_1_SQRT_2) * 0.1;
    for &kernel in MixKernel::ALL {
        let report = gained.mix_with_kernel(16, kernel).unwrap();
        assert_eq!(report.clipped_samples, 0);
        for sample in report.audio.samples {
            assert!((f64::from(sample) - expected).abs() < 1e-7);
        }
    }
    let mut lfe_only = layout_mixer(2);
    lfe_only
        .add(SoundCue::new(
            layout_audio(
                ChannelLayout::Surround5Point1,
                vec![0.0, 0.0, 0.0, 100.0, 0.0, 0.0],
            ),
            0,
            8_000,
        ))
        .unwrap();
    assert_eq!(lfe_only.mix(1).unwrap().audio.samples, [0.0, 0.0]);
}

#[test]
fn audio_pcm_identity_binds_surround_positions_and_preserves_mono_stereo_v1() {
    let mut decoder = AudioDecoder::new(AudioDecodeLimits::default(), None).unwrap();
    let back = layout_audio(ChannelLayout::Surround5Point1, vec![0.25; 6]);
    let side = layout_audio(ChannelLayout::Surround5Point1Side, vec![0.25; 6]);
    let back = decoder
        .decode(&encode_wav_with_layout(&back, SampleFormat::F32).unwrap())
        .unwrap();
    let side = decoder
        .decode(&encode_wav_with_layout(&side, SampleFormat::F32).unwrap())
        .unwrap();
    assert_eq!(back.audio.samples, side.audio.samples);
    assert_ne!(back.report.pcm_digest, side.report.pcm_digest);
    for layout in [ChannelLayout::Mono, ChannelLayout::Stereo] {
        let audio = layout_audio(layout, vec![0.25; usize::from(layout.channels())]);
        let explicit = decoder
            .decode(&encode_wav_with_layout(&audio, SampleFormat::F32).unwrap())
            .unwrap();
        let legacy = decoder
            .decode(&encode_wav(
                audio.channels,
                audio.sample_rate,
                SampleFormat::S16,
                &audio.samples,
            ))
            .unwrap();
        assert_ne!(explicit.report.source_digest, legacy.report.source_digest);
        assert_eq!(explicit.report.pcm_digest, legacy.report.pcm_digest);
        // The old domain tag and field order are a compatibility contract.
        let mut hash = fmn_hash::Sha256::new();
        hash.update(b"fmn-normalized-pcm-v1\0");
        hash.update(&audio.channels.to_le_bytes());
        hash.update(&audio.sample_rate.to_le_bytes());
        hash.update(&(audio.samples.len() as u64).to_le_bytes());
        for sample in audio.samples {
            hash.update(&sample.to_le_bytes());
        }
        assert_eq!(explicit.report.pcm_digest, hash.finalize());
    }
}

#[test]
fn audio_unknown_masks_and_surround_budgets_refuse_before_external_discovery() {
    let source = layout_audio(ChannelLayout::Surround5Point1, vec![0.25; 6]);
    let good = encode_wav_with_layout(&source, SampleFormat::F32).unwrap();
    let mut decoder = AudioDecoder::with_boundary_resolver(AudioDecodeLimits::default(), |_| {
        panic!("invalid source layout must not fall through to an external decoder")
    })
    .unwrap();
    for mask in [0_u32, 0x3, 0x7, 0x3f000, 0x8000_003f] {
        let mut bytes = good.clone();
        bytes[40..44].copy_from_slice(&mask.to_le_bytes());
        assert!(matches!(
            decoder.decode(&bytes),
            Err(AudioDecodeError::Native(WavError::UnsupportedChannelLayout {
                channels: 6,
                channel_mask,
            })) if channel_mask == (mask != 0).then_some(mask)
        ));
        let mut direct = source.clone();
        direct.channel_mask = (mask != 0).then_some(mask);
        let mut mixer = layout_mixer(2);
        assert!(matches!(
            mixer.add(SoundCue::new(direct, 0, 8_000)),
            Err(SoundError::UnsupportedChannelLayout { channels: 6, .. })
        ));
        assert!(mixer.is_empty());
    }
    assert!(matches!(
        decoder.decode_with_limits(
            &good,
            &AudioDecodeLimits {
                max_input_bytes: 1024,
                max_samples: 5
            },
        ),
        Err(AudioDecodeError::Native(WavError::TooLarge {
            max_samples: 5
        }))
    ));
    assert!(decoder.decode(&good).unwrap().report.invocation.is_none());
}
