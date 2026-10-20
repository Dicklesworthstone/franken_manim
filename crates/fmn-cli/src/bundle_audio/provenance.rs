//! FMNP closure and create-only sidecars for self-contained narrated replay.
//!
//! The input is the already-hashed recording, not the scene or media that made
//! it. Selection and padding are inputs; output hashes and scheduler counters
//! are not. Both representations publish as one immutable directory generation.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fmn::rendering::RenderReport;
use fmn_config::config::DeterminismMode;
use fmn_hash::Digest;
use fmn_output::{
    ClosureItem, ManifestError, ManifestIdentity, ManifestMode, ManifestOutput,
    ProvenanceManifest, StructuralField, frames_to_samples,
};
use fmn_platform::fs::FileSystem;
use fmn_render::engine::{EngineIdentity, Tier, journal};
use fmn_render::{FrameConfig, ScreenMap, Tiling, Viewport};
use fmn_scene::TimelineBundle;
use fmn_scene::timeline_bundle::BundleSoundtrack;

use super::{Failure, Target, preflight_destination, quote};

const SUITE: &[u8] = include_bytes!("../../../../SUITE.lock");
const SUITE_TEXT: &str = include_str!("../../../../SUITE.lock");
const BUILD_ID: &str = env!("FMN_BUILD_ID");
const TARGET: &str = env!("FMN_TARGET_TRIPLE");
const PROFILE: &str = env!("FMN_CARGO_PROFILE");
const WAV_ENGINE: &str = "compiled-pcm:scalar:s16-no-dither/v1";

impl From<ManifestError> for Failure {
    fn from(error: ManifestError) -> Self {
        Self::new("render", format!("narrated replay provenance: {error}"))
    }
}

pub(super) fn destination(artifact: &Path) -> Result<PathBuf, Failure> {
    let mut leaf = artifact
        .file_name()
        .ok_or_else(|| Failure::new("usage", "artifact has no provenance leaf name"))?
        .to_os_string();
    leaf.push(".manifest");
    Ok(artifact.with_file_name(leaf))
}

pub(super) fn preflight(fs: &dyn FileSystem, artifact: &Path) -> Result<(), Failure> {
    preflight_destination(fs, &destination(artifact)?)
}

/// Immutable input facts captured before pictures or PCM change ownership.
pub(super) struct Session {
    items: Vec<ClosureItem>,
    config: fmn_config::Config,
    mode: ManifestMode,
    filesystem: &'static str,
    toolchain: &'static str,
    fps: u32,
    frames: u32,
    segments: usize,
    sample_rate: u32,
    channels: u16,
    sample_frames: u64,
}

pub(super) struct Artifact<'a> {
    pub destination: &'a Path,
    pub target: Target,
    pub digest: Digest,
    pub frames: Range<u32>,
    pub segments: Range<usize>,
    pub keep_tail: bool,
    pub sample_rate: u32,
    pub channels: u16,
    pub sample_frames: u64,
    pub video: Option<&'a RenderReport>,
}

pub(super) struct Publication {
    pub path: PathBuf,
    pub certified: bool,
    closure: Digest,
    semantic: Digest,
}

impl Publication {
    pub fn json(&self) -> String {
        format!(
            "{{\"path\":{},\"closure_digest\":{},\"semantic_digest\":{}}}",
            quote(&self.path.to_string_lossy()),
            quote(&self.closure.to_hex()),
            quote(&self.semantic.to_hex()),
        )
    }
}

