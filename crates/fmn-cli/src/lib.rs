//! The `fmn` binary: manim's flag surface, progress reporting, doctor, and
//! batch renders (§13.6).
//!
//! The parser is data-driven by [`FLAG_SPECS`] and [`INTERACTION_SPECS`],
//! generated from the one API schema. Runtime code never reads repository TSV
//! files and does not carry a second flag inventory.
#![forbid(unsafe_code)]

mod generated;

pub use generated::{
    CommandScope, EXIT_CODE_SPECS, ExitCodeSpec, FLAG_SPECS, FlagAction, FlagArity, FlagSource,
    FlagSpec, FlagStatus, INTERACTION_SPECS, InteractionKind, InteractionSpec, SUBCOMMAND_SPECS,
    SubcommandSpec,
};

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fmt::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
#[cfg(feature = "batch")]
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;
#[cfg(feature = "batch")]
use std::time::Instant;

use fmn_core::color::Srgb;
use fmn_core::rng::{RNG_LAYOUT_VERSION, RngRoot};
use fmn_frame::convert::{rgba_to_nv12, rgba_to_p010, rgba16f_to_rgba8, swap_rb8};
use fmn_frame::{ChromaSiting, ColorRange, FrameBuffer, FrameLayout, PixelFormat};
use fmn_mobject::SceneState;
use fmn_output::{
    ArtifactDigest, ClosureItem, ColorDescription, Container, EmitterConfig, EmitterHandle,
    EncoderCapabilities, EncoderChoice, FfmpegArtifactReport, FfmpegSink, FfmpegSinkConfig,
    FfmpegTool, GifSink, GifSinkConfig, JobLimits, ManifestIdentity, ManifestMode, ManifestOutput,
    NativeArtifactReport, OrderedEmitter, PngSink, PngSinkConfig, PngTarget, ProvenanceManifest,
    SinkLimits, SinkReceipt, StructuralField, VideoJob, WireFormat, Y4mSink, Y4mSinkConfig,
};
use fmn_platform::fs::{FileSystem, FsError, FsNodeKind};
use fmn_render::bin::{Binning, ScreenMap, Tiling, Viewport};
use fmn_render::engine::{EngineIdentity, FrameConfig, FrameJob};
use fmn_render::fill::MonoTable;
use fmn_render::plan::RenderPlan;
use fmn_scene::{
    AssetRead, BundleReadError, CaptureReason, CommandRecord, DEFAULT_MAX_BUNDLE_BYTES,
    EffectClass, Entry, IntegrationError, Journal, NullSceneSink, OutputNaming, SceneSink,
    TimelineBundle,
};

/// Version of every `fmn` robot-mode record emitted by this crate.
pub const ROBOT_SCHEMA_VERSION: u32 = 1;

/// Stable refusal for Python source files presented to the standalone binary.
pub const PYTHON_SOURCE_PORTAL_MESSAGE: &str = "Python scene sources require the CPython-dependent fmn-python entry point; the standalone fmn binary never embeds, locates, or spawns CPython";

/// Reserved source identifier for the native primitive registrations compiled
/// into the standalone binary.
pub const BUILTIN_SCENE_SOURCE: &str = "@builtin";

/// Exact private argv sentinel used only between the Studio supervisor and
/// the disposable worker instance of the same `fmn` executable.
pub const INTERNAL_STUDIO_WORKER_ARG: &str = "--fmn-internal-studio-worker-v1";

const SUITE_LOCK_BYTES: &[u8] = include_bytes!("../../../SUITE.lock");
const SUITE_LOCK_TEXT: &str = include_str!("../../../SUITE.lock");
const BUILD_ID: &str = env!("FMN_BUILD_ID");
const TARGET_TRIPLE: &str = env!("FMN_TARGET_TRIPLE");
const CARGO_PROFILE: &str = env!("FMN_CARGO_PROFILE");

/// A stable CLI failure carrying its schema-owned process status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliError {
    exit_name: &'static str,
    message: String,
    rule: Option<&'static str>,
}

impl CliError {
    fn new(exit_name: &'static str, message: impl Into<String>) -> Self {
        Self {
            exit_name,
            message: message.into(),
            rule: None,
        }
    }

    fn interaction(rule: &'static str, exit_name: &'static str, message: &'static str) -> Self {
        Self {
            exit_name,
            message: message.to_owned(),
            rule: Some(rule),
        }
    }

    /// Stable exit-code identity from the generated schema.
    #[must_use]
    pub const fn exit_name(&self) -> &'static str {
        self.exit_name
    }

    /// Numeric process status.
    #[must_use]
    pub fn code(&self) -> u8 {
        exit_code(self.exit_name)
    }

    /// Human-readable detail without terminal decoration.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Interaction rule identity, when validation failed on a generated rule.
    #[must_use]
    pub const fn rule(&self) -> Option<&'static str> {
        self.rule
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(rule) = self.rule {
            write!(f, "{rule}: {}", self.message)
        } else {
            f.write_str(&self.message)
        }
    }
}

impl std::error::Error for CliError {}

/// A validated `-n START` or `-n START,END` play range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnimationRange {
    /// First play index to render.
    pub start: u64,
    /// Exclusive final play index, when bounded.
    pub end: Option<u64>,
}

/// Quality preset or exact output dimensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolutionChoice {
    /// Configured `-l` preset.
    Low,
    /// Configured `-m` preset.
    Medium,
    /// Configured `--hd` preset.
    High,
    /// Configured `--uhd` preset.
    Uhd,
    /// Exact `-r WIDTHxHEIGHT` override.
    Exact(u32, u32),
}

/// Output format selected by `--format` and the kept GIF flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    /// Derive the sink from the other output flags.
    Auto,
    /// One canonical PNG.
    Png,
    /// A canonical PNG sequence.
    PngSequence,
    /// Native GIF.
    Gif,
    /// Native y4m.
    Y4m,
    /// Native WAV.
    Wav,
    /// Video through the optional ffmpeg boundary.
    Video,
}

impl OutputFormat {
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "auto" => Self::Auto,
            "png" => Self::Png,
            "png_sequence" => Self::PngSequence,
            "gif" => Self::Gif,
            "y4m" => Self::Y4m,
            "wav" => Self::Wav,
            "video" => Self::Video,
            _ => return None,
        })
    }
}

/// Options shared by every command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommonOptions {
    /// Versioned NDJSON mode.
    pub robot: bool,
    /// Suppress human-only progress and decoration.
    pub quiet: bool,
    /// Select certified rendering.
    pub reproducible: bool,
    /// Optional user config file.
    pub config_file: Option<PathBuf>,
    /// Explicit cache root.
    pub cache_dir: Option<PathBuf>,
    /// Explicit absolute ffmpeg path.
    pub ffmpeg: Option<PathBuf>,
    /// Optional render-team thread cap.
    pub threads: Option<usize>,
    /// Optional Reference log-level spelling.
    pub log_level: Option<String>,
}

/// Semantic render request after parsing and interaction application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SceneSourceKind {
    /// A source or artifact owned by the native Rust front door.
    Native,
    /// A Python program, owned by the separately installed Python portal.
    Python,
}

/// Semantic render request after parsing and interaction application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderCommand {
    /// Shared command options.
    pub common: CommonOptions,
    /// Optional scene source path.
    pub file: Option<PathBuf>,
    /// Explicit scene names, in command-line order.
    pub scene_names: Vec<String>,
    /// Select every discovered scene.
    pub write_all: bool,
    /// Durable output requested or implied.
    pub write_file: bool,
    /// Capture final state rather than animation frames.
    pub skip_animations: bool,
    /// Quality preset or explicit resolution.
    pub resolution: Option<ResolutionChoice>,
    /// Explicit FPS.
    pub fps: Option<u32>,
    /// Play range.
    pub animation_range: Option<AnimationRange>,
    /// Presenter-controlled waits.
    pub presenter_mode: bool,
    /// Full-screen interactive presentation.
    pub full_screen: bool,
    /// Transparent output negotiation.
    pub transparent: bool,
    /// Output subdivision.
    pub subdivide: bool,
    /// Count-only pass before the real run.
    pub prerun: bool,
    /// Worker reload mode.
    pub autoreload: bool,
    /// Python/Studio breakpoint line.
    pub embed_line: Option<u64>,
    /// Background color expression.
    pub background: Option<String>,
    /// Output stem.
    pub file_name: Option<String>,
    /// Output directory.
    pub video_dir: Option<PathBuf>,
    /// ffmpeg encoder.
    pub vcodec: Option<String>,
    /// ffmpeg pixel format.
    pub pix_fmt: Option<String>,
    /// Output format.
    pub format: OutputFormat,
    /// Explicit fmd-math preamble pack; absent preserves user configuration.
    pub math_pack: Option<String>,
    /// Open the completed artifact through a host capability.
    pub open: bool,
    /// Reveal the completed artifact through a host capability.
    pub finder: bool,
    /// Per-animation progress.
    pub show_animation_progress: bool,
    /// Leave completed progress bars.
    pub leave_progress_bars: bool,
}

impl RenderCommand {
    /// Classify the selected front-door source without reading it.
    #[must_use]
    pub fn scene_source_kind(&self) -> Option<SceneSourceKind> {
        let extension = self.file.as_deref()?.extension()?.to_str()?;
        Some(
            if extension.eq_ignore_ascii_case("py") || extension.eq_ignore_ascii_case("pyw") {
                SceneSourceKind::Python
            } else {
                SceneSourceKind::Native
            },
        )
    }

    /// Translate front-door flags into Proscenium's semantic runtime config.
    #[must_use]
    pub fn runtime_config(&self, config: &fmn_config::Config) -> fmn_scene::RuntimeConfig {
        let mut runtime = fmn_scene::RuntimeConfig::from_config(config);
        runtime.windowed =
            !self.write_file || self.presenter_mode || self.full_screen || self.autoreload;
        runtime.skip_animations = self.skip_animations;
        runtime.start_at_play = self.animation_range.map(|range| range.start);
        runtime.end_at_play = self.animation_range.and_then(|range| range.end);
        runtime.presenter_mode = self.presenter_mode;
        runtime
    }
}

/// `fmn doctor`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorCommand {
    /// Shared command options.
    pub common: CommonOptions,
    /// Make missing ffmpeg an exit-4 requirement rather than a reported
    /// optional absence.
    pub require_ffmpeg: bool,
}

/// `fmn batch`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchCommand {
    /// Render options applied to each selected scene.
    pub render: RenderCommand,
    /// Wall-clock budget.
    pub budget_ms: Option<u64>,
    /// Bound simultaneously active scene jobs.
    pub max_scenes: Option<usize>,
    /// Cancel remaining jobs after the first failure.
    pub fail_fast: bool,
    /// Per-scene manifest directory.
    pub manifest_dir: Option<PathBuf>,
}

/// Studio preview transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewCodec {
    /// Permanent zero-ffmpeg multipart PNG floor.
    Png,
    /// Optional MJPEG route through Reel's ffmpeg boundary.
    Mjpeg,
}

/// `fmn studio`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StudioCommand {
    /// Scene/preview options.
    pub render: RenderCommand,
    /// Loopback bind address.
    pub bind: std::net::IpAddr,
    /// TCP port; zero requests an ephemeral port.
    pub port: u16,
    /// Suppress browser launch.
    pub no_browser: bool,
    /// Attach a terminal client.
    pub tui: bool,
    /// Maximum frame distance between replay checkpoints.
    pub checkpoint_frames: u64,
    /// Preview transport.
    pub preview_codec: PreviewCodec,
}

/// Provenance of the topology used for `fmn doctor`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TopologySource {
    /// Linux sysfs/procfs introspection succeeded.
    LinuxSysfs,
    /// A flat topology was derived from `available_parallelism`.
    Fallback {
        /// Why full introspection was unavailable.
        reason: String,
    },
}

/// Optional ffmpeg capability state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FfmpegReport {
    /// The configured path was resolved and probed.
    Available {
        /// Canonical caller-supplied absolute path.
        path: PathBuf,
        /// Executable SHA-256.
        sha256: String,
        /// First `ffmpeg -version` line.
        version: String,
        /// Recognized hardware encoders, in stable order.
        hardware_encoders: Vec<String>,
        /// Encoder-inventory probe failure, without discarding the valid
        /// executable identity and version probe.
        hardware_encoder_probe_error: Option<String>,
    },
    /// Optional capability unavailable or deliberately not resolved.
    Unavailable {
        /// Configured path or name.
        attempted: PathBuf,
        /// Stable, actionable explanation.
        reason: String,
        /// Native no-ffmpeg floor.
        alternative: String,
    },
}

impl FfmpegReport {
    /// Whether a probed ffmpeg is available.
    #[must_use]
    pub const fn is_available(&self) -> bool {
        matches!(self, Self::Available { .. })
    }
}

/// Read-only cache observation. Doctor never opens or stamps a store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheReport {
    /// A configured root was inspected without mutation.
    Configured {
        /// Configured path.
        root: PathBuf,
        /// Whether it currently exists.
        exists: bool,
        /// Number of direct entries, when readable.
        direct_entries: Option<usize>,
        /// Read-only inspection failure, when any.
        warning: Option<String>,
    },
    /// Root resolution failed before inspection.
    Unresolved {
        /// Why no path was available.
        reason: String,
    },
}

/// Font inventory state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontReport {
    /// Configured default family.
    pub selected: String,
    /// Bundled family names verified by the host integration.
    pub bundled: Vec<String>,
    /// User family names verified by the host integration.
    pub user: Vec<String>,
    /// Whether the inventory is complete.
    pub complete: bool,
    /// Truthful reason for a partial inventory.
    pub detail: Option<String>,
}

/// Certified-platform contract reported by doctor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertificationReport {
    /// Stable `os-arch` identity.
    pub platform: String,
    /// Whether Revision 4 certifies this target.
    pub supported: bool,
    /// Contract note, including pending targets.
    pub detail: String,
}

/// Stable subset of the derived [`fmn_runtime::ExecutionPlan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionPlanReport {
    /// `standard` or `certified`.
    pub determinism: &'static str,
    /// Selected execution engine.
    pub engine: &'static str,
    /// Global frame-slot bound.
    pub frames_in_flight: usize,
    /// Scene/update threads.
    pub scene_threads: usize,
    /// Render-team count.
    pub render_teams: usize,
    /// Threads across all render teams.
    pub render_threads: usize,
    /// Output threads.
    pub output_threads: usize,
    /// Fine tile edge.
    pub fine_tile: u32,
    /// Macrotile edge.
    pub macro_tile: u32,
    /// Estimated in-flight bytes.
    pub estimated_in_flight_bytes: usize,
    /// Negotiated planning pixel format.
    pub output_format: &'static str,
    /// Tuning source.
    pub tuning_source: &'static str,
}

/// Complete, versioned `fmn doctor` snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorSnapshot {
    /// Topology provenance.
    pub topology_source: TopologySource,
    /// Logical CPU count.
    pub logical_cores: u32,
    /// Physical CPU count.
    pub physical_cores: u32,
    /// Hardware-supported SIMD tier.
    pub hardware_supported_tier: String,
    /// Tier compiled into this binary.
    pub active_compiled_tier: &'static str,
    /// Derived execution plan.
    pub plan: ExecutionPlanReport,
    /// Optional ffmpeg report.
    pub ffmpeg: FfmpegReport,
    /// Read-only cache state.
    pub cache: CacheReport,
    /// Fonts.
    pub fonts: FontReport,
    /// Available fmd-math packs.
    pub math_packs: Vec<String>,
    /// Certified-platform support.
    pub certification: CertificationReport,
}

/// Fully parsed command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    /// Render or preview.
    Render(RenderCommand),
    /// Capability report.
    Doctor(DoctorCommand),
    /// Multi-scene farm.
    Batch(BatchCommand),
    /// Live Studio.
    Studio(StudioCommand),
    /// Schema-generated help for the selected command.
    Help {
        /// Selected command.
        command: CommandScope,
        /// Robot-mode response.
        robot: bool,
    },
    /// Version report.
    Version {
        /// Robot-mode response.
        robot: bool,
    },
    /// Defined cache lifecycle action through fmn-cache's one-shot owned-root
    /// authorization.
    ClearCache {
        /// Shared configuration and robot flags.
        common: CommonOptions,
    },
}

#[derive(Debug, Clone, Default)]
struct Collected {
    values: BTreeMap<&'static str, Vec<String>>,
    explicit: BTreeSet<&'static str>,
    implied: BTreeSet<&'static str>,
}

impl Collected {
    fn set_switch(&mut self, binding: &'static str, implied: bool) {
        self.values.insert(binding, vec!["true".to_owned()]);
        if implied {
            self.implied.insert(binding);
        } else {
            self.explicit.insert(binding);
        }
    }

    fn set_value(&mut self, binding: &'static str, value: String) {
        self.values.insert(binding, vec![value]);
        self.explicit.insert(binding);
    }

    fn extend_values(&mut self, binding: &'static str, values: impl IntoIterator<Item = String>) {
        let destination = self.values.entry(binding).or_default();
        destination.extend(values);
        if !destination.is_empty() {
            self.explicit.insert(binding);
        }
    }

    fn present(&self, binding: &str) -> bool {
        self.explicit.contains(binding) || self.implied.contains(binding)
    }

    fn bool(&self, binding: &str) -> bool {
        self.values
            .get(binding)
            .and_then(|values| values.last())
            .map_or_else(
                || {
                    spec_by_binding(binding)
                        .and_then(|spec| spec.default)
                        .is_some_and(parse_bool)
                },
                |value| parse_bool(value),
            )
    }

    fn value(&self, binding: &str) -> Option<&str> {
        self.values
            .get(binding)
            .and_then(|values| values.last())
            .map(String::as_str)
            .or_else(|| spec_by_binding(binding).and_then(|spec| spec.default))
    }

    fn explicit_value(&self, binding: &str) -> Option<&str> {
        self.values
            .get(binding)
            .and_then(|values| values.last())
            .map(String::as_str)
    }

    fn many(&self, binding: &str) -> Vec<String> {
        self.values.get(binding).cloned().unwrap_or_default()
    }
}

fn typed_consumer_scope(binding: &str) -> Option<CommandScope> {
    Some(match binding {
        "clear_cache" | "config_file" | "help" | "robot" | "reproducible" | "ffmpeg"
        | "cache_dir" | "threads" | "log_level" | "quiet" | "version" => CommandScope::Global,
        "autoreload"
        | "file_name"
        | "finder"
        | "fps"
        | "hd"
        | "leave_progress_bars"
        | "pix_fmt"
        | "prerun"
        | "show_animation_progress"
        | "subdivide"
        | "uhd"
        | "vcodec"
        | "video_dir"
        | "write_all"
        | "background"
        | "embed"
        | "full_screen"
        | "gif"
        | "low_quality"
        | "medium_quality"
        | "animation_range"
        | "open"
        | "presenter_mode"
        | "resolution"
        | "skip_animations"
        | "transparent"
        | "write_file"
        | "file"
        | "scene_names"
        | "format"
        | "math_pack" => CommandScope::Render,
        "require_ffmpeg" => CommandScope::Doctor,
        "budget_ms" | "max_scenes" | "fail_fast" | "manifest_dir" => CommandScope::Batch,
        "bind" | "port" | "no_browser" | "tui" | "checkpoint_frames" | "preview_codec" => {
            CommandScope::Studio
        }
        _ => return None,
    })
}

fn value_type_supported(value_type: &str) -> bool {
    matches!(
        value_type,
        "int"
            | "usize"
            | "u64"
            | "u16"
            | "ip"
            | "output_format"
            | "preview_codec"
            | "path"
            | "pack"
    )
}

fn validate_generated_contract() -> Result<(), CliError> {
    for spec in FLAG_SPECS {
        let Some(consumer_scope) = typed_consumer_scope(spec.binding) else {
            return Err(internal(format!(
                "generated binding `{}` has no typed consumer",
                spec.binding
            )));
        };
        if consumer_scope != spec.command {
            return Err(internal(format!(
                "generated binding `{}` is declared for `{}` but its typed consumer belongs to `{}`",
                spec.binding,
                scope_name(spec.command),
                scope_name(consumer_scope),
            )));
        }
        if let Some(value_type) = spec.value_type
            && !value_type_supported(value_type)
        {
            return Err(internal(format!(
                "generated value type `{value_type}` has no validator"
            )));
        }
    }
    Ok(())
}

/// Parse command-line tokens after the executable name.
///
/// Both long `--flag=value` and attached short values such as `-n3,6` are
/// accepted. Short switches may be grouped when every member is a switch.
/// A separated value beginning with `-` is treated as a missing value so an
/// unknown-option typo cannot be swallowed; spell such strings with `=`.
///
/// # Errors
///
/// [`CliError`] with the schema-owned `usage` identity.
pub fn parse_args<I, S>(args: I) -> Result<Invocation, CliError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    validate_generated_contract()?;
    let tokens: Vec<String> = args.into_iter().map(Into::into).collect();
    let mut scope = CommandScope::Render;
    let mut command_seen = false;
    let mut positionals = Vec::new();
    let mut collected = Collected::default();
    let mut index = 0usize;
    let mut options_enabled = true;

    while index < tokens.len() {
        let token = &tokens[index];
        if options_enabled && token == "--" {
            options_enabled = false;
            index += 1;
            continue;
        }
        if options_enabled && token.starts_with('-') && token != "-" {
            let consumed = collect_option(&tokens, index, &mut collected)?;
            index += consumed;
            continue;
        }
        if options_enabled
            && !command_seen
            && positionals.is_empty()
            && let Some(command) = command_named(token)
        {
            scope = command;
            command_seen = true;
            index += 1;
            continue;
        }
        positionals.push(token.clone());
        index += 1;
    }

    assign_positionals(scope, positionals, &mut collected)?;
    validate_scope(scope, &collected)?;
    apply_interactions(&mut collected, command_seen)?;
    validate_value_types(&collected)?;
    build_invocation(scope, collected, command_seen)
}

fn collect_option(
    tokens: &[String],
    index: usize,
    collected: &mut Collected,
) -> Result<usize, CliError> {
    let token = &tokens[index];
    let (alias, attached) = token
        .split_once('=')
        .map_or((token.as_str(), None), |(alias, value)| {
            (alias, Some(value))
        });
    if let Some(spec) = spec_by_alias(alias) {
        return collect_spec(tokens, index, spec, attached, collected);
    }

    if token.starts_with('-') && !token.starts_with("--") && token.len() > 2 {
        for (offset, ch) in token[1..].char_indices() {
            let alias = format!("-{ch}");
            let Some(spec) = spec_by_alias(&alias) else {
                return Err(usage(format!("unknown option `{alias}` in `{token}`")));
            };
            if spec.action == FlagAction::SetTrue {
                collected.set_switch(spec.binding, false);
                continue;
            }
            let value_offset = 1 + offset + ch.len_utf8();
            let attached = token.get(value_offset..).filter(|value| !value.is_empty());
            return collect_spec(tokens, index, spec, attached, collected);
        }
        return Ok(1);
    }

    Err(usage(format!("unknown option `{token}`")))
}

fn collect_spec(
    tokens: &[String],
    index: usize,
    spec: &'static FlagSpec,
    attached: Option<&str>,
    collected: &mut Collected,
) -> Result<usize, CliError> {
    if spec.status == FlagStatus::Excluded {
        return Err(usage(format!(
            "option `{}` is excluded from FrankenManim",
            spec.options.first().copied().unwrap_or(spec.binding)
        )));
    }
    match spec.action {
        FlagAction::SetTrue => {
            if attached.is_some() {
                return Err(usage(format!(
                    "switch `{}` does not take a value",
                    spec.options.first().copied().unwrap_or(spec.binding)
                )));
            }
            collected.set_switch(spec.binding, false);
            Ok(1)
        }
        FlagAction::Store => {
            if let Some(value) = attached {
                collected.set_value(spec.binding, value.to_owned());
                return Ok(1);
            }
            let Some(value) = tokens.get(index + 1) else {
                return Err(usage(format!(
                    "option `{}` requires a value",
                    spec.options.first().copied().unwrap_or(spec.binding)
                )));
            };
            if value.starts_with('-') && value != "-" {
                return Err(usage(format!(
                    "option `{}` requires a value before `{value}`",
                    spec.options.first().copied().unwrap_or(spec.binding)
                )));
            }
            collected.set_value(spec.binding, value.clone());
            Ok(2)
        }
    }
}

fn assign_positionals(
    scope: CommandScope,
    positionals: Vec<String>,
    collected: &mut Collected,
) -> Result<(), CliError> {
    match scope {
        CommandScope::Render | CommandScope::Batch | CommandScope::Studio => {
            let mut positionals = positionals.into_iter();
            if let Some(file) = positionals.next() {
                collected.set_value("file", file);
            }
            collected.extend_values("scene_names", positionals);
            Ok(())
        }
        CommandScope::Doctor | CommandScope::Global => {
            if positionals.is_empty() {
                Ok(())
            } else {
                Err(usage(format!(
                    "`{}` does not accept positional arguments",
                    scope_name(scope)
                )))
            }
        }
    }
}

fn validate_scope(scope: CommandScope, collected: &Collected) -> Result<(), CliError> {
    for binding in &collected.explicit {
        let Some(spec) = spec_by_binding(binding) else {
            return Err(internal(format!(
                "generated binding `{binding}` has no flag specification"
            )));
        };
        if scope_accepts(scope, spec.command) {
            continue;
        }
        return Err(usage(format!(
            "option `{}` belongs to `{}`, not `{}`",
            spec.options.first().copied().unwrap_or(spec.binding),
            scope_name(spec.command),
            scope_name(scope)
        )));
    }
    Ok(())
}

fn scope_accepts(selected: CommandScope, declared: CommandScope) -> bool {
    declared == CommandScope::Global
        || selected == declared
        || matches!(selected, CommandScope::Batch | CommandScope::Studio)
            && declared == CommandScope::Render
}

fn apply_interactions(collected: &mut Collected, command_seen: bool) -> Result<(), CliError> {
    for _ in 0..INTERACTION_SPECS.len() {
        let mut changed = false;
        for rule in INTERACTION_SPECS
            .iter()
            .filter(|rule| rule.kind == InteractionKind::Implies)
        {
            if interaction_operand_present(collected, rule.operands[0])
                && !collected.present(rule.operands[1])
            {
                collected.set_switch(rule.operands[1], true);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    for rule in INTERACTION_SPECS
        .iter()
        .filter(|rule| rule.kind != InteractionKind::Implies)
    {
        let violation = match rule.kind {
            InteractionKind::AtMostOne => {
                rule.operands
                    .iter()
                    .filter(|operand| interaction_operand_present(collected, operand))
                    .count()
                    > 1
            }
            InteractionKind::Conflicts => {
                interaction_operand_present(collected, rule.operands[0])
                    && interaction_operand_present(collected, rule.operands[1])
            }
            InteractionKind::RequiresAny => {
                interaction_operand_present(collected, rule.operands[0])
                    && !rule.operands[1..]
                        .iter()
                        .any(|operand| interaction_operand_present(collected, operand))
            }
            InteractionKind::Exclusive => {
                collected.present(rule.operands[0])
                    && (collected.explicit.iter().any(|binding| {
                        *binding != rule.operands[0] && !rule.operands[1..].contains(binding)
                    }) || command_seen && matches!(rule.operands[0], "version" | "clear_cache"))
            }
            InteractionKind::Implies => false,
        };
        if violation {
            return Err(CliError::interaction(
                rule.id,
                rule.exit_code.unwrap_or("internal"),
                rule.message,
            ));
        }
    }
    Ok(())
}

fn interaction_operand_present(collected: &Collected, operand: &str) -> bool {
    let Some((binding, selected_values)) = operand.split_once('=') else {
        return collected.present(operand);
    };
    collected
        .explicit_value(binding)
        .is_some_and(|value| selected_values.split(',').any(|selected| value == selected))
}

fn validate_value_types(collected: &Collected) -> Result<(), CliError> {
    for (binding, values) in &collected.values {
        let Some(spec) = spec_by_binding(binding) else {
            return Err(internal(format!(
                "generated binding `{binding}` has no flag specification"
            )));
        };
        let Some(value_type) = spec.value_type else {
            continue;
        };
        for value in values {
            let valid = match value_type {
                "int" => value.parse::<i64>().is_ok(),
                "usize" => value.parse::<usize>().is_ok_and(|value| value > 0),
                "u64" => value.parse::<u64>().is_ok(),
                "u16" => value.parse::<u16>().is_ok(),
                "ip" => value.parse::<std::net::IpAddr>().is_ok(),
                "output_format" => OutputFormat::parse(value).is_some(),
                "preview_codec" => matches!(value.as_str(), "png" | "mjpeg"),
                "path" | "pack" => !value.is_empty(),
                _ => {
                    return Err(internal(format!(
                        "generated value type `{value_type}` has no validator"
                    )));
                }
            };
            if !valid {
                return Err(usage(format!(
                    "invalid {value_type} value {value:?} for `{}`",
                    spec.options.first().copied().unwrap_or(spec.binding)
                )));
            }
        }
    }
    Ok(())
}

fn build_invocation(
    scope: CommandScope,
    values: Collected,
    command_seen: bool,
) -> Result<Invocation, CliError> {
    let common = common_options(&values)?;
    if values.bool("help") {
        return Ok(Invocation::Help {
            command: if command_seen {
                scope
            } else {
                CommandScope::Global
            },
            robot: common.robot,
        });
    }
    if values.bool("version") {
        return Ok(Invocation::Version {
            robot: common.robot,
        });
    }
    if values.bool("clear_cache") {
        return Ok(Invocation::ClearCache { common });
    }

    match scope {
        CommandScope::Render => build_render(values, common).map(Invocation::Render),
        CommandScope::Doctor => Ok(Invocation::Doctor(DoctorCommand {
            common,
            require_ffmpeg: values.bool("require_ffmpeg"),
        })),
        CommandScope::Batch => {
            let render = build_render(values.clone(), common)?;
            Ok(Invocation::Batch(BatchCommand {
                render,
                budget_ms: parse_optional(&values, "budget_ms")?,
                max_scenes: parse_optional(&values, "max_scenes")?,
                fail_fast: values.bool("fail_fast"),
                manifest_dir: values.explicit_value("manifest_dir").map(PathBuf::from),
            }))
        }
        CommandScope::Studio => {
            let render = build_render(values.clone(), common)?;
            let bind = values
                .value("bind")
                .unwrap_or("127.0.0.1")
                .parse::<std::net::IpAddr>()
                .map_err(|_| usage("invalid Studio bind address"))?;
            if !bind.is_loopback() {
                return Err(usage(
                    "Studio is loopback-only; --bind must be a loopback address",
                ));
            }
            let preview_codec = match values.value("preview_codec").unwrap_or("png") {
                "png" => PreviewCodec::Png,
                "mjpeg" => PreviewCodec::Mjpeg,
                _ => return Err(usage("preview codec must be `png` or `mjpeg`")),
            };
            let checkpoint_frames = parse_value(&values, "checkpoint_frames", "120")?;
            if checkpoint_frames == 0 {
                return Err(usage("--checkpoint-frames must be greater than zero"));
            }
            Ok(Invocation::Studio(StudioCommand {
                render,
                bind,
                port: parse_value(&values, "port", "0")?,
                no_browser: values.bool("no_browser"),
                tui: values.bool("tui"),
                checkpoint_frames,
                preview_codec,
            }))
        }
        CommandScope::Global => Err(internal("global scope cannot be dispatched")),
    }
}

fn common_options(values: &Collected) -> Result<CommonOptions, CliError> {
    Ok(CommonOptions {
        robot: values.bool("robot"),
        quiet: values.bool("quiet"),
        reproducible: values.bool("reproducible"),
        config_file: values.explicit_value("config_file").map(PathBuf::from),
        cache_dir: values.explicit_value("cache_dir").map(PathBuf::from),
        ffmpeg: values.explicit_value("ffmpeg").map(PathBuf::from),
        threads: parse_optional(values, "threads")?,
        log_level: values.explicit_value("log_level").map(str::to_owned),
    })
}

fn build_render(values: Collected, common: CommonOptions) -> Result<RenderCommand, CliError> {
    let gif = values.bool("gif");
    let format = if gif {
        OutputFormat::Gif
    } else {
        OutputFormat::parse(values.value("format").unwrap_or("auto"))
            .ok_or_else(|| usage("unknown output format"))?
    };

    Ok(RenderCommand {
        common,
        file: values.explicit_value("file").map(PathBuf::from),
        scene_names: values.many("scene_names"),
        write_all: values.bool("write_all"),
        write_file: values.bool("write_file"),
        skip_animations: values.bool("skip_animations"),
        resolution: parse_resolution_selection(&values)?,
        fps: parse_optional_positive(&values, "fps")?,
        animation_range: values
            .explicit_value("animation_range")
            .map(parse_animation_range)
            .transpose()?,
        presenter_mode: values.bool("presenter_mode"),
        full_screen: values.bool("full_screen"),
        transparent: values.bool("transparent"),
        subdivide: values.bool("subdivide"),
        prerun: values.bool("prerun"),
        autoreload: values.bool("autoreload"),
        embed_line: parse_optional(&values, "embed")?,
        background: values.explicit_value("background").map(str::to_owned),
        file_name: values.explicit_value("file_name").map(str::to_owned),
        video_dir: values.explicit_value("video_dir").map(PathBuf::from),
        vcodec: values.explicit_value("vcodec").map(str::to_owned),
        pix_fmt: values.explicit_value("pix_fmt").map(str::to_owned),
        format,
        math_pack: values.explicit_value("math_pack").map(str::to_owned),
        open: values.bool("open"),
        finder: values.bool("finder"),
        show_animation_progress: values.bool("show_animation_progress"),
        leave_progress_bars: values.bool("leave_progress_bars"),
    })
}

fn parse_resolution_selection(values: &Collected) -> Result<Option<ResolutionChoice>, CliError> {
    if values.bool("low_quality") {
        return Ok(Some(ResolutionChoice::Low));
    }
    if values.bool("medium_quality") {
        return Ok(Some(ResolutionChoice::Medium));
    }
    if values.bool("hd") {
        return Ok(Some(ResolutionChoice::High));
    }
    if values.bool("uhd") {
        return Ok(Some(ResolutionChoice::Uhd));
    }
    values
        .explicit_value("resolution")
        .map(|value| {
            parse_resolution(value).map(|(width, height)| ResolutionChoice::Exact(width, height))
        })
        .transpose()
}

fn parse_resolution(value: &str) -> Result<(u32, u32), CliError> {
    let Some((width, height)) = value.split_once('x').or_else(|| value.split_once('X')) else {
        return Err(usage("resolution must be WIDTHxHEIGHT"));
    };
    let width = width
        .parse::<u32>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| usage("resolution width must be a positive integer"))?;
    let height = height
        .parse::<u32>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| usage("resolution height must be a positive integer"))?;
    Ok((width, height))
}

fn parse_animation_range(value: &str) -> Result<AnimationRange, CliError> {
    let mut parts = value.split(',');
    let start = parts
        .next()
        .and_then(|part| part.parse::<u64>().ok())
        .ok_or_else(|| usage("animation range must be START or START,END"))?;
    let end = parts
        .next()
        .map(|part| {
            part.parse::<u64>()
                .map_err(|_| usage("animation range must be START or START,END"))
        })
        .transpose()?;
    if parts.next().is_some() {
        return Err(usage("animation range must be START or START,END"));
    }
    if end.is_some_and(|end| end < start) {
        return Err(usage("animation range END must not precede START"));
    }
    Ok(AnimationRange { start, end })
}

fn parse_optional<T>(values: &Collected, binding: &str) -> Result<Option<T>, CliError>
where
    T: std::str::FromStr,
{
    values
        .explicit_value(binding)
        .map(|value| {
            value
                .parse::<T>()
                .map_err(|_| usage(format!("invalid value {value:?} for `{binding}`")))
        })
        .transpose()
}

fn parse_optional_positive<T>(values: &Collected, binding: &str) -> Result<Option<T>, CliError>
where
    T: std::str::FromStr + PartialEq + From<u8>,
{
    let value = parse_optional::<T>(values, binding)?;
    if value.as_ref().is_some_and(|value| *value == T::from(0)) {
        return Err(usage(format!("`{binding}` must be greater than zero")));
    }
    Ok(value)
}

fn parse_value<T>(values: &Collected, binding: &str, default: &str) -> Result<T, CliError>
where
    T: std::str::FromStr,
{
    values
        .value(binding)
        .unwrap_or(default)
        .parse::<T>()
        .map_err(|_| usage(format!("invalid value for `{binding}`")))
}

fn command_named(name: &str) -> Option<CommandScope> {
    SUBCOMMAND_SPECS.iter().find_map(|subcommand| {
        (scope_name(subcommand.command) == name).then_some(subcommand.command)
    })
}

fn spec_by_alias(alias: &str) -> Option<&'static FlagSpec> {
    FLAG_SPECS.iter().find(|spec| spec.options.contains(&alias))
}

