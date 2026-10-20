#![forbid(unsafe_code)]
#![allow(clippy::expect_used)]

//! fm-c53.7: formatter contracts run without shipping features; `batch` adds
//! real `fmn` process checks. Fixtures are retained and never cleaned up here.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use fmn_cli::{
    CacheReport, CertificationReport, DoctorSnapshot, ExecutionPlanReport, FfmpegReport,
    FontReport, TopologySource,
};

const KINDS: [&str; 7] = [
    "topology",
    "execution_plan",
    "ffmpeg",
    "cache",
    "fonts",
    "math_packs",
    "certification",
];
const MAX_OUTPUT: usize = 1024 * 1024;
static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

fn retained_root() -> std::io::Result<PathBuf> {
    for _ in 0..1024 {
        let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "fmn-doctor-smoke-{}-{sequence}",
            std::process::id()
        ));
        match std::fs::create_dir(&root) {
            Ok(()) => return root.canonicalize(),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate a unique retained doctor fixture",
    ))
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Value {
    Text(String),
    Uint(u64),
    Bool(bool),
    Null,
    Strings(Vec<String>),
    Records(Vec<Record>),
}

type Record = BTreeMap<String, Value>;
type Check<T> = Result<T, &'static str>;

/// Limited to doctor v1 records and one level of hardware-probe objects.
/// Independent decoding catches escaping bugs in the production formatter.
struct FlatJson<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
}

impl<'a> FlatJson<'a> {
    fn space(&mut self) {
        while self
            .chars
            .peek()
            .is_some_and(|c| matches!(c, ' ' | '\t' | '\r' | '\n'))
        {
            self.chars.next();
        }
    }

    fn take(&mut self, expected: char) -> Check<()> {
        if self.chars.next() == Some(expected) {
            Ok(())
        } else {
            Err("unexpected JSON token")
        }
    }

    fn hex_quad(&mut self) -> Check<u32> {
        let mut value = 0;
        for _ in 0..4 {
            value = value * 16
                + self
                    .chars
                    .next()
                    .and_then(|c| c.to_digit(16))
                    .ok_or("invalid Unicode escape")?;
        }
        Ok(value)
    }

    fn string(&mut self) -> Check<String> {
        self.take('"')?;
        let mut value = String::new();
        loop {
            let c = self.chars.next().ok_or("unterminated string")?;
            match c {
                '"' => return Ok(value),
                '\\' => {
                    let escaped = match self.chars.next().ok_or("incomplete escape")? {
                        '"' => '"',
                        '\\' => '\\',
                        '/' => '/',
                        'b' => '\u{8}',
                        'f' => '\u{c}',
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        'u' => {
                            let first = self.hex_quad()?;
                            let scalar = if (0xd800..=0xdbff).contains(&first) {
                                self.take('\\')?;
                                self.take('u')?;
                                let second = self.hex_quad()?;
                                if !(0xdc00..=0xdfff).contains(&second) {
                                    return Err("invalid surrogate pair");
                                }
                                0x10000 + ((first - 0xd800) << 10) + second - 0xdc00
                            } else {
                                first
                            };
                            char::from_u32(scalar).ok_or("unpaired surrogate")?
                        }
                        _ => return Err("unknown string escape"),
                    };
                    value.push(escaped);
                }
                c if c <= '\u{1f}' => return Err("unescaped control character"),
                c => value.push(c),
            }
        }
    }

    fn literal(&mut self, suffix: &str, value: Value) -> Check<Value> {
        for c in suffix.chars() {
            self.take(c)?;
        }
        Ok(value)
    }

    fn value(&mut self, records_allowed: bool) -> Check<Value> {
        match self.chars.peek().copied().ok_or("missing value")? {
            '"' => self.string().map(Value::Text),
            't' => self.literal("true", Value::Bool(true)),
            'f' => self.literal("false", Value::Bool(false)),
            'n' => self.literal("null", Value::Null),
            '[' => {
                self.take('[')?;
                self.space();
                if records_allowed && self.chars.peek() == Some(&'{') {
                    let mut records = Vec::new();
                    loop {
                        if records.len() == 256 {
                            return Err("array limit exceeded");
                        }
                        records.push(self.object(false)?);
                        self.space();
                        if self.chars.peek() != Some(&',') {
                            break;
                        }
                        self.take(',')?;
                        self.space();
                    }
                    self.take(']')?;
                    return Ok(Value::Records(records));
                }
                let mut strings = Vec::new();
                if self.chars.peek() != Some(&']') {
                    loop {
                        if strings.len() == 256 {
                            return Err("array limit exceeded");
                        }
                        strings.push(self.string()?);
                        self.space();
                        if self.chars.peek() != Some(&',') {
                            break;
                        }
                        self.take(',')?;
                        self.space();
                    }
                }
                self.take(']')?;
                Ok(Value::Strings(strings))
            }
            '0'..='9' => {
                let first = self.chars.next().ok_or("missing integer")?;
                let mut number = u64::from(first.to_digit(10).ok_or("invalid integer")?);
                while self.chars.peek().is_some_and(char::is_ascii_digit) {
                    if first == '0' {
                        return Err("leading zero");
                    }
                    let digit = self
                        .chars
                        .next()
                        .and_then(|c| c.to_digit(10))
                        .ok_or("invalid integer")?;
                    number = number
                        .checked_mul(10)
                        .and_then(|n| n.checked_add(u64::from(digit)))
                        .ok_or("integer overflow")?;
                }
                Ok(Value::Uint(number))
            }
            _ => Err("unsupported doctor field value"),
        }
    }

