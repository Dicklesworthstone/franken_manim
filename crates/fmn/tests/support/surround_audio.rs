//! Independent surround fixtures and matrix oracles shared by native and e2e tests.

use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::Arc;

use fmn::exporting::BundleExportOptions;
use fmn::exporting::soundtrack::{BundleSoundtrackOptions, export_bundle_with_soundtrack_bytes};
use fmn::rendering::RenderError;
use fmn::{SceneConstruct, SoundtrackRenderOptions, Stage, render_soundtrack_with_fs};
use fmn_codec::{SampleFormat, WavLimits, decode_wav, encode_wav};
use fmn_config::config::DeterminismMode;
use fmn_hash::sha256;
use fmn_output::MixerConfig;
use fmn_platform::fs::{FileSystem, VirtualFs};
use fmn_scene::{SoundData, SoundDataLimits, SoundSource, TimelineBundle};

const LAYOUTS: [(u16, u32); 2] = [(6, 0x3f), (8, 0x63f)];

/// Eight frames, each of the first `channels` frames containing one 1/8-scale
/// speaker impulse. This RIFF writer is deliberately independent of the native
/// extensible encoder whose output and downstream interpretation are under test.
fn fixture(channels: u16, mask: Option<u32>) -> Vec<u8> {
    let header = if mask.is_some() { 68 } else { 44 };
    let data_len = 8 * u32::from(channels) * 2;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(header - 8 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&(if mask.is_some() { 40_u32 } else { 16 }).to_le_bytes());
    bytes.extend_from_slice(&(if mask.is_some() { 0xfffe_u16 } else { 1 }).to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&32_u32.to_le_bytes());
    bytes.extend_from_slice(&(32 * u32::from(channels) * 2).to_le_bytes());
    bytes.extend_from_slice(&(channels * 2).to_le_bytes());
    bytes.extend_from_slice(&16_u16.to_le_bytes());
    if let Some(mask) = mask {
        bytes.extend_from_slice(&22_u16.to_le_bytes());
        bytes.extend_from_slice(&16_u16.to_le_bytes());
        bytes.extend_from_slice(&mask.to_le_bytes());
        bytes.extend_from_slice(&[1, 0, 0, 0, 0, 0, 16, 0, 128, 0, 0, 170, 0, 56, 155, 113]);
    }
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    for frame in 0..8 {
        for channel in 0..channels {
            let sample = if frame == channel { 4096_i16 } else { 0 };
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
    }
    bytes
}

/// The two coincident cues sum to 1/4-scale per speaker. The explicit rows are
/// the expected FL, FR, FC, LFE, BL, BR, SL, SR responses, including LFE silence.
/// No production channel mapper or mixer participates in this oracle.
fn oracle(input_channels: u16, output_channels: u16) -> Vec<f32> {
    let surround = 0.25 * std::f64::consts::FRAC_1_SQRT_2;
    let rows = [
        [0.25, 0.0],
        [0.0, 0.25],
        [surround, surround],
        [0.0, 0.0],
        [surround, 0.0],
        [0.0, surround],
        [surround, 0.0],
        [0.0, surround],
    ];
    let mut samples = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let [left, right] = if index < usize::from(input_channels) {
            *row
        } else {
            [0.0, 0.0]
        };
        if output_channels == 1 {
            samples.push(((left + right) * 0.5) as f32);
        } else {
            samples.extend([left as f32, right as f32]);
        }
    }
    samples
}

struct TwoCues(SoundSource);

impl SceneConstruct for TwoCues {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        for _ in 0..2 {
            stage
                .scene_mut()
                .add_sound_source(self.0.clone(), 0.0, None, None)?;
        }
        stage.wait(0.25)?;
        Ok(())
    }
}

