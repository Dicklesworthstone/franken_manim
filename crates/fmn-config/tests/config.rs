//! The fm-3gl acceptance suite: fixture files exercising every supported
//! scalar and shape, exact-message diagnostics goldens, the precedence
//! matrix (defaults → user file(s) → CLI overlay), and the pack-registry
//! surface as W6 will consume it.

use fmn_config::config::{
    AaPolicy, DeterminismMode, Engine, Layer, LogLevel, ThreadPolicy, normalize_layer, overlay,
};
use fmn_config::{Config, ConfigError, PackError, PackRegistry, Value, yaml};

const EVERY_SHAPE: &str = include_str!("fixtures/every_shape.yml");
const CUSTOM_3B1B: &str = include_str!("fixtures/custom_config_3b1b.yml");

#[test]
fn cache_retention_uses_typed_precedence_without_changing_certified_identity() {
    let defaults = Config::resolve(&[], None).unwrap().config;
    assert_eq!(
        defaults.cache.max_bytes,
        fmn_config::config::DEFAULT_TYPESET_CACHE_MAX_BYTES
    );
    let limited = Config::resolve(
        &[Layer {
            name: "custom",
            text: "cache:\n  max_bytes: 8192\n",
        }],
        Some(overlay([("cache.max_bytes", Value::Int(0))])),
    )
    .unwrap()
    .config;
    assert_eq!(limited.cache.max_bytes, 0);
    assert_eq!(
        defaults.canonical_bytes().unwrap(),
        limited.canonical_bytes().unwrap()
    );
    for value in [Value::Int(-1), Value::Str("unlimited".to_owned())] {
        let error = Config::resolve(&[], Some(overlay([("cache.max_bytes", value)])))
            .unwrap_err()
            .to_string();
        assert!(error.starts_with("cache.max_bytes:"), "{error}");
        assert!(error.contains("non-negative integer"), "{error}");
    }
}

fn parse(src: &str) -> Value {
    let (value, warnings) = yaml::parse(src).expect("fixture parses");
    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    value
}

// ---------------------------------------------------------------------------
// Fixtures: every supported scalar and shape
// ---------------------------------------------------------------------------

