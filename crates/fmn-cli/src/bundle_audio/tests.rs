use super::*;
use fmn_output::{ManifestMode, ProvenanceManifest};
use fmn_platform::fs::VirtualFs;

fn command(extra: &[&str]) -> RenderCommand {
    let mut args = vec![
        "/scene.fmtl",
        "--format",
        "wav",
        "--video_dir",
        "/out",
        "--robot",
    ];
    args.extend_from_slice(extra);
    let Invocation::Render(command) = fmn_cli::parse_args(args).unwrap() else {
        panic!("render invocation");
    };
    command
}

fn recording() -> Vec<u8> {
    let mut stage = fmn::mobject::Stage::new();
    let mut timeline = fmn::animation::Timeline::new(3).unwrap();
    timeline.wait(1.0 / 3.0).unwrap();
    timeline.wait(1.0 / 3.0).unwrap();
    timeline.wait(1.0 / 3.0).unwrap();
    let pictures = fmn_scene::export_timeline_bundle(
        timeline,
        &mut stage,
        &fmn_core::rng::RngRoot::from_seed(0),
    )
    .unwrap();
    let pcm = BundleSoundtrack::new(10, 2, [0.25, -0.5].repeat(14)).unwrap();
    fmn_scene::timeline_bundle::attach_bundle_soundtrack(&pictures, &pcm, DEFAULT_MAX_BUNDLE_BYTES)
        .unwrap()
}

#[test]
fn full_wav_uses_embedded_pcm_and_preserves_tail_without_process_capability() {
    let fs = Arc::new(VirtualFs::new());
    let bytes = recording();
    let output = run(
        command(&[]),
        &bytes,
        TimelineBundle::from_bytes(&bytes).unwrap(),
        fs.clone(),
        &|| panic!("WAV acquired ffmpeg"),
    );
    assert_eq!(output.code, 0, "{}", output.stderr);
    let wav = fs.read(Path::new("/out/scene.wav")).unwrap();
    let decoded = fmn_codec::decode_wav(&wav, &fmn_codec::WavLimits::default()).unwrap();
    assert_eq!(decoded.sample_rate, 10);
    assert_eq!(decoded.channels, 2);
    assert_eq!(decoded.samples, [0.25, -0.5].repeat(14));
    assert!(output.stdout.contains("\"sample_frames\":14"));
    assert!(output.stdout.contains("\"certified\":false"));
    assert!(
        output
            .stdout
            .contains("\"manifest\":{\"path\":\"/out/scene.wav.manifest/manifest.fmnp\"")
    );
}

#[test]
fn subdivision_windows_join_without_a_sample_gap_or_duplicate() {
    let fs = Arc::new(VirtualFs::new());
    let bytes = recording();
    let bundle = TimelineBundle::from_bytes(&bytes).unwrap();
    let command = command(&["--subdivide"]);
    let planned = prepare(&command, &bundle, fs.as_ref()).unwrap();
    let paths: Vec<_> = planned
        .outputs
        .iter()
        .map(|(path, _)| path.clone())
        .collect();
    let output = run(command, &bytes, bundle, fs.clone(), &|| {
        panic!("WAV acquired ffmpeg")
    });
    assert_eq!(output.code, 0, "{}", output.stderr);
    let mut joined = Vec::new();
    let mut counts = Vec::new();
    for path in paths {
        let wav = fs.read(&path).unwrap();
        let audio = fmn_codec::decode_wav(&wav, &fmn_codec::WavLimits::default()).unwrap();
        counts.push(audio.samples.len() / 2);
        joined.extend(audio.samples);
    }
    assert_eq!(counts, [3, 4, 3]);
    assert_eq!(joined, [0.25, -0.5].repeat(10));
    assert_eq!(output.stdout.lines().count(), 3);
}

#[test]
fn all_subdivision_destinations_are_preflighted_before_publication() {
    let fs = Arc::new(VirtualFs::new());
    let bytes = recording();
    let bundle = TimelineBundle::from_bytes(&bytes).unwrap();
    let command = command(&["--subdivide"]);
    let planned = prepare(&command, &bundle, fs.as_ref()).unwrap();
    fs.insert(planned.outputs[1].0.clone(), b"foreign generation".to_vec());
    let output = run(command, &bytes, bundle, fs.clone(), &|| {
        panic!("WAV acquired ffmpeg")
    });
    assert_ne!(output.code, 0);
    assert!(!fs.exists(&planned.outputs[0].0));
    assert_eq!(
        fs.read(&planned.outputs[1].0).unwrap(),
        b"foreign generation"
    );
}