    fn object(&mut self, records_allowed: bool) -> Check<Record> {
        let mut record = Record::new();
        self.space();
        self.take('{')?;
        self.space();
        if self.chars.peek() != Some(&'}') {
            loop {
                if record.len() == 32 {
                    return Err("field limit exceeded");
                }
                let key = self.string()?;
                self.space();
                self.take(':')?;
                self.space();
                let value = self.value(records_allowed)?;
                if record.insert(key, value).is_some() {
                    return Err("duplicate field");
                }
                self.space();
                if self.chars.peek() != Some(&',') {
                    break;
                }
                self.take(',')?;
                self.space();
            }
        }
        self.take('}')?;
        Ok(record)
    }

    fn record(line: &'a str) -> Check<Record> {
        if line.len() > 128 * 1024 {
            return Err("record limit exceeded");
        }
        let mut parser = Self {
            chars: line.chars().peekable(),
        };
        let record = parser.object(true)?;
        parser.space();
        if parser.chars.next().is_some() {
            return Err("trailing JSON data");
        }
        Ok(record)
    }
}

fn records(output: &str) -> Check<Vec<Record>> {
    if output.len() > MAX_OUTPUT || !output.ends_with('\n') {
        return Err("invalid NDJSON framing");
    }
    let lines: Vec<_> = output.split_terminator('\n').collect();
    if lines.is_empty() || lines.len() > 8 {
        return Err("invalid NDJSON record count");
    }
    lines.into_iter().map(FlatJson::record).collect()
}

fn text<'a>(record: &'a Record, name: &str) -> Check<&'a str> {
    match record.get(name) {
        Some(Value::Text(value)) => Ok(value),
        _ => Err("expected string field"),
    }
}

#[derive(Clone, Copy)]
enum Field {
    Text,
    Uint,
    Bool,
    Strings,
    Records,
    NullableText,
    NullableUint,
}

fn fields(record: &Record, expected: &[(&str, Field)]) -> Check<()> {
    if record.len() != expected.len() {
        return Err("wrong field inventory");
    }
    for (name, kind) in expected {
        let valid = matches!(
            (kind, record.get(*name)),
            (Field::Text | Field::NullableText, Some(Value::Text(_)))
                | (Field::Uint | Field::NullableUint, Some(Value::Uint(_)))
                | (Field::Bool, Some(Value::Bool(_)))
                | (Field::Strings, Some(Value::Strings(_)))
                | (Field::Records, Some(Value::Records(_)))
                | (Field::NullableText | Field::NullableUint, Some(Value::Null))
        ) || matches!(
            (kind, record.get(*name)),
            (Field::Records, Some(Value::Strings(values))) if values.is_empty()
        );
        if !valid {
            return Err("missing or mistyped field");
        }
    }
    Ok(())
}

fn doctor_schema(records: &[Record]) -> Check<()> {
    use Field::{Bool, NullableText, NullableUint, Records, Strings, Text, Uint};
    if records.len() != KINDS.len() {
        return Err("doctor must emit seven records");
    }
    for (record, expected_kind) in records.iter().zip(KINDS) {
        if text(record, "schema")? != "fmn.doctor"
            || record.get("version") != Some(&Value::Uint(1))
            || text(record, "kind")? != expected_kind
        {
            return Err("wrong doctor identity or order");
        }
        let payload: &[(&str, Field)] = match expected_kind {
            "topology" => &[
                ("source", Text),
                ("source_detail", NullableText),
                ("logical_cores", Uint),
                ("physical_cores", Uint),
                ("hardware_supported_tier", Text),
                ("active_compiled_tier", Text),
            ],
            "execution_plan" => &[
                ("determinism", Text),
                ("engine", Text),
                ("frames_in_flight", Uint),
                ("scene_threads", Uint),
                ("render_teams", Uint),
                ("render_threads", Uint),
                ("output_threads", Uint),
                ("fine_tile", Uint),
                ("macro_tile", Uint),
                ("estimated_in_flight_bytes", Uint),
                ("output_format", Text),
                ("tuning_source", Text),
            ],
            "ffmpeg" if record.get("available") == Some(&Value::Bool(true)) => &[
                ("available", Bool),
                ("path", Text),
                ("sha256", Text),
                ("ffmpeg_version", Text),
                ("hardware_encoders", Strings),
                ("hardware_encoder_probe_error", NullableText),
                ("usable_hardware_encoders", Strings),
                ("hardware_encoder_probes", Records),
            ],
            "ffmpeg" => &[
                ("available", Bool),
                ("attempted", Text),
                ("reason", Text),
                ("alternative", Text),
            ],
            "cache" if record.get("resolved") == Some(&Value::Bool(true)) => &[
                ("resolved", Bool),
                ("root", Text),
                ("exists", Bool),
                ("direct_entries", NullableUint),
                ("typeset_max_bytes", Uint),
                ("typeset_bytes", NullableUint),
                ("warning", NullableText),
            ],
            "cache" => &[
                ("resolved", Bool),
                ("reason", Text),
                ("typeset_max_bytes", Uint),
                ("typeset_bytes", NullableUint),
            ],
            "fonts" => &[
                ("selected", Text),
                ("bundled", Strings),
                ("user", Strings),
                ("complete", Bool),
                ("detail", NullableText),
            ],
            "math_packs" => &[("packs", Strings)],
            "certification" => &[("platform", Text), ("supported", Bool), ("detail", Text)],
            _ => return Err("unknown doctor kind"),
        };
        let mut expected = vec![("schema", Text), ("version", Uint), ("kind", Text)];
        expected.extend_from_slice(payload);
        fields(record, &expected)?;
    }
    let topology = &records[0];
    for name in ["logical_cores", "physical_cores"] {
        if !matches!(topology.get(name), Some(Value::Uint(1..))) {
            return Err("empty topology");
        }
    }
    if !matches!(text(topology, "source")?, "linux-sysfs" | "fallback") {
        return Err("unknown topology source");
    }
    let plan = &records[1];
    for name in [
        "frames_in_flight",
        "scene_threads",
        "render_teams",
        "render_threads",
        "output_threads",
        "fine_tile",
        "macro_tile",
        "estimated_in_flight_bytes",
    ] {
        if !matches!(plan.get(name), Some(Value::Uint(1..))) {
            return Err("empty execution plan bound");
        }
    }
    if !matches!(text(plan, "determinism")?, "standard" | "certified")
        || !matches!(
            text(plan, "engine")?,
            "certified-cpu" | "fast-cpu" | "metal" | "cuda"
        )
    {
        return Err("unknown execution mode");
    }
    if records[2].get("available") == Some(&Value::Bool(true)) {
        let ffmpeg = &records[2];
        let digest = text(ffmpeg, "sha256")?;
        if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("invalid ffmpeg fingerprint");
        }
        let Some(Value::Strings(compiled)) = ffmpeg.get("hardware_encoders") else {
            return Err("expected compiled encoder inventory");
        };
        let probes = match ffmpeg.get("hardware_encoder_probes") {
            Some(Value::Records(probes)) => probes.as_slice(),
            // Empty JSON arrays have no element type to infer while parsing.
            Some(Value::Strings(values)) if values.is_empty() => &[],
            _ => return Err("expected hardware probe records"),
        };
        let mut observed = Vec::new();
        let mut usable = Vec::new();
        for probe in probes {
            fields(
                probe,
                &[
                    ("encoder", Text),
                    ("available", Bool),
                    ("detail", NullableText),
                ],
            )?;
            let encoder = text(probe, "encoder")?;
            if !compiled.iter().any(|name| name == encoder) || observed.contains(&encoder) {
                return Err("probe does not name a unique compiled encoder");
            }
            observed.push(encoder);
            if probe.get("available") == Some(&Value::Bool(true)) {
                if probe.get("detail") != Some(&Value::Null) {
                    return Err("successful probe carries a failure diagnostic");
                }
                usable.push(encoder.to_owned());
            } else if !matches!(probe.get("detail"), Some(Value::Text(reason)) if !reason.is_empty())
            {
                return Err("unusable encoder lacks a reason");
            }
        }
        if ffmpeg.get("usable_hardware_encoders") != Some(&Value::Strings(usable)) {
            return Err("usable inventory disagrees with functional probes");
        }
        if ffmpeg.get("hardware_encoder_probe_error") == Some(&Value::Null)
            && probes.len() != compiled.len()
        {
            return Err("compiled encoder omitted without a probe warning");
        }
    }
    for (record, name) in [(&records[4], "bundled"), (&records[5], "packs")] {
        if !matches!(record.get(name), Some(Value::Strings(values)) if !values.is_empty() && values.iter().all(|v| !v.is_empty()))
        {
            return Err("empty native inventory");
        }
    }
    Ok(())
}