#[test]
fn every_shape_fixture_parses_to_the_expected_values() {
    let v = parse(EVERY_SHAPE);

    // Plain-scalar resolution families.
    let plain = |k: &str| v.get_path(&format!("plain.{k}")).unwrap_or(&Value::Null);
    assert_eq!(plain("string_word"), &Value::Str("UR".into()));
    assert_eq!(
        plain("string_with_colon"),
        &Value::Str("https://example.com/path".into())
    );
    assert_eq!(plain("string_tuple"), &Value::Str("(1920, 1080)".into()));
    assert_eq!(plain("bool_python_true"), &Value::Bool(true));
    assert_eq!(plain("bool_python_false"), &Value::Bool(false));
    assert_eq!(plain("bool_lower"), &Value::Bool(true));
    assert_eq!(plain("bool_word"), &Value::Bool(true));
    assert_eq!(plain("bool_off"), &Value::Bool(false));
    assert_eq!(plain("int_plain"), &Value::Int(144));
    assert_eq!(plain("int_negative"), &Value::Int(-7));
    assert_eq!(plain("int_positive_sign"), &Value::Int(3));
    assert_eq!(plain("float_plain"), &Value::Float(1.0));
    assert_eq!(plain("float_bare_dot"), &Value::Float(2.0));
    assert_eq!(plain("float_leading_dot"), &Value::Float(0.5));
    assert_eq!(plain("float_negative"), &Value::Float(-0.25));
    assert_eq!(plain("float_exponent"), &Value::Float(6.02e23));
    assert_eq!(plain("not_a_float"), &Value::Str("1e3".into()));
    assert_eq!(plain("null_empty"), &Value::Null);
    assert_eq!(plain("null_tilde"), &Value::Null);
    assert_eq!(plain("null_word"), &Value::Null);

    // Quoted scalars.
    let quoted = |k: &str| v.get_path(&format!("quoted.{k}")).unwrap_or(&Value::Null);
    assert_eq!(
        quoted("double"),
        &Value::Str("hash # inside quotes is literal".into())
    );
    assert_eq!(quoted("double_empty"), &Value::Str(String::new()));
    assert_eq!(
        quoted("double_escapes"),
        &Value::Str("line\nbreak and \"quotes\" and a tab\there".into())
    );
    assert_eq!(quoted("double_unicode"), &Value::Str("café".into()));
    assert_eq!(quoted("single"), &Value::Str("single quoted".into()));
    assert_eq!(quoted("single_escape"), &Value::Str("it's escaped".into()));
    assert_eq!(quoted("utf8_direct"), &Value::Str("布局引擎".into()));
    assert_eq!(quoted("trailing_comment"), &Value::Str("value".into()));

    // Key shapes.
    assert_eq!(
        v.get_path("keys.4k"),
        Some(&Value::Str("(3840, 2160)".into()))
    );
    assert_eq!(
        v.get_path("keys.key_with_underscores"),
        Some(&Value::Int(1))
    );
    assert_eq!(
        v.get("keys").and_then(|k| k.get("quoted key")),
        Some(&Value::Int(2))
    );

    // Literal blocks, all three chomping modes.
    assert_eq!(
        v.get_path("blocks.strip"),
        Some(&Value::Str("first line\nsecond line".into()))
    );
    assert_eq!(
        v.get_path("blocks.clip"),
        Some(&Value::Str("kept newline\n".into()))
    );
    assert_eq!(
        v.get_path("blocks.keep"),
        Some(&Value::Str("trailing blanks kept\n\n".into()))
    );
    assert_eq!(
        v.get_path("blocks.after_blocks"),
        Some(&Value::Str("sentinel".into()))
    );
    assert_eq!(
        v.get_path("blocks.preamble_shape"),
        Some(&Value::Str(
            "\\usepackage{amsmath}\n%% percent lines are literal here\n\\DeclareMathSymbol{\\minus}{\\mathbin}{AMSa}{\"39}\n\n  deeper indentation is preserved\nback to base".into()
        ))
    );

    // Nesting.
    assert_eq!(
        v.get_path("nesting.level2.level3.level4.leaf"),
        Some(&Value::Str("deep".into()))
    );
}

// ---------------------------------------------------------------------------
// Diagnostics goldens: exact messages, positions included
// ---------------------------------------------------------------------------

#[test]
fn parse_diagnostics_are_golden() {
    // The full rendered message is the contract: position + expected-vs-found.
    for (src, golden) in [
        (
            "a: [1, 2]\n",
            "line 1, col 4: flow collections ([…], {…}) are outside the subset: the shipped config shapes use block mappings and tuple-strings",
        ),
        (
            "a:\n  - one\n",
            "line 2, col 3: block sequences (- item) are outside the subset: the shipped config shapes are nested mappings only",
        ),
        (
            "a: >-\n  folded\n",
            "line 1, col 4: folded block scalars (>) are outside the subset; use the literal style (|)",
        ),
        (
            "a:\n\tb: 1\n",
            "line 2, col 1: tab character in indentation; the subset indents with spaces only",
        ),
        (
            "a: 1\n   b: 2\n",
            "line 2, col 4: unexpected indentation: this mapping's entries start at column 1, found column 4",
        ),
        (
            "just some words\n",
            "line 1, col 16: expected \"key: value\" or \"key:\", found no ':' in \"just some words\"",
        ),
        (
            "a: \"unterminated\n",
            "line 1, col 17: unterminated double quote (multi-line quoted scalars are outside the subset)",
        ),
        (
            "--- \na: 1\n",
            "line 1, col 1: document markers (---, ...) are outside the subset: config files are single documents",
        ),
    ] {
        let e = yaml::parse(src).expect_err(src);
        assert_eq!(e.to_string(), golden, "for input {src:?}");
    }
}

