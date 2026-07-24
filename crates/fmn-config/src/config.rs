//! The typed configuration: the Reference's config-key surface as structs,
//! resolved through the Reference's precedence exactly.
//!
//! **Precedence** (the Reference's `initialize_manim_config`): built-in
//! defaults ([`Config::DEFAULT_DOCUMENT`], this crate's bundled YAML) →
//! user config file(s) (`custom_config.yml` from the working directory,
//! then `--config_file` if given) → the CLI overlay — all merged with the
//! Reference-exact recursive rules ([`crate::yaml::merge`]), then typed
//! once. Tuple-strings (`(1920, 1080)`) are typed here, the way the
//! Reference `literal_eval`s them after YAML loading.
//!
//! The struct family is hand-written against the shipped key surface; it
//! migrates to codegen from the one API schema when W10's fm-vn6 lands.
//! Unknown keys are preserved in [`Config::raw`] (the Reference carries
//! extras like `directories.removed_mirror_prefix` dynamically; so do we,
//! typed where known, raw where not).

use crate::yaml::{self, ParseError, Value, Warning};
use core::fmt;

/// A configuration failure: parse trouble in one source, or a precise
/// typed-extraction error naming the key path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigError {
    /// A source document failed to parse.
    Parse {
        /// Which layer ("defaults", "custom_config.yml", …).
        source: String,
        /// The positioned parse error.
        error: ParseError,
    },
    /// A key held a value of the wrong type.
    Type {
        /// Dotted key path ("camera.resolution").
        path: String,
        /// What the schema wanted.
        expected: &'static str,
        /// What the document held (type name or offending text).
        found: String,
    },
    /// A key held a well-typed but invalid value (range, enum, tuple shape).
    Value {
        /// Dotted key path.
        path: String,
        /// What is wrong and what would be right.
        message: String,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse { source, error } => write!(f, "{source}: {error}"),
            Self::Type {
                path,
                expected,
                found,
            } => write!(f, "{path}: expected {expected}, found {found}"),
            Self::Value { path, message } => write!(f, "{path}: {message}"),
        }
    }
}

impl std::error::Error for ConfigError {}

/// A duplicate-key (or similar) warning, tagged with the layer it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourcedWarning {
    /// Which layer ("custom_config.yml", …).
    pub source: String,
    /// 1-based line within that layer.
    pub line: u32,
    /// Human-readable description.
    pub message: String,
}

impl fmt::Display for SourcedWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}, line {}: {}", self.source, self.line, self.message)
    }
}

/// One configuration source layer: a name for diagnostics plus the text.
#[derive(Clone, Copy, Debug)]
pub struct Layer<'a> {
    /// Diagnostic name ("custom_config.yml", "--config_file …").
    pub name: &'a str,
    /// The document text.
    pub text: &'a str,
}

// ---------------------------------------------------------------------------
// The struct family (Reference key surface + native sections)
// ---------------------------------------------------------------------------

/// `directories:` — output/asset roots. `subdirs` stays an *open* ordered
/// map: the Reference iterates it, and user configs add entries (the 3b1b
/// tree adds `pi_creature_images`).
#[derive(Clone, Debug, PartialEq)]
pub struct DirectoriesConfig {
    /// Mirror the module path under the output directory.
    pub mirror_module_path: bool,
    /// The base directory all subdirs hang from.
    pub base: String,
    /// Named subdirectories, in file order, open-ended.
    pub subdirs: Vec<(String, String)>,
    /// Persistent-cache location; empty means the platform default.
    pub cache: String,
    /// Prefix stripped when mirroring module paths (the 3b1b tree sets it).
    pub removed_mirror_prefix: Option<String>,
}