fn source(
    fs: &VirtualFs,
    channels: u16,
    mask: u32,
    in_memory: bool,
) -> Result<(SoundSource, Vec<u8>), String> {
    let bytes = fixture(channels, Some(mask));
    if in_memory {
        let frozen = SoundData::from_wav_bytes(&bytes, SoundDataLimits::default())
            .map_err(|error| format!("surround memory admission: {error}"))?;
        let decoded = decode_wav(frozen.bytes(), &WavLimits::default())
            .map_err(|error| format!("surround canonical WAV: {error}"))?;
        if frozen.channel_mask() != Some(mask) || decoded.channel_mask != Some(mask) {
            return Err("memory admission lost the surround speaker mask".into());
        }
        Ok((SoundSource::Memory(frozen.clone()), frozen.bytes().to_vec()))
    } else {
        let path = format!("/speakers-{mask:x}.wav");
        fs.insert(&path, bytes.clone());
        Ok((SoundSource::File(path.into()), bytes))
    }
}

fn options(path: &str, channels: u16, workers: usize) -> Result<SoundtrackRenderOptions, String> {
    let mut options = SoundtrackRenderOptions::new(path).map_err(|error| error.to_string())?;
    options.config.camera.fps = 8;
    options.config.camera.resolution = (0, 0);
    options.config.determinism.mode = DeterminismMode::Certified;
    options.mixer = MixerConfig {
        sample_rate: 32,
        channels,
        max_output_frames: 8,
    };
    options.threads = NonZeroUsize::new(workers).ok_or("zero workers")?;
    options.max_scene_frames = 2;
    options.typeset_cache = false;
    Ok(options)
}

pub struct WavVerification {
    pub renders: u64,
    pub refusals: u64,
    pub wav: Vec<u8>,
}

/// Both source representations must reach the public native render path,
/// preserve input identities, decode once, and publish the independent matrix
/// oracle at every worker count.
pub fn verify_wav() -> Result<WavVerification, String> {
    let fs = Arc::new(VirtualFs::new());
    let mut renders = 0;
    let mut artifact = None;
    for (channels, mask) in LAYOUTS {
        let mut pcm_digest = None;
        for in_memory in [false, true] {
            let (source, source_bytes) = source(fs.as_ref(), channels, mask, in_memory)?;
            for output_channels in [1, 2] {
                let expected = encode_wav(
                    output_channels,
                    32,
                    SampleFormat::S16,
                    &oracle(channels, output_channels),
                );
                for workers in [1, 4, 16] {
                    let path = format!("/mix-{mask:x}-{in_memory}-{output_channels}-{workers}.wav");
                    let mut request = options(&path, output_channels, workers)?;
                    request.max_output_bytes = expected.len() as u64;
                    request.preparation.max_cues = 2;
                    // Exact unique-source budgets make an accidental second
                    // read/decode fail even if its bytes happen to be equal.
                    request.preparation.max_source_bytes = source_bytes.len();
                    request.preparation.max_decoded_samples = 8 * usize::from(channels);
                    request.preparation.max_prepared_samples = 16 * usize::from(output_channels);
                    let report = render_soundtrack_with_fs(
                        &mut TwoCues(source.clone()),
                        request,
                        fs.clone(),
                    )
                    .map_err(|error| format!("surround render {path}: {error}"))?;
                    let wav = fs
                        .read(Path::new(&path))
                        .map_err(|error| error.to_string())?;
                    if wav != expected
                        || report.artifact.digest != sha256(&expected)
                        || report.artifact.sample_frames != 8
                        || report.artifact.cues_mixed != 2
                        || report.artifact.clipped_samples != 0
                        || report.scene.time.frames() != 2
                        || report.inputs.len() != 2
                    {
                        return Err(format!("surround matrix or render receipt changed: {path}"));
                    }
                    let first = &report.inputs[0];
                    if !Arc::ptr_eq(&first.bytes, &report.inputs[1].bytes)
                        || !Arc::ptr_eq(&first.report, &report.inputs[1].report)
                        || report.inputs.iter().any(|input| {
                            input.source != source
                                || input.bytes.as_ref() != source_bytes.as_slice()
                                || input.report.source_digest != sha256(&source_bytes)
                                || input.report.invocation.is_some()
                        })
                    {
                        return Err(format!("surround cached input identity changed: {path}"));
                    }
                    if let SoundSource::Memory(data) = &source
                        && !Arc::ptr_eq(data.bytes(), &first.bytes)
                    {
                        return Err("surround memory source was copied during preparation".into());
                    }
                    if *pcm_digest.get_or_insert(first.report.pcm_digest) != first.report.pcm_digest
                    {
                        return Err(
                            "file and memory surround sources have different PCM identities".into(),
                        );
                    }
                    renders += 1;
                    artifact = Some(wav);
                }
            }
        }
    }

    let mut refusals = 0;
    for (name, mask) in [
        ("unknown", Some(0x18f)),
        ("absent", None),
        ("count", Some(0x7)),
    ] {
        let path = format!("/refused-{name}.wav");
        let bytes = fixture(6, mask);
        if SoundData::from_wav_bytes(&bytes, SoundDataLimits::default()).is_ok() {
            return Err(format!("memory source accepted {name} surround layout"));
        }
        let source_path = format!("/source-{name}.wav");
        fs.insert(&source_path, bytes.clone());
        let source = SoundSource::File(source_path.clone().into());
        let error = render_soundtrack_with_fs(
            &mut TwoCues(source.clone()),
            options(&path, 2, 4)?,
            fs.clone(),
        )
        .expect_err("unsupported surround layout must be refused");
        if !matches!(
            error,
            RenderError::SoundDecode { source: Some(identity), .. } if *identity == source.key()
        ) || fs.exists(Path::new(&path))
            || fs
                .read(Path::new(&source_path))
                .map_err(|error| error.to_string())?
                != bytes
        {
            return Err(format!(
                "{name} layout refusal lost source identity or published output"
            ));
        }
        refusals += 1;
    }
    Ok(WavVerification {
        renders,
        refusals,
        wav: artifact.ok_or("no surround WAV was rendered")?,
    })
}

