//! Drive the actual standalone WAV front door with one scripted process.

#[path = "../../fmn/tests/support/compressed_audio.rs"]
mod compressed_audio;

use super::*;
use compressed_audio::Fixture;
use fmn_platform::fs::VirtualFs;

const COMPRESSED: &[u8] = b"fLaC narration fixture";
const OUTPUT: &str = "/soundtrack-output/sound_cue.wav";

fn source_wav() -> Vec<u8> {
    // 1/4 second, resampled by the native mixer and placed at the builtin's
    // real 1/2-second cue time. The external decoder never resamples it.
    fmn_codec::encode_wav(
        1,
        32,
        SampleFormat::F32,
        &[0.25, -0.5, 0.75, 0.0, 0.125, 0.25, -0.25, 0.5],
    )
}

fn run(fs: Arc<VirtualFs>, fixture: &Fixture, workers: usize, extra: &[&str]) -> RunOutput {
    let mut args = [
        "--robot",
        "--format",
        "wav",
        "--resolution",
        "16x16",
        "--fps",
        "8",
        "--video_dir",
        "/soundtrack-output",
        "--ffmpeg",
    ]
    .map(str::to_owned)
    .to_vec();
    args.push(fixture.tool.to_str().unwrap().to_owned());
    args.extend(["--threads".to_owned(), workers.to_string()]);
    args.extend(extra.iter().map(|value| (*value).to_owned()));
    args.extend([
        BUILTIN_SCENE_SOURCE.to_owned(),
        fmn::builtins::SOUND_CUE_SCENE_NAME.to_owned(),
    ]);
    run_with_capabilities(
        args,
        fs,
        Arc::clone(&fixture.capability.runner),
        fixture.capability.locator.as_ref(),
    )
}

fn manifest(fs: &VirtualFs) -> ProvenanceManifest {
    let directory = adjacent_manifest_destination(Path::new(OUTPUT)).unwrap();
    ProvenanceManifest::from_bytes(&fs.read(&directory.join("manifest.fmnp")).unwrap()).unwrap()
}

#[test]
fn cli_compressed_sources_keep_native_wav_bits_and_source_process_policy_receipts() {
    for format in ["s16", "f32"] {
        for workers in [1, 4, 16] {
            let wav = source_wav();
            let fixture = Fixture::new(None, wav.clone(), false);
            let native = Arc::new(VirtualFs::new());
            native.insert(fmn::builtins::SOUND_CUE_ASSET_NAME, wav.clone());
            let mut encoding = vec!["--audio-sample-format", format];
            if format == "s16" {
                encoding.extend(["--audio-dither-seed", "53"]);
            }
            let reference = run(native.clone(), &fixture, workers, &encoding);
            assert_eq!(
                reference.code, 0,
                "{}{}",
                reference.stdout, reference.stderr
            );
            assert!(fixture.locates.lock().unwrap().is_empty());
            assert!(fixture.runs.lock().unwrap().is_empty());
            let expected = native.read(Path::new(OUTPUT)).unwrap();

            let fs = Arc::new(VirtualFs::new());
            fs.insert(fmn::builtins::SOUND_CUE_ASSET_NAME, COMPRESSED.to_vec());
            let mut extra = encoding;
            extra.extend([
                "--ffmpeg-timeout",
                "11",
                "--ffmpeg-max-log-bytes",
                "73",
                "--ffmpeg-max-artifact-bytes",
                "65536",
                "--ffmpeg-max-input-bytes",
                "22",
            ]);
            let result = run(fs.clone(), &fixture, workers, &extra);
            assert_eq!(result.code, 0, "{}{}", result.stdout, result.stderr);
            let actual = fs.read(Path::new(OUTPUT)).unwrap();
            assert_eq!(actual, expected);
            let decoded = decode_wav(&actual, &WavLimits::default()).unwrap();
            assert_eq!(
                (decoded.sample_rate, decoded.channels, decoded.frames()),
                (48_000, 2, 48_000)
            );
            assert!(
                decoded.samples[48_000..72_000]
                    .iter()
                    .any(|&sample| sample != 0.0)
            );
            if format == "f32" {
                assert!(
                    decoded.samples[..48_000]
                        .iter()
                        .all(|&sample| sample == 0.0)
                );
            }
            let pcm = AudioDecoder::new(AudioDecodeLimits::default(), None)
                .unwrap()
                .decode(&wav)
                .unwrap()
                .report
                .pcm_digest;
            for identity in [
                fmn_hash::sha256(COMPRESSED).to_hex(),
                pcm.to_hex(),
                fmn_hash::sha256(&std::fs::read(&fixture.tool).unwrap()).to_hex(),
                fmn_hash::sha256(&actual).to_hex(),
            ] {
                assert!(
                    result.stdout.contains(&identity),
                    "missing receipt {identity}"
                );
            }
            assert!(result.stdout.contains("\"decoder\":\"ffmpeg\""));
            assert!(result.stdout.contains("\"audio_decodes\":[{"));
            assert!(result.stdout.contains("\"max_input_bytes\":22"));
            assert!(result.stdout.contains("\"max_artifact_bytes\":65536"));
            assert!(result.stdout.contains("\"-f\",\"flac\""));
            assert!(result.stdout.contains(&format!(
                "\"wav_encoding\":{{\"sample_rate\":48000,\"channels\":2,\"sample_format\":\"{format}\""
            )));
            let manifest = manifest(&fs);
            assert_eq!(manifest.mode, ManifestMode::Standard);
            assert!(manifest.outputs.iter().all(|output| !output.certified));
            assert!(
                manifest
                    .items
                    .iter()
                    .any(|item| item.item_id == 1 && item.digest == fmn_hash::sha256(COMPRESSED))
            );
            assert!(manifest.items.iter().any(|item| item.item_id == 4
                && item.virtual_path.as_deref() == Some("native-wav-decoding.json")));
            assert!(manifest.items.iter().any(|item| item.item_id == 4
                && item.virtual_path.as_deref() == Some("native-wav-encoding.json")));
            let runs = fixture.runs.lock().unwrap();
            assert_eq!(runs.len(), 2);
            assert_eq!(runs[1].timeout, Duration::from_secs(11));
            assert_eq!(runs[1].max_output_bytes, 73);
            assert_eq!(*fixture.sources.lock().unwrap(), vec![COMPRESSED.to_vec()]);
        }
    }
}