#[test]
fn typed_extraction_diagnostics_are_golden() {
    let with_user = |text: &str| {
        Config::resolve(
            &[Layer {
                name: "custom_config.yml",
                text,
            }],
            None,
        )
    };

    for (src, golden) in [
        (
            "camera:\n  resolution: 1920x1080\n",
            "camera.resolution: expected a \"(w, h)\" tuple-string like \"(1920, 1080)\", found \"1920x1080\"",
        ),
        (
            "camera:\n  fps: many\n",
            "camera.fps: expected a non-negative integer, found string \"many\"",
        ),
        (
            "camera:\n  fps: -1\n",
            "camera.fps: -1 is out of range for a non-negative 32-bit integer",
        ),
        (
            "scene:\n  leave_progress_bars: 1\n",
            "scene.leave_progress_bars: expected a boolean (True/False), found integer",
        ),
        ("camera: off\n", "camera: expected mapping, found boolean"),
        (
            "determinism:\n  mode: \"fast\"\n",
            "determinism.mode: unknown value \"fast\"; expected one of: \"standard\", \"certified\"",
        ),
        (
            "render:\n  engine: \"gpu\"\n",
            "render.engine: unknown value \"gpu\"; expected one of: \"cpu\", \"metal\", \"cuda\"",
        ),
        (
            "render:\n  threads: 0\n",
            "render.threads: 0 is not a positive thread count",
        ),
        (
            "determinism:\n  seed: -5\n",
            "determinism.seed: -5 is negative; a seed is a non-negative integer",
        ),
        (
            "log_level: \"CHATTY\"\n",
            "log_level: unknown value \"CHATTY\"; expected one of: \"DEBUG\", \"INFO\", \"WARNING\", \"ERROR\", \"CRITICAL\"",
        ),
    ] {
        let err = with_user(src).expect_err(src);
        assert_eq!(err.to_string(), golden, "for input {src:?}");
    }

    // A parse failure names its layer.
    let err = with_user("a: [flow]\n").expect_err("flow");
    assert!(matches!(err, ConfigError::Parse { ref source, .. } if source == "custom_config.yml"));
    assert!(err.to_string().starts_with("custom_config.yml: line 1"));
}

// ---------------------------------------------------------------------------
// The precedence matrix
// ---------------------------------------------------------------------------

const DOCUMENTED_SPELLINGS: &[(&str, &str)] = &[
    ("style.font", "text.font"),
    ("style.tex_pack", "tex.template"),
    ("output.vcodec", "file_writer.video_codec"),
    ("output.pix_fmt", "file_writer.pixel_format"),
    ("directories.output", "directories.subdirs.output"),
    (
        "directories.raster_images",
        "directories.subdirs.raster_images",
    ),
];

fn typed_setting<'a>(config: &'a Config, path: &str) -> &'a str {
    match path {
        "text.font" => &config.text.font,
        "tex.template" => &config.tex.template,
        "file_writer.video_codec" => &config.file_writer.video_codec,
        "file_writer.pixel_format" => &config.file_writer.pixel_format,
        path if path.starts_with("directories.subdirs.") => {
            let name = path.strip_prefix("directories.subdirs.").unwrap();
            &config
                .directories
                .subdirs
                .iter()
                .find(|(key, _)| key == name)
                .unwrap()
                .1
        }
        _ => panic!("unknown typed setting: {path}"),
    }
}

fn setting_document(path: &str, value: &str) -> String {
    let mut document = String::new();
    let parts: Vec<_> = path.split('.').collect();
    for (depth, part) in parts.iter().enumerate() {
        document.push_str(&"  ".repeat(depth));
        document.push_str(part);
        if depth + 1 == parts.len() {
            document.push_str(&format!(": {value:?}\n"));
        } else {
            document.push_str(":\n");
        }
    }
    document
}

#[test]
fn documented_config_keys_change_typed_settings_and_effective_identity() {
    let defaults = Config::resolve(&[], None).unwrap().config;
    for &(alias, canonical) in DOCUMENTED_SPELLINGS {
        let configured = Config::resolve(
            &[Layer {
                name: "native.yml",
                text: &setting_document(alias, "chosen"),
            }],
            None,
        )
        .unwrap()
        .config;
        let reference = Config::resolve(
            &[Layer {
                name: "reference.yml",
                text: &setting_document(canonical, "chosen"),
            }],
            None,
        )
        .unwrap()
        .config;
        assert_eq!(typed_setting(&configured, canonical), "chosen", "{alias}");
        assert_eq!(configured, reference, "{alias}");
        assert_eq!(configured.raw.get_path(alias), None, "{alias}");
        assert_eq!(
            configured.canonical_bytes().unwrap(),
            reference.canonical_bytes().unwrap(),
            "equivalent spellings must name the same effective C4: {alias}"
        );
        assert_ne!(
            configured.canonical_bytes().unwrap(),
            defaults.canonical_bytes().unwrap(),
            "a changed effective setting must change C4: {alias}"
        );
    }
}