fn spec_by_binding(binding: &str) -> Option<&'static FlagSpec> {
    FLAG_SPECS.iter().find(|spec| spec.binding == binding)
}

const fn scope_name(scope: CommandScope) -> &'static str {
    match scope {
        CommandScope::Global => "global",
        CommandScope::Render => "render",
        CommandScope::Doctor => "doctor",
        CommandScope::Batch => "batch",
        CommandScope::Studio => "studio",
    }
}

fn parse_bool(value: &str) -> bool {
    matches!(value, "true" | "True" | "TRUE" | "1")
}

fn exit_code(name: &str) -> u8 {
    EXIT_CODE_SPECS
        .iter()
        .find(|exit| exit.name == name)
        .map_or(70, |exit| exit.code)
}

/// Schema-owned status used when the executable cannot publish its response.
#[must_use]
pub fn internal_exit_code() -> u8 {
    exit_code("internal")
}

fn usage(message: impl Into<String>) -> CliError {
    CliError::new("usage", message)
}

fn internal(message: impl Into<String>) -> CliError {
    CliError::new("internal", message)
}

/// Resolve a render command through the one config precedence chain.
///
/// The file is read through the supplied filesystem capability. Quality
/// presets are resolved from the user-layer values before the CLI overlay, so
/// a custom `resolution_options` table remains authoritative.
///
/// # Errors
///
/// [`CliError`] with the stable `config` identity.
pub fn resolve_render_config(
    fs: &dyn FileSystem,
    command: &RenderCommand,
) -> Result<fmn_config::Config, CliError> {
    let documents = read_config_documents(fs, command.common.config_file.as_deref())?;
    let layers = documents.layers();
    let base = fmn_config::Config::resolve(&layers, None)
        .map_err(|error| CliError::new("config", error.to_string()))?
        .config;

    let mut pairs = common_overlay(&command.common)?;
    if let Some(fps) = command.fps {
        pairs.push(("camera.fps", fmn_config::Value::Int(i64::from(fps))));
    }
    let resolution = match command.resolution {
        Some(ResolutionChoice::Low) => Some(base.resolution_options.low),
        Some(ResolutionChoice::Medium) => Some(base.resolution_options.med),
        Some(ResolutionChoice::High) => Some(base.resolution_options.high),
        Some(ResolutionChoice::Uhd) => Some(base.resolution_options.uhd),
        Some(ResolutionChoice::Exact(width, height)) => Some((width, height)),
        None => None,
    };
    if let Some((width, height)) = resolution {
        pairs.push((
            "camera.resolution",
            fmn_config::Value::Str(format!("({width}, {height})")),
        ));
    }
    if let Some(background) = &command.background {
        pairs.push((
            "camera.background_color",
            fmn_config::Value::Str(background.clone()),
        ));
    }
    if command.transparent {
        pairs.push(("camera.background_opacity", fmn_config::Value::Float(0.0)));
    }
    if command.full_screen {
        pairs.push(("window.full_screen", fmn_config::Value::Bool(true)));
    }
    if command.show_animation_progress {
        pairs.push((
            "scene.show_animation_progress",
            fmn_config::Value::Bool(true),
        ));
    }
    if command.leave_progress_bars {
        pairs.push(("scene.leave_progress_bars", fmn_config::Value::Bool(true)));
    }
    if command.autoreload {
        pairs.push(("embed.autoreload", fmn_config::Value::Bool(true)));
    }
    if let Some(vcodec) = &command.vcodec {
        pairs.push((
            "file_writer.video_codec",
            fmn_config::Value::Str(vcodec.clone()),
        ));
    }
    if let Some(pix_fmt) = &command.pix_fmt {
        pairs.push((
            "file_writer.pixel_format",
            fmn_config::Value::Str(pix_fmt.clone()),
        ));
    }
    if let Some(math_pack) = &command.math_pack {
        pairs.push(("tex.template", fmn_config::Value::Str(math_pack.clone())));
    }

    let resolved = fmn_config::Config::resolve(&layers, Some(fmn_config::config::overlay(pairs)))
        .map_err(|error| CliError::new("config", error.to_string()))?;
    fmn_config::PackRegistry::builtin()
        .resolve_template(&resolved.config.tex.template)
        .map_err(|error| CliError::new("config", error.to_string()))?;
    Ok(resolved.config)
}

/// Resolve doctor/common configuration through the same precedence chain.
///
/// # Errors
///
/// [`CliError`] with the stable `config` identity.
pub fn resolve_common_config(
    fs: &dyn FileSystem,
    common: &CommonOptions,
) -> Result<fmn_config::Config, CliError> {
    let documents = read_config_documents(fs, common.config_file.as_deref())?;
    let layers = documents.layers();
    fmn_config::Config::resolve(
        &layers,
        Some(fmn_config::config::overlay(common_overlay(common)?)),
    )
    .map(|resolved| resolved.config)
    .map_err(|error| CliError::new("config", error.to_string()))
}

#[derive(Debug)]
struct ConfigDocument {
    source: String,
    text: String,
}

#[derive(Debug, Default)]
struct ConfigDocuments {
    custom: Option<ConfigDocument>,
    explicit: Option<ConfigDocument>,
}

impl ConfigDocuments {
    fn layers(&self) -> Vec<fmn_config::config::Layer<'_>> {
        let mut layers = Vec::with_capacity(2);
        if let Some(document) = self.custom.as_ref() {
            layers.push(fmn_config::config::Layer {
                name: &document.source,
                text: &document.text,
            });
        }
        if let Some(document) = self.explicit.as_ref() {
            layers.push(fmn_config::config::Layer {
                name: &document.source,
                text: &document.text,
            });
        }
        layers
    }
}

fn read_config_documents(
    fs: &dyn FileSystem,
    explicit_path: Option<&Path>,
) -> Result<ConfigDocuments, CliError> {
    Ok(ConfigDocuments {
        custom: read_optional_config(fs, Path::new("custom_config.yml"), "custom_config.yml")?,
        explicit: explicit_path
            .map(|path| {
                read_optional_config(fs, path, format!("--config_file {:?}", path.as_os_str()))
            })
            .transpose()?
            .flatten(),
    })
}

fn read_optional_config(
    fs: &dyn FileSystem,
    path: &Path,
    source: impl Into<String>,
) -> Result<Option<ConfigDocument>, CliError> {
    let source = source.into();
    match fs.read_to_string_bounded(path, fmn_config::yaml::Limits::DEFAULT.max_bytes) {
        Ok(text) => Ok(Some(ConfigDocument { source, text })),
        Err(FsError::NotFound { .. }) => Ok(None),
        Err(error) => Err(CliError::new(
            "config",
            format!("could not read {source}: {error}"),
        )),
    }
}

fn common_overlay(
    common: &CommonOptions,
) -> Result<Vec<(&'static str, fmn_config::Value)>, CliError> {
    let mut pairs = Vec::new();
    if common.reproducible {
        pairs.push((
            "determinism.mode",
            fmn_config::Value::Str("certified".to_owned()),
        ));
        pairs.push(("render.engine", fmn_config::Value::Str("cpu".to_owned())));
    }
    if let Some(threads) = common.threads {
        let threads = i64::try_from(threads)
            .map_err(|_| CliError::new("config", "thread count exceeds i64"))?;
        pairs.push(("render.threads", fmn_config::Value::Int(threads)));
    }
    if let Some(ffmpeg) = &common.ffmpeg {
        pairs.push((
            "file_writer.ffmpeg_bin",
            fmn_config::Value::Str(strict_path_text(ffmpeg, "ffmpeg path")?.to_owned()),
        ));
    }
    if let Some(cache_dir) = &common.cache_dir {
        pairs.push((
            "directories.cache",
            fmn_config::Value::Str(strict_path_text(cache_dir, "cache root")?.to_owned()),
        ));
    }
    if let Some(level) = &common.log_level {
        pairs.push(("log_level", fmn_config::Value::Str(level.clone())));
    }
    Ok(pairs)
}

fn strict_path_text<'a>(path: &'a Path, label: &str) -> Result<&'a str, CliError> {
    if path.as_os_str().is_empty() {
        return Err(CliError::new("config", format!("{label} may not be empty")));
    }
    path.to_str().ok_or_else(|| {
        CliError::new(
            "config",
            format!("{label} is not valid UTF-8 and cannot be represented by the CLI schema"),
        )
    })
}

fn detect_topology(
    fs: &dyn FileSystem,
) -> (fmn_platform::topology::HardwareTopology, TopologySource) {
    let logical = std::thread::available_parallelism()
        .ok()
        .and_then(|count| u32::try_from(count.get()).ok())
        .unwrap_or(1);
    if cfg!(target_os = "linux") {
        match fmn_platform::topology::HardwareTopology::detect_linux(fs) {
            Ok(topology) => (topology, TopologySource::LinuxSysfs),
            Err(error) => (
                fmn_platform::topology::HardwareTopology::fallback(logical),
                TopologySource::Fallback {
                    reason: format!("Linux topology introspection failed: {error}"),
                },
            ),
        }
    } else {
        (
            fmn_platform::topology::HardwareTopology::fallback(logical),
            TopologySource::Fallback {
                reason: format!(
                    "{} topology introspection is not implemented",
                    std::env::consts::OS
                ),
            },
        )
    }
}

fn derive_execution_plan(
    fs: &dyn FileSystem,
    config: &fmn_config::Config,
    intent: fmn_runtime::RenderIntent,
    output_format: fmn_runtime::OutputPixelFormat,
) -> Result<
    (
        fmn_runtime::ExecutionPlan,
        fmn_platform::topology::HardwareTopology,
        TopologySource,
    ),
    CliError,
> {
    let (topology, topology_source) = detect_topology(fs);
    let surface =
        fmn_runtime::SurfaceSpec::lumen(config.camera.resolution.0, config.camera.resolution.1);
    let mut request = match config.determinism.mode {
        fmn_config::config::DeterminismMode::Certified => {
            fmn_runtime::PlanRequest::certified(intent, surface, output_format)
        }
        fmn_config::config::DeterminismMode::Standard => {
            fmn_runtime::PlanRequest::standard(intent, surface, output_format)
        }
    };
    let engine = match (config.determinism.mode, config.render.engine) {
        (fmn_config::config::DeterminismMode::Certified, fmn_config::config::Engine::Cpu) => {
            fmn_runtime::ExecutionEngine::CertifiedCpu
        }
        (
            fmn_config::config::DeterminismMode::Certified,
            fmn_config::config::Engine::Metal | fmn_config::config::Engine::Cuda,
        ) => {
            return Err(CliError::new(
                "config",
                "certified determinism requires render.engine=cpu",
            ));
        }
        (fmn_config::config::DeterminismMode::Standard, fmn_config::config::Engine::Cpu) => {
            fmn_runtime::ExecutionEngine::FastCpu
        }
        (fmn_config::config::DeterminismMode::Standard, fmn_config::config::Engine::Metal) => {
            return Err(CliError::new(
                "capability",
                "render.engine=metal is unavailable: this CLI has no verified compiled Metal backend",
            ));
        }
        (fmn_config::config::DeterminismMode::Standard, fmn_config::config::Engine::Cuda) => {
            return Err(CliError::new(
                "capability",
                "render.engine=cuda is unavailable: this CLI has no verified compiled CUDA backend",
            ));
        }
    };
    request = request.with_engine(engine);
    if let fmn_config::config::ThreadPolicy::Fixed(threads) = config.render.threads {
        let threads = usize::try_from(threads)
            .map_err(|_| CliError::new("config", "thread count does not fit this target"))?;
        request = request.with_max_cpu_threads(threads);
    }
    let plan = fmn_runtime::ExecutionPlan::derive(request, &topology, None)
        .map_err(execution_plan_error)?;
    Ok((plan, topology, topology_source))
}

/// Collect a truthful doctor snapshot. Optional capabilities are represented
/// as unavailable records; only configuration or plan derivation failures
/// abort collection.
///
/// ffmpeg discovery is delegated to the injected ffmpeg-only platform
/// capability. That capability may canonicalize an explicit absolute path or
/// search its snapshotted, fully validated `PATH`; this function never reads
/// ambient executable-search state.
///
/// # Errors
///
/// [`CliError`] with `config`, `capability`, or `internal` identity.
pub fn collect_doctor_snapshot(
    fs: &dyn FileSystem,
    runner: &dyn fmn_platform::process::ProcessRunner,
    locator: &dyn fmn_platform::process::FfmpegLocator,
    command: &DoctorCommand,
) -> Result<DoctorSnapshot, CliError> {
    let config = resolve_common_config(fs, &command.common)?;
    let output_format = planning_output_format(&config.file_writer.pixel_format);
    let (plan, topology, topology_source) = derive_execution_plan(
        fs,
        &config,
        fmn_runtime::RenderIntent::Offline,
        output_format,
    )?;
    let plan = ExecutionPlanReport {
        determinism: match plan.determinism {
            fmn_runtime::Determinism::Standard => "standard",
            fmn_runtime::Determinism::Certified => "certified",
        },
        engine: match plan.engine {
            fmn_runtime::ExecutionEngine::CertifiedCpu => "certified-cpu",
            fmn_runtime::ExecutionEngine::FastCpu => "fast-cpu",
            fmn_runtime::ExecutionEngine::Metal => "metal",
            fmn_runtime::ExecutionEngine::Cuda => "cuda",
        },
        frames_in_flight: plan.frames_in_flight,
        scene_threads: plan.scene_team.threads(),
        render_teams: plan.render_teams.len(),
        render_threads: plan
            .render_teams
            .iter()
            .map(fmn_runtime::TeamPlan::threads)
            .sum(),
        output_threads: plan.output_team.threads(),
        fine_tile: plan.fine_tile,
        macro_tile: plan.macro_tile,
        estimated_in_flight_bytes: plan.estimated_in_flight_bytes,
        output_format: plan.output_format.name(),
        tuning_source: match plan.tuning_source {
            fmn_runtime::TuningSource::CertifiedProfile => "certified-profile",
            fmn_runtime::TuningSource::StandardBaseline => "standard-baseline",
            fmn_runtime::TuningSource::StandardAutotuneCache => "standard-autotune-cache",
        },
    };

    let ffmpeg_path = PathBuf::from(&config.file_writer.ffmpeg_bin);
    let ffmpeg = locate_and_probe_ffmpeg(locator, runner, &ffmpeg_path);
    let cache = cache_report_from_resolution(
        fs,
        fmn_cache::resolve_host_cache_root(&config.directories.cache),
    )?;
    let math_packs = fmn_config::PackRegistry::builtin()
        .names()
        .into_iter()
        .map(str::to_owned)
        .collect();

    Ok(DoctorSnapshot {
        topology_source,
        logical_cores: topology.logical_cores(),
        physical_cores: topology.physical_cores,
        hardware_supported_tier: topology.simd_tier.name().to_owned(),
        active_compiled_tier: active_compiled_tier(),
        plan,
        ffmpeg,
        cache,
        fonts: FontReport {
            selected: config.text.font,
            bundled: Vec::new(),
            user: Vec::new(),
            complete: false,
            detail: Some(
                "font-source inventory is not yet exposed through a capability; selected family only"
                    .to_owned(),
            ),
        },
        math_packs,
        certification: certification_report(),
    })
}

fn locate_and_probe_ffmpeg(
    locator: &dyn fmn_platform::process::FfmpegLocator,
    runner: &dyn fmn_platform::process::ProcessRunner,
    configured: &Path,
) -> FfmpegReport {
    let executable = match locator.locate_ffmpeg(configured) {
        Ok(executable) => executable,
        Err(error) => {
            return FfmpegReport::Unavailable {
                attempted: configured.to_path_buf(),
                reason: error.to_string(),
                alternative: fmn_output::NATIVE_ALTERNATIVE.to_owned(),
            };
        }
    };
    match probe_ffmpeg(runner, executable) {
        available @ FfmpegReport::Available { .. } => available,
        FfmpegReport::Unavailable {
            reason,
            alternative,
            ..
        } => FfmpegReport::Unavailable {
            attempted: configured.to_path_buf(),
            reason,
            alternative,
        },
    }
}

fn cache_report_from_resolution(
    fs: &dyn FileSystem,
    resolution: Result<PathBuf, fmn_cache::CacheRootError>,
) -> Result<CacheReport, CliError> {
    match resolution {
        Ok(root) => Ok(inspect_cache(fs, &root, MAX_DOCTOR_CACHE_DIRECT_ENTRIES)),
        Err(error @ fmn_cache::CacheRootError::InvalidConfigured { .. }) => {
            Err(cache_root_cli_error(error))
        }
        Err(error) => Ok(CacheReport::Unresolved {
            reason: error.to_string(),
        }),
    }
}

fn execution_plan_error(error: fmn_runtime::PlanError) -> CliError {
    let exit_name = match error {
        fmn_runtime::PlanError::ZeroLimit(_)
        | fmn_runtime::PlanError::InvalidSurface
        | fmn_runtime::PlanError::OddSubsampledDimensions
        | fmn_runtime::PlanError::EngineNotCertifiable
        | fmn_runtime::PlanError::SizeOverflow => "config",
        fmn_runtime::PlanError::EmptyTopology
        | fmn_runtime::PlanError::InvalidProcessorGroups
        | fmn_runtime::PlanError::InvalidAutotune => "internal",
    };
    CliError::new(exit_name, format!("ExecutionPlan: {error}"))
}

fn planning_output_format(pixel_format: &str) -> fmn_runtime::OutputPixelFormat {
    match pixel_format {
        "bgra" | "bgra8" => fmn_runtime::OutputPixelFormat::Bgra8,
        "nv12" => fmn_runtime::OutputPixelFormat::Nv12,
        "p010" | "p010le" => fmn_runtime::OutputPixelFormat::P010,
        _ => fmn_runtime::OutputPixelFormat::Rgba8,
    }
}

fn probe_ffmpeg(
    runner: &dyn fmn_platform::process::ProcessRunner,
    executable: fmn_platform::process::FfmpegExecutable,
) -> FfmpegReport {
    let path = executable.canonical_path().to_path_buf();
    let tool = match fmn_output::FfmpegTool::resolve(executable, runner, &std::env::temp_dir()) {
        Ok(tool) => tool,
        Err(error) => {
            return FfmpegReport::Unavailable {
                attempted: path,
                reason: error.to_string(),
                alternative: fmn_output::NATIVE_ALTERNATIVE.to_owned(),
            };
        }
    };
    let (hardware_encoders, hardware_encoder_probe_error) =
        match fmn_output::EncoderCapabilities::probe(&tool, runner) {
            Ok(encoders) => (encoders.hardware(), None),
            Err(
                error @ (fmn_output::BoundaryError::ExecutableIdentityChanged { .. }
                | fmn_output::BoundaryError::ExecutableImageRejected { .. }
                | fmn_output::BoundaryError::Workdir { .. }),
            ) => {
                return FfmpegReport::Unavailable {
                    attempted: path,
                    reason: error.to_string(),
                    alternative: fmn_output::NATIVE_ALTERNATIVE.to_owned(),
                };
            }
            Err(error) => (Vec::new(), Some(error.to_string())),
        };
    FfmpegReport::Available {
        path: tool.path().to_path_buf(),
        sha256: tool.sha256_hex().to_owned(),
        version: tool.version().to_owned(),
        hardware_encoders,
        hardware_encoder_probe_error,
    }
}

fn cache_root_cli_error(error: fmn_cache::CacheRootError) -> CliError {
    let exit_name = match &error {
        fmn_cache::CacheRootError::InvalidConfigured { .. } => "config",
        fmn_cache::CacheRootError::CurrentDirectory { .. }
        | fmn_cache::CacheRootError::PlatformDefaultUnavailable { .. } => "capability",
    };
    CliError::new(exit_name, error.to_string())
}

const MAX_DOCTOR_CACHE_DIRECT_ENTRIES: usize = 4 * 1024;

fn inspect_cache(fs: &dyn FileSystem, root: &Path, max_entries: usize) -> CacheReport {
    let mut components: Vec<&Path> = root
        .ancestors()
        .filter(|path| !path.as_os_str().is_empty())
        .collect();
    components.reverse();
    for component in components {
        match fs.node_kind_no_follow(component) {
            Ok(Some(FsNodeKind::Directory)) => {}
            Ok(Some(kind)) => {
                return CacheReport::Configured {
                    root: root.to_path_buf(),
                    exists: component == root,
                    direct_entries: None,
                    warning: Some(format!(
                        "cache traversal refused {kind:?} at {:?}",
                        component.as_os_str()
                    )),
                };
            }
            Ok(None) => {
                return CacheReport::Configured {
                    root: root.to_path_buf(),
                    exists: false,
                    direct_entries: Some(0),
                    warning: None,
                };
            }
            Err(error) => {
                return CacheReport::Configured {
                    root: root.to_path_buf(),
                    exists: false,
                    direct_entries: None,
                    warning: Some(error.to_string()),
                };
            }
        }
    }
    match fs.count_dir_entries_bounded(root, max_entries) {
        Ok(entry_count) => CacheReport::Configured {
            root: root.to_path_buf(),
            exists: true,
            direct_entries: Some(entry_count),
            warning: None,
        },
        Err(error) => CacheReport::Configured {
            root: root.to_path_buf(),
            exists: true,
            direct_entries: None,
            warning: Some(error.to_string()),
        },
    }
}

const fn active_compiled_tier() -> &'static str {
    if cfg!(all(
        target_arch = "x86_64",
        target_feature = "avx512f",
        target_feature = "avx512bw",
        target_feature = "avx512dq",
        target_feature = "avx512vl"
    )) {
        "x86-64-v4"
    } else if cfg!(all(
        target_arch = "x86_64",
        target_feature = "avx2",
        target_feature = "bmi2",
        target_feature = "fma"
    )) {
        "x86-64-v3"
    } else if cfg!(all(target_arch = "aarch64", target_feature = "neon")) {
        "aarch64+neon"
    } else {
        "portable"
    }
}

fn certification_report() -> CertificationReport {
    let platform = platform_name(std::env::consts::OS, std::env::consts::ARCH);
    let (supported, detail) = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64" | "aarch64") => (
            true,
            "Revision 4 certified platform; full closure enforcement remains owned by G4b",
        ),
        ("macos", "aarch64") => (
            true,
            "Revision 4 certified platform; full closure enforcement remains owned by G4b",
        ),
        ("windows", "x86_64") => (
            false,
            "windows-x86_64 is pending its declared certification decision",
        ),
        _ => (false, "target is outside the Revision 4 certified matrix"),
    };
    CertificationReport {
        platform,
        supported,
        detail: detail.to_owned(),
    }
}

fn platform_name(os: &str, arch: &str) -> String {
    let arch = match arch {
        "x86_64" => "x86-64",
        other => other,
    };
    format!("{os}-{arch}")
}

impl DoctorSnapshot {
    /// Stable line-oriented robot report. Every line is an independent JSON
    /// object carrying the schema name and version.
    ///
    /// # Errors
    ///
    /// [`CliError`] if a native path cannot be represented exactly by the
    /// version-1 UTF-8 robot schema.
    pub fn to_ndjson(&self) -> Result<String, CliError> {
        let mut out = String::new();
        let (source, source_detail) = match &self.topology_source {
            TopologySource::LinuxSysfs => ("linux-sysfs", None),
            TopologySource::Fallback { reason } => ("fallback", Some(reason.as_str())),
        };
        let _ = writeln!(
            out,
            "{{\"schema\":\"fmn.doctor\",\"version\":{},\"kind\":\"topology\",\
             \"source\":{},\"source_detail\":{},\"logical_cores\":{},\
             \"physical_cores\":{},\"hardware_supported_tier\":{},\
             \"active_compiled_tier\":{}}}",
            ROBOT_SCHEMA_VERSION,
            json_string(source),
            json_option(source_detail),
            self.logical_cores,
            self.physical_cores,
            json_string(&self.hardware_supported_tier),
            json_string(self.active_compiled_tier),
        );
        let _ = writeln!(
            out,
            "{{\"schema\":\"fmn.doctor\",\"version\":{},\"kind\":\"execution_plan\",\
             \"determinism\":{},\"engine\":{},\"frames_in_flight\":{},\
             \"scene_threads\":{},\"render_teams\":{},\"render_threads\":{},\
             \"output_threads\":{},\"fine_tile\":{},\"macro_tile\":{},\
             \"estimated_in_flight_bytes\":{},\"output_format\":{},\
             \"tuning_source\":{}}}",
            ROBOT_SCHEMA_VERSION,
            json_string(self.plan.determinism),
            json_string(self.plan.engine),
            self.plan.frames_in_flight,
            self.plan.scene_threads,
            self.plan.render_teams,
            self.plan.render_threads,
            self.plan.output_threads,
            self.plan.fine_tile,
            self.plan.macro_tile,
            self.plan.estimated_in_flight_bytes,
            json_string(self.plan.output_format),
            json_string(self.plan.tuning_source),
        );
        match &self.ffmpeg {
            FfmpegReport::Available {
                path,
                sha256,
                version,
                hardware_encoders,
                hardware_encoder_probe_error,
            } => {
                let path = strict_path_text(path, "doctor ffmpeg path")?;
                let _ = writeln!(
                    out,
                    "{{\"schema\":\"fmn.doctor\",\"version\":{},\"kind\":\"ffmpeg\",\
                     \"available\":true,\"path\":{},\"sha256\":{},\"ffmpeg_version\":{},\
                     \"hardware_encoders\":{},\"hardware_encoder_probe_error\":{}}}",
                    ROBOT_SCHEMA_VERSION,
                    json_string(path),
                    json_string(sha256),
                    json_string(version),
                    json_array(hardware_encoders),
                    json_option(hardware_encoder_probe_error.as_deref()),
                );
            }
            FfmpegReport::Unavailable {
                attempted,
                reason,
                alternative,
            } => {
                let attempted = strict_path_text(attempted, "doctor ffmpeg attempted path")?;
                let _ = writeln!(
                    out,
                    "{{\"schema\":\"fmn.doctor\",\"version\":{},\"kind\":\"ffmpeg\",\
                     \"available\":false,\"attempted\":{},\"reason\":{},\
                     \"alternative\":{}}}",
                    ROBOT_SCHEMA_VERSION,
                    json_string(attempted),
                    json_string(reason),
                    json_string(alternative),
                );
            }
        }
        match &self.cache {
            CacheReport::Configured {
                root,
                exists,
                direct_entries,
                warning,
            } => {
                let root = strict_path_text(root, "doctor cache root")?;
                let entries = direct_entries.map_or_else(|| "null".to_owned(), |n| n.to_string());
                let _ = writeln!(
                    out,
                    "{{\"schema\":\"fmn.doctor\",\"version\":{},\"kind\":\"cache\",\
                     \"resolved\":true,\"root\":{},\"exists\":{},\
                     \"direct_entries\":{},\"warning\":{}}}",
                    ROBOT_SCHEMA_VERSION,
                    json_string(root),
                    exists,
                    entries,
                    json_option(warning.as_deref()),
                );
            }
            CacheReport::Unresolved { reason } => {
                let _ = writeln!(
                    out,
                    "{{\"schema\":\"fmn.doctor\",\"version\":{},\"kind\":\"cache\",\
                     \"resolved\":false,\"reason\":{}}}",
                    ROBOT_SCHEMA_VERSION,
                    json_string(reason),
                );
            }
        }
        let _ = writeln!(
            out,
            "{{\"schema\":\"fmn.doctor\",\"version\":{},\"kind\":\"fonts\",\
             \"selected\":{},\"bundled\":{},\"user\":{},\"complete\":{},\"detail\":{}}}",
            ROBOT_SCHEMA_VERSION,
            json_string(&self.fonts.selected),
            json_array(&self.fonts.bundled),
            json_array(&self.fonts.user),
            self.fonts.complete,
            json_option(self.fonts.detail.as_deref()),
        );
        let _ = writeln!(
            out,
            "{{\"schema\":\"fmn.doctor\",\"version\":{},\"kind\":\"math_packs\",\
             \"packs\":{}}}",
            ROBOT_SCHEMA_VERSION,
            json_array(&self.math_packs),
        );
        let _ = writeln!(
            out,
            "{{\"schema\":\"fmn.doctor\",\"version\":{},\"kind\":\"certification\",\
             \"platform\":{},\"supported\":{},\"detail\":{}}}",
            ROBOT_SCHEMA_VERSION,
            json_string(&self.certification.platform),
            self.certification.supported,
            json_string(&self.certification.detail),
        );
        Ok(out)
    }