impl Session {
    pub fn new(
        config: &fmn_config::Config,
        source_digest: Digest,
        bundle: &TimelineBundle,
        pcm: &BundleSoundtrack,
        fs: &dyn FileSystem,
    ) -> Result<Self, Failure> {
        let filesystem = fs.identity();
        if filesystem.is_empty() || filesystem == "opaque.file_system/v1" {
            return Err(Failure::new(
                "capability",
                "narrated replay manifests require a stable versioned FileSystem identity",
            ));
        }
        let mode = match config.determinism.mode {
            DeterminismMode::Standard => ManifestMode::Standard,
            DeterminismMode::Certified => ManifestMode::Certified,
        };
        if mode == ManifestMode::Certified {
            admit_certified_build(BUILD_ID, TARGET)?;
        }
        let toolchain = pinned_toolchain()?;
        let config_bytes = config
            .canonical_bytes()
            .map_err(|error| Failure::new("config", error.to_string()))?;
        let mut items = vec![
            ClosureItem::digest_input(
                1,
                "compiled/recording.fmtl",
                source_digest,
                "complete validated recording, including embedded PCM and recorded cameras; original scene code is not executed",
            )?,
            ClosureItem::byte_input(
                2,
                "franken_manim.build",
                BUILD_ID.as_bytes(),
                "exact compiled source identity",
            )?,
            ClosureItem::byte_input(2, "SUITE.lock", SUITE, "complete governed dependency lock")?,
            ClosureItem::structural(
                3,
                "native toolchain; no Python runtime on compiled replay",
                &[
                    StructuralField::Text(toolchain),
                    StructuralField::Text(PROFILE),
                    StructuralField::Absent("CPython portal and NumPy runtime"),
                ],
            )?,
            ClosureItem::structural_at(
                3,
                "platform/target",
                "compiled target triple and target-feature tier",
                &[
                    StructuralField::Text(TARGET),
                    StructuralField::Text(target_features()),
                    StructuralField::Text(Tier::COMPILED.name()),
                ],
            )?,
            ClosureItem::byte_input(
                4,
                "resolved-config.fmnf",
                &config_bytes,
                "resolved configuration using the shared inert-field canonicalization",
            )?,
            ClosureItem::structural(
                5,
                "configured seed and RNG layout; replay creates no fresh scene RNG",
                &[
                    StructuralField::U64(config.determinism.seed),
                    StructuralField::U64(u64::from(fmn_core::rng::RNG_LAYOUT_VERSION)),
                    StructuralField::Absent("scene RNG execution during replay"),
                ],
            )?,
            ClosureItem::structural(
                6,
                "PCM and picture assets are inside C1; compiled font inventory follows",
                &[StructuralField::Absent("external asset or original audio reads during replay")],
            )?,
            ClosureItem::structural_at(
                7,
                "compiled/input-engine",
                "engine and reconstruction-law identity checked by the production FMTL reader",
                &[StructuralField::Text(bundle.engine_version())],
            )?,
            ClosureItem::structural(
                8,
                "owned parsers and rational clocks fix engine locale/timezone",
                &[StructuralField::Text("C"), StructuralField::Text("UTC")],
            )?,
        ];
        for (name, bytes) in fmn::text::bundled_faces() {
            items.push(ClosureItem::byte_input(
                6,
                format!("bundled-font/{name}"),
                bytes.as_ref(),
                "compiled font face; conservative native font inventory",
            )?);
        }
        Ok(Self {
            items,
            config: config.clone(),
            mode,
            filesystem,
            toolchain,
            fps: bundle.fps(),
            frames: bundle.frame_count(),
            segments: bundle.segment_count(),
            sample_rate: pcm.sample_rate(),
            channels: pcm.channels(),
            sample_frames: pcm.sample_frames(),
        })
    }

    fn expected_audio_frames(&self, output: &Artifact<'_>) -> Result<u64, Failure> {
        if output.frames.start >= output.frames.end
            || output.frames.end > self.frames
            || output.segments.start >= output.segments.end
            || output.segments.end > self.segments
            || (output.keep_tail && output.frames != (0..self.frames))
        {
            return Err(Failure::new("internal", "invalid provenance selection"));
        }
        let boundary = |frame| {
            frames_to_samples(i64::from(frame), self.fps, self.sample_rate)
                .map_err(|error| Failure::new("render", error.to_string()))
                .and_then(|samples| {
                    u64::try_from(samples)
                        .map_err(|_| Failure::new("internal", "negative provenance sample boundary"))
                })
        };
        let end = boundary(output.frames.end)?;
        if output.keep_tail {
            Ok(self.sample_frames.max(end))
        } else {
            end.checked_sub(boundary(output.frames.start)?)
                .ok_or_else(|| Failure::new("internal", "reversed provenance sample interval"))
        }
    }