#[test]
fn documented_spellings_obey_layer_order_in_both_directions() {
    for &(alias, canonical) in DOCUMENTED_SPELLINGS {
        for (early, late) in [(alias, canonical), (canonical, alias)] {
            let first = setting_document(early, "first");
            let second = setting_document(late, "second");
            let layers = [
                Layer {
                    name: "custom_config.yml",
                    text: &first,
                },
                Layer {
                    name: "explicit.yml",
                    text: &second,
                },
            ];
            let configured = Config::resolve(&layers, None).unwrap().config;
            assert_eq!(typed_setting(&configured, canonical), "second", "{late}");
            for cli_path in [alias, canonical] {
                let configured = Config::resolve(
                    &layers,
                    Some(overlay([(cli_path, Value::Str("cli".into()))])),
                )
                .unwrap()
                .config;
                assert_eq!(typed_setting(&configured, canonical), "cli", "{cli_path}");
                let cli_only =
                    Config::resolve(&[], Some(overlay([(canonical, Value::Str("cli".into()))])))
                        .unwrap()
                        .config;
                assert_eq!(configured, cli_only, "shadowed values must not remain");
            }
        }
    }
}

#[test]
fn same_layer_spelling_conflicts_refuse_independent_of_key_order() {
    for &(alias, canonical) in DOCUMENTED_SPELLINGS {
        for (first, second) in [(alias, canonical), (canonical, alias)] {
            let error = Config::resolve(
                &[],
                Some(overlay([
                    (first, Value::Str("one".into())),
                    (second, Value::Str("two".into())),
                ])),
            )
            .unwrap_err()
            .to_string();
            assert!(error.contains(alias), "{error}");
            assert!(error.contains(canonical), "{error}");
            assert!(error.contains("same configuration layer"), "{error}");
        }
        let duplicates = Config::resolve(
            &[],
            Some(overlay([
                (alias, Value::Str("same".into())),
                (canonical, Value::Str("same".into())),
            ])),
        )
        .unwrap()
        .config;
        let one = Config::resolve(&[], Some(overlay([(canonical, Value::Str("same".into()))])))
            .unwrap()
            .config;
        assert_eq!(duplicates, one);
    }
}

#[test]
fn native_spellings_reject_malformed_values_and_do_not_repair_bad_parents() {
    for &(alias, canonical) in DOCUMENTED_SPELLINGS {
        for value in [
            Value::Null,
            Value::Bool(false),
            Value::Int(1),
            Value::Float(1.5),
            Value::Map(Vec::new()),
        ] {
            let error = Config::resolve(&[], Some(overlay([(alias, value)])))
                .unwrap_err()
                .to_string();
            assert!(error.starts_with(&format!("{alias}:")), "{error}");
            assert!(error.contains("expected a string"), "{error}");
        }
        let parent = canonical.rsplit_once('.').unwrap().0;
        let error = Config::resolve(
            &[],
            Some(overlay([
                (parent, Value::Bool(false)),
                (alias, Value::Str("value".into())),
            ])),
        )
        .unwrap_err()
        .to_string();
        assert!(error.starts_with(&format!("{parent}:")), "{error}");
        assert!(error.contains("expected mapping"), "{error}");
    }
    for section in ["style", "output"] {
        for value in [
            Value::Null,
            Value::Str("invalid".into()),
            Value::Bool(false),
        ] {
            let error = Config::resolve(&[], Some(overlay([(section, value)])))
                .unwrap_err()
                .to_string();
            assert!(error.starts_with(&format!("{section}:")), "{error}");
        }
    }
}