/// Portable recordings must carry the resulting stereo PCM, so they remain
/// audible without reopening the original multichannel source.
pub fn verify_bundle() -> Result<u64, String> {
    let fs = VirtualFs::new();
    let mut checked = 0;
    for (channels, mask) in LAYOUTS {
        let mut reference = None;
        for in_memory in [false, true] {
            let (source, source_bytes) = source(&fs, channels, mask, in_memory)?;
            let mut options = BundleExportOptions::new().map_err(|error| error.to_string())?;
            options.config.camera.fps = 8;
            options.config.camera.resolution = (16, 9);
            let audio = BundleSoundtrackOptions {
                mixer: MixerConfig {
                    sample_rate: 32,
                    channels: 2,
                    max_output_frames: 8,
                },
                workers: 4,
                ..BundleSoundtrackOptions::default()
            };
            let exported =
                export_bundle_with_soundtrack_bytes(&mut TwoCues(source), options, &audio, &fs)
                    .map_err(|error| format!("surround bundle export: {error}"))?;
            let mut decoded = TimelineBundle::from_bytes(&exported.bundle.bytes)
                .map_err(|error| format!("surround bundle decode: {error}"))?;
            let track = decoded
                .take_soundtrack()
                .ok_or("surround bundle lost its soundtrack")?;
            let receipt = exported
                .soundtrack
                .ok_or("surround bundle lost its receipt")?;
            if (track.sample_rate(), track.channels(), track.sample_frames()) != (32, 2, 8)
                || track.samples() != oracle(channels, 2)
                || receipt.inputs.len() != 2
                || receipt.inputs[0].report.source_digest != sha256(&source_bytes)
                || !Arc::ptr_eq(&receipt.inputs[0].bytes, &receipt.inputs[1].bytes)
                || !receipt.audio_decodes.is_empty()
            {
                return Err("surround bundle PCM or source receipt differs from the oracle".into());
            }
            if let Some(reference) = &reference {
                if &exported.bundle.bytes != reference {
                    return Err(
                        "file and memory sources produced different portable bundles".into(),
                    );
                }
            } else {
                reference = Some(exported.bundle.bytes);
            }
            checked += 1;
        }
    }
    Ok(checked)
}