/// `window:` — Studio/preview window placement.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowConfig {
    /// Corner code: UR, DL, UO, ….
    pub position_string: String,
    /// Which monitor shows the window.
    pub monitor_index: u32,
    /// Full-screen toggle.
    pub full_screen: bool,
    /// Explicit position override, if configured (tuple-string).
    pub position: Option<(i64, i64)>,
    /// Explicit size override, if configured (tuple-string).
    pub size: Option<(u32, u32)>,
}

/// `camera:` — the frame.
#[derive(Clone, Debug, PartialEq)]
pub struct CameraConfig {
    /// Output resolution (typed from the tuple-string).
    pub resolution: (u32, u32),
    /// Background color (hex string; color science is fmn-core's business).
    pub background_color: String,
    /// Frames per second.
    pub fps: u32,
    /// Background opacity.
    pub background_opacity: f64,
}

/// `file_writer:` — the encode boundary's knobs.
#[derive(Clone, Debug, PartialEq)]
pub struct FileWriterConfig {
    /// The ffmpeg executable (the one external tool, D2).
    pub ffmpeg_bin: String,
    /// Video codec name passed to ffmpeg.
    pub video_codec: String,
    /// Pixel format passed to ffmpeg.
    pub pixel_format: String,
    /// Saturation adjustment.
    pub saturation: f64,
    /// Gamma adjustment.
    pub gamma: f64,
}

/// `scene:` — runtime behavior defaults.
#[derive(Clone, Debug, PartialEq)]
pub struct SceneConfig {
    /// Per-animation progress bars.
    pub show_animation_progress: bool,
    /// Keep progress bars on screen.
    pub leave_progress_bars: bool,
    /// Render one frame per play call while skipping.
    pub preview_while_skipping: bool,
    /// `Scene.wait()` default duration in seconds.
    pub default_wait_time: f64,
}

/// `vmobject:` — vectorized-mobject style defaults.
#[derive(Clone, Debug, PartialEq)]
pub struct VMobjectConfig {
    /// Default stroke width.
    pub default_stroke_width: f64,
    /// Default stroke color.
    pub default_stroke_color: String,
    /// Default fill color.
    pub default_fill_color: String,
}

/// `mobject:` — base-mobject style defaults.
#[derive(Clone, Debug, PartialEq)]
pub struct MobjectConfig {
    /// Default mobject color.
    pub default_mobject_color: String,
    /// Default light color.
    pub default_light_color: String,
}

/// `tex:` — mathematics typesetting.
#[derive(Clone, Debug, PartialEq)]
pub struct TexConfig {
    /// The preamble-pack name, resolved through
    /// [`crate::packs::PackRegistry::resolve_template`].
    pub template: String,
    /// Font size at which `Tex("0")` is one manim unit tall.
    pub font_size_for_unit_height: f64,
}

/// `text:` — text rendering.
#[derive(Clone, Debug, PartialEq)]
pub struct TextConfig {
    /// Font family (a bundled face by default; D-08).
    pub font: String,
    /// Paragraph alignment.
    pub alignment: String,
    /// Font size at which `Text("0")` is one manim unit tall.
    pub font_size_for_unit_height: f64,
}

/// `embed:` — interactive-session behavior.
#[derive(Clone, Debug, PartialEq)]
pub struct EmbedConfig {
    /// Exception verbosity.
    pub exception_mode: String,
    /// Reload modules automatically.
    pub autoreload: bool,
}

/// `resolution_options:` — what the quality flags select.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolutionOptions {
    /// `-l`.
    pub low: (u32, u32),
    /// `-m`.
    pub med: (u32, u32),
    /// `--hd`.
    pub high: (u32, u32),
    /// `--uhd` (the document key is literally `4k`).
    pub uhd: (u32, u32),
}