#[test]
fn native_normalization_preserves_unknown_siblings_and_default_identity() {
    let document = "style:\n  font: IBM Plex Sans\n  extension: 7\noutput:\n  pix_fmt: nv12\n  extension: custom\ndirectories:\n  output: media\n  extension: local\n";
    let configured = Config::resolve(
        &[Layer {
            name: "extended.yml",
            text: document,
        }],
        None,
    )
    .unwrap()
    .config;
    assert_eq!(configured.text.font, "IBM Plex Sans");
    assert_eq!(configured.file_writer.pixel_format, "nv12");
    assert_eq!(
        configured.raw.get_path("style.extension"),
        Some(&Value::Int(7))
    );
    assert_eq!(
        configured.raw.get_path("output.extension"),
        Some(&Value::Str("custom".into()))
    );
    assert_eq!(
        configured.raw.get_path("directories.extension"),
        Some(&Value::Str("local".into()))
    );
    let normalized = normalize_layer(parse(document)).unwrap();
    assert_eq!(normalize_layer(normalized.clone()).unwrap(), normalized);
    let defaults = Config::resolve(&[], None).unwrap().config;
    assert_eq!(defaults.raw, parse(Config::DEFAULT_DOCUMENT));
    let empty_sections = overlay([
        ("style", Value::Map(Vec::new())),
        ("output", Value::Map(Vec::new())),
    ]);
    assert_eq!(
        Config::resolve(&[], Some(empty_sections)).unwrap().config,
        defaults
    );
}

#[test]
fn direct_typed_conversion_normalizes_the_same_single_document() {
    let mut raw = Config::resolve(&[], None).unwrap().config.raw;
    raw = yaml::merge(
        raw,
        overlay([
            ("text.font", Value::Str("IBM Plex Sans".into())),
            ("style.font", Value::Str("IBM Plex Sans".into())),
            ("output.pix_fmt", Value::Str("yuv420p".into())),
        ]),
    );
    let config = Config::from_value(raw).unwrap();
    assert_eq!(config.text.font, "IBM Plex Sans");
    assert_eq!(config.raw.get("style"), None);
    assert_eq!(config.raw.get("output"), None);
    assert_eq!(Config::from_value(config.raw.clone()).unwrap(), config);
}

#[test]
fn optional_video_quality_keys_layer_and_clear_without_changing_defaults() {
    let defaults = Config::resolve(&[], None).unwrap().config;
    assert_eq!(defaults.file_writer.video_crf, None);
    assert_eq!(defaults.file_writer.video_preset, None);
    assert_eq!(defaults.file_writer.video_tune, None);
    assert_eq!(defaults.file_writer.video_bitrate, None);
    for key in ["video_crf", "video_preset", "video_tune", "video_bitrate"] {
        assert_eq!(defaults.raw.get_path(&format!("file_writer.{key}")), None);
    }
    let layers = [Layer {
        name: "quality.yml",
        text: "file_writer:\n  video_crf: 16\n  video_preset: slow\n  video_tune: animation\n",
    }];
    let configured = Config::resolve(&layers, None).unwrap().config;
    assert_eq!(configured.file_writer.video_crf, Some(16));
    assert_eq!(configured.file_writer.video_preset.as_deref(), Some("slow"));
    assert_eq!(
        configured.file_writer.video_tune.as_deref(),
        Some("animation")
    );
    assert_ne!(
        configured.canonical_bytes().unwrap(),
        defaults.canonical_bytes().unwrap()
    );

    let switched = Config::resolve(
        &layers,
        Some(overlay([
            ("file_writer.video_crf", Value::Null),
            ("file_writer.video_bitrate", Value::Int(2_000_000)),
            ("file_writer.video_tune", Value::Null),
        ])),
    )
    .unwrap()
    .config;
    assert_eq!(switched.file_writer.video_crf, None);
    assert_eq!(switched.file_writer.video_bitrate, Some(2_000_000));
    assert_eq!(switched.file_writer.video_preset.as_deref(), Some("slow"));
    assert_eq!(switched.file_writer.video_tune, None);
    assert_ne!(
        switched.canonical_bytes().unwrap(),
        configured.canonical_bytes().unwrap()
    );
}