    /// Human report. This presentation is never used in robot mode.
    #[must_use]
    pub fn to_human(&self) -> String {
        let mut out = String::from("FrankenManim doctor\n");
        match &self.topology_source {
            TopologySource::LinuxSysfs => out.push_str("topology: linux sysfs (best available)\n"),
            TopologySource::Fallback { reason } => {
                let _ = writeln!(out, "topology: fallback ({reason})");
            }
        }
        let _ = writeln!(
            out,
            "cores: {} logical / {} physical",
            self.logical_cores, self.physical_cores
        );
        let _ = writeln!(
            out,
            "SIMD: hardware {} / active build {}",
            self.hardware_supported_tier, self.active_compiled_tier
        );
        let _ = writeln!(
            out,
            "plan: {} {}, {} frame slots, {} render teams / {} render threads",
            self.plan.determinism,
            self.plan.engine,
            self.plan.frames_in_flight,
            self.plan.render_teams,
            self.plan.render_threads
        );
        match &self.ffmpeg {
            FfmpegReport::Available {
                path,
                sha256,
                version,
                hardware_encoders,
                hardware_encoder_probe_error,
            } => {
                let _ = writeln!(
                    out,
                    "ffmpeg: {} ({version}, sha256 {sha256})",
                    path.display()
                );
                let _ = writeln!(
                    out,
                    "hardware encoders: {}",
                    if hardware_encoders.is_empty() {
                        "none".to_owned()
                    } else {
                        hardware_encoders.join(", ")
                    }
                );
                if let Some(error) = hardware_encoder_probe_error {
                    let _ = writeln!(out, "hardware encoder probe warning: {error}");
                }
            }
            FfmpegReport::Unavailable {
                attempted,
                reason,
                alternative,
            } => {
                let _ = writeln!(
                    out,
                    "ffmpeg: unavailable at {:?} ({reason})",
                    attempted.as_os_str()
                );
                let _ = writeln!(out, "alternative: {alternative}");
            }
        }
        match &self.cache {
            CacheReport::Configured {
                root,
                exists,
                direct_entries,
                warning,
            } => {
                let _ = writeln!(
                    out,
                    "cache: {} (exists {exists}, direct entries {})",
                    root.display(),
                    direct_entries.map_or_else(|| "unknown".to_owned(), |n| n.to_string())
                );
                if let Some(warning) = warning {
                    let _ = writeln!(out, "cache warning: {warning}");
                }
            }
            CacheReport::Unresolved { reason } => {
                let _ = writeln!(out, "cache: unresolved ({reason})");
            }
        }
        let _ = writeln!(
            out,
            "fonts: selected {}; inventory {}",
            self.fonts.selected,
            if self.fonts.complete {
                "complete"
            } else {
                "partial"
            }
        );
        let _ = writeln!(out, "math packs: {}", self.math_packs.join(", "));
        let _ = writeln!(
            out,
            "certification: {} — {}",
            if self.certification.supported {
                "supported target"
            } else {
                "unsupported/pending target"
            },
            self.certification.detail
        );
        out
    }
}

/// Captured process output used by the binary and integration tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOutput {
    /// Numeric schema-owned exit code.
    pub code: u8,
    /// Bytes for stdout.
    pub stdout: String,
    /// Bytes for stderr.
    pub stderr: String,
}

impl RunOutput {
    fn success(stdout: String) -> Self {
        Self {
            code: exit_code("success"),
            stdout,
            stderr: String::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NativeFrameFormat {
    Png,
    PngSequence,
    Gif,
    Y4m,
}

impl NativeFrameFormat {
    const fn pixel_format(self) -> PixelFormat {
        match self {
            Self::Png | Self::PngSequence | Self::Gif => PixelFormat::Rgba8,
            Self::Y4m => PixelFormat::Nv12,
        }
    }

    const fn planning_format(self) -> fmn_runtime::OutputPixelFormat {
        match self {
            Self::Png | Self::PngSequence | Self::Gif => fmn_runtime::OutputPixelFormat::Rgba8,
            Self::Y4m => fmn_runtime::OutputPixelFormat::Nv12,
        }
    }
}

struct FfmpegRenderContext {
    runner: Arc<dyn fmn_platform::process::ProcessRunner>,
    tool: FfmpegTool,
    capabilities: EncoderCapabilities,
    job: VideoJob,
    workdir_root: PathBuf,
}

enum RenderTarget {
    Native(NativeFrameFormat),
    Video(Box<FfmpegRenderContext>),
}

impl RenderTarget {
    const fn pixel_format(&self) -> PixelFormat {
        match self {
            Self::Native(format) => format.pixel_format(),
            Self::Video(context) => context.job.wire.frame_format(),
        }
    }
}

enum RenderReceipt {
    Native(SinkReceipt<NativeArtifactReport>),
    Video(SinkReceipt<FfmpegArtifactReport>),
}

struct VideoArtifactReport {
    path: PathBuf,
    frame_count: u64,
    input_bytes: u64,
    artifact_bytes: u64,
    artifact_digest: ArtifactDigest,
    tool_path: PathBuf,
    tool_sha256: String,
    tool_version: String,
    native_image_format: &'static str,
    native_image_architecture: &'static str,
    native_image_bytes: u64,
    native_image_policy_version: u32,
    encoder: Option<String>,
    process_mechanism: String,
    process_policy_version: u32,
    argv: Vec<String>,
}

enum RenderArtifactReport {
    Native(NativeArtifactReport),
    Video(VideoArtifactReport),
}

enum RenderSourceReport {
    Builtin,
    Compiled(PathBuf),
}

impl RenderSourceReport {
    const fn kind(&self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::Compiled(_) => "compiled",
        }
    }

    fn artifact(&self) -> Option<&Path> {
        match self {
            Self::Builtin => None,
            Self::Compiled(path) => Some(path),
        }
    }
}

struct CompletedRender {
    source: RenderSourceReport,
    scene: String,
    artifact: RenderArtifactReport,
    engine: String,
    render_threads: usize,
    manifest: Option<ProvenanceManifest>,
    manifest_path: Option<PathBuf>,
}

const RENDER_ACTIVE: u8 = 0;
#[cfg(feature = "batch")]
const RENDER_CANCEL_FAIL_FAST: u8 = 1;
#[cfg(feature = "batch")]
const RENDER_CANCEL_BUDGET: u8 = 2;

#[derive(Debug, Default)]
struct RenderCancellation {
    reason: AtomicU8,
    emitters: Mutex<Vec<EmitterHandle>>,
}

impl RenderCancellation {
    fn is_cancelled(&self) -> bool {
        self.reason.load(Ordering::Acquire) != RENDER_ACTIVE
    }

    #[cfg(feature = "batch")]
    fn request(&self, reason: u8) {
        let _ = self.reason.compare_exchange(
            RENDER_ACTIVE,
            reason,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        let emitters = self
            .emitters
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for emitter in emitters.iter() {
            let _ = emitter.cancel();
        }
    }

    fn register_emitter(&self, emitter: EmitterHandle) {
        let mut emitters = self
            .emitters
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.is_cancelled() {
            let _ = emitter.cancel();
        }
        emitters.push(emitter);
    }

    fn cli_checkpoint(&self) -> Result<(), CliError> {
        if self.is_cancelled() {
            Err(CliError::new("budget", "batch scene job was cancelled"))
        } else {
            Ok(())
        }
    }

    fn scene_checkpoint(&self) -> Result<(), IntegrationError> {
        if self.is_cancelled() {
            Err(IntegrationError::new(
                "batch",
                "scene job cancelled at a semantic boundary",
            ))
        } else {
            Ok(())
        }
    }
}

struct CancellableSceneSink<'a, S> {
    inner: &'a mut S,
    cancellation: &'a RenderCancellation,
}

impl<S> SceneSink for CancellableSceneSink<'_, S>
where
    S: SceneSink,
{
    fn event(&mut self, event: fmn_scene::LifecycleEvent) -> Result<(), IntegrationError> {
        self.cancellation.scene_checkpoint()?;
        self.inner.event(event)
    }

    fn capture(
        &mut self,
        reason: CaptureReason,
        packet: fmn_scene::studio_bridge::FramePacket,
    ) -> Result<(), IntegrationError> {
        self.cancellation.scene_checkpoint()?;
        self.inner.capture(reason, packet)
    }
}

enum NativeRenderInput {
    Builtin {
        names: Vec<String>,
    },
    Compiled {
        source: PathBuf,
        source_item: ClosureItem,
        name: String,
        bundle: Box<TimelineBundle>,
    },
}

#[derive(Default)]
struct StudioPacketCapture {
    packets: Vec<fmn_scene::studio_bridge::FramePacket>,
}

impl SceneSink for StudioPacketCapture {
    fn capture(
        &mut self,
        _reason: CaptureReason,
        packet: fmn_scene::studio_bridge::FramePacket,
    ) -> Result<(), IntegrationError> {
        self.packets
            .try_reserve(1)
            .map_err(|error| IntegrationError::new("studio", error.to_string()))?;
        self.packets.push(packet);
        Ok(())
    }
}

enum NativeStudioFrames {
    Captured(Vec<fmn_scene::studio_bridge::FramePacket>),
    Compiled(Box<TimelineBundle>),
}

impl NativeStudioFrames {
    fn len(&self) -> usize {
        match self {
            Self::Captured(packets) => packets.len(),
            Self::Compiled(bundle) => usize::try_from(bundle.frame_count()).unwrap_or(usize::MAX),
        }
    }

    fn stage_at(
        &self,
        index: usize,
    ) -> Result<fmn_scene::studio_bridge::Stage, fmn_studio::ServiceError> {
        match self {
            Self::Captured(packets) => packets
                .get(index)
                .map(fmn_scene::studio_bridge::FramePacket::materialize_stage)
                .ok_or_else(|| studio_service_error("preview frame is outside the scene")),
            Self::Compiled(bundle) => {
                let index = u32::try_from(index)
                    .map_err(|_| studio_service_error("preview frame index exceeds u32"))?;
                bundle
                    .stage_at(index)
                    .map_err(|error| studio_service_error(error.to_string()))
            }
        }
    }

    fn clock_frame_at(&self, index: usize) -> Result<i64, fmn_studio::ServiceError> {
        match self {
            Self::Captured(packets) => packets
                .get(index)
                .map(fmn_scene::studio_bridge::FramePacket::frame_index)
                .ok_or_else(|| studio_service_error("preview frame is outside the scene")),
            Self::Compiled(_) => i64::try_from(index)
                .map_err(|_| studio_service_error("preview frame index exceeds i64")),
        }
    }

    fn index_for_clock_frame(&self, frame: i64) -> Option<usize> {
        match self {
            Self::Captured(packets) => packets
                .iter()
                .position(|packet| packet.frame_index() == frame),
            Self::Compiled(_) => usize::try_from(frame)
                .ok()
                .filter(|index| *index < self.len()),
        }
    }
}

#[derive(Clone, Copy)]
struct NativeStudioRenderer {
    frame_config: FrameConfig,
    tiling: Tiling,
    engine: EngineIdentity,
    render_threads: usize,
}

impl NativeStudioRenderer {
    fn render(
        self,
        scene: &str,
        frame_index: usize,
        stage: &fmn_scene::studio_bridge::Stage,
    ) -> Result<fmn_studio::FrameStream, fmn_studio::ServiceError> {
        let (render_plan, mono, binning) = self.prepare(stage, frame_index)?;
        let frame = FrameJob::with_identity(
            &render_plan,
            &mono,
            &binning,
            self.frame_config,
            self.engine,
        )
        .map_err(|error| studio_service_error(error.to_string()))?
        .render(self.render_threads)
        .map_err(|error| studio_service_error(error.to_string()))?;
        let layout = FrameLayout::tight(
            PixelFormat::Rgba8,
            self.frame_config.viewport.width,
            self.frame_config.viewport.height,
        )
        .map_err(|error| studio_service_error(error.to_string()))?;
        let mut rgba = FrameBuffer::new(layout);
        rgba16f_to_rgba8(&frame, &mut rgba)
            .map_err(|error| studio_service_error(error.to_string()))?;
        let png = fmn_codec::encode_rgba8(
            self.frame_config.viewport.width,
            self.frame_config.viewport.height,
            rgba.as_bytes(),
            fmn_codec::CompressionLevel::Fast,
        );
        let digest = fmn_studio::protocol_digest(&png);
        Ok(fmn_studio::FrameStream {
            scene: scene.to_owned(),
            frame_index: u64::try_from(frame_index)
                .map_err(|_| studio_service_error("preview frame index exceeds u64"))?,
            width: self.frame_config.viewport.width,
            height: self.frame_config.viewport.height,
            stride: 0,
            encoding: fmn_studio::FrameEncoding::Png,
            payload: fmn_studio::FramePayload::Pipe { bytes: png, digest },
        })
    }

    fn prepare(
        self,
        stage: &fmn_scene::studio_bridge::Stage,
        revision: usize,
    ) -> Result<(RenderPlan, MonoTable, Binning), fmn_studio::ServiceError> {
        let revision = u64::try_from(revision)
            .map_err(|_| studio_service_error("preview revision exceeds u64"))?;
        let mut render_plan = RenderPlan::new();
        render_plan
            .sync(stage, revision.saturating_add(1))
            .map_err(|error| studio_service_error(error.to_string()))?;
        let mono = MonoTable::build(&render_plan, self.frame_config.map)
            .map_err(|error| studio_service_error(error.to_string()))?;
        let mut binning = Binning::build(
            &render_plan,
            self.frame_config.viewport,
            self.tiling,
            self.frame_config.map,
        )
        .map_err(|error| studio_service_error(error.to_string()))?;
        binning
            .prune_occluded(&render_plan)
            .map_err(|error| studio_service_error(error.to_string()))?;
        Ok((render_plan, mono, binning))
    }

    fn overlay_json(
        self,
        stage: &fmn_scene::studio_bridge::Stage,
        frame_index: usize,
        layers: fmn_studio::DebugLayerSet,
    ) -> Result<Vec<u8>, fmn_studio::ServiceError> {
        let (_, _, binning) = self.prepare(stage, frame_index)?;
        let limits = fmn_studio::InspectorLimits::default();
        fmn_studio::DebugOverlaySnapshot::capture(
            stage,
            Some((&binning, self.frame_config.viewport)),
            layers,
            limits,
        )
        .and_then(|snapshot| snapshot.to_json(limits))
        .map_err(|error| studio_service_error(error.to_string()))
    }
}

struct NativeStudioWorker {
    build_id: fmn_studio::ProtocolDigest,
    scene: String,
    frames: NativeStudioFrames,
    renderer: NativeStudioRenderer,
    current_frame: usize,
    max_frame_bytes: usize,
    fps: u32,
    seed: u64,
    checkpoint_frames: u64,
    source_read: Option<AssetRead>,
    journal_position: usize,
    journal_tail: Vec<u8>,
    last_state_hash: Option<fmn_studio::ProtocolDigest>,
    last_checkpoint_frame: Option<usize>,
}

impl NativeStudioWorker {
    fn from_command(fs: &dyn FileSystem, command: &StudioCommand) -> Result<Self, CliError> {
        let input = resolve_native_render_input(fs, &command.render)?;
        let (scene, frames, config, source_read) = match input {
            NativeRenderInput::Builtin { names } => {
                if names.len() != 1 {
                    return Err(CliError::new(
                        "scene",
                        "Studio requires exactly one scene name",
                    ));
                }
                let scene = names.into_iter().next().ok_or_else(|| {
                    CliError::new("scene", "Studio requires exactly one scene name")
                })?;
                let mut program = fmn::builtins::primitive_scene(&scene).ok_or_else(|| {
                    CliError::new("scene", "validated built-in scene disappeared")
                })?;
                let config = resolve_render_config(fs, &command.render)?;
                let mut capture = StudioPacketCapture::default();
                let completed = fmn::run_scene(
                    &mut program,
                    command.render.runtime_config(&config),
                    config.determinism.seed,
                    &mut capture,
                )
                .map_err(native_scene_error)?;
                if capture.packets.is_empty() {
                    completed
                        .into_scene()
                        .show(&mut capture)
                        .map_err(|error| CliError::new("scene", error.to_string()))?;
                }
                (
                    scene,
                    NativeStudioFrames::Captured(capture.packets),
                    config,
                    None,
                )
            }
            NativeRenderInput::Compiled {
                name,
                bundle,
                source_item,
                ..
            } => {
                let mut compiled_command = command.render.clone();
                if let Some(requested_fps) = compiled_command.fps
                    && requested_fps != bundle.fps()
                {
                    return Err(CliError::new(
                        "config",
                        format!(
                            "--fps {requested_fps} disagrees with the compiled artifact's fixed {} fps schedule",
                            bundle.fps()
                        ),
                    ));
                }
                compiled_command.fps = Some(bundle.fps());
                let config = resolve_render_config(fs, &compiled_command)?;
                let path = source_item
                    .virtual_path
                    .ok_or_else(|| internal("compiled Studio source omitted its closure path"))?;
                (
                    name,
                    NativeStudioFrames::Compiled(bundle),
                    config,
                    Some(AssetRead {
                        path,
                        digest: source_item.digest,
                    }),
                )
            }
        };
        if frames.len() == 0 {
            return Err(CliError::new(
                "scene",
                "the selected Studio scene has no preview frame",
            ));
        }
        // Studio is an interactive preview front door even when ordinary
        // render defaults would otherwise select offline semantics.
        let (plan, _, _) = derive_execution_plan(
            fs,
            &config,
            fmn_runtime::RenderIntent::Preview,
            fmn_runtime::OutputPixelFormat::Rgba8,
        )?;
        let engine = match plan.engine {
            fmn_runtime::ExecutionEngine::CertifiedCpu => EngineIdentity::certified(),
            fmn_runtime::ExecutionEngine::FastCpu => EngineIdentity::fast(),
            fmn_runtime::ExecutionEngine::Metal | fmn_runtime::ExecutionEngine::Cuda => {
                return Err(CliError::new(
                    "capability",
                    "the selected annex engine has no production Studio renderer",
                ));
            }
        };
        Ok(Self {
            build_id: fmn_studio::protocol_digest(BUILD_ID.as_bytes()),
            scene,
            frames,
            renderer: NativeStudioRenderer {
                frame_config: resolved_frame_config(&config)?,
                tiling: Tiling {
                    macro_tile: plan.macro_tile,
                    fine_tile: plan.fine_tile,
                },
                engine,
                render_threads: plan
                    .render_teams
                    .first()
                    .map_or(1, fmn_runtime::TeamPlan::threads),
            },
            current_frame: 0,
            max_frame_bytes: fmn_studio::ProtocolLimits::default().max_frame_bytes,
            fps: config.camera.fps,
            seed: config.determinism.seed,
            checkpoint_frames: command.checkpoint_frames,
            source_read,
            journal_position: 0,
            journal_tail: Vec::new(),
            last_state_hash: None,
            last_checkpoint_frame: None,
        })
    }

    fn require_scene(&self, scene: &str) -> Result<(), fmn_studio::ServiceError> {
        if scene == self.scene {
            Ok(())
        } else {
            Err(fmn_studio::ServiceError::new(
                fmn_studio::WorkerErrorCode::SceneNotFound,
                format!("scene {scene:?} is not registered in this worker"),
            ))
        }
    }

    fn resolve_frame(&self, frame: i64) -> Result<usize, fmn_studio::ServiceError> {
        let frame =
            usize::try_from(frame).map_err(|_| studio_service_error("negative preview frame"))?;
        if frame >= self.frames.len() {
            return Err(studio_service_error(format!(
                "preview frame {frame} is outside 0..{}",
                self.frames.len()
            )));
        }
        Ok(frame)
    }

    fn render_frame(
        &self,
        frame: usize,
    ) -> Result<fmn_studio::WorkerResponse, fmn_studio::ServiceError> {
        let stage = self.frames.stage_at(frame)?;
        let stream = self.renderer.render(&self.scene, frame, &stage)?;
        if let fmn_studio::FramePayload::Pipe { bytes, .. } = &stream.payload
            && bytes.len() > self.max_frame_bytes
        {
            return Err(studio_service_error(format!(
                "preview PNG is {} bytes, over the negotiated {}-byte budget",
                bytes.len(),
                self.max_frame_bytes
            )));
        }
        Ok(fmn_studio::WorkerResponse::Frame(stream))
    }

    fn state_bytes(
        &self,
        frame: usize,
        play_count: u64,
    ) -> Result<Vec<u8>, fmn_studio::ServiceError> {
        let stage = self.frames.stage_at(frame)?;
        let clock_frame = self.frames.clock_frame_at(frame)?;
        let rng = RngRoot::from_seed(self.seed)
            .substream("scene")
            .fork_frame(clock_frame.cast_unsigned());
        SceneState::capture(&stage, clock_frame, self.fps, play_count, &rng)
            .to_bytes()
            .map_err(|error| studio_service_error(error.to_string()))
    }

    fn source_reads(&self) -> Vec<AssetRead> {
        self.source_read.iter().cloned().collect()
    }

    fn record_seek(
        &mut self,
        command: CommandRecord,
    ) -> Result<fmn_studio::WorkerResponse, fmn_studio::ServiceError> {
        let frame = fmn_studio::protocol::studio_seek_frame(&self.scene, &command)
            .map_err(|error| studio_service_error(error.to_string()))?;
        let frame = self.resolve_frame(frame)?;
        let next_position = self
            .journal_position
            .checked_add(1)
            .ok_or_else(|| studio_service_error("Studio journal position exhausted"))?;
        let play_count = u64::try_from(next_position)
            .map_err(|_| studio_service_error("Studio journal position exceeds u64"))?;
        let state = self.state_bytes(frame, play_count)?;
        let state_hash = fmn_studio::protocol_digest(&state);
        let checkpoint = self
            .last_checkpoint_frame
            .is_none_or(|last| {
                u64::try_from(last.abs_diff(frame))
                    .is_ok_and(|distance| distance >= self.checkpoint_frames)
            })
            .then_some(state);
        let entry = Entry {
            command,
            effect: EffectClass::Pure,
            reads: self.source_reads(),
            subprocesses: Vec::new(),
            checkpoint,
            state_hash,
        };
        let mut segment = Journal::new();
        segment
            .record(
                entry
                    .try_clone()
                    .map_err(|error| studio_service_error(error.to_string()))?,
            )
            .map_err(|error| studio_service_error(error.to_string()))?;
        let journal = segment
            .to_bytes()
            .map_err(|error| studio_service_error(error.to_string()))?;
        let start_entry = u64::try_from(self.journal_position)
            .map_err(|_| studio_service_error("Studio journal position exceeds u64"))?;
        self.current_frame = frame;
        self.journal_position = next_position;
        self.journal_tail = journal.clone();
        self.last_state_hash = Some(state_hash);
        if entry.checkpoint.is_some() {
            self.last_checkpoint_frame = Some(frame);
        }
        Ok(fmn_studio::WorkerResponse::JournalSegment {
            scene: self.scene.clone(),
            start_entry,
            journal,
        })
    }

    fn replay(
        &mut self,
        replay: fmn_studio::JournalReplay,
    ) -> Result<fmn_studio::WorkerResponse, fmn_studio::ServiceError> {
        self.require_scene(&replay.scene)?;
        let journal = Journal::from_bytes(&replay.journal)
            .map_err(|error| studio_service_error(error.to_string()))?;
        let from = usize::try_from(replay.from_entry)
            .map_err(|_| studio_service_error("replay start exceeds usize"))?;
        let through = usize::try_from(replay.through_entry)
            .map_err(|_| studio_service_error("replay end exceeds usize"))?;
        let mut state_hashes = Vec::new();
        state_hashes
            .try_reserve_exact(through.saturating_sub(from))
            .map_err(|error| studio_service_error(error.to_string()))?;
        for (index, entry) in journal.entries()[from..through].iter().enumerate() {
            // ubs:ignore - source records are public replay identities, not secrets.
            if entry.reads.as_slice() != self.source_read.as_slice() {
                return Err(fmn_studio::ServiceError::new(
                    fmn_studio::WorkerErrorCode::ReplayFailed,
                    "the replay journal references a different compiled source",
                ));
            }
            let frame = fmn_studio::protocol::studio_seek_frame(&self.scene, &entry.command)
                .map_err(|error| studio_service_error(error.to_string()))?;
            let frame = self.resolve_frame(frame)?;
            let position = from
                .checked_add(index)
                .and_then(|value| value.checked_add(1))
                .ok_or_else(|| studio_service_error("replay position exhausted"))?;
            let play_count = u64::try_from(position)
                .map_err(|_| studio_service_error("replay position exceeds u64"))?;
            let hash = fmn_studio::protocol_digest(&self.state_bytes(frame, play_count)?);
            state_hashes.push(hash);
            self.current_frame = frame;
            self.last_state_hash = Some(hash);
        }
        self.journal_position = through;
        self.last_checkpoint_frame = journal.entries()[..through]
            .iter()
            .rposition(|entry| entry.checkpoint.is_some())
            .and_then(|index| {
                fmn_studio::protocol::studio_seek_frame(
                    &self.scene,
                    &journal.entries()[index].command,
                )
                .ok()
            })
            .and_then(|frame| usize::try_from(frame).ok());
        self.journal_tail = replay.journal;
        Ok(fmn_studio::WorkerResponse::ReplayComplete {
            from_entry: replay.from_entry,
            state_hashes,
        })
    }

    fn restore_checkpoint(
        &mut self,
        checkpoint: fmn_studio::Checkpoint,
    ) -> Result<fmn_studio::WorkerResponse, fmn_studio::ServiceError> {
        self.require_scene(&checkpoint.scene)?;
        let stage = self.frames.stage_at(0)?;
        let decoded = SceneState::from_bytes(&checkpoint.state, &stage)
            .map_err(|error| studio_service_error(error.to_string()))?;
        let frame = self
            .frames
            .index_for_clock_frame(decoded.frames_elapsed)
            .ok_or_else(|| studio_service_error("checkpoint frame is outside the scene"))?;
        let journal_position = usize::try_from(checkpoint.after_entry)
            .ok()
            .and_then(|position| position.checked_add(1))
            .ok_or_else(|| studio_service_error("checkpoint journal position exceeds usize"))?;
        let play_count = u64::try_from(journal_position)
            .map_err(|_| studio_service_error("checkpoint journal position exceeds u64"))?;
        let expected = fmn_studio::protocol_digest(&self.state_bytes(frame, play_count)?);
        // ubs:ignore - the state digest is a public replay-integrity identifier.
        if expected != checkpoint.state_hash {
            return Err(fmn_studio::ServiceError::new(
                fmn_studio::WorkerErrorCode::CheckpointRejected,
                "checkpoint state does not match the selected preview frame",
            ));
        }
        self.current_frame = frame;
        self.journal_position = journal_position;
        self.last_state_hash = Some(expected);
        self.last_checkpoint_frame = Some(frame);
        Ok(fmn_studio::WorkerResponse::Ack {
            state_hash: Some(expected),
            journal_len: play_count,
        })
    }

    fn studio_data(
        &self,
        kind: fmn_studio::StudioDataKind,
        bytes: Vec<u8>,
    ) -> fmn_studio::WorkerResponse {
        let digest = fmn_studio::protocol_digest(&bytes);
        fmn_studio::WorkerResponse::StudioData {
            scene: self.scene.clone(),
            kind,
            bytes,
            digest,
        }
    }
}

impl fmn_studio::WorkerService for NativeStudioWorker {
    fn build_id(&self) -> fmn_studio::ProtocolDigest {
        self.build_id
    }

    fn begin_session(
        &mut self,
        _supervisor_build: fmn_studio::ProtocolDigest,
        max_frame_bytes: usize,
    ) -> Result<(), fmn_studio::ServiceError> {
        self.max_frame_bytes = max_frame_bytes;
        Ok(())
    }

    fn handle(
        &mut self,
        request: fmn_studio::SupervisorRequest,
    ) -> Result<fmn_studio::WorkerResponse, fmn_studio::ServiceError> {
        match request {
            fmn_studio::SupervisorRequest::EnumerateScenes => {
                Ok(fmn_studio::WorkerResponse::Scenes(vec![self.scene.clone()]))
            }
            fmn_studio::SupervisorRequest::Play { scene, command } => {
                self.require_scene(&scene)?;
                self.record_seek(command)
            }
            fmn_studio::SupervisorRequest::Seek { scene, frame }
            | fmn_studio::SupervisorRequest::Scrub { scene, frame } => {
                self.require_scene(&scene)?;
                let frame = self.resolve_frame(frame)?;
                self.current_frame = frame;
                self.render_frame(frame)
            }
            fmn_studio::SupervisorRequest::Event { scene, .. } => {
                self.require_scene(&scene)?;
                Err(fmn_studio::ServiceError::new(
                    fmn_studio::WorkerErrorCode::InvalidRequest,
                    "the selected native preview artifact has no live command/event adapter",
                ))
            }
            fmn_studio::SupervisorRequest::Inspect { scene } => {
                self.require_scene(&scene)?;
                let stage = self.frames.stage_at(self.current_frame)?;
                let limits = fmn_studio::InspectorLimits::default();
                let bytes = fmn_studio::InspectorSnapshot::capture(
                    &stage,
                    &fmn_studio::SpanRegistry::new(),
                    limits,
                )
                .and_then(|snapshot| snapshot.to_json(limits))
                .map_err(|error| studio_service_error(error.to_string()))?;
                Ok(self.studio_data(fmn_studio::StudioDataKind::Inspection, bytes))
            }
            fmn_studio::SupervisorRequest::Overlay { scene, layers } => {
                self.require_scene(&scene)?;
                let stage = self.frames.stage_at(self.current_frame)?;
                let bytes = self
                    .renderer
                    .overlay_json(&stage, self.current_frame, layers)?;
                Ok(self.studio_data(fmn_studio::StudioDataKind::Overlay, bytes))
            }
            fmn_studio::SupervisorRequest::ReplayJournal(replay) => self.replay(replay),
            fmn_studio::SupervisorRequest::RestoreCheckpoint(checkpoint) => {
                self.restore_checkpoint(checkpoint)
            }
            fmn_studio::SupervisorRequest::Hello { .. }
            | fmn_studio::SupervisorRequest::Shutdown => Err(fmn_studio::ServiceError::new(
                fmn_studio::WorkerErrorCode::InvalidRequest,
                "the protocol driver owns handshake and shutdown requests",
            )),
        }
    }

    fn active_scene(&self) -> Option<&str> {
        Some(&self.scene)
    }

    fn journal_tail(&self) -> &[u8] {
        &self.journal_tail
    }

    fn last_state_hash(&self) -> Option<fmn_studio::ProtocolDigest> {
        self.last_state_hash
    }
}

fn studio_service_error(message: impl Into<String>) -> fmn_studio::ServiceError {
    fmn_studio::ServiceError::new(fmn_studio::WorkerErrorCode::ExecutionFailed, message)
}

struct RenderSink {
    frame_config: FrameConfig,
    tiling: Tiling,
    engine: EngineIdentity,
    render_threads: usize,
    format: PixelFormat,
    emitter: Option<OrderedEmitter>,
    receipt: RenderReceipt,
    rgba8_scratch: Option<FrameBuffer>,
    next_sequence: u64,
}

fn resolved_frame_config(config: &fmn_config::Config) -> Result<FrameConfig, CliError> {
    let (width, height) = config.camera.resolution;
    let background = Srgb::from_hex(&config.camera.background_color)
        .map_err(|error| CliError::new("config", error.to_string()))?
        .to_linear(config.camera.background_opacity);
    Ok(FrameConfig::new(
        Viewport { width, height },
        ScreenMap {
            scale: f64::from(height) / config.sizes.frame_height,
            origin: [f64::from(width) / 2.0, f64::from(height) / 2.0],
        },
        background,
    )
    .with_aa_policy(config.render.aa))
}

impl RenderSink {
    fn emitter_handle(&self) -> Option<EmitterHandle> {
        self.emitter.as_ref().map(OrderedEmitter::handle)
    }

    fn new(
        fs: Arc<dyn FileSystem>,
        config: &fmn_config::Config,
        plan: &fmn_runtime::ExecutionPlan,
        target: &RenderTarget,
        destination: PathBuf,
    ) -> Result<Self, CliError> {
        let (width, height) = config.camera.resolution;
        let format = target.pixel_format();
        let output_layout = FrameLayout::tight(format, width, height)
            .map_err(|error| CliError::new("config", error.to_string()))?;
        let limits = render_sink_limits(&output_layout)?;
        let (binding, receipt) = match target {
            RenderTarget::Native(NativeFrameFormat::Png | NativeFrameFormat::PngSequence) => {
                let single = matches!(target, RenderTarget::Native(NativeFrameFormat::Png));
                let png_target = if single {
                    PngTarget::Single(destination)
                } else {
                    PngTarget::Sequence {
                        directory: destination,
                        stem: "frame".to_owned(),
                        digits: 6,
                    }
                };
                let (binding, receipt) = PngSink::new(
                    fs,
                    PngSinkConfig {
                        target: png_target,
                        width,
                        height,
                        first_sequence: 0,
                        compression: if config.determinism.mode
                            == fmn_config::config::DeterminismMode::Certified
                        {
                            fmn_codec::CompressionLevel::Best
                        } else {
                            fmn_codec::CompressionLevel::Default
                        },
                        threads: plan.output_team.threads().max(1),
                        limits,
                        profile: None,
                    },
                )
                .map_err(output_adapter_error)?
                .into_binding(if single { "png" } else { "png-sequence" });
                (binding, RenderReceipt::Native(receipt))
            }
            RenderTarget::Native(NativeFrameFormat::Gif) => {
                let (binding, receipt) = GifSink::new(
                    fs,
                    GifSinkConfig {
                        destination,
                        width,
                        height,
                        fps: (config.camera.fps, 1),
                        loop_forever: true,
                        first_sequence: 0,
                        limits,
                        profile: None,
                    },
                )
                .map_err(output_adapter_error)?
                .into_binding("gif");
                (binding, RenderReceipt::Native(receipt))
            }
            RenderTarget::Native(NativeFrameFormat::Y4m) => {
                let (binding, receipt) = Y4mSink::new(
                    fs,
                    Y4mSinkConfig {
                        destination,
                        width,
                        height,
                        fps: (config.camera.fps, 1),
                        colorspace: fmn_codec::Y4mColorspace::C420Mpeg2,
                        first_sequence: 0,
                        limits,
                        profile: None,
                    },
                )
                .map_err(output_adapter_error)?
                .into_binding("y4m");
                (binding, RenderReceipt::Native(receipt))
            }
            RenderTarget::Video(context) => {
                let (binding, receipt) = FfmpegSink::new(
                    Arc::clone(&context.runner),
                    FfmpegSinkConfig {
                        tool: context.tool.clone(),
                        capabilities: context.capabilities.clone(),
                        job: context.job.clone(),
                        audio: None,
                        destination,
                        workdir_root: context.workdir_root.clone(),
                        job_limits: JobLimits::default(),
                        first_sequence: 0,
                        limits,
                        profile: None,
                    },
                )
                .map_err(output_adapter_error)?
                .into_binding("ffmpeg-video");
                (binding, RenderReceipt::Video(receipt))
            }
        };
        let emitter_config = EmitterConfig::new(output_layout, plan.frames_in_flight, 0)
            .map_err(|error| CliError::new("render", error.to_string()))?;
        let emitter = OrderedEmitter::new(emitter_config, vec![binding])
            .map_err(|error| CliError::new("render", error.to_string()))?;
        let frame_config = resolved_frame_config(config)?;
        let engine = match plan.engine {
            fmn_runtime::ExecutionEngine::CertifiedCpu => EngineIdentity::certified(),
            fmn_runtime::ExecutionEngine::FastCpu => EngineIdentity::fast(),
            fmn_runtime::ExecutionEngine::Metal | fmn_runtime::ExecutionEngine::Cuda => {
                return Err(CliError::new(
                    "capability",
                    "the selected annex engine has no production CLI renderer",
                ));
            }
        };
        let rgba8_scratch = (format != PixelFormat::Rgba8)
            .then(|| FrameLayout::tight(PixelFormat::Rgba8, width, height))
            .transpose()
            .map_err(|error| CliError::new("config", error.to_string()))?
            .map(FrameBuffer::new);
        Ok(Self {
            frame_config,
            tiling: Tiling {
                macro_tile: plan.macro_tile,
                fine_tile: plan.fine_tile,
            },
            engine,
            render_threads: plan
                .render_teams
                .first()
                .map_or(1, fmn_runtime::TeamPlan::threads),
            format,
            emitter: Some(emitter),
            receipt,
            rgba8_scratch,
            next_sequence: 0,
        })
    }

    fn render_stage(
        &mut self,
        stage: &fmn_scene::studio_bridge::Stage,
        revision: u64,
    ) -> Result<(), IntegrationError> {
        let mut render_plan = RenderPlan::new();
        render_plan
            .sync(stage, revision)
            .map_err(|error| IntegrationError::new("lumen", error.to_string()))?;
        let mono = MonoTable::build(&render_plan, self.frame_config.map)
            .map_err(|error| IntegrationError::new("lumen", error.to_string()))?;
        let mut binning = Binning::build(
            &render_plan,
            self.frame_config.viewport,
            self.tiling,
            self.frame_config.map,
        )
        .map_err(|error| IntegrationError::new("lumen", error.to_string()))?;
        binning
            .prune_occluded(&render_plan)
            .map_err(|error| IntegrationError::new("lumen", error.to_string()))?;
        let frame = FrameJob::with_identity(
            &render_plan,
            &mono,
            &binning,
            self.frame_config,
            self.engine,
        )
        .map_err(|error| IntegrationError::new("lumen", error.to_string()))?
        .render(self.render_threads)
        .map_err(|error| IntegrationError::new("lumen", error.to_string()))?;

        let emitter = self
            .emitter
            .as_ref()
            .ok_or_else(|| IntegrationError::new("reel", "emitter was already finalized"))?;
        let mut reservation = emitter
            .reserve(self.next_sequence)
            .map_err(|error| IntegrationError::new("reel", error.to_string()))?;
        match self.format {
            PixelFormat::Rgba8 => {
                rgba16f_to_rgba8(&frame, reservation.frame_mut())
                    .map_err(|error| IntegrationError::new("reel", error.to_string()))?;
            }
            PixelFormat::Bgra8 => {
                let rgba8 = self.rgba8_scratch.as_mut().ok_or_else(|| {
                    IntegrationError::new("reel", "BGRA conversion scratch is unavailable")
                })?;
                rgba16f_to_rgba8(&frame, rgba8)
                    .map_err(|error| IntegrationError::new("reel", error.to_string()))?;
                swap_rb8(rgba8, reservation.frame_mut())
                    .map_err(|error| IntegrationError::new("reel", error.to_string()))?;
            }
            PixelFormat::Nv12 => {
                let rgba8 = self.rgba8_scratch.as_mut().ok_or_else(|| {
                    IntegrationError::new("reel", "NV12 conversion scratch is unavailable")
                })?;
                rgba16f_to_rgba8(&frame, rgba8)
                    .map_err(|error| IntegrationError::new("reel", error.to_string()))?;
                rgba_to_nv12(
                    rgba8,
                    reservation.frame_mut(),
                    ColorRange::Limited,
                    ChromaSiting::Left,
                )
                .map_err(|error| IntegrationError::new("reel", error.to_string()))?;
            }
            PixelFormat::P010 => {
                let rgba8 = self.rgba8_scratch.as_mut().ok_or_else(|| {
                    IntegrationError::new("reel", "P010 conversion scratch is unavailable")
                })?;
                rgba16f_to_rgba8(&frame, rgba8)
                    .map_err(|error| IntegrationError::new("reel", error.to_string()))?;
                rgba_to_p010(
                    rgba8,
                    reservation.frame_mut(),
                    ColorRange::Limited,
                    ChromaSiting::Left,
                )
                .map_err(|error| IntegrationError::new("reel", error.to_string()))?;
            }
            PixelFormat::Rgba16F => {
                return Err(IntegrationError::new(
                    "reel",
                    "RGBA16F is a renderer intermediate, not a CLI sink format",
                ));
            }
        }
        reservation
            .publish()
            .map_err(|error| IntegrationError::new("reel", error.to_string()))?;
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or_else(|| IntegrationError::new("reel", "frame sequence exhausted"))?;
        Ok(())
    }

    fn finish(mut self) -> Result<RenderArtifactReport, CliError> {
        let emitter = self
            .emitter
            .take()
            .ok_or_else(|| CliError::new("internal", "render emitter was already finalized"))?;
        emitter
            .finish()
            .map_err(|error| CliError::new("render", error.to_string()))?;
        match self.receipt {
            RenderReceipt::Native(receipt) => receipt
                .take()
                .map(RenderArtifactReport::Native)
                .map_err(|error| CliError::new("render", error.to_string())),
            RenderReceipt::Video(receipt) => {
                let report = receipt
                    .take()
                    .map_err(|error| CliError::new("render", error.to_string()))?;
                let invocation = report.boundary.invocations.first().ok_or_else(|| {
                    CliError::new(
                        "internal",
                        "ffmpeg publication omitted invocation provenance",
                    )
                })?;
                Ok(RenderArtifactReport::Video(VideoArtifactReport {
                    path: report.boundary.destination.clone(),
                    frame_count: report.frame_count,
                    input_bytes: report.input_bytes,
                    artifact_bytes: report.boundary.artifact_bytes,
                    artifact_digest: report.boundary.artifact_digest,
                    tool_path: invocation.provenance.tool_path.clone(),
                    tool_sha256: invocation.provenance.tool_sha256_hex.clone(),
                    tool_version: invocation.provenance.tool_version.clone(),
                    native_image_format: native_image_format(
                        invocation.provenance.native_image.format,
                    ),
                    native_image_architecture: native_image_architecture(
                        invocation.provenance.native_image.architecture,
                    ),
                    native_image_bytes: invocation.provenance.native_image.file_bytes,
                    native_image_policy_version: invocation.provenance.native_image.policy_version,
                    encoder: invocation.provenance.encoder.clone(),
                    process_mechanism: invocation.provenance.process_mechanism.clone(),
                    process_policy_version: invocation.provenance.process_policy_version,
                    argv: invocation.provenance.argv.clone(),
                }))
            }
        }
    }
}

impl SceneSink for RenderSink {
    fn capture(
        &mut self,
        _reason: CaptureReason,
        packet: fmn::animation::FramePacket,
    ) -> Result<(), IntegrationError> {
        let stage = packet.materialize_stage();
        let revision = u64::try_from(packet.frame_index())
            .map_err(|_| IntegrationError::new("lumen", "negative frame index"))?;
        self.render_stage(&stage, revision)
    }
}

fn render_sink_limits(layout: &FrameLayout) -> Result<SinkLimits, CliError> {
    const MAX_STREAM_BYTES: u64 = 64 * 1024 * 1024 * 1024;
    const MAX_FRAMES: u64 = 1_000_000;
    let frame_bytes = u64::try_from(layout.total_bytes()).map_err(|_| {
        CliError::new(
            "budget",
            "one output frame exceeds the target address space",
        )
    })?;
    let max_resident_bytes = frame_bytes
        .checked_mul(4)
        .and_then(|bytes| bytes.checked_add(64 * 1024 * 1024))
        .ok_or_else(|| CliError::new("budget", "output resident-byte budget overflowed"))?;
    SinkLimits::new(
        MAX_FRAMES,
        max_resident_bytes,
        MAX_STREAM_BYTES,
        MAX_STREAM_BYTES,
    )
    .map_err(output_adapter_error)
}

fn output_adapter_error(error: fmn_output::SinkAdapterError) -> CliError {
    let exit_name = match error {
        fmn_output::SinkAdapterError::FrameLimitExceeded { .. }
        | fmn_output::SinkAdapterError::ResidentBytesExceeded { .. }
        | fmn_output::SinkAdapterError::StreamBytesExceeded { .. }
        | fmn_output::SinkAdapterError::ArtifactBytesExceeded { .. } => "budget",
        fmn_output::SinkAdapterError::InvalidConfig(_)
        | fmn_output::SinkAdapterError::InvalidGeometry { .. }
        | fmn_output::SinkAdapterError::FrameMismatch { .. } => "config",
        _ => "render",
    };
    CliError::new(exit_name, error.to_string())
}

fn native_scene_error(error: fmn::Error) -> CliError {
    CliError::new(error.kind().name(), error.to_string())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RequestedRenderFormat {
    Native(NativeFrameFormat),
    Video,
}

fn requested_render_format(command: &RenderCommand) -> Result<RequestedRenderFormat, CliError> {
    match command.format {
        OutputFormat::Png => Ok(RequestedRenderFormat::Native(NativeFrameFormat::Png)),
        OutputFormat::PngSequence => Ok(RequestedRenderFormat::Native(
            NativeFrameFormat::PngSequence,
        )),
        OutputFormat::Gif if command.common.reproducible => Err(CliError::new(
            "capability",
            "native GIF is outside the certified artifact set; use --format png or --format png_sequence with --reproducible",
        )),
        OutputFormat::Gif => Ok(RequestedRenderFormat::Native(NativeFrameFormat::Gif)),
        OutputFormat::Y4m => Ok(RequestedRenderFormat::Native(NativeFrameFormat::Y4m)),
        OutputFormat::Video => Ok(RequestedRenderFormat::Video),
        OutputFormat::Auto if command.vcodec.is_some() || command.pix_fmt.is_some() => {
            Ok(RequestedRenderFormat::Video)
        }
        OutputFormat::Auto => Err(CliError::new(
            "capability",
            "native registered scenes require an explicit `--format`; no format is substituted silently",
        )),
        OutputFormat::Wav => Err(CliError::new(
            "capability",
            "WAV publication requires a scene sound-cue composition path, which is not registered yet",
        )),
    }
}

fn native_image_format(format: fmn_platform::process::NativeExecutableFormat) -> &'static str {
    match format {
        fmn_platform::process::NativeExecutableFormat::Elf64 => "elf64",
        fmn_platform::process::NativeExecutableFormat::MachO64 => "mach-o64",
        fmn_platform::process::NativeExecutableFormat::MachOUniversal => "mach-o-universal",
        fmn_platform::process::NativeExecutableFormat::Pe32Plus => "pe32+",
    }
}

fn native_image_architecture(
    architecture: fmn_platform::process::NativeExecutableArchitecture,
) -> &'static str {
    match architecture {
        fmn_platform::process::NativeExecutableArchitecture::X86_64 => "x86_64",
        fmn_platform::process::NativeExecutableArchitecture::Aarch64 => "aarch64",
    }
}

fn pinned_toolchain() -> Result<&'static str, CliError> {
    let mut toolchain = false;
    for line in SUITE_LOCK_TEXT.lines() {
        if line.starts_with('[') {
            toolchain = line == "[toolchain]";
            continue;
        }
        if toolchain {
            let mut fields = line.split('\t');
            if fields.next() == Some("rustc")
                && let Some(value) = fields.next()
                && !value.is_empty()
            {
                return Ok(value);
            }
        }
    }
    Err(internal("embedded SUITE.lock has no pinned rustc identity"))
}