/// `sizes:` — the coordinate-system and buffer constants.
#[derive(Clone, Debug, PartialEq)]
pub struct SizesConfig {
    /// Frame height in manim units.
    pub frame_height: f64,
    /// SMALL_BUFF.
    pub small_buff: f64,
    /// MED_SMALL_BUFF.
    pub med_small_buff: f64,
    /// MED_LARGE_BUFF.
    pub med_large_buff: f64,
    /// LARGE_BUFF.
    pub large_buff: f64,
    /// Default `to_edge` buffer.
    pub default_mobject_to_edge_buff: f64,
    /// Default `next_to` buffer.
    pub default_mobject_to_mobject_buff: f64,
}

/// `log_level:` — the five Reference levels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogLevel {
    /// DEBUG.
    Debug,
    /// INFO.
    Info,
    /// WARNING.
    Warning,
    /// ERROR.
    Error,
    /// CRITICAL.
    Critical,
}

/// `determinism.mode:` — the two-level determinism contract (§4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeterminismMode {
    /// Deterministic given a seed on a given build/platform.
    Standard,
    /// `--reproducible`: bit-identical across the certified matrix.
    Certified,
}

/// `render.engine:` — which execution engine renders. Annex engines are
/// standard-mode only (§10.7); that constraint is enforced where engines
/// are selected, with this type just naming the choice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    /// The CPU engines (certified and fast).
    Cpu,
    /// The Metal annex.
    Metal,
    /// The CUDA annex.
    Cuda,
}

/// `render.aa:` — anti-aliasing policy (a quality knob: speed, never
/// meaning).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AaPolicy {
    /// Adaptive coverage AA (the default).
    Adaptive,
    /// Forced 2× supersampling (A/B comparisons).
    Ssaa2x,
    /// Forced 4× supersampling (A/B comparisons).
    Ssaa4x,
}

/// `render.threads:` — scheduler freedom, never semantics (D18).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadPolicy {
    /// Derive the plan from `HardwareTopology`.
    Auto,
    /// A fixed thread count.
    Fixed(u32),
}

/// `determinism:` — the native determinism section. **Never scene-visible
/// data** (§4): the seed feeds the one RNG's construction and the mode
/// selects engines/validation; neither is readable from scene code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeterminismConfig {
    /// standard | certified.
    pub mode: DeterminismMode,
    /// The one PCG64DXSM stream's seed.
    pub seed: u64,
}

/// `render:` — the native engine/backend section. Quality knobs select
/// engines and schedules; they structurally cannot change meaning (§4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RenderConfig {
    /// cpu | metal | cuda.
    pub engine: Engine,
    /// AA policy.
    pub aa: AaPolicy,
    /// Thread policy.
    pub threads: ThreadPolicy,
}

/// The fully resolved, typed configuration.
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    /// `directories:`.
    pub directories: DirectoriesConfig,
    /// `window:`.
    pub window: WindowConfig,
    /// `camera:`.
    pub camera: CameraConfig,
    /// `file_writer:`.
    pub file_writer: FileWriterConfig,
    /// `scene:`.
    pub scene: SceneConfig,
    /// `vmobject:`.
    pub vmobject: VMobjectConfig,
    /// `mobject:`.
    pub mobject: MobjectConfig,
    /// `tex:`.
    pub tex: TexConfig,
    /// `text:`.
    pub text: TextConfig,
    /// `embed:`.
    pub embed: EmbedConfig,
    /// `resolution_options:`.
    pub resolution_options: ResolutionOptions,
    /// `sizes:`.
    pub sizes: SizesConfig,
    /// `key_bindings:` (open map, file order).
    pub key_bindings: Vec<(String, String)>,
    /// `colors:` (open map, file order; hex strings).
    pub colors: Vec<(String, String)>,
    /// `log_level:`.
    pub log_level: LogLevel,
    /// `universal_import_line:`.
    pub universal_import_line: String,
    /// `ignore_manimlib_modules_on_reload:`.
    pub ignore_manimlib_modules_on_reload: bool,
    /// `determinism:` (native).
    pub determinism: DeterminismConfig,
    /// `render:` (native).
    pub render: RenderConfig,
    /// The complete merged document, for dynamic consumers reading keys the
    /// typed family does not name (Reference-tolerant behavior).
    pub raw: Value,
}