    fn seal(&self, output: &Artifact<'_>) -> Result<ProvenanceManifest, Failure> {
        if output.sample_rate != self.sample_rate
            || output.channels != self.channels
            || output.sample_frames != self.expected_audio_frames(output)?
        {
            return Err(Failure::new(
                "internal",
                "published soundtrack receipt disagrees with the admitted recording and selection",
            ));
        }
        if self.mode == ManifestMode::Certified && !matches!(output.target, Target::Wav) {
            return Err(Failure::new("capability", "ffmpeg products are never certified"));
        }
        let mut items = self.items.clone();
        let engine = match (output.target, output.video) {
            (Target::Wav, None) => {
                items.push(ClosureItem::structural(
                    7,
                    "native PCM extraction and canonical S16 publication, without rasterization",
                    &[
                        StructuralField::Text(WAV_ENGINE),
                        StructuralField::Absent("rasterization, resampling, gain, mixing and dithering"),
                    ],
                )?);
                items.push(ClosureItem::structural(
                    9,
                    "filesystem-only native WAV replay; no process capability acquired",
                    &[
                        StructuralField::Text(self.filesystem),
                        StructuralField::Absent("ProcessRunner and ffmpeg invocation"),
                        StructuralField::Absent("host clock, AssetFetcher and CPython portal"),
                    ],
                )?);
                WAV_ENGINE.to_owned()
            }
            (Target::Video(format), Some(report)) if format.is_video() => {
                if report.artifact.format != format
                    || report.artifact.path.as_path() != output.destination
                    || report.artifact.digest != output.digest
                    || report.artifact.frame_count != u64::from(output.frames.end - output.frames.start)
                    || !report.artifact.audio_decodes.is_empty()
                    || report.artifact.ffmpeg.is_empty()
                {
                    return Err(Failure::new("internal", "compiled video receipt disagrees with its provenance"));
                }
                let sound = report.artifact.soundtrack.as_ref().ok_or_else(|| {
                    Failure::new("internal", "compiled video omitted its soundtrack receipt")
                })?;
                if sound.sample_rate != self.sample_rate
                    || sound.channels != self.channels
                    || sound.sample_frames != output.sample_frames
                {
                    return Err(Failure::new("internal", "compiled video soundtrack metadata changed"));
                }
                let identity = match report.execution_plan.engine {
                    fmn_runtime::ExecutionEngine::CertifiedCpu => EngineIdentity::certified(),
                    fmn_runtime::ExecutionEngine::FastCpu => EngineIdentity::fast(),
                    _ => return Err(Failure::new("capability", "compiled replay has no accelerator provenance adapter")),
                };
                let (width, height) = self.config.camera.resolution;
                let background = fmn_core::color::Srgb::from_hex(&self.config.camera.background_color)
                    .map_err(|error| Failure::new("config", error.to_string()))?
                    .to_linear(self.config.camera.background_opacity);
                let frame = FrameConfig::new(
                    Viewport { width, height },
                    ScreenMap {
                        scale: f64::from(height) / self.config.sizes.frame_height,
                        origin: [f64::from(width) / 2.0, f64::from(height) / 2.0],
                        y_up: true,
                    },
                    background,
                ).with_aa_policy(self.config.render.aa);
                let backend = journal(identity, &frame, Tiling {
                    macro_tile: report.execution_plan.macro_tile,
                    fine_tile: report.execution_plan.fine_tile,
                });
                let engine = identity.closure_string();
                items.push(ClosureItem::structural(
                    7,
                    "actual native execution engine and frame policy; recorded cameras are in C1",
                    &[StructuralField::Text(&engine), StructuralField::Bytes(&backend)],
                )?);
                append_video_capabilities(&mut items, self.filesystem, report)?;
                engine
            }
            _ => return Err(Failure::new("internal", "artifact kind and provenance backend disagree")),
        };
        let declared = ClosureItem::structural(
            10,
            "narrated replay selection and audio policy; no output hashes or scheduling observations",
            &[
                StructuralField::Text("fmn-narrated-replay/v1"),
                StructuralField::Text(self.mode.name()),
                StructuralField::Text(output.target.extension()),
                StructuralField::U64(u64::from(self.fps)),
                StructuralField::U64(u64::from(output.frames.start)),
                StructuralField::U64(u64::from(output.frames.end)),
                StructuralField::U64(u64::try_from(output.segments.start).map_err(|_| Failure::new("budget", "segment index exceeds u64"))?),
                StructuralField::U64(u64::try_from(output.segments.end).map_err(|_| Failure::new("budget", "segment index exceeds u64"))?),
                StructuralField::Bool(output.keep_tail),
                StructuralField::Text("absolute rational frame-to-sample boundaries; zero padding; no remix"),
                StructuralField::Text("native S16 WAV without dither; video uses governed AAC mux"),
            ],
        )?;
        let declared_config_digest = declared.digest;
        items.push(declared);
        let leaf = output.destination.file_name().and_then(|leaf| leaf.to_str())
            .filter(|leaf| !leaf.is_empty())
            .ok_or_else(|| Failure::new("usage", "artifact leaf must be nonempty UTF-8"))?;
        Ok(ProvenanceManifest::new(
            self.mode,
            items,
            ManifestIdentity {
                build_id: BUILD_ID.to_owned(),
                suite_lock_digest: fmn_hash::sha256(SUITE),
                toolchain: self.toolchain.to_owned(),
                target_triple: TARGET.to_owned(),
                target_features: target_features().to_owned(),
                engine,
                simd_tier: Tier::COMPILED.name().to_owned(),
                declared_config_digest,
            },
            vec![ManifestOutput {
                virtual_path: leaf.to_owned(),
                kind: if matches!(output.target, Target::Wav) { "wav" } else { "encoded_video" }.to_owned(),
                digest: output.digest,
                certified: self.mode == ManifestMode::Certified,
            }],
            None,
        )?)
    }