#[test]
fn optional_video_quality_keys_reject_wrong_types_and_integer_overflow() {
    for (key, value) in [
        ("video_crf", "-1"),
        ("video_crf", "4294967296"),
        ("video_crf", "true"),
        ("video_bitrate", "-1"),
        ("video_bitrate", "4294967296"),
        ("video_bitrate", "2.5"),
        ("video_preset", "16"),
        ("video_tune", "false"),
    ] {
        let text = format!("file_writer:\n  {key}: {value}\n");
        let error = Config::resolve(
            &[Layer {
                name: "bad-quality.yml",
                text: &text,
            }],
            None,
        )
        .expect_err("invalid optional key must not disappear as if absent");
        assert!(
            error.to_string().contains(&format!("file_writer.{key}")),
            "{error}"
        );
    }
}

#[test]
fn precedence_defaults_then_user_files_then_cli() {
    let user1 = "camera:\n  fps: 60\n  background_color: \"#101010\"\n";
    let user2 = "camera:\n  fps: 90\n"; // --config_file, later than custom_config.yml
    let cli = overlay([("camera.fps", Value::Int(120))]);

    // Defaults alone.
    let c = Config::resolve(&[], None).unwrap().config;
    assert_eq!(c.camera.fps, 30);
    assert_eq!(c.camera.background_color, "#333333");

    // Defaults + one user layer: the layer's keys win, untouched keys hold.
    let c = Config::resolve(
        &[Layer {
            name: "custom_config.yml",
            text: user1,
        }],
        None,
    )
    .unwrap()
    .config;
    assert_eq!(c.camera.fps, 60);
    assert_eq!(c.camera.background_color, "#101010");
    assert_eq!(c.camera.resolution, (1920, 1080), "untouched key holds");

    // Two user layers: the later file wins where they overlap.
    let c = Config::resolve(
        &[
            Layer {
                name: "custom_config.yml",
                text: user1,
            },
            Layer {
                name: "--config_file extra.yml",
                text: user2,
            },
        ],
        None,
    )
    .unwrap()
    .config;
    assert_eq!(c.camera.fps, 90);
    assert_eq!(
        c.camera.background_color, "#101010",
        "non-overlapping key survives"
    );

    // CLI beats everything.
    let c = Config::resolve(
        &[
            Layer {
                name: "custom_config.yml",
                text: user1,
            },
            Layer {
                name: "--config_file extra.yml",
                text: user2,
            },
        ],
        Some(cli.clone()),
    )
    .unwrap()
    .config;
    assert_eq!(c.camera.fps, 120);

    // CLI directly over defaults.
    let c = Config::resolve(&[], Some(cli)).unwrap().config;
    assert_eq!(c.camera.fps, 120);
    assert_eq!(c.camera.background_color, "#333333");
}

#[test]
fn deep_merge_unions_open_maps_the_reference_way() {
    let resolved = Config::resolve(
        &[Layer {
            name: "custom_config.yml",
            text: CUSTOM_3B1B,
        }],
        None,
    )
    .unwrap();
    assert!(resolved.warnings.is_empty(), "{:?}", resolved.warnings);
    let c = resolved.config;

    // Overridden subdir keys take the user values…
    let subdir = |k: &str| {
        c.directories
            .subdirs
            .iter()
            .find(|(name, _)| name == k)
            .map(|(_, v)| v.as_str())
    };
    assert_eq!(subdir("raster_images"), Some("images/raster"));
    assert_eq!(subdir("downloads"), Some("manim_downloads"));
    // …the new key appears…
    assert_eq!(subdir("pi_creature_images"), Some("images/pi_creature/svg"));
    // …and untouched defaults survive the merge (the "use the default value
    // for the rest" comment in the real file, honored).
    assert_eq!(subdir("output"), Some("videos"));
    assert_eq!(subdir("sounds"), Some("sounds"));

    // Typed sections reflect the production values.
    assert!(c.directories.mirror_module_path);
    assert_eq!(
        c.directories.removed_mirror_prefix.as_deref(),
        Some("/Users/grant/cs/videos/")
    );
    assert_eq!(c.camera.resolution, (3840, 2160));
    assert_eq!(c.camera.background_color, "#000000");
    assert!((c.file_writer.saturation - 1.5).abs() < 1e-12);
    assert_eq!(
        c.file_writer.video_codec, "libx264",
        "unmentioned key holds"
    );
    assert_eq!(c.text.font, "CMU Serif");
    assert_eq!(c.text.alignment, "CENTER");
    assert!(c.embed.autoreload);
    assert_eq!(c.universal_import_line, "from manim_imports_ext import *");
}