#[test]
fn file_parent_refuses_before_native_publication() {
    let fs = Arc::new(VirtualFs::new());
    fs.insert("/out", b"not a directory".to_vec());
    let bytes = recording();
    let output = run(
        command(&[]),
        &bytes,
        TimelineBundle::from_bytes(&bytes).unwrap(),
        fs.clone(),
        &|| panic!("refusal acquired ffmpeg"),
    );
    assert_ne!(output.code, 0);
    assert_eq!(fs.read(Path::new("/out")).unwrap(), b"not a directory");
    assert!(!fs.exists(Path::new("/out/scene.wav")));
}

#[test]
fn unsupported_modes_and_clock_overrides_refuse_before_output() {
    for extra in [vec!["--fps", "4"], vec!["--write_all"]] {
        let fs = Arc::new(VirtualFs::new());
        let bytes = recording();
        let output = run(
            command(&extra),
            &bytes,
            TimelineBundle::from_bytes(&bytes).unwrap(),
            fs.clone(),
            &|| panic!("refusal acquired ffmpeg"),
        );
        assert_ne!(output.code, 0, "{extra:?}");
        assert!(!fs.exists(Path::new("/out/scene.wav")));
    }
    // The generated parser owns this CLI refusal, before native admission.
    let error =
        fmn_cli::parse_args(["/scene.fmtl", "--format", "wav", "--vcodec", "libx264"]).unwrap_err();
    assert_eq!(error.exit_name(), "usage");
    assert_eq!(error.rule(), Some("native-format-vs-codec"));
    let mut incompatible = command(&[]);
    incompatible.vcodec = Some("libx264".to_owned());
    let fs = Arc::new(VirtualFs::new());
    let bytes = recording();
    let output = run(
        incompatible,
        &bytes,
        TimelineBundle::from_bytes(&bytes).unwrap(),
        fs.clone(),
        &|| panic!("codec refusal acquired ffmpeg"),
    );
    assert_eq!(output.code, code("usage"));
    assert!(!fs.exists(Path::new("/out/scene.wav")));
}

#[test]
fn quiet_success_suppresses_human_decoration_not_robot_receipts() {
    let bytes = recording();
    for robot in [false, true] {
        let mut command = command(&["--quiet"]);
        command.common.robot = robot;
        let output = run(
            command,
            &bytes,
            TimelineBundle::from_bytes(&bytes).unwrap(),
            Arc::new(VirtualFs::new()),
            &|| None,
        );
        assert_eq!(output.code, 0);
        assert_eq!(output.stdout.is_empty(), !robot);
    }
}

#[test]
fn alpha_video_does_not_discard_explicit_encoder_or_wire_requests() {
    let fs = VirtualFs::new();
    let bundle = TimelineBundle::from_bytes(&recording()).unwrap();
    let mut command = command(&[]);
    command.format = OutputFormat::Video;
    command.transparent = true;
    command.vcodec = Some("libx264".to_owned());
    assert!(prepare(&command, &bundle, &fs).is_err());
    command.vcodec = Some("qtrle".to_owned());
    command.pix_fmt = Some("nv12".to_owned());
    assert!(prepare(&command, &bundle, &fs).is_err());
    command.pix_fmt = Some("rgba".to_owned());
    assert!(prepare(&command, &bundle, &fs).is_ok());
}

#[test]
fn json_strings_escape_controls_quotes_and_backslashes() {
    assert_eq!(quote("a\"\\\n\u{0000}\t界"), "\"a\\\"\\\\\\n\\u0000\\t界\"");
}