fn active_target_features() -> &'static str {
    match active_compiled_tier() {
        "x86-64-v3" => "+avx2,+bmi2,+fma",
        "x86-64-v4" => "+avx512f,+avx512bw,+avx512dq,+avx512vl",
        "aarch64+neon" => "+neon",
        _ => "baseline",
    }
}

const fn plan_determinism_name(determinism: fmn_runtime::Determinism) -> &'static str {
    match determinism {
        fmn_runtime::Determinism::Standard => "standard",
        fmn_runtime::Determinism::Certified => "certified",
    }
}

const fn plan_engine_name(engine: fmn_runtime::ExecutionEngine) -> &'static str {
    match engine {
        fmn_runtime::ExecutionEngine::CertifiedCpu => "certified-cpu",
        fmn_runtime::ExecutionEngine::FastCpu => "fast-cpu",
        fmn_runtime::ExecutionEngine::Metal => "metal",
        fmn_runtime::ExecutionEngine::Cuda => "cuda",
    }
}

const fn plan_intent_name(intent: fmn_runtime::RenderIntent) -> &'static str {
    match intent {
        fmn_runtime::RenderIntent::Preview => "preview",
        fmn_runtime::RenderIntent::Offline => "offline",
    }
}

const fn plan_output_name(format: fmn_runtime::OutputPixelFormat) -> &'static str {
    match format {
        fmn_runtime::OutputPixelFormat::Rgba16F => "rgba16f",
        fmn_runtime::OutputPixelFormat::Rgba8 => "rgba8",
        fmn_runtime::OutputPixelFormat::Bgra8 => "bgra8",
        fmn_runtime::OutputPixelFormat::Nv12 => "nv12",
        fmn_runtime::OutputPixelFormat::P010 => "p010",
    }
}

const fn plan_tuning_name(source: fmn_runtime::TuningSource) -> &'static str {
    match source {
        fmn_runtime::TuningSource::CertifiedProfile => "certified-profile",
        fmn_runtime::TuningSource::StandardBaseline => "standard-baseline",
        fmn_runtime::TuningSource::StandardAutotuneCache => "standard-autotune-cache",
    }
}

fn builtin_source_item(scene: &str) -> Result<ClosureItem, CliError> {
    let bytes = format!("fmn-native-registration/v1\n{scene}\n");
    ClosureItem::byte_input(
        1,
        format!("{BUILTIN_SCENE_SOURCE}/{scene}"),
        bytes.as_bytes(),
        "compiled native primitive registration",
    )
    .map_err(|error| internal(error.to_string()))
}

fn output_manifest_entry(mode: ManifestMode, artifact: &RenderArtifactReport) -> ManifestOutput {
    match artifact {
        RenderArtifactReport::Native(report) => ManifestOutput {
            virtual_path: report.path.to_string_lossy().into_owned(),
            kind: native_artifact_kind_name(report.kind).to_owned(),
            digest: report.digest,
            certified: mode == ManifestMode::Certified
                && matches!(
                    report.kind,
                    fmn_output::NativeArtifactKind::Png
                        | fmn_output::NativeArtifactKind::PngSequence
                ),
        },
        RenderArtifactReport::Video(report) => ManifestOutput {
            virtual_path: report.path.to_string_lossy().into_owned(),
            kind: "encoded_video".to_owned(),
            digest: report.artifact_digest,
            certified: false,
        },
    }
}

const fn native_artifact_kind_name(kind: fmn_output::NativeArtifactKind) -> &'static str {
    match kind {
        fmn_output::NativeArtifactKind::Png => "canonical_png",
        fmn_output::NativeArtifactKind::PngSequence => "canonical_png_sequence",
        fmn_output::NativeArtifactKind::Gif => "gif",
        fmn_output::NativeArtifactKind::Y4m => "y4m",
    }
}

fn manifest_artifact_kind(artifact: &RenderArtifactReport) -> &'static str {
    match artifact {
        RenderArtifactReport::Native(report) => native_artifact_kind_name(report.kind),
        RenderArtifactReport::Video(_) => "encoded_video",
    }
}

struct ManifestContext<'a> {
    fs: &'a dyn FileSystem,
    process_mechanism: fmn_platform::process::ProcessMechanism,
    command: &'a RenderCommand,
    config: &'a fmn_config::Config,
    plan: &'a fmn_runtime::ExecutionPlan,
    engine: EngineIdentity,
}

fn render_manifest(
    context: ManifestContext<'_>,
    source_item: ClosureItem,
    artifact: &RenderArtifactReport,
) -> Result<ProvenanceManifest, CliError> {
    let ManifestContext {
        fs,
        process_mechanism,
        command,
        config,
        plan,
        engine,
    } = context;
    if fs.identity() == "opaque.file_system/v1" {
        return Err(CliError::new(
            "capability",
            "FMNP publication requires a stable versioned FileSystem identity",
        ));
    }
    let mode = match config.determinism.mode {
        fmn_config::config::DeterminismMode::Standard => ManifestMode::Standard,
        fmn_config::config::DeterminismMode::Certified => ManifestMode::Certified,
    };
    let config_bytes = config
        .canonical_bytes()
        .map_err(|error| internal(format!("canonical resolved config: {error}")))?;
    let frame_config = resolved_frame_config(config)?;
    let tiling = Tiling {
        macro_tile: plan.macro_tile,
        fine_tile: plan.fine_tile,
    };
    let renderer_document = fmn_render::engine::journal(engine, &frame_config, tiling);
    let runtime = command.runtime_config(config);

    let build_item = ClosureItem::byte_input(
        2,
        "franken_manim.build",
        BUILD_ID.as_bytes(),
        "franken_manim git commit or release build id",
    )
    .map_err(|error| internal(error.to_string()))?;
    let suite_item = ClosureItem::byte_input(
        2,
        "SUITE.lock",
        SUITE_LOCK_BYTES,
        "complete governed dependency and toolchain lock",
    )
    .map_err(|error| internal(error.to_string()))?;
    let toolchain = pinned_toolchain()?;
    let c3 = ClosureItem::structural(
        3,
        "native Rust toolchain and target; Python portal absent",
        &[
            StructuralField::Text(toolchain),
            StructuralField::Text(TARGET_TRIPLE),
            StructuralField::Text(active_target_features()),
            StructuralField::Text(CARGO_PROFILE),
            StructuralField::Absent("CPython portal"),
            StructuralField::Absent("fmn-python wheel"),
            StructuralField::Absent("NumPy runtime"),
        ],
    )
    .map_err(|error| internal(error.to_string()))?;
    let c4 = ClosureItem::byte_input(
        4,
        "resolved-config.fmnf",
        &config_bytes,
        "fully resolved configuration after defaults, files, and CLI overlay",
    )
    .map_err(|error| internal(error.to_string()))?;
    let c5 = ClosureItem::structural(
        5,
        "PCG64DXSM root seed and named-substream layout",
        &[
            StructuralField::U64(config.determinism.seed),
            StructuralField::U64(u64::from(RNG_LAYOUT_VERSION)),
        ],
    )
    .map_err(|error| internal(error.to_string()))?;
    let c6 = ClosureItem::structural(
        6,
        "no asset or font reads on the native primitive/FMTL route",
        &[
            StructuralField::Absent("asset reads"),
            StructuralField::Absent("font reads"),
        ],
    )
    .map_err(|error| internal(error.to_string()))?;
    let engine_identity = engine.closure_string();
    let c7 = ClosureItem::structural(
        7,
        "semantic renderer and execution backend",
        &[
            StructuralField::Text(&engine_identity),
            StructuralField::Bytes(&renderer_document),
        ],
    )
    .map_err(|error| internal(error.to_string()))?;
    let c8 = ClosureItem::structural(
        8,
        "engine-visible locale and timezone are fixed by owned parsers and rational time",
        &[StructuralField::Text("C"), StructuralField::Text("UTC")],
    )
    .map_err(|error| internal(error.to_string()))?;
    let c9 = match artifact {
        RenderArtifactReport::Native(_) => ClosureItem::structural(
            9,
            "native capability policy; no external tool invoked",
            &[
                StructuralField::Text(fs.identity()),
                StructuralField::Text(process_mechanism.identity()),
                StructuralField::U64(u64::from(process_mechanism.policy_version())),
                StructuralField::Absent("host clock on render path"),
                StructuralField::Absent("AssetFetcher"),
                StructuralField::Absent("CPython portal"),
                StructuralField::Absent("ffmpeg invocation"),
            ],
        ),
        RenderArtifactReport::Video(report) => {
            let tool_path = report.tool_path.to_string_lossy();
            ClosureItem::structural(
                9,
                "native capabilities plus audited ffmpeg boundary",
                &[
                    StructuralField::Text(fs.identity()),
                    StructuralField::Text(&report.process_mechanism),
                    StructuralField::U64(u64::from(report.process_policy_version)),
                    StructuralField::Absent("host clock on render path"),
                    StructuralField::Absent("AssetFetcher"),
                    StructuralField::Absent("CPython portal"),
                    StructuralField::Text(tool_path.as_ref()),
                    StructuralField::Text(&report.tool_sha256),
                    StructuralField::Text(&report.tool_version),
                    StructuralField::Text(report.native_image_format),
                    StructuralField::Text(report.native_image_architecture),
                    StructuralField::U64(report.native_image_bytes),
                    StructuralField::U64(u64::from(report.native_image_policy_version)),
                ],
            )
        }
    }
    .map_err(|error| internal(error.to_string()))?;
    let c10 = ClosureItem::structural(
        10,
        "determinism mode and declared execution configuration",
        &[
            StructuralField::Text(mode.name()),
            StructuralField::Text(plan_determinism_name(plan.determinism)),
            StructuralField::Text(plan_engine_name(plan.engine)),
            StructuralField::Text(plan_intent_name(plan.intent)),
            StructuralField::Text(plan_output_name(plan.output_format)),
            StructuralField::Text(manifest_artifact_kind(artifact)),
            StructuralField::Text(plan_tuning_name(plan.tuning_source)),
            StructuralField::U64(u64::try_from(plan.frames_in_flight).unwrap_or(u64::MAX)),
            StructuralField::U64(u64::from(plan.fine_tile)),
            StructuralField::U64(u64::from(plan.macro_tile)),
            StructuralField::Bool(runtime.windowed),
            StructuralField::Bool(runtime.skip_animations),
            runtime.start_at_play.map_or(
                StructuralField::Absent("start_at_play"),
                StructuralField::U64,
            ),
            runtime
                .end_at_play
                .map_or(StructuralField::Absent("end_at_play"), StructuralField::U64),
            StructuralField::Bool(runtime.presenter_mode),
            StructuralField::Bytes(&renderer_document),
        ],
    )
    .map_err(|error| internal(error.to_string()))?;
    let identity = ManifestIdentity {
        build_id: BUILD_ID.to_owned(),
        suite_lock_digest: suite_item.digest,
        toolchain: toolchain.to_owned(),
        target_triple: TARGET_TRIPLE.to_owned(),
        target_features: active_target_features().to_owned(),
        engine: engine_identity,
        simd_tier: active_compiled_tier().to_owned(),
        declared_config_digest: c10.digest,
    };
    ProvenanceManifest::new(
        mode,
        vec![
            source_item,
            build_item,
            suite_item,
            c3,
            c4,
            c5,
            c6,
            c7,
            c8,
            c9,
            c10,
        ],
        identity,
        vec![output_manifest_entry(mode, artifact)],
        None,
    )
    .map_err(|error| internal(error.to_string()))
}

fn ffmpeg_wire_format(
    command: &RenderCommand,
    config: &fmn_config::Config,
) -> Result<WireFormat, CliError> {
    if command.transparent && command.pix_fmt.is_none() {
        return Ok(WireFormat::Rgba8);
    }
    match config
        .file_writer
        .pixel_format
        .to_ascii_lowercase()
        .as_str()
    {
        "rgba" | "rgba8" => Ok(WireFormat::Rgba8),
        "bgra" | "bgra8" => Ok(WireFormat::Bgra8),
        "nv12" | "yuv420p" => Ok(WireFormat::Nv12),
        "p010" | "p010le" | "yuv420p10le" => Ok(WireFormat::P010),
        other => Err(CliError::new(
            "config",
            format!(
                "ffmpeg wire pixel format {other:?} is unsupported; choose rgba, bgra, nv12/yuv420p, or p010le"
            ),
        )),
    }
}

fn ffmpeg_video_job(
    command: &RenderCommand,
    config: &fmn_config::Config,
) -> Result<VideoJob, CliError> {
    if command.common.reproducible {
        return Err(CliError::new(
            "capability",
            "ffmpeg video is outside the certified artifact set; use --format png_sequence or --format y4m with --reproducible",
        ));
    }
    // Any represented non-identity value is semantically observable. An
    // epsilon check would silently discard a requested color transform.
    if config.file_writer.saturation != 1.0 || config.file_writer.gamma != 1.0 {
        return Err(CliError::new(
            "capability",
            "non-default file_writer saturation/gamma require a native color-transform stage, which is not registered; ffmpeg filters are forbidden",
        ));
    }
    let wire = ffmpeg_wire_format(command, config)?;
    if command.transparent && !wire.has_alpha() {
        return Err(CliError::new(
            "config",
            "transparent video requires an rgba or bgra wire pixel format",
        ));
    }
    let codec = config.file_writer.video_codec.trim();
    if codec.is_empty() {
        return Err(CliError::new(
            "config",
            "file_writer.video_codec must not be empty",
        ));
    }
    let encoder = if codec.eq_ignore_ascii_case("auto")
        || (command.transparent && command.vcodec.is_none() && codec == "libx264")
    {
        EncoderChoice::Auto
    } else {
        EncoderChoice::Named(codec.to_owned())
    };
    let job = VideoJob {
        width: config.camera.resolution.0,
        height: config.camera.resolution.1,
        fps: (config.camera.fps, 1),
        wire,
        color: if wire.has_alpha() {
            ColorDescription::srgb_full()
        } else {
            ColorDescription::video_bt709()
        },
        container: if command.transparent {
            Container::MovTransparent
        } else {
            Container::Mp4
        },
        encoder,
        crf: None,
    };
    let resolved_encoder = job
        .resolved_encoder()
        .map_err(|error| CliError::new("config", error.to_string()))?;
    if command.transparent && resolved_encoder.as_deref() != Some("qtrle") {
        return Err(CliError::new(
            "config",
            "transparent video currently requires the qtrle encoder",
        ));
    }
    Ok(job)
}

