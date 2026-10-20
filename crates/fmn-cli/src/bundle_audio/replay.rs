//! Prepared narrated recordings are Send; live Stage snapshots are not.
//! Decode, validate and freeze once on the owner, then transfer immutable input
//! into the batch pool. All outputs use the same existing rendering loop.

use super::*;
use fmn_scene::timeline_bundle::SharedTimelineBundle;

pub(super) struct PreparedReplay {
    pub request: Request,
    fps: u32,
    total_frames: u32,
    source_digest: fmn_hash::Digest,
    source: PathBuf,
    soundtrack: Option<BundleSoundtrack>,
    provenance: provenance::Session,
    shared: Option<SharedTimelineBundle>,
    needs_camera: bool,
}

pub(super) fn prepare_replay(
    command: &RenderCommand,
    bytes: &[u8],
    mut bundle: TimelineBundle,
    fs: &dyn FileSystem,
    manifest_root: Option<&Path>,
) -> Result<PreparedReplay, Failure> {
    let request = if manifest_root.is_some() {
        prepare_with_manifests(command, &bundle, fs, manifest_root)?
    } else {
        prepare(command, &bundle, fs)?
    };
    let fps = bundle.fps();
    let total_frames = bundle.frame_count();
    let source_digest = fmn_hash::sha256(bytes);
    let soundtrack = bundle.take_soundtrack();
    let provenance = provenance::Session::new(
        &request.config,
        source_digest,
        &bundle,
        soundtrack
            .as_ref()
            .ok_or_else(|| Failure::new("scene", "recording has no embedded soundtrack"))?,
        fs,
    )?;
    let source = command
        .file
        .clone()
        .ok_or_else(|| Failure::new("usage", "missing FMTL source"))?;
    let video = matches!(request.target, Target::Video(_));
    let needs_camera = video && !bundle.has_camera_track() && bundle.requires_camera();
    // Convert the complete validated recording once, not once per output.
    // WAV releases pictures without freezing them or obtaining process access.
    let shared = if video {
        Some(
            bundle
                .into_shared()
                .map_err(|error| Failure::new("scene", error.to_string()))?,
        )
    } else {
        drop(bundle);
        None
    };
    Ok(PreparedReplay {
        request,
        fps,
        total_frames,
        source_digest,
        source,
        soundtrack,
        provenance,
        shared,
        needs_camera,
    })
}