fn varying_recording(empty_segments: bool) -> (Vec<u8>, Vec<f32>) {
    let mut stage = fmn::mobject::Stage::new();
    let mut timeline = fmn::animation::Timeline::new(3).unwrap();
    let durations = if empty_segments {
        vec![0.0, 1.0 / 3.0, 0.0, 1.0 / 3.0, 1.0 / 3.0, 0.0]
    } else {
        vec![1.0 / 3.0; 3]
    };
    for duration in durations {
        timeline.wait(duration).unwrap();
    }
    let pictures = fmn_scene::export_timeline_bundle(
        timeline,
        &mut stage,
        &fmn_core::rng::RngRoot::from_seed(0),
    )
    .unwrap();
    let samples: Vec<_> = (0_u8..14)
        .flat_map(|index| [f32::from(index) / 32.0, -f32::from(index) / 32.0])
        .collect();
    let pcm = BundleSoundtrack::new(10, 2, samples.clone()).unwrap();
    let bytes = fmn_scene::timeline_bundle::attach_bundle_soundtrack(
        &pictures,
        &pcm,
        DEFAULT_MAX_BUNDLE_BYTES,
    )
    .unwrap();
    (bytes, samples)
}

fn wav_samples(fs: &VirtualFs, path: &str) -> Vec<f32> {
    let wav = fs.read(Path::new(path)).unwrap();
    fmn_codec::decode_wav(&wav, &fmn_codec::WavLimits::default())
        .unwrap()
        .samples
}

#[test]
fn ranged_wav_honors_exclusive_segments_absolute_offsets_and_tail_trimming() {
    let (bytes, samples) = varying_recording(false);
    for (selection, path, expected, frames) in [
        ("1,2", "/out/scene_1_2.wav", 6..14, "[1,2]"),
        ("1", "/out/scene_1.wav", 6..20, "[1,3]"),
        ("0", "/out/scene_0.wav", 0..20, "[0,3]"),
        ("0,3", "/out/scene_0_3.wav", 0..20, "[0,3]"),
        ("2,3", "/out/scene_2_3.wav", 14..20, "[2,3]"),
    ] {
        let fs = Arc::new(VirtualFs::new());
        let output = run(
            command(&["-n", selection]),
            &bytes,
            TimelineBundle::from_bytes(&bytes).unwrap(),
            fs.clone(),
            &|| panic!("range WAV acquired ffmpeg"),
        );
        assert_eq!(output.code, 0, "{}", output.stdout);
        assert_eq!(wav_samples(&fs, path), samples[expected]);
        assert!(
            output
                .stdout
                .contains(&format!("\"source_frames\":{frames}"))
        );
        assert!(!fs.exists(Path::new("/out/scene.wav")));
    }
}

#[test]
fn ranged_subdivision_keeps_original_ordinals_and_exact_sample_partition() {
    let (bytes, samples) = varying_recording(false);
    let fs = Arc::new(VirtualFs::new());
    let output = run(
        command(&["-n", "1,3", "--subdivide", "--prerun"]),
        &bytes,
        TimelineBundle::from_bytes(&bytes).unwrap(),
        fs.clone(),
        &|| panic!("range WAV acquired ffmpeg"),
    );
    assert_eq!(output.code, 0, "{}", output.stdout);
    let mut joined = wav_samples(&fs, "/out/scene_1_3/00001.wav");
    joined.extend(wav_samples(&fs, "/out/scene_1_3/00002.wav"));
    assert_eq!(joined, samples[6..20]);
    assert!(!fs.exists(Path::new("/out/scene_1_3/00000.wav")));
    assert!(output.stdout.contains("\"subdivision\":1"));
    assert!(output.stdout.contains("\"subdivision\":2"));
    assert!(!output.stdout.contains("\"subdivision\":0"));
    assert!(output.stdout.contains("\"frames\":2,\"segments\":2"));
    assert!(output.stdout.contains("\"source_segments\":[1,3]"));
}