/// A resolved configuration plus the warnings its sources produced.
#[derive(Clone, Debug, PartialEq)]
pub struct Resolved {
    /// The typed configuration.
    pub config: Config,
    /// Duplicate-key and similar warnings from every layer.
    pub warnings: Vec<SourcedWarning>,
}

impl Config {
    /// The bundled defaults document — the first precedence layer, and the
    /// permanent fixture of the supported YAML subset.
    pub const DEFAULT_DOCUMENT: &'static str = include_str!("default_config.yml");

    /// Resolve the configuration: defaults, then `user_layers` in order,
    /// then the CLI overlay ([`overlay`] builds one), merged with the
    /// Reference's recursive rules and typed once.
    ///
    /// # Errors
    /// [`ConfigError::Parse`] naming the offending layer, or a typed
    /// extraction error naming the key path.
    pub fn resolve(
        user_layers: &[Layer<'_>],
        cli_overlay: Option<Value>,
    ) -> Result<Resolved, ConfigError> {
        let mut warnings = Vec::new();
        let mut parse_layer = |name: &str, text: &str| -> Result<Value, ConfigError> {
            let (value, layer_warnings) =
                yaml::parse(text).map_err(|error| ConfigError::Parse {
                    source: name.to_owned(),
                    error,
                })?;
            warnings.extend(layer_warnings.into_iter().map(|w: Warning| SourcedWarning {
                source: name.to_owned(),
                line: w.line,
                message: w.message,
            }));
            Ok(value)
        };

        let mut merged = parse_layer("built-in defaults", Self::DEFAULT_DOCUMENT)?;
        for layer in user_layers {
            let value = parse_layer(layer.name, layer.text)?;
            merged = yaml::merge(merged, value);
        }
        if let Some(overlay) = cli_overlay {
            merged = yaml::merge(merged, overlay);
        }

        let config = Self::from_value(merged)?;
        Ok(Resolved { config, warnings })
    }

    /// Type a fully merged document.
    ///
    /// # Errors
    /// A [`ConfigError`] naming the key path and the expected-vs-found
    /// shapes.
    pub fn from_value(root: Value) -> Result<Self, ConfigError> {
        let cx = Cx { root: &root };
        let config = Self {
            directories: DirectoriesConfig {
                mirror_module_path: cx.bool("directories.mirror_module_path")?,
                base: cx.string("directories.base")?,
                subdirs: cx.string_map("directories.subdirs")?,
                cache: cx.string("directories.cache")?,
                removed_mirror_prefix: cx.opt_string("directories.removed_mirror_prefix")?,
            },
            window: WindowConfig {
                position_string: cx.string("window.position_string")?,
                monitor_index: cx.u32("window.monitor_index")?,
                full_screen: cx.bool("window.full_screen")?,
                position: cx.opt_tuple_i64("window.position")?,
                size: cx.opt_tuple_u32("window.size")?,
            },
            camera: CameraConfig {
                resolution: cx.tuple_u32("camera.resolution")?,
                background_color: cx.string("camera.background_color")?,
                fps: cx.u32("camera.fps")?,
                background_opacity: cx.f64("camera.background_opacity")?,
            },
            file_writer: FileWriterConfig {
                ffmpeg_bin: cx.string("file_writer.ffmpeg_bin")?,
                video_codec: cx.string("file_writer.video_codec")?,
                pixel_format: cx.string("file_writer.pixel_format")?,
                saturation: cx.f64("file_writer.saturation")?,
                gamma: cx.f64("file_writer.gamma")?,
            },
            scene: SceneConfig {
                show_animation_progress: cx.bool("scene.show_animation_progress")?,
                leave_progress_bars: cx.bool("scene.leave_progress_bars")?,
                preview_while_skipping: cx.bool("scene.preview_while_skipping")?,
                default_wait_time: cx.f64("scene.default_wait_time")?,
            },
            vmobject: VMobjectConfig {
                default_stroke_width: cx.f64("vmobject.default_stroke_width")?,
                default_stroke_color: cx.string("vmobject.default_stroke_color")?,
                default_fill_color: cx.string("vmobject.default_fill_color")?,
            },
            mobject: MobjectConfig {
                default_mobject_color: cx.string("mobject.default_mobject_color")?,
                default_light_color: cx.string("mobject.default_light_color")?,
            },
            tex: TexConfig {
                template: cx.string("tex.template")?,
                font_size_for_unit_height: cx.f64("tex.font_size_for_unit_height")?,
            },
            text: TextConfig {
                font: cx.string("text.font")?,
                alignment: cx.string("text.alignment")?,
                font_size_for_unit_height: cx.f64("text.font_size_for_unit_height")?,
            },
            embed: EmbedConfig {
                exception_mode: cx.string("embed.exception_mode")?,
                autoreload: cx.bool("embed.autoreload")?,
            },
            resolution_options: ResolutionOptions {
                low: cx.tuple_u32("resolution_options.low")?,
                med: cx.tuple_u32("resolution_options.med")?,
                high: cx.tuple_u32("resolution_options.high")?,
                uhd: cx.tuple_u32("resolution_options.4k")?,
            },
            sizes: SizesConfig {
                frame_height: cx.f64("sizes.frame_height")?,
                small_buff: cx.f64("sizes.small_buff")?,
                med_small_buff: cx.f64("sizes.med_small_buff")?,
                med_large_buff: cx.f64("sizes.med_large_buff")?,
                large_buff: cx.f64("sizes.large_buff")?,
                default_mobject_to_edge_buff: cx.f64("sizes.default_mobject_to_edge_buff")?,
                default_mobject_to_mobject_buff: cx.f64("sizes.default_mobject_to_mobject_buff")?,
            },
            key_bindings: cx.string_map("key_bindings")?,
            colors: cx.string_map("colors")?,
            log_level: cx.log_level("log_level")?,
            universal_import_line: cx.string("universal_import_line")?,
            ignore_manimlib_modules_on_reload: cx.bool("ignore_manimlib_modules_on_reload")?,
            determinism: DeterminismConfig {
                mode: cx.determinism_mode("determinism.mode")?,
                seed: cx.u64("determinism.seed")?,
            },
            render: RenderConfig {
                engine: cx.engine("render.engine")?,
                aa: cx.aa_policy("render.aa")?,
                threads: cx.thread_policy("render.threads")?,
            },
            raw: Value::Null, // placed below, after the borrows end
        };
        Ok(Self {
            raw: root,
            ..config
        })
    }
}

/// Build a CLI overlay [`Value`] from dotted paths — the shape fm-c53 hands
/// to [`Config::resolve`] as the last precedence layer.
///
/// ```
/// use fmn_config::{config::overlay, Value};
/// let o = overlay([
///     ("camera.fps", Value::Int(60)),
///     ("window.full_screen", Value::Bool(true)),
/// ]);
/// assert_eq!(o.get_path("camera.fps"), Some(&Value::Int(60)));
/// ```
#[must_use]
pub fn overlay<'a>(pairs: impl IntoIterator<Item = (&'a str, Value)>) -> Value {
    let mut root = Value::Map(Vec::new());
    for (path, value) in pairs {
        let mut nested = value;
        for part in path.rsplit('.') {
            nested = Value::Map(vec![(part.to_owned(), nested)]);
        }
        root = yaml::merge(root, nested);
    }
    root
}