pub(super) fn execute_prepared(
    command: &RenderCommand,
    prepared: PreparedReplay,
    fs: Arc<dyn FileSystem>,
    capability: &dyn Fn() -> Option<FfmpegCapability>,
    stdout: &mut String,
    checkpoint: &dyn Fn() -> Result<(), Failure>,
) -> Result<(), Failure> {
    let PreparedReplay {
        request,
        fps,
        total_frames,
        source_digest,
        source,
        mut soundtrack,
        provenance,
        shared,
        needs_camera,
    } = prepared;
    if command.prerun {
        let selected_frames = request.frames.end - request.frames.start;
        if command.common.robot {
            let _ = writeln!(
                stdout,
                "{{\"schema\":\"fmn.cli\",\"version\":{},\"kind\":\"prerun\",\"source\":\"compiled\",\"frames\":{selected_frames},\"segments\":{},\"source_frames\":[{},{}],\"source_segments\":[{},{}]}}",
                fmn_cli::ROBOT_SCHEMA_VERSION,
                request.segments.len(),
                request.frames.start,
                request.frames.end,
                request.segments.start,
                request.segments.end,
            );
        } else if !command.common.quiet {
            let _ = writeln!(
                stdout,
                "compiled selection: {selected_frames} frames, {} segments; no scene executed",
                request.segments.len(),
            );
        }
    }
    checkpoint()?;
    let ffmpeg = if matches!(request.target, Target::Video(_)) {
        capability()
    } else {
        None
    };
    for (index, (destination, selection)) in request.outputs.iter().enumerate() {
        checkpoint()?;
        let range = selection.clone().unwrap_or(0..total_frames);
        let segments = if command.subdivide {
            let ordinal = request.segments.start + index;
            ordinal..ordinal + 1
        } else {
            request.segments.clone()
        };
        let (artifact_bytes, digest, frames, rate, channels, samples, video, publication) =
            match request.target {
                Target::Wav => {
                    let pcm = wav_window(&mut soundtrack, fps, &range, selection.is_none())?;
                    let rate = pcm.sample_rate();
                    let channels = pcm.channels();
                    let samples = pcm.sample_frames();
                    let mix = MixReport {
                        audio: WavAudio {
                            channels,
                            sample_rate: rate,
                            format: SampleFormat::F32,
                            samples: pcm.into_samples(),
                        },
                        clipped_samples: 0,
                        cues_mixed: 1,
                        workers_used: 0,
                        kernel: MixKernel::Scalar,
                    };
                    let report = publish_wav_new(
                        fs.as_ref(),
                        &WavPublicationConfig {
                            destination: destination.clone(),
                            format: SampleFormat::S16,
                            dither: DitherPolicy::None,
                            max_artifact_bytes: DEFAULT_MAX_BUNDLE_BYTES as u64,
                            profile: None,
                        },
                        &mix,
                    )
                    .map_err(|error| Failure::new("render", error.to_string()))?;
                    let publication = provenance.publish_with_destination(
                        Arc::clone(&fs),
                        request
                            .manifests
                            .as_ref()
                            .map(|paths| paths[index].as_path()),
                        provenance::Artifact {
                            destination,
                            target: request.target,
                            digest: report.digest,
                            frames: range.clone(),
                            segments: segments.clone(),
                            keep_tail: selection.is_none(),
                            sample_rate: rate,
                            channels,
                            sample_frames: report.sample_frames,
                            video: None,
                        },
                    )?;
                    (
                        report.bytes,
                        report.digest.to_hex(),
                        0,
                        rate,
                        channels,
                        samples,
                        String::new(),
                        publication,
                    )
                }
                Target::Video(format) => {
                    let pictures = shared.as_ref().ok_or_else(|| {
                        Failure::new("internal", "missing prepared compiled pictures")
                    })?;
                    let mut options = RenderOptions::with_format(destination.clone(), format)
                        .map_err(|error| Failure::new("config", error.to_string()))?;
                    options.config = request.config.clone();
                    options.typeset_cache = false;
                    options.max_frames = fmn_scene::DEFAULT_MAX_BUNDLE_EXPORT_FRAMES;
                    let (limits, input_bytes) = command.ffmpeg_limits.effective_limits()?;
                    options.ffmpeg_limits = limits;
                    options.max_output_bytes = input_bytes;
                    options.ffmpeg = ffmpeg.clone();
                    if needs_camera {
                        options.camera = Some(options.camera_config()?);
                    }
                    let report = options.render_shared_bundle_with_fs(
                        pictures,
                        soundtrack.as_ref(),
                        selection.clone(),
                        Arc::clone(&fs),
                    )?;
                    let sound = report.artifact.soundtrack.as_ref().ok_or_else(|| {
                        Failure::new(
                            "internal",
                            format!(
                                "video {} was published but omitted its soundtrack receipt",
                                destination.display()
                            ),
                        )
                    })?;
                    let publication = provenance.publish_with_destination(
                        Arc::clone(&fs),
                        request
                            .manifests
                            .as_ref()
                            .map(|paths| paths[index].as_path()),
                        provenance::Artifact {
                            destination,
                            target: request.target,
                            digest: report.artifact.digest,
                            frames: range.clone(),
                            segments: segments.clone(),
                            keep_tail: selection.is_none(),
                            sample_rate: sound.sample_rate,
                            channels: sound.channels,
                            sample_frames: sound.sample_frames,
                            video: Some(&report),
                        },
                    )?;
                    (
                        report.artifact.bytes,
                        report.artifact.digest.to_hex(),
                        report.artifact.frame_count,
                        sound.sample_rate,
                        sound.channels,
                        sound.sample_frames,
                        video_facts(&report),
                        publication,
                    )
                }
            };
        // Success requires both media and its complete manifest generation.
        // A later failure never rolls back or overwrites earlier outputs.
        if command.common.robot {
            let subdivision = if command.subdivide {
                format!(",\"subdivision\":{}", segments.start)
            } else {
                String::new()
            };
            let manifest = publication.json();
            let certified = publication.certified;
            let _ = writeln!(
                stdout,
                "{{\"schema\":\"fmn.cli\",\"version\":{},\"kind\":\"bundle-audio-render\",\"source\":\"compiled\",\"source_artifact\":{},\"source_sha256\":{},\"scene\":{},\"artifact\":{},\"format\":{},\"bytes\":{artifact_bytes},\"sha256\":{},\"frames\":{frames},\"fps\":{fps},\"source_frames\":[{},{}],\"source_segments\":[{},{}],\"certified\":{certified},\"manifest\":{manifest},\"soundtrack\":{{\"sample_rate\":{rate},\"channels\":{channels},\"sample_frames\":{samples}}}{subdivision}{video}}}",
                fmn_cli::ROBOT_SCHEMA_VERSION,
                quote(&source.to_string_lossy()),
                quote(&source_digest.to_hex()),
                quote(&request.scene),
                quote(&destination.to_string_lossy()),
                quote(request.target.extension()),
                quote(&digest),
                range.start,
                range.end,
                segments.start,
                segments.end,
            );
        } else if !command.common.quiet {
            let _ = writeln!(
                stdout,
                "wrote {}: {artifact_bytes} bytes; {samples} audio frames at {rate} Hz / {channels} channels; manifest {}",
                destination.display(),
                publication.path.display(),
            );
        }
    }
    Ok(())
}