#[test]
fn unselected_empty_segments_do_not_block_a_valid_excerpt() {
    let (bytes, samples) = varying_recording(true);
    let fs = Arc::new(VirtualFs::new());
    let output = run(
        command(&["-n", "3,5", "--subdivide"]),
        &bytes,
        TimelineBundle::from_bytes(&bytes).unwrap(),
        fs.clone(),
        &|| panic!("range WAV acquired ffmpeg"),
    );
    assert_eq!(output.code, 0, "{}", output.stdout);
    assert_eq!(wav_samples(&fs, "/out/scene_3_5/00003.wav"), samples[6..14]);
    assert_eq!(
        wav_samples(&fs, "/out/scene_3_5/00004.wav"),
        samples[14..20]
    );
    for extra in [vec!["-n", "0,1"], vec!["-n", "1,4", "--subdivide"]] {
        let fs = Arc::new(VirtualFs::new());
        let output = run(
            command(&extra),
            &bytes,
            TimelineBundle::from_bytes(&bytes).unwrap(),
            fs.clone(),
            &|| panic!("empty refusal acquired ffmpeg"),
        );
        assert_ne!(output.code, 0);
        assert!(!fs.exists(Path::new("/out")));
    }
    let fs = Arc::new(VirtualFs::new());
    let output = run(
        command(&["-n", "1,4"]),
        &bytes,
        TimelineBundle::from_bytes(&bytes).unwrap(),
        fs.clone(),
        &|| None,
    );
    assert_eq!(output.code, 0, "{}", output.stdout);
    assert_eq!(wav_samples(&fs, "/out/scene_1_4.wav"), samples[..14]);
}

#[test]
fn invalid_compiled_ranges_refuse_without_output_or_process_access() {
    let bytes = recording();
    for (start, end) in [
        (3, None),
        (1, Some(4)),
        (2, Some(1)),
        (0, Some(0)),
        (u64::MAX, None),
    ] {
        let fs = Arc::new(VirtualFs::new());
        let mut command = command(&[]);
        // A host may construct requests without going through parse_args.
        command.animation_range = Some(AnimationRange { start, end });
        let output = run(
            command,
            &bytes,
            TimelineBundle::from_bytes(&bytes).unwrap(),
            fs.clone(),
            &|| panic!("invalid range acquired ffmpeg"),
        );
        assert_eq!(output.code, code("usage"));
        assert!(!fs.exists(Path::new("/out")));
    }
}

#[test]
fn selected_preflight_preserves_unselected_outputs_and_refuses_late_collisions() {
    let bytes = recording();
    for collide in [false, true] {
        let fs = Arc::new(VirtualFs::new());
        fs.insert("/out/scene_1_3/00000.wav", b"unselected".to_vec());
        if collide {
            fs.insert("/out/scene_1_3/00002.wav", b"existing winner".to_vec());
        }
        let output = run(
            command(&["-n", "1,3", "--subdivide"]),
            &bytes,
            TimelineBundle::from_bytes(&bytes).unwrap(),
            fs.clone(),
            &|| None,
        );
        assert_eq!(output.code == 0, !collide, "{}", output.stdout);
        assert_eq!(
            fs.read(Path::new("/out/scene_1_3/00000.wav")).unwrap(),
            b"unselected"
        );
        if collide {
            assert!(!fs.exists(Path::new("/out/scene_1_3/00001.wav")));
            assert_eq!(
                fs.read(Path::new("/out/scene_1_3/00002.wav")).unwrap(),
                b"existing winner"
            );
        }
    }
}

#[test]
fn selected_work_budget_and_explicit_names_do_not_charge_the_prefix() {
    let bytes = recording();
    let bundle = TimelineBundle::from_bytes(&bytes).unwrap();
    assert!(selected_ranges(&bundle, None, 1).is_err());
    let selection = Some(AnimationRange {
        start: 1,
        end: Some(2),
    });
    assert_eq!(
        selected_ranges(&bundle, selection, 1).unwrap(),
        (1..2, 1..2)
    );
    let fs = Arc::new(VirtualFs::new());
    let output = run(
        command(&["-n", "1,2", "--file_name", "excerpt", "--prerun"]),
        &bytes,
        bundle,
        fs.clone(),
        &|| None,
    );
    assert_eq!(output.code, 0, "{}", output.stdout);
    assert!(fs.exists(Path::new("/out/excerpt.wav")));
    assert!(output.stdout.contains("\"source_segments\":[1,2]"));
    assert!(output.stdout.contains("\"frames\":1,\"segments\":1"));
}

fn manifest(fs: &VirtualFs, artifact: &str) -> ProvenanceManifest {
    let directory = provenance::destination(Path::new(artifact)).unwrap();
    assert!(fs.exists(&directory.join(fmn_platform::fs::ATOMIC_DIRECTORY_COMPLETE_LEAF)));
    let manifest =
        ProvenanceManifest::from_bytes(&fs.read(&directory.join("manifest.fmnp")).unwrap())
            .unwrap();
    assert_eq!(
        fs.read(&directory.join("manifest.txt")).unwrap(),
        manifest.to_text().as_bytes()
    );
    assert_eq!(
        manifest.outputs[0].digest,
        fmn_hash::sha256(&fs.read(Path::new(artifact)).unwrap())
    );
    manifest
}

