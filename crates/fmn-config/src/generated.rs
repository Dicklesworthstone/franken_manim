//! Typed config extraction and canonical projection (§16.2, fm-vn6).
//!
//! GENERATED from API_SCHEMA.tsv + API_OVERLAY.tsv by
//! fmn_conformance::schema — regenerate, never hand-edit.
//! Reference pin: 6199a00d4c1b1127ebe45cb629c3f22538b10e13
//!
//! Regenerate:  UPDATE_API_ARTIFACTS=1 cargo test -p fmn-conformance \
//!                  --test api_schema
//!
//! The bound keys come from API_OVERLAY.tsv's `[config_binding]`
//! section; the accessors are `Cx`'s in `config.rs`. A key added to
//! `default_config.yml` without a binding fails the coverage check
//! before it can reach this file.

use crate::config::CacheConfig;
use crate::config::CameraConfig;
use crate::config::CanonicalConfigField;
use crate::config::CanonicalConfigSection;
use crate::config::Config;
use crate::config::ConfigError;
use crate::config::Cx;
use crate::config::DeterminismConfig;
use crate::config::DirectoriesConfig;
use crate::config::EmbedConfig;
use crate::config::FileWriterConfig;
use crate::config::MobjectConfig;
use crate::config::RenderConfig;
use crate::config::ResolutionOptions;
use crate::config::SceneConfig;
use crate::config::SizesConfig;
use crate::config::TexConfig;
use crate::config::TextConfig;
use crate::config::VMobjectConfig;
use crate::config::WindowConfig;
use crate::yaml::Value;

/// Type a fully merged configuration document.
///
/// # Errors
/// A [`ConfigError`] naming the key path and the expected-vs-found
/// shapes.
pub(crate) fn config_from_value(root: Value) -> Result<Config, ConfigError> {
    let cx = Cx { root: &root };
    let config = Config {
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
            video_crf: cx.opt_u32("file_writer.video_crf")?,
            video_preset: cx.opt_string("file_writer.video_preset")?,
            video_tune: cx.opt_string("file_writer.video_tune")?,
            video_bitrate: cx.opt_u32("file_writer.video_bitrate")?,
            audio_bitrate: cx.opt_u32("file_writer.audio_bitrate")?,
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
        cache: CacheConfig {
            max_bytes: cx.u64("cache.max_bytes")?,
        },
        // Placed below, once the borrows of `root` have ended.
        raw: Value::Null,
    };
    Ok(Config {
        raw: root,
        ..config
    })
}