fn snapshot() -> DoctorSnapshot {
    DoctorSnapshot {
        topology_source: TopologySource::Fallback {
            reason: "fixture topology".to_owned(),
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
            path: PathBuf::from("/fixture/ffmpeg"),
            sha256: "ab".repeat(32),
            version: "ffmpeg fixture".to_owned(),
            hardware_encoders: vec!["h264_nvenc".to_owned()],
            hardware_encoder_probes: Vec::new(),
            hardware_encoder_probe_error: Some("fixture inventory warning".to_owned()),
        },
        cache: CacheReport::Configured {
            root: PathBuf::from("/fixture/cache"),
            exists: true,
            direct_entries: Some(3),
            typeset_max_bytes: 1_073_741_824,
            typeset_bytes: Some(512),
            warning: None,
        },
        fonts: FontReport {
            selected: "Computer Modern".to_owned(),
            bundled: vec!["Computer Modern".to_owned()],
            user: Vec::new(),
            complete: true,
            detail: None,
        },
        math_packs: vec!["default".to_owned(), "minimal".to_owned()],
        certification: CertificationReport {
            platform: "linux-x86-64".to_owned(),
            supported: true,
            detail: "fixture certified target".to_owned(),
        },
    }
}

#[test]
fn doctor_snapshot_robot_has_exact_typed_available_records() {
    let output = snapshot().to_ndjson().expect("format doctor snapshot");
    let parsed = records(&output).expect("strict independent NDJSON parse");
    doctor_schema(&parsed).expect("complete doctor v1 schema and values");
    assert_eq!(parsed[0]["logical_cores"], Value::Uint(8));
    assert_eq!(parsed[1]["estimated_in_flight_bytes"], Value::Uint(4096));
    assert_eq!(parsed[2]["sha256"], Value::Text("ab".repeat(32)));
    assert_eq!(parsed[3]["direct_entries"], Value::Uint(3));
    assert_eq!(parsed[3]["typeset_max_bytes"], Value::Uint(1_073_741_824));
    assert_eq!(parsed[3]["typeset_bytes"], Value::Uint(512));
    assert_eq!(parsed[4]["user"], Value::Strings(Vec::new()));
    assert_eq!(parsed[6]["supported"], Value::Bool(true));
}