#[test]
fn manifest_binds_recording_bytes_actual_output_and_every_closure_class() {
    let fs = Arc::new(VirtualFs::new());
    let bytes = recording();
    let output = run(
        command(&[]),
        &bytes,
        TimelineBundle::from_bytes(&bytes).unwrap(),
        fs.clone(),
        &|| panic!("manifest creation acquired ffmpeg for WAV"),
    );
    assert_eq!(output.code, 0, "{}", output.stdout);
    let manifest = manifest(&fs, "/out/scene.wav");
    assert_eq!(manifest.mode, ManifestMode::Standard);
    assert_eq!(manifest.outputs[0].virtual_path, "scene.wav");
    assert_eq!(manifest.outputs[0].kind, "wav");
    assert!(!manifest.outputs[0].certified);
    for class in 1..=10 {
        assert!(manifest.items.iter().any(|item| item.item_id == class));
    }
    let source = manifest
        .items
        .iter()
        .find(|item| item.item_id == 1)
        .unwrap();
    assert_eq!(source.digest, fmn_hash::sha256(&bytes));
    assert_eq!(
        source.virtual_path.as_deref(),
        Some("compiled/recording.fmtl")
    );
    assert!(
        manifest
            .items
            .iter()
            .any(|item| item.item_id == 9 && item.detail.contains("filesystem-only"))
    );
    assert!(output.stdout.contains(&manifest.closure_digest.to_hex()));
    assert!(
        output
            .stdout
            .contains(&manifest.semantic_digest().unwrap().to_hex())
    );
}

#[test]
fn every_manifest_destination_is_preflighted_before_the_first_clip() {
    let fs = Arc::new(VirtualFs::new());
    fs.insert("/out/scene/00002.wav.manifest", b"foreign sidecar".to_vec());
    let bytes = recording();
    let output = run(
        command(&["--subdivide"]),
        &bytes,
        TimelineBundle::from_bytes(&bytes).unwrap(),
        fs.clone(),
        &|| panic!("sidecar collision acquired ffmpeg"),
    );
    assert_ne!(output.code, 0);
    for index in 0..3 {
        assert!(!fs.exists(Path::new(&format!("/out/scene/{index:05}.wav"))));
    }
    assert!(!fs.exists(Path::new("/out/scene/00000.wav.manifest")));
    assert_eq!(
        fs.read(Path::new("/out/scene/00002.wav.manifest")).unwrap(),
        b"foreign sidecar"
    );
    assert!(!output.stdout.contains("\"kind\":\"bundle-audio-render\""));
}

#[test]
fn closure_is_independent_of_output_directory_and_thread_count_but_not_selection() {
    let fs = Arc::new(VirtualFs::new());
    let (bytes, _) = varying_recording(false);
    let mut first: Option<ProvenanceManifest> = None;
    for threads in [1, 4, 16] {
        let directory = format!("/workers-{threads}");
        let mut request = command(&["-n", "1,2"]);
        request.common.threads = Some(threads);
        request.video_dir = Some(PathBuf::from(&directory));
        let output = run(
            request,
            &bytes,
            TimelineBundle::from_bytes(&bytes).unwrap(),
            fs.clone(),
            &|| None,
        );
        assert_eq!(output.code, 0, "{}", output.stdout);
        let observed = manifest(&fs, &format!("{directory}/scene_1_2.wav"));
        if let Some(first) = &first {
            assert_eq!(observed.closure_digest, first.closure_digest);
            assert_eq!(
                observed.semantic_digest().unwrap(),
                first.semantic_digest().unwrap()
            );
            assert_eq!(observed.outputs, first.outputs);
        } else {
            first = Some(observed);
        }
    }
    let output = run(
        command(&["-n", "2,3"]),
        &bytes,
        TimelineBundle::from_bytes(&bytes).unwrap(),
        fs.clone(),
        &|| None,
    );
    assert_eq!(output.code, 0, "{}", output.stdout);
    let other = manifest(&fs, "/out/scene_2_3.wav");
    let first = first.unwrap();
    assert_ne!(
        other.semantic_digest().unwrap(),
        first.semantic_digest().unwrap()
    );
    assert_eq!(
        other.items.iter().find(|item| item.item_id == 1),
        first.items.iter().find(|item| item.item_id == 1)
    );
}