// ---------------------------------------------------------------------------
// Extraction
// ---------------------------------------------------------------------------

struct Cx<'a> {
    root: &'a Value,
}

impl Cx<'_> {
    fn lookup(&self, path: &str) -> Result<&Value, ConfigError> {
        let mut cur = self.root;
        let mut walked = String::new();
        for part in path.split('.') {
            if !walked.is_empty() {
                walked.push('.');
            }
            walked.push_str(part);
            cur = match cur {
                Value::Map(_) => cur.get(part).ok_or_else(|| ConfigError::Value {
                    path: walked.clone(),
                    message: "missing key (the defaults layer should always provide it)".into(),
                })?,
                other => {
                    // A parent was overridden with a scalar; name the parent.
                    let parent = walked.rsplit_once('.').map_or("", |(p, _)| p);
                    return Err(Self::type_err(parent, "mapping", other));
                }
            };
        }
        Ok(cur)
    }

    fn type_err(path: &str, expected: &'static str, found: &Value) -> ConfigError {
        ConfigError::Type {
            path: path.to_owned(),
            expected,
            found: match found {
                Value::Str(s) => format!("string {s:?}"),
                other => other.type_name().to_owned(),
            },
        }
    }

    fn bool(&self, path: &str) -> Result<bool, ConfigError> {
        match self.lookup(path)? {
            Value::Bool(v) => Ok(*v),
            other => Err(Self::type_err(path, "a boolean (True/False)", other)),
        }
    }

    fn string(&self, path: &str) -> Result<String, ConfigError> {
        match self.lookup(path)? {
            Value::Str(v) => Ok(v.clone()),
            other => Err(Self::type_err(path, "a string", other)),
        }
    }

    fn opt_string(&self, path: &str) -> Result<Option<String>, ConfigError> {
        match self.lookup(path) {
            Ok(Value::Str(v)) => Ok(Some(v.clone())),
            Ok(Value::Null) | Err(ConfigError::Value { .. }) => Ok(None),
            Ok(other) => Err(Self::type_err(path, "a string", other)),
            Err(e) => Err(e),
        }
    }

    fn f64(&self, path: &str) -> Result<f64, ConfigError> {
        match self.lookup(path)? {
            Value::Float(v) => Ok(*v),
            // The Reference's YAML gives ints where users write `144`; the
            // typed field is a float either way.
            #[allow(clippy::cast_precision_loss)]
            Value::Int(v) => Ok(*v as f64),
            other => Err(Self::type_err(path, "a number", other)),
        }
    }

    fn u32(&self, path: &str) -> Result<u32, ConfigError> {
        match self.lookup(path)? {
            Value::Int(v) => u32::try_from(*v).map_err(|_| ConfigError::Value {
                path: path.to_owned(),
                message: format!("{v} is out of range for a non-negative 32-bit integer"),
            }),
            other => Err(Self::type_err(path, "a non-negative integer", other)),
        }
    }

    fn u64(&self, path: &str) -> Result<u64, ConfigError> {
        match self.lookup(path)? {
            Value::Int(v) => u64::try_from(*v).map_err(|_| ConfigError::Value {
                path: path.to_owned(),
                message: format!("{v} is negative; a seed is a non-negative integer"),
            }),
            other => Err(Self::type_err(path, "a non-negative integer", other)),
        }
    }

    /// An open `key: "string"` map, in file order.
    fn string_map(&self, path: &str) -> Result<Vec<(String, String)>, ConfigError> {
        let value = self.lookup(path)?;
        let Value::Map(entries) = value else {
            return Err(Self::type_err(path, "a mapping", value));
        };
        entries
            .iter()
            .map(|(k, v)| match v {
                Value::Str(s) => Ok((k.clone(), s.clone())),
                other => Err(Self::type_err(&format!("{path}.{k}"), "a string", other)),
            })
            .collect()
    }

    /// A `(w, h)` tuple-string typed to `(u32, u32)` — the Reference's
    /// `literal_eval` step.
    fn tuple_u32(&self, path: &str) -> Result<(u32, u32), ConfigError> {
        let (a, b) = self.tuple_i64_at(path)?;
        let conv = |v: i64| {
            u32::try_from(v).map_err(|_| ConfigError::Value {
                path: path.to_owned(),
                message: format!("{v} is out of range for a dimension"),
            })
        };
        Ok((conv(a)?, conv(b)?))
    }

    fn opt_tuple_u32(&self, path: &str) -> Result<Option<(u32, u32)>, ConfigError> {
        match self.lookup(path) {
            Err(_) | Ok(Value::Null) => Ok(None), // absent or explicitly empty
            Ok(_) => self.tuple_u32(path).map(Some),
        }
    }

    fn opt_tuple_i64(&self, path: &str) -> Result<Option<(i64, i64)>, ConfigError> {
        match self.lookup(path) {
            Err(_) | Ok(Value::Null) => Ok(None), // absent or explicitly empty
            Ok(_) => self.tuple_i64_at(path).map(Some),
        }
    }

    fn tuple_i64_at(&self, path: &str) -> Result<(i64, i64), ConfigError> {
        let value = self.lookup(path)?;
        let Value::Str(text) = value else {
            return Err(Self::type_err(path, "a \"(w, h)\" tuple-string", value));
        };
        parse_tuple2(text).ok_or_else(|| ConfigError::Value {
            path: path.to_owned(),
            message: format!(
                "expected a \"(w, h)\" tuple-string like \"(1920, 1080)\", found {text:?}"
            ),
        })
    }

    fn keyword<T: Copy>(
        &self,
        path: &str,
        table: &[(&str, T)],
        expected: &'static str,
    ) -> Result<T, ConfigError> {
        let text = match self.lookup(path)? {
            Value::Str(v) => v.clone(),
            other => return Err(Self::type_err(path, expected, other)),
        };
        table
            .iter()
            .find(|(name, _)| *name == text)
            .map(|(_, v)| *v)
            .ok_or_else(|| ConfigError::Value {
                path: path.to_owned(),
                message: format!(
                    "unknown value {text:?}; expected one of: {}",
                    table
                        .iter()
                        .map(|(name, _)| format!("{name:?}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            })
    }

    fn log_level(&self, path: &str) -> Result<LogLevel, ConfigError> {
        self.keyword(
            path,
            &[
                ("DEBUG", LogLevel::Debug),
                ("INFO", LogLevel::Info),
                ("WARNING", LogLevel::Warning),
                ("ERROR", LogLevel::Error),
                ("CRITICAL", LogLevel::Critical),
            ],
            "a log level string",
        )
    }

    fn determinism_mode(&self, path: &str) -> Result<DeterminismMode, ConfigError> {
        self.keyword(
            path,
            &[
                ("standard", DeterminismMode::Standard),
                ("certified", DeterminismMode::Certified),
            ],
            "a determinism mode string",
        )
    }

    fn engine(&self, path: &str) -> Result<Engine, ConfigError> {
        self.keyword(
            path,
            &[
                ("cpu", Engine::Cpu),
                ("metal", Engine::Metal),
                ("cuda", Engine::Cuda),
            ],
            "an engine name string",
        )
    }

    fn aa_policy(&self, path: &str) -> Result<AaPolicy, ConfigError> {
        self.keyword(
            path,
            &[
                ("adaptive", AaPolicy::Adaptive),
                ("ssaa2x", AaPolicy::Ssaa2x),
                ("ssaa4x", AaPolicy::Ssaa4x),
            ],
            "an AA policy string",
        )
    }

    fn thread_policy(&self, path: &str) -> Result<ThreadPolicy, ConfigError> {
        match self.lookup(path)? {
            Value::Str(s) if s == "auto" => Ok(ThreadPolicy::Auto),
            Value::Int(v) => {
                let n = u32::try_from(*v).ok().filter(|n| *n > 0).ok_or_else(|| {
                    ConfigError::Value {
                        path: path.to_owned(),
                        message: format!("{v} is not a positive thread count"),
                    }
                })?;
                Ok(ThreadPolicy::Fixed(n))
            }
            Value::Str(other) => Err(ConfigError::Value {
                path: path.to_owned(),
                message: format!("unknown value {other:?}; expected \"auto\" or a thread count"),
            }),
            other => Err(Self::type_err(path, "\"auto\" or a thread count", other)),
        }
    }
}

/// Parse `(a, b)` with optional interior whitespace — the exact shape the
/// Reference feeds `literal_eval`.
fn parse_tuple2(text: &str) -> Option<(i64, i64)> {
    let inner = text.trim().strip_prefix('(')?.strip_suffix(')')?;
    let (a, b) = inner.split_once(',')?;
    if b.contains(',') {
        return None;
    }
    Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_document_types_cleanly() {
        let resolved = Config::resolve(&[], None).expect("defaults resolve");
        assert!(resolved.warnings.is_empty(), "{:?}", resolved.warnings);
        let c = &resolved.config;
        // Spot checks across every section and scalar type.
        assert!(!c.directories.mirror_module_path);
        assert_eq!(
            c.directories.subdirs.first(),
            Some(&("output".to_owned(), "videos".to_owned()))
        );
        assert_eq!(c.window.position_string, "UR");
        assert_eq!(c.window.position, None);
        assert_eq!(c.camera.resolution, (1920, 1080));
        assert_eq!(c.camera.fps, 30);
        assert!((c.camera.background_opacity - 1.0).abs() < 1e-12);
        assert_eq!(c.file_writer.ffmpeg_bin, "ffmpeg");
        assert!((c.scene.default_wait_time - 1.0).abs() < 1e-12);
        assert_eq!(c.tex.template, "default");
        assert!((c.tex.font_size_for_unit_height - 144.0).abs() < 1e-12);
        assert_eq!(c.text.font, "Computer Modern");
        assert_eq!(c.resolution_options.uhd, (3840, 2160));
        assert!((c.sizes.frame_height - 8.0).abs() < 1e-12);
        assert_eq!(
            c.key_bindings.first().map(|(k, _)| k.as_str()),
            Some("pan_3d")
        );
        assert_eq!(
            c.colors
                .iter()
                .find(|(k, _)| k == "blue_c")
                .map(|(_, v)| v.as_str()),
            Some("#58C4DD")
        );
        assert_eq!(c.log_level, LogLevel::Info);
        assert_eq!(c.determinism.mode, DeterminismMode::Standard);
        assert_eq!(c.determinism.seed, 0);
        assert_eq!(c.render.engine, Engine::Cpu);
        assert_eq!(c.render.aa, AaPolicy::Adaptive);
        assert_eq!(c.render.threads, ThreadPolicy::Auto);
    }

    #[test]
    fn overlay_builds_nested_maps_from_dotted_paths() {
        let o = overlay([
            ("camera.fps", Value::Int(60)),
            ("camera.background_color", Value::Str("#000000".into())),
            ("window.full_screen", Value::Bool(true)),
        ]);
        assert_eq!(o.get_path("camera.fps"), Some(&Value::Int(60)));
        assert_eq!(
            o.get_path("camera.background_color"),
            Some(&Value::Str("#000000".into()))
        );
        assert_eq!(o.get_path("window.full_screen"), Some(&Value::Bool(true)));
    }

    #[test]
    fn tuple_strings_are_typed_like_literal_eval() {
        assert_eq!(parse_tuple2("(1920, 1080)"), Some((1920, 1080)));
        assert_eq!(parse_tuple2("(500,500)"), Some((500, 500)));
        assert_eq!(parse_tuple2(" ( 1 , 2 ) "), Some((1, 2)));
        assert_eq!(parse_tuple2("(1, 2, 3)"), None);
        assert_eq!(parse_tuple2("1920x1080"), None);
        assert_eq!(parse_tuple2("()"), None);
    }
}