fn prepare_ffmpeg_context(
    runner: Arc<dyn fmn_platform::process::ProcessRunner>,
    locator: &dyn fmn_platform::process::FfmpegLocator,
    configured: &Path,
    job: VideoJob,
) -> Result<FfmpegRenderContext, CliError> {
    let executable = locator.locate_ffmpeg(configured).map_err(|error| {
        CliError::new(
            "capability",
            format!(
                "ffmpeg is unavailable at {}: {error}; {}",
                configured.display(),
                fmn_output::NATIVE_ALTERNATIVE
            ),
        )
    })?;
    let workdir_root = std::env::temp_dir();
    let tool =
        FfmpegTool::resolve(executable, runner.as_ref(), &workdir_root).map_err(|error| {
            CliError::new(
                "capability",
                format!("ffmpeg boundary initialization failed: {error}"),
            )
        })?;
    let capabilities = EncoderCapabilities::probe(&tool, runner.as_ref()).map_err(|error| {
        CliError::new(
            "capability",
            format!("ffmpeg encoder discovery failed: {error}"),
        )
    })?;
    if let Some(encoder) = job
        .resolved_encoder()
        .map_err(|error| CliError::new("config", error.to_string()))?
        && !capabilities.offers(&encoder)
    {
        return Err(CliError::new(
            "capability",
            format!("installed ffmpeg does not offer encoder {encoder:?}"),
        ));
    }
    Ok(FfmpegRenderContext {
        runner,
        tool,
        capabilities,
        job,
        workdir_root,
    })
}

fn bundle_read_error(error: BundleReadError) -> CliError {
    let exit_name = match error {
        BundleReadError::FrameCountUnrepresentable { .. }
        | BundleReadError::AllocationFailed { .. } => "budget",
        _ => "scene",
    };
    CliError::new(exit_name, error.to_string())
}

fn compiled_scene_name(command: &RenderCommand, source: &Path) -> Result<String, CliError> {
    if command.scene_names.len() > 1 {
        return Err(CliError::new(
            "usage",
            "an FMTL artifact contains exactly one compiled scene; select at most one output name",
        ));
    }
    let name = command.scene_names.first().cloned().unwrap_or_else(|| {
        source
            .file_stem()
            .and_then(OsStr::to_str)
            .unwrap_or("scene")
            .to_owned()
    });
    let path = Path::new(&name);
    if name.is_empty() || name == "." || name == ".." || path.file_name() != Some(OsStr::new(&name))
    {
        return Err(CliError::new(
            "usage",
            "the compiled scene output name must be one non-empty path component",
        ));
    }
    Ok(name)
}

fn resolve_native_render_input(
    fs: &dyn FileSystem,
    command: &RenderCommand,
) -> Result<NativeRenderInput, CliError> {
    let source = command.file.as_deref().ok_or_else(|| {
        CliError::new(
            "scene",
            format!("select {BUILTIN_SCENE_SOURCE} or provide one compiled .fmtl artifact"),
        )
    })?;
    if source == Path::new(BUILTIN_SCENE_SOURCE) {
        let names = if command.write_all {
            fmn::builtins::PRIMITIVE_SCENE_NAMES
                .iter()
                .map(|name| (*name).to_owned())
                .collect()
        } else if command.scene_names.is_empty() {
            return Err(CliError::new(
                "scene",
                format!(
                    "select a built-in scene or pass --write_all; available scenes: {}",
                    fmn::builtins::PRIMITIVE_SCENE_NAMES.join(", ")
                ),
            ));
        } else {
            command.scene_names.clone()
        };
        if names.len() > 1 && command.file_name.is_some() {
            return Err(CliError::new(
                "config",
                "--file_name cannot name multiple --write_all artifacts",
            ));
        }
        for name in &names {
            if fmn::builtins::primitive_scene(name).is_none() {
                return Err(CliError::new(
                    "scene",
                    format!(
                        "unknown built-in scene {name:?}; available scenes: {}",
                        fmn::builtins::PRIMITIVE_SCENE_NAMES.join(", ")
                    ),
                ));
            }
        }
        return Ok(NativeRenderInput::Builtin { names });
    }

    if source
        .extension()
        .and_then(OsStr::to_str)
        .is_none_or(|extension| !extension.eq_ignore_ascii_case("fmtl"))
    {
        return Err(CliError::new(
            "capability",
            "the standalone native artifact reader currently accepts the authoritative FMTL/1 .fmtl format",
        ));
    }
    if command.write_all {
        return Err(CliError::new(
            "usage",
            "--write_all applies to multi-registration sources; an FMTL artifact contains one compiled scene",
        ));
    }
    if command.skip_animations
        || command.animation_range.is_some()
        || command.presenter_mode
        || command.full_screen
        || command.autoreload
        || command.embed_line.is_some()
    {
        return Err(CliError::new(
            "capability",
            "an FMTL artifact has a fixed compiled schedule; skip, range, presenter, fullscreen, reload, and embed controls require an authored scene source",
        ));
    }
    let name = compiled_scene_name(command, source)?;
    let bytes = fs
        .read_bounded(source, DEFAULT_MAX_BUNDLE_BYTES)
        .map_err(|error| {
            let exit_name = if matches!(error, FsError::TooLarge { .. }) {
                "budget"
            } else {
                "scene"
            };
            CliError::new(
                exit_name,
                format!("could not read {}: {error}", source.display()),
            )
        })?;
    let source_item = ClosureItem::byte_input(
        1,
        source.to_string_lossy().into_owned(),
        &bytes,
        "compiled FMTL/1 scene artifact",
    )
    .map_err(|error| internal(error.to_string()))?;
    let bundle = TimelineBundle::from_bytes(&bytes).map_err(bundle_read_error)?;
    if u64::from(bundle.frame_count()) > fmn_scene::DEFAULT_MAX_BUNDLE_EXPORT_FRAMES {
        return Err(CliError::new(
            "budget",
            format!(
                "compiled artifact schedules {} frames, exceeding the {}-frame CLI output budget",
                bundle.frame_count(),
                fmn_scene::DEFAULT_MAX_BUNDLE_EXPORT_FRAMES
            ),
        ));
    }
    Ok(NativeRenderInput::Compiled {
        source: source.to_owned(),
        source_item,
        name,
        bundle: Box::new(bundle),
    })
}

fn execute_native_render(
    fs: Arc<dyn FileSystem>,
    runner: Arc<dyn fmn_platform::process::ProcessRunner>,
    locator: &dyn fmn_platform::process::FfmpegLocator,
    command: &RenderCommand,
) -> Result<Vec<CompletedRender>, CliError> {
    execute_native_render_with_cancellation(fs, runner, locator, command, None, false)
}

fn execute_native_render_with_cancellation(
    fs: Arc<dyn FileSystem>,
    runner: Arc<dyn fmn_platform::process::ProcessRunner>,
    locator: &dyn fmn_platform::process::FfmpegLocator,
    command: &RenderCommand,
    cancellation: Option<&RenderCancellation>,
    capture_manifest: bool,
) -> Result<Vec<CompletedRender>, CliError> {
    if let Some(cancellation) = cancellation {
        cancellation.cli_checkpoint()?;
    }
    if command.open || command.finder {
        return Err(CliError::new(
            "capability",
            "host open/reveal integration is not registered; the render artifact can still be written without `--open` or `--finder`",
        ));
    }
    if command.subdivide || command.prerun {
        return Err(CliError::new(
            "capability",
            "the native render path does not yet support `--subdivide` or `--prerun`",
        ));
    }
    let input = resolve_native_render_input(fs.as_ref(), command)?;
    let requested_format = requested_render_format(command)?;
    let mut config = resolve_render_config(fs.as_ref(), command)?;
    if let NativeRenderInput::Compiled { bundle, .. } = &input {
        if let Some(requested_fps) = command.fps
            && requested_fps != bundle.fps()
        {
            return Err(CliError::new(
                "config",
                format!(
                    "--fps {requested_fps} disagrees with the compiled artifact's fixed {} fps schedule",
                    bundle.fps()
                ),
            ));
        }
        let mut compiled_command = command.clone();
        compiled_command.fps = Some(bundle.fps());
        config = resolve_render_config(fs.as_ref(), &compiled_command)?;
    }
    let video_job = match requested_format {
        RequestedRenderFormat::Native(_) => None,
        RequestedRenderFormat::Video => Some(ffmpeg_video_job(command, &config)?),
    };
    let planning_format = match (requested_format, video_job.as_ref()) {
        (RequestedRenderFormat::Native(format), _) => format.planning_format(),
        (RequestedRenderFormat::Video, Some(job)) => match job.wire.frame_format() {
            PixelFormat::Rgba8 => fmn_runtime::OutputPixelFormat::Rgba8,
            PixelFormat::Bgra8 => fmn_runtime::OutputPixelFormat::Bgra8,
            PixelFormat::Nv12 => fmn_runtime::OutputPixelFormat::Nv12,
            PixelFormat::P010 => fmn_runtime::OutputPixelFormat::P010,
            PixelFormat::Rgba16F => fmn_runtime::OutputPixelFormat::Rgba16F,
        },
        (RequestedRenderFormat::Video, None) => {
            return Err(CliError::new(
                "internal",
                "video format resolution omitted its negotiated job",
            ));
        }
    };
    let (plan, _, _) = derive_execution_plan(
        fs.as_ref(),
        &config,
        fmn_runtime::RenderIntent::Offline,
        planning_format,
    )?;
    let process_mechanism = runner.mechanism();
    let target = match (requested_format, video_job) {
        (RequestedRenderFormat::Native(format), None) => RenderTarget::Native(format),
        (RequestedRenderFormat::Video, Some(job)) => {
            RenderTarget::Video(Box::new(prepare_ffmpeg_context(
                runner,
                locator,
                Path::new(&config.file_writer.ffmpeg_bin),
                job,
            )?))
        }
        _ => {
            return Err(CliError::new(
                "internal",
                "render format and ffmpeg job disagreed",
            ));
        }
    };
    let output_directory = command
        .video_dir
        .clone()
        .unwrap_or_else(|| configured_output_directory(&config));
    let naming = OutputNaming {
        output_directory,
        file_name: command.file_name.as_deref().map(PathBuf::from),
        start_at_play: command.animation_range.map(|range| range.start),
        end_at_play: command.animation_range.and_then(|range| range.end),
        open_on_completion: false,
    };
    let engine = match plan.engine {
        fmn_runtime::ExecutionEngine::CertifiedCpu => EngineIdentity::certified(),
        fmn_runtime::ExecutionEngine::FastCpu => EngineIdentity::fast(),
        fmn_runtime::ExecutionEngine::Metal | fmn_runtime::ExecutionEngine::Cuda => {
            return Err(CliError::new(
                "capability",
                "the selected annex engine has no production CLI renderer",
            ));
        }
    };
    let render_threads = plan
        .render_teams
        .first()
        .map_or(1, fmn_runtime::TeamPlan::threads);
    let destination = |name: &str| match &target {
        RenderTarget::Native(NativeFrameFormat::Png) => naming.artifact(name, "png"),
        RenderTarget::Native(NativeFrameFormat::PngSequence) => naming.root(name),
        RenderTarget::Native(NativeFrameFormat::Gif) => naming.artifact(name, "gif"),
        RenderTarget::Native(NativeFrameFormat::Y4m) => naming.artifact(name, "y4m"),
        RenderTarget::Video(context) => naming.artifact(name, context.job.container.extension()),
    };
    let complete = |source, source_item, scene, artifact| -> Result<CompletedRender, CliError> {
        let manifest = if capture_manifest {
            Some(render_manifest(
                ManifestContext {
                    fs: fs.as_ref(),
                    process_mechanism,
                    command,
                    config: &config,
                    plan: &plan,
                    engine,
                },
                source_item,
                &artifact,
            )?)
        } else {
            None
        };
        Ok(CompletedRender {
            source,
            scene,
            artifact,
            engine: engine.closure_string(),
            render_threads,
            manifest,
            manifest_path: None,
        })
    };
    let mut reports = Vec::new();
    match input {
        NativeRenderInput::Builtin { names } => {
            reports.reserve(names.len());
            for name in names {
                if let Some(cancellation) = cancellation {
                    cancellation.cli_checkpoint()?;
                }
                let mut sink =
                    RenderSink::new(Arc::clone(&fs), &config, &plan, &target, destination(&name))?;
                if let Some(cancellation) = cancellation {
                    let emitter = sink.emitter_handle().ok_or_else(|| {
                        CliError::new("internal", "new render sink omitted its emitter")
                    })?;
                    cancellation.register_emitter(emitter);
                    cancellation.cli_checkpoint()?;
                }
                let mut scene = fmn::builtins::primitive_scene(&name).ok_or_else(|| {
                    CliError::new("internal", "validated built-in scene disappeared")
                })?;
                if command.skip_animations
                    || matches!(target, RenderTarget::Native(NativeFrameFormat::Png))
                {
                    let mut discard = NullSceneSink;
                    let completed = if let Some(cancellation) = cancellation {
                        let mut cancellable = CancellableSceneSink {
                            inner: &mut discard,
                            cancellation,
                        };
                        fmn::run_scene(
                            &mut scene,
                            command.runtime_config(&config),
                            config.determinism.seed,
                            &mut cancellable,
                        )
                    } else {
                        fmn::run_scene(
                            &mut scene,
                            command.runtime_config(&config),
                            config.determinism.seed,
                            &mut discard,
                        )
                    }
                    .map_err(native_scene_error)?;
                    // Skip mode advances semantic state without ordinary
                    // captures. A still has the same composition shape even
                    // without `--skip_animations`: run to completion, then
                    // publish the one final-state frame explicitly.
                    let mut completed = completed.into_scene();
                    if let Some(cancellation) = cancellation {
                        let mut cancellable = CancellableSceneSink {
                            inner: &mut sink,
                            cancellation,
                        };
                        completed.show(&mut cancellable)
                    } else {
                        completed.show(&mut sink)
                    }
                    .map_err(|error| CliError::new("scene", error.to_string()))?;
                } else {
                    if let Some(cancellation) = cancellation {
                        let mut cancellable = CancellableSceneSink {
                            inner: &mut sink,
                            cancellation,
                        };
                        fmn::run_scene(
                            &mut scene,
                            command.runtime_config(&config),
                            config.determinism.seed,
                            &mut cancellable,
                        )
                    } else {
                        fmn::run_scene(
                            &mut scene,
                            command.runtime_config(&config),
                            config.determinism.seed,
                            &mut sink,
                        )
                    }
                    .map_err(native_scene_error)?;
                }
                if let Some(cancellation) = cancellation {
                    cancellation.cli_checkpoint()?;
                }
                let artifact = sink.finish()?;
                let source_item = builtin_source_item(&name)?;
                reports.push(complete(
                    RenderSourceReport::Builtin,
                    source_item,
                    name,
                    artifact,
                )?);
            }
        }
        NativeRenderInput::Compiled {
            source,
            source_item,
            name,
            bundle,
        } => {
            let mut sink =
                RenderSink::new(Arc::clone(&fs), &config, &plan, &target, destination(&name))?;
            if let Some(cancellation) = cancellation {
                let emitter = sink.emitter_handle().ok_or_else(|| {
                    CliError::new("internal", "new render sink omitted its emitter")
                })?;
                cancellation.register_emitter(emitter);
                cancellation.cli_checkpoint()?;
            }
            let frames = if matches!(target, RenderTarget::Native(NativeFrameFormat::Png)) {
                let final_frame = bundle.frame_count().checked_sub(1).ok_or_else(|| {
                    CliError::new(
                        "scene",
                        "the compiled timeline has no frame to publish as a final-state PNG",
                    )
                })?;
                final_frame..bundle.frame_count()
            } else {
                0..bundle.frame_count()
            };
            for index in frames {
                if let Some(cancellation) = cancellation {
                    cancellation.cli_checkpoint()?;
                }
                let stage = bundle.stage_at(index).map_err(bundle_read_error)?;
                sink.render_stage(&stage, u64::from(index) + 1)
                    .map_err(|error| CliError::new("render", error.to_string()))?;
            }
            if let Some(cancellation) = cancellation {
                cancellation.cli_checkpoint()?;
            }
            reports.push(complete(
                RenderSourceReport::Compiled(source),
                source_item,
                name,
                sink.finish()?,
            )?);
        }
    }
    Ok(reports)
}

fn configured_output_directory(config: &fmn_config::Config) -> PathBuf {
    let mut root = PathBuf::from(&config.directories.base);
    if let Some((_, output)) = config
        .directories
        .subdirs
        .iter()
        .find(|(name, _)| name == "output")
    {
        root.push(output);
    }
    if root.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        root
    }
}

fn successful_render_output(command: &RenderCommand, reports: Vec<CompletedRender>) -> RunOutput {
    let mut stdout = String::new();
    for CompletedRender {
        source,
        scene,
        artifact,
        engine,
        render_threads,
        manifest,
        manifest_path,
    } in reports
    {
        let source_artifact = source.artifact().map_or_else(String::new, |path| {
            format!(
                ",\"source_artifact\":{}",
                json_string(&path.to_string_lossy())
            )
        });
        let human_source = source
            .artifact()
            .map_or_else(String::new, |path| format!(" from {}", path.display()));
        let manifest_json = manifest.as_ref().zip(manifest_path.as_ref()).map_or_else(
            String::new,
            |(manifest, path)| {
                format!(
                    ",\"manifest\":{{\"path\":{},\"closure_digest\":{}}}",
                    json_string(&path.to_string_lossy()),
                    json_string(&manifest.closure_digest.to_hex()),
                )
            },
        );
        let human_manifest = manifest_path
            .as_ref()
            .map_or_else(String::new, |path| format!("; manifest {}", path.display()));
        match artifact {
            RenderArtifactReport::Native(report) => {
                if command.common.robot {
                    let _ = write!(
                        stdout,
                        "{{\"schema\":\"fmn.cli\",\"version\":{},\"kind\":\"render\",\"source\":{}{},\"scene\":{},\"format\":{},\"artifact\":{},\"frames\":{},\"bytes\":{},\"engine\":{},\"render_threads\":{},\"artifact_digest\":{}",
                        ROBOT_SCHEMA_VERSION,
                        json_string(source.kind()),
                        source_artifact,
                        json_string(&scene),
                        json_string(match report.kind {
                            fmn_output::NativeArtifactKind::PngSequence => "png_sequence",
                            fmn_output::NativeArtifactKind::Y4m => "y4m",
                            fmn_output::NativeArtifactKind::Png => "png",
                            fmn_output::NativeArtifactKind::Gif => "gif",
                        }),
                        json_string(&report.path.to_string_lossy()),
                        report.frame_count,
                        report.bytes,
                        json_string(&engine),
                        render_threads,
                        json_string(&report.digest.to_hex()),
                    );
                    stdout.push_str(&manifest_json);
                    stdout.push_str("}\n");
                } else if !command.common.quiet {
                    let _ = writeln!(
                        stdout,
                        "rendered {scene}{human_source} as {}: {} ({} frames, {} bytes; {engine}, {render_threads} threads{human_manifest})",
                        match report.kind {
                            fmn_output::NativeArtifactKind::PngSequence => "PNG sequence",
                            fmn_output::NativeArtifactKind::Y4m => "y4m",
                            fmn_output::NativeArtifactKind::Png => "PNG",
                            fmn_output::NativeArtifactKind::Gif => "GIF",
                        },
                        report.path.display(),
                        report.frame_count,
                        report.bytes,
                    );
                }
            }
            RenderArtifactReport::Video(report) => {
                if command.common.robot {
                    let _ = write!(
                        stdout,
                        "{{\"schema\":\"fmn.cli\",\"version\":{},\"kind\":\"render\",\"source\":{}{},\"scene\":{},\"format\":\"video\",\"artifact\":{},\"frames\":{},\"input_bytes\":{},\"bytes\":{},\"artifact_digest\":{},\"engine\":{},\"render_threads\":{},\"ffmpeg\":{{\"path\":{},\"sha256\":{},\"version\":{},\"native_image_format\":{},\"native_image_architecture\":{},\"native_image_bytes\":{},\"native_image_policy_version\":{},\"encoder\":{},\"process_mechanism\":{},\"process_policy_version\":{},\"argv\":{}}}",
                        ROBOT_SCHEMA_VERSION,
                        json_string(source.kind()),
                        source_artifact,
                        json_string(&scene),
                        json_string(&report.path.to_string_lossy()),
                        report.frame_count,
                        report.input_bytes,
                        report.artifact_bytes,
                        json_string(&report.artifact_digest.to_hex()),
                        json_string(&engine),
                        render_threads,
                        json_string(&report.tool_path.to_string_lossy()),
                        json_string(&report.tool_sha256),
                        json_string(&report.tool_version),
                        json_string(report.native_image_format),
                        json_string(report.native_image_architecture),
                        report.native_image_bytes,
                        report.native_image_policy_version,
                        json_option(report.encoder.as_deref()),
                        json_string(&report.process_mechanism),
                        report.process_policy_version,
                        json_array(&report.argv),
                    );
                    stdout.push_str(&manifest_json);
                    stdout.push_str("}\n");
                } else if !command.common.quiet {
                    let _ = writeln!(
                        stdout,
                        "rendered {scene}{human_source} as ffmpeg video: {} ({} frames, {} input bytes; {engine}, {render_threads} threads; {} via {}{human_manifest})",
                        report.path.display(),
                        report.frame_count,
                        report.input_bytes,
                        report.encoder.as_deref().unwrap_or("container default"),
                        report.process_mechanism,
                    );
                }
            }
        }
    }
    RunOutput::success(stdout)
}

#[cfg(feature = "batch")]
#[derive(Clone)]
struct FixedBatchFfmpegLocator {
    executable: Option<fmn_platform::process::FfmpegExecutable>,
}

#[cfg(feature = "batch")]
impl fmn_platform::process::FfmpegLocator for FixedBatchFfmpegLocator {
    fn locate_ffmpeg(
        &self,
        _configured: &Path,
    ) -> Result<fmn_platform::process::FfmpegExecutable, fmn_platform::process::FfmpegLocatorError>
    {
        self.executable
            .clone()
            .ok_or(fmn_platform::process::FfmpegLocatorError::NotFound)
    }
}

#[cfg(feature = "batch")]
struct BatchJob {
    scene: String,
    command: RenderCommand,
}

#[cfg(feature = "batch")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BatchCancellationReason {
    FailFast,
    Budget,
}

#[cfg(feature = "batch")]
impl BatchCancellationReason {
    const fn name(self) -> &'static str {
        match self {
            Self::FailFast => "fail_fast",
            Self::Budget => "budget",
        }
    }
}

#[cfg(feature = "batch")]
enum BatchJobStatus {
    Succeeded(Vec<CompletedRender>),
    Failed(CliError),
    Cancelled(BatchCancellationReason),
}

#[cfg(feature = "batch")]
fn batch_cancellation_reason(cancellation: &RenderCancellation) -> Option<BatchCancellationReason> {
    match cancellation.reason.load(Ordering::Acquire) {
        RENDER_ACTIVE => None,
        RENDER_CANCEL_FAIL_FAST => Some(BatchCancellationReason::FailFast),
        RENDER_CANCEL_BUDGET => Some(BatchCancellationReason::Budget),
        _ => Some(BatchCancellationReason::FailFast),
    }
}

#[cfg(feature = "batch")]
fn prepare_batch_jobs(
    fs: &dyn FileSystem,
    command: &RenderCommand,
) -> Result<Vec<BatchJob>, CliError> {
    match resolve_native_render_input(fs, command)? {
        NativeRenderInput::Builtin { names } => Ok(names
            .into_iter()
            .map(|scene| {
                let mut command = command.clone();
                command.scene_names = vec![scene.clone()];
                command.write_all = false;
                BatchJob { scene, command }
            })
            .collect()),
        NativeRenderInput::Compiled { name, .. } => Ok(vec![BatchJob {
            scene: name,
            command: command.clone(),
        }]),
    }
}

#[cfg(feature = "batch")]
fn prepare_batch_locator(
    fs: &dyn FileSystem,
    locator: &dyn fmn_platform::process::FfmpegLocator,
    command: &RenderCommand,
) -> Result<FixedBatchFfmpegLocator, CliError> {
    let executable = if requested_render_format(command)? == RequestedRenderFormat::Video {
        let config = resolve_render_config(fs, command)?;
        let configured = Path::new(&config.file_writer.ffmpeg_bin);
        Some(locator.locate_ffmpeg(configured).map_err(|error| {
            CliError::new(
                "capability",
                format!(
                    "ffmpeg is unavailable at {}: {error}; {}",
                    configured.display(),
                    fmn_output::NATIVE_ALTERNATIVE
                ),
            )
        })?)
    } else {
        None
    };
    Ok(FixedBatchFfmpegLocator { executable })
}

#[cfg(feature = "batch")]
fn preflight_batch_manifests(
    fs: &dyn FileSystem,
    command: &BatchCommand,
    jobs: &[BatchJob],
) -> Result<(), CliError> {
    let Some(root) = command.manifest_dir.as_deref() else {
        return Ok(());
    };
    match fs.node_kind_no_follow(root).map_err(|error| {
        CliError::new(
            "output",
            format!(
                "could not inspect manifest directory {}: {error}",
                root.display()
            ),
        )
    })? {
        None | Some(FsNodeKind::Directory) => {}
        Some(kind) => {
            return Err(CliError::new(
                "config",
                format!(
                    "manifest directory {} is a {kind:?}, not a directory",
                    root.display()
                ),
            ));
        }
    }
    let config = resolve_render_config(fs, &command.render)?;
    let output_root = command
        .render
        .video_dir
        .clone()
        .unwrap_or_else(|| configured_output_directory(&config));
    let png_sequence = requested_render_format(&command.render)?
        == RequestedRenderFormat::Native(NativeFrameFormat::PngSequence);
    for job in jobs {
        let destination = root.join(&job.scene);
        if png_sequence && destination == output_root.join(&job.scene) {
            return Err(CliError::new(
                "config",
                "--manifest-dir must differ from the PNG-sequence output directory",
            ));
        }
        if fs
            .node_kind_no_follow(&destination)
            .map_err(|error| {
                CliError::new(
                    "output",
                    format!(
                        "could not inspect manifest destination {}: {error}",
                        destination.display()
                    ),
                )
            })?
            .is_some()
        {
            return Err(CliError::new(
                "output",
                format!(
                    "manifest destination {} already exists; per-scene manifests are no-clobber generations",
                    destination.display()
                ),
            ));
        }
    }
    Ok(())
}

#[cfg(feature = "batch")]
fn publish_batch_manifests(
    fs: &Arc<dyn FileSystem>,
    root: &Path,
    reports: &mut [CompletedRender],
) -> Result<(), CliError> {
    for report in reports {
        let manifest = report
            .manifest
            .as_ref()
            .ok_or_else(|| internal("batch manifest capture completed without an FMNP document"))?;
        let binary = manifest
            .to_bytes()
            .map_err(|error| internal(format!("serialize FMNP manifest: {error}")))?;
        let text = manifest.to_text();
        let destination = root.join(&report.scene);
        let mut writer = Arc::clone(fs)
            .begin_atomic_directory(&destination)
            .map_err(|error| {
                CliError::new(
                    "output",
                    format!(
                        "could not stage manifest generation {}: {error}",
                        destination.display()
                    ),
                )
            })?;
        writer
            .write_file(Path::new("manifest.fmnp"), &binary)
            .and_then(|()| writer.write_file(Path::new("manifest.txt"), text.as_bytes()))
            .map_err(|error| {
                CliError::new(
                    "output",
                    format!(
                        "could not write manifest generation {}: {error}",
                        destination.display()
                    ),
                )
            })?;
        writer
            .prepare()
            .and_then(|prepared| prepared.commit())
            .map_err(|error| {
                CliError::new(
                    "output",
                    format!(
                        "could not publish manifest generation {}: {error}",
                        destination.display()
                    ),
                )
            })?;
        report.manifest_path = Some(destination.join("manifest.fmnp"));
    }
    Ok(())
}

#[cfg(feature = "batch")]
fn batch_output(
    command: &BatchCommand,
    scene_names: &[String],
    statuses: Vec<BatchJobStatus>,
    max_scenes: usize,
) -> RunOutput {
    let mut stdout = String::new();
    let mut stderr = String::new();
    let mut succeeded = 0_usize;
    let mut failed = 0_usize;
    let mut cancelled = 0_usize;
    let mut first_error_code = None;

    for (scene, status) in scene_names.iter().zip(statuses) {
        match status {
            BatchJobStatus::Succeeded(reports) => {
                succeeded = succeeded.saturating_add(1);
                let output = successful_render_output(&command.render, reports);
                stdout.push_str(&output.stdout);
            }
            BatchJobStatus::Failed(error) => {
                failed = failed.saturating_add(1);
                first_error_code.get_or_insert_with(|| error.code());
                if command.render.common.robot {
                    let _ = writeln!(
                        stdout,
                        "{{\"schema\":\"fmn.cli\",\"version\":{},\"kind\":\"batch_job\",\"scene\":{},\"status\":\"failed\",\"exit_code\":{},\"exit_name\":{},\"rule\":{},\"message\":{}}}",
                        ROBOT_SCHEMA_VERSION,
                        json_string(scene),
                        error.code(),
                        json_string(error.exit_name()),
                        json_option(error.rule()),
                        json_string(error.message()),
                    );
                } else {
                    let _ = writeln!(stderr, "fmn batch: {scene}: {error}");
                }
            }
            BatchJobStatus::Cancelled(reason) => {
                cancelled = cancelled.saturating_add(1);
                if command.render.common.robot {
                    let _ = writeln!(
                        stdout,
                        "{{\"schema\":\"fmn.cli\",\"version\":{},\"kind\":\"batch_job\",\"scene\":{},\"status\":\"cancelled\",\"reason\":{}}}",
                        ROBOT_SCHEMA_VERSION,
                        json_string(scene),
                        json_string(reason.name()),
                    );
                } else {
                    let _ = writeln!(stderr, "fmn batch: {scene}: cancelled ({})", reason.name());
                }
            }
        }
    }

    let ok = failed == 0 && cancelled == 0;
    if command.render.common.robot {
        let budget_ms = command
            .budget_ms
            .map_or_else(|| "null".to_owned(), |value| value.to_string());
        let _ = writeln!(
            stdout,
            "{{\"schema\":\"fmn.cli\",\"version\":{},\"kind\":\"batch\",\"status\":{},\"jobs\":{},\"succeeded\":{},\"failed\":{},\"cancelled\":{},\"max_scenes\":{},\"budget_ms\":{}}}",
            ROBOT_SCHEMA_VERSION,
            json_string(if ok { "ok" } else { "failed" }),
            scene_names.len(),
            succeeded,
            failed,
            cancelled,
            max_scenes,
            budget_ms,
        );
    } else if !command.render.common.quiet {
        let _ = writeln!(
            stdout,
            "batch finished: {succeeded} succeeded, {failed} failed, {cancelled} cancelled (max {max_scenes} concurrent)"
        );
    }

    let code = first_error_code.unwrap_or_else(|| {
        if cancelled == 0 {
            exit_code("success")
        } else {
            exit_code("budget")
        }
    });
    RunOutput {
        code,
        stdout,
        stderr,
    }
}