fn provenance_session(fs: &VirtualFs, bytes: &[u8]) -> provenance::Session {
    let mut bundle = TimelineBundle::from_bytes(bytes).unwrap();
    let config = prepare(&command(&[]), &bundle, fs).unwrap().config;
    let pcm = bundle.take_soundtrack().unwrap();
    provenance::Session::new(&config, fmn_hash::sha256(bytes), &bundle, &pcm, fs).unwrap()
}

fn full_artifact(path: &Path, digest: fmn_hash::Digest) -> provenance::Artifact<'_> {
    provenance::Artifact {
        destination: path,
        target: Target::Wav,
        digest,
        frames: 0..3,
        segments: 0..3,
        keep_tail: true,
        sample_rate: 10,
        channels: 2,
        sample_frames: 14,
        video: None,
    }
}

#[test]
fn incorrect_output_metadata_cannot_be_sealed_as_a_new_input_identity() {
    for fault in 0..5 {
        let fs = Arc::new(VirtualFs::new());
        let session = provenance_session(&fs, &recording());
        let path = Path::new("/published.wav");
        fs.insert(path, b"already-published media".to_vec());
        let mut output = full_artifact(path, fmn_hash::sha256(b"already-published media"));
        match fault {
            0 => output.sample_rate = 11,
            1 => output.channels = 1,
            2 => output.sample_frames = 13,
            3 => output.frames = 1..3,
            _ => output.segments = 0..4,
        }
        let failure = session.publish(fs.clone(), output).err().unwrap();
        assert_eq!(failure.identity, "internal");
        assert!(!fs.exists(Path::new("/published.wav.manifest")));
        assert_eq!(fs.read(path).unwrap(), b"already-published media");
    }
}

#[test]
fn output_digests_are_evidence_not_inputs_to_the_closure() {
    let fs = Arc::new(VirtualFs::new());
    let session = provenance_session(&fs, &recording());
    for (path, bytes) in [
        ("/a/scene.wav", b"first".as_slice()),
        ("/b/scene.wav", b"different".as_slice()),
    ] {
        fs.insert(path, bytes.to_vec());
        session
            .publish(
                fs.clone(),
                full_artifact(Path::new(path), fmn_hash::sha256(bytes)),
            )
            .unwrap();
    }
    let a = manifest(&fs, "/a/scene.wav");
    let b = manifest(&fs, "/b/scene.wav");
    assert_eq!(a.closure_digest, b.closure_digest);
    assert_eq!(a.semantic_digest().unwrap(), b.semantic_digest().unwrap());
    assert_ne!(a.outputs[0].digest, b.outputs[0].digest);
}

#[test]
fn sidecar_publication_never_overwrites_a_post_preflight_winner() {
    let fs = Arc::new(VirtualFs::new());
    let session = provenance_session(&fs, &recording());
    let path = Path::new("/race.wav");
    provenance::preflight(fs.as_ref(), path).unwrap();
    fs.insert(path, b"published media".to_vec());
    let mut winner = fs
        .clone()
        .begin_atomic_directory(Path::new("/race.wav.manifest"))
        .unwrap();
    winner
        .write_file(Path::new("winner.txt"), b"other generation")
        .unwrap();
    winner.prepare().unwrap().commit().unwrap();
    assert!(
        session
            .publish(
                fs.clone(),
                full_artifact(path, fmn_hash::sha256(b"published media"))
            )
            .is_err()
    );
    assert_eq!(
        fs.read(Path::new("/race.wav.manifest/winner.txt")).unwrap(),
        b"other generation"
    );
    assert!(!fs.exists(Path::new("/race.wav.manifest/manifest.fmnp")));
    assert_eq!(fs.read(path).unwrap(), b"published media");
}