#[test]
fn doctor_probe_records_preserve_usable_and_compiled_only_outcomes() {
    let diagnostic = r#"Cannot load "libcuda.so.1" from C:\GPU\é雪"#;
    for available in [true, false] {
        let mut input = snapshot();
        let FfmpegReport::Available {
            hardware_encoder_probes,
            hardware_encoder_probe_error,
            ..
        } = &mut input.ffmpeg
        else {
            panic!("the fixture contains an available executable");
        };
        *hardware_encoder_probes = vec![fmn_output::EncoderProbe {
            encoder: "h264_nvenc".to_owned(),
            available,
            detail: (!available).then(|| diagnostic.to_owned()),
        }];
        *hardware_encoder_probe_error = None;

        let parsed = records(&input.to_ndjson().expect("format probe snapshot"))
            .expect("independently decode nested probe records");
        doctor_schema(&parsed).expect("compiled and usable inventories agree with probes");
        assert_eq!(
            parsed[2]["hardware_encoders"],
            Value::Strings(vec!["h264_nvenc".to_owned()])
        );
        assert_eq!(
            parsed[2]["usable_hardware_encoders"],
            Value::Strings(if available {
                vec!["h264_nvenc".to_owned()]
            } else {
                Vec::new()
            })
        );
        let Value::Records(probes) = &parsed[2]["hardware_encoder_probes"] else {
            panic!("one compiled encoder yields one typed probe record");
        };
        assert_eq!(probes.len(), 1);
        assert_eq!(probes[0]["available"], Value::Bool(available));
        assert_eq!(
            probes[0]["detail"],
            if available {
                Value::Null
            } else {
                Value::Text(diagnostic.to_owned())
            }
        );
        let status = if available {
            "h264_nvenc: available".to_owned()
        } else {
            format!("h264_nvenc: compiled-in (unusable: {diagnostic})")
        };
        assert!(input.to_human().contains(&status));

        let mut inconsistent = parsed.clone();
        inconsistent[2].insert(
            "usable_hardware_encoders".to_owned(),
            Value::Strings(if available {
                Vec::new()
            } else {
                vec!["h264_nvenc".to_owned()]
            }),
        );
        assert!(doctor_schema(&inconsistent).is_err());
        let mut mistyped = parsed;
        let Some(Value::Records(probes)) = mistyped[2].get_mut("hardware_encoder_probes") else {
            panic!("nested probe records survive cloning");
        };
        probes[0].insert("available".to_owned(), Value::Text("true".to_owned()));
        assert!(doctor_schema(&mistyped).is_err());
    }
}

#[test]
fn doctor_default_feature_run_collects_real_host_capabilities() {
    let root = retained_root().expect("create owned doctor fixture");
    let missing = root.join("missing-ffmpeg-é雪");
    let cache = root.join("cache-é雪");
    // This live in-process case assumes a healthy host configuration. Its
    // explicit layer selects CPU/RGBA8 and a bundled font; it does not erase
    // ambient layers. The binary cases below isolate cwd and environment.
    let config = root.join("doctor.yml");
    std::fs::write(
        &config,
        b"render:\n  engine: cpu\nfile_writer:\n  pixel_format: rgba8\ntext:\n  font: IBM Plex Sans\ncache:\n  max_bytes: 8192\n",
    )
    .expect("write owned doctor config");
    let output = fmn_cli::run([
        "doctor",
        "--robot",
        "--threads",
        "1",
        "--config_file",
        config.to_str().expect("UTF-8 config path"),
        "--ffmpeg",
        missing.to_str().expect("UTF-8 missing path"),
        "--cache-dir",
        cache.to_str().expect("UTF-8 cache path"),
    ]);
    assert_eq!(output.code, 0, "{output:?}");
    assert!(output.stderr.is_empty());
    let parsed = records(&output.stdout).expect("real in-process doctor NDJSON");
    doctor_schema(&parsed).expect("real host topology, plan and bundled inventories");
    assert_eq!(parsed[2]["available"], Value::Bool(false));
    assert_eq!(
        text(&parsed[2], "attempted").expect("missing path"),
        missing.to_str().expect("UTF-8 path")
    );
    assert_eq!(
        text(&parsed[3], "root").expect("cache root"),
        cache.to_str().expect("UTF-8 path")
    );
    assert_eq!(parsed[4]["complete"], Value::Bool(true));
    assert_eq!(parsed[3]["typeset_max_bytes"], Value::Uint(8192));
    assert_eq!(parsed[3]["typeset_bytes"], Value::Uint(0));
    assert!(!cache.exists());
    assert!(!missing.exists());
}

fn doctor_cache_record(fixture: &Path, cache: &Path) -> Record {
    let config = fixture.join("cache-doctor.yml");
    std::fs::write(
        &config,
        b"render:\n  engine: cpu\nfile_writer:\n  pixel_format: rgba8\ncache:\n  max_bytes: 8192\n",
    )
    .expect("write doctor configuration");
    let missing = fixture.join("no-ffmpeg");
    let output = fmn_cli::run([
        "doctor",
        "--robot",
        "--threads",
        "1",
        "--config_file",
        config.to_str().expect("config path"),
        "--ffmpeg",
        missing.to_str().expect("missing ffmpeg path"),
        "--cache-dir",
        cache.to_str().expect("cache path"),
    ]);
    assert_eq!(output.code, 0, "{output:?}");
    assert!(output.stderr.is_empty());
    let mut parsed = records(&output.stdout).expect("doctor records");
    doctor_schema(&parsed).expect("typed doctor schema");
    parsed.remove(3)
}