#[test]
fn scalar_replaces_map_and_map_replaces_scalar() {
    // A user scalar wholesale-replaces a defaults mapping (and the typed
    // layer then names the shape error precisely) — no partial merge.
    let err = Config::resolve(
        &[Layer {
            name: "u",
            text: "sizes: none\n",
        }],
        None,
    )
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "sizes: expected mapping, found string \"none\""
    );

    // And a map can replace a scalar.
    let c = Config::resolve(
        &[Layer {
            name: "u",
            text: "log_level: \"DEBUG\"\n",
        }],
        None,
    )
    .unwrap()
    .config;
    assert_eq!(c.log_level, LogLevel::Debug);
}

#[test]
fn duplicate_keys_warn_with_their_source_layer() {
    let resolved = Config::resolve(
        &[Layer {
            name: "custom_config.yml",
            text: "camera:\n  fps: 24\n  fps: 48\n",
        }],
        None,
    )
    .unwrap();
    assert_eq!(resolved.config.camera.fps, 48, "last wins");
    assert_eq!(resolved.warnings.len(), 1);
    let w = &resolved.warnings[0];
    assert_eq!(w.source, "custom_config.yml");
    assert_eq!(w.line, 3);
    assert!(w.message.contains("duplicate key \"fps\""), "{}", w.message);
}

// ---------------------------------------------------------------------------
// The native knobs (quality selects engines, never meaning)
// ---------------------------------------------------------------------------

#[test]
fn native_knobs_type_to_opaque_enums() {
    let c = Config::resolve(
        &[Layer {
            name: "u",
            text: "determinism:\n  mode: \"certified\"\n  seed: 42\nrender:\n  engine: \"metal\"\n  aa: \"ssaa4x\"\n  threads: 8\n",
        }],
        None,
    )
    .unwrap()
    .config;
    assert_eq!(c.determinism.mode, DeterminismMode::Certified);
    assert_eq!(c.determinism.seed, 42);
    assert_eq!(c.render.engine, Engine::Metal);
    assert_eq!(c.render.aa, AaPolicy::Ssaa4x);
    assert_eq!(c.render.threads, ThreadPolicy::Fixed(8));
}

// ---------------------------------------------------------------------------
// Unknown keys are carried, not dropped (Reference tolerance)
// ---------------------------------------------------------------------------

#[test]
fn unknown_keys_survive_in_the_raw_tree() {
    let c = Config::resolve(
        &[Layer {
            name: "u",
            text: "my_extension:\n  knob: 7\ncamera:\n  fps: 25\n",
        }],
        None,
    )
    .unwrap()
    .config;
    assert_eq!(c.camera.fps, 25);
    assert_eq!(c.raw.get_path("my_extension.knob"), Some(&Value::Int(7)));
}

// ---------------------------------------------------------------------------
// The actual shipped files, when the pinned Reference checkout is present
// (scripts/manim_ref is gitignored, so this self-skips in CI; on dev
// machines it proves "the actual shipped file shapes exactly" literally)
// ---------------------------------------------------------------------------