#[cfg(feature = "batch")]
fn execute_batch(
    fs: Arc<dyn FileSystem>,
    runner: Arc<dyn fmn_platform::process::ProcessRunner>,
    locator: &dyn fmn_platform::process::FfmpegLocator,
    command: &BatchCommand,
) -> RunOutput {
    if command.max_scenes == Some(0) {
        return error_output(
            command.render.common.robot,
            &CliError::new("config", "--max-scenes must be greater than zero"),
        );
    }

    let mut jobs = match prepare_batch_jobs(fs.as_ref(), &command.render) {
        Ok(jobs) => jobs,
        Err(error) => return error_output(command.render.common.robot, &error),
    };
    if let Err(error) = preflight_batch_manifests(fs.as_ref(), command, &jobs) {
        return error_output(command.render.common.robot, &error);
    }
    let locator = match prepare_batch_locator(fs.as_ref(), locator, &command.render) {
        Ok(locator) => Arc::new(locator),
        Err(error) => return error_output(command.render.common.robot, &error),
    };
    let (topology, _) = detect_topology(fs.as_ref());
    let physical_cores = usize::try_from(topology.physical_cores).unwrap_or(usize::MAX);
    let logical_cores = usize::try_from(topology.logical_cores()).unwrap_or(usize::MAX);
    let default_scenes = physical_cores.max(1).min(jobs.len());
    let max_scenes = command
        .max_scenes
        .unwrap_or(default_scenes)
        .min(jobs.len())
        .max(1);
    if command.render.common.threads.is_none() {
        let per_scene_threads = (logical_cores / max_scenes).max(1);
        for job in &mut jobs {
            job.command.common.threads = Some(per_scene_threads);
        }
    }
    let deadline = match command.budget_ms {
        Some(milliseconds) => match Instant::now().checked_add(Duration::from_millis(milliseconds))
        {
            Some(deadline) => Some(deadline),
            None => {
                return error_output(
                    command.render.common.robot,
                    &CliError::new("config", "--budget-ms is too large for the host clock"),
                );
            }
        },
        None => None,
    };
    let runtime = match asupersync::runtime::RuntimeBuilder::current_thread()
        .blocking_threads(max_scenes, max_scenes)
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            return error_output(
                command.render.common.robot,
                &CliError::new(
                    "internal",
                    format!("batch runtime initialization failed: {error}"),
                ),
            );
        }
    };

    let cancellation = Arc::new(RenderCancellation::default());
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        cancellation.request(RENDER_CANCEL_BUDGET);
    }
    let (sender, receiver) = mpsc::channel();
    let scene_names: Vec<String> = jobs.iter().map(|job| job.scene.clone()).collect();
    let job_count = jobs.len();
    let mut handles: Vec<asupersync::runtime::blocking_pool::BlockingTaskHandle> =
        Vec::with_capacity(job_count);
    for (index, job) in jobs.into_iter().enumerate() {
        let fs = Arc::clone(&fs);
        let runner = Arc::clone(&runner);
        let locator = Arc::clone(&locator);
        let task_cancellation = Arc::clone(&cancellation);
        let sender = sender.clone();
        let fail_fast = command.fail_fast;
        let manifest_dir = command.manifest_dir.clone();
        let Some(handle) = runtime.spawn_blocking(move || {
            let status = if let Some(reason) = batch_cancellation_reason(&task_cancellation) {
                BatchJobStatus::Cancelled(reason)
            } else {
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let mut reports = execute_native_render_with_cancellation(
                        Arc::clone(&fs),
                        runner,
                        locator.as_ref(),
                        &job.command,
                        Some(task_cancellation.as_ref()),
                        manifest_dir.is_some(),
                    )?;
                    if let Some(root) = manifest_dir.as_deref() {
                        publish_batch_manifests(&fs, root, &mut reports)?;
                    }
                    Ok::<_, CliError>(reports)
                })) {
                    Ok(Ok(reports)) => BatchJobStatus::Succeeded(reports),
                    Ok(Err(error)) => {
                        if let Some(reason) = batch_cancellation_reason(&task_cancellation) {
                            BatchJobStatus::Cancelled(reason)
                        } else {
                            if fail_fast {
                                task_cancellation.request(RENDER_CANCEL_FAIL_FAST);
                            }
                            BatchJobStatus::Failed(error)
                        }
                    }
                    Err(_) => {
                        if fail_fast {
                            task_cancellation.request(RENDER_CANCEL_FAIL_FAST);
                        }
                        BatchJobStatus::Failed(CliError::new(
                            "internal",
                            "batch scene job panicked",
                        ))
                    }
                }
            };
            let _ = sender.send((index, status));
        }) else {
            cancellation.request(RENDER_CANCEL_FAIL_FAST);
            for handle in &handles {
                handle.cancel();
                handle.wait();
            }
            return error_output(
                command.render.common.robot,
                &CliError::new("internal", "Asupersync blocking pool is unavailable"),
            );
        };
        handles.push(handle);
    }
    drop(sender);

    let mut statuses: Vec<Option<BatchJobStatus>> =
        std::iter::repeat_with(|| None).take(job_count).collect();
    let mut received = 0_usize;
    while received < job_count {
        let result = if let Some(deadline) = deadline {
            let now = Instant::now();
            if now >= deadline {
                cancellation.request(RENDER_CANCEL_BUDGET);
                break;
            }
            receiver.recv_timeout(deadline.saturating_duration_since(now))
        } else {
            match receiver.recv() {
                Ok(message) => Ok(message),
                Err(_) => Err(mpsc::RecvTimeoutError::Disconnected),
            }
        };
        match result {
            Ok((index, status)) if index < job_count && statuses[index].is_none() => {
                statuses[index] = Some(status);
                received = received.saturating_add(1);
            }
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => {
                cancellation.request(RENDER_CANCEL_BUDGET);
                break;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    if batch_cancellation_reason(&cancellation) == Some(BatchCancellationReason::Budget) {
        for handle in &handles {
            handle.cancel();
        }
    }
    for handle in &handles {
        handle.wait();
    }
    while let Ok((index, status)) = receiver.try_recv() {
        if index < job_count && statuses[index].is_none() {
            statuses[index] = Some(status);
        }
    }
    let missing_reason = batch_cancellation_reason(&cancellation);
    let statuses = statuses
        .into_iter()
        .map(|status| {
            status.unwrap_or_else(|| {
                missing_reason.map_or_else(
                    || {
                        BatchJobStatus::Failed(CliError::new(
                            "internal",
                            "batch job ended without a terminal report",
                        ))
                    },
                    BatchJobStatus::Cancelled,
                )
            })
        })
        .collect();
    batch_output(command, &scene_names, statuses, max_scenes)
}

/// Parse and dispatch with explicit host capabilities.
#[must_use]
pub fn run_with_capabilities<I, S>(
    args: I,
    fs: Arc<dyn FileSystem>,
    runner: Arc<dyn fmn_platform::process::ProcessRunner>,
    locator: &dyn fmn_platform::process::FfmpegLocator,
) -> RunOutput
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let args: Vec<String> = args.into_iter().map(Into::into).collect();
    let requested_robot = args
        .iter()
        .take_while(|arg| arg.as_str() != "--")
        .any(|arg| arg == "--robot");
    let invocation = match parse_args(args) {
        Ok(invocation) => invocation,
        Err(error) => return error_output(requested_robot, &error),
    };
    match invocation {
        Invocation::Help { command, robot } => RunOutput::success(if robot {
            robot_help(command)
        } else {
            human_help(command)
        }),
        Invocation::Version { robot } => RunOutput::success(if robot {
            format!(
                "{{\"schema\":\"fmn.cli\",\"version\":{},\"kind\":\"version\",\
                 \"program\":\"fmn\",\"program_version\":{}}}\n",
                ROBOT_SCHEMA_VERSION,
                json_string(env!("CARGO_PKG_VERSION"))
            )
        } else {
            format!("fmn {}\n", env!("CARGO_PKG_VERSION"))
        }),
        Invocation::Doctor(command) => match collect_doctor_snapshot(
            fs.as_ref(),
            runner.as_ref(),
            locator,
            &command,
        ) {
            Ok(snapshot) => {
                let stdout = if command.common.robot {
                    match snapshot.to_ndjson() {
                        Ok(stdout) => stdout,
                        Err(error) => return error_output(true, &error),
                    }
                } else {
                    snapshot.to_human()
                };
                if command.require_ffmpeg && !snapshot.ffmpeg.is_available() {
                    let error = CliError::new(
                        "capability",
                        "ffmpeg was required but is unavailable; native PNG-sequence, GIF, and y4m outputs remain available",
                    );
                    let mut output = error_output(command.common.robot, &error);
                    if command.common.robot {
                        output.stdout = format!("{stdout}{}", output.stdout);
                    } else {
                        output.stdout = stdout;
                    }
                    output
                } else {
                    RunOutput::success(stdout)
                }
            }
            Err(error) => error_output(command.common.robot, &error),
        },
        Invocation::ClearCache { common } => clear_cache(fs.as_ref(), &common),
        Invocation::Render(command) => python_source_refusal(&command).unwrap_or_else(|| {
            match execute_native_render(Arc::clone(&fs), Arc::clone(&runner), locator, &command) {
                Ok(reports) => successful_render_output(&command, reports),
                Err(error) => error_output(command.common.robot, &error),
            }
        }),
        Invocation::Batch(command) => {
            if let Some(output) = python_source_refusal(&command.render) {
                return output;
            }
            #[cfg(feature = "batch")]
            {
                execute_batch(Arc::clone(&fs), Arc::clone(&runner), locator, &command)
            }
            #[cfg(not(feature = "batch"))]
            {
                error_output(
                    command.render.common.robot,
                    &CliError::new(
                        "capability",
                        "batch support is disabled in this binary; rebuild with the `batch` feature",
                    ),
                )
            }
        }
        Invocation::Studio(command) => python_source_refusal(&command.render).unwrap_or_else(|| {
            error_output(
                command.render.common.robot,
                &CliError::new(
                    "capability",
                    "Studio composition is unavailable: no concrete WorkerService or audited host-entropy capability is registered",
                ),
            )
        }),
    }
}

fn python_source_refusal(command: &RenderCommand) -> Option<RunOutput> {
    (command.scene_source_kind() == Some(SceneSourceKind::Python)).then(|| {
        error_output(
            command.common.robot,
            &CliError::new("capability", PYTHON_SOURCE_PORTAL_MESSAGE),
        )
    })
}

fn clear_cache(fs: &dyn FileSystem, common: &CommonOptions) -> RunOutput {
    if !fs.grants_host_destructive_lifecycle() {
        return error_output(
            common.robot,
            &CliError::new(
                "capability",
                "--clear-cache requires an explicit host-filesystem lifecycle capability",
            ),
        );
    }
    let config = match resolve_common_config(fs, common) {
        Ok(config) => config,
        Err(error) => return error_output(common.robot, &error),
    };
    let root_path = match fmn_cache::resolve_host_cache_root(&config.directories.cache) {
        Ok(root) => root,
        Err(error) => return error_output(common.robot, &cache_root_cli_error(error)),
    };
    let root = match strict_path_text(&root_path, "cache root") {
        Ok(root) => root.to_owned(),
        Err(error) => return error_output(common.robot, &error),
    };
    let authorization = match fmn_cache::CacheClearAuthorization::authorize(&root_path) {
        Ok(authorization) => authorization,
        Err(error) => {
            let exit_name = if matches!(error, fmn_cache::CacheError::RootRefused { .. }) {
                "config"
            } else {
                "capability"
            };
            return error_output(common.robot, &CliError::new(exit_name, error.to_string()));
        }
    };
    let outcome = match authorization.clear() {
        Ok(outcome) => outcome,
        Err(error) => {
            let exit_name = if matches!(error, fmn_cache::CacheError::RootRefused { .. }) {
                "config"
            } else {
                "capability"
            };
            return error_output(common.robot, &CliError::new(exit_name, error.to_string()));
        }
    };
    if common.robot {
        RunOutput::success(format!(
            "{{\"schema\":\"fmn.cli\",\"version\":{},\"kind\":\"cache_clear\",\
             \"root\":{},\"outcome\":{}}}\n",
            ROBOT_SCHEMA_VERSION,
            json_string(&root),
            json_string(match outcome {
                fmn_cache::CacheClearOutcome::Cleared => "cleared",
                fmn_cache::CacheClearOutcome::AlreadyAbsent => "already_absent",
            })
        ))
    } else if common.quiet {
        RunOutput::success(String::new())
    } else {
        RunOutput::success(match outcome {
            fmn_cache::CacheClearOutcome::Cleared => format!("cleared cache: {root}\n"),
            fmn_cache::CacheClearOutcome::AlreadyAbsent => {
                format!("cache already absent: {root}\n")
            }
        })
    }
}

struct SelfWorkerBuilder {
    executable: PathBuf,
    argv: Vec<String>,
    cwd: PathBuf,
    build_id: fmn_studio::ProtocolDigest,
}

impl fmn_studio::RebuildDriver for SelfWorkerBuilder {
    fn rebuild(&mut self) -> Result<fmn_studio::WorkerArtifact, fmn_studio::BuildError> {
        Ok(fmn_studio::WorkerArtifact {
            executable: self.executable.clone(),
            argv: self.argv.clone(),
            env: Vec::new(),
            cwd: Some(self.cwd.clone()),
            build_id: self.build_id,
        })
    }
}

fn os_args_to_utf8(args: &[OsString]) -> Result<Vec<String>, CliError> {
    let mut utf8 = Vec::new();
    utf8.try_reserve_exact(args.len())
        .map_err(|error| internal(format!("command argument storage failed: {error}")))?;
    for arg in args {
        utf8.push(
            arg.clone()
                .into_string()
                .map_err(|_| usage("command-line arguments must be valid UTF-8"))?,
        );
    }
    Ok(utf8)
}

/// Whether these arguments select the public Studio command.
#[must_use]
pub fn is_studio_invocation_os(args: &[OsString]) -> bool {
    os_args_to_utf8(args)
        .and_then(parse_args)
        .is_ok_and(|invocation| matches!(invocation, Invocation::Studio(_)))
}

/// Whether these arguments select the private disposable-worker entry point.
#[must_use]
pub fn is_internal_studio_worker_os(args: &[OsString]) -> bool {
    args.first()
        .is_some_and(|arg| arg == OsStr::new(INTERNAL_STUDIO_WORKER_ARG))
}

fn internal_worker_failure(error: impl fmt::Display) -> RunOutput {
    RunOutput {
        code: exit_code("internal"),
        stdout: String::new(),
        stderr: format!("fmn Studio worker: {error}\n"),
    }
}

/// Serve the private Studio protocol over this process's stdin/stdout.
///
/// The caller must first check [`is_internal_studio_worker_os`]. Protocol
/// bytes are written directly to stdout, so returned diagnostics never use
/// stdout even when the public invocation carried `--robot`.
#[must_use]
pub fn run_internal_studio_worker_os(args: &[OsString]) -> RunOutput {
    let Some(public_args) = args.get(1..) else {
        return internal_worker_failure("missing public Studio invocation");
    };
    let utf8 = match os_args_to_utf8(public_args) {
        Ok(args) => args,
        Err(error) => return internal_worker_failure(error),
    };
    let command = match parse_args(utf8) {
        Ok(Invocation::Studio(command)) => command,
        Ok(_) => return internal_worker_failure("worker argv is not a Studio invocation"),
        Err(error) => return internal_worker_failure(error),
    };
    if command.render.scene_source_kind() == Some(SceneSourceKind::Python) {
        return internal_worker_failure(PYTHON_SOURCE_PORTAL_MESSAGE);
    }
    let mut service = match NativeStudioWorker::from_command(&fmn_platform::fs::StdFs, &command) {
        Ok(service) => service,
        Err(error) => return internal_worker_failure(error),
    };
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let outcome = fmn_studio::serve_worker(
        &mut service,
        &mut stdin.lock(),
        &mut stdout.lock(),
        fmn_studio::ProtocolLimits::default(),
    );
    match outcome {
        Ok(fmn_studio::WorkerServeOutcome::Shutdown)
        | Ok(fmn_studio::WorkerServeOutcome::PeerClosed) => RunOutput::success(String::new()),
        Ok(fmn_studio::WorkerServeOutcome::HandshakeRejected) => {
            internal_worker_failure("supervisor handshake was rejected")
        }
        Ok(fmn_studio::WorkerServeOutcome::Crashed(report)) => {
            internal_worker_failure(report.message)
        }
        Err(error) => internal_worker_failure(error),
    }
}

fn studio_scene_name(fs: &dyn FileSystem, command: &RenderCommand) -> Result<String, CliError> {
    match resolve_native_render_input(fs, command)? {
        NativeRenderInput::Builtin { mut names } => {
            if names.len() != 1 {
                return Err(CliError::new(
                    "scene",
                    "Studio requires exactly one scene name",
                ));
            }
            names
                .pop()
                .ok_or_else(|| CliError::new("scene", "Studio requires exactly one scene name"))
        }
        NativeRenderInput::Compiled { name, .. } => Ok(name),
    }
}

fn studio_cache(
    clock: Arc<dyn fmn_platform::clock::Clock>,
) -> Result<fmn_cache::Namespace, CliError> {
    let root = if cfg!(windows) {
        PathBuf::from(r"C:\fmn-studio-session-cache")
    } else {
        PathBuf::from("/fmn-studio-session-cache")
    };
    fmn_cache::Store::open(
        Arc::new(fmn_platform::fs::VirtualFs::new()),
        clock,
        root,
        fmn_cache::StoreConfig::default(),
    )
    .and_then(|store| {
        store.namespace(
            "studio-replay",
            1,
            fmn_cache::NamespacePolicy {
                ceiling_bytes: None,
            },
        )
    })
    .map_err(|error| internal(format!("Studio replay cache: {error}")))
}

fn write_studio_ready(
    ready: &mut dyn std::io::Write,
    command: &StudioCommand,
    scene: &str,
    url: &str,
    generation: u64,
) -> Result<(), CliError> {
    let text = if command.render.common.robot {
        format!(
            "{{\"schema\":\"fmn.cli\",\"version\":{},\"kind\":\"studio_ready\",\
             \"scene\":{},\"url\":{},\"preview_codec\":\"png\",\
             \"browser_launch\":{},\"checkpoint_frames\":{},\"worker_generation\":{}}}\n",
            ROBOT_SCHEMA_VERSION,
            json_string(scene),
            json_string(url),
            json_string(if command.no_browser {
                "suppressed"
            } else {
                "manual"
            }),
            command.checkpoint_frames,
            generation,
        )
    } else {
        let launch = if command.no_browser {
            "browser launch suppressed"
        } else {
            "open manually; the one-external-tool policy forbids spawning a browser"
        };
        format!("Studio ready for {scene}: {url} ({launch})\n")
    };
    ready
        .write_all(text.as_bytes())
        .and_then(|()| ready.flush())
        .map_err(|error| internal(format!("could not publish Studio launch URL: {error}")))
}

fn studio_terminal_protocol(
    term: Option<&OsStr>,
    term_program: Option<&OsStr>,
    kitty_window_id: Option<&OsStr>,
) -> fmn_studio::TerminalProtocol {
    if term == Some(OsStr::new("xterm-kitty"))
        || term_program == Some(OsStr::new("kitty"))
        || kitty_window_id.is_some_and(|value| !value.is_empty())
    {
        fmn_studio::TerminalProtocol::Kitty
    } else {
        fmn_studio::TerminalProtocol::Sixel
    }
}

fn write_studio_terminal_frame(
    preview: fmn_studio::TerminalPreview,
    mut writer: &mut dyn std::io::Write,
    frame: &fmn_studio::PngFrame,
) -> Result<(), CliError> {
    let result = match preview.protocol() {
        fmn_studio::TerminalProtocol::Kitty => preview.write_png(&mut writer, &frame.png),
        fmn_studio::TerminalProtocol::Sixel => {
            let decoded = fmn_codec::decode_png(
                &frame.png,
                &fmn_codec::PngLimits {
                    max_pixels: u64::from(frame.width) * u64::from(frame.height),
                    ..fmn_codec::PngLimits::default()
                },
            )
            .map_err(|error| CliError::new("render", format!("Studio TUI frame: {error}")))?;
            preview.write_rgba8(&mut writer, decoded.width, decoded.height, &decoded.rgba)
        }
    };
    result.map_err(|error| CliError::new("render", format!("Studio TUI frame: {error}")))
}

fn serve_studio_tui(
    host: fmn_studio::StudioHost,
    frames: &fmn_studio::FrameHub,
    ready: &mut dyn std::io::Write,
    shutdown: &AtomicBool,
) -> Result<(), CliError> {
    let protocol = studio_terminal_protocol(
        std::env::var_os("TERM").as_deref(),
        std::env::var_os("TERM_PROGRAM").as_deref(),
        std::env::var_os("KITTY_WINDOW_ID").as_deref(),
    );
    let preview = fmn_studio::TerminalPreview::new(protocol, fmn_studio::TuiLimits::default())
        .map_err(|error| CliError::new("capability", format!("Studio TUI: {error}")))?;

    std::thread::scope(|scope| {
        let server = scope.spawn(|| host.serve_until(shutdown));
        let mut last_publication = None;
        let mut terminal_result = Ok(());
        while !shutdown.load(Ordering::Acquire) && !server.is_finished() {
            let Some(frame) = frames.wait_after(last_publication, Duration::from_millis(50)) else {
                continue;
            };
            last_publication = Some(frame.publication_sequence);
            if let Err(error) = write_studio_terminal_frame(preview, ready, &frame) {
                shutdown.store(true, Ordering::Release);
                terminal_result = Err(error);
                break;
            }
        }
        let server_result = server
            .join()
            .map_err(|_| internal("Studio host thread panicked"))?
            .map_err(|error| CliError::new("scene", format!("Studio host: {error}")));
        terminal_result.and(server_result)
    })
}

fn execute_studio(
    public_args: Vec<String>,
    command: StudioCommand,
    ready: &mut dyn std::io::Write,
    shutdown: &AtomicBool,
) -> Result<(), CliError> {
    if command.tui && command.render.common.robot {
        return Err(usage(
            "--tui writes terminal escape records and cannot be combined with --robot",
        ));
    }
    if command.preview_codec != PreviewCodec::Png {
        return Err(CliError::new(
            "capability",
            "MJPEG Studio preview is not yet wired; use --preview-codec png",
        ));
    }
    let fs: Arc<dyn FileSystem> = Arc::new(fmn_platform::fs::StdFs);
    let scene = studio_scene_name(fs.as_ref(), &command.render)?;
    let mut token_bytes = [0_u8; 32];
    fmn_platform::entropy::HostEntropy::fill(
        &fmn_platform::entropy::StdHostEntropy,
        &mut token_bytes,
    )
    .map_err(|error| CliError::new("capability", error.to_string()))?;
    let token = fmn_studio::CapabilityToken::new(token_bytes)
        .map_err(|error| CliError::new("capability", error.to_string()))?;
    let executable = std::env::current_exe().map_err(|error| {
        internal(format!(
            "cannot resolve the Studio worker executable: {error}"
        ))
    })?;
    let cwd = std::env::current_dir().map_err(|error| {
        internal(format!(
            "cannot resolve the Studio working directory: {error}"
        ))
    })?;
    if !executable.is_absolute() || !cwd.is_absolute() {
        return Err(internal(
            "the Studio worker executable and working directory must be absolute",
        ));
    }
    let build_id = fmn_studio::protocol_digest(BUILD_ID.as_bytes());
    let mut worker_argv = Vec::new();
    worker_argv
        .try_reserve_exact(public_args.len().saturating_add(1))
        .map_err(|error| internal(format!("Studio worker argv storage failed: {error}")))?;
    worker_argv.push(INTERNAL_STUDIO_WORKER_ARG.to_owned());
    worker_argv.extend(public_args);
    let mut builder = SelfWorkerBuilder {
        executable,
        argv: worker_argv,
        cwd,
        build_id,
    };
    let clock: Arc<dyn fmn_platform::clock::Clock> = Arc::new(fmn_platform::clock::StdClock::new());
    let protocol_limits = fmn_studio::ProtocolLimits::default();
    let mut supervisor = fmn_studio::Supervisor::new(
        Box::new(fmn_studio::StdWorkerLauncher::default()),
        Arc::clone(&clock),
        studio_cache(Arc::clone(&clock))?,
        fmn_studio::SupervisorConfig {
            protocol_limits,
            supervisor_build_id: build_id,
            ..fmn_studio::SupervisorConfig::default()
        },
    );
    supervisor
        .install_session(scene.clone(), fmn_scene::Journal::new())
        .and_then(|()| supervisor.build_and_start(&mut builder))
        .map_err(|error| CliError::new("scene", format!("Studio worker startup: {error}")))?;
    let generation = supervisor.generation();
    let initial = supervisor
        .request(
            fmn_studio::SupervisorRequest::Scrub {
                scene: scene.clone(),
                frame: 0,
            },
            &|_| false,
        )
        .map_err(|error| CliError::new("render", format!("Studio first frame: {error}")))?;
    let initial = match initial {
        fmn_studio::SupervisorReply::Worker(fmn_studio::WorkerResponse::Frame(frame)) => frame,
        fmn_studio::SupervisorReply::Worker(_) => {
            return Err(internal("Studio worker omitted the first preview frame"));
        }
        fmn_studio::SupervisorReply::Recovered { crash, .. } => {
            return Err(CliError::new(
                "scene",
                format!(
                    "Studio worker crashed during first frame: {}",
                    crash.message
                ),
            ));
        }
    };
    let frames = fmn_studio::FrameHub::new(4, protocol_limits.max_frame_bytes)
        .map_err(|error| internal(format!("Studio frame hub: {error}")))?;
    frames
        .publish(&initial, protocol_limits)
        .map_err(|error| CliError::new("render", format!("Studio first frame: {error}")))?;
    let tui_frames = command.tui.then(|| frames.clone());
    let asset_fs = Arc::clone(&fs);
    let asset_ok: Arc<dyn Fn(&AssetRead) -> bool + Send + Sync> = Arc::new(move |read| {
        asset_fs
            .read_bounded(Path::new(&read.path), DEFAULT_MAX_BUNDLE_BYTES)
            .is_ok_and(|bytes| fmn_studio::protocol_digest(&bytes) == read.digest)
    });
    let session = Arc::new(
        fmn_studio::StudioWorkerSession::new(&scene, supervisor, Box::new(builder), asset_ok)
            .map_err(|error| internal(format!("Studio session: {error}")))?,
    );
    let host = match fmn_studio::StudioHost::bind(
        Arc::clone(&session),
        frames,
        token,
        clock,
        fmn_studio::StudioHostConfig {
            bind_addr: std::net::SocketAddr::new(command.bind, command.port),
            ..fmn_studio::StudioHostConfig::default()
        },
    ) {
        Ok(host) => host,
        Err(error) => {
            session.shutdown_worker();
            return Err(CliError::new("capability", format!("Studio host: {error}")));
        }
    };
    let result = host
        .launch_url()
        .map_err(|error| internal(format!("Studio launch URL: {error}")))
        .and_then(|url| write_studio_ready(ready, &command, &scene, &url, generation))
        .and_then(|()| match tui_frames.as_ref() {
            Some(frames) => serve_studio_tui(host, frames, ready, shutdown),
            None => host
                .serve_until(shutdown)
                .map_err(|error| CliError::new("scene", format!("Studio host: {error}"))),
        });
    session.shutdown_worker();
    result
}

/// Run the public Studio composition while publishing its capability-bearing
/// launch URL before entering the server loop.
#[must_use]
pub fn run_studio_os(
    args: &[OsString],
    ready: &mut dyn std::io::Write,
    shutdown: &AtomicBool,
) -> RunOutput {
    let requested_robot = args
        .iter()
        .take_while(|arg| arg.as_os_str() != OsStr::new("--"))
        .any(|arg| arg == OsStr::new("--robot"));
    let utf8 = match os_args_to_utf8(args) {
        Ok(args) => args,
        Err(error) => return error_output(requested_robot, &error),
    };
    let invocation = match parse_args(utf8.clone()) {
        Ok(invocation) => invocation,
        Err(error) => return error_output(requested_robot, &error),
    };
    let Invocation::Studio(command) = invocation else {
        return error_output(
            requested_robot,
            &internal("run_studio_os received a non-Studio invocation"),
        );
    };
    if let Some(output) = python_source_refusal(&command.render) {
        return output;
    }
    match execute_studio(utf8, command, ready, shutdown) {
        Ok(()) => RunOutput::success(String::new()),
        Err(error) => error_output(requested_robot, &error),
    }
}

/// Dispatch against production filesystem/process capabilities.
#[must_use]
pub fn run<I, S>(args: I) -> RunOutput
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let locator = fmn_platform::process::StdFfmpegLocator::from_host_path();
    run_with_capabilities(
        args,
        Arc::new(fmn_platform::fs::StdFs),
        Arc::new(fmn_platform::process::StdProcessRunner),
        &locator,
    )
}

/// Dispatch operating-system arguments without panicking on non-UTF-8 input.
///
/// The versioned CLI grammar is UTF-8. An argument outside that grammar is a
/// stable usage error; it is never lossily rewritten into a different path.
#[must_use]
pub fn run_os<I, S>(args: I) -> RunOutput
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
    let requested_robot = args
        .iter()
        .take_while(|arg| arg.as_os_str() != OsStr::new("--"))
        .any(|arg| arg == OsStr::new("--robot"));
    let mut utf8 = Vec::with_capacity(args.len());
    for arg in args {
        let Ok(arg) = arg.into_string() else {
            return error_output(
                requested_robot,
                &usage("command-line arguments must be valid UTF-8"),
            );
        };
        utf8.push(arg);
    }
    run(utf8)
}

fn error_output(robot: bool, error: &CliError) -> RunOutput {
    if robot {
        RunOutput {
            code: error.code(),
            stdout: format!(
                "{{\"schema\":\"fmn.cli\",\"version\":{},\"kind\":\"error\",\
                 \"exit_code\":{},\"exit_name\":{},\"rule\":{},\"message\":{}}}\n",
                ROBOT_SCHEMA_VERSION,
                error.code(),
                json_string(error.exit_name()),
                json_option(error.rule()),
                json_string(error.message()),
            ),
            stderr: String::new(),
        }
    } else {
        RunOutput {
            code: error.code(),
            stdout: String::new(),
            stderr: format!("fmn: {error}\n"),
        }
    }
}

fn human_help(command: CommandScope) -> String {
    let mut out = String::new();
    let _ = match command {
        CommandScope::Doctor => writeln!(out, "Usage: fmn doctor [OPTIONS]"),
        CommandScope::Render | CommandScope::Batch | CommandScope::Studio => writeln!(
            out,
            "Usage: fmn {} [OPTIONS] [NATIVE_SCENE] [SCENE ...]",
            scope_name(command)
        ),
        CommandScope::Global => writeln!(out, "Usage: fmn [OPTIONS] [NATIVE_SCENE] [SCENE ...]"),
    };
    out.push_str("\nCommands:\n");
    for subcommand in SUBCOMMAND_SPECS {
        let _ = writeln!(
            out,
            "  {:<10} {}",
            scope_name(subcommand.command),
            subcommand.help
        );
    }
    out.push_str("\nOptions:\n");
    for spec in FLAG_SPECS
        .iter()
        .filter(|spec| help_scope_accepts(command, spec.command))
        .filter(|spec| spec.options.iter().any(|option| option.starts_with('-')))
    {
        let _ = writeln!(out, "  {:<34} {}", spec.options.join(", "), spec.help);
    }
    out
}