#[test]
fn cli_wav_budgets_and_decoder_failure_publish_no_artifact_or_manifest() {
    for (extra, source, refuse) in [
        (
            vec!["--ffmpeg-max-input-bytes", "21"],
            COMPRESSED.to_vec(),
            false,
        ),
        (
            vec!["--ffmpeg-max-artifact-bytes", "8"],
            COMPRESSED.to_vec(),
            false,
        ),
        (vec![], COMPRESSED.to_vec(), true),
        (vec![], source_wav()[..20].to_vec(), false),
        (
            vec![],
            b"#EXTM3U\nhttps://example.invalid/voice.mp3".to_vec(),
            false,
        ),
    ] {
        let fs = Arc::new(VirtualFs::new());
        fs.insert(fmn::builtins::SOUND_CUE_ASSET_NAME, source.clone());
        let fixture = Fixture::new(None, source_wav(), refuse);
        let result = run(fs.clone(), &fixture, 1, &extra);
        assert_ne!(result.code, 0, "unexpected success: {}", result.stdout);
        assert!(!fs.exists(Path::new(OUTPUT)));
        assert!(!fs.exists(&adjacent_manifest_destination(Path::new(OUTPUT)).unwrap()));
        assert_eq!(
            fs.read(Path::new(fmn::builtins::SOUND_CUE_ASSET_NAME))
                .unwrap(),
            source
        );
        if extra.first() == Some(&"--ffmpeg-max-input-bytes") || source != COMPRESSED {
            assert!(fixture.locates.lock().unwrap().is_empty());
            assert!(fixture.runs.lock().unwrap().is_empty());
        }
    }
}

#[test]
fn certified_cli_wav_refuses_compressed_decode_before_optional_discovery() {
    let fs = Arc::new(VirtualFs::new());
    fs.insert(fmn::builtins::SOUND_CUE_ASSET_NAME, COMPRESSED.to_vec());
    let fixture = Fixture::new(None, source_wav(), false);
    let result = run(fs.clone(), &fixture, 1, &["--reproducible"]);
    assert_ne!(result.code, 0);
    // Builds with unidentified sources are refused before scene execution;
    // identified builds reach the explicit compressed-input policy refusal.
    assert!(
        result.stdout.contains("certified soundtrack preparation")
            || result.stdout.contains("identified build"),
        "{}",
        result.stdout
    );
    assert!(fixture.locates.lock().unwrap().is_empty());
    assert!(fixture.runs.lock().unwrap().is_empty());
    assert!(!fs.exists(Path::new(OUTPUT)));
    assert!(!fs.exists(&adjacent_manifest_destination(Path::new(OUTPUT)).unwrap()));
}