#[test]
fn doctor_reports_owned_envelope_bytes_without_opening_or_mutating_the_namespace() {
    use fmn_cache::{
        CacheError, KeyBuilder, NamespacePolicy, Store, StoreConfig, object_relative_path,
    };
    use fmn_platform::clock::StdClock;
    use fmn_platform::fs::{FsError, StdFs};

    let fixture = retained_root().expect("fresh retained fixture");
    let cache = fixture.join("cache");
    let version = fmn::tex::TYPESET_FORMAT_VERSION;
    let store = Store::open(
        Arc::new(StdFs),
        Arc::new(StdClock::new()),
        &cache,
        StoreConfig::default(),
    )
    .expect("owned cache");
    assert_eq!(
        Store::inspect_namespace_bytes(&StdFs, &cache, "typeset", version, 0)
            .expect("absent namespace"),
        0
    );
    assert!(
        !cache.join("ns").exists(),
        "inspection creates no namespace"
    );
    let namespace = store
        .namespace(
            "typeset",
            version,
            NamespacePolicy {
                ceiling_bytes: None,
            },
        )
        .expect("typeset namespace");
    let key = KeyBuilder::new("doctor-size").finish().expect("cache key");
    let payload = b"one envelope";
    namespace.put(&key, payload).expect("owned cache entry");
    drop(namespace);
    drop(store);
    let namespace = cache.join("ns/typeset").join(format!("v{version}"));
    let object = namespace
        .join("objects")
        .join(object_relative_path(key.digest()));
    let bytes = std::fs::metadata(&object).expect("object metadata").len();
    assert!(
        bytes > payload.len() as u64,
        "usage includes envelope overhead"
    );
    let paths = [
        cache.join("STORE_OWNER"),
        cache.join("STORE_FORMAT"),
        namespace.join("index"),
        object,
    ];
    let before: Vec<_> = paths
        .iter()
        .map(|path| std::fs::read(path).expect("existing cache file"))
        .collect();
    assert_eq!(
        Store::inspect_namespace_bytes(&StdFs, &cache, "typeset", version, 2)
            .expect("exact entry budget"),
        bytes
    );
    assert!(matches!(
        Store::inspect_namespace_bytes(&StdFs, &cache, "typeset", version, 1),
        Err(CacheError::Storage(FsError::TooManyEntries {
            limit: 1,
            ..
        }))
    ));
    let report = doctor_cache_record(&fixture, &cache);
    assert_eq!(report["typeset_bytes"], Value::Uint(bytes));
    assert_eq!(report["typeset_max_bytes"], Value::Uint(8192));
    assert_eq!(report["warning"], Value::Null);
    for (path, bytes) in paths.iter().zip(before) {
        assert_eq!(
            std::fs::read(path).expect("cache file after inspection"),
            bytes
        );
    }
    assert!(
        !namespace.join("lock").exists(),
        "inspection creates no maintenance lock"
    );
}

#[test]
fn doctor_reports_unknown_usage_for_foreign_cache_roots_without_claiming_them() {
    let fixture = retained_root().expect("fresh retained fixture");
    let cache = fixture.join("foreign-cache");
    std::fs::create_dir(&cache).expect("foreign directory");
    let sentinel = cache.join("foreign.txt");
    std::fs::write(&sentinel, b"leave foreign data alone").expect("foreign sentinel");
    let report = doctor_cache_record(&fixture, &cache);
    assert_eq!(report["typeset_bytes"], Value::Null);
    assert!(
        text(&report, "warning")
            .expect("ownership refusal")
            .contains("ownership")
    );
    assert_eq!(
        std::fs::read(sentinel).expect("foreign sentinel survives"),
        b"leave foreign data alone"
    );
    assert!(!cache.join("STORE_OWNER").exists());
    assert!(!cache.join("STORE_FORMAT").exists());
    assert!(!cache.join("ns").exists());
}

#[cfg(unix)]
#[test]
fn doctor_refuses_linked_object_trees_and_entries_without_following_them() {
    use fmn_cache::{Store, StoreConfig};
    use fmn_platform::clock::StdClock;
    use fmn_platform::fs::StdFs;

    let fixture = retained_root().expect("fresh retained fixture");
    let outside = fixture.join("outside");
    std::fs::create_dir(&outside).expect("external directory");
    let sentinel = outside.join("sentinel");
    std::fs::write(&sentinel, b"external data").expect("external sentinel");
    for leaf in [false, true] {
        let cache = fixture.join(if leaf { "leaf-cache" } else { "tree-cache" });
        let store = Store::open(
            Arc::new(StdFs),
            Arc::new(StdClock::new()),
            &cache,
            StoreConfig::default(),
        )
        .expect("owned cache");
        drop(store);
        let namespace = cache
            .join("ns/typeset")
            .join(format!("v{}", fmn::tex::TYPESET_FORMAT_VERSION));
        if leaf {
            let shard = namespace.join("objects/00");
            std::fs::create_dir_all(&shard).expect("owned object shard");
            std::os::unix::fs::symlink(&sentinel, shard.join("00".repeat(31)))
                .expect("linked object");
        } else {
            std::fs::create_dir_all(&namespace).expect("owned namespace path");
            std::os::unix::fs::symlink(&outside, namespace.join("objects"))
                .expect("linked object tree");
        }
        let report = doctor_cache_record(&fixture, &cache);
        assert_eq!(report["typeset_bytes"], Value::Null);
        assert!(
            text(&report, "warning")
                .expect("link refusal")
                .contains("Link")
        );
        assert!(!namespace.join("index").exists());
        assert!(!namespace.join("lock").exists());
    }
    assert_eq!(
        std::fs::read(sentinel).expect("external sentinel survives"),
        b"external data"
    );
}