fn robot_help(command: CommandScope) -> String {
    let mut out = String::new();
    for subcommand in SUBCOMMAND_SPECS {
        let _ = writeln!(
            out,
            "{{\"schema\":\"fmn.cli\",\"version\":{},\"kind\":\"subcommand\",\
             \"name\":{},\"status\":{},\"help\":{}}}",
            ROBOT_SCHEMA_VERSION,
            json_string(scope_name(subcommand.command)),
            json_string(flag_status_name(subcommand.status)),
            json_string(subcommand.help),
        );
    }
    for spec in FLAG_SPECS
        .iter()
        .filter(|spec| help_scope_accepts(command, spec.command))
    {
        let options: Vec<String> = spec
            .options
            .iter()
            .map(|option| (*option).to_owned())
            .collect();
        let _ = writeln!(
            out,
            "{{\"schema\":\"fmn.cli\",\"version\":{},\"kind\":\"flag\",\
             \"command\":{},\"options\":{},\"binding\":{},\"action\":{},\
             \"arity\":{},\"status\":{},\"source\":{},\"help\":{}}}",
            ROBOT_SCHEMA_VERSION,
            json_string(scope_name(spec.command)),
            json_array(&options),
            json_string(spec.binding),
            json_string(match spec.action {
                FlagAction::SetTrue => "store_true",
                FlagAction::Store => "store",
            }),
            json_string(match spec.arity {
                FlagArity::None => "none",
                FlagArity::One => "one",
                FlagArity::Optional => "optional",
                FlagArity::Many => "many",
            }),
            json_string(flag_status_name(spec.status)),
            json_string(match spec.source {
                FlagSource::Reference => "reference",
                FlagSource::Native => "native",
            }),
            json_string(spec.help),
        );
    }
    for exit in EXIT_CODE_SPECS {
        let _ = writeln!(
            out,
            "{{\"schema\":\"fmn.cli\",\"version\":{},\"kind\":\"exit_code\",\
             \"code\":{},\"name\":{},\"meaning\":{}}}",
            ROBOT_SCHEMA_VERSION,
            exit.code,
            json_string(exit.name),
            json_string(exit.meaning),
        );
    }
    for interaction in INTERACTION_SPECS {
        let operands: Vec<String> = interaction
            .operands
            .iter()
            .map(|operand| (*operand).to_owned())
            .collect();
        let _ = writeln!(
            out,
            "{{\"schema\":\"fmn.cli\",\"version\":{},\"kind\":\"interaction\",\
             \"id\":{},\"interaction\":{},\"operands\":{},\"exit_name\":{},\
             \"message\":{}}}",
            ROBOT_SCHEMA_VERSION,
            json_string(interaction.id),
            json_string(interaction_kind_name(interaction.kind)),
            json_array(&operands),
            json_option(interaction.exit_code),
            json_string(interaction.message),
        );
    }
    out
}

fn help_scope_accepts(selected: CommandScope, declared: CommandScope) -> bool {
    if selected == CommandScope::Global {
        matches!(declared, CommandScope::Global | CommandScope::Render)
    } else {
        scope_accepts(selected, declared)
    }
}

const fn interaction_kind_name(kind: InteractionKind) -> &'static str {
    match kind {
        InteractionKind::AtMostOne => "at_most_one",
        InteractionKind::Conflicts => "conflicts",
        InteractionKind::RequiresAny => "requires_any",
        InteractionKind::Implies => "implies",
        InteractionKind::Exclusive => "exclusive",
    }
}

const fn flag_status_name(status: FlagStatus) -> &'static str {
    match status {
        FlagStatus::Same => "same",
        FlagStatus::Improved => "improved",
        FlagStatus::Tiered => "tiered",
        FlagStatus::Excluded => "excluded",
        FlagStatus::Unreviewed => "unreviewed",
    }
}

fn json_option(value: Option<&str>) -> String {
    value.map_or_else(|| "null".to_owned(), json_string)
}

fn json_array(values: &[String]) -> String {
    let mut out = String::from("[");
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&json_string(value));
    }
    out.push(']');
    out
}

fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len().saturating_add(2));
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch <= '\u{1f}' => {
                let _ = write!(out, "\\u{:04x}", u32::from(ch));
            }
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]

    use super::*;
    use fmn_config::config::{DeterminismMode, Engine, ThreadPolicy};
    use fmn_platform::fs::VirtualFs;

    fn no_ffmpeg_locator() -> fmn_platform::process::StdFfmpegLocator {
        fmn_platform::process::StdFfmpegLocator::default()
    }

    fn fs_capability(fs: &Arc<VirtualFs>) -> Arc<dyn FileSystem> {
        Arc::clone(fs) as Arc<dyn FileSystem>
    }

    #[test]
    fn studio_terminal_protocol_prefers_kitty_and_falls_back_to_sixel() {
        assert_eq!(
            studio_terminal_protocol(Some(OsStr::new("xterm-kitty")), None, None),
            fmn_studio::TerminalProtocol::Kitty
        );
        assert_eq!(
            studio_terminal_protocol(None, Some(OsStr::new("kitty")), None),
            fmn_studio::TerminalProtocol::Kitty
        );
        assert_eq!(
            studio_terminal_protocol(None, None, Some(OsStr::new("1"))),
            fmn_studio::TerminalProtocol::Kitty
        );
        assert_eq!(
            studio_terminal_protocol(Some(OsStr::new("xterm-256color")), None, None),
            fmn_studio::TerminalProtocol::Sixel
        );

        let png = fmn_codec::encode_rgba8(
            2,
            1,
            &[255, 0, 0, 255, 0, 255, 0, 255],
            fmn_codec::CompressionLevel::Fast,
        );
        let frame = fmn_studio::PngFrame {
            publication_sequence: 0,
            scene: "terminal-fixture".to_owned(),
            frame_index: 0,
            width: 2,
            height: 1,
            digest: fmn_studio::protocol_digest(&png),
            png,
        };
        for (protocol, prefix) in [
            (fmn_studio::TerminalProtocol::Kitty, b"\x1b_G".as_slice()),
            (
                fmn_studio::TerminalProtocol::Sixel,
                b"\x1bP0;0;0q".as_slice(),
            ),
        ] {
            let preview =
                fmn_studio::TerminalPreview::new(protocol, fmn_studio::TuiLimits::default())
                    .expect("valid terminal adapter");
            let mut output = Vec::new();
            write_studio_terminal_frame(preview, &mut output, &frame)
                .expect("validated host PNG reaches the terminal adapter");
            assert!(output.starts_with(prefix));
        }
    }

    #[cfg(unix)]
    fn write_native_ffmpeg_fixture(path: &Path) {
        use std::os::unix::fs::PermissionsExt as _;

        let bytes = if cfg!(target_os = "macos") {
            const HEADER_BYTES: usize = 32;
            const SEGMENT_BYTES: usize = 72;
            const ENTRY_BYTES: usize = 24;

            let mut bytes = vec![0_u8; HEADER_BYTES + 2 * SEGMENT_BYTES + ENTRY_BYTES + 1];
            bytes[..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
            let cpu = if cfg!(target_arch = "x86_64") {
                0x0100_0007_u32
            } else {
                0x0100_000c_u32
            };
            bytes[4..8].copy_from_slice(&cpu.to_le_bytes());
            let subtype = if cfg!(target_arch = "x86_64") {
                3_u32
            } else {
                0_u32
            };
            bytes[8..12].copy_from_slice(&subtype.to_le_bytes());
            bytes[12..16].copy_from_slice(&2_u32.to_le_bytes());
            bytes[16..20].copy_from_slice(&3_u32.to_le_bytes());
            bytes[20..24]
                .copy_from_slice(&((2 * SEGMENT_BYTES + ENTRY_BYTES) as u32).to_le_bytes());
            let image_bytes = bytes.len() as u64;
            let pagezero = &mut bytes[HEADER_BYTES..HEADER_BYTES + SEGMENT_BYTES];
            pagezero[..4].copy_from_slice(&0x19_u32.to_le_bytes());
            pagezero[4..8].copy_from_slice(&(SEGMENT_BYTES as u32).to_le_bytes());
            pagezero[8..18].copy_from_slice(b"__PAGEZERO");
            pagezero[32..40].copy_from_slice(&0x1_0000_0000_u64.to_le_bytes());
            let text_offset = HEADER_BYTES + SEGMENT_BYTES;
            let text = &mut bytes[text_offset..text_offset + SEGMENT_BYTES];
            text[..4].copy_from_slice(&0x19_u32.to_le_bytes());
            text[4..8].copy_from_slice(&(SEGMENT_BYTES as u32).to_le_bytes());
            text[8..14].copy_from_slice(b"__TEXT");
            text[24..32].copy_from_slice(&0x1_0000_0000_u64.to_le_bytes());
            text[32..40].copy_from_slice(&image_bytes.to_le_bytes());
            text[48..56].copy_from_slice(&image_bytes.to_le_bytes());
            text[56..60].copy_from_slice(&7_u32.to_le_bytes());
            text[60..64].copy_from_slice(&5_u32.to_le_bytes());
            let entry_offset = HEADER_BYTES + 2 * SEGMENT_BYTES;
            let entry = &mut bytes[entry_offset..entry_offset + ENTRY_BYTES];
            entry[..4].copy_from_slice(&0x8000_0028_u32.to_le_bytes());
            entry[4..8].copy_from_slice(&(ENTRY_BYTES as u32).to_le_bytes());
            entry[8..16].copy_from_slice(
                &((HEADER_BYTES + 2 * SEGMENT_BYTES + ENTRY_BYTES) as u64).to_le_bytes(),
            );
            *bytes.last_mut().expect("entry byte") = 0xc3;
            bytes
        } else {
            const HEADER_BYTES: usize = 64;
            const PROGRAM_BYTES: usize = 56;

            let mut bytes = vec![0_u8; HEADER_BYTES + PROGRAM_BYTES];
            bytes[..4].copy_from_slice(b"\x7fELF");
            bytes[4] = 2;
            bytes[5] = 1;
            bytes[6] = 1;
            bytes[16..18].copy_from_slice(&3_u16.to_le_bytes());
            let machine = if cfg!(target_arch = "x86_64") {
                62_u16
            } else {
                183_u16
            };
            bytes[18..20].copy_from_slice(&machine.to_le_bytes());
            bytes[20..24].copy_from_slice(&1_u32.to_le_bytes());
            bytes[24..32].copy_from_slice(&0x1000_u64.to_le_bytes());
            bytes[32..40].copy_from_slice(&(HEADER_BYTES as u64).to_le_bytes());
            bytes[52..54].copy_from_slice(&(HEADER_BYTES as u16).to_le_bytes());
            bytes[54..56].copy_from_slice(&(PROGRAM_BYTES as u16).to_le_bytes());
            bytes[56..58].copy_from_slice(&1_u16.to_le_bytes());
            let image_bytes = bytes.len() as u64;
            let program = &mut bytes[HEADER_BYTES..];
            program[..4].copy_from_slice(&1_u32.to_le_bytes());
            program[4..8].copy_from_slice(&5_u32.to_le_bytes());
            program[16..24].copy_from_slice(&0x1000_u64.to_le_bytes());
            program[32..40].copy_from_slice(&image_bytes.to_le_bytes());
            program[40..48].copy_from_slice(&image_bytes.to_le_bytes());
            program[48..56].copy_from_slice(&0x1000_u64.to_le_bytes());
            bytes
        };
        std::fs::write(path, bytes).expect("write native ffmpeg fixture");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .expect("mark native ffmpeg fixture executable");
    }

    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    struct SuccessfulFfmpegProbeRunner;

    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    impl fmn_platform::process::ProcessRunner for SuccessfulFfmpegProbeRunner {
        fn mechanism(&self) -> fmn_platform::process::ProcessMechanism {
            fmn_platform::process::ProcessMechanism::Scripted
        }

        fn start(
            &self,
            spec: &fmn_platform::process::ProcessSpec,
            _cancellation: fmn_platform::process::ProcessCancellation,
            _stdin_limits: fmn_platform::process::ProcessStdinLimits,
        ) -> Result<
            Box<dyn fmn_platform::process::RunningProcess>,
            fmn_platform::process::ProcessError,
        > {
            Err(fmn_platform::process::ProcessError::NotScripted {
                program: spec.program.clone(),
            })
        }

        fn run(
            &self,
            spec: &fmn_platform::process::ProcessSpec,
        ) -> Result<fmn_platform::process::ProcessOutcome, fmn_platform::process::ProcessError>
        {
            let stdout = if spec.argv == ["-version"] {
                b"ffmpeg version cli-locator-fixture\n".to_vec()
            } else if spec.argv == ["-hide_banner", "-encoders"] {
                b"Encoders:\n ------\n V....D h264_nvenc fixture\n".to_vec()
            } else {
                return Err(fmn_platform::process::ProcessError::NotScripted {
                    program: spec.program.clone(),
                });
            };
            Ok(fmn_platform::process::ProcessOutcome {
                termination: fmn_platform::process::ProcessTermination::Exited(Some(0)),
                stdout,
                stderr: Vec::new(),
            })
        }
    }

    #[cfg(unix)]
    struct IdentityChangingProbeRunner {
        source: PathBuf,
    }

    #[cfg(unix)]
    impl fmn_platform::process::ProcessRunner for IdentityChangingProbeRunner {
        fn mechanism(&self) -> fmn_platform::process::ProcessMechanism {
            fmn_platform::process::ProcessMechanism::Scripted
        }

        fn start(
            &self,
            spec: &fmn_platform::process::ProcessSpec,
            _cancellation: fmn_platform::process::ProcessCancellation,
            _stdin_limits: fmn_platform::process::ProcessStdinLimits,
        ) -> Result<
            Box<dyn fmn_platform::process::RunningProcess>,
            fmn_platform::process::ProcessError,
        > {
            Err(fmn_platform::process::ProcessError::NotScripted {
                program: spec.program.clone(),
            })
        }

        fn run(
            &self,
            spec: &fmn_platform::process::ProcessSpec,
        ) -> Result<fmn_platform::process::ProcessOutcome, fmn_platform::process::ProcessError>
        {
            let stdout = if spec.argv == ["-version"] {
                b"ffmpeg version cli-fixture\n".to_vec()
            } else if spec.argv == ["-hide_banner", "-encoders"] {
                std::fs::write(&self.source, b"changed executable identity").map_err(|error| {
                    fmn_platform::process::ProcessError::Plumbing {
                        program: spec.program.clone(),
                        detail: format!("replace fixture executable: {error}"),
                    }
                })?;
                b"Encoders:\n ------\n V....D libx264 fixture\n".to_vec()
            } else {
                return Err(fmn_platform::process::ProcessError::NotScripted {
                    program: spec.program.clone(),
                });
            };
            Ok(fmn_platform::process::ProcessOutcome {
                termination: fmn_platform::process::ProcessTermination::Exited(Some(0)),
                stdout,
                stderr: Vec::new(),
            })
        }
    }

    #[cfg(unix)]
    struct WorkdirReplacingProbeRunner;

    #[cfg(unix)]
    impl fmn_platform::process::ProcessRunner for WorkdirReplacingProbeRunner {
        fn mechanism(&self) -> fmn_platform::process::ProcessMechanism {
            fmn_platform::process::ProcessMechanism::Scripted
        }

        fn start(
            &self,
            spec: &fmn_platform::process::ProcessSpec,
            _cancellation: fmn_platform::process::ProcessCancellation,
            _stdin_limits: fmn_platform::process::ProcessStdinLimits,
        ) -> Result<
            Box<dyn fmn_platform::process::RunningProcess>,
            fmn_platform::process::ProcessError,
        > {
            Err(fmn_platform::process::ProcessError::NotScripted {
                program: spec.program.clone(),
            })
        }

        fn run(
            &self,
            spec: &fmn_platform::process::ProcessSpec,
        ) -> Result<fmn_platform::process::ProcessOutcome, fmn_platform::process::ProcessError>
        {
            let stdout = if spec.argv == ["-version"] {
                b"ffmpeg version cli-fixture\n".to_vec()
            } else if spec.argv == ["-hide_banner", "-encoders"] {
                let workdir = spec.program.parent().ok_or_else(|| {
                    fmn_platform::process::ProcessError::Plumbing {
                        program: spec.program.clone(),
                        detail: "encoder probe tool has no private parent".to_owned(),
                    }
                })?;
                if spec.cwd.is_some() {
                    return Err(fmn_platform::process::ProcessError::Plumbing {
                        program: spec.program.clone(),
                        detail: "encoder probe unexpectedly requested a child cwd".to_owned(),
                    });
                }
                let leaf = workdir
                    .file_name()
                    .and_then(|leaf| leaf.to_str())
                    .unwrap_or("probe");
                let displaced = workdir.with_file_name(format!("{leaf}-displaced"));
                std::fs::rename(workdir, &displaced).map_err(|error| {
                    fmn_platform::process::ProcessError::Plumbing {
                        program: spec.program.clone(),
                        detail: format!("displace encoder-probe workdir: {error}"),
                    }
                })?;
                std::fs::create_dir(workdir).map_err(|error| {
                    fmn_platform::process::ProcessError::Plumbing {
                        program: spec.program.clone(),
                        detail: format!("replace encoder-probe workdir: {error}"),
                    }
                })?;
                std::fs::write(workdir.join("foreign"), b"not ours").map_err(|error| {
                    fmn_platform::process::ProcessError::Plumbing {
                        program: spec.program.clone(),
                        detail: format!("mark replacement encoder-probe workdir: {error}"),
                    }
                })?;
                b"Encoders:\n ------\n V....D libx264 fixture\n".to_vec()
            } else {
                return Err(fmn_platform::process::ProcessError::NotScripted {
                    program: spec.program.clone(),
                });
            };
            Ok(fmn_platform::process::ProcessOutcome {
                termination: fmn_platform::process::ProcessTermination::Exited(Some(0)),
                stdout,
                stderr: Vec::new(),
            })
        }
    }

    fn render(invocation: Invocation) -> RenderCommand {
        match invocation {
            Invocation::Render(command) => command,
            other => panic!("expected render command, got {other:?}"),
        }
    }

    fn sample_value(spec: &FlagSpec) -> &'static str {
        match spec.binding {
            "fps" => "60",
            "log_level" => "INFO",
            "resolution" => "640x360",
            "animation_range" => "1,3",
            "embed" => "42",
            "background" => "#112233",
            "vcodec" => "libx264",
            "pix_fmt" => "rgba",
            "format" => "png",
            "preview_codec" => "png",
            "bind" => "127.0.0.1",
            "port" => "0",
            "math_pack" => "default",
            "threads" | "max_scenes" => "2",
            "budget_ms" | "checkpoint_frames" => "100",
            "ffmpeg" => "/usr/bin/ffmpeg",
            "cache_dir" => "/tmp/fmn-cache",
            "config_file" => "/tmp/fmn.yml",
            "file_name" => "out",
            "video_dir" | "manifest_dir" => "/tmp/out",
            _ => "value",
        }
    }

    fn isolated_args(spec: &FlagSpec, alias: &str) -> Vec<String> {
        if spec.binding == "file" {
            return vec!["scene.py".to_owned()];
        }
        if spec.binding == "scene_names" {
            return vec!["scene.py".to_owned(), "Demo".to_owned()];
        }
        let mut args = match spec.command {
            CommandScope::Doctor => vec!["doctor".to_owned()],
            CommandScope::Batch => vec!["batch".to_owned()],
            CommandScope::Studio => vec!["studio".to_owned()],
            CommandScope::Global | CommandScope::Render => Vec::new(),
        };
        args.push(alias.to_owned());
        if spec.action == FlagAction::Store {
            args.push(sample_value(spec).to_owned());
        }
        args
    }

    #[test]
    fn every_generated_binding_and_value_type_has_a_typed_consumer() {
        validate_generated_contract().expect("generated CLI contract must be fully consumed");
        for spec in FLAG_SPECS {
            assert_eq!(typed_consumer_scope(spec.binding), Some(spec.command));
            assert!(spec.value_type.is_none_or(value_type_supported));
        }
    }

    #[test]
    fn every_generated_alias_reaches_the_parser() {
        for spec in FLAG_SPECS {
            for alias in spec.options {
                let args = isolated_args(spec, alias);
                if let Err(error) = parse_args(args.clone()) {
                    panic!(
                        "generated alias {alias:?} ({}) did not parse from {args:?}: {error}",
                        spec.binding
                    );
                }
            }
        }
    }

    #[test]
    fn attached_short_values_and_grouped_switches_are_kept() {
        let command = render(parse_args(["-n3,6", "-aq", "scene.py"]).expect("valid command"));
        assert_eq!(
            command.animation_range,
            Some(AnimationRange {
                start: 3,
                end: Some(6)
            })
        );
        assert!(command.write_all);
        assert!(command.common.quiet);
    }

    #[test]
    fn missing_store_value_does_not_swallow_the_next_option() {
        for args in [["--fps", "--hd"], ["--fps", "-q"], ["--fps", "-aq"]] {
            let error = parse_args(args).expect_err("next option cannot become an FPS value");
            assert_eq!(error.exit_name(), "usage");
            assert!(error.message().contains("requires a value"));
        }
        for args in [
            &["--fps", "--robot"][..],
            &["--fps", "--hd=true"],
            &["--file_name", "--typo", "scene.py"],
        ] {
            let error = parse_args(args.iter().copied())
                .expect_err("dash-prefixed separated values require `--flag=value`");
            assert_eq!(error.exit_name(), "usage");
            assert!(error.message().contains("requires a value"));
        }

        let error = parse_args(["--fps"]).expect_err("missing final value");
        assert_eq!(error.exit_name(), "usage");
        assert!(error.message().contains("requires a value"));

        let error = parse_args(["--fps", "-1"]).expect_err("negative separated value");
        assert_eq!(error.exit_name(), "usage");
        assert!(error.message().contains("requires a value"));
        let error = parse_args(["--fps=-1"]).expect_err("attached negative reaches validation");
        assert_eq!(error.exit_name(), "usage");
        assert!(error.message().contains("invalid value"));

        let command =
            render(parse_args(["--file_name=-draft"]).expect("attached dash-prefixed string"));
        assert_eq!(command.file_name.as_deref(), Some("-draft"));
    }

    #[test]
    fn generated_implications_request_durable_output() {
        let cases: &[&[&str]] = &[
            &["--open"],
            &["--finder"],
            &["--gif"],
            &["--transparent"],
            &["--format", "png"],
            &["--reproducible"],
            &["--vcodec", "libx264"],
            &["--pix_fmt", "rgba"],
            &["--subdivide"],
            &["--file_name", "out"],
            &["--video_dir", "/tmp/out"],
        ];
        for args in cases {
            let command = render(parse_args(args.iter().copied()).expect("valid implication"));
            assert!(command.write_file, "{args:?} did not imply --write_file");
        }
    }

    #[test]
    fn generated_failing_interactions_return_usage_and_rule_identity() {
        let cases: &[(&[&str], &str)] = &[
            (&["-l", "-m"], "quality-exclusive"),
            (&["-s", "--subdivide"], "skip-vs-subdivide"),
            (&["-a", "scene.py", "Demo"], "write-all-vs-selection"),
            (&["-e", "4", "--reproducible"], "embed-vs-certified"),
            (&["--autoreload", "--reproducible"], "reload-vs-certified"),
            (&["--clear-cache", "-l"], "clear-cache-only"),
            (&["--version", "-l"], "version-only"),
            (&["--version", "doctor"], "version-only"),
            (&["doctor", "--version"], "version-only"),
            (&["--version", "--quiet"], "version-only"),
            (&["--clear-cache", "batch"], "clear-cache-only"),
            (&["--clear-cache", "--threads", "2"], "clear-cache-only"),
            (&["--help", "-l"], "help-only"),
            (&["--help", "--quiet"], "help-only"),
        ];
        for (args, rule) in cases {
            let error = parse_args(args.iter().copied()).expect_err("interaction must fail");
            assert_eq!(error.code(), 2, "{args:?}");
            assert_eq!(error.rule(), Some(*rule), "{args:?}");
        }
    }

    #[test]
    fn help_may_select_a_subcommand_while_standalone_queries_may_not() {
        assert!(matches!(
            parse_args(["doctor", "--help"]).expect("subcommand help"),
            Invocation::Help {
                command: CommandScope::Doctor,
                ..
            }
        ));
        assert!(matches!(
            parse_args(["--help", "batch"]).expect("help before subcommand"),
            Invocation::Help {
                command: CommandScope::Batch,
                ..
            }
        ));
        assert!(matches!(
            parse_args([
                "--clear-cache",
                "--robot",
                "--quiet",
                "--config_file",
                "/cfg/fmn.yml",
                "--cache-dir",
                "/cache",
                "--log-level",
                "INFO",
            ])
            .expect("clear-cache consumes its declared modifiers"),
            Invocation::ClearCache { .. }
        ));
    }

    #[test]
    fn ranges_resolutions_and_output_constraints_fail_closed() {
        for range in ["", "a", "1,2,3", "3,2"] {
            let error = parse_args(["-n", range]).expect_err("malformed animation range must fail");
            assert_eq!(error.exit_name(), "usage");
        }
        for resolution in ["", "640", "0x480", "640x0", "axb"] {
            let error = parse_args(["-r", resolution]).expect_err("malformed resolution must fail");
            assert_eq!(error.exit_name(), "usage");
        }
        for (args, rule) in [
            (&["--gif", "--format", "video"][..], "gif-vs-non-gif-format"),
            (
                &["--format", "png", "--vcodec", "libx264"],
                "native-format-vs-codec",
            ),
            (
                &["--format", "png", "--pix_fmt", "rgba"],
                "native-format-vs-pixel-format",
            ),
            (
                &["-t", "--pix_fmt", "yuv420p"],
                "transparent-vs-opaque-pixel-format",
            ),
            (
                &["-t", "--format", "y4m"],
                "transparent-vs-opaque-native-format",
            ),
        ] {
            let error =
                parse_args(args.iter().copied()).expect_err("output constraint must fail closed");
            assert_eq!(error.rule(), Some(rule), "{args:?}");
        }
        let gif = render(parse_args(["--gif", "--format", "gif"]).expect("matching GIF aliases"));
        assert_eq!(gif.format, OutputFormat::Gif);
        let png_sequence =
            render(parse_args(["--format", "png_sequence"]).expect("documented format spelling"));
        assert_eq!(png_sequence.format, OutputFormat::PngSequence);
        let error =
            parse_args(["--format", "png-sequence"]).expect_err("undocumented spelling must fail");
        assert_eq!(error.exit_name(), "usage");
    }

    #[test]
    fn video_output_negotiates_bounded_wire_formats_and_transparency() {
        let fs = VirtualFs::new();
        let command = render(parse_args(["--format", "video"]).expect("video command"));
        let config = resolve_render_config(&fs, &command).expect("default render config");
        let job = ffmpeg_video_job(&command, &config).expect("default video negotiation");
        assert_eq!(job.wire, WireFormat::Nv12);
        assert_eq!(job.container, Container::Mp4);
        assert_eq!(
            job.resolved_encoder()
                .expect("default encoder is valid")
                .as_deref(),
            Some("libx264")
        );
        for (pixel_format, expected) in [
            ("rgba", WireFormat::Rgba8),
            ("bgra", WireFormat::Bgra8),
            ("nv12", WireFormat::Nv12),
            ("p010le", WireFormat::P010),
        ] {
            let command = render(
                parse_args(["--format", "video", "--pix_fmt", pixel_format])
                    .expect("explicit wire format command"),
            );
            let config = resolve_render_config(&fs, &command).expect("explicit wire config");
            assert_eq!(
                ffmpeg_video_job(&command, &config)
                    .expect("explicit wire negotiation")
                    .wire,
                expected,
                "{pixel_format}"
            );
        }

        let command = render(
            parse_args(["--format", "video", "--transparent"]).expect("transparent video command"),
        );
        let config = resolve_render_config(&fs, &command).expect("transparent render config");
        let job = ffmpeg_video_job(&command, &config).expect("transparent video negotiation");
        assert_eq!(job.wire, WireFormat::Rgba8);
        assert_eq!(job.container, Container::MovTransparent);
        assert_eq!(
            job.resolved_encoder()
                .expect("transparent encoder is valid")
                .as_deref(),
            Some("qtrle")
        );

        let command = render(
            parse_args(["--format", "video", "--transparent", "--vcodec", "libx264"])
                .expect("explicit transparent codec command"),
        );
        let config = resolve_render_config(&fs, &command).expect("explicit codec config");
        let error = ffmpeg_video_job(&command, &config)
            .expect_err("opaque encoder must not enter transparent negotiation");
        assert_eq!(error.exit_name(), "config");
        assert!(error.message().contains("requires the qtrle encoder"));

        let command = render(parse_args(["--vcodec", "libx265"]).expect("codec command"));
        assert_eq!(
            requested_render_format(&command).expect("codec implies video"),
            RequestedRenderFormat::Video
        );
    }

    #[test]
    fn certified_video_is_refused_instead_of_mislabelled() {
        let fs = VirtualFs::new();
        let command = render(
            parse_args(["--reproducible", "--format", "video"])
                .expect("certified video parses before capability negotiation"),
        );
        let config = resolve_render_config(&fs, &command).expect("certified render config");
        let error = ffmpeg_video_job(&command, &config)
            .expect_err("ffmpeg products are outside certification");
        assert_eq!(error.exit_name(), "capability");
        assert!(
            error
                .message()
                .contains("outside the certified artifact set")
        );
    }

    #[test]
    fn command_scopes_and_loopback_security_are_enforced() {
        let error = parse_args(["doctor", "--fps", "60"]).expect_err("wrong command flag");
        assert_eq!(error.exit_name(), "usage");
        let error = parse_args(["doctor", "scene.py"]).expect_err("doctor positional");
        assert_eq!(error.exit_name(), "usage");
        let error =
            parse_args(["studio", "--bind", "0.0.0.0"]).expect_err("non-loopback Studio bind");
        assert!(error.message().contains("loopback-only"));
        let error = parse_args(["studio", "--checkpoint-frames", "0"])
            .expect_err("zero checkpoint density");
        assert!(error.message().contains("greater than zero"));

        assert!(matches!(
            parse_args(["--robot", "doctor"]).expect("global before command"),
            Invocation::Doctor(DoctorCommand {
                common: CommonOptions { robot: true, .. },
                ..
            })
        ));
    }

    #[test]
    fn config_precedence_keeps_user_presets_but_cli_wins_values() {
        let fs = VirtualFs::new();
        fs.insert(
            "/cfg/fmn.yml",
            b"camera:\n  fps: 24\nresolution_options:\n  low: (320, 180)\ntex:\n  template: basic\n"
                .to_vec(),
        );
        let command = render(
            parse_args([
                "--config_file",
                "/cfg/fmn.yml",
                "-l",
                "--fps",
                "60",
                "--threads",
                "3",
                "--reproducible",
            ])
            .expect("valid command"),
        );
        let config = resolve_render_config(&fs, &command).expect("valid config");
        assert_eq!(config.camera.fps, 60);
        assert_eq!(config.camera.resolution, (320, 180));
        assert_eq!(config.determinism.mode, DeterminismMode::Certified);
        assert_eq!(config.render.engine, Engine::Cpu);
        assert_eq!(config.render.threads, ThreadPolicy::Fixed(3));
        assert_eq!(config.tex.template, "basic");
    }

    #[test]
    fn config_precedence_is_defaults_then_cwd_then_explicit_then_cli() {
        let fs = VirtualFs::new();
        fs.insert(
            "custom_config.yml",
            b"camera:\n  fps: 24\n  background_color: \"#101010\"\ndirectories:\n  cache: /cwd-cache\n"
                .to_vec(),
        );
        fs.insert(
            "/cfg/explicit.yml",
            b"camera:\n  fps: 48\ndirectories:\n  cache: /explicit-cache\n".to_vec(),
        );
        let command = render(
            parse_args([
                "--config_file",
                "/cfg/explicit.yml",
                "--cache-dir",
                "/cli-cache",
                "--fps",
                "60",
            ])
            .expect("valid command"),
        );
        let config = resolve_render_config(&fs, &command).expect("valid layered config");
        assert_eq!(config.camera.fps, 60, "CLI wins");
        assert_eq!(
            config.camera.background_color, "#101010",
            "cwd-only value survives the explicit layer"
        );
        assert_eq!(
            config.directories.cache, "/cli-cache",
            "cache CLI overlay wins"
        );
    }

    #[test]
    fn missing_optional_config_layers_match_reference_empty_layers() {
        let fs = VirtualFs::new();
        let command =
            render(parse_args(["--config_file", "/missing/config.yml"]).expect("valid command"));
        let config = resolve_render_config(&fs, &command).expect("missing layer is empty");
        assert_eq!(config.camera.fps, 30);
    }

    #[test]
    fn config_parse_errors_name_the_exact_layer() {
        let fs = VirtualFs::new();
        fs.insert("custom_config.yml", b"camera: [unsupported]\n".to_vec());
        let command = render(parse_args([] as [&str; 0]).expect("default render command"));
        let error = resolve_render_config(&fs, &command).expect_err("invalid cwd config");
        assert!(error.message().starts_with("custom_config.yml:"));

        let fs = VirtualFs::new();
        fs.insert("/cfg/broken.yml", b"camera: [unsupported]\n".to_vec());
        let command =
            render(parse_args(["--config_file", "/cfg/broken.yml"]).expect("valid command"));
        let error = resolve_render_config(&fs, &command).expect_err("invalid explicit config");
        assert!(error.message().contains("/cfg/broken.yml"));
    }

    #[test]
    fn config_byte_budget_is_enforced_before_utf8_decoding() {
        let fs = VirtualFs::new();
        let limit = fmn_config::yaml::Limits::DEFAULT.max_bytes;
        fs.insert("/cfg/exact.yml", vec![b'#'; limit]);
        let exact = read_optional_config(&fs, Path::new("/cfg/exact.yml"), "exact")
            .expect("an exact-limit config is readable")
            .expect("the config exists");
        assert_eq!(exact.text.len(), limit);

        fs.insert("/cfg/oversized.yml", vec![0xff; limit + 1]);
        let error = read_optional_config(&fs, Path::new("/cfg/oversized.yml"), "oversized")
            .expect_err("a limit-plus-one config is refused");
        assert_eq!(error.exit_name(), "config");
        assert!(error.message().contains("exceeds the 1048576-byte limit"));
        assert!(!error.message().contains("not UTF-8"));
    }

    #[test]
    fn explicitly_empty_cache_root_is_not_reinterpreted_as_the_default() {
        let error = parse_args(["--cache-dir="]).expect_err("empty override is invalid");
        assert_eq!(error.exit_name(), "usage");
        assert!(error.message().contains("invalid path value"));
    }

    #[test]
    fn exact_resolution_is_not_reinterpreted_as_a_custom_preset() {
        let fs = VirtualFs::new();
        fs.insert(
            "/cfg/fmn.yml",
            b"resolution_options:\n  low: (320, 180)\n".to_vec(),
        );
        let command = render(
            parse_args(["--config_file", "/cfg/fmn.yml", "-r", "854x480"]).expect("valid command"),
        );
        assert_eq!(command.resolution, Some(ResolutionChoice::Exact(854, 480)));
        let config = resolve_render_config(&fs, &command).expect("valid config");
        assert_eq!(config.camera.resolution, (854, 480));
    }

    #[test]
    fn runtime_mapping_preserves_range_skip_and_presenter_semantics() {
        let fs = VirtualFs::new();
        let command = render(
            parse_args(["-w", "-s", "-p", "-n", "2,5", "--fps", "48"]).expect("valid command"),
        );
        let config = resolve_render_config(&fs, &command).expect("valid config");
        let runtime = command.runtime_config(&config);
        assert_eq!(runtime.fps, 48);
        assert!(runtime.windowed);
        assert!(runtime.skip_animations);
        assert!(runtime.presenter_mode);
        assert_eq!(runtime.start_at_play, Some(2));
        assert_eq!(runtime.end_at_play, Some(5));
    }

    #[test]
    fn unknown_math_pack_is_a_config_error() {
        let fs = VirtualFs::new();
        let command =
            render(parse_args(["--math-pack", "not-a-pack"]).expect("parser accepts names"));
        let error = resolve_render_config(&fs, &command).expect_err("pack must be resolved");
        assert_eq!(error.exit_name(), "config");
    }

    fn synthetic_doctor() -> DoctorSnapshot {
        DoctorSnapshot {
            topology_source: TopologySource::Fallback {
                reason: "fixture fallback".to_owned(),
            },
            logical_cores: 8,
            physical_cores: 4,
            hardware_supported_tier: "x86-64-v4".to_owned(),
            active_compiled_tier: "portable",
            plan: ExecutionPlanReport {
                determinism: "certified",
                engine: "certified-cpu",
                frames_in_flight: 2,
                scene_threads: 1,
                render_teams: 2,
                render_threads: 4,
                output_threads: 1,
                fine_tile: 16,
                macro_tile: 128,
                estimated_in_flight_bytes: 4096,
                output_format: "rgba8",
                tuning_source: "certified-profile",
            },
            ffmpeg: FfmpegReport::Available {
                path: PathBuf::from("/opt/ffmpeg"),
                sha256: "abc123".to_owned(),
                version: "ffmpeg \"7\"".to_owned(),
                hardware_encoders: vec!["h264_nvenc".to_owned()],
                hardware_encoder_probe_error: Some("encoder inventory timed out".to_owned()),
            },
            cache: CacheReport::Configured {
                root: PathBuf::from("/cache"),
                exists: true,
                direct_entries: Some(3),
                warning: None,
            },
            fonts: FontReport {
                selected: "Computer Modern".to_owned(),
                bundled: vec!["Computer Modern".to_owned()],
                user: vec!["Fixture Sans".to_owned()],
                complete: true,
                detail: None,
            },
            math_packs: vec!["default".to_owned(), "minimal".to_owned()],
            certification: CertificationReport {
                platform: "linux-x86_64".to_owned(),
                supported: true,
                detail: "fixture certified".to_owned(),
            },
        }
    }

    #[test]
    fn doctor_robot_output_is_a_bit_locked_ndjson_schema() {
        let expected = concat!(
            "{\"schema\":\"fmn.doctor\",\"version\":1,\"kind\":\"topology\",\"source\":\"fallback\",\"source_detail\":\"fixture fallback\",\"logical_cores\":8,\"physical_cores\":4,\"hardware_supported_tier\":\"x86-64-v4\",\"active_compiled_tier\":\"portable\"}\n",
            "{\"schema\":\"fmn.doctor\",\"version\":1,\"kind\":\"execution_plan\",\"determinism\":\"certified\",\"engine\":\"certified-cpu\",\"frames_in_flight\":2,\"scene_threads\":1,\"render_teams\":2,\"render_threads\":4,\"output_threads\":1,\"fine_tile\":16,\"macro_tile\":128,\"estimated_in_flight_bytes\":4096,\"output_format\":\"rgba8\",\"tuning_source\":\"certified-profile\"}\n",
            "{\"schema\":\"fmn.doctor\",\"version\":1,\"kind\":\"ffmpeg\",\"available\":true,\"path\":\"/opt/ffmpeg\",\"sha256\":\"abc123\",\"ffmpeg_version\":\"ffmpeg \\\"7\\\"\",\"hardware_encoders\":[\"h264_nvenc\"],\"hardware_encoder_probe_error\":\"encoder inventory timed out\"}\n",
            "{\"schema\":\"fmn.doctor\",\"version\":1,\"kind\":\"cache\",\"resolved\":true,\"root\":\"/cache\",\"exists\":true,\"direct_entries\":3,\"warning\":null}\n",
            "{\"schema\":\"fmn.doctor\",\"version\":1,\"kind\":\"fonts\",\"selected\":\"Computer Modern\",\"bundled\":[\"Computer Modern\"],\"user\":[\"Fixture Sans\"],\"complete\":true,\"detail\":null}\n",
            "{\"schema\":\"fmn.doctor\",\"version\":1,\"kind\":\"math_packs\",\"packs\":[\"default\",\"minimal\"]}\n",
            "{\"schema\":\"fmn.doctor\",\"version\":1,\"kind\":\"certification\",\"platform\":\"linux-x86_64\",\"supported\":true,\"detail\":\"fixture certified\"}\n",
        );
        assert_eq!(
            synthetic_doctor()
                .to_ndjson()
                .expect("fixture paths are UTF-8"),
            expected
        );
        for line in expected.lines() {
            assert!(line.starts_with("{\"schema\":\"fmn.doctor\",\"version\":1,"));
            assert!(line.ends_with('}'));
        }
    }

    #[cfg(unix)]
    #[test]
    fn doctor_marks_encoder_probe_identity_change_unavailable() {
        static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

        let dir = std::env::temp_dir().join(format!(
            "fmn-cli-ffmpeg-identity-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir).expect("fresh probe fixture directory");
        let source = dir.join("ffmpeg");
        write_native_ffmpeg_fixture(&source);
        use fmn_platform::process::FfmpegLocator as _;
        let executable = fmn_platform::process::StdFfmpegLocator::default()
            .locate_ffmpeg(&source)
            .expect("locate native probe fixture");
        let report = probe_ffmpeg(
            &IdentityChangingProbeRunner {
                source: source.clone(),
            },
            executable,
        );

        assert!(!report.is_available());
        assert!(matches!(
            report,
            FfmpegReport::Unavailable { ref reason, .. }
                if reason.contains("executable image")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn doctor_marks_encoder_probe_workdir_replacement_unavailable() {
        let dir = std::env::temp_dir().join(format!(
            "fmn-cli-ffmpeg-workdir-identity-{}",
            std::process::id()
        ));
        std::fs::create_dir(&dir).expect("fresh probe fixture directory");
        let source = dir.join("ffmpeg");
        write_native_ffmpeg_fixture(&source);
        use fmn_platform::process::FfmpegLocator as _;
        let executable = fmn_platform::process::StdFfmpegLocator::default()
            .locate_ffmpeg(&source)
            .expect("locate native probe fixture");
        let report = probe_ffmpeg(&WorkdirReplacingProbeRunner, executable);

        assert!(!report.is_available());
        assert!(matches!(
            report,
            FfmpegReport::Unavailable { ref reason, .. }
                if reason.contains("claimed directory identity changed")
        ));
    }

    #[test]
    fn production_doctor_reports_degraded_capabilities_without_guessing() {
        let fs = Arc::new(VirtualFs::new());
        let runner = Arc::new(fmn_platform::process::ScriptedRunner::new());
        let output = run_with_capabilities(
            ["doctor", "--robot", "--cache-dir", "/cache"],
            fs_capability(&fs),
            runner,
            &no_ffmpeg_locator(),
        );
        assert_eq!(output.code, 0);
        assert!(output.stderr.is_empty());
        assert_eq!(output.stdout.lines().count(), 7);
        assert!(output.stdout.contains("\"kind\":\"topology\""));
        assert!(output.stdout.contains("\"source\":\"fallback\""));
        assert!(
            output
                .stdout
                .contains("\"kind\":\"ffmpeg\",\"available\":false")
        );
        assert!(
            output
                .stdout
                .contains("\"kind\":\"cache\",\"resolved\":true")
        );
        assert!(output.stdout.contains("\"kind\":\"fonts\""));
        assert!(output.stdout.contains("\"complete\":false"));
    }

    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    #[test]
    fn doctor_resolves_the_default_ffmpeg_name_through_the_injected_locator() {
        static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "fmn-cli-ffmpeg-locator-{}-{sequence}",
            std::process::id()
        ));
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).expect("create locator fixture");
        let source = bin.join("ffmpeg");
        write_native_ffmpeg_fixture(&source);
        let search_path = std::env::join_paths([bin.as_path()]).expect("fixture PATH");
        let locator = fmn_platform::process::StdFfmpegLocator::from_search_path(Some(search_path));
        let canonical = std::fs::canonicalize(&source).expect("canonical source");
        let canonical = canonical.to_str().expect("fixture path is UTF-8");
        let fs = Arc::new(VirtualFs::new());

        let output = run_with_capabilities(
            ["doctor", "--robot", "--cache-dir", "/cache"],
            fs_capability(&fs),
            Arc::new(SuccessfulFfmpegProbeRunner),
            &locator,
        );

        assert_eq!(output.code, 0);
        assert!(output.stderr.is_empty());
        assert!(output.stdout.contains(&format!(
            "\"kind\":\"ffmpeg\",\"available\":true,\"path\":{}",
            json_string(canonical)
        )));
        assert!(
            output
                .stdout
                .contains("\"ffmpeg_version\":\"ffmpeg version cli-locator-fixture\"")
        );
        assert!(
            output
                .stdout
                .contains("\"hardware_encoders\":[\"h264_nvenc\"]")
        );
    }

    #[test]
    fn doctor_uses_the_same_platform_default_cache_root_as_the_store_contract() {
        let expected =
            fmn_cache::resolve_host_cache_root("").expect("supported host cache convention");
        let fs = VirtualFs::new();
        let runner = fmn_platform::process::ScriptedRunner::new();
        let command = DoctorCommand {
            common: CommonOptions {
                robot: true,
                quiet: false,
                reproducible: false,
                config_file: None,
                cache_dir: None,
                ffmpeg: None,
                threads: None,
                log_level: None,
            },
            require_ffmpeg: false,
        };
        let snapshot = collect_doctor_snapshot(&fs, &runner, &no_ffmpeg_locator(), &command)
            .expect("doctor snapshot resolves");
        assert!(matches!(
            snapshot.cache,
            CacheReport::Configured { ref root, .. } if root == &expected
        ));
    }

    #[test]
    fn doctor_degrades_unavailable_platform_defaults_but_rejects_invalid_config() {
        let fs = VirtualFs::new();
        let unavailable = fmn_cache::CacheRootError::PlatformDefaultUnavailable {
            platform: "fixture",
            reason: "no declared cache base",
        };
        assert_eq!(
            cache_report_from_resolution(&fs, Err(unavailable)).unwrap(),
            CacheReport::Unresolved {
                reason: "fixture cache default is unavailable: no declared cache base".to_owned()
            }
        );

        let invalid = fmn_cache::CacheRootError::InvalidConfigured {
            path: PathBuf::from("../escape"),
            reason: "parent-directory components are not accepted",
        };
        let error =
            cache_report_from_resolution(&fs, Err(invalid)).expect_err("invalid config must abort");
        assert_eq!(error.exit_name(), "config");
    }

    #[test]
    fn doctor_cache_inspection_refuses_a_wrong_kind_ancestor() {
        let fs = VirtualFs::new();
        fs.insert("/cache/blocker", b"foreign file".to_vec());
        match inspect_cache(
            &fs,
            Path::new("/cache/blocker/owned"),
            MAX_DOCTOR_CACHE_DIRECT_ENTRIES,
        ) {
            CacheReport::Configured {
                exists,
                direct_entries,
                warning: Some(warning),
                ..
            } => {
                assert!(!exists);
                assert_eq!(direct_entries, None);
                assert!(warning.contains("RegularFile"));
                assert!(warning.contains("/cache/blocker"));
            }
            other => panic!("expected a no-follow traversal warning, got {other:?}"),
        }
    }

    #[test]
    fn doctor_cache_entry_count_is_bounded_before_path_collection() {
        let fs = VirtualFs::new();
        fs.insert("/cache/one", Vec::new());
        fs.insert("/cache/two", Vec::new());
        match inspect_cache(&fs, Path::new("/cache"), 1) {
            CacheReport::Configured {
                exists,
                direct_entries,
                warning: Some(warning),
                ..
            } => {
                assert!(exists);
                assert_eq!(direct_entries, None);
                assert!(warning.contains("1-entry limit"));
            }
            other => panic!("expected a bounded cache-count warning, got {other:?}"),
        }
    }

    #[test]
    fn clear_cache_dispatch_uses_owned_root_authorization_and_stable_streams() {
        use fmn_cache::{KeyBuilder, NamespacePolicy, Store, StoreConfig};
        use fmn_platform::clock::StdClock;
        use fmn_platform::fs::StdFs;
        use std::sync::Arc;

        // Store::open refuses symlinked root components, and macOS's
        // temp_dir lives under /var -> /private/var; resolve it first.
        let tmp = std::env::temp_dir();
        let tmp = tmp.canonicalize().unwrap_or(tmp);
        let root = tmp.join(format!("fmn-cli-clear-cache-{}", std::process::id()));
        let _ = StdFs.remove_dir_all(&root);
        let store = Store::open(
            Arc::new(StdFs),
            Arc::new(StdClock::new()),
            root.clone(),
            StoreConfig::default(),
        )
        .expect("create owned cache root");
        let namespace = store
            .namespace("cli", 1, NamespacePolicy::default())
            .expect("open cache namespace");
        let key = KeyBuilder::new("cli-clear")
            .push_str("fixture")
            .finish()
            .expect("cache key");
        namespace.put(&key, b"cached").expect("seed cache");
        drop(namespace);
        drop(store);

        let root_text = root.to_str().expect("test path is UTF-8");
        let virtual_fs = Arc::new(VirtualFs::new());
        let runner = Arc::new(fmn_platform::process::ScriptedRunner::new());
        let refused = run_with_capabilities(
            ["--clear-cache", "--cache-dir", root_text, "--robot"],
            fs_capability(&virtual_fs),
            runner,
            &no_ffmpeg_locator(),
        );
        assert_eq!(refused.code, exit_code("capability"));
        assert!(refused.stderr.is_empty());
        assert!(refused.stdout.contains("\"exit_name\":\"capability\""));
        assert!(
            root.join("ns").is_dir(),
            "virtual dispatch mutated the host"
        );

        let output = run(["--clear-cache", "--cache-dir", root_text, "--robot"]);
        assert_eq!(output.code, 0);
        assert!(output.stderr.is_empty());
        assert_eq!(output.stdout.lines().count(), 1);
        assert!(output.stdout.contains("\"kind\":\"cache_clear\""));
        assert!(output.stdout.contains("\"outcome\":\"cleared\""));
        assert!(root.join("STORE_OWNER").is_file());
        assert!(root.join("STORE_FORMAT").is_file());
        assert!(!root.join("ns").exists());

        let output = run(["--clear-cache", "--cache-dir", root_text, "--robot"]);
        assert_eq!(output.code, 0);
        assert!(output.stderr.is_empty());
        assert!(output.stdout.contains("\"outcome\":\"already_absent\""));

        let quiet = run(["--clear-cache", "--cache-dir", root_text, "--quiet"]);
        assert_eq!(quiet.code, 0);
        assert!(quiet.stdout.is_empty());
        assert!(quiet.stderr.is_empty());
        let _ = StdFs.remove_dir_all(&root);
    }

    #[test]
    fn relative_cache_root_is_absolutized_for_doctor_and_clear() {
        use fmn_cache::{NamespacePolicy, Store, StoreConfig};
        use fmn_platform::clock::StdClock;
        use fmn_platform::fs::StdFs;
        use std::sync::Arc;

        let relative = PathBuf::from(format!(".fmn-cli-relative-cache-{}", std::process::id()));
        let absolute = std::env::current_dir()
            .expect("test current directory")
            .join(&relative);
        let _ = StdFs.remove_dir_all(&absolute);
        let store = Store::open(
            Arc::new(StdFs),
            Arc::new(StdClock::new()),
            absolute.clone(),
            StoreConfig::default(),
        )
        .expect("create resolved cache root");
        store
            .namespace("relative", 1, NamespacePolicy::default())
            .expect("create managed subtree");
        drop(store);

        let relative_text = relative.to_str().expect("test path is UTF-8");
        let virtual_fs = Arc::new(VirtualFs::new());
        let runner = Arc::new(fmn_platform::process::ScriptedRunner::new());
        let doctor = run_with_capabilities(
            ["doctor", "--robot", "--cache-dir", relative_text],
            fs_capability(&virtual_fs),
            runner,
            &no_ffmpeg_locator(),
        );
        assert_eq!(doctor.code, 0);
        let absolute_text = absolute.to_str().expect("test path is UTF-8");
        assert!(
            doctor
                .stdout
                .contains(&format!("\"root\":{}", json_string(absolute_text)))
        );

        let clear = run(["--clear-cache", "--cache-dir", relative_text, "--robot"]);
        assert_eq!(clear.code, 0);
        assert!(
            clear
                .stdout
                .contains(&format!("\"root\":{}", json_string(absolute_text)))
        );
        assert!(!absolute.join("ns").exists());
        let _ = StdFs.remove_dir_all(&absolute);
    }

    #[test]
    fn clear_cache_dispatch_refuses_a_foreign_root_without_mutation() {
        use fmn_platform::fs::StdFs;

        let root =
            std::env::temp_dir().join(format!("fmn-cli-foreign-cache-{}", std::process::id()));
        let _ = StdFs.remove_dir_all(&root);
        std::fs::create_dir(&root).expect("create foreign root");
        let sentinel = root.join("important.txt");
        std::fs::write(&sentinel, b"keep").expect("write sentinel");

        let output = run([
            "--clear-cache",
            "--cache-dir",
            root.to_str().expect("test path is UTF-8"),
            "--robot",
        ]);
        assert_eq!(output.code, exit_code("config"));
        assert!(output.stderr.is_empty());
        assert_eq!(output.stdout.lines().count(), 1);
        assert!(output.stdout.contains("\"kind\":\"error\""));
        assert!(output.stdout.contains("\"exit_name\":\"config\""));
        assert_eq!(
            std::fs::read(&sentinel).expect("sentinel survives"),
            b"keep"
        );
        let _ = StdFs.remove_dir_all(&root);
    }

    #[test]
    fn doctor_refuses_unverified_annexes_and_rejects_certified_annexes() {
        let fs = Arc::new(VirtualFs::new());
        let runner = Arc::new(fmn_platform::process::ScriptedRunner::new());
        fs.insert("/cfg/metal.yml", b"render:\n  engine: metal\n".to_vec());
        let output = run_with_capabilities(
            ["doctor", "--robot", "--config_file", "/cfg/metal.yml"],
            fs_capability(&fs),
            Arc::clone(&runner) as Arc<dyn fmn_platform::process::ProcessRunner>,
            &no_ffmpeg_locator(),
        );
        assert_eq!(output.code, 4);
        assert!(output.stderr.is_empty());
        assert!(output.stdout.contains("\"exit_name\":\"capability\""));
        assert!(output.stdout.contains("no verified compiled Metal backend"));

        fs.insert(
            "/cfg/invalid.yml",
            b"determinism:\n  mode: certified\nrender:\n  engine: metal\n".to_vec(),
        );
        let output = run_with_capabilities(
            ["doctor", "--robot", "--config_file", "/cfg/invalid.yml"],
            fs_capability(&fs),
            runner,
            &no_ffmpeg_locator(),
        );
        assert_eq!(output.code, 3);
        assert!(output.stderr.is_empty());
        assert!(output.stdout.contains("\"exit_name\":\"config\""));
        assert!(output.stdout.contains("requires render.engine=cpu"));
        assert_eq!(platform_name("linux", "x86_64"), "linux-x86-64");
        assert_eq!(platform_name("macos", "aarch64"), "macos-aarch64");
    }

    #[test]
    fn user_caused_execution_plan_failures_are_config_errors() {
        let fs = Arc::new(VirtualFs::new());
        let runner = Arc::new(fmn_platform::process::ScriptedRunner::new());
        fs.insert(
            "/cfg/odd-nv12.yml",
            b"camera:\n  resolution: (1919, 1080)\nfile_writer:\n  pixel_format: nv12\n".to_vec(),
        );
        let output = run_with_capabilities(
            ["doctor", "--robot", "--config_file", "/cfg/odd-nv12.yml"],
            fs_capability(&fs),
            runner,
            &no_ffmpeg_locator(),
        );
        assert_eq!(output.code, 3);
        assert!(output.stderr.is_empty());
        assert!(output.stdout.contains("\"exit_name\":\"config\""));
        assert!(output.stdout.contains("requires even frame dimensions"));
    }

    #[test]
    fn required_ffmpeg_returns_capability_after_the_robot_report() {
        let fs = Arc::new(VirtualFs::new());
        let runner = Arc::new(fmn_platform::process::ScriptedRunner::new());
        let output = run_with_capabilities(
            ["doctor", "--robot", "--require-ffmpeg"],
            fs_capability(&fs),
            runner,
            &no_ffmpeg_locator(),
        );
        assert_eq!(output.code, 4);
        assert!(output.stderr.is_empty());
        assert_eq!(output.stdout.lines().count(), 8);
        assert!(output.stdout.lines().last().is_some_and(|line| {
            line.contains("\"kind\":\"error\"") && line.contains("\"exit_name\":\"capability\"")
        }));
    }

    #[test]
    fn batch_dispatch_reports_feature_state_or_validates_a_real_job() {
        let fs = Arc::new(VirtualFs::new());
        let runner = Arc::new(fmn_platform::process::ScriptedRunner::new());
        let output = run_with_capabilities(
            ["batch", "--robot"],
            fs_capability(&fs),
            runner,
            &no_ffmpeg_locator(),
        );
        assert!(output.stderr.is_empty());
        #[cfg(feature = "batch")]
        {
            assert_eq!(output.code, 5);
            assert!(output.stdout.contains("select @builtin"));
        }
        #[cfg(not(feature = "batch"))]
        {
            assert_eq!(output.code, 4);
            assert!(output.stdout.contains("disabled in this binary"));
        }
    }

    #[cfg(feature = "batch")]
    #[test]
    fn batch_cancellation_reaches_the_reel_emitter() {
        let layout = FrameLayout::tight(PixelFormat::Rgba8, 1, 1).expect("valid test layout");
        let limits = SinkLimits::new(1, 1_024, 1_024, 1_024).expect("valid sink limits");
        let (binding, _) = PngSink::new(
            Arc::new(VirtualFs::new()),
            PngSinkConfig {
                target: PngTarget::Single(PathBuf::from("/batch-cancel.png")),
                width: 1,
                height: 1,
                first_sequence: 0,
                compression: fmn_codec::CompressionLevel::Default,
                threads: 1,
                limits,
                profile: None,
            },
        )
        .expect("valid test PNG sink")
        .into_binding("batch-cancel");
        let emitter = OrderedEmitter::new(
            EmitterConfig::new(layout, 1, 0).expect("valid test emitter config"),
            vec![binding],
        )
        .expect("start test emitter");
        let cancellation = RenderCancellation::default();
        cancellation.register_emitter(emitter.handle());

        cancellation.request(RENDER_CANCEL_BUDGET);

        let result = emitter.finish();
        assert!(matches!(
            result,
            Err(ref failure) if failure.error == fmn_output::EmitterError::Cancelled
        ));
    }

    #[test]
    fn robot_errors_never_mix_human_stderr_or_decoration() {
        let fs = Arc::new(VirtualFs::new());
        let runner = Arc::new(fmn_platform::process::ScriptedRunner::new());
        let output = run_with_capabilities(
            ["--robot", "-l", "-m"],
            fs_capability(&fs),
            Arc::clone(&runner) as Arc<dyn fmn_platform::process::ProcessRunner>,
            &no_ffmpeg_locator(),
        );
        assert_eq!(output.code, 2);
        assert!(output.stderr.is_empty());
        assert_eq!(output.stdout.lines().count(), 1);
        assert!(output.stdout.starts_with("{\"schema\":\"fmn.cli\""));
        assert!(output.stdout.contains("\"rule\":\"quality-exclusive\""));

        let output = run_with_capabilities(
            ["doctor", "--", "--robot"],
            fs_capability(&fs),
            runner,
            &no_ffmpeg_locator(),
        );
        assert_eq!(output.code, 2);
        assert!(output.stdout.is_empty());
        assert!(output.stderr.starts_with("fmn: "));
    }

    #[test]
    fn help_and_version_dispatch_with_command_specific_streams() {
        let fs = Arc::new(VirtualFs::new());
        let runner = Arc::new(fmn_platform::process::ScriptedRunner::new());

        let help = run_with_capabilities(
            ["--help"],
            fs_capability(&fs),
            Arc::clone(&runner) as Arc<dyn fmn_platform::process::ProcessRunner>,
            &no_ffmpeg_locator(),
        );
        assert_eq!(help.code, 0);
        assert!(help.stderr.is_empty());
        assert!(
            help.stdout
                .starts_with("Usage: fmn [OPTIONS] [NATIVE_SCENE] [SCENE ...]\n")
        );
        assert!(help.stdout.contains("--format"));

        let explicit_render_help = run_with_capabilities(
            ["render", "--help"],
            fs_capability(&fs),
            Arc::clone(&runner) as Arc<dyn fmn_platform::process::ProcessRunner>,
            &no_ffmpeg_locator(),
        );
        assert_eq!(explicit_render_help.code, 0);
        assert!(
            explicit_render_help
                .stdout
                .starts_with("Usage: fmn render [OPTIONS] [NATIVE_SCENE] [SCENE ...]\n")
        );

        let help = run_with_capabilities(
            ["doctor", "--help"],
            fs_capability(&fs),
            Arc::clone(&runner) as Arc<dyn fmn_platform::process::ProcessRunner>,
            &no_ffmpeg_locator(),
        );
        assert_eq!(help.code, 0);
        assert!(help.stderr.is_empty());
        assert!(help.stdout.starts_with("Usage: fmn doctor [OPTIONS]\n"));
        assert!(
            !help
                .stdout
                .lines()
                .next()
                .is_some_and(|line| { line.contains("[NATIVE_SCENE]") || line.contains("[SCENE") })
        );

        let version = run_with_capabilities(
            ["--robot", "--version"],
            fs_capability(&fs),
            Arc::clone(&runner) as Arc<dyn fmn_platform::process::ProcessRunner>,
            &no_ffmpeg_locator(),
        );
        assert_eq!(version.code, 0);
        assert!(version.stderr.is_empty());
        assert_eq!(
            version.stdout,
            concat!(
                "{\"schema\":\"fmn.cli\",\"version\":1,\"kind\":\"version\",",
                "\"program\":\"fmn\",\"program_version\":\"0.1.0\"}\n"
            )
        );

        let robot_help = run_with_capabilities(
            ["doctor", "--robot", "--help"],
            fs_capability(&fs),
            runner,
            &no_ffmpeg_locator(),
        );
        assert_eq!(robot_help.code, 0);
        assert!(robot_help.stderr.is_empty());
        assert!(robot_help.stdout.contains("\"kind\":\"exit_code\""));
        assert!(robot_help.stdout.contains("\"kind\":\"interaction\""));
        assert!(
            robot_help
                .stdout
                .lines()
                .all(|line| line.starts_with("{\"schema\":\"fmn.cli\",\"version\":1,"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_os_arguments_fail_without_lossy_path_rewriting() {
        use std::os::unix::ffi::OsStringExt as _;

        let invalid = OsString::from_vec(vec![0xff]);
        let output = run_os([invalid.clone()]);
        assert_eq!(output.code, 2);
        assert!(output.stdout.is_empty());
        assert!(output.stderr.contains("must be valid UTF-8"));

        let output = run_os([OsString::from("--robot"), invalid]);
        assert_eq!(output.code, 2);
        assert!(output.stderr.is_empty());
        assert!(output.stdout.contains("\"exit_name\":\"usage\""));
    }

    #[cfg(unix)]
    #[test]
    fn doctor_robot_output_refuses_non_utf8_paths_without_replacement() {
        use std::os::unix::ffi::OsStringExt as _;

        let mut snapshot = synthetic_doctor();
        snapshot.cache = CacheReport::Configured {
            root: PathBuf::from(OsString::from_vec(b"/cache-\xff".to_vec())),
            exists: false,
            direct_entries: Some(0),
            warning: None,
        };
        let error = snapshot
            .to_ndjson()
            .expect_err("version-1 schema cannot carry native non-UTF-8");
        assert_eq!(error.exit_name(), "config");
        assert!(error.message().contains("not valid UTF-8"));
    }
}