    pub fn publish(
        &self,
        fs: Arc<dyn FileSystem>,
        output: Artifact<'_>,
    ) -> Result<Publication, Failure> {
        if fs.identity() != self.filesystem {
            return Err(Failure::new("capability", "provenance filesystem identity changed during replay"));
        }
        let manifest = self.seal(&output)?;
        let binary = manifest.to_bytes()?;
        let text = manifest.to_text();
        let semantic = manifest.semantic_digest()?;
        let sidecar = destination(output.destination)?;
        let publication_error = |error| Failure::new(
            "render",
            format!("artifact {} was published, but provenance {} was not completed: {error}",
                output.destination.display(), sidecar.display()),
        );
        let mut writer = fs.begin_atomic_directory(&sidecar).map_err(publication_error)?;
        writer.write_file(Path::new("manifest.fmnp"), &binary).map_err(publication_error)?;
        writer.write_file(Path::new("manifest.txt"), text.as_bytes()).map_err(publication_error)?;
        writer.prepare().map_err(publication_error)?.commit().map_err(publication_error)?;
        Ok(Publication {
            path: sidecar.join("manifest.fmnp"),
            certified: manifest.mode == ManifestMode::Certified,
            closure: manifest.closure_digest,
            semantic,
        })
    }
}

fn append_video_capabilities(
    items: &mut Vec<ClosureItem>,
    filesystem: &str,
    report: &RenderReport,
) -> Result<(), Failure> {
    let limits = report.artifact.ffmpeg_limits.as_ref().ok_or_else(|| {
        Failure::new("internal", "video receipt omitted its effective ffmpeg limits")
    })?.to_json();
    items.push(ClosureItem::structural(
        9,
        "filesystem and governed encode/mux; decoded source media and scene callbacks are absent",
        &[
            StructuralField::Text(filesystem),
            StructuralField::Text(&limits),
            StructuralField::Absent("host clock, AssetFetcher and CPython portal"),
            StructuralField::Absent("source-media decoder and scene callbacks"),
        ],
    )?);
    for (index, invocation) in report.artifact.ffmpeg.iter().enumerate() {
        let p = &invocation.provenance;
        let image_format = match p.native_image.format {
            fmn_platform::process::NativeExecutableFormat::Elf64 => "elf64",
            fmn_platform::process::NativeExecutableFormat::MachO64 => "mach-o64",
            fmn_platform::process::NativeExecutableFormat::MachOUniversal => "mach-o-universal",
            fmn_platform::process::NativeExecutableFormat::Pe32Plus => "pe32+",
        };
        let architecture = match p.native_image.architecture {
            fmn_platform::process::NativeExecutableArchitecture::X86_64 => "x86_64",
            fmn_platform::process::NativeExecutableArchitecture::Aarch64 => "aarch64",
        };
        let mut fields = vec![
            StructuralField::Bytes(p.tool_path.as_os_str().as_encoded_bytes()),
            StructuralField::Bytes(p.bound_tool_path.as_os_str().as_encoded_bytes()),
            StructuralField::Text(&p.tool_sha256_hex),
            StructuralField::Text(&p.tool_version),
            StructuralField::Text(image_format),
            StructuralField::Text(architecture),
            StructuralField::U64(p.native_image.file_bytes),
            StructuralField::U64(u64::from(p.native_image.policy_version)),
            StructuralField::Text(&p.process_mechanism),
            StructuralField::U64(u64::from(p.process_policy_version)),
            p.encoder.as_deref().map_or(StructuralField::Absent("encoder"), StructuralField::Text),
            StructuralField::U64(u64::try_from(p.argv.len()).map_err(|_| Failure::new("budget", "ffmpeg argv exceeds u64"))?),
        ];
        fields.extend(p.argv.iter().map(|arg| StructuralField::Text(arg)));
        items.push(ClosureItem::structural_at(
            9,
            format!("ffmpeg/{index:05}"),
            "actual invocation: exact native image, process policy and ordered argv",
            &fields,
        )?);
    }
    Ok(())
}