#[test]
fn doctor_snapshot_robot_round_trips_escapes_and_optional_branches() {
    let unusual = "é雪🚀/quote\"-slash\\-line\n-tab\t-return\r-control\u{1f}";
    let mut input = snapshot();
    input.ffmpeg = FfmpegReport::Unavailable {
        attempted: PathBuf::from(unusual),
        reason: unusual.to_owned(),
        alternative: "native PNG, GIF, y4m".to_owned(),
    };
    input.cache = CacheReport::Configured {
        root: PathBuf::from(unusual),
        exists: false,
        direct_entries: None,
        typeset_max_bytes: u64::MAX,
        typeset_bytes: None,
        warning: Some(unusual.to_owned()),
    };
    input.fonts.selected = unusual.to_owned();
    input.fonts.user = vec![unusual.to_owned()];
    input.fonts.complete = false;
    input.fonts.detail = Some(unusual.to_owned());
    let output = input.to_ndjson().expect("UTF-8 paths are representable");
    assert_eq!(
        output.lines().count(),
        7,
        "embedded controls must remain escaped"
    );
    let parsed = records(&output).expect("escaped NDJSON parses");
    doctor_schema(&parsed).expect("unavailable ffmpeg and nullable cache fields are typed");
    for (index, key) in [
        (2, "attempted"),
        (2, "reason"),
        (3, "root"),
        (3, "warning"),
        (4, "selected"),
        (4, "detail"),
    ] {
        assert_eq!(text(&parsed[index], key).expect("decoded field"), unusual);
    }
    assert_eq!(parsed[4]["user"], Value::Strings(vec![unusual.to_owned()]));
    assert_eq!(parsed[3]["direct_entries"], Value::Null);
    assert_eq!(parsed[3]["typeset_max_bytes"], Value::Uint(u64::MAX));
    assert_eq!(parsed[3]["typeset_bytes"], Value::Null);
    input.cache = CacheReport::Unresolved {
        reason: unusual.to_owned(),
        typeset_max_bytes: 0,
    };
    let parsed = records(&input.to_ndjson().expect("unresolved snapshot"))
        .expect("unresolved record parses");
    doctor_schema(&parsed).expect("unresolved cache variant schema");
    assert_eq!(text(&parsed[3], "reason").expect("cache reason"), unusual);
    assert_eq!(parsed[3]["typeset_max_bytes"], Value::Uint(0));
    assert_eq!(parsed[3]["typeset_bytes"], Value::Null);
}

#[test]
fn doctor_snapshot_human_exposes_capabilities_and_degradation() {
    let mut input = snapshot();
    let human = input.to_human();
    for expected in [
        "FrankenManim doctor",
        "topology: fallback (fixture topology)",
        "cores: 8 logical / 4 physical",
        "SIMD: hardware x86-64-v4 / active build portable",
        "plan: certified certified-cpu, 2 frame slots, 2 render teams / 4 render threads",
        "hardware encoders:",
        "  h264_nvenc: compiled-in (not probed)",
        "hardware encoder probe warning: fixture inventory warning",
        "cache: /fixture/cache (exists true, direct entries 3)",
        "typeset cache ceiling: 1073741824 bytes (cache.max_bytes)",
        "typeset cache usage: 512 bytes",
        "fonts: selected Computer Modern; inventory complete",
        "math packs: default, minimal",
        "certification: supported target — fixture certified target",
    ] {
        assert!(
            human.lines().any(|line| line == expected),
            "missing human report line: {expected}"
        );
    }
    assert!(human.contains("ffmpeg: /fixture/ffmpeg (ffmpeg fixture, sha256 ab"));
    input.ffmpeg = FfmpegReport::Unavailable {
        attempted: PathBuf::from("/fixture/missing"),
        reason: "fixture missing tool".to_owned(),
        alternative: "native PNG, GIF, y4m".to_owned(),
    };
    input.cache = CacheReport::Unresolved {
        reason: "fixture cache unavailable".to_owned(),
        typeset_max_bytes: 0,
    };
    input.fonts.complete = false;
    input.certification.supported = false;
    let human = input.to_human();
    assert!(human.contains("ffmpeg: unavailable at"));
    assert!(human.contains("fixture missing tool"));
    for expected in [
        "alternative: native PNG, GIF, y4m",
        "cache: unresolved (fixture cache unavailable)",
        "typeset cache ceiling: 0 bytes (cache.max_bytes)",
        "typeset cache usage: unknown",
        "fonts: selected Computer Modern; inventory partial",
        "certification: unsupported/pending target — fixture certified target",
    ] {
        assert!(
            human.lines().any(|line| line == expected),
            "missing degraded report: {expected}"
        );
    }
}