#[test]
fn certified_video_is_refused_before_any_process_or_output() {
    let bytes = recording();
    for transparent in [false, true] {
        let mut request = command(&["--reproducible"]);
        request.format = OutputFormat::Video;
        request.transparent = transparent;
        let fs = Arc::new(VirtualFs::new());
        let output = run(
            request,
            &bytes,
            TimelineBundle::from_bytes(&bytes).unwrap(),
            fs.clone(),
            &|| panic!("certified video acquired a process capability"),
        );
        assert_eq!(output.code, code("capability"));
        assert!(output.stdout.contains("outside the certified artifact set"));
        assert!(!fs.exists(Path::new("/out")));
    }
}

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ),
    all(target_os = "macos", target_arch = "aarch64"),
))]
#[test]
fn certified_wav_full_cut_and_subdivisions_have_comparable_bits_at_every_thread_count() {
    let (bytes, samples) = varying_recording(false);
    for extra in [vec![], vec!["-n", "1,2"], vec!["-n", "1", "--subdivide"]] {
        let mut first: Option<Vec<ProvenanceManifest>> = None;
        let fs = Arc::new(VirtualFs::new());
        for threads in [1, 4, 16] {
            let directory = format!("/certified-{threads}");
            let mut arguments = vec!["--reproducible"];
            arguments.extend_from_slice(&extra);
            let mut request = command(&arguments);
            request.common.threads = Some(threads);
            request.video_dir = Some(PathBuf::from(&directory));
            let bundle = TimelineBundle::from_bytes(&bytes).unwrap();
            let planned = prepare(&request, &bundle, fs.as_ref()).unwrap();
            let output = run(request, &bytes, bundle, fs.clone(), &|| {
                panic!("certified WAV acquired ffmpeg")
            });
            assert_eq!(output.code, 0, "{}", output.stdout);
            assert!(output.stdout.contains("\"certified\":true"));
            let manifests: Vec<_> = planned
                .outputs
                .iter()
                .map(|(path, _)| {
                    let m = manifest(&fs, path.to_str().unwrap());
                    assert_eq!(m.mode, ManifestMode::Certified);
                    assert!(m.outputs[0].certified);
                    assert_eq!(m.identity.engine, "compiled-pcm:scalar:s16-no-dither/v1");
                    m
                })
                .collect();
            if let Some(first) = &first {
                for (a, b) in first.iter().zip(&manifests) {
                    assert!(a.compare(b).unwrap().certified_bits_agree());
                    assert_eq!(a.closure_digest, b.closure_digest);
                }
            } else {
                first = Some(manifests);
            }
            let mut actual = Vec::new();
            for (path, _) in &planned.outputs {
                actual.extend(wav_samples(&fs, path.to_str().unwrap()));
            }
            let expected = if extra.is_empty() {
                &samples[..]
            } else if extra.contains(&"--subdivide") {
                &samples[6..20]
            } else {
                &samples[6..14]
            };
            assert_eq!(actual, expected);
        }
    }
}

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ),
    all(target_os = "macos", target_arch = "aarch64"),
))]
#[test]
fn certified_config_layer_and_standard_mode_keep_identical_native_wav_bytes() {
    let fs = Arc::new(VirtualFs::new());
    let bytes = recording();
    let standard = run(
        command(&[]),
        &bytes,
        TimelineBundle::from_bytes(&bytes).unwrap(),
        fs.clone(),
        &|| None,
    );
    assert_eq!(standard.code, 0, "{}", standard.stdout);
    fs.insert(
        "/certified.yml",
        b"determinism:\n  mode: certified\n".to_vec(),
    );
    let mut request = command(&["--config_file", "/certified.yml"]);
    request.video_dir = Some(PathBuf::from("/certified"));
    assert!(!request.common.reproducible);
    let output = run(
        request,
        &bytes,
        TimelineBundle::from_bytes(&bytes).unwrap(),
        fs.clone(),
        &|| panic!("configured certified WAV acquired ffmpeg"),
    );
    assert_eq!(output.code, 0, "{}", output.stdout);
    assert_eq!(
        fs.read(Path::new("/out/scene.wav")).unwrap(),
        fs.read(Path::new("/certified/scene.wav")).unwrap()
    );
    assert_eq!(
        manifest(&fs, "/certified/scene.wav").mode,
        ManifestMode::Certified
    );
    assert_eq!(manifest(&fs, "/out/scene.wav").mode, ManifestMode::Standard);
}