fn pinned_toolchain() -> Result<&'static str, Failure> {
    let mut active = false;
    for line in SUITE_TEXT.lines() {
        if line.starts_with('[') {
            active = line == "[toolchain]";
        } else if active {
            let mut fields = line.split('\t');
            if fields.next() == Some("rustc")
                && let Some(value) = fields.next()
                && !value.is_empty()
            {
                return Ok(value);
            }
        }
    }
    Err(Failure::new("internal", "SUITE.lock omitted its pinned rustc identity"))
}

fn target_features() -> &'static str {
    match Tier::COMPILED.name() {
        "x86-64-v3" => "+avx2,+bmi2,+fma",
        "x86-64-v4" => "+avx512f,+avx512bw,+avx512dq,+avx512vl",
        "aarch64-neon" => "+neon",
        _ => "baseline",
    }
}

fn admit_certified_build(build_id: &str, target: &str) -> Result<(), Failure> {
    if let Some(reason) = fmn_output::certified_build_refusal(build_id) {
        return Err(Failure::new("capability", reason));
    }
    if !matches!(target,
        "x86_64-unknown-linux-gnu" | "x86_64-unknown-linux-musl"
        | "aarch64-unknown-linux-gnu" | "aarch64-unknown-linux-musl"
        | "aarch64-apple-darwin")
    {
        return Err(Failure::new("capability", "certified narrated WAV requires a target in the existing certified matrix"));
    }
    Ok(())
}