#[test]
fn the_actual_reference_files_parse_when_checked_out() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/manim_ref");
    let Ok(default_yml) = std::fs::read_to_string(root.join("manimlib/default_config.yml")) else {
        fmn_core::test_inputs::skip_or_fail(
            "fmn-config::the_actual_reference_files_parse_when_checked_out",
            "scripts/manim_ref (pinned Reference checkout)",
        );
        return;
    };

    // default_config.yml: parses warning-free and types cleanly end-to-end
    // (its LaTeX-era keys land in the open maps and the raw tree).
    let (value, warnings) = yaml::parse(&default_yml).expect("default_config.yml parses");
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(
        value.get_path("camera.resolution"),
        Some(&Value::Str("(1920, 1080)".into()))
    );
    let resolved = Config::resolve(
        &[Layer {
            name: "manimlib/default_config.yml",
            text: &default_yml,
        }],
        None,
    )
    .expect("the shipped defaults type cleanly over ours");
    assert_eq!(resolved.config.camera.fps, 30);
    assert_eq!(
        resolved.config.text.font, "Consolas",
        "Reference value wins the merge"
    );

    // tex_templates.yml: 58 templates of quoted scalars and |- blocks.
    let templates = std::fs::read_to_string(root.join("manimlib/tex_templates.yml"))
        .expect("tex_templates.yml readable");
    let (value, warnings) = yaml::parse(&templates).expect("tex_templates.yml parses");
    assert!(warnings.is_empty(), "{warnings:?}");
    let entries = value.as_map().expect("top-level mapping");
    assert_eq!(entries.len(), 58, "every template present");
    let preamble = value
        .get_path("default.preamble")
        .and_then(|v| match v {
            Value::Str(s) => Some(s.as_str()),
            _ => None,
        })
        .expect("default preamble is a block scalar");
    assert!(preamble.starts_with("\\usepackage[english]{babel}"));
    assert!(preamble.ends_with("\\DeclareMathSymbol{\\minus}{\\mathbin}{AMSa}{\"39}"));
    assert_eq!(
        value.get_path("empty.preamble"),
        Some(&Value::Str(String::new()))
    );

    // And every template name the registry's compatibility mapping claims
    // to know is either a native pack or a *named* refusal — no template in
    // the shipped file falls through to UnknownTemplate.
    let registry = PackRegistry::builtin();
    for (name, _) in entries {
        match registry.resolve_template(name) {
            Ok(_) | Err(PackError::UnsupportedTemplate { .. }) => {}
            Err(e @ PackError::UnknownTemplate { .. }) => {
                panic!("shipped template {name:?} is unmapped: {e}");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The pack registry, as fmn-tex will consume it
// ---------------------------------------------------------------------------

#[test]
fn the_config_template_key_resolves_through_the_registry() {
    let registry = PackRegistry::builtin();

    // The default config's template resolves to the default pack.
    let c = Config::resolve(&[], None).unwrap().config;
    let pack = registry
        .resolve_template(&c.tex.template)
        .expect("default pack");
    assert_eq!(pack.name, "default");
    assert_eq!(pack.content_id, "fmd-math/pack/default");

    // A user selecting an out-of-tier template gets the named refusal, with
    // the config layer none the wiser (packs resolve at typeset time).
    let c = Config::resolve(
        &[Layer {
            name: "u",
            text: "tex:\n  template: \"ctex\"\n",
        }],
        None,
    )
    .unwrap()
    .config;
    assert_eq!(
        c.tex.template, "ctex",
        "the config layer carries the name verbatim"
    );
    match registry.resolve_template(&c.tex.template) {
        Err(PackError::UnsupportedTemplate { reason, .. }) => {
            assert!(reason.contains("CJK"), "{reason}");
        }
        other => panic!("expected the named refusal, got {other:?}"),
    }
}

#[test]
fn aac_bitrate_layers_clears_and_is_part_of_configuration_identity() {
    let default = Config::resolve(&[], None).unwrap().config;
    assert_eq!(default.file_writer.audio_bitrate, None);
    let layers = [Layer {
        name: "audio.yml",
        text: "file_writer:\n  audio_bitrate: 96000\n",
    }];
    let configured = Config::resolve(&layers, None).unwrap().config;
    assert_eq!(configured.file_writer.audio_bitrate, Some(96_000));
    assert_ne!(
        configured.canonical_bytes().unwrap(),
        default.canonical_bytes().unwrap()
    );
    let override_config = Config::resolve(
        &layers,
        Some(overlay([(
            "file_writer.audio_bitrate",
            Value::Int(192_000),
        )])),
    )
    .unwrap()
    .config;
    assert_eq!(override_config.file_writer.audio_bitrate, Some(192_000));
    let cleared = Config::resolve(
        &layers,
        Some(overlay([("file_writer.audio_bitrate", Value::Null)])),
    )
    .unwrap()
    .config;
    assert_eq!(cleared.file_writer.audio_bitrate, None);
    for value in ["-1", "4294967296", "true", "1.5", "96k"] {
        let text = format!("file_writer:\n  audio_bitrate: {value}\n");
        let err = Config::resolve(
            &[Layer {
                name: "bad-audio.yml",
                text: &text,
            }],
            None,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("file_writer.audio_bitrate"),
            "{err}"
        );
    }
}