#[test]
fn flat_json_oracle_accepts_all_string_escapes_and_u64_boundary() {
    let parsed = FlatJson::record(r#"{"s":"\"\\\/\b\f\n\r\t\u0000\u00e9\ud83d\ude80","n":18446744073709551615,"a":["雪","é"],"b":false,"z":null}"#).expect("all supported JSON scalar forms");
    assert_eq!(
        text(&parsed, "s").expect("string"),
        "\"\\/\u{8}\u{c}\n\r\t\0é🚀"
    );
    assert_eq!(parsed["n"], Value::Uint(u64::MAX));
    assert_eq!(
        parsed["a"],
        Value::Strings(vec!["雪".to_owned(), "é".to_owned()])
    );
    assert_eq!(parsed["b"], Value::Bool(false));
    assert_eq!(parsed["z"], Value::Null);
}

#[test]
fn flat_json_oracle_refuses_malformed_duplicate_and_trailing_values() {
    for invalid in [
        "",
        "[]",
        "{",
        r#"{"x":1,}"#,
        r#"{"x":1}false"#,
        r#"{"x":1,"x":2}"#,
        r#"{"x":1,"\u0078":2}"#,
        r#"{"x":01}"#,
        r#"{"x":-1}"#,
        r#"{"x":1.0}"#,
        r#"{"x":1e2}"#,
        r#"{"x":18446744073709551616}"#,
        r#"{"x":NaN}"#,
        r#"{"x":truex}"#,
        r#"{"x":{}}"#,
        r#"{"x":[1]}"#,
        r#"{"x":["a",]}"#,
        r#"{"x":[{"encoder":"n","encoder":"m"}]}"#,
        r#"{"x":[{"nested":[{}]}]}"#,
        r#"{"x":[{},"mixed"]}"#,
        r#"{"x":["mixed",{}]}"#,
        r#"{"x":[{},]}"#,
        r#"{"x":[{}]false}"#,
        r#"{"x":"\q"}"#,
        r#"{"x":"\u12xz"}"#,
        r#"{"x":"\ud800"}"#,
        r#"{"x":"\udc00"}"#,
        r#"{"x":"\ud800\u0041"}"#,
        "{\"x\":\"raw\nnewline\"}",
        "{\"x\":\"raw\ttab\"}",
        "{\"x\":\"unterminated}",
    ] {
        assert!(
            FlatJson::record(invalid).is_err(),
            "oracle accepted malformed input: {invalid:?}"
        );
    }
    assert!(records("{}").is_err(), "missing final newline");
    assert!(records("{}\n\n").is_err(), "blank record");
    assert!(records(&"{}\n".repeat(9)).is_err(), "too many records");
}

#[test]
fn doctor_schema_oracle_refuses_structural_and_value_drift() {
    let good = records(&snapshot().to_ndjson().expect("fixture")).expect("fixture JSON");
    for (index, key, wrong) in [
        (0, "version", Value::Uint(2)),
        (0, "logical_cores", Value::Text("8".to_owned())),
        (1, "frames_in_flight", Value::Uint(0)),
        (2, "available", Value::Text("true".to_owned())),
        (2, "sha256", Value::Text("short".to_owned())),
        (
            2,
            "usable_hardware_encoders",
            Value::Strings(vec!["h264_nvenc".to_owned()]),
        ),
        (
            2,
            "hardware_encoder_probes",
            Value::Strings(vec!["untyped probe".to_owned()]),
        ),
        (3, "direct_entries", Value::Bool(false)),
        (4, "bundled", Value::Strings(Vec::new())),
        (6, "supported", Value::Text("true".to_owned())),
    ] {
        let mut bad = good.clone();
        bad[index].insert(key.to_owned(), wrong);
        assert!(doctor_schema(&bad).is_err(), "oracle accepted wrong {key}");
    }
    let mut missing = good.clone();
    missing[5].remove("packs");
    assert!(doctor_schema(&missing).is_err());
    let mut extra = good.clone();
    extra[0].insert("unversioned_extra".to_owned(), Value::Null);
    assert!(doctor_schema(&extra).is_err());
    let mut reordered = good.clone();
    reordered.swap(0, 1);
    assert!(doctor_schema(&reordered).is_err());
    assert!(doctor_schema(&good[..6]).is_err());
}

#[cfg(feature = "batch")]
mod binary {
    use super::*;
    use std::io::Read;
    use std::process::{Child, Command, Output, Stdio};
    use std::time::{Duration, Instant};

    struct Fixture {
        root: PathBuf,
        cache: PathBuf,
        ffmpeg: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let root = retained_root().expect("create owned doctor fixture");
            Self {
                cache: root.join("cache-é雪"),
                ffmpeg: root.join("missing-ffmpeg-é雪"),
                root,
            }
        }

        fn run(&self, extra: &[&str]) -> Output {
            let mut command = Command::new(env!("CARGO_BIN_EXE_fmn"));
            command
                .args(["doctor", "--threads", "1", "--ffmpeg"])
                .arg(&self.ffmpeg)
                .arg("--cache-dir")
                .arg(&self.cache)
                .args(extra)
                .current_dir(&self.root)
                .env_clear()
                .env("PATH", "")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            if cfg!(windows)
                && let Some(system_root) = std::env::var_os("SystemRoot")
            {
                command.env("SystemRoot", system_root);
            }
            let mut child = Running(command.spawn().expect("launch Cargo's actual fmn artifact"));
            let stdout = child.0.stdout.take().expect("piped stdout");
            let stderr = child.0.stderr.take().expect("piped stderr");
            let (send, receive) = std::sync::mpsc::channel();
            for (label, mut pipe) in [
                (true, Box::new(stdout) as Box<dyn Read + Send>),
                (false, Box::new(stderr) as Box<dyn Read + Send>),
            ] {
                let send = send.clone();
                std::thread::Builder::new()
                    .name("doctor-smoke-pipe".to_owned())
                    .spawn(move || {
                        let mut bytes = Vec::new();
                        let result = pipe
                            .by_ref()
                            .take((MAX_OUTPUT + 1) as u64)
                            .read_to_end(&mut bytes)
                            .map(|_| bytes);
                        let _ = send.send((label, result));
                    })
                    .expect("start bounded output reader");
            }
            drop(send);
            let deadline = Instant::now() + Duration::from_secs(20);
            let status = loop {
                if let Some(status) = child.0.try_wait().expect("observe doctor process") {
                    break status;
                }
                assert!(
                    Instant::now() < deadline,
                    "doctor process exceeded its deadline"
                );
                std::thread::sleep(Duration::from_millis(10));
            };
            let mut output = Output {
                status,
                stdout: Vec::new(),
                stderr: Vec::new(),
            };
            for _ in 0..2 {
                let (is_stdout, bytes) = receive
                    .recv_timeout(Duration::from_secs(5))
                    .expect("doctor pipes close after process exit");
                let bytes = bytes.expect("read doctor output");
                assert!(bytes.len() <= MAX_OUTPUT, "doctor exceeded output budget");
                if is_stdout {
                    output.stdout = bytes;
                } else {
                    output.stderr = bytes;
                }
            }
            output
        }
    }

    struct Running(Child);
    impl Drop for Running {
        fn drop(&mut self) {
            if self.0.try_wait().is_ok_and(|status| status.is_some()) {
                return;
            }
            let _ = self.0.kill();
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline {
                if self.0.try_wait().is_ok_and(|status| status.is_some()) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    fn stdout(output: &Output) -> &str {
        std::str::from_utf8(&output.stdout).expect("stdout must be UTF-8")
    }
    fn successful_records(output: &Output) -> Vec<Record> {
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        let parsed = records(stdout(output)).expect("actual doctor output parses as NDJSON");
        doctor_schema(&parsed).expect("actual doctor has seven complete typed records");
        parsed
    }

    #[test]
    fn doctor_binary_robot_reports_real_native_inventory_and_missing_optional_ffmpeg() {
        let fixture = Fixture::new();
        let parsed = successful_records(&fixture.run(&["--robot"]));
        assert_eq!(parsed[2]["available"], Value::Bool(false));
        assert_eq!(
            text(&parsed[2], "attempted").expect("attempted path"),
            fixture.ffmpeg.to_str().expect("UTF-8 fixture")
        );
        assert!(
            !text(&parsed[2], "reason")
                .expect("unavailable reason")
                .is_empty()
        );
        assert!(
            text(&parsed[2], "alternative")
                .expect("native alternative")
                .contains("y4m")
        );
        assert_eq!(
            text(&parsed[3], "root").expect("cache path"),
            fixture.cache.to_str().expect("UTF-8 fixture")
        );
        assert_eq!(parsed[3]["exists"], Value::Bool(false));
        assert_eq!(parsed[4]["complete"], Value::Bool(true));
        assert!(
            !fixture.cache.exists(),
            "doctor must not create a cache store"
        );
        assert!(!fixture.ffmpeg.exists());
    }

    #[test]
    fn doctor_binary_required_ffmpeg_is_a_typed_refusal_after_the_full_snapshot() {
        let fixture = Fixture::new();
        for extra in [
            ["--robot", "--require-ffmpeg"].as_slice(),
            ["--robot", "--quiet", "--require-ffmpeg"].as_slice(),
        ] {
            let output = fixture.run(extra);
            assert_eq!(output.status.code(), Some(4), "{output:?}");
            assert!(output.stderr.is_empty());
            let parsed = records(stdout(&output)).expect("refusal NDJSON");
            assert_eq!(parsed.len(), 8);
            doctor_schema(&parsed[..7]).expect("full snapshot precedes refusal");
            let error = &parsed[7];
            fields(
                error,
                &[
                    ("schema", Field::Text),
                    ("version", Field::Uint),
                    ("kind", Field::Text),
                    ("exit_code", Field::Uint),
                    ("exit_name", Field::Text),
                    ("rule", Field::NullableText),
                    ("message", Field::Text),
                ],
            )
            .expect("typed CLI error schema");
            assert_eq!(text(error, "schema").expect("error schema"), "fmn.cli");
            assert_eq!(error.get("version"), Some(&Value::Uint(1)));
            assert_eq!(text(error, "kind").expect("error kind"), "error");
            assert_eq!(text(error, "exit_name").expect("typed error"), "capability");
            assert_eq!(error.get("exit_code"), Some(&Value::Uint(4)));
            assert_eq!(error.get("rule"), Some(&Value::Null));
            assert!(
                text(error, "message")
                    .expect("actionable refusal")
                    .contains("native PNG-sequence, GIF, and y4m outputs remain available")
            );
        }
    }

    #[test]
    fn doctor_binary_human_report_is_visible_and_quiet_keeps_errors_and_robot_records() {
        let fixture = Fixture::new();
        let output = fixture.run(&[]);
        assert_eq!(output.status.code(), Some(0));
        assert!(output.stderr.is_empty());
        assert!(stdout(&output).starts_with("FrankenManim doctor\n"));
        for prefix in [
            "topology:",
            "cores:",
            "SIMD:",
            "plan:",
            "ffmpeg: unavailable",
            "alternative:",
            "cache:",
            "fonts:",
            "math packs:",
            "certification:",
        ] {
            assert!(
                stdout(&output).lines().any(|line| line.starts_with(prefix)),
                "missing human section {prefix}"
            );
        }
        let quiet = fixture.run(&["--quiet"]);
        assert_eq!(quiet.status.code(), Some(0));
        assert!(quiet.stdout.is_empty() && quiet.stderr.is_empty());
        let refusal = fixture.run(&["--quiet", "--require-ffmpeg"]);
        assert_eq!(refusal.status.code(), Some(4));
        assert!(refusal.stdout.is_empty());
        assert!(
            std::str::from_utf8(&refusal.stderr)
                .expect("error text")
                .contains("ffmpeg was required but is unavailable")
        );
        successful_records(&fixture.run(&["--robot", "--quiet"]));
    }

    #[cfg(unix)]
    #[test]
    fn doctor_binary_robot_round_trips_actual_control_character_paths() {
        let mut fixture = Fixture::new();
        let component = "é雪🚀-quote\"-slash\\-line\n-tab\t-return\r-control\u{1f}";
        fixture.cache = fixture.root.join(format!("cache-{component}"));
        fixture.ffmpeg = fixture.root.join(format!("missing-{component}"));
        let parsed = successful_records(&fixture.run(&["--robot"]));
        assert_eq!(
            text(&parsed[3], "root").expect("cache root"),
            fixture.cache.to_str().expect("UTF-8 path")
        );
        assert_eq!(
            text(&parsed[2], "attempted").expect("ffmpeg path"),
            fixture.ffmpeg.to_str().expect("UTF-8 path")
        );
        assert!(
            !fixture.cache.exists(),
            "read-only inspection retains absence"
        );
    }
}