/// Serialize the effective typed fields and preserve unknown source keys.
/// Field paths and ordering come from the same bindings as extraction.
pub(crate) fn put_canonical_config(writer: &mut fmn_hash::Writer, config: &Config) {
    let field_0 = &config.directories.mirror_module_path;
    let field_1 = &config.directories.base;
    let field_2 = &config.directories.subdirs;
    let field_3 = &config.directories.cache;
    let field_4 = &config.directories.removed_mirror_prefix;
    let field_5 = &config.window.position_string;
    let field_6 = &config.window.monitor_index;
    let field_7 = &config.window.full_screen;
    let field_8 = &config.window.position;
    let field_9 = &config.window.size;
    let field_10 = &config.camera.resolution;
    let field_11 = &config.camera.background_color;
    let field_12 = &config.camera.fps;
    let field_13 = &config.camera.background_opacity;
    let field_14 = &config.file_writer.ffmpeg_bin;
    let field_15 = &config.file_writer.video_codec;
    let field_16 = &config.file_writer.pixel_format;
    let field_17 = &config.file_writer.video_crf;
    let field_18 = &config.file_writer.video_preset;
    let field_19 = &config.file_writer.video_tune;
    let field_20 = &config.file_writer.video_bitrate;
    let field_21 = &config.file_writer.audio_bitrate;
    let field_22 = &config.file_writer.saturation;
    let field_23 = &config.file_writer.gamma;
    let field_24 = &config.scene.show_animation_progress;
    let field_25 = &config.scene.leave_progress_bars;
    let field_26 = &config.scene.preview_while_skipping;
    let field_27 = &config.scene.default_wait_time;
    let field_28 = &config.vmobject.default_stroke_width;
    let field_29 = &config.vmobject.default_stroke_color;
    let field_30 = &config.vmobject.default_fill_color;
    let field_31 = &config.mobject.default_mobject_color;
    let field_32 = &config.mobject.default_light_color;
    let field_33 = &config.tex.template;
    let field_34 = &config.tex.font_size_for_unit_height;
    let field_35 = &config.text.font;
    let field_36 = &config.text.alignment;
    let field_37 = &config.text.font_size_for_unit_height;
    let field_38 = &config.embed.exception_mode;
    let field_39 = &config.embed.autoreload;
    let field_40 = &config.resolution_options.low;
    let field_41 = &config.resolution_options.med;
    let field_42 = &config.resolution_options.high;
    let field_43 = &config.resolution_options.uhd;
    let field_44 = &config.sizes.frame_height;
    let field_45 = &config.sizes.small_buff;
    let field_46 = &config.sizes.med_small_buff;
    let field_47 = &config.sizes.med_large_buff;
    let field_48 = &config.sizes.large_buff;
    let field_49 = &config.sizes.default_mobject_to_edge_buff;
    let field_50 = &config.sizes.default_mobject_to_mobject_buff;
    let field_51 = &config.key_bindings;
    let field_52 = &config.colors;
    let field_53 = &config.log_level;
    let field_54 = &config.universal_import_line;
    let field_55 = &config.ignore_manimlib_modules_on_reload;
    let field_56 = &config.determinism.mode;
    let field_57 = &config.determinism.seed;
    let field_58 = &config.render.engine;
    let field_59 = &config.render.aa;
    let field_60 = &config.render.threads;
    let field_61 = &config.cache.max_bytes;
    let directories = CanonicalConfigSection {
        section: Some("directories"),
        raw: config.raw.get("directories"),
        fields: &[
            ("mirror_module_path", field_0),
            ("base", field_1),
            ("subdirs", field_2),
            ("cache", field_3),
            ("removed_mirror_prefix", field_4),
        ],
    };
    let window = CanonicalConfigSection {
        section: Some("window"),
        raw: config.raw.get("window"),
        fields: &[
            ("position_string", field_5),
            ("monitor_index", field_6),
            ("full_screen", field_7),
            ("position", field_8),
            ("size", field_9),
        ],
    };
    let camera = CanonicalConfigSection {
        section: Some("camera"),
        raw: config.raw.get("camera"),
        fields: &[
            ("resolution", field_10),
            ("background_color", field_11),
            ("fps", field_12),
            ("background_opacity", field_13),
        ],
    };
    let file_writer = CanonicalConfigSection {
        section: Some("file_writer"),
        raw: config.raw.get("file_writer"),
        fields: &[
            ("ffmpeg_bin", field_14),
            ("video_codec", field_15),
            ("pixel_format", field_16),
            ("video_crf", field_17),
            ("video_preset", field_18),
            ("video_tune", field_19),
            ("video_bitrate", field_20),
            ("audio_bitrate", field_21),
            ("saturation", field_22),
            ("gamma", field_23),
        ],
    };
    let scene = CanonicalConfigSection {
        section: Some("scene"),
        raw: config.raw.get("scene"),
        fields: &[
            ("show_animation_progress", field_24),
            ("leave_progress_bars", field_25),
            ("preview_while_skipping", field_26),
            ("default_wait_time", field_27),
        ],
    };
    let vmobject = CanonicalConfigSection {
        section: Some("vmobject"),
        raw: config.raw.get("vmobject"),
        fields: &[
            ("default_stroke_width", field_28),
            ("default_stroke_color", field_29),
            ("default_fill_color", field_30),
        ],
    };
    let mobject = CanonicalConfigSection {
        section: Some("mobject"),
        raw: config.raw.get("mobject"),
        fields: &[
            ("default_mobject_color", field_31),
            ("default_light_color", field_32),
        ],
    };
    let tex = CanonicalConfigSection {
        section: Some("tex"),
        raw: config.raw.get("tex"),
        fields: &[
            ("template", field_33),
            ("font_size_for_unit_height", field_34),
        ],
    };
    let text = CanonicalConfigSection {
        section: Some("text"),
        raw: config.raw.get("text"),
        fields: &[
            ("font", field_35),
            ("alignment", field_36),
            ("font_size_for_unit_height", field_37),
        ],
    };
    let embed = CanonicalConfigSection {
        section: Some("embed"),
        raw: config.raw.get("embed"),
        fields: &[("exception_mode", field_38), ("autoreload", field_39)],
    };
    let resolution_options = CanonicalConfigSection {
        section: Some("resolution_options"),
        raw: config.raw.get("resolution_options"),
        fields: &[
            ("low", field_40),
            ("med", field_41),
            ("high", field_42),
            ("4k", field_43),
        ],
    };
    let sizes = CanonicalConfigSection {
        section: Some("sizes"),
        raw: config.raw.get("sizes"),
        fields: &[
            ("frame_height", field_44),
            ("small_buff", field_45),
            ("med_small_buff", field_46),
            ("med_large_buff", field_47),
            ("large_buff", field_48),
            ("default_mobject_to_edge_buff", field_49),
            ("default_mobject_to_mobject_buff", field_50),
        ],
    };
    let determinism = CanonicalConfigSection {
        section: Some("determinism"),
        raw: config.raw.get("determinism"),
        fields: &[("mode", field_56), ("seed", field_57)],
    };
    let render = CanonicalConfigSection {
        section: Some("render"),
        raw: config.raw.get("render"),
        fields: &[
            ("engine", field_58),
            ("aa", field_59),
            ("threads", field_60),
        ],
    };
    let cache = CanonicalConfigSection {
        section: Some("cache"),
        raw: config.raw.get("cache"),
        fields: &[("max_bytes", field_61)],
    };
    let root = CanonicalConfigSection {
        section: None,
        raw: Some(&config.raw),
        fields: &[
            ("directories", &directories),
            ("window", &window),
            ("camera", &camera),
            ("file_writer", &file_writer),
            ("scene", &scene),
            ("vmobject", &vmobject),
            ("mobject", &mobject),
            ("tex", &tex),
            ("text", &text),
            ("embed", &embed),
            ("resolution_options", &resolution_options),
            ("sizes", &sizes),
            ("key_bindings", field_51),
            ("colors", field_52),
            ("log_level", field_53),
            ("universal_import_line", field_54),
            ("ignore_manimlib_modules_on_reload", field_55),
            ("determinism", &determinism),
            ("render", &render),
            ("cache", &cache),
        ],
    };
    root.put(writer);
}
